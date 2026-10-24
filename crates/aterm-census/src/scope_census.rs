// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scope-cardinality census (OB-13 through OB-16).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::lock_order::{mask_gated_items, mask_literals};
use crate::{CensusOutcome, collect_rs_files, is_test_file, strip_line_comment};

/// The physical thing the safety budget belongs to. Declared, not derived; see
/// [`SCOPE_PRECISION_NOTE`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// One per OS window. Two windows are two enforcers on purpose.
    Window,
}

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

struct Replica {
    file: &'static str,
    /// `struct` name for a field replica, or the enclosing `fn` for a
    /// construction-site replica.
    owner: &'static str,
    decl: &'static str,
    kind: ReplicaKind,
    justification: &'static str,
}

struct Aggregator {
    symbol: &'static str,
    file: &'static str,
    /// The access path each shard must appear as inside that fn's body.
    reads: &'static [&'static str],
}

struct ScopeClaim {
    /// Stable id; appears in diagnostics.
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
    /// The sentence a reviewer reads. Non-empty, enforced by OB-16.
    rationale: &'static str,
}

const WEB_PIPELINE_SCOPE: &str = "The web page's pipeline runs in a DIFFERENT PROCESS \
    driving its own browser surface — its window, its budget (the same limiter is \
    documented at aterm-wasm/src/effects_api.rs). It never shares a retina with a \
    native WindowState, so it is a second SCOPE, not a second enforcer inside one scope.";

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
                file: "crates/aterm-gui/src/settings_preview.rs",
                owner: "place_companion",
                decl: "WordDecorations::default()",
                kind: ReplicaKind::SeparateScope,
                justification: "The Settings cursor preview's companion engine, one per \
                    raster thread. It only bakes and places the pet and flying-kitty sprites \
                    (`begin_host_frame`, `pet_cursor`, `kitty_cursor`, then its own `SpriteLayers`); it never \
                    scans rows or ticks, so it reaches neither `super_prepass` nor \
                    `nova_prepass`, the only writers of the `ignitions` reservation vec. It \
                    holds no flash budget and cannot put a luminance transition on the glass.",
            },
            Replica {
                file: "crates/aterm-bench/examples/effects_harness.rs",
                owner: "GlowHost",
                decl: "fx: EffectsPipeline,",
                kind: ReplicaKind::SeparateScope,
                justification: "The effects perf lane's typing host: a dev-only bench process \
                    whose one pipeline stands in for one window's, presenting to an offscreen \
                    frame no retina sees. Each scene builds a fresh host, so at most one \
                    pipeline is live at a time; its budget is that simulated window's whole.",
            },
            Replica {
                file: "crates/aterm-bench/examples/effects_harness.rs",
                owner: "pipeline_scene",
                decl: "EffectsPipeline::new()",
                kind: ReplicaKind::SeparateScope,
                justification: "The effects harness's measurement pipeline (`targo \
                    --unverified verify perf effects`): one per scene run, in a process \
                    that opens no window. It draws offscreen (`present_virtual`) and on \
                    the CPU presenter to count what the rain, the cats and the pet cost, \
                    so no retina ever sees its frames and its flash budget limits only \
                    the frames it measures.",
            },
        ],
        aggregator: None,
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

const SELF_EXCLUDED_DIR: &str = "crates/aterm-census/src";

/// The honest limits of this census, printed in every red diagnostic.
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
      - ONLY REGISTERED CLAIMS ARE CHECKED. An enforcer nobody registered in
        SCOPE_CLAIMS is not guarded, whatever its doc comment says.
      - IT PROVES THE ENFORCER DOES NOT MULTIPLY, NOT THAT NOT-MULTIPLYING IS
        SUFFICIENT. Whether the aggregator's scan is CORRECT over every shard
        is a model obligation (the `machine` field), checked elsewhere.
      - SCOPE OF THE SWEEPS: non-test `crates/**/*.rs`, with #[cfg(test)]
        items blanked and crates/aterm-census/src excluded (the
        registry itself names every token).
";

