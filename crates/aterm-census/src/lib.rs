// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The L0 censuses and the source guards: lexical walks over an aterm
//! checkout, each a pure function of the tree that returns a verdict and its
//! transcript.
//!
//! | census      | runner                    | obligations           |
//! |-------------|---------------------------|-----------------------|
//! | `mainloop`  | [`run_mainloop_census`]   | OB-1..OB-6, OB-22     |
//! | `locks`     | [`run_lock_order_census`] | OB-7                  |
//! | `wasm`      | [`run_wasm_census`]       | OB-8..OB-10, OB-12    |
//! | `scope`     | [`run_scope_census`]      | OB-13..OB-16          |
//! | `lazy-init` | [`run_lazy_init_census`]  | OB-19..OB-21          |
//! | `guards`    | [`run_guards`]            | the [`GUARDS`] rules  |
//!
//! `aterm-census` (bare, or `--all`) runs every one over the checkout and
//! exits non-zero on any finding; [`run_all`] is the same in-process, and the
//! `live_tree_*` tests run each over this checkout. All three go through
//! [`run_censuses`], which also holds each clean walk to its [`Floor`]s: a walk
//! that read far less than an aterm checkout holds is blind, not clean.
//!
//! # The main-loop census
//!
//! The hazard: O(session-history) work (a width change rewrapping the whole
//! off-screen scrollback) on the winit event-loop thread while it holds the
//! per-session `term` mutex. The sanctioned shape is the offload
//! (`resize_offloading_scrollback` detaches the tiered store in O(1), a worker
//! rewraps it off the lock, `finish_resize_offload` re-attaches it). The census
//! walks the name-based call graph of `crates/aterm-gui/src` from each
//! main-thread root (the winit `ApplicationHandler` handlers and the resize
//! timer arm), models one `term_lock` hop, and fails, printing root → call path
//! → sink and the repair options, if a root synchronously reaches an unbounded
//! sink not behind an offload boundary.
//!
//! Obligations (`[OB-n]` tags appear in the diagnostics):
//!
//! * OB-1  marker ↔ registry: every `// COST: UNBOUNDED(<dim>)` marker in the
//!   derived GUI-process closure ([`scan_set::derive_gui_scan_set`], workspace
//!   `src/` only) maps to a registered sink, and every registered sink is still
//!   marked. Markers in workspace crates outside the closure are reported, never
//!   registry-checked.
//! * OB-2  every declared main-thread root resolves to a real fn.
//! * OB-3  every [`OFFLOAD_ALLOWLIST`] boundary carries a written justification
//!   and is still defined at its registered file.
//! * OB-4  at least one allowlisted offload-boundary call exists in the GUI
//!   source.
//! * OB-5  no main-thread-reachable fn synchronously calls an unbounded
//!   Terminal method (`resize`) on a `term_lock` guard.
//! * OB-6  no main-thread-reachable fn calls a registered unbounded sink
//!   directly.
//! * OB-22 ([`held_guard`]) the per-site half: no shipped line of
//!   `crates/aterm-gui/src` or `crates/aterm-control/src` does O(history) work
//!   while the `term` guard is held.
//!
//! These are lexical censuses, not a borrow checker; their limits are in
//! [`PRECISION_NOTE`] and [`lock_order::LOCK_PRECISION_NOTE`], printed with the
//! red diagnostics and documented in docs/temporal-safety-gate.md.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod guards;
mod held_guard;
mod lazy_init;
mod lex;
mod lock_order;
pub mod scan_set;
mod scope_census;
mod tree;
mod wasm_census;
mod wasm_cfg;
pub use guards::{Finding, GUARDS, GuardReport, Rule, run_guards};
pub use lazy_init::{LAZY_INIT_PRECISION_NOTE, run_lazy_init_census};
pub use lock_order::run_lock_order_census;
pub use scope_census::{SCOPE_PRECISION_NOTE, run_scope_census};
/// The browser cdylib roots, re-exported for `aterm-forge`'s `[OB-17]` check
/// (`check::ledger_findings`), which requires a measured cell for every crate
/// on this list.
pub use wasm_census::WASM_ROOT_CRATES;
pub use wasm_census::run_wasm_census;

/// A census verdict, its transcript, and the counts its walk saw. The log is
/// returned, not printed, so each consumer decides where it goes.
pub struct CensusOutcome {
    /// `true` iff every obligation held (and, from [`run_censuses`], every
    /// floor).
    pub ok: bool,
    /// The one-line verdict: `<census>: GREEN — …` or `<census>: FAILED — …`.
    pub verdict: String,
    /// The complete transcript; its last line is `verdict`.
    pub log: String,
    /// What a clean walk saw that an aterm checkout keeps in range.
    pub floors: Vec<Floor>,
    title: &'static str,
}

impl CensusOutcome {
    /// A census that found a violation: `log` carries the findings.
    pub(crate) fn red(title: &'static str, log: String, why: impl std::fmt::Display) -> Self {
        Self::finish(false, title, log, format!("{title}: FAILED — {why}"))
    }

    /// A census whose every obligation held.
    pub(crate) fn green(title: &'static str, log: String, summary: impl std::fmt::Display) -> Self {
        Self::finish(true, title, log, format!("{title}: GREEN — {summary}"))
    }

    fn finish(ok: bool, title: &'static str, mut log: String, verdict: String) -> Self {
        log.push_str(&verdict);
        log.push('\n');
        Self {
            ok,
            verdict,
            log,
            floors: Vec::new(),
            title,
        }
    }

    /// Attach the counts [`run_censuses`] holds a clean walk to.
    pub(crate) fn with_floors(mut self, floors: Vec<Floor>) -> Self {
        self.floors = floors;
        self
    }

    /// Turn a clean walk red when a floor is missed. A red walk keeps its own
    /// verdict: its findings already say what is wrong.
    fn hold_floors(&mut self) {
        let missed: Vec<String> = self
            .floors
            .iter()
            .filter(|f| !f.met())
            .map(|f| format!("  ✗ FAIL [floor] {f}\n"))
            .collect();
        if !self.ok || missed.is_empty() {
            return;
        }
        // `finish` made the verdict the log's last line; replace it.
        self.log
            .truncate(self.log.len().saturating_sub(self.verdict.len() + 1));
        self.log.extend(missed.iter().map(String::as_str));
        self.ok = false;
        self.verdict = format!(
            "{}: FAILED — {} floor(s) missed; the walk itself found no violation",
            self.title,
            missed.len()
        );
        self.log.push_str(&self.verdict);
        self.log.push('\n');
    }
}

/// A count a clean walk saw and the range an aterm checkout keeps it in. A
/// walk that reads a fraction of the tree (a renamed idiom, a parser
/// regression) passes every obligation vacuously; a floor makes it red.
/// Held by [`run_censuses`], not by the bare `run_*_census` walks, so the
/// synthetic fixtures stay small.
#[derive(Debug, Clone)]
pub struct Floor {
    /// What was counted.
    pub what: &'static str,
    /// The count this walk saw.
    pub seen: usize,
    /// The inclusive range an aterm checkout keeps it in.
    pub range: std::ops::RangeInclusive<usize>,
    /// What a count outside the range means, and the repair.
    pub missed: &'static str,
}

impl Floor {
    /// At least `min`: the walk still reads the tree it is meant to.
    pub(crate) fn at_least(
        what: &'static str,
        seen: usize,
        min: usize,
        missed: &'static str,
    ) -> Self {
        Self {
            what,
            seen,
            range: min..=usize::MAX,
            missed,
        }
    }

    /// None at all: a ledger that must stay empty.
    pub(crate) fn none(what: &'static str, seen: usize, missed: &'static str) -> Self {
        Self {
            what,
            seen,
            range: 0..=0,
            missed,
        }
    }

    /// Whether the count is in range.
    #[must_use]
    pub fn met(&self) -> bool {
        self.range.contains(&self.seen)
    }
}

impl std::fmt::Display for Floor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (min, max) = (*self.range.start(), *self.range.end());
        let bound = match (min, max) {
            (_, usize::MAX) => format!("at least {min}"),
            (0, 0) => "none".to_string(),
            _ => format!("{min}..={max}"),
        };
        write!(
            f,
            "{}: {} (an aterm checkout shows {bound}) — {}",
            self.what, self.seen, self.missed
        )
    }
}

/// One census: the name the CLI selects it by and its runner.
pub struct Census {
    /// The selection flag without its dashes (`--locks` selects `locks`).
    pub name: &'static str,
    /// What it checks, for `--help`.
    pub about: &'static str,
    /// The census over the checkout at the given root.
    pub run: fn(&Path) -> CensusOutcome,
}

