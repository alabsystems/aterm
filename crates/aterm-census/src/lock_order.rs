// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lock-order census: the lock-graph sense of L0-DEADLOCK, as a fail-closed
//! obligation (docs/L0-CATALOG.md, the L0-DEADLOCK row).
//!
//! Model deadlock (a `ty_model!` state with no enabled action) is ty's job;
//! this census covers lock-graph deadlock, the ABBA cycle between real mutexes.
//! There is no waiver channel: an allowlisted cycle would be a shipped
//! deadlock, so a detected cycle can only be fixed (impose the order, or narrow
//! a guard's scope).
//!
//! 1. Lock-site enumeration: every acquisition (`term_lock(`, `.lock()`,
//!    `.lock_or_recover()`, `.read()`, `.write()`, `.try_*()`) resolved to a
//!    lexical identity (the receiver name; a `self`-rooted chain takes the
//!    enclosing impl type's, `Spill::0`). An unresolvable site is reported as
//!    UNKNOWN and counted.
//! 2. Acquired-while-holding pairs: within each fn, an acquisition inside the
//!    live scope of an earlier guard, plus one interprocedural hop (a call made
//!    while holding, into a fn that acquires).
//! 3. The graph: edges `A -> B` ("B acquired while A held"); any cycle fails
//!    the census, naming every edge with both sites. Self-edges fail as
//!    re-entrancy suspects (std locks are not re-entrant, and cross-instance
//!    nesting needs an instance order this census cannot see).
//!
//! Limits are in [`LOCK_PRECISION_NOTE`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::{
    CensusOutcome, Floor, collect_rs_files, fn_defined_in, ident_ending_at, is_test_file,
    parse_source_fns, strip_line_comment,
};

/// The walker's limits, printed with a red walk's findings.
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
        `unwrap`/`expect`/`unwrap_or_else`/`map_err` adapters only) lives to the end of its
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
        same-named fn and reported a lock cycle that does not exist. A bare
        call to a simply named fn parameter or live local binding is an
        unresolved callback, never a same-named corpus fn; those held calls
        are counted and listed too. Locals shadow only after their initializer
        statement, until their block ends. Qualified paths and `self.name(..)`
        are not shadowed by a parameter or local.
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
        items masked (test code never compiles into the shipped GUI process);
        string/char literals masked before token scans;
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

// The scanned crates are the aterm-gui process (one process, one lock
// namespace), derived every run by [`crate::scan_set::derive_gui_scan_set`];
// vendored `[patch]` forks are scanned in vendored-identity mode.

/// Acquisition vocabulary: the method-call tokens that take a lock, and
/// whether the acquisition can block (a `try_*` cannot, so it is never the
/// waiting half of a deadlock).
///
/// Scan invariant (relied on by [`acquisitions_on`]): every token starts with
/// `.`, contains no other `.`, and no token is a prefix of another, so one pass
/// over each `.` with `starts_with` finds every match.
const ACQ_METHODS: &[(&str, bool)] = &[
    (".lock()", true),
    (".lock_or_recover()", true),
    (".read()", true),
    (".write()", true),
    (".try_lock()", false),
    (".try_read()", false),
    (".try_write()", false),
];

/// Result-of-lock adapters that pass the guard through: a chain of only these
/// to the end of a `let` means the guard itself is bound (block-scoped), not a
/// value read out of it (statement-scoped).
const GUARD_ADAPTERS: &[&str] = &["unwrap", "expect", "unwrap_or_else", "map_err"];

/// A fn that acquires a lock and returns the guard: its callers hold the lock
/// with no `.lock()` token at the call site, so each registered helper call
/// counts as a direct acquisition of the declared identity. Fail-closed both
/// ways:
///   * every entry must still define `fn <symbol>` at `def_file`, and its body
///     must still acquire the declared identity;
///   * every guard-returning fn found in the corpus must be registered here, or
///     be a method (takes `self`) with a standard name the token vocabulary
///     covers. A free fn named `lock`/`read`/… is not exempt: its call sites
///     (`lock(&X)`) carry no `.lock()` token.
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
        // The windowed terminal lives in the library so the single `aterm`
        // binary can serve it.
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        // The UI thread's registering `term_lock` that books its wait: it
        // blocks on `term_lock(term)`, so it is the same lock and graph node.
        symbol: "term_lock_ui",
        identity: "term",
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        // Settings ▸ Security's conflicting-copies retire worker: returns the
        // guard of `RetireState.shared`, which the worker also locks as
        // `shared`. Named `retire_slot` because a bare `slot(` would match
        // aterm-effects' `SkyIndex::slot`.
        symbol: "retire_slot",
        identity: "shared",
        def_file: "crates/aterm-gui/src/consent_retire.rs",
    },
    GuardHelper {
        // The PTY keeper link's state lock: returns the guard of the
        // `KEEPER_STATE` static. Named `keeper_state` because a bare `state(`
        // is many crates' accessor.
        symbol: "keeper_state",
        identity: "KEEPER_STATE",
        def_file: "crates/aterm-gui/src/keeper_link.rs",
    },
    GuardHelper {
        symbol: "lock_fonts",
        identity: "chrome_fonts",
        def_file: "crates/aterm-gui/src/tray_raster.rs",
    },
    GuardHelper {
        // The workspace's process-environment mutation lock. `scoped`/
        // `scoped_unset` hold this guard across a caller-supplied body.
        symbol: "env_lock",
        identity: "ENV_LOCK",
        def_file: "crates/aterm-log/src/lib.rs",
    },
    GuardHelper {
        // Returns the guard of the `SEARCH_SNAPSHOTS` ring static.
        symbol: "search_cache_lock",
        identity: "SEARCH_SNAPSHOTS",
        def_file: "crates/aterm-gui/src/control_query.rs",
    },
    GuardHelper {
        // The session-connections record store: the per-instance
        // `(dst, src) -> ConnectionRecord` map behind one mutex. `records()`
        // is not a standard acquisition name, so its callers' holds need
        // registration. The identity takes the enclosing impl type
        // (`self.records.lock()` inside `impl ConnectionTable`).
        symbol: "records",
        identity: "records",
        def_file: "crates/aterm-gui/src/connections.rs",
    },
    GuardHelper {
        // The isearch prefix-narrowing frame stacks: a bare static `Mutex`
        // whose guard escapes to callers, registered like `search_cache_lock`.
        symbol: "narrow_sessions_lock",
        identity: "NARROW_SESSIONS",
        def_file: "crates/aterm-gui/src/control_query.rs",
    },
    GuardHelper {
        // Operator claims/management hold the host authority gate across
        // their durable queue transition; callers get the guard from here.
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
    GuardHelper {
        // `fabric attach`'s supervisor latch. `supervise` has no stop handle,
        // so `arm` must check-and-set under one hold or a second attach would
        // stack a second relaunch loop; this free fn returns that hold.
        symbol: "supervisor",
        identity: "SUPERVISOR",
        def_file: "crates/aterm-gui/src/fabric_launch.rs",
    },
    GuardHelper {
        // atpkg's terminal meter: the one stderr line it draws and its current
        // owner. `draw`, `around` and `TtyMeter`'s drop take it through this
        // free fn, and `around` keeps it across a caller's print.
        symbol: "screen_lock",
        identity: "SCREEN",
        def_file: "crates/atpkg/src/meter.rs",
    },
    GuardHelper {
        // The password-sheet gate returns its posting mutex to hwkey so an
        // admitted native event pair finishes before a sheet can attach.
        // Presentation drops the same mutex before entering AppKit.
        symbol: "posting_permit",
        identity: "posting",
        def_file: "crates/aterm-gui/src/askpass.rs",
    },
];

