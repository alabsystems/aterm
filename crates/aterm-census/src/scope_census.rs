// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SCOPE-CARDINALITY CENSUS (OB-13..OB-18) — the "one enforcer, N instances"
//! CLASS, as a fail-closed, build-blocking OBLIGATION.
//!
//! THE BUG THIS GUARDS: `FlashLimiter` (aterm-spec `derive.rs`) proves at most
//! 2 ignitions per rolling second over the four 250 ms slots of ONE limiter,
//! and `flash_limiter_conformance_real_limiter_projects_onto_model` drives ONE
//! `&mut Vec<IgnitionReservation>` through a scripted storm. Both stay GREEN if
//! every split pane gets its own `WordDecorations`: each limiter independently
//! satisfies the invariant while a photosensitive viewer — who has one retina,
//! not one per pane — sees 2N flashes per second. The WCAG 2.3.1 argument is
//! WINDOW-WIDE; nothing in the model, the binding, or the type system pinned
//! how many limiters exist. It was caught by a human READING the words
//! "window-wide" in a doc comment.
//!
//! THE CLASS: a model verifies a LOCAL property of one instance of an
//! enforcing structure while the property that matters is GLOBAL across all
//! live instances, and nothing pins the instance count.
//!
//! THE MOVE: the doc comment stops being prose and becomes a [`ScopeClaim`] —
//! a declared ownership CHAIN from the scope root down to the enforcing state,
//! plus the CLOSURE of every other place that state may live. The census
//! re-derives both from the tree on every build. Multiplying an enforcer
//! either re-types a pinned link (OB-13) or opens the closure (OB-14), and the
//! compile fails.
//!
//! The obligations (`[OB-n]` tags in the diagnostics):
//!
//! * OB-13 CHAIN PINNED — every declared link's field declaration occurs
//!   EXACTLY ONCE, verbatim, in the declared owner's struct body. Re-typing
//!   `word_decos: WordDecorations` to a per-pane map is a 0-occurrence RED.
//! * OB-14 CLOSURE — every non-test struct-field declaration and every
//!   construction site of a claim's `closed_tokens` type must be accounted for
//!   by its `chain` or its `replicas`. A NEW owner (a per-pane struct, a
//!   per-pane constructor in a loop) that never touches the chain is RED here.
//! * OB-15 AGGREGATION — a claim with SHARD replicas must name an
//!   `aggregator` whose body reads every declared access path. Two copies of
//!   the enforcing state with no site that reads both IS the bug, restated.
//! * OB-16 LIVENESS — anti-vacuity, per claim: files exist, owners resolve,
//!   the `machine` (when declared) resolves in aterm-spec's `derive.rs`, prose
//!   fields are non-empty, `chain[0].owner == root`, and every closed token is
//!   actually mentioned by the chain it claims to pin.
//! * OB-17 VOCABULARY LOCK — a [`RESERVED_SCOPE_PHRASES`] phrase in a `///`
//!   doc block requires the file to be in some claim's `covers_prose_in`, or
//!   the block to carry an explicit `scope-waiver: <reason>`. This is the
//!   RECALL knob: it catches exactly what the human caught.
//! * OB-18 STANDING COHERENCE — every registered standing finding must still
//!   be re-detected this run (fixed ⇒ stale-RED, the celebration path) and
//!   carry a non-empty written finding. Not a waiver channel: a standing
//!   finding is reprinted in full on every build and re-verified every build.
//!
//! OB-13/14/15/16/18 have NO waiver channel, matching the OB-7 lock-order
//! precedent ("a detected cycle can only be fixed"). Only OB-17 has one,
//! because it is the recall knob rather than the safety obligation, and a
//! vocabulary migration with no waiver channel would be rejected on contact.
//!
//! ONE implementation, THREE consumers, so verb, instrument and gate cannot
//! diverge: `cargo xtask gate scope`, `cargo run -p aterm-census -- <root>
//! --scope`, and tools/freeze-safety-gate/build.rs (obligation 5).

use std::fmt::Write as _;
use std::path::Path;

use crate::lock_order::{mask_gated_items, mask_literals};
use crate::{CensusOutcome, collect_rs_files, is_test_file, strip_line_comment};

// ---------------------------------------------------------------------------
// The claim vocabulary
// ---------------------------------------------------------------------------

/// The physical thing the safety budget belongs to. DECLARED, never derived:
/// the census enforces the declaration. Choosing it wrong is a review defect,
/// not a scan defect — see [`SCOPE_PRECISION_NOTE`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// One per OS window. Two windows are two enforcers ON PURPOSE.
    Window,
}

/// One link of the ownership chain from the scope root down to the enforcing
/// state.
///
/// `decl` is the field declaration VERBATIM (trimmed). Pinning the SPELLING
/// rather than parsing the type is the point: `HashMap<PaneId,
/// WordDecorations>`, `Vec<WordDecorations>`, a type alias and a newtype all
/// read as a change, and a parser that normalised them away would normalise
/// away the bug.
struct Link {
    file: &'static str,
    owner: &'static str,
    decl: &'static str,
}

/// Why a second copy of the enforcing state is sound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReplicaKind {
    /// A DIFFERENT scope instance (another process, another surface). Its
    /// budget is its own; nothing has to reconcile the two.
    SeparateScope,
    /// The SAME scope's state, split across containers. The aggregate property
    /// is only real if some site reads every shard — hence [`Aggregator`].
    Shard,
}

/// A place the enforcing state legitimately lives OUTSIDE the primary chain,
/// with the argument for why a second one is sound.
struct Replica {
    file: &'static str,
    /// `struct` name for a field replica, or the enclosing `fn` for a
    /// construction-site replica.
    owner: &'static str,
    decl: &'static str,
    kind: ReplicaKind,
    justification: &'static str,
}

/// The fn that must re-establish the aggregate across every shard. Required
/// whenever a claim has [`ReplicaKind::Shard`] replicas: two copies of the
/// state with no site that reads both IS the bug, restated.
struct Aggregator {
    symbol: &'static str,
    file: &'static str,
    /// The access path each shard must appear as inside that fn's body.
    reads: &'static [&'static str],
}

/// A registered STANDING FINDING: a known, real, deliberately-unfixed
/// violation, reported on every run and RE-DETECTED every run (OB-18) — never
/// a waiver. Mirrors [`crate::wasm_census`]'s OB-10/OB-11 discipline: a
/// finding that stops reproducing is stale-RED and must be deleted, so the
/// registry cannot rot into a silent exemption.
struct StandingScopeFinding {
    claim: &'static str,
    obligation: &'static str,
    /// The exact subject the violation must name (the `detail` of the
    /// [`Violation`] it excuses). Matching on the SUBJECT, not just the
    /// obligation, is what stops a second, different violation of the same
    /// obligation from hiding behind this entry.
    detail: &'static str,
    finding: &'static str,
}

/// A global-scope safety claim: a doc-comment assertion turned into an
/// obligation the census re-derives from the tree on every build.
struct ScopeClaim {
    /// Stable id; appears in diagnostics and in standing-finding cross-refs.
    id: &'static str,
    /// The `ty` machine whose invariants ARE the safety argument. `Some`
    /// forces the model to keep existing (OB-16); `None` requires
    /// `unmodelled_because` to say why not.
    machine: Option<&'static str>,
    unmodelled_because: &'static str,
    scope: Scope,
    /// The type that owns the scope — the chain's root. `chain[0].owner` must
    /// equal it, and its cardinality is `scope` by construction.
    root: &'static str,
    chain: &'static [Link],
    /// Type tokens whose every non-test field declaration and construction
    /// site must be accounted for by `chain` + `replicas` (OB-14).
    closed_tokens: &'static [&'static str],
    replicas: &'static [Replica],
    aggregator: Option<Aggregator>,
    /// Files whose reserved-vocabulary doc claims this claim discharges.
    covers_prose_in: &'static [&'static str],
    /// The sentence a reviewer reads. Non-empty, enforced by OB-16.
    rationale: &'static str,
}

