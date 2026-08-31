// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! LAZY-INIT REENTRANCY CENSUS — the L0-DEADLOCK class in its REENTRANCY
//! sense, as a fail-closed, build-blocking obligation (OB-19..OB-21).
//!
//! THE BUG THIS GUARDS (shipped in v0.65.0 and v0.66.0; fixed by 9811b83c):
//!
//! ```text
//! pub(crate) fn debug_seamless_reexec_armed() -> bool {
//!     static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
//!     *ARMED.get_or_init(|| {
//!         let on = crate::app_update_screen::debug_seamless_reexec_armed();
//!         //       ^^^^ the initializer calls the accessor it is running under
//!         …
//!     })
//! }
//! ```
//!
//! `Once::call` marks the cell RUNNING before it invokes the initializer, so
//! the nested call found its own `Once` already claimed and PARKED THE CALLING
//! THREAD — permanently, with no timeout and no way out. The site is reached
//! from `apply_staged_update_now` on the winit MAIN THREAD, so the first
//! automatic update apply froze the whole terminal: window up, caret alive,
//! nothing responding. A hang report from the field (2026-08-30, 0.65.0,
//! unresponsive 3588 s before sampling) shows the main thread stopped inside
//! `std::sync::Once::call` → `wait` → `_dispatch_semaphore_wait_slow`, with
//! `Once::call`'s frame appearing TWICE — the closure arm above the wait arm.
//!
//! Nothing in the build caught it. It is not a lock-ORDER cycle, so OB-7 is
//! silent (a `Once` is not in the lock graph); it is not an unbounded sink, so
//! OB-1..OB-6 are silent; `clippy` has no lint for it; and the unit test that
//! DOES pin it can only pin it by HANGING, which is a stopped suite rather
//! than a red one. This census is the standing CLASS guard: it derives every
//! lazy cell in `crates/*/src`, derives which functions BLOCKINGLY touch each
//! one, and fails the build if any initializer can reach a blocking touch of
//! the cell it is itself initializing — directly, or around a cycle.
//!
//! ONE implementation, TWO consumers (so the verb and the gate cannot diverge):
//!
//!   * `cargo xtask gate lazyinit` (crates/xtask/src/gate.rs) — the standalone
//!     verb, part of `gate all`.
//!   * `tools/freeze-safety-gate/build.rs` — the same `cargo build` that runs
//!     the temporal proof gate and the other four censuses; any violation
//!     fails the compile.
//!
//! The obligations (each fail-closed; `[OB-n]` tags appear in the diagnostics):
//!
//! * OB-19 SELF-REENTRANCY. No lazy initializer may reach a BLOCKING touch of
//!   the very cell it is initializing. This is a self-loop in the lazy-init
//!   graph and it is ALWAYS a deadlock — the thread waits on itself. There is
//!   NO waiver channel, ever.
//! * OB-20 MUTUAL REENTRANCY. The lazy-init graph — an edge `S -> T` whenever
//!   `S`'s initializer can reach a blocking touch of `T` — must be ACYCLIC.
//!   A cycle of length ≥ 2 is the same deadlock reached the long way round on
//!   one thread, and an ordering hazard on two. Also no waiver channel.
//! * OB-21 CENSUS LIVENESS — an ANTI-VACUITY floor, and nothing more. If the
//!   derivation finds no cells, no initializers or no blocking touches at all,
//!   the construct has been renamed wholesale (a wrapper type, a macro) and
//!   this census is blind rather than clean, so the build fails until the
//!   walker is re-audited — the same posture OB-4 takes for the offload
//!   boundary.
//!
//!   BE CLEAR ABOUT WHAT IT DOES NOT DO, because this comment used to overclaim
//!   it and the adversarial audit called that out: OB-21 fires only when a
//!   count reaches ZERO TREE-WIDE. With 137 cells standing it can never fire,
//!   so it is not an incremental backstop. ONE site refactored behind a wrapper
//!   type is invisible to it, and the census will report "the walk sees the
//!   idiom" while missing that site. Coverage of individual sites rests on the
//!   vocabulary and the join rules below, not on OB-21.
//!
//! WHAT COUNTS AS A BLOCKING TOUCH. Only the operations that can PARK:
//! `get_or_init` / `get_or_try_init` on a `OnceLock`/`OnceCell`, `call_once` /
//! `call_once_force` on a `Once`, and ANY use of a `LazyLock`/`Lazy` cell
//! (every use derefs, and the first deref runs the initializer). `OnceLock::get`,
//! `::set`, `::get_mut` and `Once::is_completed` are NOT blocking touches and
//! are deliberately ignored — re-entering through `get()` is the SUPPORTED
//! escape from this hazard, and flagging it would push authors away from the
//! fix.

use crate::CensusOutcome;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The honest limits of this walker, printed verbatim in every RED diagnostic.
// NOTE: a plain multi-line literal (no `\` continuations, which would strip the
// leading indentation the diagnostic relies on).
pub const LAZY_INIT_PRECISION_NOTE: &str =
    "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL, name-based: cells are found at their `static NAME: <Lazy…>`
        declarations, blocking touches by the receiver identifier before a
        blocking method (or any mention, for a deref-lazy cell), and calls as
        `<ident>(` over rustfmt-segmented fn bodies. No type, trait or borrow
        information exists here. Two cells sharing an identifier are ONE node.
      - REACH IS DELIBERATELY SHALLOW, and this is the load-bearing precision
        decision. A name-merged call graph's transitive closure over an
        84-crate workspace reaches almost everything from almost everything —
        measured on this tree, an unbounded walk turned 87 cells into one
        27-cell `cycle` of pure noise. So the walk follows: (a) the
        initializer's own text, (b) its DIRECT callees, resolved workspace-wide,
        and (c) transitively, only callees DEFINED IN THE SAME FILE, to a small
        depth. A name with many definitions carries no information once merged
        and is not followed at all. Consequence, stated plainly: an initializer
        that re-enters its own cell through a chain of cross-file helpers is
        NOT caught. Every instance of this hazard that has actually shipped —
        including the v0.65.0 freeze this census exists for — is a direct
        self-call, and that is caught with the path printed.
      - CELLS ARE FOUND TWO WAYS, and the second is narrower on purpose: a
        `static NAME: <LazyType>` anywhere (including inside a fn body, as the
        shipped bug was), and a STRUCT FIELD `name: <LazyType>,`. A field cell
        is tracked through its blocking METHODS only, never through a bare
        mention of its name: `cache` or `state` is not globally unique the way
        a static's name is, and treating every occurrence as a touch would
        manufacture edges in an obligation that has no waiver channel. So a
        `LazyLock` FIELD re-entered by a plain deref is NOT caught, and a field
        written without rustfmt's trailing comma is not seen as a field.
      - SCOPE: non-test sources of every workspace member (`crates/*/src`).
        `tools/*/src` and the sibling workspaces (`aterm-link/`,
        `astream-oracle/`) are NOT censused — they are dev/host code outside the
        shipped binary — so a lazy cell there is unguarded; there is one today,
        at tools/temporal-extract/src/main.rs. This crate's own sources are
        excluded as well, because its red fixtures quote whole Rust programs.
        Within the scanned set,
        with `#[cfg(test)]` / `#[cfg(kani)]` items blanked: test-only code
        never ships, and a deadlock in a test is a stopped suite, not a frozen
        terminal. Cells declared INSIDE a fn body are covered (the shipped bug
        was one).
      - NOT flagged: `OnceLock::get`, `::set`, `Once::is_completed` and other
        NON-blocking touches. Re-entering a cell through `get()` is the
        supported escape from this hazard, not an instance of it.
";

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// Type tokens that mark a `static` as a lazily-initialised cell whose EVERY
/// use derefs it — so every mention is a blocking touch.
const DEREF_LAZY_TYPES: &[&str] = &["LazyLock", "Lazy<", "SyncLazy"];

/// Type tokens whose blocking surface is exactly [`BLOCKING_METHODS`].
const METHOD_LAZY_TYPES: &[&str] = &["OnceLock", "OnceCell", "SyncOnceCell", "Once"];

/// The methods that can PARK the calling thread inside a lazy cell: each one
/// claims the cell and then runs (or waits for) the initializer.
const BLOCKING_METHODS: &[&str] = &[
    "get_or_init",
    "get_or_try_init",
    "get_or_init_with",
    "call_once",
    "call_once_force",
];

