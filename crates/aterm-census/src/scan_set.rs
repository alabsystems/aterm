// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SCAN-SET DERIVATION (OB-7) — the lock-order census's crate scope, DERIVED
//! from the workspace manifests instead of a manually-pinned list, so the
//! obligation's own SCOPE is automatic: a new crate added to aterm-gui's
//! dependency graph is scanned with zero census edits, and a crate leaving the
//! graph is dropped the same way. No human memory in the loop.
//!
//! WHAT IT COMPUTES: the aterm-gui PROCESS closure — every workspace crate
//! reachable from `crates/aterm-gui` over NORMAL dependency edges — by parsing
//! the checked-in `Cargo.toml`s directly (root + each reachable crate). This
//! is deliberately NOT `cargo metadata`: the derivation must run inside
//! tools/freeze-safety-gate's build script, where a recursive cargo invocation
//! can deadlock on the cargo package lock and costs seconds; a manifest parse
//! is offline, deterministic, and microseconds.
//!
//! THE RULES (each fail-closed — an unclassifiable construct is a HARD ERROR,
//! never a silent skip):
//!
//!   * NORMAL deps only: `[dependencies]` and
//!     `[target.'cfg(...)'.dependencies]`. Dev- and build-dependencies never
//!     link into the shipped process and are excluded.
//!   * cfg-GATED TARGET deps are IN, platform-independently: lock discipline
//!     is not a per-OS property — a `cfg(windows)` workspace crate's ABBA
//!     ships in the Windows build of this same source tree, so it is scanned
//!     on every host. (On HEAD this is a no-op: `cargo tree --target all`
//!     equals the host-target closure.)
//!   * ONLY path deps are followed: a `path = "..."` spec, or
//!     `workspace = true` whose `[workspace.dependencies]` entry carries a
//!     path. Version deps are external registry code (out of the workspace's
//!     lexical naming discipline).
//!   * FEATURES are resolved: an `optional = true` path dep is in the closure
//!     iff aterm-gui's DEFAULT build activates it (`dep:name` / `name/feat`
//!     items, implicit optional-dep features, edge `features = [...]` lists,
//!     `default-features = false`, unified across all edges — the same
//!     semantics as cargo's resolver for the shipped `cargo build -p
//!     aterm-gui`). This is what keeps `aterm-bidi`/`aterm-sixel` in (their
//!     features are default-active) and `aterm-spec` out (`spec-anchors` is
//!     never activated by a normal default build).
//!   * PROC-MACRO crates (`[lib] proc-macro = true`) are classified out:
//!     they run inside rustc, never load into the GUI process. Their deps are
//!     not followed. Each exclusion is reported, never silent.
//!   * VENDORED `[patch.*]` forks (winit, libm, …) are the one category a
//!     manifest cannot classify alone. Those that link into the GUI process
//!     are SCANNED in **vendored-identity mode** (2026-07-13; formerly the
//!     printed "standing vendored-code gap"): each crate's lock identities
//!     live in a per-crate NAMESPACE (`winit::…`), so a foreign receiver name
//!     can never merge with an aterm identity or with another vendored
//!     crate's, and per-platform subtrees that do NOT compile into the
//!     shipped macOS GUI process are registered as labeled PLATFORM SLICES
//!     (sites counted every run, never graphed — graphing locks that exist
//!     in no shipped process could manufacture a cycle the no-waiver
//!     obligation cannot repair without editing upstream code). Build-time-only
//!     patches (pkg-config) are classified out with a written justification.
//!     The registry is [`REVIEWED_VENDORED_CRATES`], fail-closed BOTH ways
//!     (the VOCABULARY_INTERIORS discipline): an unregistered patch entry is
//!     a hard error (a human must review + register it), and a registered
//!     entry that no longer matches the patch table, whose path no longer
//!     exists on disk, or whose registered platform-slice paths are gone is a
//!     hard error (stale review). Every classification is printed every run.
//!   * FIRST-PARTY `[patch.*]` targets (a patch whose path is under
//!     `crates/`) are NOT vendored forks and get NO review row. The registry
//!     above exists to track THIRD-PARTY code this repository must keep
//!     re-reviewing; a patch entry pointing at a workspace member is aterm's
//!     own crate, already inside every whole-tree gate (license headers,
//!     lints, this closure derivation) and already scanned unnamespaced if it
//!     links into the process. Keying the review registry on IS-PATCHED
//!     rather than on IS-THIRD-PARTY was the bug: it would have demanded a
//!     "reviewed vendored fork" row for code we wrote. The classification is
//!     [`classify_patch_target`], and it is still fail-closed — a patch path
//!     under neither `vendor/` nor `crates/` is a hard error, and a
//!     first-party path that DOES carry a review row is a mis-registration
//!     and a hard error too.
//!   * DRIFT GUARD: every derived crate dir must actually contain `src/` —
//!     a closure the walker cannot scan is a hard error, not a silent shrink.
//!
//! HONEST LIMITS of the manifest parser: it covers the TOML subset this
//! workspace uses (line comments, single-line basic strings, inline tables,
//! multi-line arrays, dotted `name.workspace = true` deps). Anything outside
//! that subset in a DEPENDENCY-bearing section — git deps, `package = `
//! renames, multi-line `\"\"\"` strings, unknown spec keys — fails the
//! derivation loudly. Fail-closed: the census would rather stop the build
//! than scan the wrong set.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// The process root whose dependency closure IS the lock namespace: the
/// shipped aterm-gui binary.
pub const GUI_ROOT_CRATE: &str = "crates/aterm-gui";

/// A group of per-platform subtrees of a scanned vendored crate that do NOT
/// compile into the shipped macOS GUI process (winit's non-macOS backends).
/// Their acquisition sites are COUNTED and reported under the label every run
/// — never graphed: an edge from code that links into no shipped process could
/// close a cycle the no-waiver obligation cannot repair (vendored code may not
/// be edited). Promotion path: the day aterm ships a GUI for one of these
/// platforms, move its slice out of this list so the platform's code is
/// graphed (as its own per-target run, not a cross-platform union).
#[derive(Debug)]
pub struct PlatformSlice {
    /// The platform label (`linux`, `windows`, …) for the transcript.
    pub label: &'static str,
    /// Crate-relative paths (a directory, or a single `.rs` file) that the
    /// macOS build does not compile. Each must exist on disk (stale-review
    /// hard error otherwise).
    pub paths: &'static [&'static str],
}

/// How a reviewed vendored `[patch.*]` crate participates in the census.
pub enum VendoredMode {
    /// Links into the GUI process: SCANNED, with every lock identity living
    /// in the per-crate `namespace` (`winit::…`) so foreign receiver names
    /// can never merge with aterm identities or another vendored crate's.
    Scanned {
        /// The identity namespace prefix (unique across entries; `::` cannot
        /// appear in a lexical receiver name, so no collision is possible).
        namespace: &'static str,
        /// Per-platform subtrees not compiled into the shipped macOS binary:
        /// counted + labeled, never graphed. Empty for platform-independent
        /// crates.
        platform_slices: &'static [PlatformSlice],
        /// The review note: what the crate is and what its lock surface
        /// looked like when registered.
        audit: &'static str,
    },
    /// Never linked into the shipped GUI process (a build-script host tool):
    /// classified out, with the verification recorded.
    BuildDepOnly {
        /// Why (and how it was verified) this never links into the process.
        justification: &'static str,
    },
}

/// A vendored `[patch.*]` fork, REVIEWED and classified. See the module doc;
/// every entry is existence-checked both ways every run.
pub struct VendoredCrate {
    /// The patch table key (the crates-io package being replaced).
    pub package: &'static str,
    /// The declared replacement path, repo-relative (must match the patch
    /// table AND exist on disk).
    pub path: &'static str,
    /// Scanned-with-namespace, or build-dep-only.
    pub mode: VendoredMode,
}

/// The reviewed vendored-fork registry (see [`VendoredCrate`]). Fail-closed
/// both ways: a `[patch.*]` path entry NOT registered here is a hard error
/// (review + classify it); an entry here that is no longer in the patch
/// table, whose path is gone, or whose registered platform-slice paths are
/// gone is a stale review and a hard error until re-audited.
pub const REVIEWED_VENDORED_CRATES: &[VendoredCrate] = &[
    VendoredCrate {
        package: "winit",
        path: "vendor/winit",
        mode: VendoredMode::Scanned {
            namespace: "winit",
            // Survey 2026-07-13 (winit 0.30.13): 294 lock-vocabulary lines
            // total, but the macOS GUI process compiles ONLY the shared code
            // + platform_impl/macos + the macOS-gated platform/ extension
            // modules — exactly 2 blocking `.lock()` sites (event.rs
            // InnerSizeWriter; macos/window_delegate.rs scale-factor
            // new_inner_size). The per-OS backends below are cfg'd out of
            // the shipped binary (platform_impl/mod.rs + platform/mod.rs
            // gates) and are counted, labeled slices instead.
            platform_slices: &[
                PlatformSlice {
                    label: "linux",
                    paths: &[
                        "src/platform_impl/linux",
                        "src/platform/x11.rs",
                        "src/platform/wayland.rs",
                        "src/platform/startup_notify.rs",
                    ],
                },
                PlatformSlice {
                    label: "windows",
                    paths: &["src/platform_impl/windows", "src/platform/windows.rs"],
                },
                PlatformSlice {
                    label: "web",
                    paths: &["src/platform_impl/web", "src/platform/web.rs"],
                },
                PlatformSlice {
                    label: "android",
                    paths: &["src/platform_impl/android", "src/platform/android.rs"],
                },
                PlatformSlice {
                    label: "ios",
                    paths: &["src/platform_impl/ios", "src/platform/ios.rs"],
                },
                PlatformSlice {
                    label: "orbital",
                    paths: &["src/platform_impl/orbital", "src/platform/orbital.rs"],
                },
            ],
            audit: "vendored upstream winit 0.30.13 (+ the Wayland DnD patch): runs ON \
                    the main thread of the GUI process; the macOS-compiled surface is \
                    graphed under the `winit::` namespace, the non-macOS backends are \
                    labeled platform slices (no shipped aterm GUI compiles them today; \
                    the wasm surface is a different process, outside this census's \
                    one-process deadlock domain — censused since 2026-07-14 by \
                    run_wasm_census, where lock-order is a documented vacuous \
                    posture and L0-FREEZE is the live obligation)",
        },
    },
    VendoredCrate {
        package: "libm",
        path: "vendor/libm",
        mode: VendoredMode::Scanned {
            namespace: "libm",
            platform_slices: &[],
            audit: "aterm-trust fork of upstream libm (overflow fixes): pure math — \
                    ZERO lock-vocabulary sites at registration; scanned so that claim \
                    is re-checked by the walker every run instead of by review",
        },
    },
    VendoredCrate {
        package: "indexmap",
        path: "vendor/indexmap",
        mode: VendoredMode::Scanned {
            namespace: "indexmap",
            platform_slices: &[],
            audit: "aterm-trust fork of upstream indexmap (index-panic fix): one \
                    zero-arg `.read()` (extract.rs) that is core::ptr::read, \
                    categorized by the propagated raw-pointer evidence; no locks",
        },
    },
    VendoredCrate {
        package: "pkg-config",
        path: "vendor/pkg-config",
        mode: VendoredMode::BuildDepOnly {
            justification: "aterm-trust fork of upstream pkg-config: a build-script \
                            host helper. Verified 2026-07-13 via `cargo tree --edges \
                            all -i pkg-config`: reachable ONLY through the \
                            [build-dependencies] of zstd-sys — it never links into \
                            the shipped GUI process",
        },
    },
    VendoredCrate {
        package: "smol_str",
        path: "vendor/smol_str",
        mode: VendoredMode::Scanned {
            namespace: "smol_str",
            platform_slices: &[],
            audit: "aterm-trust fork of upstream smol_str (winit's key-text string \
                    type): ZERO lock-vocabulary sites at registration; scanned so the \
                    claim is re-checked every run",
        },
    },
];