// ---------------------------------------------------------------------------
// The registry (the census's fail-closed ground truth)
// ---------------------------------------------------------------------------

const SCOPE_CLAIMS: &[ScopeClaim] = &[
    ScopeClaim {
        id: "flash-limiter",
        machine: Some("FlashLimiterWindow"),
        unmodelled_because: "",
        scope: Scope::Window,
        root: "WindowState",
        chain: &[
            Link {
                file: "crates/aterm-gui/src/lib.rs",
                owner: "WindowState",
                decl: "word_decos: crate::word_decorations::WordDecorations,",
            },
            Link {
                file: "crates/aterm-effects/src/word_decorations.rs",
                owner: "WordDecorations",
                decl: "ignitions: Vec<IgnitionReservation>,",
            },
        ],
        closed_tokens: &["WordDecorations"],
        replicas: &[
            Replica {
                file: "crates/aterm-effects/src/pipeline.rs",
                owner: "EffectsPipeline",
                decl: "decos: WordDecorations,",
                kind: ReplicaKind::SeparateScope,
                justification: "The wasm/web pipeline is a DIFFERENT PROCESS driving its \
                    own browser surface — its window, its budget (the same limiter is \
                    documented at aterm-wasm/src/effects_api.rs). It never shares a \
                    retina with a native WindowState, so it is a second SCOPE, not a \
                    second enforcer inside one scope.",
            },
            Replica {
                file: "crates/aterm-gui/src/settings_preview.rs",
                owner: "kitty_layer",
                decl: "WordDecorations::default()",
                kind: ReplicaKind::SeparateScope,
                justification: "An offscreen still: the preview engine bakes ONE Nyan \
                    frame into a sprite atlas and is dropped. It never ignites (no \
                    scan_row, no tick), so it holds no flash budget and cannot put a \
                    luminance transition on the glass.",
            },
            Replica {
                file: "crates/aterm-gui/src/settings_preview.rs",
                owner: "pet_layer",
                decl: "WordDecorations::default()",
                kind: ReplicaKind::SeparateScope,
                justification: "`kitty_layer`'s twin for the settings PET preview (landed \
                    2026-08-26 with the pet species picker). Same shape, same argument: one \
                    sitting pose per species is baked into a `OnceLock` sprite atlas and the \
                    engine is dropped. `pet_cursor` only drives the tile bakers — it reaches \
                    neither `super_prepass` nor `nova_prepass`, the only writers of the \
                    `ignitions` reservation vec — so this instance holds no flash budget and \
                    cannot put a luminance transition on the glass.",
            },
        ],
        // All three replicas are separate SCOPES, not shards, so there is
        // nothing to re-aggregate: OB-15 requires an aggregator only for
        // shards, and forbids a dead one.
        aggregator: None,
        covers_prose_in: &[
            "crates/aterm-effects/src/word_decorations.rs",
            "crates/aterm-gui/src/app_render.rs",
            "crates/aterm-wasm/src/effects_api.rs",
            "crates/aterm-gpu-web/src/effects_api.rs",
            "crates/aterm-spec/src/derive.rs",
            // `flash_limiter_window_model` and its "window-wide" prose moved
            // here when the model catalog was split out of derive.rs by family.
            "crates/aterm-spec/src/derive/models_effects.rs",
        ],
        rationale: "WCAG 2.3.1 charges 2 flash pairs per ignition against a \
            more-than-3-flashes-per-second threshold measured on ONE retina. The budget \
            therefore belongs to the WINDOW, not to the pane: N per-pane limiters would \
            each satisfy `FlashLimiter` while the glass showed 2N flashes per second. \
            The reservation vec MUST NOT multiply with the pane count. \
            `FlashLimiterWindow`'s MULTIPLY gate is the machine-checked form of that \
            sentence.",
    },
    ScopeClaim {
        id: "supernova-burst-mutex",
        machine: None,
        unmodelled_because: "No ty_model! exists for the §3.2 two-way burst mutex (derive.rs \
            has no MAX_ACTIVE_SUPERNOVAE / nova_add machine). Modelling it is the follow-up \
            this claim's STANDING FINDING blocks on; the CARDINALITY is pinned here \
            regardless, which is what stops the shard set growing further.",
        scope: Scope::Window,
        root: "WindowState",
        chain: &[
            Link {
                file: "crates/aterm-gui/src/lib.rs",
                owner: "WindowState",
                decl: "word_decos: crate::word_decorations::WordDecorations,",
            },
            Link {
                file: "crates/aterm-effects/src/word_decorations.rs",
                owner: "WordDecorations",
                decl: "persist: FxHashMap<u64, Episode>,",
            },
        ],
        // The leaf type is `HashMap<u64, Episode>` — a vocabulary far too
        // common to close over. This claim's teeth are OB-13 (the chain) plus
        // OB-15 (the aggregator), not a closure sweep.
        closed_tokens: &[],
        replicas: &[Replica {
            file: "crates/aterm-effects/src/word_decorations.rs",
            owner: "ParkedPane",
            decl: "persist: FxHashMap<u64, Episode>,",
            kind: ReplicaKind::Shard,
            justification: "Episodes are grid-shaped, so a grid the engine is not \
                currently scanning parks its own map and a swap brings it back. TWO \
                seams do that swap, both keyed by SESSION id: `bind_pane` for a \
                composed host's panes, and `set_scan_session` for an unsplit host's \
                TABS (added 2026-08-09 — one window's tabs shared one map, so the same \
                word at the same cell in two tabs was one episode). This is a SHARD, \
                not a separate scope: the burst mutex derived from these maps is still \
                window-wide (supernova.rs `MAX_ACTIVE_SUPERNOVAE`), so the mutex's own \
                scan must cover every parked map — hence the required `aggregator`, \
                which reads `self.parked` and therefore already covers the tabs.",
        }],
        aggregator: Some(Aggregator {
            symbol: "super_prepass",
            file: "crates/aterm-effects/src/word_decorations.rs",
            reads: &["self.persist", "self.parked"],
        }),
        covers_prose_in: &[
            "crates/aterm-effects/src/supernova.rs",
            "crates/aterm-effects/src/word_decorations.rs",
        ],
        rationale: "§3.2 two-way burst mutex: MAX_ACTIVE_SUPERNOVAE = 1 is what keeps the \
            combined `nova_add` channel under MAX_NOVA_QUADS = 1536. Two live supernovae \
            make it 2 x 900 = 1800, or 3 x 392 + 900 = 2076 mixed — and the const-assert \
            derivation in supernova.rs is falsified SILENTLY, because that assert is over \
            CONSTANTS and can never observe a second instance.",
    },
];

/// Registered standing findings — re-detected and REPRINTED on every run
/// (OB-18), never silenced. EMPTY since 2026-08-08: the supernova-burst-mutex
/// aggregator finding closed when `super_prepass`'s busy scan grew the
/// `self.parked` shard chain (the exact repair the finding prescribed) — the
/// celebration path. The regression test below keeps the check honest.
const SCOPE_STANDING_FINDINGS: &[StandingScopeFinding] = &[];