/// A fn that implements a standard acquisition method by delegating to the
/// wrapped std primitive through a bare-`self` receiver (`impl MutexExt for
/// Mutex<T> { fn lock_or_recover(&self) { self.lock() … } }`). The receiver is
/// each caller's lock, so the identity is resolved at every call site by the
/// token the fn implements. The interior site gets its own audited category
/// and a unique per-site node (like UNKNOWN, it can never close a cycle).
///
/// Fail-closed both ways:
///   * every entry must name a STANDARD_METHOD_NAMES symbol, else callers
///     would escape the token vocabulary;
///   * the fn must still be defined at `def_file` and still contain the
///     bare-`self` acquisition;
///   * only bare-`self` receivers qualify: a `self.<field>` delegation is
///     nameable.
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

/// Lexical `std::fs::File` evidence for a `let` RHS: a local bound from one of
/// these idioms is an OS file, so a later `.lock()`/`.try_lock()` on it is the
/// advisory file lock (a cross-process rendezvous), not an in-process mutex.
/// Only these exact shapes qualify.
fn is_file_binding_rhs(line: &str) -> bool {
    line.contains("File::open(")
        || line.contains("File::create(")
        || line.contains("File::create_new(")
        || (line.contains("OpenOptions::new()") && line.contains(".open("))
        // An explicit type ascription is compiler-enforced and survives the
        // constructor moving behind a helper. Exact-suffix match so
        // `Option<std::fs::File>` never qualifies; a bare `File` ascription is
        // not accepted because it carries no token anchoring it to std.
        || line.contains(": std::fs::File =")
        || line.contains(": fs::File =")
}

/// Raw-pointer-producing method names: a zero-arg `.read()` on the direct
/// result of one of these is `core::ptr::read`, not a lock.
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
/// these idioms is a raw pointer, so a later zero-arg `.read()` on it is
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

/// Propagated raw-pointer evidence: the RHS calls a raw-pointer-producing
/// method on a receiver already proven a raw pointer (`let item = base.add(i)`
/// with `base` in the ledger). The receiver must be a live proven binding.
fn rhs_extends_raw_ptr(line: &str, ptr_vars: &BTreeMap<String, (i32, String)>) -> bool {
    // Every `true` path needs `ptr_vars.contains_key(recv)`, and this runs on
    // every `let` line of the corpus with an almost always empty ledger.
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
/// `&`/`&mut`) in the fn signature text?
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
/// `Some(evidence)` iff the receiver is lexically proven a raw pointer (a live
/// raw-ptr `let` binding, a raw-ptr parameter, or the direct result of a
/// [`RAW_PTR_METHODS`] call). The site is then excluded from the mutex graph.
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

/// Method names the token vocabulary covers at call sites: a guard-returning
/// method with one of these names needs no helper registration.
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

/// The identity namespace of the aterm workspace crates, which carry no
/// prefix; only vendored `[patch]` forks do (`winit::…`).
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
    /// `Some(binding span)` when the receiver is lexically proven a
    /// `std::fs::File` (see [`is_file_binding_rhs`]): the OS advisory file
    /// lock, listed in its own category and excluded from the mutex graph.
    advisory: Option<String>,
    /// `Some(audit)` for a registered [`VocabularyInterior`] site; it keeps a
    /// unique per-site node.
    vocab: Option<&'static str>,
    /// `Some(evidence)` when the receiver is proven a raw pointer (see
    /// [`raw_ptr_read_evidence`]): `core::ptr::read`, listed in its own
    /// category and excluded from the mutex graph.
    raw_ptr: Option<String>,
    /// `Some(namespace)` for a site in a scanned vendored crate: its identity
    /// and UNKNOWN node are namespace-prefixed and its counts are summarized
    /// per crate.
    namespace: Option<&'static str>,
}

impl AcqSite {
    /// Does this site join the mutex order graph? File-advisory and
    /// raw-pointer sites are listed but never graphed.
    fn graphed(&self) -> bool {
        self.advisory.is_none() && self.raw_ptr.is_none()
    }

