// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A closure key per gated library, recorded beside each facts record and never judged.
//!
//! A facts record is filed under one key over the whole tree less its floors
//! (`trust_facts::facts_key`), so any edit re-runs every library. Before a per-library key
//! may let a run reuse results, it must be shown to predict what it measures; these records
//! measure the reuse such a key would have had and catch every "same key, different
//! Summary" case. They are filed apart, in `<store>/trust-closure/<facts key>`
//! ([`file_closures`]), so no reader of records sees them and no verdict, floor or receipt
//! depends on them (`trust_facts`'s `closure_keys_never_reach_the_judge`).
//!
//! The key is a SHA-256 over [`CLOSURE_SCHEMA`] and one digest per component, each recorded
//! so a diff names which input moved ([`ClosureKey::moved`]):
//!
//! * `prover`: the prover identity the run filed under (atpkg's `tree_root` of the
//!   toolchain directory);
//! * `pins`: `trustc -vV`'s commit-hash, the driver's `--version` and the lane's pinned
//!   environment (`lane-pin NAME=VALUE`, sorted);
//! * `root-config`: the commit's `.cargo/config.toml`, `.cargo/config`, `Cargo.toml` and
//!   `rust-toolchain.toml`, each as canonical TOML (comments and layout drop out), or
//!   `absent`;
//! * `profile`: the library's `targo trust check` argv as the lane ran it;
//! * `packages`: the git tree id at the commit of every path package in the library's
//!   dependency closure (normal and build edges, every platform: a superset); a package in
//!   a submodule by its gitlink and its path inside it;
//! * `dep-info`: every input the `.d` files of the library and its closure's path packages
//!   list, by normalized path ([`Normal`]) and content digest;
//! * `env-deps`: every `# env-dep:` line of every `.d` the check wrote, registry packages
//!   included, its value normalized as a path;
//! * `lock`: the `Cargo.lock` entry of every package in the closure.
//!
//! Only the `.d` files the check itself wrote are read: a slot runs one check after another,
//! and a path package compiled for another library's check, with other features, leaves its
//! own `.d` there. The lane lists them beside the check's log (`<log>.depinfo`, those at least
//! as new as the check's start: [`crate::trust_lane::record`]).
//!
//! A vendored fork is checked by its own manifest, so the root `Cargo.lock` and `[patch]` do
//! not apply; its `packages` and `lock` come from the lock its check used, which the lane
//! keeps as `<log>.lock`.
//!
//! An input the lane cannot read makes its component `unknown:<why>` and the key the literal
//! [`UNKNOWN`], never a digest over what was readable: a partial key could later read as a
//! match. A closure package with no `.d` of its own is not unknown (the graph spans every
//! platform), but the library's own `.d` missing from its check's list is.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;
use aterm_json::Value;

use crate::ledger::GitRunner;
use crate::trust_facts::{FACTS_PREFIX, hex, one_line};

/// The closure key's schema line: a change to what it covers bumps it, and no
/// key of another schema is ever compared with this one's.
pub const CLOSURE_SCHEMA: &str = "aterm-trust closure key 1";

/// The key of a closure with any component the lane could not read.
pub const UNKNOWN: &str = "unknown";

/// The directory, in the facts store, of the closure files: one per record,
/// named by the record's key.
pub const CLOSURE_DIR: &str = "trust-closure";

/// Beside a check's log: the `.d` files that check wrote, one path a line.
pub const DEPINFO_SUFFIX: &str = ".depinfo";

/// Beside a fork's check log: the `Cargo.lock` its check resolved and used.
pub const LOCK_SUFFIX: &str = ".lock";

/// The components, in the order they are hashed and recorded.
pub const COMPONENTS: [&str; 8] = [
    "prover",
    "pins",
    "root-config",
    "profile",
    "packages",
    "dep-info",
    "env-deps",
    "lock",
];

/// The root config files, relative to the checkout, in the order hashed.
const ROOT_CONFIG: [&str; 4] = [
    ".cargo/config.toml",
    ".cargo/config",
    "Cargo.toml",
    "rust-toolchain.toml",
];

/// One gated library's closure key and the digest of each component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosureKey {
    /// SHA-256 over [`CLOSURE_SCHEMA`] and each component's digest, or
    /// [`UNKNOWN`] when any component is unknown.
    pub key: String,
    /// `(component, digest)` in [`COMPONENTS`] order; an unreadable one's
    /// digest is `unknown:<why>`.
    pub parts: Vec<(String, String)>,
}

impl ClosureKey {
    /// The key over each component's material (the text it digests), `Err(why)` for one the
    /// lane could not read. `material` is in [`COMPONENTS`] order.
    #[must_use]
    pub fn of(material: [std::result::Result<String, String>; 8]) -> ClosureKey {
        let parts = COMPONENTS
            .iter()
            .zip(material)
            .map(|(name, m)| {
                let digest = match m {
                    Ok(text) => hex(&Sha256::digest(text.as_bytes())),
                    Err(why) => format!("{UNKNOWN}:{}", one_line(&why)),
                };
                ((*name).to_string(), digest)
            })
            .collect();
        ClosureKey::from_parts(parts)
    }

    fn from_parts(parts: Vec<(String, String)>) -> ClosureKey {
        let known = parts.iter().all(|(_, d)| !d.starts_with(UNKNOWN));
        let key = if known {
            let mut text = format!("{CLOSURE_SCHEMA}\n");
            for (name, digest) in &parts {
                text.push_str(&format!("{name} {digest}\n"));
            }
            hex(&Sha256::digest(text.as_bytes()))
        } else {
            UNKNOWN.to_string()
        };
        ClosureKey { key, parts }
    }

    /// Whether every component was read.
    #[must_use]
    pub fn is_known(&self) -> bool {
        self.key != UNKNOWN
    }

    /// The components that tell `self` and `other` apart — one unknown on
    /// either side included, since an unknown is never the same as anything.
    /// Empty exactly when both keys are known and equal.
    #[must_use]
    pub fn moved(&self, other: &ClosureKey) -> Vec<String> {
        self.parts
            .iter()
            .zip(&other.parts)
            .filter(|((_, a), (_, b))| a != b || a.starts_with(UNKNOWN))
            .map(|((name, _), _)| name.clone())
            .collect()
    }

    /// As recorded: `<key>\t<component>=<digest>…`.
    #[must_use]
    pub fn fields(&self) -> String {
        let mut out = self.key.clone();
        for (name, digest) in &self.parts {
            out.push_str(&format!("\t{name}={digest}"));
        }
        out
    }

    /// Read [`ClosureKey::fields`] back: every component, in order, and a key
    /// that is the one its components give — anything else is no key.
    #[must_use]
    pub fn from_fields(f: &[&str]) -> Option<ClosureKey> {
        let (key, rest) = f.split_first()?;
        if rest.len() != COMPONENTS.len() {
            return None;
        }
        let mut parts = Vec::new();
        for (field, want) in rest.iter().zip(COMPONENTS) {
            let (name, digest) = field.split_once('=')?;
            if name != want || digest.is_empty() {
                return None;
            }
            parts.push((name.to_string(), digest.to_string()));
        }
        let k = ClosureKey::from_parts(parts);
        (k.key == *key).then_some(k)
    }
}

