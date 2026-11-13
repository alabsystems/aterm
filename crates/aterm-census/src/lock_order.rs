// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lock-order census: the lock-graph sense of L0-DEADLOCK, as a fail-closed obligation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::{
    CensusOutcome, Floor, collect_rs_files, fn_defined_in, ident_ending_at, is_test_file,
    parse_source_fns, strip_line_comment, test_gated_module_files,
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
        acquires. The callee's callees are NOT followed. An unqualified call
        prefers a free fn in its proven file-level module. Nested/unproved
        module shapes retain the conservative fallback. Other same-named fns merge
        WITHIN a namespace unless an explicit qualifier names a known impl
        owner. `Self::name` uses the caller's enclosing impl; `Type::name`
        selects that impl when the corpus supplies it, including UFCS methods.
        Lexical fn/impl type parameters never prove a concrete owner. Unproved
        aliases, reexports and generic forms keep the conservative by-name
        fallback. A module path narrows only through actual declared file loads
        (including #[path]) to a matching root free-function definition. A full
        std/core/alloc path is outside this corpus only when its root is lexically
        unshadowed and no caller/ancestor include can introduce a binding.
        This uses lexical type names, not compiler resolution;
        same-named types can still merge. Across a namespace they do not merge
        — see NAMESPACE BOUNDARY ON THE HOP.
        Otherwise the callee is chosen BY NAME, so the shapes are limited: a free fn and
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
        items masked and the files a `#[cfg(test)] mod` declares left out (test
        code never compiles into the shipped GUI process);
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
      - VENDORED-IDENTITY MODE: the vendored [patch] crates (vendor/forge.toml
        [[fork]]) and the absorbed crates (ABSORBED_CRATES) that link into the
        GUI process are scanned with every identity in a per-crate namespace
        (`aterm_window::…`), so a foreign receiver name can
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

/// Acquisition vocabulary.
const ACQ_METHODS: &[(&str, bool)] = &[
    (".lock()", true),
    (".lock_or_recover()", true),
    (".read()", true),
    (".write()", true),
    (".try_lock()", false),
    (".try_read()", false),
    (".try_write()", false),
];

/// Result-of-lock adapters that pass the guard through.
const GUARD_ADAPTERS: &[&str] = &["unwrap", "expect", "unwrap_or_else", "map_err"];

/// A fn that acquires a lock and returns the guard.
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
        symbol: "tcp_lane",
        identity: "LANE",
        def_file: "crates/aterm-sysprobe/src/linux_tcp.rs",
    },
    GuardHelper {
        symbol: "held_namespaces",
        identity: "NAMESPACES",
        def_file: "crates/aterm-sysprobe/src/linux_tcp.rs",
    },
    GuardHelper {
        symbol: "posts",
        identity: "posts",
        def_file: "crates/aterm-link/src/state.rs",
    },
    GuardHelper {
        symbol: "judge_posts",
        identity: "posts",
        def_file: "crates/aterm-link/src/state.rs",
    },
    GuardHelper {
        symbol: "asks",
        identity: "asks",
        def_file: "crates/aterm-link/src/state.rs",
    },
    GuardHelper {
        symbol: "link_state",
        identity: "state",
        def_file: "crates/aterm-net/src/fleet/link.rs",
    },
    GuardHelper {
        symbol: "live_state",
        identity: "state",
        def_file: "crates/aterm-net/src/fleet/link.rs",
    },
    GuardHelper {
        symbol: "lock_bytes",
        identity: "bytes",
        def_file: "crates/aterm-restore/src/store.rs",
    },
    GuardHelper {
        symbol: "term_lock",
        identity: "term",
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        symbol: "term_lock_ui",
        identity: "term",
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        symbol: "term_lock_whole",
        identity: "term",
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        symbol: "term_lock_whole_when",
        identity: "term",
        def_file: "crates/aterm-gui/src/lib.rs",
    },
    GuardHelper {
        symbol: "retire_slot",
        identity: "shared",
        def_file: "crates/aterm-gui/src/consent_retire.rs",
    },
    GuardHelper {
        symbol: "keeper_state",
        identity: "KEEPER_STATE",
        def_file: "crates/aterm-gui/src/keeper_link.rs",
    },
    GuardHelper {
        symbol: "board_lock",
        identity: "board",
        def_file: "crates/aterm-gui/src/disk_board.rs",
    },
    GuardHelper {
        symbol: "disk_episode",
        identity: "board",
        def_file: "crates/aterm-gui/src/harness_host.rs",
    },
    GuardHelper {
        symbol: "env_lock",
        identity: "ENV_LOCK",
        def_file: "crates/aterm-log/src/lib.rs",
    },
    GuardHelper {
        symbol: "records",
        identity: "records",
        def_file: "crates/aterm-gui/src/connections.rs",
    },
    GuardHelper {
        symbol: "supervisor",
        identity: "SUPERVISOR",
        def_file: "crates/aterm-gui/src/fabric_launch.rs",
    },
    GuardHelper {
        symbol: "screen_lock",
        identity: "SCREEN",
        def_file: "crates/atpkg/src/meter.rs",
    },
    GuardHelper {
        symbol: "posting_permit",
        identity: "posting",
        def_file: "crates/aterm-gui/src/askpass.rs",
    },
    GuardHelper {
        symbol: "lock_server_state",
        identity: "server_state",
        def_file: "crates/aterm-secrets/src/server.rs",
    },
    GuardHelper {
        symbol: "lock_peers",
        identity: "peers",
        def_file: "crates/aterm-secrets/src/server.rs",
    },
    GuardHelper {
        symbol: "lock_puts",
        identity: "puts",
        def_file: "crates/aterm-secrets/src/server.rs",
    },
    GuardHelper {
        symbol: "join_writes",
        identity: "join_writes",
        def_file: "crates/aterm-secrets/src/server.rs",
    },
    GuardHelper {
        symbol: "hold_rollout",
        identity: "COPIES",
        def_file: "crates/aterm-gui/src/self_claim.rs",
    },
    GuardHelper {
        symbol: "hold_target",
        identity: "COPIES",
        def_file: "crates/aterm-gui/src/self_claim.rs",
    },
    GuardHelper {
        symbol: "spawn_lock_shared",
        identity: "SPAWN_LOCK",
        def_file: "crates/aterm-uds/src/spawn_lock.rs",
    },
    GuardHelper {
        symbol: "spawn_lock_exclusive",
        identity: "SPAWN_LOCK",
        def_file: "crates/aterm-uds/src/spawn_lock.rs",
    },
    GuardHelper {
        symbol: "one_test_at_a_time",
        identity: "REPO_READ_TESTS",
        def_file: "crates/aterm-agent/src/harness/repo_home.rs",
    },
];

/// A fn that implements a standard acquisition method by delegating through bare `self`.
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

/// Lexical `std::fs::File` evidence for a `let` RHS.
fn is_file_binding_rhs(line: &str) -> bool {
    line.contains("File::open(")
        || line.contains("File::create(")
        || line.contains("File::create_new(")
        || (line.contains("OpenOptions::new()") && line.contains(".open("))
        || line.contains(": std::fs::File =")
        || line.contains(": fs::File =")
}

/// Raw-pointer-producing method names.
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

/// Lexical raw-pointer evidence for a `let` RHS.
fn is_raw_ptr_binding_rhs(line: &str) -> bool {
    line.contains(".as_ptr()")
        || line.contains(".as_mut_ptr()")
        || line.contains("as *const")
        || line.contains("as *mut")
        || line.contains("&raw const")
        || line.contains("&raw mut")
}

/// Propagated raw-pointer evidence.
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