/// Every census, in the order `--all` runs and prints them.
pub const CENSUSES: &[Census] = &[
    Census {
        name: "mainloop",
        about: "main-thread reach to unbounded work (OB-1..OB-6, OB-22)",
        run: run_mainloop_census,
    },
    Census {
        name: "locks",
        about: "lock-order cycles across the GUI process (OB-7)",
        run: run_lock_order_census,
    },
    Census {
        name: "wasm",
        about: "wasm-module reach to unbounded work; threads (OB-8..OB-10, OB-12)",
        run: run_wasm_census,
    },
    Census {
        name: "scope",
        about: "window-scoped enforcers that multiply (OB-13..OB-16)",
        run: run_scope_census,
    },
    Census {
        name: "lazy-init",
        about: "lazy cells re-entered by their own initializer (OB-19..OB-21)",
        run: run_lazy_init_census,
    },
    Census {
        name: "guards",
        about: "source rules no type or lint can hold (aterm_census::GUARDS)",
        run: guards::run_guard_census,
    },
];

/// Run `selected` over the checkout at `root`, each census on its own thread
/// (they share nothing but the read-only tree), and hold each clean walk to
/// its [`Floor`]s. Outcomes come back in the order given; a census that
/// panics comes back red.
#[must_use]
pub fn run_censuses(root: &Path, selected: &[&Census]) -> Vec<CensusOutcome> {
    std::thread::scope(|scope| {
        let running: Vec<_> = selected
            .iter()
            .map(|&census| {
                // The walkers recurse through directory trees and nested
                // items; give each the main thread's stack, not a spawned
                // thread's default.
                std::thread::Builder::new()
                    .name(format!("census-{}", census.name))
                    .stack_size(8 << 20)
                    .spawn_scoped(scope, move || (census.run)(root))
            })
            .collect();
        running
            .into_iter()
            .zip(selected)
            .map(|(handle, census)| {
                let joined = handle.map_err(|e| e.to_string()).and_then(|h| {
                    h.join()
                        .map_err(|_| "it panicked (message above)".to_string())
                });
                let mut outcome = joined.unwrap_or_else(|why| {
                    CensusOutcome::red(
                        census.name,
                        format!("=== {} census ===\n", census.name),
                        format!("the census could not run: {why}"),
                    )
                });
                outcome.hold_floors();
                outcome
            })
            .collect()
    })
}

/// Every census over the checkout at `root`, paired with its name, in
/// [`CENSUSES`] order.
#[must_use]
pub fn run_all(root: &Path) -> Vec<(&'static str, CensusOutcome)> {
    let every: Vec<&Census> = CENSUSES.iter().collect();
    CENSUSES
        .iter()
        .map(|c| c.name)
        .zip(run_censuses(root, &every))
        .collect()
}

/// The walker's limits, printed after a red verdict that found an
/// unbounded-work site. A plain multi-line literal: `\` continuations would
/// strip the indentation the diagnostic relies on.
pub const PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL, name-based: functions are segmented by rustfmt's
        closing-brace-at-fn-indent invariant; calls are matched as `<ident>(`;
        there is no type, trait, or borrow information. Same-named fns are
        merged into one node (an over-approximation — sound for a fail-closed
        census). Macro-generated calls, fn pointers, and deliberate aliasing
        can evade it; the realistic regression — reverting an offloaded resize
        to a synchronous `term_lock(..).resize(..)` on a main-thread path — is
        caught, with the reachability path printed.
      - SCOPE: the call graph covers non-test `crates/aterm-gui/src` ONLY
        (OB-22's per-site walk adds `crates/aterm-control/src`) —
        test/bench FILES dropped by path, and in-file items whose `cfg` gate
        ENTAILS `test` (`all(test, ..)` yes; `any(test, unix)` and
        `not(test)` no) blanked before segmentation, so a gate that does not
        entail it stays visible and fails rather than skipping silently —
        plus ONE modeled `term_lock` hop into Terminal methods. It does NOT
        recurse into aterm-core/aterm-grid bodies; instead the unbounded sinks
        are pinned at their definitions by `// COST: UNBOUNDED(<dim>)` markers,
        swept over the DERIVED GUI-process closure (the same workspace-member
        src/ scan set as OB-7; vendored code is not swept — it carries no
        aterm markers by definition) and kept consistent with the registry
        fail-closed in both directions. Markers in workspace crates outside
        the closure are reported, never registry-checked.
";

// ---------------------------------------------------------------------------
// Registries (the census's fail-closed ground truth)
// ---------------------------------------------------------------------------

/// A grid/terminal function that performs O(session-history) work. Each is
/// pinned at its definition by a `// COST: UNBOUNDED(<dim>)` marker whose
/// presence the census verifies (fail-closed both ways).
struct UnboundedSink {
    /// The fn name as it appears at its `fn <symbol>` definition.
    symbol: &'static str,
    /// The cost dimension named in the marker (documentation + failure text).
    dim: &'static str,
    /// Repo-relative file the marked definition lives in (for the failure text).
    def_file: &'static str,
}

/// The O(history) sinks the width-reflow funnels into (`Terminal::resize` ->
/// `Grid::resize` -> `resize_with_reflow_mode`, whose width branch calls
/// `take_scrollback_lines` + `reflow_scrollback_lines`), and the disk walker's
/// O(tree) entry points.
const UNBOUNDED_REGISTRY: &[UnboundedSink] = &[
    UnboundedSink {
        symbol: "reflow_scrollback_lines",
        dim: "session-history-cells",
        def_file: "crates/aterm-grid/src/grid/scrollback_reflow.rs",
    },
    UnboundedSink {
        symbol: "take_scrollback_lines",
        dim: "ring+tiered-history-lines",
        def_file: "crates/aterm-grid/src/grid/scrollback_reflow.rs",
    },
    UnboundedSink {
        symbol: "resize_with_reflow_mode",
        dim: "scrollback-width-reflow",
        def_file: "crates/aterm-grid/src/grid/reflow.rs",
    },
    // The disk walker (design §10.5): a slice is clock-bounded but for one
    // listing call, and a whole walk is the tree. Its thread runs them; the
    // event loop never does.
    UnboundedSink {
        symbol: "diskwalk_slice",
        dim: "entries",
        def_file: "crates/aterm-diskwalk/src/walk.rs",
    },
    UnboundedSink {
        symbol: "diskwalk_size_full",
        dim: "entries",
        def_file: "crates/aterm-diskwalk/src/walk.rs",
    },
];

/// The winit `ApplicationHandler` handlers and the resize timer arm: the entry
/// points that run on the event-loop (main) thread. `flush_pending_resize` is
/// the arm `new_events` dispatches when a coalesced live-resize settles.
///
/// OB-2: every name here must resolve in `crates/aterm-gui/src`.
const MAIN_THREAD_ROOTS: &[&str] = &[
    "new_events",
    "resumed",
    "user_event",
    "window_event",
    "about_to_wait",
    "flush_pending_resize",
];

/// The Terminal (`term_lock` hop) methods that reach an unbounded sink
/// synchronously under the lock (OB-5).
const UNBOUNDED_TERM_METHODS: &[&str] = &["resize"];

/// A deliberate offload/deferral boundary: bounded work on the lock, the
/// unbounded work handed off. Each entry (OB-3) carries a written
/// justification and must still be defined at `def_file`.
struct OffloadBoundary {
    /// The method name as called on the `term_lock` guard.
    symbol: &'static str,
    /// Repo-relative file holding the `fn <symbol>` definition (existence-checked).
    def_file: &'static str,
    /// The audited justification: why this is genuinely an offload boundary.
    justification: &'static str,
}

/// The audited offload boundaries of the scrollback-reflow protocol. A guarded
/// call to one counts as evidence the offload is in place (OB-4).
const OFFLOAD_ALLOWLIST: &[OffloadBoundary] = &[
    OffloadBoundary {
        symbol: "resize_offloading_scrollback",
        def_file: "crates/aterm-core/src/terminal/callback_setters.rs",
        justification: "The detach half of the L0 fix: O(1) detach of the tiered \
            scrollback store BEFORE the resize, so the synchronous reflow under the \
            lock touches only the bounded in-memory ring (O(ring), independent of \
            session lifetime). The unbounded decompress+rewrap leaves as a `Send` \
            PendingScrollbackReflow for a worker (enforced off-lock by construction: \
            `reflow()` consumes the detached value).",
    },
    OffloadBoundary {
        symbol: "finish_resize_offload",
        def_file: "crates/aterm-core/src/terminal/callback_setters.rs",
        justification: "The re-attach half: O(1) store re-attach plus a bounded \
            lazy-buffer drain (O(window output), not O(history)) under a brief lock, \
            after the worker did the unbounded rewrap off-thread. Guarded against \
            clobbering (reset raced) and against resurrecting erased history \
            (clear_gen check).",
    },
    OffloadBoundary {
        symbol: "abort_resize_offload",
        def_file: "crates/aterm-core/src/terminal/callback_setters.rs",
        justification: "The wedge-recovery half: O(1) flag-clear + bounded \
            lazy-buffer discard when a reflow worker dies and will never re-attach. \
            Without it the detach window stays open forever (unbounded lazy growth).",
    },
    OffloadBoundary {
        symbol: "reattach_reflowed_scrollback",
        def_file: "crates/aterm-grid/src/grid/scrollback_offload.rs",
        justification: "The Grid-level primitive `finish_resize_offload` forwards \
            to: O(1) attach of the already-rewrapped store + bounded lazy drain. \
            Registered so GUI code driving a Grid directly (no Terminal wrapper) \
            still has a sanctioned re-attach shape; it IS the underlying boundary, \
            not a suppression.",
    },
];

// ---------------------------------------------------------------------------
// Source scanning helpers
// ---------------------------------------------------------------------------

/// Is this file a test-only source file (excluded from "implementation" scans)?
/// A `tests_*.rs` beside `tests.rs` is one of its parts (`include!`d or `#[path]`ed into a
/// `#[cfg(test)]` module), the same as an `*_tests.rs`.
pub fn is_test_file(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/tests/")
        || s.ends_with("_tests.rs")
        || path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tests_"))
        || s.contains("/benches/")
        || s.ends_with("/proofs.rs")
        || s.contains("proofs_")
}

/// Recursively collect `*.rs` files under `dir`, skipping `target/` and hidden
/// directories (`.git`, worktrees under `.claude/`).
pub fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !tree::exists(dir) {
        return Ok(());
    }
    for entry in tree::read_dir(dir)? {
        let path = entry?;
        if tree::is_dir(&path) {
            // Compare encoded bytes, not `OsStr::to_str`: the same answer for
            // these ASCII names, without `to_str`'s UTF-8-revalidation unsafe in
            // this frame (the Trust L0 hardened lane). The function-path
            // spelling keeps `as_encoded_bytes` an opaque callee.
            let name = path
                .file_name()
                .map_or(&b""[..], std::ffi::OsStr::as_encoded_bytes);
            if name == b"target" || matches!(name, [b'.', ..]) {
                continue;
            }
            collect_rs_files(&path, out)?;
        } else if path
            .extension()
            .map_or(&b""[..], std::ffi::OsStr::as_encoded_bytes)
            == b"rs"
        {
            out.push(path);
        }
    }
    Ok(())
}