/// Phrases that assert a scope WIDER than the declaration they sit on. Using
/// one is a CLAIM, and a claim is an obligation — that is the whole lesson of
/// the flash limiter: the only thing standing between a photosensitive user
/// and 2N flashes per second was a human reading the words "window-wide".
///
/// "process-wide" is DELIBERATELY ABSENT: a `static` / `OnceLock` is its own
/// cardinality proof, and every process-wide claim in this tree is one. The
/// dangerous claims are the ones scoped to a container the type system does
/// not make unique.
const RESERVED_SCOPE_PHRASES: &[&str] = &[
    "window-wide",
    "window wide",
    "app-wide",
    "screen-wide",
    "session-wide",
    "must not multiply",
    "one per window",
    "one per process",
    "one per app",
];

/// The explicit OB-17 escape hatch. It is a DIRECTIVE, so it must OPEN a line
/// of the same doc block (`/// scope-waiver: <reason>`) — see
/// [`has_waiver_directive`] for why merely naming it in a sentence must not
/// count.
const SCOPE_WAIVER_MARKER: &str = "scope-waiver:";

/// Sources this census never reads: its own registry names every token and
/// every reserved phrase, so scanning it would flag the gate as its own
/// violation.
const SELF_EXCLUDED_DIR: &str = "crates/aterm-census/src";

/// The honest limits of this census, printed verbatim in every RED diagnostic
/// (and quoted in docs/temporal-safety-gate.md so the docs cannot drift).
// NOTE: a plain multi-line literal (no `\` continuations, which would strip the
// leading indentation the diagnostic relies on).
pub const SCOPE_PRECISION_NOTE: &str = "    PRECISION / SCOPE (the honest limits of this census):
      - LEXICAL, declaration-text based. A chain link is pinned by the VERBATIM
        field declaration inside a `struct <owner> {` body, comments and string
        literals masked. Multiplication through `Box<dyn Any>`, a slot map, a
        `thread_local!`, or a macro-generated field is INVISIBLE: the realistic
        regression (re-typing the field to a per-pane collection, or adding a
        per-pane owner/constructor) is caught, an enforcer that never had a
        field is not.
      - THE SCOPE ROOT IS DECLARED, NOT DERIVED. The census enforces `exactly
        one enforcer per WindowState`; it cannot know whether WindowState is
        the physically right scope (WCAG's real scope is a retina, which spans
        overlapping windows). Its contribution is turning an unstated
        assumption into a stated, diffable one.
      - PROSE COVERAGE IS PER FILE. `covers_prose_in` discharges EVERY reserved
        phrase in a file, not the specific doc block — coarse on purpose, so
        the migration is a few decisions rather than a few hundred. OB-17 also
        only sees `///` blocks: `//!` module prose and undocumented enforcers
        are out of its reach by construction.
      - IT PROVES THE ENFORCER DOES NOT MULTIPLY, NOT THAT NOT-MULTIPLYING IS
        SUFFICIENT. Whether the aggregator's scan is CORRECT over every shard
        is a model obligation (the `machine` field), checked elsewhere.
      - SCOPE OF THE SWEEPS: non-test `crates/**/*.rs`, with #[cfg(test)] /
        #[cfg(kani)] items blanked and crates/aterm-census/src excluded (the
        registry itself names every token and phrase).
";

// ---------------------------------------------------------------------------
// Source primitives
// ---------------------------------------------------------------------------

/// Every file OB-16 resolves a claim's `machine` against: aterm-spec's
/// `derive.rs` plus each `derive/*.rs` family module the model catalog is split
/// across. Enumerated rather than listed so splitting or merging a family
/// module cannot silently take a machine out of the census's reach — which is
/// exactly how `FlashLimiterWindow` stopped resolving when the catalog moved
/// out of the single file.
fn derive_model_sources(root: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = vec![root.join("crates/aterm-spec/src/derive.rs")];
    if let Ok(entries) = std::fs::read_dir(root.join("crates/aterm-spec/src/derive")) {
        let mut split: Vec<std::path::PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .collect();
        split.sort();
        paths.extend(split);
    }
    paths
}

/// Blank comments and string literals, preserving line count and byte offsets
/// well enough for line-oriented matching. Used by every structural
/// obligation, so a token inside a doc comment or a diagnostic string can
/// never be mistaken for a declaration. OB-17 deliberately reads the UNmasked
/// text — the doc comments are its subject.
fn mask_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let literals_masked = mask_literals(line);
        out.push_str(strip_line_comment(&literals_masked));
        out.push('\n');
    }
    out
}

/// The body of `struct <owner> {` … matching `}` in `source` (comments already
/// masked). `None` when the struct is absent — OB-16's "the claim went stale"
/// signal, never a silent pass.
fn struct_body<'a>(source: &'a str, owner: &str) -> Option<&'a str> {
    let head = format!("struct {owner} {{");
    let start = source.find(&head)? + head.len();
    brace_body(source, start)
}

/// From `start` (just past an opening `{`), the slice up to its matching `}`.
fn brace_body(source: &str, start: usize) -> Option<&str> {
    let mut depth = 1i32;
    for (i, b) in source.as_bytes()[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[start..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every `struct <Name> { … }` in a masked source, as `(name, body)`. Tuple
/// structs (`struct X(..)`) have no named fields and are skipped.
fn all_struct_bodies(source: &str) -> Vec<(String, &str)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = source[from..].find("struct ") {
        let at = from + rel;
        from = at + "struct ".len();
        // `struct` must be a whole word: reject `_struct`, `mystruct`.
        if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
            continue;
        }
        let rest = &source[from..];
        let name_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if name_len == 0 {
            continue;
        }
        let name = &rest[..name_len];
        let tail = rest[name_len..].trim_start();
        if !tail.starts_with('{') {
            continue;
        }
        let brace = from + name_len + rest[name_len..].find('{').unwrap_or(0) + 1;
        if let Some(body) = brace_body(source, brace) {
            out.push((name.to_string(), body));
        }
    }
    out
}

/// The body of `fn <symbol>(` … matching `}` in a masked source. The first `{`
/// at paren-depth 0 after the signature opens the body, so `-> Option<Instant>`
/// and multi-line parameter lists are handled without parsing types.
fn fn_body<'a>(source: &'a str, symbol: &str) -> Option<&'a str> {
    let head = format!("fn {symbol}(");
    let at = source.find(&head)?;
    let mut depth = 0i32;
    let bytes = source.as_bytes();
    let mut i = at + head.len() - 1;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'{' if depth == 0 => return brace_body(source, i + 1),
            b';' if depth == 0 => return None, // a declaration without a body
            _ => {}
        }
        i += 1;
    }
    None
}

/// The name of the `fn` a byte offset sits inside — the nearest preceding
/// `fn <ident>(`. Closures are not fns, which is what makes a constructor
/// inside `|| …` attribute to its enclosing function.
fn enclosing_fn(source: &str, at: usize) -> Option<String> {
    let head = &source[..at];
    let mut best: Option<String> = None;
    let mut from = 0usize;
    while let Some(rel) = head[from..].find("fn ") {
        let idx = from + rel;
        from = idx + 3;
        let before = head.as_bytes().get(idx.wrapping_sub(1)).copied();
        if idx > 0 && matches!(before, Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            continue;
        }
        let rest = &head[from..];
        let name_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if name_len == 0 || !rest[name_len..].starts_with('(') {
            continue;
        }
        best = Some(rest[..name_len].to_string());
    }
    best
}