/// What a `[patch.crates-io]` entry's replacement IS, which is what decides
/// whether it owes a review row.
///
/// The two are told apart by the replacement PATH, because that is the thing
/// the repository actually guarantees: `vendor/` holds upstream source we
/// redistribute and must keep reviewing; `crates/` holds workspace members we
/// wrote. Nothing else is accepted — a third location would be an
/// unclassifiable patch, and the census fails closed rather than guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchTargetKind {
    /// Under `vendor/`: third-party source, owes a [`REVIEWED_VENDORED_CRATES`]
    /// row and the whole provenance family (`cargo forge attest`).
    Vendored,
    /// Under `crates/` AND carrying none of the marks of a redistribution:
    /// a workspace member, code aterm wrote. Owes NO review row, NO NOTICE
    /// line and no `vendor/forge.toml` block — it is not a redistribution of
    /// anybody's work.
    FirstParty,
}

/// The marks of a REDISTRIBUTION, checked against the repository's own records
/// rather than inferred from where a directory sits.
///
/// Returns `Some(reason)` when `dir` looks like third-party source this
/// repository redistributes. Two independent signals, either sufficient,
/// because either one alone is enough to create the obligation:
///
/// * a RETAINED UPSTREAM LICENSE file in the crate root — the same signal
///   `cargo forge attest` `[OB-5]` already uses to decide a vendored fork has
///   kept its terms; and
/// * a naming of the path in the top-level `NOTICE`, which is this repo's
///   authoritative record of what it redistributes and under whose terms.
fn redistribution_evidence(root: &std::path::Path, path: &str) -> Option<String> {
    if let Ok(entries) = std::fs::read_dir(root.join(path)) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let upper = name.to_uppercase();
            if upper.starts_with("LICENSE") || upper.starts_with("COPYING") {
                return Some(format!(
                    "it retains an upstream license file (`{path}/{name}`)"
                ));
            }
        }
    }
    let notice = std::fs::read_to_string(root.join("NOTICE")).unwrap_or_default();
    if notice.contains(path) {
        return Some(format!(
            "the top-level NOTICE names `{path}` as redistributed"
        ));
    }
    None
}

/// Classify a `[patch.crates-io]` replacement path.
///
/// LOCATION IS NOT THE TEST, and the correction matters. An earlier form of
/// this function read `crates/` as "code aterm wrote" — but `crates/aterm-lz4`
/// is a modified, block-mode-only subset DERIVED FROM lz4_flex 0.11.5, whose
/// files carry `Copyright (c) 2020 Pascal Seitz et al.`, which retains
/// `crates/aterm-lz4/LICENSE-MIT`, and which the top-level NOTICE records as a
/// redistribution. So `crates/` already holds third-party source, and a patch
/// pointed there would have skipped the entire provenance family — reopening,
/// at a new address, exactly the hole this classifier exists to close.
///
/// The test is therefore EVIDENCE OF REDISTRIBUTION (see
/// [`redistribution_evidence`]), and a `crates/` path that shows any is an
/// ERROR rather than a silent reclassification: third-party source living
/// outside `vendor/` is an anomaly a human must resolve, not something a gate
/// should quietly decide either way.
///
/// Fail-closed throughout: any location other than `vendor/` or `crates/` is
/// an error naming both.
pub fn classify_patch_target(
    pkg: &str,
    path: &str,
    root: &std::path::Path,
) -> Result<PatchTargetKind, String> {
    if path.starts_with("vendor/") {
        return Ok(PatchTargetKind::Vendored);
    }
    if !path.starts_with("crates/") {
        return Err(format!(
            "[patch] entry `{pkg}` points at `{path}`, which is under neither `vendor/` \
             (third-party source this repository redistributes and reviews) nor `crates/` \
             (a first-party workspace member). The census cannot decide which obligations \
             apply to a third location, so it refuses to guess (fail-closed): move the \
             replacement under one of the two, or teach \
             `aterm_census::scan_set::classify_patch_target` a new kind first"
        ));
    }
    if let Some(why) = redistribution_evidence(root, path) {
        return Err(format!(
            "[patch] entry `{pkg}` points at `{path}`, which is under `crates/` but is NOT \
             first-party code: {why}. Treating it as first-party would skip the whole \
             provenance family (review row, NOTICE line, retained LICENSE, Apache-2.0 \
             section 4(b) byte-diff) for source this repository redistributes on somebody \
             else's terms. Fail-closed, because the two wrong answers are not symmetric: \
             move the fork under `vendor/` where the obligations are enforced, or — if the \
             evidence is a false positive — say why in \
             `aterm_census::scan_set::redistribution_evidence`"
        ));
    }
    Ok(PatchTargetKind::FirstParty)
}

/// One vendored crate the census SCANS (resolved from the registry against
/// the tree): the walker's vendored-identity-mode input.
#[derive(Debug)]
pub struct ScannedVendored {
    /// The package name (== the patch table key).
    pub package: String,
    /// The identity namespace prefix (`winit` ⇒ identities `winit::…`).
    pub namespace: &'static str,
    /// Repo-relative crate dir (`vendor/winit`).
    pub crate_dir: String,
    /// Repo-relative source dir (`vendor/winit/src`) — the walk root.
    pub scan_dir: String,
    /// Labeled per-platform subtrees to count, never graph.
    pub platform_slices: &'static [PlatformSlice],
    /// The registry's review note, printed in the transcript.
    pub audit: &'static str,
}

/// The derived scan set plus its reported classifications (never silent).
#[derive(Debug)]
pub struct ScanSet {
    /// Repo-relative `<crate dir>/src` scan dirs, sorted — the walker's input.
    pub scan_dirs: Vec<String>,
    /// Excluded proc-macro crates: (package name, crate dir).
    pub proc_macros: Vec<(String, String)>,
    /// The reviewed vendored `[patch.*]` crates SCANNED in vendored-identity
    /// mode, sorted by package.
    pub vendored_scanned: Vec<ScannedVendored>,
    /// The reviewed vendored `[patch.*]` crates classified out as
    /// build-time-only: (package, path, justification).
    pub vendored_build_only: Vec<(String, String, &'static str)>,
    /// `[patch.*]` entries pointing at a FIRST-PARTY workspace member
    /// (`crates/…`), sorted: (package, path). Recorded and printed — never
    /// review-registered, because they are not third-party code. A member
    /// that links into the GUI process is already in `scan_dirs` through the
    /// ordinary path-dependency closure, unnamespaced, like every other
    /// workspace crate.
    pub first_party_patches: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Minimal fail-closed TOML-subset reader
// ---------------------------------------------------------------------------

/// A parsed TOML value from the subset this workspace uses.
#[derive(Debug, Clone)]
enum Value {
    Str(String),
    Bool(bool),
    Array(Vec<Value>),
    Table(Vec<(String, Value)>),
    /// Anything else (numbers, …) — a hard error wherever a
    /// dependency-bearing section is being interpreted (rendered via the
    /// derived Debug in those diagnostics).
    #[allow(dead_code)] // read only through the derived Debug in error paths
    Other(String),
}

/// Strip a line comment (`# …`) that sits OUTSIDE any basic string on this
/// physical line. `\"` escapes honored.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// Net bracket/brace depth delta of a line, ignoring brackets inside strings.
fn depth_delta(line: &str) -> i32 {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut d = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'{' | b'[' if !in_str => d += 1,
            b'}' | b']' if !in_str => d -= 1,
            _ => {}
        }
        i += 1;
    }
    d
}

/// Parse one value starting at `s` (already trimmed); returns the value and
/// the unconsumed rest.
fn parse_value(s: &str) -> Result<(Value, &str), String> {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    match bytes.first() {
        Some(b'"') => {
            let mut i = 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => return Ok((Value::Str(s[1..i].to_string()), &s[i + 1..])),
                    _ => i += 1,
                }
            }
            Err("unterminated string".to_string())
        }
        Some(b'[') => {
            let mut rest = &s[1..];
            let mut items = Vec::new();
            loop {
                rest = rest.trim_start();
                if let Some(r) = rest.strip_prefix(']') {
                    return Ok((Value::Array(items), r));
                }
                let (v, r) = parse_value(rest)?;
                items.push(v);
                rest = r.trim_start();
                if let Some(r) = rest.strip_prefix(',') {
                    rest = r;
                } else if !rest.starts_with(']') {
                    return Err(format!("expected `,` or `]` in array near `{rest}`"));
                }
            }
        }
        Some(b'{') => {
            let mut rest = &s[1..];
            let mut pairs = Vec::new();
            loop {
                rest = rest.trim_start();
                if let Some(r) = rest.strip_prefix('}') {
                    return Ok((Value::Table(pairs), r));
                }
                let eq = rest.find('=').ok_or_else(|| {
                    format!("expected `key = value` in inline table near `{rest}`")
                })?;
                let key = rest[..eq].trim().to_string();
                let (v, r) = parse_value(&rest[eq + 1..])?;
                pairs.push((key, v));
                rest = r.trim_start();
                if let Some(r) = rest.strip_prefix(',') {
                    rest = r;
                } else if !rest.starts_with('}') {
                    return Err(format!(
                        "expected `,` or `}}` in inline table near `{rest}`"
                    ));
                }
            }
        }
        _ => {
            if let Some(r) = s.strip_prefix("true") {
                return Ok((Value::Bool(true), r));
            }
            if let Some(r) = s.strip_prefix("false") {
                return Ok((Value::Bool(false), r));
            }
            // Bare scalar (number, date, …): read to the next delimiter.
            let end = s.find([',', '}', ']']).unwrap_or(s.len());
            if end == 0 {
                return Err(format!("cannot parse value near `{s}`"));
            }
            Ok((Value::Other(s[..end].trim().to_string()), &s[end..]))
        }
    }
}