/// A parsed source function: its name, and the masked (comment-stripped) body
/// lines used for token scans + callee extraction.
struct GuiFn {
    name: String,
    /// Repo-relative `file:line` of the definition (for the failure path).
    span: String,
    body: Vec<String>,
}

/// The `src` directories of the window crate's own split-out libraries,
/// `<root>/crates/aterm-gui-*/src` (its fabric endpoint, tab and document
/// models, access card, timing value), in path order: the GUI's source as much
/// as `crates/aterm-gui/src` is, so every walk of the GUI takes them too. A
/// crate split must not narrow a gate.
pub(crate) fn gui_split_src_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = tree::read_dir(&root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("aterm-gui-"))
        })
        .map(|path| path.join("src"))
        .filter(|src| tree::is_dir(src))
        .collect();
    dirs.sort();
    dirs
}

/// Non-test `*.rs` files under `<root>/crates/aterm-gui/src` and the window
/// crate's split-out libraries ([`gui_split_src_dirs`]): one GUI source,
/// wherever a module of it was moved to cut the window crate's rebuild.
fn gui_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates/aterm-gui/src"), &mut files);
    for src in gui_split_src_dirs(root) {
        let _ = collect_rs_files(&src, &mut files);
    }
    files.retain(|p| !is_test_file(p));
    files.sort();
    files
}

/// Blank every in-file item whose `cfg` gate entails `test`
/// ([`cfg_attr_never_ships`]): the in-file half of the non-test scope that
/// [`gui_source_files`] applies by path. The walk merges same-named fns, so an
/// unmasked test helper would lend its body to every shipped fn of its name.
/// The shared item walker ([`lock_order::mask_items_where`]) preserves line
/// count and restores any span it cannot terminate.
fn mask_unshipped_items(text: &str) -> String {
    crate::lock_order::mask_items_where(text, &|attr| cfg_attr_never_ships(attr))
}

/// Per line of a lexed code view, whether it is outside every item whose
/// `cfg` proves it test-only (see [`mask_unshipped_items`]).
fn shipped_lines(code: &str) -> Vec<bool> {
    crate::lock_order::unmasked_lines(code, &|attr| cfg_attr_never_ships(attr))
}

/// Does this trimmed attribute line prove its item absent from a shipped
/// build? True only for `#[cfg(<pred>)]` whose `<pred>` entails `test` (see
/// [`cfg_pred_entails_unshipped`]); anything else keeps the item.
fn cfg_attr_never_ships(attr: &str) -> bool {
    let Some(rest) = attr.strip_prefix("#[cfg(") else {
        return false;
    };
    let Some(pred) = rest.strip_suffix(")]") else {
        return false;
    };
    cfg_pred_entails_unshipped(pred)
}

/// Does `pred` (a `cfg` predicate without the `cfg(..)` wrapper) hold only when
/// `test` is set? `false` means "not proven unshipped":
///
///   * the bare ident `test`;
///   * `all(..)`: one entailing child suffices, so
///     `all(test, any(windows, target_os = "linux"))` is test-only;
///   * `any(..)`: every child must entail (`any(test, unix)` ships on unix);
///   * everything else, `not(..)` included, is false.
fn cfg_pred_entails_unshipped(pred: &str) -> bool {
    let pred = pred.trim();
    if pred == "test" {
        return true;
    }
    for (head, all_children_required) in [("all(", false), ("any(", true)] {
        if let Some(rest) = pred.strip_prefix(head)
            && let Some(inner) = rest.strip_suffix(')')
        {
            let children = split_top_level_commas(inner);
            if children.is_empty() {
                return false;
            }
            return if all_children_required {
                children.iter().all(|c| cfg_pred_entails_unshipped(c))
            } else {
                children.iter().any(|c| cfg_pred_entails_unshipped(c))
            };
        }
    }
    false
}

/// Split a `cfg` argument list on its top-level commas — commas inside nested
/// parentheses or inside a string literal (`target_os = "linux"`) do not
/// separate arguments. Empty/whitespace-only pieces are dropped.
fn split_top_level_commas(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut start = 0usize;
    for (i, c) in args.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth = depth.saturating_sub(1),
            ',' if !in_str && depth == 0 => {
                let piece = args[start..i].trim();
                if !piece.is_empty() {
                    out.push(piece);
                }
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    let piece = args[start..].trim();
    if !piece.is_empty() {
        out.push(piece);
    }
    out
}

/// Strip a whole-line `//` comment and any inline ` // …` trailing comment, so
/// token scans never fire on commented-out code or prose. (A `//` inside a
/// string literal is not special-cased.)
fn strip_line_comment(line: &str) -> &str {
    let t = line.trim_start();
    if t.starts_with("//") {
        return "";
    }
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Extract `<ident>` from a `fn <ident>` occurrence starting at `after_fn` (the
/// slice right after `"fn "`).
fn ident_after_fn(after_fn: &str) -> Option<String> {
    let id: String = after_fn
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if id.is_empty() { None } else { Some(id) }
}

/// Is `line` a function definition? Returns `(indent, name)`. Recognises the
/// rustfmt-normalised `((pub…) )?(const |async |unsafe |extern "…" )*fn <name>`
/// prefixes; ignores `fn` appearing elsewhere (e.g. `Fn(` bounds, `fn` in a type).
fn parse_fn_def(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start().len();
    let mut rest = line.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("fn ") {
            return ident_after_fn(after).map(|n| (indent, n));
        }
        let mut advanced = false;
        for kw in [
            "pub(crate) ",
            "pub(super) ",
            "pub(self) ",
            "pub ",
            "const ",
            "async ",
            "unsafe ",
            "default ",
        ] {
            if let Some(s) = rest.strip_prefix(kw) {
                rest = s.trim_start();
                advanced = true;
                break;
            }
        }
        // `pub(in path) ` / `extern "C" `: skip a single parenthesized or
        // quoted qualifier token, then continue.
        if !advanced
            && let Some(s) = rest.strip_prefix("pub(in ")
            && let Some((_, after)) = s.split_once(") ")
        {
            rest = after.trim_start();
            advanced = true;
        }
        if !advanced && let Some(s) = rest.strip_prefix("extern ") {
            rest = s.trim_start().trim_start_matches('"');
            rest = rest
                .split_once('"')
                .map(|(_, r)| r.trim_start())
                .unwrap_or(rest);
            advanced = true;
        }
        if !advanced {
            return None;
        }
    }
}

/// Segment every function in `text` using rustfmt's closing-brace-at-fn-indent
/// invariant. `rel` is the repo-relative path (for spans). Bodies are stored
/// with line comments stripped. Shared by the main-loop and lock-order
/// censuses, so their walked surfaces agree.
fn parse_source_fns(text: &str, rel: &str, out: &mut Vec<GuiFn>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let Some((indent, name)) = parse_fn_def(lines[i]) else {
            i += 1;
            continue;
        };
        // The body ends at the first later line that is exactly `<indent>}`.
        // A def line with balanced braces (`fn f() { … }`) is the whole body;
        // otherwise a one-line fn would swallow the following fns.
        let def_line = strip_line_comment(lines[i]);
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
            // If no matching close was found (parse slip), end stays i: take just
            // the def line so callee extraction still sees inline calls.
        }
        let body: Vec<String> = lines[i..=end]
            .iter()
            .map(|l| strip_line_comment(l).to_string())
            .collect();
        out.push(GuiFn {
            name,
            span: format!("{rel}:{}", i + 1),
            body,
        });
        i = end + 1;
    }
}

