// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Change-cone selection for `targo --unverified verify`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::scope::Scope;
use crate::toolchain::Toolchain;

/// One workspace member reported by the resolved dependency graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// Repo-relative directory of its manifest.
    pub dir: String,
    /// Whether the member declares a library target.
    pub has_lib: bool,
}

/// Workspace members and their resolved `name@version` package specs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Members(Vec<Member>, BTreeMap<String, String>);

impl Members {
    #[must_use]
    pub fn new(mut entries: Vec<Member>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries.dedup_by(|a, b| a.name == b.name);
        Self(entries, BTreeMap::new())
    }

    /// Attach `name -> name@version` specs for shared-name workspace packages.
    #[must_use]
    pub fn with_specs(mut self, specs: BTreeMap<String, String>) -> Self {
        self.1 = specs;
        self
    }

    /// Package spec passed to `cargo -p` / `cargo tree --invert` for `name`.
    #[must_use]
    pub fn spec<'a>(&'a self, name: &'a str) -> &'a str {
        self.1.get(name).map_or(name, String::as_str)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.0.iter().map(|m| m.name.clone()).collect()
    }

    /// Whether any member in `crates` declares a library target.
    #[must_use]
    pub fn any_has_lib(&self, crates: &[String]) -> bool {
        if !self.0.iter().any(|m| m.has_lib) {
            return true;
        }
        crates
            .iter()
            .any(|c| self.0.iter().any(|m| m.has_lib && &m.name == c))
    }
}

/// Map from repo-relative directory to its `Cargo.toml` `[package].name`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifests(BTreeMap<String, Option<String>>);

impl Manifests {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, dir: impl Into<String>, package: Option<String>) {
        self.0.insert(dir.into(), package);
    }

    /// Look up the package name declared by the manifest at `dir`.
    #[must_use]
    pub fn package_at(&self, dir: &str) -> Option<Option<&str>> {
        self.0.get(dir).map(|p| p.as_deref())
    }
}

impl<S: Into<String>> FromIterator<(S, Option<S>)> for Manifests {
    fn from_iter<I: IntoIterator<Item = (S, Option<S>)>>(it: I) -> Self {
        let mut m = Self::new();
        for (dir, pkg) in it {
            m.insert(dir, pkg.map(Into::into));
        }
        m
    }
}

/// Outcome of change-cone selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The cone widened to the entire workspace for the given reason.
    Widened(String),
    /// The cone narrowed to changed seeds and their reverse dependencies.
    Narrowed {
        /// Packages whose own files changed.
        seeds: Vec<String>,
        /// `seeds` plus every workspace crate that depends on them.
        crates: Vec<String>,
        /// Whether any selected crate declares a library target ([`Members::any_has_lib`]).
        any_lib: bool,
    },
}

/// Repo-relative paths whose changes can replan or re-flag the workspace.
pub const REPLANNING_PATHS: [&str; 5] = [
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    ".cargo/config",
    ".cargo/config.toml",
];

#[must_use]
pub fn replans_everything(path: &str) -> bool {
    REPLANNING_PATHS.contains(&path)
}

/// Repo-relative files or directories outside crates that specific members test.
pub const TESTED_BY: [(&str, &str); 2] = [
    ("tools/install.sh", "aterm-update"),
    ("publish/", "aterm-release"),
];

/// Workspace member whose tests exercise `path` ([`TESTED_BY`]), if any.
#[must_use]
pub fn tested_by(path: &str) -> Option<&'static str> {
    TESTED_BY
        .iter()
        .find(|(file, _)| {
            file.strip_suffix('/')
                .map_or(*file == path, |dir| path.starts_with(&format!("{dir}/")))
        })
        .map(|(_, member)| *member)
}

/// Impact of a change to one of [`REPLANNING_PATHS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replan {
    /// No crate builds differently.
    Nothing,
    /// Specific packages whose build inputs changed.
    Packages(BTreeSet<String>),
    /// Every crate may build differently for the given reason.
    Everything(String),
}

/// Replan that widens to the entire workspace.
#[must_use]
pub fn replan_everything() -> Replan {
    Replan::Everything("it can change how every crate is built".to_string())
}

/// Compute the [`Replan`] for a root `Cargo.toml` diff from `base` to `work`.
#[must_use]
pub fn manifest_replan(base: Option<&str>, work: Option<&str>) -> Replan {
    use aterm_toml::Value;
    let (Some(base), Some(work)) = (parse_toml(base), parse_toml(work)) else {
        return Replan::Everything("it does not parse here, or it was added or removed".into());
    };
    let mut seeds = BTreeSet::new();
    for key in changed_keys(&base, &work) {
        let (b, w) = (base.get(key), work.get(key));
        match key {
            "workspace" => {
                for sub in changed_keys_of(b, w) {
                    let (sb, sw) = (b.and_then(|t| t.get(sub)), w.and_then(|t| t.get(sub)));
                    match sub {
                        "default-members" => {}
                        "dependencies" => {
                            for dep in changed_keys_of(sb, sw) {
                                for row in [sb, sw].into_iter().flatten().filter_map(|t| t.get(dep))
                                {
                                    let package =
                                        row.get("package").and_then(Value::as_str).unwrap_or(dep);
                                    seeds.insert(package.to_string());
                                }
                            }
                        }
                        other => {
                            return Replan::Everything(format!("[workspace.{other}] changed"));
                        }
                    }
                }
            }
            "profile" => {
                for name in changed_keys_of(b, w) {
                    let (pb, pw) = (b.and_then(|t| t.get(name)), w.and_then(|t| t.get(name)));
                    for row in changed_keys_of(pb, pw) {
                        if row != "package" {
                            return Replan::Everything(format!("[profile.{name}] {row} changed"));
                        }
                        let (kb, kw) = (pb.and_then(|t| t.get(row)), pw.and_then(|t| t.get(row)));
                        for package in changed_keys_of(kb, kw) {
                            if package == "*" {
                                return Replan::Everything(format!(
                                    "[profile.{name}.package.\"*\"] changed"
                                ));
                            }
                            seeds.insert(package.to_string());
                        }
                    }
                }
            }
            other => return Replan::Everything(format!("[{other}] changed")),
        }
    }
    if seeds.is_empty() {
        Replan::Nothing
    } else {
        Replan::Packages(seeds)
    }
}

/// Compute the [`Replan`] for a `Cargo.lock` diff from `base` to `work`.
#[must_use]
pub fn lock_replan(base: Option<&str>, work: Option<&str>) -> Replan {
    use aterm_toml::Value;
    let (Some(base), Some(work)) = (parse_toml(base), parse_toml(work)) else {
        return Replan::Everything("it does not parse here, or it was added or removed".into());
    };
    if let Some(key) = changed_keys(&base, &work).find(|k| *k != "package") {
        return Replan::Everything(format!("its {key} changed"));
    }
    let entries = |doc: &aterm_toml::Table| -> Option<BTreeMap<(String, String), Value>> {
        let Some(list) = doc.get("package") else {
            return Some(BTreeMap::new());
        };
        list.as_array()?
            .iter()
            .map(|p| {
                let name = p.get("name")?.as_str()?.to_string();
                let version = p.get("version")?.as_str()?.to_string();
                Some(((name, version), p.clone()))
            })
            .collect()
    };
    let (Some(b), Some(w)) = (entries(&base), entries(&work)) else {
        return Replan::Everything("a [[package]] entry has no name or version".into());
    };
    let versions = |name: &str| w.keys().filter(|(n, _)| n == name).count();
    let mut seeds = BTreeSet::new();
    for key in b.keys().chain(w.keys()) {
        if b.get(key) == w.get(key) {
            continue;
        }
        let (name, version) = key;
        match versions(name) {
            0 => {}
            1 => {
                seeds.insert(name.clone());
            }
            _ if w.contains_key(key) => {
                seeds.insert(format!("{name}@{version}"));
            }
            _ => {}
        }
    }
    if seeds.is_empty() {
        Replan::Nothing
    } else {
        Replan::Packages(seeds)
    }
}

