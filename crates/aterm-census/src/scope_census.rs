// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scope-cardinality census (OB-13..OB-18): the "one enforcer, N instances"
//! class as a fail-closed, build-blocking obligation.
//!
//! A model can verify a local property of one instance of an enforcing
//! structure while the property that matters is global across all live
//! instances. `FlashLimiter` proves at most 2 ignitions per rolling second for
//! one limiter; if every split pane had its own `WordDecorations`, each limiter
//! would satisfy that while the viewer sees 2N flashes per second. WCAG 2.3.1
//! is window-wide.
//!
//! A [`ScopeClaim`] declares the ownership chain from the scope root down to the
//! enforcing state, plus the closure of every other place that state may live.
//! The census re-derives both from the tree on every build, so multiplying an
//! enforcer either re-types a pinned link (OB-13) or opens the closure (OB-14).
//!
//! Obligations (`[OB-n]` tags in the diagnostics):
//!
//! * OB-13 chain pinned: each link's field declaration occurs exactly once,
//!   verbatim, in the declared owner's struct body.
//! * OB-14 closure: every non-test struct field and construction site of a
//!   claim's `closed_tokens` type is accounted for by its `chain` or
//!   `replicas`.
//! * OB-15 aggregation: a claim with shard replicas names an `aggregator` whose
//!   body reads every declared access path.
//! * OB-16 liveness: files exist, owners resolve, the `machine` (if declared)
//!   resolves in aterm-spec's derive catalog, prose fields are non-empty,
//!   `chain[0].owner == root`, and every closed token is mentioned by the chain.
//! * OB-17 vocabulary lock: a [`RESERVED_SCOPE_PHRASES`] phrase in a `///` doc
//!   block requires the file to be in some claim's `covers_prose_in`, or a
//!   `scope-waiver: <reason>` line in the block.
//! * OB-18 standing coherence: every registered standing finding is re-detected
//!   this run and carries a written finding.
//!
//! Only OB-17 has a waiver channel. Consumers: the `aterm-census` bin
//! (`targo --unverified run -p aterm-census -- [<root>] --scope`) and
//! `tools/freeze-safety-gate/build.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::lock_order::{mask_gated_items, mask_literals};
use crate::{CensusOutcome, collect_rs_files, is_test_file, strip_line_comment};

// The claim vocabulary.

/// The physical thing the safety budget belongs to. Declared, not derived; see
/// [`SCOPE_PRECISION_NOTE`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// One per OS window. Two windows are two enforcers on purpose.
    Window,
}

/// One link of the ownership chain from the scope root down to the enforcing
/// state.
///
/// `decl` is the field declaration verbatim (trimmed). Pinning the spelling
/// means a map, `Vec`, alias or newtype all read as a change.
struct Link {
    file: &'static str,
    owner: &'static str,
    decl: &'static str,
}

/// Why a second copy of the enforcing state is sound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReplicaKind {
    /// A different scope instance (another process, another surface). Its
    /// budget is its own; nothing has to reconcile the two.
    SeparateScope,
    /// The same scope's state, split across containers. The aggregate property
    /// is only real if some site reads every shard — hence [`Aggregator`].
    Shard,
}

/// A place the enforcing state legitimately lives outside the primary chain,
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
/// whenever a claim has [`ReplicaKind::Shard`] replicas.
struct Aggregator {
    symbol: &'static str,
    file: &'static str,
    /// The access path each shard must appear as inside that fn's body.
    reads: &'static [&'static str],
}

/// A registered standing finding: a known, deliberately unfixed violation,
/// reported and re-detected every run (OB-18). A finding that stops
/// reproducing is stale and red until deleted.
struct StandingScopeFinding {
    claim: &'static str,
    obligation: &'static str,
    /// The exact subject the violation must name (its [`Violation`] `detail`),
    /// so a different violation of the same obligation cannot hide here.
    detail: &'static str,
    finding: &'static str,
}