/// Is `hay[at..at + needle.len()]` a whole-word occurrence of `needle`?
fn word_at(hay: &str, at: usize, needle: &str) -> bool {
    let b = hay.as_bytes();
    let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
    let end = at + needle.len();
    let after_ok = end >= b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_');
    before_ok && after_ok
}

/// The field name of a declaration like `word_decos: crate::…::T,`.
fn field_name(decl: &str) -> Option<&str> {
    let (lhs, _) = decl.split_once(':')?;
    let name = lhs.rsplit(' ').next()?.trim();
    (!name.is_empty()).then_some(name)
}

/// The 1-based line number containing byte offset `at`.
fn line_of(source: &str, at: usize) -> usize {
    source[..at].bytes().filter(|b| *b == b'\n').count() + 1
}

/// Every non-test source under `root/crates`, minus this census's own sources.
fn sweep_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);
    files.retain(|p| {
        !is_test_file(p)
            && !p
                .to_string_lossy()
                .replace('\\', "/")
                .contains(SELF_EXCLUDED_DIR)
    });
    files.sort();
    files
}

/// Item gates whose code never ships: masked before every sweep, so test-only
/// engines and kani harnesses can construct as many enforcers as they like.
const UNSHIPPED_GATES: &[&str] = &["#[cfg(test)]", "#[cfg(kani)]"];

/// Repo-relative, forward-slashed path for diagnostics and registry matching.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// One obligation violation, before the standing-finding registry decides
/// whether it is a build-blocking failure or a reported hazard.
struct Violation {
    claim: &'static str,
    obligation: &'static str,
    /// The exact subject, matched against [`StandingScopeFinding::detail`].
    detail: String,
    message: String,
}

/// The SCOPE-CARDINALITY census over the live registry. See the module doc.
#[must_use]
pub fn run_scope_census(root: &Path) -> CensusOutcome {
    run_scope_census_over(root, SCOPE_CLAIMS, SCOPE_STANDING_FINDINGS)
}

/// The census body, parameterised by registry so the tests can drive ONE claim
/// over a copied-and-mutated real tree (which is what makes the demonstration
/// a mutation test against the real files rather than a synthetic lookalike).
fn run_scope_census_over(
    root: &Path,
    claims: &[ScopeClaim],
    findings: &[StandingScopeFinding],
) -> CensusOutcome {
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== scope-cardinality census (OB-13..OB-18): {} claim(s) over {} ===",
        claims.len(),
        root.display()
    );
    let mut v: Vec<Violation> = Vec::new();

    for claim in claims {
        check_liveness(root, claim, &mut v);
        for link in claim.chain {
            check_link(root, claim, link, &mut v);
        }
        check_aggregation(root, claim, &mut v);
    }
    // OB-14 and OB-17 both need every non-test source; read the tree ONCE.
    let waivers = sweep(root, claims, &mut v);

    // The standing-finding registry: partition the violations, then check the
    // registry's own coherence (OB-18).
    let mut matched = vec![false; findings.len()];
    let mut hard: Vec<&Violation> = Vec::new();
    let mut standing: Vec<(&StandingScopeFinding, &Violation)> = Vec::new();
    for viol in &v {
        match findings.iter().position(|f| {
            f.claim == viol.claim && f.obligation == viol.obligation && f.detail == viol.detail
        }) {
            Some(i) => {
                matched[i] = true;
                standing.push((&findings[i], viol));
            }
            None => hard.push(viol),
        }
    }
    for viol in &hard {
        let _ = writeln!(log, "{}", viol.message);
    }
    for (f, viol) in &standing {
        let _ = writeln!(
            log,
            "  ! STANDING FINDING [{}] claim `{}` (subject `{}`) — REGISTERED, real, \
             deliberately unfixed; re-detected this run and reprinted in full (never a \
             waiver):\n\x20     {}\n\x20   detected as: {}",
            f.obligation,
            f.claim,
            f.detail,
            f.finding,
            viol.message.trim_start()
        );
    }
    let mut failures = hard.len();
    for (i, f) in findings.iter().enumerate() {
        if f.finding.trim().is_empty() {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-18] standing finding for claim `{}` ({}) has an EMPTY finding \
                 text — every entry must carry the written analysis it excuses.",
                f.claim, f.obligation
            );
            failures += 1;
        }
        if !matched[i] {
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-18] standing finding for claim `{}` ({}, subject `{}`) was NOT \
                 re-detected this run — either it was FIXED (delete the entry: the finding \
                 is closed, this is the celebration path) or the claim it hangs off changed \
                 shape and it no longer describes the code. A standing finding may not \
                 silently stop reproducing.",
                f.claim, f.obligation, f.detail
            );
            failures += 1;
        }
    }

    if failures > 0 {
        let _ = write!(log, "{SCOPE_PRECISION_NOTE}");
        let _ = writeln!(
            log,
            "gate scope: FAILED — {failures} obligation violation(s) across {} claim(s). \
             This census blocks BOTH `cargo xtask gate scope` and the `cargo build` of \
             tools/freeze-safety-gate. OB-13/14/15/16/18 have NO waiver channel.",
            claims.len()
        );
        return CensusOutcome { ok: false, log };
    }
    let _ = writeln!(
        log,
        "gate scope: GREEN — {} scope claim(s) re-derived from the tree ({} pinned chain \
         link(s), {} accounted replica(s), {} closed token(s)); {} STANDING finding(s) \
         reported above (registered, re-detected and reprinted every run — not waivers); \
         {} reserved-vocabulary doc block(s) waived explicitly.",
        claims.len(),
        claims.iter().map(|c| c.chain.len()).sum::<usize>(),
        claims.iter().map(|c| c.replicas.len()).sum::<usize>(),
        claims.iter().map(|c| c.closed_tokens.len()).sum::<usize>(),
        standing.len(),
        waivers,
    );
    let _ = writeln!(
        log,
        "    scope: verbatim declaration pinning + a closure sweep over non-test \
         crates/**/*.rs (precision limits: docs/temporal-safety-gate.md)."
    );
    CensusOutcome { ok: true, log }
}

/// The WHY + REPAIR block every cardinality diagnostic ends with. Printed in
/// full rather than cross-referenced: the diagnostic has to teach the class to
/// whoever hit it, including an agent with no memory of this file.
fn why_and_repair(claim: &ScopeClaim) -> String {
    format!(
        "\x20   WHY THIS IS AN OBLIGATION: {}\n\
         \x20   REPAIR: if the new shape still holds EXACTLY ONE enforcer per {:?}, update \
         the claim `{}` in SCOPE_CLAIMS and say why in the rationale. If it holds one PER \
         PANE / PER SESSION / PER ANYTHING SMALLER, that is the defect this obligation \
         exists to stop: the model proves the budget for one instance and the user \
         experiences the sum of all of them.",
        claim.rationale, claim.scope, claim.id
    )
}