/// A TOML document's root table, or `None` for no text or text that does not
/// parse.
fn parse_toml(text: Option<&str>) -> Option<aterm_toml::Table> {
    match aterm_toml::from_str::<aterm_toml::Value>(text?).ok()? {
        aterm_toml::Value::Table(t) => Some(t),
        _ => None,
    }
}

/// The keys whose values differ between two tables, either side's.
fn changed_keys<'a>(
    a: &'a aterm_toml::Table,
    b: &'a aterm_toml::Table,
) -> impl Iterator<Item = &'a str> + 'a {
    differing(Some(a), Some(b)).into_iter()
}

/// [`changed_keys`] of two values that should be tables: a side that is absent
/// or no table reads as an empty table, so every key of the other side differs.
fn changed_keys_of<'a>(
    a: Option<&'a aterm_toml::Value>,
    b: Option<&'a aterm_toml::Value>,
) -> Vec<&'a str> {
    differing(
        a.and_then(aterm_toml::Value::as_table),
        b.and_then(aterm_toml::Value::as_table),
    )
}

/// The sorted keys of either table whose values differ (an absent table holds
/// nothing).
fn differing<'a>(
    a: Option<&'a aterm_toml::Table>,
    b: Option<&'a aterm_toml::Table>,
) -> Vec<&'a str> {
    let get = |t: Option<&'a aterm_toml::Table>, k: &str| t.and_then(|t| t.get(k));
    let mut keys: Vec<&str> = a
        .into_iter()
        .flat_map(aterm_toml::Map::keys)
        .chain(b.into_iter().flat_map(aterm_toml::Map::keys))
        .map(String::as_str)
        .filter(|k| get(a, k) != get(b, k))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// The parent directory of a repo-relative path; `""` at the top.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Nearest ancestor workspace member owning repo-relative `path`.
#[must_use]
pub fn owning_member<'a>(
    path: &str,
    manifests: &'a Manifests,
    members: &BTreeSet<String>,
) -> Option<&'a str> {
    let dir = owning_dir(path, manifests)?;
    manifests.package_at(dir)?.filter(|n| members.contains(*n))
}

/// Select the workspace crates that the change cone must build and test.
#[must_use]
pub fn select(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
    path_packages: &dyn Fn() -> Option<Vec<(String, String)>>,
    dependents: &dyn Fn(&str) -> Option<Vec<String>>,
    replan: &dyn Fn(&str) -> Replan,
) -> Selection {
    if members.is_empty() {
        return Selection::Widened("`targo tree` returned no workspace members".to_string());
    }
    let names = members.names();

    let (seeds, _) = seeds(paths, manifests, &names);
    let mut replanned: BTreeSet<String> = BTreeSet::new();
    for p in paths.iter().filter(|p| replans_everything(p)) {
        match replan(p) {
            Replan::Everything(why) => {
                return Selection::Widened(format!("{p} changed: {why}"));
            }
            Replan::Packages(packages) => replanned.extend(packages),
            Replan::Nothing => {}
        }
    }
    let Some(packages) = path_packages() else {
        return Selection::Widened(
            "`targo tree --workspace` failed, so the path packages outside the workspace \
             are unknown"
                .to_string(),
        );
    };
    let mut cone: BTreeSet<String> = seeds.clone();
    cone.extend(replanned.iter().filter(|p| names.contains(*p)).cloned());
    let mut seeds = seeds;
    seeds.extend(replanned);
    seeds.extend(
        forks_touched(paths, &packages)
            .into_iter()
            .filter(|p| !names.contains(p)),
    );
    for seed in &seeds {
        let Some(found) = dependents(seed.as_str()) else {
            return Selection::Widened(format!(
                "`targo tree --invert {seed}` failed, so its dependents are unknown"
            ));
        };
        cone.extend(found.into_iter().filter(|n| names.contains(n)));
    }
    cone.extend(
        paths
            .iter()
            .filter_map(|p| tested_by(p))
            .map(str::to_string)
            .filter(|n| names.contains(n)),
    );

    let named: Vec<String> = cone.into_iter().collect();
    let any_lib = members.any_has_lib(&named);
    Selection::Narrowed {
        seeds: seeds.into_iter().collect(),
        crates: named.iter().map(|n| members.spec(n).to_string()).collect(),
        any_lib,
    }
}

/// Workspace members whose own files changed in `paths`.
#[must_use]
pub fn seeds(
    paths: &[String],
    manifests: &Manifests,
    members: &BTreeSet<String>,
) -> (BTreeSet<String>, Option<String>) {
    let mut replanned: Option<String> = None;
    let mut seeds: BTreeSet<String> = BTreeSet::new();
    for p in paths {
        if replans_everything(p) && replanned.is_none() {
            replanned = Some(p.clone());
        }
        if let Some(name) = owning_member(p, manifests, members) {
            seeds.insert(name.to_string());
        }
    }
    (seeds, replanned)
}

/// The directory of the nearest manifest above a repo-relative path, member or
/// not ([`owning_member`]'s walk).
fn owning_dir<'a>(path: &'a str, manifests: &Manifests) -> Option<&'a str> {
    let mut dir = parent_dir(path);
    while !dir.is_empty() {
        if manifests.package_at(dir).is_some() {
            return Some(dir);
        }
        dir = parent_dir(dir);
    }
    None
}

