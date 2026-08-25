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
        package: "winnow",
        path: "vendor/winnow",
        mode: VendoredMode::Scanned {
            namespace: "winnow",
            platform_slices: &[],
            audit: "aterm-trust fork of upstream winnow (offset_from fix): three \
                    `writer.lock()` stderr-stream sites (combinator/debug/internals.rs), \
                    all behind the `debug` feature no aterm build activates — graphed \
                    anyway as `winnow::writer` (over-approximation, fail-closed: \
                    statement-shaped holds that nest nothing)",
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
    for (pkg, path) in &tables.patches {
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

    Ok(ScanSet {
        scan_dirs,
        proc_macros,
        vendored_scanned,
        vendored_build_only,
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

    /// The derived GUI-process closure, pinned. A LEGITIMATE dependency
    /// change (a new crate entering aterm-gui's graph, or one leaving it)
    /// just updates this pin — that is the automation working; the diff in
    /// review IS the audit trail. An UNEXPECTED delta (a crate vanishing
    /// that should still be linked, or appearing that should not) means the
    /// dependency graph or the derivation drifted: investigate before
    /// touching the pin.
    ///
    /// PROVENANCE of the current pin: equals, crate for crate, the manual
    /// 42-crate list this derivation replaced (itself derived 2026-07-13
    /// from `cargo tree -p aterm-gui --edges normal`, macOS host target) —
    /// verified equal at the switchover, and re-verified against
    /// `cargo tree --target all` (identical: no cfg-gated workspace path
    /// dep exists today, so the cfg-deps-IN decision is currently a no-op).
    #[test]
    fn derived_closure_matches_the_pinned_canary() {
        const PINNED: &[&str] = &[
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
            "crates/aterm-effects/src",
            "crates/aterm-error/src",
            "crates/aterm-ffi-types/src",
            "crates/aterm-gpu/src",
            "crates/aterm-grapheme/src",
            "crates/aterm-grid/src",
            "crates/aterm-gui/src",
            "crates/aterm-hash/src",
            "crates/aterm-lexicon/src",
            "crates/aterm-log/src",
            "crates/aterm-lz4/src",
            "crates/aterm-net/src",
            "crates/aterm-observe/src",
            "crates/aterm-parser/src",
            "crates/aterm-policy/src",
            "crates/aterm-predict/src",
            "crates/aterm-provenance/src",
            "crates/aterm-pty/src",
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
        // All six reviewed vendored forks classified: five scanned in
        // vendored-identity mode, one (pkg-config) build-time-only.
        let scanned: Vec<&str> = set
            .vendored_scanned
            .iter()
            .map(|v| v.package.as_str())
            .collect();
        assert_eq!(
            scanned,
            vec!["indexmap", "libm", "smol_str", "winit", "winnow"],
            "the scanned vendored set changed"
        );
        assert_eq!(
            set.vendored_build_only.len(),
            1,
            "pkg-config is the one build-dep-only patch"
        );
        assert_eq!(set.vendored_build_only[0].0, "pkg-config");
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
