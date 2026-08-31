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
//!      lexical lock IDENTITY (the receiver/accessor name; a `self`-rooted
//!      chain takes the enclosing impl type's — `Spill::0`). Fail-honest: a
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
      - SELF RECEIVERS TAKE THE ENCLOSING IMPL TYPE: a receiver chain rooted at
        `self` has no name of its own, so it borrows the type's — `self.lock()`
        inside `impl Spill` => `Spill::self`; `self.0.lock()` => `Spill::0`;
        `self.pair.1.read()` => `Spill::pair.1`. The qualification is what keeps
        it honest: same-named fields on DIFFERENT types never merge
        (`Spill::0` is not `Queue::0`), and — unlike an UNKNOWN — such a site
        CAN close a cycle. Fail-closed wherever the type is not nameable: a
        blanket `impl<T> .. for T`, a macro fragment, a structural type
        (`&[T]`, a tuple), or one of the std lock/pointer wrappers (an
        `impl .. for Mutex<T>` is the extension-trait shape, where `self` is
        EVERY caller's mutex — one node for the whole process's mutexes would
        invent cycles between locks that never meet). Those keep a unique
        per-site node. The qualifier is the type NAME, so two types sharing a
        name in different modules merge — the same lexical posture (and the
        same over-report risk) the census already takes for two same-named
        fields.
      - SINGLE-LETTER ALIASES: a one-letter local is no identity, but a live
        `let g = &self.spill;` binding whose RHS is a PURE PATH lends `g`
        exactly what that path resolves to (`spill`) — the identity the field
        spelling would already have produced, so the alias can add no merge
        class the census did not already have. Any other RHS (a call, an
        operator, a block) leaves the receiver UNKNOWN; a rebind or a block
        close drops the alias.
      - UNKNOWN, NEVER DROPPED: what stays unresolvable — a `self` chain with
        no nameable enclosing impl type, a tuple field of a LOCAL (`pair.0`), a
        bare single-letter local, a receiver that is a call result — is
        reported as UNKNOWN@site and counted. An UNKNOWN is a unique node (it
        never unifies), so it can appear in reported edges but can never close
        a cycle — the UNKNOWN count is the census's standing honesty gap,
        printed every run.
      - GUARD SCOPE is tracked lexically: a `let g = <acquire>` guard (through
        `unwrap`/`expect`/`unwrap_or_else` adapters only) lives to the end of its
        enclosing block (brace depth), honoring `drop(g)` and shadowing; an
        acquisition consumed inside its statement lives for that statement;
        `match`/`if let`/`while let` scrutinee guards live for the whole
        construct. Guards stored in structs/tuples or moved into
        `thread::spawn` closures are approximated (the spawn body is attributed
        to the spawning fn).
      - INTERPROCEDURAL DEPTH: exactly ONE hop — a call made while a guard is
        held, to a same-corpus fn (free-fn `name(..)`, bare `self.name(..)`, or
        a TYPE-DIRECTED `self.field.name(..)`) whose own body directly
        acquires. The callee's callees are NOT followed. Same-named fns merge
        WITHIN a namespace (over-approximation, same posture as the main-loop
        census); across one they do not — see NAMESPACE BOUNDARY ON THE HOP.
        The callee is chosen BY NAME, so the shapes are limited: a free fn and
        a method of `Self` are fns this corpus defines, while `other.name(..)`
        dispatches on a type this census cannot resolve and is never followed.
        `self.field.name(..)` is followed only when the FIELD's declared type is
        defined in this corpus AND exactly one corpus fn bears the callee's
        name AND that fn is in the call site's own namespace; otherwise the hop
        is counted and LISTED beside UNKNOWN. Following it unconditionally once
        bound std's `Condvar::wait` (which RELEASES its guard) to an unrelated
        same-named fn and reported a lock cycle that does not exist.
      - NAMESPACE BOUNDARY ON THE HOP: the by-name callee table spans the
        workspace crates AND every scanned vendored fork, so a name shared
        across that boundary must not resolve across it. A definition in the
        CALL SITE's own namespace SHADOWS the foreign ones — a bare `start(..)`
        beside a local `fn start` is the local one, never winnow's `fn start`
        (which locks `winnow::writer`); a bare call cannot mean an import that
        collides with a local definition, because that does not compile. Only
        an UNSHADOWED name — one the caller's namespace does not define at all
        — resolves across the boundary, because such a call must be to an
        IMPORT, and that is exactly how the real aterm↔winit hops are seen.
        Refusing the unshadowed ones as well would trade a false edge for a
        SILENT missing one — the worse failure in a deadlock census. Following
        the shadowed ones fails the other way, and worse: an edge minted from a
        name collision can touch a vendored identity, and a cycle through
        vendored code is unrepairable — that code may not be edited, and this
        obligation has no waiver channel.
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
        std::fs::File (`File::open(`/`File::create(`/`OpenOptions::new()…open(`,
        or an explicit `: std::fs::File =` ascription — compiler-enforced)
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
        never merge with an aterm identity or another vendored crate's. The
        namespace is carried by the fn TABLE as well as by the identities, so a
        foreign fn NAME cannot capture an aterm call site either (see NAMESPACE
        BOUNDARY ON THE HOP — identity namespacing alone would not have stopped
        aterm's `start(..)` from inheriting `winnow::writer`).
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
    GuardHelper {
        // The SESSION-CONNECTIONS record store (design §1.3): the per-instance
        // `(dst, src) -> ConnectionRecord` map behind one mutex. `records()` is
        // method-shaped but is NOT one of the standard acquisition names, so
        // its call sites carry no token and every caller's hold would be
        // invisible — registration places those held-acquire edges instead.
        // `self.records.lock()` inside `impl ConnectionTable`, so the identity
        // takes the enclosing impl type exactly as the census's self-receiver
        // rule spells it.
        symbol: "records",
        identity: "records",
        def_file: "crates/aterm-gui/src/connections.rs",
    },
    GuardHelper {
        // The isearch prefix-narrowing stacks (SA-1): a grown query verifies
        // only the previous frame's lines, and the per-terminal frame stacks
        // live in one static behind this helper. Structurally the twin of
        // `search_cache_lock` above — same file, same shape, a bare static
        // `Mutex` whose guard escapes to callers — so it registers the same
        // way. Registration EXTENDS the census: every caller's held-acquire
        // edges are now placed against NARROW_SESSIONS instead of being
        // invisible at call sites that carry no `.lock()` token.
        symbol: "narrow_sessions_lock",
        identity: "NARROW_SESSIONS",
        def_file: "crates/aterm-gui/src/control_query.rs",
    },
    GuardHelper {
        // Operator claims/management hold the one host authority gate across
        // their durable queue transition. Callers receive that guard through
        // this helper, so the hold must remain visible to the lock graph.
        symbol: "accepting_guard",
        identity: "fleet_fault",
        def_file: "crates/aterm-gui/src/operator_host.rs",
    },
    GuardHelper {
        // Cleanup/reconciliation uses the same host authority gate through a
        // distinct policy helper; it is the same lock identity, not a waiver.
        symbol: "mutation_guard",
        identity: "fleet_fault",
        def_file: "crates/aterm-gui/src/operator_host.rs",
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
        // An EXPLICIT type ascription on the binding is compiler-enforced —
        // stronger than any constructor-shape inference, and the shape that
        // keeps evidence alive when the constructor moves behind a helper
        // (aterm-update-core's `FileLock::open_lock_file` refactor is what
        // silently demoted the update flock into the mutex graph and tripped
        // the categorization existence test, 2026-08-22). Exact-suffix match
        // (`… =` after the path) so a generic like `Option<std::fs::File>`
        // never qualifies; the bare `File` spelling is deliberately NOT
        // accepted — unlike `File::open(`, a bare-name ascription carries no
        // constructor token to anchor it to std.
        || line.contains(": std::fs::File =")
        || line.contains(": fs::File =")
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

/// The identity namespace of the aterm WORKSPACE crates: they are this
/// census's home corpus and carry no prefix — only the vendored `[patch]`
/// forks do (`winit::…`, `winnow::…`). Spelled once, because several places
/// must ask "which side of the namespace boundary is this?" and a bare `None`
/// at each of them says nothing about which boundary is meant.
const WORKSPACE_NS: Option<&'static str> = None;

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
///
/// HOW A GATED ITEM ENDS — and why it is NOT a brace count. Until 2026-08-30
/// this walker found the end by counting `{` and `}` over the RAW line text,
/// literals INCLUDED. One `"}"`, one `'{'`, one `format!("{{")` inside a
/// `#[cfg(test)]` module desynchronised the depth; the `depth <= 0` close then
/// never held again and EVERY REMAINING LINE OF THE FILE was blanked. Measured
/// on this tree the day of the repair: `#[cfg(test)] mod pkg_progress_tests` at
/// `crates/aterm-gui/src/lib.rs:20281` swallowed the remaining 19 553 lines of
/// that 39 833-line file, hiding SHIPPING code from every census that reads it
/// — `spawn_pkg_update_check` (20493), `static JUST_UPDATED: OnceLock<bool>`
/// (21201), `attach_parent_console` (21208); `aterm-gui/src/seamless.rs`, the
/// subsystem the v0.65.0 freeze lived in, lost everything after line 3 046 of
/// 5 813. THREE censuses were calling this one function while it was broken —
/// lock-order (through [`mask_cfg_test_items`]), wasm and scope — so all three
/// were reading roughly the first two thirds of the tree and calling it the
/// whole of it. (Four, now that `lazy_init` has folded its private copy back in
/// — that copy existed only to route around this bug. The finding that recorded
/// the bug also said four, but by counting the main-loop census, which masks
/// nothing at all; that is its own posture, not this function's.) Un-blinding
/// the three is worth 5 050 newly VISIBLE lines across
/// `crates/**` and, in the other direction, 3 236 lines of test code the old
/// counter had left visible after an early break — all of `atpkg/src/flow.rs`'s
/// test module after `assert!(.., "{index_body}")`, all of
/// `aterm-gui/src/video_key_analysis.rs`'s after a `split([',', '\n', '}'])`.
/// On this tree the lock-order census goes from 821 acquisition sites and 118
/// identities to 826 and 119 (a new `progress_proxy`), and the graph stays
/// ACYCLIC.
///
/// The end of an item is now found the way rustfmt guarantees it can be, with
/// literals masked before every token test:
///   - a `;` at the end of a line — the body-less item (`use`, `const`, a
///     statement, a `fn` declaration);
///   - a `,` at the end of a line WHOSE OWN INDENT EQUALS THE GATE'S — the
///     brace-less item that has no `;`: an enum variant, a struct field, a
///     match arm, a struct-literal field. The indent equality is what keeps a
///     wrapped signature (`fn f(`↵`    a: u32,`↵`) -> T {`) out: its parameter
///     lines are indented DEEPER, and reading `a: u32,` as the end is exactly
///     the over-mask this rule must not commit;
///   - otherwise the first `{` opens a body, which ends at the first later line
///     that is `<indent>}` — rustfmt's closing-brace-at-item-indent invariant,
///     the same one the fn segmenter ([`crate::parse_source_fns`]) already
///     trusts. `},` / `});` / `};` count (an item can be a variant, a macro
///     call or a `let`); `} else {` does not, because a line that reopens a
///     brace is a continuation, not an end.
///
/// The seed of this is `lazy_init::mask_unshipped`, written and proven in that
/// lane first (docs/temporal-safety-gate.md, "CLOSED FINDING (2026-08-30): the
/// shared `#[cfg(test)]` mask blanked to EOF" — which is where the whole
/// adjudication is written down); the `,`-terminated and literal-masked
/// rules are what the fallout adjudication added — without them a
/// `#[cfg(test)]` enum variant in `aterm-gui/src/lib.rs:2570` ate the next 830
/// lines of shipped `enum` and a `#[cfg(test)] debug_assert!(` in
/// `aterm-scrollback/src/cold_tier.rs:612` was cut off at its own format string.
///
/// FAIL-OPEN ON AN UNRECOGNISED SHAPE, deliberately — the opposite direction
/// from the exact-match gate spelling above, because the two errors are not
/// symmetric. Masking is SUBTRACTIVE: leaving a gated body VISIBLE costs at
/// worst a false positive that a human reads and answers, while blanking
/// shipped code removes it from the census's sight and the census then reports
/// GREEN. So when no `<indent>}` closes the body, or the walk runs off the end
/// of the file without any terminator, the lines already marked are RESTORED
/// rather than guessed at.
///
/// WHY THE RESTORE MATTERS BEYOND HONESTY: the masked text is re-parsed
/// downstream by brace counters ([`crate::scope_census`]'s `all_struct_bodies`
/// and `fn_body`). Blanking a `{` without its `}` would splice the file and
/// hand them a struct body that runs to the next unbalanced brace — which is
/// how the first cut of this repair moved the close of `struct WindowState`'s
/// body from `crates/aterm-gui/src/lib.rs:8681` to 10783 and made a
/// struct-LITERAL line at 9814 read as a field declaration — a phantom OB-14
/// "UNACCOUNTED owner of `WordDecorations`", six red tests deep. Every
/// blanked span is now a COMPLETE item, and the check that holds it to that is
/// external: masking `crates/**/*.rs` and re-parsing each result with rustfmt
/// leaves 1 file of 1 638 unparseable, against 18 for the brace counter — and,
/// under the wasm census's longer gate list, 1 against 27.
///
/// That one file is this crate's own `lazy_init.rs`, and it is the caveat the
/// repair does not remove: a RAW STRING containing a line that is exactly
/// `<indent>}` can still close a body early. It happens only in files that
/// quote whole Rust programs — in practice this crate's fixtures, which is why
/// [`crate::scope_census`] and `lazy_init` exclude `crates/aterm-census/src`
/// from their scans, and why the lock-order and wasm scan sets (derived from
/// `aterm-gui`'s and the wasm root's dependency closures) never contain it.
/// Closing early is the subtractive direction, so it costs at most a false
/// positive.
pub(crate) fn mask_gated_items(text: &str, gates: &[&str]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    // The code of a line: comments stripped, then string/char literals blanked
    // to spaces (length-preserving, so column indices still line up). EVERY
    // token test below reads this, never the raw line — a literal brace is
    // what broke the counter this replaced.
    let code_of = |line: &str| mask_literals(strip_line_comment(line));
    let indent_of = |line: &str| line.len() - line.trim_start().len();
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
        // WHERE THE ITEM'S BODY STARTS may be lines below its first line: a
        // wrapped signature (`fn f(` … `) -> T {`) opens no brace on the line
        // the attribute introduces. Reading only that line declared such an
        // item body-less and left it UNMASKED — so a `#[cfg(not(target_arch =
        // "wasm32"))]` function with a multi-line signature had its body
        // scanned as if it shipped, which is exactly how a gated thread spawn
        // failed the wasm posture check. Walk forward to the first `{`, taking
        // a `;` or a gate-indent `,` reached first as the genuine end.
        let mut k = j;
        let mut opened = false;
        let mut terminated = false;
        while k < lines.len() {
            let item = code_of(lines[k]);
            keep[k] = false;
            if item.contains('{') {
                opened = true;
                terminated = true;
                break;
            }
            let s = item.trim_end();
            if s.ends_with(';') || (s.ends_with(',') && indent_of(lines[k]) == indent) {
                terminated = true;
                break;
            }
            k += 1;
        }
        if !terminated {
            // Ran off the end of the file with no terminator: an unrecognised
            // shape. Restore rather than blank the tail — see FAIL-OPEN above.
            for slot in keep.iter_mut().take(lines.len()).skip(j) {
                *slot = true;
            }
            break;
        }
        if opened {
            // A body that closes on the line it opened (`fn f() -> u8 { 1 }`,
            // `mod m {}`, `Variant { a: u32 },`) has no `<indent>}` line of its
            // own; anything else runs to rustfmt's.
            let first = code_of(lines[k]);
            let single = first.matches('{').count() == first.matches('}').count();
            if !single {
                let pad = " ".repeat(indent);
                let end = (k + 1..lines.len()).find(|m| {
                    let c = code_of(lines[*m]);
                    lines[*m].starts_with(&pad)
                        && c.as_bytes().get(indent) == Some(&b'}')
                        && !c.contains('{')
                });
                match end {
                    Some(end) => {
                        for slot in keep.iter_mut().take(end + 1).skip(k) {
                            *slot = false;
                        }
                        k = end;
                    }
                    // Unrecognised shape: restore the item's lines. Blanking a
                    // `{` whose `}` survives would splice the file for the
                    // brace counters downstream.
                    None => {
                        for slot in keep.iter_mut().take(k + 1).skip(j) {
                            *slot = true;
                        }
                    }
                }
            }
        }
        i = k + 1;
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
// Enclosing-impl resolution — the identity of a `self` receiver
// ---------------------------------------------------------------------------

/// Self types an `impl` block may NOT lend to a `self`-rooted lock identity.
/// An `impl .. for Mutex<T>` (or `Arc<T>`, `Box<T>`, …) is the EXTENSION-TRAIT
/// shape — the workspace's own `MutexExt::lock_or_recover` is one — where
/// `self` is EVERY caller's lock: a `Mutex::self` node would collapse the
/// whole process's mutexes into ONE identity and manufacture cycles between
/// locks that never meet. Such sites keep their unique per-site node (UNKNOWN,
/// or the audited vocabulary-interior category). Adding a name here only ever
/// moves sites BACK to a per-site node, so the list is fail-closed on the
/// merging side — the only side that can invent a deadlock.
const UNQUALIFIABLE_SELF_TYPES: &[&str] = &[
    "Arc",
    "Box",
    "Cell",
    "Condvar",
    "LazyLock",
    "Mutex",
    "OnceCell",
    "OnceLock",
    "Pin",
    "Rc",
    "RefCell",
    "ReentrantLock",
    "RwLock",
    "UnsafeCell",
    "Weak",
];

/// How many physical lines an `impl` header may span before the scan gives up
/// on finding its `{` (a wrapped generic list plus a where-clause; rustfmt
/// keeps these short). A line that never opens a block is not an impl header.
const MAX_IMPL_HEADER_LINES: usize = 24;

/// One `impl` block's line span and the SELF TYPE it implements — the name a
/// `self` receiver inside it borrows (`impl Foo`, `impl<T> Foo<T>`,
/// `impl Trait for Foo` => `Foo`).
struct ImplSpan {
    /// 1-based first line (the `impl` keyword).
    start: usize,
    /// 1-based last line (the block's closing brace).
    end: usize,
    /// `None` when the header names no type this census may use as a lock
    /// identity (see [`impl_self_type`]). The span is recorded ANYWAY, so an
    /// ENCLOSING impl can never claim these lines: the sites inside stay
    /// UNKNOWN instead of inheriting a type that is not theirs.
    ty: Option<String>,
}

/// Is `b` an identifier byte (for keyword-boundary checks)?
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte offsets of every occurrence of the keyword `word` at nesting depth 0
/// of `s` (bounded by non-identifier bytes on both sides). `->` is stepped
/// over: the `>` of a `Fn() -> T` bound must not close a generic list.
fn top_level_words(s: &str, word: &str) -> Vec<usize> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'>') {
            i += 2;
            continue;
        }
        match bytes[i] {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            _ => {}
        }
        // `is_char_boundary` first: `i` walks BYTES, and slicing into the
        // middle of a multi-byte character panics (a build script must not).
        if depth == 0
            && s.is_char_boundary(i)
            && s[i..].starts_with(word)
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && bytes.get(i + word.len()).is_none_or(|b| !is_ident_byte(*b))
        {
            out.push(i);
            i += word.len();
            continue;
        }
        i += 1;
    }
    out
}