/// One manifest, reduced to sections of `key = value` entries.
struct Doc {
    /// (section header text without the outer brackets, entries).
    sections: Vec<(String, Vec<(String, Value)>)>,
}

/// Parse a manifest into sections + entries. Multi-line values (arrays,
/// inline tables spanning lines) are joined by bracket depth; comments are
/// stripped per physical line first (so comments inside multi-line arrays are
/// fine). Fail-closed: an entry that does not parse is a hard error naming
/// the file.
fn parse_doc(text: &str, file: &str) -> Result<Doc, String> {
    let mut sections: Vec<(String, Vec<(String, Value)>)> = vec![(String::new(), Vec::new())];
    let mut lines = text.lines().enumerate().peekable();
    while let Some((lineno, raw)) = lines.next() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                return Err(format!(
                    "{file}:{}: malformed section header `{line}`",
                    lineno + 1
                ));
            }
            let header = line.trim_matches(['[', ']']).trim().to_string();
            sections.push((header, Vec::new()));
            continue;
        }
        // Entry: join physical lines until brackets balance.
        let mut logical = line.to_string();
        let mut depth = depth_delta(line);
        while depth > 0 {
            let Some((_, next)) = lines.next() else {
                return Err(format!(
                    "{file}:{}: unbalanced brackets in entry",
                    lineno + 1
                ));
            };
            let next = strip_comment(next).trim();
            depth += depth_delta(next);
            logical.push(' ');
            logical.push_str(next);
        }
        let eq = logical.find('=').ok_or_else(|| {
            format!(
                "{file}:{}: expected `key = value`, got `{logical}`",
                lineno + 1
            )
        })?;
        let key = logical[..eq].trim().to_string();
        let (value, rest) =
            parse_value(&logical[eq + 1..]).map_err(|e| format!("{file}:{}: {e}", lineno + 1))?;
        if !rest.trim().is_empty() {
            return Err(format!(
                "{file}:{}: trailing text `{}` after value",
                lineno + 1,
                rest.trim()
            ));
        }
        sections
            .last_mut()
            .expect("sections is never empty")
            .1
            .push((key, value));
    }
    Ok(Doc { sections })
}

// ---------------------------------------------------------------------------
// Manifest model
// ---------------------------------------------------------------------------

/// One dependency edge as declared (normal deps only).
struct DepEdge {
    /// The dependency key (== the package name; renames are a hard error).
    key: String,
    /// `path = "…"` (relative to the declaring crate dir), if any.
    path: Option<String>,
    /// `workspace = true`?
    workspace: bool,
    optional: bool,
    default_features: bool,
    features: Vec<String>,
}

/// One crate manifest, reduced to what the closure walk needs.
struct Manifest {
    name: String,
    proc_macro: bool,
    features: BTreeMap<String, Vec<String>>,
    deps: Vec<DepEdge>,
}

/// Is this section header a NORMAL-dependency section (followed), a dev/build
/// one (excluded), or something else? Fail-closed: any header that mentions
/// dependencies but matches no known shape is an error.
enum SectionKind {
    NormalDeps,
    ExcludedDeps,
    Features,
    Lib,
    Package,
    WorkspaceDeps,
    Patch,
    Ignored,
}

fn classify_section(header: &str, file: &str) -> Result<SectionKind, String> {
    let h = header.trim();
    Ok(match h {
        "dependencies" => SectionKind::NormalDeps,
        "dev-dependencies" | "build-dependencies" => SectionKind::ExcludedDeps,
        "features" => SectionKind::Features,
        "lib" => SectionKind::Lib,
        "package" => SectionKind::Package,
        "workspace.dependencies" => SectionKind::WorkspaceDeps,
        _ if h.starts_with("patch.") => SectionKind::Patch,
        _ if h.starts_with("target.") => {
            if h.ends_with(".dependencies") {
                SectionKind::NormalDeps // cfg-target deps are IN (platform-independent)
            } else if h.ends_with(".dev-dependencies") || h.ends_with(".build-dependencies") {
                SectionKind::ExcludedDeps
            } else {
                return Err(format!(
                    "{file}: unrecognized target section `[{h}]` — the derivation cannot \
                     soundly classify it (fail-closed)"
                ));
            }
        }
        _ if h.contains("dependencies") => {
            return Err(format!(
                "{file}: unrecognized dependency-like section `[{h}]` — the derivation \
                 cannot soundly classify it (fail-closed)"
            ));
        }
        _ => SectionKind::Ignored,
    })
}

/// Interpret one `key = value` entry of a dependency section as a [`DepEdge`].
/// Handles the dotted `name.workspace = true` form. Unknown spec keys (git,
/// package renames, registry, …) are hard errors: the derivation refuses to
/// guess.
fn dep_edge(key: &str, value: &Value, file: &str) -> Result<DepEdge, String> {
    // Dotted form: `aterm-log.workspace = true`.
    if let Some(name) = key.strip_suffix(".workspace") {
        if matches!(value, Value::Bool(true)) {
            return Ok(DepEdge {
                key: name.to_string(),
                path: None,
                workspace: true,
                optional: false,
                default_features: true,
                features: Vec::new(),
            });
        }
        return Err(format!("{file}: `{key}` must be `= true`"));
    }
    if key.contains('.') {
        return Err(format!(
            "{file}: dotted dependency key `{key}` is not a form the derivation \
             understands (fail-closed)"
        ));
    }
    let mut edge = DepEdge {
        key: key.to_string(),
        path: None,
        workspace: false,
        optional: false,
        default_features: true,
        features: Vec::new(),
    };
    match value {
        Value::Str(_) => {} // bare version: external registry dep
        Value::Table(pairs) => {
            for (k, v) in pairs {
                match (k.as_str(), v) {
                    ("version", Value::Str(_)) => {}
                    ("path", Value::Str(p)) => edge.path = Some(p.clone()),
                    ("workspace", Value::Bool(true)) => edge.workspace = true,
                    ("optional", Value::Bool(b)) => edge.optional = *b,
                    ("default-features", Value::Bool(b)) => edge.default_features = *b,
                    ("features", Value::Array(items)) => {
                        for it in items {
                            match it {
                                Value::Str(s) => edge.features.push(s.clone()),
                                other => {
                                    return Err(format!(
                                        "{file}: dependency `{key}`: non-string feature \
                                         {other:?}"
                                    ));
                                }
                            }
                        }
                    }
                    (other, _) => {
                        return Err(format!(
                            "{file}: dependency `{key}` uses spec key `{other}`, which the \
                             derivation cannot soundly classify (git/package-rename/registry \
                             deps are out of the modeled subset — fail-closed)"
                        ));
                    }
                }
            }
        }
        other => {
            return Err(format!(
                "{file}: dependency `{key}` has unsupported value {other:?} (fail-closed)"
            ));
        }
    }
    Ok(edge)
}

/// Load + reduce one crate manifest at `<root>/<dir>/Cargo.toml`.
fn load_manifest(root: &Path, dir: &str) -> Result<Manifest, String> {
    let file_rel = format!("{dir}/Cargo.toml");
    let path = root.join(&file_rel);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {file_rel}: {e} — a path dependency must have a manifest \
             (fail-closed; is the dependency graph broken?)"
        )
    })?;
    let doc = parse_doc(&text, &file_rel)?;
    let mut m = Manifest {
        name: String::new(),
        proc_macro: false,
        features: BTreeMap::new(),
        deps: Vec::new(),
    };
    for (header, entries) in &doc.sections {
        if header.is_empty() {
            continue;
        }
        match classify_section(header, &file_rel)? {
            SectionKind::NormalDeps => {
                for (k, v) in entries {
                    m.deps.push(dep_edge(k, v, &file_rel)?);
                }
            }
            SectionKind::Package => {
                for (k, v) in entries {
                    if k == "name"
                        && let Value::Str(s) = v
                    {
                        m.name = s.clone();
                    }
                }
            }
            SectionKind::Lib => {
                for (k, v) in entries {
                    if k == "proc-macro" && matches!(v, Value::Bool(true)) {
                        m.proc_macro = true;
                    }
                }
            }
            SectionKind::Features => {
                for (k, v) in entries {
                    let Value::Array(items) = v else {
                        return Err(format!(
                            "{file_rel}: feature `{k}` is not an array (fail-closed)"
                        ));
                    };
                    let mut list = Vec::new();
                    for it in items {
                        match it {
                            Value::Str(s) => list.push(s.clone()),
                            other => {
                                return Err(format!(
                                    "{file_rel}: feature `{k}` has non-string item {other:?}"
                                ));
                            }
                        }
                    }
                    m.features.insert(k.clone(), list);
                }
            }
            SectionKind::ExcludedDeps
            | SectionKind::Ignored
            | SectionKind::WorkspaceDeps
            | SectionKind::Patch => {}
        }
    }
    if m.name.is_empty() {
        return Err(format!("{file_rel}: no `[package] name` (fail-closed)"));
    }
    Ok(m)
}

/// Lexically normalize `base/rel` ("crates/aterm-gui" + "../aterm-buffer" =>
/// "crates/aterm-buffer"). A path escaping the repo root is a hard error.
fn normalize_rel(base: &str, rel: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!(
                        "path `{rel}` (relative to `{base}`) escapes the repository root"
                    ));
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

// ---------------------------------------------------------------------------
// Root manifest: workspace dep table + patch table
// ---------------------------------------------------------------------------

/// A `[workspace.dependencies]` entry the members inherit via
/// `workspace = true`.
struct WsSpec {
    /// Repo-relative crate dir, if this is a path dep.
    dir: Option<String>,
    default_features: bool,
    features: Vec<String>,
}

struct RootTables {
    ws_deps: BTreeMap<String, WsSpec>,
    /// `[patch.*]` entries with a `path` redirect: key -> repo-relative path.
    patches: BTreeMap<String, String>,
}