/// A global-scope safety claim: a doc-comment assertion turned into an
/// obligation the census re-derives from the tree on every build.
struct ScopeClaim {
    /// Stable id; appears in diagnostics and in standing-finding cross-refs.
    id: &'static str,
    /// The `ty` machine whose invariants are the safety argument. `Some` keeps
    /// the model required (OB-16); `None` requires `unmodelled_because`.
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

// The registry (the census's fail-closed ground truth).

/// Why a web host's pipeline is its own scope, not a second enforcer.
const WEB_PIPELINE_SCOPE: &str = "The web page's pipeline runs in a DIFFERENT PROCESS \
    driving its own browser surface — its window, its budget (the same limiter is \
    documented at aterm-wasm/src/effects_api.rs). It never shares a retina with a \
    native WindowState, so it is a second SCOPE, not a second enforcer inside one scope.";

/// Why a demo binary's pipeline is its own scope.
const DEMO_PIPELINE_SCOPE: &str = "A standalone example binary (not shipped) that drives \
    one pipeline over its own offscreen surface: its own process and its own frames, \
    never a native WindowState's retina — a second SCOPE, not a second enforcer.";

const SCOPE_CLAIMS: &[ScopeClaim] = &[
    ScopeClaim {
        id: "flash-limiter",
        machine: Some("FlashLimiterWindow"),
        unmodelled_because: "",
        scope: Scope::Window,
        root: "WindowState",
        // The window owns one `EffectsPipeline`, which owns the one
        // `WordDecorations`. Both types are closed: a second pipeline anywhere
        // is a second limiter.
        chain: &[
            Link {
                file: "crates/aterm-gui/src/lib.rs",
                owner: "WindowState",
                decl: "fx: aterm_effects::pipeline::EffectsPipeline,",
            },
            Link {
                file: "crates/aterm-effects/src/pipeline.rs",
                owner: "EffectsPipeline",
                decl: "decos: WordDecorations,",
            },
            Link {
                file: "crates/aterm-effects/src/word_decorations.rs",
                owner: "WordDecorations",
                decl: "ignitions: Vec<IgnitionReservation>,",
            },
        ],
        closed_tokens: &["WordDecorations", "EffectsPipeline"],
        replicas: &[
            Replica {
                file: "crates/aterm-wasm/src/lib.rs",
                owner: "AtermTerminal",
                decl: "pub(crate) effects: EffectsPipeline,",
                kind: ReplicaKind::SeparateScope,
                justification: WEB_PIPELINE_SCOPE,
            },
            Replica {
                file: "crates/aterm-gpu-web/src/lib.rs",
                owner: "AtermGpuTerminal",
                decl: "pub(crate) effects: EffectsPipeline,",
                kind: ReplicaKind::SeparateScope,
                justification: WEB_PIPELINE_SCOPE,
            },
            Replica {
                file: "crates/aterm-effects/examples/wasm_host_echo.rs",
                owner: "Page",
                decl: "p: EffectsPipeline,",
                kind: ReplicaKind::SeparateScope,
                justification: DEMO_PIPELINE_SCOPE,
            },
            Replica {
                file: "crates/aterm-effects/examples/wasm_host_echo.rs",
                owner: "boot",
                decl: "EffectsPipeline::new()",
                kind: ReplicaKind::SeparateScope,
                justification: DEMO_PIPELINE_SCOPE,
            },
            Replica {
                file: "crates/aterm-render/examples/cursor_magic_demo.rs",
                owner: "render_literal_rain",
                decl: "EffectsPipeline::new()",
                kind: ReplicaKind::SeparateScope,
                justification: DEMO_PIPELINE_SCOPE,
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
        // Every replica is a separate scope, not a shard, so OB-15 requires
        // (and permits) no aggregator.
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
            // `max_streaks`' doc names this limiter to disclaim it: the streak
            // engine never reserves against `FlashLimiterWindow`.
            "crates/aterm-gui/src/app_config.rs",
            // `OutputStreak`'s doc names this claim's window-wide budget to
            // disclaim it (the streak engine never reserves against
            // `FlashLimiterWindow`). Removing this line makes OB-17 red.
            "crates/aterm-effects/src/output_streak.rs",
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
        machine: Some("SupernovaBurstMutex"),
        unmodelled_because: "",
        scope: Scope::Window,
        root: "WindowState",
        chain: &[
            Link {
                file: "crates/aterm-gui/src/lib.rs",
                owner: "WindowState",
                decl: "fx: aterm_effects::pipeline::EffectsPipeline,",
            },
            Link {
                file: "crates/aterm-effects/src/pipeline.rs",
                owner: "EffectsPipeline",
                decl: "decos: WordDecorations,",
            },
            Link {
                file: "crates/aterm-effects/src/word_decorations.rs",
                owner: "WordDecorations",
                decl: "persist: FxHashMap<u64, Episode>,",
            },
        ],
        // The leaf type `HashMap<u64, Episode>` is too common to close over;
        // this claim relies on OB-13 (the chain) and OB-15 (the aggregator).
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
            DECORATION share of the shared `nova_add` channel inside MAX_NOVA_QUADS = 1536 \
            WITHOUT the funding clamp ever binding. Two live supernovae make that share \
            2 x 900 = 1800, or 3 x 392 + 900 = 2076 mixed, so \
            `MAX_NOVA_QUADS.saturating_sub(nova.len())` would start TRUNCATING a live \
            effect mid-emission — and the const-assert derivation in supernova.rs is \
            falsified SILENTLY, because that assert is over CONSTANTS and can never \
            observe a second instance. 1536 is a FUNDING ceiling, not a channel total: \
            PRISM WAKE (aterm-effects/src/output_streak.rs) is a second producer whose \
            per-pane quads the host appends AFTER this pass has finished spending, so they \
            are neither funded by the budget nor counted by it. That overrun is measured, \
            and shown to truncate nothing and allocate nothing per frame, in \
            aterm-effects/tests/nova_channel_budget.rs. `SupernovaBurstMutex` is the \
            machine-checked form of the instance count; its Tier-1 bind to the real \
            grants is TEST-ONLY (aterm-effects' word_decorations test \
            `real_burst_grants_conform_to_the_supernova_mutex_model`), with no \
            `#[refines]` anchor on `super_prepass`/`nova_prepass`.",
    },
];

/// Registered standing findings, re-detected and reprinted every run (OB-18).
/// Empty; the regression test below keeps the check honest.
const SCOPE_STANDING_FINDINGS: &[StandingScopeFinding] = &[];

/// Phrases that assert a scope wider than the declaration they sit on; using
/// one is a claim, and a claim is an obligation.
///
/// "process-wide" is absent: a `static` / `OnceLock` is its own cardinality
/// proof. The risky claims are scoped to containers the type system does not
/// make unique.
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

/// The OB-17 escape hatch. It must open a line of the doc block
/// (`/// scope-waiver: <reason>`); see [`has_waiver_directive`].
const SCOPE_WAIVER_MARKER: &str = "scope-waiver:";

/// Sources this census never reads: its own registry names every token and
/// reserved phrase.
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
        field is not. A construction is `T {`, `T::new(`, `T::default(`, or
        `T::f(` for any `f` an `impl T` block in the tree returns `Self` from;
        one reached through a free fn or a `.clone()` is not seen.
      - THE SCOPE ROOT IS DECLARED, NOT DERIVED. The census enforces `exactly
        one enforcer per WindowState`; it cannot know whether WindowState is
        the physically right scope (WCAG's real scope is a retina, which spans
        overlapping windows). Its contribution is turning an unstated
        assumption into a stated, diffable one.
      - PROSE COVERAGE IS PER FILE. `covers_prose_in` discharges EVERY reserved
        phrase in a file, not the specific doc block — coarse on purpose, so
        the migration is a few decisions rather than a few hundred. OB-17 also
        only sees `///` blocks: `//!` module prose and undocumented enforcers
        are out of its reach by construction. And it is POLARITY-BLIND: a
        reserved phrase fires whether the sentence asserts that scope or
        names it to DISCLAIM it, so correct disclaiming prose still needs its
        file listed (output_streak.rs is listed for exactly that).
      - IT PROVES THE ENFORCER DOES NOT MULTIPLY, NOT THAT NOT-MULTIPLYING IS
        SUFFICIENT. Whether the aggregator's scan is CORRECT over every shard
        is a model obligation (the `machine` field), checked elsewhere.
      - SCOPE OF THE SWEEPS: non-test `crates/**/*.rs`, with #[cfg(test)] /
        #[cfg(kani)] items blanked and crates/aterm-census/src excluded (the
        registry itself names every token and phrase).
";

// Source primitives.

/// Every file OB-16 resolves a claim's `machine` against: aterm-spec's
/// `derive.rs` plus each `derive/*.rs` family module, enumerated so a split or
/// merge cannot take a machine out of reach.
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

/// Blank comments and string literals, preserving line count and offsets, so
/// a token in a doc comment or string is never read as a declaration. OB-17
/// reads the unmasked text, since doc comments are its subject.
fn mask_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let literals_masked = mask_literals(line);
        out.push_str(strip_line_comment(&literals_masked));
        out.push('\n');
    }
    out
}

/// The body of `struct <owner> {` … matching `}` in masked `source`. `None`
/// when the struct is absent (OB-16's stale-claim signal).
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

/// The name of the `fn` a byte offset sits inside (the nearest preceding
/// `fn <ident>(`). A constructor inside a closure attributes to its enclosing
/// function.
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

/// Every non-test source under `root/crates` (examples included), minus this
/// census's own sources.
fn sweep_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);
    files.retain(|p| !is_test_file(p) && !p.to_string_lossy().contains(SELF_EXCLUDED_DIR));
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