/// Byte offset of the `>` matching the `<` at byte 0 of `s`, if any (same
/// `->` care as [`top_level_words`]).
fn matching_angle(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'>') {
            i += 2;
            continue;
        }
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The comma-separated items of a generic parameter list, split at nesting
/// depth 0.
fn split_top_level(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'>') {
            i += 2;
            continue;
        }
        match bytes[i] {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// The base name of an impl's self type: `Foo<T>` => `Foo`;
/// `std::sync::Mutex<T>` => `Mutex`; `&'a mut Foo` => `Foo`. `None` for
/// anything this census must not turn into a lock identity — a generic
/// PARAMETER of the impl (a blanket impl implements every type at once), an
/// [`UNQUALIFIABLE_SELF_TYPES`] wrapper, a macro fragment (`$ty`), a
/// structural type (`[T]`, `(A, B)`, `*mut T`), or a trait object.
fn base_type_name(ty: &str, params: &BTreeSet<&str>) -> Option<String> {
    let mut t = ty.trim();
    // `&`, `&'a `, `&mut ` — a reference to the type is still the type. `dyn`
    // is NOT peeled: a trait object names a trait, not one concrete lock.
    while let Some(rest) = t.strip_prefix('&') {
        let mut rest = rest.trim_start();
        if let Some(lt) = rest.strip_prefix('\'') {
            rest = lt
                .split_once(char::is_whitespace)
                .map_or("", |(_, r)| r)
                .trim_start();
        }
        t = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
    }
    let path: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    let name = path.rsplit("::").next().unwrap_or_default();
    if !name.starts_with(|c: char| c.is_alphabetic() || c == '_')
        || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }
    // A one-character type name is a generic parameter by universal
    // convention. Even where the lexical parse missed the parameter list (a
    // macro body, a wrapped header), refusing it costs this census nothing and
    // forecloses the blanket-impl merge.
    if name.chars().count() == 1
        || params.contains(name)
        || UNQUALIFIABLE_SELF_TYPES.contains(&name)
    {
        return None;
    }
    Some(name.to_string())
}

/// The nameable self type of an `impl` header (the text from `impl` up to the
/// block's `{`): `impl Foo` / `impl<T> Foo<T>` / `impl Trait for Foo` =>
/// `Foo`. `None` when the header names nothing this census may use as a lock
/// identity — see [`base_type_name`]; the fail-closed direction leaves the
/// block's `self` receivers UNKNOWN rather than merging them.
fn impl_self_type(header: &str) -> Option<String> {
    let h = header.trim_start();
    let h = h.strip_prefix("unsafe ").unwrap_or(h).trim_start();
    let h = h.strip_prefix("default ").unwrap_or(h).trim_start();
    let mut rest = h.strip_prefix("impl")?;
    let mut params: BTreeSet<&str> = BTreeSet::new();
    if rest.starts_with('<') {
        let close = matching_angle(rest)?;
        for p in split_top_level(&rest[1..close]) {
            let p = p.trim();
            let p = p.strip_prefix("const ").unwrap_or(p).trim_start();
            let name: &str = p
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .unwrap_or_default();
            if !name.is_empty() {
                params.insert(name);
            }
        }
        rest = &rest[close + 1..];
    }
    // A where-clause is not part of the self type — and cutting it first keeps
    // its `for<'a>` HRTBs from being read as the trait-impl `for`.
    let rest = match top_level_words(rest, "where").first() {
        Some(&at) => &rest[..at],
        None => rest,
    };
    // `impl Trait for SelfTy`: the self type follows the LAST top-level `for`.
    let self_ty = match top_level_words(rest, "for").last() {
        Some(&at) => &rest[at + "for".len()..],
        None => rest,
    };
    base_type_name(self_ty, &params)
}

