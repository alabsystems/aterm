// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lazy-init reentrancy census (OB-19..OB-21, [`BLOCKING_METHODS`]).

use crate::{CensusOutcome, Floor};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "lazy_init_calls.rs"]
mod calls;
use calls::{CallScope, known_free_exports};

/// This walker's limits, printed after every red verdict.
pub const LAZY_INIT_PRECISION_NOTE: &str =
    "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL: statics retain distinct file/declaration/enclosing-function
        identities. An unqualified proved-local static selects that declaration;
        uncertain imported globals and field receivers retain conservative candidates.
        Dot calls stay in the method domain; unproved associated/module paths may
        name either domain. Impl and trait bodies use the existing bounded item parser.
        A Cargo alias selects a free export only when its source binding is proved.
      - REACH IS DELIBERATELY SHALLOW: direct initializer calls are resolved across
        the scan set. The initializer file and directly called providers contribute
        bounded same-file helper effects; additional cross-file chains and dynamic
        dispatch remain outside this walk. Ambiguous names limit deeper reach.
        Direct cell-value arguments retain their originating call through the same
        bounded helper walk. Transitive parameter substitution is NOT proved:
        even an ignored root borrow may conservatively reach a deeper helper.
        Such a candidate refuses without claiming a proven runtime cycle.
        Aliases, arbitrary call-result values and dynamic containers need
        type/dataflow analysis outside this lexical census.
      - CELLS ARE FOUND TWO WAYS: static lazy declarations, including fn-local
        statics, and struct fields written with a trailing comma. Fields retain
        a conservative field-name domain. They contribute blocking method/helper
        effects, never an unconditional blocking touch from a bare field mention.
        A LazyLock field re-entered only by a plain deref is not caught.
      - SCOPE: non-test sources of every workspace member (`crates/*/src`).
        `tools/*/src` and the sibling workspaces are NOT censused — they are
        dev/host code outside the shipped binary — so a lazy cell there is
        unguarded. This crate's own sources are excluded as well, because its
        red fixtures quote whole Rust programs. Within the scanned set,
        `#[cfg(test)]` items are blanked: test-only code never ships, and a
        deadlock in a test is a stopped suite, not a frozen terminal. Cells
        declared INSIDE a fn body are covered (the shipped bug was one).
      - NOT flagged: `OnceLock::get`, `::set`, `Once::is_completed` and other
        NON-blocking touches. Re-entering a cell through `get()` is the
        supported escape from this hazard, not an instance of it.
";

/// Type tokens that mark a `static` as a lazily-initialised cell whose every use derefs it.
const DEREF_LAZY_TYPES: &[&str] = &["LazyLock", "Lazy<", "SyncLazy"];

/// Type tokens whose blocking surface is exactly [`BLOCKING_METHODS`].
const METHOD_LAZY_TYPES: &[&str] = &["OnceLock", "OnceCell", "SyncOnceCell", "Once"];

/// The methods that can park the calling thread inside a lazy cell.
const BLOCKING_METHODS: &[&str] = &[
    "get_or_init",
    "get_or_try_init",
    "get_or_init_with",
    "call_once",
    "call_once_force",
];

/// The constructors of a deref-lazy cell.
const LAZY_CTORS: &[&str] = &["LazyLock::new", "Lazy::new", "SyncLazy::new"];

/// Max same-file callee depth from an initializer ([`LAZY_INIT_PRECISION_NOTE`]).
const MAX_LOCAL_DEPTH: usize = 4;

/// Max definitions across the scan set before a callee name is treated as ambiguous.
const AMBIGUOUS_DEFS: usize = 3;

/// A lazily-initialised cell found at its `static` declaration.
#[derive(Debug, Clone)]
struct Cell {
    /// Declaration identity, separate from its display spelling.
    id: String,
    file: String,
    owner: Option<String>,
    /// The static's identifier.
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
    /// The enclosing fn, if the touch is inside one.
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
    /// Direct callees, with method and free-function domains kept separate.
    callees: BTreeSet<String>,
    /// Cells this initializer blockingly touches in its own text (depth 0).
    direct_touches: BTreeSet<String>,
    /// Direct cell-value arguments, keyed by the initializer call receiving them.
    borrowed_calls: BTreeMap<String, BTreeSet<String>>,
}

/// One edge of the lazy-init graph, with the witness that produced it.
#[derive(Debug, Clone)]
struct Edge {
    from: String,
    to: String,
    /// Root-borrow association through helpers without parameter substitution.
    conservative: bool,
    /// The rendered witness: where the initializer is and how it gets back.
    path: String,
}