fn load_root_tables(root: &Path) -> Result<RootTables, String> {
    let file_rel = "Cargo.toml";
    let text = std::fs::read_to_string(root.join(file_rel))
        .map_err(|e| format!("cannot read the workspace root {file_rel}: {e} (fail-closed)"))?;
    let doc = parse_doc(&text, file_rel)?;
    let mut out = RootTables {
        ws_deps: BTreeMap::new(),
        patches: BTreeMap::new(),
    };
    for (header, entries) in &doc.sections {
        if header.is_empty() {
            continue;
        }
        match classify_section(header, file_rel)? {
            SectionKind::WorkspaceDeps => {
                for (k, v) in entries {
                    let edge = dep_edge(k, v, file_rel)?;
                    if edge.workspace {
                        return Err(format!(
                            "{file_rel}: workspace dep `{k}` cannot itself say \
                             `workspace = true`"
                        ));
                    }
                    let dir = match &edge.path {
                        Some(p) => Some(normalize_rel("", p)?),
                        None => None,
                    };
                    out.ws_deps.insert(
                        edge.key.clone(),
                        WsSpec {
                            dir,
                            default_features: edge.default_features,
                            features: edge.features,
                        },
                    );
                }
            }
            SectionKind::Patch => {
                for (k, v) in entries {
                    let edge = dep_edge(k, v, file_rel)?;
                    let Some(p) = edge.path else {
                        return Err(format!(
                            "{file_rel}: `[patch]` entry `{k}` is not a path redirect — \
                             the derivation cannot classify it (fail-closed; review it)"
                        ));
                    };
                    out.patches.insert(k.clone(), normalize_rel("", &p)?);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Feature resolution (the default aterm-gui build's activation, unified)
// ---------------------------------------------------------------------------

/// What one crate's active features imply.
#[derive(Default)]
struct Activation {
    /// Optional dep keys activated (`dep:x`, implicit `x`, strong `x/f`).
    active_optional: BTreeSet<String>,
    /// Per-dep feature requests from `x/f` / `x?/f` items (weak requests are
    /// only consumed for deps that end up included, which is exactly cargo's
    /// weak semantics for closure membership).
    dep_requests: BTreeMap<String, BTreeSet<String>>,
}

/// Expand a crate's requested features (+ `default` when enabled) through its
/// feature table. Fail-closed: an item that is neither a feature, an optional
/// dep, nor a dep-feature reference is a hard error.
fn expand_features(
    m: &Manifest,
    dir: &str,
    defaults: bool,
    requested: &BTreeSet<String>,
) -> Result<Activation, String> {
    let dep_by_key: BTreeMap<&str, &DepEdge> = m.deps.iter().map(|d| (d.key.as_str(), d)).collect();
    let mut act = Activation::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut work: Vec<String> = requested.iter().cloned().collect();
    if defaults && m.features.contains_key("default") {
        work.push("default".to_string());
    }
    while let Some(item) = work.pop() {
        if !seen.insert(item.clone()) {
            continue;
        }
        if let Some(rest) = item.strip_prefix("dep:") {
            if !dep_by_key.contains_key(rest) {
                return Err(format!(
                    "{dir}/Cargo.toml: feature item `dep:{rest}` names no dependency \
                     of `{}` (fail-closed)",
                    m.name
                ));
            }
            act.active_optional.insert(rest.to_string());
            continue;
        }
        if let Some((dep, feat)) = item.split_once('/') {
            let (dep, weak) = match dep.strip_suffix('?') {
                Some(d) => (d, true),
                None => (dep, false),
            };
            let Some(edge) = dep_by_key.get(dep) else {
                return Err(format!(
                    "{dir}/Cargo.toml: feature item `{item}` names no dependency \
                     of `{}` (fail-closed)",
                    m.name
                ));
            };
            if !weak && edge.optional {
                act.active_optional.insert(dep.to_string());
            }
            act.dep_requests
                .entry(dep.to_string())
                .or_default()
                .insert(feat.to_string());
            continue;
        }
        if m.features.contains_key(&item) {
            for sub in &m.features[&item] {
                work.push(sub.clone());
            }
            continue;
        }
        if let Some(edge) = dep_by_key.get(item.as_str())
            && edge.optional
        {
            // Implicit optional-dep feature (old style).
            act.active_optional.insert(item.clone());
            continue;
        }
        return Err(format!(
            "{dir}/Cargo.toml: feature item `{item}` of `{}` is neither a feature nor \
             an optional dependency — the derivation cannot soundly classify it \
             (fail-closed)",
            m.name
        ));
    }
    Ok(act)
}

// ---------------------------------------------------------------------------
// The closure walk
// ---------------------------------------------------------------------------

/// Per-crate unified feature state during the fixpoint.
#[derive(Default, Clone, PartialEq)]
struct CrateState {
    defaults: bool,
    requested: BTreeSet<String>,
}

/// Derive the aterm-gui process scan set from the workspace manifests at
/// `root`. Pure function of the checked-in TOML files — no cargo, no network,
/// no build artifacts — safe inside a build script. Every error message is a
/// hard failure of the census (fail-closed; see the module doc for the rules).
pub fn derive_gui_scan_set(root: &Path) -> Result<ScanSet, String> {
    derive_process_scan_set(root, &[GUI_ROOT_CRATE])
}

/// Derive the process closure of `root_crates` (one process = one closure; a
/// multi-root process — the wasm renderer page, whose three cdylib modules
/// load into the one Electron renderer — seeds every root and takes the
/// UNION, with cargo's feature-unification semantics). Same rules, same
/// fail-closed posture as [`derive_gui_scan_set`], which is exactly this with
/// the single [`GUI_ROOT_CRATE`] seed.
pub fn derive_process_scan_set(root: &Path, root_crates: &[&str]) -> Result<ScanSet, String> {
    let tables = load_root_tables(root)?;

    // ---- Fixpoint over (crate dir -> unified feature state). ----
    let mut manifests: BTreeMap<String, Manifest> = BTreeMap::new();
    let mut state: BTreeMap<String, CrateState> = BTreeMap::new();
    for rc in root_crates {
        state.insert(
            (*rc).to_string(),
            CrateState {
                defaults: true,
                requested: BTreeSet::new(),
            },
        );
    }
    let mut rounds = 0usize;
    loop {
        rounds += 1;
        if rounds > 1000 {
            return Err("feature-resolution fixpoint failed to converge (internal error)".into());
        }
        let mut changed = false;
        let dirs: Vec<String> = state.keys().cloned().collect();
        for dir in dirs {
            if !manifests.contains_key(&dir) {
                manifests.insert(dir.clone(), load_manifest(root, &dir)?);
            }
            let m = &manifests[&dir];
            if m.proc_macro {
                continue; // compiler-host code: its deps never enter the process
            }
            let st = state[&dir].clone();
            let act = expand_features(m, &dir, st.defaults, &st.requested)?;
            // Collect the child updates first (cannot mutate `state` while
            // borrowing the manifest map).
            let mut updates: Vec<(String, bool, BTreeSet<String>)> = Vec::new();
            for edge in &m.deps {
                // Resolve the effective spec (workspace-table merge).
                let (child_dir, mut defaults, mut feats) = if edge.workspace {
                    let Some(ws) = tables.ws_deps.get(&edge.key) else {
                        return Err(format!(
                            "{dir}/Cargo.toml: `{}` says `workspace = true` but the root \
                             [workspace.dependencies] has no such entry (fail-closed)",
                            edge.key
                        ));
                    };
                    let mut f: BTreeSet<String> = edge.features.iter().cloned().collect();
                    f.extend(ws.features.iter().cloned());
                    (
                        ws.dir.clone(),
                        edge.default_features && ws.default_features,
                        f,
                    )
                } else {
                    let d = match &edge.path {
                        Some(p) => Some(normalize_rel(&dir, p)?),
                        None => None,
                    };
                    (
                        d,
                        edge.default_features,
                        edge.features.iter().cloned().collect(),
                    )
                };
                let included = !edge.optional || act.active_optional.contains(&edge.key);
                if !included {
                    continue;
                }
                let Some(child_dir) = child_dir else {
                    continue; // external registry dep: out of the workspace
                };
                if let Some(req) = act.dep_requests.get(&edge.key) {
                    feats.extend(req.iter().cloned());
                }
                // `default-features = false` on one edge never DISABLES what
                // another edge enables: union semantics, like cargo.
                if !defaults && state.get(&child_dir).is_some_and(|s| s.defaults) {
                    defaults = true;
                }
                updates.push((child_dir, defaults, feats));
            }
            for (child_dir, defaults, feats) in updates {
                let entry = state.entry(child_dir.clone()).or_insert_with(|| {
                    changed = true;
                    CrateState::default()
                });
                if defaults && !entry.defaults {
                    entry.defaults = true;
                    changed = true;
                }
                for f in feats {
                    if entry.requested.insert(f) {
                        changed = true;
                    }
                }
                // Sanity: the child's manifest name must match the dep key.
                if !manifests.contains_key(&child_dir) {
                    manifests.insert(child_dir.clone(), load_manifest(root, &child_dir)?);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // ---- Name/key consistency + proc-macro classification. ----
    let mut scan_dirs: Vec<String> = Vec::new();
    let mut proc_macros: Vec<(String, String)> = Vec::new();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for dir in state.keys() {
        let m = &manifests[dir];
        if let Some(prev) = names.insert(m.name.clone(), dir.clone()) {
            return Err(format!(
                "two crates in the closure share the package name `{}`: {prev} and {dir} \
                 (fail-closed)",
                m.name
            ));
        }
        if m.proc_macro {
            proc_macros.push((m.name.clone(), dir.clone()));
        } else {
            scan_dirs.push(format!("{dir}/src"));
        }
    }
    scan_dirs.sort();
    proc_macros.sort();

    // ---- Drift guard: the walker must be able to scan every derived crate.
    for sd in &scan_dirs {
        if !root.join(sd).is_dir() {
            return Err(format!(
                "derived closure crate `{sd}` has no src/ directory on disk — the scan \
                 set and the scannable tree have drifted (fail-closed; fix the crate \
                 layout or the dependency edge)"
            ));
        }
    }

    // ---- Vendored [patch.*] forks: reviewed classifications, fail-closed
    // BOTH ways.
    let mut vendored_scanned: Vec<ScannedVendored> = Vec::new();
    let mut vendored_build_only: Vec<(String, String, &'static str)> = Vec::new();
    let mut first_party_patches: Vec<(String, String)> = Vec::new();
    for (pkg, path) in &tables.patches {
        // FIRST, what KIND of replacement is this? Only third-party source
        // owes a review row; a patch pointing at a workspace member is aterm's
        // own crate and is covered by the closure derivation above.
        if classify_patch_target(pkg, path, root)? == PatchTargetKind::FirstParty {
            if REVIEWED_VENDORED_CRATES.iter().any(|r| r.package == pkg) {
                return Err(format!(
                    "[patch] entry `{pkg}` points at first-party `{path}` but ALSO has a \
                     REVIEWED_VENDORED_CRATES row — that registry records third-party \
                     code this repository redistributes and must keep reviewing, and a \
                     workspace member is neither. Remove the row (fail-closed: a false \
                     provenance claim is as wrong as a missing one)"
                ));
            }
            if !root.join(path).join("Cargo.toml").is_file() {
                return Err(format!(
                    "[patch] entry `{pkg}` points at first-party `{path}`, which has no \
                     Cargo.toml on disk — cargo cannot resolve this workspace at all \
                     (fail-closed)"
                ));
            }
            first_party_patches.push((pkg.clone(), path.clone()));
            continue;
        }
        let Some(reviewed) = REVIEWED_VENDORED_CRATES.iter().find(|r| r.package == pkg) else {
            return Err(format!(
                "[patch] entry `{pkg}` (path `{path}`) is NOT in \
                 REVIEWED_VENDORED_CRATES — a vendored fork links into the process \
                 unclassified, so a human must review it: register it as Scanned \
                 (vendored-identity mode) or BuildDepOnly with a written \
                 justification (crates/aterm-census/src/scan_set.rs) \
                 (fail-closed, never silent)"
            ));
        };
        if reviewed.path != path {
            return Err(format!(
                "REVIEWED_VENDORED_CRATES entry `{pkg}` is registered at path \
                 `{}` but the [patch] table now says `{path}` — the review is STALE; \
                 re-audit and update the entry",
                reviewed.path
            ));
        }
        match &reviewed.mode {
            VendoredMode::Scanned {
                namespace,
                platform_slices,
                audit,
            } => {
                if namespace.is_empty() || audit.trim().is_empty() {
                    return Err(format!(
                        "REVIEWED_VENDORED_CRATES entry `{pkg}` (Scanned) has an empty \
                         namespace or audit note — every scanned vendored crate must \
                         carry both (fail-closed)"
                    ));
                }
                let scan_dir = format!("{path}/src");
                if !root.join(&scan_dir).is_dir() {
                    return Err(format!(
                        "scanned vendored crate `{pkg}` has no `{scan_dir}` directory \
                         on disk — the registration and the tree have drifted \
                         (fail-closed)"
                    ));
                }
                for slice in *platform_slices {
                    for p in slice.paths {
                        if !root.join(path).join(p).exists() {
                            return Err(format!(
                                "REVIEWED_VENDORED_CRATES entry `{pkg}` registers \
                                 platform slice `{}` path `{p}`, which no longer \
                                 exists under `{path}` — the review is STALE \
                                 (an upstream layout change must be re-audited, \
                                 fail-closed)",
                                slice.label
                            ));
                        }
                    }
                }
                vendored_scanned.push(ScannedVendored {
                    package: pkg.clone(),
                    namespace,
                    crate_dir: path.clone(),
                    scan_dir,
                    platform_slices,
                    audit,
                });
            }
            VendoredMode::BuildDepOnly { justification } => {
                if justification.trim().is_empty() {
                    return Err(format!(
                        "REVIEWED_VENDORED_CRATES entry `{pkg}` (BuildDepOnly) has an \
                         empty justification — the classification must record its \
                         verification (fail-closed)"
                    ));
                }
                vendored_build_only.push((pkg.clone(), path.clone(), justification));
            }
        }
    }
    // Namespace uniqueness: two crates sharing a namespace would merge their
    // foreign identities — exactly what vendored-identity mode exists to
    // prevent.
    {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for v in &vendored_scanned {
            if !seen.insert(v.namespace) {
                return Err(format!(
                    "two scanned vendored crates share the identity namespace \
                     `{}` — namespaces must be unique (fail-closed)",
                    v.namespace
                ));
            }
        }
    }
    for r in REVIEWED_VENDORED_CRATES {
        if !tables.patches.contains_key(r.package) {
            return Err(format!(
                "REVIEWED_VENDORED_CRATES entry `{}` is no longer in the root \
                 [patch] table — a STALE review cannot keep describing the tree; \
                 remove or update the entry (fail-closed)",
                r.package
            ));
        }
        if !root.join(r.path).join("Cargo.toml").is_file() {
            return Err(format!(
                "REVIEWED_VENDORED_CRATES entry `{}` points at `{}`, which has no \
                 Cargo.toml on disk — the review is STALE (fail-closed)",
                r.package, r.path
            ));
        }
    }

    first_party_patches.sort();

    Ok(ScanSet {
        scan_dirs,
        proc_macros,
        vendored_scanned,
        vendored_build_only,
        first_party_patches,
    })
}

/// Render the derivation summary for the census log (printed on every run,
/// GREEN or RED, so the scope and its exclusions are never invisible).
pub fn render_scan_set(set: &ScanSet) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "    scan set: DERIVED from the workspace manifests — the Cargo.toml \
         path-dependency closure of {GUI_ROOT_CRATE} ({} crates; normal deps only, \
         dev-/build-deps excluded; cfg-target deps included platform-independently; \
         optional deps feature-resolved for the default build).",
        set.scan_dirs.len()
    );
    if !set.proc_macros.is_empty() {
        let listed: Vec<String> = set
            .proc_macros
            .iter()
            .map(|(n, d)| format!("{n} ({d})"))
            .collect();
        let _ = writeln!(
            out,
            "      excluded proc-macro crate(s) (compiler-host code, never loaded into \
             the GUI process): {}",
            listed.join(", ")
        );
    }
    if !set.vendored_scanned.is_empty() {
        let listed: Vec<String> = set
            .vendored_scanned
            .iter()
            .map(|v| {
                if v.platform_slices.is_empty() {
                    format!("{} ({})", v.package, v.crate_dir)
                } else {
                    let labels: Vec<&str> = v.platform_slices.iter().map(|s| s.label).collect();
                    format!(
                        "{} ({}; per-platform slices labeled, not graphed: {})",
                        v.package,
                        v.crate_dir,
                        labels.join("/")
                    )
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "      vendored [patch] crate(s) SCANNED in vendored-identity mode \
             (REVIEWED_VENDORED_CRATES, fail-closed both ways) — upstream process \
             code, each crate's lock identities in its own `<crate>::…` namespace \
             so foreign receiver names can never merge with aterm identities or \
             each other: {}",
            listed.join(", ")
        );
    }
    if !set.vendored_build_only.is_empty() {
        let listed: Vec<String> = set
            .vendored_build_only
            .iter()
            .map(|(n, p, _)| format!("{n} ({p})"))
            .collect();
        let _ = writeln!(
            out,
            "      excluded vendored [patch] crate(s), REVIEWED build-time-only \
             (build-script host code, never linked into the GUI process): {}",
            listed.join(", ")
        );
    }
    if !set.first_party_patches.is_empty() {
        let listed: Vec<String> = set
            .first_party_patches
            .iter()
            .map(|(n, p)| format!("{n} ({p})"))
            .collect();
        let _ = writeln!(
            out,
            "      FIRST-PARTY [patch] target(s) — workspace members that replace a \
             crates.io package for every consumer at once, NOT vendored forks: no \
             review row, no NOTICE line, no provenance obligations; each is scanned \
             like any other member IF it is in the closure above, and invisible here \
             if it is not: {}",
            listed.join(", ")
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_fixtures {
    //! Shared synthetic-tree manifest fixtures: the minimal workspace the
    //! derivation accepts (root manifest + patch table + vendor stubs +
    //! per-crate manifests), used by this module's tests AND the lock_order
    //! synthetic-tree tests.

    use super::{REVIEWED_VENDORED_CRATES, VendoredMode};

    /// The derived GUI-process closure, pinned. A LEGITIMATE dependency
    /// change (a new crate entering aterm-gui's graph, or one leaving it)
    /// just updates this pin — that is the automation working; the diff in
    /// review IS the audit trail. An UNEXPECTED delta (a crate vanishing
    /// that should still be linked, or appearing that should not) means the
    /// dependency graph or the derivation drifted: investigate before
    /// touching the pin.
    ///
    /// Read by TWO tests: scan_set's `derived_closure_matches_the_pinned_canary`
    /// (the member list, crate for crate) and lock_order's
    /// `scanned_set_covers_the_full_gui_process_closure` (the crate COUNT the
    /// census transcript must report — derived from this list's length, so a
    /// new crate is reviewed exactly once, here, and never re-counted by hand).
    ///
    /// PROVENANCE of the current pin: equals, crate for crate, the manual
    /// 42-crate list this derivation replaced (itself derived 2026-07-13
    /// from `cargo tree -p aterm-gui --edges normal`, macOS host target) —
    /// verified equal at the switchover, and re-verified against
    /// `cargo tree --target all`.
    ///
    /// The cfg-deps-IN decision is NO LONGER a no-op. `aterm-shell-integration`
    /// reaches `aterm-uds` through a `cfg(any(unix, windows))` target section
    /// (the one audited entropy surface, for the capability-nonce mint), which
    /// is the workspace's first cfg-gated workspace path dep. The derivation
    /// deliberately OVER-approximates there — a cfg-gated edge is IN on every
    /// platform — because the census's job is to scan every source file that
    /// could be process code, and scanning a file that a given target does not
    /// link is safe while missing one is not. `aterm-uds` was already in this
    /// GUI closure through `aterm-session`/`aterm-control` anyway, so the
    /// over-approximation costs this pin nothing; the wasm twin
    /// (`wasm_census.rs`) is where it actually shows, and it is written up
    /// there.
    pub(crate) const PINNED_GUI_CLOSURE: &[&str] = &[
        // Entered the closure with the embedded operator: aterm-gui now
        // depends on aterm-agent for the durable queue/WAL, and aterm-agent
        // on aterm-ctl for the one-binary fleet client. Both are normal
        // [dependencies] edges, so both crates are GUI process code and
        // their locks belong in the census.
        "crates/aterm-agent/src",
        "crates/aterm-alloc/src",
        "crates/aterm-bidi/src",
        "crates/aterm-bits/src",
        "crates/aterm-buffer/src",
        "crates/aterm-cap/src",
        "crates/aterm-codec/src",
        "crates/aterm-containment/src",
        // Entered the closure with the SessionHost seam extraction (f2284d67):
        // aterm-gui hosts the selection/block control verbs through
        // `control_host::GuiHost`. A normal [dependencies] edge, so the
        // whole crate is process code.
        "crates/aterm-control/src",
        "crates/aterm-core/src",
        "crates/aterm-ctl/src",
        // Entered the closure when the first-party SHA-256/HMAC-SHA256 crate
        // replaced `sha2` + `hmac`: aterm-gui, aterm-net, aterm-agent, atpkg
        // and aterm-update all hash through it now. A normal [dependencies]
        // edge, so it is GUI process code — it holds no locks, but the census
        // walks it for exactly that reason.
        "crates/aterm-digest/src",
        // Entered the closure when the first-party directory-handle crate
        // replaced `rustix` (72,832 lines, 2 packages): aterm-gui opens its
        // pinned-directory and media handles through it. A normal
        // [dependencies] edge, so it is GUI process code. It holds no locks of
        // its own — one syscall per entry point, names kept off the heap in a
        // stack buffer — and the `flock` the pinned-directory lane takes lives
        // in aterm-gui, where the census already sees it.
        "crates/aterm-dirfd/src",
        "crates/aterm-effects/src",
        "crates/aterm-error/src",
        "crates/aterm-ffi-types/src",
        "crates/aterm-gpu/src",
        "crates/aterm-grapheme/src",
        "crates/aterm-grid/src",
        "crates/aterm-gui/src",
        "crates/aterm-hash/src",
        // Entered the closure when the first-party HTTP/1.1 client replaced
        // `ureq` and the ureq-proto/http/bytes/httparse stack behind it: the
        // title-summary transport speaks to a local model server through it.
        // A normal [dependencies] edge, so it is GUI process code. It holds no
        // locks — a client owns its connection and nothing is shared across
        // threads — but the census walks it because it runs on the worker the
        // summary job spawns.
        "crates/aterm-http/src",
        // Entered the closure when the first-party JSON reader/writer replaced
        // `serde_json` (and the `zmij` float formatter and `itoa` behind it):
        // aterm-gui builds provider request bodies and reads their replies
        // through it. A normal [dependencies] edge, so it is GUI process code.
        // It holds NO synchronisation at all — the deserializer is a cursor
        // over a borrowed slice and the serializer a `Vec<u8>`, neither shared
        // across threads — so it can participate in no lock order; the census
        // walks it because it runs on whatever thread parses the reply.
        "crates/aterm-json/src",
        "crates/aterm-lexicon/src",
        "crates/aterm-log/src",
        "crates/aterm-lz4/src",
        "crates/aterm-net/src",
        "crates/aterm-observe/src",
        "crates/aterm-parser/src",
        // Entered the closure when the first-party PNG codec replaced `png`
        // and the second compression stack behind it (flate2 + miniz_oxide +
        // fdeflate + simd-adler32 + adler2, 7 packages / 33,439 lines):
        // aterm-render, aterm-gpu, aterm-effects and aterm-render-api all
        // decode inline images through it. A normal [dependencies] edge, so it
        // is GUI process code. Its only synchronisation is a `OnceLock` over
        // the CRC tables — a pure function of nothing, acquiring no other lock
        // inside its initialiser, so it cannot participate in an order.
        "crates/aterm-png/src",
        "crates/aterm-policy/src",
        "crates/aterm-predict/src",
        // Entered the closure with the agent auto-prime (2026-08-26): the
        // window primes every detected coding agent itself, so the primer
        // installer moved out of aterm-cli into this std-only leaf that
        // aterm-gui depends on directly. A normal [dependencies] edge, so
        // it is GUI process code; reviewed: pure string transforms plus
        // std::fs writes under $HOME, run on a detached thread the spawn
        // seam starts — never on the winit thread — and it holds no locks.
        "crates/aterm-primer/src",
        "crates/aterm-provenance/src",
        "crates/aterm-pty/src",
        // Entered the closure when the first-party regular-expression
        // engine (a bounded Pike VM) replaced `regex` — and with it
        // regex-automata, regex-syntax and aho-corasick, 4 packages /
        // 158,471 lines. aterm-selection, aterm-observe and aterm-search
        // compile patterns through it now. A normal [dependencies] edge, so
        // it is GUI process code — it holds no locks (no interior mutability
        // at all: the compiled program is immutable and the VM's state lives
        // on the stack), but the census walks it for exactly that reason.
        "crates/aterm-regex/src",
        "crates/aterm-render/src",
        "crates/aterm-render-api/src",
        "crates/aterm-rle/src",
        "crates/aterm-sandbox/src",
        "crates/aterm-scene/src",
        "crates/aterm-scrollback/src",
        "crates/aterm-search/src",
        "crates/aterm-selection/src",
        "crates/aterm-session/src",
        "crates/aterm-shell-integration/src",
        "crates/aterm-sixel/src",
        "crates/aterm-suggest/src",
        "crates/aterm-tempfile/src",
        // Entered the closure when the first-party clock replaced
        // `web-time`: aterm-core, -types, -effects, -gpu, -predict,
        // -policy, -observe and -agent all sample time through it now. A
        // normal [dependencies] edge, so it is GUI process code — it holds
        // no locks, but the census walks it for exactly that reason.
        "crates/aterm-time/src",
        // Entered the closure when the first-party TOML crate replaced `toml`
        // and `toml_edit` — and the winnow fork behind them — retiring 130
        // packages / 1,807,100 lines across five forks. Config, themes,
        // keybindings and every toy pack parse through it, on the startup path
        // before the window appears. A normal [dependencies] edge, so it is
        // GUI process code. It holds no locks: a parse owns its document and
        // shares nothing.
        "crates/aterm-toml/src",
        "crates/aterm-types/src",
        "crates/aterm-uds/src",
        "crates/aterm-update/src",
        "crates/aterm-update-core/src",
        "crates/aterm-vi/src",
        // Entered the closure when the K-2 winit→engine key map was split
        // out of aterm-types into its own crate: as an OPTIONAL FEATURE of
        // aterm-types it was unified on for every consumer in the workspace
        // resolve, which linked AppKit into the dependency-free aterm-ctl.
        // A normal [dependencies] edge from aterm-gui, so the whole crate is
        // process code — and the closure is unchanged in substance: the same
        // code was already in it via aterm-types.
        "crates/aterm-winit-keymap/src",
        "crates/atpkg/src",
    ];

    /// The `[patch.crates-io]` table + vendor stub trees satisfying the
    /// reviewed-classification checks: every registered entry present + on
    /// disk, scanned entries with a stub `src/` and every registered
    /// platform-slice path existing (a `.rs` slice path becomes a stub file;
    /// a directory slice path gets a stub file inside it).
    pub(crate) fn patch_fixture() -> (String, Vec<(String, String)>) {
        let mut table = String::from("[patch.crates-io]\n");
        let mut stubs = Vec::new();
        for r in REVIEWED_VENDORED_CRATES {
            table.push_str(&format!("{} = {{ path = \"{}\" }}\n", r.package, r.path));
            stubs.push((
                format!("{}/Cargo.toml", r.path),
                format!("[package]\nname = \"{}\"\n", r.package),
            ));
            if let VendoredMode::Scanned {
                platform_slices, ..
            } = &r.mode
            {
                stubs.push((format!("{}/src/lib.rs", r.path), "// stub\n".to_string()));
                for slice in *platform_slices {
                    for p in slice.paths {
                        let rel = if p.ends_with(".rs") {
                            format!("{}/{p}", r.path)
                        } else {
                            format!("{}/{p}/stub.rs", r.path)
                        };
                        stubs.push((rel, "// stub\n".to_string()));
                    }
                }
            }
        }
        (table, stubs)
    }

    /// A minimal derivable workspace: aterm-gui depending on aterm-types plus
    /// `extra` crates (each `(name, extra manifest sections)`), with the patch
    /// fixture. Returns (repo-relative path, contents) pairs; src files are
    /// the caller's business (the drift guard needs each crate to HAVE src/).
    pub(crate) fn workspace_manifests(extras: &[(&str, &str)]) -> Vec<(String, String)> {
        let (patch_table, mut files) = patch_fixture();
        let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
        root.push_str("[workspace.dependencies]\n");
        root.push_str("aterm-types = { path = \"crates/aterm-types\" }\n");
        for (name, _) in extras {
            root.push_str(&format!("{name} = {{ path = \"crates/{name}\" }}\n"));
        }
        root.push_str(&patch_table);
        files.push(("Cargo.toml".to_string(), root));
        let mut gui = String::from(
            "[package]\nname = \"aterm-gui\"\n[dependencies]\n\
             aterm-types = { workspace = true }\n",
        );
        for (name, _) in extras {
            gui.push_str(&format!("{name} = {{ workspace = true }}\n"));
        }
        files.push(("crates/aterm-gui/Cargo.toml".to_string(), gui));
        files.push((
            "crates/aterm-types/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-types\"\n".to_string(),
        ));
        for (name, sections) in extras {
            files.push((
                format!("crates/{name}/Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n{sections}"),
            ));
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn synth_tree(name: &str, files: &[(String, String)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("aterm-scan-set-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
            std::fs::write(&path, contents).expect("write synth file");
        }
        root
    }

    /// Base fixture + a src/lib.rs for every crate dir mentioned, so the
    /// drift guard is satisfied.
    fn with_src(mut files: Vec<(String, String)>) -> Vec<(String, String)> {
        let dirs: Vec<String> = files
            .iter()
            .filter_map(|(p, _)| {
                p.strip_suffix("/Cargo.toml")
                    .filter(|d| d.starts_with("crates/"))
                    .map(str::to_string)
            })
            .collect();
        for d in dirs {
            files.push((format!("{d}/src/lib.rs"), "// synth\n".to_string()));
        }
        files
    }

    fn derive(name: &str, files: Vec<(String, String)>) -> Result<ScanSet, String> {
        let root = synth_tree(name, &files);
        let out = derive_gui_scan_set(&root);
        let _ = std::fs::remove_dir_all(&root);
        out
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-census lives two levels under the repo root")
            .to_path_buf()
    }

    // ------------------------------------------------------------------
    // THE CANARY: the derived closure on THIS tree, pinned.
    // ------------------------------------------------------------------

    /// The pin is [`test_fixtures::PINNED_GUI_CLOSURE`] — hoisted there so the
    /// lock-order census's own transcript test derives its crate count from
    /// the same reviewed list instead of a hand-typed literal that had to be
    /// bumped in step with it (and was not, the day `aterm-primer` joined).
    #[test]
    fn derived_closure_matches_the_pinned_canary() {
        const PINNED: &[&str] = test_fixtures::PINNED_GUI_CLOSURE;
        let set = derive_gui_scan_set(&repo_root()).expect("derivation must succeed on HEAD");
        let mut pinned: Vec<String> = PINNED.iter().map(|s| s.to_string()).collect();
        pinned.sort();
        assert_eq!(
            set.scan_dirs, pinned,
            "\nthe DERIVED GUI-process closure changed.\n\
             If you just added/removed a dependency of the aterm-gui graph, this is \
             the automation working: update the pin above to the new derived set (the \
             review diff is the audit trail).\n\
             If you did NOT change dependencies, the derivation or the workspace \
             drifted unexpectedly — investigate before touching the pin.\n"
        );
        // The one proc-macro in the closure, classified out by its manifest.
        assert_eq!(
            set.proc_macros,
            vec![(
                "aterm-error-derive".to_string(),
                "crates/aterm-error-derive".to_string()
            )],
            "proc-macro classification changed"
        );
        // All five reviewed VENDORED forks classified: four scanned in
        // vendored-identity mode, one (pkg-config) build-time-only. (The
        // count read "six … five" until the winnow fork retired with
        // toml_edit; the assertions below were right and the prose was not.)
        // The other five [patch] entries are first-party and asserted separately.
        let scanned: Vec<&str> = set
            .vendored_scanned
            .iter()
            .map(|v| v.package.as_str())
            .collect();
        assert_eq!(
            scanned,
            vec!["indexmap", "libm", "smol_str", "winit"],
            "the scanned vendored set changed"
        );
        assert_eq!(
            set.vendored_build_only.len(),
            1,
            "pkg-config is the one build-dep-only patch"
        );
        assert_eq!(set.vendored_build_only[0].0, "pkg-config");
        // The SIX FIRST-PARTY patch targets — workspace members aterm wrote,
        // reached only by third-party consumers through the patch table:
        //   `arrayvec`  crates/aterm-arrayvec   re-export of aterm_alloc::ArrayVec
        //   `cfg-if`    crates/aterm-cfg-if     the cfg_if! macro
        //   `libc`      crates/aterm-libc       first-party libc replacement
        //   `log`       crates/aterm-log-shim   no-op facade (NOT crates/aterm-log,
        //                                       which is aterm's real logger)
        //   `profiling` crates/aterm-profiling  no-op facade
        //   `tracing`   crates/aterm-tracing    no-op facade
        // Sorted, because `derive` sorts them. Each owes NO review row, and
        // each is absent from `scan_dirs` above because no first-party crate
        // depends on it.
        assert_eq!(
            set.first_party_patches,
            vec![
                ("arrayvec".to_string(), "crates/aterm-arrayvec".to_string()),
                ("cfg-if".to_string(), "crates/aterm-cfg-if".to_string()),
                ("libc".to_string(), "crates/aterm-libc".to_string()),
                ("log".to_string(), "crates/aterm-log-shim".to_string()),
                (
                    "profiling".to_string(),
                    "crates/aterm-profiling".to_string()
                ),
                ("tracing".to_string(), "crates/aterm-tracing".to_string()),
            ],
            "the first-party [patch] target set changed"
        );
        // ASSERTED PER CRATE, not as a count: the hazard is one of them
        // acquiring a first-party dependant and quietly becoming GUI process
        // code whose locks nothing censuses. `crates/aterm-arrayvec` is the
        // live risk — it is the only one of the five that is a real data
        // structure a member could plausibly start using, and it already
        // depends on `aterm-alloc`, which IS in the closure.
        for dir in [
            "crates/aterm-arrayvec/src",
            "crates/aterm-cfg-if/src",
            "crates/aterm-log-shim/src",
            "crates/aterm-profiling/src",
            "crates/aterm-tracing/src",
        ] {
            assert!(
                !set.scan_dirs.contains(&dir.to_string()),
                "{dir} is reached only through the patch table, never through a \
                 first-party path dependency; if that changed, the crate is now GUI \
                 process code and belongs in PINNED_GUI_CLOSURE"
            );
        }
        // winit's platform slices resolve on disk (checked in derive) and
        // cover exactly the six non-macOS backends.
        let winit = set
            .vendored_scanned
            .iter()
            .find(|v| v.package == "winit")
            .expect("winit is scanned");
        let labels: Vec<&str> = winit.platform_slices.iter().map(|s| s.label).collect();
        assert_eq!(
            labels,
            vec!["linux", "windows", "web", "android", "ios", "orbital"]
        );
    }

    #[test]
    fn scanned_vendored_crate_missing_src_or_slice_path_fails_closed() {
        // (a) a Scanned entry whose src/ is gone: stale registration.
        let mut files = test_fixtures::workspace_manifests(&[]);
        files.retain(|(p, _)| p != "vendor/winit/src/lib.rs");
        // Remove EVERY winit src file so the src/ dir is not created at all.
        files.retain(|(p, _)| !p.starts_with("vendor/winit/src/"));
        let err = derive("nosrcvendor", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("no `vendor/winit/src` directory"),
            "err: {err}"
        );
        // (b) a registered platform-slice path gone from disk: stale review
        // (an upstream layout change must be re-audited, not silently
        // re-scoped).
        let mut files = test_fixtures::workspace_manifests(&[]);
        files.retain(|(p, _)| p != "vendor/winit/src/platform/x11.rs");
        let err = derive("staleslice", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("platform slice `linux` path `src/platform/x11.rs`")
                && err.contains("STALE"),
            "err: {err}"
        );
    }

    /// A `[patch]` entry pointing at a workspace member is FIRST-PARTY: it is
    /// recorded, it is NOT demanded a review row, and — the direction that
    /// matters — a review row for it is refused. The old code keyed the
    /// registry on IS-PATCHED and would have hard-errored on the first line
    /// of this test.
    #[test]
    fn a_first_party_patch_target_is_recorded_and_never_review_registered() {
        let mut files = test_fixtures::workspace_manifests(&[("aterm-shim", "")]);
        // Append a first-party patch entry to the fixture's patch table.
        for (path, contents) in files.iter_mut() {
            if path == "Cargo.toml" {
                contents.push_str("shimmed = { path = \"crates/aterm-shim\" }\n");
            }
        }
        let set = derive("firstparty", with_src(files)).expect("derivation must succeed");
        assert_eq!(
            set.first_party_patches,
            vec![("shimmed".to_string(), "crates/aterm-shim".to_string())]
        );
        // And it is NOT counted as a vendored fork in either direction.
        assert!(set.vendored_scanned.iter().all(|v| v.package != "shimmed"));
        assert!(set.vendored_build_only.iter().all(|v| v.0 != "shimmed"));
    }

    /// Still fail-closed: a patch path under neither `vendor/` nor `crates/`
    /// is unclassifiable, and the census stops rather than guess which
    /// obligations apply.
    #[test]
    fn a_patch_target_outside_vendor_and_crates_fails_closed() {
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (path, contents) in files.iter_mut() {
            if path == "Cargo.toml" {
                contents.push_str("elsewhere = { path = \"third_party/elsewhere\" }\n");
            }
        }
        files.push((
            "third_party/elsewhere/Cargo.toml".to_string(),
            "[package]\nname = \"elsewhere\"\n".to_string(),
        ));
        let err = derive("oddpatch", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("under neither `vendor/`") && err.contains("fail-closed"),
            "err: {err}"
        );
    }

    /// A first-party patch target that has been mis-filed in the vendored
    /// registry is a FALSE provenance claim, and is refused by name.
    #[test]
    fn a_review_row_for_a_first_party_patch_target_is_refused() {
        // The real tree's registry cannot be mutated from a test, so this
        // asserts the invariant the derivation enforces over the REAL tree:
        // no REVIEWED_VENDORED_CRATES row may point outside vendor/.
        for r in REVIEWED_VENDORED_CRATES {
            assert_eq!(
                classify_patch_target(r.package, r.path, &repo_root()),
                Ok(PatchTargetKind::Vendored),
                "REVIEWED_VENDORED_CRATES row `{}` points at `{}`, which is not \
                 third-party vendored source",
                r.package,
                r.path
            );
        }
        for (pkg, path) in [
            ("tracing", "crates/aterm-tracing"),
            ("profiling", "crates/aterm-profiling"),
            ("cfg-if", "crates/aterm-cfg-if"),
            ("arrayvec", "crates/aterm-arrayvec"),
            ("log", "crates/aterm-log-shim"),
        ] {
            assert_eq!(
                classify_patch_target(pkg, path, &repo_root()),
                Ok(PatchTargetKind::FirstParty),
                "{pkg} -> {path}"
            );
        }
    }

    /// THE GUARD IS ARMED — proved against the real anomaly, not a fixture.
    ///
    /// `crates/aterm-lz4` is a modified subset derived from lz4_flex 0.11.5:
    /// its files carry `Copyright (c) 2020 Pascal Seitz et al.`, it retains
    /// `crates/aterm-lz4/LICENSE-MIT`, and the top-level NOTICE records it as
    /// a redistribution. It is the living counter-example to "everything under
    /// `crates/` is code we wrote", and the reason this classifier tests for
    /// EVIDENCE instead of location.
    ///
    /// This test exists because the previous version of the refusal was
    /// VACUOUS — it asserted the tree's current state and never reached the
    /// branch. Here the branch is reached, on a path that really exists, and
    /// the message must name why. If someone ever relicenses aterm-lz4 as
    /// original work and drops its LICENSE-MIT and NOTICE entry, this test
    /// fails and asks them to confirm that on purpose.
    #[test]
    fn a_redistribution_under_crates_is_refused_not_called_first_party() {
        let err = classify_patch_target("lz4_flex", "crates/aterm-lz4", &repo_root())
            .expect_err("aterm-lz4 is derived from lz4_flex and must not read as first-party");
        assert!(
            err.contains("LICENSE-MIT") || err.contains("NOTICE"),
            "the refusal must NAME the evidence so the reader can check it; got: {err}"
        );
    }

    /// The negative control for the test above: a crate with no retained
    /// license file and no NOTICE line classifies FirstParty. Without this,
    /// `redistribution_evidence` returning `Some` unconditionally would still
    /// pass every assertion in this module.
    #[test]
    fn a_genuine_first_party_crate_is_not_mistaken_for_a_redistribution() {
        assert_eq!(
            redistribution_evidence(&repo_root(), "crates/aterm-hash"),
            None,
            "aterm-hash is original first-party code and must show no evidence"
        );
    }

    // ------------------------------------------------------------------
    // Derivation unit tests (synthetic workspaces)
    // ------------------------------------------------------------------

    #[test]
    fn direct_workspace_and_dotted_path_deps_are_followed() {
        let mut files = test_fixtures::workspace_manifests(&[
            // workspace-table dep, inline form (in gui via fixture): aterm-types.
            // Direct path dep + dotted workspace dep, chained:
            (
                "aterm-a",
                "[dependencies]\naterm-b = { path = \"../aterm-b\" }\n",
            ),
            ("aterm-c", ""),
        ]);
        // aterm-b is NOT a workspace-table entry: reached only via aterm-a's
        // direct path dep. aterm-c reached via gui. Add the dotted form on b.
        files.push((
            "crates/aterm-b/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-b\"\n[dependencies]\naterm-types.workspace = true\n"
                .to_string(),
        ));
        let set = derive("chain", with_src(files)).expect("derivation");
        assert_eq!(
            set.scan_dirs,
            vec![
                "crates/aterm-a/src",
                "crates/aterm-b/src",
                "crates/aterm-c/src",
                "crates/aterm-gui/src",
                "crates/aterm-types/src",
            ]
        );
    }

    #[test]
    fn dev_and_build_deps_are_excluded() {
        let mut files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[dev-dependencies]\naterm-devonly = { path = \"../aterm-devonly\" }\n\
             [build-dependencies]\naterm-buildonly = { path = \"../aterm-buildonly\" }\n",
        )]);
        for name in ["aterm-devonly", "aterm-buildonly"] {
            files.push((
                format!("crates/{name}/Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            ));
        }
        let set = derive("devbuild", with_src(files)).expect("derivation");
        assert!(
            !set.scan_dirs.iter().any(|d| d.contains("only")),
            "dev-/build-deps must never enter the process closure: {:?}",
            set.scan_dirs
        );
    }

    #[test]
    fn cfg_target_path_deps_are_included_platform_independently() {
        // The documented decision: lock discipline is not a per-OS property,
        // so a cfg(windows) path dep is scanned on every host.
        let mut files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[target.'cfg(windows)'.dependencies]\n\
             aterm-winonly = { path = \"../aterm-winonly\" }\n\
             [target.'cfg(unix)'.dev-dependencies]\n\
             aterm-devonly = { path = \"../aterm-devonly\" }\n",
        )]);
        for name in ["aterm-winonly", "aterm-devonly"] {
            files.push((
                format!("crates/{name}/Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            ));
        }
        let set = derive("cfgdeps", with_src(files)).expect("derivation");
        assert!(
            set.scan_dirs
                .contains(&"crates/aterm-winonly/src".to_string()),
            "cfg-target NORMAL deps are IN: {:?}",
            set.scan_dirs
        );
        assert!(
            !set.scan_dirs
                .contains(&"crates/aterm-devonly/src".to_string()),
            "cfg-target DEV deps stay out: {:?}",
            set.scan_dirs
        );
    }

    #[test]
    fn optional_deps_follow_default_feature_activation() {
        // Mirrors the real graph: gui's default feature forwards to a dep
        // feature (`aterm-core/sixel` shape) which `dep:`-activates an
        // optional path dep; an UNACTIVATED optional dep (the `spec-anchors`
        // shape) stays out; an edge `features = [...]` list (the `bidi`
        // shape) activates a third.
        let (patch_table, mut files) = test_fixtures::patch_fixture();
        let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
        root.push_str("[workspace.dependencies]\n");
        for n in ["aterm-mid", "aterm-on", "aterm-off", "aterm-edge"] {
            root.push_str(&format!("{n} = {{ path = \"crates/{n}\" }}\n"));
        }
        root.push_str(&patch_table);
        files.push(("Cargo.toml".to_string(), root));
        files.push((
            "crates/aterm-gui/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-gui\"\n\
             [features]\ndefault = [\"fancy\"]\nfancy = [\"aterm-mid/glitter\"]\n\
             [dependencies]\naterm-mid = { workspace = true, features = [\"edgefeat\"] }\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-mid/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-mid\"\n\
             [features]\nglitter = [\"dep:aterm-on\"]\nanchors = [\"dep:aterm-off\"]\n\
             edgefeat = [\"aterm-edge\"]\n\
             [dependencies]\naterm-on = { workspace = true, optional = true }\n\
             aterm-off = { workspace = true, optional = true }\n\
             aterm-edge = { workspace = true, optional = true }\n"
                .to_string(),
        ));
        for n in ["aterm-on", "aterm-off", "aterm-edge"] {
            files.push((
                format!("crates/{n}/Cargo.toml"),
                format!("[package]\nname = \"{n}\"\n"),
            ));
        }
        let set = derive("features", with_src(files)).expect("derivation");
        assert!(
            set.scan_dirs.contains(&"crates/aterm-on/src".to_string()),
            "default->fancy->aterm-mid/glitter->dep:aterm-on must be IN: {:?}",
            set.scan_dirs
        );
        assert!(
            set.scan_dirs.contains(&"crates/aterm-edge/src".to_string()),
            "edge features=[\"edgefeat\"] -> implicit optional dep must be IN: {:?}",
            set.scan_dirs
        );
        assert!(
            !set.scan_dirs.contains(&"crates/aterm-off/src".to_string()),
            "an unactivated optional dep (the spec-anchors shape) must stay OUT: {:?}",
            set.scan_dirs
        );
    }

    #[test]
    fn proc_macro_crates_are_classified_out_and_reported() {
        let files = test_fixtures::workspace_manifests(&[(
            "aterm-derive",
            "[lib]\nproc-macro = true\n\
             [dependencies]\naterm-macdep = { path = \"../aterm-macdep\" }\n",
        )]);
        // aterm-macdep only exists behind the proc-macro edge: it must NOT be
        // followed (compiler-host code), so no manifest for it is even needed.
        let set = derive("procmacro", with_src(files)).expect("derivation");
        assert!(
            !set.scan_dirs.iter().any(|d| d.contains("aterm-derive")),
            "proc-macro crates must not be scanned: {:?}",
            set.scan_dirs
        );
        assert!(
            !set.scan_dirs.iter().any(|d| d.contains("aterm-macdep")),
            "proc-macro deps must not be followed: {:?}",
            set.scan_dirs
        );
        assert_eq!(
            set.proc_macros,
            vec![(
                "aterm-derive".to_string(),
                "crates/aterm-derive".to_string()
            )],
            "the exclusion must be REPORTED, never silent"
        );
    }

    #[test]
    fn missing_dep_manifest_and_missing_src_fail_closed() {
        // (a) a path dep with no Cargo.toml on disk.
        let files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[dependencies]\naterm-ghost = { path = \"../aterm-ghost\" }\n",
        )]);
        let err = derive("ghost", with_src(files)).expect_err("must fail");
        assert!(err.contains("aterm-ghost/Cargo.toml"), "err: {err}");
        // (b) a derived crate whose src/ is missing (manifest present).
        let files = test_fixtures::workspace_manifests(&[("aterm-nosrc", "")]);
        let mut files = with_src(files);
        files.retain(|(p, _)| p != "crates/aterm-nosrc/src/lib.rs");
        let err = derive("nosrc", files).expect_err("must fail");
        assert!(
            err.contains("no src/ directory"),
            "the drift guard must hard-error: {err}"
        );
    }

    #[test]
    fn unclassifiable_dep_specs_fail_closed() {
        // A git dep in a NORMAL section: outside the modeled subset.
        let files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[dependencies]\nweird = { git = \"https://example.com/x\" }\n",
        )]);
        let err = derive("gitdep", with_src(files)).expect_err("must fail");
        assert!(err.contains("cannot soundly classify"), "err: {err}");
        // An unknown feature item.
        let files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[features]\ndefault = [\"nonexistent-thing\"]\n",
        )]);
        let err = derive("badfeat", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("neither a feature nor an optional dependency"),
            "err: {err}"
        );
    }

    #[test]
    fn exclusion_list_is_fail_closed_both_ways() {
        // (a) REVERSE: an UNREGISTERED [patch] path entry must hard-error
        // (a human has to review it, not the census silently skipping it).
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (p, c) in &mut files {
            if p == "Cargo.toml" {
                c.push_str("shiny-new-fork = { path = \"vendor/shiny-new-fork\" }\n");
            }
        }
        files.push((
            "vendor/shiny-new-fork/Cargo.toml".to_string(),
            "[package]\nname = \"shiny-new-fork\"\n".to_string(),
        ));
        let err = derive("unreviewed", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("NOT in REVIEWED_VENDORED_CRATES"),
            "err: {err}"
        );
        // (b) FORWARD: a registered exclusion missing from the patch table is
        // a STALE review and must hard-error.
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (p, c) in &mut files {
            if p == "Cargo.toml" {
                *c = c.replace("winit = { path = \"vendor/winit\" }\n", "");
            }
        }
        let err = derive("stale", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("no longer in the root [patch] table") && err.contains("winit"),
            "err: {err}"
        );
        // (c) FORWARD existence: the registered path must exist on disk.
        let mut files = test_fixtures::workspace_manifests(&[]);
        files.retain(|(p, _)| p != "vendor/winit/Cargo.toml");
        let err = derive("gone", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("no Cargo.toml on disk") && err.contains("winit"),
            "err: {err}"
        );
    }

    #[test]
    fn default_features_false_suppresses_and_union_reenables() {
        // One edge with default-features = false: the optional dep behind the
        // child's `default` stays OUT. Adding a second edge that keeps
        // defaults re-enables it (union semantics, like cargo).
        let (patch_table, base) = test_fixtures::patch_fixture();
        let mk = |second_edge: &str| {
            let mut files = base.clone();
            let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
            root.push_str("[workspace.dependencies]\n");
            for n in ["aterm-mid", "aterm-other", "aterm-opt"] {
                root.push_str(&format!("{n} = {{ path = \"crates/{n}\" }}\n"));
            }
            root.push_str(&patch_table);
            files.push(("Cargo.toml".to_string(), root));
            files.push((
                "crates/aterm-gui/Cargo.toml".to_string(),
                format!(
                    "[package]\nname = \"aterm-gui\"\n[dependencies]\n\
                     aterm-mid = {{ workspace = true, default-features = false }}\n\
                     {second_edge}"
                ),
            ));
            files.push((
                "crates/aterm-other/Cargo.toml".to_string(),
                "[package]\nname = \"aterm-other\"\n[dependencies]\n\
                 aterm-mid = { workspace = true }\n"
                    .to_string(),
            ));
            files.push((
                "crates/aterm-mid/Cargo.toml".to_string(),
                "[package]\nname = \"aterm-mid\"\n\
                 [features]\ndefault = [\"dep:aterm-opt\"]\n\
                 [dependencies]\naterm-opt = { workspace = true, optional = true }\n"
                    .to_string(),
            ));
            files.push((
                "crates/aterm-opt/Cargo.toml".to_string(),
                "[package]\nname = \"aterm-opt\"\n".to_string(),
            ));
            files
        };
        let set = derive("dfoff", with_src(mk(""))).expect("derivation");
        assert!(
            !set.scan_dirs.contains(&"crates/aterm-opt/src".to_string()),
            "default-features = false must suppress the default-activated \
             optional dep: {:?}",
            set.scan_dirs
        );
        let set = derive(
            "dfunion",
            with_src(mk("aterm-other = { workspace = true }\n")),
        )
        .expect("derivation");
        assert!(
            set.scan_dirs.contains(&"crates/aterm-opt/src".to_string()),
            "a second defaults-on edge must re-enable via union: {:?}",
            set.scan_dirs
        );
    }
}
