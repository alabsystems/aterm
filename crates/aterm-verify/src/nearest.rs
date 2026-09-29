// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEAREST BASE — OPT-IN, OFF BY DEFAULT, PENDING THE OWNER'S DECISION
//! (`tools/verify.sh --nearest-base`, 2026-09-28).
//!
//! WHY. The differential verdict ([`crate::differential`]) excuses a red only
//! against a receipt for the EXACT merge-base M of HEAD and `origin/main`.
//! Main moves about forty commits an hour and a baseline takes an hour, so
//! almost no run finds one, and almost every run is judged by the absolute
//! rule. The owner has not approved relaxing that default, and this module
//! does not: without `--nearest-base` nothing here is ever called, and the
//! gate's every line and every receipt are byte-for-byte what they were.
//!
//! WHAT THE FLAG DOES. Only when M has no receipt that serves the run (the
//! exact rule found none — none at all, one made by other tools, a narrowed
//! one), the run may be judged against the NEWEST first-parent ancestor B of
//! M that has one, within [`Bound::DEFAULT`] — at most [`NEAREST_COMMITS`]
//! commits and [`NEAREST_SPAN_SECS`] of committer time before M ([`find`]).
//! Every other reason for the absolute rule (no `origin/main`, a stale one, a
//! remote that cannot be read, a run on main, a baseline, `--measure`) still
//! holds: the flag replaces only "M has no receipt". Past the bound, or with
//! no receipt inside it, the run is judged by the absolute rule and says so.
//!
//! WHAT B MAY EXCUSE. B's reds are main's at B, not at M: main may have fixed
//! one between them, and a branch that breaks it again reads as main's. So a
//! red is inherited from B only when nothing that decides it can have
//! changed between B and M — its BLAST RADIUS ([`qualify`]):
//!
//!  * it is a FAILED TEST, whose owning crate its id names (`-p <crate>`,
//!    [`crate::ladder::Finding::package`]). A whole-row red (a lint, a guard,
//!    a suite, a build) is never inherited through a non-exact base, however
//!    its row is shaped: what it covers is the tree, not a crate ([`WHOLE_ROW`]);
//!  * the crate and every crate it builds on — its path dependencies, normal,
//!    dev and build, read transitively from MAIN'S MANIFESTS AT M (git, not the
//!    worktree: the graph that decided main's red is main's), `workspace =
//!    true` resolved through the root's `[workspace.dependencies]` — have no
//!    file that `git diff --name-only --no-renames B M` names
//!    ([`RADIUS_CHANGED`]); none of them lives in a git submodule, whose
//!    manifests and files are not in main's tree ([`SUBMODULE`]);
//!  * nothing that re-plans every crate changed: ANY `Cargo.toml` (cargo
//!    unifies features across the workspace, so a crate outside the radius
//!    can change how one inside it compiles from its own manifest, with
//!    `Cargo.lock` unmoved — [`is_manifest`]), `Cargo.lock`,
//!    `rust-toolchain(.toml)`, anything under `.cargo/`, or a
//!    `[patch]`/`[replace]` path of the root manifest ([`WORKSPACE_CHANGED`]);
//!  * the gate itself did not change ([`GATE_PATHS`], [`GATE_CHANGED`]): B's
//!    receipt was fingerprinted by the gate as it was at B;
//!  * no crate in that set NAMES A PATH OUTSIDE ITS OWN DIRECTORY
//!    ([`READS_OUTSIDE`]). A crate's tests read files no dependency edge
//!    names (`changed.rs` counted 56 of 93 members doing so on 2026-09-23:
//!    `aterm-census` reads `aterm-gui`'s sources, `aterm-release` reads
//!    `tools/`), and a change to one of those between B and M is invisible to
//!    the graph. The check is LEXICAL and over-approximating: any file under
//!    the crate's directory at M (its `Cargo.toml` read as TOML instead) that
//!    holds one of [`READ_OUTSIDE_MARKERS`] disqualifies it — a `../`, a
//!    `.parent()`, a `CARGO_MANIFEST_DIR`, a `"git"` — and so does any
//!    manifest string that climbs out, and any symlink under it whose target
//!    leaves it ([`Tree::mark_symlink`]). It is a heuristic, not a proof: a
//!    test that reaches the tree some other way (an absolute path from the
//!    environment, a path assembled from pieces no marker names) is the
//!    residual gap the owner's decision has to weigh.
//!
//! ANYTHING UNKNOWABLE IS NEW. A crate M's manifests do not name (or name
//! twice), a manifest the reader cannot parse, a path dependency whose
//! directory holds no manifest at M, a path that climbs above the repository,
//! a git call that fails: the red is NEW, or — when the diff or the tree
//! cannot be read at all — the run is judged by the absolute rule. Never
//! inherited.
//!
//! WHAT IT PRINTS AND RECORDS. The `verify: base …` line names B, M, how far
//! apart they are and how many crates qualify ([`crate::differential::Plan::header_line`]);
//! the verdict names why each inherited red qualified; the receipt carries
//! `base-mode nearest <B>` beside `base <B>` ([`crate::receipt::Receipt::base_nearest`]).
//! The release cutter reads such a receipt exactly as any other: `inherited`
//! lines make a tree pass gate only a commit with the same parents, and
//! nothing about a cut's requirements moves (`aterm-release` `gates.rs`) — so
//! a red excused here carries through to cut eligibility, on weaker evidence
//! than an exact base's (docs/PROCESS.md §7 offers the owner the stricter
//! variant).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::Stdio;

use crate::build_config::{self, Value};
use crate::differential::{self, Found, Tools, short};
use crate::ladder::Finding;

/// How many first-parent commits before the merge-base the walk reads.
pub const NEAREST_COMMITS: usize = 200;