/// Everything the keys are read from: the run's commit and checkout, its
/// prover, pins and slots, and the graph it gated from.
pub struct Sources<'a> {
    pub git: &'a dyn GitRunner,
    /// The checkout the lane ran in (relative `.d` paths are relative to it).
    pub repo: &'a Path,
    pub commit: &'a str,
    /// The prover identity the record is filed under.
    pub prover: &'a str,
    /// `trustc -vV`'s commit-hash (`unknown` when it named none).
    pub trustc: &'a str,
    pub driver_id: &'a str,
    pub lane_pins: &'a BTreeMap<String, String>,
    /// `targo metadata`'s JSON as the lane read it; `None` when the plan named none.
    pub metadata: Option<&'a Path>,
    /// The slots' root (`<root>/j<N>/…/deps/*.d`).
    pub target_root: &'a Path,
    pub cargo_home: Option<&'a Path>,
}

/// One gated library: its name, the spec the lane selected it by
/// (`name@version`), the argv its check ran with (`None` when the plan named
/// none), and its check's log, beside which the lane left the `.d` files that
/// check wrote ([`DEPINFO_SUFFIX`]) and, for a fork, its lock ([`LOCK_SUFFIX`]).
pub struct Subject<'a> {
    pub name: &'a str,
    pub spec: &'a str,
    pub argv: Option<&'a str>,
    pub log: PathBuf,
}

impl Subject<'_> {
    /// Checked by its own manifest: a vendored fork, its own workspace.
    fn is_fork(&self) -> bool {
        self.argv
            .is_some_and(|a| a.split_whitespace().any(|w| w == "--manifest-path"))
    }

    fn beside(&self, suffix: &str) -> PathBuf {
        let mut p = self.log.clone().into_os_string();
        p.push(suffix);
        PathBuf::from(p)
    }
}

/// A package of the graph.
struct Pkg {
    name: String,
    version: String,
    source: Option<String>,
    /// The manifest's directory.
    dir: PathBuf,
    /// Its library target's crate name (`-` as `_`), when it has one.
    lib: Option<String>,
}

/// The graph as the lane read it: its root, its packages, and each package's
/// normal and build dependencies.
struct Graph {
    root: PathBuf,
    pkgs: BTreeMap<String, Pkg>,
    deps: BTreeMap<String, Vec<String>>,
}

impl Graph {
    fn read(path: Option<&Path>) -> std::result::Result<Graph, String> {
        let path = path.ok_or("the lane's plan names no dependency graph")?;
        let text = fs::read_to_string(path)
            .map_err(|e| format!("cannot read the graph {}: {e}", path.display()))?;
        let v: Value = aterm_json::from_str(&text)
            .map_err(|e| format!("the graph {} is not JSON: {e}", path.display()))?;
        let bad = |what: &str| format!("the graph {} has no {what}", path.display());
        let root = PathBuf::from(
            v.get("workspace_root")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("workspace_root"))?,
        );
        let mut pkgs = BTreeMap::new();
        for p in v
            .get("packages")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("packages"))?
        {
            let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
            let (Some(id), Some(name), Some(version), Some(manifest)) =
                (s("id"), s("name"), s("version"), s("manifest_path"))
            else {
                return Err(bad("well-formed package"));
            };
            let lib = p
                .get("targets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|t| {
                    t.get("kind")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .any(|k| matches!(k, "lib" | "rlib" | "proc-macro" | "dylib"))
                })
                .and_then(|t| t.get("name").and_then(Value::as_str))
                .map(|n| n.replace('-', "_"));
            let dir = Path::new(&manifest)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            pkgs.insert(
                id,
                Pkg {
                    name,
                    version,
                    source: s("source"),
                    dir,
                    lib,
                },
            );
        }
        let mut deps = BTreeMap::new();
        for n in v
            .get("resolve")
            .and_then(|r| r.get("nodes"))
            .and_then(Value::as_array)
            .ok_or_else(|| bad("resolve"))?
        {
            let id = n
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("well-formed resolve node"))?;
            let edges = n
                .get("deps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|d| {
                    d.get("dep_kinds")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .any(|k| {
                            matches!(k.get("kind"), None | Some(Value::Null))
                                || k.get("kind").and_then(Value::as_str) == Some("build")
                        })
                })
                .filter_map(|d| d.get("pkg").and_then(Value::as_str).map(str::to_string))
                .collect();
            deps.insert(id.to_string(), edges);
        }
        Ok(Graph { root, pkgs, deps })
    }

    /// The package the lane selected by `spec` (`name@version`, or a name; a
    /// fork-feature row's `name+feature@version` is that package's).
    fn find(&self, spec: &str) -> std::result::Result<&str, String> {
        let (name, version) = spec.split_once('@').unwrap_or((spec, ""));
        let name = crate::trust_lane::package_identity(name);
        let hits: Vec<&str> = self
            .pkgs
            .iter()
            .filter(|(_, p)| p.name == name && (version.is_empty() || p.version == version))
            .map(|(id, _)| id.as_str())
            .collect();
        match hits[..] {
            [one] => Ok(one),
            _ => Err(format!("the graph has {} packages for {spec}", hits.len())),
        }
    }

    /// `id` and every package it reaches by a normal or build edge.
    fn closure(&self, id: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(at) = stack.pop() {
            if seen.insert(at.clone()) {
                stack.extend(self.deps.get(&at).into_iter().flatten().cloned());
            }
        }
        seen
    }
}

/// The `.d` files one check wrote, as the lane listed them beside its log
/// ([`DEPINFO_SUFFIX`]), by the crate name each file name carries
/// (`<crate>-<hash>.d`).
fn check_dep_files(list: &Path) -> std::result::Result<BTreeMap<String, Vec<PathBuf>>, String> {
    let text = fs::read_to_string(list).map_err(|e| {
        format!(
            "the lane listed no dep-info for this check ({}: {e})",
            list.display()
        )
    })?;
    let mut out: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let p = PathBuf::from(line);
        if let Some((name, _)) = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.rsplit_once('-'))
        {
            out.entry(name.to_string()).or_default().push(p);
        }
    }
    for v in out.values_mut() {
        v.sort();
        v.dedup();
    }
    Ok(out)
}

/// One `.d` file: the inputs it lists and its `# env-dep:` lines.
fn read_dep_info(text: &str) -> (Vec<String>, Vec<String>) {
    let (mut inputs, mut env) = (Vec::new(), Vec::new());
    for line in text.lines() {
        if let Some(read) = line.strip_prefix("# env-dep:") {
            env.push(read.to_string());
        } else if !line.starts_with('#')
            && let Some(path) = line.strip_suffix(':')
            && !path.is_empty()
        {
            // rustc lists each input again as a rule of its own with no
            // prerequisites; a space in a path is escaped.
            inputs.push(path.replace("\\ ", " "));
        }
    }
    (inputs, env)
}

