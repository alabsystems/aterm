// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! MAIN-LOOP COMPLETENESS CENSUS — the L0 whole-Mac-freeze CLASS, as a
//! fail-closed, build-blocking OBLIGATION.
//!
//! THE BUG THIS GUARDS: a width change used to rewrap the ENTIRE off-screen
//! scrollback synchronously (`Grid::resize` → `resize_with_reflow_mode` →
//! `take_scrollback_lines` + `reflow_scrollback_lines`, cost O(session
//! history)) on the winit event-loop thread while holding the per-session
//! `term` mutex — an observed, real 42-second whole-Mac freeze after a 6.6 h
//! session. Fixed by the offload (commit a69a6bb3):
//! `resize_offloading_scrollback` detaches the tiered store O(1), a worker
//! rewraps it off the lock, `finish_resize_offload` re-attaches it. This census
//! is the standing CLASS guard: it walks the name-based call graph of
//! `crates/aterm-gui/src` from each main-thread ROOT (the winit
//! `ApplicationHandler` handlers + the resize timer arm), models one `term_lock`
//! hop, and FAILS — printing the exact root → call path → sink, why it is L0,
//! and the repair options — if a root synchronously reaches an UNBOUNDED
//! O(history) sink that is not behind a justified offload boundary.
//!
//! ONE implementation, TWO consumers (so the verb and the gate cannot diverge):
//!
//!   * `cargo xtask gate mainloop` (crates/xtask/src/gate.rs) — the standalone
//!     verb: part of `gate all`, and invoked by tools/verify.sh (line 495). NOT
//!     "the pre-push hook", which this sentence used to claim: MEASURED
//!     2026-08-01, `.githooks/pre-push` runs exactly ONE command — the
//!     freeze-safety-gate build below (line 111 of the hook), which fuses this
//!     same census. So the CENSUS does run pre-push; this VERB does not.
//!   * `tools/freeze-safety-gate/build.rs` — the SAME `cargo build` that runs
//!     the temporal proof gate runs this census and fails the compile on any
//!     obligation violation. That fusion is what makes the census AUTOMATIC:
//!     the temporal models are opt-in (annotated protocols → derived model →
//!     Trust `ty` proof), while the census requires NO annotation or opt-in
//!     from the code under scan — new hazardous code is caught by construction.
//!
//! The obligations (each fail-closed; `[OB-n]` tags appear in the diagnostics):
//!
//! * OB-1  marker ↔ registry: every `// COST: UNBOUNDED(<dim>)` marker maps to
//!   a registered sink AND every registered sink is still marked at its
//!   definition (both directions, mirroring `gate drift`'s witnesses). The
//!   sweep's SCOPE is the DERIVED GUI-process closure — the same
//!   [`scan_set::derive_gui_scan_set`] result OB-7 consumes (workspace
//!   members' `src/` only; vendored code carries no aterm markers by
//!   definition and is not swept); a marker in a workspace crate OUTSIDE the
//!   closure is an other-process hazard note: REPORTED every run, never
//!   registry-checked (see `collect_out_of_closure_markers`).
//! * OB-2  every declared main-thread ROOT must resolve to a real fn — a
//!   renamed winit handler must update [`MAIN_THREAD_ROOTS`], not silently
//!   shrink the walked surface.
//! * OB-3  every [`OFFLOAD_ALLOWLIST`] boundary must carry a non-empty written
//!   justification AND still be defined at its registered file — a stale entry
//!   cannot keep sanctioning a shape that no longer exists.
//! * OB-4  at least one allowlisted offload-boundary call must exist in the
//!   walked GUI source — if the offload disappears (or is renamed away), the
//!   hazard class is unguarded and the build fails until the census is
//!   re-audited.
//! * OB-5  no main-thread-reachable fn may SYNCHRONOUSLY call an unbounded
//!   Terminal method (`resize`) on a `term_lock` guard — the original freeze
//!   shape.
//! * OB-6  no main-thread-reachable fn may call a registered unbounded sink
//!   DIRECTLY (e.g. `reflow_scrollback_lines(..)`), bypassing the Terminal hop
//!   entirely.
//! * OB-7  LOCK-ORDER CENSUS ([`lock_order`], `run_lock_order_census`) — the
//!   lock-graph sense of the RFC §2.1c L0-DEADLOCK entry: every
//!   acquire-while-holding pair across the GUI-process crates, the global lock
//!   graph required ACYCLIC, any cycle failing the build with both sites of
//!   every edge. NO waiver channel, ever.
//! * OB-19..OB-21  LAZY-INIT REENTRANCY CENSUS ([`lazy_init`],
//!   `run_lazy_init_census`) — the REENTRANCY sense of the same L0-DEADLOCK
//!   entry, which OB-7 is structurally blind to: OB-7 asks whether two threads
//!   can take two locks in opposite orders, this asks whether ONE thread can
//!   arrive twice at the same lazy cell. A `OnceLock`/`LazyLock`/`Once`
//!   initializer that reaches a blocking touch of the cell it is itself
//!   initializing parks its own thread forever. The lazy-init graph is
//!   required ACYCLIC (OB-19 self-loops, OB-20 components) and the walk itself
//!   must stay non-blind (OB-21). NO waiver channel, ever. Added after that
//!   exact shape shipped in v0.65.0/v0.66.0 and froze the terminal on the main
//!   thread at the first automatic update apply.
//!
//! PRECISION: see [`PRECISION_NOTE`] / [`lock_order::LOCK_PRECISION_NOTE`] —
//! printed in every RED diagnostic and documented in
//! docs/temporal-safety-gate.md. These are lexical completeness CENSUSES, not
//! a borrow checker: honest about what they can and cannot see.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod lazy_init;
mod lock_order;
pub mod scan_set;
mod scope_census;
mod wasm_census;
pub use lazy_init::{LAZY_INIT_PRECISION_NOTE, run_lazy_init_census};
pub use lock_order::run_lock_order_census;
pub use scope_census::{SCOPE_PRECISION_NOTE, run_scope_census};
pub use wasm_census::run_wasm_census;