/// The identifier ending exactly at byte offset `end` in `s` (the receiver before
/// a `.method(`), if the char before it is not an identifier char.
fn ident_ending_at(s: &str, end: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == end {
        None
    } else {
        Some(&s[start..end])
    }
}

/// Term-lock guard variables bound in `body` via `let [mut] <ident> =
/// term_lock(` or the UI thread's registering `[crate::]term_lock_ui(`, which
/// returns the same guard.
fn guard_vars(body: &[String]) -> std::collections::BTreeSet<String> {
    let mut guards = std::collections::BTreeSet::new();
    for line in body {
        let Some(eq) = line
            .find("= term_lock(")
            .or_else(|| line.find("= crate::term_lock_ui("))
            .or_else(|| line.find("= term_lock_ui("))
        else {
            continue;
        };
        let lhs = line[..eq].trim_end();
        // `let mut IDENT` / `let IDENT`
        let name: String = lhs
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if !name.is_empty() {
            guards.insert(name);
        }
    }
    guards
}

/// A synchronous term-hop call found in a function body: the unbounded sink method
/// it lands on and the source line (for the failure text).
struct TermHopHazard {
    method: String,
    line: String,
}

/// Detect synchronous `term_lock`-guarded calls to an unbounded term method in
/// `body`: chained (`term_lock(...).<m>(`) or via a bound guard
/// (`<guard>.<m>(`). Calls to OFFLOAD_ALLOWLIST methods are the sanctioned
/// shape and are counted separately as evidence.
fn term_hop_calls(body: &[String]) -> (Vec<TermHopHazard>, usize) {
    let guards = guard_vars(body);
    let mut hazards = Vec::new();
    let mut offloads = 0usize;
    let classify = |method: &str, line: &str, hz: &mut Vec<TermHopHazard>, off: &mut usize| {
        if UNBOUNDED_TERM_METHODS.contains(&method) {
            hz.push(TermHopHazard {
                method: method.to_string(),
                line: line.trim().to_string(),
            });
        } else if OFFLOAD_ALLOWLIST.iter().any(|b| b.symbol == method) {
            *off += 1;
        }
    };
    for line in body {
        // Idiom (1): a call chained directly on the lock guard, `term_lock(..).m(`
        // (or `term_lock_ui(..).m(` — the `_ui` infix breaks the plain needle).
        if line.contains("term_lock(") || line.contains("term_lock_ui(") {
            let mut search = line.as_str();
            while let Some(rel) = search.find(").") {
                let after = &search[rel + 2..];
                let method: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let is_call = after[method.len()..].starts_with('(');
                if is_call && !method.is_empty() {
                    classify(&method, line, &mut hazards, &mut offloads);
                }
                search = &search[rel + 2..];
            }
        }
        // Idiom (2): a call on a bound `term_lock` guard, `<guard>.m(`.
        for g in &guards {
            let needle = format!("{g}.");
            let mut from = 0;
            while let Some(rel) = line[from..].find(&needle) {
                let at = from + rel;
                // Require a receiver boundary: the char before `<guard>` is not an
                // identifier char (so `subterm.` doesn't match guard `term`).
                let boundary = at == 0 || {
                    let prev = line.as_bytes()[at - 1];
                    !(prev.is_ascii_alphanumeric() || prev == b'_')
                };
                let after = &line[at + needle.len()..];
                let method: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let is_call = after[method.len()..].starts_with('(');
                if boundary && is_call && !method.is_empty() {
                    classify(&method, line, &mut hazards, &mut offloads);
                }
                from = at + needle.len();
            }
        }
    }
    (hazards, offloads)
}