// The census.

/// One obligation violation, before the standing-finding registry decides
/// whether it is a build-blocking failure or a reported hazard.
struct Violation {
    claim: &'static str,
    obligation: &'static str,
    /// The exact subject, matched against [`StandingScopeFinding::detail`].
    detail: String,
    message: String,
}

/// The scope-cardinality census over the live registry. See the module doc.
#[must_use]
pub fn run_scope_census(root: &Path) -> CensusOutcome {
    run_scope_census_over(root, SCOPE_CLAIMS, SCOPE_STANDING_FINDINGS)
}

/// The census body, parameterised by registry so tests can drive one claim over
/// a copied-and-mutated real tree.
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
    // OB-14 and OB-17 both need every non-test source; read the tree once.
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
    let mut standing_blocks: Vec<String> = Vec::new();
    for (f, viol) in &standing {
        let block = format!(
            "  ! STANDING FINDING [{}] claim `{}` (subject `{}`) — REGISTERED, real, \
             deliberately unfixed; re-detected this run and reprinted in full (never a \
             waiver):\n\x20     {}\n\x20   detected as: {}",
            f.obligation,
            f.claim,
            f.detail,
            f.finding,
            viol.message.trim_start()
        );
        let _ = writeln!(log, "{block}");
        standing_blocks.push(block);
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
            "scope-cardinality census: FAILED — {failures} obligation violation(s) across {} \
             claim(s). This census blocks BOTH `targo --unverified run -p aterm-census -- --scope` \
             and the build of tools/freeze-safety-gate. OB-13/14/15/16/18 have NO waiver \
             channel.",
            claims.len()
        );
        return CensusOutcome::red(log);
    }
    let _ = writeln!(
        log,
        "scope-cardinality census: GREEN — {} scope claim(s) re-derived from the tree ({} \
         pinned chain link(s), {} accounted replica(s), {} closed token(s)); {} standing \
         finding(s); {} reserved-vocabulary doc block(s) waived explicitly.",
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
    CensusOutcome {
        ok: true,
        log,
        standing: standing_blocks,
    }
}