/// The census verdict plus its full human/AI-readable transcript. The log is
/// returned (not printed) so each consumer routes it appropriately: the xtask
/// verb to stderr, the build gate into the compile error.
pub struct CensusOutcome {
    /// `true` iff every obligation held.
    pub ok: bool,
    /// The complete diagnostic transcript (GREEN summary or `[OB-n]` failures).
    pub log: String,
}

/// The honest limits of the walker, printed verbatim in every RED diagnostic
/// (and quoted in docs/temporal-safety-gate.md so the docs cannot drift).
// NOTE: a plain multi-line literal (no `\` continuations, which would strip the
// leading indentation the diagnostic relies on).
pub const PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL, name-based: functions are segmented by rustfmt's
        closing-brace-at-fn-indent invariant; calls are matched as `<ident>(`;
        there is no type, trait, or borrow information. Same-named fns are
        merged into one node (an over-approximation — sound for a fail-closed
        census). Macro-generated calls, fn pointers, and deliberate aliasing
        can evade it; the realistic regression — reverting an offloaded resize
        to a synchronous `term_lock(..).resize(..)` on a main-thread path — is
        caught, with the reachability path printed.
      - SCOPE: the call graph covers non-test `crates/aterm-gui/src` ONLY,
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

/// The O(history) sinks the width-reflow funnels into. `Terminal::resize` ->
/// `Grid::resize` -> `resize_with_reflow_mode`, whose width branch calls
/// `take_scrollback_lines` + `reflow_scrollback_lines`.
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
];

/// The winit `ApplicationHandler` handlers + the resize timer arm — the entry
/// points that run ON the event-loop (main) thread. A synchronous reach from any
/// of these into an UNBOUNDED sink under the `term` lock is the L0 freeze class.
/// (`window_event` / `user_event` fan out to essentially the whole app, so the
/// walk's coverage is broad by construction; `flush_pending_resize` is the timer
/// arm `about_to_wait` dispatches when a coalesced live-resize settles.)
///
/// OB-2: every name here MUST resolve in `crates/aterm-gui/src` — renaming a
/// handler without updating this list fails the census (a missing root would
/// otherwise silently shrink the walked surface).
const MAIN_THREAD_ROOTS: &[&str] = &[
    "new_events",
    "resumed",
    "user_event",
    "window_event",
    "about_to_wait",
    "flush_pending_resize",
];