/// Run the lazy-init reentrancy census over an aterm checkout at `root`.
#[must_use]
pub fn run_lazy_init_census(root: &Path) -> CensusOutcome {
    let files = scan_files(root);
    let sources = read_sources(root, &files);
    let dirs: Vec<&str> = sources
        .iter()
        .filter_map(|(file, _)| source_crate(file))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let scopes: DependencyScopes = par_map(&dirs, |dir| {
        crate::scan_set::member_dependency_aliases(root, dir)
            .ok()
            .map(|aliases| ((*dir).to_owned(), aliases))
    })
    .into_iter()
    .flatten()
    .collect();
    let derived = derive_with_scopes(&sources, &scopes);
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
        d.ambiguities.len()
    );

    let mut failures = 0usize;

    if file_count == 0 || d.cells.is_empty() || d.inits.is_empty() || d.touches.is_empty() {
        failures += 1;
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-21] the census derived {file_count} file(s), {} cell(s), {} \
             initializer(s) and {} blocking touch(es). At least one of those is ZERO, so \
             this census is BLIND, not clean. Either the scan root moved, or the \
             lazy-init idiom was renamed (a wrapper type, a macro) — in which case teach \
             the walker the new spelling in `DEREF_LAZY_TYPES` / `METHOD_LAZY_TYPES` / \
             `BLOCKING_METHODS`. Do NOT delete this obligation to make the census pass: a \
             blind census is exactly how the v0.65.0 freeze shipped.",
            d.cells.len(),
            d.inits.len(),
            d.touches.len()
        );
    }

    let edges = derive_edges(d);
    let (self_loops, components) = find_cycles(&edges);

    for edge in &self_loops {
        failures += 1;
        let cell = d.cell(&edge.from);
        if edge.conservative {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-19] POTENTIALLY SELF-REENTRANT lazy init: `{}` ({}).\n    {}\n    REFUSAL: the root cell borrow is conservatively associated with a deeper blocking helper. Parameter substitution is unresolved; this is not proof of runtime recursion.",
                cell.map_or(edge.from.as_str(), |cell| cell.name.as_str()),
                cell.map_or("unknown declaration", |cell| cell.span.as_str()),
                edge.path
            );
            continue;
        }
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
            cell.map_or(edge.from.as_str(), |cell| cell.name.as_str()),
            cell.map_or("lazy cell".to_string(), |c| {
                if c.field {
                    format!("{} FIELD", c.kind)
                } else {
                    c.kind.clone()
                }
            }),
            cell.map_or("<unknown>", |c| c.span.as_str()),
            cell.map_or(edge.from.as_str(), |cell| cell.name.as_str()),
            edge.path
        );
    }

    for component in &components {
        failures += 1;
        let members: BTreeSet<&str> = component.iter().map(|e| e.from.as_str()).collect();
        let rendered = members
            .iter()
            .map(|id| {
                d.cell(id).map_or_else(
                    || (*id).to_owned(),
                    |cell| format!("`{}` ({})", cell.name, cell.span),
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let potential = component.iter().any(|edge| edge.conservative);
        let qualifier = if potential { "POTENTIALLY " } else { "" };
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-20] {qualifier}MUTUALLY-REENTRANT lazy init: {{{rendered}}} is a strongly \
             connected component of the lazy-init graph — each of these cells can reach a \
             blocking touch of the others."
        );
        for e in component {
            let display = |id: &str| {
                d.cell(id).map_or_else(
                    || id.to_owned(),
                    |cell| format!("`{}` ({})", cell.name, cell.span),
                )
            };
            let _ = writeln!(
                log,
                "\x20     {} -> {}: {}",
                display(&e.from),
                display(&e.to),
                e.path
            );
        }
        if potential {
            let _ = writeln!(
                log,
                "    REFUSAL: a conservative root-borrow association participates in this cycle. Transitive parameter substitution is unresolved; this is not proof of runtime recursion."
            );
            continue;
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
             all non-empty)",
            edges.len(),
        );
        let summary = format!(
            "{} lazy cell(s) over {file_count} file(s), no reentrancy cycle.",
            d.cells.len()
        );
        let missed = "the walk lost most of the tree (a moved scan root or a renamed \
                      idiom), so OB-19..OB-21 hold vacuously; teach the walker the new \
                      spelling";
        CensusOutcome::green(LAZY_INIT, log, summary).with_floors(vec![
            Floor::at_least("non-test source files read", file_count, 500, missed),
            Floor::at_least("lazy cells derived", d.cells.len(), 20, missed),
        ])
    } else {
        log.push_str(LAZY_INIT_PRECISION_NOTE);
        CensusOutcome::red(
            LAZY_INIT,
            log,
            format!(
                "{failures} obligation violation(s). There is NO waiver channel for a \
                 reentrancy cycle: it can only be fixed."
            ),
        )
    }
}

/// The lazy-init census's verdict title.
const LAZY_INIT: &str = "lazy-init census";

/// Everything the walk derived from one tree.
struct Derived {
    cells: Vec<Cell>,
    inits: Vec<Initializer>,
    touches: Vec<Touch>,
    /// Actual declaration membership; file identity is never discarded at a join.
    defs: BTreeSet<(String, String)>,
    /// The existing corpus-wide ambiguity boundary for body traversal.
    ambiguities: BTreeMap<String, usize>,
    borrowed_triggers: BTreeMap<String, BTreeSet<String>>,
    /// file -> (fn name -> the names it calls directly), for the local walk.
    local: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    /// cell -> the fns that blockingly touch it (its trigger entry points).
    triggers: BTreeMap<String, BTreeSet<(String, String)>>,
    /// (cell, actual source file, trigger fn) -> the representative touch span.
    trigger_spans: BTreeMap<(String, String, String), String>,
}

impl Derived {
    fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.id == name)
    }
}

/// Non-test `*.rs` under every workspace member's `src/` (`members = ["crates/*"]`).
fn scan_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = crate::tree::read_dir(&root.join("crates")) else {
        return files;
    };
    let mut dirs: Vec<PathBuf> = entries
        .into_iter()
        .flatten()
        .map(|p| p.join("src"))
        .filter(|p| crate::tree::is_dir(p))
        .collect();
    dirs.sort();
    for dir in dirs {
        let _ = crate::collect_rs_files(&dir, &mut files);
    }
    files.retain(|p| !crate::is_test_file(p));
    files.retain(|p| !crate::slash(p).contains(SELF_EXCLUDED_DIR));
    files.sort();
    files
}

/// This crate's own sources — excluded from the scan; see [`scan_files`].
const SELF_EXCLUDED_DIR: &str = "crates/aterm-census/src";

/// Read each source once, as written ([`derive_from_sources`]).
fn read_sources(root: &Path, files: &[PathBuf]) -> Vec<(String, String)> {
    par_map(files, |path| {
        crate::tree::read_to_string(path)
            .ok()
            .map(|raw| (crate::rel_path(root, path), raw))
    })
    .into_iter()
    .flatten()
    .collect()
}