/// The why + repair block every cardinality diagnostic ends with, printed in
/// full so the diagnostic explains the class on its own.
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

/// OB-16: anti-vacuity. A claim that no longer resolves against the tree is
/// red.
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

/// OB-13: the pinned declaration occurs exactly once in its declared owner.
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

/// The one pass over the tree that discharges OB-14 (closure) and OB-17
/// (vocabulary). Returns the explicit-waiver count.
fn sweep(root: &Path, claims: &[ScopeClaim], v: &mut Vec<Violation>) -> usize {
    let closing = claims.iter().any(|c| !c.closed_tokens.is_empty());
    let files = sweep_files(root);
    let ctors = if closing {
        closed_token_ctors(&files, claims)
    } else {
        BTreeMap::new()
    };
    let no_ctors = BTreeSet::new();
    let mut waivers = 0usize;
    for path in files {
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
                let named = ctors.get(token).unwrap_or(&no_ctors);
                check_closure_ctors(claim, &file, &masked, token, named, v);
            }
        }
    }
    waivers
}

/// The field half of OB-14. Enumerating struct bodies keeps enum variants
/// (`DeadlineOwner::WordDecorations`) and fn parameters
/// (`decos: &mut WordDecorations`) out of the closure.
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

/// Whether the text before a type token ends in a fn signature's return
/// arrow, optionally through a reference (`-> T`, `-> &T`, `-> &mut T`,
/// `-> &'a mut T`).
fn returns_token(before: &str) -> bool {
    let mut rest = before.trim_end();
    if let Some(r) = rest.strip_suffix("mut")
        && r.ends_with(|c: char| c.is_whitespace() || c == '&')
    {
        rest = r.trim_end();
    }
    if let Some(q) = rest.rfind('\'')
        && !rest[q + 1..].is_empty()
        && rest[q + 1..]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_')
    {
        rest = rest[..q].trim_end();
    }
    if let Some(r) = rest.strip_suffix('&') {
        rest = r.trim_end();
    }
    rest.ends_with("->")
}