/// How much older, in committer time, than the merge-base the nearest base
/// may be.
pub const NEAREST_SPAN_SECS: u64 = 24 * 60 * 60;

/// How far before the merge-base a nearest base may sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    /// First-parent commits before the merge-base.
    pub commits: usize,
    /// Seconds of committer time before the merge-base's.
    pub span_secs: u64,
}

impl Bound {
    /// [`NEAREST_COMMITS`] and [`NEAREST_SPAN_SECS`].
    pub const DEFAULT: Self = Self {
        commits: NEAREST_COMMITS,
        span_secs: NEAREST_SPAN_SECS,
    };
}

/// Root files that change how every crate is built: a change to one between
/// the nearest base and the merge-base disqualifies every red.
pub const WORKSPACE_FILES: [&str; 4] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
];

/// The root directory whose every file changes how every crate is built.
pub const WORKSPACE_DIR: &str = ".cargo";

/// Is `path` a manifest? A change to ANY `Cargo.toml` between the nearest
/// base and the merge-base disqualifies every red, not only the root's: the
/// test stage builds the whole workspace, and cargo's resolver unifies
/// features across every member, so a crate OUTSIDE a crate's blast radius
/// can switch on a feature of a dependency INSIDE it (a path crate or a
/// registry one) by editing its own manifest alone — changing how the
/// qualifying crate's tests compile while `Cargo.lock` stays byte-identical.
#[must_use]
pub fn is_manifest(path: &str) -> bool {
    path == "Cargo.toml" || path.ends_with("/Cargo.toml")
}

/// The gate's own sources: a change to one between the nearest base and the
/// merge-base disqualifies every red, because the ids and fingerprints in the
/// nearest base's receipt were made by the gate as it was at that base, and a
/// normalization that changed since could make two different failures hash
/// alike. `aterm-verify` has no dependencies (its manifest says why), so its
/// directory and the shell shim that runs it are all of it.
pub const GATE_PATHS: [&str; 2] = ["crates/aterm-verify", "tools/verify.sh"];

/// The words that make a file NAME A PATH OUTSIDE ITS CRATE'S DIRECTORY, as
/// the lexical scan reads them ([`READS_OUTSIDE`]): a climb (`../`, `/..`,
/// `".."`, `ParentDir`), a walk up (`.parent()`, `.ancestors()`, and their
/// UFCS spellings `Path::parent(p)` / `Path::ancestors(p)`), any use at all of
/// the crate's own directory or the process's (`CARGO_MANIFEST_DIR`,
/// `current_dir`, `current_exe` — from which `PathBuf::pop()` or string
/// surgery such as `rsplit_once('/')` climbs with no other marker), the
/// repository's root asked for (`show-toplevel`, `CARGO_WORKSPACE_DIR`), or
/// git itself run. Symlinks are not text and are read separately
/// ([`Tree::mark_symlink`]).
pub const READ_OUTSIDE_MARKERS: [&str; 14] = [
    "../",
    "/..",
    "\"..\"",
    "ParentDir",
    ".parent()",
    ".ancestors()",
    "::parent(",
    "::ancestors(",
    "CARGO_MANIFEST_DIR",
    "current_dir",
    "current_exe",
    "show-toplevel",
    "CARGO_WORKSPACE_DIR",
    "\"git\"",
];

/// Never inherited through a nearest base: a whole-row red.
pub const WHOLE_ROW: &str = "the base is not the merge-base (--nearest-base), and a red that is \
     not one failed test of a named crate — a whole row — is inherited only from the exact \
     merge-base";

/// Never inherited through a nearest base: a crate main's manifests at the
/// merge-base do not name.
pub const NO_CRATE: &str = "--nearest-base: main's manifests at the merge-base name no such \
     crate (or name it twice), so what it builds on is unknown";

/// Never inherited through a nearest base: a dependency set that cannot be read.
pub const UNREADABLE: &str = "--nearest-base: what its crate builds on could not be read from \
     main's manifests at the merge-base, so it may have changed since the nearest base";

/// Never inherited through a nearest base: a crate that names paths outside
/// its directory.
pub const READS_OUTSIDE: &str = "--nearest-base: its crate, or one it builds on, names a path \
     outside its own directory (a lexical scan), so a file no dependency edge names may decide \
     it";

/// Never inherited through a nearest base: a workspace-wide build input changed.
pub const WORKSPACE_CHANGED: &str = "--nearest-base: a Cargo.toml (any crate's: features unify \
     across the workspace), Cargo.lock, the toolchain pin, .cargo/ or a [patch] path changed \
     between the nearest base and the merge-base";

/// Never inherited through a nearest base: the gate itself changed.
pub const GATE_CHANGED: &str = "--nearest-base: the gate itself (crates/aterm-verify, \
     tools/verify.sh) changed between the nearest base and the merge-base, so the nearest base's \
     ids and fingerprints may not be this run's";

/// Never inherited through a nearest base: a crate in the blast radius lives
/// in a git submodule, whose manifests and files the gate does not read.
pub const SUBMODULE: &str = "--nearest-base: its crate, or one it builds on, lives in a git \
     submodule, whose own manifests and files the gate does not read, so what it builds on is \
     unknown";

/// Never inherited through a nearest base: its blast radius changed.
pub const RADIUS_CHANGED: &str = "--nearest-base: a file of its crate, or of a crate it builds \
     on, changed between the nearest base and the merge-base";

/// What a nearest base may excuse: main's receipt is for `Found::commit` (B),
/// `steps` first-parent commits and `span_secs` of committer time before the
/// merge-base; each crate main's manifests name at the merge-base, with why
/// its failed tests may be inherited from B, or why not ([`qualify`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    /// The merge-base M, which had no receipt that serves.
    pub merge_base: String,
    pub steps: usize,
    pub span_secs: u64,
    pub crates: BTreeMap<String, Result<String, &'static str>>,
}