/// Callee short-names referenced in `body` (any `<ident>(`), for the call graph.
fn callee_names(body: &[String]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for line in body {
        let bytes = line.as_bytes();
        for (idx, &b) in bytes.iter().enumerate() {
            if b == b'('
                && let Some(name) = ident_ending_at(line, idx)
            {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// One swept `// COST: UNBOUNDED(<dim>)` marker: the fn it annotates, its
/// declared cost dimension, and the repo-relative `file:line` of the marker.
struct Marker {
    symbol: String,
    dim: String,
    span: String,
}

/// The marker-sweep file exclusions shared by both sweep scopes: test files
/// and this crate (its tests synthesize marker lines).
fn marker_sweep_file(p: &Path) -> bool {
    !is_test_file(p) && !p.to_string_lossy().contains("/aterm-census/")
}

/// Sweep `files` for `// COST: UNBOUNDED(<dim>)` markers, mapping each to the
/// `fn <name>` it annotates (spans repo-relative to `root`).
fn sweep_markers(root: &Path, files: &[PathBuf]) -> Vec<Marker> {
    let mut found = Vec::new();
    for file in files {
        let Ok(text) = tree::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("// COST: UNBOUNDED(") else {
                continue;
            };
            let dim: String = rest.chars().take_while(|c| *c != ')').collect();
            // The fn name is the next `fn <name>` line (allow attr/comment/blank).
            for cand in lines.iter().skip(i + 1) {
                let ct = cand.trim_start();
                if ct.is_empty() || ct.starts_with("//") || ct.starts_with("#[") {
                    continue;
                }
                if let Some((_, name)) = parse_fn_def(cand) {
                    found.push(Marker {
                        symbol: name,
                        dim: dim.clone(),
                        span: format!("{rel}:{}", i + 1),
                    });
                }
                break;
            }
        }
    }
    found
}

/// The OB-1 marker sweep over the derived GUI-process closure (`scan_dirs`,
/// the workspace-member `src/` set OB-7 uses; build scripts and examples are
/// other processes). Vendored `[patch]` code carries no aterm markers and is
/// not swept, so a sink whose `def_file` pointed into `vendor/` would fail
/// OB-1's reverse direction.
fn collect_closure_markers(root: &Path, scan_dirs: &[String]) -> Vec<Marker> {
    let mut files = Vec::new();
    for dir in scan_dirs {
        let _ = collect_rs_files(&root.join(dir), &mut files);
    }
    files.retain(|p| marker_sweep_file(p));
    files.sort();
    sweep_markers(root, &files)
}

/// The out-of-closure marker sweep: every crate dir under `<root>/crates` not
/// in the derived GUI-process closure (CLI tools, spec tooling, proc-macros,
/// separate-process binaries). Markers found here are reported but not subject
/// to OB-1; a crate entering aterm-gui's dependency graph joins the closure
/// and its markers become OB-1 obligations.
fn collect_out_of_closure_markers(root: &Path, scan_dirs: &[String]) -> Vec<Marker> {
    let closure_crate_dirs: std::collections::BTreeSet<&str> = scan_dirs
        .iter()
        .filter_map(|d| d.strip_suffix("/src"))
        .collect();
    let mut files = Vec::new();
    if let Ok(entries) = tree::read_dir(&root.join("crates")) {
        for path in entries.into_iter().flatten() {
            if !tree::is_dir(&path) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if closure_crate_dirs.contains(rel.as_str()) {
                continue;
            }
            let _ = collect_rs_files(&path, &mut files);
        }
    }
    files.retain(|p| marker_sweep_file(p));
    files.sort();
    sweep_markers(root, &files)
}

/// Does `<root>/<rel_file>` still hold a `fn <symbol>` definition (comment lines
/// excluded)? The OB-3 stale-sanction check.
fn fn_defined_in(root: &Path, rel_file: &str, symbol: &str) -> bool {
    let Ok(text) = tree::read_to_string(&root.join(rel_file)) else {
        return false;
    };
    text.lines()
        .any(|l| parse_fn_def(strip_line_comment(l)).is_some_and(|(_, name)| name == symbol))
}

// ---------------------------------------------------------------------------
// The census run
// ---------------------------------------------------------------------------

/// Append the shared why-L0 + repair block for an OB-5/OB-6 violation.
fn append_why_and_repair(log: &mut String) {
    let _ = writeln!(
        log,
        "    WHY THIS IS L0: the winit event-loop thread performs O(session-history)\n\
         \x20        work (decompressing + rewrapping every scrollback line) while\n\
         \x20        HOLDING the per-session `term` mutex. Input, redraw, and every\n\
         \x20        window served by this loop stall until it finishes — the class\n\
         \x20        that shipped a real 42-second whole-Mac freeze (6.6 h session;\n\
         \x20        fixed by the a69a6bb3 offload).\n\
         \x20   HOW TO REPAIR (pick one):\n\
         \x20     1. OFFLOAD (the canonical fix): replace `.resize(rows, cols)` with\n\
         \x20        `.resize_offloading_scrollback(rows, cols)` inside the lock; hand\n\
         \x20        the returned `PendingScrollbackReflow` to a worker (its `reflow()`\n\
         \x20        runs OFF the lock), re-attach with `.finish_resize_offload(..)`,\n\
         \x20        and call `.abort_resize_offload()` if the worker dies. Canonical\n\
         \x20        shape: crates/aterm-gui/src/app_render.rs `resize_panes_scoped`.\n\
         \x20     2. BOUND the work: if this site provably never reflows unbounded\n\
         \x20        history (alt-screen grid, fixed-size scratch terminal), use a path\n\
         \x20        that cannot reach the width reflow (e.g. `resize_no_reflow`) and\n\
         \x20        write down why at the call site.\n\
         \x20     3. NEW JUSTIFIED BOUNDARY (last resort): wrap the bounded/deferred\n\
         \x20        variant in a dedicated, honestly-named Terminal method and register\n\
         \x20        it in OFFLOAD_ALLOWLIST (crates/aterm-census/src/lib.rs) WITH a\n\
         \x20        written justification — the census fail-closes on undefined or\n\
         \x20        unjustified entries (OB-3)."
    );
}

/// The main-loop census's verdict title.
const MAINLOOP: &str = "main-loop census";

/// Run the main-loop census over the checkout at `root` (the directory holding
/// `crates/`). A pure function of the source tree.
pub fn run_mainloop_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== main-loop census (L0 freeze class) ===\n\
         \x20   root: {}",
        root.display()
    );

    // [OB-1] The marker scope is the same derived GUI-process closure OB-7
    // uses. An unclassifiable dependency graph stops the census.
    let scan = match scan_set::derive_gui_scan_set(root) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] SCAN-SET DERIVATION FAILED — the census cannot soundly \
                 determine the aterm-gui process closure from the workspace manifests, so \
                 it refuses to sweep a guessed marker scope (fail-closed).\n\
                 \x20       {e}"
            );
            return CensusOutcome::red(MAINLOOP, log, "1 obligation violation(s).");
        }
    };
    let _ = writeln!(
        log,
        "    marker sweep: the DERIVED GUI-process closure ({} workspace crates, src/ \
         only — the same scan set as OB-7). Vendored [patch] code is NOT marker-swept \
         (aterm's marker vocabulary; vendored forks carry no aterm COST markers by \
         definition). Markers in workspace crates OUTSIDE the closure are reported \
         below, never registry-checked.",
        scan.scan_dirs.len()
    );

    // [OB-1] Marker <-> registry consistency (fail-closed, both directions).
    let markers = collect_closure_markers(root, &scan.scan_dirs);
    for m in &markers {
        if !UNBOUNDED_REGISTRY.iter().any(|u| u.symbol == m.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] `{}` carries a `// COST: UNBOUNDED` marker (at {}) but is \
                 NOT in UNBOUNDED_REGISTRY (register the sink in \
                 crates/aterm-census/src/lib.rs so the census tracks it).",
                m.symbol, m.span
            );
            failures += 1;
        }
    }
    for sink in UNBOUNDED_REGISTRY {
        if !markers.iter().any(|m| m.symbol == sink.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] registered sink `{}` ({}) has LOST its `// COST: UNBOUNDED` \
                 marker at {} (restore the marker at its def, or drop the registry entry). \
                 NOTE: the sweep covers the derived GUI-process closure (workspace src/) — \
                 a marker moved to a crate outside the closure cannot satisfy the registry.",
                sink.symbol, sink.dim, sink.def_file
            );
            failures += 1;
        }
    }

    // Out-of-closure markers: reported, never registry-checked (see
    // collect_out_of_closure_markers).
    let out_markers = collect_out_of_closure_markers(root, &scan.scan_dirs);
    for m in &out_markers {
        let _ = writeln!(
            log,
            "  • NOTE [OB-1 scope] out-of-process `// COST: UNBOUNDED({})` marker on fn \
             `{}` at {} — this crate is OUTSIDE the derived aterm-gui process closure, so \
             the marker documents a hazard in a process the main-loop census does not \
             model. It is reported here (never silently dropped) but is NOT subject to \
             the marker ↔ registry coherence. If the crate enters the aterm-gui \
             dependency graph, it joins the closure automatically and this marker \
             becomes an OB-1 obligation.",
            m.dim, m.symbol, m.span
        );
    }

    // [OB-3] Every allowlisted offload boundary must be justified and still defined.
    for b in OFFLOAD_ALLOWLIST {
        if b.justification.trim().is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-3] offload boundary `{}` has an EMPTY justification — every \
                 allowlist entry must say why it is genuinely a boundary.",
                b.symbol
            );
            failures += 1;
        }
        if !fn_defined_in(root, b.def_file, b.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-3] offload boundary `{}` is allowlisted but `fn {}` is no longer \
                 defined in {} — a STALE entry cannot keep sanctioning the shape. Re-audit: \
                 update def_file if it moved, or remove the entry (and re-run the census).",
                b.symbol, b.symbol, b.def_file
            );
            failures += 1;
        }
    }

    // Build the aterm-gui call graph + per-fn hazard classification.
    let mut fns: Vec<GuiFn> = Vec::new();
    for file in gui_source_files(root) {
        let Ok(text) = tree::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        // Blank in-file test items before segmentation, so a gated helper
        // cannot lend its body to a same-named shipped fn (see
        // `mask_unshipped_items`).
        parse_source_fns(&mask_unshipped_items(&text), &rel, &mut fns);
    }
    // name -> all definition indices. Every same-named def is walked; picking
    // the first would drop a hazard in a later one. Over-approximation is sound
    // for a fail-closed census.
    let mut by_name: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (idx, f) in fns.iter().enumerate() {
        by_name.entry(f.name.clone()).or_default().push(idx);
    }
    let edges: Vec<std::collections::BTreeSet<String>> =
        fns.iter().map(|f| callee_names(&f.body)).collect();
    let hop: Vec<(Vec<TermHopHazard>, usize)> =
        fns.iter().map(|f| term_hop_calls(&f.body)).collect();
    let offload_sites: usize = hop.iter().map(|(_, n)| *n).sum();
    let hazards: Vec<&Vec<TermHopHazard>> = hop.iter().map(|(h, _)| h).collect();

    // [OB-2] BFS from every main-thread root; record parents for path
    // reconstruction. Every declared root must resolve (fail-closed).
    let mut parent: std::collections::BTreeMap<usize, Option<usize>> = Default::default();
    let mut queue: std::collections::VecDeque<usize> = Default::default();
    let mut roots_found = 0;
    for r in MAIN_THREAD_ROOTS {
        if let Some(indices) = by_name.get(*r) {
            roots_found += 1;
            for &idx in indices {
                if parent.insert(idx, None).is_none() {
                    queue.push_back(idx);
                }
            }
        } else {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-2] declared main-thread root `{r}` not found in \
                 crates/aterm-gui/src — a renamed handler must update MAIN_THREAD_ROOTS \
                 (crates/aterm-census/src/lib.rs), or the walked surface silently shrinks."
            );
            failures += 1;
        }
    }
    if roots_found == 0 {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-2] no MAIN_THREAD_ROOTS resolved — the census walked nothing \
             (parser broke, or this root is not an aterm checkout?)."
        );
        return CensusOutcome::red(
            MAINLOOP,
            log,
            format!("{} obligation violation(s).", failures + 1),
        );
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

    // [OB-5] Any reachable fn with an UNBOUNDED term-hop call is a class violation.
    let mut hazard_hits = 0usize;
    for idx in 0..fns.len() {
        if !parent.contains_key(&idx) || hazards[idx].is_empty() {
            continue;
        }
        for hz in hazards[idx] {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-5] L0 OBLIGATION VIOLATED — a main-thread root synchronously \
                 reaches an\n\
                 \x20        UNBOUNDED sink under the per-session `term` mutex (whole-Mac-freeze \
                 class).\n\
                 \x20   PATH:  {}\n\
                 \x20   SITE:  {} (fn `{}`)\n\
                 \x20   CALL:  `.{}(..)` on a `term_lock(..)` guard\n\
                 \x20   LINE:  {}\n\
                 \x20   SINK:  Terminal::{} -> Grid::resize -> Grid::resize_with_reflow_mode\n\
                 \x20          [COST: UNBOUNDED({}) @ {}], whose width branch synchronously runs\n\
                 \x20          take_scrollback_lines [UNBOUNDED({})] then reflow_scrollback_lines\n\
                 \x20          [UNBOUNDED({})] over the ENTIRE session history.",
                reconstruct(idx),
                fns[idx].span,
                fns[idx].name,
                hz.method,
                hz.line,
                hz.method,
                sink_dim("resize_with_reflow_mode"),
                sink_file("resize_with_reflow_mode"),
                sink_dim("take_scrollback_lines"),
                sink_dim("reflow_scrollback_lines"),
            );
            append_why_and_repair(&mut log);
            failures += 1;
            hazard_hits += 1;
        }
    }

    // [OB-6] A reachable fn calling a registered sink DIRECTLY bypasses the
    // Terminal hop entirely — same class, no lock even needed to freeze the loop.
    for idx in 0..fns.len() {
        if !parent.contains_key(&idx) {
            continue;
        }
        for sink in UNBOUNDED_REGISTRY {
            if edges[idx].contains(sink.symbol) {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-6] L0 OBLIGATION VIOLATED — a main-thread root reaches a \
                     DIRECT call to\n\
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
                append_why_and_repair(&mut log);
                failures += 1;
                hazard_hits += 1;
            }
        }
    }

    // [OB-22] No unbounded work under a held `term` guard at any shipped site
    // of the GUI's code, reachable from a root or not (see `held_guard`).
    let held = match held_guard::scan_tree(root) {
        Ok(scan) => {
            if scan.acquires == 0 {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-22] the held-guard walk read {} file(s) and saw ZERO `term` \
                     acquires — a blind walk, not a clean one (renamed acquire, or a parser \
                     regression).",
                    scan.files
                );
                failures += 1;
            }
            for f in &scan.findings {
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-22] L0 OBLIGATION VIOLATED — unbounded work while the \
                     per-session `term` guard is held.\n\
                     \x20   SITE:  {}\n\
                     \x20   LINE:  {}",
                    f.span, f.line
                );
                append_why_and_repair(&mut log);
                failures += 1;
                hazard_hits += 1;
            }
            Some((scan.files, scan.acquires))
        }
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-22] the held-guard walk could not run: {e} (scope: {}, \
                 crates/aterm-gui-*/src).",
                held_guard::HELD_GUARD_SCOPE.join(", ")
            );
            failures += 1;
            None
        }
    };

    // [OB-4] The offload boundary must still hold the line somewhere in the GUI.
    if offload_sites == 0 {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-4] ZERO calls to any allowlisted offload boundary ({}) found in \
             crates/aterm-gui/src — either the offload was removed (the freeze class is \
             unguarded) or the boundary was renamed (update OFFLOAD_ALLOWLIST with a fresh \
             justification).",
            allowlist_symbols().join("/")
        );
        failures += 1;
    }

    if failures > 0 {
        if hazard_hits > 0 {
            let _ = write!(log, "{PRECISION_NOTE}");
        }
        return CensusOutcome::red(
            MAINLOOP,
            log,
            format!("{failures} obligation violation(s) ({hazard_hits} unbounded-work site(s))."),
        );
    }
    let (held_files, held_acquires) = held.unwrap_or_default();
    let _ = writeln!(
        log,
        "    [OB-22] no unbounded work under a held `term` guard: {held_files} file(s) of {} + \
         crates/aterm-gui-*/src read, {held_acquires} acquiring line(s).",
        held_guard::HELD_GUARD_SCOPE.join(" + ")
    );
    let _ = writeln!(
        log,
        "    scope: lexical name-based walk of crates/aterm-gui/src + crates/aterm-gui-*/src + one \
         term_lock hop; \
         markers swept over the derived {}-crate GUI-process closure \
         (precision limits: docs/temporal-safety-gate.md).",
        scan.scan_dirs.len()
    );
    let summary = format!(
        "{} fn(s) walked from {roots_found} main-thread root(s); no synchronous reach to an \
         UNBOUNDED sink; {offload_sites} offload boundary call(s) ({}) hold the line; {} \
         sink(s) marked + registered; {} boundaries defined + justified; {} out-of-closure \
         marker(s) reported; {held_acquires} `term` acquire(s) held to no unbounded work.",
        parent.len(),
        allowlist_symbols().join("/"),
        UNBOUNDED_REGISTRY.len(),
        OFFLOAD_ALLOWLIST.len(),
        out_markers.len(),
    );
    CensusOutcome::green(MAINLOOP, log, summary).with_floors(vec![Floor::at_least(
        "`term` acquires the held-guard walk read",
        held_acquires,
        100,
        "the walk lost most of the GUI's lock sites (a renamed acquire or a parser \
         regression), so OB-22 holds vacuously; teach `held_guard` the new spelling",
    )])
}