/// Whether `name` is declared as a raw pointer (`*const`/`*mut`) in the fn signature.
fn param_is_raw_ptr(sig: &str, name: &str) -> bool {
    let pat = format!("{name}: ");
    let mut from = 0;
    while let Some(rel) = sig[from..].find(&pat) {
        let at = from + rel;
        from = at + pat.len();
        if at > 0 {
            let prev = sig.as_bytes()[at - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                continue;
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
const fn match_paren_back(line: &str, close: usize) -> Option<usize> {
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

/// Raw-pointer evidence for the zero-arg `.read()` whose `.` sits at `dot`.
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

/// Method names the token vocabulary covers at call sites.
const STANDARD_METHOD_NAMES: &[&str] = &[
    "lock",
    "lock_or_recover",
    "read",
    "write",
    "try_lock",
    "try_read",
    "try_write",
];

/// The identity namespace of the aterm workspace crates, which carry no prefix.
const WORKSPACE_NS: Option<&'static str> = None;

/// One lock-acquisition site.
struct AcqSite {
    /// Resolved lexical identity (`term`, `store`, …) or `None` = UNKNOWN.
    identity: Option<String>,
    /// The matched token, for the diagnostics (`term_lock(`, `.read()`, …).
    kind: &'static str,
    /// Can this acquisition block?
    blocking: bool,
    /// Repo-relative `file:line` (first physical line of the statement).
    span: String,
    /// Enclosing fn name.
    fn_name: String,
    /// The (trimmed, capped) statement text, for the diagnostics.
    excerpt: String,
    /// `Some(binding span)` when the receiver is lexically proven a `std::fs::File`.
    advisory: Option<String>,
    /// `Some(audit)` for a registered [`VocabularyInterior`] site.
    vocab: Option<&'static str>,
    /// `Some(evidence)` when the receiver is proven a raw pointer (see [`raw_ptr_read_evidence`]).
    raw_ptr: Option<String>,
    /// `Some(namespace)` for a site in a scanned vendored crate.
    namespace: Option<&'static str>,
}

impl AcqSite {
    /// Does this site join the mutex order graph?
    const fn graphed(&self) -> bool {
        self.advisory.is_none() && self.raw_ptr.is_none()
    }

    /// Graph node name: the identity (namespace-prefixed for vendored sites), or a unique
    /// per-site node for vocabulary-interior and UNKNOWN sites.
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

/// One witness for an `A -> B` edge.
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

/// Mask string literals (`"…"`, honoring `\"`) and char literals with spaces, so token scans and brace counting never fire inside literal text.
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

/// Blank items whose complete cfg predicate is false with test disabled;
/// other cfg atoms and uncertain item boundaries remain conservative.
pub(crate) fn mask_cfg_test_items(text: &str) -> String {
    mask_items_where(text, &crate::cfg_attr_never_ships)
}

/// The graph reads Rust code, not diagnostic strings or quoted source fixtures.
/// Lex before the line-shaped fn parser: stripping `//` inside a URL literal
/// first can destroy its closing quote and hide real code in a later lexical pass.
fn lock_graph_code(text: &str) -> String {
    crate::lex::blank_non_code(&mask_cfg_test_items(text))
}

/// Blank every item whose immediately preceding attribute line matches one of `gates`.
pub(crate) fn mask_gated_items(text: &str, gates: &[&str]) -> String {
    mask_items_where(text, &|attr| gates.contains(&attr))
}

/// [`mask_gated_items`] with the gate test as a predicate over the trimmed attribute line.
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

/// Per line of `text`, whether [`mask_items_where`] keeps it. A line that opens inside a literal or
/// comment carried over from an earlier line ends no item by its indent: its code view starts
/// wherever the literal happens to end.
pub(crate) fn unmasked_lines(text: &str, is_gate: &dyn Fn(&str) -> bool) -> Vec<bool> {
    // Literal/comment state spans physical lines. A fixture's column-zero `}`
    // or `#[cfg(test)]` is data, never an item boundary or an attribute. Keep
    // attribute values in the text view for value-aware target predicates.
    // One lexing serves both views, the attributes and the carried lines.
    let spans = crate::lex::spans(text);
    let lexed = lexed_views(text, &spans);
    let lines: Vec<&str> = lexed.code.lines().collect();
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    // By the line each starts on; of two on one line, the later one.
    let mut attributes: Vec<Option<(usize, &str, bool)>> = vec![None; line_starts.len()];
    for (start, end) in outer_attributes_of(&lexed.code) {
        let first = line_starts.partition_point(|&at| at <= start) - 1;
        let last = line_starts.partition_point(|&at| at <= end) - 1;
        let tail = lexed.code[end..]
            .lines()
            .next()
            .is_some_and(|line| !line.trim().is_empty());
        attributes[first] = Some((last, &lexed.text[start..end], tail));
    }
    let attribute_at = |line: usize| attributes.get(line).copied().flatten();
    let carried = lines_opening_in_a_span(&spans, &line_starts, lines.len());
    let indent_of = |line: &str| line.len() - line.trim_start().len();
    let mut keep = vec![true; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let Some((attr_end, attr, tail)) = attribute_at(i) else {
            i += 1;
            continue;
        };
        let t = lines[i].trim_start();
        if !t.starts_with("#[") || tail || !is_gate(attr.trim()) {
            i += 1;
            continue;
        }
        let indent = lines[i].len() - t.len();
        let mut j = attr_end + 1;
        let mut ambiguous_tail = false;
        while let Some((end, _, tail)) = attribute_at(j) {
            if tail {
                ambiguous_tail = true;
                break;
            }
            j = end + 1;
        }
        // This line-based mask cannot exclude one item while preserving another on
        // the same line. Keep that spelling rather than hiding unrelated code.
        if ambiguous_tail {
            i = j + 1;
            continue;
        }
        keep[i..j].fill(false);
        if j >= lines.len() {
            break;
        }
        let mut k = j;
        let mut opened = false;
        let mut terminated = false;
        let mut ambiguous = false;
        let mut item_delimiters = Vec::new();
        while k < lines.len() {
            let item = lines[k];
            if k > j && !carried[k] && !item.trim().is_empty() && indent_of(lines[k]) < indent {
                k -= 1;
                terminated = true;
                break;
            }
            // A gated const/use can end before a different item's brace on this
            // same line. Inspect the top-level terminator before treating any brace
            // as belonging to the gated candidate.
            if item_has_ambiguous_tail(item, &mut item_delimiters) {
                keep[i..=k].fill(true);
                ambiguous = true;
                break;
            }
            keep[k] = false;
            if item.contains('{') {
                opened = true;
                terminated = true;
                break;
            }
            let s = item.trim_end();
            if item_delimiters.is_empty()
                && (s.ends_with(';')
                    || (s.ends_with(',') && !carried[k] && indent_of(lines[k]) == indent))
            {
                terminated = true;
                break;
            }
            k += 1;
        }
        if ambiguous {
            i = k + 1;
            continue;
        }
        if !terminated {
            for slot in keep.iter_mut().take(lines.len()).skip(j) {
                *slot = true;
            }
            break;
        }
        if opened {
            let first = lines[k];
            let single = first.matches('{').count() == first.matches('}').count();
            if single && !brace_item_ends_line(first) {
                keep[i..=k].fill(true);
            } else if !single {
                let end = (k + 1..lines.len()).find(|m| {
                    let c = lines[*m];
                    c.as_bytes().get(indent) == Some(&b'}')
                        && c.as_bytes()[..indent].iter().all(|&b| b == b' ')
                        && !c.contains('{')
                        && item_tail(&c[indent + 1..])
                });
                match end {
                    Some(end) => {
                        for slot in keep.iter_mut().take(end + 1).skip(k) {
                            *slot = false;
                        }
                        k = end;
                    }
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

/// A balanced brace item may share its line with another item; that is not an
/// exclusion this line mask can represent.
fn brace_item_ends_line(code: &str) -> bool {
    let mut depth = 0usize;
    let mut opened = false;
    for (at, byte) in code.bytes().enumerate() {
        match byte {
            b'{' => {
                opened = true;
                depth += 1;
            }
            b'}' if opened => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return item_tail(&code[at + 1..]);
                }
            }
            _ => {}
        }
    }
    false
}

/// A closure/array call may close delimiters after its brace. No named call,
/// operator or second item can be hidden by accepting only this punctuation.
fn item_tail(tail: &str) -> bool {
    let tail = tail.trim();
    if tail.is_empty() {
        return true;
    }
    let Some(body) = tail.strip_suffix(';').or_else(|| tail.strip_suffix(',')) else {
        return false;
    };
    body.bytes()
        .all(|byte| byte.is_ascii_whitespace() || matches!(byte, b')' | b']' | b'?'))
}

/// Array lengths/repetitions and nested expressions contain semicolons without
/// ending the candidate. A top-level terminator followed by code is a different
/// item/statement that this line-based mask must preserve.
fn item_has_ambiguous_tail(code: &str, stack: &mut Vec<u8>) -> bool {
    for (at, byte) in code.bytes().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => stack.push(byte),
            b')' | b']' | b'}' => {
                let wanted = match byte {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if stack.pop() != Some(wanted) {
                    return true;
                }
            }
            b';' if stack.is_empty() => return !code[at + 1..].trim().is_empty(),
            _ => {}
        }
    }
    false
}

/// Per line (`count` of them, as [`str::lines`] splits the text `line_starts` indexes), whether
/// the line opens inside one of `spans` that began on an earlier line.
fn lines_opening_in_a_span(
    spans: &[crate::lex::Span],
    line_starts: &[usize],
    count: usize,
) -> Vec<bool> {
    let starts = &line_starts[..count.min(line_starts.len())];
    let mut carried = vec![false; count];
    for span in spans {
        let first = starts.partition_point(|&start| start <= span.start);
        for (line, &start) in starts.iter().enumerate().skip(first) {
            if start >= span.end {
                break;
            }
            carried[line] = true;
        }
    }
    carried
}

/// [`crate::lex::lex`] of `src` from its already-read `spans`.
fn lexed_views(src: &str, spans: &[crate::lex::Span]) -> crate::lex::Lexed {
    let blank = |bytes: &mut [u8]| {
        for b in bytes {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    let utf8 = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    };
    let mut code = src.as_bytes().to_vec();
    let mut text = code.clone();
    for span in spans {
        blank(&mut code[span.start..span.end]);
        if span.kind == crate::lex::SpanKind::Comment {
            blank(&mut text[span.start..span.end]);
        }
        if let Some((open, close)) = span.quotes
            && !src.as_bytes()[span.start..span.end].contains(&b'\n')
        {
            code[open] = b'"';
            code[close] = b'"';
        }
    }
    crate::lex::Lexed {
        code: utf8(code),
        text: utf8(text),
    }
}

/// [`crate::lex::outer_attributes`] of the source whose code view is `code`.
fn outer_attributes_of(code: &str) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at + 1 < bytes.len() {
        // Only a `#` can open one.
        match code[at..].find('#') {
            Some(hash) => at += hash,
            None => break,
        }
        if at + 1 >= bytes.len() {
            break;
        }
        if bytes[at + 1] == b'[' {
            let start = at;
            at += 2;
            let mut depth = 1usize;
            while at < bytes.len() && depth > 0 {
                match bytes[at] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                at += 1;
            }
            if depth == 0 {
                out.push((start, at));
            }
        } else {
            at += 1;
        }
    }
    out
}

/// A logical statement line.
struct LogicalLine {
    text: String,
    lineno: usize,
}

/// Join a fn body's physical lines (already comment-stripped) into logical lines.
fn logical_lines(body: &[String], start_line: usize) -> Vec<LogicalLine> {
    let mut out: Vec<LogicalLine> = Vec::new();
    for (i, raw) in body.iter().enumerate() {
        let mut text = mask_literals(raw);
        text.truncate(text.trim_end().len());
        let start = text.len() - text.trim_start().len();
        if text.is_empty() {
            continue;
        }
        if text[start..].starts_with('.')
            && let Some(prev) = out.last_mut()
        {
            prev.text.push_str(&text[start..]);
            continue;
        }
        text.drain(..start);
        out.push(LogicalLine {
            text,
            lineno: start_line + i,
        });
    }
    out
}

/// Self types an `impl` may not lend to a `self`-rooted lock identity.
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

/// How many physical lines an `impl` header may span before the scan gives up on finding its `{`.
const MAX_IMPL_HEADER_LINES: usize = 24;

/// One `impl` block's line span and the self type it implements.
pub(crate) struct ImplSpan {
    /// 1-based first line (the `impl` keyword).
    pub(crate) start: usize,
    /// 1-based last line (the block's closing brace).
    pub(crate) end: usize,
    /// `None` when the header names no type usable as a lock identity (see [`impl_self_type`]).
    ty: Option<String>,
    /// Type parameters that can shadow a concrete corpus type inside this impl.
    type_params: BTreeSet<String>,
}

/// Is `b` an identifier byte (for keyword-boundary checks)?
const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte offsets of every occurrence of the keyword `word` at nesting depth 0 of `s`.
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
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' | b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
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

/// Byte offset of the `>` matching the `<` at byte 0 of `s`, if any.
pub(crate) fn matching_angle(s: &str) -> Option<usize> {
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

/// The comma-separated items of a generic parameter list, split at nesting depth 0.
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

/// Type names declared by a leading generic list; lifetimes and consts name other namespaces.
fn generic_type_parameters(rest: &str) -> BTreeSet<String> {
    let rest = rest.trim_start();
    let mut names = BTreeSet::new();
    if !rest.starts_with('<') {
        return names;
    }
    let Some(close) = matching_angle(rest) else {
        return names;
    };
    for parameter in split_top_level(&rest[1..close]) {
        let parameter = parameter.trim();
        if parameter.starts_with('\'') || parameter.starts_with("const ") {
            continue;
        }
        if let Some(name) = ident_prefix(parameter) {
            names.insert(name.to_owned());
        }
    }
    names
}

/// The base name of an impl's self type.
fn base_type_name(ty: &str, params: &BTreeSet<&str>) -> Option<String> {
    let mut t = ty.trim();
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
    if name.chars().count() == 1
        || params.contains(name)
        || UNQUALIFIABLE_SELF_TYPES.contains(&name)
    {
        return None;
    }
    Some(name.to_string())
}

/// The nameable self type of an `impl` header (text up to the block's `{`).
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
    let rest = match top_level_words(rest, "where").first() {
        Some(&at) => &rest[..at],
        None => rest,
    };
    let self_ty = match top_level_words(rest, "for").last() {
        Some(&at) => &rest[at + "for".len()..],
        None => rest,
    };
    base_type_name(self_ty, &params)
}

/// Every `impl` block in a file.
pub(crate) fn impl_spans(text: &str) -> Vec<ImplSpan> {
    associated_spans(text, false)
}

/// Bounded impl and trait bodies, including default associated functions.
/// The same item/header/closing-brace parser supplies both census consumers.
pub(crate) fn method_owner_spans(text: &str) -> Vec<ImplSpan> {
    associated_spans(text, true)
}

fn associated_spans(text: &str, include_traits: bool) -> Vec<ImplSpan> {
    let lines: Vec<&str> = text.lines().collect();
    let line_starts: Vec<_> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let mut candidates = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        // Inspect only possible item openings; cutting a trailing comment cannot
        // change the opening. The lazy-init consumer also includes trait defaults.
        let opening = raw.trim_start();
        if !["impl", "unsafe ", "default "]
            .iter()
            .any(|word| opening.starts_with(word))
            && !(include_traits
                && ["trait ", "pub ", "pub(", "auto "]
                    .iter()
                    .any(|word| opening.starts_with(word)))
        {
            continue;
        }
        let line = strip_line_comment(raw);
        let head = line.trim_start();
        let head = if include_traits {
            if let Some(rest) = head.strip_prefix("pub(") {
                rest.split_once(')')
                    .map_or(head, |(_, after)| after.trim_start())
            } else {
                head.strip_prefix("pub ").unwrap_or(head).trim_start()
            }
        } else {
            head
        };
        let head = head.strip_prefix("unsafe ").unwrap_or(head).trim_start();
        let head = head.strip_prefix("default ").unwrap_or(head).trim_start();
        let head = head.strip_prefix("auto ").unwrap_or(head).trim_start();
        let (after, trait_item) = if let Some(after) = head.strip_prefix("impl") {
            (after, false)
        } else if include_traits && let Some(after) = head.strip_prefix("trait") {
            (after, true)
        } else {
            continue;
        };
        if !(after.starts_with('<') || after.starts_with(char::is_whitespace)) {
            continue;
        }
        let start = line_starts[i] + line.len() - head.len();
        let limit = line_starts
            .get(i + MAX_IMPL_HEADER_LINES)
            .copied()
            .unwrap_or(text.len());
        candidates.push((i, trait_item, start, limit));
    }
    let starts: Vec<_> = candidates
        .iter()
        .map(|&(_, _, start, limit)| (start, limit))
        .collect();
    let bodies = crate::items::item_bodies_at(text, &starts);
    let mut out = Vec::new();
    for (i, trait_item, start, _) in candidates {
        let Some((header, end)) = bodies.get(&start) else {
            continue;
        };
        out.push(ImplSpan {
            start: i + 1,
            end: line_starts.partition_point(|&at| at < *end),
            ty: if trait_item {
                None
            } else {
                impl_self_type(header)
            },
            type_params: top_level_words(header, "impl")
                .first()
                .map(|at| generic_type_parameters(&header[at + "impl".len()..]))
                .unwrap_or_default(),
        });
    }
    out
}

/// A raw acquisition found on one logical line, ordered by byte position.
struct RawAcq {
    pos: usize,
    kind: &'static str,
    blocking: bool,
    identity: Option<String>,
}

/// Where an acquisition sits, for the identity refinements that need more than the line itself.
struct SiteCtx<'a> {
    /// The file's `impl` blocks (see [`impl_spans`]).
    impls: &'a [ImplSpan],
    /// 1-based physical line of the logical line being scanned.
    lineno: usize,
    /// Live single-letter aliases in this fn (see [`alias_target`]).
    aliases: &'a BTreeMap<String, (i32, String)>,
}

impl SiteCtx<'_> {
    /// Self type of the innermost `impl` block covering this line, if named by this census.
    fn impl_ty(&self) -> Option<&str> {
        self.impls
            .iter()
            .filter(|s| s.start <= self.lineno && self.lineno <= s.end)
            .max_by_key(|s| s.start)
            .and_then(|s| s.ty.as_deref())
    }
}

/// The receiver chain ending at `dot` as a field path below `self`, when rooted there.
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

/// Resolve the lexical identity of the receiver whose final `.` sits at byte `dot`.
fn resolve_receiver(line: &str, dot: usize, ctx: &SiteCtx<'_>) -> Option<String> {
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
                return Some(format!("{ty}::{chain}"));
            }
            if let Some((_, aliased)) = ctx.aliases.get(seg) {
                return Some(aliased.clone());
            }
            return None;
        }
        return Some(seg.to_string());
    }
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

/// All acquisitions on one logical line, in byte order.
fn acquisitions_on(line: &str, ctx: &SiteCtx<'_>) -> Vec<RawAcq> {
    let mut out = Vec::new();
    // A helper call is `<symbol>(` with no identifier byte before the symbol, so each `(`
    // is asked once which symbols end at it.
    let bytes = line.as_bytes();
    for (open, _) in line.match_indices('(') {
        for h in GUARD_HELPERS {
            let sym = h.symbol;
            let Some(at) = open.checked_sub(sym.len()) else {
                continue;
            };
            if !bytes[..open].ends_with(sym.as_bytes()) {
                continue;
            }
            let boundary = at == 0 || {
                let prev = bytes[at - 1];
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

/// Byte index just past the acquisition call's closing paren.
fn call_end(line: &str, acq: &RawAcq) -> usize {
    if acq.kind.starts_with('.') {
        acq.pos + acq.kind.len()
    } else {
        let open = acq.pos + acq.kind.len();
        match_paren(line, open).map_or(line.len(), |c| c + 1)
    }
}

/// Does the guard produced by this acquisition survive as the statement's final value?
fn guard_is_final_value(line: &str, acq: &RawAcq) -> bool {
    let bytes = line.as_bytes();
    let mut i = call_end(line, acq);
    loop {
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'?') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b';' || line[i..].starts_with("else") {
            return true;
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
            return true;
        };
        i = close + 1;
    }
}

/// How a logical line binds the guards acquired on it.
enum LineKind {
    /// `let <var> = …` / `let <pattern> = …` (var carried when plain-ident).
    Let(Option<String>),
    /// `match … {` / `if let … {` / `while let … {`.
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
            return LineKind::Other;
        }
        let after = rest[var.len()..].trim_start_matches(')').trim_start();
        if !var.is_empty() && (after.starts_with('=') || after.starts_with(':')) {
            return LineKind::Let(Some(var));
        }
        return LineKind::Let(None);
    }
    LineKind::Other
}

/// Byte offset of a `let` statement's binding `=`.
fn binding_eq(line: &str) -> Option<usize> {
    const OPS: &[u8] = b"=!<>+-*/%&|^";
    let bytes = line.as_bytes();
    let at = line.find('=')?;
    if bytes.get(at + 1) == Some(&b'=') || (at > 0 && OPS.contains(&bytes[at - 1])) {
        return None;
    }
    Some(at)
}

/// Names bound by ordinary let patterns and complete if/while-let headers. Constructors,
/// paths, type annotations and struct field labels are not bindings. Unsupported syntax
/// retains the conservative call lookup instead of suppressing an edge.
fn local_pattern_bindings(line: &str) -> BTreeSet<String> {
    let text = line.trim_start_matches(['}', ' ']);
    let text = text.strip_prefix("else ").unwrap_or(text).trim_start();
    let pattern = text.strip_prefix("let ").or_else(|| {
        text.ends_with('{').then(|| {
            text.strip_prefix("if let ")
                .or_else(|| text.strip_prefix("while let "))
        })?
    });
    let Some(pattern) = pattern.and_then(|rest| binding_eq(rest).map(|eq| &rest[..eq])) else {
        return BTreeSet::new();
    };
    let mut pending = vec![pattern];
    let mut names = BTreeSet::new();
    while let Some(pattern) = pending.pop() {
        let mut pattern = pattern.trim();
        // A type annotation belongs to the declaration, not its pattern. Ignore :: paths.
        let mut depth = 0;
        for (at, byte) in pattern.bytes().enumerate() {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b':' if depth == 0
                    && !pattern[..at].ends_with(':')
                    && !pattern[at + 1..].starts_with(':') =>
                {
                    pattern = pattern[..at].trim_end();
                    break;
                }
                _ => {}
            }
        }
        loop {
            let next = pattern
                .strip_prefix('&')
                .or_else(|| pattern.strip_prefix("ref "))
                .or_else(|| pattern.strip_prefix("mut "));
            match next {
                Some(rest) => pattern = rest.trim_start(),
                None => break,
            }
        }
        if matches!(pattern, "" | "_" | ".." | "true" | "false") {
            continue;
        }
        if ident_prefix(pattern) == Some(pattern) {
            // Capitalized constants/unit variants are not evidence of a callback binding.
            // Retain the ordinary lower-case binding posture used by local call lookup.
            if pattern.starts_with(|c: char| c.is_lowercase() || c == '_') {
                names.insert(pattern.to_owned());
            }
            continue;
        }
        let Some(open) = pattern.find(['(', '[', '{']) else {
            return BTreeSet::new();
        };
        let prefix = pattern[..open].trim();
        if prefix == "const"
            || (pattern[open..].starts_with('[') && !prefix.is_empty())
            || (pattern[open..].starts_with('{') && prefix.is_empty())
        {
            return BTreeSet::new();
        }
        if !prefix.is_empty()
            && !prefix
                .trim_start_matches("::")
                .split("::")
                .all(|part| !part.is_empty() && ident_prefix(part) == Some(part))
        {
            return BTreeSet::new();
        }
        let group = &pattern[open..];
        let mut closers = Vec::new();
        for (at, byte) in group.bytes().enumerate() {
            match byte {
                b'(' => closers.push(b')'),
                b'[' => closers.push(b']'),
                b'{' => closers.push(b'}'),
                b')' | b']' | b'}' => {
                    if closers.pop() != Some(byte) || (closers.is_empty() && at + 1 != group.len())
                    {
                        return BTreeSet::new();
                    }
                }
                _ => {}
            }
        }
        if !closers.is_empty() {
            return BTreeSet::new();
        }
        for field in split_top_level(&group[1..group.len() - 1]) {
            let field = field.trim();
            let payload = if group.starts_with('{') {
                field.split_once(':').map_or(field, |(label, payload)| {
                    if ident_prefix(label.trim()) == Some(label.trim()) && !payload.starts_with(':')
                    {
                        payload
                    } else {
                        field
                    }
                })
            } else {
                field
            };
            pending.push(payload);
        }
    }
    names
}

/// The identity a `let` binding aliases when its RHS is a pure path.
fn alias_target(line: &str, ctx: &SiteCtx<'_>) -> Option<String> {
    let eq = binding_eq(line)?;
    let mut path = line[eq + 1..].trim().trim_end_matches(';').trim_end();
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

/// A one-hop target. A proved impl qualifier keeps unrelated same-named functions apart.
#[derive(Clone)]
struct CallTarget {
    name: String,
    qualifier: Option<String>,
    /// Every written path segment before the function, retaining the external root.
    path: Vec<String>,
    /// Neither a qualified path nor a method call: module-local free functions may shadow.
    unqualified: bool,
    /// A lexical parameter can shadow `Key::f`, but not the explicit path `crate::Key::f`.
    bare_qualifier: bool,
}

/// A proven module target, an unshadowed language-library root, or no proof.
enum ModuleResolution {
    File(String),
    External,
    Unknown,
}

/// What [`ModulePaths`] reads of one file, gathered on the worker that scanned it.
struct ModuleFile {
    /// The file's [`lock_graph_code`], which the shadow checks read.
    code: String,
    /// Each load the module loader reads ([`crate::guards::SourceFile::loads`]): its module
    /// path (`None` splices the file into its includer), whether it is test-gated, and the
    /// files it can name.
    loads: Vec<(Option<Vec<String>>, bool, Vec<String>)>,
}

impl ModuleFile {
    fn read(rel: &str, text: String, code: String) -> Self {
        let reader = crate::guards::SourceFile::outside(rel.to_owned(), text);
        let loads = reader
            .loads()
            .iter()
            .map(|load| (load.module.clone(), load.gated, load.files.clone()))
            .collect();
        Self { code, loads }
    }
}

/// Only declarations read by the existing module loader establish module edges.
struct ModulePaths {
    files: BTreeMap<String, ModuleFile>,
    edges: BTreeMap<(String, Vec<String>), BTreeSet<String>>,
    parents: BTreeMap<String, BTreeSet<String>>,
    spliced: BTreeSet<String>,
    /// Pure lexical facts reused only within this immutable source snapshot.
    shadow_facts: BTreeMap<(String, String, bool), bool>,
    /// Each asked file's root-independent shadow facts, read once.
    file_shadows: BTreeMap<String, ShadowFacts>,
}

impl ModulePaths {
    fn new(files: BTreeMap<String, ModuleFile>) -> Self {
        let mut edges: BTreeMap<(String, Vec<String>), BTreeSet<String>> = BTreeMap::new();
        let mut parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut spliced = BTreeSet::new();
        for (file, read) in &files {
            for (module, gated, targets) in &read.loads {
                let Some(module) = module else {
                    spliced.insert(file.clone());
                    continue;
                };
                if *gated {
                    continue;
                }
                let targets: Vec<_> = targets
                    .iter()
                    .filter(|path| files.contains_key(*path))
                    .collect();
                if let [target] = targets.as_slice() {
                    edges
                        .entry((file.clone(), module.clone()))
                        .or_default()
                        .insert((*target).clone());
                    if module.len() == 1 {
                        parents
                            .entry((*target).clone())
                            .or_default()
                            .insert(file.clone());
                    }
                }
            }
        }
        Self {
            files,
            edges,
            parents,
            spliced,
            shadow_facts: BTreeMap::new(),
            file_shadows: BTreeMap::new(),
        }
    }

    fn shadowed(&mut self, file: &str, root: &str, module: bool) -> bool {
        let key = (file.to_owned(), root.to_owned(), module);
        if let Some(&fact) = self.shadow_facts.get(&key) {
            return fact;
        }
        let fact = self.files.get(file).is_none_or(|read| {
            let facts = self
                .file_shadows
                .entry(file.to_owned())
                .or_insert_with(|| ShadowFacts::of(&read.code));
            if module {
                facts.module_root_shadowed(&read.code, root)
            } else {
                facts.root_may_be_shadowed(&read.code, root)
            }
        });
        self.shadow_facts.insert(key, fact);
        fact
    }

    /// `file` and every file that declares it, at any depth.
    fn scopes(&self, file: &str) -> BTreeSet<String> {
        let mut scopes = BTreeSet::from([file.to_owned()]);
        let mut pending = vec![file.to_owned()];
        while let Some(scope) = pending.pop() {
            if let Some(parents) = self.parents.get(&scope) {
                for parent in parents {
                    if scopes.insert(parent.clone()) {
                        pending.push(parent.clone());
                    }
                }
            }
        }
        scopes
    }

    /// Read, across the workers, the shadow facts of every file [`Self::resolve`] can ask
    /// about for `callers`' held calls: the files it asks are among these.
    fn read_shadows(&mut self, callers: &[FnLockFacts]) {
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        for caller in callers {
            for (_, target, _) in &caller.held_calls {
                let Some(first) = target.path.first() else {
                    continue;
                };
                if caller.type_params.contains(first) {
                    continue;
                }
                if matches!(first.as_str(), "std" | "core" | "alloc") && target.path.len() > 1 {
                    wanted.extend(self.scopes(&caller.source_file));
                }
                if !matches!(first.as_str(), "crate" | "self" | "super")
                    && let Some(module) = &caller.module
                {
                    wanted.insert(module.clone());
                }
            }
        }
        let wanted: Vec<String> = wanted
            .into_iter()
            .filter(|file| self.files.contains_key(file) && !self.file_shadows.contains_key(file))
            .collect();
        let files = &self.files;
        let facts = run_parallel(wanted.len(), |at| ShadowFacts::of(&files[&wanted[at]].code));
        self.file_shadows.extend(wanted.into_iter().zip(facts));
    }

    fn resolve(&mut self, caller: &FnLockFacts, target: &CallTarget) -> ModuleResolution {
        let file = &caller.source_file;
        let Some(first) = target.path.first() else {
            return ModuleResolution::Unknown;
        };
        if caller.type_params.contains(first) {
            return ModuleResolution::Unknown;
        }
        if matches!(first.as_str(), "std" | "core" | "alloc") && target.path.len() > 1 {
            let scopes = self.scopes(file);
            let shadowed = scopes
                .iter()
                .any(|scope| self.shadowed(scope, first, false));
            if !shadowed && scopes.iter().all(|scope| !self.spliced.contains(scope)) {
                return ModuleResolution::External;
            }
        }
        let Some(file) = &caller.module else {
            return ModuleResolution::Unknown;
        };
        if !matches!(first.as_str(), "crate" | "self" | "super") && self.shadowed(file, first, true)
        {
            return ModuleResolution::Unknown;
        }
        let mut file = file.clone();
        let mut path = target.path.as_slice();
        match first.as_str() {
            "crate" => {
                let mut visited = BTreeSet::new();
                while let Some(parents) = self.parents.get(&file) {
                    if parents.len() != 1 || !visited.insert(file.clone()) {
                        return ModuleResolution::Unknown;
                    }
                    file = parents.first().expect("one parent").clone();
                }
                if !file.ends_with("/lib.rs") && !file.ends_with("/main.rs") {
                    return ModuleResolution::Unknown;
                }
                path = &path[1..];
            }
            "self" => path = &path[1..],
            "super" => {
                while path.first().is_some_and(|part| part == "super") {
                    let Some(parents) = self.parents.get(&file) else {
                        return ModuleResolution::Unknown;
                    };
                    if parents.len() != 1 {
                        return ModuleResolution::Unknown;
                    }
                    file = parents.first().expect("one parent").clone();
                    path = &path[1..];
                }
            }
            _ => {}
        }
        while !path.is_empty() {
            let found = (1..=path.len()).rev().find_map(|len| {
                self.edges
                    .get(&(file.clone(), path[..len].to_vec()))
                    .filter(|files| files.len() == 1)
                    .and_then(|files| files.first())
                    .map(|file| (file.clone(), len))
            });
            let Some((next, len)) = found else {
                return ModuleResolution::Unknown;
            };
            file = next;
            path = &path[len..];
        }
        if self.spliced.contains(&file) {
            ModuleResolution::Unknown
        } else {
            ModuleResolution::File(file)
        }
    }
}

/// The lexical evidence one file's code holds against a root, before any root is asked.
struct ShadowFacts {
    /// Byte offset of every `r#` in the code.
    raw: Vec<usize>,
    /// Every word a `type`, `struct`, `enum`, `trait`, `union`, `as` or `crate` precedes.
    declared: BTreeSet<String>,
    /// How many times `mod` precedes each word.
    mods: BTreeMap<String, usize>,
    /// Some `use` tree holds a glob.
    glob: bool,
    /// Every `use` tree, trimmed.
    imports: Vec<String>,
}

impl ShadowFacts {
    /// Words and `use` trees are read with every `r#` removed.
    fn of(code: &str) -> Self {
        let raw: Vec<usize> = code.match_indices("r#").map(|(at, _)| at).collect();
        let code = if raw.is_empty() {
            std::borrow::Cow::Borrowed(code)
        } else {
            std::borrow::Cow::Owned(code.replace("r#", ""))
        };
        let mut declared = BTreeSet::new();
        let mut mods: BTreeMap<String, usize> = BTreeMap::new();
        let mut before: Option<&str> = None;
        for word in ascii_words(&code) {
            match before {
                Some("mod") => match mods.get_mut(word) {
                    Some(count) => *count += 1,
                    None => {
                        mods.insert(word.to_owned(), 1);
                    }
                },
                Some("type" | "struct" | "enum" | "trait" | "union" | "as" | "crate")
                    if !declared.contains(word) =>
                {
                    declared.insert(word.to_owned());
                }
                _ => {}
            }
            before = Some(word);
        }
        let mut glob = false;
        let mut imports = Vec::new();
        for at in use_starts(&code) {
            let import = code[at + 3..].split(';').next().unwrap_or_default();
            glob |= import.contains('*');
            imports.push(import.trim().to_owned());
        }
        Self {
            raw,
            declared,
            mods,
            glob,
            imports,
        }
    }

    /// `code` spells `r#<root>`.
    fn raw_root(&self, code: &str, root: &str) -> bool {
        self.raw.iter().any(|&at| code[at + 2..].starts_with(root))
    }

    /// A glob, or a `use` tree naming `root` other than as its own first segment.
    fn imports_root(&self, root: &str) -> bool {
        self.glob
            || self.imports.iter().any(|import| {
                import
                    .strip_prefix(&format!("{root}::"))
                    .unwrap_or(import)
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|word| word == root)
            })
    }

    /// Lexical uncertainty is not proof of an external root. In particular a glob may
    /// import a module with this name, and an alias or type parameter may shadow it.
    fn root_may_be_shadowed(&self, code: &str, root: &str) -> bool {
        self.raw_root(code, root)
            || self.mods.contains_key(root)
            || self.declared.contains(root)
            || self.imports_root(root)
    }

    /// A local import/type can shadow a same-named declared module. More than one
    /// lexical module declaration is not enough evidence to pick a scope.
    fn module_root_shadowed(&self, code: &str, root: &str) -> bool {
        self.raw_root(code, root)
            || self.mods.get(root).is_some_and(|&count| count > 1)
            || self.declared.contains(root)
            || self.imports_root(root)
    }
}

/// The maximal runs of ASCII alphanumerics and `_` in `code`, in order: what splitting it at
/// every other char leaves, empty pieces dropped.
fn ascii_words(code: &str) -> impl Iterator<Item = &str> {
    let bytes = code.as_bytes();
    let mut at = 0;
    std::iter::from_fn(move || {
        let word = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_';
        let start = at + bytes[at..].iter().position(word)?;
        let end = bytes[start..]
            .iter()
            .position(|b| !word(b))
            .map_or(bytes.len(), |len| start + len);
        at = end;
        Some(&code[start..end])
    })
}

/// Keyword boundaries allow whitespace or comments between `use` and its tree.
fn use_starts(code: &str) -> Vec<usize> {
    let ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    code.match_indices("use")
        .filter_map(|(at, _)| {
            let before = at.checked_sub(1).and_then(|at| code.as_bytes().get(at));
            let after = code.as_bytes().get(at + 3);
            (!before.is_some_and(|&b| ident(b)) && !after.is_some_and(|&b| ident(b))).then_some(at)
        })
        .collect()
}

/// A call the census will follow one hop: `(byte offset in the line, target)`.
type TrustedCall = (usize, CallTarget);

/// A call through a `self.field.…` receiver: `(byte offset, field, callee)`.
type FieldRecvCall = (usize, String, String);

/// Type and field declarations harvested from the workspace crates.
#[derive(Default)]
struct FieldTypes {
    /// Every type name the corpus defines.
    defined: BTreeSet<String>,
    /// Field name -> every type identifier in its declared type (`Arc<Shared>` -> {Arc, Shared}).
    field_tys: BTreeMap<String, BTreeSet<String>>,
}

impl FieldTypes {
    /// Is `field`'s declared type defined in this corpus, asked from namespace `at`?
    fn is_corpus_typed(&self, at: Option<&'static str>, field: &str) -> bool {
        at == WORKSPACE_NS
            && self
                .field_tys
                .get(field)
                .is_some_and(|tys| tys.iter().any(|t| self.defined.contains(t)))
    }

    /// Add another file's harvest: each set is the union of every file's.
    fn absorb(&mut self, other: Self) {
        self.defined.extend(other.defined);
        for (field, tys) in other.field_tys {
            self.field_tys.entry(field).or_default().extend(tys);
        }
    }

    /// Harvest declarations from one already-cfg-masked source file.
    fn harvest(&mut self, text: &str) {
        let mut body_depth: Option<i32> = None;
        let mut depth: i32 = 0;
        for raw in text.lines() {
            let line = before_double_slash(raw);
            let t = line.trim_start();
            let mut opens_fields = false;
            for kw in ["struct ", "enum ", "union ", "trait ", "type "] {
                let Some(rest) = t.strip_prefix(kw).or_else(|| {
                    t.strip_prefix("pub ")
                        .and_then(|p| p.trim_start().strip_prefix(kw))
                        .or_else(|| {
                            t.strip_prefix("pub(")
                                .and_then(|p| p.split_once(')'))
                                .and_then(|(_, p)| p.trim_start().strip_prefix(kw))
                        })
                }) else {
                    continue;
                };
                if let Some(n) = ident_prefix(rest.trim_start()) {
                    self.defined.insert(n.to_string());
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
            let (opens, closes) = brace_counts(line);
            depth += opens as i32 - closes as i32;
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

/// `s` up to its first `//`.
fn before_double_slash(s: &str) -> &str {
    let mut from = 0;
    while let Some(slash) = s[from..].find('/') {
        let at = from + slash;
        if s.as_bytes().get(at + 1) == Some(&b'/') {
            return &s[..at];
        }
        from = at + 1;
    }
    s
}

/// The leading identifier of `s`, if it starts with one.
fn ident_prefix(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    (end > 0).then(|| &s[..end])
}

/// Simple parameter bindings that shadow bare fn names in the signature.
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

/// One-hop call targets on `line`.
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
            continue;
        }
        let start = idx - name.len();
        if line[..start].trim_end().ends_with("fn") {
            continue;
        }
        if start > 0 && bytes[start - 1] == b'.' {
            let Some(recv) = ident_ending_at(line, start - 1) else {
                continue;
            };
            if recv != "self" {
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
        let qualifier_path = line[..start]
            .trim_end()
            .strip_suffix("::")
            .map(str::trim_end);
        let path = qualifier_path.and_then(ascii_qualified_segments);
        // A non-ASCII candidate is unsupported as a whole. Retaining only its
        // ASCII suffix could falsely prove either an external root or an impl.
        let qualifier = path
            .as_ref()
            .and(qualifier_path)
            .and_then(|prefix| ident_ending_at(prefix, prefix.len()))
            .map(str::to_owned);
        let bare_qualifier =
            qualifier_path
                .zip(qualifier.as_deref())
                .is_some_and(|(prefix, name)| {
                    !prefix[..prefix.len() - name.len()]
                        .trim_end()
                        .ends_with("::")
                });
        out.push((
            idx,
            CallTarget {
                name: name.to_string(),
                qualifier,
                path: path.unwrap_or_default(),
                unqualified: !line[..start].trim_end().ends_with(['.', ':']),
                bare_qualifier,
            },
        ));
    }
    (out, field_recv)
}

/// Preserve the complete supported candidate, never a suffix after an unsupported
/// Unicode identifier or whitespace-separated continuation. Character offsets keep
/// every slice on a UTF-8 boundary; unsupported candidates retain by-name edges.
fn ascii_qualified_segments(prefix: &str) -> Option<Vec<String>> {
    let mut start = 0;
    for (at, character) in prefix.char_indices().rev() {
        if !character.is_ascii() || matches!(character, '#' | '$') {
            return None;
        }
        if !(character.is_ascii_alphanumeric() || matches!(character, '_' | ':')) {
            start = at + character.len_utf8();
            break;
        }
    }
    if prefix[..start].trim_end().ends_with("::") || prefix[start..].starts_with("::") {
        return None;
    }
    Some(
        prefix[start..]
            .split("::")
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Everything the census learned about one fn.
struct FnLockFacts {
    name: String,
    /// Repo-relative `file:line` of the definition.
    span: String,
    /// The identity namespace of the crate defining this fn ([`WORKSPACE_NS`] for aterm crates).
    namespace: Option<&'static str>,
    /// Source file even when the function's inline module cannot be resolved.
    source_file: String,
    /// Proven file-level module; inline/unproved modules keep the by-name fallback.
    module: Option<String>,
    /// An actual free function, not a method or an unnameable impl's associated function.
    free_function: bool,
    /// The enclosing named impl, using the same lexical type identity as lock receivers.
    impl_ty: Option<String>,
    /// Lexical fn and enclosing impl type parameters, which are not concrete call owners.
    type_params: BTreeSet<String>,
    /// Does the signature return a `…Guard`?
    returns_guard: bool,
    /// Is the fn method-shaped (first parameter `self`)?
    takes_self: bool,
    /// Indices into the global site table.
    acq: Vec<usize>,
    /// Intra-fn held-acquire pairs: (holder site, acquired site).
    edges: Vec<(usize, usize)>,
    /// Calls made while holding: (holder site, target, call `file:line`).
    held_calls: Vec<(usize, CallTarget, String)>,
    /// Calls made while holding through a field receiver (`self.field.name(..)`).
    field_recv_calls: Vec<(usize, String, String, String)>,
    /// Parameter and local names that shadow bare fn calls inside the body.
    binding_calls: Vec<(String, String)>,
}

impl FnLockFacts {
    /// Move every site index by `base`, as when its file's sites join the global table there.
    fn rebase(&mut self, base: usize) {
        for site in &mut self.acq {
            *site += base;
        }
        for (hold, acq) in &mut self.edges {
            *hold += base;
            *acq += base;
        }
        for (hold, _, _) in &mut self.held_calls {
            *hold += base;
        }
        for (hold, _, _, _) in &mut self.field_recv_calls {
            *hold += base;
        }
    }
}

/// Lexical guard-return detection over the signature region.
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

/// Does any Rust identifier in `ret` end with `Guard`?
fn return_type_names_a_guard(ret: &str) -> bool {
    ret.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|ident| ident.ends_with("Guard"))
}

/// Lexical method-shape detection over the signature region.
fn takes_self(body: &[String]) -> bool {
    let mut sig = String::new();
    for line in body {
        sig.push_str(line);
        sig.push(' ');
        if line.contains('{') {
            break;
        }
    }
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
    /// `let … = <acquire> else { … }`: the guard binds only after the else block.
    active: bool,
}

/// A simple local name shadows bare fn lookup only after its initializer statement.
struct LocalBinding {
    name: String,
    binding_depth: i32,
    active: bool,
}

/// The file-level lexical facts a body scan needs beyond the body's own text.
struct FileCtx<'a> {
    /// Repo-relative path, for the site spans.
    rel: &'a str,
    /// `Some(prefix)` in vendored-identity mode.
    namespace: Option<&'static str>,
    /// The file's `impl` blocks.
    impls: &'a [ImplSpan],
    root_lines: &'a BTreeSet<usize>,
}

/// Lines beginning at the file's module root. Strings and comments are already masked;
/// nested modules remain unproved rather than borrowing this file-level identity.
fn module_root_lines(masked: &str) -> BTreeSet<usize> {
    let mut depth = 0i32;
    masked
        .lines()
        .enumerate()
        .filter_map(|(line, code)| {
            let root = depth == 0;
            let (opens, closes) = brace_counts(code);
            depth += opens as i32 - closes as i32;
            root.then_some(line + 1)
        })
        .collect()
}

/// How many `{` and `}` bytes `code` holds.
fn brace_counts(code: &str) -> (usize, usize) {
    code.bytes()
        .fold((0, 0), |(opens, closes), byte| match byte {
            b'{' => (opens + 1, closes),
            b'}' => (opens, closes + 1),
            _ => (opens, closes),
        })
}

/// Scan one fn body.
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
    let enclosing_impl = file
        .impls
        .iter()
        .filter(|span| span.start <= start_line && start_line <= span.end)
        .max_by_key(|span| span.start);
    let module_root = file
        .root_lines
        .contains(&enclosing_impl.map_or(start_line, |span| span.start));
    let nested_functions: BTreeSet<_> = body
        .iter()
        .skip(1)
        .filter_map(|line| crate::parse_fn_def(line).map(|(_, name)| name))
        .collect();
    let mut facts = FnLockFacts {
        name: name.to_string(),
        span: span.to_string(),
        namespace: file.namespace,
        source_file: file.rel.to_owned(),
        module: module_root.then(|| file.rel.to_owned()),
        free_function: enclosing_impl.is_none() && module_root,
        impl_ty: file
            .impls
            .iter()
            .filter(|s| s.start <= start_line && start_line <= s.end)
            .max_by_key(|s| s.start)
            .and_then(|s| s.ty.clone()),
        type_params: file
            .impls
            .iter()
            .filter(|s| s.start <= start_line && start_line <= s.end)
            .flat_map(|s| s.type_params.iter().cloned())
            .collect(),
        returns_guard: returns_guard(body),
        takes_self: takes_self(body),
        acq: Vec::new(),
        edges: Vec::new(),
        held_calls: Vec::new(),
        field_recv_calls: Vec::new(),
        binding_calls: Vec::new(),
    };
    let mut live: Vec<LiveGuard> = Vec::new();
    let mut file_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    let mut ptr_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    let mut alias_vars: BTreeMap<String, (i32, String)> = BTreeMap::new();
    let mut local_bindings: Vec<LocalBinding> = Vec::new();
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
    if let Some((_, rest)) = sig_text.split_once("fn ")
        && let Some(name) = ident_prefix(rest)
    {
        facts
            .type_params
            .extend(generic_type_parameters(&rest[name.len()..]));
    }
    let mut depth: i32 = 0;
    for ll in &lines {
        let line = &ll.text;
        let leading_closes = line
            .chars()
            .take_while(|c| c.is_whitespace() || *c == '}')
            .filter(|c| *c == '}')
            .count();
        // `} else {` has no net depth change, but the previous branch's locals have ended
        // before its condition or body can call an unshadowed corpus function.
        let entry_depth = depth - i32::try_from(leading_closes).unwrap_or(0);
        local_bindings.retain(|binding| entry_depth >= binding.binding_depth);
        let (opens, closes) = brace_counts(line);
        let opens = i32::try_from(opens).unwrap_or(0);
        let closes = i32::try_from(closes).unwrap_or(0);
        let depth_end = depth + opens - closes;
        if !live.is_empty() && line.contains("drop(") {
            live.retain(|g| {
                g.var.as_ref().is_none_or(|v| {
                    !line.contains(&format!("drop({v})")) && !line.contains(&format!("drop(&{v})"))
                })
            });
        }
        let kind = classify_line(line);
        if let LineKind::Let(Some(var)) = &kind {
            live.retain(|g| g.var.as_ref() != Some(var));
        }
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
            let raw_ptr = if r.kind == ".read()" {
                raw_ptr_read_evidence(line, r.pos, &sig_text, &ptr_vars)
            } else {
                None
            };
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
                None
            } else {
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
            for (pos, mut target) in trusted {
                let callee = target.name.as_str();
                // A nested function item can shadow the module function too. Its scope is not
                // proved here, so it must retain the conservative lookup rather than narrow.
                if nested_functions.contains(callee) {
                    target.unqualified = false;
                }
                let call_span = format!("{}:{}", file.rel, ll.lineno);
                let before = &line[..pos - callee.len()];
                if !before.trim_end().ends_with(['.', ':']) {
                    let binding = if local_bindings.iter().any(|b| b.active && b.name == callee) {
                        Some("local")
                    } else if parameters.contains(callee) {
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
                        .push((g.site, target.clone(), call_span.clone()));
                }
                for (k, r) in raw.iter().enumerate() {
                    if r.pos < pos && sites[this_line[k]].graphed() {
                        facts
                            .held_calls
                            .push((this_line[k], target.clone(), call_span.clone()));
                    }
                }
            }
        }
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
        let names = local_pattern_bindings(line);
        for name in names {
            local_bindings.push(LocalBinding {
                name,
                binding_depth: if matches!(kind, LineKind::MatchLike) {
                    depth_end.max(depth)
                } else {
                    depth
                },
                active: matches!(kind, LineKind::MatchLike),
            });
        }
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
        depth = depth_end;
        live.retain(|g| depth >= g.binding_depth);
        file_vars.retain(|_, (d, _)| depth >= *d);
        ptr_vars.retain(|_, (d, _)| depth >= *d);
        alias_vars.retain(|_, (d, _)| depth >= *d);
        local_bindings.retain(|binding| depth >= binding.binding_depth);
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

/// Append the why + repair block for a lock-order cycle.
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

/// One file's part of the census, read on a worker and merged in the census's file order.
struct FileScan {
    rel: String,
    module: ModuleFile,
    /// The declarations of a workspace file; an isolated library's add none.
    field_types: FieldTypes,
    /// Its fns, their site indices into [`Self::sites`].
    fns: Vec<FnLockFacts>,
    sites: Vec<AcqSite>,
    /// For a file in a labeled platform slice, the slice and its site count, in place of
    /// [`Self::fns`] and [`Self::sites`].
    slice: Option<(usize, usize)>,
}

/// Read and scan one file of a workspace crate (`vendored` `None`) or of an isolated library.
fn scan_file(
    root: &Path,
    file: &Path,
    vendored: Option<&crate::scan_set::ScannedVendored>,
) -> Option<FileScan> {
    let text = crate::tree::read_to_string(file).ok()?;
    let slice_idx = vendored.and_then(|v| {
        let in_crate = crate::rel_path(&root.join(&v.crate_dir), file);
        v.platform_slices.iter().position(|s| {
            s.paths
                .iter()
                .any(|p| in_crate == *p || in_crate.starts_with(&format!("{p}/")))
        })
    });
    let rel = crate::rel_path(root, file);
    let masked = lock_graph_code(&text);
    let mut field_types = FieldTypes::default();
    if vendored.is_none() {
        field_types.harvest(&masked);
    }
    let impls = impl_spans(&masked);
    let root_lines = module_root_lines(&masked);
    let ctx = FileCtx {
        rel: &rel,
        namespace: vendored.map(|v| v.namespace),
        impls: &impls,
        root_lines: &root_lines,
    };
    let mut parsed = Vec::new();
    parse_source_fns(&masked, &rel, &mut parsed);
    let mut fns = Vec::new();
    let mut sites = Vec::new();
    let slice = match slice_idx {
        Some(i) => {
            for f in &parsed {
                let _ = scan_fn(&f.name, &f.span, &f.body, &ctx, &mut sites);
            }
            let count = sites.len();
            sites.clear();
            Some((i, count))
        }
        None => {
            for f in &parsed {
                fns.push(scan_fn(&f.name, &f.span, &f.body, &ctx, &mut sites));
            }
            None
        }
    };
    let module = ModuleFile::read(&rel, text, masked);
    Some(FileScan {
        rel,
        module,
        field_types,
        fns,
        sites,
        slice,
    })
}

/// `job(0)..job(count)` across worker threads (this one among them), each answer in its slot.
fn run_parallel<T: Send>(count: usize, job: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if at >= count {
                return done;
            }
            done.push((at, job(at)));
        }
    };
    let workers = aterm_cores::available_parallelism()
        .map_or(4, |n| n.get().min(16))
        .min(count);
    let mut slots: Vec<Option<T>> = (0..count).map(|_| None).collect();
    // Each worker carries this thread's memo scope, so what it reads is the census unit's
    // question ([`crate::memo::threads`]).
    crate::memo::threads(|scope| {
        // A worker that cannot start leaves its share to the others.
        let helpers: Vec<_> = (1..workers)
            .filter_map(|n| {
                scope
                    .spawn_named(format!("census-locks-{n}"), 8 << 20, work)
                    .ok()
            })
            .collect();
        for (at, answer) in work() {
            slots[at] = Some(answer);
        }
        for helper in helpers {
            match helper.join() {
                Ok(done) => {
                    for (at, answer) in done {
                        slots[at] = Some(answer);
                    }
                }
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });
    slots
        .into_iter()
        .map(|slot| slot.expect("every job ran"))
        .collect()
}

/// The lock-order census's verdict title.
const LOCKS: &str = "lock-order census";

pub fn run_lock_order_census(root: &Path) -> CensusOutcome {
    let mut log = String::new();
    let mut failures = 0usize;
    let _ = writeln!(
        log,
        "=== lock-order census (L0 deadlock, lock-graph sense) ===\n\
         \x20   root: {}",
        root.display()
    );

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

    // Each crate's files as the census reads them: the workspace crates, then the isolated
    // libraries. Every file is read on a worker; the merge below keeps this order.
    let mut units: Vec<(
        Vec<std::path::PathBuf>,
        Option<&crate::scan_set::ScannedVendored>,
    )> = Vec::new();
    for crate_dir in &scan.scan_dirs {
        if scan
            .vendored_scanned
            .iter()
            .any(|entry| entry.scan_dir == *crate_dir)
        {
            continue;
        }
        let mut files = Vec::new();
        let _ = collect_rs_files(&root.join(crate_dir), &mut files);
        files.retain(|p| !is_test_file(p));
        units.push((files, None));
    }
    for v in &scan.vendored_scanned {
        let mut files = Vec::new();
        let _ = collect_rs_files(&root.join(&v.scan_dir), &mut files);
        files.retain(|p| !is_test_file(p));
        units.push((files, Some(v)));
    }
    let gates: Vec<std::sync::OnceLock<BTreeSet<std::path::PathBuf>>> =
        units.iter().map(|_| std::sync::OnceLock::new()).collect();
    // The longest jobs first: each crate's test-module walk, largest crate first, then the files.
    let mut jobs: Vec<(usize, Option<usize>)> = (0..units.len()).map(|u| (u, None)).collect();
    jobs.sort_by_key(|&(u, _)| std::cmp::Reverse(units[u].0.len()));
    for (u, (files, _)) in units.iter().enumerate() {
        jobs.extend((0..files.len()).map(|i| (u, Some(i))));
    }
    let mut scanned: BTreeMap<(usize, usize), FileScan> = BTreeMap::new();
    for (job, done) in jobs.iter().zip(run_parallel(jobs.len(), |at| {
        let (u, file) = jobs[at];
        let (files, vendored) = &units[u];
        let Some(i) = file else {
            let _ = gates[u].set(test_gated_module_files(files));
            return None;
        };
        // A file its crate already declared test-only is left out by the merge anyway.
        if gates[u]
            .get()
            .is_some_and(|gated| gated.contains(&files[i]))
        {
            return None;
        }
        scan_file(root, &files[i], *vendored)
    })) {
        if let (Some(scan), (u, Some(i))) = (done, *job) {
            scanned.insert((u, i), scan);
        }
    }

    let mut module_files: BTreeMap<String, ModuleFile> = BTreeMap::new();
    let mut sites: Vec<AcqSite> = Vec::new();
    let mut fns: Vec<FnLockFacts> = Vec::new();
    let mut field_types = FieldTypes::default();
    let mut test_gated = 0usize;
    let mut slice_counts: Vec<(String, &'static str, usize)> = Vec::new();
    for (u, ((files, vendored), gated)) in units.iter().zip(gates).enumerate() {
        let gated = gated.into_inner().unwrap_or_default();
        test_gated += gated.len();
        let mut order: Vec<usize> = (0..files.len())
            .filter(|&i| !gated.contains(&files[i]))
            .collect();
        order.sort_by(|&a, &b| files[a].cmp(&files[b]));
        let mut per_slice = vec![0usize; vendored.map_or(0, |v| v.platform_slices.len())];
        for i in order {
            let Some(file) = scanned.remove(&(u, i)) else {
                continue;
            };
            module_files.insert(file.rel, file.module);
            field_types.absorb(file.field_types);
            if let Some((slice, count)) = file.slice {
                per_slice[slice] += count;
                continue;
            }
            let base = sites.len();
            sites.extend(file.sites);
            fns.extend(file.fns.into_iter().map(|mut f| {
                f.rebase(base);
                f
            }));
        }
        if let Some(v) = vendored {
            for (i, slice) in v.platform_slices.iter().enumerate() {
                slice_counts.push((v.package.clone(), slice.label, per_slice[i]));
            }
        }
    }
    let _ = writeln!(
        log,
        "    {test_gated} file(s) a `#[cfg(test)] mod` declares left out: test code, which no \
         shipped process links"
    );

    if sites.is_empty() {
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] ZERO lock-acquisition sites found under {} — the census \
             walked nothing (parser broke, or this root is not an aterm checkout?).",
            scan.scan_dirs.join(", ")
        );
        return CensusOutcome::red(LOCKS, log, "1 obligation violation(s).");
    }

    let mut unresolved_field_calls: Vec<(String, String)> = Vec::new();
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
                f.held_calls.push((
                    site,
                    CallTarget {
                        name: callee,
                        qualifier: None,
                        path: Vec::new(),
                        unqualified: false,
                        bare_qualifier: false,
                    },
                    span,
                ));
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

    let mut modules = ModulePaths::new(module_files);
    modules.read_shadows(&fns);
    let mut by_name: BTreeMap<&str, BTreeMap<Option<&'static str>, Vec<usize>>> = BTreeMap::new();
    for (idx, f) in fns.iter().enumerate() {
        by_name
            .entry(&f.name)
            .or_default()
            .entry(f.namespace)
            .or_default()
            .push(idx);
    }

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
            || (f.namespace == WORKSPACE_NS && GUARD_HELPERS.iter().any(|h| h.symbol == f.name))
        {
            continue;
        }
        // A row names aterm's own crates only. One naming a fn private to a vendored or
        // external-workspace crate would go stale on that crate's next refactor, and astream
        // lands first, so it could never move the row in the same change.
        let remedy = if f.namespace == WORKSPACE_NS {
            "Register it in GUARD_HELPERS (crates/aterm-census/src/lock_order.rs) with its \
             lock identity — registration EXTENDS coverage; it is not a waiver. (A FREE fn \
             with a standard method name — `lock`/`read`/… — is NOT exempt: `lock(&X)` call \
             sites carry no token. If it wraps several different locks, inline the standard \
             vocabulary at its call sites instead, e.g. `MutexExt::lock_or_recover`.)"
        } else {
            "It is not one of aterm's own crates, which are all GUARD_HELPERS can name: spell \
             the acquisition out at each of its call sites, in that crate's own source."
        };
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-7] UNREGISTERED GUARD-RETURNING HELPER — `fn {}` ({}) acquires a \
             lock AND returns a `…Guard`, so its CALLERS hold that lock invisibly (no \
             `.lock()` token at their call sites) and the census cannot place their \
             held-acquire edges. {remedy}",
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
        for (hold, target, call_span) in &f.held_calls {
            let resolution = modules.resolve(f, target);
            if matches!(resolution, ModuleResolution::External) {
                continue;
            }
            let Some(per_ns) = by_name.get(target.name.as_str()) else {
                continue;
            };
            let qualifier = target
                .qualifier
                .as_deref()
                .filter(|owner| !(target.bare_qualifier && f.type_params.contains(*owner)))
                .and_then(|owner| {
                    if owner == "Self" {
                        f.impl_ty.as_deref()
                    } else {
                        Some(owner)
                    }
                });
            // Only narrow a by-name hop when the corpus supplies the named impl owner.
            // Aliases, module paths and other unproved forms keep the conservative fallback.
            let owner = qualifier
                .filter(|_| {
                    !target
                        .path
                        .first()
                        .is_some_and(|root| matches!(root.as_str(), "std" | "core" | "alloc"))
                })
                .filter(|owner| {
                    per_ns
                        .values()
                        .flatten()
                        .any(|&g| fns[g].impl_ty.as_deref() == Some(*owner))
                });
            let defined_in_module = match &resolution {
                ModuleResolution::File(module) => per_ns
                    .values()
                    .flatten()
                    .any(|&g| fns[g].free_function && fns[g].module.as_ref() == Some(module)),
                _ => false,
            };
            let compatible = |g: usize| match &resolution {
                ModuleResolution::File(module) if defined_in_module => {
                    fns[g].free_function && fns[g].module.as_ref() == Some(module)
                }
                ModuleResolution::File(_) => true,
                _ => owner.is_none_or(|owner| fns[g].impl_ty.as_deref() == Some(owner)),
            };
            let module_local = |g: usize| {
                target.unqualified
                    && fns[g].free_function
                    && fns[g].namespace == f.namespace
                    && f.module
                        .as_ref()
                        .is_some_and(|module| fns[g].module.as_ref() == Some(module))
            };
            let local_definition = per_ns.values().flatten().copied().any(module_local);
            let shadowed = per_ns
                .get(&f.namespace)
                .is_some_and(|local| local.iter().copied().any(compatible));
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for (def_ns, callee_fns) in per_ns {
                if shadowed && *def_ns != f.namespace {
                    continue;
                }
                for &g in callee_fns {
                    if !compatible(g) || (local_definition && !module_local(g)) {
                        continue;
                    }
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
                                via: Some(format!(
                                    "call to `{}` at {call_span}",
                                    target.qualifier.as_ref().map_or_else(
                                        || target.name.clone(),
                                        |owner| format!("{owner}::{}", target.name)
                                    )
                                )),
                            });
                    }
                }
            }
        }
    }

    let advisory_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.advisory.is_some()).collect();
    let raw_ptr_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.raw_ptr.is_some()).collect();
    let vocab_sites: Vec<&AcqSite> = sites.iter().filter(|s| s.vocab.is_some()).collect();
    let blocking = sites.iter().filter(|s| s.blocking && s.graphed()).count();
    let tries = sites.iter().filter(|s| !s.blocking && s.graphed()).count();
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
            "    isolated libraries in the graph (vendored or external-workspace identity mode: every \
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
         over {}; one interprocedural hop; a lexical walk, not a \
         proof.",
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

/// Iterative Tarjan strongly-connected components.
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
#[path = "lock_order_tests.rs"]
mod tests;
