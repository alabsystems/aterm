// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! LOCK-ORDER CENSUS (OB-7) — the lock-graph sense of L0-DEADLOCK, as a
//! fail-closed, build-blocking obligation.
//!
//! DESIGN AUTHORITY: docs/RFC-trust-temporal-extraction.md §2.1c, the
//! L0-DEADLOCK row and its "two senses" paragraph. Deadlock has two senses that
//! need different engines: **model deadlock** (a ty model reaches a state with
//! no enabled action) is checked by ty's deadlock detection on every derived
//! model; **lock-graph deadlock** (the classic ABBA cycle between real mutexes)
//! is THIS census: statically enumerate every acquire-while-holding pair, keep
//! the global graph acyclic, and fail the build naming both sites of any cycle.
//!
//! NO WAIVER CHANNEL EXISTS, EVER — deliberately. Every other obligation in
//! this crate has an audited allowlist or justification path; this one has
//! none, because an allowlisted lock cycle would be a *standing deadlock*
//! shipped on purpose. A detected cycle can only be fixed (impose the canonical
//! order, or narrow a guard's scope), never talked past.
//!
//! WHAT IT DOES, mechanically:
//!
//!   1. LOCK-SITE ENUMERATION — every acquisition site in the scanned crates
//!      (`term_lock(`, `.lock()`, `.lock_or_recover()`, `.read()`, `.write()`,
//!      `.try_lock()`, `.try_read()`, `.try_write()`), each resolved to a
//!      lexical lock IDENTITY (the receiver/accessor name). Fail-honest: a
//!      site whose identity cannot be resolved is reported as UNKNOWN and
//!      counted, never silently dropped.
//!   2. ACQUIRED-WHILE-HOLDING PAIRS — within each fn, an acquisition inside
//!      the live lexical scope of an earlier guard; plus ONE interprocedural
//!      hop (a call made while holding, into a fn that itself acquires).
//!   3. THE GRAPH — directed edges `A -> B` ("B acquired while A held") over
//!      all scanned crates; the build FAILS on any cycle, naming EVERY edge of
//!      the cycle with both sites and the repair guidance. Self-edges
//!      (`A -> A`) fail as re-entrancy suspects: std::sync locks are not
//!      re-entrant (same-instance nesting deadlocks outright), and
//!      cross-instance nesting of one identity needs an instance order this
//!      census cannot see.
//!
//! Consumers (same fan-out as the main-loop census, so the verb and the gate
//! cannot diverge): `cargo xtask gate lockorder`, the fused
//! `tools/freeze-safety-gate/build.rs` build, and the `aterm-census` bin.
//!
//! PRECISION: see [`LOCK_PRECISION_NOTE`] — printed in every RED diagnostic
//! and quoted in docs/temporal-safety-gate.md.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::{
    CensusOutcome, collect_rs_files, fn_defined_in, ident_ending_at, is_test_file,
    parse_source_fns, strip_line_comment,
};

/// The honest limits of the lock-order walker, printed verbatim in every RED
/// diagnostic (and quoted in docs/temporal-safety-gate.md so the docs cannot
/// drift).
pub const LOCK_PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL IDENTITY: a lock's identity is the receiver/accessor NAME at the
        acquisition site (`term_lock(..)` => `term`; `store.read()` /
        `self.store.write()` => `store`; `proxies().write()` => `proxies`). Two
        different locks sharing a name MERGE (may over-report a cycle); one lock
        reached through different local names SPLITS (may under-report). Renaming
        a receiver changes its identity — the census trusts the field/static
        naming discipline; it is a tripwire, not a type-resolved proof.
      - UNKNOWN, NEVER DROPPED: `self`-only, tuple-field (`self.0`), and
        single-letter receivers are unresolvable; each is reported as
        UNKNOWN@site and counted. An UNKNOWN is a unique node (it never unifies),
        so it can appear in reported edges but can never close a cycle — the
        UNKNOWN count is the census's standing honesty gap, printed every run.
      - GUARD SCOPE is tracked lexically: a `let g = <acquire>` guard (through
        `unwrap`/`expect`/`unwrap_or_else` adapters only) lives to the end of its
        enclosing block (brace depth), honoring `drop(g)` and shadowing; an
        acquisition consumed inside its statement lives for that statement;
        `match`/`if let`/`while let` scrutinee guards live for the whole
        construct. Guards stored in structs/tuples or moved into
        `thread::spawn` closures are approximated (the spawn body is attributed
        to the spawning fn).
      - INTERPROCEDURAL DEPTH: exactly ONE hop — a call made while a guard is
        held, to a same-corpus fn (free-fn `name(..)` or `self.name(..)` call
        shapes only; `other.name(..)` is type-ambiguous and excluded) whose own
        body directly acquires. The callee's callees are NOT followed. Same-named
        fns merge (over-approximation, same posture as the main-loop census).
      - BLOCKING EDGES ONLY: `try_lock`/`try_read`/`try_write` cannot block, so
        a try-guard is an edge SOURCE (once held it is held) but never an edge
        TARGET. Scope: non-test sources of the scanned crates with `#[cfg(test)]`
        AND `#[cfg(kani)]` items masked (neither compiles into the shipped GUI
        process: test code never ships; kani code exists only under cargo-kani's
        `--cfg kani`); string/char literals masked before token scans;
        `.read()`/`.write()` with ZERO arguments are assumed RwLock acquisitions
        unless raw-pointer evidence applies (next bullet).
      - RAW-POINTER ptr::read: a zero-arg `.read()` is `core::ptr::read`, not an
        RwLock acquisition, when the receiver is lexically PROVEN a raw pointer:
        it is the direct result of a raw-pointer-producing method (`.add(..)`,
        `.offset(..)`, `.as_ptr()`, `.cast()`, ...), OR its live `let` binding
        (same fn, still in scope) constructs a raw pointer (`.as_ptr()`,
        `as *const`, `&raw const`, ..., or pointer arithmetic on an
        already-proven binding — evidence propagates through `let`s:
        `base.add(i)` from a proven `base` proves `item`), OR the enclosing
        fn's signature declares it `*const`/`*mut`. Such sites are categorized, listed with
        their evidence, and EXCLUDED from the mutex order graph (a raw pointer
        is not a lock). Fail-closed: without evidence the site keeps its normal
        resolution (or stays UNKNOWN). Zero-arg `.write()` needs no such
        category: `ptr::write` always takes the value argument, so it can never
        match the zero-arg token.
      - OS FILE-ADVISORY LOCKS: `.lock()`/`.try_lock()` on a receiver whose live
        `let` binding (same fn, still in scope) lexically constructs a
        std::fs::File (`File::open(`/`File::create(`/`OpenOptions::new()…open(`)
        is the flock-class OS ADVISORY lock, not an in-process mutex. Such sites
        are categorized and listed separately and EXCLUDED from the mutex order
        graph: their waits are against OTHER PROCESSES, which an in-process lock
        graph cannot model. Fail-closed: without that lexical File evidence the
        site stays UNKNOWN; a rebound or out-of-scope name loses the evidence.
      - VOCABULARY INTERIORS: a bare-`self` acquisition inside a registered
        implementation of a standard acquisition method (VOCABULARY_INTERIORS,
        fail-closed both ways) is the vocabulary's own delegation — the
        receiver IS each caller's lock, resolved per call site by the token the
        fn implements. The interior is listed with its audit note and keeps a
        unique per-site node (edges reported, never able to close a cycle).
      - VENDORED-IDENTITY MODE: the vendored [patch] crates that link into the
        GUI process (REVIEWED_VENDORED_CRATES) are scanned with every identity
        in a per-crate namespace (`winit::…`), so a foreign receiver name can
        never merge with an aterm identity or another vendored crate's.
        Vendored UNKNOWNs keep the per-site-node discipline (counted,
        summarized per crate, never able to close a cycle). Cross-boundary
        holds are subject to the same ONE-hop limit — and the trusted call
        shapes (free-fn `name(..)`, `self.name(..)`) mean most aterm↔vendored
        edges are visible only at DIRECT call sites; a hold carried through
        `other.method(..)` receivers or callback registration is not seen.
        winit's per-platform backends that do not compile into the shipped
        macOS GUI process are labeled slices: sites counted every run, never
        graphed (an edge from code linked into no shipped process could close
        a cycle the no-waiver obligation cannot repair without editing
        upstream code).
";

// The crates whose lock namespace this census walks — the FULL aterm-gui
// PROCESS surface — are no longer a manually-pinned list: they are DERIVED on
// every run by [`crate::scan_set::derive_gui_scan_set`], the offline
// Cargo.toml path-dependency closure of `crates/aterm-gui` (normal deps only;
// dev-/build-deps excluded; cfg-target deps included platform-independently;
// optional deps feature-resolved for the default build; proc-macro crates
// classified out by their manifests; vendored `[patch]` forks classified by
// the fail-closed-both-ways REVIEWED_VENDORED_CRATES registry — the linked
// ones scanned in vendored-identity mode, per-crate namespaces + labeled
// platform slices; pkg-config classified out as build-time-only). One
// process = one lock namespace = one deadlock domain; the per-session `term`
// mutex discipline and every registry/store/queue lock the GUI composes with
// live here.
//
// Extending coverage now requires NO census edit at all: adding a crate to
// the aterm-gui dependency graph adds it to the scan set automatically (and a
// crate leaving the graph is dropped the same way) — the automatic-obligation
// property extended to the obligation's own scope. The derived closure is
// pinned by `scan_set::tests::derived_closure_matches_the_pinned_canary`, so
// every scope change still surfaces as a reviewable test diff. (History:
// widened 2026-07-13 from 8 crates to the manually-derived 42-crate closure;
// replaced by this derivation the same day, verified equal at switchover.)

/// Acquisition vocabulary: the method-call tokens that take a lock, and
/// whether the acquisition can BLOCK (a `try_*` cannot, so it can never be the
/// waiting half of a deadlock).
///
/// SCAN INVARIANT (relied on by [`acquisitions_on`]): every token starts with
/// `.` and contains NO other `.`, and no token is a prefix of another. That is
/// what lets the scanner visit each `.` in a line once and `starts_with` the
/// whole table there, instead of running seven independent `str::find`s. A
/// future token violating either half would break that equivalence.
const ACQ_METHODS: &[(&str, bool)] = &[
    (".lock()", true),
    (".lock_or_recover()", true),
    (".read()", true),
    (".write()", true),
    (".try_lock()", false),
    (".try_read()", false),
    (".try_write()", false),
];

/// Result-of-lock adapters that pass the guard through unchanged: a chain of
/// only these between the acquisition and the end of a `let` statement means
/// the GUARD ITSELF is what got bound (block-scoped), not a value read out of
/// it (statement-scoped).
const GUARD_ADAPTERS: &[&str] = &["unwrap", "expect", "unwrap_or_else"];

/// A fn that ACQUIRES a lock and RETURNS the guard — its CALLERS hold the lock
/// invisibly (no `.lock()` token at the call site), so each registered helper
/// call is treated as a direct acquisition of the declared identity. The
/// registry is fail-closed BOTH ways, mirroring OB-1/OB-3:
///   * every entry must still define `fn <symbol>` at `def_file`, and its own
///     body must still acquire the declared identity (a moved/renamed helper
///     or a rewired interior fails the census until re-audited);
///   * every guard-returning fn the signature scan finds in the corpus (one
///     that acquires AND returns a `…Guard`) must be registered here or carry
///     a standard method name already covered by the token vocabulary AND be
///     method-shaped (take `self`, so callers really do spell the token) —
///     an unregistered helper would hide its callers' holds from the graph.
///     A FREE fn named `lock`/`read`/… is NOT exempt: its call sites
///     (`lock(&X)`) carry no `.lock()` token, so its callers' holds would be
///     invisible (found the hard way in aterm-pty's Windows module, since
///     refactored onto `lock_or_recover`).
struct GuardHelper {
    /// The fn name as called (`<symbol>(…)`).
    symbol: &'static str,
    /// The lock identity its guard represents.
    identity: &'static str,
    /// Repo-relative file holding the `fn <symbol>` definition.
    def_file: &'static str,
}

const GUARD_HELPERS: &[GuardHelper] = &[
    GuardHelper {
        symbol: "term_lock",
        identity: "term",
        // Moved main.rs -> lib.rs in the ONE-binary refactor (the windowed terminal
        // now lives in the library so the single `aterm` binary can serve it).
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        symbol: "lock_fonts",
        identity: "chrome_fonts",
        def_file: "crates/aterm-gui/src/tray_raster.rs",
    },
    GuardHelper {
        // The workspace's ONE process-environment mutation lock. `scoped`/
        // `scoped_unset` hold this guard across a caller-supplied body, so the
        // hold really is invisible at those call sites — exactly what this
        // registry exists to make visible to the graph.
        symbol: "env_lock",
        identity: "ENV_LOCK",
        def_file: "crates/aterm-log/src/lib.rs",
    },
    GuardHelper {
        // The serious-mode/search refactor replaced the singular
        // `SEARCH_SNAPSHOT` option with the eight-entry `SEARCH_SNAPSHOTS`
        // ring. The helper still returns that one static's guard, so the
        // registered identity must follow the plural receiver exactly.
        symbol: "search_cache_lock",
        identity: "SEARCH_SNAPSHOTS",
        def_file: "crates/aterm-gui/src/control_query.rs",
    },
];

/// The INTERIOR of the acquisition vocabulary itself: a fn that IMPLEMENTS one
/// of the standard acquisition method names by delegating to the wrapped std
/// primitive through a bare-`self` receiver (`impl MutexExt for Mutex<T> {
/// fn lock_or_recover(&self) { self.lock() … } }`). The receiver IS each
/// caller's lock, so the identity has no caller-independent name — it is
/// resolved at every call site by the very token the fn implements (callers'
/// `.lock_or_recover()` etc. are direct acquisitions of the callers' receiver
/// names). The interior site is rendered in its own AUDITED category, keeps a
/// unique per-site graph node (edges reported, can never unify into a cycle —
/// the same posture as UNKNOWN), and is never silently dropped.
///
/// Fail-closed BOTH ways, mirroring GUARD_HELPERS:
///   * every entry must name a STANDARD_METHOD_NAMES symbol (else callers
///     would NOT be captured by the token vocabulary and the categorization
///     would hide real holds — the census FAILS such a registration);
///   * the fn must still be defined at `def_file` AND its body must still
///     contain the bare-`self` acquisition — a moved/renamed/rewired fn makes
///     the entry STALE and FAILS the census until re-audited;
///   * only bare-`self` receivers qualify: a `self.<field>` delegation is
///     nameable (name the field — see aterm-types/src/sync.rs `raw`), so it
///     never lands here.
struct VocabularyInterior {
    /// The implementing fn (must be one of [`STANDARD_METHOD_NAMES`]).
    symbol: &'static str,
    /// Repo-relative file holding the `fn <symbol>` definition.
    def_file: &'static str,
    /// The audit note: what the delegation actually is, reviewable in place.
    audit: &'static str,
}