impl Gate {
    /// May `f` be inherited from the nearest base: `Ok(why)` — a failed test
    /// of a crate whose blast radius did not change — or `Err(why not)`.
    ///
    /// # Errors
    /// Why `f` is never inherited through this base.
    pub fn qualifies(&self, f: &Finding) -> Result<&str, &'static str> {
        let Some(package) = &f.package else {
            return Err(WHOLE_ROW);
        };
        match self.crates.get(package) {
            None => Err(NO_CRATE),
            Some(Ok(why)) => Ok(why),
            Some(Err(why)) => Err(why),
        }
    }

    /// How many crates' failed tests may be inherited.
    #[must_use]
    pub fn qualifying(&self) -> usize {
        self.crates.values().filter(|q| q.is_ok()).count()
    }
}

// ---------------------------------------------------------------------------
// Main's tree at the merge-base, as values
// ---------------------------------------------------------------------------

/// What [`qualify`] reads of main's tree at the merge-base.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// Every `Cargo.toml`, by its directory (`""` for the root's): its text.
    pub manifests: BTreeMap<String, String>,
    /// Every git submodule's path.
    pub gitlinks: BTreeSet<String>,
    /// Every manifest directory holding a file (other than a `Cargo.toml`)
    /// that holds one of [`READ_OUTSIDE_MARKERS`], or a symlink that points
    /// outside it ([`Tree::mark_symlink`]) — EVERY one above the file, not
    /// only the nearest: a crate's tests can read a nested fixture crate's
    /// files as easily as their own.
    pub escaping: BTreeSet<String>,
}

impl Tree {
    /// The submodule `dir` is, or sits inside, if any.
    fn gitlink_holding(&self, dir: &str) -> Option<&String> {
        self.gitlinks.iter().find(|g| under(dir, g))
    }

    /// Mark every manifest directory holding `path` as escaping.
    pub fn mark_escaping(&mut self, path: &str) {
        let holders: Vec<String> = self
            .manifests
            .keys()
            .filter(|d| under(path, d) && path != d.as_str())
            .cloned()
            .collect();
        self.escaping.extend(holders);
    }

    /// The symlink `path` (a mode-120000 entry, invisible to `git grep`)
    /// points at `target`: unless it resolves to a path inside the manifest
    /// directory owning it, every manifest directory holding it is escaping.
    pub fn mark_symlink(&mut self, path: &str, target: &str) {
        let inside = self.owner(path).is_some_and(|owner| {
            resolve_path(parent(path), target).is_some_and(|to| under(&to, &owner))
        });
        if !inside {
            self.mark_escaping(path);
        }
    }