/// Every `impl` block in a file, with its line span and self type. Uses the
/// same rustfmt closing-brace-at-item-indent invariant as the fn segmenter, so
/// the two agree about where a block ends. Nested impls are recorded too — the
/// lookup takes the INNERMOST span, so an impl inside a fn inside an impl
/// cannot borrow the outer type.
fn impl_spans(text: &str) -> Vec<ImplSpan> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = strip_line_comment(raw);
        let head = line.trim_start();
        let indent = line.len() - head.len();
        let head = head.strip_prefix("unsafe ").unwrap_or(head).trim_start();
        let head = head.strip_prefix("default ").unwrap_or(head).trim_start();
        let Some(after) = head.strip_prefix("impl") else {
            continue;
        };
        if !(after.starts_with('<') || after.starts_with(char::is_whitespace)) {
            continue; // `implements_x()`, `impl_from!(..)` — not a block header.
        }
        // The header runs to the `{` that opens the block; rustfmt wraps a long
        // generic list and a where-clause onto continuation lines.
        let mut header = String::new();
        let mut open = None;
        for (k, l) in lines.iter().enumerate().skip(i).take(MAX_IMPL_HEADER_LINES) {
            let l = strip_line_comment(l);
            if let Some(b) = l.find('{') {
                header.push_str(&l[..b]);
                open = Some(k);
                break;
            }
            header.push_str(l);
            header.push(' ');
        }
        let Some(open) = open else {
            continue;
        };
        let opener = strip_line_comment(lines[open]);
        let end = if opener.matches('{').count() == opener.matches('}').count() {
            open // `unsafe impl Send for Foo {}` — the whole block is one line.
        } else {
            // Fail-closed: an impl whose close cannot be found (a macro body
            // that does not follow rustfmt's indentation) spans its header
            // only, so no acquisition inherits a guessed type.
            let close = format!("{}}}", " ".repeat(indent));
            lines
                .iter()
                .enumerate()
                .skip(open + 1)
                .find(|(_, l)| **l == close)
                .map_or(open, |(k, _)| k)
        };
        out.push(ImplSpan {
            start: i + 1,
            end: end + 1,
            ty: impl_self_type(&header),
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

/// Where an acquisition sits, for the identity refinements that need more
/// than the line itself.
struct SiteCtx<'a> {
    /// The file's `impl` blocks (see [`impl_spans`]).
    impls: &'a [ImplSpan],
    /// 1-based physical line of the logical line being scanned.
    lineno: usize,
    /// Live single-letter aliases in this fn (see [`alias_target`]).
    aliases: &'a BTreeMap<String, (i32, String)>,
}

impl SiteCtx<'_> {
    /// The self type of the INNERMOST `impl` block covering this line, when it
    /// is one this census may name.
    fn impl_ty(&self) -> Option<&str> {
        self.impls
            .iter()
            .filter(|s| s.start <= self.lineno && self.lineno <= s.end)
            .max_by_key(|s| s.start)
            .and_then(|s| s.ty.as_deref())
    }
}

/// The receiver chain ending at `dot` rendered as the field path below `self`,
/// when it is rooted THERE: `self.lock()` => `self`; `self.0.lock()` => `0`;
/// `self.control.1.read()` => `control.1`. `None` for any other root — a
/// LOCAL's tuple field (`pair.0`) is not this type's field, and qualifying it
/// with the impl type would merge two unrelated locks.
fn self_rooted_chain(line: &str, dot: usize) -> Option<String> {
    let bytes = line.as_bytes();
    let mut segs: Vec<&str> = Vec::new();
    let mut at = dot;
    loop {
        let seg = ident_ending_at(line, at)?;
        let start = at - seg.len();
        if seg == "self" {
            if segs.is_empty() {
                return Some("self".to_string());
            }
            segs.reverse();
            return Some(segs.join("."));
        }
        if start == 0 || bytes[start - 1] != b'.' {
            return None;
        }
        segs.push(seg);
        at = start - 1;
    }
}

/// Resolve the lexical identity of the receiver whose final `.` sits at byte
/// `dot`. `store.read()` / `self.store.read()` => `store`; `proxies().write()`
/// => `proxies` (the zero-arg accessor-fn idiom for statics).
///
/// A receiver with no name of its own is REFINED rather than dropped: a chain
/// rooted at `self` takes the enclosing impl type's name (`self.0.lock()` in
/// `impl Spill` => `Spill::0`), and a single-letter local takes the identity
/// of the path it aliases. Both refinements apply ONLY where the plain rule
/// resolved nothing, so every identity the census mints today it still mints,
/// unchanged.
///
/// What DOES change is unification — in the direction that can turn the gate
/// RED, which is the point of the refinement rather than a side effect of it.
/// An UNKNOWN is still graphed, as a per-site UNIQUE node (see
/// [`AcqSite::node`]): seen, but unable to unify with anything, so it can
/// never close a cycle. Naming two `self.0.lock()` sites inside one
/// `impl Spill` collapses two unique nodes into one `Spill::0`, and a real
/// ordering through that lock becomes visible for the first time.
///
/// (The draft this was salvaged from justified itself the opposite way — that
/// it could "SPLIT a per-site UNKNOWN into a real node, never MERGE two nodes
/// the graph keeps apart". That is backwards, and it is worth being exact
/// about: this is sound because the sites it merges ARE one lock — the same
/// field of the same type, under the same impl header — not because no merge
/// occurs.)
///
/// What still cannot be named stays None (UNKNOWN — honestly unresolvable).
fn resolve_receiver(line: &str, dot: usize, ctx: &SiteCtx<'_>) -> Option<String> {
    // `self.store?.read()` — the `?` is postfix error propagation, not part of
    // the receiver: it unwraps an Option/Result and the lock on the other side
    // is still the one `store` names. Without this the token before the dot is
    // `?`, no identifier is found, and a perfectly nameable lock is reported
    // UNKNOWN — which is the honesty gap widening for a punctuation mark rather
    // than for anything actually unresolvable.
    //
    // Stripped BEFORE the refinements below, not after: `self.store?.lock()`
    // must reach `self_rooted_chain` at the receiver's real dot, or a `?` in
    // the chain would send a nameable field back to UNKNOWN by the same
    // punctuation accident this fix exists to close.
    let dot = {
        let bytes = line.as_bytes();
        let mut d = dot;
        while d > 0 && bytes[d - 1] == b'?' {
            d -= 1;
        }
        d
    };
    if let Some(seg) = ident_ending_at(line, dot) {
        if seg == "self" || seg.chars().all(|c| c.is_ascii_digit()) || seg.len() == 1 {
            if let Some(chain) = self_rooted_chain(line, dot)
                && let Some(ty) = ctx.impl_ty()
            {
                // `Ty::self` / `Ty::0` can never collide with a plain receiver
                // identity (no receiver name contains `::`) nor with a
                // vendored `<crate>::<recv>` one: `self` and a tuple index are
                // exactly what plain resolution refuses to return.
                return Some(format!("{ty}::{chain}"));
            }
            if let Some((_, aliased)) = ctx.aliases.get(seg) {
                return Some(aliased.clone());
            }
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
fn acquisitions_on(line: &str, ctx: &SiteCtx<'_>) -> Vec<RawAcq> {
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
                    identity: resolve_receiver(line, dot, ctx),
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

/// Byte offset of a `let` statement's binding `=` — not a comparison or a
/// compound assignment. `None` (fail-closed) for any other shape.
fn binding_eq(line: &str) -> Option<usize> {
    // Operator bytes that turn a following `=` into something other than a
    // binding (`==`, `!=`, `<=`, `+=`, …).
    const OPS: &[u8] = b"=!<>+-*/%&|^";
    let bytes = line.as_bytes();
    let at = line.find('=')?;
    if bytes.get(at + 1) == Some(&b'=') || (at > 0 && OPS.contains(&bytes[at - 1])) {
        return None;
    }
    Some(at)
}

/// The identity a `let` binding ALIASES, when its right-hand side is a PURE
/// PATH (`&self.spill`, `self.0`, `&QUEUE`): exactly what that path would
/// resolve to at an acquisition site, which is the identity the field spelling
/// would already have produced — so an alias can introduce no identity, and no
/// merge, the census could not already mint. Fail-closed: any RHS that is not
/// a bare path — a call, an index, an operator, a block — yields None, and the
/// aliased receiver stays UNKNOWN.
fn alias_target(line: &str, ctx: &SiteCtx<'_>) -> Option<String> {
    let eq = binding_eq(line)?;
    let mut path = line[eq + 1..].trim().trim_end_matches(';').trim_end();
    // A borrow of the path is the same lock.
    while let Some(rest) = path.strip_prefix('&') {
        let rest = rest.trim_start();
        path = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
    }
    let pure_path = path
        .bytes()
        .all(|b| is_ident_byte(b) || b == b'.' || b == b':');
    if path.is_empty() || !pure_path {
        return None;
    }
    resolve_receiver(path, path.len(), ctx)
}

/// Callee names invoked on this logical line in the shapes the one-hop pass
/// trusts: free-fn/path `name(` and `self.name(` (an `other.name(` method call
/// is type-ambiguous and excluded — see the precision note). Returns
/// `(byte position, name)`; keyword pseudo-calls and macro bangs excluded.
/// A call the census will follow one hop: `(byte offset in the line, callee)`.
type TrustedCall = (usize, String);

/// A call through a `self.field.…` receiver: `(byte offset, field, callee)`.
/// Whether it is followed is decided later, against [`FieldTypes`].
type FieldRecvCall = (usize, String, String);

/// What the scanned corpus declares about types and struct fields — the
/// evidence that decides whether a `self.field.name(..)` hop can be followed.
///
/// Both halves are name-keyed and therefore over-approximate (two structs with a
/// field of the same name merge). That is the census's standing posture for
/// same-named fns, and it errs toward FOLLOWING a hop — the direction that risks
/// a visible false positive rather than a silent missed cycle.
///
/// The harvest runs over the WORKSPACE crates only (the vendored `[patch]`
/// sources are parsed for lock SITES, never for declarations), so this evidence
/// describes [`WORKSPACE_NS`] types and nothing else — see
/// [`FieldTypes::is_corpus_typed`].
#[derive(Default)]
struct FieldTypes {
    /// Every type NAME the corpus defines (`struct`/`enum`/`union`/`type` and
    /// `trait`, which a `dyn`/`impl` field resolves through).
    defined: BTreeSet<String>,
    /// Field name -> every type identifier appearing in its declared type. A
    /// wrapper contributes its own name too (`Arc<Shared>` -> {Arc, Shared}),
    /// so the test is "does ANY of them name a corpus type".
    field_tys: BTreeMap<String, BTreeSet<String>>,
}

impl FieldTypes {
    /// Is `field`'s declared type defined in this corpus, where `at` is the
    /// namespace of the CALL SITE asking? Unknown fields answer `false`: with
    /// no declaration in evidence there is nothing to resolve through, and a
    /// guess is what this whole mechanism exists to avoid.
    ///
    /// A call site outside [`WORKSPACE_NS`] answers `false` for the same
    /// reason: the harvest only ever read the workspace crates, so every
    /// declaration in here is aterm's. That a struct aterm declares has a field
    /// named `state` is not evidence about the type of the field winit spells
    /// `self.state` — resolving a vendored hop through it would be the very
    /// guess the mechanism refuses, dressed as evidence.
    fn is_corpus_typed(&self, at: Option<&'static str>, field: &str) -> bool {
        at == WORKSPACE_NS
            && self
                .field_tys
                .get(field)
                .is_some_and(|tys| tys.iter().any(|t| self.defined.contains(t)))
    }

    /// Harvest declarations from one already-cfg-masked source file.
    fn harvest(&mut self, text: &str) {
        // Depth of the struct/enum body we are inside, if any. Field lines are
        // collected only at depth 1 of such a body, so fn bodies, match arms
        // and `let x: T` never masquerade as field declarations.
        let mut body_depth: Option<i32> = None;
        let mut depth: i32 = 0;
        for raw in text.lines() {
            let line = raw.split("//").next().unwrap_or(raw);
            let t = line.trim_start();
            let mut opens_fields = false;
            for kw in ["struct ", "enum ", "union ", "trait ", "type "] {
                let Some(rest) = t.strip_prefix(kw).or_else(|| {
                    t.strip_prefix("pub ")
                        .and_then(|p| p.trim_start().strip_prefix(kw))
                        .or_else(|| {
                            // `pub(crate) struct X`, `pub(super) enum Y`, …
                            t.strip_prefix("pub(")
                                .and_then(|p| p.split_once(')'))
                                .and_then(|(_, p)| p.trim_start().strip_prefix(kw))
                        })
                }) else {
                    continue;
                };
                if let Some(n) = ident_prefix(rest.trim_start()) {
                    self.defined.insert(n.to_string());
                    // A body opens on this line for the data types only; `type`
                    // is an alias and `trait` bodies hold fns, not fields.
                    opens_fields = line.contains('{') && (kw == "struct " || kw == "union ");
                }
                break;
            }
            if body_depth == Some(depth)
                && let Some((lhs, rhs)) = t.split_once(':')
                && !t.starts_with("//")
                && !lhs.contains('(')
                && !rhs.starts_with(':')
            {
                let name = lhs
                    .rsplit(|c: char| c.is_whitespace() || c == ')')
                    .next()
                    .unwrap_or("")
                    .trim();
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    let entry = self.field_tys.entry(name.to_string()).or_default();
                    for tok in rhs.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                        if !tok.is_empty() {
                            entry.insert(tok.to_string());
                        }
                    }
                }
            }
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            // Fields live one level INSIDE the declaration's brace, so the depth
            // to match is the one this line leaves us at.
            if opens_fields {
                body_depth = Some(depth);
            } else if let Some(d) = body_depth
                && depth < d
            {
                body_depth = None;
            }
        }
    }
}

/// The leading identifier of `s`, if it starts with one.
fn ident_prefix(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (end > 0).then(|| &s[..end])
}

/// One-hop call targets on `line`: the unconditionally trusted shapes, plus the
/// field-receiver calls that are trusted only once their FIELD TYPE is resolved.
///
/// Returns `(trusted, field_receiver)`, the latter as `(field, callee)`.
///
/// A callee is looked up by NAME across the whole corpus, so a call shape may be
/// followed only when the name is known to belong to a same-corpus fn:
///
///   * `name(..)` — a free fn; if the corpus has one, that is the callee.
///   * `self.name(..)` — a method on `Self`, whose impl is in this corpus.
///
/// `self.field.name(..)` is neither: it dispatches on the FIELD's type, making it
/// exactly as ambiguous as the `other.name(..)` the header excludes, with `other`
/// merely spelled `self.field`. Following it unconditionally once bound
/// `self.drained.wait(guard)` — the std `Condvar::wait` that ATOMICALLY RELEASES
/// its guard — to an unrelated same-named `wait` in another crate, and
/// synthesized a `spill -> lock` edge that closed a two-lock cycle in
/// `aterm-session`'s sink. OB-7 has no waiver channel by design, so that one
/// false edge blocked every build of the gate and could not be waived.
///
/// Dropping the shape outright would trade that false positive for false
/// NEGATIVES — `self.shared.spill_is_empty()` is a REAL held-acquire hop — and in
/// a deadlock census a missed edge is the worse failure, because it is silent.
/// So the receiver is TYPE-DIRECTED instead (see [`FieldTypes`]): the hop is
/// followed when the field's declared type is defined in the scanned corpus (its
/// methods are corpus fns, so the name lookup is the same over-approximation
/// bare-`self` already makes), and counted-but-not-followed when the type is
/// foreign (`Condvar`, `AtomicU64`, `HashMap`), where the lookup would be a guess.
fn held_call_targets(line: &str) -> (Vec<TrustedCall>, Vec<FieldRecvCall>) {
    const KEYWORDS: &[&str] = &[
        "if", "while", "for", "match", "return", "fn", "loop", "unsafe", "move", "let", "else",
        "in", "as", "await", "Some", "Ok", "Err", "None", "drop",
    ];
    let mut out = Vec::new();
    let mut field_recv = Vec::new();
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
            // Method call. Only a BARE `self.name(` receiver names a method of
            // the enclosing type, i.e. a fn this corpus defines. Anything else
            // — `other.name(`, and equally `self.field.name(` — dispatches on a
            // type we cannot resolve lexically, so the name-based callee lookup
            // would be a guess. Field chains are counted (see the doc comment);
            // foreign receivers were never followed and stay silent.
            let Some(recv) = ident_ending_at(line, start - 1) else {
                continue;
            };
            if recv != "self" {
                // A `self.…field.name(` chain is resolvable through the field's
                // declared type; a chain rooted in a foreign local is not, and
                // was never in scope. `recv` is the LAST field of the chain —
                // the one whose type the callee dispatches on.
                let mut at = start - 1 - recv.len();
                let rooted_at_self = loop {
                    if at == 0 || bytes[at - 1] != b'.' {
                        break false;
                    }
                    let Some(seg) = ident_ending_at(line, at - 1) else {
                        break false;
                    };
                    if seg == "self" {
                        break true;
                    }
                    at -= 1 + seg.len();
                };
                if rooted_at_self {
                    field_recv.push((idx, recv.to_string(), name.to_string()));
                }
                continue;
            }
        }
        out.push((idx, name.to_string()));
    }
    (out, field_recv)
}