/// OB-16: anti-vacuity. A claim that no longer resolves against the tree must
/// go RED, never quietly green — the whole point of pinning.
fn check_liveness(root: &Path, claim: &ScopeClaim, v: &mut Vec<Violation>) {
    let mut fail = |detail: &str, message: String| {
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-16",
            detail: detail.to_string(),
            message,
        });
    };
    if claim.rationale.trim().is_empty() {
        fail(
            "rationale",
            format!(
                "  ✗ FAIL [OB-16] claim `{}` has an EMPTY rationale — the sentence a \
                 reviewer reads IS the claim.",
                claim.id
            ),
        );
    }
    if claim.chain.is_empty() {
        fail(
            "chain",
            format!(
                "  ✗ FAIL [OB-16] claim `{}` pins NO chain link — a claim with an empty \
                 chain constrains nothing.",
                claim.id
            ),
        );
    } else if claim.chain[0].owner != claim.root {
        fail(
            "root",
            format!(
                "  ✗ FAIL [OB-16] claim `{}` declares root `{}` but its chain starts at \
                 `{}` — the chain must begin AT the scope root, or its cardinality is \
                 unproven.",
                claim.id, claim.root, claim.chain[0].owner
            ),
        );
    }
    match claim.machine {
        Some(machine) => {
            if !claim.unmodelled_because.trim().is_empty() {
                fail(
                    "unmodelled_because",
                    format!(
                        "  ✗ FAIL [OB-16] claim `{}` names machine `{machine}` AND carries an \
                         `unmodelled_because` excuse — one or the other, or the excuse rots.",
                        claim.id
                    ),
                );
            }
            let resolved = derive_model_sources(root).iter().any(|path| {
                std::fs::read_to_string(path)
                    .map(|s| mask_comments(&s).contains(&format!("{machine} {{")))
                    .unwrap_or(false)
            });
            if !resolved {
                fail(
                    "machine",
                    format!(
                        "  ✗ FAIL [OB-16] claim `{}` names ty machine `{machine}`, which does \
                         not resolve anywhere in aterm-spec's derived-model catalog \
                         (crates/aterm-spec/src/derive.rs + derive/*.rs). The machine-checked \
                         half of this claim was deleted or renamed: restore it (the census \
                         proves the enforcer does not multiply; the MODEL proves that \
                         not-multiplying is the property that matters).",
                        claim.id
                    ),
                );
            }
        }
        None => {
            if claim.unmodelled_because.trim().is_empty() {
                fail(
                    "unmodelled_because",
                    format!(
                        "  ✗ FAIL [OB-16] claim `{}` has NO ty machine and NO \
                         `unmodelled_because` — an unmodelled safety claim must say why.",
                        claim.id
                    ),
                );
            }
        }
    }
    for r in claim.replicas {
        if r.justification.trim().is_empty() {
            fail(
                r.decl,
                format!(
                    "  ✗ FAIL [OB-16] claim `{}`: replica `{}` in {} has an EMPTY \
                     justification — a second copy of the enforcing state is exactly what \
                     this census exists to question.",
                    claim.id, r.owner, r.file
                ),
            );
        }
    }
    for token in claim.closed_tokens {
        if !claim.chain.iter().any(|l| l.decl.contains(token)) {
            fail(
                token,
                format!(
                    "  ✗ FAIL [OB-16] claim `{}` closes over token `{token}` that no chain \
                     link mentions — the closure and the chain have drifted apart, so the \
                     sweep is guarding a different type from the one that is pinned.",
                    claim.id
                ),
            );
        }
    }
}

/// OB-13: the pinned declaration occurs EXACTLY ONCE in its declared owner.
fn check_link(root: &Path, claim: &ScopeClaim, link: &Link, v: &mut Vec<Violation>) {
    let Ok(source) = std::fs::read_to_string(root.join(link.file)) else {
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-16",
            detail: link.file.to_string(),
            message: format!(
                "  ✗ FAIL [OB-16] claim `{}`: {} is unreadable — a claimed file that moved \
                 leaves the cardinality unguarded.",
                claim.id, link.file
            ),
        });
        return;
    };
    let masked = mask_comments(&source);
    let Some(body) = struct_body(&masked, link.owner) else {
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-16",
            detail: link.owner.to_string(),
            message: format!(
                "  ✗ FAIL [OB-16] claim `{}`: `struct {}` not found in {}. The claim is \
                 STALE: update the chain (and re-argue the cardinality), or the enforcer is \
                 now owned by something this census cannot see.",
                claim.id, link.owner, link.file
            ),
        });
        return;
    };
    let found = body.lines().filter(|l| l.trim() == link.decl).count();
    if found == 1 {
        return;
    }
    v.push(Violation {
        claim: claim.id,
        obligation: "OB-13",
        detail: link.decl.to_string(),
        message: format!(
            "  ✗ FAIL [OB-13] claim `{}` ({:?}-scoped): the pinned declaration\n\
             \x20       {}\n\
             \x20   occurs {found} time(s) in `struct {}` ({}).\n\
             {}",
            claim.id,
            claim.scope,
            link.decl,
            link.owner,
            link.file,
            why_and_repair(claim)
        ),
    });
}

/// The ONE pass over the tree that discharges OB-14 (closure) and OB-17
/// (vocabulary). Returns the explicit-waiver count.
fn sweep(root: &Path, claims: &[ScopeClaim], v: &mut Vec<Violation>) -> usize {
    let closing = claims.iter().any(|c| !c.closed_tokens.is_empty());
    let mut waivers = 0usize;
    for path in sweep_files(root) {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = rel(root, &path);
        // The gate mask is shared; OB-17 reads the doc comments this leaves
        // intact, OB-14 reads the further comment-masked text.
        let gated = mask_gated_items(&raw, UNSHIPPED_GATES);
        waivers += check_vocabulary(claims, &file, &gated, v);
        if !closing {
            continue;
        }
        let masked = mask_comments(&gated);
        let structs = all_struct_bodies(&masked);
        for claim in claims {
            for token in claim.closed_tokens {
                check_closure_fields(claim, &file, &structs, token, v);
                check_closure_ctors(claim, &file, &masked, token, v);
            }
        }
    }
    waivers
}

/// The FIELD half of OB-14. Enumerating struct BODIES (rather than grepping
/// lines) is what keeps enum variants (`DeadlineOwner::WordDecorations`) and
/// fn parameters (`decos: &mut WordDecorations`) out of the closure — a
/// line-regex cannot tell them apart.
fn check_closure_fields(
    claim: &ScopeClaim,
    file: &str,
    structs: &[(String, &str)],
    token: &str,
    v: &mut Vec<Violation>,
) {
    for (owner, body) in structs {
        for line in body.lines() {
            let decl = line.trim();
            let Some(at) = decl.find(token) else { continue };
            if !word_at(decl, at, token) || !decl.contains(':') {
                continue;
            }
            let accounted = claim
                .chain
                .iter()
                .any(|l| l.file == file && l.owner == owner.as_str() && l.decl == decl)
                || claim
                    .replicas
                    .iter()
                    .any(|r| r.file == file && r.owner == owner.as_str() && r.decl == decl);
            if accounted {
                continue;
            }
            v.push(Violation {
                claim: claim.id,
                obligation: "OB-14",
                detail: decl.to_string(),
                message: format!(
                    "  ✗ FAIL [OB-14] claim `{}`: UNACCOUNTED owner of `{token}` —\n\
                     \x20       {file}: struct {owner} {{ {decl} }}\n\
                     \x20   The claim's chain and replicas do not mention it, so this is a \
                     SECOND place the enforcing state can live and nothing bounds how many \
                     of them exist.\n\
                     {}",
                    claim.id,
                    why_and_repair(claim)
                ),
            });
        }
    }
}