    /// The manifest directory owning the file `path`: the nearest directory
    /// above it with a `Cargo.toml` (`""`, the root, when only it has one).
    #[must_use]
    pub fn owner(&self, path: &str) -> Option<String> {
        let mut dir = parent(path);
        loop {
            if self.manifests.contains_key(dir) {
                return Some(dir.to_string());
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent(dir);
        }
    }
}

/// `path`'s parent directory; `""` at the top.
fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// Is `path` the directory `dir`, or under it? Everything is under the root.
fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `rel`, written in the manifest at `dir`, as a repository-relative
/// directory — `None` for an absolute path or one that climbs above the root.
fn resolve_path(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// What one manifest says about the build: its dependencies (with the path
/// each names, when it names one), the root's `[workspace.dependencies]`
/// paths and `[patch]`/`[replace]` paths, and whether any other value climbs
/// out of its directory.
#[derive(Debug, Default, PartialEq, Eq)]
struct Edges {
    deps: Vec<(String, Option<String>)>,
    workspace: Vec<(String, Option<String>)>,
    patches: Vec<(String, Option<String>)>,
    escapes: bool,
}

/// The tables cargo reads dependencies from.
const DEP_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "dev_dependencies",
    "build-dependencies",
    "build_dependencies",
];

/// A manifest's [`Edges`], or `None` when it cannot be read — a document the
/// TOML reader cannot parse, or a dependency whose `path` is not one plain
/// string.
fn edges(text: &str) -> Option<Edges> {
    let mut e = Edges::default();
    for s in build_config::statements(text).ok()? {
        let full: Vec<String> = s.table.iter().chain(&s.key).cloned().collect();
        let first = full.first().map(String::as_str);
        let at = usize::from(first == Some("target") && full.len() >= 3) * 2;
        if full
            .get(at)
            .is_some_and(|t| DEP_TABLES.contains(&t.as_str()))
        {
            dep(&full[at + 1..], &s.value, &mut e.deps)?;
        } else if first == Some("workspace")
            && full.get(1).map(String::as_str) == Some("dependencies")
        {
            dep(&full[2..], &s.value, &mut e.workspace)?;
        } else if first == Some("patch") && full.len() >= 2 {
            dep(&full[2..], &s.value, &mut e.patches)?;
        } else if first == Some("replace") {
            dep(&full[1..], &s.value, &mut e.patches)?;
        } else if climbs(&s.value) {
            e.escapes = true;
        }
    }
    Some(e)
}

/// One dependency entry — `rest` the key after its table (`[name]`, `[name,
/// "path"]`, or empty for a whole table given inline) — into `out`.
fn dep(rest: &[String], value: &Value, out: &mut Vec<(String, Option<String>)>) -> Option<()> {
    match rest {
        [] => {
            let Value::Table(entries) = value else {
                return None;
            };
            for (k, v) in entries {
                dep(k, v, out)?;
            }
        }
        [name] => {
            let path = match value {
                Value::Table(entries) => {
                    let mut path = None;
                    for (k, v) in entries {
                        if k.first().map(String::as_str) == Some("path") {
                            match (k.len(), v, path.is_none()) {
                                (1, Value::Str(p), true) => path = Some(p.clone()),
                                _ => return None,
                            }
                        }
                    }
                    path
                }
                Value::Array(_) => return None,
                Value::Str(_) | Value::Escaped(_) | Value::Other => None,
            };
            out.push((name.clone(), path));
        }
        [name, key, more @ ..] => {
            let path = if key == "path" {
                match (more, value) {
                    ([], Value::Str(p)) => Some(p.clone()),
                    _ => return None,
                }
            } else {
                None
            };
            out.push((name.clone(), path));
        }
    }
    Some(())
}

/// Does `v` hold a string that climbs out of its directory?
fn climbs(v: &Value) -> bool {
    match v {
        Value::Str(s) | Value::Escaped(s) => {
            s == ".." || s.starts_with("../") || s.contains("/../") || s.ends_with("/..")
        }
        Value::Array(items) => items.iter().any(climbs),
        Value::Table(entries) => entries.iter().any(|(_, v)| climbs(v)),
        Value::Other => false,
    }
}

/// THE BLAST RADIUS OF EVERY CRATE (the pure half of the gate): for each
/// package `tree`'s manifests name, `Ok(why)` when its failed tests may be
/// inherited from a nearest base whose diff to the merge-base is `changed`,
/// or `Err(why not)` — see the module's doc for each rule.
#[must_use]
pub fn qualify(tree: &Tree, changed: &[String]) -> BTreeMap<String, Result<String, &'static str>> {
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (dir, text) in &tree.manifests {
        if let Some(name) = crate::changed::manifest_package_name(text) {
            names.entry(name).or_default().push(dir.clone());
        }
    }
    let root = tree.manifests.get("").and_then(|t| edges(t));
    let everything = |why: &'static str| names.keys().map(|n| (n.clone(), Err(why))).collect();
    let Some(root) = root else {
        return everything(UNREADABLE);
    };
    let mut patches = Vec::new();
    let mut workspace: BTreeMap<String, String> = BTreeMap::new();
    for (name, path) in &root.workspace {
        if let Some(path) = path {
            let Some(dir) = resolve_path("", path) else {
                return everything(UNREADABLE);
            };
            workspace.insert(name.clone(), dir);
        }
    }
    for (_, path) in &root.patches {
        let Some(dir) = path.as_deref().map(|p| resolve_path("", p)) else {
            continue;
        };
        let Some(dir) = dir else {
            return everything(UNREADABLE);
        };
        patches.push(dir);
    }
    if changed
        .iter()
        .any(|p| GATE_PATHS.iter().any(|g| under(p, g)))
    {
        return everything(GATE_CHANGED);
    }
    let global = changed.iter().any(|p| {
        WORKSPACE_FILES.contains(&p.as_str())
            || is_manifest(p)
            || under(p, WORKSPACE_DIR)
            || patches.iter().any(|d| under(p, d))
    });
    if global {
        return everything(WORKSPACE_CHANGED);
    }
    names
        .into_iter()
        .map(|(name, dirs)| {
            let verdict = match dirs.as_slice() {
                [dir] => radius(tree, &workspace, dir).and_then(|fence| {
                    if changed.iter().any(|p| fence.iter().any(|d| under(p, d))) {
                        Err(RADIUS_CHANGED)
                    } else {
                        Ok(format!(
                            "no file of `{name}` or of the {} crate(s) it builds on changed \
                             between the two",
                            fence.len() - 1
                        ))
                    }
                }),
                _ => Err(NO_CRATE),
            };
            (name, verdict)
        })
        .collect()
}

/// The directories whose files decide a test of the crate at `start`: it and
/// every crate it builds on, transitively — or why that cannot be known. A
/// crate that is, or builds on, one inside a git submodule is [`SUBMODULE`]:
/// the submodule's manifests (its own path dependencies, perhaps back into
/// this tree) and its files (the lexical scan's) are not in main's tree.
fn radius(
    tree: &Tree,
    workspace: &BTreeMap<String, String>,
    start: &str,
) -> Result<BTreeSet<String>, &'static str> {
    let mut fence: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(dir) = stack.pop() {
        if !fence.insert(dir.clone()) {
            continue;
        }
        if tree.gitlink_holding(&dir).is_some() {
            return Err(SUBMODULE);
        }
        let text = tree.manifests.get(&dir).ok_or(UNREADABLE)?;
        let e = edges(text).ok_or(UNREADABLE)?;
        if e.escapes || tree.escaping.contains(&dir) {
            return Err(READS_OUTSIDE);
        }
        for (name, path) in e.deps {
            let target = match path {
                Some(p) => resolve_path(&dir, &p).ok_or(UNREADABLE)?,
                None => match workspace.get(&name) {
                    Some(d) => d.clone(),
                    None => continue,
                },
            };
            stack.push(target);
        }
    }
    Ok(fence)
}

// ---------------------------------------------------------------------------
// Reading it from git
// ---------------------------------------------------------------------------

