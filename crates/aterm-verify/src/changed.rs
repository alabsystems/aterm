// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CHANGE CONE: what a bare `targo --unverified verify` lints, tests and
//! documents. The crates whose own files changed since the merge-base of HEAD
//! and the base (local `origin/main` unless `--base` names another) —
//! uncommitted and untracked changes included — CLOSED UNDER `depends on`, read
//! from `targo tree --invert`, the graph the build itself uses, dev edges
//! included. A path package outside the workspace that the graph carries (a
//! `[patch]`ed fork under `vendor/`, an astream crate under the `vendor/astream`
//! gitlink) is closed the same way when a change touches it. Offline: nothing is
//! fetched.
//!
//! It FAILS WIDE. Anything it cannot answer — no targo, no merge-base, an
//! unreadable graph, a change to a file that re-plans every unit — widens the
//! run to the whole workspace and says why: a narrower that guesses low is a
//! false green, one that guesses high is only slow. The root manifest and the
//! lock are read table by table first ([`manifest_replan`], [`lock_replan`]): a
//! comment or a `default-members` edit selects nothing, a dependency row or a
//! locked package its dependents, and anything else still widens.
//!
//! [`select`] is a PURE function of (changed paths, manifests, members, the
//! inverted-graph query), so its rules are unit-tested without a repo;
//! [`resolve`] fetches those inputs and adds consumers of shared input files
//! that Cargo's package graph does not represent.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::scope::Scope;
use crate::toolchain::Toolchain;

// ---------------------------------------------------------------------------
// The inputs, as values
// ---------------------------------------------------------------------------

/// One workspace member, as the resolved graph reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// Repo-relative directory of its manifest (`crates/aterm-grid`).
    pub dir: String,
    /// Does it have a library target? `cargo test --doc -p X` is a hard ERROR
    /// when NO selected package has one, and an all-binary selection is an
    /// ordinary outcome here (`aterm-lab` is bin-only), so the doctest stage asks.
    pub has_lib: bool,
}

/// The workspace members, taken from the resolved graph rather than from a
/// `crates/*` glob — the selection must not be able to disagree with the build
/// about what a workspace member is — and the `name@version` spec of each
/// member whose name the lock shares with another package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Members(Vec<Member>, BTreeMap<String, String>);

impl Members {
    #[must_use]
    pub fn new(mut entries: Vec<Member>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries.dedup_by(|a, b| a.name == b.name);
        Self(entries, BTreeMap::new())
    }

    /// `specs`: member name to the `name@version` cargo must be told, for the
    /// members whose name another package in the lock also has.
    #[must_use]
    pub fn with_specs(mut self, specs: BTreeMap<String, String>) -> Self {
        self.1 = specs;
        self
    }

    /// How cargo is told about the member `name` (`-p`, `tree --invert`): its
    /// `name@version` when the lock carries another package of that name, where
    /// a bare name is refused as ambiguous (crates/aterm-rustix-compat was
    /// package `rustix` 0.38.44 beside the registry rustix 1.x, until LW.7),
    /// else the name.
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

    /// Does any of `crates` have a library target?
    ///
    /// Fail-closed in the direction that RUNS the stage: if no member at all
    /// claims a lib the table is not believable (the script's rule — an
    /// unreadable member list answers YES), so the doctest stage runs and
    /// reports whatever cargo says.
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

/// Every `Cargo.toml` on the way up from a changed path: repo-relative directory
/// to the name its `[package]` table declares (`None` for a virtual manifest).
///
/// A *directory* is a key only when a manifest really sits there, because the
/// ownership walk stops at the first manifest it meets whether or not that
/// manifest belongs to the workspace — `crates/aterm-render/tests/font_oracle/recorder/`
/// (its own workspace, inside `aterm-render`) is a real example in this repo, and
/// charging its files to `aterm-render` would be an invented claim.
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

    /// `Some(package)` when a manifest sits at `dir`; the inner `Option` is its
    /// `[package] name`, absent for a virtual manifest.
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

// ---------------------------------------------------------------------------
// The result
// ---------------------------------------------------------------------------

/// What the selection decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Could not narrow honestly. The run does MORE work, and says why.
    Widened(String),
    /// The seeds, and the seeds closed under reverse dependency.
    Narrowed {
        /// Crates whose own files changed: members, and path packages outside
        /// the workspace (a vendored fork, an astream crate under its gitlink).
        seeds: Vec<String>,
        /// `seeds` plus every workspace crate that depends on one of them.
        /// Legitimately EMPTY when the diff touched no crate at all.
        crates: Vec<String>,
        /// Does any selected crate have a library target? (see [`Members::any_has_lib`])
        any_lib: bool,
    },
}