/// The constructors of a deref-lazy cell — the argument of one of these, in a
/// `static` declaration, is that cell's initializer.
const LAZY_CTORS: &[&str] = &["LazyLock::new", "Lazy::new", "SyncLazy::new"];

/// How far the walk follows SAME-FILE callees out of an initializer.
///
/// Four hops covers "the initializer calls a local helper that calls a local
/// helper that asks the accessor again" — the deepest shape this hazard has
/// taken here — while keeping the derived graph sparse. See the reach
/// paragraph of [`LAZY_INIT_PRECISION_NOTE`] for why it is not unbounded.
const MAX_LOCAL_DEPTH: usize = 4;

/// A callee name carried by MORE than this many definitions across the scan
/// set is too merged to mean anything (`new`, `default`, `len`, `lock`, …), so
/// the walk does not follow it.
///
/// Without this the census reports `TABLES.get_or_init(TransferTables::new)` as
/// self-reentrant, because the ONE merged `new` node calls everything that any
/// `new` in the workspace calls. Skipping over-merged names is the same
/// report-what-is-legible choice the other censuses make when a name cannot be
/// resolved, and it is stated in the precision note rather than hidden.
const AMBIGUOUS_DEFS: usize = 3;

// ---------------------------------------------------------------------------
// Derived facts
// ---------------------------------------------------------------------------

/// A lazily-initialised cell found at its `static` declaration.
#[derive(Debug, Clone)]
struct Cell {
    /// The static's identifier, e.g. `ARMED`.
    name: String,
    /// `LazyLock`/`OnceLock`/… — the spelling found, for the diagnostic.
    kind: String,
    /// Repo-relative `file:line` of the declaration.
    span: String,
    /// `true` for `LazyLock`/`Lazy` (every use derefs = every use blocks).
    deref_lazy: bool,
    /// `true` when the cell is a STRUCT FIELD rather than a `static`.
    field: bool,
}

/// One place a cell is touched in a way that can PARK the calling thread.
#[derive(Debug, Clone)]
struct Touch {
    cell: String,
    /// The enclosing fn, if the touch is inside one. A touch at item scope
    /// cannot be REACHED by a call, so it never becomes a graph edge.
    in_fn: Option<String>,
    span: String,
    /// How it blocks — `get_or_init` / `deref` — for the diagnostic.
    how: String,
}

/// A cell's initializer: the code that runs under that cell's own `Once`.
#[derive(Debug, Clone)]
struct Initializer {
    cell: String,
    span: String,
    /// Repo-relative file the initializer is written in (the same-file walk).
    file: String,
    /// Every `<ident>(` called directly in the initializer's text, plus an
    /// UNQUALIFIED function path passed instead of a closure.
    callees: BTreeSet<String>,
    /// Cells this initializer blockingly touches in its own text (depth 0).
    direct_touches: BTreeSet<String>,
}

/// One edge of the lazy-init graph, with the witness that produced it.
#[derive(Debug, Clone)]
struct Edge {
    from: String,
    to: String,
    /// The rendered witness: where the initializer is and how it gets back.
    path: String,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the lazy-init reentrancy census over an aterm checkout at `root`.
#[must_use]
pub fn run_lazy_init_census(root: &Path) -> CensusOutcome {
    let files = scan_files(root);
    let sources = read_sources(root, &files);
    let derived = derive_from_sources(&sources);
    report(&root.display().to_string(), sources.len(), &derived)
}

fn report(root: &str, file_count: usize, d: &Derived) -> CensusOutcome {
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== lazy-init reentrancy census (OB-19..OB-21) over {root} ==="
    );
    let _ = writeln!(
        log,
        "  scanned {file_count} non-test source file(s); {} lazy cell(s), {} initializer(s), \
         {} blocking touch site(s), {} distinct fn name(s)",
        d.cells.len(),
        d.inits.len(),
        d.touches.len(),
        d.defs.len()
    );

    let mut failures = 0usize;

    // ---- OB-21: the walk must still see the idiom. ----
    if file_count == 0 || d.cells.is_empty() || d.inits.is_empty() || d.touches.is_empty() {
        failures += 1;
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-21] the census derived {file_count} file(s), {} cell(s), {} \
             initializer(s) and {} blocking touch(es). At least one of those is ZERO, so \
             this census is BLIND, not clean. Either the scan root moved, or the \
             lazy-init idiom was renamed (a wrapper type, a macro) — in which case teach \
             the walker the new spelling in `DEREF_LAZY_TYPES` / `METHOD_LAZY_TYPES` / \
             `BLOCKING_METHODS`. Do NOT delete this obligation to make the build pass: a \
             blind census is exactly how the v0.65.0 freeze shipped.",
            d.cells.len(),
            d.inits.len(),
            d.touches.len()
        );
    }

    // ---- OB-19 / OB-20: the lazy-init graph must be acyclic. ----
    let edges = derive_edges(d);
    let (self_loops, components) = find_cycles(&edges);

    for edge in &self_loops {
        failures += 1;
        let cell = d.cell(&edge.from);
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-19] SELF-REENTRANT lazy init: `{}` ({} declared at {}) is \
             initialised by code that BLOCKINGLY touches `{}` again.\n\
             \x20     {}\n\
             \x20   WHY THIS IS L0: the cell is marked RUNNING before the initializer \
             runs, so the nested touch waits for an initializer that is its own caller. \
             The thread parks and never wakes — no timeout, no panic, no log line. On a \
             main-thread path (a menu action, an event handler, the update apply) that is \
             the whole window frozen with the process still alive.\n\
             \x20   REPAIR: compute the value from FIRST SOURCES — the environment, a \
             parsed config, a constant — never by asking the accessor you are running \
             under. If the nested read is only a cache peek, use the non-blocking `get()` \
             and handle `None`.",
            edge.from,
            cell.map_or("lazy cell".to_string(), |c| {
                if c.field {
                    format!("{} FIELD", c.kind)
                } else {
                    c.kind.clone()
                }
            }),
            cell.map_or("<unknown>", |c| c.span.as_str()),
            edge.from,
            edge.path
        );
    }

    for component in &components {
        failures += 1;
        let members: BTreeSet<&str> = component.iter().map(|e| e.from.as_str()).collect();
        let rendered = members.iter().copied().collect::<Vec<_>>().join(", ");
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-20] MUTUALLY-REENTRANT lazy init: {{{rendered}}} is a strongly \
             connected component of the lazy-init graph — each of these cells can reach a \
             blocking touch of the others."
        );
        for e in component {
            let _ = writeln!(log, "\x20     {} -> {}: {}", e.from, e.to, e.path);
        }
        let _ = writeln!(
            log,
            "\x20   WHY THIS IS L0: one thread entering anywhere in this component claims \
             the first cell, runs its initializer, and arrives at a cell it is already \
             inside — the same permanent park as OB-19, reached the long way round. Two \
             threads entering at different points deadlock against each other without any \
             self-loop at all.\n\
             \x20   REPAIR: break the component. Hoist the shared value into a third cell \
             both initializers read, compute one side eagerly, or make the back-edge a \
             non-blocking `get()`."
        );
    }

    if failures == 0 {
        let _ = writeln!(
            log,
            "  ✓ GREEN [OB-19] no initializer reaches a blocking touch of its own cell\n\
             \x20 ✓ GREEN [OB-20] the lazy-init graph is ACYCLIC ({} edge(s) between cells)\n\
             \x20 ✓ GREEN [OB-21] the walk sees the idiom (cells, initializers and touches \
             all non-empty)\n\
             gate lazyinit: GREEN — {} lazy cell(s) over {file_count} file(s), no \
             reentrancy cycle.",
            edges.len(),
            d.cells.len()
        );
    } else {
        let _ = writeln!(
            log,
            "gate lazyinit: FAILED — {failures} obligation violation(s). There is NO \
             waiver channel for a reentrancy cycle: it can only be fixed."
        );
        log.push_str(LAZY_INIT_PRECISION_NOTE);
    }

    CensusOutcome {
        ok: failures == 0,
        log,
    }
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// Everything the walk derived from one tree.
struct Derived {
    cells: Vec<Cell>,
    inits: Vec<Initializer>,
    touches: Vec<Touch>,
    /// fn name -> how many definitions carry it (the ambiguity filter).
    defs: BTreeMap<String, usize>,
    /// file -> (fn name -> the names it calls directly), for the local walk.
    local: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    /// cell -> the fns that blockingly touch it (its trigger entry points).
    triggers: BTreeMap<String, BTreeSet<String>>,
    /// (cell, trigger fn) -> a representative touch span, for the diagnostic.
    trigger_spans: BTreeMap<(String, String), String>,
}