const VOCABULARY_INTERIORS: &[VocabularyInterior] = &[VocabularyInterior {
    symbol: "lock_or_recover",
    def_file: "crates/aterm-types/src/mutex_ext.rs",
    audit: "MutexExt::lock_or_recover, the poison-recovery impl for every \
            std::sync::Mutex<T>: `self.lock()` IS the caller's mutex — each call \
            site is captured by the `.lock_or_recover()` token and resolves to \
            the caller's receiver name",
}];

/// Lexical std::fs::File evidence for a `let` RHS: a local bound from one of
/// these construction idioms IS an OS file, so a later `.lock()`/`.try_lock()`
/// on it is the std::fs::File ADVISORY lock (flock-class — a cross-process
/// rendezvous), not an in-process mutex. Fail-closed: only these exact shapes
/// qualify; any receiver without this evidence keeps its normal resolution
/// (or stays UNKNOWN).
fn is_file_binding_rhs(line: &str) -> bool {
    line.contains("File::open(")
        || line.contains("File::create(")
        || line.contains("File::create_new(")
        || (line.contains("OpenOptions::new()") && line.contains(".open("))
}

/// Raw-pointer-PRODUCING method names: a zero-arg `.read()` whose receiver is
/// the direct result of one of these calls is `core::ptr::read` (these return
/// raw pointers; a raw pointer is not a lock). Used by the raw-pointer
/// evidence check, fail-closed: no evidence => no categorization.
const RAW_PTR_METHODS: &[&str] = &[
    "add",
    "sub",
    "offset",
    "wrapping_add",
    "wrapping_sub",
    "wrapping_offset",
    "byte_add",
    "byte_sub",
    "as_ptr",
    "as_mut_ptr",
    "cast",
];

/// Lexical raw-pointer evidence for a `let` RHS: a local bound from one of
/// these idioms IS a raw pointer, so a later zero-arg `.read()` on it is
/// `core::ptr::read`, not an RwLock acquisition. Fail-closed: only these
/// exact shapes qualify (mirrors [`is_file_binding_rhs`]).
fn is_raw_ptr_binding_rhs(line: &str) -> bool {
    line.contains(".as_ptr()")
        || line.contains(".as_mut_ptr()")
        || line.contains("as *const")
        || line.contains("as *mut")
        || line.contains("&raw const")
        || line.contains("&raw mut")
}

/// Propagated raw-pointer evidence for a `let` RHS: the RHS calls a
/// raw-pointer-producing method on a receiver ALREADY proven a raw pointer
/// (`let item = base.add(i)` where `base` is in the ledger) — pointer
/// arithmetic on a pointer yields a pointer. Fail-closed: the receiver must
/// be a live proven binding; anything else keeps its normal resolution.
/// (The vendored indexmap `extract.rs` shape: `entries.as_mut_ptr()` seeds
/// `base`, `base.add(current)` extends to `item`, `item.read()` is then
/// `core::ptr::read`.)
fn rhs_extends_raw_ptr(line: &str, ptr_vars: &BTreeMap<String, (i32, String)>) -> bool {
    // Every path to `true` below runs through `ptr_vars.contains_key(recv)`, so
    // an EMPTY ledger can only fall through to `false`. Check it first: this fn
    // runs on every `let <ident> = …` line of the corpus, and only two fns in
    // the whole tree ever bind a proven raw pointer, so the 11 `format!`s + 11
    // line scans below are computed and discarded for essentially every call.
    if ptr_vars.is_empty() {
        return false;
    }
    for m in RAW_PTR_METHODS {
        let pat = format!(".{m}(");
        let mut from = 0;
        while let Some(rel) = line[from..].find(&pat) {
            let dot = from + rel;
            from = dot + pat.len();
            if let Some(recv) = ident_ending_at(line, dot)
                && ptr_vars.contains_key(recv)
            {
                return true;
            }
        }
    }
    false
}

/// Is `name` declared as a raw pointer (`*const`/`*mut`, possibly behind
/// `&`/`&mut`) in the fn signature text? (`input_ptr: &mut *const u8` — the
/// vendored-lz4 pointer-walk params.)
fn param_is_raw_ptr(sig: &str, name: &str) -> bool {
    let pat = format!("{name}: ");
    let mut from = 0;
    while let Some(rel) = sig[from..].find(&pat) {
        let at = from + rel;
        from = at + pat.len();
        if at > 0 {
            let prev = sig.as_bytes()[at - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                continue; // `_input_ptr: ` must not match `input_ptr: `.
            }
        }
        let ty = sig[at + pat.len()..].trim_start();
        let ty = ty
            .strip_prefix("&mut ")
            .or_else(|| ty.strip_prefix('&'))
            .unwrap_or(ty)
            .trim_start();
        if ty.starts_with("*const ") || ty.starts_with("*mut ") {
            return true;
        }
    }
    false
}

/// Byte index of the `(` matching the `)` at `close` (scanning backwards).
fn match_paren_back(line: &str, close: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = close;
    loop {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Raw-pointer evidence for the zero-arg `.read()` whose `.` sits at `dot`:
/// `Some(evidence text)` iff the receiver is lexically PROVEN a raw pointer
/// (named receiver with a live raw-ptr `let` binding or a raw-ptr fn-signature
/// declaration; or the direct result of a [`RAW_PTR_METHODS`] call). The site
/// is then `core::ptr::read`, categorized and EXCLUDED from the mutex graph.
fn raw_ptr_read_evidence(
    line: &str,
    dot: usize,
    sig: &str,
    ptr_vars: &BTreeMap<String, (i32, String)>,
) -> Option<String> {
    if let Some(name) = ident_ending_at(line, dot) {
        if let Some((_, span)) = ptr_vars.get(name) {
            return Some(format!(
                "receiver `{name}` proven a raw pointer by its binding at {span}"
            ));
        }
        if param_is_raw_ptr(sig, name) {
            return Some(format!(
                "receiver `{name}` declared `*const`/`*mut` in the fn signature"
            ));
        }
        return None;
    }
    // Chain shape: `….m(…).read()` where m produces a raw pointer.
    let bytes = line.as_bytes();
    if dot == 0 || bytes[dot - 1] != b')' {
        return None;
    }
    let open = match_paren_back(line, dot - 1)?;
    let m = ident_ending_at(line, open)?;
    let m_start = open - m.len();
    if m_start == 0 || bytes[m_start - 1] != b'.' || !RAW_PTR_METHODS.contains(&m) {
        return None;
    }
    Some(format!(
        "receiver is the result of `.{m}(…)`, a raw-pointer-producing method"
    ))
}

/// Method names the token vocabulary already covers at CALL sites — a
/// guard-returning method with one of these names (e.g. `Subscribers::lock`)
/// needs no helper registration when METHOD-shaped (callers spell the token).
const STANDARD_METHOD_NAMES: &[&str] = &[
    "lock",
    "lock_or_recover",
    "read",
    "write",
    "try_lock",
    "try_read",
    "try_write",
];

// ---------------------------------------------------------------------------
// Site model
// ---------------------------------------------------------------------------

/// One lock-acquisition site.
struct AcqSite {
    /// Resolved lexical identity (`term`, `store`, …) or `None` = UNKNOWN.
    identity: Option<String>,
    /// The matched token, for the diagnostics (`term_lock(`, `.read()`, …).
    kind: &'static str,
    /// Can this acquisition block? (`try_*` cannot.)
    blocking: bool,
    /// Repo-relative `file:line` (first physical line of the statement).
    span: String,
    /// Enclosing fn name.
    fn_name: String,
    /// The (trimmed, capped) statement text, for the diagnostics.
    excerpt: String,
    /// `Some(binding span)` when the receiver is lexically PROVEN to be a
    /// `std::fs::File` (see [`is_file_binding_rhs`]): the acquisition is then
    /// the OS ADVISORY file lock, not an in-process mutex — reported in its
    /// own category (with the evidence span) and EXCLUDED from the mutex order
    /// graph; never UNKNOWN, never silently dropped.
    advisory: Option<String>,
    /// `Some(audit)` when this is a registered [`VocabularyInterior`] site:
    /// the bare-`self` delegation inside an implementation of the acquisition
    /// vocabulary — identity lives at the CALL sites; the interior keeps a
    /// unique per-site node (same cycle posture as UNKNOWN), audited here.
    vocab: Option<&'static str>,
    /// `Some(evidence)` when the receiver is lexically PROVEN a raw pointer
    /// (see [`raw_ptr_read_evidence`]): the site is `core::ptr::read`, not an
    /// RwLock acquisition — reported in its own category (with the evidence)
    /// and EXCLUDED from the mutex order graph; never UNKNOWN, never dropped.
    raw_ptr: Option<String>,
    /// `Some(namespace)` when the site lives in a scanned vendored crate
    /// (vendored-identity mode): its resolved identity is already
    /// namespace-prefixed, its UNKNOWN node is namespaced too, and its counts
    /// are summarized per crate.
    namespace: Option<&'static str>,
}

impl AcqSite {
    /// Does this site participate in the mutex order graph? OS file-advisory
    /// locks (cross-process waits) and raw-pointer `ptr::read` sites (not
    /// locks at all) are categorized + listed but never graphed.
    fn graphed(&self) -> bool {
        self.advisory.is_none() && self.raw_ptr.is_none()
    }

    /// Graph node name: the identity (namespace-prefixed for vendored sites),
    /// a registered vocabulary-interior node, or a per-site UNKNOWN (both
    /// unique — such a site must be SEEN but can never unify into a cycle).
    fn node(&self) -> String {
        if self.vocab.is_some() {
            return format!("callers-of(`{}`)@{}", self.fn_name, self.span);
        }
        match (&self.identity, self.namespace) {
            (Some(id), _) => id.clone(),
            (None, Some(ns)) => format!("{ns}::UNKNOWN@{}", self.span),
            (None, None) => format!("UNKNOWN@{}", self.span),
        }
    }
}

/// One witness for an `A -> B` edge: where A was being held, and where B was
/// acquired (possibly via a one-hop call).
struct EdgeWitness {
    hold_span: String,
    hold_fn: String,
    hold_excerpt: String,
    acq_span: String,
    acq_fn: String,
    acq_excerpt: String,
    /// `Some("call to `f` at file:line")` for the interprocedural hop.
    via: Option<String>,
}

// ---------------------------------------------------------------------------
// Lexical preprocessing
// ---------------------------------------------------------------------------

/// Mask string literals (`"…"`, honoring `\"` escapes) and char literals with
/// spaces, so token scans and brace counting never fire inside literal text
/// (log messages mentioning `.lock()`, `'{'` chars, format strings with
/// braces). Lifetimes (`'a`) are left intact. Raw strings are not special-cased
/// (rare; covered by "string/char literals masked" in the precision note).
pub(crate) fn mask_literals(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                out.push(b'"');
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        out.extend_from_slice(b"  ");
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        out.push(b'"');
                        i += 1;
                        break;
                    }
                    out.push(b' ');
                    i += 1;
                }
            }
            b'\'' => {
                // Char literal `'x'` / `'\x'`; anything else (a lifetime) passes.
                if i + 2 < bytes.len() && bytes[i + 2] == b'\'' && bytes[i + 1] != b'\\' {
                    out.extend_from_slice(b"   ");
                    i += 3;
                } else if i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
                    out.extend_from_slice(b"    ");
                    i += 4;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}

/// Blank out `#[cfg(test)]`- and `#[cfg(kani)]`-gated items (in-file test
/// mods, test-only fns, kani verification harnesses) so the census graphs only
/// the SHIPPED lock discipline: test code never ships, and kani code compiles
/// only under cargo-kani's `--cfg kani` — neither is ever loaded into the GUI
/// process. Line count is preserved (blanked lines become empty), so spans
/// stay correct. Uses the same rustfmt closing-brace-at-item-indent invariant
/// as the fn segmenter.
fn mask_cfg_test_items(text: &str) -> String {
    mask_gated_items(text, &["#[cfg(test)]", "#[cfg(kani)]"])
}