/// `f` of every item, in `items`' order. Scoped worker threads beside this one take the
/// items one at a time, so a few huge files do not leave the other workers idle; each carries
/// this thread's memo scope ([`crate::memo::threads`]), so what it reads is the census unit's
/// question.
fn par_map<'s, T: Sync, R: Send>(items: &'s [T], f: impl Fn(&'s T) -> R + Sync) -> Vec<R> {
    let workers = aterm_cores::available_parallelism().map_or(4, |n| n.get().min(16));
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let at = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(at) else {
                return done;
            };
            done.push((at, f(item)));
        }
    };
    let mut done: Vec<(usize, R)> = crate::memo::threads(|scope| {
        let helpers: Vec<_> = (1..workers.min(items.len()))
            .filter_map(|_| {
                scope
                    .spawn_named("census-lazy-init-worker".to_string(), 8 << 20, work)
                    .ok()
            })
            .collect();
        let mut done = work();
        for helper in helpers {
            done.extend(
                helper
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
        done
    });
    done.sort_unstable_by_key(|(at, _)| *at);
    done.into_iter().map(|(_, mapped)| mapped).collect()
}

/// The derivation proper, over `(repo-relative path, raw text)` pairs.
#[cfg(test)]
fn derive_from_sources(raw_sources: &[(String, String)]) -> Derived {
    derive_with_scopes(raw_sources, &DependencyScopes::new())
}

type DependencyScopes = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

fn source_crate(file: &str) -> Option<&str> {
    file.split_once("/src/")
        .map(|(dir, _)| dir)
        .filter(|dir| dir.starts_with("crates/"))
}

fn method_fn(name: &str) -> String {
    format!("method::{name}")
}

fn owned_fn(dir: &str, name: &str) -> String {
    format!("{dir}::{name}")
}

/// What one source contributes before any other file is read: everything but the touches,
/// which need every file's cells.
struct FileWalk<'a> {
    rel: &'a str,
    fns: Enclosing,
    blocks: Vec<Vec<(usize, usize)>>,
    scope_code: Vec<String>,
    calls: CallScope<'a>,
    /// fn name (and its crate-owned name) -> the names it calls directly.
    local: BTreeMap<String, BTreeSet<String>>,
    /// The `static` cells declared here, in line order, before [`add_cells`] merges them.
    statics: Vec<Cell>,
    /// The field cells declared here, in line order, before [`add_cells`] merges them.
    fields: Vec<Cell>,
}

impl<'a> FileWalk<'a> {
    fn of(
        rel: &'a str,
        text: &'a str,
        scopes: &DependencyScopes,
        exports: &'a BTreeMap<String, BTreeSet<String>>,
    ) -> Self {
        let scope_code: Vec<String> = crate::lex::blank_non_code(text)
            .lines()
            .map(str::to_owned)
            .collect();
        let fns = enclosing_fns(&scope_code.iter().map(String::as_str).collect::<Vec<_>>());
        let mut blocks = Vec::new();
        let mut stack = Vec::new();
        for (line, code) in scope_code.iter().enumerate() {
            blocks.push(stack.clone());
            for (at, byte) in code.bytes().enumerate() {
                match byte {
                    b'{' => stack.push((line, at)),
                    b'}' => {
                        stack.pop();
                    }
                    _ => {}
                }
            }
        }
        let code: Vec<&str> = scope_code.iter().map(String::as_str).collect();
        let calls = CallScope::new(rel, text, &code, scopes, exports);
        let dir = source_crate(rel);
        let mut local: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for span in &fns.spans {
            let targets = calls.callees(&scope_code[span.first..=span.last]);
            if let Some(dir) = dir {
                local
                    .entry(owned_fn(dir, &span.name))
                    .or_default()
                    .extend(targets.iter().cloned());
            }
            local.entry(span.name.clone()).or_default().extend(targets);
        }
        let mut statics = static_cells(&code, rel);
        for cell in &mut statics {
            let line = cell
                .span
                .rsplit_once(':')
                .and_then(|(_, line)| line.parse::<usize>().ok())
                .unwrap_or(1)
                - 1;
            cell.owner = fns
                .span_at(line)
                .map(|span| format!("{}:{}", span.first, span.name));
        }
        let fields = field_cells(&code, rel);
        Self {
            rel,
            fns,
            blocks,
            scope_code,
            calls,
            local,
            statics,
            fields,
        }
    }
}

fn derive_with_scopes(raw_sources: &[(String, String)], scopes: &DependencyScopes) -> Derived {
    let sources: Vec<(String, String)> = par_map(raw_sources, |(rel, raw)| {
        (rel.clone(), crate::lock_order::mask_cfg_test_items(raw))
    });
    let sources = &sources[..];
    let exports = known_free_exports(sources);
    let mut files: Vec<FileWalk<'_>> = par_map(sources, |(rel, text)| {
        FileWalk::of(rel, text, scopes, &exports)
    });
    let mut cells: Vec<Cell> = Vec::new();
    let mut defs = BTreeSet::new();
    let mut ambiguities: BTreeMap<String, usize> = BTreeMap::new();
    let mut local: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();

    for file in &mut files {
        let dir = source_crate(file.rel);
        let mut seen_here: BTreeSet<&str> = BTreeSet::new();
        for span in &file.fns.spans {
            if seen_here.insert(&span.name) {
                defs.insert((file.rel.to_owned(), span.name.clone()));
                *ambiguities.entry(span.name.clone()).or_default() += 1;
                if let Some(dir) = dir {
                    let owned = owned_fn(dir, &span.name);
                    defs.insert((file.rel.to_owned(), owned.clone()));
                    *ambiguities.entry(owned).or_default() += 1;
                }
            }
        }
        let fns = std::mem::take(&mut file.local);
        let per_file = local.entry(file.rel.to_string()).or_default();
        if per_file.is_empty() {
            *per_file = fns;
        } else {
            for (name, callees) in fns {
                per_file.entry(name).or_default().extend(callees);
            }
        }
        add_cells(
            &mut cells,
            std::mem::take(&mut file.statics),
            std::mem::take(&mut file.fields),
        );
    }

    let deref = DerefNames::of(&cells);
    let mut touches: Vec<Touch> = Vec::new();
    let mut inits: Vec<Initializer> = Vec::new();
    let mut borrowed_by_file = BTreeMap::new();
    for (rel, (file_touches, file_inits, borrowed)) in
        files
            .iter()
            .map(|file| file.rel)
            .zip(par_map(&files, |file| {
                collect_touches_and_inits(file, &cells, &deref)
            }))
    {
        borrowed_by_file.insert(rel.to_owned(), borrowed);
        touches.extend(file_touches);
        inits.extend(file_inits);
    }

    let mut triggers: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    let mut trigger_spans: BTreeMap<(String, String, String), String> = BTreeMap::new();
    for touch in &touches {
        let Some(function) = &touch.in_fn else {
            continue;
        };
        let Some((file, _)) = touch.span.rsplit_once(':') else {
            continue;
        };
        let mut names = vec![function.clone()];
        if let Some(dir) = source_crate(file) {
            names.push(owned_fn(dir, function));
        }
        for name in names {
            triggers
                .entry(touch.cell.clone())
                .or_default()
                .insert((file.to_owned(), name.clone()));
            trigger_spans
                .entry((touch.cell.clone(), file.to_owned(), name))
                .or_insert_with(|| format!("{} ({})", touch.span, touch.how));
        }
    }

    // Keep direct effects in their actual source context. The single bounded
    // traversal below accounts for every helper hop; no pre-expanded summary
    // gets another depth budget or bypasses the ambiguity boundary.
    for (file, functions) in &mut borrowed_by_file {
        if let Some(dir) = source_crate(file) {
            functions.extend(functions.clone().iter().map(|name| owned_fn(dir, name)));
        }
    }
    let borrowed_triggers = borrowed_by_file;

    Derived {
        cells,
        inits,
        touches,
        defs,
        ambiguities,
        borrowed_triggers,
        local,
        triggers,
        trigger_spans,
    }
}