impl Derived {
    fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.name == name)
    }
}

/// Non-test `*.rs` under every workspace member's `src/` (`members = ["crates/*"]`).
fn scan_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return files;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path().join("src"))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let _ = crate::collect_rs_files(&dir, &mut files);
    }
    files.retain(|p| !crate::is_test_file(p));
    // THE CENSUS DOES NOT POLICE ITS OWN FIXTURES — the same self-exclusion
    // `scope_census::SELF_EXCLUDED_DIR` takes, for the same reason. This lane's
    // red fixtures embed whole Rust programs in raw strings at column 0,
    // including their `}` lines, and those are indistinguishable from real
    // declarations to a lexical walker. (They also end this file's own
    // `#[cfg(test)]` mask early, since a raw string CAN forge a line that is
    // exactly `}` at indent 0 — the one way the indent rule below is weaker
    // than it looks, and it is confined to files that quote Rust.)
    files.retain(|p| {
        !p.to_string_lossy()
            .replace('\\', "/")
            .contains(SELF_EXCLUDED_DIR)
    });
    files.sort();
    files
}

/// This crate's own sources — excluded from the scan; see [`scan_files`].
const SELF_EXCLUDED_DIR: &str = "crates/aterm-census/src";

/// Read each source once, as written. Masking happens in
/// [`derive_from_sources`], so the synthetic-source seam the red fixtures drive
/// goes through the SAME masking the real tree does.
fn read_sources(root: &Path, files: &[PathBuf]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for path in files {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push((rel, raw));
    }
    out
}

/// The derivation proper, over `(repo-relative path, masked text)` pairs — the
/// seam the unit tests drive with synthetic sources.
fn derive_from_sources(raw_sources: &[(String, String)]) -> Derived {
    // Test-only and kani-only items never ship: a deadlock there stops a
    // suite, which is loud, not a terminal, which is silent.
    let sources: Vec<(String, String)> = raw_sources
        .iter()
        .map(|(rel, raw)| (rel.clone(), mask_unshipped(raw)))
        .collect();
    let sources = &sources[..];
    let mut cells: Vec<Cell> = Vec::new();
    let mut defs: BTreeMap<String, usize> = BTreeMap::new();
    let mut local: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();

    // PASS 1: every cell in the tree, and the per-file call graph. Cells must
    // all be known before touches are collected, because a deref-lazy cell is
    // touched by files that may be read before its declaration.
    for (rel, text) in sources {
        // Attribute every call to the fn whose body the line sits in, using
        // THIS module's segmentation rather than `parse_source_fns` — the
        // shared one has no body-less-declaration guard, and an `extern "C"`
        // block's `fn foo(..) -> T;` there silently absorbs the next real fn's
        // body into its own callee set. That is not a hypothetical: it is what
        // produced the phantom `IOServiceMatching -> connection` edge in
        // keymap.rs the first time this census ran.
        let lines: Vec<&str> = text.lines().collect();
        let enclosing = enclosing_fns(&lines);
        let per_file = local.entry(rel.clone()).or_default();
        let mut seen_here: BTreeSet<String> = BTreeSet::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(owner) = enclosing[i].as_ref() else {
                continue;
            };
            if seen_here.insert(owner.clone()) {
                *defs.entry(owner.clone()).or_default() += 1;
            }
            let code = crate::strip_line_comment(line).to_string();
            per_file
                .entry(owner.clone())
                .or_default()
                .extend(crate::callee_names(std::slice::from_ref(&code)));
        }
        collect_cells(text, rel, &mut cells);
    }

    // PASS 2: blocking touches and initializers, now that every cell is known.
    let mut touches: Vec<Touch> = Vec::new();
    let mut inits: Vec<Initializer> = Vec::new();
    for (rel, text) in sources {
        collect_touches_and_inits(text, rel, &cells, &mut touches, &mut inits);
    }

    let mut triggers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut trigger_spans: BTreeMap<(String, String), String> = BTreeMap::new();
    for t in &touches {
        let Some(f) = t.in_fn.as_ref() else { continue };
        triggers
            .entry(t.cell.clone())
            .or_default()
            .insert(f.clone());
        trigger_spans
            .entry((t.cell.clone(), f.clone()))
            .or_insert_with(|| format!("{} ({})", t.span, t.how));
    }

    Derived {
        cells,
        inits,
        touches,
        defs,
        local,
        triggers,
        trigger_spans,
    }
}

/// Blank `#[cfg(test)]` / `#[cfg(kani)]` items — the SHARED masker, which this
/// lane's adversarial audit is the reason to trust.
///
/// This was a private COPY, and the comment in its place said why:
/// `lock_order::mask_gated_items` found the end of a gated item by counting `{`
/// and `}` INCLUDING braces inside string and char literals, so one `"}"`,
/// `'{'` or `format!("{{")` desynchronised the depth and every remaining line
/// of the file was blanked — 19 553 of `crates/aterm-gui/src/lib.rs`'s 39 833,
/// and everything after line 3 046 of `seamless.rs`'s 5 813, the very subsystem
/// the v0.65.0 freeze lived in. The copy ended a body at rustfmt's `<indent>}`
/// instead, and the note recorded that fixing the SHARED masker was the right
/// repair, deferred only because un-blinding the other censuses had to be
/// adjudicated by their owners rather than arrive as a side effect of this
/// lane's work.
///
/// That adjudication happened (2026-08-30) and the repair now lives in
/// `mask_gated_items` itself — with two rules the copy never had. An item can
/// end at a COMMA at the gate's own indent (an enum variant, a struct field, a
/// match arm): the copy ran such an item on to the next `<indent>}`, which at
/// `crates/aterm-gui/src/lib.rs:2570` means 830 lines of shipped `enum`
/// swallowed. And the walk that looks for the body's opening brace masks
/// literals first: without that, `debug_assert!(` … `"exceeds count({})"` … `)`
/// at `crates/aterm-scrollback/src/cold_tier.rs:612` is cut in half at its own
/// format string. Keeping the copy would have kept both of those bugs in this
/// lane alone, so the copy is gone: ONE masker, one place to fix it, and this
/// lane's fixtures still drive it through [`derive_from_sources`].
///
/// The caveat the copy's comment named is unchanged and now documented at the
/// shared masker too: a raw string CAN forge a line that is exactly `<indent>}`
/// and end a body early. It costs at most a false positive (masking is
/// subtractive), it only happens in files that quote whole Rust programs, and
/// this lane already excludes its own — see [`SELF_EXCLUDED_DIR`].
fn mask_unshipped(text: &str) -> String {
    crate::lock_order::mask_gated_items(text, &["#[cfg(test)]", "#[cfg(kani)]"])
}

/// `static [mut] NAME: …<lazy type>… = …;` declarations, wherever they sit —
/// item scope OR inside a fn body (the shipped bug's cell was fn-local).
fn collect_cells(text: &str, rel: &str, out: &mut Vec<Cell>) {
    for (i, line) in text.lines().enumerate() {
        let code = crate::strip_line_comment(line);
        let Some(name) = static_name(code) else {
            continue;
        };
        // The declaration may wrap; read forward to the terminating `;` so the
        // type is visible however it is formatted.
        let decl = declaration_text(text, i);
        let deref_lazy = DEREF_LAZY_TYPES.iter().any(|t| decl.contains(t));
        let method_lazy = METHOD_LAZY_TYPES.iter().any(|t| decl.contains(t));
        if !deref_lazy && !method_lazy {
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|c| c.name == name) {
            // Same-named cells merge into ONE node — the documented
            // over-approximation. Merge toward the WIDER hazard: if either
            // spelling derefs, the merged node derefs, or an earlier
            // `OnceLock` named `CACHE` would silently erase a later
            // `LazyLock CACHE`'s mention rule.
            existing.deref_lazy |= deref_lazy;
            // A `static` won this merge, so the node is no longer field-shaped.
            existing.field = false;
            continue;
        }
        let kind = DEREF_LAZY_TYPES
            .iter()
            .chain(METHOD_LAZY_TYPES.iter())
            .find(|t| decl.contains(**t))
            .map_or("lazy", |t| t.trim_end_matches('<'))
            .to_string();
        out.push(Cell {
            name,
            kind,
            span: format!("{rel}:{}", i + 1),
            deref_lazy,
            field: false,
        });
    }
    collect_field_cells(text, rel, out);
}

