// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The workspace's `[patch]` targets: the vendored fork ledger, the absorbed crates and the
//! classification of each target.
//!
//! Registry packages and immutable Git packages are external lock-graph nodes,
//! not local source bodies. Git declarations must match one exact locked source;
//! their transitive dependency edges still participate in process guards.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Repo-relative path to the vendored fork ledger (`vendor/forge.toml`).
pub const FORK_LEDGER: &str = "vendor/forge.toml";

/// Reviewed vendored `[patch.*]` fork entry from [`FORK_LEDGER`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewedFork {
    /// Replaced crates.io package name.
    pub package: String,
    /// Repo-relative path to the vendored fork.
    pub path: String,
}

/// Parses all `[[fork]]` entries from [`FORK_LEDGER`] under `root`.
pub fn reviewed_forks(root: &Path) -> Result<Vec<ReviewedFork>, String> {
    let text = match crate::tree::read_to_string(&root.join(FORK_LEDGER)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {FORK_LEDGER}: {e} (fail-closed)")),
    };
    let doc = parse_doc(&text, FORK_LEDGER)?;
    let blocks: Vec<&Vec<(String, Value)>> = doc
        .sections
        .iter()
        .filter(|(header, _)| header == "fork")
        .map(|(_, entries)| entries)
        .collect();
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
        out.push(ReviewedFork {
            package: package.to_string(),
            path: path.to_string(),
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

/// Absorbed upstream crate owned as a workspace member under `crates/`.
pub struct AbsorbedCrate {
    /// Original upstream package name recorded in `NOTICE`.
    pub upstream: &'static str,
    /// Repo-relative crate directory under `crates/`.
    pub path: &'static str,
    /// Audit note explaining the absorbed crate's status.
    pub audit: &'static str,
}

/// Registry of the upstream crates absorbed as workspace members.
pub const ABSORBED_CRATES: &[AbsorbedCrate] = &[AbsorbedCrate {
    upstream: "winit",
    path: "crates/aterm-window",
    audit: "winit 0.30.13, absorbed as crates/aterm-window (2026-10-15; vendored as \
            vendor/winit before; the package renamed aterm-window 2026-10-19): runs ON \
            the main thread of the GUI process; the wasm cells do not compile it",
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

/// Where a scan of TOML text stands: outside any string, or inside one of TOML's four
/// string kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lexed {
    Code,
    Basic,
    Literal,
    MultiBasic,
    MultiLiteral,
}

/// The length of the run of `quote` bytes at the start of `bytes`.
fn quote_run(bytes: &[u8], quote: u8) -> usize {
    bytes.iter().take_while(|&&b| b == quote).count()
}

/// Where a multi-line string's body ends in `body` (the text after its opening delimiter),
/// and where its closing delimiter ends: the first run of three or more `quote`s outside an
/// escape closes it, its last three being the delimiter (TOML lets one or two quotes stand
/// right before it).
fn multi_line_end(body: &str, quote: u8, escapes: bool) -> Option<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if escapes && bytes[i] == b'\\' {
            i += 2;
        } else if bytes[i] == quote {
            let run = quote_run(&bytes[i..], quote);
            if run >= 3 {
                return Some((i + run - 3, i + run));
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// The document's entries, each a section header or a `key = value`, with the 1-based line
/// it starts on: comments removed and the lines of a multi-line array or inline table joined
/// by a space. Every TOML string kind is read as a string, so a `#`, a bracket or a quote of
/// the other kind inside one is text, and a multi-line string keeps its newlines.
fn entries(text: &str, file: &str) -> Result<Vec<(usize, String)>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut entry: Vec<u8> = Vec::new();
    let (mut line, mut start, mut depth, mut lexed, mut i) = (1, 1, 0i32, Lexed::Code, 0);
    let mut finish = |entry: &mut Vec<u8>, start: usize| -> Result<(), String> {
        let text = String::from_utf8(std::mem::take(entry))
            .map_err(|e| format!("{file}:{start}: not UTF-8: {e}"))?;
        if !text.trim().is_empty() {
            out.push((start, text.trim().to_string()));
        }
        Ok(())
    };
    while i < bytes.len() {
        let b = bytes[i];
        let mut step = 1;
        match (lexed, b) {
            (Lexed::Code, b'#') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            (Lexed::Code, b'\n') => {
                line += 1;
                if depth > 0 {
                    entry.push(b' ');
                } else {
                    finish(&mut entry, start)?;
                    (start, depth) = (line, 0);
                }
                i += 1;
                continue;
            }
            (Lexed::Code, b'"' | b'\'') => {
                let multi = quote_run(&bytes[i..], b) >= 3;
                (lexed, step) = match (b, multi) {
                    (b'"', false) => (Lexed::Basic, 1),
                    (b'"', true) => (Lexed::MultiBasic, 3),
                    (_, false) => (Lexed::Literal, 1),
                    (_, true) => (Lexed::MultiLiteral, 3),
                };
            }
            (Lexed::Code, b'[' | b'{') => depth += 1,
            (Lexed::Code, b']' | b'}') => depth -= 1,
            (Lexed::Basic | Lexed::MultiBasic, b'\\') => step = 2.min(bytes.len() - i),
            (Lexed::Basic, b'"') | (Lexed::Literal, b'\'') => lexed = Lexed::Code,
            (Lexed::Basic | Lexed::Literal, b'\n') => {
                return Err(format!("{file}:{line}: unterminated string"));
            }
            (Lexed::MultiBasic, b'"') | (Lexed::MultiLiteral, b'\'') => {
                step = quote_run(&bytes[i..], b);
                if step >= 3 {
                    lexed = Lexed::Code;
                }
            }
            _ => {}
        }
        line += bytes[i..i + step].iter().filter(|&&b| b == b'\n').count();
        entry.extend_from_slice(&bytes[i..i + step]);
        i += step;
    }
    if lexed != Lexed::Code {
        return Err(format!("{file}:{start}: unterminated string"));
    }
    if depth > 0 {
        return Err(format!("{file}:{start}: unbalanced brackets in entry"));
    }
    finish(&mut entry, start)?;
    Ok(out)
}

fn parse_value(s: &str) -> Result<(Value, &str), String> {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    match bytes.first() {
        Some(&quote) if matches!(quote, b'"' | b'\'') && bytes.starts_with(&[quote; 3]) => {
            let body = &s[3..];
            let (end, close) = multi_line_end(body, quote, quote == b'"')
                .ok_or_else(|| "unterminated string".to_string())?;
            // A newline right after the opening delimiter is not part of the string.
            let text = &body[..end];
            let text = text
                .strip_prefix("\r\n")
                .or_else(|| text.strip_prefix('\n'))
                .unwrap_or(text);
            Ok((Value::Str(text.to_string()), &body[close..]))
        }
        Some(b'\'') => {
            let end = s[1..]
                .find('\'')
                .ok_or_else(|| "unterminated string".to_string())?;
            Ok((Value::Str(s[1..=end].to_string()), &s[end + 2..]))
        }
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
    for (lineno, logical) in entries(text, file)? {
        if logical.starts_with('[') {
            if !logical.ends_with(']') {
                return Err(format!(
                    "{file}:{lineno}: malformed section header `{logical}`"
                ));
            }
            let header = logical.trim_matches(['[', ']']).trim().to_string();
            sections.push((header, Vec::new()));
            continue;
        }
        let eq = logical
            .find('=')
            .ok_or_else(|| format!("{file}:{lineno}: expected `key = value`, got `{logical}`"))?;
        let key = logical[..eq].trim().to_string();
        let (value, rest) =
            parse_value(&logical[eq + 1..]).map_err(|e| format!("{file}:{lineno}: {e}"))?;
        if !rest.trim().is_empty() {
            return Err(format!(
                "{file}:{lineno}: trailing text `{}` after value",
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

/// Single `[[package]]` entry parsed from `Cargo.lock`.
#[derive(Clone, Debug)]
pub(crate) struct LockPackage {
    pub(crate) name: String,
    /// Package `source` (`None` for workspace members and path patches).
    pub(crate) source: Option<String>,
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
        let (Some(name), Some(_)) = (get("name"), get("version")) else {
            return Err(format!(
                "{file}: a [[{header}]] with no name or version (fail-closed)"
            ));
        };
        out.push(LockPackage {
            source: get("source"),
            name,
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

#[cfg(test)]
#[path = "scan_set_test_fixtures_tests.rs"]
pub(crate) mod test_fixtures;

#[cfg(test)]
#[path = "scan_set_tests.rs"]
mod tests;
