// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lazy-init reentrancy census (OB-19..OB-21): the L0 deadlock class in its
//! reentrancy sense, as a fail-closed, build-blocking obligation.
//!
//! The shape it guards against:
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
//! `Once::call` marks the cell running before invoking the initializer, so the
//! nested call parks the thread on its own `Once` forever. Reached from the
//! winit main thread, that freezes the whole terminal. It is not a lock-order
//! cycle (OB-7), not an unbounded sink (OB-1..OB-6), and clippy has no lint for
//! it. This census derives every lazy cell in `crates/*/src` and which fns
//! block on each, and fails the build if an initializer can reach a blocking
//! touch of its own cell, directly or around a cycle.
//!
//! Consumers: the `aterm-census` bin (`targo --unverified run -p aterm-census
//! -- --lazy-init`) and `tools/freeze-safety-gate/build.rs`.
//!
//! Obligations (`[OB-n]` tags in the diagnostics; none has a waiver):
//!
//! * OB-19 self-reentrancy: no initializer reaches a blocking touch of its own
//!   cell (always a deadlock).
//! * OB-20 mutual reentrancy: the lazy-init graph (`S -> T` when `S`'s
//!   initializer can reach a blocking touch of `T`) is acyclic.
//! * OB-21 liveness: an anti-vacuity floor. It fires only when the tree-wide
//!   count of cells, initializers or blocking touches reaches zero, so it does
//!   not catch a single site hidden behind a wrapper type; per-site coverage
//!   rests on the vocabulary and join rules.
//!
//! Blocking touches are the operations that can park: `get_or_init` /
//! `get_or_try_init` on `OnceLock`/`OnceCell`, `call_once` / `call_once_force`
//! on `Once`, and any use of a `LazyLock`/`Lazy` cell. `OnceLock::get`, `::set`,
//! `::get_mut` and `Once::is_completed` are ignored: re-entering through
//! `get()` is the supported escape.

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
        with `#[cfg(test)]` items blanked: test-only code
        never ships, and a deadlock in a test is a stopped suite, not a frozen
        terminal. Cells declared INSIDE a fn body are covered (the shipped bug
        was one).
      - NOT flagged: `OnceLock::get`, `::set`, `Once::is_completed` and other
        NON-blocking touches. Re-entering a cell through `get()` is the
        supported escape from this hazard, not an instance of it.
";

// Vocabulary.

/// Type tokens that mark a `static` as a lazily-initialised cell whose every
/// use derefs it — so every mention is a blocking touch.
const DEREF_LAZY_TYPES: &[&str] = &["LazyLock", "Lazy<", "SyncLazy"];

/// Type tokens whose blocking surface is exactly [`BLOCKING_METHODS`].
const METHOD_LAZY_TYPES: &[&str] = &["OnceLock", "OnceCell", "SyncOnceCell", "Once"];

/// The methods that can park the calling thread inside a lazy cell: each one
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

/// How far the walk follows same-file callees out of an initializer: enough for
/// helper-calls-helper-calls-accessor while keeping the graph sparse (see
/// [`LAZY_INIT_PRECISION_NOTE`]).
const MAX_LOCAL_DEPTH: usize = 4;

/// A callee name with more than this many definitions across the scan set
/// (`new`, `default`, `len`, …) is too merged to follow; otherwise the merged
/// `new` node would make `TABLES.get_or_init(TransferTables::new)` look
/// self-reentrant. Stated in the precision note.
const AMBIGUOUS_DEFS: usize = 3;

// Derived facts.

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
    /// `true` when the cell is a struct field rather than a `static`.
    field: bool,
}

/// One place a cell is touched in a way that can park the calling thread.
#[derive(Debug, Clone)]
struct Touch {
    cell: String,
    /// The enclosing fn, if the touch is inside one. A touch at item scope
    /// cannot be reached by a call, so it never becomes a graph edge.
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
    /// unqualified function path passed instead of a closure.
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

// Entry point.

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
             lazy-init census: GREEN — {} lazy cell(s) over {file_count} file(s), no \
             reentrancy cycle.",
            edges.len(),
            d.cells.len()
        );
    } else {
        let _ = writeln!(
            log,
            "lazy-init census: FAILED — {failures} obligation violation(s). There is NO \
             waiver channel for a reentrancy cycle: it can only be fixed."
        );
        log.push_str(LAZY_INIT_PRECISION_NOTE);
    }

    if failures == 0 {
        CensusOutcome::green(log)
    } else {
        CensusOutcome::red(log)
    }
}