/// Keep each static declaration distinct. Unresolved field receivers retain the
/// existing conservative field-name domain, separate from all static declarations.
fn add_cells(out: &mut Vec<Cell>, statics: Vec<Cell>, fields: Vec<Cell>) {
    out.extend(statics);
    for cell in fields {
        if !out.iter().any(|existing| existing.id == cell.id) {
            out.push(cell);
        }
    }
}

/// `static [mut] NAME: ...` lazy-cell declarations, over a file's comment-stripped lines.
fn static_cells(code: &[&str], rel: &str) -> Vec<Cell> {
    let mut out = Vec::new();
    for (i, line) in code.iter().enumerate() {
        let Some(name) = static_name(line) else {
            continue;
        };
        let decl = declaration_text(code, i);
        let deref_lazy = DEREF_LAZY_TYPES.iter().any(|t| decl.contains(t));
        let method_lazy = METHOD_LAZY_TYPES.iter().any(|t| decl.contains(t));
        if !deref_lazy && !method_lazy {
            continue;
        }
        let kind = DEREF_LAZY_TYPES
            .iter()
            .chain(METHOD_LAZY_TYPES.iter())
            .find(|t| decl.contains(**t))
            .map_or("lazy", |t| t.trim_end_matches('<'))
            .to_string();
        out.push(Cell {
            id: format!("static::{rel}:{}::{name}", i + 1),
            file: rel.to_owned(),
            owner: None,
            name: name.to_string(),
            kind,
            span: format!("{rel}:{}", i + 1),
            deref_lazy,
            field: false,
        });
    }
    out
}

/// Lazy cells declared as struct fields, over a file's comment-stripped lines.
fn field_cells(code: &[&str], rel: &str) -> Vec<Cell> {
    let mut out = Vec::new();
    for (i, line) in code.iter().enumerate() {
        let Some(name) = field_cell_name(line) else {
            continue;
        };
        let kind = DEREF_LAZY_TYPES
            .iter()
            .chain(METHOD_LAZY_TYPES.iter())
            .find(|t| line.contains(**t))
            .map_or("lazy", |t| t.trim_end_matches('<'))
            .to_string();
        out.push(Cell {
            id: format!("field::{name}"),
            file: rel.to_owned(),
            owner: None,
            name,
            kind,
            span: format!("{rel}:{}", i + 1),
            deref_lazy: false,
            field: true,
        });
    }
    out
}