/// The Terminal (`term_lock` hop) methods that reach an UNBOUNDED sink
/// SYNCHRONOUSLY under the lock. Calling one on a `term_lock(...)` guard on a
/// main-thread-reachable path is the hazard (OB-5).
const UNBOUNDED_TERM_METHODS: &[&str] = &["resize"];

/// A deliberate offload / deferral boundary: a method whose guarded call is the
/// SANCTIONED shape. Every entry is an audited obligation (OB-3): it must carry
/// a written justification for WHY it is genuinely a boundary (bounded work on
/// the lock; the unbounded work handed off), and its definition must still
/// exist at `def_file` — a renamed or deleted boundary fails the census until
/// the entry is re-audited.
struct OffloadBoundary {
    /// The method name as called on the `term_lock` guard.
    symbol: &'static str,
    /// Repo-relative file holding the `fn <symbol>` definition (existence-checked).
    def_file: &'static str,
    /// The audited justification: why this is genuinely an offload boundary.
    justification: &'static str,
}

/// The audited offload boundaries of the scrollback-reflow protocol (the
/// a69a6bb3 L0 fix). A guarded call to one of these counts as evidence the
/// offload is in place (OB-4) rather than being flagged.
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
            Without it the detach window stays open forever (unbounded lazy growth); \
            the temporal proof gate's WindowCloses invariant covers the same class \
            from the protocol side.",
    },
    OffloadBoundary {
        symbol: "reattach_reflowed_scrollback",
        def_file: "crates/aterm-grid/src/grid/scrollback_offload.rs",
        justification: "The Grid-level primitive `finish_resize_offload` forwards \
            to: O(1) attach of the already-rewrapped store + bounded lazy drain. \
            Registered so GUI code driving a Grid directly (no Terminal wrapper) \
            still has a sanctioned re-attach shape. Audit 2026-07-08: zero direct \
            GUI call sites today (the Terminal wrapper is used); kept because it IS \
            the underlying boundary, not a suppression.",
    },
];

// ---------------------------------------------------------------------------
// Source scanning helpers (shared with the other xtask gates via re-use)
// ---------------------------------------------------------------------------

/// Is this file a test-only source file (excluded from "implementation" scans)?
pub fn is_test_file(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/tests/")
        || s.ends_with("_tests.rs")
        || s.contains("/benches/")
        || s.ends_with("/proofs.rs")
        || s.contains("proofs_")
}

/// Recursively collect `*.rs` files under `dir`, skipping `target/` and any
/// hidden directory (name starting with `.`). The hidden-dir skip matters
/// because workflow worktrees live under `.claude/worktrees/` (each a full repo
/// checkout); descending into them would count every source file N+1 times.
/// This matches the `grep -rn` semantics the count gates cite (BSD/GNU
/// `grep -r .` does not descend into dot-directories), so a developer with
/// active worktrees gets the same counts as a clean checkout.
pub fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            // Compare the file name's encoded bytes directly instead of
            // round-tripping through `OsStr::to_str`: for the checks below
            // this is equivalent (`b"target"` is ASCII, and the encoding is
            // ASCII-compatible, so a leading `b'.'` byte is exactly a leading
            // '.' for every name a checkout can contain), and it avoids
            // `to_str`'s UTF-8-revalidation unsafe being inlined into this
            // frame, which the Trust L0 hardened lane refutes for lacking a
            // local SAFETY comment. The function-path spelling (not a
            // closure/method call) keeps `as_encoded_bytes` an opaque callee
            // — same shape as `aterm_shell_integration::ShellType::detect`.
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
            // Byte-equality with the ASCII literal `b"rs"` holds iff
            // `to_str()` would succeed and equal "rs" — identical semantics.
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

/// Non-test `*.rs` files under `<root>/crates/aterm-gui/src`.
fn gui_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates/aterm-gui/src"), &mut files);
    files.retain(|p| !is_test_file(p));
    files.sort();
    files
}

/// Strip a whole-line `//` comment and any inline ` // …` trailing comment, so
/// token scans never fire on commented-out code or prose. (A `//` inside a string
/// literal is not special-cased — acceptable for these token scans, matching the
/// other gates' line-comment discipline.)
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

