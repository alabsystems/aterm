// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Change-cone selection for `targo --unverified verify`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::scope::Scope;
use crate::toolchain::Toolchain;

mod exclusions;

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

    /// Every workspace member as the exact sorted `-p` spec Cargo accepts. A package whose name
    /// is shared with another locked package retains its `name@version` disambiguator.
    pub(crate) fn specs(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|member| self.spec(&member.name).to_owned())
            .collect()
    }

    /// Each workspace member as the exact `-p` spec Cargo accepts and its repo-relative
    /// directory. Shared-name packages retain their versioned spec.
    pub(crate) fn lint_packages(&self) -> Vec<(String, String)> {
        self.0
            .iter()
            .map(|member| (self.spec(&member.name).to_owned(), member.dir.clone()))
            .collect()
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

    /// The repo-relative directory of the member `name`.
    #[must_use]
    pub fn dir(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.dir.as_str())
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
        /// The members, by name, one edge from a seed, normal, build or dev ([`Reach::direct`]):
        /// those whose code can name what the change touched.
        direct_dependents: Vec<String>,
        /// The members among `seeds`, by name, whose every changed file is confined to the member
        /// itself ([`crate::own_tests`]: its test code, comments no macro reads, a manifest whose
        /// values are the same): no build of a dependent reads the change, so the cone holds
        /// none of theirs and judges them alone, with all of their own rows.
        alone: Vec<String>,
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
    /// The members one edge from the package, normal, build or dev: the code that can name its
    /// items. A target only a feature switches on (`required-features`) can name them where no
    /// other target of that member does ([`crate::scope::Changed::direct_dependents`]).
    pub direct: Vec<String>,
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
    let (Some(base), Some(work)) = (parse_toml(base), parse_toml(work)) else {
        return Replan::Everything("it does not parse here, or it was added or removed".into());
    };
    manifest_tables_replan(&base, &work)
}