/// The generalized item-blanking pass behind [`mask_cfg_test_items`]: blank
/// every item whose immediately-preceding attribute line (trimmed) EXACTLY
/// matches one of `gates`. Shared with the wasm-process census, whose shipped
/// surface additionally excludes `#[test]` fns and
/// `#[cfg(not(target_arch = "wasm32"))]` items (native-only code that the
/// wasm32 build never compiles). Exact-match is the fail-closed direction: an
/// unrecognized cfg spelling is NOT masked, so a hazard token under it still
/// fails the census and forces a human re-audit (never a silent skip).
pub(crate) fn mask_gated_items(text: &str, gates: &[&str]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![true; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if !gates.contains(&t) {
            i += 1;
            continue;
        }
        let indent = lines[i].len() - t.len();
        keep[i] = false;
        // Skip any further attributes, then find the gated item line.
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim_start().starts_with("#[") {
            keep[j] = false;
            j += 1;
        }
        if j >= lines.len() {
            break;
        }
        keep[j] = false;
        let item = strip_line_comment(lines[j]);
        if item.matches('{').count() > item.matches('}').count() {
            // Block item (mod/fn/impl): blank until the `<indent>}` close.
            let close = format!("{}}}", " ".repeat(indent));
            let mut k = j + 1;
            while k < lines.len() {
                keep[k] = false;
                if lines[k] == close {
                    break;
                }
                k += 1;
            }
            i = k + 1;
        } else {
            i = j + 1;
        }
    }
    let mut out = String::with_capacity(text.len());
    for (idx, line) in lines.iter().enumerate() {
        if keep[idx] {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A logical statement line: rustfmt chain continuations (lines starting with
/// `.`) joined onto their receiver line, so `self`↵`.store`↵`.read()` scans as
/// `self.store.read()`. Keeps the FIRST physical line number for the span.
struct LogicalLine {
    text: String,
    lineno: usize,
}

/// Join a fn body's physical lines (already comment-stripped) into logical
/// lines. `start_line` is the 1-based file line of `body[0]`.
fn logical_lines(body: &[String], start_line: usize) -> Vec<LogicalLine> {
    let mut out: Vec<LogicalLine> = Vec::new();
    for (i, raw) in body.iter().enumerate() {
        let masked = mask_literals(raw);
        let t = masked.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('.')
            && let Some(prev) = out.last_mut()
        {
            prev.text.push_str(t);
            continue;
        }
        out.push(LogicalLine {
            text: t.to_string(),
            lineno: start_line + i,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Acquisition + identity resolution
// ---------------------------------------------------------------------------

/// A raw acquisition found on one logical line, ordered by byte position.
struct RawAcq {
    pos: usize,
    kind: &'static str,
    blocking: bool,
    identity: Option<String>,
}

/// Resolve the lexical identity of the receiver whose final `.` sits at byte
/// `dot`. `store.read()` / `self.store.read()` => `store`; `proxies().write()`
/// => `proxies` (the zero-arg accessor-fn idiom for statics); `self` /
/// `self.0` / single-letter locals => None (UNKNOWN — honestly unresolvable).
fn resolve_receiver(line: &str, dot: usize) -> Option<String> {
    if let Some(seg) = ident_ending_at(line, dot) {
        if seg == "self" || seg.chars().all(|c| c.is_ascii_digit()) || seg.len() == 1 {
            return None;
        }
        return Some(seg.to_string());
    }
    // Receiver ends in `()` — an accessor call like `proxies()` / `store()`.
    let bytes = line.as_bytes();
    if dot < 2 || bytes[dot - 1] != b')' || bytes[dot - 2] != b'(' {
        return None;
    }
    let callee = ident_ending_at(line, dot - 2)?;
    if callee.len() == 1 {
        return None;
    }
    Some(callee.to_string())
}

/// All acquisitions on one logical line, in byte order. Registered
/// guard-returning helpers (`term_lock(`, `lock_fonts(`, …) are direct
/// acquisitions of their declared identity (`term_lock`'s own interior
/// `term.lock()` resolves to the same identity — one lock, one node). A helper
/// name immediately preceded by `fn` is the definition, not an acquisition.
fn acquisitions_on(line: &str) -> Vec<RawAcq> {
    let mut out = Vec::new();
    for h in GUARD_HELPERS {
        // Scan for the symbol's first byte and confirm with `starts_with`,
        // rather than `find(&format!("{}(", h.symbol))`. The needle is a
        // compile-time constant, but the old spelling paid for it once per
        // LOGICAL LINE OF THE WHOLE CORPUS (~1259 files): a heap `String` per
        // helper, plus a fresh two-way `StrSearcher` (critical-factorization
        // setup) per helper — setup that costs more than scanning the ~100-byte
        // line it enables. Three helpers x every line.
        //
        // Equivalence: on a full match `from` advances past the whole
        // `symbol(` token exactly as `find` + `token.len()` did; on a partial
        // match it advances one byte, which is strictly more conservative than
        // `find`, so the match set and its order are unchanged.
        let sym = h.symbol;
        let first = sym.as_bytes()[0];
        let mut from = 0;
        while let Some(rel) = line.as_bytes()[from..].iter().position(|&c| c == first) {
            // `at` is a char boundary: an ASCII byte can only occur at one.
            let at = from + rel;
            if !line[at..].starts_with(sym) || !line[at + sym.len()..].starts_with('(') {
                from = at + 1;
                continue;
            }
            from = at + sym.len() + 1;
            let boundary = at == 0 || {
                let prev = line.as_bytes()[at - 1];
                !(prev.is_ascii_alphanumeric() || prev == b'_')
            };
            if !boundary || line[..at].trim_end().ends_with("fn") {
                continue;
            }
            out.push(RawAcq {
                pos: at,
                kind: h.symbol,
                blocking: true,
                identity: Some(h.identity.to_string()),
            });
        }
    }
    // ONE `.`-scan for the whole table instead of seven `str::find`s (seven
    // more `StrSearcher` constructions per line — the single largest cost in
    // the lock-order census profile). Sound by the SCAN INVARIANT documented on
    // [`ACQ_METHODS`]: a token can only begin at a `.`, so visiting every `.`
    // sees every match; no token contains a second `.`, so the old
    // `from = dot + token.len()` skip could never have hidden a later start;
    // and no token is a prefix of another, so at most one matches per `.` and
    // the stable `sort_by_key` below cannot reorder anything. (Guard-helper
    // tokens never start with `.`, so they can never tie one of these
    // positions either.)
    for (dot, _) in line.match_indices('.') {
        let tail = &line[dot..];
        for (token, blocking) in ACQ_METHODS {
            if tail.starts_with(token) {
                out.push(RawAcq {
                    pos: dot,
                    kind: token,
                    blocking: *blocking,
                    identity: resolve_receiver(line, dot),
                });
            }
        }
    }
    out.sort_by_key(|a| a.pos);
    out
}

/// Byte index of the matching `)` for the `(` at `open`, if on this line.
fn match_paren(line: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, b) in line.bytes().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Byte index just past the acquisition call's closing paren. Method tokens
/// carry their own `()`; helper tokens (no leading `.`) need the argument
/// parens matched.
fn call_end(line: &str, acq: &RawAcq) -> usize {
    if acq.kind.starts_with('.') {
        acq.pos + acq.kind.len()
    } else {
        let open = acq.pos + acq.kind.len();
        match_paren(line, open).map_or(line.len(), |c| c + 1)
    }
}

/// Does the guard produced by this acquisition survive as the statement's
/// FINAL value? True iff everything between the call and the end of the
/// statement is a poison adapter (`.unwrap()`, `.expect(..)`,
/// `.unwrap_or_else(..)`) or `?`. Anything else (`.by_local(..)`, a `,`, a
/// closing paren) consumes the guard WITHIN the statement, so a `let` binds
/// the read-out value, not the guard.
fn guard_is_final_value(line: &str, acq: &RawAcq) -> bool {
    let bytes = line.as_bytes();
    let mut i = call_end(line, acq);
    loop {
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'?') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b';' || line[i..].starts_with("else") {
            return true; // statement end (let-else diverges; the guard is bound)
        }
        if bytes[i] != b'.' {
            return false;
        }
        let ident: String = line[i + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !GUARD_ADAPTERS.contains(&ident.as_str()) {
            return false;
        }
        let open = i + 1 + ident.len();
        if bytes.get(open) != Some(&b'(') {
            return false;
        }
        let Some(close) = match_paren(line, open) else {
            // The adapter's argument spans lines (`unwrap_or_else(|p| {` …):
            // the statement DOES end in the adapter chain, so the guard is
            // bound. (Erring toward "bound" is the fail-closed direction:
            // more tracked holds, never fewer.)
            return true;
        };
        i = close + 1;
    }
}

/// How a logical line binds the guards acquired on it.
enum LineKind {
    /// `let <var> = …` / `let <pattern> = …` (var carried when plain-ident):
    /// an acquisition that is the statement's final value is block-scoped.
    Let(Option<String>),
    /// `match … {` / `if let … {` / `while let … {`: scrutinee temporaries
    /// (guards included) live for the whole construct.
    MatchLike,
    /// Anything else: acquisitions live for their own statement.
    Other,
}

/// Classify a logical line's binding shape.
fn classify_line(line: &str) -> LineKind {
    let t = line.trim_start_matches(['}', ' ']);
    let t = t.strip_prefix("else ").unwrap_or(t).trim_start();
    if (t.starts_with("match ") || t.starts_with("if let ") || t.starts_with("while let "))
        && t.ends_with('{')
    {
        return LineKind::MatchLike;
    }
    if let Some(rest) = t.strip_prefix("let ") {
        // Reach through the refutable wrappers of let-else (`let Ok(g) = …`,
        // `let Some(mut g) = …`) to the inner binding.
        let rest = ["Ok(", "Some("]
            .iter()
            .find_map(|w| rest.strip_prefix(w))
            .unwrap_or(rest);
        let rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
        let var: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if var == "_" {
            return LineKind::Other; // `let _ = lock()` drops the guard at once.
        }
        let after = rest[var.len()..].trim_start_matches(')').trim_start();
        if !var.is_empty() && (after.starts_with('=') || after.starts_with(':')) {
            return LineKind::Let(Some(var));
        }
        return LineKind::Let(None); // destructuring pattern: anonymous binding.
    }
    LineKind::Other
}

/// Callee names invoked on this logical line in the shapes the one-hop pass
/// trusts: free-fn/path `name(` and `self.name(` (an `other.name(` method call
/// is type-ambiguous and excluded — see the precision note). Returns
/// `(byte position, name)`; keyword pseudo-calls and macro bangs excluded.
fn held_call_targets(line: &str) -> Vec<(usize, String)> {
    const KEYWORDS: &[&str] = &[
        "if", "while", "for", "match", "return", "fn", "loop", "unsafe", "move", "let", "else",
        "in", "as", "await", "Some", "Ok", "Err", "None", "drop",
    ];
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b'(' {
            continue;
        }
        let Some(name) = ident_ending_at(line, idx) else {
            continue;
        };
        if KEYWORDS.contains(&name)
            || GUARD_HELPERS.iter().any(|h| h.symbol == name)
            || STANDARD_METHOD_NAMES.contains(&name)
        {
            // Guard helpers and the standard acquisition methods are modeled
            // as DIRECT acquisitions — treating them as one-hop callees too
            // would double-count every site against every same-named fn.
            continue;
        }
        let start = idx - name.len();
        if line[..start].trim_end().ends_with("fn") {
            continue; // a definition, not a call
        }
        if start > 0 && bytes[start - 1] == b'.' {
            // Method call: only trust receiver chains rooted at `self`
            // (`self.name(`, `self.field.name(` — same-object resolution);
            // `other.name(` is type-ambiguous and excluded.
            let mut at = start - 1;
            let rooted_at_self = loop {
                let Some(seg) = ident_ending_at(line, at) else {
                    break false;
                };
                let seg_start = at - seg.len();
                if seg == "self" {
                    break true;
                }
                if seg_start == 0 || bytes[seg_start - 1] != b'.' {
                    break false;
                }
                at = seg_start - 1;
            };
            if !rooted_at_self {
                continue;
            }
        }
        out.push((idx, name.to_string()));
    }
    out
}

// ---------------------------------------------------------------------------
// Per-fn scan: sites, intra-fn edges, held calls
// ---------------------------------------------------------------------------

/// Everything the census learned about one fn.
struct FnLockFacts {
    name: String,
    /// Repo-relative `file:line` of the definition.
    span: String,
    /// Does the signature return a `…Guard` (lexical match)? Combined with a
    /// direct acquisition, this is the acquire-and-return helper shape whose
    /// callers hold invisibly — it must be registered in [`GUARD_HELPERS`].
    returns_guard: bool,
    /// Is the fn METHOD-shaped (first parameter `self`)? Only a method may
    /// claim the standard-name exemption from [`GUARD_HELPERS`] registration:
    /// callers of `fn lock(&self)` spell `receiver.lock()` (token-captured);
    /// callers of a FREE `fn lock(m: &Mutex<T>)` spell `lock(&X)` — no token,
    /// invisible holds.
    takes_self: bool,
    /// Indices into the global site table.
    acq: Vec<usize>,
    /// Intra-fn held-acquire pairs: (holder site, acquired site).
    edges: Vec<(usize, usize)>,
    /// Calls made while holding: (holder site, callee name, call `file:line`).
    held_calls: Vec<(usize, String, String)>,
}

/// Lexical guard-return detection over the signature region (the def line up
/// to the line that opens the body): a `->` whose return type mentions
/// `Guard`. Multi-line rustfmt signatures put `) -> Type {` on its own line,
/// which this covers.
fn returns_guard(body: &[String]) -> bool {
    for line in body {
        if let Some(p) = line.rfind("->")
            && line[p..].contains("Guard")
        {
            return true;
        }
        if line.contains('{') {
            break;
        }
    }
    false
}

/// Lexical method-shape detection over the signature region: does the first
/// parameter bind `self` (`&self`, `&mut self`, `&'a self`, `self`,
/// `mut self`, `self: Arc<Self>`)? Free fns return false.
fn takes_self(body: &[String]) -> bool {
    let mut sig = String::new();
    for line in body {
        sig.push_str(line);
        sig.push(' ');
        if line.contains('{') {
            break;
        }
    }
    let Some(open) = sig.find('(') else {
        return false;
    };
    let after = sig[open + 1..].trim_start();
    let after = after.strip_prefix("&mut ").unwrap_or(after);
    let after = after.strip_prefix('&').unwrap_or(after);
    let after = if let Some(rest) = after.strip_prefix('\'') {
        // `&'a self` / `&'a mut self`: skip the lifetime token.
        rest.split_once(' ').map_or("", |(_, r)| r)
    } else {
        after
    };
    let after = after.trim_start();
    let after = after.strip_prefix("mut ").unwrap_or(after);
    after.starts_with("self,")
        || after.starts_with("self)")
        || after.starts_with("self:")
        || after.starts_with("self ")
}

/// A live block-scoped guard during the scan.
struct LiveGuard {
    site: usize,
    /// Bound var name when the pattern was a plain ident (for `drop`/shadow).
    var: Option<String>,
    /// Dies when a line's end-of-line brace depth drops below this.
    binding_depth: i32,
    /// `let … = <acquire> else { … }`: the guard binds only AFTER the else
    /// block (inside it the pattern did not match — nothing is held). The
    /// guard stays inactive (no edges, no held calls) until the line depth
    /// returns to `binding_depth`.
    active: bool,
}

/// Scan one fn body. `sites` is the global site table (appended to).
/// `namespace` is `Some(prefix)` in vendored-identity mode: every resolved
/// identity is prefixed `prefix::…` so foreign receiver names can never merge
/// across the namespace boundary.
fn scan_fn(
    name: &str,
    file_rel: &str,
    span: &str,
    body: &[String],
    namespace: Option<&'static str>,
    sites: &mut Vec<AcqSite>,
) -> FnLockFacts {
    let start_line: usize = span
        .rsplit(':')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let lines = logical_lines(body, start_line);
    let mut facts = FnLockFacts {
        name: name.to_string(),
        span: span.to_string(),
        returns_guard: returns_guard(body),
        takes_self: takes_self(body),
        acq: Vec::new(),
        edges: Vec::new(),
        held_calls: Vec::new(),
    };
    let mut live: Vec<LiveGuard> = Vec::new();
    // Locals lexically PROVEN to hold a std::fs::File: name -> (binding brace
    // depth, binding span). A `.lock()`/`.try_lock()` on one of these is the
    // OS advisory file lock, not a mutex acquisition. Fail-closed both ways:
    // a rebind to anything else removes the evidence; a block close removes
    // every binding from inside the block.
    let mut file_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    // Locals lexically PROVEN to hold a raw pointer (same ledger discipline):
    // a zero-arg `.read()` on one of these is `core::ptr::read`, not RwLock.
    let mut ptr_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    // The signature region (def line through the body-opening line), for the
    // raw-pointer PARAM evidence (`input_ptr: &mut *const u8`).
    let sig_text: String = {
        let mut s = String::new();
        for line in body {
            s.push_str(line);
            s.push(' ');
            if line.contains('{') {
                break;
            }
        }
        s
    };
    let mut depth: i32 = 0;
    for ll in &lines {
        let line = &ll.text;
        let opens = i32::try_from(line.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(line.matches('}').count()).unwrap_or(0);
        let depth_end = depth + opens - closes;
        // 1. `drop(var)` ends a guard's life explicitly.
        live.retain(|g| {
            g.var.as_ref().is_none_or(|v| {
                !line.contains(&format!("drop({v})")) && !line.contains(&format!("drop(&{v})"))
            })
        });
        // 2. Shadowing: a `let g = …` rebind drops the prior guard named `g`.
        let kind = classify_line(line);
        if let LineKind::Let(Some(var)) = &kind {
            live.retain(|g| g.var.as_ref() != Some(var));
        }
        // 3. Acquisitions on this line, left to right. Each BLOCKING
        //    acquisition is an edge target for every live guard and for every
        //    earlier acquisition on the same statement. An OS FILE-ADVISORY
        //    acquisition (receiver proven std::fs::File) is categorized but
        //    joins NO mutex edges — in either direction (rationale in the
        //    summary and the precision note).
        let raw = acquisitions_on(line);
        let mut this_line: Vec<usize> = Vec::new();
        for r in &raw {
            let site_idx = sites.len();
            let advisory = if matches!(r.kind, ".lock()" | ".try_lock()") {
                ident_ending_at(line, r.pos)
                    .and_then(|recv| file_vars.get(recv))
                    .map(|(_, binding_span)| binding_span.clone())
            } else {
                None
            };
            // Raw-pointer evidence: a zero-arg `.read()` on a lexically-proven
            // raw pointer is `core::ptr::read`, not an RwLock acquisition.
            let raw_ptr = if r.kind == ".read()" {
                raw_ptr_read_evidence(line, r.pos, &sig_text, &ptr_vars)
            } else {
                None
            };
            // A registered vocabulary interior: bare-`self` acquisition inside
            // `fn <symbol>` at its registered def_file (and nothing else).
            let vocab = VOCABULARY_INTERIORS
                .iter()
                .find(|v| {
                    r.identity.is_none()
                        && advisory.is_none()
                        && raw_ptr.is_none()
                        && v.symbol == name
                        && v.def_file == file_rel
                        && r.kind.starts_with('.')
                        && ident_ending_at(line, r.pos) == Some("self")
                })
                .map(|v| v.audit);
            let mut excerpt: String = line.chars().take(160).collect();
            if excerpt.len() < line.len() {
                excerpt.push('…');
            }
            // Vendored-identity mode: the identity lives in the crate's own
            // namespace (categorized advisory/raw-ptr sites are excluded from
            // the graph anyway and keep their local name in the listing).
            let identity = match (namespace, &r.identity) {
                (Some(ns), Some(id)) if advisory.is_none() && raw_ptr.is_none() => {
                    Some(format!("{ns}::{id}"))
                }
                _ => r.identity.clone(),
            };
            sites.push(AcqSite {
                identity,
                kind: r.kind,
                blocking: r.blocking,
                span: format!("{file_rel}:{}", ll.lineno),
                fn_name: name.to_string(),
                excerpt,
                advisory,
                vocab,
                raw_ptr,
                namespace,
            });
            facts.acq.push(site_idx);
            if r.blocking && sites[site_idx].graphed() {
                for g in live.iter().filter(|g| g.active) {
                    facts.edges.push((g.site, site_idx));
                }
                for &earlier in &this_line {
                    if sites[earlier].graphed() {
                        facts.edges.push((earlier, site_idx));
                    }
                }
            }
            this_line.push(site_idx);
        }
        // 4. Calls made while holding (active guards, plus same-line guards
        //    acquired at an earlier byte position than the call).
        if live.iter().any(|g| g.active) || !this_line.is_empty() {
            for (pos, callee) in held_call_targets(line) {
                let call_span = format!("{file_rel}:{}", ll.lineno);
                for g in live.iter().filter(|g| g.active) {
                    facts
                        .held_calls
                        .push((g.site, callee.clone(), call_span.clone()));
                }
                for (k, r) in raw.iter().enumerate() {
                    if r.pos < pos && sites[this_line[k]].graphed() {
                        facts
                            .held_calls
                            .push((this_line[k], callee.clone(), call_span.clone()));
                    }
                }
            }
        }
        // 5. Promote block-scoped acquisitions to live guards: a `let`-bound
        //    guard (the acquisition is the statement's final value) or a
        //    match-like scrutinee guard. A let-ELSE guard (`… else {` opens a
        //    divergent block in which the pattern did NOT match) binds
        //    INACTIVE and activates only when the else block closes.
        let is_let_else =
            matches!(kind, LineKind::Let(_)) && line.ends_with('{') && line.contains(" else ");
        for (k, r) in raw.iter().enumerate() {
            let (block_scoped, var, binding_depth) = match &kind {
                LineKind::MatchLike => (true, None, depth_end.max(depth)),
                LineKind::Let(var) => (guard_is_final_value(line, r), var.clone(), depth),
                LineKind::Other => (false, None, depth),
            };
            if block_scoped && sites[this_line[k]].graphed() {
                live.push(LiveGuard {
                    site: this_line[k],
                    var,
                    binding_depth,
                    active: !is_let_else,
                });
            }
        }
        // 5b. Evidence ledgers: `let <var> = <std::fs::File construction>`
        //     marks the var as an OS file; `let <var> = <raw-ptr construction>`
        //     marks it a raw pointer. A rebind of the same name to anything
        //     WITHOUT that evidence removes it (fail-closed — a mutex must
        //     never inherit stale File or raw-pointer evidence).
        if let LineKind::Let(Some(var)) = &kind {
            if is_file_binding_rhs(line) {
                file_vars.insert(var.clone(), (depth, format!("{file_rel}:{}", ll.lineno)));
            } else {
                file_vars.remove(var);
            }
            if is_raw_ptr_binding_rhs(line) || rhs_extends_raw_ptr(line, &ptr_vars) {
                ptr_vars.insert(var.clone(), (depth, format!("{file_rel}:{}", ll.lineno)));
            } else {
                ptr_vars.remove(var);
            }
        }
        // 6. Block ends kill the guards bound inside them; a let-else guard
        //    whose else block just closed (depth back at its binding depth)
        //    becomes active.
        depth = depth_end;
        live.retain(|g| depth >= g.binding_depth);
        file_vars.retain(|_, (d, _)| depth >= *d);
        ptr_vars.retain(|_, (d, _)| depth >= *d);
        for g in &mut live {
            if depth == g.binding_depth {
                g.active = true;
            }
        }
    }
    facts
}

// ---------------------------------------------------------------------------
// The census run
// ---------------------------------------------------------------------------

/// Append the WHY + repair block for a lock-order cycle. `invert` names the
/// minority edge (fewest witness sites) — the single edge whose inversion
/// breaks the cycle with the least churn.
fn append_cycle_repair(log: &mut String, invert: &str) {
    let _ = writeln!(
        log,
        "    WHY THIS IS L0-DEADLOCK: two threads taking these locks in opposite\n\
         \x20        orders can each hold one half and block forever on the other (the\n\
         \x20        classic ABBA). Every thread that later parks behind either lock —\n\
         \x20        including the winit main thread — then stalls unrecoverably: no\n\
         \x20        input, no redraw, no quit. This entry is PREVENTIVE (no shipped\n\
         \x20        ABBA incident; the census exists so there never is one).\n\
         \x20   HOW TO REPAIR (pick one; there is NO waiver channel, by design —\n\
         \x20   an allowlisted cycle would be a standing deadlock):\n\
         \x20     1. IMPOSE THE CANONICAL ORDER: keep the majority direction and invert\n\
         \x20        the minority edge — here `{invert}` (fewest witness sites).\n\
         \x20        At its HOLD site, end the first guard (drop(g), a scoped block, or\n\
         \x20        clone the needed data out) BEFORE the second acquisition, or hoist\n\
         \x20        the second acquisition ABOVE the first, so every path agrees on\n\
         \x20        one global order.\n\
         \x20     2. NARROW THE GUARD: if the held guard only feeds a read, copy the\n\
         \x20        value out and drop the guard before acquiring the next lock (the\n\
         \x20        census tracks `drop(guard)` and scoped blocks).\n\
         \x20     3. MERGE: if the two locks in truth protect ONE invariant, put that\n\
         \x20        state behind one lock so the pair (and the ordering question)\n\
         \x20        disappears."
    );
}

/// Run the lock-order census over the aterm checkout at `root` (the directory
/// holding `crates/`). Pure function of the source tree — no network, no
/// toolchain, no build artifacts — safe inside a build script and safe to
/// point at ANY checkout (a worktree of a historical commit included).
pub fn run_lock_order_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== gate lockorder (lock-order census: L0-DEADLOCK, lock-graph sense) ===\n\
         \x20   root: {}",
        root.display()
    );

    // ---- [OB-7] Derive the scan set (the obligation's own scope) from the
    // workspace manifests — fail-closed: an unclassifiable dependency graph
    // must stop the build, never shrink the deadlock domain silently.
    let scan = match crate::scan_set::derive_gui_scan_set(root) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] SCAN-SET DERIVATION FAILED — the census cannot \
                 soundly determine the aterm-gui process closure from the workspace \
                 manifests, so it refuses to scan a guessed set (fail-closed).\n\
                 \x20       {e}\n\
                 gate lockorder: FAILED — 1 obligation violation(s)."
            );
            return CensusOutcome { ok: false, log };
        }
    };
    log.push_str(&crate::scan_set::render_scan_set(&scan));

    // ---- Parse every fn in the scanned crates. ----
    let mut sites: Vec<AcqSite> = Vec::new();
    let mut fns: Vec<FnLockFacts> = Vec::new();
    for crate_dir in &scan.scan_dirs {
        let mut files = Vec::new();
        let _ = collect_rs_files(&root.join(crate_dir), &mut files);
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
            let masked = mask_cfg_test_items(&text);
            let mut parsed = Vec::new();
            parse_source_fns(&masked, &rel, &mut parsed);
            for f in &parsed {
                fns.push(scan_fn(&f.name, &rel, &f.span, &f.body, None, &mut sites));
            }
        }
    }

    // ---- Parse the scanned vendored crates (vendored-identity mode). ----
    // Files under a registered per-platform slice do not compile into the
    // shipped macOS GUI process: their sites are COUNTED under the slice
    // label (scanned into a scratch table, never the graph) so the labeled
    // gap is visible every run without manufacturing edges no shipped
    // process contains.
    // (crate package, slice label, acquisition sites in the slice)
    let mut slice_counts: Vec<(String, &'static str, usize)> = Vec::new();
    for v in &scan.vendored_scanned {
        let crate_root = root.join(&v.crate_dir);
        let mut files = Vec::new();
        let _ = collect_rs_files(&root.join(&v.scan_dir), &mut files);
        files.retain(|p| !is_test_file(p));
        files.sort();
        // slice index (into v.platform_slices) -> scratch site count.
        let mut per_slice = vec![0usize; v.platform_slices.len()];
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let in_crate = file
                .strip_prefix(&crate_root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let slice_idx = v.platform_slices.iter().position(|s| {
                s.paths
                    .iter()
                    .any(|p| in_crate == *p || in_crate.starts_with(&format!("{p}/")))
            });
            let rel = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .into_owned();
            let masked = mask_cfg_test_items(&text);
            let mut parsed = Vec::new();
            parse_source_fns(&masked, &rel, &mut parsed);
            match slice_idx {
                Some(i) => {
                    // Labeled platform slice: count, never graph.
                    let mut scratch: Vec<AcqSite> = Vec::new();
                    for f in &parsed {
                        let _ = scan_fn(
                            &f.name,
                            &rel,
                            &f.span,
                            &f.body,
                            Some(v.namespace),
                            &mut scratch,
                        );
                    }
                    per_slice[i] += scratch.len();
                }
                None => {
                    for f in &parsed {
                        fns.push(scan_fn(
                            &f.name,
                            &rel,
                            &f.span,
                            &f.body,
                            Some(v.namespace),
                            &mut sites,
                        ));
                    }
                }
            }
        }
        for (i, slice) in v.platform_slices.iter().enumerate() {
            slice_counts.push((v.package.clone(), slice.label, per_slice[i]));
        }
    }

    // [OB-7] Fail-closed sanity: a census that finds NO acquisition sites
    // walked nothing (parser rot / wrong root) — that must never pass as
    // "acyclic".
    if sites.is_empty() {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] ZERO lock-acquisition sites found under {} — the census \
             walked nothing (parser broke, or this root is not an aterm checkout?).\n\
             gate lockorder: FAILED — 1 obligation violation(s).",
            scan.scan_dirs.join(", ")
        );
        return CensusOutcome { ok: false, log };
    }

    // fn name -> indices (ALL same-named fns — over-approximation, the same
    // fail-closed posture as the main-loop census).
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (idx, f) in fns.iter().enumerate() {
        by_name.entry(&f.name).or_default().push(idx);
    }

    // [OB-7] Guard-helper registry, fail-closed BOTH ways (the OB-1/OB-3
    // pattern). Forward: every entry still defined at its file AND its own
    // body still acquires the declared identity. Reverse: every
    // acquire-and-return-a-Guard fn in the corpus is either registered or
    // carries a standard method name the token vocabulary already covers —
    // otherwise its callers hold a lock the graph cannot see.
    for h in GUARD_HELPERS {
        if !fn_defined_in(root, h.def_file, h.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] guard helper `{}` is registered but `fn {}` is no longer \
                 defined in {} — a STALE registration cannot keep modeling callers' holds. \
                 Update def_file if it moved, or remove the entry (and re-run the census).",
                h.symbol, h.symbol, h.def_file
            );
            failures += 1;
            continue;
        }
        let interior_ok = by_name.get(h.symbol).is_some_and(|idxs| {
            idxs.iter().any(|&i| {
                fns[i]
                    .acq
                    .iter()
                    .any(|&s| sites[s].identity.as_deref() == Some(h.identity))
            })
        });
        if !interior_ok {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] guard helper `{}` is registered with identity `{}` but its \
                 own body no longer acquires that identity — the registration has drifted \
                 from the code. Re-audit the helper and fix the entry.",
                h.symbol, h.identity
            );
            failures += 1;
        }
    }
    // [OB-7] Vocabulary-interior registry, fail-closed BOTH ways (see the
    // struct doc): the symbol must be a standard method name (else callers
    // would escape the token vocabulary and the categorization would HIDE
    // holds), the fn must still be defined at its file, and its body must
    // still contain the bare-`self` acquisition the audit describes.
    for v in VOCABULARY_INTERIORS {
        if !STANDARD_METHOD_NAMES.contains(&v.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] vocabulary-interior registration `{}` is NOT a standard \
                 acquisition method name — its callers would not be captured by the token \
                 vocabulary, so categorizing its interior would hide real holds. Remove or \
                 fix the entry.",
                v.symbol
            );
            failures += 1;
            continue;
        }
        if !fn_defined_in(root, v.def_file, v.symbol) {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] vocabulary interior `{}` is registered but `fn {}` is no \
                 longer defined in {} — a STALE registration cannot keep describing the \
                 delegation. Update def_file if it moved, or remove the entry (and re-run).",
                v.symbol, v.symbol, v.def_file
            );
            failures += 1;
            continue;
        }
        let live = sites
            .iter()
            .any(|s| s.vocab.is_some() && s.fn_name == v.symbol && s.span.starts_with(v.def_file));
        if !live {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] vocabulary interior `{}` ({}) is registered but its body \
                 no longer contains the bare-`self` acquisition the audit describes — the \
                 registration has drifted from the code. Re-audit the fn and fix the entry.",
                v.symbol, v.def_file
            );
            failures += 1;
        }
    }
    for f in &fns {
        if !f.returns_guard
            || f.acq
                .iter()
                .all(|&s| !sites[s].blocking || !sites[s].graphed())
            || f.acq.is_empty()
            || (STANDARD_METHOD_NAMES.contains(&f.name.as_str()) && f.takes_self)
            || GUARD_HELPERS.iter().any(|h| h.symbol == f.name)
        {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] UNREGISTERED GUARD-RETURNING HELPER — `fn {}` ({}) acquires a \
             lock AND returns a `…Guard`, so its CALLERS hold that lock invisibly (no \
             `.lock()` token at their call sites) and the census cannot place their \
             held-acquire edges. Register it in GUARD_HELPERS \
             (crates/aterm-census/src/lock_order.rs) with its lock identity — registration \
             EXTENDS coverage; it is not a waiver. (A FREE fn with a standard method name \
             — `lock`/`read`/… — is NOT exempt: `lock(&X)` call sites carry no token. If \
             it wraps several different locks, inline the standard vocabulary at its call \
             sites instead, e.g. `MutexExt::lock_or_recover`.)",
            f.name, f.span
        );
        failures += 1;
    }
    if failures > 0 {
        let _ = write!(log, "{LOCK_PRECISION_NOTE}");
        let _ = writeln!(
            log,
            "gate lockorder: FAILED — {failures} obligation violation(s) (registry/coverage \
             stage; the graph stage did not run because its ground truth is broken)."
        );
        return CensusOutcome { ok: false, log };
    }

    // ---- Assemble the edge set: intra-fn pairs + the one-hop calls. ----
    // (from node, to node) -> witnesses.
    let mut edges: BTreeMap<(String, String), Vec<EdgeWitness>> = BTreeMap::new();
    let mut intra_pairs = 0usize;
    let mut hop_pairs = 0usize;
    for f in &fns {
        for &(hold, acq) in &f.edges {
            intra_pairs += 1;
            edges
                .entry((sites[hold].node(), sites[acq].node()))
                .or_default()
                .push(EdgeWitness {
                    hold_span: sites[hold].span.clone(),
                    hold_fn: sites[hold].fn_name.clone(),
                    hold_excerpt: sites[hold].excerpt.clone(),
                    acq_span: sites[acq].span.clone(),
                    acq_fn: sites[acq].fn_name.clone(),
                    acq_excerpt: sites[acq].excerpt.clone(),
                    via: None,
                });
        }
        for (hold, callee, call_span) in &f.held_calls {
            let Some(callee_fns) = by_name.get(callee.as_str()) else {
                continue;
            };
            // One hop: every DIRECT blocking acquisition in every same-named
            // callee (distinct identities once per call site).
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for &g in callee_fns {
                for &acq in &fns[g].acq {
                    if !sites[acq].blocking
                        || !sites[acq].graphed()
                        || !seen.insert(sites[acq].node())
                    {
                        continue;
                    }
                    hop_pairs += 1;
                    edges
                        .entry((sites[*hold].node(), sites[acq].node()))
                        .or_default()
                        .push(EdgeWitness {
                            hold_span: sites[*hold].span.clone(),
                            hold_fn: sites[*hold].fn_name.clone(),
                            hold_excerpt: sites[*hold].excerpt.clone(),
                            acq_span: sites[acq].span.clone(),
                            acq_fn: sites[acq].fn_name.clone(),
                            acq_excerpt: sites[acq].excerpt.clone(),
                            via: Some(format!("call to `{callee}` at {call_span}")),
                        });
                }
            }
        }
    }

    // ---- Site / identity bookkeeping (the honest summary). ----
    // Advisory (OS file lock) and raw-pointer (ptr::read) sites are their own
    // categories: not mutex identities, not UNKNOWN, not in the graph — but
    // always counted + listed.
    let advisory_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.advisory.is_some()).collect();
    let raw_ptr_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.raw_ptr.is_some()).collect();
    let vocab_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.vocab.is_some()).collect();
    let blocking = sites.iter().filter(|s| s.blocking && s.graphed()).count();
    let tries = sites.iter().filter(|s| !s.blocking && s.graphed()).count();
    // UNKNOWNs split by origin: aterm sites are LISTED (each is a naming task
    // for this repo); vendored sites are per-site nodes too but SUMMARIZED
    // per crate (upstream receivers cannot be renamed here — the honest
    // handling at scale is the count plus a per-crate ledger, not hundreds of
    // listings).
    let unknown: Vec<&AcqSite> = sites
        .iter()
        .filter(|s| {
            s.identity.is_none() && s.graphed() && s.vocab.is_none() && s.namespace.is_none()
        })
        .collect();
    let vendored_unknown: Vec<&AcqSite> = sites
        .iter()
        .filter(|s| {
            s.identity.is_none() && s.graphed() && s.vocab.is_none() && s.namespace.is_some()
        })
        .collect();
    let mut identities: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &sites {
        if let Some(id) = &s.identity
            && s.graphed()
        {
            *identities.entry(id.as_str()).or_default() += 1;
        }
    }

    // ---- Self-edges: re-entrancy suspects (std locks are not re-entrant). ----
    for ((id, to), wit) in &edges {
        if id != to {
            continue;
        }
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] LOCK RE-ENTRANCY SUSPECT — `{id}` acquired while `{id}` is \
             already held.\n\
             \x20        std::sync locks are NOT re-entrant: if both sites hit the SAME lock\n\
             \x20        instance this deadlocks outright; if they are DIFFERENT instances of\n\
             \x20        the `{id}` class (e.g. two sessions' term mutexes), the nesting needs\n\
             \x20        an instance ORDER this census cannot verify — restructure so the\n\
             \x20        first guard ends before the next `{id}` is taken."
        );
        for w in wit.iter().take(3) {
            let via = w
                .via
                .as_deref()
                .map(|v| format!(" [via {v}]"))
                .unwrap_or_default();
            let _ = writeln!(
                log,
                "        HOLD:    {} (fn `{}`)  {}\n\
                 \x20       ACQUIRE: {} (fn `{}`){}  {}",
                w.hold_span, w.hold_fn, w.hold_excerpt, w.acq_span, w.acq_fn, via, w.acq_excerpt
            );
        }
        if wit.len() > 3 {
            let _ = writeln!(log, "        … and {} more witness site(s).", wit.len() - 3);
        }
        failures += 1;
    }

    // ---- Cycle detection (Tarjan SCC) over the full graph. ----
    let nodes: Vec<String> = {
        let mut set = BTreeSet::new();
        for (a, b) in edges.keys() {
            set.insert(a.clone());
            set.insert(b.clone());
        }
        set.into_iter().collect()
    };
    let index_of: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (a, b) in edges.keys() {
        if a != b {
            adj[index_of[a.as_str()]].push(index_of[b.as_str()]);
        }
    }
    for scc in tarjan_sccs(&adj).iter().filter(|c| c.len() > 1) {
        failures += 1;
        let members: BTreeSet<usize> = scc.iter().copied().collect();
        let cycle_edges: Vec<(&(String, String), &Vec<EdgeWitness>)> = edges
            .iter()
            .filter(|((a, b), _)| {
                a != b
                    && members.contains(&index_of[a.as_str()])
                    && members.contains(&index_of[b.as_str()])
            })
            .collect();
        let mut names: Vec<&str> = scc.iter().map(|&i| nodes[i].as_str()).collect();
        names.sort_unstable();
        // The minority edge: fewest witnesses — the one to invert.
        let invert = cycle_edges
            .iter()
            .min_by_key(|(_, w)| w.len())
            .map(|((a, b), _)| format!("{a} -> {b}"))
            .unwrap_or_default();
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] L0-DEADLOCK OBLIGATION VIOLATED — the global lock graph has a \
             CYCLE.\n\
             \x20   LOCKS IN THE CYCLE: {{{}}}\n\
             \x20   EVERY EDGE OF THE CYCLE (\"B acquired while A held\"), both sites each:",
            names.join(", ")
        );
        for ((a, b), wit) in &cycle_edges {
            let _ = writeln!(
                log,
                "     EDGE {a} -> {b}  ({} witness site(s)):",
                wit.len()
            );
            for w in wit.iter().take(3) {
                let via = w
                    .via
                    .as_deref()
                    .map(|v| format!(" [via {v}]"))
                    .unwrap_or_default();
                let _ = writeln!(
                    log,
                    "       HOLD:    {} (fn `{}`)  {}\n\
                     \x20      ACQUIRE: {} (fn `{}`){}  {}",
                    w.hold_span,
                    w.hold_fn,
                    w.hold_excerpt,
                    w.acq_span,
                    w.acq_fn,
                    via,
                    w.acq_excerpt
                );
            }
            if wit.len() > 3 {
                let _ = writeln!(log, "       … and {} more witness site(s).", wit.len() - 3);
            }
        }
        append_cycle_repair(&mut log, &invert);
    }

    if failures > 0 {
        let _ = write!(log, "{LOCK_PRECISION_NOTE}");
        let _ = writeln!(
            log,
            "gate lockorder: FAILED — {failures} obligation violation(s). A lock-order \
             cycle has NO waiver channel (L0-DEADLOCK: none, ever) — it can only be \
             fixed. This census blocks BOTH `cargo xtask gate lockorder` and the \
             `cargo build` of tools/freeze-safety-gate."
        );
        return CensusOutcome { ok: false, log };
    }

    // ---- GREEN summary (with the honesty ledger). ----
    let _ = writeln!(
        log,
        "gate lockorder: GREEN — {} acquisition site(s) across {} workspace crate(s) + {} \
         vendored crate(s) ({} blocking, {} try_*, {} OS file-advisory, {} raw-pointer \
         ptr::read); {} resolved identities; {} UNKNOWN-identity site(s) + {} vendored \
         UNKNOWN site(s); {} audited vocabulary-interior site(s); {} held-acquire \
         pair(s) ({} intra-fn, {} via one-hop calls; {} distinct ordered identity \
         pairs); 0 self-edges; global lock graph ACYCLIC.",
        sites.len(),
        scan.scan_dirs.len(),
        scan.vendored_scanned.len(),
        blocking,
        tries,
        advisory_sites.len(),
        raw_ptr_sites.len(),
        identities.len(),
        unknown.len(),
        vendored_unknown.len(),
        vocab_sites.len(),
        intra_pairs + hop_pairs,
        intra_pairs,
        hop_pairs,
        edges.len(),
    );
    let mut ids: Vec<(&str, usize)> = identities.into_iter().collect();
    ids.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let listed: Vec<String> = ids.iter().map(|(n, c)| format!("{n}({c})")).collect();
    let _ = writeln!(log, "    identities: {}", listed.join(" "));
    if !edges.is_empty() {
        let _ = writeln!(
            log,
            "    held-acquire edges (the ORDER ledger — every nested pair, with its first \
             witness):"
        );
        for ((a, b), wit) in edges.iter().take(30) {
            let w = &wit[0];
            let via = w
                .via
                .as_deref()
                .map(|v| format!(" [via {v}]"))
                .unwrap_or_default();
            let _ = writeln!(
                log,
                "      {a} -> {b}  ({} witness(es); first: hold {} fn `{}`, acquire {} fn \
                 `{}`{})",
                wit.len(),
                w.hold_span,
                w.hold_fn,
                w.acq_span,
                w.acq_fn,
                via
            );
        }
        if edges.len() > 30 {
            let _ = writeln!(log, "      … and {} more edge(s).", edges.len() - 30);
        }
    }
    if !unknown.is_empty() {
        let _ = writeln!(
            log,
            "    UNKNOWN-identity sites (counted, never dropped — each is a unique node \
             that cannot unify into a cycle; resolve by naming the receiver):"
        );
        for s in unknown.iter().take(20) {
            let _ = writeln!(
                log,
                "      {} (fn `{}`) {}  {}",
                s.span, s.fn_name, s.kind, s.excerpt
            );
        }
        if unknown.len() > 20 {
            let _ = writeln!(log, "      … and {} more.", unknown.len() - 20);
        }
    }
    if !scan.vendored_scanned.is_empty() {
        let _ = writeln!(
            log,
            "    vendored [patch] crates in the graph (vendored-identity mode: every \
             identity in the crate's own namespace, so foreign receiver names can \
             never merge with aterm identities or each other; cross-boundary holds \
             are visible only at DIRECT call sites, one hop):"
        );
        for v in &scan.vendored_scanned {
            let graphed = sites
                .iter()
                .filter(|s| s.namespace == Some(v.namespace) && s.graphed())
                .count();
            let unknowns = vendored_unknown
                .iter()
                .filter(|s| s.namespace == Some(v.namespace))
                .count();
            let categorized = sites
                .iter()
                .filter(|s| s.namespace == Some(v.namespace) && !s.graphed())
                .count();
            let mut line = format!(
                "      {} ({}): {graphed} graphed site(s), {unknowns} UNKNOWN, \
                 {categorized} categorized (advisory/raw-ptr)",
                v.package, v.crate_dir
            );
            let slices: Vec<String> = slice_counts
                .iter()
                .filter(|(pkg, _, _)| *pkg == v.package)
                .map(|(_, label, n)| format!("{label} {n}"))
                .collect();
            if !slices.is_empty() {
                let _ = write!(
                    line,
                    "; per-platform slices NOT compiled into the shipped macOS GUI \
                     process (sites counted, never graphed): {}",
                    slices.join(", ")
                );
            }
            let _ = writeln!(log, "{line}");
        }
    }
    if !vendored_unknown.is_empty() {
        let mut per_crate: BTreeMap<&str, Vec<&AcqSite>> = BTreeMap::new();
        for s in &vendored_unknown {
            if let Some(ns) = s.namespace {
                per_crate.entry(ns).or_default().push(s);
            }
        }
        let _ = writeln!(
            log,
            "    vendored UNKNOWN-identity sites, summarized per crate (each is a unique \
             namespaced node that cannot unify into a cycle; upstream receivers cannot \
             be renamed here, so the count is the ledger — first 3 spans each):"
        );
        for (ns, list) in &per_crate {
            let sample: Vec<&str> = list.iter().take(3).map(|s| s.span.as_str()).collect();
            let _ = writeln!(
                log,
                "      {ns}: {} site(s)  [{}]",
                list.len(),
                sample.join(", ")
            );
        }
    }
    if !vocab_sites.is_empty() {
        let _ = writeln!(
            log,
            "    audited vocabulary-interior site(s) — registered implementations of the \
             acquisition\n\
             \x20    vocabulary itself, delegating through bare `self`: the receiver IS each \
             caller's lock,\n\
             \x20    so the identity is resolved at every CALL site by the very token the fn \
             implements; the\n\
             \x20    interior keeps a unique per-site graph node (edges reported, can never \
             close a cycle).\n\
             \x20    Registry: VOCABULARY_INTERIORS (fail-closed both ways):"
        );
        for s in &vocab_sites {
            let _ = writeln!(
                log,
                "      {} (fn `{}`) {}  {}\n\
                 \x20       audit: {}",
                s.span,
                s.fn_name,
                s.kind,
                s.excerpt,
                s.vocab.unwrap_or("?")
            );
        }
    }
    if !advisory_sites.is_empty() {
        let _ = writeln!(
            log,
            "    OS file-advisory lock site(s) — std::fs::File advisory locks (flock-class), \
             a cross-PROCESS\n\
             \x20    rendezvous, NOT an in-process mutex: their waits are against other \
             processes, which an\n\
             \x20    in-process lock graph cannot model, so they are EXCLUDED from the mutex \
             order graph\n\
             \x20    (each site listed with its File evidence, never silently dropped):"
        );
        for s in &advisory_sites {
            let _ = writeln!(
                log,
                "      {} (fn `{}`) {}  {}  [receiver proven std::fs::File by its binding \
                 at {}]",
                s.span,
                s.fn_name,
                s.kind,
                s.excerpt,
                s.advisory.as_deref().unwrap_or("?")
            );
        }
    }
    if !raw_ptr_sites.is_empty() {
        let _ = writeln!(
            log,
            "    raw-pointer ptr::read site(s) — `core::ptr::read` on a lexically-proven raw \
             pointer, NOT an\n\
             \x20    RwLock acquisition: a raw pointer is not a lock, so these sites are \
             EXCLUDED from the\n\
             \x20    mutex order graph (each listed with its evidence, never silently \
             dropped):"
        );
        for s in &raw_ptr_sites {
            let _ = writeln!(
                log,
                "      {} (fn `{}`) {}  {}  [{}]",
                s.span,
                s.fn_name,
                s.kind,
                s.excerpt,
                s.raw_ptr.as_deref().unwrap_or("?")
            );
        }
    }
    let vendored_dirs: Vec<&str> = scan
        .vendored_scanned
        .iter()
        .map(|v| v.scan_dir.as_str())
        .collect();
    let _ = writeln!(
        log,
        "    scope: lexical receiver-name identities over {}; vendored-identity mode \
         over {}; one interprocedural hop (precision limits: \
         docs/temporal-safety-gate.md).",
        scan.scan_dirs.join(", "),
        vendored_dirs.join(", ")
    );
    CensusOutcome { ok: true, log }
}