// Derivation.

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
    // Skip this census's own sources, like `scope_census::SELF_EXCLUDED_DIR`:
    // the red fixtures embed whole Rust programs in raw strings at column 0,
    // which a lexical walker cannot tell from real declarations (and which can
    // end the file's `#[cfg(test)]` mask early).
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
/// [`derive_from_sources`], so synthetic fixtures go through the same masking.
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
    // Test-only items never ship: a deadlock there stops a
    // suite, which is loud, not a terminal, which is silent.
    let sources: Vec<(String, String)> = raw_sources
        .iter()
        .map(|(rel, raw)| (rel.clone(), crate::lock_order::mask_cfg_test_items(raw)))
        .collect();
    let sources = &sources[..];
    let mut cells: Vec<Cell> = Vec::new();
    let mut defs: BTreeMap<String, usize> = BTreeMap::new();
    let mut local: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();

    // Pass 1: every cell and the per-file call graph. All cells must be known
    // before touches are collected, since a deref-lazy cell may be touched in a
    // file read before its declaration.
    for (rel, text) in sources {
        // Attribute calls with this module's segmentation, not
        // `parse_source_fns`, which lacks the body-less-declaration guard: an
        // `extern "C"` block's `fn foo(..) -> T;` would absorb the next fn's
        // body.
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

/// `static [mut] NAME: …<lazy type>… = …;` declarations, wherever they sit —
/// item scope or inside a fn body.
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
            // Same-named cells merge into one node (the documented
            // over-approximation), toward the wider hazard: if either spelling
            // derefs, the merged node derefs.
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

/// Cells that are a struct field rather than a `static`, e.g.
/// `*self.cache.get_or_init(|| self.armed())`, which parks just like a
/// `static`. Touches are found by the identifier before the blocking method,
/// which for `self.cache.get_or_init(..)` is `cache`.
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
            // Field cells are tracked through blocking methods only, never bare
            // mentions: names like `cache` are not unique and would manufacture
            // edges. So a `LazyLock` field re-entered by a bare deref is not
            // caught (recorded in the precision note).
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
    // A borrowed cell is someone else's, passed in; its owner declares it.
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

/// The identifier a `[pub[(..)]] static [mut] NAME…` line declares; the
/// visibility prefix is stripped first.
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
    // The terminator is a `;` at bracket depth 0: a type like
    // `LazyLock<[f32; 256]>` contains one, and the initializer follows on the
    // next line.
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
/// The scan is over the whole file, not line by line: rustfmt can put the
/// receiver and the blocking method on different lines, e.g.
///
/// ```text
///     *ARMED_SEAMLESS_REEXEC_FLAG
///         .get_or_init(|| debug_seamless_reexec_armed())
/// ```
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

/// Find the next `.<method>(` after `from`, tolerating a turbofish between the
/// name and the parenthesis (`CELL.get_or_init::<_>(|| ..)`).
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
/// closing-brace-at-fn-indent invariant, as in the main-loop census). Nested
/// fns and closures resolve to the outermost fn.
fn enclosing_fns(lines: &[&str]) -> Vec<Option<String>> {
    let mut out = vec![None; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        let Some((indent, name)) = crate::parse_fn_def(lines[i]) else {
            i += 1;
            continue;
        };
        // A fn declaration has no body (`extern "C" { fn f(..) -> T; }`, trait
        // signatures); treating one as a body would swallow lines down to the
        // next same-indent `}` and invent callers.
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

/// Whether the fn definition starting at `at` is a body-less declaration: its
/// signature reaches a `;` before it ever reaches a `{`.
fn is_bodyless_declaration(lines: &[&str], at: usize) -> bool {
    // Only a `;` at bracket depth 0 ends a signature: `fn table() -> [f32; 256] {`
    // has one inside its return type.
    let mut depth = 0i32;
    for line in lines.iter().skip(at).take(24) {
        for b in crate::strip_line_comment(line).bytes() {
            match b {
                // Only `()` and `[]`: counting `<`/`>` would let the `>` of
                // `->` decrement on every signature.
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

/// The text between the `(` at byte offset `at` and its matching `)`, as lines,
/// capped at 40 000 bytes.
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

/// Every `<ident>(` in the initializer, plus a function path passed instead of
/// a closure (`CELL.get_or_init(build_it)`).
fn initializer_callees(body: &[String]) -> BTreeSet<String> {
    let mut out = crate::callee_names(body);
    let joined = body.join(" ");
    let inner = joined
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if !inner.contains('|') && !inner.contains('(') && !inner.is_empty() {
        // A qualified path's last segment is followed too; the ambiguity filter
        // in `reachable` drops merged names like `new`, while a unique name
        // such as `Type::with_builtin_rules` is worth following.
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

/// Cells this initializer text touches blockingly in its own body (depth 0).
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

// The lazy-init graph.

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
/// from. Direct callees are taken as written; deeper calls are followed only
/// within `init.file` and never through an over-merged name (see
/// [`LAZY_INIT_PRECISION_NOTE`]).
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
/// Mutual cycles are reported as strongly connected components: enumerating
/// elementary cycles is exponential on an over-approximated graph, while SCCs
/// are linear and are the unit of repair.
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

    // Render each component as the edges inside it, sorted for determinism.
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

/// Drive the census over synthetic `(path, source)` pairs, through the same
/// derivation and verdict logic as the build gate.
#[cfg(test)]
fn run_synth_sources(sources: &[(String, String)]) -> CensusOutcome {
    let derived = derive_from_sources(sources);
    report("<synthetic>", sources.len(), &derived)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_set::test_fixtures::repo_root;

    fn src(path: &str, body: &str) -> (String, String) {
        (path.to_string(), body.to_string())
    }

    /// Green is not vacuous: the walk sees the idiom on this tree.
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

    /// The self-reentrant `debug_seamless_reexec_armed` shape (fn-local
    /// `OnceLock`, multi-statement closure, self-call first) plus its
    /// main-thread caller.
    ///
    /// Hoisted out of the fixture body because `xtask`'s non-vacuity checker
    /// bounds a fixture at the first `\n    }` after its `fn` line, and this
    /// snippet's own `    })` would truncate it.
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

    /// Non-vacuity fixture: the self-reentrant fn-local `OnceLock` shape must
    /// go red.
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
            out.log.contains("lazy-init census: FAILED"),
            "the FAILED verdict line is missing:\n{}",
            out.log
        );
    }

    /// The repair (the initializer reads the environment, not itself) is green.
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

    /// Peeking at the own cell through `get()` cannot park and is not a finding.
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

    /// A `LazyLock` has no accessor method to look for — every use derefs it,
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

    /// A body-less `extern "C"` declaration owns no lines, so it cannot absorb
    /// the next function's body and invent a self-reentrancy.
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

    /// A qualified constructor reference (`CELL.get_or_init(Type::new)`) is not
    /// followed through the merged `new` node.
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

    /// An unqualified fn reference passed instead of a closure is the
    /// initializer and is followed.
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
    /// way.
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

    /// A `OnceLock` struct field re-entered from its own initializer is red.
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

    /// A field cell whose initializer does not re-enter is green.
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

    /// A borrowed `&OnceLock<T>` parameter is not a declaration; the owner
    /// declares the cell.
    #[test]
    fn a_borrowed_cell_parameter_is_not_a_declaration() {
        assert!(field_cell_name("    cache: std::sync::OnceLock<bool>,").is_some());
        assert!(field_cell_name("    cell: &OnceLock<T>,").is_none());
        assert!(field_cell_name("    let cache: OnceLock<bool> = OnceLock::new(),").is_none());
        assert!(field_cell_name("    pub(crate) cache: OnceLock<bool>,").is_some());
        assert!(field_cell_name("    pub cache: OnceLock<bool>,").is_some());
        assert!(field_cell_name("    name: String,").is_none());
    }

    /// The receiver and the blocking method on different lines (as rustfmt
    /// wraps long identifiers) is still red.
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

    /// `pub(crate) static CELL: OnceLock<..>` is recognised as a cell.
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

    /// An array length in a return type (`fn table() -> [f32; 256] {`) does not
    /// end the signature, so the function stays in the call graph.
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

    /// A real body-less declaration still owns no lines.
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

    /// A `;` inside a static's type (`LazyLock<[f32; 256]>`) does not end its
    /// declaration, so the wrapped initializer is found.
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

    /// A turbofish (`CELL.get_or_init::<_>(|| ..)`) is still a touch with an
    /// initializer.
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

    /// The name must end at the method: `get_or_init_with` is its own
    /// vocabulary entry, not `get_or_init` with a suffix.
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