/// Vendored forks touched by `paths`.
#[must_use]
pub fn forks_touched(paths: &[String], forks: &[(String, String)]) -> BTreeSet<String> {
    forks
        .iter()
        .filter(|(_, dir)| {
            paths.iter().any(|p| {
                let p = p.trim_end_matches('/');
                p == dir
                    || p.strip_prefix(dir.as_str())
                        .is_some_and(|r| r.starts_with('/'))
                    || dir.strip_prefix(p).is_some_and(|r| r.starts_with('/'))
            })
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Convert a [`Selection`] into a check [`Scope`] and summary line.
#[must_use]
pub fn cone(base: &str, sel: &Selection) -> (Scope, String) {
    match sel {
        Selection::Widened(why) => (
            Scope::workspace(),
            format!("the whole workspace (the change cone widened: {why})"),
        ),
        Selection::Narrowed { crates, .. } if crates.is_empty() => (
            Scope::changed(base, Vec::new(), false),
            format!("the change cone since {base} is empty: no workspace crate changed"),
        ),
        Selection::Narrowed {
            seeds,
            crates,
            any_lib,
        } => {
            let dependents = crates.iter().filter(|c| !seeds.contains(c)).count();
            let more = if dependents == 0 {
                String::new()
            } else {
                format!(" + {dependents} dependent(s)")
            };
            (
                Scope::changed(base, crates.clone(), *any_lib),
                format!("the change cone since {base}: {}{more}", seeds.join(" ")),
            )
        }
    }
}

/// Default git base ref for change-cone comparison when `--base` is omitted.
#[must_use]
pub fn default_base(root: &Path, path_env: &OsStr) -> String {
    let resolves = |r: &str| {
        git(root, path_env)
            .args(["rev-parse", "--verify", "--quiet", r])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if resolves("origin/main") {
        "origin/main".to_string()
    } else {
        "main".to_string()
    }
}

/// Resolve the change cone against a repository checkout.
#[must_use]
pub fn resolve(root: &Path, tools: &Toolchain, path_env: &OsStr, base: &str) -> Selection {
    if !tools.have_targo() {
        return Selection::Widened(
            "targo is absent, so the dependency graph cannot be read".to_string(),
        );
    }
    let (merge_base, paths) = match changed_paths(root, path_env, base) {
        Ok(found) => found,
        Err(why) => return Selection::Widened(why),
    };
    let members = read_members(root, tools, path_env);
    let paths = match with_shared_inputs(root, path_env, paths, &members) {
        Ok(paths) => paths,
        Err(why) => return Selection::Widened(why),
    };
    let manifests = manifests_near(root, &paths);
    select(
        &paths,
        &manifests,
        &members,
        &|| read_path_packages(root, tools, path_env, &members),
        &|c| invert(root, tools, path_env, members.spec(c)),
        &|p| replan_of(root, path_env, &merge_base, p),
    )
}

/// What the change to the replanning file `path` since `merge_base` asks of the
/// cone: the root manifest and the lock read table by table, from git's copy at
/// the merge-base and the working tree's ([`manifest_replan`], [`lock_replan`]);
/// any other one widens.
fn replan_of(root: &Path, path_env: &OsStr, merge_base: &str, path: &str) -> Replan {
    let read = || {
        let out = git(root, path_env)
            .args(["show", &format!("{merge_base}:{path}")])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let work = || std::fs::read_to_string(root.join(path)).ok();
    match path {
        "Cargo.toml" => manifest_replan(read().as_deref(), work().as_deref()),
        "Cargo.lock" => lock_replan(read().as_deref(), work().as_deref()),
        _ => replan_everything(),
    }
}

/// Cargo's dependency graph cannot see source and assets included across a
/// package boundary (`#[path]`, `include!`, `include_str!`, `include_bytes!`),
/// nor a folder a build script or a test reads.
fn with_shared_inputs(
    root: &Path,
    path_env: &OsStr,
    mut paths: Vec<String>,
    members: &Members,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(paths);
    }
    let out = git(root, path_env)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|e| format!("cannot list shared source inputs: {e}"))?;
    if !out.status.success() {
        return Err("git could not list shared source inputs".into());
    }
    let mut sources = Vec::new();
    for entry in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let source = String::from_utf8_lossy(entry);
        if !source.ends_with(".rs") {
            continue;
        }
        let Some(member) = members.0.iter().find(|m| {
            source
                .strip_prefix(&m.dir)
                .is_some_and(|p| p.starts_with('/'))
        }) else {
            continue;
        };
        let file = root.join(source.as_ref());
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read shared input {}: {e}", file.display())),
        };
        if text.contains("../") {
            sources.push((source.into_owned(), file, text, root.join(&member.dir)));
        }
    }
    loop {
        let changed: Vec<_> = paths.iter().map(|p| root.join(p)).collect();
        let before = paths.len();
        for (source, file, text, member) in &sources {
            if !paths.contains(source) && shares_changed_input(file, text, member, &changed) {
                paths.push(source.clone());
            }
        }
        if paths.len() == before {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn shares_changed_input(
    source: &Path,
    text: &str,
    member: &Path,
    changed: &[std::path::PathBuf],
) -> bool {
    let Some(parent) = source.parent() else {
        return false;
    };
    text.split('"').any(|literal| {
        if !literal.contains("../") {
            return false;
        }
        let folder = literal.ends_with('/')
            && literal
                .split('/')
                .any(|segment| !segment.is_empty() && segment != "." && segment != "..");
        [parent, member].into_iter().any(|base| {
            let mut target = std::path::PathBuf::new();
            for component in base.join(literal).components() {
                match component {
                    std::path::Component::ParentDir => {
                        target.pop();
                    }
                    std::path::Component::CurDir => {}
                    component => target.push(component.as_os_str()),
                }
            }
            let folder = folder && !member.starts_with(&target);
            !target.starts_with(member)
                && changed
                    .iter()
                    .any(|p| target.starts_with(p) || (folder && p.starts_with(&target)))
        })
    })
}

/// Git's own environment, which would redirect every call at a different
/// repository (a run started from a git hook inherits `GIT_DIR`).
const GIT_REDIRECTS: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Build a `git --no-optional-locks` command in `dir`.
#[must_use]
pub fn git(dir: &Path, path_env: &OsStr) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .env("PATH", path_env)
        .arg("--no-optional-locks")
        .stdin(std::process::Stdio::null());
    for k in GIT_REDIRECTS {
        c.env_remove(k);
    }
    c
}

/// The merge-base of HEAD and `base` (the replan reads the root manifest and
/// the lock as they were there), and every path changed since it: tracked
/// changes in the working tree (staged or not), edits an index flag hides from
/// git ([`flag_hidden_edits`]), and untracked files git does not ignore.
fn changed_paths(
    root: &Path,
    path_env: &OsStr,
    base: &str,
) -> Result<(String, Vec<String>), String> {
    let git_out = |args: &[&str]| -> Option<Vec<u8>> {
        let out = git(root, path_env)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    };
    if git_out(&["rev-parse", "--git-dir"]).is_none() {
        return Err("this is not a git checkout, so there is no diff to scope by".to_string());
    }
    let Some(mb) = git_out(&["merge-base", "HEAD", base])
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return Err(format!("no merge-base between HEAD and '{base}'"));
    };
    let listed = git_out(&[
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--ignore-submodules=none",
        &mb,
        "--",
    ])
    .zip(git_out(&[
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
    ]));
    let Some((tracked, untracked)) = listed else {
        return Err("git could not list the changed files".to_string());
    };
    let Some(hidden) = flag_hidden_edits(root, path_env) else {
        return Err("git could not list the index's flagged files".to_string());
    };
    let mut paths: Vec<String> = tracked
        .split(|b| *b == 0)
        .chain(untracked.split(|b| *b == 0))
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .chain(hidden)
        .collect();
    paths.sort();
    paths.dedup();
    Ok((mb, paths))
}

/// Tracked files an index flag hides from `git diff` — assume-unchanged (a
/// lowercase `ls-files -v` tag) or skip-worktree (`S`) — whose bytes on disk
/// differ from their index entry.
fn flag_hidden_edits(root: &Path, path_env: &OsStr) -> Option<Vec<String>> {
    let out = git(root, path_env)
        .args(["ls-files", "-v", "-s", "-z"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut flagged: Vec<(String, String)> = Vec::new();
    let mut gone: Vec<String> = Vec::new();
    for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = rec.iter().position(|b| *b == b'\t')?;
        let meta = String::from_utf8_lossy(&rec[..tab]);
        let path = String::from_utf8_lossy(&rec[tab + 1..]).into_owned();
        let mut fields = meta.split(' ');
        let (tag, mode, blob) = (fields.next()?, fields.next()?, fields.next()?);
        let assumed = tag.chars().next()?.is_ascii_lowercase();
        if !(assumed || tag == "S") || !mode.starts_with("100") {
            continue;
        }
        if root.join(&path).is_file() {
            flagged.push((path, blob.to_string()));
        } else if assumed {
            gone.push(path);
        }
    }
    if flagged.is_empty() {
        return Some(gone);
    }
    let mut remaining = flagged.as_slice();
    while !remaining.is_empty() {
        let mut bytes = 0;
        let mut count = 0;
        for (path, _) in remaining.iter().take(64) {
            if count > 0 && bytes + path.len() > 8 * 1024 {
                break;
            }
            bytes += path.len();
            count += 1;
        }
        let (batch, rest) = remaining.split_at(count);
        let out = git(root, path_env)
            .args(["hash-object", "--"])
            .args(batch.iter().map(|(path, _)| path))
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let now = String::from_utf8_lossy(&out.stdout);
        let now: Vec<&str> = now.lines().collect();
        if now.len() != batch.len() {
            return None;
        }
        gone.extend(
            batch
                .iter()
                .zip(now)
                .filter(|((_, blob), now)| blob != now)
                .map(|((path, _), _)| path.clone()),
        );
        remaining = rest;
    }
    Some(gone)
}

/// A child's stdout on success, `None` on any failure.
fn stdout_of(cmd: &mut Command, root: &Path, path_env: &OsStr) -> Option<String> {
    let out = cmd.current_dir(root).env("PATH", path_env).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn targo_tree(root: &Path, tools: &Toolchain, path_env: &OsStr, args: &[&str]) -> Option<String> {
    stdout_of(Command::new(&tools.targo).args(args), root, path_env)
}

/// Parse `(name, manifest_dir)` entries from `cargo tree` output.
#[must_use]
pub fn parse_graph_entries(text: &str, root_prefix: &str) -> Vec<(String, String)> {
    let open = format!("({root_prefix}");
    text.lines()
        .filter_map(|line| {
            let line = line.strip_suffix(" (*)").unwrap_or(line);
            let name = line.split_whitespace().next()?;
            let at = line.find(&open)?;
            let rest = line[at + 1..].strip_suffix(')')?;
            let dir = rest.strip_prefix(root_prefix)?.to_string();
            Some((name.to_string(), dir))
        })
        .collect()
}

fn read_members(root: &Path, tools: &Toolchain, path_env: &OsStr) -> Members {
    let Some(out) = targo_tree(
        root,
        tools,
        path_env,
        &[
            "tree",
            "--workspace",
            "--depth",
            "0",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
    ) else {
        return Members::default();
    };
    let prefix = format!("{}/", root.display());
    let shared = std::fs::read_to_string(root.join("Cargo.lock"))
        .map(|lock| lock_shared_names(&lock))
        .unwrap_or_default();
    let specs = parse_graph_versions(&out, &prefix)
        .into_iter()
        .filter(|(name, _)| shared.contains(name))
        .map(|(name, version)| {
            let spec = format!("{name}@{version}");
            (name, spec)
        })
        .collect();
    Members::new(
        parse_graph_entries(&out, &prefix)
            .into_iter()
            .map(|(name, dir)| {
                let at = root.join(&dir);
                let has_lib = at.join("src/lib.rs").is_file()
                    || std::fs::read_to_string(at.join("Cargo.toml"))
                        .is_ok_and(|t| declares_lib_target(&t));
                Member { name, dir, has_lib }
            })
            .collect(),
    )
    .with_specs(specs)
}

/// Parse `name -> version` mappings for in-repo packages from `cargo tree` output.
#[must_use]
pub fn parse_graph_versions(text: &str, root_prefix: &str) -> BTreeMap<String, String> {
    let open = format!("({root_prefix}");
    text.lines()
        .filter(|line| line.contains(&open))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let version = words.next()?.strip_prefix('v')?;
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

/// Package names that appear in more than one `[[package]]` entry in `Cargo.lock`.
#[must_use]
pub fn lock_shared_names(lock: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut shared = BTreeSet::new();
    let mut in_package = false;
    for line in lock.lines() {
        if line.starts_with('[') {
            in_package = line == "[[package]]";
        } else if in_package
            && let Some(name) = line
                .strip_prefix("name = \"")
                .and_then(|rest| rest.strip_suffix('"'))
            && !seen.insert(name.to_string())
        {
            shared.insert(name.to_string());
        }
    }
    shared
}

/// The in-repo path packages the resolved graph carries that are not workspace
/// members (the `[patch]`ed forks under `vendor/`, the astream crates aterm-link
/// path-depends on), each with its repo-relative directory, sorted.
fn read_path_packages(
    root: &Path,
    tools: &Toolchain,
    path_env: &OsStr,
    members: &Members,
) -> Option<Vec<(String, String)>> {
    let out = targo_tree(
        root,
        tools,
        path_env,
        &[
            "tree",
            "--workspace",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--edges",
            "normal,build,dev",
        ],
    )?;
    let names = members.names();
    let prefix = format!("{}/", root.display());
    let mut found: Vec<(String, String)> = parse_graph_entries(&out, &prefix)
        .into_iter()
        .filter(|(name, _)| !names.contains(name))
        .collect();
    found.sort();
    found.dedup();
    Some(found)
}

fn invert(
    root: &Path,
    tools: &Toolchain,
    path_env: &OsStr,
    crate_name: &str,
) -> Option<Vec<String>> {
    let out = targo_tree(
        root,
        tools,
        path_env,
        &[
            "tree",
            "--workspace",
            "--invert",
            crate_name,
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--edges",
            "normal,build,dev",
        ],
    )?;
    let prefix = format!("{}/", root.display());
    Some(
        parse_graph_entries(&out, &prefix)
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
    )
}

/// Every `Cargo.toml` between the changed paths and the repo root.
fn manifests_near(root: &Path, paths: &[String]) -> Manifests {
    let mut m = Manifests::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for p in paths {
        let mut dir = parent_dir(p);
        while !dir.is_empty() {
            if seen.insert(dir.to_string())
                && let Ok(text) = std::fs::read_to_string(root.join(dir).join("Cargo.toml"))
            {
                m.insert(dir, manifest_package_name(&text));
            }
            dir = parent_dir(dir);
        }
    }
    m
}

/// Extract `[package].name` from a `Cargo.toml` text.
#[must_use]
pub fn manifest_package_name(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        if line.starts_with('[') {
            in_package = line.starts_with("[package]");
            continue;
        }
        if !in_package {
            continue;
        }
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("name") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        return rest.find('"').map(|end| rest[..end].to_string());
    }
    None
}

/// Whether a `Cargo.toml` text explicitly declares a `[lib]` target.
#[must_use]
pub fn declares_lib_target(manifest: &str) -> bool {
    manifest.lines().any(|l| l.starts_with("[lib]"))
}

/// Whether workspace member `crates/<name>` has a library target.
#[must_use]
pub fn crate_dir_has_lib(root: &std::path::Path, name: &str) -> bool {
    let dir = root.join("crates").join(name);
    match std::fs::read_to_string(dir.join("Cargo.toml")) {
        Ok(manifest) => declares_lib_target(&manifest) || dir.join("src/lib.rs").is_file(),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`super::select`] with every replanning path read as widening, the rule
    /// for a path with no table-by-table reading ([`replan_everything`]).
    fn select(
        paths: &[String],
        manifests: &Manifests,
        members: &Members,
        path_packages: &dyn Fn() -> Option<Vec<(String, String)>>,
        dependents: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> Selection {
        super::select(
            paths,
            manifests,
            members,
            path_packages,
            dependents,
            &|_| replan_everything(),
        )
    }

    const MANIFEST: &str = r#"# The workspace.
[workspace]
members = ["crates/*"]
default-members = ["crates/a"]

[workspace.package]
version = "0.1.0"

[workspace.dependencies]
# TLS.
ring = { version = "0.17.14", default-features = false }
serde = { package = "aterm-serde", path = "crates/aterm-serde" }

[profile.dev]
debug = "line-tables-only"

[profile.dev.package.aterm-png]
opt-level = 2

[patch.crates-io]
libc = { path = "crates/aterm-libc" }
"#;

    /// THE ROOT MANIFEST, TABLE BY TABLE: a comment or `default-members` selects
    /// nothing; a dependency row selects the package it names; a profile's
    /// package row its package; any other table, and text that does not parse,
    /// widens.
    #[test]
    fn a_root_manifest_change_selects_what_its_tables_reach() {
        let edit = |from: &str, to: &str| {
            assert!(MANIFEST.contains(from), "{from}");
            manifest_replan(Some(MANIFEST), Some(&MANIFEST.replace(from, to)))
        };
        let packages =
            |names: &[&str]| Replan::Packages(names.iter().map(|n| n.to_string()).collect());
        assert_eq!(edit("# TLS.", "# TLS, via ring."), Replan::Nothing);
        assert_eq!(
            edit(
                "

[profile.dev]",
                "


[profile.dev]"
            ),
            Replan::Nothing
        );
        assert_eq!(
            edit(
                r#"default-members = ["crates/a"]"#,
                r#"default-members = ["crates/a", "crates/b"]"#
            ),
            Replan::Nothing
        );
        assert_eq!(edit(r#""0.17.14""#, r#""0.17.15""#), packages(&["ring"]));
        assert_eq!(
            edit(
                r#"path = "crates/aterm-serde" }"#,
                r#"path = "crates/aterm-serde", features = ["derive"] }"#
            ),
            packages(&["aterm-serde"]),
            "a renamed row selects the package it names"
        );
        assert_eq!(
            edit(
                "[workspace.dependencies]
",
                "[workspace.dependencies]
aterm-x = { path = \"crates/x\" }
"
            ),
            packages(&["aterm-x"])
        );
        assert_eq!(
            edit("opt-level = 2", "opt-level = 1"),
            packages(&["aterm-png"])
        );
        for (from, to) in [
            ("libc = { path", "libm = { path"),
            (r#"version = "0.1.0""#, r#"version = "0.2.0""#),
            (r#"debug = "line-tables-only""#, "debug = false"),
            (
                "[profile.dev.package.aterm-png]",
                "[profile.dev.package.\"*\"]",
            ),
            (
                r#"members = ["crates/*"]"#,
                r#"members = ["crates/*", "x"]"#,
            ),
            ("[patch.crates-io]", "[patch.crates-io"),
        ] {
            assert!(
                matches!(edit(from, to), Replan::Everything(_)),
                "{from} -> {to} must widen"
            );
        }
        assert!(matches!(
            manifest_replan(None, Some(MANIFEST)),
            Replan::Everything(_)
        ));
    }

    const LOCK: &str = r#"version = 4

[[package]]
name = "aterm-http"
version = "0.105.0"
dependencies = [
 "ring",
]

[[package]]
name = "ring"
version = "0.17.14"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"
"#;

    /// THE LOCK, ENTRY BY ENTRY: a changed `[[package]]` selects that package,
    /// a name gone from the lock selects nothing of its own (its dependents'
    /// entries changed with it), a name the lock holds twice is selected by its
    /// version, and any other table widens.
    #[test]
    fn a_lock_change_selects_the_packages_whose_entries_changed() {
        let packages =
            |names: &[&str]| Replan::Packages(names.iter().map(|n| n.to_string()).collect());
        assert_eq!(lock_replan(Some(LOCK), Some(LOCK)), Replan::Nothing);
        let bumped = LOCK.replace("0.17.14", "0.17.15").replace("aaa", "bbb");
        assert_eq!(lock_replan(Some(LOCK), Some(&bumped)), packages(&["ring"]));
        let dropped = LOCK.replace(" \"ring\",\n", "").replace(
            &LOCK[LOCK.find("[[package]]\nname = \"ring\"").expect("ring")..],
            "",
        );
        assert_eq!(
            lock_replan(Some(LOCK), Some(&dropped)),
            packages(&["aterm-http"]),
            "ring left the lock: its one dependent's entry changed"
        );
        let twice = format!(
            "{LOCK}\n[[package]]\nname = \"ring\"\nversion = \"0.16.20\"\nsource = \"registry+x\"\n"
        );
        assert_eq!(
            lock_replan(Some(LOCK), Some(&twice)),
            packages(&["ring@0.16.20"])
        );
        assert!(matches!(
            lock_replan(
                Some(LOCK),
                Some(&LOCK.replace("version = 4", "version = 3"))
            ),
            Replan::Everything(_)
        ));
        assert!(matches!(
            lock_replan(Some(LOCK), Some("[[package")),
            Replan::Everything(_)
        ));
    }

    /// The replan joins the cone: nothing for a comment, the dependents of the
    /// packages it names, and a widening said with its reason.
    #[test]
    fn a_replanning_change_narrows_to_what_it_reaches() {
        let m = members(&["aterm-http", "aterm-gui", "aterm-grid"]);
        let edges: &'static [(&str, &str)] = &[("ring", "aterm-http"), ("aterm-http", "aterm-gui")];
        let pick = |replan: Replan| {
            super::select(
                &paths(&["Cargo.toml"]),
                &manifests(),
                &m,
                &no_packages,
                &graph(edges),
                &|_| replan.clone(),
            )
        };
        assert_eq!(narrowed(&pick(Replan::Nothing)), (vec![], vec![]));
        assert_eq!(
            narrowed(&pick(Replan::Packages(BTreeSet::from(
                ["ring".to_string()]
            )))),
            (
                vec!["ring".to_string()],
                vec!["aterm-gui".to_string(), "aterm-http".to_string()]
            )
        );
        assert!(matches!(
            pick(Replan::Everything("[patch] changed".into())),
            Selection::Widened(why) if why == "Cargo.toml changed: [patch] changed"
        ));
    }

    /// WHAT A CHANGE TOUCHED, WHATEVER THE CALLER'S GIT CONFIG HIDES:
    /// a file moved out of crate `a` lists `a`'s side too (rename detection
    /// printed only `b`'s, so the cone never built the `a` it broke), a gitlink
    /// bump under `diff.ignoreSubmodules = all` is still listed (it listed
    /// nothing, and the cone selected no fork), and a non-ASCII path is
    /// listed as itself (`core.quotePath` printed it quoted, owned by no crate).
    #[test]
    fn a_move_an_ignored_gitlink_and_a_quoted_path_are_each_listed() {
        let repo = crate::mktemp_dir("atv-changed-listed").expect("mktemp");
        let sub = repo.join("vendor/astream");
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let git = |dir: &Path, args: &[&str]| {
            let out = git(dir, &path_env)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "advice.addEmbeddedRepo=false"])
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        for dir in ["crates/a/src", "crates/b/src", "vendor/astream"] {
            std::fs::create_dir_all(repo.join(dir)).expect("dir");
        }
        std::fs::write(repo.join("crates/a/src/x.rs"), "pub fn x() {}\n").expect("write");
        git(&sub, &["init", "-q"]);
        git(&sub, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&repo, &["init", "-q"]);
        for (key, value) in [
            ("diff.renames", "true"),
            ("diff.ignoreSubmodules", "all"),
            ("core.quotePath", "true"),
        ] {
            git(&repo, &["config", key, value]);
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["branch", "shared"]);
        git(&repo, &["mv", "crates/a/src/x.rs", "crates/b/src/x.rs"]);
        git(&sub, &["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&repo, &["add", "vendor/astream"]);
        git(&repo, &["commit", "-qm", "move x out of a; bump astream"]);
        std::fs::write(repo.join("crates/b/src/é.rs"), "\n").expect("write");
        let listed = changed_paths(&repo, &path_env, "shared").map(|(_, paths)| paths);
        let _ = std::fs::remove_dir_all(&repo);
        assert_eq!(
            listed.expect("listed"),
            [
                "crates/a/src/x.rs",
                "crates/b/src/x.rs",
                "crates/b/src/é.rs",
                "vendor/astream"
            ]
        );
    }

    /// An edit an assume-unchanged or skip-worktree flag hides from `git diff`
    /// is in the listing; a flagged file left as it was is not.
    #[test]
    fn an_edit_an_index_flag_hides_is_listed() {
        let repo = crate::mktemp_dir("atv-changed-flagged").expect("mktemp");
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let git = |args: &[&str]| {
            let out = git(&repo, &path_env)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        for c in ["a", "b", "c"] {
            std::fs::create_dir_all(repo.join(format!("crates/{c}/src"))).expect("dir");
            std::fs::write(repo.join(format!("crates/{c}/src/lib.rs")), "\n").expect("write");
        }
        let names: Vec<String> = (0..257).map(|i| format!("source-{i:04}.rs")).collect();
        #[cfg(unix)]
        let names = {
            let mut names = names;
            names.extend(["line\nbreak.rs".into(), "\"quoted.rs".into()]);
            names
        };
        for name in &names {
            std::fs::write(repo.join(name), "unchanged\n").expect("write");
        }
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        git(&["branch", "shared"]);
        git(&["update-index", "--assume-unchanged", "crates/a/src/lib.rs"]);
        git(&["update-index", "--skip-worktree", "crates/b/src/lib.rs"]);
        git(&["update-index", "--skip-worktree", "crates/c/src/lib.rs"]);
        let mut args = vec!["update-index", "--assume-unchanged", "--"];
        args.extend(names.iter().map(String::as_str));
        git(&args);
        std::fs::write(repo.join("crates/a/src/lib.rs"), "pub fn a() {}\n").expect("write");
        std::fs::write(repo.join("crates/b/src/lib.rs"), "pub fn b() {}\n").expect("write");
        let mut want: Vec<String> = names.iter().skip(256).cloned().collect();
        want.push(names[0].clone());
        for name in &want {
            std::fs::write(repo.join(name), "changed\n").expect("write");
        }
        want.extend(["crates/a/src/lib.rs".into(), "crates/b/src/lib.rs".into()]);
        want.sort();
        let listed = changed_paths(&repo, &path_env, "shared").map(|(_, paths)| paths);
        let _ = std::fs::remove_dir_all(&repo);
        assert_eq!(listed.expect("listed"), want);
    }

    fn members(names: &[&str]) -> Members {
        Members::new(
            names
                .iter()
                .map(|n| Member {
                    name: (*n).to_string(),
                    dir: format!("crates/{n}"),
                    has_lib: true,
                })
                .collect(),
        )
    }

    /// A synthetic manifest set shaped like this repo's, including the
    /// non-member ones that stop the ownership walk: a workspace nested inside
    /// a member, and a standalone one.
    fn manifests() -> Manifests {
        [
            ("crates/aterm-grid", Some("aterm-grid")),
            ("crates/aterm-gui", Some("aterm-gui")),
            ("crates/aterm-render", Some("aterm-render")),
            (
                "crates/aterm-render/tests/font_oracle/recorder",
                Some("font-oracle-recorder"),
            ),
            ("crates/aterm-scrollback", Some("aterm-scrollback")),
            ("crates/aterm-lab", Some("aterm-lab")),
            ("libc-oracle", Some("libc-oracle")),
        ]
        .into_iter()
        .collect()
    }

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// A graph that carries no path package outside the workspace.
    fn no_packages() -> Option<Vec<(String, String)>> {
        Some(Vec::new())
    }

    /// A stand-in for `targo tree --invert`: direct reverse edges, closed
    /// transitively the way the real graph query already is.
    fn graph(
        edges: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<Vec<String>> {
        move |name: &str| {
            let mut out = vec![name.to_string()];
            let mut i = 0;
            while i < out.len() {
                let cur = out[i].clone();
                for (dep, user) in edges {
                    if *dep == cur && !out.iter().any(|s| s == user) {
                        out.push((*user).to_string());
                    }
                }
                i += 1;
            }
            Some(out)
        }
    }

    const EDGES: &[(&str, &str)] = &[
        ("aterm-grid", "aterm-gui"),
        ("aterm-gui", "aterm-cli"),
        ("aterm-grid", "aterm-bench"),
    ];

    fn narrowed(sel: &Selection) -> (Vec<String>, Vec<String>) {
        match sel {
            Selection::Narrowed { seeds, crates, .. } => (seeds.clone(), crates.clone()),
            Selection::Widened(why) => panic!("widened unexpectedly: {why}"),
        }
    }

    /// THE OWNER OF A PATH is the nearest manifest's package, when it is a
    /// member: a non-member manifest (the font-oracle recorder nested in
    /// aterm-render, the libc oracle's workspace) stops the walk instead of
    /// charging its parent; the virtual root manifest owns nothing; docs/,
    /// tools/ and the root are no crate's (the whole-tree checks cover them).
    #[test]
    fn a_path_is_owned_by_the_member_whose_manifest_is_nearest() {
        let names = members(&[
            "aterm-grid",
            "aterm-gui",
            "aterm-render",
            "aterm-scrollback",
            "aterm-lab",
        ])
        .names();
        let mut mf = manifests();
        mf.insert("", None);
        for (path, want) in [
            ("crates/aterm-grid/src/lib.rs", Some("aterm-grid")),
            ("crates/aterm-grid/tests/deep/nested.rs", Some("aterm-grid")),
            ("crates/aterm-grid/Cargo.toml", Some("aterm-grid")),
            (
                "crates/aterm-scrollback/src/lib.rs",
                Some("aterm-scrollback"),
            ),
            ("crates/aterm-render/src/lib.rs", Some("aterm-render")),
            (
                "crates/aterm-render/tests/font_oracle/recorder/src/main.rs",
                None,
            ),
            ("libc-oracle/src/lib.rs", None),
            ("docs/PROCESS.md", None),
            ("tools/install.sh", None),
            ("README.md", None),
            ("AGENTS.md", None),
        ] {
            assert_eq!(owning_member(path, &mf, &names), want, "{path}");
        }
    }

    /// THE CONE: the seeds closed under reverse dependency — direct and
    /// transitive dependents, two seeds merged without repeats, the seed kept
    /// even when the graph forgets it (a selection that could DROP a changed
    /// crate is the one failure this tier may never have), non-members dropped
    /// — and a crate's own manifest seeds it without widening.
    #[test]
    fn the_cone_is_the_seeds_closed_under_reverse_dependency() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let pick = |p: &[&str], dependents: &dyn Fn(&str) -> Option<Vec<String>>| {
            narrowed(&select(
                &paths(p),
                &manifests(),
                &m,
                &no_packages,
                dependents,
            ))
        };
        let all = paths(&["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"]);
        assert_eq!(
            pick(&["crates/aterm-grid/src/lib.rs"], &graph(EDGES)),
            (paths(&["aterm-grid"]), all.clone())
        );
        assert_eq!(
            pick(
                &[
                    "crates/aterm-grid/src/lib.rs",
                    "crates/aterm-gui/src/app.rs",
                    "crates/aterm-gui/src/other.rs",
                ],
                &graph(EDGES)
            ),
            (paths(&["aterm-grid", "aterm-gui"]), all)
        );
        assert_eq!(
            pick(&["crates/aterm-grid/Cargo.toml"], &graph(EDGES)).0,
            ["aterm-grid"]
        );
        assert_eq!(
            pick(&["crates/aterm-grid/src/lib.rs"], &|_| Some(vec![])).1,
            ["aterm-grid"]
        );
        assert_eq!(
            pick(&["crates/aterm-grid/src/lib.rs"], &|_| {
                Some(vec!["aterm-grid".into(), "some-registry-crate".into()])
            })
            .1,
            ["aterm-grid"]
        );
        let (seeds, crates) = pick(&["docs/PROCESS.md", "tools/install.sh"], &graph(EDGES));
        assert!(seeds.is_empty() && crates.is_empty());
    }

    /// UNKNOWN IS NEVER NONE: every question the selection cannot answer
    /// widens to the whole workspace, never a narrower set underneath — a
    /// re-planning file whatever seeds came before it, no members, an
    /// unreadable graph for a crate or for the packages outside the workspace,
    /// a touched package whose dependents cannot be read, and an absent targo.
    #[test]
    fn whatever_the_selection_cannot_answer_widens() {
        let m = members(&["aterm-grid", "aterm-link"]);
        let lib = "crates/aterm-grid/src/lib.rs";
        let fork = || {
            Some(vec![(
                "astream-wire".to_string(),
                "vendor/astream/crates/astream-wire".to_string(),
            )])
        };
        let mut cases: Vec<(String, Selection)> = REPLANNING_PATHS
            .iter()
            .map(|f| {
                (
                    format!("{f} changed"),
                    select(
                        &paths(&[lib, f]),
                        &manifests(),
                        &m,
                        &no_packages,
                        &graph(EDGES),
                    ),
                )
            })
            .collect();
        cases.push((
            "no members".into(),
            select(
                &paths(&[lib]),
                &manifests(),
                &Members::default(),
                &no_packages,
                &graph(EDGES),
            ),
        ));
        cases.push((
            "an unreadable inverted graph".into(),
            select(&paths(&[lib]), &manifests(), &m, &no_packages, &|_| None),
        ));
        cases.push((
            "unreadable path packages".into(),
            select(
                &paths(&["vendor/astream"]),
                &manifests(),
                &m,
                &|| None,
                &graph(EDGES),
            ),
        ));
        cases.push((
            "a touched package's dependents unread".into(),
            select(&paths(&["vendor/astream"]), &manifests(), &m, &fork, &|c| {
                (c != "astream-wire").then(Vec::new)
            }),
        ));
        let no_targo = Toolchain::discover(
            Some(Path::new("/nonexistent")),
            Path::new("/nonexistent"),
            OsStr::new(""),
            None,
        );
        cases.push((
            "no targo".into(),
            resolve(Path::new("/"), &no_targo, OsStr::new(""), "main"),
        ));
        for (case, sel) in cases {
            assert!(matches!(sel, Selection::Widened(_)), "{case}: {sel:?}");
            assert!(cone("main", &sel).0.is_workspace(), "{case}");
        }
    }

    #[test]
    fn a_file_a_members_tests_exercise_puts_that_member_in_the_cone() {
        let m = members(&["aterm-update", "aterm-gui"]);
        let edges: &'static [(&str, &str)] = &[("aterm-update", "aterm-gui")];
        let sel = select(
            &paths(&["tools/install.sh"]),
            &manifests(),
            &m,
            &no_packages,
            &graph(edges),
        );
        assert_eq!(narrowed(&sel), (vec![], vec!["aterm-update".to_string()]));
        let m = members(&["aterm-release"]);
        for file in [
            "publish/manifest.txt",
            "publish/pre-promote",
            "publish/transforms.sh",
        ] {
            let sel = select(&paths(&[file]), &manifests(), &m, &no_packages, &graph(&[]));
            assert_eq!(
                narrowed(&sel),
                (vec![], vec!["aterm-release".to_string()]),
                "{file}"
            );
        }
        assert_eq!(
            tested_by("publishing/x"),
            None,
            "a sibling sharing the prefix"
        );
        assert_eq!(tested_by("tools/install.sh"), Some("aterm-update"));
        let m = members(&["aterm-update", "aterm-gui"]);
        let sel = select(
            &paths(&["docs/PERF-REGRESSION-DEFENCE.md"]),
            &manifests(),
            &m,
            &no_packages,
            &graph(edges),
        );
        assert_eq!(narrowed(&sel), (vec![], vec![]), "a file no test claims");
    }

    /// A gitlink bump (only `vendor/astream` is listed) touches every astream crate the
    /// graph carries beneath it, and a `[patch]`ed fork's file touches the fork; either
    /// selects its dependents.
    #[test]
    fn a_vendored_package_or_its_gitlink_selects_the_members_that_depend_on_it() {
        const VENDORED: &[(&str, &str)] = &[
            ("astream-cap", "astream-broker"),
            ("astream-broker", "aterm-link"),
            ("astream-cap", "aterm-link"),
            ("aterm-link", "aterm"),
            ("indexmap", "aterm-gui"),
        ];
        let m = members(&["aterm", "aterm-grid", "aterm-gui", "aterm-link"]);
        let packages = || {
            Some(
                [
                    ("astream-broker", "vendor/astream/crates/astream-broker"),
                    ("astream-cap", "vendor/astream/crates/astream-cap"),
                    ("indexmap", "vendor/indexmap"),
                ]
                .map(|(n, d)| (n.to_string(), d.to_string()))
                .to_vec(),
            )
        };
        let pick = |p: &[&str]| select(&paths(p), &manifests(), &m, &packages, &graph(VENDORED));

        let bump = pick(&["vendor/astream"]);
        assert_eq!(
            narrowed(&bump),
            (
                paths(&["astream-broker", "astream-cap"]),
                paths(&["aterm", "aterm-link"])
            )
        );
        assert_eq!(
            cone("origin/main", &bump).0.crates(),
            ["aterm", "aterm-link"],
            "a vendored seed is no crate of the cone"
        );
        assert_eq!(
            narrowed(&pick(&["vendor/indexmap/src/map.rs"])).1,
            ["aterm-gui"]
        );
        let (seeds, crates) = narrowed(&pick(&[
            "vendor/README-astream.md",
            "vendor/forge.toml",
            "vendor/astream-other/src/lib.rs",
        ]));
        assert!(seeds.is_empty() && crates.is_empty());
    }

    #[test]
    fn shared_source_and_asset_consumers_join_the_change_cone_transitively() {
        let repo = crate::mktemp_dir("atv-shared-inputs").expect("mktemp");
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let write = |path: &str, text: &str| {
            let file = repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write("crates/a/src/shared.rs", "pub const VALUE: u8 = 1;");
        write("crates/a/assets/font.bin", "font");
        write(
            "crates/b/src/lib.rs",
            "#[path = \"../../a/src/shared.rs\"] mod shared;",
        );
        write("crates/c/src/lib.rs", "include!(\"../../b/src/lib.rs\");");
        write(
            "crates/d/src/lib.rs",
            "const FONT: &[u8] = include_bytes!(r\"../../a/assets/font.bin\");",
        );
        write(
            "crates/e/src/lib.rs",
            "const QUOTE: &str = \"\\\"\"; include_str!(\"../../a/assets/font.bin\");",
        );
        let status = git(&repo, &path_env).args(["init", "-q"]).status().unwrap();
        assert!(status.success());
        let m = Members::new(
            ["a", "b", "c", "d", "e"]
                .into_iter()
                .map(|name| Member {
                    name: name.into(),
                    dir: format!("crates/{name}"),
                    has_lib: true,
                })
                .collect(),
        );
        let source =
            with_shared_inputs(&repo, &path_env, paths(&["crates/a/src/shared.rs"]), &m).unwrap();
        assert_eq!(
            source,
            paths(&[
                "crates/a/src/shared.rs",
                "crates/b/src/lib.rs",
                "crates/c/src/lib.rs"
            ])
        );
        let asset =
            with_shared_inputs(&repo, &path_env, paths(&["crates/a/assets/font.bin"]), &m).unwrap();
        assert_eq!(
            asset,
            paths(&[
                "crates/a/assets/font.bin",
                "crates/d/src/lib.rs",
                "crates/e/src/lib.rs"
            ])
        );
        let docs = paths(&["docs/PROCESS.md"]);
        assert_eq!(
            with_shared_inputs(&repo, &path_env, docs.clone(), &m).unwrap(),
            docs
        );
        std::fs::remove_dir_all(repo).unwrap();
    }

    /// A literal ending in '/' names a folder: a changed file beneath it
    /// seeds the consumer, whether the literal is relative to the source
    /// file or to its package root (a build script, a `CARGO_MANIFEST_DIR`
    /// join).
    #[test]
    fn a_directory_literal_seeds_its_consumer() {
        let root = Path::new("/repo");
        let member = root.join("crates/aterm-theme-pack");
        let changed = [root.join("contrib/themes/pack/Iceberg Dark.conf")];
        let build = member.join("build.rs");
        let test = member.join("src/tests.rs");
        let folder = "let pack = dir.join(\"../../contrib/themes/pack/\");";
        assert!(shares_changed_input(&build, folder, &member, &changed));
        assert!(shares_changed_input(&test, folder, &member, &changed));
        let path = "let pack = dir.join(\"../../contrib/themes/pack\");";
        assert!(!shares_changed_input(&build, path, &member, &changed));
        assert!(!shares_changed_input(&test, path, &member, &changed));
        let elsewhere = [root.join("contrib/fonts/x.ttf")];
        assert!(!shares_changed_input(&build, folder, &member, &elsewhere));
    }

    /// A literal that is only `../` segments (a `starts_with("../")` test)
    /// or names a folder holding the member reads no input: a change in
    /// another crate does not seed the member.
    #[test]
    fn a_bare_parent_literal_seeds_nothing() {
        let root = Path::new("/repo");
        let member = root.join("crates/x");
        let lib = member.join("src/lib.rs");
        let changed = [root.join("crates/y/src/z.rs")];
        for text in [
            "if s.starts_with(\"../\") {}",
            "let up = \"../../\";",
            "let crates = \"../\";",
            "let here = \"./../\";",
        ] {
            assert!(
                !shares_changed_input(&lib, text, &member, &changed),
                "{text}"
            );
        }
        let named = "let y = dir.join(\"../y/\");";
        assert!(shares_changed_input(&lib, named, &member, &changed));
    }

    /// The facade is package `rustix` beside the registry rustix 1.x: the
    /// cone names it by version, for `-p` as for `tree --invert`, and every
    /// other member by its bare name.
    #[test]
    fn a_member_whose_name_the_lock_shares_is_named_with_its_version() {
        let lock = "version = 4\n\n[[package]]\nname = \"aterm-gui\"\nversion = \"0.1.0\"\n\n\
                    [[package]]\nname = \"rustix\"\nversion = \"0.38.44\"\n\n\
                    [[package]]\nname = \"rustix\"\nversion = \"1.1.4\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                    [[patch.unused]]\nname = \"aterm-gui\"\nversion = \"0.0.1\"\n";
        let shared = lock_shared_names(lock);
        assert_eq!(shared, BTreeSet::from(["rustix".to_string()]));
        let listing = "aterm-gui v0.1.0 (/repo/crates/aterm-gui)\n\
                     rustix v0.38.44 (/repo/crates/aterm-rustix-compat)\n\
                     rustix v1.1.4\n";
        let versions = parse_graph_versions(listing, "/repo/");
        assert_eq!(versions.len(), 2);
        let specs = versions
            .into_iter()
            .filter(|(n, _)| shared.contains(n))
            .map(|(n, v)| (n.clone(), format!("{n}@{v}")))
            .collect();
        let m = members(&["aterm-gui", "rustix"]).with_specs(specs);
        assert_eq!(m.spec("rustix"), "rustix@0.38.44");
        assert_eq!(m.spec("aterm-gui"), "aterm-gui");
        let edges: &'static [(&str, &str)] = &[("rustix", "aterm-gui")];
        let sel = select(
            &paths(&["crates/rustix/src/lib.rs"]),
            &[("crates/rustix", Some("rustix"))].into_iter().collect(),
            &m,
            &no_packages,
            &graph(edges),
        );
        let (seeds, crates) = narrowed(&sel);
        assert_eq!(seeds, ["rustix"]);
        assert_eq!(crates, ["aterm-gui", "rustix@0.38.44"]);
    }

    /// The scope the cone hands the checks: its crates, one `-p` each; an
    /// empty cone selects nothing.
    #[test]
    fn the_cone_hands_the_checks_its_crates_or_nothing() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let sel = |p: &[&str]| select(&paths(p), &manifests(), &m, &no_packages, &graph(EDGES));
        let (scope, _) = cone("origin/main", &sel(&["crates/aterm-grid/src/lib.rs"]));
        assert_eq!(
            scope.crates(),
            ["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"]
        );
        assert!(
            cone("origin/main", &sel(&["docs/x.md"]))
                .0
                .selects_nothing()
        );
    }

    /// THE DOCTEST LIB QUESTION, answered fail-closed (an unanswerable one is
    /// YES): a scoped crate from its own manifest or `src/lib.rs`, the cone
    /// from its members — an all-binary selection has none (`cargo test --doc
    /// -p aterm-lab` is a hard error), and a table where no member claims a lib is
    /// not believed.
    #[test]
    fn the_lib_question_is_answered_from_the_manifest_or_yes() {
        let tmp = std::env::temp_dir().join(format!("atv-scoped-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        assert!(!crate_dir_has_lib(&tmp, "binonly"));
        std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
        assert!(crate_dir_has_lib(&tmp, "binonly"));
        let declared = tmp.join("crates/declared");
        std::fs::create_dir_all(&declared).unwrap();
        std::fs::write(
            declared.join("Cargo.toml"),
            "[package]\nname = \"declared\"\n[lib]\npath = \"other.rs\"\n",
        )
        .unwrap();
        assert!(crate_dir_has_lib(&tmp, "declared"));
        assert!(crate_dir_has_lib(&tmp, "no-such-member"));
        let _ = std::fs::remove_dir_all(&tmp);
        let m = Members::new(vec![
            Member {
                name: "aterm-lab".into(),
                dir: "crates/aterm-lab".into(),
                has_lib: false,
            },
            Member {
                name: "aterm-grid".into(),
                dir: "crates/aterm-grid".into(),
                has_lib: true,
            },
        ]);
        assert!(!m.any_has_lib(&["aterm-lab".to_string()]));
        assert!(m.any_has_lib(&["aterm-lab".to_string(), "aterm-grid".to_string()]));
        let none = Members::new(vec![Member {
            name: "aterm-lab".into(),
            dir: "crates/aterm-lab".into(),
            has_lib: false,
        }]);
        assert!(none.any_has_lib(&["aterm-lab".to_string()]));
        assert!(!declares_lib_target(
            "[package]\nname=\"x\"\n[[bin]]\nname=\"x\"\n"
        ));
    }

    /// `targo tree`'s `{p}` lines: the name is field one and membership is the
    /// path being inside the repo; registry crates have no path, and
    /// parentheses in the checkout's or a package's path survive.
    #[test]
    fn graph_lines_are_parsed_by_name_and_by_being_inside_the_repo() {
        let text = "\
aterm-grid v0.10.0 (/repo/crates/aterm-grid)
aterm-error-derive v0.10.0 (proc-macro) (/repo/crates/aterm-error-derive)
aterm-gui v0.10.0 (/repo/crates/aterm-gui) (*)
libc v0.2.0
indexmap v2.14.0 (/repo/vendor/indexmap)
";
        let names: Vec<String> = parse_graph_entries(text, "/repo/")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            ["aterm-grid", "aterm-error-derive", "aterm-gui", "indexmap"]
        );
        let text = "a v1.0.0 (/repo (copy)/crates/a (local)) (*)\nb v1.0.0 (proc-macro) (/repo (copy)/crates/b)";
        assert_eq!(
            parse_graph_entries(text, "/repo (copy)/"),
            [
                ("a".into(), "crates/a (local)".into()),
                ("b".into(), "crates/b".into()),
            ]
        );
        assert!(parse_graph_entries("a v1.0.0 (/repo (copy)/crates/a", "/repo (copy)/").is_empty());
    }

    #[test]
    fn the_package_name_comes_from_the_package_table_and_nowhere_else() {
        assert_eq!(
            manifest_package_name("[package]\nname = \"aterm-grid\"\nversion = \"0.1.0\"\n")
                .as_deref(),
            Some("aterm-grid")
        );
        assert_eq!(
            manifest_package_name("[workspace]\nmembers = [\"crates/*\"]\n"),
            None,
            "a virtual manifest declares no package"
        );
        assert_eq!(
            manifest_package_name("[package]\nname=\"tight\"\n").as_deref(),
            Some("tight")
        );
        assert_eq!(
            manifest_package_name("[package]\nversion = \"1\"\n\n[[bin]]\nname = \"notthis\"\n"),
            None,
            "only the package name is a -p spec"
        );
        assert_eq!(
            manifest_package_name("[package]\nname = \"first\"\n\n[lib]\nname = \"second\"\n")
                .as_deref(),
            Some("first")
        );
    }
}