/// Main's tree at `commit`, as [`qualify`] reads it: every manifest (one
/// `git ls-tree`, one `git cat-file --batch`), every submodule, and the
/// manifest directories the lexical scan marks (one `git grep`).
///
/// # Errors
/// The git call that failed.
pub fn read_tree(root: &Path, commit: &str) -> Result<Tree, String> {
    let listed = differential::git_stdout(root, &["ls-tree", "-r", "-z", "--full-tree", commit])
        .ok_or_else(|| format!("`git ls-tree {}` failed", short(commit)))?;
    let mut tree = Tree::default();
    let mut blobs: Vec<(String, String)> = Vec::new();
    let mut links: Vec<(String, String)> = Vec::new();
    for entry in listed.split('\0').filter(|e| !e.is_empty()) {
        let Some((meta, path)) = entry.split_once('\t') else {
            return Err(format!("`git ls-tree` printed `{entry}`"));
        };
        let mut f = meta.split_whitespace();
        let (mode, kind, id) = (f.next(), f.next(), f.next());
        match (mode, kind, id) {
            (_, Some("commit"), _) => {
                tree.gitlinks.insert(path.to_string());
            }
            (Some("120000"), Some("blob"), Some(id)) => {
                links.push((path.to_string(), id.to_string()));
            }
            (_, Some("blob"), Some(id)) if is_manifest(path) => {
                blobs.push((parent(path).to_string(), id.to_string()));
            }
            _ => {}
        }
    }
    let texts = cat_blobs(root, blobs.iter().map(|(_, id)| id.as_str()))?;
    tree.manifests = blobs.into_iter().map(|(dir, _)| dir).zip(texts).collect();
    // A symlink is a blob holding its target, which `git grep` on a commit
    // never searches: read each, and mark it unless it stays inside its crate.
    let targets = cat_blobs(root, links.iter().map(|(_, id)| id.as_str()))?;
    for ((path, _), target) in links.iter().zip(&targets) {
        tree.mark_symlink(path, target);
    }
    let mut args = vec!["grep", "-I", "-l", "-z", "-F"];
    for m in READ_OUTSIDE_MARKERS {
        args.extend(["-e", m]);
    }
    args.extend([commit, "--", ".", ":(exclude,glob)**/Cargo.toml"]);
    let out = differential::git(root, &args)
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("`git grep`: {e}"))?;
    match out.status.code() {
        Some(0) => {}
        Some(1) => return Ok(tree),
        _ => return Err(format!("`git grep` at {} failed", short(commit))),
    }
    let prefix = format!("{commit}:");
    let text = String::from_utf8_lossy(&out.stdout);
    for hit in text.split('\0').filter(|h| !h.is_empty()) {
        let path = hit.strip_prefix(&prefix).unwrap_or(hit);
        tree.mark_escaping(path);
    }
    Ok(tree)
}