/// Cells that are a STRUCT FIELD rather than a `static`.
///
/// The first cut of this census only knew `static NAME: OnceLock<..>`, and a
/// `OnceLock` field re-entered from its own initializer —
/// `*self.cache.get_or_init(|| self.armed())` — sailed through GREEN. That is
/// not a contrived shape: it is the ordinary way to memoise something per
/// session, and it parks exactly as hard as the shipped `static` did. Measured
/// on this tree when the gap was found: one field cell
/// (`operator_host.rs:434 marker_queue`), zero blocking touches on it — so the
/// hazard was latent, not live, which is the moment to close it.
///
/// Everything downstream needs no change: a touch is found by the identifier
/// before the blocking method, and for `self.cache.get_or_init(..)` that
/// identifier is already `cache`.
fn collect_field_cells(text: &str, rel: &str, out: &mut Vec<Cell>) {
    for (i, line) in text.lines().enumerate() {
        let code = crate::strip_line_comment(line);
        let Some(name) = field_cell_name(code) else {
            continue;
        };
        if out.iter().any(|c| c.name == name) {
            continue;
        }
        let kind = DEREF_LAZY_TYPES
            .iter()
            .chain(METHOD_LAZY_TYPES.iter())
            .find(|t| code.contains(**t))
            .map_or("lazy", |t| t.trim_end_matches('<'))
            .to_string();
        out.push(Cell {
            name,
            kind,
            span: format!("{rel}:{}", i + 1),
            // A FIELD cell is tracked through its blocking METHODS only, never
            // through bare mentions. A `static LazyLock` earns the mention rule
            // because its name is globally unique and every use derefs it; a
            // field name like `cache` or `state` is neither, and treating every
            // occurrence of it as a touch would manufacture edges — and this
            // obligation has no waiver channel, so a false RED is expensive.
            // Consequence, recorded in the precision note: a `LazyLock` FIELD
            // re-entered by a bare deref is not caught.
            deref_lazy: false,
            field: true,
        });
    }
}

/// `<name>: <LazyType><..>,` as a struct field — not a `let`, not a `static`,
/// not a by-reference fn parameter.
fn field_cell_name(code: &str) -> Option<String> {
    let t = code.trim();
    if !t.ends_with(',') || t.contains('=') {
        return None;
    }
    for kw in ["let ", "static ", "const ", "fn ", "if ", "match ", "for "] {
        if t.starts_with(kw) {
            return None;
        }
    }
    // Strip a visibility prefix: `pub `, or `pub(crate) ` / `pub(super) ` etc.
    let body = if let Some(rest) = t.strip_prefix("pub(") {
        match rest.split_once(") ") {
            Some((_, after)) => after.trim_start(),
            None => return None,
        }
    } else {
        t.strip_prefix("pub ").map_or(t, str::trim_start)
    };
    let (name, ty) = body.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let ty = ty.trim();
    // A borrowed cell is someone else's, passed in — the OWNER declares it.
    if ty.starts_with('&') {
        return None;
    }
    let is_cell = DEREF_LAZY_TYPES
        .iter()
        .chain(METHOD_LAZY_TYPES.iter())
        .any(|k| ty.contains(k));
    if !is_cell {
        return None;
    }
    Some(name.to_string())
}

/// The identifier a `[pub[(..)]] static [mut] NAME…` line declares.
///
/// The visibility prefix is stripped first. Without that, `pub(crate) static
/// CELL: OnceLock<..>` was not recognised as a cell AT ALL — the census simply
/// did not see it, and a self-recursive initializer on it passed GREEN.
fn static_name(code: &str) -> Option<String> {
    let t = code.trim_start();
    let t = if let Some(rest) = t.strip_prefix("pub(") {
        rest.split_once(") ").map(|(_, after)| after.trim_start())?
    } else {
        t.strip_prefix("pub ").map_or(t, str::trim_start)
    };
    let rest = t.strip_prefix("static ")?;
    let rest = rest.strip_prefix("mut ").unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

/// The `static` declaration starting at `line_idx`, up to and including its
/// terminating `;` (bounded, so a parse slip cannot run away).
fn declaration_text(text: &str, line_idx: usize) -> String {
    // The terminator is a `;` at BRACKET DEPTH 0. Stopping at the first `;`
    // anywhere truncated every declaration whose TYPE contains one —
    // `static LUT: LazyLock<[f32; 256]> =` ends its first line inside the
    // array length — so the wrapped `LazyLock::new(..)` on the next line was
    // never found and the cell entered the graph with NO initializer at all.
    // Measured when this was found: 6 of the 10 shipping deref-lazy cells.
    let mut out = String::new();
    let mut depth = 0i32;
    for line in text.lines().skip(line_idx).take(40) {
        let code = crate::strip_line_comment(line);
        out.push_str(code);
        out.push('\n');
        let mut done = false;
        for b in code.bytes() {
            match b {
                b'(' | b'[' | b'<' => depth += 1,
                b')' | b']' | b'>' => depth -= 1,
                b';' if depth <= 0 => done = true,
                _ => {}
            }
        }
        if done {
            break;
        }
    }
    out
}

/// Blocking touches and initializers in one file.
///
/// THE SCAN IS OVER THE WHOLE FILE, NOT LINE BY LINE, and that is a correctness
/// requirement rather than a refactor. Rust is not line-oriented and rustfmt
/// wraps at 100 columns, so the incident's own shape — with identifiers only
/// slightly longer than the ones that shipped — formats as
///
/// ```text
///     *ARMED_SEAMLESS_REEXEC_FLAG
///         .get_or_init(|| debug_seamless_reexec_armed())
/// ```
///
/// where the receiver and the blocking method are on DIFFERENT LINES. The first
/// cut of this census matched `.get_or_init(` within a single line and took the
/// identifier immediately before it, so that shape — the real bug, merely
/// reformatted — passed GREEN. `cargo fmt` could turn a RED tree green.
fn collect_touches_and_inits(
    text: &str,
    rel: &str,
    cells: &[Cell],
    touches: &mut Vec<Touch>,
    inits: &mut Vec<Initializer>,
) {
    let lines: Vec<&str> = text.lines().collect();
    let masked: Vec<String> = lines
        .iter()
        .map(|l| crate::strip_line_comment(l).to_string())
        .collect();
    let enclosing = enclosing_fns(&lines);
    // One comment-masked buffer with the newlines kept, so byte offsets map
    // back to line numbers exactly.
    let joined = masked.join("\n");
    let line_starts = line_start_index(&joined);
    let line_of = |at: usize| match line_starts.binary_search(&at) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };

    // --- method form: `CELL.get_or_init(` and friends, however it is wrapped.
    for method in BLOCKING_METHODS {
        let mut from = 0usize;
        while let Some((at, open)) = find_method_call(&joined, method, from) {
            from = open + 1;
            let Some(recv) = receiver_before(&joined, at) else {
                continue;
            };
            let Some(cell) = cells.iter().find(|c| c.name == recv) else {
                continue;
            };
            let ln = line_of(at);
            touches.push(Touch {
                cell: cell.name.clone(),
                in_fn: enclosing.get(ln).cloned().flatten(),
                span: format!("{rel}:{}", ln + 1),
                how: (*method).to_string(),
            });
            // The argument of a blocking method IS the initializer.
            let body = balanced(&joined, open);
            inits.push(Initializer {
                cell: cell.name.clone(),
                span: format!("{rel}:{}", ln + 1),
                file: rel.to_string(),
                callees: initializer_callees(&body),
                direct_touches: direct_touches(&body, cells),
            });
        }
    }

    // --- deref form: any mention of a `LazyLock`/`Lazy` cell. ---
    for (i, code) in masked.iter().enumerate() {
        let declares = static_name(code);
        for cell in cells.iter().filter(|c| c.deref_lazy) {
            if !mentions_ident(code, &cell.name) {
                continue;
            }
            if declares.as_deref() == Some(cell.name.as_str()) {
                // The declaration is where the initializer lives, not a touch.
                let decl = declaration_text(text, i);
                for ctor in LAZY_CTORS {
                    let needle = format!("{ctor}(");
                    let Some(off) = decl.find(&needle) else {
                        continue;
                    };
                    let before = &decl[..off + needle.len() - 1];
                    let dl = before.matches('\n').count();
                    let col = before.rsplit('\n').next().map_or(0, str::len);
                    let abs = line_starts.get(i + dl).map_or(0, |s| s + col);
                    let body = balanced(&joined, abs);
                    inits.push(Initializer {
                        cell: cell.name.clone(),
                        span: format!("{rel}:{}", i + 1),
                        file: rel.to_string(),
                        callees: initializer_callees(&body),
                        direct_touches: direct_touches(&body, cells),
                    });
                }
                continue;
            }
            touches.push(Touch {
                cell: cell.name.clone(),
                in_fn: enclosing[i].clone(),
                span: format!("{rel}:{}", i + 1),
                how: "deref".to_string(),
            });
        }
    }
}