/// The two spellings of a directory: as given and as the file system resolves it.
fn spellings(dir: &Path) -> Vec<String> {
    let mut out = vec![dir.display().to_string()];
    if let Ok(c) = fs::canonicalize(dir) {
        let c = c.display().to_string();
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

/// Writes a path the same on every run: under the slots as `$TARGET/` with the
/// slot number and each `-<16 hex>` unit suffix dropped, under the checkout
/// relative to it, under the cargo home as `$CARGO_HOME/`.
struct Normal {
    target: Vec<String>,
    repo: Vec<String>,
    home: Vec<String>,
}

impl Normal {
    fn new(src: &Sources<'_>, graph_root: Option<&Path>) -> Normal {
        let mut repo = spellings(src.repo);
        if let Some(r) = graph_root {
            for s in spellings(r) {
                if !repo.contains(&s) {
                    repo.push(s);
                }
            }
        }
        Normal {
            target: spellings(src.target_root),
            repo,
            home: src.cargo_home.map(spellings).unwrap_or_default(),
        }
    }

    fn path(&self, p: &str) -> String {
        let under = |roots: &[String]| {
            roots.iter().find_map(|r| {
                p.strip_prefix(r.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
            })
        };
        if let Some(rest) = under(&self.target) {
            // `j<N>/<profile>/build/<unit>-<hash>/out/…`: the slot goes, and
            // each per-check hash suffix.
            let mut comps = rest.split('/');
            comps.next();
            let kept: Vec<String> = comps
                .map(|c| match c.rsplit_once('-') {
                    Some((stem, h))
                        if h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()) =>
                    {
                        format!("{stem}-*")
                    }
                    _ => c.to_string(),
                })
                .collect();
            return format!("$TARGET/{}", kept.join("/"));
        }
        if let Some(rest) = under(&self.home) {
            return format!("$CARGO_HOME/{rest}");
        }
        if let Some(rest) = under(&self.repo) {
            return rest.to_string();
        }
        p.to_string()
    }

    /// An env-dep value: each path it names normalized (a value is a path or
    /// carries none).
    fn value(&self, v: &str) -> String {
        v.split(':')
            .map(|piece| self.path(piece))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// The text of `path` at `commit`, `Ok(None)` when the commit holds no such file.
fn at_commit(
    git: &dyn GitRunner,
    commit: &str,
    path: &str,
) -> std::result::Result<Option<String>, String> {
    let listed = git
        .git(&["ls-tree", commit, "--", path])
        .map_err(|e| e.to_string())?;
    if !listed.success() {
        return Err(format!("git cannot list {path} at {commit}"));
    }
    if listed.stdout_utf8().trim().is_empty() {
        return Ok(None);
    }
    let shown = git
        .git(&["show", &format!("{commit}:{path}")])
        .map_err(|e| e.to_string())?;
    if !shown.success() {
        return Err(format!("git cannot read {path} at {commit}"));
    }
    Ok(Some(shown.stdout_utf8()))
}

/// The root config, each file canonical TOML or `absent`.
fn root_config(src: &Sources<'_>) -> std::result::Result<String, String> {
    let mut out = String::new();
    for path in ROOT_CONFIG {
        match at_commit(src.git, src.commit, path)? {
            None => out.push_str(&format!("{path} absent\n")),
            Some(text) => {
                let table: aterm_toml::Table = aterm_toml::from_str(&text)
                    .map_err(|e| format!("{path} does not parse as TOML: {e}"))?;
                let canon = aterm_toml::to_string(&table)
                    .map_err(|e| format!("{path} does not write back as TOML: {e}"))?;
                out.push_str(&format!("{path}\n{canon}\n"));
            }
        }
    }
    Ok(out)
}

/// The tree id of the package directory `rel` at `commit`; inside a submodule,
/// the submodule's gitlink and the path inside it.
fn package_tree(
    git: &dyn GitRunner,
    commit: &str,
    rel: &str,
) -> std::result::Result<String, String> {
    let spec = if rel.is_empty() {
        format!("{commit}^{{tree}}")
    } else {
        format!("{commit}:{rel}")
    };
    if let Ok(out) = git.git(&["rev-parse", "--verify", "-q", &spec])
        && out.success()
    {
        return Ok(out.stdout_utf8().trim().to_string());
    }
    let mut at = Path::new(rel);
    while let Some(up) = at.parent() {
        let up_s = up.to_string_lossy();
        if up_s.is_empty() {
            break;
        }
        if let Ok(out) = git.git(&["ls-tree", commit, "--", &up_s])
            && out.success()
        {
            let line = out.stdout_utf8();
            let mut f = line.split_whitespace();
            if let (Some("160000"), Some("commit"), Some(sha)) = (f.next(), f.next(), f.next()) {
                let inside = Path::new(rel)
                    .strip_prefix(up)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                return Ok(format!("gitlink {sha} {inside}"));
            }
        }
        at = up;
    }
    Err(format!("git names no tree for {rel} at {commit}"))
}

/// `Cargo.lock`'s `[[package]]` entries by `(name, version)`: each entry's
/// `source` and its lines trimmed.
type Lock = BTreeMap<(String, String), Vec<(Option<String>, String)>>;

/// A `.d` file's inputs and its `# env-dep:` lines, or why it was not read.
type DepInfo = std::result::Result<(Vec<String>, Vec<String>), String>;

/// [`Lock`] of a `Cargo.lock`'s text.
fn lock_entries(text: &str) -> Lock {
    let mut out = Lock::new();
    for block in text.split("[[package]]").skip(1) {
        let lines: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let field = |k: &str| {
            lines.iter().find_map(|l| {
                let (key, v) = l.split_once('=')?;
                (key.trim() == k).then(|| v.trim().trim_matches('"').to_string())
            })
        };
        if let (Some(name), Some(version)) = (field("name"), field("version")) {
            out.entry((name, version))
                .or_default()
                .push((field("source"), lines.join("\n")));
        }
    }
    out
}

/// What the keys share: read once per run.
struct Shared {
    prover: std::result::Result<String, String>,
    pins: std::result::Result<String, String>,
    root_config: std::result::Result<String, String>,
    graph: std::result::Result<Graph, String>,
    lock: std::result::Result<Lock, String>,
    normal: Normal,
    /// Each path package's tree id, by its path in the checkout, read once.
    trees: HashMap<String, std::result::Result<String, String>>,
    /// Each `.d` file's inputs and env-deps, read once.
    dep_info: HashMap<PathBuf, DepInfo>,
    /// Each input file's content digest, read once.
    digests: HashMap<PathBuf, std::result::Result<String, String>>,
}

/// The keys of every subject, read from `src` ([`ClosureKey`]). Never fails: an input that
/// cannot be read is an unknown component of the keys that need it.
#[must_use]
pub fn closure_keys(src: &Sources<'_>, subjects: &[Subject<'_>]) -> BTreeMap<String, ClosureKey> {
    let graph = Graph::read(src.metadata);
    let normal = Normal::new(src, graph.as_ref().ok().map(|g| g.root.as_path()));
    let mut shared = Shared {
        prover: if src.prover.is_empty() {
            Err("the run names no prover".into())
        } else {
            Ok(src.prover.to_string())
        },
        pins: pins(src),
        root_config: root_config(src),
        graph,
        lock: at_commit(src.git, src.commit, "Cargo.lock").and_then(|t| {
            t.map(|t| lock_entries(&t))
                .ok_or_else(|| "the commit holds no Cargo.lock".to_string())
        }),
        normal,
        trees: HashMap::new(),
        dep_info: HashMap::new(),
        digests: HashMap::new(),
    };
    subjects
        .iter()
        .map(|s| (s.name.to_string(), key_of(src, &mut shared, s)))
        .collect()
}

fn pins(src: &Sources<'_>) -> std::result::Result<String, String> {
    if src.trustc.is_empty() || src.trustc == UNKNOWN {
        return Err("trustc -vV named no commit-hash".into());
    }
    let mut out = format!(
        "driver-id {}\ntrustc {}\n",
        one_line(src.driver_id),
        src.trustc
    );
    for (k, v) in src.lane_pins {
        out.push_str(&format!("lane-pin {k}={v}\n"));
    }
    Ok(out)
}

fn key_of(src: &Sources<'_>, sh: &mut Shared, s: &Subject<'_>) -> ClosureKey {
    let profile = s
        .argv
        .map(|a| format!("targo trust check {a} --allow-l0-gaps"))
        .ok_or_else(|| format!("the plan names no argv for {}", s.name));
    let fallback_lib = s.name.replace('-', "_");
    // A fork's own resolve: the lock its check used, as the lane kept it.
    let fork_lock = s.is_fork().then(|| {
        let at = s.beside(LOCK_SUFFIX);
        fs::read_to_string(&at)
            .map(|t| lock_entries(&t))
            .map_err(|e| {
                format!(
                    "a fork resolves on its own and the lane kept no lock of its check ({}: {e})",
                    at.display()
                )
            })
    });
    // The graph's part: the closure, its path packages' trees and its lock
    // entries, and the crate names whose `.d` files are read.
    let (packages, lock, own_lib, path_libs) = match &sh.graph {
        Err(why) => (Err(why.clone()), Err(why.clone()), fallback_lib, Vec::new()),
        Ok(g) => match g.find(s.spec) {
            Err(why) => (Err(why.clone()), Err(why), fallback_lib, Vec::new()),
            Ok(id) => {
                let own_lib = g.pkgs[id].lib.clone().unwrap_or(fallback_lib);
                let (pkgs, lock) = match &fork_lock {
                    None => {
                        let closure = g.closure(id);
                        let pkgs: Vec<&Pkg> =
                            closure.iter().filter_map(|c| g.pkgs.get(c)).collect();
                        let lock = lock(&sh.lock, &pkgs);
                        (Ok(pkgs), lock)
                    }
                    Some(Err(why)) => (Err(why.clone()), Err(why.clone())),
                    Some(Ok(own)) => (fork_paths(g, own), Ok(whole_lock(own))),
                };
                let path_libs = pkgs
                    .as_ref()
                    .map(|p| {
                        p.iter()
                            .filter(|p| p.source.is_none())
                            .filter_map(|p| p.lib.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let packages = pkgs.and_then(|p| packages(src, &mut sh.trees, g, &p));
                (packages, lock, own_lib, path_libs)
            }
        },
    };
    let (dep_info, env_deps) = match check_dep_files(&s.beside(DEPINFO_SUFFIX)) {
        Err(why) => (Err(why.clone()), Err(why)),
        Ok(files) => dep_info(sh, &files, &own_lib, &path_libs),
    };
    ClosureKey::of([
        sh.prover.clone(),
        sh.pins.clone(),
        sh.root_config.clone(),
        profile,
        packages,
        dep_info,
        env_deps,
        lock,
    ])
}

/// A fork's path packages: those its own lock lists with no source, each the
/// graph's path package of that name and version — or unknown.
fn fork_paths<'g>(g: &'g Graph, own: &Lock) -> std::result::Result<Vec<&'g Pkg>, String> {
    own.iter()
        .filter(|(_, entries)| entries.iter().any(|(src, _)| src.is_none()))
        .map(|((name, version), _)| {
            g.pkgs
                .values()
                .find(|p| p.source.is_none() && p.name == *name && p.version == *version)
                .ok_or_else(|| {
                    format!("{name}@{version}, a path package of the fork's own lock, is not in the graph")
                })
        })
        .collect()
}

/// Every entry of a lock, sorted: a fork's whole resolve.
fn whole_lock(own: &Lock) -> String {
    let mut lines: Vec<&str> = own
        .values()
        .flatten()
        .map(|(_, text)| text.as_str())
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines.join("\n\n")
}

fn packages(
    src: &Sources<'_>,
    trees: &mut HashMap<String, std::result::Result<String, String>>,
    g: &Graph,
    pkgs: &[&Pkg],
) -> std::result::Result<String, String> {
    let roots = spellings(&g.root);
    let mut lines = Vec::new();
    for p in pkgs.iter().filter(|p| p.source.is_none()) {
        let dir = p.dir.display().to_string();
        let rel = roots
            .iter()
            .find_map(|r| {
                if dir == *r {
                    Some(String::new())
                } else {
                    dir.strip_prefix(&format!("{r}/")).map(str::to_string)
                }
            })
            .ok_or_else(|| format!("{}@{} lies outside the checkout", p.name, p.version))?;
        let tree = trees
            .entry(rel.clone())
            .or_insert_with(|| package_tree(src.git, src.commit, &rel))
            .clone()?;
        lines.push(format!("{}@{} {rel} {tree}", p.name, p.version));
    }
    lines.sort();
    Ok(lines.join("\n"))
}

fn lock(
    lock: &std::result::Result<Lock, String>,
    pkgs: &[&Pkg],
) -> std::result::Result<String, String> {
    let lock = lock.as_ref().map_err(Clone::clone)?;
    let mut lines = Vec::new();
    for p in pkgs {
        let entries = lock
            .get(&(p.name.clone(), p.version.clone()))
            .map(Vec::as_slice)
            .unwrap_or_default();
        // The entry of this source, or the one entry of this name and version
        // (a git source's spelling differs between the graph and the lock).
        let entry = entries
            .iter()
            .find(|(src, _)| src == &p.source)
            .or(match entries {
                [one] => Some(one),
                _ => None,
            })
            .ok_or_else(|| {
                format!(
                    "Cargo.lock has no single entry for {}@{}",
                    p.name, p.version
                )
            })?;
        lines.push(entry.1.clone());
    }
    lines.sort();
    lines.dedup();
    Ok(lines.join("\n\n"))
}

/// The `dep-info` and `env-deps` components, from the `.d` files the check
/// wrote (`check`): inputs of the library's and its path packages'; env-deps
/// of every one.
fn dep_info(
    sh: &mut Shared,
    check: &BTreeMap<String, Vec<PathBuf>>,
    own: &str,
    path_libs: &[String],
) -> (
    std::result::Result<String, String>,
    std::result::Result<String, String>,
) {
    if !check.contains_key(own) {
        let why = format!("its check wrote no dep-info for {own}");
        return (Err(why.clone()), Err(why));
    }
    let mut input_files: Vec<PathBuf> = std::iter::once(own)
        .chain(path_libs.iter().map(String::as_str))
        .filter_map(|l| check.get(l))
        .flatten()
        .cloned()
        .collect();
    input_files.sort();
    input_files.dedup();
    let env_files: Vec<PathBuf> = check.values().flatten().cloned().collect();
    let mut read = |p: &PathBuf| -> DepInfo {
        sh.dep_info
            .entry(p.clone())
            .or_insert_with(|| {
                fs::read_to_string(p)
                    .map(|t| read_dep_info(&t))
                    .map_err(|e| format!("cannot read {}: {e}", p.display()))
            })
            .clone()
    };
    let mut env = BTreeSet::new();
    let mut env_err = None;
    for p in &env_files {
        match read(p) {
            Ok((_, e)) => env.extend(e),
            Err(why) => env_err = env_err.or(Some(why)),
        }
    }
    let mut inputs = BTreeSet::new();
    let mut in_err = None;
    for p in &input_files {
        match read(p) {
            Ok((i, _)) => inputs.extend(i),
            Err(why) => in_err = in_err.or(Some(why)),
        }
    }
    let env = match env_err {
        Some(why) => Err(why),
        None => Ok(env
            .iter()
            .map(|e| match e.split_once('=') {
                Some((n, v)) => format!("{n}={}", sh.normal.value(v)),
                None => e.clone(),
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join("\n")),
    };
    let inputs = match in_err {
        Some(why) => Err(why),
        None => {
            let mut lines = BTreeSet::new();
            let mut err = None;
            for i in &inputs {
                let full = if Path::new(i).is_absolute() {
                    PathBuf::from(i)
                } else {
                    sh.normal
                        .repo
                        .first()
                        .map_or_else(|| PathBuf::from(i), |r| Path::new(r).join(i))
                };
                let digest = sh
                    .digests
                    .entry(full.clone())
                    .or_insert_with(|| {
                        fs::read(&full)
                            .map(|b| hex(&Sha256::digest(&b)))
                            .map_err(|e| format!("cannot read {}: {e}", full.display()))
                    })
                    .clone();
                match digest {
                    Ok(d) => {
                        lines.insert(format!("{} {d}", sh.normal.path(i)));
                    }
                    Err(why) => {
                        err = err.or(Some(why));
                    }
                }
            }
            match err {
                Some(why) => Err(why),
                None => Ok(lines.into_iter().collect::<Vec<_>>().join("\n")),
            }
        }
    };
    (inputs, env)
}

/// The closure file of a record: [`CLOSURE_SCHEMA`], then one
/// `closure <library>\t<key>\t<component>=<digest>…` line per library.
#[must_use]
pub fn closure_record(keys: &BTreeMap<String, ClosureKey>) -> String {
    let mut out = format!("{CLOSURE_SCHEMA}\n");
    for (name, k) in keys {
        out.push_str(&format!("closure {}\t{}\n", one_line(name), k.fields()));
    }
    out
}

/// Read [`closure_record`] back, leniently: a file of another schema reads as
/// none, and a line that does not read whole is skipped — these are
/// measurements, and nothing that judges reads them.
#[must_use]
pub fn parse_closures(text: &str) -> BTreeMap<String, ClosureKey> {
    let mut lines = text.lines();
    if lines.next() != Some(CLOSURE_SCHEMA) {
        return BTreeMap::new();
    }
    lines
        .filter_map(|l| {
            let fields: Vec<&str> = l.strip_prefix("closure ")?.split('\t').collect();
            let (name, rest) = fields.split_first()?;
            Some(((*name).to_string(), ClosureKey::from_fields(rest)?))
        })
        .collect()
}

/// The closure file of the record filed under `facts_key` in `store`.
fn closure_path(store: &Path, facts_key: &str) -> PathBuf {
    store.join(CLOSURE_DIR).join(facts_key)
}

/// The keys filed beside the record under `facts_key`; none when there is no
/// such file.
#[must_use]
pub fn read_closures(store: &Path, facts_key: &str) -> BTreeMap<String, ClosureKey> {
    fs::read_to_string(closure_path(store, facts_key))
        .map(|t| parse_closures(&t))
        .unwrap_or_default()
}

/// File the keys beside the record under `facts_key`, replacing any there, and drop each
/// closure file whose record the store no longer holds (the receipts store keeps only its
/// newest files), so the directory follows the records.
///
/// # Errors
/// The file could not be written.
pub fn file_closures(
    store: &Path,
    facts_key: &str,
    keys: &BTreeMap<String, ClosureKey>,
) -> std::result::Result<PathBuf, String> {
    let dir = store.join(CLOSURE_DIR);
    let path = closure_path(store, facts_key);
    let tmp = dir.join(format!(".{facts_key}.{}", std::process::id()));
    fs::create_dir_all(&dir)
        .and_then(|()| fs::write(&tmp, closure_record(keys)))
        .and_then(|()| fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("could not write {}: {e}", path.display())
        })?;
    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && !store.join(format!("{FACTS_PREFIX}{name}")).exists() {
            let _ = fs::remove_file(e.path());
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::testing::Git;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn material() -> [std::result::Result<String, String>; 8] {
        COMPONENTS.map(|c| Ok(format!("{c} material")))
    }

    /// The key is a function of its components: equal material, equal key;
    /// any one component moved moves the key, and `moved` names that one alone;
    /// an unreadable one makes the key unknown — never a digest over the rest —
    /// and an unknown is never the same as anything, itself included.
    #[test]
    fn a_key_is_its_components_and_names_the_one_that_moved() {
        let base = ClosureKey::of(material());
        assert_eq!(base, ClosureKey::of(material()), "stable on equal inputs");
        assert!(base.is_known());
        assert!(base.moved(&ClosureKey::of(material())).is_empty());
        for (i, name) in COMPONENTS.iter().enumerate() {
            let mut m = material();
            m[i] = Ok("another".into());
            let k = ClosureKey::of(m);
            assert_ne!(k.key, base.key, "{name} moves the key");
            assert_eq!(base.moved(&k), vec![(*name).to_string()]);
            let mut m = material();
            m[i] = Err("could not read it".into());
            let u = ClosureKey::of(m);
            assert_eq!(u.key, UNKNOWN, "{name} unread: the key is unknown");
            assert!(!u.is_known());
            assert_eq!(u.parts[i].1, "unknown:could not read it");
            assert_eq!(u.moved(&u), vec![(*name).to_string()], "unknown != unknown");
        }
        // as recorded, and read back only whole and consistent
        let f = base.fields();
        let fields: Vec<&str> = f.split('\t').collect();
        assert_eq!(ClosureKey::from_fields(&fields), Some(base.clone()));
        let tampered = f.replacen(&base.key, &"0".repeat(64), 1);
        let t: Vec<&str> = tampered.split('\t').collect();
        assert_eq!(
            ClosureKey::from_fields(&t),
            None,
            "a key its parts do not give"
        );
        assert_eq!(
            ClosureKey::from_fields(&fields[..8]),
            None,
            "a component missing"
        );
        let swapped = f.replace("\tprover=", "\tprover2=");
        let s: Vec<&str> = swapped.split('\t').collect();
        assert_eq!(ClosureKey::from_fields(&s), None, "a component misnamed");
    }

    /// A fork-feature row (`alpha+sealed@1.0.0`) keys the package it checks.
    #[test]
    fn a_fork_feature_row_finds_its_package() {
        let pkg = |name: &str, version: &str| Pkg {
            name: name.into(),
            version: version.into(),
            source: None,
            dir: PathBuf::new(),
            lib: None,
        };
        let g = Graph {
            root: PathBuf::new(),
            pkgs: BTreeMap::from([
                ("alpha-id".to_string(), pkg("alpha", "1.0.0")),
                ("beta-id".to_string(), pkg("beta", "1.0.0")),
            ]),
            deps: BTreeMap::new(),
        };
        assert_eq!(g.find("alpha+sealed@1.0.0"), Ok("alpha-id"));
        assert_eq!(g.find("alpha@1.0.0"), Ok("alpha-id"));
        assert!(g.find("alpha+sealed@2.0.0").is_err());
    }

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A checkout with a workspace (alpha -> beta, a path package, and `reg`,
    /// a registry one), its lock and config, the graph the lane read, and a slot
    /// holding the `.d` files a check of alpha leaves.
    struct World {
        root: PathBuf,
        pins: BTreeMap<String, String>,
        /// HEAD as [`World::commit`] last left it, so a key spawns no `rev-parse HEAD`.
        head: std::cell::RefCell<String>,
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    const LOCK: &str = "# generated\nversion = 4\n\n[[package]]\nname = \"alpha\"\nversion = \"1.0.0\"\ndependencies = [\n \"beta\",\n \"reg\",\n]\n\n[[package]]\nname = \"beta\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"reg\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aaaa\"\n";
    const HASH: &str = "0123456789abcdef";

    impl World {
        fn new() -> World {
            let root = std::env::temp_dir().join(format!(
                "aterm-trust-closure-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("work")).unwrap();
            let w = World {
                root,
                pins: BTreeMap::from([("SOURCE_DATE_EPOCH".to_string(), "0".to_string())]),
                head: std::cell::RefCell::default(),
            };
            crate::ledger::testing::init(
                &w.work(),
                &[
                    ("user.name", "Test"),
                    ("user.email", "test@example.invalid"),
                    ("commit.gpgsign", "false"),
                ],
            );
            w.write_all(&[
                (
                    "Cargo.toml",
                    "[workspace]\nmembers = [\"crates/*\"]\n\n[profile.trust-lane]\ninherits = \"dev\"\n",
                ),
                (
                    ".cargo/config.toml",
                    "# the off switch\n[target.'cfg(trust_verify)']\nrustflags = [\"-Ztrust-verify=off\"]\n",
                ),
                ("Cargo.lock", LOCK),
                ("crates/alpha/Cargo.toml", "[package]\nname = \"alpha\"\n"),
                ("crates/alpha/src/lib.rs", "pub fn a() {}\n"),
                ("crates/beta/Cargo.toml", "[package]\nname = \"beta\"\n"),
                ("crates/beta/src/lib.rs", "pub fn b() {}\n"),
                ("crates/beta/README", "beta\n"),
                ("docs/shared.txt", "shared\n"),
            ]);
            w.commit();
            w.slot(0, HASH);
            fs::write(w.metadata(), w.graph()).unwrap();
            w
        }
        fn work(&self) -> PathBuf {
            self.root.join("work")
        }
        fn troot(&self) -> PathBuf {
            self.root.join("troot")
        }
        fn metadata(&self) -> PathBuf {
            self.root.join("metadata.json")
        }
        /// alpha's check log; the lane lists its `.d` files beside it.
        fn log(&self) -> PathBuf {
            self.root.join("logs/alpha@1.0.0.log")
        }
        fn list(&self) -> PathBuf {
            self.root
                .join(format!("logs/alpha@1.0.0.log{DEPINFO_SUFFIX}"))
        }
        fn git(&self, args: &[&str]) -> String {
            let out = crate::ledger::testing::git()
                .arg("-C")
                .arg(self.work())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        fn write_all(&self, files: &[(&str, &str)]) {
            for (path, body) in files {
                let full = self.work().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, body).unwrap();
            }
        }
        fn commit(&self) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "--allow-empty", "-m", "c"]);
            let head = self.git(&["rev-parse", "HEAD"]);
            self.head.replace(head.clone());
            head
        }
        /// The `.d` files one check of alpha leaves in slot `j<n>`, its units
        /// suffixed `-<hash>` as a check's session nonce makes them, and the
        /// lane's list of them beside alpha's log.
        fn slot(&self, n: usize, hash: &str) {
            let base = self.troot().join(format!("j{n}/trust-lane"));
            let deps = base.join("deps");
            let out = base.join(format!("build/alpha-{hash}/out"));
            fs::create_dir_all(&deps).unwrap();
            fs::create_dir_all(&out).unwrap();
            fs::write(out.join("gen.rs"), "// generated\n").unwrap();
            let gen_rs = out.join("gen.rs").display().to_string();
            fs::write(
                deps.join(format!("alpha-{hash}.d")),
                format!(
                    "{d}/alpha-{hash}.rmeta: crates/alpha/src/lib.rs docs/shared.txt {gen_rs}\n\n\
                     crates/alpha/src/lib.rs:\ndocs/shared.txt:\n{gen_rs}:\n\n\
                     # env-dep:CARGO_PKG_VERSION=1.0.0\n# env-dep:OUT_DIR={o}\n",
                    d = deps.display(),
                    o = out.display(),
                ),
            )
            .unwrap();
            fs::write(
                deps.join(format!("beta-{hash}.d")),
                "x.rmeta: crates/beta/src/lib.rs\n\ncrates/beta/src/lib.rs:\n",
            )
            .unwrap();
            let reg = self.root.join("home/registry/src/reg-1.0.0/src/lib.rs");
            fs::create_dir_all(reg.parent().unwrap()).unwrap();
            fs::write(&reg, "pub fn r() {}\n").unwrap();
            fs::write(
                deps.join(format!("reg-{hash}.d")),
                format!(
                    "x.rmeta: {r}\n\n{r}:\n\n# env-dep:MACOSX_DEPLOYMENT_TARGET\n",
                    r = reg.display()
                ),
            )
            .unwrap();
            fs::create_dir_all(self.list().parent().unwrap()).unwrap();
            let listed: String = ["alpha", "beta", "reg"]
                .iter()
                .map(|c| format!("{}\n", deps.join(format!("{c}-{hash}.d")).display()))
                .collect();
            fs::write(self.list(), listed).unwrap();
        }
        fn graph(&self) -> String {
            let w = self.work().display().to_string();
            let pkg = |name: &str, source: &str, dir: &str| {
                format!(
                    "{{\"id\":\"{name}-id\",\"name\":\"{name}\",\"version\":\"1.0.0\",\"source\":{source},\
                     \"manifest_path\":\"{dir}/Cargo.toml\",\"targets\":[{{\"name\":\"{name}\",\"kind\":[\"lib\"]}}]}}"
                )
            };
            let reg = self
                .root
                .join("home/registry/src/reg-1.0.0")
                .display()
                .to_string();
            format!(
                "{{\"workspace_root\":\"{w}\",\"packages\":[{},{},{},{}],\"resolve\":{{\"nodes\":[\
                 {{\"id\":\"alpha-id\",\"deps\":[\
                   {{\"pkg\":\"beta-id\",\"dep_kinds\":[{{\"kind\":null,\"target\":null}}]}},\
                   {{\"pkg\":\"reg-id\",\"dep_kinds\":[{{\"kind\":\"build\",\"target\":null}}]}},\
                   {{\"pkg\":\"devonly-id\",\"dep_kinds\":[{{\"kind\":\"dev\",\"target\":null}}]}}]}},\
                 {{\"id\":\"beta-id\",\"deps\":[]}},{{\"id\":\"reg-id\",\"deps\":[]}},\
                 {{\"id\":\"devonly-id\",\"deps\":[]}}]}}}}",
                pkg("alpha", "null", &format!("{w}/crates/alpha")),
                pkg("beta", "null", &format!("{w}/crates/beta")),
                pkg(
                    "reg",
                    "\"registry+https://github.com/rust-lang/crates.io-index\"",
                    &reg
                ),
                // alpha's dev-dependency: in no tree and no lock entry, so a
                // closure that followed a dev edge would read unknown
                pkg("devonly", "null", &format!("{w}/crates/devonly")),
            )
        }
        fn key_with(&self, prover: &str, argv: Option<&str>, trustc: &str) -> ClosureKey {
            let git = Git::new(self.work());
            self.keys_with(&git, prover, argv, trustc, &["alpha"])
                .remove("alpha")
                .unwrap()
        }
        /// The keys of `names`, each checked as alpha was (one log, one list).
        fn keys_with(
            &self,
            git: &dyn GitRunner,
            prover: &str,
            argv: Option<&str>,
            trustc: &str,
            names: &[&str],
        ) -> BTreeMap<String, ClosureKey> {
            let head = self.head.borrow().clone();
            let metadata = self.metadata();
            let troot = self.troot();
            let home = self.root.join("home");
            let src = Sources {
                git,
                repo: &self.work(),
                commit: &head,
                prover,
                trustc,
                driver_id: "targo 1.99.0-dev",
                lane_pins: &self.pins,
                metadata: metadata.is_file().then_some(metadata.as_path()),
                target_root: &troot,
                cargo_home: Some(&home),
            };
            let subjects: Vec<Subject<'_>> = names
                .iter()
                .map(|&name| Subject {
                    name,
                    spec: "alpha@1.0.0",
                    argv,
                    log: self.log(),
                })
                .collect();
            closure_keys(&src, &subjects)
        }
        fn key(&self) -> ClosureKey {
            self.key_with(
                "prover-a",
                Some("--profile trust-lane -p alpha --lib"),
                "abc",
            )
        }
    }

    /// The same inputs read the same key in another slot under another nonce; each input
    /// moved alone moves its own component.
    #[test]
    fn each_input_the_check_read_moves_its_own_component() {
        let w = World::new();
        let base = w.key();
        assert!(base.is_known(), "{base:?}");
        assert_eq!(w.key(), base, "a rerun on the same inputs");
        fs::remove_dir_all(w.troot()).unwrap();
        w.slot(3, "fedcba9876543210");
        assert_eq!(w.key(), base, "another slot and nonce, the same inputs");
        // a comment or a reordering of the root config reads the same
        w.write_all(&[(
            ".cargo/config.toml",
            "[target.'cfg(trust_verify)']\n# reworded\nrustflags = [ \"-Ztrust-verify=off\" ]\n",
        )]);
        w.commit();
        assert_eq!(w.key(), base, "the root config, normalized");
        let moved = |k: &ClosureKey| base.moved(k);
        assert_eq!(
            moved(&w.key_with(
                "prover-b",
                Some("--profile trust-lane -p alpha --lib"),
                "abc"
            )),
            ["prover"]
        );
        assert_eq!(
            moved(&w.key_with(
                "prover-a",
                Some("--profile trust-lane -p alpha --lib"),
                "abd"
            )),
            ["pins"]
        );
        assert_eq!(
            moved(&w.key_with("prover-a", Some("-p alpha --lib"), "abc")),
            ["profile"]
        );
        let mut pinned = World::new();
        let pinned_base = pinned.key();
        pinned.pins.insert("SOURCE_DATE_EPOCH".into(), "1".into());
        assert_eq!(pinned_base.moved(&pinned.key()), ["pins"]);
        // one input at a time, each committed as the lane's clean checkout is
        let steps: [(&str, &str, &[&str]); 4] = [
            (
                ".cargo/config.toml",
                "[build]\njobs = 2\n",
                &["root-config"],
            ),
            ("crates/beta/README", "beta, again\n", &["packages"]),
            ("docs/shared.txt", "shared, again\n", &["dep-info"]),
            ("Cargo.lock", &LOCK.replace("aaaa", "bbbb"), &["lock"]),
        ];
        for (path, body, want) in steps {
            let w = World::new();
            let before = w.key();
            w.write_all(&[(path, body)]);
            w.commit();
            assert_eq!(before.moved(&w.key()), want, "{path}");
        }
        // a source file of a path package is in its tree AND its dep-info
        let w2 = World::new();
        let before = w2.key();
        w2.write_all(&[("crates/beta/src/lib.rs", "pub fn b2() {}\n")]);
        w2.commit();
        assert_eq!(before.moved(&w2.key()), ["packages", "dep-info"]);
        // a registry package's compile-time read of the environment
        let w3 = World::new();
        let before = w3.key();
        let d = w3.troot().join(format!("j0/trust-lane/deps/reg-{HASH}.d"));
        let text = fs::read_to_string(&d).unwrap();
        fs::write(
            &d,
            text.replace("MACOSX_DEPLOYMENT_TARGET", "MACOSX_DEPLOYMENT_TARGET=11.0"),
        )
        .unwrap();
        assert_eq!(before.moved(&w3.key()), ["env-deps"]);
    }

    /// Each input the lane cannot read makes its component, and so the key, unknown.
    #[test]
    fn an_input_that_cannot_be_read_makes_the_key_unknown() {
        let unknown = |k: ClosureKey, want: &[&str]| {
            assert_eq!(k.key, UNKNOWN, "{k:?}");
            let named: Vec<&str> = k
                .parts
                .iter()
                .filter(|(_, d)| d.starts_with(UNKNOWN))
                .map(|(n, _)| n.as_str())
                .collect();
            assert_eq!(named, want, "{k:?}");
        };
        let w = World::new();
        unknown(w.key_with("", Some("x"), "abc"), &["prover"]);
        unknown(w.key_with("p", Some("x"), "unknown"), &["pins"]);
        unknown(w.key_with("p", None, "abc"), &["profile"]);
        fs::remove_file(w.root.join("home/registry/src/reg-1.0.0/src/lib.rs")).unwrap();
        // a registry input is the lock's, not the dep-info's: still known
        assert!(w.key().is_known());
        fs::remove_file(w.work().join("docs/shared.txt")).unwrap();
        unknown(w.key(), &["dep-info"]);
        let w = World::new();
        fs::remove_file(w.troot().join(format!("j0/trust-lane/deps/alpha-{HASH}.d"))).unwrap();
        unknown(w.key(), &["dep-info", "env-deps"]);
        // the lane listed nothing for this check (an older lane, a listing
        // that failed)
        let w = World::new();
        fs::remove_file(w.list()).unwrap();
        unknown(w.key(), &["dep-info", "env-deps"]);
        let w = World::new();
        fs::remove_file(w.metadata()).unwrap();
        unknown(w.key(), &["packages", "lock"]);
        let w = World::new();
        fs::write(w.metadata(), "{ not json").unwrap();
        unknown(w.key(), &["packages", "lock"]);
        let w = World::new();
        w.write_all(&[(
            "Cargo.lock",
            &LOCK.replace("name = \"reg\"", "name = \"other\""),
        )]);
        w.commit();
        unknown(w.key(), &["lock"]);
        let w = World::new();
        w.write_all(&[(".cargo/config.toml", "[build\n")]);
        w.commit();
        unknown(w.key(), &["root-config"]);
    }

    /// A `.d` another library's check left in a slot moves nothing and stands in for nothing
    /// this check did not write.
    #[test]
    fn a_key_reads_only_the_dep_info_its_own_check_wrote() {
        let w = World::new();
        let base = w.key();
        assert!(base.is_known(), "{base:?}");
        let other = w.troot().join("j1/trust-lane/deps");
        fs::create_dir_all(&other).unwrap();
        w.write_all(&[("docs/feature.txt", "only with the feature\n")]);
        w.commit();
        for c in ["alpha", "beta"] {
            fs::write(
                other.join(format!("{c}-fedcba9876543210.d")),
                "x.rmeta: docs/feature.txt\n\ndocs/feature.txt:\n\n# env-dep:FEATURE_ONLY=1\n",
            )
            .unwrap();
        }
        assert_eq!(w.key(), base, "another check's compiles are not this one's");
        // this check wrote no `.d` of its own: another check's copy of
        // alpha does not make the key known
        let listed = fs::read_to_string(w.list()).unwrap();
        let without: String = listed
            .lines()
            .filter(|l| !l.contains("/alpha-"))
            .map(|l| format!("{l}\n"))
            .collect();
        fs::write(w.list(), without).unwrap();
        let k = w.key();
        assert_eq!(k.key, UNKNOWN, "{k:?}");
        assert!(
            k.parts[5]
                .1
                .starts_with("unknown:its check wrote no dep-info for alpha")
        );
    }

    /// A fork's `packages` and `lock` come from the lock its own check used, kept beside its
    /// log (unknown without it); the root lock and the root graph's closure move nothing.
    #[test]
    fn a_fork_keys_the_lock_its_own_check_resolved() {
        let fork = Some("--manifest-path crates/alpha/Cargo.toml --lib");
        let w = World::new();
        let key = || w.key_with("prover-a", fork, "abc");
        let k = key();
        assert_eq!(k.key, UNKNOWN, "{k:?}");
        let unknown: Vec<&str> = k
            .parts
            .iter()
            .filter(|(_, d)| d.starts_with(UNKNOWN))
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(unknown, ["packages", "lock"], "{k:?}");
        let own = w.root.join(format!("logs/alpha@1.0.0.log{LOCK_SUFFIX}"));
        let fork_lock = "version = 4\n\n[[package]]\nname = \"alpha\"\nversion = \"1.0.0\"\ndependencies = [\n \"reg\",\n]\n\n[[package]]\nname = \"reg\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aaaa\"\n";
        fs::write(&own, fork_lock).unwrap();
        let base = key();
        assert!(base.is_known(), "{base:?}");
        assert_eq!(key(), base);
        // the root lock is not the fork's
        w.write_all(&[("Cargo.lock", &LOCK.replace("aaaa", "bbbb"))]);
        w.commit();
        assert!(base.moved(&key()).is_empty(), "the root lock moved");
        // nor is the root graph's closure: beta, alpha's path dependency in
        // the root resolve, is not in the fork's own
        w.write_all(&[("crates/beta/README", "beta, again\n")]);
        w.commit();
        assert!(base.moved(&key()).is_empty(), "beta's tree moved");
        // a newer registry release the fork's own resolve took
        fs::write(&own, fork_lock.replace("aaaa", "cccc")).unwrap();
        assert_eq!(base.moved(&key()), ["lock"]);
        // its own tree still keys it
        fs::write(&own, fork_lock).unwrap();
        w.write_all(&[(
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\n# x\n",
        )]);
        w.commit();
        assert_eq!(base.moved(&key()), ["packages"]);
        // a path package of its own lock the graph does not hold
        fs::write(
            &own,
            format!("{fork_lock}\n[[package]]\nname = \"ghost\"\nversion = \"1.0.0\"\n"),
        )
        .unwrap();
        assert!(key().parts[4].1.starts_with("unknown:ghost@1.0.0"));
    }

    /// A git runner that counts the tree lookups it is asked for.
    struct Counting {
        cli: Git,
        rev_parses: AtomicUsize,
    }

    impl GitRunner for Counting {
        fn git(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
            if args.first() == Some(&"rev-parse") {
                self.rev_parses.fetch_add(1, Ordering::Relaxed);
            }
            self.cli.git(args)
        }
    }

    /// Each path package's tree is asked of git once per run, however many
    /// gated libraries have it in their closure.
    #[test]
    fn a_package_tree_is_read_once_per_run() {
        let w = World::new();
        let git = Counting {
            cli: Git::new(w.work()),
            rev_parses: AtomicUsize::new(0),
        };
        let argv = Some("-p alpha --lib");
        let keys = w.keys_with(&git, "p", argv, "abc", &["alpha", "again", "thrice"]);
        assert_eq!(keys.len(), 3);
        assert!(keys.values().all(|k| k == &keys["alpha"] && k.is_known()));
        // alpha's and beta's trees, once each
        assert_eq!(git.rev_parses.load(Ordering::Relaxed), 2, "{keys:?}");
    }

    /// A closure file reads back whole and leniently (a bad line is skipped, another schema
    /// reads as none) and is dropped once the store no longer holds its record.
    #[test]
    fn closure_files_round_trip_and_follow_their_records() {
        let w = World::new();
        let store = w.root.join("store");
        fs::create_dir_all(&store).unwrap();
        let keys = BTreeMap::from([
            ("alpha".to_string(), ClosureKey::of(material())),
            (
                "beta".to_string(),
                ClosureKey::of(COMPONENTS.map(|_| Err("unread".into()))),
            ),
        ]);
        fs::write(store.join(format!("{FACTS_PREFIX}k1")), "a record").unwrap();
        let at = file_closures(&store, "k1", &keys).unwrap();
        assert_eq!(at, store.join(CLOSURE_DIR).join("k1"));
        assert_eq!(read_closures(&store, "k1"), keys);
        assert!(read_closures(&store, "k2").is_empty());
        let text = fs::read_to_string(&at).unwrap();
        fs::write(&at, format!("{text}closure gamma\tnot a key\n")).unwrap();
        assert_eq!(read_closures(&store, "k1"), keys, "a bad line is skipped");
        fs::write(
            &at,
            text.replace(CLOSURE_SCHEMA, "aterm-trust closure key 0"),
        )
        .unwrap();
        assert!(read_closures(&store, "k1").is_empty(), "another schema");
        // k1's record leaves the store: filing k2 drops k1's closures
        fs::remove_file(store.join(format!("{FACTS_PREFIX}k1"))).unwrap();
        fs::write(store.join(format!("{FACTS_PREFIX}k2")), "a record").unwrap();
        file_closures(&store, "k2", &keys).unwrap();
        assert!(!at.exists());
        assert_eq!(read_closures(&store, "k2"), keys);
    }
}