/// Is `line` a function DEFINITION? Returns `(indent, name)`. Recognises the
/// rustfmt-normalised `((pub…) )?(const |async |unsafe |extern "…" )*fn <name>`
/// prefixes; ignores `fn` appearing elsewhere (e.g. `Fn(` bounds, `fn` in a type).
fn parse_fn_def(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start().len();
    let mut rest = line.trim_start();
    // A definition begins with a visibility / qualifier chain then `fn `.
    // Reject lines where `fn` is not in leading position (e.g. `where F: Fn()`).
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
        // `pub(in path) ` / `extern "C" ` — skip a single parenthesised or quoted
        // qualifier token, then continue.
        // `pub(in path) ` — the comment above promised this and the code did
        // not do it, so such a fn was never segmented and every call in it was
        // missing from the graph. Zero instances in the tree today; the gap was
        // latent, which is the moment to close it.
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
/// invariant. `rel` is the repo-relative path (for spans). Bodies are stored with
/// line comments stripped. Shared by BOTH censuses (main-loop and lock-order):
/// one fn segmenter, so their walked surfaces can never disagree.
fn parse_source_fns(text: &str, rel: &str, out: &mut Vec<GuiFn>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let Some((indent, name)) = parse_fn_def(lines[i]) else {
            i += 1;
            continue;
        };
        // Body ends at the first later line that is exactly `<indent>}` (the fn's
        // own close — every nested close is indented deeper under rustfmt). If the
        // signature line itself already closes (`fn f() { … }` / `fn f() {}`), the
        // body IS the def line — detected by a balanced brace count on the
        // (comment-stripped) line. Without this, a single-line fn would swallow
        // every following fn up to the NEXT same-indent `}` and DROP those defs
        // from the graph (a falsely-shrunken walked surface — a soundness bug the
        // synthetic-tree tests pin).
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

/// Term-lock guard variables bound in `body` via `let [mut] <ident> = term_lock(`.
fn guard_vars(body: &[String]) -> std::collections::BTreeSet<String> {
    let mut guards = std::collections::BTreeSet::new();
    for line in body {
        let Some(eq) = line.find("= term_lock(") else {
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

/// Detect synchronous `term_lock`-guarded calls to an UNBOUNDED term method in
/// `body`, in two idioms: (1) chained on the lock, `term_lock(...).<m>(`, and
/// (2) via a bound guard, `<guard>.<m>(` where `<guard>` came from `term_lock`.
/// Calls to OFFLOAD_ALLOWLIST methods are the sanctioned shape and returned
/// separately (as evidence, not a hazard).
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
        // Idiom (1): a call chained directly on the lock guard, `term_lock(..).m(`.
        if line.contains("term_lock(") {
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

/// The marker-sweep file exclusions, shared by BOTH sweep scopes: test files,
/// THIS crate (its tests synthesise marker lines, and a synthetic marker must
/// never satisfy — or mask the loss of — a real one) and `xtask/src/gate.rs`
/// (whose prose mentions the marker token; defensive).
fn marker_sweep_file(p: &Path) -> bool {
    !is_test_file(p)
        && !p.ends_with("xtask/src/gate.rs")
        && !p.to_string_lossy().contains("/aterm-census/")
}

/// Sweep `files` for `// COST: UNBOUNDED(<dim>)` markers, mapping each to the
/// `fn <name>` it annotates (spans repo-relative to `root`).
fn sweep_markers(root: &Path, files: &[PathBuf]) -> Vec<Marker> {
    let mut found = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
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

/// The OB-1 marker sweep: `// COST: UNBOUNDED(<dim>)` markers over the DERIVED
/// GUI-process closure (`scan_dirs` — the same workspace-member `src/` scan set
/// OB-7 derives per run). Fail-closed: every marker here must map to a registry
/// entry, and every registry entry must be marked (OB-1).
///
/// SCOPE (2026-07-13; formerly a wholesale `crates/` walk): a marker is only an
/// OB-1 obligation if its crate can actually load into the aterm-gui process —
/// the process whose main loop this census guards. Workspace members only,
/// `src/` only (matching OB-7's walk domain: build scripts and examples run in
/// other processes / are not shipped). VENDORED `[patch]` code is NOT
/// marker-swept: the marker is aterm's own annotation vocabulary, and the
/// vendored forks are minimally-diverged upstream snapshots that carry no aterm
/// COST markers by definition (survey 2026-07-13: zero occurrences under
/// vendor/; the previous wholesale sweep never covered vendor/ either). If a
/// registered sink's `def_file` ever pointed into vendor/, OB-1's reverse
/// direction would fail (marker unfindable), forcing a conscious re-audit.
fn collect_closure_markers(root: &Path, scan_dirs: &[String]) -> Vec<Marker> {
    let mut files = Vec::new();
    for dir in scan_dirs {
        let _ = collect_rs_files(&root.join(dir), &mut files);
    }
    files.retain(|p| marker_sweep_file(p));
    files.sort();
    sweep_markers(root, &files)
}

/// The OUT-OF-CLOSURE marker sweep: every crate dir under `<root>/crates` that
/// is NOT in the derived GUI-process closure (CLI tools, spec/conformance
/// tooling, proc-macros, separate-process binaries), walked wholesale. Markers
/// found here are REPORTED in the transcript — never silently dropped — but are
/// NOT subject to OB-1's registry coherence: they document a hazard in a
/// process this census does not model. If such a crate ever enters the
/// aterm-gui dependency graph, it joins the closure automatically (the OB-7
/// derivation) and its markers become OB-1 obligations with zero census edits.
fn collect_out_of_closure_markers(root: &Path, scan_dirs: &[String]) -> Vec<Marker> {
    let closure_crate_dirs: std::collections::BTreeSet<&str> = scan_dirs
        .iter()
        .filter_map(|d| d.strip_suffix("/src"))
        .collect();
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
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
    let Ok(text) = std::fs::read_to_string(root.join(rel_file)) else {
        return false;
    };
    text.lines()
        .any(|l| parse_fn_def(strip_line_comment(l)).is_some_and(|(_, name)| name == symbol))
}

// ---------------------------------------------------------------------------
// The census run
// ---------------------------------------------------------------------------

/// Append the shared WHY-L0 + repair block for an OB-5/OB-6 violation.
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

/// Run the main-loop completeness census over the aterm checkout at `root` (the
/// directory holding `crates/`). Pure function of the source tree: no network,
/// no toolchain, no build artifacts — safe inside a build script and safe to
/// point at ANY checkout (a worktree of a historical commit included).
pub fn run_mainloop_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== gate mainloop (main-loop completeness census: L0 freeze CLASS) ===\n\
         \x20   root: {}",
        root.display()
    );

    // [OB-1] The marker sweep's SCOPE is the derived GUI-process closure — the
    // SAME `scan_set::derive_gui_scan_set` result OB-7 consumes, so the two
    // censuses can never disagree about what "the GUI process" is. Fail-closed:
    // an unclassifiable dependency graph must stop the build, never shrink (or
    // inflate) the marker scope silently.
    let scan = match scan_set::derive_gui_scan_set(root) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-1] SCAN-SET DERIVATION FAILED — the census cannot soundly \
                 determine the aterm-gui process closure from the workspace manifests, so \
                 it refuses to sweep a guessed marker scope (fail-closed).\n\
                 \x20       {e}\n\
                 gate mainloop: FAILED — 1 obligation violation(s)."
            );
            return CensusOutcome { ok: false, log };
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

    // OUT-OF-CLOSURE markers: reported, never silently dropped — and never
    // registry-checked (they document hazards of processes this census does
    // not model; see collect_out_of_closure_markers).
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

    // [OB-3] Every allowlisted offload boundary must be justified AND still defined.
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
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        parse_source_fns(&text, &rel, &mut fns);
    }
    // name -> ALL definition indices. A name-based census MUST walk every
    // same-named def: first-def-wins would drop a hazard living in a non-first
    // `fn resize` (etc.) from the graph and go falsely GREEN (audit bug H).
    // Over-approximation is sound for a fail-closed census.
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
    // reconstruction. EVERY declared root must resolve (fail-closed).
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
             (parser broke, or this root is not an aterm checkout?).\n\
             gate mainloop: FAILED — {} obligation violation(s).",
            failures + 1
        );
        return CensusOutcome { ok: false, log };
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
        // Deterministic order: by fn index.
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
        let _ = writeln!(
            log,
            "gate mainloop: FAILED — {failures} obligation violation(s) ({hazard_hits} \
             main-thread-reachable unbounded-work site(s)). This census blocks BOTH \
             `cargo xtask gate mainloop` and the `cargo build` of tools/freeze-safety-gate."
        );
        return CensusOutcome { ok: false, log };
    }
    let _ = writeln!(
        log,
        "gate mainloop: GREEN — {} fn(s) walked from {roots_found} main-thread root(s); \
         no synchronous reach to an UNBOUNDED sink; {offload_sites} offload boundary call(s) \
         ({}) hold the line; {} sink(s) marked + registered; {} boundaries defined + \
         justified; {} out-of-closure marker(s) reported.",
        parent.len(),
        allowlist_symbols().join("/"),
        UNBOUNDED_REGISTRY.len(),
        OFFLOAD_ALLOWLIST.len(),
        out_markers.len(),
    );
    let _ = writeln!(
        log,
        "    scope: lexical name-based walk of crates/aterm-gui/src + one term_lock hop; \
         markers swept over the derived {}-crate GUI-process closure \
         (precision limits: docs/temporal-safety-gate.md).",
        scan.scan_dirs.len()
    );
    CensusOutcome { ok: true, log }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> Vec<String> {
        src.lines().map(|l| l.to_string()).collect()
    }

    // ------------------------------------------------------------------
    // Unit tests for the lexical walker (moved verbatim from xtask gate.rs
    // when the census became this shared crate).
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
        // `Fn(` in a where-clause / bound is NOT a definition.
        assert!(parse_fn_def("    where F: Fn(u16) -> bool,").is_none());
        assert!(parse_fn_def("        let f = default_fn(x);").is_none());
    }

    #[test]
    fn term_hop_flags_synchronous_guarded_resize_both_idioms() {
        // Idiom (1): chained on the lock (the cross_resize bug shape).
        let (hz, _) = term_hop_calls(&body("    let p = term_lock(term).resize(rows, cols);"));
        assert_eq!(hz.len(), 1, "chained term_lock(..).resize( must flag");
        assert_eq!(hz[0].method, "resize");
        // Idiom (2): via a bound guard (the resize_panes_scoped bug shape).
        let (hz, _) = term_hop_calls(&body(
            "    let mut term = term_lock(&s.term);\n    term.resize(sub_rows, sub_cols);",
        ));
        assert_eq!(hz.len(), 1, "guard.resize( must flag");
    }

    #[test]
    fn term_hop_passes_the_offloaded_shape_and_bounded_neighbours() {
        // The sanctioned offload call is NOT a hazard (and is counted as evidence).
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
        // temporal `replay_at`: `term` is a LOCAL Terminal (not a term_lock guard),
        // so its synchronous resize is the private-terminal case, not the hazard.
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

    // ------------------------------------------------------------------
    // Synthetic-tree tests: the census run end-to-end against a minimal
    // fabricated checkout, GREEN and RED. All marker lines are built with
    // format!/escapes (never a raw multi-line literal), so THIS source file
    // can never satisfy — or mask — a real marker even though the scan also
    // excludes the census crate by path.
    // ------------------------------------------------------------------

    /// Write `files` (repo-relative path, contents) under a fresh temp root and
    /// return it. The caller removes it.
    fn synth_tree(name: &str, files: &[(String, String)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aterm-census-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
            std::fs::write(&path, contents).expect("write synth file");
        }
        root
    }

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
        ]
    }

    /// A DERIVABLE synthetic workspace (the scan_set fixture pattern): the
    /// marker sweep is closure-scoped since 2026-07-13, so every synthetic
    /// tree needs manifests the derivation accepts. The derived closure here
    /// is {aterm-gui, aterm-types, aterm-core, aterm-grid} — exactly the
    /// crates the support files populate — plus src/ stubs so the derivation's
    /// drift guard holds even when a test filters a support file away.
    fn synth_manifests() -> Vec<(String, String)> {
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[
            ("aterm-core", ""),
            ("aterm-grid", ""),
        ]);
        for stub in [
            "crates/aterm-types/src/lib.rs",
            "crates/aterm-core/src/lib.rs",
            "crates/aterm-grid/src/lib.rs",
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
        let root = synth_tree("green", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.ok, "expected GREEN, got:\n{}", out.log);
        assert!(
            out.log.contains("gate mainloop: GREEN"),
            "log:\n{}",
            out.log
        );
        // The sweep scope is the DERIVED closure of the synthetic workspace
        // (aterm-gui, aterm-types, aterm-core, aterm-grid), stated in the log.
        assert!(
            out.log
                .contains("the DERIVED GUI-process closure (4 workspace crates"),
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
        // The pre-a69a6bb3 shape: a synchronous guarded resize on a
        // window_event-reachable path (plus one offload call elsewhere so ONLY
        // OB-5 distinguishes this tree from the green one).
        let mut files = synth_base();
        let gui = synth_gui(
            "fn resize_helper() {\n    \
             let mut term = term_lock(&s.term);\n    \
             term.resize(sub_rows, sub_cols);\n}\n\
             fn unrelated_offload() {\n    \
             term_lock(&s.term).finish_resize_offload(reflowed);\n}\n",
        );
        files.push(("crates/aterm-gui/src/main.rs".to_string(), gui));
        let root = synth_tree("red-ob5", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        let root = synth_tree("red-ob6", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        // Drop the callback_setters.rs defs AND all offload calls: OB-3 (stale
        // sanction) and OB-4 (boundary vanished) must both fire. (aterm-core
        // keeps its src/lib.rs stub, so the scan-set drift guard still holds
        // and the tree fails on exactly OB-3/OB-4, not the derivation.)
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .filter(|(rel, _)| !rel.ends_with("callback_setters.rs"))
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui("fn resize_helper() {}\n"),
            )])
            .collect();
        let root = synth_tree("red-ob3-ob4", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        let root = synth_tree("red-ob2", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(out.log.contains("[OB-2]"), "log:\n{}", out.log);
        assert!(
            out.log.contains("flush_pending_resize"),
            "log:\n{}",
            out.log
        );
    }

    // ------------------------------------------------------------------
    // Closure-scoped marker sweep (2026-07-13): positive/negative + the
    // out-of-closure reporting posture.
    // ------------------------------------------------------------------

    #[test]
    fn out_of_closure_marker_is_not_swept_and_is_reported() {
        // A marker on an UNREGISTERED fn in a crate OUTSIDE the derived
        // closure (the cli-only-tool shape). The pre-2026-07-13 wholesale
        // `crates/` sweep would have gone RED [OB-1] (unregistered marker);
        // the closure-scoped sweep stays GREEN — the marker documents a
        // hazard of a process this census does not model — and the posture
        // is REPORT, never silent drop.
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
        let root = synth_tree("out-of-closure", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        // Move a registered sink's marker OUT of the closure (marker + fn def
        // duplicated in a non-closure crate; the in-closure def loses its
        // marker): OB-1's reverse direction must still fire — an out-of-
        // closure marker can never satisfy the registry — and the stray
        // marker must be reported.
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
        let root = synth_tree("moved-marker", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
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
        // No root Cargo.toml: the scan-set derivation cannot determine the
        // closure, so the marker sweep must refuse to guess (RED), exactly
        // like OB-7 already does.
        let files: Vec<(String, String)> = synth_base()
            .into_iter()
            .filter(|(rel, _)| rel != "Cargo.toml")
            .chain([(
                "crates/aterm-gui/src/main.rs".to_string(),
                synth_gui("fn resize_helper() {}\n"),
            )])
            .collect();
        let root = synth_tree("underivable", &files);
        let out = run_mainloop_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(!out.ok, "expected RED, got:\n{}", out.log);
        assert!(
            out.log.contains("SCAN-SET DERIVATION FAILED"),
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
    fn census_is_green_on_this_tree() {
        let out = run_mainloop_census(&repo_root());
        assert!(out.ok, "census RED on the current tree:\n{}", out.log);
    }

    #[test]
    fn offload_allowlist_entries_resolve_on_this_tree() {
        let root = repo_root();
        for b in OFFLOAD_ALLOWLIST {
            assert!(
                !b.justification.trim().is_empty(),
                "boundary `{}` must carry a justification",
                b.symbol
            );
            assert!(
                fn_defined_in(&root, b.def_file, b.symbol),
                "boundary `{}` is not defined in {} — stale allowlist entry",
                b.symbol,
                b.def_file
            );
        }
    }
}