/// Byte offset of the start of each line in `joined`.
fn line_start_index(joined: &str) -> Vec<usize> {
    let mut out = vec![0usize];
    for (i, b) in joined.bytes().enumerate() {
        if b == b'\n' {
            out.push(i + 1);
        }
    }
    out
}

/// Find the next `.<method>(` after `from`, tolerating a TURBOFISH between the
/// name and the parenthesis: `CELL.get_or_init::<_>(|| ..)` is ordinary Rust and
/// its `F` is inferrable, so nothing stops an author writing it. Matching the
/// literal `.get_or_init(` missed it entirely — the touch AND its initializer
/// both vanished, and a self-recursive cell spelled that way passed the gate
/// GREEN. Found by `codex review`, not by the adversarial sweep.
///
/// Returns `(dot_index, open_paren_index)`.
fn find_method_call(hay: &str, method: &str, from: usize) -> Option<(usize, usize)> {
    let needle = format!(".{method}");
    let bytes = hay.as_bytes();
    let mut cur = from;
    while let Some(rel) = hay[cur..].find(&needle) {
        let at = cur + rel;
        cur = at + needle.len();
        // The name must END here: `.get_or_init_with` must not match `get_or_init`.
        if bytes.get(cur).copied().is_some_and(is_ident_byte) {
            continue;
        }
        let mut p = cur;
        while bytes
            .get(p)
            .copied()
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            p += 1;
        }
        // Optional `::< … >`, angle-balanced so a nested generic is skipped whole.
        if hay[p..].starts_with("::<") {
            let mut depth = 0i32;
            let mut q = p + 2;
            while q < bytes.len() {
                match bytes[q] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            q += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                q += 1;
            }
            p = q;
            while bytes
                .get(p)
                .copied()
                .is_some_and(|b| b.is_ascii_whitespace())
            {
                p += 1;
            }
        }
        if bytes.get(p) == Some(&b'(') {
            return Some((at, p));
        }
    }
    None
}

/// The receiver identifier before the `.` at `at`, skipping any whitespace and
/// newlines the formatter put between them.
fn receiver_before(joined: &str, at: usize) -> Option<&str> {
    let bytes = joined.as_bytes();
    let mut end = at;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    crate::ident_ending_at(joined, end)
}

/// `ident` occurs in `code` as a whole word.
fn mentions_ident(code: &str, ident: &str) -> bool {
    let bytes = code.as_bytes();
    let mut from = 0usize;
    while let Some(rel_at) = code[from..].find(ident) {
        let at = from + rel_at;
        from = at + ident.len();
        let before_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let after = at + ident.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// For every line, the fn whose body it sits in (rustfmt's
/// closing-brace-at-fn-indent invariant, the same segmentation the main-loop
/// census uses). Nested fns and closures resolve to the OUTERMOST fn, which is
/// the entry point a caller can actually reach.
fn enclosing_fns(lines: &[&str]) -> Vec<Option<String>> {
    let mut out = vec![None; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        let Some((indent, name)) = crate::parse_fn_def(lines[i]) else {
            i += 1;
            continue;
        };
        // A fn DECLARATION has no body: `unsafe extern "C" { fn f(..) -> T; }`
        // and trait-method signatures both end in `;` before any `{`. Treating
        // one as an opening body makes the close-brace scan swallow everything
        // down to the next same-indent `}` — which is how `fn IOServiceMatching`
        // in keymap.rs came out as the enclosing fn of a `get_or_init` 20 lines
        // later, i.e. a phantom self-reentrancy finding. Observed, not feared.
        if is_bodyless_declaration(lines, i) {
            i += 1;
            continue;
        }
        let def_line = crate::strip_line_comment(lines[i]);
        let opens = def_line.matches('{').count();
        let single_line = opens > 0 && opens == def_line.matches('}').count();
        let close = format!("{}}}", " ".repeat(indent));
        let mut end = i;
        if !single_line {
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j] == close {
                    end = j;
                    break;
                }
                j += 1;
            }
        }
        for slot in out.iter_mut().take(end + 1).skip(i) {
            *slot = Some(name.clone());
        }
        i = end + 1;
    }
    out
}

/// Whether the fn definition starting at `at` is a body-less DECLARATION: its
/// signature reaches a `;` before it ever reaches a `{`.
fn is_bodyless_declaration(lines: &[&str], at: usize) -> bool {
    // DEPTH MATTERS. `fn table() -> [f32; 256] {` has a `;` before its `{`, and
    // reading that as "declaration, no body" made the census skip the whole
    // function — every call in it vanished from the graph, and a cell it
    // re-entered went GREEN. Only a `;` at bracket depth 0 ends a signature.
    let mut depth = 0i32;
    for line in lines.iter().skip(at).take(24) {
        for b in crate::strip_line_comment(line).bytes() {
            match b {
                // ONLY `()` and `[]`. Counting `<`/`>` as brackets looks right
                // and is wrong: the `>` of the return arrow `->` decrements on
                // every signature, so `fn t() -> [f32; 256] {` arrived at its
                // array's `;` already at depth -1 and was still read as a
                // body-less declaration. `[]` is what carries an array length,
                // which is the only `;` a signature realistically holds.
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'{' if depth <= 0 => return false,
                b';' if depth <= 0 => return true,
                _ => {}
            }
        }
    }
    false
}

/// The text between the `(` at byte offset `at` and its matching `)`, capped at
/// 40 000 bytes (a bound, not a semantic limit — no real initializer is longer).
/// Returned as lines, which is what the callee scan wants.
fn balanced(joined: &str, at: usize) -> Vec<String> {
    let bytes = joined.as_bytes();
    let mut depth = 0i32;
    let mut end = joined.len().min(at + 40_000);
    for (i, b) in joined[at..end].bytes().enumerate() {
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                end = at + i + 1;
                break;
            }
        }
    }
    let _ = bytes;
    joined[at..end].lines().map(str::to_string).collect()
}

/// Every `<ident>(` in the initializer, plus an UNQUALIFIED function path
/// passed instead of a closure (`CELL.get_or_init(build_it)`).
///
/// A QUALIFIED path (`CELL.get_or_init(TransferTables::new)`) is deliberately
/// not followed: its last segment is a merged super-node (`new`) that carries
/// no information about the function actually referenced.
fn initializer_callees(body: &[String]) -> BTreeSet<String> {
    let mut out = crate::callee_names(body);
    let joined = body.join(" ");
    let inner = joined
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if !inner.contains('|') && !inner.contains('(') && !inner.is_empty() {
        // A QUALIFIED path's last segment is followed too, and the ambiguity
        // filter in `reachable` is what makes that safe: `Type::new` resolves
        // to the merged `new` super-node (246 definitions here) and is dropped
        // there, while `Type::with_builtin_rules` names exactly one function
        // and is worth following. Refusing every qualified path — the first
        // cut — meant `CELL.get_or_init(Type::assoc_fn)` produced an
        // initializer with NO outgoing edges at all.
        let last = inner.rsplit("::").next().unwrap_or(inner);
        let ident: String = last
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !ident.is_empty() {
            out.insert(ident);
        }
    }
    out
}