fn derive_model_sources(root: &Path) -> Vec<std::path::PathBuf> {
    let rs_files = |dir: std::path::PathBuf| -> Vec<std::path::PathBuf> {
        crate::tree::read_dir(&dir)
            .map(|entries| {
                entries
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut paths = vec![root.join("crates/aterm-spec/src/derive.rs")];
    let mut models: Vec<std::path::PathBuf> = crate::tree::read_dir(&root.join("crates"))
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(Result::ok)
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("aterm-models-"))
                })
                .flat_map(|path| rs_files(path.join("src")))
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    paths.extend(models);
    paths
}

fn mask_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let literals_masked = mask_literals(line);
        out.push_str(strip_line_comment(&literals_masked));
        out.push('\n');
    }
    out
}

fn struct_body<'a>(source: &'a str, owner: &str) -> Option<&'a str> {
    let head = format!("struct {owner} {{");
    let start = source.find(&head)? + head.len();
    brace_body(source, start)
}

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

fn all_struct_bodies(source: &str) -> Vec<(String, &str)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = source[from..].find("struct ") {
        let at = from + rel;
        from = at + "struct ".len();
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

const fn word_at(hay: &str, at: usize, needle: &str) -> bool {
    let b = hay.as_bytes();
    let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
    let end = at + needle.len();
    let after_ok = end >= b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_');
    before_ok && after_ok
}

fn field_name(decl: &str) -> Option<&str> {
    let (lhs, _) = decl.split_once(':')?;
    let name = lhs.rsplit(' ').next()?.trim();
    (!name.is_empty()).then_some(name)
}

fn line_of(source: &str, at: usize) -> usize {
    source[..at].bytes().filter(|b| *b == b'\n').count() + 1
}

fn sweep_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let _ = collect_rs_files(&root.join("crates"), &mut files);
    files.retain(|p| !is_test_file(p) && !p.to_string_lossy().contains(SELF_EXCLUDED_DIR));
    files.sort();
    files
}

const UNSHIPPED_GATES: &[&str] = &["#[cfg(test)]"];

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The scope-cardinality census over the live registry.
#[must_use]
pub fn run_scope_census(root: &Path) -> CensusOutcome {
    run_scope_census_over(root, SCOPE_CLAIMS)
}

const SCOPE: &str = "scope-cardinality census";

fn run_scope_census_over(root: &Path, claims: &[ScopeClaim]) -> CensusOutcome {
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== scope-cardinality census (OB-13..OB-16): {} claim(s) over {} ===",
        claims.len(),
        root.display()
    );
    let mut v: Vec<String> = Vec::new();

    for claim in claims {
        check_liveness(root, claim, &mut v);
        for link in claim.chain {
            check_link(root, claim, link, &mut v);
        }
        check_aggregation(root, claim, &mut v);
    }
    check_closure(root, claims, &mut v);

    if !v.is_empty() {
        for message in &v {
            let _ = writeln!(log, "{message}");
        }
        let _ = write!(log, "{SCOPE_PRECISION_NOTE}");
        return CensusOutcome::red(
            SCOPE,
            log,
            format!(
                "{} obligation violation(s) across {} claim(s).",
                v.len(),
                claims.len()
            ),
        );
    }
    let _ = writeln!(
        log,
        "    scope: verbatim declaration pinning + a closure sweep over non-test \
         crates/**/*.rs (precision limits: docs/temporal-safety-gate.md)."
    );
    let summary = format!(
        "{} scope claim(s) re-derived from the tree ({} pinned chain link(s), {} accounted \
         replica(s), {} closed token(s)).",
        claims.len(),
        claims.iter().map(|c| c.chain.len()).sum::<usize>(),
        claims.iter().map(|c| c.replicas.len()).sum::<usize>(),
        claims.iter().map(|c| c.closed_tokens.len()).sum::<usize>(),
    );
    CensusOutcome::green(SCOPE, log, summary)
}

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