    /// Graph node name: the identity (namespace-prefixed for vendored sites),
    /// or a unique per-site node for vocabulary-interior and UNKNOWN sites.
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

/// Mask string literals (`"…"`, honoring `\"`) and char literals with spaces,
/// so token scans and brace counting never fire inside literal text.
/// Lifetimes are left intact; raw strings are not special-cased.
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

/// Blank out `#[cfg(test)]`-gated items so the census graphs only the shipped
/// lock discipline. Line count is preserved, so spans stay correct.
pub(crate) fn mask_cfg_test_items(text: &str) -> String {
    mask_gated_items(text, &["#[cfg(test)]"])
}

/// Blank every item whose immediately preceding attribute line (trimmed)
/// exactly matches one of `gates`. Shared with the wasm and scope censuses,
/// each with its own gate list. Exact match is fail-closed: an unrecognized cfg
/// spelling is not masked, so a hazard under it still fails the census.
///
/// An item's end is found by rustfmt's layout, never by counting braces, with
/// literals masked before every token test:
///   - a `;` at the end of a line: a body-less item;
///   - a `,` at the end of a line whose indent equals the gate's: an enum
///     variant, struct field, match arm or struct-literal field;
///   - otherwise the first `{` opens a body, which ends at the first later line
///     that is `<indent>}` (`},`, `});`, `};` count; `} else {` does not).
///
/// Fail-open on an unrecognized shape (the marked lines are restored), since
/// masking is subtractive. Every blanked span is a complete item, which
/// brace counters ([`crate::scope_census`]) rely on. A raw string containing
/// a line that is exactly `<indent>}` can close a body early; readers carrying
/// literal state across lines must blank literals first (as
/// [`crate::lex::blank_non_code`] does).
pub(crate) fn mask_gated_items(text: &str, gates: &[&str]) -> String {
    mask_items_where(text, &|attr| gates.contains(&attr))
}

/// [`mask_gated_items`] with the gate test as a predicate over the trimmed
/// attribute line, for the main-loop census's cfg matcher
/// ([`crate::cfg_attr_never_ships`]), which must see
/// `#[cfg(all(test, any(windows, target_os = "linux")))]` as test-only.
pub(crate) fn mask_items_where(text: &str, is_gate: &dyn Fn(&str) -> bool) -> String {
    let keep = unmasked_lines(text, is_gate);
    let mut out = String::with_capacity(text.len());
    for (line, keep) in text.lines().zip(keep) {
        if keep {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Per line of `text`, whether [`mask_items_where`] keeps it: `false` for
/// every line of a gated item. For a reader that masks two aligned views of
/// one file the same way.
pub(crate) fn unmasked_lines(text: &str, is_gate: &dyn Fn(&str) -> bool) -> Vec<bool> {
    let lines: Vec<&str> = text.lines().collect();
    // A line's code: comments stripped, literals blanked (length-preserving).
    // Every token test below reads this, never the raw line.
    let code_of = |line: &str| mask_literals(strip_line_comment(line));
    let indent_of = |line: &str| line.len() - line.trim_start().len();
    let mut keep = vec![true; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if !is_gate(t) {
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
        // The body may open lines below the attribute (a wrapped signature).
        // Walk forward to the first `{`, taking a `;` or a gate-indent `,`
        // reached first as the end.
        let mut k = j;
        let mut opened = false;
        let mut terminated = false;
        while k < lines.len() {
            let item = code_of(lines[k]);
            // A line dedented below the gate closes the enclosing block, so the
            // item ended on the line before (an unformatted last variant, field
            // or element with no trailing comma).
            if k > j && !item.trim().is_empty() && indent_of(lines[k]) < indent {
                k -= 1;
                terminated = true;
                break;
            }
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
            // No terminator before EOF: restore rather than blank the tail.
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
                    // Unrecognized shape: restore the item's lines, so no `{`
                    // is blanked while its `}` survives.
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
    keep
}

/// A logical statement line: rustfmt chain continuations (lines starting with
/// `.`) joined onto their receiver line, so `self`↵`.store`↵`.read()` scans as
/// `self.store.read()`. Keeps the first physical line number for the span.
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

/// Self types an `impl` may not lend to a `self`-rooted lock identity. An
/// `impl .. for Mutex<T>` (or `Arc<T>`, `Box<T>`, …) is the extension-trait
/// shape where `self` is every caller's lock: a `Mutex::self` node would merge
/// all the process's mutexes and manufacture cycles. Such sites keep a unique
/// per-site node. Adding a name only ever un-merges, so the list is fail-closed.
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

/// One `impl` block's line span and the self type it implements — the name a
/// `self` receiver inside it borrows (`impl Foo`, `impl<T> Foo<T>`,
/// `impl Trait for Foo` => `Foo`).
struct ImplSpan {
    /// 1-based first line (the `impl` keyword).
    start: usize,
    /// 1-based last line (the block's closing brace).
    end: usize,
    /// `None` when the header names no type usable as a lock identity (see
    /// [`impl_self_type`]). The span is still recorded, so an enclosing impl
    /// cannot claim these lines.
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
        // middle of a multi-byte character panics.
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
/// `std::sync::Mutex<T>` => `Mutex`; `&'a mut Foo` => `Foo`. `None` for a
/// generic parameter of the impl, an [`UNQUALIFIABLE_SELF_TYPES`] wrapper, a
/// macro fragment (`$ty`), a structural type, or a trait object.
fn base_type_name(ty: &str, params: &BTreeSet<&str>) -> Option<String> {
    let mut t = ty.trim();
    // `&`, `&'a `, `&mut ` — a reference to the type is still the type. `dyn`
    // is not peeled: a trait object names a trait, not one concrete lock.
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
    // A one-character type name is treated as a generic parameter, even where
    // the parameter list was missed (a macro body, a wrapped header).
    if name.chars().count() == 1
        || params.contains(name)
        || UNQUALIFIABLE_SELF_TYPES.contains(&name)
    {
        return None;
    }
    Some(name.to_string())
}

/// The nameable self type of an `impl` header (text up to the block's `{`):
/// `impl Foo` / `impl<T> Foo<T>` / `impl Trait for Foo` => `Foo`. `None`
/// leaves the block's `self` receivers UNKNOWN (see [`base_type_name`]).
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
    // `impl Trait for SelfTy`: the self type follows the last top-level `for`.
    let self_ty = match top_level_words(rest, "for").last() {
        Some(&at) => &rest[at + "for".len()..],
        None => rest,
    };
    base_type_name(self_ty, &params)
}

/// Every `impl` block in a file, with its line span and self type, ended by
/// the same closing-brace-at-item-indent rule as the fn segmenter. Nested
/// impls are recorded; lookup takes the innermost span.
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
            // An impl whose close cannot be found spans its header only, so no
            // acquisition inherits a guessed type.
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
    /// The self type of the innermost `impl` block covering this line, when it
    /// is one this census may name.
    fn impl_ty(&self) -> Option<&str> {
        self.impls
            .iter()
            .filter(|s| s.start <= self.lineno && self.lineno <= s.end)
            .max_by_key(|s| s.start)
            .and_then(|s| s.ty.as_deref())
    }
}

/// The receiver chain ending at `dot` as a field path below `self`, when rooted
/// there: `self.lock()` => `self`; `self.0.lock()` => `0`;
/// `self.control.1.read()` => `control.1`. `None` for any other root.
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
/// => `proxies` (the accessor-fn idiom for statics).
///
/// Where the plain rule resolves nothing, two refinements apply: a chain rooted
/// at `self` takes the enclosing impl type's name (`self.0.lock()` in
/// `impl Spill` => `Spill::0`), and a single-letter local takes the identity of
/// the path it aliases. These merge per-site UNKNOWN nodes into one named node,
/// which is sound because the merged sites are the same field of the same type.
/// What still cannot be named stays `None` (UNKNOWN).
fn resolve_receiver(line: &str, dot: usize, ctx: &SiteCtx<'_>) -> Option<String> {
    // `self.store?.read()`: the `?` is postfix propagation and the lock is
    // still `store`. Stripped before the refinements below, so
    // `self.store?.lock()` reaches `self_rooted_chain` at the receiver's dot.
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
                // `Ty::self` / `Ty::0` cannot collide with a plain receiver or a
                // vendored `<crate>::<recv>` identity: plain resolution never
                // returns `self` or a tuple index.
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
        // avoiding a heap needle and a `StrSearcher` per helper per line. The
        // match set and order equal `find`'s.
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
    // One `.`-scan for the whole table, sound by the `ACQ_METHODS` scan invariant.
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
/// final value? True iff everything between the call and the end of the
/// statement is a result adapter (`.unwrap()`, `.expect(..)`,
/// `.unwrap_or_else(..)`, `.map_err(..)`) or `?`. Mapping only the error also
/// leaves an Ok guard inside a bound Result held. Anything else (`.by_local(..)`, a `,`, a
/// closing paren) consumes the guard within the statement, so a `let` binds
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
            // The adapter's argument spans lines (`unwrap_or_else(|p| {` …): the
            // guard is bound (erring toward more tracked holds).
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

/// The identity a `let` binding aliases when its RHS is a pure path
/// (`&self.spill`, `self.0`, `&QUEUE`): what that path would resolve to at an
/// acquisition site, so an alias introduces no new identity. Any other RHS
/// yields `None` and the aliased receiver stays UNKNOWN.
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

/// A call the census will follow one hop: `(byte offset in the line, callee)`.
/// Trusted shapes are free-fn/path `name(` and `self.name(`.
type TrustedCall = (usize, String);

/// A call through a `self.field.…` receiver: `(byte offset, field, callee)`.
/// Whether it is followed is decided later, against [`FieldTypes`].
type FieldRecvCall = (usize, String, String);

/// Type and field declarations harvested from the workspace crates: the
/// evidence deciding whether a `self.field.name(..)` hop can be followed.
///
/// Name-keyed, so it over-approximates (same-named fields merge), erring toward
/// following a hop. Vendored sources are not harvested, so this describes only
/// [`WORKSPACE_NS`] types (see [`FieldTypes::is_corpus_typed`]).
#[derive(Default)]
struct FieldTypes {
    /// Every type name the corpus defines (`struct`/`enum`/`union`/`type` and
    /// `trait`, which a `dyn`/`impl` field resolves through).
    defined: BTreeSet<String>,
    /// Field name -> every type identifier in its declared type
    /// (`Arc<Shared>` -> {Arc, Shared}).
    field_tys: BTreeMap<String, BTreeSet<String>>,
}

impl FieldTypes {
    /// Is `field`'s declared type defined in this corpus, asked from namespace
    /// `at`? Unknown fields and call sites outside [`WORKSPACE_NS`] answer
    /// `false`: the harvest read only the workspace crates.
    fn is_corpus_typed(&self, at: Option<&'static str>, field: &str) -> bool {
        at == WORKSPACE_NS
            && self
                .field_tys
                .get(field)
                .is_some_and(|tys| tys.iter().any(|t| self.defined.contains(t)))
    }

    /// Harvest declarations from one already-cfg-masked source file.
    fn harvest(&mut self, text: &str) {
        // Depth of the struct/enum body we are inside; fields are collected
        // only at depth 1, so fn bodies and `let x: T` never count.
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
            // Fields live one level inside the declaration's brace, so the depth
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

/// Simple parameter bindings shadow bare fn names even when their type is a
/// generic, an `impl FnOnce`, or a fn pointer. Patterns this lexical walk cannot
/// name remain under the existing by-name approximation.
fn parameter_bindings(sig: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some((_, rest)) = sig.split_once("fn ") else {
        return names;
    };
    let Some(name) = ident_prefix(rest) else {
        return names;
    };
    let mut rest = rest[name.len()..].trim_start();
    if rest.starts_with('<') {
        let Some(close) = matching_angle(rest) else {
            return names;
        };
        rest = rest[close + 1..].trim_start();
    }
    if !rest.starts_with('(') {
        return names;
    }
    let Some(close) = match_paren(rest, 0) else {
        return names;
    };
    for param in split_top_level(&rest[1..close]) {
        let Some((binding, _)) = param.split_once(':') else {
            continue;
        };
        let binding = binding.trim();
        let binding = binding.strip_prefix("mut ").unwrap_or(binding).trim();
        if binding != "self" && ident_prefix(binding) == Some(binding) {
            names.insert(binding.to_string());
        }
    }
    names
}

/// One-hop call targets on `line`: `(trusted, field_receiver)`, the latter as
/// `(field, callee)`. Callees are looked up by name, so only `name(..)` and
/// `self.name(..)` are trusted. `self.field.name(..)` dispatches on the field's
/// type: the census follows it only when [`FieldTypes`] finds that type in the
/// corpus and one same-namespace fn bears the name, and otherwise counts it
/// (a foreign `Condvar` or `HashMap`). Following it blindly can bind a
/// guard-releasing `Condvar::wait` to an unrelated `wait`; dropping it would
/// miss real hops like `self.shared.spill_is_empty()`.
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
            // Guard helpers and standard acquisition methods are direct
            // acquisitions; treating them as callees would double-count.
            continue;
        }
        let start = idx - name.len();
        if line[..start].trim_end().ends_with("fn") {
            continue; // a definition, not a call
        }
        if start > 0 && bytes[start - 1] == b'.' {
            // Method call: only a bare `self.name(` names a method of the
            // enclosing type. Field chains are counted (see the doc comment);
            // other receivers are not followed.
            let Some(recv) = ident_ending_at(line, start - 1) else {
                continue;
            };
            if recv != "self" {
                // A `self.…field.name(` chain resolves through the declared
                // type of `recv`, the last field of the chain.
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
    /// The identity namespace of the crate defining this fn ([`WORKSPACE_NS`]
    /// for aterm crates). One-hop lookup is by name across both sides, so the
    /// namespace is half of a callee's identity.
    namespace: Option<&'static str>,
    /// Does the signature return a `…Guard`? With a direct acquisition, this
    /// is the helper shape that must be registered in [`GUARD_HELPERS`].
    returns_guard: bool,
    /// Is the fn method-shaped (first parameter `self`)? Only a method may use
    /// the standard-name exemption from [`GUARD_HELPERS`]: callers of a free
    /// `fn lock(m: &Mutex<T>)` spell `lock(&X)`, with no token.
    takes_self: bool,
    /// Indices into the global site table.
    acq: Vec<usize>,
    /// Intra-fn held-acquire pairs: (holder site, acquired site).
    edges: Vec<(usize, usize)>,
    /// Calls made while holding: (holder site, callee name, call `file:line`).
    held_calls: Vec<(usize, String, String)>,
    /// Calls made while holding through a field receiver (`self.field.name(..)`).
    /// Followed only once the field's type is resolved against the corpus — see
    /// [`held_call_targets`] and [`FieldTypes`].
    /// `(holder site, field name, callee name, call `file:line`)`.
    field_recv_calls: Vec<(usize, String, String, String)>,
    /// Bare calls to a parameter/local are callbacks whose target this walk
    /// cannot resolve, not calls to an unrelated same-named corpus fn.
    binding_calls: Vec<(String, String)>,
}

/// Lexical guard-return detection over the signature region (def line to the
/// line opening the body): a `->` whose return type has an identifier ending in
/// `Guard`, so `-> GuardedInput` stays out and `Option<MutexGuard<'_, T>>` is
/// caught. A non-guard registered as one would claim callers hold a lock they
/// do not, which can mask a real cycle.
fn returns_guard(body: &[String]) -> bool {
    for line in body {
        if let Some(p) = line.rfind("->")
            && return_type_names_a_guard(&line[p + 2..])
        {
            return true;
        }
        if line.contains('{') {
            break;
        }
    }
    false
}

/// Does any Rust identifier in `ret` end with `Guard`? Splits on everything
/// that cannot appear inside an identifier, so paths, generic arguments,
/// references and lifetimes are all traversed.
fn return_type_names_a_guard(ret: &str) -> bool {
    ret.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|ident| ident.ends_with("Guard"))
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
    // The parameter list opens at the first `(` after `fn `: a restricted
    // visibility (`pub(crate) fn lock(&self)`) opens one of its own first.
    let Some(open) = sig
        .find("fn ")
        .and_then(|at| sig[at..].find('(').map(|p| at + p))
    else {
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
    /// `let … = <acquire> else { … }`: the guard binds only after the else
    /// block (inside it the pattern did not match — nothing is held). The
    /// guard stays inactive (no edges, no held calls) until the line depth
    /// returns to `binding_depth`.
    active: bool,
}

/// A simple local name shadows bare fn lookup only after its initializer
/// statement. Keep each nested binding so closing an inner block restores an
/// outer local, rather than incorrectly restoring a same-named corpus fn.
struct LocalBinding {
    name: String,
    binding_depth: i32,
    active: bool,
}

/// The file-level lexical facts a body scan needs beyond the body's own text.
/// File-scoped, not body-scoped, because every one of them is declared outside
/// the fn that uses it.
struct FileCtx<'a> {
    /// Repo-relative path, for the site spans.
    rel: &'a str,
    /// `Some(prefix)` in vendored-identity mode: every resolved identity is
    /// prefixed `prefix::…` so foreign receiver names can never merge across
    /// the namespace boundary.
    namespace: Option<&'static str>,
    /// The file's `impl` blocks: the header giving a `self` receiver its
    /// identity sits above the fn.
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
        binding_calls: Vec::new(),
    };
    let mut live: Vec<LiveGuard> = Vec::new();
    // Locals proven to hold a std::fs::File: name -> (binding depth, span). A
    // rebind to anything else or the block closing removes the evidence.
    let mut file_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    // Locals lexically proven to hold a raw pointer (same ledger discipline):
    // a zero-arg `.read()` on one of these is `core::ptr::read`, not RwLock.
    let mut ptr_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    // Single-letter locals aliasing a nameable path (`let m = &self.spill;`):
    // name -> (binding depth, identity; see [`alias_target`]). A non-path
    // rebind or the block closing drops the alias.
    let mut alias_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    let mut local_bindings: Vec<LocalBinding> = Vec::new();
    // The signature region (def line through the body-opening line), for the
    // raw-pointer parameter evidence (`input_ptr: &mut *const u8`).
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
    let parameters = parameter_bindings(&sig_text);
    let mut depth: i32 = 0;
    for ll in &lines {
        let line = &ll.text;
        let opens = i32::try_from(line.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(line.matches('}').count()).unwrap_or(0);
        let depth_end = depth + opens - closes;
        // 1. `drop(var)` ends a guard explicitly. `live` is empty on most lines,
        //    so test it first, then the constant `drop(` needle.
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
        // 3. Acquisitions on this line, left to right. Each blocking one is an
        //    edge target for every live guard and every earlier acquisition in
        //    the statement. A file-advisory acquisition joins no edges.
        //    The ctx borrows `alias_vars` only until the call below.
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
                // A vocabulary interior keeps its unique per-site node: `self`
                // there is every caller's lock. (Its impl self type is an
                // UNQUALIFIABLE_SELF_TYPES wrapper anyway; the registry checks
                // depend on this.)
                None
            } else {
                // Vendored-identity mode: the identity lives in the crate's
                // namespace (categorized sites keep their local name).
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
                let before = &line[..pos - callee.len()];
                if !before.trim_end().ends_with(['.', ':']) {
                    let binding = if local_bindings.iter().any(|b| b.active && b.name == callee) {
                        Some("local")
                    } else if parameters.contains(&callee) {
                        Some("parameter")
                    } else {
                        None
                    };
                    if let Some(binding) = binding {
                        facts
                            .binding_calls
                            .push((format!("{binding} `{callee}`"), call_span));
                        continue;
                    }
                }
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
        //    match-like scrutinee guard. A let-else guard (`… else {` opens a
        //    divergent block in which the pattern did not match) binds
        //    inactive and activates only when the else block closes.
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
        // 5b. Evidence ledgers: a File or raw-pointer construction marks the
        //     var; a rebind without that evidence removes it.
        if let LineKind::Let(Some(var)) = &kind {
            local_bindings.push(LocalBinding {
                name: var.clone(),
                binding_depth: depth,
                active: false,
            });
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
            // 5c. The alias ledger, single-letter bindings only: a longer name
            //     is already an identity, and re-pointing it would merge nodes.
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
        local_bindings.retain(|binding| depth >= binding.binding_depth);
        // The new name is absent from its own initializer, including a
        // multiline/block initializer and a let-else's divergent else body.
        if line.ends_with(';') {
            for binding in &mut local_bindings {
                if depth == binding.binding_depth {
                    binding.active = true;
                }
            }
        }
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

/// Append the why + repair block for a lock-order cycle. `invert` names the
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

/// The lock-order census's verdict title.
const LOCKS: &str = "lock-order census";

/// Run the lock-order census over the checkout at `root` (the directory
/// holding `crates/`). A pure function of the source tree.
pub fn run_lock_order_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== lock-order census (L0 deadlock, lock-graph sense) ===\n\
         \x20   root: {}",
        root.display()
    );

    // Derive the scan set from the workspace manifests. An unclassifiable
    // dependency graph stops the census rather than shrinking the domain.
    let scan = match crate::scan_set::derive_gui_scan_set(root) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-7] SCAN-SET DERIVATION FAILED — the census cannot \
                 soundly determine the aterm-gui process closure from the workspace \
                 manifests, so it refuses to scan a guessed set (fail-closed).\n\
                 \x20       {e}"
            );
            return CensusOutcome::red(LOCKS, log, "1 obligation violation(s).");
        }
    };
    log.push_str(&crate::scan_set::render_scan_set(&scan));

    // ---- Parse every fn in the scanned crates. ----
    let mut sites: Vec<AcqSite> = Vec::new();
    let mut fns: Vec<FnLockFacts> = Vec::new();
    // Field-type evidence for the field-receiver hop, from the workspace files
    // only: pooling vendored declarations would let one crate's struct answer
    // for another's fields.
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

    // Parse the scanned vendored crates (vendored-identity mode). Files under a
    // registered per-platform slice do not compile into the shipped macOS GUI
    // process: their sites are counted under the slice label, never graphed.
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

    // A census that finds no acquisition sites walked nothing (parser rot or
    // wrong root) and must not pass as acyclic.
    if sites.is_empty() {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] ZERO lock-acquisition sites found under {} — the census \
             walked nothing (parser broke, or this root is not an aterm checkout?).",
            scan.scan_dirs.join(", ")
        );
        return CensusOutcome::red(LOCKS, log, "1 obligation violation(s).");
    }

    // Held calls through a `self.field.…` receiver whose field type is foreign:
    // counted and listed instead of guessed at.
    let mut unresolved_field_calls: Vec<(String, String)> = Vec::new();
    // Per fn name, how many fns bear it in each namespace. A field hop is
    // followed only when exactly one corpus fn has the name and it is in the
    // call site's own namespace: a second candidate makes the lookup a guess,
    // and a candidate across a vendored boundary is not a method of an aterm
    // type.
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
    let binding_calls: BTreeSet<_> = fns
        .iter()
        .flat_map(|f| f.binding_calls.iter().cloned())
        .collect();

    // One-hop callee lookup: fn name -> definitions, grouped by the defining
    // crate's namespace. Vendored identities are namespaced but fn names are
    // not, so a bare name could otherwise resolve into a vendored fork and
    // mint an unrepairable cross-boundary edge. Per call site:
    //   * the caller's own namespace defines the name => only those
    //     definitions are candidates (Rust agrees: a local definition wins);
    //   * it does not => the call is to something imported, possibly the
    //     vendored fork, and the hop is followed.
    let mut by_name: BTreeMap<&str, BTreeMap<Option<&'static str>, Vec<usize>>> = BTreeMap::new();
    for (idx, f) in fns.iter().enumerate() {
        by_name
            .entry(&f.name)
            .or_default()
            .entry(f.namespace)
            .or_default()
            .push(idx);
    }

    // Guard-helper registry, fail-closed both ways. Forward: every entry is
    // still defined at its file and still acquires the declared identity.
    // Reverse: every acquire-and-return-a-Guard fn is registered or is a
    // method with a standard name.
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
        // Only workspace definitions satisfy the interior check: a same-named
        // fn in a vendored fork is a different fn.
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
    // Vocabulary-interior registry, fail-closed both ways (see the struct
    // doc): a standard method name, still defined at its file, still
    // containing the bare-`self` acquisition.
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
            // The registry exemption is the workspace's: a vendored fn that
            // merely shares a GUARD_HELPERS name is a different helper and
            // still hides its callers' holds.
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
        return CensusOutcome::red(
            LOCKS,
            log,
            format!(
                "{failures} obligation violation(s) (registry/coverage stage; the graph stage \
                 did not run because its ground truth is broken)."
            ),
        );
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
            // Definitions in the caller's own namespace shadow foreign ones
            // (see `by_name`).
            let shadowed = per_ns.contains_key(&f.namespace);
            // One hop: every direct blocking acquisition in every same-named
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

    // Site and identity bookkeeping. Advisory (file lock) and raw-pointer
    // sites are their own categories: not identities, not UNKNOWN, not
    // graphed, always counted and listed.
    let advisory_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.advisory.is_some()).collect();
    let raw_ptr_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.raw_ptr.is_some()).collect();
    let vocab_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.vocab.is_some()).collect();
    let blocking = sites.iter().filter(|s| s.blocking && s.graphed()).count();
    let tries = sites.iter().filter(|s| !s.blocking && s.graphed()).count();
    // UNKNOWNs by origin: aterm sites are listed (each is a naming task);
    // vendored sites are summarized per crate, since upstream receivers
    // cannot be renamed here.
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
             \x20        first guard ends before the next `{id}` is taken. If `{id}` is not a lock at \
             all — a method merely NAMED read/write/lock/try_* — RENAME the method: this \
             census identifies a lock by its name and excludes nothing (no registry, no \
             waiver: every exclusion tried under-reported a real deadlock), so a screen \
             `read` held across another `read` is indistinguishable from a re-entrant \
             lock."
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
        return CensusOutcome::red(
            LOCKS,
            log,
            format!(
                "{failures} obligation violation(s). A lock-order cycle has NO waiver channel \
                 (L0-DEADLOCK: none, ever) — it can only be fixed."
            ),
        );
    }

    // ---- GREEN: the summary, then the honesty ledger behind it. ----
    let term_sites = identities.get("term").copied().unwrap_or(0);
    let summary = format!(
        "{} acquisition site(s) across {} workspace crate(s) + {} \
         vendored crate(s) ({} blocking, {} try_*, {} OS file-advisory, {} raw-pointer \
         ptr::read); {} resolved identities; {} UNKNOWN-identity site(s) + {} vendored \
         UNKNOWN site(s); {} audited vocabulary-interior site(s); {} held-acquire \
         pair(s) ({} intra-fn, {} via one-hop calls; {} distinct ordered identity \
         pairs); {} held call(s) through a `self.field.…` receiver NOT followed; \
         {} held call(s) through a fn parameter/local binding NOT followed; \
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
        binding_calls.len(),
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
    // Listed like UNKNOWN, so a reader can see which hops were not followed.
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
    if !binding_calls.is_empty() {
        let _ = writeln!(
            log,
            "    held calls through fn parameters/local bindings NOT followed — \
             the binding shadows a corpus fn name, but its callback target is not resolved:"
        );
        for (binding, span) in binding_calls.iter().take(20) {
            let _ = writeln!(log, "      {span}  {binding}(…)");
        }
        if binding_calls.len() > 20 {
            let _ = writeln!(log, "      … and {} more.", binding_calls.len() - 20);
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
    let floors = vec![
        Floor::at_least(
            "`term` lock acquisition sites",
            term_sites,
            100,
            "the walk lost most of the GUI's lock sites (a renamed accessor or a parser \
             regression), so the graph is acyclic vacuously",
        ),
        Floor::none(
            "UNKNOWN-identity lock sites in aterm crates",
            unknown.len(),
            "an UNKNOWN is a node that can never close a cycle, so a deadlock through it is \
             invisible; name each receiver listed above, never suppress the site",
        ),
        Floor::none(
            "UNKNOWN-identity lock sites in vendored crates",
            vendored_unknown.len(),
            "a vendored fork gained an acquisition the census cannot name; give its \
             receiver an identity (the per-crate list above has the spans)",
        ),
    ];
    CensusOutcome::green(LOCKS, log, summary).with_floors(floors)
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
    use crate::scan_set::test_fixtures::synth_tree;

    /// Crates a synthetic tree must contain beyond the base fixture (aterm-gui
    /// and aterm-types) because a `GUARD_HELPERS` entry is registered in them:
    /// the registry's interior check reads the scanned corpus. Every synthetic
    /// tree composing its own manifest set must splice these in.
    const SYNTH_HELPER_CRATES: &[(&str, &str)] = &[("aterm-log", ""), ("atpkg", "")];

    /// Every registered helper and vocabulary interior at its `def_file`, its
    /// acquisition spelled as the shipping one is, so the registry checks hold.
    fn synth_helper_files() -> Vec<(String, String)> {
        let mut files = crate::scan_set::test_fixtures::workspace_manifests(SYNTH_HELPER_CRATES);
        files.push((
            // term_lock's registered def_file; must match GUARD_HELPERS above.
            "crates/aterm-gui/src/lib.rs".to_string(),
            // `term_lock_ui` shares the file and the `term` identity: its interior
            // acquires through `term_lock(term)`, exactly as the shipping helper does.
            "pub(crate) fn term_lock(term: &Mutex<Terminal>) -> TermGuard<'_> {\n    \
             term.lock().unwrap()\n}\n\
             pub(crate) fn term_lock_ui(term: &Mutex<Terminal>, w: &AtomicU32) -> TermGuard<'_> {\n    \
             term_lock(term)\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/consent_retire.rs".to_string(),
            "impl RetireState {\n    \
             fn retire_slot(&self) -> std::sync::MutexGuard<'_, Slot> {\n        \
             self.shared.lock().unwrap_or_else(|p| p.into_inner())\n    \
             }\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/tray_raster.rs".to_string(),
            "fn lock_fonts() -> std::sync::MutexGuard<'static, ChromeFonts> {\n    \
             chrome_fonts().lock().unwrap()\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/keeper_link.rs".to_string(),
            "fn keeper_state() -> std::sync::MutexGuard<'static, State> {\n    \
             KEEPER_STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/fabric_launch.rs".to_string(),
            "fn supervisor() -> std::sync::MutexGuard<'static, Supervisor> {\n    \
             SUPERVISOR.lock().unwrap_or_else(|p| p.into_inner())\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/control_query.rs".to_string(),
            "fn search_cache_lock() -> MutexGuard<'static, VecDeque<SearchSnapshot>> {\n    \
             SEARCH_SNAPSHOTS.lock().unwrap()\n}\n\
             fn narrow_sessions_lock() -> MutexGuard<'static, VecDeque<NarrowSession>> {\n    \
             NARROW_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/operator_host.rs".to_string(),
            "fn accepting_guard(&self) -> MutexGuard<'_, HostActuationGate> {\n    \
             self.shared.fleet_fault.lock().unwrap()\n}\n\
             fn mutation_guard(&self) -> MutexGuard<'_, HostActuationGate> {\n    \
             self.shared.fleet_fault.lock().unwrap()\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-log/src/lib.rs".to_string(),
            "fn env_lock() -> MutexGuard<'static, ()> {\n    \
             ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/connections.rs".to_string(),
            "impl ConnectionTable {\n    \
             pub(crate) fn records(&self) -> MutexGuard<'_, Records> {\n        \
             self.records.lock().unwrap_or_else(|p| p.into_inner())\n    }\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/atpkg/src/meter.rs".to_string(),
            "fn screen_lock() -> std::sync::MutexGuard<'static, Screen> {\n    \
             SCREEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)\n}\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-gui/src/askpass.rs".to_string(),
            "impl PromptInputGate {\n    \
             pub(crate) fn posting_permit(&self) -> Result<std::sync::MutexGuard<'_, ()>, ()> {\n        \
             let posting = self.posting.lock().unwrap_or_else(|e| e.into_inner());\n        \
             if self.is_open() { Err(()) } else { Ok(posting) }\n    \
             }\n}\n"
                .to_string(),
        ));
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

    /// Every GUARD_HELPERS / VOCABULARY_INTERIORS entry is defined in
    /// [`synth_helper_files`] at its `def_file`, in a crate the synthetic scan
    /// set reaches. Otherwise every synthetic tree fails on the real registry
    /// and the whole self-test suite goes red for an unrelated reason.
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
        let root = synth_tree("aterm-lock-census", name, &files);
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
        let root = synth_tree("aterm-lock-census", name, &files);
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
        // The `?` strip must run before this refinement: at the raw dot the
        // token before it is `?`, and the field would fall back to UNKNOWN.
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
        // A named field keeps its plain, unqualified identity.
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
            // The shapes that must never lend an identity — each one would
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

    /// Item-end shapes the masker must get right: literal braces, brace-less
    /// items ending at a comma, wrapped signatures, and fail-open on an
    /// unrecognized close.
    ///
    /// Fixtures are slices of lines because the assertions are about
    /// indentation, which an escaped literal hides.
    #[test]
    fn gated_items_end_at_their_own_shape_not_at_a_brace_count() {
        fn src(lines: &[&str]) -> String {
            let mut s = lines.join("\n");
            s.push('\n');
            s
        }
        let gates: &[&str] = &["#[cfg(test)]"];

        // (1) A `'{'` char literal must not desynchronize the walker and blank
        // the rest of the file.
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

        // (2) An enum variant ends at its comma, not at the next brace.
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

        // (3) A match arm, same rule, one nesting level deeper.
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

        // (4) The comma rule must not end a wrapped signature: `a: u32,` is
        // indented deeper than the gate, so it is a parameter.
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

        // (5) A gated statement with a brace in a format string: the walk for
        // the body's `{` must read literal-masked text.
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

        // (6) Fail-open: a body whose close is not at the gate's indent is not
        // masked at all, and the result stays brace-balanced.
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

        // (7) A last variant with no trailing comma ends at the enclosing
        // block's close, not at the next item's `;` or `{`.
        let m = mask_gated_items(
            &src(&[
                "enum Ev {",
                "    A,",
                "    #[cfg(test)]",
                "    B",
                "}",
                "fn shipped() {",
                "    alpha.lock();",
                "}",
            ]),
            gates,
        );
        assert!(!m.contains('B'), "the gated variant must go:\n{m}");
        assert!(
            m.contains("fn shipped") && m.contains("alpha.lock()"),
            "the item after the enum must survive:\n{m}"
        );
        assert_eq!(
            m.matches('{').count(),
            m.matches('}').count(),
            "the enum's close must survive:\n{m}"
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
        // Every edge of the cycle, both sites each.
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
        // f holds alpha and calls helper_b (locks beta); g holds beta and calls
        // helper_a (locks alpha): a cycle visible only through the one-hop pass.
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
    fn callable_parameters_shadow_only_bare_one_hop_targets() {
        let prefix = "fn post() {\n    posting.lock().unwrap();\n}\n";
        for (label, signature, call) in [
            ("impl", "fn with_post(post: impl FnOnce() -> ())", "post()"),
            (
                "generic",
                "fn with_post<F: FnOnce(u8, u8) -> ()>(mut post: F)",
                "post(1, 2)",
            ),
            (
                "pointer",
                "fn with_post(post: fn(u8, u8) -> ())",
                "post(1, 2)",
            ),
        ] {
            let out = run_synth(
                label,
                &format!(
                    "{prefix}{signature} {{\n    let g = posting.lock().unwrap();\n    {call};\n}}\n"
                ),
            );
            assert!(
                out.ok,
                "the parameter shadows the unrelated fn, not its locks:\n{}",
                out.log
            );
            assert!(
                out.log.contains("parameter `post`"),
                "an unresolved callback must remain visible in the census:\n{}",
                out.log
            );
        }
        for (label, signature, call) in [
            ("free", "fn with_post()", "post()"),
            (
                "qualified",
                "fn with_post(post: impl FnOnce())",
                "crate::post()",
            ),
            (
                "qualified-space",
                "fn with_post(post: impl FnOnce())",
                "crate :: post()",
            ),
            (
                "method",
                "fn with_post(&self, post: impl FnOnce())",
                "self.post()",
            ),
        ] {
            let out = run_synth(
                label,
                &format!(
                    "{prefix}{signature} {{\n    let g = posting.lock().unwrap();\n    {call};\n}}\n"
                ),
            );
            assert!(
                !out.ok && out.log.contains("RE-ENTRANCY SUSPECT"),
                "an actual fn or method call still acquires the held lock:\n{}",
                out.log
            );
        }
    }

    #[test]
    fn error_mapping_preserves_a_guard_until_consumed_or_dropped() {
        let reverse = "fn reverse() {\n    let g = beta_lock.lock().unwrap();\n    \
                       let h = alpha_lock.lock().unwrap();\n}\n";
        for (label, tail, release, cycle) in [
            ("mapped-question", "?", "", true),
            ("mapped-result", "", "", true),
            ("mapped-consumed", ".is_ok()", "", false),
            ("mapped-dropped", "?", "drop(g);", false),
        ] {
            let out = run_synth(
                label,
                &format!(
                    "fn forward() -> Result<(), ()> {{\n    \
                     let g = alpha_lock.lock().map_err(|_| ()){tail};\n    \
                     {release}\n    let h = beta_lock.lock().unwrap();\n    \
                     Ok(())\n}}\n{reverse}"
                ),
            );
            assert_eq!(
                !out.ok, cycle,
                "mapping an error leaves the Ok guard held; consuming or dropping \
                 the result releases it:\n{}",
                out.log
            );
            if cycle {
                assert!(
                    out.log.contains("EDGE alpha_lock -> beta_lock")
                        && out.log.contains("EDGE beta_lock -> alpha_lock"),
                    "both real orders must form the cycle:\n{}",
                    out.log
                );
            }
        }
    }

    #[test]
    fn local_bindings_shadow_only_live_bare_one_hop_targets() {
        for (label, body) in [
            (
                "local-pointer",
                "unsafe {\n        let post: unsafe extern \"C-unwind\" fn() = msg();\n        post();\n    }",
            ),
            ("local-closure", "let post = || ();\n    post();"),
            (
                "local-nested",
                "let post = || ();\n    {\n        let post = || ();\n        post();\n    }\n    post();",
            ),
            (
                "local-multiline",
                "let post = {\n        || ()\n    };\n    post();",
            ),
        ] {
            let out = run_synth(
                label,
                &format!(
                    "fn post() {{\n    posting.lock().unwrap();\n}}\n\
                     fn caller() {{\n    let g = posting.lock().unwrap();\n    {body}\n}}\n"
                ),
            );
            assert!(
                out.ok,
                "a local callback is not the corpus fn:\n{}",
                out.log
            );
            assert!(
                out.log.contains("local `post`"),
                "the unresolved local target must remain reported:\n{}",
                out.log
            );
        }
    }

    #[test]
    fn local_binding_initializers_and_out_of_scope_calls_keep_real_edges() {
        for (label, body) in [
            ("local-rhs", "let post = post();"),
            ("local-wrapped-rhs", "let post =\n        post();"),
            (
                "local-block-rhs",
                "let post = {\n        post();\n        || ()\n    };",
            ),
            (
                "local-else",
                "let Some(post) = callback else {\n        post();\n        return;\n    };\n    post();",
            ),
            (
                "local-expired",
                "{\n        let post = || ();\n        post();\n    }\n    post();",
            ),
            ("local-qualified", "let post = || ();\n    crate :: post();"),
            ("local-method", "let post = || ();\n    self.post();"),
        ] {
            let out = run_synth(
                label,
                &format!(
                    "fn post() {{\n    posting.lock().unwrap();\n}}\n\
                     fn caller(&self) {{\n    let g = posting.lock().unwrap();\n    {body}\n}}\n"
                ),
            );
            assert!(
                !out.ok && out.log.contains("RE-ENTRANCY SUSPECT"),
                "this call resolves to the actual fn or method:\n{}",
                out.log
            );
        }
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
        // Both halves of this ABBA are spelled `self.0.lock()`. Qualified by
        // the enclosing impl type the nodes are `Alpha::0` and `Beta::0`, and
        // the cycle closes; per-site UNKNOWN nodes could never detect it.
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
        // The qualification must discriminate: `Alpha::0` and `Beta::0` are
        // different locks, and merging them into one `0` node would invent a
        // cycle this tree does not have.
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
        // A blanket `impl<T> .. for T` and an extension trait on the lock
        // primitive implement whole families of types: `T::self` /
        // `Mutex::self` would merge every lock. Both stay UNKNOWN.
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
        // The registered `lock_or_recover` interior (present in every synth
        // tree) lands in the audited vocabulary-interior category, never
        // UNKNOWN, with its audit note and a unique per-site node.
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
        let root = synth_tree("aterm-lock-census", "stalevocab", &files);
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
        let root = synth_tree("aterm-lock-census", "driftvocab", &files);
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
        // `f` is proven a std::fs::File (OpenOptions binding), so `f.lock()` is
        // the advisory lock with its evidence printed; `m.lock()` has no such
        // evidence and stays UNKNOWN.
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
        // A binding ascribed `: std::fs::File` is File evidence even when the
        // constructor is behind a helper. A generic mentioning the type
        // (`Option<std::fs::File>`) is not, and stays UNKNOWN.
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
        // mutex->mutex order survives, but no edge may touch the advisory
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
        // (1) `f` rebound to a non-File: the later f.lock() must not inherit
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

    /// A guard-returning helper is one whose return type names a guard
    /// identifier, not one containing the letters: `-> GuardedInput` is not
    /// flagged, while a guard wrapped in anything still is.
    #[test]
    fn a_return_type_must_name_a_guard_not_merely_contain_the_letters() {
        // A substring test (`contains("Guard")`) matches this name; the
        // identifier test must not.
        assert!(
            "GuardedInput".contains("Guard"),
            "the substring the old check matched on"
        );
        assert!(
            !super::return_type_names_a_guard("GuardedInput"),
            "and which this check must refuse"
        );
        for ret in [
            "std::sync::MutexGuard<'static, Supervisor>",
            "MutexGuard<'_, T>",
            "Option<MutexGuard<'_, T>>",
            "io::Result<RwLockReadGuard<'_, T>>",
            "TermGuard<'_>",
            "RwLockWriteGuard<'a, Store>",
            "MappedMutexGuard<'_, T>",
        ] {
            assert!(
                super::return_type_names_a_guard(ret),
                "a real guard must still be detected: {ret}"
            );
        }
        for ret in [
            "GuardedInput",
            "Guarded<T>",
            "Vec<GuardedInput>",
            "Result<GuardedInput, Error>",
            "GuardRail",
            "bool",
        ] {
            assert!(
                !super::return_type_names_a_guard(ret),
                "a type that is not a guard must not be flagged: {ret}"
            );
        }
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
        // try-source edge exists; try-target edge must not close the cycle.
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
    fn synthetic_raw_ptr_reads_are_categorized_not_unknown() {
        // All three evidence shapes: (1) the receiver is the result of a
        // raw-pointer-producing method; (2) a live `let` binding constructing a
        // raw pointer; (3) a `*const`/`*mut` fn param. Without evidence,
        // `opaque.read()` stays a resolved RwLock identity.
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
        // A free fn named `lock` that returns a guard: its call sites
        // (`lock(&X)`) carry no `.lock()` token, so the standard-name
        // exemption does not apply. A method named `lock` stays exempt.
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
        // The same method behind a restricted visibility: `pub(crate)` opens a
        // parenthesis before the parameter list does, and must not hide `self`.
        let out = run_synth(
            "cratelock",
            "impl Wrapper {\n    pub(crate) fn lock(&self) -> MutexGuard<'_, u32> {\n        \
             self.raw.lock().unwrap()\n    }\n}\n",
        );
        assert!(
            out.ok,
            "a `pub(crate)` METHOD named `lock` is token-captured too:\n{}",
            out.log
        );
    }

    // ------------------------------------------------------------------
    // Vendored-identity mode.
    // ------------------------------------------------------------------

    #[test]
    fn synthetic_vendored_identities_are_namespaced_and_never_merge() {
        // aterm nests state -> queue; the vendored winit stub nests
        // queue -> state. The per-crate namespace keeps these four distinct
        // identities, so the graph stays acyclic, and both namespaced
        // identities appear in the ledger.
        let out = run_synth_files(
            "vendoredns",
            "fn aterm_order() {\n    let g = state.lock().unwrap();\n    \
             let h = queue.lock().unwrap();\n}\n",
            &[(
                "crates/aterm-window/src/planted.rs",
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
                "crates/aterm-window/src/planted.rs",
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
        // Vendored fn names share the by-name callee table with aterm's, so
        // each side must resolve to its own definition: an aterm hold that
        // inherited a vendored lock identity could close a cycle nothing could
        // repair. The complementary case (an unshadowed name resolves across
        // the boundary) is `synthetic_cross_boundary_one_hop_edge_is_graphed`.
        let out = run_synth_files(
            "nsshadow",
            "fn start() {\n    aterm_trace.lock().unwrap().push(1);\n}\n\
             fn aterm_hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
             start();\n}\n",
            &[(
                "vendor/indexmap/src/planted.rs",
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
            !out.log.contains("alpha_lock -> indexmap::writer"),
            "an aterm hold must NOT inherit a vendored lock identity through a \
             shared fn name:\n{}",
            out.log
        );
        assert!(
            out.log.contains("indexmap::stream -> indexmap::writer"),
            "the vendored call site must resolve to the VENDORED `start`:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("indexmap::stream -> aterm_trace"),
            "and the boundary holds in both directions — a vendored hold must \
             not reach an aterm fn of the same name:\n{}",
            out.log
        );
    }

    #[test]
    fn a_question_mark_receiver_still_names_its_lock() {
        // `self.store?.read()` resolves to `store`, not UNKNOWN.
        //
        // A `bare_ctx` (no impl, no aliases) so the refinements cannot fire and
        // the UNKNOWN assertions test the `?` rule alone; `self.0?` under an
        // impl has its own test.
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
        // The genuinely unresolvable receivers stay UNKNOWN: the `?` rule
        // invents no names.
        let acqs = acquisitions_on("*self.0?.lock().unwrap() = None;", &ctx);
        assert!(acqs[0].identity.is_none());
        let acqs = acquisitions_on("a?.read().unwrap().clone()", &ctx);
        assert!(acqs[0].identity.is_none());
    }

    #[test]
    fn a_field_hop_is_followed_when_the_field_type_and_the_callee_both_resolve() {
        // The true positive: `self.shared.…` with `shared: Arc<Shared>`, a
        // corpus struct, calling a callee only one corpus fn bears. The hop is
        // followed and its edge graphed.
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
        // aterm defines its own `Condvar` newtype, so the field type resolves,
        // but two corpus fns are named `wait`: the lookup would be a guess, so
        // the hop is not followed and no phantom cycle forms.
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
        // A unique callee name is not enough if the field's type is foreign:
        // `open: AtomicBool` is std, so `.load(..)` is std's.
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
        // One lock on each side of the namespace boundary, taken in opposite
        // orders (winit calling back into aterm, aterm calling into winit while
        // holding): RED, naming both sides' sites.
        let out = run_synth_files(
            "xboundabba",
            "fn hold_and_call() {\n    let g = alpha_lock.lock().unwrap();\n    \
             winit_side_helper();\n}\n\
             fn aterm_side_helper() {\n    alpha_lock.lock().unwrap().push(1);\n}\n",
            &[(
                "crates/aterm-window/src/planted.rs",
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
            out.log.contains("crates/aterm-window/src/planted.rs:2")
                && out.log.contains("crates/aterm-gui/src/extra.rs:2"),
            "both crates' sites must be named in the diagnostic:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_platform_slice_sites_are_counted_never_graphed() {
        // An ABBA inside a registered winit platform slice (linux) does not
        // compile into the shipped macOS GUI process: it must not graph, but
        // its sites are counted under the slice label.
        let out = run_synth_files(
            "sliceabba",
            "fn plain() {\n    let g = alpha_lock.lock().unwrap();\n}\n",
            &[(
                "crates/aterm-window/src/platform_impl/linux/planted.rs",
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
                "crates/aterm-window/src/planted.rs",
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
            out.log.contains(
                "winit::UNKNOWN@crates/aterm-window/src/planted.rs:2 -> winit::event_state"
            ),
            "the vendored unknown's held-acquire edge must still be reported, \
             namespaced:\n{}",
            out.log
        );
    }

    #[test]
    fn synthetic_propagated_raw_ptr_binding_is_categorized() {
        // `.as_mut_ptr()` seeds `base`, `base.add(i)` extends the evidence to
        // `item`, so `item.read()` is `core::ptr::read`. `other.add(i)` with no
        // proven receiver does not extend, so `thing.read()` stays an RwLock.
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
        // Manifests present (the scan set derives fine) but no lock sites in
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
        let root = synth_tree("aterm-lock-census", "empty", &files);
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
            "aterm-lock-census",
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
        let root = synth_tree("aterm-lock-census", "stalehelper", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            !out.ok,
            "a stale helper registration must be RED:\n{}",
            out.log
        );
        assert!(out.log.contains("no longer defined"), "log:\n{}", out.log);
    }

    #[test]
    fn synthetic_new_gui_dependency_is_scanned_automatically() {
        // A new crate entering aterm-gui's dependency graph, with no census
        // change: its lock sites are scanned (a) and an ABBA inside it fails
        // the census (b).
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
        let root = synth_tree("aterm-lock-census", "newdepgreen", &files);
        let out = run_lock_order_census(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.ok, "GREEN expected:\n{}", out.log);
        // aterm-gui and aterm-types, the new dependency, and every helper crate.
        let crates = 3 + SYNTH_HELPER_CRATES.len();
        assert!(
            out.log
                .contains(&format!("across {crates} workspace crate(s)"))
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
        // (b) An ABBA planted in the new crate goes RED — half the cycle in
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
        let root = synth_tree("aterm-lock-census", "newdepabba", &files);
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
}