/// Cells this initializer text touches BLOCKINGLY in its own body (depth 0).
fn direct_touches(body: &[String], cells: &[Cell]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in body {
        for method in BLOCKING_METHODS {
            let mut from = 0usize;
            while let Some((at, open)) = find_method_call(line, method, from) {
                from = open + 1;
                if let Some(recv) = receiver_before(line, at)
                    && cells.iter().any(|c| c.name == recv)
                {
                    out.insert(recv.to_string());
                }
            }
        }
        for cell in cells.iter().filter(|c| c.deref_lazy) {
            if mentions_ident(line, &cell.name) {
                out.insert(cell.name.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The lazy-init graph
// ---------------------------------------------------------------------------

/// `S -> T` whenever `S`'s initializer can reach a blocking touch of `T`.
fn derive_edges(d: &Derived) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for init in &d.inits {
        // Depth 0: written right there in the initializer.
        for target in &init.direct_touches {
            if seen.insert((init.cell.clone(), target.clone())) {
                edges.push(Edge {
                    from: init.cell.clone(),
                    to: target.clone(),
                    path: format!("initializer at {} touches `{target}` directly", init.span),
                });
            }
        }
        // Depth 1 (workspace-wide) and deeper (same file only).
        let reach = reachable(d, init);
        for (target, trigger_fns) in &d.triggers {
            let Some(hit) = trigger_fns.iter().find(|f| reach.contains_key(f.as_str())) else {
                continue;
            };
            if !seen.insert((init.cell.clone(), target.clone())) {
                continue;
            }
            let touch = d
                .trigger_spans
                .get(&(target.clone(), hit.clone()))
                .map_or("?", String::as_str);
            edges.push(Edge {
                from: init.cell.clone(),
                to: target.clone(),
                path: format!(
                    "initializer at {} calls {}, which touches `{target}` at {touch}",
                    init.span,
                    render_path(&reach, hit)
                ),
            });
        }
    }
    edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    edges
}

/// Fn names reachable from `init`, mapped to the caller each was first reached
/// from. The DIRECT callees are taken as written (a call out of the initializer
/// names a real function); everything deeper is followed only within
/// `init.file`, and never through an over-merged name. See the reach paragraph
/// of [`LAZY_INIT_PRECISION_NOTE`].
fn reachable(d: &Derived, init: &Initializer) -> BTreeMap<String, Option<String>> {
    let empty = BTreeMap::new();
    let local = d.local.get(&init.file).unwrap_or(&empty);
    let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for c in &init.callees {
        if seen.insert(c.clone(), None).is_none() {
            queue.push_back((c.clone(), 0));
        }
    }
    while let Some((f, depth)) = queue.pop_front() {
        if depth >= MAX_LOCAL_DEPTH || d.defs.get(&f).copied().unwrap_or(0) > AMBIGUOUS_DEFS {
            continue;
        }
        let Some(callees) = local.get(&f) else {
            continue;
        };
        for c in callees {
            if seen.insert(c.clone(), Some(f.clone())).is_none() {
                queue.push_back((c.clone(), depth + 1));
            }
        }
    }
    seen
}

/// `` `a` -> `b` -> `c` `` for the path the walk took to `target`.
fn render_path(reach: &BTreeMap<String, Option<String>>, target: &str) -> String {
    let mut chain = vec![target.to_string()];
    let mut cur = target.to_string();
    let mut guard: BTreeSet<String> = BTreeSet::new();
    guard.insert(cur.clone());
    while let Some(prev) = reach.get(&cur).and_then(Clone::clone) {
        if !guard.insert(prev.clone()) {
            break;
        }
        chain.push(prev.clone());
        cur = prev;
    }
    chain.reverse();
    chain
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Split the graph's cycles into self-loops (OB-19) and mutual cycles (OB-20).
///
/// Mutual cycles are reported as STRONGLY CONNECTED COMPONENTS, not enumerated
/// elementary cycles. Enumerating every elementary cycle of an over-approximated
/// graph is exponential — the first draft of this walker was OOM-killed doing
/// exactly that, which would have made the freeze gate itself the hang it
/// exists to prevent. An SCC is linear, and it is the right unit of repair
/// anyway: every cell in one component can reach every other.
fn find_cycles(edges: &[Edge]) -> (Vec<Edge>, Vec<Vec<Edge>>) {
    let mut self_loops: Vec<Edge> = Vec::new();
    let mut nodes: BTreeSet<&str> = BTreeSet::new();
    for e in edges {
        nodes.insert(e.from.as_str());
        nodes.insert(e.to.as_str());
        if e.from == e.to {
            self_loops.push(e.clone());
        }
    }
    let order: Vec<&str> = nodes.iter().copied().collect();
    let index_of: BTreeMap<&str, usize> = order.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for e in edges {
        if e.from != e.to {
            adjacency[index_of[e.from.as_str()]].push(index_of[e.to.as_str()]);
        }
    }

    // Iterative Tarjan: the graph is derived, so its depth is not something a
    // build-blocking gate gets to assume.
    let n = order.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut components: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some((v, child)) = work.pop() {
            if child == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if child < adjacency[v].len() {
                let w = adjacency[v][child];
                work.push((v, child + 1));
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut component = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                if component.len() > 1 {
                    components.push(component);
                }
            }
        }
    }

    // Render each component as the edges INSIDE it, sorted for determinism.
    let mut out: Vec<Vec<Edge>> = Vec::new();
    for component in components {
        let members: BTreeSet<&str> = component.iter().map(|i| order[*i]).collect();
        let mut inner: Vec<Edge> = edges
            .iter()
            .filter(|e| {
                e.from != e.to
                    && members.contains(e.from.as_str())
                    && members.contains(e.to.as_str())
            })
            .cloned()
            .collect();
        inner.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
        if !inner.is_empty() {
            out.push(inner);
        }
    }
    out.sort_by(|a, b| a[0].from.cmp(&b[0].from));
    (self_loops, out)
}

/// Drive the census over synthetic `(path, source)` pairs — the seam the red
/// fixtures use, so a demonstration exercises the SAME derivation and the SAME
/// verdict logic the build gate runs, not a lookalike.
#[cfg(test)]
fn run_synth_sources(sources: &[(String, String)]) -> CensusOutcome {
    let derived = derive_from_sources(sources);
    report("<synthetic>", sources.len(), &derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crates/aterm-census lives two levels under the aterm root")
            .to_path_buf()
    }

    fn src(path: &str, body: &str) -> (String, String) {
        (path.to_string(), body.to_string())
    }

    /// The tree as it stands must be clean — this census is only worth having
    /// if GREEN means something on the real sources.
    #[test]
    fn lazy_init_census_is_green_on_this_tree() {
        let out = run_lazy_init_census(&repo_root());
        assert!(
            out.ok,
            "lazy-init reentrancy census RED on the current tree:\n{}",
            out.log
        );
    }

    /// GREEN must not be vacuous: the walk has to actually SEE the idiom on
    /// this tree, or OB-21 is the only thing standing between a renamed
    /// construct and a silently blind gate.
    #[test]
    fn the_real_tree_derivation_is_not_empty() {
        let files = scan_files(&repo_root());
        let derived = derive_from_sources(&read_sources(&repo_root(), &files));
        assert!(
            files.len() > 500,
            "scan set collapsed: {} file(s)",
            files.len()
        );
        assert!(
            derived.cells.len() > 20,
            "too few lazy cells derived ({}) — the walker has gone blind",
            derived.cells.len()
        );
        assert!(!derived.inits.is_empty() && !derived.touches.is_empty());
    }

    /// The v0.65.0 / v0.66.0 source of `debug_seamless_reexec_armed`, verbatim
    /// from `crates/aterm-gui/src/app_update_screen.rs:51` at tag v0.65.0 —
    /// fn-local `OnceLock`, multi-statement closure, the self-call first — plus
    /// the main-thread caller that reached it on every apply.
    ///
    /// Hoisted OUT of the fixture body on purpose: `xtask`'s non-vacuity
    /// checker bounds a registered fixture at the first `\n    }` after its
    /// `fn` line, and this snippet's own `    })` would truncate the body
    /// before the assertions, making a real red fixture read as no fixture.
    const V065_SHIPPED_SHAPE: &str = r#"
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let on = crate::app_update_screen::debug_seamless_reexec_armed();
        if on {
            aterm_log::warn!("the QA seam is armed");
        }
        on
    })
}

pub(crate) fn apply_staged_update_now() -> bool {
    let debug_seamless = debug_seamless_reexec_armed();
    debug_seamless
}
"#;

    /// THE RED FIXTURE (non-vacuity): the EXACT shape that shipped in v0.65.0
    /// and v0.66.0 and froze the terminal on its first automatic update apply.
    #[test]
    fn the_v065_self_recursive_once_lock_is_red_with_its_path() {
        let sources = vec![src(
            "crates/aterm-gui/src/app_update_screen.rs",
            V065_SHIPPED_SHAPE,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "the shipped freeze shape must be RED:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
        assert!(
            out.log.contains("`ARMED`"),
            "must name the cell:\n{}",
            out.log
        );
        assert!(
            out.log.contains("debug_seamless_reexec_armed"),
            "must name the accessor the initializer calls back into:\n{}",
            out.log
        );
        assert!(
            out.log.contains("gate lazyinit: FAILED"),
            "the verdict line the build gate greps for is missing:\n{}",
            out.log
        );
    }

    /// …and the SHIPPED REPAIR is green. Same file, same cell, the initializer
    /// reading the environment instead of itself.
    #[test]
    fn the_shipped_repair_of_that_site_is_green() {
        let sources = vec![src(
            "crates/aterm-gui/src/app_update_screen.rs",
            r#"
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let on = std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some();
        if on {
            aterm_log::warn!("the QA seam is armed");
        }
        on
    })
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(out.ok, "the shipped repair must be GREEN:\n{}", out.log);
    }

    /// OB-20: two accessors that initialise through each other. No self-loop
    /// anywhere, and still a permanent park for whichever thread arrives first.
    #[test]
    fn a_mutual_pair_is_red_as_one_component() {
        let sources = vec![src(
            "crates/aterm-gui/src/pair.rs",
            r#"
fn alpha() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(|| beta())
}

fn beta() -> bool {
    static B: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *B.get_or_init(|| alpha())
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(!out.ok, "a mutual pair must be RED:\n{}", out.log);
        assert!(
            out.log.contains("[OB-20]"),
            "wrong obligation:\n{}",
            out.log
        );
        assert!(
            out.log.contains('A') && out.log.contains('B'),
            "both cells must be named:\n{}",
            out.log
        );
    }

    /// The SUPPORTED escape is not a finding: `get()` cannot park, so an
    /// initializer that peeks at its own cell through it is fine. Flagging
    /// this would push authors away from the repair the census recommends.
    #[test]
    fn a_non_blocking_get_back_edge_is_not_a_finding() {
        let sources = vec![src(
            "crates/aterm-gui/src/peek.rs",
            r#"
fn armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let previously = ARMED.get().copied().unwrap_or(false);
        previously || std::env::var_os("KNOB").is_some()
    })
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(out.ok, "a `get()` peek must stay GREEN:\n{}", out.log);
    }

    /// A `LazyLock` has no accessor method to look for — EVERY use derefs it,
    /// so the reader that mentions it is the trigger.
    #[test]
    fn a_deref_lazy_cell_reentered_through_a_reader_is_red() {
        let sources = vec![src(
            "crates/aterm-gui/src/table.rs",
            r#"
static TABLE: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| build());

fn build() -> Vec<u8> {
    seed()
}

fn seed() -> Vec<u8> {
    TABLE.clone()
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(!out.ok, "a deref-lazy re-entry must be RED:\n{}", out.log);
        assert!(
            out.log.contains("`TABLE`"),
            "must name the cell:\n{}",
            out.log
        );
    }

    /// OB-21 is what keeps GREEN honest: nothing to scan is not the same as
    /// nothing to find.
    #[test]
    fn an_empty_tree_fails_ob21_rather_than_passing_vacuously() {
        let out = run_synth_sources(&[]);
        assert!(
            !out.ok,
            "an empty derivation must not be GREEN:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-21]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// PRECISION REGRESSION, observed on the real tree the first time this
    /// census ran: `unsafe extern "C" { fn f(..) -> T; }` declares a fn with no
    /// body. Segmenting it as if it had one let it absorb the NEXT function's
    /// body, and `keymap.rs`'s `connection()` — which does own a `OnceLock` —
    /// came out as a phantom self-reentrancy. A declaration must own no lines.
    #[test]
    fn an_extern_declaration_does_not_absorb_the_next_fn_body() {
        let sources = vec![src(
            "crates/aterm-gui/src/keymap.rs",
            r#"
unsafe extern "C" {
    fn IOServiceMatching(name: *const u8) -> *mut u8;
    fn IOServiceOpen(
        service: u32,
        task: u32,
    ) -> i32;
}

fn connection() -> Option<u32> {
    static CONN: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *CONN.get_or_init(|| unsafe { open_port(IOServiceMatching(b"x\0".as_ptr())) })
}

fn open_port(dict: *mut u8) -> Option<u32> {
    let _ = dict;
    None
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            out.ok,
            "an FFI declaration must not fabricate a reentrancy finding:\n{}",
            out.log
        );
    }

    /// A QUALIFIED constructor reference (`CELL.get_or_init(Type::new)`) must
    /// not be followed: its last segment is the merged `new` super-node, and
    /// following it reported `TABLES` in aterm-core as self-reentrant on the
    /// first run of this census. Real shape, kept as a fixture.
    #[test]
    fn a_qualified_ctor_reference_is_not_followed() {
        let sources = vec![
            src(
                "crates/aterm-core/src/terminal/color_resolve.rs",
                r#"
fn transfer_tables() -> &'static TransferTables {
    static TABLES: std::sync::OnceLock<TransferTables> = std::sync::OnceLock::new();
    TABLES.get_or_init(TransferTables::new)
}
"#,
            ),
            src(
                "crates/aterm-core/src/other.rs",
                r#"
fn new() -> u8 {
    transfer_tables_reader()
}

fn transfer_tables_reader() -> u8 {
    0
}
"#,
            ),
        ];
        let out = run_synth_sources(&sources);
        assert!(
            out.ok,
            "a qualified ctor reference must not be walked into:\n{}",
            out.log
        );
    }

    /// An UNQUALIFIED fn reference passed instead of a closure IS the
    /// initializer, and must be followed — otherwise the commonest terse
    /// spelling of this hazard is invisible.
    #[test]
    fn an_unqualified_fn_reference_initializer_is_followed() {
        let sources = vec![src(
            "crates/aterm-gui/src/terse.rs",
            r#"
fn rules() -> &'static Rules {
    static RULES: std::sync::OnceLock<Rules> = std::sync::OnceLock::new();
    RULES.get_or_init(build_rules)
}

fn build_rules() -> Rules {
    rules().clone()
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "a fn-reference initializer must be walked:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// `#[cfg(test)]` code never ships, and a deadlock there stops a suite
    /// rather than a terminal — so it is masked out, not reported.
    #[test]
    fn a_test_only_self_recursion_is_not_a_shipped_finding() {
        let sources = vec![src(
            "crates/aterm-gui/src/gated.rs",
            r#"
#[cfg(test)]
fn probe() -> bool {
    static P: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *P.get_or_init(|| probe())
}

fn live() -> bool {
    static L: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var_os("K").is_some())
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            out.ok,
            "a cfg(test) initializer must not fail the shipped census:\n{}",
            out.log
        );
    }

    /// A `Once` is a cell too: `ONCE.call_once(|| ..)` parks exactly the same
    /// way, and the shipped surface has 15 of them.
    #[test]
    fn a_self_recursive_once_call_once_is_red() {
        let sources = vec![src(
            "crates/aterm-net/src/tls.rs",
            r#"
fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        init_crypto();
    });
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(!out.ok, "a self-recursive Once must be RED:\n{}", out.log);
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// REGRESSION: the first cut of this census only knew `static` cells, so a
    /// `OnceLock` STRUCT FIELD re-entered from its own initializer passed
    /// GREEN. That is not a contrived shape — it is the ordinary way to
    /// memoise per session — and it parks exactly as hard as the shipped
    /// `static` did. Found by attacking this census rather than by reading it.
    #[test]
    fn a_self_recursive_oncelock_field_is_red() {
        let sources = vec![src(
            "crates/demo/src/session.rs",
            r#"
pub struct Session {
    cache: std::sync::OnceLock<bool>,
}

impl Session {
    pub fn armed(&self) -> bool {
        *self.cache.get_or_init(|| self.armed())
    }
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "a self-recursive field cell must be RED:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
        assert!(
            out.log.contains("FIELD"),
            "the diagnostic must say the cell is a field:\n{}",
            out.log
        );
    }

    /// …and an ordinary field cell whose initializer does NOT re-enter is
    /// GREEN. 49 of this tree's 136 cells are fields; a census that reddened
    /// them would be deleted within the hour.
    #[test]
    fn an_ordinary_oncelock_field_is_green() {
        let sources = vec![src(
            "crates/demo/src/session.rs",
            r#"
pub struct Session {
    cache: std::sync::OnceLock<bool>,
    name: String,
}

impl Session {
    pub fn armed(&self) -> bool {
        *self.cache.get_or_init(|| self.name.is_empty())
    }
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            out.ok,
            "an ordinary field cell must stay GREEN:\n{}",
            out.log
        );
    }

    /// A BORROWED cell is someone else's — the owner declares it, and treating
    /// a `&OnceLock<T>` parameter as a declaration would register the same cell
    /// once per function that takes one.
    #[test]
    fn a_borrowed_cell_parameter_is_not_a_declaration() {
        assert!(field_cell_name("    cache: std::sync::OnceLock<bool>,").is_some());
        assert!(field_cell_name("    cell: &OnceLock<T>,").is_none());
        assert!(field_cell_name("    let cache: OnceLock<bool> = OnceLock::new(),").is_none());
        assert!(field_cell_name("    pub(crate) cache: OnceLock<bool>,").is_some());
        assert!(field_cell_name("    pub cache: OnceLock<bool>,").is_some());
        assert!(field_cell_name("    name: String,").is_none());
    }

    /// THE WORST ONE THE ADVERSARIAL AUDIT FOUND, and the reason this walker
    /// scans the whole file instead of line by line: rustfmt wraps at 100
    /// columns, so the incident's own shape with identifiers only slightly
    /// longer than the ones that shipped puts the receiver and the blocking
    /// method on DIFFERENT LINES. The line-oriented first cut passed it GREEN
    /// — meaning `cargo fmt` could turn a red tree green.
    #[test]
    fn a_receiver_the_formatter_wrapped_onto_its_own_line_is_still_red() {
        let sources = vec![src(
            "crates/aterm-gui/src/app_update_screen.rs",
            r#"
pub fn debug_seamless_reexec_armed() -> bool {
    static ARMED_SEAMLESS_REEXEC_FLAG: std::sync::OnceLock<bool> =
        std::sync::OnceLock::new();
    *ARMED_SEAMLESS_REEXEC_FLAG
        .get_or_init(|| crate::app_update_screen::debug_seamless_reexec_armed())
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "a wrapped receiver must still be RED:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// A visibility prefix is not a disguise. `pub(crate) static CELL:
    /// OnceLock<..>` was not recognised as a cell AT ALL — `static_name`
    /// stripped only a bare `static `, so the census did not merely miss the
    /// cycle, it never saw the cell.
    #[test]
    fn a_pub_static_cell_is_still_a_cell() {
        for vis in ["pub ", "pub(crate) ", "pub(super) ", ""] {
            let sources = vec![src(
                "crates/demo/src/lib.rs",
                &format!(
                    r#"
{vis}static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn armed() -> bool {{
    *ARMED.get_or_init(|| armed())
}}
"#
                ),
            )];
            let out = run_synth_sources(&sources);
            assert!(
                !out.ok,
                "a `{vis}static` cell must be seen and its cycle caught:\n{}",
                out.log
            );
        }
    }

    /// An array length in a return type is not the end of a signature.
    /// `fn table() -> [f32; 256] {` reached its `;` and was read as a body-less
    /// declaration, which deleted the whole function from the call graph — so
    /// a cell it re-entered went GREEN. (The first fix for this counted `<`
    /// and `>` as brackets too, which is worse: the `>` of `->` fires on every
    /// signature in the workspace.)
    #[test]
    fn an_array_length_in_a_signature_does_not_delete_the_function() {
        let sources = vec![src(
            "crates/demo/src/lut.rs",
            r#"
static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();

pub fn table() -> &'static [f32; 256] {
    TABLE.get_or_init(|| build())
}

fn build() -> [f32; 256] {
    let t = table();
    *t
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "an array-typed accessor must not vanish from the graph:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// A real body-less declaration still owns no lines — the array fix must
    /// not have re-broken the `extern "C"` case the other regression pins.
    #[test]
    fn the_depth_rule_still_treats_a_real_declaration_as_bodyless() {
        let lines = vec![
            "unsafe extern \"C\" {",
            "    fn IOServiceMatching(name: *const u8) -> *mut u8;",
            "}",
            "fn real() -> [u8; 4] {",
            "    [0; 4]",
            "}",
        ];
        assert!(
            is_bodyless_declaration(&lines, 1),
            "an extern decl is bodyless"
        );
        assert!(
            !is_bodyless_declaration(&lines, 3),
            "an array-returning fn has a body"
        );
    }

    /// A `;` inside a static's TYPE is not the end of its declaration.
    /// `declaration_text` stopped at the first `;` anywhere, so
    /// `static LUT: LazyLock<[f32; 256]> =` ended on its own first line and the
    /// wrapped `LazyLock::new(..)` beneath it was never found — the cell
    /// entered the graph with NO initializer, which cannot cycle. Six of the
    /// ten shipping deref-lazy cells were in exactly that state.
    #[test]
    fn a_wrapped_ctor_after_a_semicolon_in_the_type_is_still_found() {
        let sources = vec![src(
            "crates/demo/src/lut.rs",
            r#"
static SRGB_TO_LINEAR_LUT: std::sync::LazyLock<[f32; 256]> =
    std::sync::LazyLock::new(|| build());

fn build() -> [f32; 256] {
    *SRGB_TO_LINEAR_LUT
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "a wrapped LazyLock ctor must still be found and its cycle caught:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// TURBOFISH. `CELL.get_or_init::<_>(|| ..)` is ordinary Rust — the closure
    /// type is inferrable, so nothing stops an author writing it — and matching
    /// the literal `.get_or_init(` missed the touch AND its initializer, so a
    /// self-recursive cell spelled that way passed the mandatory gate GREEN.
    /// Found by `codex review`, not by the six-adversary sweep.
    #[test]
    fn a_turbofished_blocking_call_is_still_a_touch() {
        let sources = vec![src(
            "crates/demo/src/lib.rs",
            r#"
static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn armed() -> bool {
    *ARMED.get_or_init::<_>(|| armed())
}
"#,
        )];
        let out = run_synth_sources(&sources);
        assert!(
            !out.ok,
            "a turbofished get_or_init must be RED:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-19]"),
            "wrong obligation:\n{}",
            out.log
        );
    }

    /// …and the name must still END at the method: `get_or_init_with` is its own
    /// entry in the vocabulary and must not be matched as `get_or_init` with a
    /// stray suffix, or the initializer span would start at the wrong paren.
    #[test]
    fn a_longer_method_name_is_not_matched_as_a_shorter_one() {
        let hay = "CELL.get_or_init_with(|| 1)";
        assert!(find_method_call(hay, "get_or_init", 0).is_none());
        assert!(find_method_call(hay, "get_or_init_with", 0).is_some());
        // Turbofish and whitespace both tolerated, on the right name.
        assert!(find_method_call("C.call_once::<F>(f)", "call_once", 0).is_some());
        assert!(find_method_call("C.get_or_init ( f )", "get_or_init", 0).is_some());
        // A mention that is not a call is not a call.
        assert!(find_method_call("// see .get_or_init above", "get_or_init", 0).is_none());
    }
}
