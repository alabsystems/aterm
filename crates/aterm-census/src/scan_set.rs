// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scan-set derivation: the lock-order census's crate scope, derived from the
//! workspace manifests, so a crate entering or leaving aterm-gui's dependency
//! graph changes the scan set with no census edit.
//!
//! It computes the aterm-gui process closure (every workspace crate reachable
//! from `crates/aterm-gui` over normal dependency edges) by parsing the
//! checked-in `Cargo.toml`s directly. Not `cargo metadata`: a manifest parse
//! is offline, needs no toolchain, and takes milliseconds.
//!
//! Rules (an unclassifiable construct is a hard error, never a skip):
//!
//!   * Normal deps only: `[dependencies]` and `[target.'cfg(...)'.dependencies]`.
//!     Dev- and build-dependencies are excluded.
//!   * cfg-gated target deps are in on every host: lock discipline is not
//!     per-OS, and a `cfg(windows)` crate ships in the Windows build.
//!   * Only path deps are followed (`path = "..."`, or `workspace = true` whose
//!     workspace entry has a path); version deps are external code.
//!   * Features are resolved: an `optional = true` path dep is in iff
//!     aterm-gui's default build activates it (`dep:name` / `name/feat`,
//!     implicit optional-dep features, edge `features = [...]`,
//!     `default-features = false`, unified across edges as cargo does).
//!   * Proc-macro crates are classified out (they run inside rustc) and their
//!     deps are not followed; each exclusion is reported.
//!   * Vendored `[patch.*]` forks are classified by their `[[fork]]` block in
//!     the fork ledger, [`FORK_LEDGER`] ([`reviewed_forks`]): the one record
//!     of what aterm vendored, which forge's `[OB-17]` holds to the tree.
//!     Forks that link into the GUI process are scanned in vendored-identity
//!     mode: lock identities live in a per-crate namespace (`indexmap::…`).
//!     The review is fail-closed both ways: a vendored patch entry with no
//!     block, or a block whose package is missing from the patch table or from
//!     disk, is a hard error. Every classification is printed.
//!   * First-party `[patch.*]` targets (under `crates/`, with no evidence of
//!     redistribution) owe no review row; they are scanned unnamespaced if they
//!     link into the process. ABSORBED targets — upstream source aterm now owns
//!     as a workspace member, which keeps its upstream license and is named by
//!     the top-level NOTICE (`forge attest` holds those records) — owe no review
//!     row either, but until they are carved under first-party lock discipline
//!     they are scanned exactly as while vendored: [`ABSORBED_CRATES`] gives
//!     each its identity namespace and platform slices, fail-closed both ways.
//!     An absorbed crate renamed off its upstream name (`crates/aterm-window`,
//!     once `winit`) leaves the patch table and is found by its row's path.
//!     See [`classify_patch_target`]: a patch path under neither `vendor/` nor
//!     `crates/`, a `crates/` path with only one of the two absorption records,
//!     or a first-party or absorbed path carrying a review row, is a hard error.
//!   * Drift guard: every derived crate dir must contain `src/`.
//!
//! The manifest parser covers the TOML subset this workspace uses (line
//! comments, single-line basic strings, inline tables, multi-line arrays,
//! dotted `name.workspace = true` deps, and the one-table-per-dependency form
//! cargo itself writes, `[dependencies.<name>]` and
//! `[target.'cfg(…)'.dependencies.<name>]`, which the absorbed winit's
//! manifest keeps). A `package = ` rename is understood on any dependency, as
//! cargo reads it: the walk follows a path dependency by its path, never by
//! name, while the KEY is what features (`dep:<key>`, `<key>/feat`) and
//! `workspace = true` name (the workspace `serde` row is
//! `package = "aterm-serde"`, docs/AUDIT-third-party-2026-10-15.md SP.6).
//! Anything else in a dependency-bearing section (git deps, multi-line
//! strings, unknown spec keys) fails the derivation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// The process root whose dependency closure is the lock namespace: the
/// shipped aterm-gui binary.
pub const GUI_ROOT_CRATE: &str = "crates/aterm-gui";

/// A group of per-platform subtrees of a scanned vendored crate that the
/// shipped macOS GUI process does not compile (winit's non-macOS backends).
/// Their acquisition sites are counted under the label, never graphed: an
/// edge from code no shipped process links could close an unrepairable cycle.
/// When aterm ships a GUI for one of these platforms, its slice leaves this
/// list and is graphed in its own per-target run.
#[derive(Debug)]
pub struct PlatformSlice {
    /// The platform label (`linux`, `windows`, …) for the transcript.
    pub label: &'static str,
    /// Crate-relative paths (a directory or one `.rs` file) the macOS build
    /// does not compile. Each must exist on disk.
    pub paths: &'static [&'static str],
}

/// The fork ledger, repo-relative: one `[[fork]]` block per vendored
/// `[patch.crates-io]` entry, edited by hand, its comment block the review
/// note. Forge reads it whole (`aterm_forge::policy`, `[OB-17]` holds every
/// field to the tree); the census reads the review: `name`, `path` and the
/// `census.mode`/`census.namespace` keys ([`reviewed_forks`]).
pub const FORK_LEDGER: &str = "vendor/forge.toml";

/// A vendored `[patch.*]` fork, REVIEWED and classified by its `[[fork]]`
/// block in [`FORK_LEDGER`]. Its one census mode is `scanned`: it links into
/// the GUI process, every lock identity in its own `namespace`. A vendored fork
/// has no platform slices (the one crate that needed them, winit, is ABSORBED:
/// [`ABSORBED_CRATES`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewedFork {
    /// The crates-io package being replaced (the block's `name`).
    pub package: String,
    /// The replacement path, repo-relative (must match the patch table and
    /// exist on disk).
    pub path: String,
    /// The identity namespace prefix (`census.namespace`; unique across every
    /// scanned crate, and `::` cannot appear in a lexical receiver name, so no
    /// collision is possible).
    pub namespace: &'static str,
}