/// The CONSTRUCTION half of OB-14: a per-pane engine needs no field at all if
/// something builds one inside a loop.
fn check_closure_ctors(
    claim: &ScopeClaim,
    file: &str,
    masked: &str,
    token: &str,
    v: &mut Vec<Violation>,
) {
    let mut from = 0usize;
    while let Some(rel_at) = masked[from..].find(token) {
        let at = from + rel_at;
        from = at + token.len();
        if !word_at(masked, at, token) {
            continue;
        }
        let tail = &masked[at + token.len()..];
        let trimmed = tail.trim_start();
        // `T::default()` / `T::new(` / `T {` — the three ways this tree builds
        // an engine. A single space before the brace keeps `T| {` (a closure
        // parameter) and `T) {` (a fn signature) out.
        let is_ctor = tail.starts_with("::default()")
            || tail.starts_with("::new(")
            || (trimmed.starts_with('{') && tail.len() - trimmed.len() <= 1);
        if !is_ctor {
            continue;
        }
        // `struct X {`, `impl X {`, `impl Default for X {`, `enum`, `trait`,
        // `union` are DEFINITIONS, not constructions.
        let prev_word = masked[..at]
            .split_whitespace()
            .next_back()
            .unwrap_or_default();
        if matches!(
            prev_word,
            "struct" | "impl" | "for" | "enum" | "trait" | "union"
        ) {
            continue;
        }
        let line_start = masked[..at].rfind('\n').map_or(0, |i| i + 1);
        let line_end = masked[at..].find('\n').map_or(masked.len(), |i| at + i);
        let line = masked[line_start..line_end].trim();
        let owner_fn = enclosing_fn(masked, at);
        let accounted_by_field = claim
            .chain
            .iter()
            .map(|l| (l.file, l.decl))
            .chain(claim.replicas.iter().map(|r| (r.file, r.decl)))
            .any(|(f, decl)| {
                f == file && field_name(decl).is_some_and(|n| line.starts_with(&format!("{n}:")))
            });
        let accounted_by_fn = claim.replicas.iter().any(|r| {
            r.file == file && owner_fn.as_deref() == Some(r.owner) && line.contains(r.decl)
        });
        if accounted_by_field || accounted_by_fn {
            continue;
        }
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-14",
            detail: format!("{file}:{}", line_of(masked, at)),
            message: format!(
                "  ✗ FAIL [OB-14] claim `{}`: UNACCOUNTED construction of `{token}` —\n\
                 \x20       {file}:{} (in fn `{}`)\n\
                 \x20       {line}\n\
                 \x20   Nothing in the claim's chain or replicas owns this instance, so the \
                 enforcer count is whatever this call site's caller decides — which is \
                 exactly the shape a per-pane engine takes.\n\
                 {}",
                claim.id,
                line_of(masked, at),
                owner_fn.as_deref().unwrap_or("<module scope>"),
                why_and_repair(claim)
            ),
        });
    }
}

/// OB-15: shards require an aggregator that reads every one of them.
fn check_aggregation(root: &Path, claim: &ScopeClaim, v: &mut Vec<Violation>) {
    let shards = claim
        .replicas
        .iter()
        .filter(|r| r.kind == ReplicaKind::Shard)
        .count();
    let Some(agg) = &claim.aggregator else {
        if shards > 0 {
            v.push(Violation {
                claim: claim.id,
                obligation: "OB-15",
                detail: "aggregator".to_string(),
                message: format!(
                    "  ✗ FAIL [OB-15] claim `{}` declares {shards} SHARD replica(s) but no \
                     aggregator. Two copies of the enforcing state with no site that reads \
                     both IS the bug this census exists to catch, restated: name the fn \
                     that re-establishes the aggregate, or argue the replica is a separate \
                     SCOPE instead.\n{}",
                    claim.id,
                    why_and_repair(claim)
                ),
            });
        }
        return;
    };
    if shards == 0 {
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-15",
            detail: "aggregator".to_string(),
            message: format!(
                "  ✗ FAIL [OB-15] claim `{}` names aggregator `{}` but declares NO shard \
                 replicas — dead machinery whose failure could never be observed. Remove it \
                 or declare the shard it reconciles.",
                claim.id, agg.symbol
            ),
        });
        return;
    }
    let Ok(source) = std::fs::read_to_string(root.join(agg.file)) else {
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-16",
            detail: agg.file.to_string(),
            message: format!(
                "  ✗ FAIL [OB-16] claim `{}`: aggregator file {} is unreadable.",
                claim.id, agg.file
            ),
        });
        return;
    };
    let masked = mask_comments(&source);
    let Some(body) = fn_body(&masked, agg.symbol) else {
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-16",
            detail: agg.symbol.to_string(),
            message: format!(
                "  ✗ FAIL [OB-16] claim `{}`: aggregator `fn {}` not found in {} — STALE; \
                 re-audit which fn re-establishes the aggregate now.",
                claim.id, agg.symbol, agg.file
            ),
        });
        return;
    };
    for path in agg.reads {
        if body.contains(path) {
            continue;
        }
        v.push(Violation {
            claim: claim.id,
            obligation: "OB-15",
            detail: (*path).to_string(),
            message: format!(
                "  ✗ FAIL [OB-15] claim `{}`: aggregator `{}` ({}) never reads `{path}`, so \
                 the aggregate is computed over ONE shard while the property is claimed over \
                 all of them.\n{}",
                claim.id,
                agg.symbol,
                agg.file,
                why_and_repair(claim)
            ),
        });
    }
}

/// OB-17: a reserved scope phrase in a `///` block is a CLAIM. Returns the
/// number of explicit waivers seen, so the exception set is visible in the
/// GREEN line rather than silent.
fn check_vocabulary(
    claims: &[ScopeClaim],
    file: &str,
    gated: &str,
    v: &mut Vec<Violation>,
) -> usize {
    let mut waivers = 0usize;
    if claims.iter().any(|c| c.covers_prose_in.contains(&file)) {
        return waivers;
    }
    for (start, block) in doc_blocks(gated) {
        let lower = block.to_lowercase();
        let Some(phrase) = RESERVED_SCOPE_PHRASES.iter().find(|p| lower.contains(**p)) else {
            continue;
        };
        if has_waiver_directive(&block) {
            waivers += 1;
            continue;
        }
        v.push(Violation {
            claim: "",
            obligation: "OB-17",
            detail: format!("{file}:{start}"),
            message: format!(
                "  ✗ FAIL [OB-17] UNDECLARED SCOPE CLAIM — `{phrase}` in the doc block at \
                     {file}:{start} asserts a scope WIDER than the declaration it sits on, and \
                     nothing checks it.\n\
                     \x20   THE LESSON THIS ENCODES: the only thing standing between a \
                     photosensitive user and 2N flashes per second was a human reading the \
                     words \"window-wide\" in a doc comment. Prose is not a check.\n\
                     \x20   REPAIR (pick one): (a) register a ScopeClaim in \
                     crates/aterm-census/src/scope_census.rs pinning the ownership chain, and \
                     add this file to its `covers_prose_in`; or (b) if the phrase describes \
                     something no instance count can falsify (a config value, a derived \
                     scalar, a namespace tag), OPEN a line of the SAME doc block with \
                     `{SCOPE_WAIVER_MARKER} <reason>` — a directive, not a mention; the \
                     gate counts and reports every waiver."
            ),
        });
    }
    waivers
}

/// Does this doc block TAKE the OB-17 waiver, as opposed to merely talking
/// about it?
///
/// The marker must OPEN a doc line (`/// scope-waiver: <reason>`). A
/// `contains` test over the whole block exempts any block that names the
/// marker in passing — and one in this tree did exactly that: `kitty_pet.rs`'s
/// flight-scalar doc explained that it was deliberately NOT taking the waiver,
/// spelled the marker to say so, and was silently waived for it. A waiver
/// channel that can be opened by accident is the "registry rots into a silent
/// exemption" failure this census exists to refuse. Every real waiver in the
/// tree is already written as its own directive line, so the tightening costs
/// them nothing.
fn has_waiver_directive(block: &str) -> bool {
    block.lines().any(|line| {
        line.trim_start()
            .strip_prefix("///")
            .map(str::trim_start)
            .is_some_and(|text| text.to_lowercase().starts_with(SCOPE_WAIVER_MARKER))
    })
}

