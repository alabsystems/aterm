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
    /// Repo-relative directory of its manifest, which is what says a package is this member: a
    /// package outside the workspace can have its name ([`Members::at`]).
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
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.0.iter().map(|m| m.name.clone()).collect()
    }

    /// The name of the member whose manifest is in `dir`. A name cannot say this:
    /// `crates/aterm-windows-sys` is excluded from the workspace because it has the name of the
    /// member `crates/aterm-windows-sys-052`.
    #[must_use]
    pub fn at(&self, dir: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|m| m.dir == dir)
            .map(|m| m.name.as_str())
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

/// The repo-relative directories that hold a `Cargo.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifests(BTreeSet<String>);

impl Manifests {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, dir: impl Into<String>) {
        self.0.insert(dir.into());
    }

    /// Whether a manifest is in `dir`.
    #[must_use]
    pub fn holds(&self, dir: &str) -> bool {
        self.0.contains(dir)
    }
}

impl<S: Into<String>> FromIterator<S> for Manifests {
    fn from_iter<I: IntoIterator<Item = S>>(it: I) -> Self {
        let mut m = Self::new();
        for dir in it {
            m.insert(dir);
        }
        m
    }
}

/// Outcome of change-cone selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The cone widened to the entire workspace for the given reason.
    Widened(String),
    /// The cone narrowed to the seeds and what each reaches ([`Reach`]).
    Narrowed {
        /// What the change itself touched: members whose own files changed, the packages a lock or
        /// root manifest row names, and vendored packages whose files changed.
        seeds: Vec<String>,
        /// By `-p` spec, the members among `seeds`, those a seed reaches, and those [`TESTED_BY`] a
        /// changed file.
        crates: Vec<String>,
        /// Whether any selected crate declares a library target ([`Members::any_has_lib`]).
        any_lib: bool,
        /// The members, by name, whose test, bench and example targets the cone's lint compiles:
        /// those whose own files changed and those [`TESTED_BY`] a changed file. A root manifest
        /// row, a lock row other than a touched member's own entry, or a fork moves code under any
        /// member's tests, so any of those makes this the whole cone.
        all_targets: Vec<String>,
        /// The members, by name, the cone's lint leaves out: those it holds only by a dev edge
        /// ([`Reach::dev_only`]) and not in `all_targets`. Their libraries and binaries link
        /// nothing that changed, and the test compile, which takes all of `crates`, fires any
        /// compile error in their tests.
        unlinted: Vec<String>,
    },
}

/// Repo-relative paths whose changes can replan or re-flag the workspace, or change what its lint
/// denies: clippy reads the nearest config above a manifest, so a root one is the lint of every
/// member without its own, which no member's files say; and `trust.lock` names the compiler of
/// every member. A rustfmt config is not here: no compile reads it, and `fmt` reads the whole tree
/// whatever the selection.
pub const REPLANNING_PATHS: [&str; 9] = [
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    aterm_toolchain::trust_lock::LOCK_FILE,
    ".cargo/config",
    ".cargo/config.toml",
    "clippy.toml",
    ".clippy.toml",
];

#[must_use]
pub fn replans_everything(path: &str) -> bool {
    REPLANNING_PATHS.contains(&path)
}

/// Repo-relative files or directories outside crates that specific members test: install.sh's
/// suites are aterm-update's `install_sh` and aterm-bootstrap-verify's (its Linux half, through
/// the bootstrap verifier).
pub const TESTED_BY: [(&str, &str); 3] = [
    ("tools/install.sh", "aterm-update"),
    ("tools/install.sh", "aterm-bootstrap-verify"),
    ("publish/", "aterm-release"),
];

/// Every workspace member whose tests exercise `path` ([`TESTED_BY`]), in its order.
pub fn tested_by(path: &str) -> impl Iterator<Item = &'static str> + '_ {
    TESTED_BY
        .iter()
        .filter(move |(file, _)| {
            file.strip_suffix('/')
                .map_or(*file == path, |dir| path.starts_with(&format!("{dir}/")))
        })
        .map(|(_, member)| *member)
}

/// The members a change to one package reaches ([`reached`]), by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    /// Of the package and what closes over normal and build edges from it, the members: their
    /// libraries and binaries link the change.
    pub links: Vec<String>,
    /// The member dev dependents of those that are not among them: only their test, bench and
    /// example targets link the change.
    pub dev_only: Vec<String>,
}

/// Impact of a change to one of [`REPLANNING_PATHS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replan {
    /// No crate builds differently.
    Nothing,
    /// Specific packages whose build inputs changed.
    Packages(BTreeSet<String>),
    /// Every crate may build or lint differently for the given reason.
    Everything(String),
}

/// Replan that widens to the entire workspace.
#[must_use]
pub fn replan_everything() -> Replan {
    Replan::Everything("it can change how every crate is built or linted".to_string())
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
                        "package" => {
                            for field in changed_keys_of(sb, sw) {
                                if field != "version" {
                                    return Replan::Everything(format!(
                                        "[workspace.package] {field} changed"
                                    ));
                                }
                                // The product version: only the `aterm` bin package, which
                                // nothing depends on, inherits it (census `identity-read`).
                                seeds.insert("aterm".to_string());
                            }
                        }
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

fn parse_toml(text: Option<&str>) -> Option<aterm_toml::Table> {
    match aterm_toml::from_str::<aterm_toml::Value>(text?).ok()? {
        aterm_toml::Value::Table(t) => Some(t),
        _ => None,
    }
}

fn changed_keys<'a>(
    a: &'a aterm_toml::Table,
    b: &'a aterm_toml::Table,
) -> impl Iterator<Item = &'a str> + 'a {
    differing(Some(a), Some(b)).into_iter()
}

/// [`changed_keys`] of two values that should be tables.
fn changed_keys_of<'a>(
    a: Option<&'a aterm_toml::Value>,
    b: Option<&'a aterm_toml::Value>,
) -> Vec<&'a str> {
    differing(
        a.and_then(aterm_toml::Value::as_table),
        b.and_then(aterm_toml::Value::as_table),
    )
}

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

fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// The workspace member owning repo-relative `path`: the one whose manifest ([`Members::at`]) is
/// the nearest above it, so a manifest that is no member's owns its subtree for no member. The
/// spec table at a manifest's own root (`<dir>/SPECS.md`) is owned by none, as `docs/*.md` is: no
/// build, include or test reads it.
#[must_use]
pub fn owning_member<'a>(
    path: &str,
    manifests: &Manifests,
    members: &'a Members,
) -> Option<&'a str> {
    let dir = owning_dir(path, manifests)?;
    if path.strip_prefix(dir) == Some("/SPECS.md") {
        return None;
    }
    members.at(dir)
}