/// Every `[[fork]]` block of the ledger at `root`, as the census reads it. A
/// missing ledger reviews nothing (every vendored patch entry is then
/// unreviewed, which the derivation refuses); a block without a `name`, a
/// `path`, `census.mode = "scanned"` or a non-empty `census.namespace` is an
/// error naming it. Other keys are forge's, which refuses unknown ones.
pub fn reviewed_forks(root: &Path) -> Result<Vec<ReviewedFork>, String> {
    let text = match std::fs::read_to_string(root.join(FORK_LEDGER)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {FORK_LEDGER}: {e} (fail-closed)")),
    };
    let doc = parse_doc(&text, FORK_LEDGER)?;
    // Each `[[fork]]` block, with a `[fork.census]` table folded into the
    // block it follows as dotted keys.
    let mut blocks: Vec<Vec<(String, Value)>> = Vec::new();
    for (header, entries) in &doc.sections {
        match header.as_str() {
            "fork" => blocks.push(entries.clone()),
            "fork.census" => {
                let Some(block) = blocks.last_mut() else {
                    return Err(format!(
                        "{FORK_LEDGER}: a [fork.census] table before any [[fork]] block"
                    ));
                };
                block.extend(
                    entries
                        .iter()
                        .map(|(k, v)| (format!("census.{k}"), v.clone())),
                );
            }
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(blocks.len());
    for (i, block) in blocks.iter().enumerate() {
        let get = |key: &str| {
            block.iter().find_map(|(k, v)| match v {
                Value::Str(s) if k == key => Some(s.as_str()),
                _ => None,
            })
        };
        let at = format!("{FORK_LEDGER}: [[fork]] block {}", i + 1);
        let (Some(package), Some(path)) = (get("name"), get("path")) else {
            return Err(format!(
                "{at} has no `name` or no `path` string, so the census cannot tell which \
                 [patch] entry it reviews (fail-closed)"
            ));
        };
        if get("census.mode") != Some("scanned") {
            return Err(format!(
                "{at} (`{package}`) has census.mode {:?}; the census scans every vendored \
                 fork (`census.mode = \"scanned\"`, fail-closed)",
                get("census.mode")
            ));
        }
        let namespace = get("census.namespace").unwrap_or("").trim();
        if namespace.is_empty() {
            return Err(format!(
                "{at} (`{package}`) has no census.namespace: a scanned fork's lock \
                 identities must live in its own namespace (fail-closed)"
            ));
        }
        out.push(ReviewedFork {
            package: package.to_string(),
            path: path.to_string(),
            namespace: intern(namespace),
        });
    }
    Ok(out)
}

/// `s` for the life of the process, once per distinct string: the lock-order
/// census carries namespaces as `&'static str`, and the ledger names a handful.
fn intern(s: &str) -> &'static str {
    static NAMES: std::sync::OnceLock<std::sync::Mutex<BTreeSet<&'static str>>> =
        std::sync::OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = names.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// An ABSORBED crate as the lock-order census scans it: upstream source aterm
/// owns as a workspace member, either still patched in under its upstream name
/// ([`PatchTargetKind::Absorbed`]) or renamed and reached by path alone. Both
/// keep the absorption records (a retained upstream license in the crate root
/// and a NOTICE line naming it). It is a workspace member, so the path-dependency
/// closure reaches it, but its code was written to upstream's lock discipline,
/// not aterm's: its receiver names would merge with aterm's identities, and its
/// per-OS backends (which no single binary links together) would meet each
/// other through same-named fns, so the census would report cycles between
/// locks that never coexist (measured on the winit re-home: a
/// `shared_state`/`window_state` cycle from X11 into Windows). So it keeps the
/// vendored-identity mode it had under `vendor/`: one namespace, the platform
/// slices counted and labeled, never graphed. A row leaves when the carve
/// brings the crate under the first-party discipline.
pub struct AbsorbedCrate {
    /// The upstream package it was absorbed from, as NOTICE names it. While the
    /// member still answers to that name it is also the patch table key; a
    /// renamed member is in no patch row.
    pub upstream: &'static str,
    /// The member's path, repo-relative, under `crates/`.
    pub path: &'static str,
    /// The identity namespace prefix (unique across every namespaced crate).
    pub namespace: &'static str,
    /// Per-platform subtrees not compiled into the shipped macOS binary.
    pub platform_slices: &'static [PlatformSlice],
    /// What the crate is and why it is still namespaced.
    pub audit: &'static str,
}

/// The absorbed crates the census scans namespaced (see [`AbsorbedCrate`]).
/// An absorbed `[patch]` target without a row, or a row that is neither an
/// absorbed `[patch]` entry at that path nor an absorbed member there, is a hard
/// error.
pub const ABSORBED_CRATES: &[AbsorbedCrate] = &[AbsorbedCrate {
    upstream: "winit",
    path: "crates/aterm-window",
    namespace: "aterm_window",
    // The macOS GUI process compiles only the shared code, platform_impl/macos
    // and the macOS-gated platform/ extension modules (2 blocking `.lock()`
    // sites: event.rs InnerSizeWriter, macos/window_delegate.rs
    // new_inner_size). The per-OS backends below are cfg'd out of the macOS
    // binary and counted as slices; graphing them is the carve's (winit-core
    // steps 5 and 8, docs/AUDIT-third-party-2026-10-15.md).
    platform_slices: &[
        PlatformSlice {
            label: "linux",
            paths: &[
                "src/platform_impl/linux",
                "src/platform/x11.rs",
                "src/platform/wayland.rs",
            ],
        },
        PlatformSlice {
            label: "windows",
            paths: &["src/platform_impl/windows", "src/platform/windows.rs"],
        },
    ],
    audit: "winit 0.30.13, absorbed as crates/aterm-window (2026-10-15; vendored as \
            vendor/winit before; the package renamed aterm-window 2026-10-19): runs ON \
            the main thread of the GUI process; the macOS-compiled surface is graphed \
            under the `aterm_window::` namespace, the non-macOS backends are labeled \
            platform slices until the carve; the wasm cells do not compile it",
}];

/// What a `[patch.crates-io]` replacement is, which decides whether it owes a
/// review row. `vendor/` holds redistributed upstream source; `crates/` holds
/// workspace members. Any other location is unclassifiable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchTargetKind {
    /// Under `vendor/`: third-party source, owes a `[[fork]]` block in
    /// [`FORK_LEDGER`] and the whole provenance family (`targo --unverified
    /// forge attest`).
    Vendored,
    /// Under `crates/` with no marks of redistribution: code aterm wrote. Owes
    /// no review row, NOTICE line or `vendor/forge.toml` block.
    FirstParty,
    /// Under `crates/`, upstream source aterm ABSORBED: a workspace member it
    /// owns and changes like its own code, which still redistributes someone
    /// else's work. Both records are present: the retained upstream license
    /// file in the crate root and the top-level NOTICE naming the directory.
    /// Owes no review row or `vendor/forge.toml` block; `targo --unverified
    /// forge attest` holds its NOTICE line, license text and Apache-2.0 §4(b)
    /// modification notices.
    Absorbed,
}

/// Evidence that `dir` is third-party source this repository redistributes,
/// from the repository's own records rather than the directory's location.
/// Either signal suffices:
///
/// * a retained upstream license file in the crate root (the signal
///   `targo --unverified forge attest` uses); or
/// * a naming of the path in the top-level `NOTICE`.
fn redistribution_evidence(root: &std::path::Path, path: &str) -> Option<String> {
    if let Some(name) = retained_license(root, path) {
        return Some(format!(
            "it retains an upstream license file (`{path}/{name}`)"
        ));
    }
    if notice_names(root, path) {
        return Some(format!(
            "the top-level NOTICE names `{path}` as redistributed"
        ));
    }
    None
}

/// The first upstream license file (`LICENSE*`, `COPYING*`) in `path`'s root.
fn retained_license(root: &std::path::Path, path: &str) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(path))
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let upper = n.to_uppercase();
            upper.starts_with("LICENSE") || upper.starts_with("COPYING")
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

/// Does the top-level NOTICE name `path`?
fn notice_names(root: &std::path::Path, path: &str) -> bool {
    std::fs::read_to_string(root.join("NOTICE"))
        .unwrap_or_default()
        .contains(path)
}

/// Classify a `[patch.crates-io]` replacement path.
///
/// Location alone is not the test: `crates/aterm-lz4` is derived from
/// lz4_flex and recorded in NOTICE as a redistribution. A `crates/` path with
/// BOTH absorption records — a retained upstream license and a NOTICE line —
/// is [`PatchTargetKind::Absorbed`]: a human decided aterm owns it and recorded
/// that where the notary reads it. A `crates/` path with only one of the two
/// is an error (third-party source outside `vendor/` whose redistribution is
/// half-recorded needs a human), and any location other than `vendor/` or
/// `crates/` is an error naming both.
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
    if retained_license(root, path).is_some() && notice_names(root, path) {
        return Ok(PatchTargetKind::Absorbed);
    }
    if let Some(why) = redistribution_evidence(root, path) {
        return Err(format!(
            "[patch] entry `{pkg}` points at `{path}`, which is under `crates/` but is NOT \
             first-party code: {why}. Treating it as first-party would skip the whole \
             provenance family (NOTICE line, retained LICENSE, Apache-2.0 section 4(b) \
             byte-diff) for source this repository redistributes on somebody else's terms. \
             Fail-closed, because the two wrong answers are not symmetric: record the \
             absorption whole (the upstream license file in the crate root AND a NOTICE \
             line naming `{path}`), move the fork under `vendor/` where the review \
             obligations are enforced, or — if the evidence is a false positive — say why \
             in `aterm_census::scan_set::redistribution_evidence`"
        ));
    }
    Ok(PatchTargetKind::FirstParty)
}