/// `<name>: <LazyType><..>,` as a struct field.
fn field_cell_name(code: &str) -> Option<String> {
    let t = code.trim_end();
    if !t.ends_with(',') {
        return None;
    }
    let t = t.trim_start();
    if t.contains('=') {
        return None;
    }
    for kw in ["let ", "static ", "const ", "fn ", "if ", "match ", "for "] {
        if t.starts_with(kw) {
            return None;
        }
    }
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
fn static_name(code: &str) -> Option<&str> {
    let t = code.trim_start();
    let t = if let Some(rest) = t.strip_prefix("pub(") {
        rest.split_once(") ").map(|(_, after)| after.trim_start())?
    } else {
        t.strip_prefix("pub ").map_or(t, str::trim_start)
    };
    let rest = t.strip_prefix("static ")?;
    let rest = rest.strip_prefix("mut ").unwrap_or(rest);
    let len = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    (!rest[..len].is_empty()).then_some(&rest[..len])
}

/// The `static` declaration starting at `line_idx` of a file's comment-stripped lines, up to
/// and including its terminating `;`.
fn declaration_text(code_lines: &[&str], line_idx: usize) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for code in code_lines.iter().skip(line_idx).take(40) {
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

/// Bind an occurrence to a proved local declaration before considering unknown
/// imported globals or fields. Function-local statics never alias other functions.
fn bound_cells<'a>(
    file: &FileWalk<'_>,
    cells: &'a [Cell],
    text: &str,
    end: usize,
    line: usize,
    name: &str,
) -> Vec<&'a Cell> {
    let before = text[..end].trim_end();
    let prefix = before.strip_suffix(name).unwrap_or("").trim_end();
    let matching = || cells.iter().filter(|cell| cell.name == name);
    if prefix.ends_with('.') {
        return matching().filter(|cell| cell.field).collect();
    }
    if let Some(path) = prefix.strip_suffix("::") {
        let path = path.trim_end();
        let alias = crate::ident_ending_at(path, path.len()).unwrap_or("");
        let owner = if matches!(alias, "crate" | "self" | "super") {
            source_crate(file.rel)
        } else {
            file.calls.dependency(alias)
        };
        return matching()
            .filter(|cell| {
                !cell.field
                    && cell.owner.is_none()
                    && owner.is_none_or(|dir| source_crate(&cell.file) == Some(dir))
            })
            .collect();
    }
    let owner = file
        .fns
        .span_at(line)
        .map(|span| format!("{}:{}", span.first, span.name));
    let column = text[..end].rsplit('\n').next().map_or(0, str::len);
    let mut scope = file.blocks.get(line).cloned().unwrap_or_default();
    if let Some(code) = file.scope_code.get(line) {
        for (at, byte) in code.bytes().enumerate().take(column) {
            match byte {
                b'{' => scope.push((line, at)),
                b'}' => {
                    scope.pop();
                }
                _ => {}
            }
        }
    }
    let mut local: Vec<_> = matching()
        .filter(|cell| {
            if cell.field || cell.file != file.rel || cell.owner.is_none() || cell.owner != owner {
                return false;
            }
            let declaration = cell
                .span
                .rsplit_once(':')
                .and_then(|(_, line)| line.parse::<usize>().ok())
                .unwrap_or(0)
                .saturating_sub(1);
            file.blocks
                .get(declaration)
                .is_some_and(|block| scope.starts_with(block))
        })
        .collect();
    // Static items are visible throughout their enclosing block, including
    // before their declaration. Only the nearest proved enclosing block shadows.
    if let Some(depth) = local
        .iter()
        .filter_map(|cell| {
            cell.span
                .rsplit_once(':')
                .and_then(|(_, line)| line.parse::<usize>().ok())
                .and_then(|line| file.blocks.get(line - 1))
                .map(Vec::len)
        })
        .max()
    {
        local.retain(|cell| {
            cell.span
                .rsplit_once(':')
                .and_then(|(_, line)| line.parse::<usize>().ok())
                .and_then(|line| file.blocks.get(line - 1))
                .is_some_and(|block| block.len() == depth)
        });
        return local;
    }
    let globals: Vec<_> = matching()
        .filter(|cell| !cell.field && cell.file == file.rel && cell.owner.is_none())
        .collect();
    if !globals.is_empty() {
        return globals;
    }
    matching()
        .filter(|cell| cell.field || cell.owner.is_none())
        .collect()
}

fn unresolved_receiver(
    file: &FileWalk<'_>,
    cells: &[Cell],
    text: &str,
    end: usize,
    line: usize,
    name: &str,
) -> bool {
    let before = text[..end].trim_end();
    let prefix = before.strip_suffix(name).unwrap_or("").trim_end();
    if prefix.ends_with('.') || prefix.ends_with("::") {
        return false;
    }
    !bound_cells(file, cells, text, end, line, name)
        .iter()
        .any(|cell| !cell.field && cell.file == file.rel)
}

fn ident_ends<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(name).filter_map(move |(at, _)| {
        let end = at + name.len();
        (text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
            && text[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_'))
        .then_some(end)
    })
}

/// A direct cell-value argument. Derived values returned by arbitrary calls,
/// aliases and dynamic containers require type/dataflow analysis outside this
/// lexical census; they are not inferred from a matching word elsewhere.
fn cell_value_path(mut value: &str) -> Option<&str> {
    loop {
        value = value.trim();
        if let Some(rest) = value.strip_prefix('&') {
            value = rest
                .trim_start()
                .strip_prefix("mut ")
                .unwrap_or(rest)
                .trim_start();
        } else if value.starts_with('(') && value.ends_with(')') {
            value = &value[1..value.len() - 1];
        } else {
            break;
        }
    }
    let path = value;
    let mut rest = value.strip_prefix("::").unwrap_or(value).trim_start();
    loop {
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end == 0
            || !rest
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            return None;
        }
        rest = rest[end..].trim_start();
        if rest.is_empty() {
            return Some(path);
        }
        rest = rest
            .strip_prefix("::")
            .or_else(|| rest.strip_prefix('.'))?
            .trim_start();
    }
}

fn borrowed_call_arguments(
    file: &FileWalk<'_>,
    cells: &[Cell],
    body: &[String],
    line: usize,
    first_column: usize,
) -> BTreeMap<String, BTreeSet<String>> {
    // `body` is a slice of the one whole-source lexical view. Retain its
    // original first-line column so block lookup uses actual source offsets.
    let code = format!("{}{}", " ".repeat(first_column), body.join("\n"));
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (open, byte) in code.bytes().enumerate() {
        if byte != b'(' {
            continue;
        }
        let functions = file.calls.reference(&code[..open]);
        if functions.is_empty() {
            continue;
        }
        let mut depth = 0usize;
        let mut first = open + 1;
        for (relative, byte) in code[open + 1..].bytes().enumerate() {
            let at = open + 1 + relative;
            let boundary = match byte {
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    false
                }
                b')' if depth == 0 => true,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    false
                }
                b',' if depth == 0 => true,
                _ => false,
            };
            if !boundary {
                continue;
            }
            let argument = &code[first..at];
            if let Some(value) = cell_value_path(argument) {
                let end = first + argument.find(value).expect("argument subslice") + value.len();
                let name = crate::ident_ending_at(value, value.len()).unwrap_or("");
                let prefix = value[..value.len() - name.len()].trim_end();
                let value_line = line + code[..end].bytes().filter(|byte| *byte == b'\n').count();
                let targets = bound_cells(file, cells, &code, end, value_line, name);
                for function in &functions {
                    found.entry(function.clone()).or_default().extend(
                        targets
                            .iter()
                            .filter(|cell| !cell.field || prefix.ends_with('.'))
                            .map(|cell| cell.id.clone()),
                    );
                }
            }
            first = at + 1;
            if byte == b')' {
                break;
            }
        }
    }
    found
}