/// Every maximal run of `///` lines in a source, as `(1-based start line,
/// text)`. `//!` module prose is deliberately out of scope: it documents a
/// module, whose cardinality no declaration owns.
fn doc_blocks(source: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (i, line) in source.lines().enumerate() {
        if line.trim_start().starts_with("///") {
            match &mut current {
                Some((_, text)) => {
                    text.push('\n');
                    text.push_str(line.trim_start());
                }
                None => current = Some((i + 1, line.trim_start().to_string())),
            }
        } else if let Some(block) = current.take() {
            out.push(block);
        }
    }
    if let Some(block) = current {
        out.push(block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The real aterm checkout this crate lives in. The centrepiece test is a
    /// MUTATION test against the REAL claim files (copied, then edited), not a
    /// synthetic lookalike: a gate demonstrated only on a fixture proves
    /// nothing about the tree it is supposed to guard.
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-census lives two levels under the aterm root")
            .to_path_buf()
    }

    fn claim_by_id(id: &str) -> &'static ScopeClaim {
        SCOPE_CLAIMS
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no claim `{id}` in SCOPE_CLAIMS"))
    }

    /// Every repo-relative file a claim names, plus the whole derived-model
    /// catalog (OB-16 resolves the `machine` across `derive.rs` + `derive/*.rs`,
    /// so a fixture root missing the split modules would fail OB-16 for a
    /// reason the fixture is not testing).
    fn claim_files(claim: &ScopeClaim) -> Vec<String> {
        let repo = repo_root();
        let mut files: Vec<String> = derive_model_sources(&repo)
            .iter()
            .filter_map(|path| path.strip_prefix(&repo).ok())
            .map(|rel| rel.to_string_lossy().into_owned())
            .collect();
        files.extend(claim.chain.iter().map(|l| l.file.to_string()));
        files.extend(claim.replicas.iter().map(|r| r.file.to_string()));
        files.extend(claim.aggregator.iter().map(|a| a.file.to_string()));
        files.extend(claim.covers_prose_in.iter().map(|f| (*f).to_string()));
        files.sort();
        files.dedup();
        files
    }

    /// Copy `files` out of the live checkout into a fresh temp root, preserving
    /// repo-relative paths. The caller mutates and removes it.
    fn copy_root(name: &str, files: &[String]) -> PathBuf {
        let src = repo_root();
        let root =
            std::env::temp_dir().join(format!("aterm-scope-census-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for rel in files {
            let from = src.join(rel);
            let to = root.join(rel);
            std::fs::create_dir_all(to.parent().expect("rel has a parent")).expect("mkdir");
            let text = std::fs::read_to_string(&from)
                .unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
            std::fs::write(&to, text).expect("write copy");
        }
        root
    }

    fn write_file(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write file");
    }

    /// Apply a mutation to a copied file, asserting it actually applied.
    /// Without this guard a stale `from` would make the whole demonstration
    /// vacuous: the census would go green on an unmutated tree and the test
    /// would still pass.
    fn mutate(root: &Path, rel: &str, from: &str, to: &str) {
        let path = root.join(rel);
        let before = std::fs::read_to_string(&path).expect("read for mutation");
        let after = before.replace(from, to);
        assert_ne!(
            before, after,
            "the mutation no longer applies to {rel} (looking for `{from}`) — the pinned \
             text is stale, so this demonstration proves nothing"
        );
        std::fs::write(&path, after).expect("write mutation");
    }

    fn run_one(root: &Path, claim: &'static ScopeClaim) -> CensusOutcome {
        run_scope_census_over(root, std::slice::from_ref(claim), &[])
    }

    /// The union of every claim's files — used where the WHOLE registry has to
    /// run (the vocabulary lock is registry-wide, so a partial registry over a
    /// tree containing another claim's prose would fail for the wrong reason).
    fn all_claim_files() -> Vec<String> {
        let mut files: Vec<String> = SCOPE_CLAIMS.iter().flat_map(claim_files).collect();
        files.sort();
        files.dedup();
        files
    }

    /// THE CENTREPIECE. The proposed refactor — every split pane gets its own
    /// `WordDecorations`, hence its own limiter — applied VERBATIM to a copy of
    /// the real `crates/aterm-gui/src/lib.rs`. GREEN before, RED after.
    #[test]
    fn a_per_pane_word_decorations_map_fails_the_flash_limiter_chain() {
        let claim = claim_by_id("flash-limiter");
        let root = copy_root("per-pane-map", &claim_files(claim));

        let before = run_one(&root, claim);
        assert!(
            before.ok,
            "the unmutated real claim files must be GREEN, or the RED below proves \
             nothing:\n{}",
            before.log
        );

        mutate(
            &root,
            "crates/aterm-gui/src/lib.rs",
            "    word_decos: crate::word_decorations::WordDecorations,\n",
            "    word_decos: std::collections::HashMap<u64, \
             crate::word_decorations::WordDecorations>,\n",
        );
        let after = run_one(&root, claim);
        assert!(
            !after.ok,
            "a per-pane WordDecorations map MUST fail the flash-limiter chain:\n{}",
            after.log
        );
        assert!(
            after.log.contains("[OB-13] claim `flash-limiter`"),
            "the failure must be attributed to the pinned chain link:\n{}",
            after.log
        );
        assert!(
            after.log.contains("WCAG 2.3.1"),
            "the diagnostic must carry the safety argument, not just a diff:\n{}",
            after.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The chain is UNTOUCHED, so OB-13 alone would miss this: a brand-new
    /// per-pane owner of the engine.
    #[test]
    fn a_new_per_pane_owner_fails_the_closure() {
        let claim = claim_by_id("flash-limiter");
        let root = copy_root("per-pane-owner", &claim_files(claim));
        assert!(run_one(&root, claim).ok);

        write_file(
            &root,
            "crates/aterm-gui/src/pane_state.rs",
            "use aterm_effects::word_decorations::WordDecorations;\n\
             \n\
             pub(crate) struct PaneState {\n\
             \x20   pub(crate) decos: WordDecorations,\n\
             }\n",
        );
        let out = run_one(&root, claim);
        assert!(
            !out.ok,
            "a new per-pane owner of the enforcing state must open the closure:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-14]") && out.log.contains("PaneState"),
            "OB-14 must name the unaccounted owner:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No field at all: a per-pane engine built inside a loop. OB-13 and the
    /// field half of OB-14 both see nothing.
    #[test]
    fn a_per_pane_constructor_fails_the_closure() {
        let claim = claim_by_id("flash-limiter");
        let root = copy_root("per-pane-ctor", &claim_files(claim));
        assert!(run_one(&root, claim).ok);

        write_file(
            &root,
            "crates/aterm-gui/src/pane_engines.rs",
            "use aterm_effects::word_decorations::WordDecorations;\n\
             \n\
             pub(crate) fn build_pane_engines(panes: usize) -> Vec<WordDecorations> {\n\
             \x20   let mut out = Vec::new();\n\
             \x20   for _pane in 0..panes {\n\
             \x20       out.push(WordDecorations::default());\n\
             \x20   }\n\
             \x20   out\n\
             }\n",
        );
        let out = run_one(&root, claim);
        assert!(
            !out.ok,
            "a per-pane constructor must open the closure even with no field:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-14]") && out.log.contains("build_pane_engines"),
            "OB-14 must name the constructing fn:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity in both directions: a claim that stops matching the tree
    /// goes RED, it never quietly passes. This is the per-claim form of the
    /// `wasm_clock_safety` / `hdr_gate` discipline.
    #[test]
    fn a_renamed_field_or_owner_fails_stale_rather_than_silently_passing() {
        let claim = claim_by_id("flash-limiter");

        let root = copy_root("renamed-field", &claim_files(claim));
        mutate(
            &root,
            "crates/aterm-gui/src/lib.rs",
            "    word_decos: crate::word_decorations::WordDecorations,\n",
            "    decos: crate::word_decorations::WordDecorations,\n",
        );
        let out = run_one(&root, claim);
        assert!(!out.ok, "a renamed claimed field must go RED:\n{}", out.log);
        assert!(
            out.log.contains("occurs 0 time(s)"),
            "the diagnostic must say the pinned declaration vanished:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);

        let root = copy_root("renamed-owner", &claim_files(claim));
        mutate(
            &root,
            "crates/aterm-gui/src/lib.rs",
            "struct WindowState {",
            "struct WindowShell {",
        );
        let out = run_one(&root, claim);
        assert!(!out.ok, "a renamed claimed owner must go RED:\n{}", out.log);
        assert!(
            out.log.contains("[OB-16]") && out.log.contains("STALE"),
            "the diagnostic must say the claim went stale:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// OB-17 both ways on a synthetic tree (no claims at all, so ONLY the
    /// vocabulary lock runs).
    #[test]
    fn an_undeclared_window_wide_doc_claim_fails_the_vocabulary_lock() {
        let root = std::env::temp_dir().join(format!("aterm-scope-vocab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_file(
            &root,
            "crates/aterm-thing/src/lib.rs",
            "/// The blink budget: at most two per second, window-wide.\n\
             pub const BLINK_BUDGET: u32 = 2;\n",
        );
        let out = run_scope_census_over(&root, &[], &[]);
        assert!(
            !out.ok && out.log.contains("[OB-17]"),
            "an undeclared window-wide doc claim must fail the vocabulary lock:\n{}",
            out.log
        );

        write_file(
            &root,
            "crates/aterm-thing/src/lib.rs",
            "/// The blink budget: at most two per second, window-wide.\n\
             ///\n\
             /// scope-waiver: a constant cannot multiply.\n\
             pub const BLINK_BUDGET: u32 = 2;\n",
        );
        let out = run_scope_census_over(&root, &[], &[]);
        assert!(
            out.ok
                && out
                    .log
                    .contains("1 reserved-vocabulary doc block(s) waived"),
            "an explicit waiver must pass AND be counted in the verdict:\n{}",
            out.log
        );

        // A MENTION is not a directive. This is the shape that was silently
        // waived in kitty_pet.rs: a block that names the marker to explain why
        // it is NOT taking the waiver. `contains` over the block let it
        // through; the line-leading rule does not.
        write_file(
            &root,
            "crates/aterm-thing/src/lib.rs",
            "/// The blink budget: at most two per second, window-wide.\n\
             ///\n\
             /// Stated plainly rather than asserting the scope and excusing it\n\
             /// with a `scope-waiver:` note.\n\
             pub const BLINK_BUDGET: u32 = 2;\n",
        );
        let out = run_scope_census_over(&root, &[], &[]);
        assert!(
            !out.ok && out.log.contains("[OB-17]"),
            "naming `scope-waiver:` mid-sentence must NOT open the escape hatch — a \
             waiver channel that can be taken by accident is a silent exemption:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// FALSE-POSITIVE CALIBRATION, pinned against the real shapes: an enum
    /// VARIANT named after the type, a `Self::`-qualified variant, an `impl`
    /// block, and a `&mut` fn PARAMETER are not owners of the enforcing state.
    /// A line-regex closure would flag every one of them.
    #[test]
    fn enum_variants_and_fn_parameters_are_not_owners() {
        let claim = claim_by_id("flash-limiter");
        let mut files = claim_files(claim);
        for extra in [
            "crates/aterm-gui/src/metrics.rs",
            "crates/aterm-gui/src/motion.rs",
        ] {
            files.push(extra.to_string());
        }
        let root = copy_root("false-positives", &files);

        // Pin the calibration: these shapes must really be present, or the
        // GREEN below is testing nothing.
        let metrics = std::fs::read_to_string(root.join("crates/aterm-gui/src/metrics.rs"))
            .expect("read metrics.rs");
        assert!(
            metrics.contains("WordDecorations = 21,"),
            "enum variant gone"
        );
        let motion = std::fs::read_to_string(root.join("crates/aterm-gui/src/motion.rs"))
            .expect("read motion.rs");
        assert!(
            motion.contains("SeriousEffect::WordDecorations"),
            "qualified variant gone"
        );
        let render = std::fs::read_to_string(root.join("crates/aterm-gui/src/app_render.rs"))
            .expect("read app_render.rs");
        assert!(
            render.contains("decos: &mut aterm_effects::word_decorations::WordDecorations,"),
            "fn parameter gone"
        );
        let engine =
            std::fs::read_to_string(root.join("crates/aterm-effects/src/word_decorations.rs"))
                .expect("read word_decorations.rs");
        assert!(engine.contains("impl WordDecorations {"), "impl block gone");

        let out = run_one(&root, claim);
        assert!(
            out.ok,
            "variants, qualified variants, impl blocks and fn parameters must NOT count as \
             owners of the enforcing state:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// OB-15 both ways, post-celebration: the repaired aggregator passes clean
    /// on the real tree, and REGRESSING it (dropping the parked-shard chain)
    /// goes RED immediately — with no standing finding left to excuse it.
    #[test]
    fn the_supernova_aggregator_regression_goes_red() {
        let root = copy_root("supernova-standing", &all_claim_files());

        let out = run_scope_census_over(&root, SCOPE_CLAIMS, SCOPE_STANDING_FINDINGS);
        assert!(
            out.ok,
            "the repaired aggregator must pass with zero findings:\n{}",
            out.log
        );
        assert!(
            !out.log.contains("STANDING FINDING"),
            "no standing finding may remain registered for a repaired scan:\n{}",
            out.log
        );

        // Regress the aggregator: drop the parked-shard contribution from the
        // scan. The aggregator no longer CHAINS every parked shard's whole
        // episode map onto its live walk (that was `O(panes × total episodes)`
        // per presented frame); each shard now carries an O(1) burst-mutex
        // summary re-derived at its one choke point, and the aggregator folds
        // those summaries in. The obligation is unchanged — the aggregate must
        // still be computed over ALL shards, not just the bound one — so the
        // demonstration deletes the fold, which is exactly today's spelling of
        // "this scan sees one pane only".
        mutate(
            &root,
            "crates/aterm-effects/src/word_decorations.rs",
            "        for p in self.parked.values() {",
            "        for p in std::iter::empty::<&ParkedPane>() {",
        );
        let out = run_scope_census_over(&root, SCOPE_CLAIMS, SCOPE_STANDING_FINDINGS);
        assert!(
            !out.ok,
            "a per-bound-pane scan must fail OB-15 outright now:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-15]") && out.log.contains("self.parked"),
            "the failure must name the missing shard read:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The live-root smoke test: the shipping registry over the real checkout.
    #[test]
    fn real_tree_scope_census_is_green_with_no_standing_findings() {
        let out = run_scope_census(&repo_root());
        assert!(out.ok, "the live tree must be GREEN:\n{}", out.log);
        assert!(
            !out.log.contains("STANDING FINDING"),
            "the supernova aggregator finding closed on 2026-08-08; nothing may reprint:\n{}",
            out.log
        );
    }
}