/// One vendored or absorbed crate the census SCANS namespaced (resolved from
/// [`reviewed_forks`] or [`ABSORBED_CRATES`] against the tree): the
/// walker's vendored-identity-mode input.
#[derive(Debug)]
pub struct ScannedVendored {
    /// The package name (the patch table key, or a renamed absorbed member's
    /// own name).
    pub package: String,
    /// The identity namespace prefix (`aterm_window` ⇒ identities
    /// `aterm_window::…`).
    pub namespace: &'static str,
    /// Repo-relative crate dir (`vendor/indexmap`, `crates/aterm-window`).
    pub crate_dir: String,
    /// Repo-relative source dir (`<crate dir>/src`) — the walk root.
    pub scan_dir: String,
    /// Labeled per-platform subtrees to count, never graph.
    pub platform_slices: &'static [PlatformSlice],
}

/// The derived scan set plus its reported classifications (never silent).
#[derive(Debug)]
pub struct ScanSet {
    /// Repo-relative `<crate dir>/src` scan dirs, sorted — the walker's input.
    pub scan_dirs: Vec<String>,
    /// Excluded proc-macro crates: (package name, crate dir).
    pub proc_macros: Vec<(String, String)>,
    /// The reviewed vendored and the absorbed `[patch.*]` crates SCANNED in
    /// vendored-identity mode, sorted by package.
    pub vendored_scanned: Vec<ScannedVendored>,
    /// `[patch.*]` entries pointing at a first-party workspace member, sorted:
    /// (package, path). Printed, never review-registered; a member that links
    /// into the GUI process is already in `scan_dirs`, unnamespaced.
    pub first_party_patches: Vec<(String, String)>,
    /// The ABSORBED crates ([`ABSORBED_CRATES`]), sorted: (package, path), the
    /// package being the patch table key of one still patched in under its
    /// upstream name ([`PatchTargetKind::Absorbed`]) or a renamed member's own
    /// name. Listed apart from [`Self::first_party_patches`] because they still
    /// redistribute upstream source; scanned namespaced, in
    /// [`Self::vendored_scanned`].
    pub absorbed: Vec<(String, String)>,
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
    /// Anything else (numbers, …): a hard error wherever a dependency-bearing
    /// section is interpreted (rendered via Debug in those diagnostics).
    #[expect(
        dead_code,
        reason = "read only through the derived Debug in error paths"
    )]
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

/// Parse a manifest into sections and entries. Multi-line values are joined by
/// bracket depth; comments are stripped per physical line first. An entry that
/// does not parse is a hard error naming the file.
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
    /// The dependency key: what features name (`dep:<key>`, `<key>/feat`) and
    /// what `workspace = true` looks up in the root table.
    key: String,
    /// The package behind the key when `package = "…"` renames it; the key
    /// is the package name otherwise.
    package: Option<String>,
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

/// Is this section header a normal-dependency section (followed), a dev/build
/// one (excluded), or something else? A header mentioning dependencies in no
/// known shape is an error.
enum SectionKind {
    NormalDeps,
    ExcludedDeps,
    /// `[dependencies.<name>]` (or a `[target.….dependencies.<name>]`): ONE
    /// normal dependency, whose entries are its spec.
    NormalDep(String),
    /// The same one-table form of a dev- or build-dependency: excluded.
    ExcludedDep,
    Features,
    Lib,
    Package,
    WorkspaceDeps,
    Patch,
    Ignored,
}