fn allowlist_symbols() -> Vec<&'static str> {
    OFFLOAD_ALLOWLIST.iter().map(|b| b.symbol).collect()
}

fn sink_dim(symbol: &str) -> &'static str {
    UNBOUNDED_REGISTRY
        .iter()
        .find(|u| u.symbol == symbol)
        .map(|u| u.dim)
        .unwrap_or("history")
}

fn sink_file(symbol: &str) -> &'static str {
    UNBOUNDED_REGISTRY
        .iter()
        .find(|u| u.symbol == symbol)
        .map(|u| u.def_file)
        .unwrap_or("crates/aterm-grid")
}

/// Rust source with comments/literal contents blanked, and with comments alone
/// blanked. Both retain byte offsets; developer generators use the same lexer
/// as the source guards rather than treating braces in strings as syntax.
#[must_use]
pub fn rust_source_views(source: &str) -> (String, String) {
    let views = lex::lex(source);
    (views.code, views.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_set::test_fixtures::synth_tree;

    #[test]
    fn a_tests_part_is_a_test_file_and_a_name_that_only_contains_it_is_not() {
        let test = |p: &str| is_test_file(Path::new(p));
        assert!(test(
            "crates/aterm-shell-integration/src/tests_pwsh_keys.rs"
        ));
        assert!(test("crates/aterm-pty/src/unix_tests.rs"));
        assert!(test("crates/aterm-pty/tests/smoke.rs"));
        assert!(!test("crates/aterm-pty/src/lib.rs"));
        assert!(!test("crates/aterm-pty/src/latests_seen.rs"));
        assert!(!test("crates/aterm-pty/src/tests_dir/lib.rs"));
    }

    /// `files` (repo-relative paths) as an empty-file synthetic tree.
    fn empty_files(name: &str, files: &[&str]) -> crate::tree::memory::Tree {
        let files: Vec<_> = files
            .iter()
            .map(|rel| ((*rel).to_string(), String::new()))
            .collect();
        synth_tree("aterm-census-walk", name, &files)
    }

    /// [`collect_rs_files`] collects every `.rs` below the directory at any
    /// depth, and none under a `target/` or any dot directory (a worktree under
    /// `.claude/`, a tool's hidden scratch), whose copies are not the
    /// checkout's sources.
    #[test]
    fn the_rs_walk_skips_target_and_dot_directories() {
        let root = empty_files(
            "skips",
            &[
                "crates/a/src/lib.rs",
                "crates/a/src/m/x.rs",
                "crates/a/src/notes.md",
                "crates/a/target/debug/build/out.rs",
                "crates/a/.claude/worktrees/w/src/lib.rs",
                "crates/a/src/.scratch/x.rs",
            ],
        );
        let mut files = Vec::new();
        collect_rs_files(&root.join("crates"), &mut files).expect("every entry reads");
        files.sort();
        assert_eq!(
            files,
            [
                root.join("crates/a/src/lib.rs"),
                root.join("crates/a/src/m/x.rs")
            ]
        );
    }

    /// What the walk cannot read ends it with the error, however deep: an
    /// entry a directory's listing cannot read, or a directory it may not list
    /// (the disk's unreadable directory). The walks that refuse on it
    /// (`held_guard::scan_tree`, the guards' corpus) would otherwise pass a
    /// tree they did not see whole. Negative control: the same tree read whole.
    #[test]
    fn the_rs_walk_stops_where_it_cannot_read() {
        use std::io::ErrorKind;
        let walk = |fault: fn(&crate::tree::memory::Tree)| {
            let root = empty_files("unread", &["crates/a/src/lib.rs", "crates/a/src/m/x.rs"]);
            fault(&root);
            let mut files = Vec::new();
            collect_rs_files(&root.join("crates"), &mut files)
                .map(|()| files.len())
                .map_err(|e| e.kind())
        };
        assert_eq!(walk(|_| {}), Ok(2), "read whole");
        assert_eq!(
            walk(|root| root.unreadable("crates/a/src/m/x.rs")),
            Err(ErrorKind::Other)
        );
        assert_eq!(
            walk(|root| root.unlistable("crates/a/src/m")),
            Err(ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn run_censuses_keeps_the_given_order_and_turns_a_panic_red() {
        fn clean(_: &Path) -> CensusOutcome {
            CensusOutcome::green("a", String::new(), "clean")
        }
        fn broken(_: &Path) -> CensusOutcome {
            panic!("a walker bug")
        }
        let a = Census {
            name: "a",
            about: "",
            run: clean,
        };
        let b = Census {
            name: "b",
            about: "",
            run: broken,
        };
        let out = run_censuses(Path::new("."), &[&b, &a]);
        assert!(
            !out[0].ok && out[0].log.contains("b census"),
            "{}",
            out[0].log
        );
        assert!(out[1].ok, "{}", out[1].log);
    }

    /// A clean walk that missed a floor comes back red, naming the floor; a
    /// walk already red keeps its own verdict.
    #[test]
    fn run_censuses_turns_a_missed_floor_red() {
        fn thin(_: &Path) -> CensusOutcome {
            CensusOutcome::green("thin", "=== thin ===\n".to_string(), "1 site").with_floors(vec![
                Floor::at_least("sites read", 1, 100, "the walk went blind"),
                Floor::at_least("files read", 3, 1, "no files"),
            ])
        }
        fn red(_: &Path) -> CensusOutcome {
            CensusOutcome::red("red", String::new(), "1 violation").with_floors(vec![
                Floor::at_least("sites read", 0, 100, "the walk went blind"),
            ])
        }
        let (a, b) = (
            Census {
                name: "thin",
                about: "",
                run: thin,
            },
            Census {
                name: "red",
                about: "",
                run: red,
            },
        );
        assert!(thin(Path::new(".")).ok, "the bare walk holds no floors");
        let out = run_censuses(Path::new("."), &[&a, &b]);
        assert!(!out[0].ok, "{}", out[0].log);
        assert!(
            out[0].log.contains("[floor] sites read") && !out[0].log.contains("files read"),
            "only the missed floor is named:\n{}",
            out[0].log
        );
        assert!(
            out[1].verdict.ends_with("1 violation"),
            "{}",
            out[1].verdict
        );
    }

    fn body(src: &str) -> Vec<String> {
        src.lines().map(|l| l.to_string()).collect()
    }

    // ------------------------------------------------------------------
    // Unit tests for the lexical walker.
    // ------------------------------------------------------------------

    #[test]
    fn parse_fn_def_recognises_qualified_definitions_not_fn_bounds() {
        assert_eq!(
            parse_fn_def("    pub(crate) fn resize_panes_scoped(&mut self) {"),
            Some((4, "resize_panes_scoped".to_string()))
        );
        assert_eq!(
            parse_fn_def("fn cross_resize(term: &Arc) -> String {"),
            Some((0, "cross_resize".to_string()))
        );
        // `Fn(` in a where-clause / bound is not a definition.
        assert!(parse_fn_def("    where F: Fn(u16) -> bool,").is_none());
        assert!(parse_fn_def("        let f = default_fn(x);").is_none());
    }

    #[test]
    fn term_hop_flags_synchronous_guarded_resize_both_idioms() {
        // Idiom (1): chained on the lock.
        let (hz, _) = term_hop_calls(&body("    let p = term_lock(term).resize(rows, cols);"));
        assert_eq!(hz.len(), 1, "chained term_lock(..).resize( must flag");
        assert_eq!(hz[0].method, "resize");
        // Idiom (2): through a bound guard.
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = term_lock(&s.term);\n    term.resize(sub_rows, sub_cols);",
        ));
        assert_eq!(hz.len(), 1, "guard.resize( must flag");
    }

    #[test]
    fn term_hop_passes_the_offloaded_shape_and_bounded_neighbours() {
        // The sanctioned offload call is not a hazard (and is counted as evidence).
        let (hz, off) = term_hop_calls(&body(
            "    let mut term = term_lock(&s.term);\n    \
             term.resize_offloading_scrollback(sub_rows, sub_cols)",
        ));
        assert!(hz.is_empty(), "resize_offloading_scrollback must not flag");
        assert_eq!(off, 1, "offload boundary should be counted");
        // Bounded neighbour methods on the guard are ignored.
        let (hz, off) = term_hop_calls(&body("    term_lock(&s.term).set_cell_pixel_size(w, h);"));
        assert!(hz.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn term_hop_ignores_vec_and_local_terminal_resize() {
        // Vec::resize / Row::resize on a non-guard receiver: not a term hop.
        let (hz, _) = term_hop_calls(&body("    self.born_scratch.resize(rows * cols, None);"));
        assert!(hz.is_empty());
        // `term` here is a local Terminal, not a `term_lock` guard: a private
        // terminal's resize is not the hazard.
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = Terminal::from_checkpoint(cp, host);\n    \
             term.resize(rows, cols);",
        ));
        assert!(hz.is_empty(), "local Terminal::resize must not flag");
        assert!(
            guard_vars(&body(
                "    let mut term = Terminal::from_checkpoint(cp, host);"
            ))
            .is_empty()
        );
    }

    #[test]
    fn guard_vars_captures_the_term_lock_binding() {
        let g = guard_vars(&body("        let mut term = term_lock(&s.term);"));
        assert!(g.contains("term"));
    }

    /// The registering `term_lock_ui` binds the same guard, so `term.resize(`
    /// under it is a hazard. Covers path-qualified and bare spellings and the
    /// chained idiom.
    #[test]
    fn guard_vars_captures_the_term_lock_ui_binding() {
        let g = guard_vars(&body(
            "            let mut term = crate::term_lock_ui(&front_terminal.term, ui_waiting, site);",
        ));
        assert!(
            g.contains("term"),
            "crate::term_lock_ui( binding yields `term`"
        );
        let g = guard_vars(&body(
            "        let mut t = term_lock_ui(&term, &ui_waiting, site);",
        ));
        assert!(g.contains("t"), "bare term_lock_ui( binding yields `t`");
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = crate::term_lock_ui(&s.term, w, site);\n    term.resize(r, c);",
        ));
        assert_eq!(
            hz.len(),
            1,
            "resize on a term_lock_ui-bound guard must flag"
        );
        assert_eq!(hz[0].method, "resize");
        let (hz, _) = term_hop_calls(&body(
            "    crate::term_lock_ui(&s.term, w, site).resize(r, c);",
        ));
        assert_eq!(hz.len(), 1, "chained term_lock_ui(..).resize( must flag");
    }

    // ------------------------------------------------------------------
    // Synthetic-tree tests: the census end-to-end against a minimal checkout,
    // GREEN and RED. Marker lines are built with format!/escapes so this file
    // never carries a real marker.
    // ------------------------------------------------------------------

    /// The marker + sink-def files every synthetic tree needs (OB-1/OB-3 green).
    fn synth_support_files() -> Vec<(String, String)> {
        let m = "// COST: UNBOUNDED"; // assembled so this line is never a marker
        vec![
            (
                "crates/aterm-grid/src/grid/reflow.rs".to_string(),
                format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
            ),
            (
                "crates/aterm-grid/src/grid/scrollback_reflow.rs".to_string(),
                format!(
                    "{m}(ring+tiered-history-lines)\npub fn take_scrollback_lines() {{}}\n\
                     {m}(session-history-cells)\npub fn reflow_scrollback_lines() {{}}\n"
                ),
            ),
            (
                "crates/aterm-core/src/terminal/callback_setters.rs".to_string(),
                "pub fn resize_offloading_scrollback() {}\n\
                 pub fn finish_resize_offload() {}\n\
                 pub fn abort_resize_offload() {}\n"
                    .to_string(),
            ),
            (
                "crates/aterm-grid/src/grid/scrollback_offload.rs".to_string(),
                "pub fn reattach_reflowed_scrollback() {}\n".to_string(),
            ),
            // OB-22's second scope directory.
            (
                "crates/aterm-control/src/lib.rs".to_string(),
                "// stub\n".to_string(),
            ),
            (
                "crates/aterm-diskwalk/src/walk.rs".to_string(),
                format!(
                    "{m}(entries)\npub fn diskwalk_slice() {{}}\n\
                     {m}(entries)\npub fn diskwalk_size_full() {{}}\n"
                ),
            ),
        ]
    }

    /// A derivable synthetic workspace: the marker sweep is closure-scoped, so
    /// every synthetic tree needs manifests. The derived closure is
    /// {aterm-gui, aterm-types, aterm-core, aterm-grid, aterm-diskwalk}, plus
    /// src/ stubs so the drift guard holds when a test filters a support file
    /// away.
    fn synth_manifests() -> Vec<(String, String)> {
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[
            ("aterm-core", ""),
            ("aterm-grid", ""),
            ("aterm-diskwalk", ""),
        ]);
        for stub in [
            "crates/aterm-types/src/lib.rs",
            "crates/aterm-core/src/lib.rs",
            "crates/aterm-grid/src/lib.rs",
            "crates/aterm-diskwalk/src/lib.rs",
        ] {
            files.push((stub.to_string(), "// stub\n".to_string()));
        }
        files
    }

    /// Manifests + support files: the green base every mainloop synthetic
    /// tree starts from.
    fn synth_base() -> Vec<(String, String)> {
        let mut files = synth_manifests();
        files.extend(synth_support_files());
        files
    }

    /// A gui main.rs with every declared root present; `extra` fns appended.
    fn synth_gui(extra: &str) -> String {
        format!(
            "fn new_events() {{}}\n\
             fn resumed() {{}}\n\
             fn user_event() {{\n    window_event();\n}}\n\
             fn window_event() {{\n    resize_helper();\n}}\n\
             fn about_to_wait() {{}}\n\
             fn flush_pending_resize() {{}}\n\
             {extra}"
        )
    }

    #[test]
    fn synthetic_offloaded_tree_is_green() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let pending = term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "green", &files);
        let out = run_mainloop_census(&root);
        assert!(out.ok, "expected GREEN, got:\n{}", out.log);
        assert!(
            out.log.contains("main-loop census: GREEN"),
            "log:\n{}",
            out.log
        );
        // The sweep scope is the synthetic workspace's derived closure
        // (aterm-gui, aterm-types, aterm-core, aterm-grid, aterm-diskwalk),
        // stated in the log.
        assert!(
            out.log
                .contains("the DERIVED GUI-process closure (5 workspace crates"),
            "the transcript must state the derived marker-sweep scope; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("0 out-of-closure marker(s) reported"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_prefix_resize_shape_is_red_with_path() {
        // A synchronous guarded resize on a window_event-reachable path, plus
        // one offload call elsewhere so only OB-5 distinguishes this tree.
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let mut term = term_lock(&s.term);\n    \
             term.resize(sub_rows, sub_cols);\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "red-ob5", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-5]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("window_event -> resize_helper"),
            "the diagnostic must print the root -> hazard path; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("term.resize(sub_rows, sub_cols);"),
            "the diagnostic must quote the offending line; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("HOW TO REPAIR"),
            "the diagnostic must carry the repair options; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("main-loop census: FAILED — "),
            "the verdict line must name the census; log:\n{}",
            out.log
        );
    }

    // ------------------------------------------------------------------
    // The in-file unshipped mask (`#[cfg(test)]` and its conjunctive kin).
    // ------------------------------------------------------------------

    #[test]
    fn cfg_predicate_entails_unshipped_only_when_it_really_does() {
        // Entailing: the bare gates, and any conjunction containing one
        // (`#[cfg(all(test, any(windows, target_os = "linux")))]`).
        for yes in [
            "#[cfg(test)]",
            "#[cfg(all(test, unix))]",
            "#[cfg(all(test, any(windows, target_os = \"linux\")))]",
            "#[cfg(all(unix, all(test, debug_assertions)))]",
        ] {
            assert!(cfg_attr_never_ships(yes), "`{yes}` must be masked");
        }
        // Not entailing: a disjunction with a shipped arm, a negation, a
        // feature merely named "test", and anything unparsed.
        for no in [
            "#[cfg(any(test, unix))]",
            "#[cfg(not(test))]",
            "#[cfg(all(not(test), unix))]",
            "#[cfg(feature = \"test\")]",
            "#[cfg(target_os = \"macos\")]",
            "#[cfg(all())]",
            "#[test]",
            "#[derive(Debug)]",
            "let x = 1;",
        ] {
            assert!(!cfg_attr_never_ships(no), "`{no}` must NOT be masked");
        }
    }

    #[test]
    fn top_level_comma_split_ignores_nesting_and_string_literals() {
        assert_eq!(
            split_top_level_commas("test, any(windows, target_os = \"linux\")"),
            vec!["test", "any(windows, target_os = \"linux\")"]
        );
        assert_eq!(
            split_top_level_commas("target_os = \"a,b\", unix"),
            vec!["target_os = \"a,b\"", "unix"]
        );
        assert!(split_top_level_commas("  ").is_empty());
    }

    // A guarded `resize` that exists only in a test build is not a shipped
    // main-thread hazard. The helper shares its name with a fn the root
    // reaches, because name-merging is what would lend it to the shipped node.
    #[test]
    fn conjunctive_cfg_test_helper_is_masked_not_graphed() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             compose();\n}\n\
             fn compose() {}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n\
             #[cfg(all(test, any(windows, target_os = \"linux\")))]\n\
             mod strip_band_landing_tests {\n    \
             fn compose() {\n        \
             let mut term = term_lock(&s.term);\n        \
             term.resize(rows, cols);\n    }\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "mask-cfg-all-test", &files);
        let out = run_mainloop_census(&root);
        assert!(
            out.ok,
            "a `cfg(all(test, ..))` helper never enters the shipped GUI \
             process and must not fail OB-5:\n{}",
            out.log
        );
    }

    // A gate that does not entail `test` leaves the hazard visible and red.
    #[test]
    fn a_disjunctive_cfg_test_gate_is_not_masked_and_stays_red() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             compose();\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n\
             #[cfg(any(test, unix))]\n\
             mod maybe_shipped {\n    \
             fn compose() {\n        \
             let mut term = term_lock(&s.term);\n        \
             term.resize(rows, cols);\n    }\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "no-mask-any-test", &files);
        let out = run_mainloop_census(&root);
        assert!(
            !out.ok,
            "`any(test, unix)` compiles on a shipped unix build — the hazard \
             under it must still fail OB-5:\n{}",
            out.log
        );
        assert!(out.log.contains("[OB-5]"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_direct_sink_call_is_red_ob6() {
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let lines = reflow_scrollback_lines(&lines, cols);\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("aterm-census", "red-ob6", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-6]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("reflow_scrollback_lines"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_missing_boundary_def_is_red_ob3_and_missing_offload_is_red_ob4() {
        // Drop the callback_setters.rs defs and all offload calls: OB-3 and
        // OB-4 both fire. (aterm-core keeps its src/lib.rs stub, so the tree
        // fails on OB-3/OB-4, not the derivation.)
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .filter(|(rel, _)| !rel.ends_with("callback_setters.rs"))
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui("fn resize_helper() {}\n"),
            )])
            .collect();
        let root = synth_tree("aterm-census", "red-ob3-ob4", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-3]"), "log:\n{}", out.log);
        assert!(out.log.contains("[OB-4]"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_missing_root_is_red_ob2() {
        let mut files = synth_base();
        // No `flush_pending_resize` fn anywhere.
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            "fn new_events() {}\n\
             fn resumed() {}\n\
             fn user_event() {}\n\
             fn window_event() {}\n\
             fn about_to_wait() {}\n\
             fn some_offload() {\n    \
             term_lock(&s.term).resize_offloading_scrollback(r, c);\n}\n"
                .to_string(),
        ));
        let root = synth_tree("aterm-census", "red-ob2", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-2]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("flush_pending_resize"),
            "log:\n{}",
            out.log
        );
    }

    // ------------------------------------------------------------------
    // Closure-scoped marker sweep: positive, negative, out-of-closure reporting.
    // ------------------------------------------------------------------

    #[test]
    fn out_of_closure_marker_is_not_swept_and_is_reported() {
        // A marker on an unregistered fn in a crate outside the derived closure
        // (a CLI tool): GREEN, and the marker is reported.
        let m = "// COST: UNBOUNDED"; // assembled so this line is never a marker
        let mut files = synth_base();
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui(
                "fn resize_helper() {\n    \
                 term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
            ),
        ));
        files.push((
            "crates/aterm-clitool/src/main.rs".to_string(),
            format!("{m}(whole-archive-dump)\npub fn dump_everything() {{}}\n"),
        ));
        let root = synth_tree("aterm-census", "out-of-closure", &files);
        let out = run_mainloop_census(&root);
        assert!(
            out.ok,
            "an out-of-closure marker must NOT enter OB-1 coherence; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("NOTE [OB-1 scope]")
                && out.log.contains("dump_everything")
                && out.log.contains("crates/aterm-clitool/src/main.rs:1"),
            "the out-of-closure marker must be REPORTED with its span; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("1 out-of-closure marker(s) reported"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn out_of_closure_marker_cannot_satisfy_the_registry() {
        // Move a registered sink's marker out of the closure: OB-1's reverse
        // direction still fires (an out-of-closure marker cannot satisfy the
        // registry) and the stray marker is reported.
        let m = "// COST: UNBOUNDED"; // assembled so this line is never a marker
        let mut files: Vec<(String, String)> = synth_base()
            .into_iter()
            .map(|(rel, contents)| {
                if rel.ends_with("grid/reflow.rs") {
                    // The fn def stays (OB-3 untouched); only the marker is gone.
                    (rel, "pub fn resize_with_reflow_mode() {}\n".to_string())
                } else {
                    (rel, contents)
                }
            })
            .collect();
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            synth_gui(
                "fn resize_helper() {\n    \
                 term_lock(&s.term).resize_offloading_scrollback(rows, cols);\n}\n",
            ),
        ));
        files.push((
            "crates/aterm-clitool/src/lib.rs".to_string(),
            format!("{m}(scrollback-width-reflow)\npub fn resize_with_reflow_mode() {{}}\n"),
        ));
        let root = synth_tree("aterm-census", "moved-marker", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("[OB-1]")
                && out.log.contains("has LOST its")
                && out.log.contains("resize_with_reflow_mode"),
            "the registry must not be satisfiable from outside the closure; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("NOTE [OB-1 scope]"),
            "the stray out-of-closure marker must still be reported; log:\n{}",
            out.log
        );
    }

    #[test]
    fn underivable_workspace_fails_the_mainloop_census_closed() {
        // No root Cargo.toml: the closure cannot be derived, so the sweep
        // refuses to guess (RED), as OB-7 does.
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .filter(|(rel, _)| rel != "Cargo.toml")
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui("fn resize_helper() {}\n"),
            )])
            .collect();
        let root = synth_tree("aterm-census", "underivable", &files);
        let out = run_mainloop_census(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("SCAN-SET DERIVATION FAILED"),
            "log:\n{}",
            out.log
        );
    }
}