/// Blocking touches and initializers in one file.
fn collect_touches_and_inits(
    file: &FileWalk<'_>,
    cells: &[Cell],
    deref: &DerefNames<'_>,
) -> (Vec<Touch>, Vec<Initializer>, BTreeSet<String>) {
    let FileWalk {
        rel,
        fns: enclosing,
        calls,
        ..
    } = file;
    let mut touches = Vec::new();
    let mut inits = Vec::new();
    let mut borrowed = BTreeSet::new();
    let masked: Vec<&str> = file.scope_code.iter().map(String::as_str).collect();
    let joined = masked.join("\n");
    let line_starts = line_start_index(&joined);
    let line_of = |at: usize| match line_starts.binary_search(&at) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };

    // Each method's `.<method>` occurrences, in order: every place [`find_method_call`] would
    // try, since `.` opens the needle and occurs in it nowhere else, so no two overlap.
    let mut named: [Vec<usize>; BLOCKING_METHODS.len()] = Default::default();
    for (at, _) in joined.match_indices('.') {
        let rest = &joined[at + 1..];
        for (method, found) in BLOCKING_METHODS.iter().zip(&mut named) {
            if rest.starts_with(method) {
                found.push(at);
            }
        }
    }
    for (method, found) in BLOCKING_METHODS.iter().zip(&named) {
        let mut from = 0usize;
        for &at in found {
            if at < from {
                continue;
            }
            let Some(open) = call_open(&joined, at + 1 + method.len()) else {
                continue;
            };
            from = open + 1;
            let ln = line_of(at);
            let Some(recv) = receiver_before(&joined, at) else {
                if let Some(function) = enclosing.at(ln) {
                    borrowed.insert(function.to_owned());
                }
                continue;
            };
            let targets = bound_cells(file, cells, &joined, at, ln, recv);
            if (targets.is_empty() || unresolved_receiver(file, cells, &joined, at, ln, recv))
                && let Some(function) = enclosing.at(ln)
            {
                borrowed.insert(function.to_owned());
            }
            if targets.is_empty() {
                continue;
            }
            let body = balanced(&joined, open);
            for cell in targets {
                touches.push(Touch {
                    cell: cell.id.clone(),
                    in_fn: enclosing.at(ln).map(str::to_string),
                    span: format!("{rel}:{}", ln + 1),
                    how: (*method).to_string(),
                });
                inits.push(Initializer {
                    cell: cell.id.clone(),
                    span: format!("{rel}:{}", ln + 1),
                    file: rel.to_string(),
                    callees: initializer_callees(&body, calls),
                    direct_touches: direct_touches(
                        file,
                        &body,
                        cells,
                        line_of(open),
                        open - line_starts[line_of(open)],
                    ),
                    borrowed_calls: borrowed_call_arguments(
                        file,
                        cells,
                        &body,
                        line_of(open),
                        open - line_starts[line_of(open)],
                    ),
                });
            }
        }
    }

    for (i, code) in masked.iter().enumerate() {
        if !deref.may_mention(code) {
            continue;
        }
        let declares = static_name(code);
        for cell in cells.iter().filter(|c| c.deref_lazy) {
            if !mentions_ident(code, &cell.name) {
                continue;
            }
            if declares == Some(cell.name.as_str()) && cell.span == format!("{rel}:{}", i + 1) {
                let decl = declaration_text(&masked, i);
                for ctor in LAZY_CTORS {
                    let Some(off) = decl
                        .find(ctor)
                        .filter(|&off| decl.as_bytes().get(off + ctor.len()) == Some(&b'('))
                    else {
                        continue;
                    };
                    let before = &decl[..off + ctor.len()];
                    let dl = before.matches('\n').count();
                    let col = before.rsplit('\n').next().map_or(0, str::len);
                    let abs = line_starts.get(i + dl).map_or(0, |s| s + col);
                    let body = balanced(&joined, abs);
                    inits.push(Initializer {
                        cell: cell.id.clone(),
                        span: format!("{rel}:{}", i + 1),
                        file: rel.to_string(),
                        callees: initializer_callees(&body, calls),
                        direct_touches: direct_touches(file, &body, cells, i + dl, col),
                        borrowed_calls: borrowed_call_arguments(file, cells, &body, i + dl, col),
                    });
                }
                continue;
            }
            if !ident_ends(code, &cell.name).any(|end| {
                bound_cells(file, cells, code, end, i, &cell.name)
                    .iter()
                    .any(|bound| bound.id == cell.id)
            }) {
                continue;
            }
            touches.push(Touch {
                cell: cell.id.clone(),
                in_fn: enclosing.at(i).map(str::to_string),
                span: format!("{rel}:{}", i + 1),
                how: "deref".to_string(),
            });
        }
    }
    (touches, inits, borrowed)
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

/// Find the next `.<method>(` after `from`.
fn find_method_call(hay: &str, method: &str, from: usize) -> Option<(usize, usize)> {
    let needle = format!(".{method}");
    let mut cur = from;
    while let Some(rel) = hay[cur..].find(&needle) {
        let at = cur + rel;
        cur = at + needle.len();
        if let Some(open) = call_open(hay, cur) {
            return Some((at, open));
        }
    }
    None
}

/// The `(` that calls the method name ending at `after_name`: the name ends there, and only
/// whitespace and a turbofish stand before the parenthesis.
fn call_open(hay: &str, after_name: usize) -> Option<usize> {
    let bytes = hay.as_bytes();
    if bytes.get(after_name).copied().is_some_and(is_ident_byte) {
        return None;
    }
    let mut p = after_name;
    while bytes
        .get(p)
        .copied()
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        p += 1;
    }
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
    (bytes.get(p) == Some(&b'(')).then_some(p)
}

/// The receiver identifier before the `.` at `at`.
fn receiver_before(joined: &str, at: usize) -> Option<&str> {
    let bytes = joined.as_bytes();
    let mut end = at;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    crate::ident_ending_at(joined, end)
}

/// Names looked up word by word. A word whose length or first byte no name has is turned
/// away before it is hashed.
struct WordSet<'a> {
    names: HashSet<&'a str>,
    /// Bit `n` is set when some name is `n` bytes long, for `n` below 64.
    lengths: u64,
    /// Some name is 64 bytes long or longer.
    long: bool,
    /// Whether some name starts with the byte.
    first: [bool; 256],
}