fn classify_section(header: &str, file: &str) -> Result<SectionKind, String> {
    let h = header.trim();
    if let Some(kind) = dependency_section(h) {
        return Ok(kind);
    }
    Ok(match h {
        "features" => SectionKind::Features,
        "lib" => SectionKind::Lib,
        "package" => SectionKind::Package,
        "workspace.dependencies" => SectionKind::WorkspaceDeps,
        _ if h.starts_with("patch.") => SectionKind::Patch,
        _ if h.starts_with("target.") => {
            return Err(format!(
                "{file}: unrecognized target section `[{h}]` — the derivation cannot \
                 soundly classify it (fail-closed)"
            ));
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

/// A dependency section header, with or without its `target.<cfg or triple>.`
/// prefix (cfg-target deps are IN: platform-independent): the table form
/// (`dependencies`) or cargo's one-table-per-dependency form
/// (`dependencies.<name>`). `None` for anything else.
fn dependency_section(h: &str) -> Option<SectionKind> {
    let rest = match h.strip_prefix("target.") {
        Some(target) => {
            // The key is a quoted cfg (dots inside the quotes) or a bare triple.
            let after = match target.chars().next()? {
                quote @ ('\'' | '"') => {
                    let close = target[1..].find(quote)? + 1;
                    &target[close + 1..]
                }
                _ => &target[target.find('.')?..],
            };
            after.strip_prefix('.')?
        }
        None => h,
    };
    let (table, name) = match rest.split_once('.') {
        Some((table, name)) => (table, Some(name.trim_matches(['"', '\'']))),
        None => (rest, None),
    };
    match (table, name) {
        ("dependencies", None) => Some(SectionKind::NormalDeps),
        ("dependencies", Some(name)) if !name.is_empty() && !name.contains('.') => {
            Some(SectionKind::NormalDep(name.to_string()))
        }
        ("dev-dependencies" | "build-dependencies", None) => Some(SectionKind::ExcludedDeps),
        ("dev-dependencies" | "build-dependencies", Some(name))
            if !name.is_empty() && !name.contains('.') =>
        {
            Some(SectionKind::ExcludedDep)
        }
        _ => None,
    }
}

/// Interpret one `key = value` entry of a dependency section as a [`DepEdge`],
/// including the dotted `name.workspace = true` form. Unknown spec keys (git,
/// registry, …) are hard errors.
fn dep_edge(key: &str, value: &Value, file: &str) -> Result<DepEdge, String> {
    // Dotted form: `aterm-log.workspace = true`.
    if let Some(name) = key.strip_suffix(".workspace") {
        if matches!(value, Value::Bool(true)) {
            return Ok(DepEdge {
                key: name.to_string(),
                package: None,
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
        package: None,
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
                    // The package behind a renamed key. The walk follows a
                    // path dependency by its path, and features name the key,
                    // so a rename changes neither.
                    ("package", Value::Str(p)) => edge.package = Some(p.clone()),
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
                             derivation cannot soundly classify (git/registry deps are out \
                             of the modeled subset — fail-closed)"
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
            SectionKind::NormalDep(name) => {
                m.deps
                    .push(dep_edge(&name, &Value::Table(entries.clone()), &file_rel)?);
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
            | SectionKind::ExcludedDep
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

/// The package name of every normal dependency of `<root>/<dir>/Cargo.toml`
/// (`[dependencies]` and the `[target.…]` tables; dev and build excluded,
/// optional ones included) that is not a path dependency (directly, or through
/// a `workspace = true` row with a path): what `Cargo.lock` must answer for,
/// since the closure walk follows the path ones itself. A renamed dependency
/// answers with the package it names, so `gui = { package = "winit", … }` is
/// still `winit`.
pub(crate) fn external_dependency_names(root: &Path, dir: &str) -> Result<Vec<String>, String> {
    let tables = load_root_tables(root)?;
    Ok(load_manifest(root, dir)?
        .deps
        .into_iter()
        .filter(|edge| {
            edge.path.is_none()
                && !(edge.workspace
                    && tables
                        .ws_deps
                        .get(&edge.key)
                        .is_some_and(|ws| ws.dir.is_some()))
        })
        .map(|edge| edge.package.unwrap_or(edge.key))
        .collect())
}

/// The workspace members (`crates/…`) the crate at `dir` reaches through its normal
/// dependencies, transitively, each with the chain of crate names from `dir`. Optional
/// dependencies are followed and features ignored, so the reach is an upper bound of any
/// build's closure. A vendored crate (outside `crates/`) is a leaf: it belongs to another
/// workspace, which no crate of this one sits below. Fail-closed on a manifest it cannot
/// read or a `workspace = true` the root table does not answer.
pub(crate) fn member_reach(
    root: &Path,
    dir: &str,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let tables = load_root_tables(root)?;
    let mut reach: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([(dir.to_string(), Vec::new())]);
    while let Some((at, chain)) = queue.pop_front() {
        let manifest = load_manifest(root, &at)?;
        for edge in &manifest.deps {
            let child = if edge.workspace {
                let Some(ws) = tables.ws_deps.get(&edge.key) else {
                    return Err(format!(
                        "{at}/Cargo.toml: `{}` says `workspace = true` but the root \
                         [workspace.dependencies] has no such entry (fail-closed)",
                        edge.key
                    ));
                };
                ws.dir.clone()
            } else {
                edge.path
                    .as_deref()
                    .map(|p| normalize_rel(&at, p))
                    .transpose()?
            };
            let Some(child) = child.filter(|c| c.starts_with("crates/")) else {
                continue;
            };
            if child == dir || reach.contains_key(&child) {
                continue;
            }
            let mut through = chain.clone();
            through.push(edge.package.clone().unwrap_or_else(|| edge.key.clone()));
            reach.insert(child.clone(), through.clone());
            queue.push_back((child, through));
        }
    }
    Ok(reach)
}

/// Every `[patch]` target that is a workspace member (under `crates/`):
/// package → repo-relative path. `Cargo.lock` lists a member's dev-dependencies
/// beside its normal ones, so a walk that reaches one reads its manifest.
pub(crate) fn member_patch_targets(root: &Path) -> Result<BTreeMap<String, String>, String> {
    Ok(load_root_tables(root)?
        .patches
        .into_values()
        .filter(|(_, path)| path.starts_with("crates/"))
        .collect())
}

/// One `[[package]]` of `Cargo.lock`.
#[derive(Clone, Debug)]
pub(crate) struct LockPackage {
    pub(crate) name: String,
    pub(crate) version: String,
    /// `None` for a path package (a workspace member or a `[patch]` target).
    pub(crate) source: Option<String>,
    /// Each dependency as the lock spells it: the name, and the version when
    /// the lock holds more than one of that name.
    pub(crate) deps: Vec<(String, Option<String>)>,
}

/// The checkout's `Cargo.lock`, read with the manifest reader. Unreadable or
/// malformed is an error (fail-closed): the walks it serves cannot vouch for a
/// closure they cannot see.
pub(crate) fn read_lock(root: &Path) -> Result<Vec<LockPackage>, String> {
    let file = "Cargo.lock";
    let text = std::fs::read_to_string(root.join(file))
        .map_err(|e| format!("cannot read {file}: {e} (fail-closed)"))?;
    let doc = parse_doc(&text, file)?;
    let mut out = Vec::new();
    for (header, entries) in doc.sections.iter().filter(|(h, _)| h == "package") {
        let get = |key: &str| {
            entries.iter().find_map(|(k, v)| match v {
                Value::Str(s) if k == key => Some(s.clone()),
                _ => None,
            })
        };
        let (Some(name), Some(version)) = (get("name"), get("version")) else {
            return Err(format!(
                "{file}: a [[{header}]] with no name or version (fail-closed)"
            ));
        };
        let mut deps = Vec::new();
        match entries
            .iter()
            .find(|(k, _)| k == "dependencies")
            .map(|(_, v)| v)
        {
            None => {}
            Some(Value::Array(items)) => {
                for item in items {
                    let Value::Str(spec) = item else {
                        return Err(format!(
                            "{file}: `{name}`'s dependencies hold a non-string (fail-closed)"
                        ));
                    };
                    let mut words = spec.split_whitespace();
                    let dep = words.next().unwrap_or("").to_string();
                    deps.push((dep, words.next().map(str::to_string)));
                }
            }
            Some(_) => {
                return Err(format!(
                    "{file}: `{name}`'s dependencies are not an array (fail-closed)"
                ));
            }
        }
        out.push(LockPackage {
            source: get("source"),
            name,
            version,
            deps,
        });
    }
    Ok(out)
}

/// Where the manifest `text` (at `file`) turns on `dep`'s `feature` for a
/// shipped build: an edge in `[dependencies]`, a `[target.…]` table or
/// `[workspace.dependencies]` listing it, or a `[features]` item `dep/feature`
/// (`dep?/feature`). Dev and build edges are not shipped and are not read.
pub(crate) fn shipped_feature_enablers(
    text: &str,
    file: &str,
    dep: &str,
    feature: &str,
) -> Result<Vec<String>, String> {
    let doc = parse_doc(text, file)?;
    let forwards = [format!("{dep}/{feature}"), format!("{dep}?/{feature}")];
    let mut out = Vec::new();
    for (header, entries) in &doc.sections {
        if header.is_empty() {
            continue;
        }
        match classify_section(header, file)? {
            SectionKind::NormalDeps | SectionKind::WorkspaceDeps => {
                for (k, v) in entries {
                    if k != dep {
                        continue;
                    }
                    if dep_edge(k, v, file)?.features.iter().any(|f| f == feature) {
                        out.push(format!(
                            "[{header}] {dep} = {{ features = [\"{feature}\"] }}"
                        ));
                    }
                }
            }
            SectionKind::Features => {
                for (k, v) in entries {
                    let Value::Array(items) = v else { continue };
                    if items
                        .iter()
                        .any(|it| matches!(it, Value::Str(s) if forwards.contains(s)))
                    {
                        out.push(format!("[{header}] {k} forwards {dep}/{feature}"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
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
    /// `[patch.*]` entries with a `path` redirect: row key -> (the package it
    /// replaces, repo-relative path). The package is the key unless the row is
    /// renamed (`package = "…"`).
    patches: BTreeMap<String, (String, String)>,
}

/// A `[patch]` row's spec with a `package = "…"` rename taken out, and the
/// package it replaces (the row key when there is none).
///
/// A renamed row is cargo's form for patching two version lines of one
/// package (`windows-sys-052 = { package = "windows-sys", path = … }`). The
/// closure walk never follows a patch row by name, and every use of the row
/// below asks for the PACKAGE it replaces.
fn patch_row(key: &str, value: &Value) -> (String, Value) {
    let Value::Table(pairs) = value else {
        return (key.to_string(), value.clone());
    };
    let mut package = key.to_string();
    let mut rest = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        match (k.as_str(), v) {
            ("package", Value::Str(p)) => package = p.clone(),
            _ => rest.push((k.clone(), v.clone())),
        }
    }
    (package, Value::Table(rest))
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
                    let (package, spec) = patch_row(k, v);
                    let edge = dep_edge(k, &spec, file_rel)?;
                    let Some(p) = edge.path else {
                        return Err(format!(
                            "{file_rel}: `[patch]` entry `{k}` is not a path redirect — \
                             the derivation cannot classify it (fail-closed; review it)"
                        ));
                    };
                    out.patches
                        .insert(k.clone(), (package, normalize_rel("", &p)?));
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
    /// consumed only for deps that end up included, as in cargo).
    dep_requests: BTreeMap<String, BTreeSet<String>>,
}

/// Expand a crate's requested features (plus `default` when enabled) through
/// its feature table. An item that is neither a feature, an optional dep, nor
/// a dep-feature reference is a hard error.
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
/// `root`: a pure function of the checked-in TOML.
/// Every error is a hard failure of the census.
pub fn derive_gui_scan_set(root: &Path) -> Result<ScanSet, String> {
    derive_process_scan_set(root, &[GUI_ROOT_CRATE])
}

/// Derive the process closure of `root_crates`. A multi-root process (the wasm
/// renderer page, whose three cdylib modules load into one Electron renderer)
/// seeds every root and takes the union with cargo's feature unification.
/// [`derive_gui_scan_set`] is this with the single [`GUI_ROOT_CRATE`] seed.
pub fn derive_process_scan_set(root: &Path, root_crates: &[&str]) -> Result<ScanSet, String> {
    let tables = load_root_tables(root)?;
    let reviewed = reviewed_forks(root)?;

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

    // ---- Vendored [patch.*] forks: the ledger's review, fail-closed both
    // ways.
    let mut vendored_scanned: Vec<ScannedVendored> = Vec::new();
    let mut first_party_patches: Vec<(String, String)> = Vec::new();
    let mut absorbed: Vec<(String, String)> = Vec::new();
    for (pkg, path) in tables.patches.values() {
        // Only vendored source owes a review row; a patch pointing at a
        // workspace member, first-party or absorbed, is covered by the closure
        // derivation above.
        let kind = classify_patch_target(pkg, path, root)?;
        if kind != PatchTargetKind::Vendored {
            let what = if kind == PatchTargetKind::Absorbed {
                "absorbed"
            } else {
                "first-party"
            };
            if reviewed.iter().any(|r| r.package == *pkg) {
                return Err(format!(
                    "[patch] entry `{pkg}` points at {what} `{path}` but ALSO has a \
                     `[[fork]]` block in {FORK_LEDGER} — the ledger records vendored \
                     third-party code this repository must keep reviewing, and a \
                     workspace member is not that. Remove the block (fail-closed: a false \
                     provenance claim is as wrong as a missing one)"
                ));
            }
            if !root.join(path).join("Cargo.toml").is_file() {
                return Err(format!(
                    "[patch] entry `{pkg}` points at {what} `{path}`, which has no \
                     Cargo.toml on disk — cargo cannot resolve this workspace at all \
                     (fail-closed)"
                ));
            }
            if kind == PatchTargetKind::Absorbed {
                let Some(row) = ABSORBED_CRATES.iter().find(|a| a.upstream == pkg) else {
                    return Err(format!(
                        "[patch] entry `{pkg}` points at absorbed `{path}`, which has no \
                         ABSORBED_CRATES row — the census must be told how to scan upstream \
                         code it does not hold to aterm's lock discipline (its identity \
                         namespace and platform slices): add the row in \
                         crates/aterm-census/src/scan_set.rs (fail-closed, never silent)"
                    ));
                };
                if row.path != path {
                    return Err(format!(
                        "ABSORBED_CRATES entry `{pkg}` is registered at `{}` but the [patch] \
                         table says `{path}` — the row is STALE; update it",
                        row.path
                    ));
                }
                vendored_scanned.push(scan_absorbed(root, row, pkg, &mut scan_dirs)?);
                absorbed.push((pkg.clone(), path.clone()));
            } else {
                first_party_patches.push((pkg.clone(), path.clone()));
            }
            continue;
        }
        let Some(fork) = reviewed.iter().find(|r| r.package == *pkg) else {
            return Err(format!(
                "[patch] entry `{pkg}` (path `{path}`) has NO `[[fork]]` block in \
                 {FORK_LEDGER} — a vendored fork links into the process unreviewed, so a \
                 human must review it: add its block, the review note in the comment \
                 above it and `census.mode = \"scanned\"` with its `census.namespace` \
                 (fail-closed, never silent)"
            ));
        };
        if fork.path != *path {
            return Err(format!(
                "{FORK_LEDGER} reviews `{pkg}` at path `{}` but the [patch] table now \
                 says `{path}` — the review is STALE; re-audit and update the block",
                fork.path
            ));
        }
        let scan_dir = format!("{path}/src");
        if !root.join(&scan_dir).is_dir() {
            return Err(format!(
                "scanned vendored crate `{pkg}` has no `{scan_dir}` directory on disk — \
                 the review and the tree have drifted (fail-closed)"
            ));
        }
        vendored_scanned.push(ScannedVendored {
            package: pkg.clone(),
            namespace: fork.namespace,
            crate_dir: path.clone(),
            scan_dir,
            platform_slices: &[],
        });
    }
    for r in &reviewed {
        if !tables.patches.values().any(|(pkg, _)| *pkg == r.package) {
            return Err(format!(
                "{FORK_LEDGER}'s `[[fork]] {}` is no longer in the root [patch] table — a \
                 STALE review cannot keep describing the tree; remove or update the block \
                 (fail-closed)",
                r.package
            ));
        }
        if !root.join(&r.path).join("Cargo.toml").is_file() {
            return Err(format!(
                "{FORK_LEDGER}'s `[[fork]] {}` points at `{}`, which has no Cargo.toml on \
                 disk — the review is STALE (fail-closed)",
                r.package, r.path
            ));
        }
    }

    // ---- Absorbed crates renamed off their upstream name: no [patch] row
    // serves them and cargo reaches them by path, so each is found by its
    // row's path, and must still carry both absorption records there.
    for row in ABSORBED_CRATES {
        if absorbed.iter().any(|(_, p)| p == row.path) {
            continue;
        }
        let member = root.join(row.path).join("Cargo.toml").is_file();
        if !(member && retained_license(root, row.path).is_some() && notice_names(root, row.path)) {
            return Err(format!(
                "ABSORBED_CRATES entry `{}` (`{}`) is neither an absorbed [patch] target nor an \
                 absorbed member of this tree (a crate there that retains its upstream license \
                 file AND that the top-level NOTICE names) — a STALE row cannot keep \
                 describing it; remove or update the entry (fail-closed)",
                row.upstream, row.path
            ));
        }
        let package = match manifests.get(row.path) {
            Some(m) => m.name.clone(),
            None => load_manifest(root, row.path)?.name,
        };
        vendored_scanned.push(scan_absorbed(root, row, &package, &mut scan_dirs)?);
        absorbed.push((package, row.path.to_string()));
    }
    // Namespace uniqueness: two crates sharing a namespace would merge their
    // identities.
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
    vendored_scanned.sort_by(|a, b| a.package.cmp(&b.package));
    first_party_patches.sort();
    absorbed.sort();

    Ok(ScanSet {
        scan_dirs,
        proc_macros,
        vendored_scanned,
        first_party_patches,
        absorbed,
    })
}

/// One [`ABSORBED_CRATES`] row resolved for the walker: every platform-slice
/// path must exist, and the crate's source leaves `scan_dirs` (it is a member,
/// so the closure walk may have reached it), scanned namespaced instead, never
/// twice.
fn scan_absorbed(
    root: &Path,
    row: &AbsorbedCrate,
    package: &str,
    scan_dirs: &mut Vec<String>,
) -> Result<ScannedVendored, String> {
    for slice in row.platform_slices {
        for p in slice.paths {
            if !root.join(row.path).join(p).exists() {
                return Err(format!(
                    "ABSORBED_CRATES entry `{}` registers platform slice `{}` path `{p}`, \
                     which no longer exists under `{}` — the row is STALE (fail-closed)",
                    row.upstream, slice.label, row.path
                ));
            }
        }
    }
    let scan_dir = format!("{}/src", row.path);
    scan_dirs.retain(|d| *d != scan_dir);
    Ok(ScannedVendored {
        package: package.to_string(),
        namespace: row.namespace,
        crate_dir: row.path.to_string(),
        scan_dir,
        platform_slices: row.platform_slices,
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
            "      vendored [patch] and absorbed crate(s) SCANNED in vendored-identity \
             mode (vendor/forge.toml's [[fork]] blocks and ABSORBED_CRATES, fail-closed both \
             ways) — upstream process code, each crate's lock identities in its own \
             `<crate>::…` namespace so foreign receiver names can never merge with \
             aterm identities or each other: {}",
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
    if !set.absorbed.is_empty() {
        let listed: Vec<String> = set
            .absorbed
            .iter()
            .map(|(n, p)| format!("{n} ({p})"))
            .collect();
        let _ = writeln!(
            out,
            "      ABSORBED crate(s) — upstream source aterm owns as a workspace member \
             (retained upstream license + NOTICE line; `forge attest` holds its notices), \
             patched in under the upstream name or renamed and reached by path, no review \
             row; scanned NAMESPACED with its platform slices (ABSORBED_CRATES) until the \
             carve, listed with the vendored crates above: {}",
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
    //! derivation accepts, used by this module's tests and the lock_order
    //! synthetic-tree tests.

    use std::path::{Path, PathBuf};

    use super::{ABSORBED_CRATES, AbsorbedCrate, FORK_LEDGER};

    /// The vendored fork every fixture reviews: (package, path, namespace).
    pub(crate) const FIXTURE_FORK: (&str, &str, &str) = ("indexmap", "vendor/indexmap", "indexmap");

    /// A fork ledger reviewing `forks` (package, path, namespace), in the
    /// shape `vendor/forge.toml` writes them.
    pub(crate) fn ledger(forks: &[(&str, &str, &str)]) -> String {
        let mut text = String::from("[forge]\nloc_method = \"x\"\n");
        for (package, path, namespace) in forks {
            text.push_str(&format!(
                "\n# census review: a fixture fork\n[[fork]]\nname = \"{package}\"\n\
                 version = \"1.0.0\"\npath = \"{path}\"\nlicense = \"MIT\"\n\
                 apache_notice = false\ncensus.mode = \"scanned\"\n\
                 census.namespace = \"{namespace}\"\n"
            ));
        }
        text
    }

    /// This checkout's root (aterm-census lives two levels under it).
    pub(crate) fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-census lives two levels under the repo root")
            .to_path_buf()
    }

    /// Write `files` (relative path, contents) under a fresh temp dir named for
    /// `prefix`, `name` and this process, and return it; the caller removes it.
    /// Each module keeps its own `prefix`, so concurrent tests in different
    /// modules never share a dir.
    pub(crate) fn synth_tree(prefix: &str, name: &str, files: &[(String, String)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{prefix}-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
            std::fs::write(&path, contents).expect("write synth file");
        }
        root
    }

    /// The package name a fixture gives an absorbed crate's member: its
    /// directory's, as aterm names its crates (`crates/aterm-window` ⇒
    /// `aterm-window`).
    pub(crate) fn member_name(row: &AbsorbedCrate) -> &'static str {
        row.path.rsplit('/').next().unwrap_or(row.path)
    }

    /// The `[patch.crates-io]` table and vendor stub trees that satisfy the
    /// reviewed-classification checks: [`FIXTURE_FORK`] patched in, reviewed by
    /// a ledger and with a stub `src/`. Each absorbed crate is a renamed
    /// `crates/` member (in no patch row, named by [`member_name`]) with both
    /// absorption records (a LICENSE in its root and a NOTICE line naming it)
    /// and every platform-slice path existing.
    pub(crate) fn patch_fixture() -> (String, Vec<(String, String)>) {
        let mut table = String::from("[patch.crates-io]\n");
        let (package, path, _) = FIXTURE_FORK;
        table.push_str(&format!("{package} = {{ path = \"{path}\" }}\n"));
        let mut stubs = vec![
            (
                format!("{path}/Cargo.toml"),
                format!("[package]\nname = \"{package}\"\n"),
            ),
            (format!("{path}/src/lib.rs"), "// stub\n".to_string()),
            (FORK_LEDGER.to_string(), ledger(&[FIXTURE_FORK])),
        ];
        let mut notice = String::from("NOTICE\n");
        for a in ABSORBED_CRATES {
            notice.push_str(&format!("- {} (`{}/`)\n", a.upstream, a.path));
            stubs.push((
                format!("{}/Cargo.toml", a.path),
                format!("[package]\nname = \"{}\"\n", member_name(a)),
            ));
            stubs.push((format!("{}/LICENSE", a.path), "upstream\n".to_string()));
            stubs.push((format!("{}/src/lib.rs", a.path), "// stub\n".to_string()));
            for slice in a.platform_slices {
                for p in slice.paths {
                    let rel = if p.ends_with(".rs") {
                        format!("{}/{p}", a.path)
                    } else {
                        format!("{}/{p}/stub.rs", a.path)
                    };
                    stubs.push((rel, "// stub\n".to_string()));
                }
            }
        }
        stubs.push(("NOTICE".to_string(), notice));
        (table, stubs)
    }

    /// A minimal derivable workspace: aterm-gui depending on aterm-types plus
    /// `extra` crates (`(name, extra manifest sections)`), with the patch
    /// fixture. Returns (repo-relative path, contents); src files are the
    /// caller's (the drift guard needs each crate to have `src/`).
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
    use crate::scan_set::test_fixtures::synth_tree;

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
        let root = synth_tree("aterm-scan-set", name, &files);
        let out = derive_gui_scan_set(&root);
        let _ = std::fs::remove_dir_all(&root);
        out
    }

    #[test]
    fn scanned_vendored_crate_missing_src_or_slice_path_fails_closed() {
        // (a) a Scanned entry whose src/ is gone: stale registration.
        let mut files = test_fixtures::workspace_manifests(&[]);
        files.retain(|(p, _)| !p.starts_with("vendor/indexmap/src/"));
        let err = derive("nosrcvendor", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("no `vendor/indexmap/src` directory"),
            "err: {err}"
        );
        // (b) a registered platform-slice path gone from disk: stale row.
        let mut files = test_fixtures::workspace_manifests(&[]);
        files.retain(|(p, _)| p != "crates/aterm-window/src/platform/x11.rs");
        let err = derive("staleslice", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("platform slice `linux` path `src/platform/x11.rs`")
                && err.contains("STALE"),
            "err: {err}"
        );
    }

    /// The fixture tree with `dep` (a workspace dependency on the absorbed
    /// crate, by path) in the root table and aterm-gui's dependencies, so the
    /// closure walk reaches the crate as a member.
    fn reaching_absorbed(dep: &str, path: &str) -> Vec<(String, String)> {
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (rel, contents) in files.iter_mut() {
            if rel == "Cargo.toml" {
                *contents = contents.replace(
                    "aterm-types = { path = \"crates/aterm-types\" }\n",
                    &format!(
                        "aterm-types = {{ path = \"crates/aterm-types\" }}\n\
                         {dep} = {{ path = \"{path}\" }}\n"
                    ),
                );
            }
            if rel == "crates/aterm-gui/Cargo.toml" {
                contents.push_str(&format!("{dep} = {{ workspace = true }}\n"));
            }
        }
        files
    }

    /// Assert `set` scans the absorbed crate namespaced under `package`, with
    /// its slices, and never also as a plain member.
    fn assert_scanned_namespaced(set: &ScanSet, row: &AbsorbedCrate, package: &str) {
        assert_eq!(
            set.absorbed,
            vec![(package.to_string(), row.path.to_string())]
        );
        assert!(
            !set.scan_dirs.contains(&format!("{}/src", row.path)),
            "an absorbed crate is not scanned unnamespaced: {:?}",
            set.scan_dirs
        );
        let scanned = set
            .vendored_scanned
            .iter()
            .find(|v| v.package == package)
            .expect("the absorbed crate is scanned namespaced");
        assert_eq!(scanned.namespace, row.namespace);
        assert_eq!(scanned.crate_dir, row.path);
        assert_eq!(scanned.platform_slices.len(), row.platform_slices.len());
    }

    /// An absorbed crate renamed off its upstream name is in no patch row; it
    /// is found by its row's path and scanned namespaced, with its slices, and
    /// never also as a plain member, even when the closure reaches it.
    #[test]
    fn a_renamed_absorbed_member_is_scanned_namespaced_not_as_a_member() {
        let row = &ABSORBED_CRATES[0];
        let member = test_fixtures::member_name(row);
        let files = reaching_absorbed(member, row.path);
        let set = derive("absorbedmember", with_src(files)).expect("derivation must succeed");
        assert_scanned_namespaced(&set, row, member);
    }

    /// An absorbed crate still patched in under its upstream name is scanned
    /// the same way, found by its patch row.
    #[test]
    fn an_absorbed_patch_target_is_scanned_namespaced_not_as_a_member() {
        let row = &ABSORBED_CRATES[0];
        let mut files = reaching_absorbed(row.upstream, row.path);
        for (rel, contents) in files.iter_mut() {
            if rel == "Cargo.toml" {
                contents.push_str(&format!(
                    "{} = {{ path = \"{}\" }}\n",
                    row.upstream, row.path
                ));
            }
            if *rel == format!("{}/Cargo.toml", row.path) {
                *contents = format!("[package]\nname = \"{}\"\n", row.upstream);
            }
        }
        let set = derive("absorbedpatch", with_src(files)).expect("derivation must succeed");
        assert_scanned_namespaced(&set, row, row.upstream);
    }

    /// An absorbed target with no ABSORBED_CRATES row, and a row whose crate
    /// lost an absorption record (here its NOTICE line), both stop the census.
    #[test]
    fn the_absorbed_registry_is_fail_closed_both_ways() {
        let absorbed = &ABSORBED_CRATES[0];
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (path, contents) in files.iter_mut() {
            if path == "Cargo.toml" {
                contents.push_str("absorbed-new = { path = \"crates/aterm-absorbed-new\" }\n");
            }
            if path == "NOTICE" {
                contents.push_str("- absorbed-new (`crates/aterm-absorbed-new/`)\n");
            }
        }
        files.push((
            "crates/aterm-absorbed-new/Cargo.toml".to_string(),
            "[package]\nname = \"absorbed-new\"\n".to_string(),
        ));
        files.push((
            "crates/aterm-absorbed-new/LICENSE-MIT".to_string(),
            "MIT\n".to_string(),
        ));
        let err = derive("unrowed", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("`absorbed-new`") && err.contains("no ABSORBED_CRATES row"),
            "err: {err}"
        );
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (path, contents) in files.iter_mut() {
            if path == "NOTICE" {
                *contents = contents.replace(
                    &format!("- {} (`{}/`)\n", absorbed.upstream, absorbed.path),
                    "",
                );
            }
        }
        let err = derive("staleabsorbed", with_src(files)).expect_err("must fail");
        assert!(
            err.contains(&format!("ABSORBED_CRATES entry `{}`", absorbed.upstream))
                && err.contains("STALE"),
            "err: {err}"
        );
    }

    /// A `[patch]` entry pointing at a workspace member is first-party: it is
    /// recorded, owes no review row, and a review row for it is refused.
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
        // And it is not counted as a vendored fork.
        assert!(set.vendored_scanned.iter().all(|v| v.package != "shimmed"));
    }

    /// A RENAMED patch row (cargo's form for two version lines of one package)
    /// is recorded under the package it replaces, not its key.
    #[test]
    fn a_renamed_patch_row_is_recorded_under_the_package_it_replaces() {
        let mut files = test_fixtures::workspace_manifests(&[("aterm-shim-052", "")]);
        for (path, contents) in files.iter_mut() {
            if path == "Cargo.toml" {
                contents.push_str(
                    "shimmed-052 = { package = \"shimmed\", path = \"crates/aterm-shim-052\" }\n",
                );
            }
        }
        let set = derive("renamed", with_src(files)).expect("derivation must succeed");
        assert_eq!(
            set.first_party_patches,
            vec![("shimmed".to_string(), "crates/aterm-shim-052".to_string())]
        );
    }

    /// A patch path under neither `vendor/` nor `crates/` is unclassifiable
    /// and stops the census.
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
        let (package, path, _) = test_fixtures::FIXTURE_FORK;
        let mut files = test_fixtures::workspace_manifests(&[("aterm-shim", "")]);
        for (rel, contents) in files.iter_mut() {
            if rel == "Cargo.toml" {
                *contents = contents.replace(
                    &format!("{package} = {{ path = \"{path}\" }}"),
                    &format!("{package} = {{ path = \"crates/aterm-shim\" }}"),
                );
            }
        }
        let err = derive("misfiled", with_src(files)).expect_err("must fail");
        assert!(
            err.contains(&format!("`{package}` points at first-party"))
                && err.contains("ALSO has a `[[fork]]` block"),
            "err: {err}"
        );
    }

    /// Under `crates/`, a retained upstream license file or a NOTICE line
    /// ALONE marks a half-recorded redistribution: refused, naming the evidence.
    /// Both together are an absorbed crate.
    #[test]
    fn a_redistribution_under_crates_is_refused_not_called_first_party() {
        let root = synth_tree(
            "aterm-scan-set",
            "redistribution",
            &[
                (
                    "crates/aterm-fork/LICENSE-MIT".to_string(),
                    "MIT\n".to_string(),
                ),
                (
                    "crates/aterm-noticed/src/lib.rs".to_string(),
                    "// derived\n".to_string(),
                ),
                (
                    "NOTICE".to_string(),
                    "crates/aterm-noticed is derived from upstream\n".to_string(),
                ),
            ],
        );
        let license = classify_patch_target("fork", "crates/aterm-fork", &root);
        let notice = classify_patch_target("noticed", "crates/aterm-noticed", &root);
        std::fs::write(root.join("crates/aterm-noticed/LICENSE"), "upstream\n")
            .expect("write license");
        let both = classify_patch_target("noticed", "crates/aterm-noticed", &root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(both, Ok(PatchTargetKind::Absorbed));
        let license = license.expect_err("a retained license is a redistribution");
        assert!(
            license.contains("crates/aterm-fork/LICENSE-MIT"),
            "{license}"
        );
        let notice = notice.expect_err("a NOTICE line is a redistribution");
        assert!(notice.contains("NOTICE"), "{notice}");
    }

    /// Negative control: a crate with no retained license file and no NOTICE
    /// line classifies FirstParty.
    #[test]
    fn a_genuine_first_party_crate_is_not_mistaken_for_a_redistribution() {
        let root = synth_tree(
            "aterm-scan-set",
            "own",
            &[
                (
                    "crates/aterm-own/src/lib.rs".to_string(),
                    "// original\n".to_string(),
                ),
                (
                    "NOTICE".to_string(),
                    "crates/aterm-other is derived from upstream\n".to_string(),
                ),
            ],
        );
        let evidence = redistribution_evidence(&root, "crates/aterm-own");
        let kind = classify_patch_target("own", "crates/aterm-own", &root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(evidence, None);
        assert_eq!(kind, Ok(PatchTargetKind::FirstParty));
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
        // aterm-b is not a workspace-table entry: reached only via aterm-a's
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

    /// Cargo's own one-table-per-dependency form, as a normalized manifest
    /// writes it (the absorbed winit's): a dotted normal dep is followed, a
    /// dotted cfg-target dep with a dotted cfg is followed, a dotted dev dep
    /// stays out, and a renamed REGISTRY dep is understood (and not walked).
    #[test]
    fn the_one_table_dependency_form_is_followed_like_the_inline_one() {
        let mut files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[dependencies.aterm-b]\n\
             path = \"../aterm-b\"\n\
             [dependencies.rwh_06]\n\
             version = \"0.6\"\n\
             features = [\n    \"std\",\n]\n\
             package = \"raw-window-handle\"\n\
             [target.'cfg(all(unix, not(any(target_os = \"ios\", target_os = \"macos\"))))'.dependencies.aterm-c]\n\
             path = \"../aterm-c\"\n\
             [dev-dependencies.aterm-devonly]\n\
             path = \"../aterm-devonly\"\n",
        )]);
        for name in ["aterm-b", "aterm-c", "aterm-devonly"] {
            files.push((
                format!("crates/{name}/Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            ));
        }
        let set = derive("onetable", with_src(files)).expect("derivation");
        for followed in ["crates/aterm-b/src", "crates/aterm-c/src"] {
            assert!(
                set.scan_dirs.contains(&followed.to_string()),
                "{followed} is a normal dependency: {:?}",
                set.scan_dirs
            );
        }
        assert!(
            !set.scan_dirs
                .contains(&"crates/aterm-devonly/src".to_string()),
            "a dotted dev dependency stays out: {:?}",
            set.scan_dirs
        );
        // A renamed path dependency in the same form is followed by its path.
        let mut files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[dependencies.renamed]\npath = \"../aterm-b\"\npackage = \"aterm-b\"\n",
        )]);
        files.push((
            "crates/aterm-b/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-b\"\n".to_string(),
        ));
        let set = derive("renamedpath", with_src(files)).expect("derivation");
        assert!(
            set.scan_dirs.contains(&"crates/aterm-b/src".to_string()),
            "a renamed path dependency is followed by its path: {:?}",
            set.scan_dirs
        );
    }

    /// A renamed path dependency, as the workspace `serde` row is
    /// (`package = "aterm-serde"`, docs/AUDIT-third-party-2026-10-15.md SP.6):
    /// the walk follows its path, and features name its KEY, as cargo resolves
    /// them. Naming the package where the key belongs is refused, so the key
    /// is what activation reads.
    #[test]
    fn a_renamed_path_dependency_is_walked_by_path_and_activated_by_key() {
        let manifests = |gui_feature: &str| {
            let (patch_table, mut files) = test_fixtures::patch_fixture();
            let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
            root.push_str("[workspace.dependencies]\n");
            root.push_str("serde = { package = \"aterm-serde\", path = \"crates/aterm-serde\" }\n");
            root.push_str(&patch_table);
            files.push(("Cargo.toml".to_string(), root));
            files.push((
                "crates/aterm-gui/Cargo.toml".to_string(),
                format!(
                    "[package]\nname = \"aterm-gui\"\n\
                     [features]\ndefault = [\"persist\"]\npersist = [\"{gui_feature}\"]\n\
                     [dependencies]\nserde = {{ workspace = true, optional = true }}\n\
                     quiet = {{ package = \"aterm-off\", path = \"../aterm-off\", optional = true }}\n"
                ),
            ));
            for n in ["aterm-serde", "aterm-off"] {
                files.push((
                    format!("crates/{n}/Cargo.toml"),
                    format!("[package]\nname = \"{n}\"\n"),
                ));
            }
            with_src(files)
        };
        let set = derive("renamedws", manifests("dep:serde")).expect("derivation");
        assert!(
            set.scan_dirs
                .contains(&"crates/aterm-serde/src".to_string()),
            "default -> persist -> dep:serde must take in crates/aterm-serde: {:?}",
            set.scan_dirs
        );
        assert!(
            !set.scan_dirs.contains(&"crates/aterm-off/src".to_string()),
            "an unactivated renamed optional dependency stays out: {:?}",
            set.scan_dirs
        );
        let err = derive("renamedwspkg", manifests("dep:aterm-serde")).expect_err("must fail");
        assert!(err.contains("names no dependency"), "err: {err}");
    }

    #[test]
    fn optional_deps_follow_default_feature_activation() {
        // Mirrors the real graph: gui's default feature forwards to a dep
        // feature (the `aterm-core/sixel` shape) which `dep:`-activates an
        // optional path dep; an unactivated optional dep (a feature left off) stays
        // out; an edge `features = [...]` list (`bidi`) activates a third.
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
            "an unactivated optional dep (a feature left off) must stay OUT: {:?}",
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
        // aterm-macdep only exists behind the proc-macro edge: it must not be
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

    /// The review is read off the real ledger, and every shape it accepts or
    /// refuses is the one `aterm_forge::policy` writes.
    #[test]
    fn the_review_set_is_the_fork_ledger() {
        // The real ledger reviews no fork since indexmap, the last one, went back to
        // the registry; any block it holds again names a crate the tree has.
        let repo = test_fixtures::repo_root();
        for fork in reviewed_forks(&repo).expect("the real ledger reads") {
            assert!(
                repo.join(&fork.path).join("Cargo.toml").is_file(),
                "{fork:?}"
            );
        }
        let read = |name: &str, ledger: &str| {
            let root = synth_tree(
                "aterm-scan-set",
                name,
                &[(FORK_LEDGER.to_string(), ledger.to_string())],
            );
            let out = reviewed_forks(&root);
            let _ = std::fs::remove_dir_all(&root);
            out
        };
        let table = "[[fork]]\nname = \"a\"\npath = \"vendor/a\"\n\
                     [fork.census]\nmode = \"scanned\"\nnamespace = \"a_ns\"\n";
        assert_eq!(
            read("ledger-table", table).expect("a [fork.census] table reads"),
            [ReviewedFork {
                package: "a".to_string(),
                path: "vendor/a".to_string(),
                namespace: "a_ns",
            }]
        );
        let good = test_fixtures::ledger(&[("a", "vendor/a", "a_ns")]);
        for (name, ledger, says) in [
            (
                "ledger-no-ns",
                good.replace("census.namespace = \"a_ns\"\n", ""),
                "no census.namespace",
            ),
            (
                "ledger-mode",
                good.replace("\"scanned\"", "\"build-dep-only\""),
                "census.mode Some(\"build-dep-only\")",
            ),
            (
                "ledger-no-path",
                good.replace("path = \"vendor/a\"\n", ""),
                "no `name` or no `path`",
            ),
        ] {
            let err = read(name, &ledger).expect_err("must fail");
            assert!(err.contains(says), "{name}: {err}");
        }
        let root = synth_tree("aterm-scan-set", "ledger-absent", &[]);
        assert_eq!(reviewed_forks(&root), Ok(Vec::new()));
        let _ = std::fs::remove_dir_all(&root);
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
            err.contains("has NO `[[fork]]` block in vendor/forge.toml"),
            "err: {err}"
        );
        // (b) FORWARD: a registered exclusion missing from the patch table is
        // a STALE review and must hard-error.
        let mut files = test_fixtures::workspace_manifests(&[]);
        for (p, c) in &mut files {
            if p == "Cargo.toml" {
                *c = c.replace("indexmap = { path = \"vendor/indexmap\" }\n", "");
            }
        }
        let err = derive("stale", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("no longer in the root [patch] table") && err.contains("indexmap"),
            "err: {err}"
        );
        // (c) FORWARD existence: the registered path must exist on disk.
        let mut files = test_fixtures::workspace_manifests(&[]);
        files.retain(|(p, _)| p != "vendor/indexmap/Cargo.toml");
        let err = derive("gone", with_src(files)).expect_err("must fail");
        assert!(
            err.contains("no Cargo.toml on disk") && err.contains("indexmap"),
            "err: {err}"
        );
    }

    #[test]
    fn default_features_false_suppresses_and_union_reenables() {
        // One edge with default-features = false: the optional dep behind the
        // child's `default` stays out. A second edge keeping defaults
        // re-enables it (union semantics, like cargo).
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