/// Iterative Tarjan strongly-connected components. Returns each SCC as a list
/// of node indices (singletons included; the caller filters).
fn tarjan_sccs(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut sccs: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if index[start] != usize::MAX {
            continue;
        }
        // Explicit DFS frames: (node, next child position).
        let mut frames: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(v, ci)) = frames.last() {
            if ci == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = adj[v].get(ci) {
                frames.last_mut().expect("frame exists").1 += 1;
                if index[w] == usize::MAX {
                    frames.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                frames.pop();
                if let Some(&(parent, _)) = frames.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(comp);
                }
            }
        }
    }
    sccs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Write `files` (repo-relative path, contents) under a fresh temp root and
    /// return it. The caller removes it.
    fn synth_tree(name: &str, files: &[(String, String)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("aterm-lock-census-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
            std::fs::write(&path, contents).expect("write synth file");
        }
        root
    }

    /// Everything a synthetic tree needs to pass the census's fail-closed
    /// ground-truth checks: the derivable workspace manifests (root + gui +
    /// aterm-types + the reviewed-patch fixture — the scan set is DERIVED, so
    /// a tree without manifests cannot be scanned at all), the GUARD_HELPERS
    /// definitions (each helper defined at its registered file, interior
    /// acquiring its declared identity), and the registered vocabulary
    /// interior.
    /// Crates a synthetic tree must contain BEYOND the base fixture (aterm-gui +
    /// aterm-types), because a `GUARD_HELPERS` entry is registered in them: the
    /// registry's interior check reads the SCANNED corpus, so a helper file
    /// written into a tree that never scans that crate would fail the very check
    /// it exists to satisfy. Every synthetic tree that composes its own manifest
    /// set must splice these in too.
    const SYNTH_HELPER_CRATES: &[(&str, &str)] = &[("aterm-log", "")];

    fn synth_helper_files() -> Vec<(String, String)> {
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(SYNTH_HELPER_CRATES);
        files.push((
            // term_lock's registered def_file (moved main.rs -> lib.rs in the
            // ONE-binary refactor); must match GUARD_HELPERS above.
            "crates/aterm-gui/src/lib.rs".to_string(),
            "pub(crate) fn term_lock(term: &Mutex<Terminal>) -> TermGuard<'_> {\n    \
             term.lock().unwrap()\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/tray_raster.rs".to_string(),
            "fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {\n    \
             chrome_fonts().lock().unwrap()\n}\n"
                .to_string(),
        ));
        files.push((
            // Mirrors the shipping helper and its plural ring identity; this
            // fixture makes a stale singular registration fail closed.
            "crates/aterm-gui/src/control_query.rs".to_string(),
            "fn search_cache_lock() -> MutexGuard<'static, VecDeque<SearchSnapshot>> {\n    \
             SEARCH_SNAPSHOTS.lock().unwrap()\n}\n"
                .to_string(),
        ));
        files.push((
            // The workspace's ONE process-environment mutation lock
            // (`aterm_log::env`). `scoped`/`scoped_unset` hold its guard across a
            // caller-supplied body, which is precisely the invisible hold this
            // registry models.
            "crates/aterm-log/src/lib.rs".to_string(),
            "fn env_lock() -> MutexGuard<'static, ()> {\n    \
             ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())\n}\n"
                .to_string(),
        ));
        // The registered vocabulary interior (VOCABULARY_INTERIORS):
        // `fn lock_or_recover` with its bare-`self` acquisition, at its
        // registered def_file, so the fail-closed registry checks hold.
        files.push((
            "crates/aterm-types/src/mutex_ext.rs".to_string(),
            "impl<T> MutexExt<T> for Mutex<T> {\n    \
             fn lock_or_recover(&self) -> MutexGuard<'_, T> {\n        \
             match self.lock() {\n            Ok(guard) => guard,\n            \
             Err(poisoned) => poisoned.into_inner(),\n        }\n    }\n}\n"
                .to_string(),
        ));
        files
    }

    /// Replace one synthetic file's contents (by exact repo-relative path).
    fn replace_file(files: &mut [(String, String)], rel: &str, contents: &str) {
        let slot = files
            .iter_mut()
            .find(|(p, _)| p == rel)
            .unwrap_or_else(|| panic!("fixture has no {rel}"));
        slot.1 = contents.to_string();
    }

    fn run_synth(name: &str, gui_extra: &str) -> CensusOutcome {
        let mut files = synth_helper_files();
        files.push((
            "crates/aterm-gui/src/extra.rs".to_string(),
            gui_extra.to_string(),
        ));
        let root = synth_tree(name, &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        out
    }

    /// Like [`run_synth`] but with extra files planted anywhere in the tree
    /// (vendored-identity-mode tests plant sources under `vendor/…/src`).
    fn run_synth_files(name: &str, gui_extra: &str, extra: &[(&str, &str)]) -> CensusOutcome {
        let mut files = synth_helper_files();
        files.push((
            "crates/aterm-gui/src/extra.rs".to_string(),
            gui_extra.to_string(),
        ));
        for (rel, contents) in extra {
            files.push((rel.to_string(), contents.to_string()));
        }
        let root = synth_tree(name, &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        out
    }

    // ------------------------------------------------------------------
    // Walker unit tests
    // ------------------------------------------------------------------

    #[test]
    fn receiver_resolution_handles_fields_accessors_and_unknowns() {
        let acqs = acquisitions_on("let g = self.store.read().unwrap();");
        assert_eq!(acqs.len(), 1);
        assert_eq!(acqs[0].identity.as_deref(), Some("store"));
        let acqs = acquisitions_on("proxies().write().unwrap().insert(child, entry);");
        assert_eq!(acqs[0].identity.as_deref(), Some("proxies"));
        // `self.0` / single-letter receivers are UNKNOWN, not dropped.
        let acqs = acquisitions_on("*self.0.lock().unwrap() = None;");
        assert_eq!(acqs.len(), 1);
        assert!(acqs[0].identity.is_none());
        let acqs = acquisitions_on("a.read().unwrap().clone()");
        assert!(acqs[0].identity.is_none());
        // term_lock is the registered helper, identity `term`; its def is not
        // an acquisition.
        let acqs = acquisitions_on("let mut t = term_lock(&s.term);");
        assert_eq!(acqs[0].identity.as_deref(), Some("term"));
        assert!(acquisitions_on("pub(crate) fn term_lock(term: &Mutex<Terminal>) {").is_empty());
    }

    #[test]
    fn guard_final_value_distinguishes_bound_guard_from_consumed_value() {
        // Bound guard: only poison adapters between the call and `;`.
        let line = "let g = store.read().unwrap_or_else(|p| p.into_inner());";
        let acq = &acquisitions_on(line)[0];
        assert!(guard_is_final_value(line, acq));
        // Consumed within the statement: `.by_local(..)` follows the adapters.
        let line = "let gone = self.store.read().unwrap_or_else(|p| p.into_inner()).by_local(s);";
        let acq = &acquisitions_on(line)[0];
        assert!(!guard_is_final_value(line, acq));
        // Helper call bound directly.
        let line = "let mut term = term_lock(&s.term);";
        let acq = &acquisitions_on(line)[0];
        assert!(guard_is_final_value(line, acq));
    }

    #[test]
    fn literals_are_masked_before_token_scans() {
        assert!(
            acquisitions_on(&mask_literals(r#"log!("call .lock() and term_lock(x)");"#)).is_empty()
        );
        let masked = mask_literals(r#"let b = matches!(c, '{');"#);
        assert_eq!(masked.matches('{').count(), 0);
    }

    // ------------------------------------------------------------------
    // Synthetic-tree tests: the census end-to-end, GREEN and RED.
    // ------------------------------------------------------------------

    #[test]
    fn synthetic_diamond_without_cycle_is_green() {
        // alpha -> beta, alpha -> gamma, beta -> delta, gamma -> delta: a
        // diamond, no cycle — must pass with the edges enumerated.
        let out = run_synth(
            "diamond",
            "fn ab() {\n    let g = alpha_lock.lock().unwrap();\n    \
             let h = beta_lock.lock().unwrap();\n}\n\
             fn ac() {\n    let g = alpha_lock.lock().unwrap();\n    \
             let h = gamma_lock.lock().unwrap();\n}\n\
             fn bd() {\n    let g = beta_lock.lock().unwrap();\n    \
             let h = delta_lock.lock().unwrap();\n}\n\
             fn cd() {\n    let g = gamma_lock.lock().unwrap();\n    \
             let h = delta_lock.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "diamond must be GREEN:\n{}", out.log);
        assert!(out.log.contains("ACYCLIC"), "log:\n{}", out.log);
        assert!(
            out.log.contains("alpha_lock -> beta_lock"),
            "edges must be enumerated; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_abba_cycle_is_red_naming_both_sites() {
        let out = run_synth(
            "abba",
            "fn take_ab() {\n    let g = alpha_lock.lock().unwrap();\n    \
             let h = beta_lock.lock().unwrap();\n}\n\
             fn take_ba() {\n    let g = beta_lock.lock().unwrap();\n    \
             let h = alpha_lock.lock().unwrap();\n}\n",
        );
        assert!(!out.ok, "ABBA must be RED:\n{}", out.log);
        assert!(out.log.contains("[OB-7]"), "log:\n{}", out.log);
        assert!(out.log.contains("CYCLE"), "log:\n{}", out.log);
        // EVERY edge of the cycle, both sites each.
        assert!(
            out.log.contains("EDGE alpha_lock -> beta_lock"),
            "log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("EDGE beta_lock -> alpha_lock"),
            "log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("crates/aterm-gui/src/extra.rs:2")
                && out.log.contains("crates/aterm-gui/src/extra.rs:3")
                && out.log.contains("crates/aterm-gui/src/extra.rs:6")
                && out.log.contains("crates/aterm-gui/src/extra.rs:7"),
            "both sites of both edges must be named; log:\n{}",
            out.log
        );
        assert!(out.log.contains("HOW TO REPAIR"), "log:\n{}", out.log);
        assert!(
            out.log.contains("NO waiver channel"),
            "the no-waiver stance must be stated; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_one_hop_interprocedural_cycle_is_red() {
        // f holds alpha and calls helper_b (which locks beta); g holds beta
        // and calls helper_a (which locks alpha): a cycle only visible through
        // the one-hop pass.
        let out = run_synth(
            "onehop",
            "fn f() {\n    let g = alpha_lock.lock().unwrap();\n    helper_b();\n}\n\
             fn helper_b() {\n    beta_lock.lock().unwrap().push(1);\n}\n\
             fn g() {\n    let g = beta_lock.lock().unwrap();\n    helper_a();\n}\n\
             fn helper_a() {\n    alpha_lock.lock().unwrap().push(1);\n}\n",
        );
        assert!(!out.ok, "one-hop ABBA must be RED:\n{}", out.log);
        assert!(
            out.log.contains("[via call to `helper_b`"),
            "log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[via call to `helper_a`"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_self_edge_is_red_reentrancy_suspect() {
        let out = run_synth(
            "reentrant",
            "fn nest_same() {\n    let g = alpha_lock.lock().unwrap();\n    \
             let h = alpha_lock.lock().unwrap();\n}\n",
        );
        assert!(!out.ok, "A-while-A must be RED:\n{}", out.log);
        assert!(out.log.contains("RE-ENTRANCY SUSPECT"), "log:\n{}", out.log);
        assert!(out.log.contains("alpha_lock"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_unknown_identity_is_counted_never_dropped() {
        let out = run_synth(
            "unknown",
            "fn opaque() {\n    let g = m.lock().unwrap();\n    let h = beta_lock.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "an UNKNOWN cannot cycle — GREEN:\n{}", out.log);
        assert!(
            out.log.contains("1 UNKNOWN-identity site(s)"),
            "the UNKNOWN must be counted; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("UNKNOWN-identity sites"),
            "the UNKNOWN must be listed; log:\n{}",
            out.log
        );
        // The held-acquire edge from the unknown is still reported.
        assert!(
            out.log
                .contains("UNKNOWN@crates/aterm-gui/src/extra.rs:2 -> beta_lock"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_vocabulary_interior_is_audited_not_unknown() {
        // The registered `lock_or_recover` interior (bare-`self` delegation,
        // present in every synth tree via synth_helper_files) must land in the
        // audited vocabulary-interior category — never UNKNOWN — with its
        // audit note printed and a unique per-site node.
        let out = run_synth(
            "vocab",
            "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("1 audited vocabulary-interior site(s)"),
            "log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("0 UNKNOWN-identity site(s)"),
            "the interior must not count as UNKNOWN; log:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("audit: MutexExt::lock_or_recover, the poison-recovery impl"),
            "the audit note must be rendered; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_stale_vocabulary_registration_is_red() {
        // (a) fn gone from the registered file: RED "no longer defined".
        let mut files = synth_helper_files();
        replace_file(
            &mut files,
            "crates/aterm-types/src/mutex_ext.rs",
            "// no lock_or_recover here\n",
        );
        files.push((
            "crates/aterm-gui/src/extra.rs".to_string(),
            "fn f() {\n    let g = alpha_lock.lock().unwrap();\n}\n".to_string(),
        ));
        let root = synth_tree("stalevocab", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            !out.ok,
            "a stale vocabulary entry must be RED:\n{}",
            out.log
        );
        assert!(
            out.log.contains("vocabulary interior `lock_or_recover`")
                && out.log.contains("no longer defined"),
            "log:\n{}",
            out.log
        );
        // (b) fn present but the bare-`self` acquisition rewired away: RED
        // "no longer contains" (the audit drifted from the code).
        let mut files = synth_helper_files();
        replace_file(
            &mut files,
            "crates/aterm-types/src/mutex_ext.rs",
            "fn lock_or_recover() -> u32 {\n    OTHER.lock().unwrap()\n}\n",
        );
        let root = synth_tree("driftvocab", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            !out.ok,
            "a drifted vocabulary entry must be RED:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("no longer contains the bare-`self` acquisition"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_file_advisory_lock_is_categorized_not_unknown() {
        // `f` is lexically PROVEN to be a std::fs::File (OpenOptions binding),
        // so `f.lock()` is the OS advisory lock — its own category, with the
        // binding evidence printed. `m.lock()` has no such evidence and must
        // stay UNKNOWN (fail-closed).
        let out = run_synth(
            "fileadvisory",
            "fn ledger_lock(path: &Path) -> Option<std::fs::File> {\n    \
             let f = std::fs::OpenOptions::new().read(true).open(path).ok()?;\n    \
             f.lock().ok()?;\n    Some(f)\n}\n\
             fn opaque() {\n    let g = m.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("1 OS file-advisory"),
            "the advisory site must be counted in the summary; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("1 UNKNOWN-identity site(s)"),
            "the File-less receiver must STAY UNKNOWN; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("EXCLUDED from the mutex order graph"),
            "the exclusion rationale must be printed; log:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("proven std::fs::File by its binding at crates/aterm-gui/src/extra.rs:2"),
            "the binding evidence span must be printed; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_file_advisory_lock_joins_no_mutex_edges() {
        // A file lock taken while a mutex is held, then another mutex: the
        // mutex->mutex order survives, but NO edge may touch the advisory
        // site in either direction.
        let out = run_synth(
            "fileexclude",
            "fn hold_mutex_then_flock(path: &Path) {\n    \
             let g = alpha_lock.lock().unwrap();\n    \
             let f = std::fs::File::create(path).unwrap();\n    \
             f.lock().unwrap();\n    \
             let h = beta_lock.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("alpha_lock -> beta_lock"),
            "the mutex order must still be tracked across the flock; log:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("UNKNOWN@"),
            "the advisory site must not appear as an UNKNOWN graph node; log:\n{}",
            out.log
        );
        assert!(out.log.contains("1 OS file-advisory"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_file_evidence_is_fail_closed_on_rebind_and_scope() {
        // (1) `f` rebound to a non-File: the later f.lock() must NOT inherit
        // the stale File evidence (stays UNKNOWN). (2) A block-scoped File
        // binding dies with its block: the outer f.lock() stays UNKNOWN.
        let out = run_synth(
            "filerebind",
            "fn rebind(path: &Path) {\n    \
             let f = std::fs::File::open(path).unwrap();\n    \
             f.lock().unwrap();\n    \
             let f = some_mutex_handle();\n    \
             f.lock().unwrap();\n}\n\
             fn scoped(path: &Path) {\n    {\n        \
             let f = std::fs::File::open(path).unwrap();\n        \
             f.lock().unwrap();\n    }\n    \
             f.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("2 OS file-advisory"),
            "only the evidenced sites are advisory; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("2 UNKNOWN-identity site(s)"),
            "rebound/out-of-scope receivers must fall back to UNKNOWN; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_drop_and_statement_scope_end_the_hold() {
        // (1) drop(g) before the second acquisition; (2) a consumed-value
        // `let` (guard dies at the semicolon); (3) an inner-block guard dying
        // at its close — none of these may produce an alpha edge.
        let out = run_synth(
            "scopes",
            "fn dropped() {\n    let g = alpha_lock.lock().unwrap();\n    drop(g);\n    \
             let h = beta_lock.lock().unwrap();\n}\n\
             fn consumed() {\n    let n = alpha_lock.lock().unwrap().len();\n    \
             let h = beta_lock.lock().unwrap();\n}\n\
             fn scoped() {\n    {\n        let g = alpha_lock.lock().unwrap();\n    }\n    \
             let h = beta_lock.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            !out.log.contains("alpha_lock -> beta_lock"),
            "no alpha edge may survive drop/statement/block scope end; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_multiline_chain_resolves_receiver_and_nests() {
        // rustfmt chain style: the receiver sits lines above `.read()`; the
        // bound guard then covers a later term_lock -> edge store -> term.
        let out = run_synth(
            "chain",
            "fn chained(s: &S) {\n    let g = s\n        .store\n        .read()\n        \
             .unwrap_or_else(|p| p.into_inner());\n    let t = term_lock(&s.term);\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("store -> term"),
            "the chained receiver must resolve and the edge must appear; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_try_lock_is_a_source_but_never_a_target() {
        let out = run_synth(
            "trylock",
            "fn try_then_block() {\n    let Ok(g) = alpha_lock.try_lock() else { return };\n    \
             let h = beta_lock.lock().unwrap();\n}\n\
             fn block_then_try() {\n    let g = beta_lock.lock().unwrap();\n    \
             let h = alpha_lock.try_lock();\n}\n",
        );
        // try-source edge exists; try-target edge must NOT close the cycle.
        assert!(out.ok, "try_lock cannot be the waiting half:\n{}", out.log);
        assert!(
            out.log.contains("alpha_lock -> beta_lock"),
            "a held try-guard is still a hold; log:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("beta_lock -> alpha_lock"),
            "a try acquisition never blocks, so it is no edge target; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_unregistered_guard_returning_helper_is_red() {
        let out = run_synth(
            "helpergap",
            "fn my_guard() -> std::sync::MutexGuard<'static, u32> {\n    \
             MY_LOCK.lock().unwrap()\n}\n",
        );
        assert!(
            !out.ok,
            "an unregistered acquire-and-return helper hides holds:\n{}",
            out.log
        );
        assert!(
            out.log.contains("UNREGISTERED GUARD-RETURNING HELPER"),
            "log:\n{}",
            out.log
        );
        assert!(out.log.contains("my_guard"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_cfg_kani_items_are_masked() {
        // A kani-only ABBA (verification harness code, compiled only under
        // cargo-kani's --cfg kani) must not graph: it is never loaded into the
        // GUI process — the same shipped-discipline rationale as cfg(test).
        let out = run_synth(
            "kanimask",
            "fn shipped() {\n    let g = alpha_lock.lock().unwrap();\n}\n\
             #[cfg(kani)]\nmod verification {\n    fn kani_only_ab() {\n        \
             let g = alpha_lock.lock().unwrap();\n        \
             let h = beta_lock.lock().unwrap();\n    }\n    fn kani_only_ba() {\n        \
             let g = beta_lock.lock().unwrap();\n        \
             let h = alpha_lock.lock().unwrap();\n    }\n}\n",
        );
        assert!(out.ok, "cfg(kani) lock use must not graph:\n{}", out.log);
        assert!(
            !out.log.contains("alpha_lock -> beta_lock"),
            "kani-mod edges must be masked; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_raw_ptr_reads_are_categorized_not_unknown() {
        // All three evidence shapes: (1) chain — the receiver is the result of
        // a raw-pointer-producing method; (2) a live `let` binding constructing
        // a raw pointer; (3) a fn param declared `*const`/`*mut`. A zero-arg
        // `.read()` WITHOUT such evidence keeps its normal resolution
        // (fail-closed): `opaque.read()` stays a resolved RwLock identity.
        let out = run_synth(
            "rawptr",
            "fn chain(buf: &[u8], pos: usize) -> u8 {\n    \
             unsafe { buf.as_ptr().add(pos).read() }\n}\n\
             fn bound(buf: &[u8]) -> u8 {\n    let src_ptr = buf.as_ptr();\n    \
             unsafe { src_ptr.read() }\n}\n\
             fn param(input_ptr: &mut *const u8) -> u8 {\n    \
             unsafe { input_ptr.read() }\n}\n\
             fn no_evidence() {\n    let v = *opaque.read().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("3 raw-pointer ptr::read"),
            "all three evidence shapes must be categorized; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("raw-pointer-producing method")
                && out.log.contains("proven a raw pointer by its binding at")
                && out
                    .log
                    .contains("declared `*const`/`*mut` in the fn signature"),
            "each site must carry its evidence; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("opaque(1)"),
            "the evidence-less zero-arg .read() must KEEP its RwLock resolution \
             (fail-closed); log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("0 UNKNOWN-identity site(s)"),
            "raw-ptr sites must not be UNKNOWN; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_raw_ptr_read_joins_no_mutex_edges() {
        // A ptr::read between two mutex acquisitions: the mutex->mutex order
        // survives; no edge may touch the raw-pointer site in either direction.
        let out = run_synth(
            "rawptredge",
            "fn hold_then_ptr_then_lock(buf: &[u8]) {\n    \
             let g = alpha_lock.lock().unwrap();\n    \
             let b = unsafe { buf.as_ptr().add(1).read() };\n    \
             let h = beta_lock.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("alpha_lock -> beta_lock"),
            "the mutex order must survive across the ptr::read; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("1 raw-pointer ptr::read"),
            "log:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("UNKNOWN@"),
            "the raw-ptr site must not appear as an UNKNOWN graph node; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_free_fn_with_standard_name_is_not_exempt() {
        // A FREE fn named `lock` that acquires and returns a guard: its call
        // sites (`lock(&X)`) carry NO `.lock()` token, so the standard-name
        // exemption must NOT apply (this exact shape hid the aterm-pty Windows
        // registry holds until 2026-07-13). A METHOD named `lock` stays exempt:
        // callers spell `receiver.lock()`, which the vocabulary captures.
        let out = run_synth(
            "freefnlock",
            "fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {\n    \
             m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}\n",
        );
        assert!(
            !out.ok,
            "a free guard-returning fn named `lock` hides its callers' holds:\n{}",
            out.log
        );
        assert!(
            out.log.contains("UNREGISTERED GUARD-RETURNING HELPER"),
            "log:\n{}",
            out.log
        );
        let out = run_synth(
            "methodlock",
            "impl Wrapper {\n    pub fn lock(&self) -> MutexGuard<'_, u32> {\n        \
             self.raw.lock().unwrap()\n    }\n}\n",
        );
        assert!(
            out.ok,
            "a METHOD named `lock` is token-captured at its call sites:\n{}",
            out.log
        );
    }

    // ------------------------------------------------------------------
    // Vendored-identity mode (the OB-7 vendored coverage, 2026-07-13).
    // ------------------------------------------------------------------

    #[test]
    fn synthetic_vendored_identities_are_namespaced_and_never_merge() {
        // aterm nests state -> queue; the vendored winit stub nests
        // queue -> state. Without the per-crate namespace these would merge
        // into a state/queue ABBA; WITH it they are four distinct identities
        // and the graph stays acyclic — and both namespaced identities are
        // visible in the ledger.
        let out = run_synth_files(
            "vendoredns",
            "fn aterm_order() {\n    let g = state.lock().unwrap();\n    \
             let h = queue.lock().unwrap();\n}\n",
            &[(
                "vendor/winit/src/planted.rs",
                "fn winit_order() {\n    let g = queue.lock().unwrap();\n    \
                 let h = state.lock().unwrap();\n}\n",
            )],
        );
        assert!(
            out.ok,
            "namespaced foreign receivers must NOT merge into a cycle:\n{}",
            out.log
        );
        assert!(
            out.log.contains("winit::state(1)") && out.log.contains("winit::queue(1)"),
            "vendored identities must be namespaced in the ledger:\n{}",
            out.log
        );
        assert!(
            out.log.contains("state -> queue") && out.log.contains("winit::queue -> winit::state"),
            "both crates' nestings must be tracked, separately:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_cross_boundary_one_hop_edge_is_graphed() {
        // The winit-callback class, as seen at a DIRECT call site: aterm
        // holds alpha_lock and calls a fn whose (vendored) body acquires a
        // winit-namespaced lock — the cross-boundary edge must be graphed.
        let out = run_synth_files(
            "xboundedge",
            "fn hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
             winit_side_helper();\n}\n",
            &[(
                "vendor/winit/src/planted.rs",
                "fn winit_side_helper() {\n    event_state.lock().unwrap().push(1);\n}\n",
            )],
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("alpha_lock -> winit::event_state"),
            "the aterm-held -> winit-acquire edge must be in the ledger:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_cross_boundary_abba_is_red_across_the_namespace() {
        // THE COUNTERFACTUAL CLASS the vendored mode exists for: one lock on
        // each side of the namespace boundary, taken in opposite orders (the
        // winit-internal hold calling back into aterm code, and aterm code
        // calling into winit while holding). RED, naming both sides' sites.
        let out = run_synth_files(
            "xboundabba",
            "fn hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
             winit_side_helper();\n}\n\
             fn aterm_side_helper() {\n    alpha_lock.lock().unwrap().push(1);\n}\n",
            &[(
                "vendor/winit/src/planted.rs",
                "fn winit_side_helper() {\n    event_state.lock().unwrap().push(1);\n}\n\
                 fn winit_hold_and_call() {\n    \
                 let g = event_state.lock().unwrap();\n    aterm_side_helper();\n}\n",
            )],
        );
        assert!(!out.ok, "a cross-boundary ABBA must be RED:\n{}", out.log);
        assert!(
            out.log.contains("CYCLE")
                && out.log.contains("alpha_lock")
                && out.log.contains("winit::event_state"),
            "the cycle must name both sides of the namespace boundary:\n{}",
            out.log
        );
        assert!(
            out.log.contains("vendor/winit/src/planted.rs:2")
                && out.log.contains("crates/aterm-gui/src/extra.rs:2"),
            "both crates' sites must be named in the diagnostic:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_platform_slice_sites_are_counted_never_graphed() {
        // An ABBA planted inside a registered winit platform slice (linux):
        // that code does not compile into the shipped macOS GUI process, so
        // it must NOT graph (no cycle, no identities) — but its sites must be
        // COUNTED under the slice label (never silent).
        let out = run_synth_files(
            "sliceabba",
            "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
            &[(
                "vendor/winit/src/platform_impl/linux/planted.rs",
                "fn ab() {\n    let g = sctk_state.lock().unwrap();\n    \
                 let h = x11_state.lock().unwrap();\n}\n\
                 fn ba() {\n    let g = x11_state.lock().unwrap();\n    \
                 let h = sctk_state.lock().unwrap();\n}\n",
            )],
        );
        assert!(
            out.ok,
            "platform-slice code links into no shipped process — never graphed:\n{}",
            out.log
        );
        assert!(
            out.log.contains("linux 4"),
            "the slice's sites must be counted under its label:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("winit::sctk_state") && !out.log.contains("winit::x11_state"),
            "slice sites must not appear as graphed identities:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_vendored_unknown_is_summarized_per_crate() {
        // An unresolvable receiver in vendored code: still a unique
        // namespaced node (counted, can never close a cycle), but summarized
        // per crate — upstream receivers cannot be renamed here.
        let out = run_synth_files(
            "vendorunknown",
            "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
            &[(
                "vendor/winit/src/planted.rs",
                "fn opaque(&self) {\n    let g = self.0.lock().unwrap();\n    \
                 let h = event_state.lock().unwrap();\n}\n",
            )],
        );
        assert!(
            out.ok,
            "a vendored UNKNOWN cannot cycle — GREEN:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("0 UNKNOWN-identity site(s) + 1 vendored UNKNOWN site(s)"),
            "the vendored UNKNOWN must be counted separately from aterm's:\n{}",
            out.log
        );
        assert!(
            out.log.contains("winit: 1 site(s)"),
            "the per-crate vendored-UNKNOWN ledger must name the crate:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("winit::UNKNOWN@vendor/winit/src/planted.rs:2 -> winit::event_state"),
            "the vendored unknown's held-acquire edge must still be reported, \
             namespaced:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_propagated_raw_ptr_binding_is_categorized() {
        // The vendored-indexmap shape: `.as_mut_ptr()` seeds `base`, pointer
        // arithmetic (`base.add(i)`) extends the evidence to `item`, so
        // `item.read()` is core::ptr::read — categorized, not a lock
        // identity. Fail-closed: `other.add(i)` with no proven receiver
        // does NOT extend, so `thing.read()` keeps its RwLock resolution.
        let out = run_synth(
            "ptrpropagate",
            "fn walk(entries: &mut [u8]) -> u8 {\n    \
             let base = entries.as_mut_ptr();\n    \
             let item = base.add(1);\n    \
             unsafe { item.read() }\n}\n\
             fn no_evidence(other: &Wrapper) {\n    \
             let thing = other.add(1);\n    \
             let v = *thing.read().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("1 raw-pointer ptr::read")
                && out
                    .log
                    .contains("receiver `item` proven a raw pointer by its binding"),
            "the propagated binding must be categorized with its evidence:\n{}",
            out.log
        );
        assert!(
            out.log.contains("thing(1)"),
            "an unproven receiver must KEEP its RwLock resolution (fail-closed):\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_cfg_test_items_are_masked() {
        let out = run_synth(
            "testmask",
            "fn shipped() {\n    let g = alpha_lock.lock().unwrap();\n}\n\
             #[cfg(test)]\nmod tests {\n    fn test_only_abba() {\n        \
             let g = alpha_lock.lock().unwrap();\n        \
             let h = beta_lock.lock().unwrap();\n    }\n    fn test_only_baab() {\n        \
             let g = beta_lock.lock().unwrap();\n        \
             let h = alpha_lock.lock().unwrap();\n    }\n}\n",
        );
        assert!(out.ok, "cfg(test) lock use must not graph:\n{}", out.log);
        assert!(
            !out.log.contains("alpha_lock -> beta_lock"),
            "test-mod edges must be masked; log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_empty_tree_fails_closed() {
        // Manifests present (the scan set derives fine) but NO lock sites in
        // the whole derived closure: the zero-sites tripwire must fire.
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(&[]);
        files.push((
            "crates/aterm-gui/src/main.rs".to_string(),
            "// empty\n".to_string(),
        ));
        files.push((
            "crates/aterm-types/src/lib.rs".to_string(),
            "// empty\n".to_string(),
        ));
        let root = synth_tree("empty", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            !out.ok,
            "a census that walked nothing must FAIL:\n{}",
            out.log
        );
        assert!(
            out.log.contains("ZERO lock-acquisition"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_tree_without_manifests_fails_closed() {
        // The scan set is DERIVED: a tree whose dependency graph cannot be
        // read must fail the derivation stage loudly, never walk a guess.
        let root = synth_tree(
            "nomanifest",
            &[(
                "crates/aterm-gui/src/main.rs".to_string(),
                "// no Cargo.toml anywhere\n".to_string(),
            )],
        );
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(!out.ok, "derivation must fail closed:\n{}", out.log);
        assert!(
            out.log.contains("SCAN-SET DERIVATION FAILED"),
            "log:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_stale_helper_registration_is_red() {
        // term_lock registered but its file lacks the definition.
        let mut files = synth_helper_files();
        replace_file(
            &mut files,
            "crates/aterm-gui/src/lib.rs",
            "// no term_lock here\n",
        );
        files.push((
            "crates/aterm-gui/src/extra.rs".to_string(),
            "fn f() {\n    let g = alpha_lock.lock().unwrap();\n}\n".to_string(),
        ));
        let root = synth_tree("stalehelper", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            !out.ok,
            "a stale helper registration must be RED:\n{}",
            out.log
        );
        assert!(out.log.contains("no longer defined"), "log:\n{}", out.log);
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
    fn lock_order_census_is_green_on_this_tree() {
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.ok,
            "lock-order census RED on the current tree:\n{}",
            out.log
        );
        assert!(out.log.contains("ACYCLIC"), "log:\n{}", out.log);
    }

    #[test]
    fn no_unknown_identities_on_this_tree() {
        // The UNKNOWN count was driven to ZERO (2026-07-13) by naming every
        // previously-unresolvable receiver and categorizing the one OS
        // file-advisory lock — and HELD at zero through the same-day widening
        // from 8 crates to the full 42-crate GUI-process closure (cfg(kani)
        // masking, the raw-pointer ptr::read category, the pty-Windows
        // lock_or_recover refactor, the Sentinel read_state rename). This
        // pins it there: a new `self.0.lock()` / single-letter receiver
        // reopens the honesty gap and fails here — name the receiver (see the
        // UNKNOWN section's guidance), don't suppress the site.
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log.contains("0 UNKNOWN-identity site(s)"),
            "UNKNOWN-identity sites reappeared — resolve them by naming the \
             receiver, never by suppression:\n{}",
            out.log
        );
    }

    #[test]
    fn file_advisory_locks_are_categorized_on_this_tree() {
        // The two real OS file-advisory sites (`restore::with_restore_lock`'s
        // sibling-lock flock over the restore manifest; the updater's
        // install-ledger lock in aterm-update-core, picked up by the 2026-07-13
        // crate-set widening) must be classified by File EVIDENCE, listed with
        // their binding spans, and excluded from the mutex graph —
        // existence-checked here so the classification cannot silently rot into
        // UNKNOWN (or vanish).
        //
        // NOTE (2026-07-24): this named `crates/aterm-gui/src/kitty_log.rs` for
        // the first site long after that flock moved to `restore.rs` — kitty_log
        // has carried ZERO `.lock()` calls since. The assertion had been failing
        // on `main` for an unknown span, i.e. this gate was dark. Naming the
        // ACTUAL site restores its teeth.
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log.contains("2 OS file-advisory"),
            "expected exactly the restore-manifest + update-core flocks in the \
             advisory category:\n{}",
            out.log
        );
        assert!(
            out.log.contains("crates/aterm-gui/src/restore.rs")
                && out.log.contains("crates/aterm-update-core/src/sys.rs")
                && out.log.contains("proven std::fs::File by its binding at"),
            "each advisory listing must carry its audit evidence:\n{}",
            out.log
        );
    }

    #[test]
    fn lz4_raw_pointer_reads_are_categorized_on_this_tree() {
        // The six real `core::ptr::read` sites — five in the upstream-derived
        // lz4 block codec (kept close to lz4_flex for reviewability, so receiver
        // renames are off the table there) plus the vendored
        // indexmap `extract.rs` site (categorized by the PROPAGATED evidence:
        // `entries.as_mut_ptr()` seeds `base`, `base.add(current)` extends to
        // `item`) — must be classified by raw-pointer EVIDENCE, listed, and
        // excluded from the mutex graph — never UNKNOWN, never misread as
        // RwLock identities.
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log.contains("6 raw-pointer ptr::read"),
            "expected exactly the five vendored-lz4 + one indexmap ptr::read sites:\n{}",
            out.log
        );
        assert!(
            out.log.contains("crates/aterm-lz4/src/block/compress.rs")
                && out.log.contains("crates/aterm-lz4/src/block/decompress.rs")
                && out.log.contains("crates/aterm-lz4/src/sink.rs")
                && out.log.contains("vendor/indexmap/src/inner/extract.rs"),
            "each raw-pointer listing must name its site:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("input_ptr(") && !out.log.contains("source_ptr("),
            "raw pointers must not appear as resolved lock identities:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("indexmap::item("),
            "the indexmap ptr walk must not appear as a resolved lock identity:\n{}",
            out.log
        );
    }

    #[test]
    fn scanned_set_covers_the_full_gui_process_closure() {
        // The scan set is DERIVED (scan_set::derive_gui_scan_set) — the full
        // aterm-gui process surface, currently 45 crates. The exact member
        // list is pinned by scan_set's derived_closure_matches_the_pinned_canary;
        // this asserts the census actually WALKS the derived set and reports
        // its provenance + exclusions in the transcript.
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log
                .contains("across 45 workspace crate(s) + 5 vendored crate(s)"),
            "the census must report the full derived closure + the scanned vendored \
             crates:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("scan set: DERIVED from the workspace manifests"),
            "the derivation provenance must be printed every run:\n{}",
            out.log
        );
        assert!(
            out.log.contains("excluded proc-macro crate(s)")
                && out.log.contains("aterm-error-derive"),
            "the proc-macro exclusion must be reported, never silent:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("vendored [patch] crate(s) SCANNED in vendored-identity mode")
                && out.log.contains("winit (vendor/winit"),
            "the scanned vendored crates must be reported, never silent:\n{}",
            out.log
        );
        assert!(
            out.log.contains("REVIEWED build-time-only")
                && out.log.contains("pkg-config (vendor/pkg-config)"),
            "the build-dep-only classification must be reported, never silent:\n{}",
            out.log
        );
    }

    #[test]
    fn vendored_identity_mode_covers_the_linked_forks_on_this_tree() {
        // The vendored-coverage canary (the vendor trees are pinned in-repo,
        // so these counts are as stable as any other pin; a vendor bump that
        // changes them is exactly the reviewable diff we want). Survey
        // 2026-07-13, winit 0.30.13: the macOS GUI process compiles TWO
        // winit lock sites (event.rs InnerSizeWriter::request_inner_size;
        // macos/window_delegate.rs scale-factor round-trip) — note they are
        // the SAME Arc<Mutex> reached through two receiver names, so they
        // SPLIT (the stated lexical posture, under-reporting the pair as two
        // identities; harmless here since neither nests). winnow's three
        // stderr-stream locks are graphed as `winnow::writer` (behind the
        // never-activated `debug` feature — over-approximation, stated).
        // libm/smol_str: zero sites, the walker re-checks that claim every
        // run. The non-macOS winit backends are labeled slices, counted and
        // never graphed.
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log
                .contains("winit (vendor/winit): 2 graphed site(s), 0 UNKNOWN, 0 categorized"),
            "the winit macOS-slice surface changed — re-audit the vendored \
             registry entry:\n{}",
            out.log
        );
        assert!(
            out.log.contains("winit::inner(1)")
                && out.log.contains("winit::new_inner_size(1)")
                && out.log.contains("winnow::writer(3)"),
            "the vendored identities must be namespaced in the ledger:\n{}",
            out.log
        );
        assert!(
            out.log
                .contains("linux 107, windows 47, web 4, android 0, ios 1, orbital 7"),
            "the per-platform slice counts must be reported (never silent); a \
             changed count means the vendored winit tree changed — re-audit:\n{}",
            out.log
        );
        assert!(
            out.log.contains("+ 0 vendored UNKNOWN site(s)"),
            "vendored UNKNOWNs reappeared — they are summarized per crate below; \
             re-audit the new sites:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_new_gui_dependency_is_scanned_automatically() {
        // THE COUNTERFACTUAL the derivation exists for: a NEW crate enters
        // aterm-gui's dependency graph — with NO census-source change, its
        // lock sites must be scanned (a), and an ABBA cycle inside it must
        // fail the census (b). No human memory in the loop.
        let newdep = ("aterm-newdep", "[dependencies]\n");
        // (a) The new crate's sites are counted and its identities resolved.
        let extras: Vec<(&str, &str)> = std::iter::once(newdep)
            .chain(SYNTH_HELPER_CRATES.iter().copied())
            .collect();
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(&extras);
        files.extend(synth_helper_files().into_iter().filter(|(p, _)| {
            !p.ends_with("Cargo.toml") // keep ONE manifest set (with the new dep)
        }));
        files.push((
            "crates/aterm-newdep/src/lib.rs".to_string(),
            "fn newdep_touch() {\n    let g = newdep_registry.lock().unwrap();\n    \
             let h = newdep_queue.lock().unwrap();\n}\n"
                .to_string(),
        ));
        let root = synth_tree("newdepgreen", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("across 4 workspace crate(s)")
                && out.log.contains("crates/aterm-newdep/src"),
            "the new dependency must be scanned automatically:\n{}",
            out.log
        );
        assert!(
            out.log.contains("newdep_registry(1)")
                && out.log.contains("newdep_registry -> newdep_queue"),
            "its sites and edges must be counted:\n{}",
            out.log
        );
        // (b) An ABBA planted in the NEW crate goes RED — half the cycle in
        // aterm-gui, half in the crate the census never heard of until now.
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(&extras);
        files.extend(
            synth_helper_files()
                .into_iter()
                .filter(|(p, _)| !p.ends_with("Cargo.toml")),
        );
        files.push((
            "crates/aterm-gui/src/extra.rs".to_string(),
            "fn take_ab() {\n    let g = alpha_lock.lock().unwrap();\n    \
             let h = beta_lock.lock().unwrap();\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-newdep/src/lib.rs".to_string(),
            "fn take_ba() {\n    let g = beta_lock.lock().unwrap();\n    \
             let h = alpha_lock.lock().unwrap();\n}\n"
                .to_string(),
        ));
        let root = synth_tree("newdepabba", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            !out.ok,
            "an ABBA half-planted in the new dependency must be RED:\n{}",
            out.log
        );
        assert!(
            out.log.contains("CYCLE") && out.log.contains("crates/aterm-newdep/src/lib.rs:2"),
            "the cycle diagnostic must name the new crate's sites:\n{}",
            out.log
        );
    }

    #[test]
    fn term_identity_dominates_on_this_tree() {
        // Rot canary: the per-session `term` mutex is aterm's dominant lock.
        // If the walker stops seeing it in force, the census went blind — a
        // GREEN from a blind census is worthless.
        let out = run_lock_order_census(&repo_root());
        let ids_line = out
            .log
            .lines()
            .find(|l| l.trim_start().starts_with("identities:"))
            .expect("identities line");
        let count: usize = ids_line
            .split("term(")
            .nth(1)
            .and_then(|r| r.split(')').next())
            .and_then(|n| n.parse().ok())
            .expect("term(<count>) in the identities line");
        assert!(
            count >= 100,
            "expected >=100 `term` acquisition sites, saw {count} — walker rot?\n{ids_line}"
        );
    }
}
