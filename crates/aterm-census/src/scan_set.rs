// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Workspace dependency graph and scan-set derivation.
//!
//! Registry packages and immutable Git packages are external lock-graph nodes,
//! not local source bodies. Git declarations must match one exact locked source;
//! their transitive dependency edges still participate in process guards.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// Default process root crate (`crates/aterm-gui`).
pub const GUI_ROOT_CRATE: &str = "crates/aterm-gui";

/// Labeled group of per-platform subtrees not compiled into the macOS binary.
#[derive(Debug)]
pub struct PlatformSlice {
    /// Platform label (`linux`, `windows`, etc.).
    pub label: &'static str,
    /// Crate-relative directory or `.rs` file paths in this slice.
    pub paths: &'static [&'static str],
}

/// Repo-relative path to the vendored fork ledger (`vendor/forge.toml`).
pub const FORK_LEDGER: &str = "vendor/forge.toml";

/// Reviewed vendored `[patch.*]` fork entry from [`FORK_LEDGER`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewedFork {
    /// Replaced crates.io package name.
    pub package: String,
    /// Repo-relative path to the vendored fork.
    pub path: String,
    /// Unique census identity namespace prefix (`census.namespace`).
    pub namespace: &'static str,
}

/// Parses all `[[fork]]` entries from [`FORK_LEDGER`] under `root`.
pub fn reviewed_forks(root: &Path) -> Result<Vec<ReviewedFork>, String> {
    let text = match crate::tree::read_to_string(&root.join(FORK_LEDGER)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {FORK_LEDGER}: {e} (fail-closed)")),
    };
    let doc = parse_doc(&text, FORK_LEDGER)?;
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

/// The Cargo package names whose source trees carry fork obligations: every reviewed vendored
/// fork and every absorbed upstream crate. The policy and each absorbed crate's own manifest are
/// the authorities; callers never maintain a second roster.
///
/// # Errors
/// The fork ledger or an absorbed crate's manifest cannot be read or parsed.
pub fn fork_packages(root: &Path) -> Result<Vec<String>, String> {
    if !crate::tree::is_file(&root.join(FORK_LEDGER)) {
        return Err(format!(
            "cannot read {FORK_LEDGER}: the authoritative fork ledger is missing (fail-closed)"
        ));
    }
    let mut packages: Vec<String> = reviewed_forks(root)?
        .into_iter()
        .map(|fork| fork.package)
        .collect();
    for row in ABSORBED_CRATES {
        let package = load_manifest(root, row.path)?.name;
        if package.is_empty() {
            return Err(format!(
                "{}/Cargo.toml has no package.name for absorbed fork `{}` (fail-closed)",
                row.path, row.upstream
            ));
        }
        packages.push(package);
    }
    packages.sort_unstable();
    packages.dedup();
    Ok(packages)
}

fn intern(s: &str) -> &'static str {
    static NAMES: std::sync::OnceLock<std::sync::Mutex<BTreeSet<&'static str>>> =
        std::sync::OnceLock::new();
    let interned_names = NAMES.get_or_init(Default::default);
    let mut names = interned_names
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = names.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// Absorbed upstream crate owned as a workspace member under `crates/`.
pub struct AbsorbedCrate {
    /// Original upstream package name recorded in `NOTICE`.
    pub upstream: &'static str,
    /// Repo-relative crate directory under `crates/`.
    pub path: &'static str,
    /// Unique census identity namespace prefix.
    pub namespace: &'static str,
    /// Per-platform subtrees not compiled on macOS.
    pub platform_slices: &'static [PlatformSlice],
    /// Audit note explaining the absorbed crate's status.
    pub audit: &'static str,
}

/// Registry of absorbed crates scanned in namespaced mode.
pub const ABSORBED_CRATES: &[AbsorbedCrate] = &[AbsorbedCrate {
    upstream: "winit",
    path: "crates/aterm-window",
    namespace: "aterm_window",
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

/// Classification of a `[patch.crates-io]` target directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchTargetKind {
    /// Third-party fork under `vendor/` requiring a [`FORK_LEDGER`] entry.
    Vendored,
    /// First-party workspace member under `crates/` without upstream provenance.
    FirstParty,
    /// Absorbed upstream crate under `crates/` listed in [`ABSORBED_CRATES`].
    Absorbed,
}

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

fn retained_license(root: &std::path::Path, path: &str) -> Option<String> {
    let mut names: Vec<String> = crate::tree::read_dir(&root.join(path))
        .ok()?
        .into_iter()
        .flatten()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| {
            let upper = n.to_uppercase();
            upper.starts_with("LICENSE") || upper.starts_with("COPYING")
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

fn notice_names(root: &std::path::Path, path: &str) -> bool {
    crate::tree::read_to_string(&root.join("NOTICE"))
        .unwrap_or_default()
        .contains(path)
}

/// Classifies a `[patch.crates-io]` target at `rel_path` under `root`.
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

/// Vendored, absorbed, or separately managed workspace library scanned with
/// its own lock and function identities. This does not exempt internal cycles.
#[derive(Debug)]
pub struct ScannedVendored {
    /// Package name.
    pub package: String,
    /// Census identity namespace prefix.
    pub namespace: &'static str,
    /// Repo-relative crate directory.
    pub crate_dir: String,
    /// Repo-relative `<crate_dir>/src` scan directory.
    pub scan_dir: String,
    /// Labeled per-platform subtrees.
    pub platform_slices: &'static [PlatformSlice],
    /// A separately managed workspace library, rather than a redistributed fork.
    pub external_workspace: bool,
}

/// Derived census scan set and package classifications.
#[derive(Debug)]
pub struct ScanSet {
    /// Sorted repo-relative `<crate_dir>/src` directories to walk.
    pub scan_dirs: Vec<String>,
    /// Excluded proc-macro crates as `(package, crate_dir)`.
    pub proc_macros: Vec<(String, String)>,
    /// Libraries with independently scoped lock and function identities.
    pub vendored_scanned: Vec<ScannedVendored>,
    /// First-party `[patch.*]` redirects as `(package, path)`.
    pub first_party_patches: Vec<(String, String)>,
    /// Absorbed crates as `(package, path)`.
    pub absorbed: Vec<(String, String)>,
    /// Target-gated dependency edges excluded by [`derive_linked_scan_set`].
    pub unlinked: Vec<(String, String, String)>,
}

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

const fn depth_delta(line: &str) -> i32 {
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
            let end = s.find([',', '}', ']']).unwrap_or(s.len());
            if end == 0 {
                return Err(format!("cannot parse value near `{s}`"));
            }
            Ok((Value::Other(s[..end].trim().to_string()), &s[end..]))
        }
    }
}

struct Doc {
    /// (section header text without the outer brackets, entries).
    sections: Vec<(String, Vec<(String, Value)>)>,
}

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

#[derive(Clone)]
struct GitPin {
    url: String,
    revision: String,
}

struct DepEdge {
    /// The dependency key: what features name (`dep:<key>`, `<key>/feat`) and
    /// what `workspace = true` looks up in the root table.
    key: String,
    /// The package behind the key when `package = "…"` renames it; the key
    /// is the package name otherwise.
    package: Option<String>,
    /// `path = "…"` (relative to the declaring crate dir), if any.
    path: Option<String>,
    /// A fully pinned external source. Its dependency closure remains in Cargo.lock.
    git: Option<GitPin>,
    /// `workspace = true`?
    workspace: bool,
    optional: bool,
    default_features: bool,
    features: Vec<String>,
    /// The `[target.<key>.dependencies]` table's key (a quoted `cfg(..)`,
    /// unquoted, or a triple) when the edge is target-gated; `None` in a
    /// plain `[dependencies]` table.
    target: Option<String>,
}

struct Manifest {
    name: String,
    proc_macro: bool,
    lib_name: Option<String>,
    lib_path: Option<String>,
    features: BTreeMap<String, Vec<String>>,
    deps: Vec<DepEdge>,
    ws_deps: BTreeMap<String, WsSpec>,
    workspace_root: String,
}

enum SectionKind {
    NormalDeps,
    ExcludedDeps,
    /// `[dependencies.<name>]` (or a `[target.….dependencies.<name>]`): one
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

fn dependency_section(h: &str) -> Option<SectionKind> {
    let (_, rest) = split_target(h)?;
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

fn split_target(h: &str) -> Option<(Option<&str>, &str)> {
    let Some(target) = h.strip_prefix("target.") else {
        return Some((None, h));
    };
    let (key, after) = match target.chars().next()? {
        quote @ ('\'' | '"') => {
            let close = target[1..].find(quote)? + 1;
            (&target[1..close], &target[close + 1..])
        }
        _ => {
            let dot = target.find('.')?;
            (&target[..dot], &target[dot..])
        }
    };
    Some((Some(key), after.strip_prefix('.')?))
}

fn dep_edge(key: &str, value: &Value, file: &str) -> Result<DepEdge, String> {
    // Dotted form: `aterm-log.workspace = true`.
    if let Some(name) = key.strip_suffix(".workspace") {
        if matches!(value, Value::Bool(true)) {
            return Ok(DepEdge {
                key: name.to_string(),
                package: None,
                path: None,
                git: None,
                workspace: true,
                optional: false,
                default_features: true,
                features: Vec::new(),
                target: None,
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
        git: None,
        workspace: false,
        optional: false,
        default_features: true,
        features: Vec::new(),
        target: None,
    };
    let mut git = None;
    let mut revision = None;
    match value {
        Value::Str(_) => {}
        Value::Table(pairs) => {
            for (k, v) in pairs {
                match (k.as_str(), v) {
                    ("version", Value::Str(_)) => {}
                    ("package", Value::Str(p)) => edge.package = Some(p.clone()),
                    ("path", Value::Str(p)) => edge.path = Some(p.clone()),
                    ("git", Value::Str(url)) if git.is_none() => git = Some(url.clone()),
                    ("rev", Value::Str(rev)) if revision.is_none() => revision = Some(rev.clone()),
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
                             derivation cannot soundly classify (outside the modeled \
                             subset — fail-closed)"
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
    match (git, revision) {
        (None, None) => {}
        (Some(url), Some(revision))
            if edge.path.is_none()
                && !edge.workspace
                && !url.is_empty()
                && !url
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '?' | '#'))
                && revision.len() == 40
                && revision.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            edge.git = Some(GitPin { url, revision });
        }
        _ => {
            return Err(format!(
                "{file}: dependency `{key}` cannot soundly classify a Git source without a \
             unique URL and full immutable rev (no path/workspace mixture; fail-closed)"
            ));
        }
    }
    Ok(edge)
}

fn load_manifest(root: &Path, dir: &str) -> Result<Manifest, String> {
    let file_rel = format!("{dir}/Cargo.toml");
    let path = root.join(&file_rel);
    let text = crate::tree::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {file_rel}: {e} — a path dependency must have a manifest \
             (fail-closed; is the dependency graph broken?)"
        )
    })?;
    let doc = parse_doc(&text, &file_rel)?;
    let mut m = Manifest {
        name: String::new(),
        proc_macro: false,
        lib_name: None,
        lib_path: None,
        features: BTreeMap::new(),
        deps: Vec::new(),
        ws_deps: BTreeMap::new(),
        workspace_root: String::new(),
    };
    for (header, entries) in &doc.sections {
        if header.is_empty() {
            continue;
        }
        let target = split_target(header.trim())
            .and_then(|(key, _)| key)
            .map(str::to_string);
        match classify_section(header, &file_rel)? {
            SectionKind::NormalDeps => {
                for (k, v) in entries {
                    m.deps.push(DepEdge {
                        target: target.clone(),
                        ..dep_edge(k, v, &file_rel)?
                    });
                }
            }
            SectionKind::NormalDep(name) => {
                m.deps.push(DepEdge {
                    target,
                    ..dep_edge(&name, &Value::Table(entries.clone()), &file_rel)?
                });
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
                    if let Value::Str(value) = v {
                        if k == "name" {
                            m.lib_name = Some(value.clone());
                        } else if k == "path" {
                            m.lib_path = Some(value.clone());
                        }
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
    let (workspace_root, tables) = declaring_workspace(root, dir, &doc)?;
    m.workspace_root = workspace_root;
    m.ws_deps = tables.ws_deps;
    for edge in &m.deps {
        let pinned = if edge.workspace {
            m.ws_deps
                .get(&edge.key)
                .and_then(|ws| ws.git.as_ref())
                .map(|(name, pin)| (name.as_str(), pin))
        } else {
            edge.git
                .as_ref()
                .map(|pin| (edge.package.as_deref().unwrap_or(&edge.key), pin))
        };
        if let Some((name, pin)) = pinned {
            validate_git_lock(root, name, pin)?;
        }
    }
    Ok(m)
}

/// Git sources follow the same external lock-graph path as registry packages,
/// but only when the declaration and the exact locked commit agree.
fn validate_git_lock(root: &Path, name: &str, pin: &GitPin) -> Result<(), String> {
    let lock = read_lock(root)?;
    let matches: Vec<_> = lock.iter().filter(|package| package.name == name).collect();
    let expected = format!("git+{}?rev={}#{}", pin.url, pin.revision, pin.revision);
    if matches.len() != 1 || matches[0].source.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "Git dependency `{name}` must have one Cargo.lock entry matching its full \
             immutable source `{expected}` (missing, different or ambiguous; fail-closed)"
        ));
    }
    Ok(())
}

/// Returns normal external dependency package names declared in `<root>/<dir>/Cargo.toml`.
pub(crate) fn external_dependency_names(root: &Path, dir: &str) -> Result<Vec<String>, String> {
    let manifest = load_manifest(root, dir)?;
    Ok(manifest
        .deps
        .into_iter()
        .filter(|edge| {
            edge.path.is_none()
                && !(edge.workspace
                    && manifest
                        .ws_deps
                        .get(&edge.key)
                        .is_some_and(|ws| ws.dir.is_some()))
        })
        .map(|edge| {
            if edge.workspace
                && let Some((name, _)) = manifest
                    .ws_deps
                    .get(&edge.key)
                    .and_then(|ws| ws.git.as_ref())
            {
                return name.clone();
            }
            edge.package.unwrap_or(edge.key)
        })
        .collect())
}

/// Normal dependency aliases with a proved default library source in another workspace member.
/// Multiple cfg-selected destinations remain a set; callers must not choose one arbitrarily.
pub(crate) fn member_dependency_aliases(
    root: &Path,
    dir: &str,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let manifest = load_manifest(root, dir)?;
    let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for edge in &manifest.deps {
        let destinations = aliases.entry(edge.key.replace('-', "_")).or_default();
        let child = if edge.workspace {
            manifest
                .ws_deps
                .get(&edge.key)
                .and_then(|dep| dep.dir.clone())
        } else {
            edge.path
                .as_deref()
                .map(|path| normalize_rel(dir, path))
                .transpose()?
        };
        let Some(child) = child.filter(|path| path.starts_with("crates/")) else {
            destinations.insert(String::new());
            continue;
        };
        let target = load_manifest(root, &child)?;
        if target.proc_macro || target.lib_name.is_some() || target.lib_path.is_some() {
            destinations.insert(String::new());
            continue;
        }
        destinations.insert(child);
    }
    Ok(aliases)
}

/// Library and active build-script roots of a normal process dependency.
///
/// # Errors
/// The manifest is unreadable, an explicit source path is malformed, or it escapes the tree.
pub(crate) fn dependency_source_roots(root: &Path, dir: &str) -> Result<Vec<String>, String> {
    let rel = format!("{dir}/Cargo.toml");
    let text = crate::tree::read_to_string(&root.join(&rel))
        .map_err(|e| format!("cannot read {rel}: {e}"))?;
    let doc = parse_doc(&text, &rel)?;
    let mut library = "src/lib.rs".to_string();
    let mut build = None;
    let mut disabled = false;
    for (header, entries) in doc.sections {
        for (key, value) in entries {
            if header == "lib" && key == "path" {
                let Value::Str(path) = value else {
                    return Err(format!("{rel}: lib.path must be a string"));
                };
                library = path;
            } else if header == "package" && key == "build" {
                match value {
                    Value::Bool(false) => disabled = true,
                    Value::Bool(true) => {}
                    Value::Str(path) => build = Some(path),
                    _ => return Err(format!("{rel}: build must be a string or boolean")),
                }
            }
        }
    }
    let source = |path: &str| {
        if Path::new(path).is_absolute() {
            return Err(format!(
                "{rel}: source path `{path}` is not repository-relative"
            ));
        }
        normalize_rel(dir, path)
    };
    let mut roots = vec![source(&library)?];
    if !disabled {
        if let Some(path) = build {
            roots.push(source(&path)?);
        } else if crate::tree::is_file(&root.join(dir).join("build.rs")) {
            roots.push(source("build.rs")?);
        }
    }
    Ok(roots)
}

/// One manifest's Cargo targets as its text declares them.
pub(crate) struct ManifestTargets {
    /// It has a `[package]`: a virtual manifest declares no target.
    pub(crate) package: bool,
    /// The package's `name`.
    pub(crate) name: Option<String>,
    /// The kinds (`lib`, `bin`, `test`, `bench`, `example`) whose discovery `[package]` turns
    /// off (`autotests = false`).
    pub(crate) undiscovered: Vec<&'static str>,
    /// Each target table (`[lib]`, `[[bin]]`, `[[test]]`, `[[bench]]`, `[[example]]`) as its
    /// kind, its `name` and its `path`.
    pub(crate) declared: Vec<(&'static str, Option<String>, Option<String>)>,
    /// Parallel to [`Self::declared`]: whether Cargo's `--tests` harness runs `#[test]` functions
    /// in that target (`test` enabled and `harness` not `false`).
    pub(crate) runs_libtest: Vec<bool>,
    /// `[package] build`: `None` where it is absent or `true` (Cargo builds `build.rs` when it
    /// exists), `Some(None)` for `false`, `Some(Some(path))` for a path.
    pub(crate) build: Option<Option<String>>,
}

/// The target tables of the manifest `text` (at `file`, for errors).
pub(crate) fn manifest_targets(text: &str, file: &str) -> Result<ManifestTargets, String> {
    const KINDS: [(&str, &str); 5] = [
        ("lib", "autolib"),
        ("bin", "autobins"),
        ("test", "autotests"),
        ("bench", "autobenches"),
        ("example", "autoexamples"),
    ];
    let mut out = ManifestTargets {
        package: false,
        name: None,
        undiscovered: Vec::new(),
        declared: Vec::new(),
        runs_libtest: Vec::new(),
        build: None,
    };
    for (header, entries) in parse_doc(text, file)?.sections {
        let string = |key: &str| {
            entries.iter().find_map(|(k, v)| match v {
                Value::Str(s) if k == key => Some(s.clone()),
                _ => None,
            })
        };
        let flag = |key: &str| {
            entries.iter().find_map(|(k, v)| match v {
                Value::Bool(b) if k == key => Some(*b),
                _ => None,
            })
        };
        if header == "package" {
            out.package = true;
            out.name = string("name");
            out.build = match flag("build") {
                Some(false) => Some(None),
                Some(true) => None,
                None => string("build").map(Some),
            };
            for (kind, auto) in KINDS {
                if entries
                    .iter()
                    .any(|(k, v)| k == auto && matches!(v, Value::Bool(false)))
                {
                    out.undiscovered.push(kind);
                }
            }
        } else if let Some(&(kind, _)) = KINDS.iter().find(|(kind, _)| header == *kind) {
            let default_test = matches!(kind, "lib" | "bin" | "test");
            let libtest = flag("harness").unwrap_or(true) && flag("test").unwrap_or(default_test);
            out.declared.push((kind, string("name"), string("path")));
            out.runs_libtest.push(libtest);
        }
    }
    Ok(out)
}

/// Computes the transitive closure of workspace members reachable from `dir`.
pub(crate) fn member_reach(
    root: &Path,
    dir: &str,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut reach: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([(dir.to_string(), Vec::new())]);
    while let Some((at, chain)) = queue.pop_front() {
        let manifest = load_manifest(root, &at)?;
        for edge in &manifest.deps {
            let child = if edge.workspace {
                let Some(ws) = manifest.ws_deps.get(&edge.key) else {
                    return Err(format!(
                        "{at}/Cargo.toml: `{}` says `workspace = true` but its workspace \
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

/// Maps `[patch]` package names to repo-relative paths for workspace-member targets.
pub(crate) fn member_patch_targets(root: &Path) -> Result<BTreeMap<String, String>, String> {
    Ok(load_root_tables(root)?
        .patches
        .into_values()
        .filter(|(_, path)| path.starts_with("crates/"))
        .collect())
}

/// Single `[[package]]` entry parsed from `Cargo.lock`.
#[derive(Clone, Debug)]
pub(crate) struct LockPackage {
    pub(crate) name: String,
    pub(crate) version: String,
    /// Package `source` (`None` for workspace members and path patches).
    pub(crate) source: Option<String>,
    /// Dependencies as `(name, optional_version)` pairs.
    pub(crate) deps: Vec<(String, Option<String>)>,
}

/// Parses all `[[package]]` entries from `<root>/Cargo.lock`.
pub(crate) fn read_lock(root: &Path) -> Result<Vec<LockPackage>, String> {
    let file = "Cargo.lock";
    let text = crate::tree::read_to_string(&root.join(file))
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

fn dep_targets_pkg(edge: &DepEdge, dep: &str) -> bool {
    match edge.package.as_deref() {
        Some(pkg) => pkg == dep,
        None => {
            edge.key == dep
                || edge
                    .path
                    .as_deref()
                    .is_some_and(|p| p == dep || p.ends_with(&format!("/{dep}")))
        }
    }
}

/// Finds manifest lines in `text` that enable `dep`'s `feature` in a shipped build.
pub(crate) fn shipped_feature_enablers(
    text: &str,
    file: &str,
    dep: &str,
    feature: &str,
) -> Result<Vec<String>, String> {
    let doc = parse_doc(text, file)?;
    let mut aliases = BTreeSet::from([dep.to_string()]);
    let mut out = Vec::new();
    for (header, entries) in &doc.sections {
        if header.is_empty() {
            continue;
        }
        match classify_section(header, file)? {
            SectionKind::NormalDeps | SectionKind::WorkspaceDeps => {
                for (k, v) in entries {
                    let edge = dep_edge(k, v, file)?;
                    if !dep_targets_pkg(&edge, dep) {
                        continue;
                    }
                    aliases.insert(edge.key);
                    if edge.features.iter().any(|f| f == feature) {
                        out.push(format!("[{header}] {k} = {{ features = [\"{feature}\"] }}"));
                    }
                }
            }
            SectionKind::NormalDep(name) => {
                let edge = dep_edge(&name, &Value::Table(entries.clone()), file)?;
                if dep_targets_pkg(&edge, dep) {
                    aliases.insert(edge.key);
                    if edge.features.iter().any(|f| f == feature) {
                        out.push(format!("[{header}] features = [\"{feature}\"]"));
                    }
                }
            }
            _ => {}
        }
    }
    let forwards: BTreeSet<String> = aliases
        .iter()
        .flat_map(|a| [format!("{a}/{feature}"), format!("{a}?/{feature}")])
        .collect();
    for (header, entries) in &doc.sections {
        if header.is_empty() || !matches!(classify_section(header, file)?, SectionKind::Features) {
            continue;
        }
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
    Ok(out)
}

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

struct WsSpec {
    /// Repo-relative crate dir, if this is a path dep.
    dir: Option<String>,
    git: Option<(String, GitPin)>,
    default_features: bool,
    features: Vec<String>,
}

struct RootTables {
    ws_deps: BTreeMap<String, WsSpec>,
    /// `[patch.*]` entries with a `path` redirect: row key -> (the package it
    /// replaces, repo-relative path).
    patches: BTreeMap<String, (String, String)>,
}

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
    load_tables(root, "")
}

/// Cargo inheritance belongs to the package's nearest (or explicitly named)
/// workspace. A vendored workspace does not inherit the consumer's root table.
fn declaring_workspace(root: &Path, dir: &str, doc: &Doc) -> Result<(String, RootTables), String> {
    let explicit = doc
        .sections
        .iter()
        .filter(|(header, _)| header == "package")
        .flat_map(|(_, entries)| entries)
        .find_map(|(key, value)| {
            if key == "workspace" {
                Some(value)
            } else {
                None
            }
        });
    let mut candidate = match explicit {
        Some(Value::Str(path)) if !Path::new(path).is_absolute() => normalize_rel(dir, path)?,
        Some(_) => {
            return Err(format!(
                "{dir}/Cargo.toml: package.workspace must be a repository-relative path"
            ));
        }
        None => dir.to_string(),
    };
    loop {
        let file = if candidate.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{candidate}/Cargo.toml")
        };
        match crate::tree::read_to_string(&root.join(&file)) {
            Ok(text) => {
                let parent = parse_doc(&text, &file)?;
                if parent
                    .sections
                    .iter()
                    .any(|(header, _)| header == "workspace" || header.starts_with("workspace."))
                {
                    let tables = load_tables(root, &candidate)?;
                    return Ok((candidate, tables));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {}
            Err(error) => return Err(format!("cannot read declaring workspace {file}: {error}")),
        }
        if explicit.is_some() || candidate.is_empty() {
            return Err(format!(
                "{dir}/Cargo.toml: inherited dependency has no declaring workspace (fail-closed)"
            ));
        }
        candidate = candidate
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent)
            .to_string();
    }
}

fn load_tables(root: &Path, base: &str) -> Result<RootTables, String> {
    let file_rel = if base.is_empty() {
        "Cargo.toml".to_string()
    } else {
        format!("{base}/Cargo.toml")
    };
    let text = crate::tree::read_to_string(&root.join(&file_rel))
        .map_err(|e| format!("cannot read the workspace root {file_rel}: {e} (fail-closed)"))?;
    let doc = parse_doc(&text, &file_rel)?;
    let mut out = RootTables {
        ws_deps: BTreeMap::new(),
        patches: BTreeMap::new(),
    };
    for (header, entries) in &doc.sections {
        if header.is_empty() {
            continue;
        }
        match classify_section(header, &file_rel)? {
            SectionKind::WorkspaceDeps => {
                for (k, v) in entries {
                    let edge = dep_edge(k, v, &file_rel)?;
                    if edge.workspace {
                        return Err(format!(
                            "{file_rel}: workspace dep `{k}` cannot itself say \
                             `workspace = true`"
                        ));
                    }
                    let dir = match &edge.path {
                        Some(p) => Some(normalize_rel(base, p)?),
                        None => None,
                    };
                    out.ws_deps.insert(
                        edge.key.clone(),
                        WsSpec {
                            dir,
                            git: edge
                                .git
                                .map(|pin| (edge.package.unwrap_or_else(|| edge.key.clone()), pin)),
                            default_features: edge.default_features,
                            features: edge.features,
                        },
                    );
                }
            }
            SectionKind::Patch => {
                for (k, v) in entries {
                    let (package, spec) = patch_row(k, v);
                    let edge = dep_edge(k, &spec, &file_rel)?;
                    let Some(p) = edge.path else {
                        return Err(format!(
                            "{file_rel}: `[patch]` entry `{k}` is not a path redirect — \
                             the derivation cannot classify it (fail-closed; review it)"
                        ));
                    };
                    out.patches
                        .insert(k.clone(), (package, normalize_rel(base, &p)?));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// What one crate's active features imply.
#[derive(Default)]
struct Activation {
    /// Optional dep keys activated (`dep:x`, implicit `x`, strong `x/f`).
    active_optional: BTreeSet<String>,
    /// Per-dep feature requests from `x/f` / `x?/f` items (weak requests are
    /// consumed only for deps that end up included, as in cargo).
    dep_requests: BTreeMap<String, BTreeSet<String>>,
}

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

/// Per-crate unified feature state during the fixpoint.
#[derive(Default, Clone, PartialEq)]
struct CrateState {
    defaults: bool,
    requested: BTreeSet<String>,
}

/// Derives the `aterm-gui` process scan set from workspace manifests at `root`.
pub fn derive_gui_scan_set(root: &Path) -> Result<ScanSet, String> {
    derive_process_scan_set(root, &[GUI_ROOT_CRATE])
}

/// Derives the union scan set reachable from `root_crates`.
pub fn derive_process_scan_set(root: &Path, root_crates: &[&str]) -> Result<ScanSet, String> {
    derive_linked_scan_set(root, root_crates, &|_| true)
}

/// Derives the scan set reachable from `root_crates` when linked for `target_arch`.
pub fn derive_linked_scan_set(
    root: &Path,
    root_crates: &[&str],
    links: &dyn Fn(&str) -> bool,
) -> Result<ScanSet, String> {
    let tables = load_root_tables(root)?;
    let mut unlinked: BTreeSet<(String, String, String)> = BTreeSet::new();
    let reviewed = reviewed_forks(root)?;

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
                continue;
            }
            let st = state[&dir].clone();
            let act = expand_features(m, &dir, st.defaults, &st.requested)?;
            let mut updates: Vec<(String, bool, BTreeSet<String>)> = Vec::new();
            for edge in &m.deps {
                let (child_dir, mut defaults, mut feats) = if edge.workspace {
                    let Some(ws) = m.ws_deps.get(&edge.key) else {
                        return Err(format!(
                            "{dir}/Cargo.toml: `{}` says `workspace = true` but its workspace \
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
                if let Some(key) = &edge.target
                    && !links(key)
                {
                    unlinked.insert((dir.clone(), edge.key.clone(), key.clone()));
                    continue;
                }
                let Some(child_dir) = child_dir else {
                    continue;
                };
                if let Some(req) = act.dep_requests.get(&edge.key) {
                    feats.extend(req.iter().cloned());
                }
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

    for sd in &scan_dirs {
        if !crate::tree::is_dir(&root.join(sd)) {
            return Err(format!(
                "derived closure crate `{sd}` has no src/ directory on disk — the scan \
                 set and the scannable tree have drifted (fail-closed; fix the crate \
                 layout or the dependency edge)"
            ));
        }
    }

    let mut vendored_scanned: Vec<ScannedVendored> = Vec::new();
    let mut first_party_patches: Vec<(String, String)> = Vec::new();
    let mut absorbed: Vec<(String, String)> = Vec::new();
    for (pkg, path) in tables.patches.values() {
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
            if !crate::tree::is_file(&root.join(path).join("Cargo.toml")) {
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
        if !crate::tree::is_dir(&root.join(&scan_dir)) {
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
            external_workspace: false,
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
        if !crate::tree::is_file(&root.join(&r.path).join("Cargo.toml")) {
            return Err(format!(
                "{FORK_LEDGER}'s `[[fork]] {}` points at `{}`, which has no Cargo.toml on \
                 disk — the review is STALE (fail-closed)",
                r.package, r.path
            ));
        }
    }

    for row in ABSORBED_CRATES {
        if absorbed.iter().any(|(_, p)| p == row.path) {
            continue;
        }
        let member = crate::tree::is_file(&root.join(row.path).join("Cargo.toml"));
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
    for (dir, manifest) in &manifests {
        if manifest.workspace_root.is_empty()
            || manifest.proc_macro
            || !state.contains_key(dir)
            || vendored_scanned.iter().any(|entry| entry.crate_dir == *dir)
        {
            continue;
        }
        vendored_scanned.push(ScannedVendored {
            package: manifest.name.clone(),
            namespace: intern(&format!("external::{}", manifest.name)),
            crate_dir: dir.clone(),
            scan_dir: format!("{dir}/src"),
            platform_slices: &[],
            external_workspace: true,
        });
    }
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
        unlinked: unlinked.into_iter().collect(),
    })
}

fn scan_absorbed(
    root: &Path,
    row: &AbsorbedCrate,
    package: &str,
    scan_dirs: &mut Vec<String>,
) -> Result<ScannedVendored, String> {
    for slice in row.platform_slices {
        for p in slice.paths {
            if !crate::tree::exists(&root.join(row.path).join(p)) {
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
        external_workspace: false,
    })
}

/// Formats the scan-set derivation summary for the census transcript.
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
                if v.external_workspace {
                    format!("{} ({}; separate workspace)", v.package, v.crate_dir)
                } else if v.platform_slices.is_empty() {
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
            "      vendored, absorbed, and external-workspace libraries SCANNED in isolated-identity \
             mode (fork provenance remains checked; external workspaces are derived from manifests) \
             — process code, each crate's lock identities in its own \
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

#[cfg(test)]
#[path = "scan_set_test_fixtures_tests.rs"]
pub(crate) mod test_fixtures;

#[cfg(test)]
#[path = "scan_set_tests.rs"]
mod tests;