fn check_liveness(root: &Path, claim: &ScopeClaim, v: &mut Vec<String>) {
    if claim.rationale.trim().is_empty() {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}` has an EMPTY rationale — the sentence a \
             reviewer reads IS the claim.",
            claim.id
        ));
    }
    if claim.chain.is_empty() {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}` pins NO chain link — a claim with an empty \
             chain constrains nothing.",
            claim.id
        ));
    } else if claim.chain[0].owner != claim.root {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}` declares root `{}` but its chain starts at \
             `{}` — the chain must begin AT the scope root, or its cardinality is \
             unproven.",
            claim.id, claim.root, claim.chain[0].owner
        ));
    }
    match claim.machine {
        Some(machine) => {
            if !claim.unmodelled_because.trim().is_empty() {
                v.push(format!(
                    "  ✗ FAIL [OB-16] claim `{}` names machine `{machine}` AND carries an \
                     `unmodelled_because` excuse — one or the other, or the excuse rots.",
                    claim.id
                ));
            }
            let resolved = derive_model_sources(root).iter().any(|path| {
                crate::tree::read_to_string(path)
                    .map(|s| mask_comments(&s).contains(&format!("{machine} {{")))
                    .unwrap_or(false)
            });
            if !resolved {
                v.push(format!(
                    "  ✗ FAIL [OB-16] claim `{}` names ty machine `{machine}`, which does \
                     not resolve anywhere in the derived-model catalog \
                     (crates/aterm-models-*/src/*.rs). The machine-checked \
                     half of this claim was deleted or renamed: restore it (the census \
                     proves the enforcer does not multiply; the MODEL proves that \
                     not-multiplying is the property that matters).",
                    claim.id
                ));
            }
        }
        None => {
            if claim.unmodelled_because.trim().is_empty() {
                v.push(format!(
                    "  ✗ FAIL [OB-16] claim `{}` has NO ty machine and NO \
                     `unmodelled_because` — an unmodelled safety claim must say why.",
                    claim.id
                ));
            }
        }
    }
    for r in claim.replicas {
        if r.justification.trim().is_empty() {
            v.push(format!(
                "  ✗ FAIL [OB-16] claim `{}`: replica `{}` in {} has an EMPTY \
                 justification — a second copy of the enforcing state is exactly what \
                 this census exists to question.",
                claim.id, r.owner, r.file
            ));
        }
    }
    for token in claim.closed_tokens {
        if !claim.chain.iter().any(|l| l.decl.contains(token)) {
            v.push(format!(
                "  ✗ FAIL [OB-16] claim `{}` closes over token `{token}` that no chain \
                 link mentions — the closure and the chain have drifted apart, so the \
                 sweep is guarding a different type from the one that is pinned.",
                claim.id
            ));
        }
    }
}

fn check_link(root: &Path, claim: &ScopeClaim, link: &Link, v: &mut Vec<String>) {
    let Ok(source) = crate::tree::read_to_string(&root.join(link.file)) else {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}`: {} is unreadable — a claimed file that moved \
             leaves the cardinality unguarded.",
            claim.id, link.file
        ));
        return;
    };
    let masked = mask_comments(&source);
    let Some(body) = struct_body(&masked, link.owner) else {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}`: `struct {}` not found in {}. The claim is \
             STALE: update the chain (and re-argue the cardinality), or the enforcer is \
             now owned by something this census cannot see.",
            claim.id, link.owner, link.file
        ));
        return;
    };
    let found = body.lines().filter(|l| l.trim() == link.decl).count();
    if found == 1 {
        return;
    }
    v.push(format!(
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
    ));
}

fn check_closure(root: &Path, claims: &[ScopeClaim], v: &mut Vec<String>) {
    if claims.iter().all(|c| c.closed_tokens.is_empty()) {
        return;
    }
    let files = sweep_files(root);
    let ctors = closed_token_ctors(&files, claims);
    let no_ctors = BTreeSet::new();
    for path in files {
        let Ok(raw) = crate::tree::read_to_string(&path) else {
            continue;
        };
        let file = rel(root, &path);
        let masked = mask_comments(&mask_gated_items(&raw, UNSHIPPED_GATES));
        let structs = all_struct_bodies(&masked);
        for claim in claims {
            for token in claim.closed_tokens {
                check_closure_fields(claim, &file, &structs, token, v);
                let named = ctors.get(token).unwrap_or(&no_ctors);
                check_closure_ctors(claim, &file, &masked, token, named, v);
            }
        }
    }
}

fn check_closure_fields(
    claim: &ScopeClaim,
    file: &str,
    structs: &[(String, &str)],
    token: &str,
    v: &mut Vec<String>,
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
            v.push(format!(
                "  ✗ FAIL [OB-14] claim `{}`: UNACCOUNTED owner of `{token}` —\n\
                 \x20       {file}: struct {owner} {{ {decl} }}\n\
                 \x20   The claim's chain and replicas do not mention it, so this is a \
                 SECOND place the enforcing state can live and nothing bounds how many \
                 of them exist.\n\
                 {}",
                claim.id,
                why_and_repair(claim)
            ));
        }
    }
}

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
        let Ok(raw) = crate::tree::read_to_string(path) else {
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

fn check_closure_ctors(
    claim: &ScopeClaim,
    file: &str,
    masked: &str,
    token: &str,
    named_ctors: &BTreeSet<String>,
    v: &mut Vec<String>,
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
        v.push(format!(
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
        ));
    }
}

fn check_aggregation(root: &Path, claim: &ScopeClaim, v: &mut Vec<String>) {
    let shards = claim
        .replicas
        .iter()
        .filter(|r| r.kind == ReplicaKind::Shard)
        .count();
    let Some(agg) = &claim.aggregator else {
        if shards > 0 {
            v.push(format!(
                "  ✗ FAIL [OB-15] claim `{}` declares {shards} SHARD replica(s) but no \
                 aggregator. Two copies of the enforcing state with no site that reads \
                 both IS the bug this census exists to catch, restated: name the fn \
                 that re-establishes the aggregate, or argue the replica is a separate \
                 SCOPE instead.\n{}",
                claim.id,
                why_and_repair(claim)
            ));
        }
        return;
    };
    if shards == 0 {
        v.push(format!(
            "  ✗ FAIL [OB-15] claim `{}` names aggregator `{}` but declares NO shard \
             replicas — dead machinery whose failure could never be observed. Remove it \
             or declare the shard it reconciles.",
            claim.id, agg.symbol
        ));
        return;
    }
    let Ok(source) = crate::tree::read_to_string(&root.join(agg.file)) else {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}`: aggregator file {} is unreadable.",
            claim.id, agg.file
        ));
        return;
    };
    let masked = mask_comments(&source);
    let Some(body) = fn_body(&masked, agg.symbol) else {
        v.push(format!(
            "  ✗ FAIL [OB-16] claim `{}`: aggregator `fn {}` not found in {} — STALE; \
             re-audit which fn re-establishes the aggregate now.",
            claim.id, agg.symbol, agg.file
        ));
        return;
    };
    for path in agg.reads {
        if body.contains(path) {
            continue;
        }
        v.push(format!(
            "  ✗ FAIL [OB-15] claim `{}`: aggregator `{}` ({}) never reads `{path}`, so \
             the aggregate is computed over ONE shard while the property is claimed over \
             all of them.\n{}",
            claim.id,
            agg.symbol,
            agg.file,
            why_and_repair(claim)
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scan_set::test_fixtures::{Tree, repo_root, synth_tree};

    fn claim_by_id(id: &str) -> &'static ScopeClaim {
        SCOPE_CLAIMS
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no claim `{id}` in SCOPE_CLAIMS"))
    }

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
        files.sort();
        files.dedup();
        files
    }

    fn copy_root(name: &str, files: &[String]) -> Tree {
        let src = repo_root();
        let copied: Vec<(String, String)> = files
            .iter()
            .map(|rel| {
                let from = src.join(rel);
                let text = std::fs::read_to_string(&from)
                    .unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
                (rel.clone(), text)
            })
            .collect();
        synth_tree("aterm-scope-census", name, &copied)
    }

    fn write_file(root: &Tree, rel: &str, contents: &str) {
        root.write(rel, contents);
    }

    fn mutate(root: &Tree, rel: &str, from: &str, to: &str) {
        let before = crate::tree::read_to_string(&root.join(rel)).expect("read for mutation");
        let after = before.replace(from, to);
        assert_ne!(
            before, after,
            "the mutation no longer applies to {rel} (looking for `{from}`) — the pinned \
             text is stale, so this demonstration proves nothing"
        );
        root.write(rel, &after);
    }

    fn run_one(root: &Path, claim: &'static ScopeClaim) -> CensusOutcome {
        run_scope_census_over(root, std::slice::from_ref(claim))
    }

    fn all_claim_files() -> Vec<String> {
        let mut files: Vec<String> = SCOPE_CLAIMS.iter().flat_map(claim_files).collect();
        files.sort();
        files.dedup();
        files
    }

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
    }

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
    }

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
    }

    #[test]
    fn named_constructors_are_discovered_from_impl_blocks() {
        let src = "pub struct EffectsPipeline;\n\
                   impl EffectsPipeline {\n\
                   \x20   pub fn new() -> Self {\n        Self\n    }\n\
                   \x20   pub fn for_window() -> Self {\n        Self::new()\n    }\n\
                   \x20   pub fn word_panes(&self) -> &WordPanes {\n        &self.panes\n    }\n\
                   }\n\
                   impl Default for EffectsPipeline {\n\
                   \x20   fn default() -> Self {\n        Self::new()\n    }\n\
                   }\n\
                   #[cfg(test)]\n\
                   impl EffectsPipeline {\n\
                   \x20   fn for_test() -> Self {\n        Self\n    }\n\
                   }\n";
        let rel = "crates/aterm-effects/src/pipeline.rs";
        let root = synth_tree(
            "aterm-scope-census",
            "ctors",
            &[(rel.to_string(), src.to_string())],
        );
        let ctors = closed_token_ctors(&[root.join(rel)], SCOPE_CLAIMS);
        let fx: Vec<&str> = ctors["EffectsPipeline"]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(fx, ["default", "for_window", "new"]);
        let body = "fn a() -> Self {} fn b(&self) -> &Self {} fn c() -> Option<Self> {}\n\
                    fn d(&self) -> u32 { 0 } fn e() -> T where X: Y {}";
        assert_eq!(self_returning_fns(body, "T"), ["a", "c", "e"]);
        let src = "impl<'a> T<'a> { fn a() {} }\nimpl Default for crate::m::T { fn b() {} }\n\
                   impl U { fn c() {} }\nimpl X for Ts { fn d() {} }";
        assert_eq!(impl_bodies(src, "T").len(), 2);
    }

    #[test]
    fn a_per_pane_named_constructor_fails_the_closure() {
        let claim = claim_by_id("flash-limiter");
        let mut files = claim_files(claim);
        files.push("crates/aterm-effects/src/pipeline_native.rs".to_string());
        let root = copy_root("per-pane-named-ctor", &files);
        let native = crate::tree::read_to_string(&root.join(&files[files.len() - 1]))
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
    }

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
    }

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

    #[test]
    fn enum_variants_and_fn_parameters_are_not_owners() {
        let claim = claim_by_id("flash-limiter");
        let root = copy_root("false-positives", &claim_files(claim));
        write_file(
            &root,
            "crates/aterm-effects/src/calibration.rs",
            "pub enum Owner {\n    WordDecorations = 21,\n}\n\
             pub enum SeriousEffect {\n    WordDecorations,\n}\n\
             fn which() -> SeriousEffect {\n    SeriousEffect::WordDecorations\n}\n\
             pub fn drain(decos: &mut crate::word_decorations::WordDecorations, n: u32) {}\n\
             impl WordDecorations {\n    fn len(&self) -> usize {\n        0\n    }\n}\n",
        );
        let out = run_one(&root, claim);
        assert!(
            out.ok,
            "variants, qualified variants, impl blocks and fn parameters must NOT count as \
             owners of the enforcing state:\n{}",
            out.log
        );
    }

    #[test]
    fn the_supernova_aggregator_regression_goes_red() {
        let root = copy_root("supernova", &all_claim_files());

        let out = run_scope_census_over(&root, SCOPE_CLAIMS);
        assert!(out.ok, "the aggregator must pass as written:\n{}", out.log);

        mutate(
            &root,
            "crates/aterm-effects/src/word_decorations.rs",
            "        for p in self.parked.values() {",
            "        for p in std::iter::empty::<&ParkedPane>() {",
        );
        let out = run_scope_census_over(&root, SCOPE_CLAIMS);
        assert!(
            !out.ok,
            "a per-bound-pane scan must fail OB-15:\n{}",
            out.log
        );
        assert!(
            out.log.contains("[OB-15]") && out.log.contains("self.parked"),
            "the failure must name the missing shard read:\n{}",
            out.log
        );
    }
}