impl<'a> WordSet<'a> {
    fn new(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut set = Self {
            names: HashSet::new(),
            lengths: 0,
            long: false,
            first: [false; 256],
        };
        for name in names {
            set.insert(name);
        }
        set
    }

    fn insert(&mut self, name: &'a str) {
        match u32::try_from(name.len()) {
            Ok(len) if len < u64::BITS => self.lengths |= 1 << len,
            _ => self.long = true,
        }
        if let Some(&byte) = name.as_bytes().first() {
            self.first[usize::from(byte)] = true;
        }
        self.names.insert(name);
    }

    fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    fn contains(&self, word: &str) -> bool {
        let sized = match u32::try_from(word.len()) {
            Ok(len) if len < u64::BITS => self.lengths & (1 << len) != 0,
            _ => self.long,
        };
        sized
            && word
                .as_bytes()
                .first()
                .is_none_or(|&byte| self.first[usize::from(byte)])
            && self.names.contains(word)
    }
}

/// The deref-lazy cells' names, read once per line instead of once per cell per line.
struct DerefNames<'a> {
    /// Pure `[A-Za-z0-9_]` names matched by [`mentions_ident`].
    words: WordSet<'a>,
    /// Any other name, which only [`mentions_ident`] itself can judge.
    other: Vec<&'a str>,
}

impl<'a> DerefNames<'a> {
    fn of(cells: &'a [Cell]) -> Self {
        let mut names = Self {
            words: WordSet::new([]),
            other: Vec::new(),
        };
        for name in cells.iter().filter(|c| c.deref_lazy).map(|c| &*c.name) {
            if name.bytes().all(is_ident_byte) {
                names.words.insert(name);
            } else {
                names.other.push(name);
            }
        }
        names
    }

    /// Whether `code` can mention any deref-lazy cell ([`mentions_ident`]).
    fn may_mention(&self, code: &str) -> bool {
        self.has_word(code) || self.other.iter().any(|n| mentions_ident(code, n))
    }

    /// Whether a piece of `code` split at every char outside `[A-Za-z0-9_]` is a word name.
    /// Bytes of one non-ASCII char separate no pieces between them.
    fn has_word(&self, code: &str) -> bool {
        let bytes = code.as_bytes();
        let mut start = 0;
        let mut at = 0;
        while at < bytes.len() {
            if is_ident_byte(bytes[at]) {
                at += 1;
                continue;
            }
            if self.words.contains(&code[start..at]) {
                return true;
            }
            at += 1;
            while bytes.get(at).is_some_and(|byte| byte & 0xC0 == 0x80) {
                at += 1;
            }
            start = at;
        }
        self.words.contains(&code[start..])
    }
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

/// The fns whose bodies a file's lines sit in, as [`enclosing_fns`] finds them.
struct Enclosing {
    /// Disjoint spans in line order.
    spans: Vec<FnSpan>,
}

/// One fn's name and the first and last line it covers.
struct FnSpan {
    name: String,
    first: usize,
    last: usize,
}

impl Enclosing {
    fn span_at(&self, line: usize) -> Option<&FnSpan> {
        let next = self.spans.partition_point(|span| span.last < line);
        self.spans.get(next).filter(|span| span.first <= line)
    }
    /// The fn whose body `line` sits in, including its dispatch domain.
    fn at(&self, line: usize) -> Option<&str> {
        self.span_at(line).map(|span| span.name.as_str())
    }
}

/// The fn bodies of a file's lines: every line of a span sits in that fn's body.
fn enclosing_fns(lines: &[&str]) -> Enclosing {
    let mut spans = Vec::new();
    let impls = crate::lock_order::method_owner_spans(&lines.join("\n"));
    let mut i = 0usize;
    while i < lines.len() {
        let Some((indent, name)) = crate::parse_fn_def(lines[i]) else {
            i += 1;
            continue;
        };
        if crate::is_bodyless_declaration(lines, i) {
            i += 1;
            continue;
        }
        let def_line = crate::strip_line_comment(lines[i]);
        let opens = def_line.matches('{').count();
        let single_line = opens > 0 && opens == def_line.matches('}').count();
        let mut end = i;
        if !single_line {
            let mut j = i + 1;
            while j < lines.len() {
                if crate::is_closing_brace_at_indent(lines[j], indent) {
                    end = j;
                    break;
                }
                j += 1;
            }
        }
        let line_number = i + 1;
        let name = if impls
            .iter()
            .any(|span| span.start <= line_number && line_number <= span.end)
        {
            method_fn(&name)
        } else {
            name
        };
        spans.push(FnSpan {
            name,
            first: i,
            last: end,
        });
        i = end + 1;
    }
    Enclosing { spans }
}

/// The text between the `(` at byte offset `at` and its matching `)`.
fn balanced(joined: &str, at: usize) -> Vec<String> {
    let at = joined.floor_char_boundary(at.min(joined.len()));
    let mut depth = 0i32;
    let mut end = joined.floor_char_boundary(joined.len().min(at.saturating_add(40_000)));
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
    joined[at..end].lines().map(str::to_string).collect()
}

/// Direct calls and function-path references in the initializer.
fn initializer_callees(body: &[String], calls: &CallScope<'_>) -> BTreeSet<String> {
    let mut out = calls.callees(body);
    let joined = body.join(" ");
    let inner = joined
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if !inner.contains('|') && !inner.contains('(') && !inner.is_empty() {
        out.extend(calls.reference(inner));
    }
    out
}

/// Cells this initializer text touches blockingly in its own body (depth 0).
fn direct_touches(
    file: &FileWalk<'_>,
    body: &[String],
    cells: &[Cell],
    first_line: usize,
    first_column: usize,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (offset, body_line) in body.iter().enumerate() {
        let aligned = if offset == 0 {
            format!("{}{body_line}", " ".repeat(first_column))
        } else {
            body_line.clone()
        };
        let line = aligned.as_str();
        for method in BLOCKING_METHODS {
            let mut from = 0;
            while let Some((at, open)) = find_method_call(line, method, from) {
                from = open + 1;
                if let Some(recv) = receiver_before(line, at) {
                    out.extend(
                        bound_cells(file, cells, line, at, first_line + offset, recv)
                            .into_iter()
                            .map(|cell| cell.id.clone()),
                    );
                }
            }
        }
        for cell in cells.iter().filter(|cell| cell.deref_lazy) {
            for end in ident_ends(line, &cell.name) {
                out.extend(
                    bound_cells(file, cells, line, end, first_line + offset, &cell.name)
                        .into_iter()
                        .filter(|bound| bound.deref_lazy)
                        .map(|cell| cell.id.clone()),
                );
            }
        }
    }
    out
}

/// `S -> T` whenever `S`'s initializer can reach a blocking touch of `T`.
fn derive_edges(d: &Derived) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for init in &d.inits {
        for target in &init.direct_touches {
            if seen.insert((init.cell.clone(), target.clone())) {
                edges.push(Edge {
                    from: init.cell.clone(),
                    to: target.clone(),
                    conservative: false,
                    path: format!(
                        "initializer at {} touches `{}` directly",
                        init.span,
                        d.cell(target)
                            .map_or(target.as_str(), |cell| cell.name.as_str())
                    ),
                });
            }
        }
        let reach = reachable(d, init);
        for hit in reach.inspected.iter().filter(|(file, function, _)| {
            d.borrowed_triggers
                .get(file)
                .is_some_and(|functions| functions.contains(function))
        }) {
            for target in init.borrowed_calls.get(&hit.2).into_iter().flatten() {
                if seen.insert((init.cell.clone(), target.clone())) {
                    edges.push(Edge { from: init.cell.clone(), to: target.clone(), conservative: true, path: format!("initializer at {} passes a cell value to `{}` and conservatively reaches unresolved blocking helper {}", init.span, hit.2, render_path(&reach, hit)) });
                }
            }
        }
        for (target, trigger_fns) in &d.triggers {
            let Some(hit) = reach.paths.keys().find(|(file, function, _)| {
                trigger_fns.contains(&(file.clone(), function.clone()))
            }) else {
                continue;
            };
            if !seen.insert((init.cell.clone(), target.clone())) {
                continue;
            }
            let touch = d
                .trigger_spans
                .get(&(target.clone(), hit.0.clone(), hit.1.clone()))
                .map_or("?", String::as_str);
            edges.push(Edge {
                from: init.cell.clone(),
                to: target.clone(),
                conservative: false,
                path: format!(
                    "initializer at {} calls {}, which touches `{}` at {touch}",
                    init.span,
                    render_path(&reach, hit),
                    d.cell(target)
                        .map_or(target.as_str(), |cell| cell.name.as_str())
                ),
            });
        }
    }
    edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    edges
}