fn manifest_tables_replan(base: &aterm_toml::Table, work: &aterm_toml::Table) -> Replan {
    use aterm_toml::Value;
    let mut seeds = BTreeSet::new();
    for key in changed_keys(base, work) {
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
/// spec table at a manifest's own root (`<dir>/SPECS.md`) is owned by none, as root Markdown and
/// `docs/**/*.md` are (`aterm_census::no_reach::class`): no build, include or test reads one,
/// which census's `no-reach-read` guard holds.
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

/// Select the workspace crates that the change cone must build and test. `confined` says whether a
/// member's changed files (the second argument) are all confined to the member itself
/// ([`crate::own_tests::confined`]); such a member is judged alone ([`Selection::Narrowed`]'s
/// `alone`).
#[must_use]
pub fn select(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
    path_packages: &dyn Fn() -> Option<Vec<(String, String)>>,
    dependents: &dyn Fn(&str) -> Option<Reach>,
    replan: &dyn Fn(&str) -> Replan,
    confined: &dyn Fn(&str, &[&str]) -> bool,
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
    let forked = forks_touched(paths, &packages);
    // A member a lock or root manifest row also names, or a fork it is, builds differently for
    // every dependent whatever its files say.
    let alone: BTreeSet<String> = own
        .iter()
        .filter(|seed| !replanned.contains(*seed) && !forked.contains(*seed))
        .filter(|seed| {
            let owned: Vec<&str> = paths
                .iter()
                .filter(|p| owning_member(p, manifests, members) == Some(seed.as_str()))
                .map(String::as_str)
                .collect();
            confined(seed, &owned)
        })
        .cloned()
        .collect();
    let mut seeds = seeds;
    seeds.extend(replanned);
    seeds.extend(forked);
    let mut dev_only: BTreeSet<String> = BTreeSet::new();
    let mut direct_dependents: BTreeSet<String> = BTreeSet::new();
    for seed in seeds.iter().filter(|seed| !alone.contains(*seed)) {
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
        direct_dependents.extend(found.direct.into_iter().filter(|n| names.contains(n)));
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
        direct_dependents: direct_dependents.into_iter().collect(),
        alone: alone.into_iter().collect(),
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
            direct_dependents,
            alone,
        } => {
            let dependents = crates.iter().filter(|c| !seeds.contains(c)).count();
            let more = if dependents == 0 {
                String::new()
            } else {
                format!(" + {dependents} dependent(s)")
            };
            let named: Vec<String> = seeds
                .iter()
                .map(|seed| {
                    if alone.contains(seed) {
                        format!("{seed} (alone)")
                    } else {
                        seed.clone()
                    }
                })
                .collect();
            (
                Scope::changed(base, crates.clone(), *any_lib)
                    .with_seeds(seeds.clone())
                    .with_all_targets(all_targets.clone())
                    .with_unlinted(unlinted.clone())
                    .with_direct_dependents(direct_dependents.clone())
                    .with_alone(alone.clone()),
                format!("the change cone since {base}: {}{more}", named.join(" ")),
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
    resolve_paths(root, tools, path_env, &merge_base, paths, Some(&merge_base))
}

/// Resolve an explicit committed path selection through the ordinary dependency cone. `from` is
/// the tree every one of `paths` changed from, whose files [`crate::own_tests::confined`] compares
/// with the working tree's; with none (paths gathered from several commits), no member is judged
/// alone.
pub fn resolve_paths(
    root: &Path,
    tools: &Toolchain,
    path_env: &OsStr,
    merge_base: &str,
    paths: Vec<String>,
    from: Option<&str>,
) -> Selection {
    let members = read_members(root, tools, path_env);
    // Shared-input discovery can only add crates. Once a changed compiler/configuration
    // input already demands every crate, that scan cannot change the selection. Archive
    // merges can owe tens of thousands of paths: do not cross them with every source file
    // merely to rediscover the same workspace-wide obligation.
    for p in paths.iter().filter(|p| replans_everything(p)) {
        if let Replan::Everything(why) = replan_of(root, path_env, merge_base, p, &members) {
            return Selection::Widened(format!("{p} changed: {why}"));
        }
    }
    let listed = if paths.is_empty() {
        Vec::new()
    } else {
        match listed_files(root, path_env) {
            Ok(listed) => listed,
            Err(why) => return Selection::Widened(why),
        }
    };
    let changed: BTreeSet<String> = paths.iter().cloned().collect();
    let Shared {
        paths,
        readers,
        named,
    } = match with_shared_inputs(root, &listed, paths, &members) {
        Ok(shared) => shared,
        Err(why) => return Selection::Widened(why),
    };
    let manifests = manifests_near(root, &paths);
    let checkout = from.map(|base| crate::own_tests::Checkout {
        root,
        path_env,
        base,
        listed: &listed,
    });
    select(
        &paths,
        &manifests,
        &members,
        &|| read_path_packages(root, tools, path_env, &members),
        &|c| invert(root, &tools.targo, path_env, members.spec(c), &members),
        &|p| replan_of(root, path_env, merge_base, p, &members),
        // What another member reads, or a file that reads another's, is the cone's to follow.
        &|member, owned| {
            owned
                .iter()
                .all(|p| changed.contains(*p) && !readers.contains(*p) && !named.contains(*p))
                && match (&checkout, members.dir(member)) {
                    (Some(checkout), Some(dir)) => {
                        crate::own_tests::confined(checkout, dir, owned).is_ok()
                    }
                    _ => false,
                }
        },
    )
}

/// Every file of the checkout, tracked or not, by repo-relative path.
fn listed_files(root: &Path, path_env: &OsStr) -> Result<Vec<String>, String> {
    let out = git(root, path_env)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|e| format!("cannot list the checkout's files: {e}"))?;
    if !out.status.success() {
        return Err("git could not list the checkout's files".into());
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Replan for `path` since `merge_base` ([`manifest_replan`], [`lock_replan`]).
fn replan_of(
    root: &Path,
    path_env: &OsStr,
    merge_base: &str,
    path: &str,
    members: &Members,
) -> Replan {
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
        "Cargo.toml" => {
            let (base, work) = (read(), work());
            exclusions::replan(
                root,
                path_env,
                merge_base,
                base.as_deref(),
                work.as_deref(),
                members,
            )
            .unwrap_or_else(|| manifest_replan(base.as_deref(), work.as_deref()))
        }
        "Cargo.lock" => lock_replan(read().as_deref(), work().as_deref()),
        _ => replan_everything(),
    }
}

/// A change with what other members read of it ([`with_shared_inputs`]).
struct Shared {
    /// The changed paths and every member source that reads one of them from outside its member.
    paths: Vec<String>,
    /// Every member source that reads one of `paths` from outside its member by a `../` literal
    /// ([`shares_changed_input`]), whether it changed itself or not.
    readers: BTreeSet<String>,
    /// The changed paths whose repo-relative path a source of another member spells.
    named: BTreeSet<String>,
}

/// `paths` and every member source, of the checkout's `listed` files, that reads one of them from
/// outside its member by a `../` literal ([`shares_changed_input`]), with those readers and the
/// paths another member's source names by their repo-relative path.
fn with_shared_inputs(
    root: &Path,
    listed: &[String],
    mut paths: Vec<String>,
    members: &Members,
) -> Result<Shared, String> {
    let mut readers = BTreeSet::new();
    let mut named = BTreeSet::new();
    if paths.is_empty() {
        return Ok(Shared {
            paths,
            readers,
            named,
        });
    }
    let owner = |path: &str| {
        members
            .0
            .iter()
            .find(|m| {
                path.strip_prefix(&m.dir)
                    .is_some_and(|p| p.starts_with('/'))
            })
            .map(|m| m.dir.as_str())
    };
    // Each path's owner once, and one trie walk per source for every path its text spells: a
    // pair-by-pair `text.contains` over thousands of paths and sources ran for hours on an
    // archive merge's input (27,657 paths against 9,009 sources).
    let owners: Vec<_> = paths.iter().map(|p| owner(p)).collect();
    let spelled = PathTrie::new(paths.iter().map(String::as_str));
    let mut sources = Vec::new();
    for source in listed {
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
        let file = root.join(source);
        let text = match crate::read_regular_text(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read shared input {}: {e}", file.display())),
        };
        for index in spelled.contained(&text) {
            if owners[index] != Some(member.dir.as_str()) {
                named.insert(paths[index].clone());
            }
        }
        if text.contains("../") {
            sources.push((source.clone(), file, text, root.join(&member.dir)));
        }
    }
    let mut present: BTreeSet<String> = paths.iter().cloned().collect();
    loop {
        let changed: BTreeSet<_> = paths.iter().map(|p| root.join(p)).collect();
        let before = paths.len();
        for (source, file, text, member) in &sources {
            if !readers.contains(source) && shares_changed_input(file, text, member, &changed) {
                readers.insert(source.clone());
                if present.insert(source.clone()) {
                    paths.push(source.clone());
                }
            }
        }
        if paths.len() == before {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    Ok(Shared {
        paths,
        readers,
        named,
    })
}

/// A byte trie of paths. One walk from each position of a text finds every path the text
/// contains: exactly the paths for which `text.contains(path)` holds, by the same byte match.
struct PathTrie {
    nodes: Vec<TrieNode>,
}

#[derive(Default)]
struct TrieNode {
    /// The next byte and its node, sorted by byte.
    next: Vec<(u8, usize)>,
    /// The indices of the paths that end here.
    ends: Vec<usize>,
}

impl PathTrie {
    fn new<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut nodes = vec![TrieNode::default()];
        for (index, path) in paths.into_iter().enumerate() {
            let mut at = 0;
            for &byte in path.as_bytes() {
                at = match nodes[at].next.binary_search_by_key(&byte, |&(b, _)| b) {
                    Ok(found) => nodes[at].next[found].1,
                    Err(slot) => {
                        nodes.push(TrieNode::default());
                        let new = nodes.len() - 1;
                        nodes[at].next.insert(slot, (byte, new));
                        new
                    }
                };
            }
            nodes[at].ends.push(index);
        }
        Self { nodes }
    }

    /// The indices of the paths `text` contains, each once, in index order.
    fn contained(&self, text: &str) -> BTreeSet<usize> {
        let bytes = text.as_bytes();
        let mut found: BTreeSet<usize> = self.nodes[0].ends.iter().copied().collect();
        for start in 0..bytes.len() {
            let mut at = 0;
            for byte in &bytes[start..] {
                let node = &self.nodes[at];
                let Ok(next) = node.next.binary_search_by_key(byte, |&(b, _)| b) else {
                    break;
                };
                at = node.next[next].1;
                found.extend(self.nodes[at].ends.iter().copied());
            }
        }
        found
    }
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
pub(crate) fn shares_changed_input(
    source: &Path,
    text: &str,
    member: &Path,
    changed: &BTreeSet<std::path::PathBuf>,
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
            // A changed path at or above `target` is one of its ancestors. One under it sorts
            // first at or after it, since paths order by components and a folder's
            // descendants follow it before any path that is not one.
            !target.starts_with(member)
                && (target.ancestors().any(|a| changed.contains(a))
                    || (folder
                        && changed
                            .range::<Path, _>((
                                std::ops::Bound::Included(target.as_path()),
                                std::ops::Bound::Unbounded,
                            ))
                            .next()
                            .is_some_and(|p| p.starts_with(&target))))
        })
    })
}

pub(crate) const GIT_REDIRECTS: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Build a `git --no-optional-locks` command in `dir`: the invocation's pinned provider, run with
/// `path_env` as its PATH.
#[must_use]
pub fn git(dir: &Path, path_env: &OsStr) -> Command {
    let mut c = Command::new(crate::git_provider::program());
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

pub(crate) fn read_members(root: &Path, tools: &Toolchain, path_env: &OsStr) -> Members {
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
/// oracle runs on a bump of) still reaches its dev dependents. The members one line under the
/// root, under any section header, are its direct dependents ([`Reach::direct`]).
///
/// `None` on a non-empty line that is neither a section header nor a package one level under what
/// is printed above it: a misread must widen the cone, never narrow it to whatever did parse. An
/// empty printout reads as empty: `cargo tree` found no such package on this host ([`select`]).
fn reached(tree: &str, root_prefix: &str, members: &Members) -> Option<Reach> {
    let mut member_of: BTreeMap<&str, String> = BTreeMap::new();
    let mut closure: BTreeSet<&str> = BTreeSet::new();
    let mut roots: BTreeSet<&str> = BTreeSet::new();
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
                roots.insert(package);
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
    let direct: BTreeSet<&str> = edges
        .iter()
        .filter(|(dependency, dependent, _)| {
            roots.contains(dependency) && !roots.contains(dependent)
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
        direct: names(&direct),
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
#[path = "changed_tests.rs"]
mod tests;