/// Select the workspace crates that the change cone must build and test.
#[must_use]
pub fn select(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
    path_packages: &dyn Fn() -> Option<Vec<(String, String)>>,
    dependents: &dyn Fn(&str) -> Option<Reach>,
    replan: &dyn Fn(&str) -> Replan,
) -> Selection {
    if members.is_empty() {
        return Selection::Widened("`targo tree` returned no workspace members".to_string());
    }
    let names = members.names();

    let (seeds, _) = seeds(paths, manifests, members);
    let mut replanned: BTreeSet<String> = BTreeSet::new();
    // A root manifest row changes how every dependent builds the package it names, whichever files
    // changed beside it.
    let mut root_row = false;
    for p in paths.iter().filter(|p| replans_everything(p)) {
        match replan(p) {
            Replan::Everything(why) => {
                return Selection::Widened(format!("{p} changed: {why}"));
            }
            Replan::Packages(packages) => {
                root_row |= p == "Cargo.toml";
                replanned.extend(packages);
            }
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
    let own = seeds.clone();
    let mut seeds = seeds;
    seeds.extend(replanned);
    seeds.extend(forks_touched(paths, &packages));
    let mut dev_only: BTreeSet<String> = BTreeSet::new();
    for seed in &seeds {
        let Some(found) = dependents(seed.as_str()) else {
            return Selection::Widened(format!(
                "`targo tree --invert {seed}` failed or could not be read, so its dependents are \
                 unknown"
            ));
        };
        if !found
            .links
            .iter()
            .chain(&found.dev_only)
            .any(|n| names.contains(n))
        {
            return Selection::Widened(format!(
                "`targo tree --invert {seed}` reaches no member: this host's graph does not hold \
                 it, so what depends on it on another target is unknown"
            ));
        }
        cone.extend(found.links.into_iter().filter(|n| names.contains(n)));
        dev_only.extend(found.dev_only.into_iter().filter(|n| names.contains(n)));
    }
    dev_only.retain(|n| !cone.contains(n));
    cone.extend(dev_only.iter().cloned());
    let tested: BTreeSet<String> = paths
        .iter()
        .flat_map(|p| tested_by(p))
        .map(str::to_string)
        .filter(|n| names.contains(n))
        .collect();
    cone.extend(tested.iter().cloned());
    let (all_targets, unlinted) = if root_row || !seeds.is_subset(&own) {
        (cone.iter().cloned().collect(), Vec::new())
    } else {
        let every: BTreeSet<&String> = own.iter().chain(&tested).collect();
        (
            every.into_iter().cloned().collect(),
            dev_only.difference(&tested).cloned().collect(),
        )
    };

    let named: Vec<String> = cone.into_iter().collect();
    let any_lib = members.any_has_lib(&named);
    Selection::Narrowed {
        seeds: seeds.into_iter().collect(),
        crates: named.iter().map(|n| members.spec(n).to_string()).collect(),
        any_lib,
        all_targets,
        unlinted,
    }
}

/// Workspace members whose own files changed in `paths`.
#[must_use]
pub fn seeds(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
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

/// The directory of the nearest manifest above a repo-relative path ([`owning_member`]).
fn owning_dir<'a>(path: &'a str, manifests: &Manifests) -> Option<&'a str> {
    let mut dir = parent_dir(path);
    while !dir.is_empty() {
        if manifests.holds(dir) {
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
            all_targets,
            unlinted,
        } => {
            let dependents = crates.iter().filter(|c| !seeds.contains(c)).count();
            let more = if dependents == 0 {
                String::new()
            } else {
                format!(" + {dependents} dependent(s)")
            };
            (
                Scope::changed(base, crates.clone(), *any_lib)
                    .with_seeds(seeds.clone())
                    .with_all_targets(all_targets.clone())
                    .with_unlinted(unlinted.clone()),
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
        &|c| invert(root, &tools.targo, path_env, members.spec(c), &members),
        &|p| replan_of(root, path_env, &merge_base, p),
    )
}

/// Replan for `path` since `merge_base` ([`manifest_replan`], [`lock_replan`]).
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
    let work = || crate::read_regular_text(root.join(path)).ok();
    match path {
        "Cargo.toml" => manifest_replan(read().as_deref(), work().as_deref()),
        "Cargo.lock" => lock_replan(read().as_deref(), work().as_deref()),
        _ => replan_everything(),
    }
}

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
        let text = match crate::read_regular_text(&file) {
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

/// The path a literal continues from a directory it does not spell: what follows its leading `/`
/// (`concat!(env!("CARGO_MANIFEST_DIR"), "/../b/x")`) or its leading format argument and `/`
/// (`format!("{}/../b/x", env!("CARGO_MANIFEST_DIR"))`, `format!("{dir}/../b/x")`).
fn continued_path(literal: &str) -> Option<&str> {
    let after_argument = literal
        .strip_prefix('{')
        .and_then(|rest| rest.split_once('}'))
        .map(|(_, rest)| rest);
    after_argument.unwrap_or(literal).strip_prefix('/')
}

/// Whether `text`, a source of the member at `member`, names something outside that member that
/// changed through a quoted `../` literal. A literal that continues a directory
/// ([`continued_path`]) starts at the member: each such literal here that names a real file
/// continues the manifest directory, and any other matches nothing while no such path exists. Any
/// other literal may start at the source's folder (`include_str!`) or the member (`Path::join`, a build
/// script). The literal names one file unless it ends in `/`, which names a folder: a change
/// under `steps` selects no reader of `"/../a/steps"`, and one of `"/../a/steps/"` does.
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
        let (path, start) = match continued_path(literal) {
            Some(path) => (path, member),
            None => (literal, parent),
        };
        let folder = path.ends_with('/')
            && path
                .split('/')
                .any(|segment| !segment.is_empty() && segment != "." && segment != "..");
        [start, member].into_iter().any(|base| {
            let mut target = std::path::PathBuf::new();
            for component in base.join(path).components() {
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
    let mut c = Command::new(aterm_uds::os_path!("git"));
    c.current_dir(dir)
        .env("PATH", path_env)
        .arg("--no-optional-locks")
        .stdin(std::process::Stdio::null());
    for k in GIT_REDIRECTS {
        c.env_remove(k);
    }
    c
}

/// Every path changed since the merge-base of HEAD and `base`: committed, staged, unstaged and
/// untracked ([`flag_hidden_edits`] too).
///
/// # Errors
/// This is no git checkout, HEAD and `base` share no merge-base, or git cannot list the files.
pub fn changed_since(root: &Path, path_env: &OsStr, base: &str) -> Result<Vec<String>, String> {
    changed_paths(root, path_env, base).map(|(_, paths)| paths)
}

/// The merge-base of HEAD and `base`, and every path changed since it ([`flag_hidden_edits`]).
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

fn stdout_of(cmd: &mut Command, root: &Path, path_env: &OsStr) -> Option<String> {
    let out = cmd.current_dir(root).env("PATH", path_env).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One `cargo tree` read, in plain text whatever colour the environment or a cargo config asks for
/// ([`reached`] reads its prefixes by column).
fn targo_tree(root: &Path, targo: &Path, path_env: &OsStr, args: &[&str]) -> Option<String> {
    stdout_of(
        Command::new(targo).args(args).args(["--color", "never"]),
        root,
        path_env,
    )
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

/// What `cargo tree`, run in `root`, prints before the directory of a package under it.
fn tree_root_prefix(root: &Path) -> String {
    tree_prefix(&root.display().to_string(), cfg!(windows))
}

/// `root` as a child started in it reads its own directory, then the separator. Off Windows that
/// is `root` as given. On Windows std starts the child in `root` without a verbatim `\\?\` (what
/// `fs::canonicalize` returns), turning `\\?\UNC\` into `\\`, and cargo prints each path with
/// `\`, where git's `rev-parse --show-toplevel` spells the root `C:/…`.
fn tree_prefix(root: &str, windows: bool) -> String {
    let (mut prefix, sep) = if windows {
        let plain = match root.strip_prefix(r"\\?\UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => root.strip_prefix(r"\\?\").unwrap_or(root).to_owned(),
        };
        (plain.replace('/', "\\"), '\\')
    } else {
        (root.to_owned(), '/')
    };
    if !prefix.ends_with(sep) {
        prefix.push(sep);
    }
    prefix
}

fn normalize_manifest_dir(dir: String) -> String {
    if cfg!(windows) {
        dir.replace('\\', "/")
    } else {
        dir
    }
}

fn read_members(root: &Path, tools: &Toolchain, path_env: &OsStr) -> Members {
    let Some(out) = targo_tree(
        root,
        &tools.targo,
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
    let prefix = tree_root_prefix(root);
    let shared = crate::read_regular_text(root.join("Cargo.lock"))
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
                let dir = normalize_manifest_dir(dir);
                let at = root.join(&dir);
                let has_lib = at.join("src/lib.rs").is_file()
                    || crate::read_regular_text(at.join("Cargo.toml"))
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

/// The path packages outside the workspace, on any target's graph ([`outside_packages`]): a change
/// to one this host's graph does not hold is still a seed, which [`select`] then widens on.
fn read_path_packages(
    root: &Path,
    tools: &Toolchain,
    path_env: &OsStr,
    members: &Members,
) -> Option<Vec<(String, String)>> {
    let out = targo_tree(
        root,
        &tools.targo,
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
            "--target",
            "all",
        ],
    )?;
    outside_packages(&out, &tree_root_prefix(root), members)
}

/// Of one `cargo tree --format {p}` printout, the in-repo packages whose directory is no member's,
/// each as `(spec, dir)`: its name, or `name@version` where a member has that name, because
/// [`Members::spec`] answers a bare name with that member. `None` on a line it cannot read the
/// version of, which widens.
fn outside_packages(
    tree: &str,
    root_prefix: &str,
    members: &Members,
) -> Option<Vec<(String, String)>> {
    let names = members.names();
    let mut found = Vec::new();
    for line in tree.lines() {
        let Some((name, dir)) = parse_graph_entries(line, root_prefix).pop() else {
            continue;
        };
        let dir = normalize_manifest_dir(dir);
        if members.at(&dir).is_some() {
            continue;
        }
        let spec = if names.contains(&name) {
            let (_, version) = tree_package(line)?.split_once(" v")?;
            format!("{name}@{version}")
        } else {
            name
        };
        found.push((spec, dir));
    }
    found.sort();
    found.dedup();
    Some(found)
}

/// The members a change to `crate_name` reaches ([`reached`]); `None` when `cargo tree` failed or
/// printed what [`reached`] cannot read. The one tree has dev edges in it for two reasons: they
/// are the final hop, and asking for them is what resolves features with dev units on, so a normal
/// edge only a dev-dependency's feature turns on (`aterm-wasm-abi` on its macros, by its
/// dev-dependency on itself) is in the tree at all.
fn invert(
    root: &Path,
    targo: &Path,
    path_env: &OsStr,
    crate_name: &str,
    members: &Members,
) -> Option<Reach> {
    let out = targo_tree(
        root,
        targo,
        path_env,
        &[
            "tree",
            "--workspace",
            "--invert",
            crate_name,
            "--charset",
            "ascii",
            "--format",
            "{p}",
            "--edges",
            "normal,build,dev",
        ],
    )?;
    reached(&out, &tree_root_prefix(root), members)
}

/// The `name vX.Y.Z` that opens a `cargo tree` entry, or `None` when it opens with anything else.
fn tree_package(entry: &str) -> Option<&str> {
    let (name, rest) = entry.split_once(' ')?;
    let version = rest.split(' ').next()?;
    let named = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'));
    let versioned = version
        .strip_prefix('v')
        .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()));
    (named && versioned).then(|| &entry[..name.len() + 1 + version.len()])
}

/// Of one `cargo tree --invert <X>` printout (`--charset ascii`, each level four columns, each
/// package's edges printed once), the members that build differently when X changes, each known by
/// its directory ([`Members::at`]): X, whatever closes over normal and build edges from it, and
/// then every dev dependent of any of those. A dev edge is the final hop and never transit: a
/// member that dev-depends on X links it for its own tests (aterm-models dev-depends on every crate
/// that anchors a model, so a change to any of them brings its source-spec closure test into the
/// cone), but its own dependents build no dev-dependency of it. X is in the closure as the tree's
/// root, so a package only dev edges reach (`astream-term`, which `aterm-conformance`'s differential
/// oracle runs on a bump of) still reaches its dev dependents.
///
/// `None` on a non-empty line that is neither a section header nor a package one level under what
/// is printed above it: a misread must widen the cone, never narrow it to whatever did parse. An
/// empty printout reads as empty: `cargo tree` found no such package on this host ([`select`]).
fn reached(tree: &str, root_prefix: &str, members: &Members) -> Option<Reach> {
    let mut member_of: BTreeMap<&str, String> = BTreeMap::new();
    let mut closure: BTreeSet<&str> = BTreeSet::new();
    let mut edges: Vec<(&str, &str, bool)> = Vec::new();
    // The package at each depth of the line being read, and whether the edges below it are dev.
    let mut path: Vec<(&str, bool)> = Vec::new();
    for line in tree.lines().filter(|line| !line.is_empty()) {
        let entry = line.trim_start_matches(['|', '`', '-', ' ']);
        let indent = line.len() - entry.len();
        if indent % 4 != 0 {
            return None;
        }
        let depth = indent / 4;
        if matches!(entry, "[dev-dependencies]" | "[build-dependencies]") {
            path.get_mut(depth)?.1 = entry == "[dev-dependencies]";
            continue;
        }
        if depth > path.len() {
            return None;
        }
        let package = tree_package(entry)?;
        if let Some((_, dir)) = parse_graph_entries(entry, root_prefix).pop()
            && let Some(name) = members.at(&normalize_manifest_dir(dir))
        {
            member_of.insert(package, name.to_string());
        }
        path.truncate(depth);
        match path.last() {
            Some((dependency, dev)) => edges.push((*dependency, package, *dev)),
            None => {
                closure.insert(package);
            }
        }
        path.push((package, false));
    }
    while edges
        .iter()
        .fold(false, |grew, (dependency, dependent, dev)| {
            (!*dev && closure.contains(dependency) && closure.insert(dependent)) || grew
        })
    {}
    let final_hop: BTreeSet<&str> = edges
        .iter()
        .filter(|(dependency, dependent, dev)| {
            *dev && closure.contains(dependency) && !closure.contains(dependent)
        })
        .map(|(_, dependent, _)| *dependent)
        .collect();
    let names = |packages: &BTreeSet<&str>| {
        packages
            .iter()
            .filter_map(|package| member_of.get(package).cloned())
            .collect()
    };
    Some(Reach {
        links: names(&closure),
        dev_only: names(&final_hop),
    })
}

fn manifests_near(root: &Path, paths: &[String]) -> Manifests {
    let mut m = Manifests::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for p in paths {
        let mut dir = parent_dir(p);
        while !dir.is_empty() {
            if seen.insert(dir.to_string())
                && crate::read_regular_text(root.join(dir).join("Cargo.toml")).is_ok()
            {
                m.insert(dir);
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
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("name") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|&c| c == '"' || c == '\'') else {
            continue;
        };
        return rest[1..]
            .find(quote)
            .map(|end| rest[1..1 + end].to_string());
    }
    None
}

/// Whether a `Cargo.toml` text explicitly declares a `[lib]` target.
#[must_use]
pub fn declares_lib_target(manifest: &str) -> bool {
    manifest
        .lines()
        .any(|l| l.split('#').next().unwrap_or("").trim() == "[lib]")
}

/// Whether workspace member `crates/<name>` has a library target.
#[must_use]
pub fn crate_dir_has_lib(root: &std::path::Path, name: &str) -> bool {
    let name = crate::scope::spec_name(name).unwrap_or(name);
    let dir = root.join("crates").join(name);
    if let Ok(manifest) = crate::read_regular_text(dir.join("Cargo.toml"))
        && manifest_package_name(&manifest).is_none_or(|pkg| pkg == name)
    {
        return declares_lib_target(&manifest) || dir.join("src/lib.rs").is_file();
    }
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            if !entry
                .file_type()
                .map(|ft| ft.is_dir() || ft.is_symlink())
                .unwrap_or(true)
            {
                continue;
            }
            let member_dir = entry.path();
            if let Ok(manifest) = crate::read_regular_text(member_dir.join("Cargo.toml"))
                && manifest_package_name(&manifest).as_deref() == Some(name)
            {
                return declares_lib_target(&manifest) || member_dir.join("src/lib.rs").is_file();
            }
        }
    }
    true
}

/// A `-p` spec no host check can build: it names a package of `Cargo.lock` that this host's
/// build graph does not hold (the `windows-sys` 0.61.2 facade, which only Windows targets
/// reach), and cargo panics resolving its features rather than say so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Foreign {
    /// The spec as `-p` gave it.
    pub spec: String,
    /// The workspace crates that depend on it on some target, when the graph names them.
    pub consumers: Vec<String>,
}

/// Every `(name, version)` a `targo tree --format {p}` listing names, whatever its prefix.
#[must_use]
pub fn parse_tree_packages(text: &str) -> BTreeSet<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start_matches(|c: char| c.is_ascii_digit());
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let version = words.next()?.strip_prefix('v')?;
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

/// Every `[[package]]`'s `(name, version)` in a `Cargo.lock`.
#[must_use]
pub fn lock_packages(lock: &str) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    let mut name: Option<&str> = None;
    for line in lock.lines() {
        if line.starts_with('[') {
            name = None;
        } else if let Some(n) = line
            .strip_prefix("name = \"")
            .and_then(|r| r.strip_suffix('"'))
        {
            name = Some(n);
        } else if let Some(v) = line
            .strip_prefix("version = \"")
            .and_then(|r| r.strip_suffix('"'))
            && let Some(n) = name.take()
        {
            found.insert((n.to_string(), v.to_string()));
        }
    }
    found
}

/// Whether the `-p` spec names the package `name` at `version`, as cargo matches one: by name,
/// and by version when the spec gives one, a partial one (`0.61`) matching each release it
/// prefixes.
#[must_use]
pub fn spec_matches(spec: &str, name: &str, version: &str) -> bool {
    if crate::scope::spec_name(spec) != Some(name) {
        return false;
    }
    let tail = spec.rsplit_once('#').map_or(spec, |(_, t)| t);
    match tail.split_once('@') {
        None => true,
        Some((_, want)) => {
            version == want
                || version
                    .strip_prefix(want)
                    .is_some_and(|rest| rest.starts_with('.'))
        }
    }
}

/// The specs of `specs` that name a package of `lock` and none of `graph`, the host's build
/// graph. A spec the lock does not hold either is left to cargo, which names it.
#[must_use]
pub fn outside_graph(
    specs: &[String],
    lock: &BTreeSet<(String, String)>,
    graph: &BTreeSet<(String, String)>,
) -> Vec<String> {
    let holds = |set: &BTreeSet<(String, String)>, spec: &str| {
        set.iter().any(|(n, v)| spec_matches(spec, n, v))
    };
    specs
        .iter()
        .filter(|spec| holds(lock, spec) && !holds(graph, spec))
        .cloned()
        .collect()
}

/// The `-p` specs of `specs` this host cannot build ([`Foreign`]), each with its consumers. An
/// unanswered graph read refuses nothing: cargo then judges the specs itself.
#[must_use]
pub fn foreign_specs(
    root: &Path,
    tools: &Toolchain,
    path_env: &OsStr,
    specs: &[String],
) -> Vec<Foreign> {
    let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return Vec::new();
    };
    let lock = lock_packages(&lock);
    if !specs
        .iter()
        .any(|spec| lock.iter().any(|(n, v)| spec_matches(spec, n, v)))
    {
        return Vec::new();
    }
    let tree = |args: &[&str]| targo_tree(root, &tools.targo, path_env, args);
    let Some(graph) = tree(&[
        "tree",
        "--workspace",
        "--prefix",
        "none",
        "--format",
        "{p}",
        "--edges",
        "normal,build,dev",
    ]) else {
        return Vec::new();
    };
    outside_graph(specs, &lock, &parse_tree_packages(&graph))
        .into_iter()
        .map(|spec| {
            let consumers = tree(&[
                "tree",
                "--workspace",
                "--target",
                "all",
                "--invert",
                &spec,
                "--depth",
                "1",
                "--prefix",
                "depth",
                "--format",
                "{p}",
                "--edges",
                "normal,build,dev",
            ])
            .map(|out| direct_dependents(&out))
            .unwrap_or_default();
            Foreign { spec, consumers }
        })
        .collect()
}

/// The names at depth 1 of a `targo tree --invert --prefix depth` listing: what depends on its
/// root directly.
#[must_use]
pub fn direct_dependents(text: &str) -> Vec<String> {
    let mut names: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix('1'))
        .filter(|l| !l.starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The refusal of a run whose `-p` names [`Foreign`] specs: one line naming each, who builds it,
/// and the command that does (`verify cells` with those specs: a bare `verify cells` takes the
/// change's scope, which need not hold them).
#[must_use]
pub fn foreign_refusal(foreign: &[Foreign]) -> String {
    let each: Vec<String> = foreign
        .iter()
        .map(|f| {
            let by = match f.consumers.as_slice() {
                [] => String::new(),
                all => format!(" ({} builds it for another OS's target)", all.join(", ")),
            };
            format!("-p {}{by}", f.spec)
        })
        .collect();
    let (them, it) = if foreign.len() == 1 {
        ("a package", "it")
    } else {
        ("packages", "them")
    };
    let cells: String = foreign.iter().map(|f| format!(" -p {}", f.spec)).collect();
    format!(
        "verify: {} names {them} this host's build graph does not hold, so no lint, test, doc or \
         checkers run here can build {it} (cargo panics resolving {it}); the cells build {it} on \
         the targets that hold {it}: targo --unverified verify cells{cells}. No check was run",
        each.join(" and ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `-p` naming a package of the lock that the host's build graph lacks is named, with who
    /// builds it; one the graph holds, one the lock lacks (a typo cargo names), a partial
    /// version and a path spec read as cargo reads them.
    #[test]
    fn a_spec_outside_the_hosts_graph_is_named_before_cargo_panics_on_it() {
        let lock = lock_packages(
            "version = 4\n\n[[package]]\nname = \"windows-sys\"\nversion = \"0.52.0\"\n\n\
             [[package]]\nname = \"windows-sys\"\nversion = \"0.61.2\"\ndependencies = [\n \
             \"windows-link\",\n]\n\n[[package]]\nname = \"aterm-core\"\nversion = \"0.148.0\"\n",
        );
        assert_eq!(lock.len(), 3, "{lock:?}");
        let graph = parse_tree_packages(
            "aterm-core v0.148.0 (/r/crates/aterm-core)\n\
             windows-sys v0.52.0 (/r/crates/aterm-windows-sys-052)\n\
             windows-sys v0.52.0 (/r/crates/aterm-windows-sys-052) (*)\n",
        );
        let specs = |s: &[&str]| s.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        assert_eq!(
            outside_graph(
                &specs(&[
                    "windows-sys@0.61.2",
                    "windows-sys@0.61",
                    "path+file:///r/crates/aterm-windows-sys#windows-sys@0.61.2",
                    "windows-sys@0.52.0",
                    "aterm-core",
                    "aterm-nonesuch",
                    "windows-sys@0.6",
                ]),
                &lock,
                &graph
            ),
            specs(&[
                "windows-sys@0.61.2",
                "windows-sys@0.61",
                "path+file:///r/crates/aterm-windows-sys#windows-sys@0.61.2",
            ]),
        );
        let held = parse_tree_packages(
            "aterm-core v0.148.0 (/r/crates/aterm-core)\n\
             windows-sys v0.61.2 (/r/crates/aterm-windows-sys)\n",
        );
        assert!(
            outside_graph(&specs(&["windows-sys@0.61.2"]), &lock, &held).is_empty(),
            "negative control: a graph that holds it refuses nothing"
        );

        let consumers = direct_dependents(
            "0windows-sys v0.61.2 (/r/crates/aterm-windows-sys)\n\
             1aterm-window v0.30.13 (/r/crates/aterm-window)\n\
             2aterm-gui v0.148.0 (/r/crates/aterm-gui)\n\
             1aterm-window v0.30.13 (/r/crates/aterm-window) (*)\n",
        );
        assert_eq!(consumers, ["aterm-window"]);
        let line = foreign_refusal(&[Foreign {
            spec: "windows-sys@0.61.2".into(),
            consumers,
        }]);
        for named in [
            "-p windows-sys@0.61.2",
            "aterm-window",
            "verify cells -p windows-sys@0.61.2.",
        ] {
            assert!(line.contains(named), "{named}: {line}");
        }
    }

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
            &|name| dependents(name).map(linking),
            &|_| replan_everything(),
        )
    }

    fn linking(links: Vec<String>) -> Reach {
        Reach {
            links,
            dev_only: Vec::new(),
        }
    }

    const MANIFEST: &str = r#"# The workspace.
[workspace]
members = ["crates/*"]
default-members = ["crates/a"]

[workspace.package]
version = "0.1.0"
edition = "2024"

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
        assert_eq!(
            edit(r#"version = "0.1.0""#, r#"version = "0.2.0""#),
            packages(&["aterm"]),
            "a bump reaches the one package that reads the product version"
        );
        for (from, to) in [
            ("libc = { path", "libm = { path"),
            (r#"edition = "2024""#, r#"edition = "2021""#),
            (
                r#"version = "0.1.0""#,
                "version = \"0.2.0\"\nrust-version = \"1.90\"",
            ),
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
                &|name| graph(edges)(name).map(linking),
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

    /// A cone's lint compiles every target of a crate whose own files changed and of one a changed
    /// file is tested by. CONTROL: a seed the change reached any other way (a lock row, a root
    /// manifest row, a fork) can move code under any member's tests, so the whole cone keeps its
    /// test targets, and a root manifest row does so whichever files changed beside it.
    #[test]
    fn only_what_the_change_touched_has_all_its_targets_linted() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-update", "aterm-release"]);
        let edges: &'static [(&str, &str)] = &[("ring", "aterm-grid"), ("aterm-grid", "aterm-gui")];
        let forks = || Some(vec![("ring".to_string(), "vendor/ring".to_string())]);
        let all_targets = |changed: &[&str], replan: Replan| match super::select(
            &paths(changed),
            &manifests(),
            &m,
            &forks,
            &|name| graph(edges)(name).map(linking),
            &|_| replan.clone(),
        ) {
            Selection::Narrowed { all_targets, .. } => all_targets,
            Selection::Widened(why) => panic!("widened unexpectedly: {why}"),
        };
        assert_eq!(
            all_targets(&["crates/aterm-grid/src/lib.rs"], Replan::Nothing),
            ["aterm-grid"],
            "its dependent aterm-gui is in the cone for its default targets"
        );
        assert_eq!(
            all_targets(&["publish/config.sh"], Replan::Nothing),
            ["aterm-release"]
        );
        assert_eq!(
            all_targets(
                &["crates/aterm-grid/src/lib.rs", "publish/config.sh"],
                Replan::Nothing
            ),
            ["aterm-grid", "aterm-release"]
        );
        let cone_of_ring = ["aterm-grid", "aterm-gui"];
        let lock_row = Replan::Packages(BTreeSet::from(["ring".to_string()]));
        assert_eq!(all_targets(&["Cargo.lock"], lock_row.clone()), cone_of_ring);
        assert_eq!(
            all_targets(&["Cargo.toml"], lock_row),
            cone_of_ring,
            "a [workspace.dependencies] row"
        );
        assert_eq!(
            all_targets(&["vendor/ring/src/lib.rs"], Replan::Nothing),
            cone_of_ring
        );
        assert_eq!(
            all_targets(
                &["Cargo.toml"],
                Replan::Packages(BTreeSet::from(["aterm-grid".to_string()]))
            ),
            cone_of_ring,
            "a row naming a member changes how its dependents build it"
        );
        assert_eq!(
            all_targets(
                &["Cargo.toml", "crates/aterm-grid/src/lib.rs"],
                Replan::Packages(BTreeSet::from(["aterm-grid".to_string()]))
            ),
            cone_of_ring,
            "and still does when that member's own files changed with it"
        );
        assert_eq!(
            all_targets(
                &["crates/aterm-grid/Cargo.toml", "Cargo.lock"],
                Replan::Packages(BTreeSet::from(["aterm-grid".to_string()]))
            ),
            ["aterm-grid"],
            "a member's own lock entry, from its own manifest, reaches no one else's tests"
        );
    }

    /// A crate the cone holds only by a dev edge leaves the lint, because its library and binaries
    /// link nothing that changed, and stays in the test compile, which takes its tests. One a seed
    /// also links keeps its default targets, and one a changed file is tested by keeps all its
    /// targets. CONTROL: a fork keeps the whole cone in the first pass, those crates included.
    #[test]
    fn a_crate_the_cone_holds_only_by_a_dev_edge_leaves_the_lint() {
        let m = members(&["seed", "seed-two", "normal", "dev", "both", "aterm-update"]);
        let reach = |name: &str| {
            let (links, dev_only): (&[&str], &[&str]) = match name {
                "seed" => (&["seed", "normal"], &["dev", "both", "aterm-update"]),
                "ring" => (&["normal"], &["dev"]),
                _ => (&["seed-two", "both"], &[]),
            };
            let owned = |names: &[&str]| names.iter().map(ToString::to_string).collect();
            Some(Reach {
                links: owned(links),
                dev_only: owned(dev_only),
            })
        };
        let forks = || Some(vec![("ring".to_string(), "vendor/ring".to_string())]);
        let pick = |changed: &[&str]| {
            let manifests: Manifests = ["crates/seed", "crates/seed-two"].into_iter().collect();
            super::select(&paths(changed), &manifests, &m, &forks, &reach, &|_| {
                Replan::Nothing
            })
        };
        let touched = [
            "crates/seed/src/lib.rs",
            "crates/seed-two/src/lib.rs",
            "tools/install.sh",
        ];
        let selection = pick(&touched);
        let Selection::Narrowed {
            crates,
            all_targets,
            unlinted,
            ..
        } = &selection
        else {
            panic!("widened unexpectedly: {selection:?}");
        };
        let cone_of_all = ["aterm-update", "both", "dev", "normal", "seed", "seed-two"];
        assert_eq!(crates, &cone_of_all);
        assert_eq!(all_targets, &["aterm-update", "seed", "seed-two"]);
        assert_eq!(unlinted, &["dev"]);
        let (scope, _) = cone("main", &selection);
        let linted = crate::stages::tippy_passes(&scope)
            .into_iter()
            .flat_map(|pass| pass.args)
            .collect::<Vec<_>>();
        assert!(
            !linted.iter().any(|arg| arg == "dev"),
            "the lint leaves it out: {linted:?}"
        );
        assert!(
            crate::stages::test_compile_args(&scope)
                .iter()
                .any(|arg| arg == "dev"),
            "the test compile keeps it"
        );
        let Selection::Narrowed {
            all_targets,
            unlinted,
            ..
        } = pick(&["crates/seed/src/lib.rs", "vendor/ring/src/lib.rs"])
        else {
            panic!("widened unexpectedly");
        };
        assert_eq!(
            all_targets,
            ["aterm-update", "both", "dev", "normal", "seed"]
        );
        assert!(unlinted.is_empty(), "{unlinted:?}");
    }

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

    fn manifests() -> Manifests {
        [
            "crates/aterm-grid",
            "crates/aterm-gui",
            "crates/aterm-render",
            "crates/aterm-render/tests/font_oracle/recorder",
            "crates/aterm-scrollback",
            "crates/aterm-lab",
            "libc-oracle",
        ]
        .into_iter()
        .collect()
    }

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn no_packages() -> Option<Vec<(String, String)>> {
        Some(Vec::new())
    }

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

    #[test]
    fn a_path_is_owned_by_the_member_whose_manifest_is_nearest() {
        let m = Members::new(
            [
                "aterm-grid",
                "aterm-gui",
                "aterm-render",
                "aterm-scrollback",
                "aterm-lab",
            ]
            .into_iter()
            .map(|name| Member {
                name: name.to_string(),
                dir: format!("crates/{name}"),
                has_lib: true,
            })
            .chain([Member {
                name: "windows-sys".to_string(),
                dir: "crates/aterm-windows-sys-052".to_string(),
                has_lib: true,
            }])
            .collect(),
        );
        let mut mf = manifests();
        mf.insert("");
        mf.insert("crates/aterm-windows-sys-052");
        mf.insert("crates/aterm-windows-sys");
        for (path, want) in [
            ("crates/aterm-grid/src/lib.rs", Some("aterm-grid")),
            ("crates/aterm-grid/tests/deep/nested.rs", Some("aterm-grid")),
            ("crates/aterm-grid/Cargo.toml", Some("aterm-grid")),
            (
                "crates/aterm-scrollback/src/lib.rs",
                Some("aterm-scrollback"),
            ),
            ("crates/aterm-render/src/lib.rs", Some("aterm-render")),
            ("crates/aterm-render/SPECS.md", None),
            ("crates/aterm-render/README.md", Some("aterm-render")),
            ("crates/aterm-render/src/SPECS.md", Some("aterm-render")),
            (
                "crates/aterm-render/tests/font_oracle/recorder/src/main.rs",
                None,
            ),
            ("libc-oracle/src/lib.rs", None),
            ("docs/PROCESS.md", None),
            ("tools/install.sh", None),
            ("README.md", None),
            ("AGENTS.md", None),
            (
                "crates/aterm-windows-sys-052/src/lib.rs",
                Some("windows-sys"),
            ),
            ("crates/aterm-windows-sys/src/lib.rs", None),
        ] {
            assert_eq!(owning_member(path, &mf, &m), want, "{path}");
        }
    }

    #[test]
    fn the_cone_is_the_seeds_and_what_builds_on_them() {
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
            pick(&["crates/aterm-grid/src/lib.rs"], &|_| {
                Some(vec!["aterm-gui".into(), "some-registry-crate".into()])
            })
            .1,
            ["aterm-grid", "aterm-gui"],
            "a changed crate stays in the cone though the answer leaves it out"
        );
        let (seeds, crates) = pick(&["docs/PROCESS.md", "tools/install.sh"], &graph(EDGES));
        assert!(seeds.is_empty() && crates.is_empty());
    }

    /// The graph the cone closes over, read from a real workspace. A member that names a crate only
    /// among its dev-dependencies (as aterm-models names every anchored crate, for its source-spec
    /// closure test) reaches it, and so does one that dev-depends on what links it normally. A dev
    /// edge is the final hop: nothing that depends on that member links the crate, so `downstream`
    /// and `tested_closure` stay out. A package only a dev edge reaches still reaches its dev
    /// dependent (`astream-term` and `aterm-conformance`), and a normal edge that only a
    /// dev-dependency's feature turns on still counts (`aterm-wasm-abi` on its macros). The whole
    /// fixture asks cargo for colour by its config, which the read must not obey. A package only
    /// another target's graph holds reads as reaching nothing here, so a change to its files widens,
    /// where a change to a member's narrows.
    #[test]
    fn a_dev_edge_is_the_final_hop_of_the_cone() {
        let made = crate::mktemp_dir("atv-changed-dev-edge").expect("mktemp");
        let root = std::fs::canonicalize(&made).expect("the fixture's own path");
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver='2'\nexclude=['vendored','offhost']\nmembers=['anchored','closure',\
             'downstream','tested_closure','linked','tested_linked','conformance','macros','abi',\
             'consumer','other']\n",
        )
        .expect("workspace manifest");
        std::fs::create_dir_all(root.join(".cargo")).expect("config dir");
        std::fs::write(root.join(".cargo/config.toml"), "[term]\ncolor='always'\n")
            .expect("config");
        for (name, deps) in [
            ("anchored", ""),
            (
                "closure",
                "[dev-dependencies]\nanchored={path='../anchored'}\n",
            ),
            (
                "downstream",
                "[dependencies]\nclosure={path='../closure'}\n",
            ),
            (
                "tested_closure",
                "[dev-dependencies]\nclosure={path='../closure'}\n",
            ),
            ("linked", "[dependencies]\nanchored={path='../anchored'}\n"),
            (
                "tested_linked",
                "[dev-dependencies]\nlinked={path='../linked'}\n",
            ),
            ("vendored", ""),
            (
                "conformance",
                "[dev-dependencies]\nvendored={path='../vendored'}\n",
            ),
            ("macros", ""),
            (
                "abi",
                "[dependencies]\nmacros={path='../macros',optional=true}\n\
                 [features]\nexport=['dep:macros']\n\
                 [dev-dependencies]\nabi={path='.',features=['export']}\n",
            ),
            ("consumer", "[dependencies]\nabi={path='../abi'}\n"),
            ("offhost", ""),
            (
                "other",
                "[target.'cfg(target_os=\"hermit\")'.dependencies]\noffhost={path='../offhost'}\n",
            ),
        ] {
            let package = root.join(name);
            std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
            std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
            std::fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
            )
            .expect("package manifest");
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let m = Members::new(
            [
                "anchored",
                "closure",
                "downstream",
                "tested_closure",
                "linked",
                "tested_linked",
                "conformance",
                "macros",
                "abi",
                "consumer",
                "other",
            ]
            .map(|name| Member {
                name: name.to_string(),
                dir: name.to_string(),
                has_lib: true,
            })
            .to_vec(),
        );
        let found = |of: &str| {
            invert(&root, Path::new(env!("CARGO")), &path, of, &m)
                .expect("the inverted graph reads")
        };
        let owned = |names: &[&str]| -> Vec<String> {
            names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>()
        };
        let reach = |links: &[&str], dev_only: &[&str]| Reach {
            links: owned(links),
            dev_only: owned(dev_only),
        };
        let (anchored, vendored, macros) = (found("anchored"), found("vendored"), found("macros"));
        let offhost = found("offhost");
        let unknown = invert(
            &root,
            Path::new(env!("CARGO")),
            &path,
            "no-such-package",
            &m,
        );
        let after_change_to = committed_fixture(&root);
        let member_file = after_change_to("anchored/src/lib.rs");
        let offhost_file = after_change_to("offhost/src/lib.rs");
        let _ = std::fs::remove_dir_all(&made);
        assert_eq!(
            anchored,
            reach(&["anchored", "linked"], &["closure", "tested_linked"])
        );
        assert_eq!(vendored, reach(&[], &["conformance"]));
        assert_eq!(macros, reach(&["abi", "consumer", "macros"], &[]));
        assert_eq!(
            offhost,
            Reach::default(),
            "this host's graph does not hold it"
        );
        assert_eq!(unknown, None, "a spec the graph cannot answer widens");
        assert!(
            matches!(&member_file, Selection::Narrowed { seeds, .. } if seeds == &["anchored"]),
            "{member_file:?}"
        );
        assert!(
            matches!(&offhost_file, Selection::Widened(why) if why.contains("offhost")),
            "{offhost_file:?}"
        );
    }

    /// `root`, whose lock cargo has written, committed as the branch `base`: what [`resolve`] selects
    /// on it, run with this test's cargo, after a change to one file.
    fn committed_fixture(root: &Path) -> impl Fn(&str) -> Selection + '_ {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let git = {
            let path = path.clone();
            move |args: &[&str]| {
                let out = git(root, &path)
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args)
                    .output()
                    .expect("git runs");
                assert!(out.status.success(), "git {args:?}: {out:?}");
            }
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        git(&["branch", "base"]);
        let mut tools = Toolchain::discover(
            Some(Path::new("/nonexistent")),
            Path::new("/nonexistent"),
            OsStr::new(""),
            None,
            None,
        );
        tools.targo = env!("CARGO").into();
        tools.refused = None;
        move |file: &str| {
            std::fs::write(root.join(file), "pub fn changed() {}\n").expect("the change");
            let selection = resolve(root, &tools, &path, "base");
            git(&["checkout", "-q", "--", "."]);
            selection
        }
    }

    /// A package outside the workspace can have a member's name, as `crates/aterm-windows-sys` has
    /// `crates/aterm-windows-sys-052`'s, so a directory says which is which. A change to the one
    /// outside is a seed of its own, named with its version since its bare name is the member's: it
    /// widens where only another target's graph holds it, and where this host's does, it selects
    /// what depends on it and not the member. CONTROL: a change to the member narrows to it and what
    /// builds on it, and a version the read cannot follow widens.
    #[test]
    fn a_same_named_package_outside_the_workspace_is_no_member() {
        let made = crate::mktemp_dir("atv-changed-same-name").expect("mktemp");
        let root = std::fs::canonicalize(&made).expect("the fixture's own path");
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver='2'\nexclude=['crates/shared','crates/held']\n\
             members=['crates/shared-052','crates/user','crates/far','crates/held-052',\
             'crates/near']\n",
        )
        .expect("workspace manifest");
        for (dir, name, version, rest) in [
            ("shared-052", "shared", "0.52.0", ""),
            ("shared", "shared", "0.61.2", "[workspace]\n"),
            (
                "user",
                "user",
                "1.0.0",
                "[dependencies]\nshared={path='../shared-052'}\n",
            ),
            (
                "far",
                "far",
                "1.0.0",
                "[target.'cfg(target_os=\"hermit\")'.dependencies]\n\
                 shared={path='../shared'}\n",
            ),
            ("held-052", "held", "0.52.0", ""),
            ("held", "held", "0.61.2", "[workspace]\n"),
            (
                "near",
                "near",
                "1.0.0",
                "[dependencies]\nheld={path='../held'}\n",
            ),
        ] {
            let package = root.join("crates").join(dir);
            std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
            std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
            std::fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='{version}'\nedition='2021'\n{rest}"),
            )
            .expect("package manifest");
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        targo_tree(
            &root,
            Path::new(env!("CARGO")),
            &path,
            &["tree", "--workspace"],
        )
        .expect("cargo writes the lock");
        let after_change_to = committed_fixture(&root);
        let member = after_change_to("crates/shared-052/src/lib.rs");
        let off_host = after_change_to("crates/shared/src/lib.rs");
        let on_host = after_change_to("crates/held/src/lib.rs");
        let _ = std::fs::remove_dir_all(&made);
        assert_eq!(
            narrowed(&member),
            (paths(&["shared"]), paths(&["shared@0.52.0", "user"]))
        );
        assert!(
            matches!(&off_host, Selection::Widened(why) if why.contains("shared@0.61.2")),
            "{off_host:?}"
        );
        assert_eq!(
            narrowed(&on_host),
            (paths(&["held@0.61.2"]), paths(&["near"]))
        );
        let m = Members::new(vec![Member {
            name: "shared".into(),
            dir: "crates/shared-052".into(),
            has_lib: true,
        }]);
        assert_eq!(
            outside_packages(
                "shared v0.52.0 (/r/crates/shared-052)\nshared v0.61.2 (/r/crates/shared)\n",
                "/r/",
                &m,
            ),
            Some(vec![(
                "shared@0.61.2".to_string(),
                "crates/shared".to_string()
            )])
        );
        assert_eq!(
            outside_packages("shared 0.61.2 (/r/crates/shared)\n", "/r/", &m),
            None
        );
    }

    /// The tree's prefixes say what depends on what, so a line that reads as neither a section
    /// header nor a package one level under what it follows fails the whole read, which widens:
    /// parsing past it would narrow the cone to whatever did parse. An ANSI-coloured tree (these
    /// lines are cargo's under `CARGO_TERM_COLOR=always`) would otherwise read as one root per line.
    /// An empty printout is cargo's `nothing to print`, which [`select`] widens on instead. CONTROL:
    /// the same tree in plain text reads.
    #[test]
    fn a_tree_read_it_cannot_follow_fails_rather_than_narrowing() {
        let m = Members::new(
            ["x", "a", "b", "c", "d", "e"]
                .map(|name| Member {
                    name: name.to_string(),
                    dir: name.to_string(),
                    has_lib: true,
                })
                .to_vec(),
        );
        let read = |tree: &str| reached(tree, "/r/", &m);
        let plain = "x v1.0.0 (/r/x)\n\
                     |-- a v1.0.0 (/r/a)\n\
                     |   [dev-dependencies]\n\
                     |   `-- b v1.0.0 (/r/b)\n\
                     `-- c v1.0.0 (proc-macro) (/r/c) (*)\n\
                     [dev-dependencies]\n\
                     `-- d v1.0.0 (/r/d)\n\
                     \x20   `-- e v1.0.0 (/r/e)\n\
                     \n\
                     registry v2.0.0\n";
        assert_eq!(
            read(plain),
            Some(Reach {
                links: ["a", "c", "x"].map(String::from).to_vec(),
                dev_only: ["b", "d"].map(String::from).to_vec(),
            })
        );
        let coloured = "x v1.0.0 (/r/x)\n\
                        \x1b[2m|--\x1b[0m a v1.0.0 (/r/a)\n\
                        \x1b[2m|\x1b[0m   \x1b[96m\x1b[1m[dev-dependencies]\x1b[39m\x1b[22m\n\
                        \x1b[2m|\x1b[0m   \x1b[1m\x1b[36m`--\x1b[0m b v1.0.0 (/r/b)\n";
        assert_eq!(read(coloured), None);
        assert_eq!(
            read("x v1.0.0 (/r/x)\n\x1b[2m|--\x1b[0m a v1.0.0 (/r/a)\n"),
            None,
            "a prefix alone"
        );
        for misread in [
            "x v1.0.0 (/r/x)\nwarning: not a package\n",
            "x v1.0.0 (/r/x)\n|--\n",
            "x v1.0.0 (/r/x)\n|       `-- deep v1.0.0 (/r/d)\n",
            "x v1.0.0 (/r/x)\n  a v1.0.0 (/r/a)\n",
            "x v1.0.0 (/r/x)\n[unknown-dependencies]\n",
            "[dev-dependencies]\n",
            "|   [dev-dependencies]\n",
        ] {
            assert_eq!(read(misread), None, "{misread:?}");
        }
        assert_eq!(read(""), Some(Reach::default()));
        assert_eq!(read("\n"), Some(Reach::default()));
    }

    /// A seed whose answer holds no member says this host's graph does not hold it, so the seed is
    /// not read as selecting nothing. A lock row off this host's graph is the real case.
    #[test]
    fn a_seed_this_hosts_graph_does_not_hold_widens() {
        let m = members(&["aterm-grid", "aterm-gui"]);
        let row = Replan::Packages(BTreeSet::from(["offhost".to_string()]));
        let pick = |dependents: &dyn Fn(&str) -> Option<Vec<String>>| {
            super::select(
                &paths(&["Cargo.lock"]),
                &manifests(),
                &m,
                &no_packages,
                &|name| dependents(name).map(linking),
                &|_| row.clone(),
            )
        };
        for answer in [vec![], vec!["offhost".to_string()]] {
            let sel = pick(&|_| Some(answer.clone()));
            assert!(
                matches!(&sel, Selection::Widened(why) if why.contains("offhost")),
                "{answer:?}: {sel:?}"
            );
            assert!(cone("main", &sel).0.is_workspace());
        }
        assert_eq!(
            narrowed(&pick(&|_| Some(vec!["aterm-gui".to_string()]))),
            (vec!["offhost".to_string()], vec!["aterm-gui".to_string()]),
            "one that reaches a member narrows"
        );
    }

    /// clippy reads the nearest config above a manifest, so a root one is every other member's
    /// lint, and no member owns it: it widens, through the replan a checkout answers. CONTROL: a
    /// member's own config is that member's, and a rustfmt config selects nothing, because no
    /// compile reads it and `fmt` takes the whole tree whatever the selection.
    #[test]
    fn a_root_lint_config_widens_and_a_members_own_selects_that_member() {
        let m = members(&["aterm-grid", "aterm-link"]);
        let pick = |changed: &str| {
            super::select(
                &paths(&[changed]),
                &manifests(),
                &m,
                &no_packages,
                &|name| graph(EDGES)(name).map(linking),
                &|p| replan_of(Path::new("/no-such-checkout"), OsStr::new(""), "HEAD", p),
            )
        };
        for config in ["clippy.toml", ".clippy.toml", "rust-toolchain"] {
            assert_eq!(
                pick(config),
                Selection::Widened(format!(
                    "{config} changed: it can change how every crate is built or linted"
                ))
            );
        }
        assert_eq!(
            narrowed(&pick("crates/aterm-grid/clippy.toml")).0,
            ["aterm-grid"]
        );
        for formatting in ["rustfmt.toml", ".rustfmt.toml"] {
            let (seeds, crates) = narrowed(&pick(formatting));
            assert!(seeds.is_empty() && crates.is_empty(), "{formatting}");
        }
        assert!(!crate::checks::Check::Fmt.reads(&Scope::changed("main", Vec::new(), false)));
    }

    /// A lock move compiles every member with another compiler: a change to the root
    /// `trust.lock` replans everything, as `rust-toolchain.toml` does, and a file of that name
    /// below the root, which verify's discovery (from the root) never reads, does not.
    #[test]
    fn a_trust_lock_change_replans_everything() {
        assert!(replans_everything("trust.lock"));
        assert!(!replans_everything("crates/x/trust.lock"));
    }

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
        let m = members(&["aterm-update", "aterm-bootstrap-verify", "aterm-gui"]);
        let sel = select(
            &paths(&["tools/install.sh"]),
            &manifests(),
            &m,
            &no_packages,
            &graph(edges),
        );
        assert_eq!(
            narrowed(&sel),
            (
                vec![],
                vec![
                    "aterm-bootstrap-verify".to_string(),
                    "aterm-update".to_string()
                ]
            ),
            "both of install.sh's suites"
        );
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
            tested_by("publishing/x").next(),
            None,
            "a sibling sharing the prefix"
        );
        assert_eq!(
            tested_by("tools/install.sh").collect::<Vec<_>>(),
            ["aterm-update", "aterm-bootstrap-verify"]
        );
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
        assert!(
            cone("origin/main", &bump).0.touches("astream-cap"),
            "and is still what the change touched"
        );
        assert!(!cone("origin/main", &bump).0.touches("aterm-link"));
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
        write(
            "crates/f/tests/pin.rs",
            "include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../a/assets/font.bin\"));",
        );
        write(
            "crates/g/tests/pin.rs",
            "format!(\"{}/../a/assets/font.bin\", env!(\"CARGO_MANIFEST_DIR\"));",
        );
        let status = git(&repo, &path_env).args(["init", "-q"]).status().unwrap();
        assert!(status.success());
        let m = Members::new(
            ["a", "b", "c", "d", "e", "f", "g"]
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
                "crates/e/src/lib.rs",
                "crates/f/tests/pin.rs",
                "crates/g/tests/pin.rs"
            ])
        );
        let docs = paths(&["docs/PROCESS.md"]);
        assert_eq!(
            with_shared_inputs(&repo, &path_env, docs.clone(), &m).unwrap(),
            docs
        );
        std::fs::remove_dir_all(repo).unwrap();
    }

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

    /// A literal that continues the manifest directory, by `concat!` or a format argument, names
    /// what the same path spelled relatively names, and starts at the member and never at the
    /// source's folder. It names a folder as any literal does, by a trailing `/`. CONTROL: another
    /// file, a path that stays in the member, and a `{}` that does not lead a path segment name
    /// nothing.
    #[test]
    fn a_literal_that_continues_the_manifest_dir_starts_at_the_member() {
        let root = Path::new("/repo");
        let member = root.join("crates/a");
        let test = member.join("tests/pin.rs");
        let lib = member.join("src/lib.rs");
        let changed = [root.join("crates/b/x")];
        for text in [
            "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../b/x\"));",
            "format!(\"{}/../b/x\", env!(\"CARGO_MANIFEST_DIR\"));",
            "format!(\"{dir}/../b/x\");",
            "format!(\"{dir:?}/../b/x\");",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../b/\", \"x\");",
            "format!(\"{}/../b/\", dir);",
        ] {
            for source in [&test, &lib] {
                assert!(
                    shares_changed_input(source, text, &member, &changed),
                    "{text} in {}",
                    source.display()
                );
            }
        }

        let continued = "concat!(dir, \"/../../b/x\");";
        let relative = "include_str!(\"../../b/x\");";
        let above = [root.join("b/x")];
        assert!(shares_changed_input(&lib, relative, &member, &changed));
        assert!(shares_changed_input(&lib, relative, &member, &above));
        assert!(!shares_changed_input(&lib, continued, &member, &changed));
        assert!(shares_changed_input(&lib, continued, &member, &above));

        let under_a_folder = [root.join("crates/b/steps/one.jsonl")];
        let folder = "concat!(dir, \"/../b/steps/\");";
        let no_slash = "concat!(dir, \"/../b/steps\");";
        assert!(shares_changed_input(&lib, folder, &member, &under_a_folder));
        assert!(!shares_changed_input(
            &lib,
            no_slash,
            &member,
            &under_a_folder
        ));

        for text in [
            "concat!(dir, \"/../c/x\");",
            "concat!(dir, \"/x/../b\");",
            "format!(\"{}/x/../b\", dir);",
            "format!(\"{}../b/x\", dir);",
            "format!(\"{{}}/../b/x\");",
        ] {
            assert!(
                !shares_changed_input(&test, text, &member, &changed),
                "{text}"
            );
        }
    }

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
            "concat!(dir, \"/../\");",
            "concat!(dir, \"/../../\");",
            "format!(\"{}/../\", dir);",
            "format!(\"{dir}/../../\");",
        ] {
            assert!(
                !shares_changed_input(&lib, text, &member, &changed),
                "{text}"
            );
        }
        let named = "let y = dir.join(\"../y/\");";
        assert!(shares_changed_input(&lib, named, &member, &changed));
    }

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
            &["crates/rustix"].into_iter().collect(),
            &m,
            &no_packages,
            &graph(edges),
        );
        let (seeds, crates) = narrowed(&sel);
        assert_eq!(seeds, ["rustix"]);
        assert_eq!(crates, ["aterm-gui", "rustix@0.38.44"]);
    }

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

    /// The root prefix a Windows checkout's `cargo tree` lines start with, from each spelling the
    /// root reaches it in: git's `C:/…`, `fs::canonicalize`'s `\\?\C:\…` and `\\?\UNC\…`, and the
    /// plain one. NEGATIVE CONTROL: git's `C:/…` root with only the separator appended, the prefix
    /// this replaced, reads no package, and the cone then widens to the whole workspace.
    #[test]
    fn a_windows_root_is_spelled_as_cargo_tree_prints_it() {
        let tree =
            "a v1.0.0 (C:\\w\\crates\\a)\nb v1.0.0 (\\\\srv\\share\\w\\b) (*)\nserde v1.0.0\n";
        for root in ["C:/w", "C:\\w", "\\\\?\\C:\\w", "C:\\w\\"] {
            assert_eq!(
                parse_graph_entries(tree, &tree_prefix(root, true)),
                [("a".into(), "crates\\a".into())],
                "{root}"
            );
        }
        assert_eq!(
            parse_graph_entries(tree, &tree_prefix("\\\\?\\UNC\\srv\\share\\w", true)),
            [("b".into(), "b".into())]
        );
        assert!(parse_graph_entries(tree, "C:/w\\").is_empty());
        assert_eq!(tree_prefix("/repo (copy)", false), "/repo (copy)/");
        assert_eq!(tree_prefix("/", false), "/");
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
        assert_eq!(
            manifest_package_name(
                "[package.metadata.docs]\nname = \"wrong\"\n[package] # comment\nname = 'single'\n"
            )
            .as_deref(),
            Some("single")
        );
        assert!(declares_lib_target(
            "  [lib] # comment\npath = \"src/lib.rs\"\n"
        ));
        assert!(!declares_lib_target("[lib-extra]\nname = \"x\"\n"));
    }
}