/// The paths that CAN re-plan or re-flag every unit in the graph. A change to
/// one is asked what it changed ([`Replan`]): the root manifest and the lock are
/// read table by table, and every other one widens. Narrowing past one unread
/// would be a claim about crates whose build inputs just changed.
///
/// Root-relative and exact, deliberately: `crates/aterm-grid/Cargo.toml` changes
/// one crate's manifest and is OWNED by that crate, so it seeds normally, and a
/// vendored package's change selects its dependents ([`select`]).
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

/// Files outside every crate that a member's tests exercise: a change to one puts
/// that member in the cone. Only the member, not its dependents: the file is no
/// input to its library. A row ending in `/` is a directory: every path under it.
pub const TESTED_BY: [(&str, &str); 2] = [
    ("tools/install.sh", "aterm-update"),
    // The publication policy, hooks and transforms the engine reads
    // (manifest.txt, config.sh, pre-promote, post-promote, transforms.sh,
    // publish.sh, …): aterm-release's tests judge all of it.
    ("publish/", "aterm-release"),
];

/// The member whose tests exercise `path` ([`TESTED_BY`]), if any.
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

/// What a change to one of the [`REPLANNING_PATHS`] asks of the cone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replan {
    /// Nothing builds differently: comments, whitespace, `default-members`.
    Nothing,
    /// These packages (members or not, `name` or `name@version`), which join the
    /// cone's seeds and are closed under their dependents like one.
    Packages(BTreeSet<String>),
    /// Every crate may build differently; the reason, as a clause.
    Everything(String),
}

/// The replan for a path with no table-by-table reading: everything.
#[must_use]
pub fn replan_everything() -> Replan {
    Replan::Everything("it can change how every crate is built".to_string())
}

/// THE ROOT MANIFEST, table by table: what a change from `base` to `work` (either
/// `None`: the file was added or removed) asks of the cone. Comments, whitespace
/// and `[workspace] default-members` select nothing; a `[workspace.dependencies]`
/// row selects the package it names (its `package =`, else its key) and so its
/// dependents; a `[profile.<p>.package.<x>]` row selects `x`. Everything else —
/// `[patch]`, `[workspace.package]`, `[workspace.lints]`, the workspace's members,
/// a profile's own rows, `package."*"`, text that does not parse — widens.
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

/// THE LOCK, entry by entry: what a change from `base` to `work` asks of the
/// cone. Every `[[package]]` entry that was added, removed or changed selects its
/// package (`name@version` where the working lock holds more than one version of
/// the name) and so its dependents — except a name the working lock no longer
/// holds, whose dependents' own entries changed with it. Any other table
/// (`version`, `[[patch.unused]]`), or text that does not parse, widens.
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
            // A version gone while others of the name stay: those entries'
            // dependents changed their own entries with it.
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

// ---------------------------------------------------------------------------
// The pure selection
// ---------------------------------------------------------------------------

/// The parent directory of a repo-relative path; `""` at the top.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// The workspace member owning a repo-relative path: nearest ancestor manifest,
/// accepted only if its package is a member.
///
/// A path no member owns (`docs/`, `tools/`, and non-member manifests) contributes
/// no ownership seed. [`resolve`] separately seeds members that include shared
/// inputs, and [`select`] the dependents of a path package outside the workspace
/// the path touches; other such paths are covered by the whole-tree checks. The walk
/// never consults the repo-root manifest: it is virtual, and a
/// root-level change that matters is a [`REPLANNING_PATHS`] widening instead.
#[must_use]
pub fn owning_member<'a>(
    path: &str,
    manifests: &'a Manifests,
    members: &BTreeSet<String>,
) -> Option<&'a str> {
    let dir = owning_dir(path, manifests)?;
    // A non-member manifest owns its subtree too; do not walk past it to
    // charge those files to an enclosing workspace member.
    manifests.package_at(dir)?.filter(|n| members.contains(*n))
}

/// Select the crates the change cone must build and test.
///
/// `dependents` is the inverted-graph query — `targo tree --invert <crate>` —
/// returning every workspace crate that depends on its argument (transitively,
/// and across dev edges), or `None` when the graph could not be read for that
/// crate. `None` widens: unknown dependents are not the same as none.
///
/// `path_packages` answers the in-repo path packages the resolved graph carries
/// that are not members, each with its directory ([`read_path_packages`]): a
/// change touching one ([`forks_touched`]: a file inside it, or the gitlink
/// above it) seeds that package, and its dependents join the cone like a
/// member's. Asked once, after the replanning check; `None` widens.
///
/// `replan` answers what a change to one of the [`REPLANNING_PATHS`] asks
/// ([`manifest_replan`], [`lock_replan`], else [`replan_everything`]): nothing,
/// packages that join the seeds, or the whole workspace.
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
            // Decided to widen: the cone below would cost a `targo tree
            // --invert` per seed crate and change nothing.
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
        // The seed is already included: an incomplete inverted tree must
        // never silently drop a changed workspace member.
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