// ---------------------------------------------------------------------------
// Per-fn scan: sites, intra-fn edges, held calls
// ---------------------------------------------------------------------------

/// Everything the census learned about one fn.
struct FnLockFacts {
    name: String,
    /// Repo-relative `file:line` of the definition.
    span: String,
    /// The identity namespace of the crate DEFINING this fn: [`WORKSPACE_NS`]
    /// for the aterm crates, `Some("winnow")` etc. for a scanned vendored fork.
    /// One-hop callee lookup is by NAME over a table that spans both sides, so
    /// the name alone cannot be the key — a definition's namespace is half of
    /// its identity as a callee.
    namespace: Option<&'static str>,
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
    /// Calls made while holding through a FIELD receiver (`self.field.name(..)`).
    /// Followed only once the field's type is resolved against the corpus — see
    /// [`held_call_targets`] and [`FieldTypes`].
    /// `(holder site, field name, callee name, call `file:line`)`.
    field_recv_calls: Vec<(usize, String, String, String)>,
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

/// The FILE-level lexical facts a body scan needs beyond the body's own text.
/// File-scoped, not body-scoped, because every one of them is DECLARED outside
/// the fn that uses it.
struct FileCtx<'a> {
    /// Repo-relative path, for the site spans.
    rel: &'a str,
    /// `Some(prefix)` in vendored-identity mode: every resolved identity is
    /// prefixed `prefix::…` so foreign receiver names can never merge across
    /// the namespace boundary.
    namespace: Option<&'static str>,
    /// The file's `impl` blocks: the `impl Spill {` header that gives a `self`
    /// receiver its identity sits ABOVE the fn, not inside it.
    ///
    /// This struct carried a `condvars` set when it was drafted. It does not
    /// now: `1fd130e3` took corpus-wide field-TYPE resolution over the
    /// per-file Condvar carve-out, and `FieldTypes` answers that question
    /// better than a lexical `drained: Condvar,` scan of one file could.
    impls: &'a [ImplSpan],
}