/// Every closed token's named constructors: associated fns of its `impl`
/// blocks whose return type is the token (`-> Self`, `-> Option<Self>`, …),
/// e.g. `EffectsPipeline::for_window`.
fn closed_token_ctors(
    files: &[std::path::PathBuf],
    claims: &[ScopeClaim],
) -> BTreeMap<&'static str, BTreeSet<String>> {
    let tokens: BTreeSet<&'static str> = claims
        .iter()
        .flat_map(|c| c.closed_tokens.iter().copied())
        .collect();
    let mut out: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    for path in files {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        if !raw.contains("impl") || !tokens.iter().any(|t| raw.contains(t)) {
            continue;
        }
        let masked = mask_comments(&mask_gated_items(&raw, UNSHIPPED_GATES));
        for &token in &tokens {
            for body in impl_bodies(&masked, token) {
                out.entry(token)
                    .or_default()
                    .extend(self_returning_fns(body, token));
            }
        }
    }
    out
}

/// The bodies of every `impl … <token> {` (inherent or trait) in a masked
/// source: the self type is what follows `for`, else what follows `impl` and
/// its generics, with any path and generic arguments stripped.
fn impl_bodies<'a>(masked: &'a str, token: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = masked[from..].find("impl") {
        let at = from + rel;
        from = at + "impl".len();
        if !word_at(masked, at, "impl") {
            continue;
        }
        let Some(open) = masked[from..].find(['{', ';']).map(|i| from + i) else {
            break;
        };
        if masked.as_bytes()[open] != b'{' {
            continue;
        }
        let mut header = masked[from..open].trim();
        if let Some(where_at) = header.find(" where ") {
            header = header[..where_at].trim();
        }
        let self_ty = match header.rfind(" for ") {
            Some(f) => header[f + " for ".len()..].trim(),
            None if header.starts_with('<') => skip_generics(header),
            None => header,
        };
        let self_ty = self_ty.split('<').next().unwrap_or_default();
        let self_ty = self_ty.rsplit("::").next().unwrap_or_default().trim();
        if self_ty == token
            && let Some(body) = brace_body(masked, open + 1)
        {
            out.push(body);
        }
    }
    out
}