/// The SEED half of [`select`], on its own: the workspace members whose own
/// files changed, and the first [`REPLANNING_PATHS`] entry among the paths, if
/// any. No reverse cone.
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

/// The forks among `forks` a change to `paths` touched: a path inside a fork's
/// directory, or a path that CONTAINS it — a submodule bump lists only its
/// gitlink (`vendor/astream`), never the files of the crates beneath it.
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

/// The scope a selection hands the checks that read one, and the sentence the
/// run's header says it in.
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
            // A seed can be a vendored package, which is no crate of the cone.
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

/// The base a cone is taken against when `--base` names none: the shared
/// branch as this checkout last fetched it, `origin/main`, else a local `main`.
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

// ---------------------------------------------------------------------------
// Fetching the inputs. The only judgement here is "I could not read this".
// ---------------------------------------------------------------------------

/// Run the cone's selection against a real repo.
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
/// nor a folder a build script or a test reads. Seed each consumer too.
/// Inspect relative path literals conservatively: comments and test-only
/// references can add work, never remove a check. A literal is resolved
/// against the file's own folder (where `include!` resolves it) and against
/// its package's root (where `CARGO_MANIFEST_DIR` joins and build scripts
/// resolve it). A literal that ends in `/` names a folder: any changed file
/// beneath it seeds the consumer (`"../../contrib/themes/pack/"`).
/// Git supplies the source inventory, including untracked files and excluding
/// generated build output. No per-file dependency registry is needed.
/// This accounts for the direct relative literals used by this tree, not
/// computed paths or the different path base of an inline module attribute.
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
    // Includes can themselves include another package's file. Close those
    // edges before Cargo closes the ordinary dependency edges.
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
    // Rust's raw literals work here too: their quoted contents are identical.
    // Ordinary escaped literals may over-select, which is safe for a cone.
    text.split('"').any(|literal| {
        if !literal.contains("../") {
            return false;
        }
        // A folder literal names a folder: some segment that is not `.` or
        // `..` (a bare `"../"` is a prefix test, not an input).
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
            // A folder that holds the member itself (its `crates/` parent,
            // the repository) is not an input the member reads.
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

/// `git --no-optional-locks <args>` in `dir`, with the redirects removed.
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
/// git ([`flag_hidden_edits`]), and untracked files git does not ignore. Pinned
/// against the caller's git config: `-z` (no `core.quotePath`), `--no-renames`
/// (a move lists the crate it left), and `--ignore-submodules=none` (a gitlink
/// bump is listed). `Err` says which read failed.
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
/// differ from their index entry. The compiler reads the file, whatever the
/// index says, so such an edit is in the cone. A skip-worktree file that is
/// absent is a sparse checkout's, not an edit. `None` when git cannot answer.
fn flag_hidden_edits(root: &Path, path_env: &OsStr) -> Option<Vec<String>> {
    let out = git(root, path_env)
        .args(["ls-files", "-v", "-s", "-z"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `<tag> <mode> <blob> <stage>\t<path>`
    let mut flagged: Vec<(String, String)> = Vec::new();
    let mut gone: Vec<String> = Vec::new();
    for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = rec.iter().position(|b| *b == b'\t')?;
        let meta = String::from_utf8_lossy(&rec[..tab]);
        let path = String::from_utf8_lossy(&rec[tab + 1..]).into_owned();
        let mut fields = meta.split(' ');
        let (tag, mode, blob) = (fields.next()?, fields.next()?, fields.next()?);
        let assumed = tag.chars().next()?.is_ascii_lowercase();
        // A file: a gitlink or a symlink is no source the compiler opens as one.
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
    // Pass paths as argv, not Git's newline/C-quoted --stdin-paths format.
    // Bounded batches fit exec's argument ceiling and avoid a pipe deadlock:
    // Git can fill stdout before a synchronous writer finishes a long stdin.
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

/// A child's stdout on success, `None` on any failure — stdout ONLY, because a
/// warning on stderr must never be parsed as part of the dependency graph.
fn stdout_of(cmd: &mut Command, root: &Path, path_env: &OsStr) -> Option<String> {
    let out = cmd.current_dir(root).env("PATH", path_env).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn targo_tree(root: &Path, tools: &Toolchain, path_env: &OsStr, args: &[&str]) -> Option<String> {
    stdout_of(Command::new(&tools.targo).args(args), root, path_env)
}

/// `{p}` renders as `<name> v<ver> [(proc-macro)] (<path>)`, so the name is field
/// one and workspace membership is "the path is inside the repo root".
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
    // No lock read: no name is known to be shared, and cargo's own refusal of
    // an ambiguous name still only fails the stage that asked.
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

/// `name -> version` for the in-repo packages a `{p}` graph listing names
/// (`rustix v0.38.44 (/repo/crates/aterm-rustix-compat)`); a registry line
/// (`rustix v1.1.4`) carries no path under the root and is skipped.
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

/// The package names a `Cargo.lock` lists in more than one `[[package]]`
/// table: for those a bare `-p <name>` is ambiguous to cargo.
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
/// members — the `[patch]`ed forks under `vendor/` and the astream crates
/// aterm-link path-depends on — each with its repo-relative directory, sorted.
/// Read from the graph [`invert`] queries (`targo tree`, the same edges), so
/// every package listed is one it can answer for: a package the graph does not
/// carry (an astream crate aterm does not use, one behind an off-by-default
/// feature) is no build input of any default-feature check. `None` when the
/// graph could not be read.
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
            // every member, not the default ones: a tool left out of
            // `default-members` is as affected as any other dependant
            "--workspace",
            "--invert",
            crate_name,
            "--prefix",
            "none",
            "--format",
            "{p}",
            // DEV edges included: a crate whose TESTS use the changed crate is
            // as affected as one whose library does.
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

/// The name a `Cargo.toml` declares in its `[package]` table (`None` for a
/// virtual manifest). Deliberately does not read `[[bin]]`/`[lib]` name keys:
/// only the package name is a `-p` spec.
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

/// Cargo's own rule for "has a library", minus the autodiscovered `src/lib.rs`
/// the caller checks on disk: an explicit `[lib]` table.
#[must_use]
pub fn declares_lib_target(manifest: &str) -> bool {
    manifest.lines().any(|l| l.starts_with("[lib]"))
}

/// Does the member at `crates/<name>` have a library target — for the ONE
/// scope shape (`-p <crate>`) that never resolved the graph and so has no
/// `Members` table to ask. Both halves of cargo's rule: the explicit `[lib]`
/// table ([`declares_lib_target`]) and the autodiscovered `src/lib.rs` on
/// disk. The `crates/<name>` convention is this workspace's layout; a member
/// living elsewhere makes the manifest unreadable, and an unanswerable
/// question answers YES — the same fail-closed direction as
/// [`Members::any_has_lib`], here meaning the doc-driver diagnosis fires
/// rather than a bare run that would die raw at rustdoc exec if the answer
/// was really yes.
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
    /// is in the listing; a flagged file left as it was is not. The flagged
    /// files are hashed in bounded batches of argv, so 257 of them come through,
    /// and so do names a line protocol (`--stdin-paths`) would split or unquote.
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
    /// — and a crate's own manifest seeds it without widening. A diff that
    /// touches no crate selects nothing, and does NOT widen.
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
        // tools/install.sh belongs to no crate, but aterm-update's tests run it:
        // that member is tested, and nothing that depends on its library.
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
        // Every file under publish/ is aterm-release's to judge: the policy, the
        // hooks and the transforms its tests run.
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

    /// A VENDORED PACKAGE IS A BUILD INPUT OF ITS DEPENDENTS: a submodule bump
    /// (only the gitlink `vendor/astream` is listed) touches every astream crate
    /// the graph carries beneath it, and a file of a `[patch]`ed fork touches the
    /// fork; either selects the members that depend on it, named after the
    /// packages. Negative controls: vendor/ files no package owns, and a sibling
    /// directory sharing a prefix, select nothing and do not widen.
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
    /// join). Negative control: without the trailing '/' it names one
    /// path, and a file beneath it seeds nothing.
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
    /// another crate does not seed the member. Control: a folder literal
    /// naming that crate does.
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
        // Bin-only: a manifest with no [lib] and no src/lib.rs answers NO.
        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        assert!(!crate_dir_has_lib(&tmp, "binonly"));
        // The autodiscovered src/lib.rs half of cargo's rule.
        std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
        assert!(crate_dir_has_lib(&tmp, "binonly"));
        // The explicit [lib] table half.
        let declared = tmp.join("crates/declared");
        std::fs::create_dir_all(&declared).unwrap();
        std::fs::write(
            declared.join("Cargo.toml"),
            "[package]\nname = \"declared\"\n[lib]\npath = \"other.rs\"\n",
        )
        .unwrap();
        assert!(crate_dir_has_lib(&tmp, "declared"));
        // Unanswerable answers YES — the Members::any_has_lib direction.
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
        // Not believable => run the stage and let cargo answer.
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