/// Scan one fn body. `sites` is the global site table (appended to).
fn scan_fn(
    name: &str,
    span: &str,
    body: &[String],
    file: &FileCtx<'_>,
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
        namespace: file.namespace,
        returns_guard: returns_guard(body),
        takes_self: takes_self(body),
        acq: Vec::new(),
        edges: Vec::new(),
        held_calls: Vec::new(),
        field_recv_calls: Vec::new(),
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
    // Single-letter locals that ALIAS a nameable receiver path (`let m =
    // &self.spill;`): name -> (binding brace depth, the identity that path
    // resolves to). A one-letter receiver has no identity of its own; the
    // alias lends it the one the field spelling would already have produced
    // (see [`alias_target`]). Same fail-closed ledger discipline as the two
    // above: a rebind to anything that is not a pure path drops the alias, and
    // a block close drops every alias bound inside it.
    let mut alias_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
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
        // 1. `drop(var)` ends a guard's life explicitly. Both needles begin with
        //    the literal `drop(`, so a line without it cannot end any guard: the
        //    cheap constant-needle test replaces two heap `String`s + two
        //    two-way `StrSearcher`s per live named guard. The `live.is_empty()`
        //    test comes FIRST and is what keeps this honest — `live` is empty on
        //    the overwhelming majority of the corpus's lines, and paying a
        //    `StrSearcher` construction on every one of them to save allocations
        //    on the few percent that hold a guard is the anti-pattern documented
        //    on the guard-helper scan above.
        if !live.is_empty() && line.contains("drop(") {
            live.retain(|g| {
                g.var.as_ref().is_none_or(|v| {
                    !line.contains(&format!("drop({v})")) && !line.contains(&format!("drop(&{v})"))
                })
            });
        }
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
        // The ctx borrows `alias_vars`; its last use is the call below, so the
        // ledger is free to be updated again at 5c.
        let ctx = SiteCtx {
            impls: file.impls,
            lineno: ll.lineno,
            aliases: &alias_vars,
        };
        let raw = acquisitions_on(line, &ctx);
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
                    advisory.is_none()
                        && raw_ptr.is_none()
                        && v.symbol == name
                        && v.def_file == file.rel
                        && r.kind.starts_with('.')
                        && ident_ending_at(line, r.pos) == Some("self")
                })
                .map(|v| v.audit);
            let mut excerpt: String = line.chars().take(160).collect();
            if excerpt.len() < line.len() {
                excerpt.push('…');
            }
            let identity = if vocab.is_some() {
                // A registered vocabulary interior keeps its UNIQUE PER-SITE
                // node whatever the receiver refinements produced: `self` there
                // is EVERY caller's lock (the identity lives at the call
                // sites), so any shared identity would merge locks that never
                // meet. Belt and braces — such an impl's self type is an
                // UNQUALIFIABLE_SELF_TYPES wrapper anyway — because the
                // registry's fail-closed checks depend on this staying true.
                None
            } else {
                // Vendored-identity mode: the identity lives in the crate's own
                // namespace (categorized advisory/raw-ptr sites are excluded
                // from the graph anyway and keep their local name in the
                // listing).
                match (file.namespace, &r.identity) {
                    (Some(ns), Some(id)) if advisory.is_none() && raw_ptr.is_none() => {
                        Some(format!("{ns}::{id}"))
                    }
                    _ => r.identity.clone(),
                }
            };
            sites.push(AcqSite {
                identity,
                kind: r.kind,
                blocking: r.blocking,
                span: format!("{}:{}", file.rel, ll.lineno),
                fn_name: name.to_string(),
                excerpt,
                advisory,
                vocab,
                raw_ptr,
                namespace: file.namespace,
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
            let (trusted, field_recv) = held_call_targets(line);
            for (pos, field, callee) in field_recv {
                let call_span = format!("{}:{}", file.rel, ll.lineno);
                for g in live.iter().filter(|g| g.active) {
                    facts.field_recv_calls.push((
                        g.site,
                        field.clone(),
                        callee.clone(),
                        call_span.clone(),
                    ));
                }
                for (k, r) in raw.iter().enumerate() {
                    if r.pos < pos && sites[this_line[k]].graphed() {
                        facts.field_recv_calls.push((
                            this_line[k],
                            field.clone(),
                            callee.clone(),
                            call_span.clone(),
                        ));
                    }
                }
            }
            for (pos, callee) in trusted {
                let call_span = format!("{}:{}", file.rel, ll.lineno);
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
                file_vars.insert(var.clone(), (depth, format!("{}:{}", file.rel, ll.lineno)));
            } else {
                file_vars.remove(var);
            }
            if is_raw_ptr_binding_rhs(line) || rhs_extends_raw_ptr(line, &ptr_vars) {
                ptr_vars.insert(var.clone(), (depth, format!("{}:{}", file.rel, ll.lineno)));
            } else {
                ptr_vars.remove(var);
            }
            // 5c. The alias ledger, for SINGLE-LETTER bindings only. A longer
            //     local name IS already an identity (`mx.lock()` => `mx`), and
            //     re-pointing it here would MERGE two nodes the graph keeps
            //     apart today — the one direction that can invent a cycle.
            //     Refinement is for UNKNOWNs only.
            if var.len() == 1 && var.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                let alias_ctx = SiteCtx {
                    impls: file.impls,
                    lineno: ll.lineno,
                    aliases: &alias_vars,
                };
                let aliased = alias_target(line, &alias_ctx);
                if let Some(id) = aliased {
                    alias_vars.insert(var.clone(), (depth, id));
                } else {
                    alias_vars.remove(var);
                }
            }
        }
        // 6. Block ends kill the guards bound inside them; a let-else guard
        //    whose else block just closed (depth back at its binding depth)
        //    becomes active.
        depth = depth_end;
        live.retain(|g| depth >= g.binding_depth);
        file_vars.retain(|_, (d, _)| depth >= *d);
        ptr_vars.retain(|_, (d, _)| depth >= *d);
        alias_vars.retain(|_, (d, _)| depth >= *d);
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
    // Type evidence for the field-receiver hop, harvested from the same files —
    // the WORKSPACE files only. The vendored loop below deliberately does not
    // harvest: pooling winit's declarations with aterm's would let one crate's
    // struct answer for the other's fields, and giving each fork its own
    // evidence would EXTEND which hops are followed (more edges), which is a
    // coverage change to review on its own, not a side effect of namespacing.
    let mut field_types = FieldTypes::default();
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
            field_types.harvest(&masked);
            let impls = impl_spans(&masked);
            let ctx = FileCtx {
                rel: &rel,
                namespace: None,
                impls: &impls,
            };
            let mut parsed = Vec::new();
            parse_source_fns(&masked, &rel, &mut parsed);
            for f in &parsed {
                fns.push(scan_fn(&f.name, &f.span, &f.body, &ctx, &mut sites));
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
            let impls = impl_spans(&masked);
            let ctx = FileCtx {
                rel: &rel,
                namespace: Some(v.namespace),
                impls: &impls,
            };
            let mut parsed = Vec::new();
            parse_source_fns(&masked, &rel, &mut parsed);
            match slice_idx {
                Some(i) => {
                    // Labeled platform slice: count, never graph.
                    let mut scratch: Vec<AcqSite> = Vec::new();
                    for f in &parsed {
                        let _ = scan_fn(&f.name, &f.span, &f.body, &ctx, &mut scratch);
                    }
                    per_slice[i] += scratch.len();
                }
                None => {
                    for f in &parsed {
                        fns.push(scan_fn(&f.name, &f.span, &f.body, &ctx, &mut sites));
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

    // Held calls through a `self.field.…` receiver, split by whether the field's
    // declared type is defined in this corpus. Corpus-typed ones are followed
    // like any other hop; foreign-typed ones cannot be resolved by name and are
    // counted + listed instead of guessed at.
    let mut unresolved_field_calls: Vec<(String, String)> = Vec::new();
    // Per fn name, how many fns bear it IN EACH NAMESPACE. A field hop is
    // followed only when the whole corpus holds exactly ONE such fn AND it is
    // the call site's own namespace that defines it. Both halves are the same
    // requirement seen twice: the hop's entire justification is that the field's
    // declared type is a corpus type, so its methods are corpus fns — a second
    // candidate makes the by-name lookup a PICK (that is how `self.drained
    // .wait(g)` on aterm-types' `Condvar` newtype merged with an unrelated
    // `MemoryWait::wait` and fabricated a cycle), and a lone candidate on the far
    // side of a vendored namespace boundary is not a method of that type at all
    // (an aterm struct's method is an aterm fn; winnow's unique `fn start` can
    // never be the callee of an aterm `self.trace.start(..)`).
    let mut name_counts: BTreeMap<String, BTreeMap<Option<&'static str>, usize>> = BTreeMap::new();
    for f in &fns {
        *name_counts
            .entry(f.name.clone())
            .or_default()
            .entry(f.namespace)
            .or_default() += 1;
    }
    for f in &mut fns {
        let caller_ns = f.namespace;
        for (site, field, callee, span) in std::mem::take(&mut f.field_recv_calls) {
            let uniquely_local = name_counts
                .get(&callee)
                .is_some_and(|per_ns| per_ns.len() == 1 && per_ns.get(&caller_ns) == Some(&1));
            let resolvable = field_types.is_corpus_typed(caller_ns, &field) && uniquely_local;
            if resolvable {
                f.held_calls.push((site, callee, span));
            } else {
                let _ = site;
                unresolved_field_calls.push((format!("{field}.{callee}"), span));
            }
        }
    }
    unresolved_field_calls.sort();
    unresolved_field_calls.dedup();
    let field_recv_calls = unresolved_field_calls.len();

    // Callee lookup for the one-hop pass: fn name -> definitions, GROUPED by the
    // namespace of the crate defining each. The name alone cannot be the key.
    // The table merges the workspace crates with every scanned vendored fork,
    // and the vendored crates' identities are namespaced but their fn NAMES are
    // not: winnow's `fn start` and `fn end` both lock `winnow::writer`, and
    // aterm has its own `start`/`end` with hop-eligible call sites. Resolving a
    // bare `start()` in aterm code against the merged table would mint an
    // aterm-lock -> `winnow::writer` edge out of nothing but a name collision —
    // and a cycle closed that way could not be repaired AT ALL, since the
    // vendored half of it may not be edited and OB-7 has no waiver channel.
    //
    // The boundary rule, applied per call site (see the precision note):
    //   * the caller's OWN namespace defines the name => those definitions are
    //     the candidates and the foreign ones are dropped. Rust agrees: a bare
    //     `start(..)` beside a local `fn start` is the local one, because an
    //     import colliding with a local definition does not compile.
    //   * it does not => the call must be to something IMPORTED, which may well
    //     be the vendored fork (aterm holding a lock across a call into winit;
    //     winit calling back out). Those hops stay followed — refusing them
    //     would trade a false edge for a SILENT missing one, and in a deadlock
    //     census the silent failure is the worse one.
    let mut by_name: BTreeMap<&str, BTreeMap<Option<&'static str>, Vec<usize>>> = BTreeMap::new();
    for (idx, f) in fns.iter().enumerate() {
        by_name
            .entry(&f.name)
            .or_default()
            .entry(f.namespace)
            .or_default()
            .push(idx);
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
        // Only the WORKSPACE definitions can satisfy the interior check: a
        // registered helper is aterm's own (its `def_file` is verified above),
        // so a same-named fn inside a vendored fork is a different fn and must
        // not be able to vouch for the registration.
        let interior_ok = by_name
            .get(h.symbol)
            .and_then(|per_ns| per_ns.get(&WORKSPACE_NS))
            .is_some_and(|idxs| {
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
            // The registry exemption is the WORKSPACE's: every GUARD_HELPERS
            // entry names an aterm fn (its `def_file` is verified above), so a
            // vendored fn that merely shares the name is a DIFFERENT
            // guard-returning helper and still hides its own callers' holds.
            // Letting the name alone excuse it would be the same
            // boundary-blind by-name match the one-hop table just stopped
            // making — here failing OPEN instead of closed.
            || (f.namespace == WORKSPACE_NS && GUARD_HELPERS.iter().any(|h| h.symbol == f.name))
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
            let Some(per_ns) = by_name.get(callee.as_str()) else {
                continue;
            };
            // The namespace boundary (see `by_name`): definitions in the
            // caller's own namespace SHADOW every foreign one, so a name the
            // caller's crate defines never reaches across the boundary.
            let shadowed = per_ns.contains_key(&f.namespace);
            // One hop: every DIRECT blocking acquisition in every same-named
            // callee (distinct identities once per call site).
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for (def_ns, callee_fns) in per_ns {
                if shadowed && *def_ns != f.namespace {
                    continue;
                }
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
         pairs); {} held call(s) through a `self.field.…` receiver NOT followed \
         (foreign field type, callee name not unique in the corpus, or its one \
         definition on the far side of a vendored namespace boundary — the \
         census's second standing honesty gap, alongside UNKNOWN); \
         0 self-edges; global lock graph ACYCLIC.",
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
        field_recv_calls,
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
    // The second honesty gap, listed for the same reason UNKNOWN is: a reader
    // must be able to see WHICH hops the census declined to follow, and judge
    // whether one of them hides an ordering it should have graphed.
    let field_calls = &unresolved_field_calls;
    if !field_calls.is_empty() {
        let _ = writeln!(
            log,
            "    held calls through a `self.field.…` receiver NOT followed — the field's \
             declared type is not defined in this corpus, or more than one corpus fn \
             bears the callee's name (the by-name lookup would pick a candidate rather \
             than resolve one), or the one fn that bears it is defined in another \
             crate's namespace, where a method of THIS field's type cannot live — each \
             a guess, not an over-approximation:"
        );
        for (callee, span) in field_calls.iter().take(20) {
            let _ = writeln!(log, "      {span}  .{callee}(…)");
        }
        if field_calls.len() > 20 {
            let _ = writeln!(log, "      … and {} more.", field_calls.len() - 20);
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
            // Mirrors the shipping helpers and their static ring identities;
            // this fixture makes a stale singular registration fail closed.
            // BOTH control_query.rs helpers live here — one file, one fixture
            // entry — because the registry check reads the SCANNED corpus.
            "crates/aterm-gui/src/control_query.rs".to_string(),
            "fn search_cache_lock() -> MutexGuard<'static, VecDeque<SearchSnapshot>> {\n    \
             SEARCH_SNAPSHOTS.lock().unwrap()\n}\n\
             fn narrow_sessions_lock() -> MutexGuard<'static, VecDeque<NarrowSession>> {\n    \
             NARROW_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)\n}\n"
                .to_string(),
        ));
        files.push((
            // The embedded operator's two guard-returning policy helpers both
            // expose the same host authority mutex to their callers.
            "crates/aterm-gui/src/operator_host.rs".to_string(),
            "fn accepting_guard(&self) -> MutexGuard<'_, HostActuationGate> {\n    \
             self.shared.fleet_fault.lock().unwrap()\n}\n\
             fn mutation_guard(&self) -> MutexGuard<'_, HostActuationGate> {\n    \
             self.shared.fleet_fault.lock().unwrap()\n}\n"
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
        files.push((
            // The session-connections record store: a `&self` method whose
            // name is outside the standard vocabulary, so the registry — not
            // the token scan — is what makes its callers' holds visible.
            "crates/aterm-gui/src/connections.rs".to_string(),
            "impl ConnectionTable {\n    \
             pub(crate) fn records(&self) -> MutexGuard<'_, Records> {\n        \
             self.records.lock().unwrap_or_else(|p| p.into_inner())\n    }\n}\n"
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

    /// The synthetic trees carry the registries' ground truth: EVERY
    /// GUARD_HELPERS / VOCABULARY_INTERIORS entry must be defined in
    /// [`synth_helper_files`], at its registered `def_file`, in a crate the
    /// synthetic scan set actually reaches. Registering a helper without
    /// mirroring it here leaves every synthetic tree failing OB-7 on the real
    /// registry — which reds the whole self-test suite (30+ tests) with a
    /// failure that has nothing to do with what each test measures. This test
    /// names that chore directly, in one message, so the next registration
    /// cannot hide behind the noise.
    #[test]
    fn every_registry_entry_is_mirrored_in_the_synthetic_fixture() {
        let files = synth_helper_files();
        let find =
            |rel: &str| -> Option<&String> { files.iter().find(|(p, _)| p == rel).map(|(_, c)| c) };
        // The crates the synthetic workspace derives: the base fixture's two
        // plus whatever SYNTH_HELPER_CRATES splices in.
        let scanned_crate = |def_file: &str| -> bool {
            let krate = def_file
                .strip_prefix("crates/")
                .and_then(|rest| rest.split('/').next());
            krate.is_some_and(|k| {
                k == "aterm-gui"
                    || k == "aterm-types"
                    || SYNTH_HELPER_CRATES.iter().any(|(name, _)| *name == k)
            })
        };
        let acquires = |contents: &str, identity: &str| -> bool {
            STANDARD_METHOD_NAMES.iter().any(|m| {
                contents.contains(&format!("{identity}.{m}("))
                    || contents.contains(&format!("{identity}().{m}("))
            })
        };
        for h in GUARD_HELPERS {
            assert!(
                scanned_crate(h.def_file),
                "GUARD_HELPERS entry `{}` is registered in {}, a crate the synthetic \
                 workspace never scans — add its crate to SYNTH_HELPER_CRATES (the OB-7 \
                 interior check reads the SCANNED corpus)",
                h.symbol,
                h.def_file
            );
            let contents = find(h.def_file).unwrap_or_else(|| {
                panic!(
                    "GUARD_HELPERS entry `{}` has no fixture file at its registered \
                     def_file {} — add it to synth_helper_files() (one fixture entry per \
                     def_file; helpers sharing a file share its contents), or every \
                     synthetic tree fails OB-7 as a STALE registration",
                    h.symbol, h.def_file
                )
            });
            assert!(
                contents.contains(&format!("fn {}(", h.symbol)),
                "the fixture at {} does not define `fn {}` — the registry's forward \
                 fail-closed check reds every synthetic tree until it does",
                h.def_file,
                h.symbol
            );
            assert!(
                acquires(contents, h.identity),
                "the fixture at {} defines `fn {}` but its body never acquires the \
                 registered identity `{}` — the registry's interior check reads the \
                 SCANNED corpus, so the mirror must acquire what the shipping helper does",
                h.def_file,
                h.symbol,
                h.identity
            );
        }
        for v in VOCABULARY_INTERIORS {
            assert!(
                scanned_crate(v.def_file),
                "VOCABULARY_INTERIORS entry `{}` is registered in {}, a crate the \
                 synthetic workspace never scans — add its crate to SYNTH_HELPER_CRATES",
                v.symbol,
                v.def_file
            );
            let contents = find(v.def_file).unwrap_or_else(|| {
                panic!(
                    "VOCABULARY_INTERIORS entry `{}` has no fixture file at its registered \
                     def_file {} — add it to synth_helper_files()",
                    v.symbol, v.def_file
                )
            });
            assert!(
                contents.contains(&format!("fn {}(", v.symbol)) && acquires(contents, "self"),
                "the fixture at {} must define `fn {}` WITH its bare-`self` acquisition — \
                 both halves of the interior registry are fail-closed",
                v.def_file,
                v.symbol
            );
        }
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

    /// The aliases ledger of a fn that has bound none.
    fn no_aliases() -> BTreeMap<String, (i32, String)> {
        BTreeMap::new()
    }

    /// A site context outside any `impl` block, with no live aliases: the
    /// walker's plain resolution, unrefined.
    fn bare_ctx(aliases: &BTreeMap<String, (i32, String)>) -> SiteCtx<'_> {
        SiteCtx {
            impls: &[],
            lineno: 1,
            aliases,
        }
    }

    #[test]
    fn receiver_resolution_handles_fields_accessors_and_unknowns() {
        let aliases = no_aliases();
        let ctx = bare_ctx(&aliases);
        let acqs = acquisitions_on("let g = self.store.read().unwrap();", &ctx);
        assert_eq!(acqs.len(), 1);
        assert_eq!(acqs[0].identity.as_deref(), Some("store"));
        let acqs = acquisitions_on("proxies().write().unwrap().insert(child, entry);", &ctx);
        assert_eq!(acqs[0].identity.as_deref(), Some("proxies"));
        // Outside any nameable impl, `self.0` / single-letter receivers are
        // UNKNOWN, not dropped.
        let acqs = acquisitions_on("*self.0.lock().unwrap() = None;", &ctx);
        assert_eq!(acqs.len(), 1);
        assert!(acqs[0].identity.is_none());
        let acqs = acquisitions_on("a.read().unwrap().clone()", &ctx);
        assert!(acqs[0].identity.is_none());
        // term_lock is the registered helper, identity `term`; its def is not
        // an acquisition.
        let acqs = acquisitions_on("let mut t = term_lock(&s.term);", &ctx);
        assert_eq!(acqs[0].identity.as_deref(), Some("term"));
        assert!(
            acquisitions_on("pub(crate) fn term_lock(term: &Mutex<Terminal>) {", &ctx).is_empty()
        );
    }

    #[test]
    fn self_receivers_take_the_enclosing_impl_type() {
        let impls = impl_spans("impl Spill {\n    fn f(&self) {\n        drop(0);\n    }\n}\n");
        let aliases = no_aliases();
        let ctx = SiteCtx {
            impls: &impls,
            lineno: 3,
            aliases: &aliases,
        };
        // The three self-rooted shapes, each qualified by the impl type.
        assert_eq!(
            acquisitions_on("*self.0.lock().unwrap() = None;", &ctx)[0]
                .identity
                .as_deref(),
            Some("Spill::0")
        );
        assert_eq!(
            acquisitions_on("let g = self.lock().unwrap();", &ctx)[0]
                .identity
                .as_deref(),
            Some("Spill::self")
        );
        assert_eq!(
            acquisitions_on("let g = self.pair.1.read().unwrap();", &ctx)[0]
                .identity
                .as_deref(),
            Some("Spill::pair.1")
        );
        // THE SEAM between the `?` rule and this refinement, and the reason
        // the `?` strip runs FIRST: at the raw dot the token before it is `?`,
        // `self_rooted_chain` finds no chain, and a field this impl can name
        // falls back to UNKNOWN — the punctuation accident the `?` fix exists
        // to close, reintroduced one layer up. Neither change's own tests
        // cover the composition; this one does.
        assert_eq!(
            acquisitions_on("*self.0?.lock().unwrap() = None;", &ctx)[0]
                .identity
                .as_deref(),
            Some("Spill::0")
        );
        assert_eq!(
            acquisitions_on("let g = self.pair.1??.read().unwrap();", &ctx)[0]
                .identity
                .as_deref(),
            Some("Spill::pair.1")
        );
        // A NAMED field keeps the plain identity it has always had: renaming
        // those would split every existing edge in the graph.
        assert_eq!(
            acquisitions_on("let g = self.store.read().unwrap();", &ctx)[0]
                .identity
                .as_deref(),
            Some("store")
        );
        // A LOCAL's tuple field is not this type's field — never qualified.
        assert!(
            acquisitions_on("let g = pair.0.lock().unwrap();", &ctx)[0]
                .identity
                .is_none()
        );
        // Outside the impl's line span the qualification is gone (fail-closed).
        let outside = SiteCtx {
            impls: &impls,
            lineno: 99,
            aliases: &aliases,
        };
        assert!(
            acquisitions_on("*self.0.lock().unwrap() = None;", &outside)[0]
                .identity
                .is_none()
        );
    }

    #[test]
    fn impl_headers_resolve_to_a_nameable_self_type_or_none() {
        for (header, want) in [
            ("impl Spill ", Some("Spill")),
            ("impl<T> Spill<T> ", Some("Spill")),
            ("impl fmt::Display for Spill ", Some("Spill")),
            (
                "impl<'a, T: Clone> Sink<'a> for Spill<'a, T> ",
                Some("Spill"),
            ),
            (
                "impl<K, V, S> IndexMap<K, V, S> where S: Hash, ",
                Some("IndexMap"),
            ),
            ("unsafe impl Send for Spill ", Some("Spill")),
            ("impl Sink for &'a mut Spill ", Some("Spill")),
            // The shapes that must NEVER lend an identity — each one would
            // merge locks that never meet.
            ("impl<T> MutexExt<T> for Mutex<T> ", None), // the lock itself
            ("impl<T> Ext for Arc<T> ", None),           // a transparent wrapper
            ("impl<T> ToSmolStr for T ", None),          // a blanket impl
            ("impl<F: Fn() -> u32, T> Ext for T ", None), // `->` must not close `<`
            ("impl<T> UpdateSlice for &[T] ", None),     // a structural type
            ("impl FfiErrorCode for $ty ", None),        // a macro fragment
        ] {
            assert_eq!(
                impl_self_type(header).as_deref(),
                want,
                "impl header `{header}`"
            );
        }
    }

    #[test]
    fn single_letter_receivers_take_the_path_they_alias() {
        let aliases = no_aliases();
        // A pure-path RHS lends its identity; anything else does not.
        assert_eq!(
            alias_target("let m = &self.spill;", &bare_ctx(&aliases)).as_deref(),
            Some("spill")
        );
        assert_eq!(
            alias_target("let q = &QUEUE;", &bare_ctx(&aliases)).as_deref(),
            Some("QUEUE")
        );
        assert!(alias_target("let m = pick(a, b);", &bare_ctx(&aliases)).is_none());
        assert!(alias_target("let m = self.spill.lock().unwrap();", &bare_ctx(&aliases)).is_none());
        assert!(alias_target("let m = if x { &a } else { &b };", &bare_ctx(&aliases)).is_none());
        // The aliased receiver then resolves exactly as the field spelling
        // would have — no new identity, so no new merge class.
        let mut aliases = no_aliases();
        aliases.insert("m".to_string(), (0, "spill".to_string()));
        assert_eq!(
            acquisitions_on("let g = m.lock().unwrap();", &bare_ctx(&aliases))[0]
                .identity
                .as_deref(),
            Some("spill")
        );
    }

    #[test]
    fn guard_final_value_distinguishes_bound_guard_from_consumed_value() {
        let aliases = no_aliases();
        let ctx = bare_ctx(&aliases);
        // Bound guard: only poison adapters between the call and `;`.
        let line = "let g = store.read().unwrap_or_else(|p| p.into_inner());";
        let acq = &acquisitions_on(line, &ctx)[0];
        assert!(guard_is_final_value(line, acq));
        // Consumed within the statement: `.by_local(..)` follows the adapters.
        let line = "let gone = self.store.read().unwrap_or_else(|p| p.into_inner()).by_local(s);";
        let acq = &acquisitions_on(line, &ctx)[0];
        assert!(!guard_is_final_value(line, acq));
        // Helper call bound directly.
        let line = "let mut term = term_lock(&s.term);";
        let acq = &acquisitions_on(line, &ctx)[0];
        assert!(guard_is_final_value(line, acq));
    }

    #[test]
    fn literals_are_masked_before_token_scans() {
        let aliases = no_aliases();
        let ctx = bare_ctx(&aliases);
        assert!(
            acquisitions_on(
                &mask_literals(r#"log!("call .lock() and term_lock(x)");"#),
                &ctx
            )
            .is_empty()
        );
        let masked = mask_literals(r#"let b = matches!(c, '{');"#);
        assert_eq!(masked.matches('{').count(), 0);
    }

    /// THE RUNAWAY THIS MASKER WAS REWRITTEN TO STOP, plus every shape the
    /// rewrite had to learn before it stopped over-masking instead.
    ///
    /// Each case is a shape the brace-counting predecessor got WRONG on this
    /// tree, cited with where it was measured. The predecessor counted `{` and
    /// `}` over raw line text, so one literal brace desynchronised it forever;
    /// the first cut of the replacement ended a body only at `<indent>}`, which
    /// is right for a `mod`/`fn` and catastrophic for the brace-less items
    /// (enum variant, match arm, struct field) that end at a comma.
    ///
    /// A fixture is a slice of LINES rather than one `\n`-spliced literal
    /// because every assertion here is about column-0 vs column-4 indentation,
    /// and an escaped literal hides exactly that.
    #[test]
    fn gated_items_end_at_their_own_shape_not_at_a_brace_count() {
        fn src(lines: &[&str]) -> String {
            let mut s = lines.join("\n");
            s.push('\n');
            s
        }
        let gates: &[&str] = &["#[cfg(test)]"];

        // (1) THE RUNAWAY. One `'{'` char literal (net +1 to a counter that
        // reads raw text; the `"{}"` beside it nets 0) left the depth stuck
        // above zero, the close never held again, and every later line of the
        // file was blanked. crates/aterm-gui/src/lib.rs lost 19 553 lines and
        // three shipping items this way, `spawn_pkg_update_check` among them.
        let m = mask_gated_items(
            &src(&[
                "#[cfg(test)]",
                "mod tests {",
                "    fn t() {",
                "        assert!(matches!(c, '{'), \"{}\", render());",
                "    }",
                "}",
                "fn spawn_pkg_update_check() {",
                "    let g = pkg.lock();",
                "}",
            ]),
            gates,
        );
        assert!(
            !m.contains("render()"),
            "the test mod must be blanked:\n{m}"
        );
        assert!(
            m.contains("fn spawn_pkg_update_check") && m.contains("pkg.lock()"),
            "shipped code AFTER a gated mod must survive:\n{m}"
        );
        assert_eq!(
            m.matches('{').count(),
            m.matches('}').count(),
            "the mask must leave the file brace-balanced:\n{m}"
        );

        // (2) AN ENUM VARIANT ends at its comma, not at the next brace it can
        // find. crates/aterm-gui/src/lib.rs:2570 gates one variant of the
        // event enum; ending it at a brace swallowed the next 830 lines of
        // shipped variants.
        let m = mask_gated_items(
            &src(&[
                "enum Ev {",
                "    Shipped(u8),",
                "    #[cfg(test)]",
                "    TestOnly,",
                "    AlsoShipped {",
                "        payload: u8,",
                "    },",
                "}",
            ]),
            gates,
        );
        assert!(!m.contains("TestOnly"), "the gated variant must go:\n{m}");
        assert!(
            m.contains("AlsoShipped") && m.contains("payload"),
            "the variants AFTER it must survive:\n{m}"
        );

        // (3) A MATCH ARM, same rule, one nesting level deeper — the shape at
        // crates/aterm-gui/src/lib.rs:9427.
        let m = mask_gated_items(
            &src(&[
                "fn settings(&self) -> Option<&S> {",
                "    match &self.overlay {",
                "        #[cfg(test)]",
                "        Some(Overlay::Settings(s)) => Some(s),",
                "        _ => None,",
                "    }",
                "}",
            ]),
            gates,
        );
        assert!(!m.contains("Overlay::Settings"), "arm must go:\n{m}");
        assert!(m.contains("_ => None"), "the next arm must survive:\n{m}");

        // (4) THE COMMA RULE MUST NOT REACH A WRAPPED SIGNATURE: `a: u32,` is
        // indented DEEPER than the gate, so it is a parameter, not an end.
        // Reading it as one would leave a gated body scanned as if it shipped
        // — the wasm-posture bug the predecessor's forward walk was added for.
        let m = mask_gated_items(
            &src(&[
                "#[cfg(test)]",
                "fn helper(",
                "    a: u32,",
                ") -> u32 {",
                "    alpha.lock();",
                "}",
                "fn shipped() {}",
            ]),
            gates,
        );
        assert!(!m.contains("alpha.lock()"), "gated body must go:\n{m}");
        assert!(m.contains("fn shipped"), "the next item must survive:\n{m}");

        // (5) A GATED STATEMENT whose argument list carries a brace in a
        // FORMAT STRING — crates/aterm-scrollback/src/cold_tier.rs:612. The
        // walk looking for the body's `{` must read literal-masked text, or it
        // mistakes the format string for the body and cuts the statement in
        // half (which then splices the file for the brace counters downstream).
        let m = mask_gated_items(
            &src(&[
                "fn truncate(&mut self, n: usize) {",
                "    #[cfg(test)]",
                "    debug_assert!(",
                "        n <= self.count,",
                "        \"truncate({n}) exceeds count({})\",",
                "        self.count",
                "    );",
                "    self.count -= n;",
                "}",
            ]),
            gates,
        );
        assert!(!m.contains("debug_assert"), "the assert must go:\n{m}");
        assert!(
            m.contains("self.count -= n;"),
            "the shipped statement after it must survive:\n{m}"
        );
        assert_eq!(
            m.matches('{').count(),
            m.matches('}').count(),
            "the enclosing fn must still be brace-balanced:\n{m}"
        );

        // (6) FAIL-OPEN. A body whose close is not at the gate's indent is a
        // shape this walker does not recognise, and the answer is to mask
        // NOTHING of it: a visible test lock is a false positive a human
        // answers, a blanked shipped one is a census that reports GREEN
        // because it cannot see. It must also stay brace-BALANCED — blanking
        // the `{` while its `}` survives is what moved the close of
        // `struct WindowState`'s body, under the first cut of this repair,
        // from crates/aterm-gui/src/lib.rs:8681 to 10783.
        let m = mask_gated_items(
            &src(&[
                "#[cfg(test)]",
                "fn odd() {",
                "    let g = alpha.lock();",
                "  }",
                "fn shipped() {}",
            ]),
            gates,
        );
        assert!(
            m.contains("fn odd") && m.contains("alpha.lock()"),
            "an unrecognised shape must be masked LESS, not more:\n{m}"
        );
        assert_eq!(
            m.matches('{').count(),
            m.matches('}').count(),
            "the mask must never splice a brace off its partner:\n{m}"
        );

        // Line count is preserved in every case, or every span the censuses
        // print afterwards is off by the size of the mask.
        let raw = src(&["#[cfg(test)]", "mod t {", "    fn a() {}", "}", "fn b() {}"]);
        assert_eq!(
            mask_gated_items(&raw, gates).lines().count(),
            raw.lines().count()
        );
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
    fn synthetic_tuple_field_abba_across_two_types_is_red() {
        // THE COUNTERFACTUAL the impl-type qualification exists for. Both
        // halves of this ABBA are spelled `self.0.lock()`. While such a
        // receiver resolved to a per-site UNKNOWN node it could never unify,
        // so this cycle — a real deadlock, one thread per method pair — was
        // undetectable BY CONSTRUCTION. Qualified by the enclosing impl type
        // the two nodes are `Alpha::0` and `Beta::0`, and the cycle closes.
        let out = run_synth(
            "selffieldabba",
            "impl Alpha {\n    fn ab(&self) {\n        \
             let g = self.0.lock().unwrap();\n        self.beta_step();\n    }\n    \
             fn alpha_step(&self) {\n        self.0.lock().unwrap().push(1);\n    }\n}\n\
             impl Beta {\n    fn ba(&self) {\n        \
             let g = self.0.lock().unwrap();\n        self.alpha_step();\n    }\n    \
             fn beta_step(&self) {\n        self.0.lock().unwrap().push(1);\n    }\n}\n",
        );
        assert!(!out.ok, "a `self.0` ABBA must be RED:\n{}", out.log);
        assert!(
            out.log.contains("EDGE Alpha::0 -> Beta::0")
                && out.log.contains("EDGE Beta::0 -> Alpha::0"),
            "both edges of the cycle must be named with their qualified \
             identities:\n{}",
            out.log
        );
        assert!(
            out.log.contains("crates/aterm-gui/src/extra.rs:3")
                && out.log.contains("crates/aterm-gui/src/extra.rs:16"),
            "both sites of the cycle must be named:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[via call to `beta_step`")
                && out.log.contains("[via call to `alpha_step`"),
            "the one-hop witness of each half must be named:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_same_field_on_different_types_does_not_merge() {
        // The other half of the ruling: the qualification must be at least as
        // DISCRIMINATING as the UNKNOWN it replaces. `Alpha::0` and `Beta::0`
        // are different locks; merging them into one `0` node would invent
        // exactly the cycle this tree does not have (`0 -> shared_lock` and
        // `shared_lock -> 0`), and a false cycle has no waiver channel to
        // escape through — it can only be "repaired" by rewriting correct code.
        let out = run_synth(
            "selffieldnomerge",
            "impl Alpha {\n    fn ab(&self) {\n        \
             let g = self.0.lock().unwrap();\n        \
             let h = shared_lock.lock().unwrap();\n    }\n}\n\
             impl Beta {\n    fn ba(&self) {\n        \
             let g = shared_lock.lock().unwrap();\n        \
             let h = self.0.lock().unwrap();\n    }\n}\n",
        );
        assert!(
            out.ok,
            "same-named fields on DIFFERENT types must not merge into a \
             cycle:\n{}",
            out.log
        );
        assert!(
            out.log.contains("Alpha::0 -> shared_lock")
                && out.log.contains("shared_lock -> Beta::0"),
            "both nestings must be tracked, under distinct identities:\n{}",
            out.log
        );
        assert!(
            out.log.contains("Alpha::0(1)") && out.log.contains("Beta::0(1)"),
            "each type's field must be its own identity in the ledger:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_unqualifiable_impl_types_keep_the_per_site_node() {
        // Fail-closed the other way. A blanket `impl<T> .. for T` and an
        // extension trait on the lock primitive itself both implement a whole
        // FAMILY of types: `T::self` / `Mutex::self` would be one node for
        // every lock in the process — the merge that manufactures cycles. Both
        // stay UNKNOWN (seen, counted, unable to close a cycle).
        let out = run_synth(
            "blanketself",
            "impl<T> Peek for T {\n    fn peek(&self) {\n        \
             self.0.lock().unwrap().len();\n    }\n}\n\
             impl<T> Ext for Mutex<T> {\n    fn twice(&self) {\n        \
             self.lock().unwrap().len();\n    }\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("2 UNKNOWN-identity site(s)"),
            "neither shape may borrow a type name:\n{}",
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
    fn synthetic_file_ascription_is_evidence_and_generics_are_not() {
        // The compiler-enforced ascription arm: a binding declared
        // `: std::fs::File` is File evidence even when the constructor lives
        // behind a helper (the exact shape whose loss silently graphed the
        // update flock as a mutex, 2026-08-22). A GENERIC mentioning the type
        // (`Option<std::fs::File>`) is NOT the ascription and must stay
        // fail-closed UNKNOWN.
        let out = run_synth(
            "fileascription",
            "fn ledger_lock(path: &Path) -> Option<std::fs::File> {\n    \
             let f: std::fs::File = open_lock_file(path).ok()?;\n    \
             f.lock().ok()?;\n    Some(f)\n}\n\
             fn generic_is_not_evidence(path: &Path) {\n    \
             let g: Option<std::fs::File> = maybe_file(path);\n    \
             let g = g.unwrap();\n    g.lock().unwrap();\n}\n",
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("1 OS file-advisory"),
            "the ascribed binding is advisory evidence; log:\n{}",
            out.log
        );
        assert!(
            out.log.contains("1 UNKNOWN-identity site(s)"),
            "the generic-typed rebind must stay UNKNOWN; log:\n{}",
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
    fn a_vendored_fn_name_cannot_capture_an_aterm_one_hop_call() {
        // THE LOADED GUN the namespace boundary disarms. Vendored-identity mode
        // namespaces the vendored crates' lock IDENTITIES, but their fn NAMES
        // land in the same by-name callee table as aterm's. Each side must
        // resolve to ITS OWN definition: an aterm hold that inherited a
        // vendored lock identity could close a cycle NOTHING could repair,
        // because vendored sources may not be edited and OB-7 has no waiver
        // channel.
        //
        // The instance that motivated this was winnow, which defined `fn start`
        // and `fn end` (combinator/debug/internals.rs) both taking
        // `writer.lock()`, against aterm's own `start`/`end` with hop-eligible
        // call sites. That fork left the tree with `toml`/`toml_edit`, so the
        // fixture is planted under a fork that is still here. The hazard did
        // not leave with it — it belongs to ANY vendored crate sharing a
        // function name with aterm — and a fixture pinned to a deleted
        // directory tests nothing at all.
        //
        // The complementary case — a name the caller's namespace does NOT define,
        // which must still resolve across the boundary — is
        // `synthetic_cross_boundary_one_hop_edge_is_graphed` above.
        let out = run_synth_files(
            "nsshadow",
            "fn start() {\n    aterm_trace.lock().unwrap().push(1);\n}\n\
             fn aterm_hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
             start();\n}\n",
            &[(
                "vendor/smol_str/src/planted.rs",
                "fn start() {\n    writer.lock().unwrap().push(1);\n}\n\
                 fn vendored_hold_and_call() {\n    let g = stream.lock().unwrap();\n    \
                 start();\n}\n",
            )],
        );
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        assert!(
            out.log.contains("alpha_lock -> aterm_trace"),
            "the aterm call site must still resolve to ATERM's `start`:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("alpha_lock -> smol_str::writer"),
            "an aterm hold must NOT inherit a vendored lock identity through a \
             shared fn name:\n{}",
            out.log
        );
        assert!(
            out.log.contains("smol_str::stream -> smol_str::writer"),
            "the vendored call site must resolve to the VENDORED `start`:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("smol_str::stream -> aterm_trace"),
            "and the boundary holds in both directions — a vendored hold must \
             not reach an aterm fn of the same name:\n{}",
            out.log
        );
    }

    #[test]
    fn a_question_mark_receiver_still_names_its_lock() {
        // `self.store?.read()` — the `?` unwraps an Option/Result; the lock on
        // the other side is still `store`. Reporting it UNKNOWN would widen the
        // census's honesty gap for a punctuation mark, and UNKNOWN nodes never
        // unify, so a real ordering through this lock could not be seen.
        //
        // Deliberately a `bare_ctx`: outside any impl, with no live aliases,
        // the self/single-letter REFINEMENTS cannot fire, so the two UNKNOWN
        // assertions below still test the `?` rule alone rather than silently
        // becoming assertions about `impl_ty()`. The refinements have their
        // own tests, and one of them pins `self.0?` UNDER an impl.
        let aliases = no_aliases();
        let ctx = bare_ctx(&aliases);
        let acqs = acquisitions_on(
            "let g = self.store?.read().unwrap_or_else(|p| p.into_inner());",
            &ctx,
        );
        assert_eq!(acqs.len(), 1);
        assert_eq!(
            acqs[0].identity.as_deref(),
            Some("store"),
            "a `?` before the acquisition dot must not erase the receiver"
        );
        // Chained `??` (Option<Result<_>>) resolves the same way.
        let acqs = acquisitions_on("self.inner??.lock().unwrap();", &ctx);
        assert_eq!(acqs[0].identity.as_deref(), Some("inner"));
        // And the genuinely unresolvable receivers stay UNKNOWN — the fix must
        // not become a way to invent names.
        let acqs = acquisitions_on("*self.0?.lock().unwrap() = None;", &ctx);
        assert!(acqs[0].identity.is_none());
        let acqs = acquisitions_on("a?.read().unwrap().clone()", &ctx);
        assert!(acqs[0].identity.is_none());
    }

    #[test]
    fn a_field_hop_is_followed_when_the_field_type_and_the_callee_both_resolve() {
        // THE TRUE POSITIVE the field-receiver rule must keep. `self.shared.…`
        // where `shared: Arc<Shared>` and `Shared` is a corpus struct, calling a
        // callee only ONE corpus fn bears: both halves of the evidence hold, so
        // the hop is followed and its held-acquire edge is graphed.
        let out = run_synth(
            "fieldhop",
            "struct Holder {\n    shared: Arc<Shared>,\n}\nstruct Shared {\n    inner_lock: Mutex<u8>,\n}\nfn peek_shared(&self) {\n    beta_lock.lock().unwrap();\n}\nfn hold_and_hop(&self) {\n    let g = alpha_lock.lock().unwrap();\n    self.shared.peek_shared();\n}\n",
        );
        assert!(
            out.log.contains("alpha_lock -> beta_lock"),
            "a field hop with a corpus field type AND a unique callee must be \
             FOLLOWED — dropping it is a silent missed edge:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("shared.peek_shared"),
            "a followed hop must not also be listed as not-followed:\n{}",
            out.log
        );
    }

    #[test]
    fn a_field_hop_is_not_followed_when_the_callee_name_is_ambiguous() {
        // THE REGRESSION GUARD for the phantom cycle this rule was written for:
        // aterm defines its own `Condvar` newtype, so the FIELD type resolves —
        // but two corpus fns are named `wait`, so the by-name lookup would PICK
        // one rather than resolve it. Following it bound a guard-RELEASING
        // `Condvar::wait` to an unrelated `wait` that acquires, and closed a
        // two-lock cycle that does not exist. OB-7 has no waiver channel, so
        // that phantom edge blocked every build of the freeze gate.
        let out = run_synth(
            "fieldambig",
            "struct Holder {\n    parked: Condvar,\n}\nstruct Condvar {\n    raw: u8,\n}\nfn wait(&self) {\n    beta_lock.lock().unwrap();\n}\nfn hold_and_hop(&self) {\n    let g = alpha_lock.lock().unwrap();\n    self.parked.wait(g);\n}\nfn wait(&self) {\n    gamma_lock.lock().unwrap();\n}\n",
        );
        assert!(
            !out.log.contains("alpha_lock -> beta_lock")
                && !out.log.contains("alpha_lock -> gamma_lock"),
            "an ambiguous callee name must NOT be resolved by picking a \
             candidate — that is how the phantom cycle was fabricated:\n{}",
            out.log
        );
        assert!(
            out.log.contains("parked.wait"),
            "a hop the census declined to follow must be LISTED, so the \
             narrowing is auditable rather than silent:\n{}",
            out.log
        );
    }

    #[test]
    fn a_field_hop_is_not_followed_when_the_field_type_is_foreign() {
        // The other half of the evidence: a unique callee name is not enough if
        // the field's type is not defined here. `open: AtomicBool` is std, so
        // `.load(..)` is std's — not the corpus fn that happens to share a name.
        let out = run_synth(
            "fieldforeign",
            "struct Holder {\n    open: AtomicBool,\n}\nfn load(&self) {\n    beta_lock.lock().unwrap();\n}\nfn hold_and_hop(&self) {\n    let g = alpha_lock.lock().unwrap();\n    self.open.load(Ordering::Relaxed);\n}\n",
        );
        assert!(
            !out.log.contains("alpha_lock -> beta_lock"),
            "a foreign field type must not resolve to a same-named corpus \
             fn:\n{}",
            out.log
        );
        assert!(
            out.log.contains("open.load"),
            "the declined hop must be listed:\n{}",
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
        // The three real OS file-advisory sites (`restore::with_restore_lock`'s
        // sibling-lock flock over the restore manifest; the updater's
        // install-ledger lock in aterm-update-core — its blocking `acquire`
        // plus the bounded-wait `try_lock` loop `acquire_within` grew for the
        // launch path) must be classified by File EVIDENCE, listed with their
        // binding spans, and excluded from the mutex graph —
        // existence-checked here so the classification cannot silently rot into
        // UNKNOWN (or vanish).
        //
        // NOTE (2026-07-24): this named `crates/aterm-gui/src/kitty_log.rs` for
        // the first site long after that flock moved to `restore.rs` — kitty_log
        // has carried ZERO `.lock()` calls since. The assertion had been failing
        // on `main` for an unknown span, i.e. this gate was dark. Naming the
        // ACTUAL site restores its teeth.
        //
        // NOTE (2026-08-22): the update flock's constructor moved behind
        // `FileLock::open_lock_file`, which stripped the binding of its lexical
        // File evidence and silently GRAPHED the flock as an in-process mutex —
        // this test is what caught it. The bindings now carry an explicit
        // `: std::fs::File` ascription (compiler-enforced evidence the census
        // accepts), and the count includes `acquire_within`'s try_lock loop.
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log.contains("3 OS file-advisory"),
            "expected exactly the restore-manifest flock plus update-core's two \
             sites (blocking acquire + the bounded-wait try_lock loop) in the \
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
        // aterm-gui process surface. The exact member list is pinned by
        // scan_set's derived_closure_matches_the_pinned_canary, and the crate
        // COUNT the transcript must report is that pin's length — never a
        // literal retyped here: the literal sat at 51 while the pin (and the
        // tree) were at 52, so this test failed for a crate the canary had
        // already been made to review. One reviewed list, one count. This
        // asserts the census actually WALKS the derived set and reports its
        // provenance + exclusions in the transcript.
        // The closure grew as first-party crates replaced third-party ones
        // (aterm-winit-keymap out of aterm-types, aterm-agent + aterm-ctl with
        // the embedded operator, aterm-digest for sha2+hmac, aterm-time for
        // web-time, aterm-regex for regex, aterm-primer with the agent
        // auto-prime) — which is exactly why the count is derived, not typed.
        let pinned = crate::scan_set::test_fixtures::PINNED_GUI_CLOSURE.len();
        // DERIVED for the same reason, and it had drifted the same way: the
        // vendored half stayed a literal `5` while the winnow fork left the
        // tree with `toml`/`toml_edit`, so this test failed for a retirement
        // the registry had already been made to review. Count the registry's
        // Scanned entries — the build-time-only ones are excluded from the
        // process and reported separately.
        let scanned_vendored = crate::scan_set::REVIEWED_VENDORED_CRATES
            .iter()
            .filter(|v| matches!(v.mode, crate::scan_set::VendoredMode::Scanned { .. }))
            .count();
        let out = run_lock_order_census(&repo_root());
        assert!(
            out.log.contains(&format!(
                "across {pinned} workspace crate(s) + {scanned_vendored} vendored crate(s)"
            )),
            "the census must report the full derived closure ({pinned} crates, the \
             canary's pin) + the {scanned_vendored} scanned vendored crate(s):\n{}",
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
        // identities; harmless here since neither nests). winnow used to
        // contribute three stderr-stream locks here as `winnow::writer`; that
        // fork left the tree when `toml`/`toml_edit` were retired for
        // aterm-toml, so the identity is gone with it and the assertion below
        // no longer names it. indexmap/libm/smol_str: zero sites, the walker
        // re-checks that claim every run. The non-macOS winit backends are labeled slices, counted and
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
            out.log.contains("winit::inner(1)") && out.log.contains("winit::new_inner_size(1)"),
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