/// The contents of `ids`, in order, through one `git cat-file --batch`.
fn cat_blobs<'a>(root: &Path, ids: impl Iterator<Item = &'a str>) -> Result<Vec<String>, String> {
    let ids: Vec<&str> = ids.collect();
    let mut child = differential::git(root, &["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("`git cat-file`: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("`git cat-file` has no stdin")?;
    let request: String = ids.iter().map(|id| format!("{id}\n")).collect();
    // Written from a thread: git answers as it reads, and a pipe full of
    // answers nobody reads would stop it reading.
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
    let mut raw = Vec::new();
    child
        .stdout
        .take()
        .ok_or("`git cat-file` has no stdout")?
        .read_to_end(&mut raw)
        .map_err(|e| format!("`git cat-file`: {e}"))?;
    let wrote = writer
        .join()
        .map_err(|_| "`git cat-file`: its writer panicked")?;
    let status = child.wait().map_err(|e| format!("`git cat-file`: {e}"))?;
    if wrote.is_err() || !status.success() {
        return Err("`git cat-file --batch` failed".to_string());
    }
    let mut out = Vec::with_capacity(ids.len());
    let mut at = 0;
    for id in ids {
        let nl = raw[at..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("`git cat-file` answered short")?;
        let header = String::from_utf8_lossy(&raw[at..at + nl]).into_owned();
        at += nl + 1;
        let mut f = header.split(' ');
        let (Some(got), Some("blob"), Some(size)) = (f.next(), f.next(), f.next()) else {
            return Err(format!("`git cat-file` answered `{header}` for {id}"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("`git cat-file` answered `{header}`"))?;
        if got != id || at + size > raw.len() {
            return Err(format!("`git cat-file` answered `{header}` for {id}"));
        }
        out.push(String::from_utf8_lossy(&raw[at..at + size]).into_owned());
        at += size + 1;
    }
    Ok(out)
}

/// THE NEAREST BASE of a run whose merge-base `merge_base` has no receipt
/// that serves `tools`: the newest first-parent ancestor inside `bound` whose
/// receipt does (store by commit, then tree, then the local note —
/// [`differential::lookup`]), with its [`Gate`].
///
/// # Errors
/// Why there is none — nothing inside the bound, or main's tree or the diff
/// between the two could not be read — for the `verify: base …` line.
pub fn find(root: &Path, merge_base: &str, tools: &Tools, bound: Bound) -> Result<Found, String> {
    let n = bound.commits.saturating_add(1).to_string();
    let walk = differential::git_stdout(
        root,
        &[
            "log",
            "--first-parent",
            "-n",
            &n,
            "--format=%H %T %ct",
            merge_base,
        ],
    )
    .ok_or_else(|| {
        format!(
            "main's history before {} could not be read",
            short(merge_base)
        )
    })?;
    let mut lines = walk.lines().map(|l| {
        let mut f = l.split(' ');
        (
            f.next().unwrap_or_default().to_string(),
            f.next().unwrap_or_default().to_string(),
            f.next().and_then(|t| t.parse::<u64>().ok()),
        )
    });
    let Some((_, _, Some(m_time))) = lines.next() else {
        return Err(format!(
            "the merge-base {}'s commit time could not be read",
            short(merge_base)
        ));
    };
    let noted: BTreeSet<String> = differential::git_stdout(
        root,
        &[
            "notes",
            &format!("--ref={}", differential::NOTES_REF),
            "list",
        ],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
    .collect();
    let store = crate::receipt::dir(root).ok();
    let (mut scanned, mut passed_over, mut past_span) = (0usize, 0usize, false);
    let mut chosen = None;
    for (steps, (commit, tree, time)) in lines.enumerate().map(|(i, l)| (i + 1, l)) {
        let Some(time) = time else {
            return Err(format!(
                "{}'s commit time could not be read",
                short(&commit)
            ));
        };
        if m_time.saturating_sub(time) > bound.span_secs {
            past_span = true;
            break;
        }
        scanned = steps;
        let held = store.as_ref().is_some_and(|s| {
            s.join(&commit).is_file() || s.join(crate::receipt::tree_key(&tree)).is_file()
        });
        if !(held || noted.contains(&commit)) {
            continue;
        }
        match differential::lookup(root, &commit, true, Some(tools)) {
            Ok(found) => {
                chosen = Some((found, steps, m_time.saturating_sub(time)));
                break;
            }
            Err(_) => passed_over += 1,
        }
    }
    let Some((mut found, steps, span_secs)) = chosen else {
        return Err(format!(
            "no receipt that serves this run on the {scanned} main commit(s) before it{} \
             (the bound: {} commits, {} h){}",
            if past_span {
                ", the next one older than the bound"
            } else {
                ""
            },
            bound.commits,
            bound.span_secs / 3600,
            if passed_over > 0 {
                format!("; {passed_over} receipt(s) there could not serve")
            } else {
                String::new()
            }
        ));
    };
    let tree = read_tree(root, merge_base)?;
    let diff = differential::git_stdout(
        root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &found.commit,
            merge_base,
        ],
    )
    .ok_or_else(|| {
        format!(
            "`git diff {} {}` failed",
            short(&found.commit),
            short(merge_base)
        )
    })?;
    let changed: Vec<String> = diff
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    found.nearest = Some(Gate {
        merge_base: merge_base.to_string(),
        steps,
        span_secs,
        crates: qualify(&tree, &changed),
    });
    Ok(found)
}

/// The owning crate a failed test's re-run spec names: `-p <crate>`,
/// `-p=<crate>`, `--package <crate>` or `--package=<crate>` — `None` when it
/// names none (then the red is never inherited through a nearest base).
#[must_use]
pub fn package_of(spec: &str) -> Option<String> {
    let mut words = spec.split_whitespace();
    while let Some(w) = words.next() {
        if w == "-p" || w == "--package" {
            return words.next().map(str::to_string);
        }
        if let Some(p) = w
            .strip_prefix("-p=")
            .or_else(|| w.strip_prefix("--package="))
        {
            return (!p.is_empty()).then(|| p.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of manifests: `(dir, manifest)`, the root's first.
    fn tree(manifests: &[(&str, &str)]) -> Tree {
        Tree {
            manifests: manifests
                .iter()
                .map(|(d, t)| ((*d).to_string(), (*t).to_string()))
                .collect(),
            ..Tree::default()
        }
    }

    const ROOT: &str = "[workspace]\nmembers = [\"crates/*\"]\n\n\
        [workspace.dependencies]\nb = { path = \"crates/b\" }\nserde = \"1\"\n\n\
        [patch.crates-io]\nwinit = { path = \"vendor/winit\" }\n";

    /// `a` builds on `b` (through `workspace = true`) and dev-depends on `d`
    /// (by path); `c` stands alone; `b` builds on nothing of ours.
    fn workspace() -> Tree {
        tree(&[
            ("", ROOT),
            (
                "crates/a",
                "[package]\nname = \"a\"\n\n[dependencies]\nb.workspace = true\nserde = \
                 \"1\"\n\n[target.'cfg(unix)'.dev-dependencies.d]\npath = \"../d\"\n",
            ),
            ("crates/b", "[package]\nname = \"b\"\n\n[dependencies]\n"),
            ("crates/c", "[package]\nname = \"c\"\n"),
            ("crates/d", "[package]\nname = \"d\"\n"),
        ])
    }

    fn changed(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn a_manifest_names_its_path_dependencies_in_every_shape_cargo_takes() {
        let e = edges(
            "[package]\nname = \"x\"\n\n[dependencies]\ninline = { path = \"../i\", version = \
             \"1\" }\ndotted.path = \"../dot\"\nplain = \"1\"\nws = { workspace = true }\n\n\
             [dependencies.table]\npath = \"../t\"\n\n[build-dependencies]\nbuilt = { path = \
             \"../b\" }\n\n[target.'cfg(unix)'.dev-dependencies]\nunixy = { path = \"../u\" }\n",
        )
        .expect("a manifest");
        let with_path: Vec<(&str, &str)> = e
            .deps
            .iter()
            .filter_map(|(n, p)| p.as_deref().map(|p| (n.as_str(), p)))
            .collect();
        assert_eq!(
            with_path,
            [
                ("inline", "../i"),
                ("dotted", "../dot"),
                ("table", "../t"),
                ("built", "../b"),
                ("unixy", "../u"),
            ]
        );
        assert!(e.deps.iter().any(|(n, p)| n == "ws" && p.is_none()));
        assert!(!e.escapes, "a path dependency is an edge, not a climb");

        let root = edges(ROOT).expect("the root");
        assert_eq!(root.workspace[0], ("b".into(), Some("crates/b".into())));
        assert_eq!(
            root.patches,
            [("winit".into(), Some("vendor/winit".into()))]
        );

        // A target that climbs out of the crate is a climb; a description
        // with dots in it is not.
        let lib = edges("[package]\nname = \"x\"\ndescription = \"a... b\"\n").expect("x");
        assert!(!lib.escapes);
        let out =
            edges("[package]\nname = \"x\"\n\n[lib]\npath = \"../shared/lib.rs\"\n").expect("x");
        assert!(out.escapes);
        // A path spelled with escapes cannot be read: the manifest is unreadable.
        assert!(edges("[dependencies]\nq = { path = \"..\\u002fq\" }\n").is_none());
        assert!(edges("[dependencies\n").is_none(), "unparseable");
    }

    #[test]
    fn a_crate_qualifies_only_while_nothing_it_builds_on_changed() {
        let t = workspace();
        // The control: a change to a crate `a` does not build on.
        let q = qualify(&t, &changed(&["crates/c/src/lib.rs", "docs/PROCESS.md"]));
        let why = q["a"].as_ref().expect("a qualifies");
        assert!(why.contains("the 2 crate(s) it builds on"), "{why}");
        assert!(q["b"].is_ok() && q["d"].is_ok());
        assert_eq!(q["c"], Err(RADIUS_CHANGED));

        // A change inside the crate itself.
        let q = qualify(&t, &changed(&["crates/a/tests/it.rs"]));
        assert_eq!(q["a"], Err(RADIUS_CHANGED));
        assert!(q["b"].is_ok());
        // A change in a crate it builds on — through `workspace = true`, and
        // through a target-specific dev-dependency.
        assert_eq!(
            qualify(&t, &changed(&["crates/b/src/lib.rs"]))["a"],
            Err(RADIUS_CHANGED)
        );
        assert_eq!(
            qualify(&t, &changed(&["crates/d/src/lib.rs"]))["a"],
            Err(RADIUS_CHANGED)
        );
        // Every build-wide input disqualifies every crate.
        for p in [
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "vendor/winit/src/lib.rs",
        ] {
            let q = qualify(&t, &changed(&[p]));
            assert!(
                q.values().all(|v| *v == Err(WORKSPACE_CHANGED)),
                "{p}: {q:?}"
            );
        }
    }

    /// FEATURE UNIFICATION. `a` builds on `b`; `c` does not build on `a` and
    /// `a` does not build on `c`, but `c` dev-depends on `b` and its manifest
    /// alone turns on a feature of `b` — which, unified across the workspace,
    /// changes how `a`'s tests compile, with `Cargo.lock` unmoved. So a change
    /// to ANY manifest makes `a`'s red NEW, not only one in its radius.
    #[test]
    fn a_manifest_outside_the_radius_still_disqualifies_every_crate() {
        let t = tree(&[
            ("", "[workspace]\nmembers = [\"crates/*\"]\n"),
            (
                "crates/a",
                "[package]\nname = \"a\"\n[dependencies]\nb = { path = \"../b\" }\n",
            ),
            ("crates/b", "[package]\nname = \"b\"\n[features]\nx = []\n"),
            (
                "crates/c",
                "[package]\nname = \"c\"\n[dev-dependencies]\nb = { path = \"../b\", \
                 features = [\"x\"] }\n",
            ),
        ]);
        // The control: `c`'s sources alone leave `a` qualifying.
        assert!(qualify(&t, &changed(&["crates/c/src/lib.rs"]))["a"].is_ok());
        // `c`'s manifest — the feature switched on there — does not.
        for p in ["crates/c/Cargo.toml", "tools/nested/Cargo.toml"] {
            let q = qualify(&t, &changed(&[p]));
            assert!(
                q.values().all(|v| *v == Err(WORKSPACE_CHANGED)),
                "{p}: {q:?}"
            );
        }
    }

    /// THE GATE'S OWN VERSION. A nearest base's receipt was fingerprinted by
    /// the gate as it was at that base: a change to the gate between the two
    /// disqualifies every red; a file merely named like it does not.
    #[test]
    fn a_change_to_the_gate_itself_disqualifies_every_crate() {
        let t = workspace();
        for p in ["crates/aterm-verify/src/differential.rs", "tools/verify.sh"] {
            let q = qualify(&t, &changed(&[p]));
            assert!(q.values().all(|v| *v == Err(GATE_CHANGED)), "{p}: {q:?}");
        }
        assert!(qualify(&t, &changed(&["crates/aterm-verify-x/lib.rs"]))["a"].is_ok());
        assert!(qualify(&t, &changed(&["tools/verify.sh.md"]))["a"].is_ok());
    }

    #[test]
    fn what_cannot_be_known_never_qualifies() {
        // A crate (or one it builds on) that names a path outside itself.
        let mut t = workspace();
        t.escaping.insert("crates/b".into());
        let q = qualify(&t, &[]);
        assert_eq!(q["a"], Err(READS_OUTSIDE));
        assert_eq!(q["b"], Err(READS_OUTSIDE));
        assert!(q["c"].is_ok(), "the control: an unrelated crate");

        // A path dependency whose directory holds no manifest.
        let mut t = workspace();
        t.manifests.remove("crates/d");
        assert_eq!(qualify(&t, &[])["a"], Err(UNREADABLE));
        // One that climbs above the repository.
        let t = tree(&[
            ("", ROOT),
            (
                "crates/e",
                "[package]\nname = \"e\"\n[dependencies]\nf = { path = \"../../../f\" }\n",
            ),
        ]);
        assert_eq!(qualify(&t, &[])["e"], Err(UNREADABLE));
        // Two manifests naming one package.
        let mut t = workspace();
        t.manifests
            .insert("tools/c".into(), "[package]\nname = \"c\"\n".into());
        assert_eq!(qualify(&t, &[])["c"], Err(NO_CRATE));
        // No readable root: nothing qualifies.
        let mut t = workspace();
        t.manifests.insert(String::new(), "[workspace\n".into());
        assert!(qualify(&t, &[]).values().all(|v| *v == Err(UNREADABLE)));
    }

    /// A path dependency landing in a submodule is unknowable: its manifests
    /// (perhaps depending back into this tree) and its files are not in
    /// main's tree. The control: a crate that does not build on it.
    #[test]
    fn a_path_dependency_into_a_submodule_never_qualifies() {
        let mut t = tree(&[
            ("", "[workspace]\n"),
            (
                "crates/link",
                "[package]\nname = \"link\"\n[dependencies]\nbroker = { path = \
                 \"../../vendor/astream/crates/broker\" }\n",
            ),
            (
                "crates/user",
                "[package]\nname = \"user\"\n[dependencies]\nlink = { path = \"../link\" }\n",
            ),
            ("crates/solo", "[package]\nname = \"solo\"\n"),
        ]);
        t.gitlinks.insert("vendor/astream".into());
        let q = qualify(&t, &[]);
        assert_eq!(q["link"], Err(SUBMODULE));
        assert_eq!(q["user"], Err(SUBMODULE), "transitively");
        assert!(q["solo"].is_ok());
    }

    /// The lexical scan marks EVERY crate holding a file, not only the nearest
    /// manifest above it: a crate's tests read a nested fixture crate's files
    /// as easily as their own.
    #[test]
    fn a_marked_file_marks_every_crate_holding_it() {
        let mut t = tree(&[
            ("", "[workspace]\n"),
            ("crates/a", "[package]\nname = \"a\"\n"),
            ("crates/a/fixtures/x", "[package]\nname = \"x\"\n"),
            ("crates/ab", "[package]\nname = \"ab\"\n"),
        ]);
        t.mark_escaping("crates/a/fixtures/x/src/lib.rs");
        assert!(t.escaping.contains("crates/a/fixtures/x") && t.escaping.contains("crates/a"));
        assert!(
            !t.escaping.contains("crates/ab"),
            "a sibling with a shared prefix"
        );
        let q = qualify(&t, &[]);
        assert_eq!(q["a"], Err(READS_OUTSIDE));
        assert!(q["ab"].is_ok());
    }

    /// A symlink is invisible to `git grep`: one whose target leaves its crate
    /// marks it; one that stays inside does not.
    #[test]
    fn a_symlink_out_of_its_crate_marks_it() {
        let base = || {
            tree(&[
                ("", "[workspace]\n"),
                ("crates/a", "[package]\nname = \"a\"\n"),
            ])
        };
        for target in ["../../docs/x", "/etc/passwd", "../b/src", "../../../../up"] {
            let mut t = base();
            t.mark_symlink("crates/a/fix", target);
            assert!(t.escaping.contains("crates/a"), "{target}");
            assert_eq!(qualify(&t, &[])["a"], Err(READS_OUTSIDE), "{target}");
        }
        for target in ["src/lib.rs", "./tests/../data", "."] {
            let mut t = base();
            t.mark_symlink("crates/a/tests/fix", target);
            assert!(t.escaping.is_empty(), "{target}: {:?}", t.escaping);
        }
    }

    /// The markers the scan reads name every known way out, including the
    /// ones no `../` shows: the manifest dir and the process's own.
    #[test]
    fn the_markers_name_the_ways_out_no_climb_shows() {
        for m in [
            "ParentDir",
            "::parent(",
            "::ancestors(",
            "CARGO_MANIFEST_DIR",
            "current_dir",
            "current_exe",
        ] {
            assert!(READ_OUTSIDE_MARKERS.contains(&m), "{m}");
        }
        assert!(is_manifest("Cargo.toml") && is_manifest("crates/a/Cargo.toml"));
        assert!(!is_manifest("crates/a/NotCargo.toml") && !is_manifest("Cargo.toml.orig"));
    }

    #[test]
    fn a_finding_qualifies_by_its_crate_and_a_row_never() {
        let gate = Gate {
            merge_base: "m".into(),
            steps: 3,
            span_secs: 60,
            crates: qualify(&workspace(), &changed(&["crates/c/src/lib.rs"])),
        };
        let f = |package: Option<&str>| Finding {
            id: "x".into(),
            hash: "0123456789abcdef".into(),
            opaque: None,
            timing: None,
            package: package.map(str::to_string),
        };
        assert!(gate.qualifies(&f(Some("a"))).is_ok());
        assert_eq!(gate.qualifies(&f(Some("c"))), Err(RADIUS_CHANGED));
        assert_eq!(gate.qualifies(&f(Some("zz"))), Err(NO_CRATE));
        assert_eq!(gate.qualifies(&f(None)), Err(WHOLE_ROW));
        assert_eq!(gate.qualifying(), 3);
    }

    #[test]
    fn a_rerun_spec_names_its_package() {
        assert_eq!(
            package_of("-p atpkg --test index_probe").as_deref(),
            Some("atpkg")
        );
        assert_eq!(
            package_of("--lib -p=aterm-gui").as_deref(),
            Some("aterm-gui")
        );
        assert_eq!(package_of("--package x --doc").as_deref(), Some("x"));
        assert_eq!(package_of("--workspace --lib"), None);
        assert_eq!(package_of("-p"), None);
    }

    #[test]
    fn the_bound_is_two_hundred_commits_and_a_day() {
        assert_eq!(
            Bound::DEFAULT,
            Bound {
                commits: 200,
                span_secs: 24 * 3600
            }
        );
    }

    #[test]
    fn a_path_resolves_inside_the_repository_or_not_at_all() {
        assert_eq!(
            resolve_path("crates/a", "../b").as_deref(),
            Some("crates/b")
        );
        assert_eq!(resolve_path("", "./vendor/x/").as_deref(), Some("vendor/x"));
        assert_eq!(resolve_path("crates/a", "../../.."), None);
        assert_eq!(resolve_path("crates/a", "/abs"), None);
        assert!(!under("crates/ab/x", "crates/a"));
        assert!(under("crates/a/x", "crates/a") && under("anything", ""));
    }
}