/// `<'a, T: X<Y>> Rest` → `Rest`.
fn skip_generics(header: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in header.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return header[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    ""
}

/// The names of the fns in an impl body whose return type names `Self` or
/// the token by value (a `&Self` return lends, it builds nothing).
fn self_returning_fns(body: &str, token: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = body[from..].find("fn ") {
        let at = from + rel;
        from = at + "fn ".len();
        if !word_at(body, at, "fn") {
            continue;
        }
        let rest = &body[from..];
        let name_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if name_len == 0 {
            continue;
        }
        let mut depth = 0i32;
        let mut end = rest.len();
        for (i, b) in rest.bytes().enumerate().skip(name_len) {
            match b {
                b'(' => depth += 1,
                b')' => depth -= 1,
                b'{' | b';' if depth == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let Some((_, ret)) = rest[..end].split_once("->") else {
            continue;
        };
        let ret = ret.split(" where ").next().unwrap_or_default().trim();
        let names = |w: &str| ret.match_indices(w).any(|(i, _)| word_at(ret, i, w));
        if !ret.starts_with('&') && (names("Self") || names(token)) {
            out.push(rest[..name_len].to_string());
        }
    }
    out
}

/// The construction half of OB-14: a per-pane engine needs no field at all if
/// something builds one inside a loop.
fn check_closure_ctors(
    claim: &ScopeClaim,
    file: &str,
    masked: &str,
    token: &str,
    named_ctors: &BTreeSet<String>,
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
        // `T::default()` / `T::new(` / `T::<named ctor>(` / `T {`. `default`
        // and `new` count even for derives or out-of-tree definitions. The
        // single space before the brace excludes `T| {` and `T) {`.
        let assoc = tail.strip_prefix("::").and_then(|t| {
            let len = t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
            t[len..].starts_with('(').then_some(&t[..len])
        });
        let is_ctor = assoc
            .is_some_and(|f| f == "default" || f == "new" || named_ctors.contains(f))
            || (trimmed.starts_with('{') && tail.len() - trimmed.len() <= 1);
        if !is_ctor {
            continue;
        }
        // `struct X {`, `impl X {`, `impl Default for X {`, `enum`, `trait`,
        // `union` are definitions, not constructions.
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
        // `fn decos(&self) -> &X {` is a signature whose body brace follows
        // its return type: an accessor lends the one instance, it builds none.
        if returns_token(&masked[..at]) {
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

/// OB-17: a reserved scope phrase in a `///` block is a claim. Returns the
/// number of explicit waivers seen, shown in the green line.
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

/// Whether this doc block takes the OB-17 waiver, as opposed to mentioning it.
/// The marker must open a doc line (`/// scope-waiver: <reason>`); naming it
/// in a sentence does not count.
fn has_waiver_directive(block: &str) -> bool {
    block.lines().any(|line| {
        line.trim_start()
            .strip_prefix("///")
            .map(str::trim_start)
            .is_some_and(|text| text.to_lowercase().starts_with(SCOPE_WAIVER_MARKER))
    })
}

/// Every maximal run of `///` lines in a source, as `(1-based start line,
/// text)`. `//!` module prose is out of scope: no declaration owns a module's
/// cardinality.
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
    // The real checkout: the main tests mutate copies of the real claim files
    // rather than synthetic lookalikes.
    use crate::scan_set::test_fixtures::repo_root;
    use std::path::PathBuf;

    fn claim_by_id(id: &str) -> &'static ScopeClaim {
        SCOPE_CLAIMS
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no claim `{id}` in SCOPE_CLAIMS"))
    }

    /// Every repo-relative file a claim names, plus the whole derived-model
    /// catalog OB-16 resolves `machine` against.
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

    /// Apply a mutation to a copied file, asserting it applied (a stale `from`
    /// would make the test vacuous).
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

    /// The union of every claim's files, for runs that need the whole registry
    /// (the vocabulary lock is registry-wide).
    fn all_claim_files() -> Vec<String> {
        let mut files: Vec<String> = SCOPE_CLAIMS.iter().flat_map(claim_files).collect();
        files.sort();
        files.dedup();
        files
    }

    /// Giving every split pane its own effects pipeline (and so its own
    /// limiter), applied to a copy of the real `crates/aterm-gui/src/lib.rs`:
    /// green before, red after.
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
            "    fx: aterm_effects::pipeline::EffectsPipeline,\n",
            "    fx: std::collections::HashMap<u64, \
             aterm_effects::pipeline::EffectsPipeline>,\n",
        );
        let after = run_one(&root, claim);
        assert!(
            !after.ok,
            "a per-pane pipeline map MUST fail the flash-limiter chain:\n{}",
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

    /// With the chain untouched, a new per-pane owner of the engine or of a
    /// pipeline around it is caught by OB-14.
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

        let root = copy_root("per-pane-pipeline", &claim_files(claim));
        write_file(
            &root,
            "crates/aterm-gui/src/pane_fx.rs",
            "pub(crate) struct PaneFx {\n\
             \x20   pub(crate) fx: aterm_effects::pipeline::EffectsPipeline,\n\
             }\n",
        );
        let out = run_one(&root, claim);
        assert!(
            !out.ok && out.log.contains("[OB-14]") && out.log.contains("PaneFx"),
            "a per-pane pipeline is a second limiter and must open the closure:\n{}",
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

    /// The discovery, pinned on the real tree: the window's constructor is
    /// found, and a method that returns something else is not.
    #[test]
    fn named_constructors_are_discovered_from_impl_blocks() {
        let ctors = closed_token_ctors(&sweep_files(&repo_root()), SCOPE_CLAIMS);
        let fx = &ctors["EffectsPipeline"];
        for f in ["new", "default", "for_window"] {
            assert!(fx.contains(f), "`EffectsPipeline::{f}` builds one: {fx:?}");
        }
        assert!(
            !fx.contains("word_panes"),
            "a borrow is no constructor: {fx:?}"
        );
        let body = "fn a() -> Self {} fn b(&self) -> &Self {} fn c() -> Option<Self> {}\n\
                    fn d(&self) -> u32 { 0 } fn e() -> T where X: Y {}";
        assert_eq!(self_returning_fns(body, "T"), ["a", "c", "e"]);
        let src = "impl<'a> T<'a> { fn a() {} }\nimpl Default for crate::m::T { fn b() {} }\n\
                   impl U { fn c() {} }\nimpl X for Ts { fn d() {} }";
        assert_eq!(impl_bodies(src, "T").len(), 2);
    }

    /// The same loop through a named constructor
    /// (`EffectsPipeline::for_window()`): any associated fn of a closed token
    /// returning `Self` builds one.
    #[test]
    fn a_per_pane_named_constructor_fails_the_closure() {
        let claim = claim_by_id("flash-limiter");
        let mut files = claim_files(claim);
        files.push("crates/aterm-effects/src/pipeline_native.rs".to_string());
        let root = copy_root("per-pane-named-ctor", &files);
        let native = std::fs::read_to_string(root.join(&files[files.len() - 1]))
            .expect("read pipeline_native.rs");
        assert!(
            native.contains("pub fn for_window() -> Self {"),
            "the window's constructor moved: re-pin this row, or it proves nothing"
        );
        assert!(run_one(&root, claim).ok);

        write_file(
            &root,
            "crates/aterm-gui/src/pane_fx_probe.rs",
            "pub(crate) fn pane_pipelines(n: usize) -> Vec<aterm_effects::pipeline::EffectsPipeline> {\n\
             \x20   let mut v = Vec::new();\n\
             \x20   for _ in 0..n {\n\
             \x20       v.push(aterm_effects::pipeline::EffectsPipeline::for_window());\n\
             \x20   }\n\
             \x20   v\n\
             }\n",
        );
        let out = run_one(&root, claim);
        assert!(
            !out.ok
                && out.log.contains("[OB-14]")
                && out.log.contains("construction of `EffectsPipeline`")
                && out.log.contains("pane_pipelines"),
            "a per-pane `for_window()` is a second limiter and must open the closure, \
             naming the fn:\n{}",
            out.log
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity: a claim that stops matching the tree goes red.
    #[test]
    fn a_renamed_field_or_owner_fails_stale_rather_than_silently_passing() {
        let claim = claim_by_id("flash-limiter");

        let root = copy_root("renamed-field", &claim_files(claim));
        mutate(
            &root,
            "crates/aterm-gui/src/lib.rs",
            "    fx: aterm_effects::pipeline::EffectsPipeline,\n",
            "    effects: aterm_effects::pipeline::EffectsPipeline,\n",
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

    /// OB-17 both ways on a synthetic tree (no claims at all, so only the
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

        // A mention of the marker, explaining why the waiver is not taken, is
        // not a directive.
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

    /// An accessor's signature is not a construction: its body brace follows
    /// its return type (`-> &T {`), which the ctor scan would otherwise read
    /// as a struct literal. A real literal behind a borrow still counts.
    #[test]
    fn a_return_type_is_not_a_construction() {
        for before in [
            "pub fn decos(&self) -> ",
            "pub fn decos(&self) -> &",
            "pub fn decos_mut(&mut self) -> &mut ",
            "fn lend<'a>(x: &'a Y) -> &'a mut ",
            "fn lend<'a>(x: &'a Y) -> &'a ",
        ] {
            assert!(returns_token(before), "{before:?} is a return type");
        }
        for before in ["let a = &", "let a = ", "    decos: ", "out.push("] {
            assert!(!returns_token(before), "{before:?} is not a return type");
        }
    }

    /// False-positive calibration on real shapes: an enum variant named after
    /// the type, a `Self::`-qualified variant, an `impl` block and a `&mut` fn
    /// parameter are not owners of the enforcing state.
    #[test]
    fn enum_variants_and_fn_parameters_are_not_owners() {
        let claim = claim_by_id("flash-limiter");
        let mut files = claim_files(claim);
        for extra in [
            "crates/aterm-gui/src/metrics.rs",
            "crates/aterm-effects/src/motion.rs",
            "crates/aterm-effects/src/sound_policy.rs",
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
        let motion = std::fs::read_to_string(root.join("crates/aterm-effects/src/motion.rs"))
            .expect("read motion.rs");
        assert!(
            motion.contains("SeriousEffect::WordDecorations"),
            "qualified variant gone"
        );
        // The curse-bonk drain's `decos` parameter.
        let drain = std::fs::read_to_string(root.join("crates/aterm-effects/src/sound_policy.rs"))
            .expect("read sound_policy.rs");
        assert!(
            drain.contains("decos: &mut crate::word_decorations::WordDecorations,"),
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

    /// OB-15 both ways: the aggregator passes on the real tree, and dropping
    /// the parked-shard fold goes red.
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

        // Drop the parked-shard fold: each shard carries an O(1) burst-mutex
        // summary that the aggregator must fold in, so removing it means the
        // scan sees one pane only.
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

    /// A registered standing finding keeps the census green and is returned
    /// whole in `standing`, which the build gate forwards as warnings.
    #[test]
    fn a_registered_standing_finding_is_returned_whole_beside_the_log() {
        let root = copy_root("supernova-registered", &all_claim_files());
        mutate(
            &root,
            "crates/aterm-effects/src/word_decorations.rs",
            "        for p in self.parked.values() {",
            "        for p in std::iter::empty::<&ParkedPane>() {",
        );
        let registered = [StandingScopeFinding {
            claim: "supernova-burst-mutex",
            obligation: "OB-15",
            detail: "self.parked",
            finding: "registered by this test so the aggregator scan is a standing hazard",
        }];
        let out = run_scope_census_over(&root, SCOPE_CLAIMS, &registered);
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.ok, "a registered finding must not block:\n{}", out.log);
        assert_eq!(out.standing.len(), 1, "log:\n{}", out.log);
        let block = &out.standing[0];
        assert!(
            block.starts_with("  ! STANDING FINDING [OB-15] claim `supernova-burst-mutex`")
                && block.contains("registered by this test")
                && block.contains("detected as:"),
            "{block}"
        );
        assert!(
            out.log.contains(block.as_str()),
            "the log carries the same block"
        );
        assert!(
            out.log.contains("1 standing finding(s);"),
            "the verdict counts it:\n{}",
            out.log
        );
    }

    /// No standing finding is registered. The live tree is walked by
    /// `tools/freeze-safety-gate`'s build, not here.
    #[test]
    fn no_scope_standing_finding_is_registered() {
        assert!(
            SCOPE_STANDING_FINDINGS.is_empty(),
            "a scope standing finding was registered; re-audit before accepting it"
        );
    }
}