/// A function name in the file whose body the bounded walk may inspect.
type FunctionSite = (String, String, String);
struct Reach {
    paths: BTreeMap<FunctionSite, Option<FunctionSite>>,
    inspected: BTreeSet<FunctionSite>,
}

/// One original bounded budget for initializer and directly-called provider
/// helpers. A provider's later hops stay in that provider's source context.
fn reachable(d: &Derived, init: &Initializer) -> Reach {
    let mut reach = Reach {
        paths: BTreeMap::new(),
        inspected: BTreeSet::new(),
    };
    let mut queue = VecDeque::new();
    for function in &init.callees {
        let mut providers = Vec::new();
        if d.local
            .get(&init.file)
            .is_some_and(|functions| functions.contains_key(function))
        {
            providers.push(init.file.clone());
        }
        // An unproved qualified call may have flattened to this symbol. A
        // same-file spelling does not prove it is the only possible provider.
        if d.ambiguities.get(function).copied().unwrap_or(0) <= AMBIGUOUS_DEFS {
            providers.extend(
                d.local
                    .iter()
                    .filter(|(_, functions)| functions.contains_key(function))
                    .map(|(file, _)| file.clone()),
            );
        }
        providers.sort();
        providers.dedup();
        if providers.is_empty() {
            providers.push(init.file.clone());
        }
        for file in providers {
            let site = (file, function.clone(), function.clone());
            if reach.paths.insert(site.clone(), None).is_none() {
                queue.push_back((site, 0));
            }
        }
    }
    while let Some((site, depth)) = queue.pop_front() {
        if depth >= MAX_LOCAL_DEPTH
            || d.ambiguities.get(&site.1).copied().unwrap_or(0) > AMBIGUOUS_DEFS
        {
            continue;
        }
        let Some(callees) = d
            .local
            .get(&site.0)
            .and_then(|functions| functions.get(&site.1))
        else {
            continue;
        };
        if !d.defs.contains(&(site.0.clone(), site.1.clone())) {
            continue;
        }
        reach.inspected.insert(site.clone());
        for function in callees {
            let next = (site.0.clone(), function.clone(), site.2.clone());
            if !reach.paths.contains_key(&next) {
                reach.paths.insert(next.clone(), Some(site.clone()));
                queue.push_back((next, depth + 1));
            }
        }
    }
    reach
}

/// `` `a` -> `b` -> `c` `` for the path the walk took to `target`.
fn render_path(reach: &Reach, target: &FunctionSite) -> String {
    let mut chain = vec![target.1.clone()];
    let mut current = target.clone();
    let mut guard = BTreeSet::from([current.clone()]);
    while let Some(previous) = reach.paths.get(&current).and_then(Clone::clone) {
        if !guard.insert(previous.clone()) {
            break;
        }
        chain.push(previous.1.clone());
        current = previous;
    }
    chain.reverse();
    chain
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Split the graph's cycles into self-loops (OB-19) and mutual cycles (OB-20).
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
/// derivation and verdict logic as the live census.
#[cfg(test)]
fn run_synth_sources(sources: &[(String, String)]) -> CensusOutcome {
    let derived = derive_from_sources(sources);
    report("<synthetic>", sources.len(), &derived)
}

#[cfg(test)]
#[path = "lazy_init_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lazy_init_binding_tests.rs"]
mod binding_tests;
