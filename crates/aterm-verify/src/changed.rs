// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CHANGE-SCOPED SELECTION (`--changed`): the missing middle between the ~2 s
//! pre-push L0 hook and a whole-tree run.
//!
//! The set is "crates whose own files changed" CLOSED UNDER `depends on` — a
//! change to `aterm-grid` must re-test `aterm-gui`, or the tier is a trap rather
//! than a shortcut. The cone is read from `targo tree --invert`, i.e. from the
//! SAME resolved dependency graph the build itself uses, so it cannot drift from
//! a hand-maintained table; dev-dependency edges are included because a crate
//! whose TESTS use the changed crate is just as affected as one whose library
//! does.
//!
//! THE DIRECTION OF FAILURE IS FIXED. Anything this cannot answer honestly —
//! absent targo, no merge-base, an unreadable graph, a manifest-level change that
//! re-plans everything — WIDENS the run to the whole workspace and says so. A
//! narrower that guesses low is a false green; a narrower that guesses high is
//! only slow. Every `Widened` below is one of those admissions, and the reason
//! is carried as text because the run has to print WHY it did more work.
//!
//! THE SHAPE, which is the whole point of the port: [`select`] is a PURE function
//! of (changed paths, manifests, workspace members, the inverted-graph query). It
//! never touches the filesystem, git, or a subprocess, so every seed rule, the
//! reverse-dependency closure and every widening trigger are unit-testable
//! without a repo — the bash original could only be exercised by running it
//! inside a git checkout with a Trust stage2 installed. The impure half is
//! [`resolve`], which does nothing but *fetch* those four inputs and hand them
//! over; when a fetch fails it widens, which is the only judgement it makes.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::ladder::Report;
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
    /// ordinary outcome here (`xtask` is bin-only), so the doctest stage asks.
    pub has_lib: bool,
}

/// The workspace members, taken from the resolved graph rather than from a
/// `crates/*` glob — the selection must not be able to disagree with the build
/// about what a workspace member is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Members(Vec<Member>);

impl Members {
    #[must_use]
    pub fn new(mut entries: Vec<Member>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries.dedup_by(|a, b| a.name == b.name);
        Self(entries)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|m| m.name == name)
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
/// manifest belongs to the workspace — `crates/aterm-scrollback/fuzz/` is a real
/// example in this repo, and charging its files to `aterm-scrollback` would be an
/// invented claim.
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
        /// Crates whose own files changed.
        seeds: Vec<String>,
        /// `seeds` plus every workspace crate that depends on one of them.
        /// Legitimately EMPTY when the diff touched no crate at all.
        crates: Vec<String>,
        /// Does any selected crate have a library target? (see [`Members::any_has_lib`])
        any_lib: bool,
    },
}

/// The paths that re-plan or re-flag EVERY unit in the graph. Narrowing past one
/// of these would be a claim about crates whose build inputs just changed.
///
/// Root-relative and exact, deliberately: `crates/aterm-grid/Cargo.toml` changes
/// one crate's manifest and is OWNED by that crate, so it seeds normally.
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
/// A path no member owns (`docs/`, `tools/`, `scripts/`, and the non-member
/// manifests under `tools/` and `crates/*/fuzz/`) contributes NO crate — those
/// are covered by the whole-tree guard/gate stages, which run unchanged in every
/// mode. The walk never consults the repo-root manifest: it is virtual, and a
/// root-level change that matters is a [`REPLANNING_PATHS`] widening instead.
#[must_use]
pub fn owning_member<'a>(
    path: &str,
    manifests: &'a Manifests,
    members: &BTreeSet<String>,
) -> Option<&'a str> {
    let mut dir = parent_dir(path);
    while !dir.is_empty() {
        if let Some(pkg) = manifests.package_at(dir) {
            // STOP at the first manifest whether or not it is a member: a
            // non-member manifest OWNS its subtree, and walking past it would
            // charge its files to the enclosing crate.
            return pkg.filter(|n| members.contains(*n));
        }
        dir = parent_dir(dir);
    }
    None
}

/// Select the crates a `--changed` run must build and test.
///
/// `dependents` is the inverted-graph query — `targo tree --invert <crate>` —
/// returning every workspace crate that depends on its argument (transitively,
/// and across dev edges), or `None` when the graph could not be read for that
/// crate. `None` widens: unknown dependents are not the same as none.
#[must_use]
pub fn select(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
    dependents: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Selection {
    if members.is_empty() {
        return Selection::Widened("`targo tree` returned no workspace members".to_string());
    }
    let names = members.names();

    let mut widened: Option<String> = None;
    let mut seeds: BTreeSet<String> = BTreeSet::new();
    for p in paths {
        if replans_everything(p) && widened.is_none() {
            widened = Some(format!(
                "{p} changed, which can change how every crate is built"
            ));
        }
        if let Some(name) = owning_member(p, manifests, &names) {
            seeds.insert(name.to_string());
        }
    }
    // Already decided to widen: the cone below would cost a `targo tree --invert`
    // per seed crate and change nothing.
    if let Some(why) = widened.take() {
        return Selection::Widened(why);
    }

    let mut cone: BTreeSet<String> = seeds.clone();
    for seed in &seeds {
        match dependents(seed.as_str()) {
            // The seed is unioned in above rather than trusted to appear in its
            // own inverted tree: a selection that could silently DROP a changed
            // crate is the one failure this tier may never have.
            Some(found) => cone.extend(found.into_iter().filter(|n| names.contains(n))),
            None if widened.is_none() => {
                widened = Some(format!(
                    "`targo tree --invert {seed}` failed, so its dependents are unknown"
                ));
            }
            None => {}
        }
    }
    if let Some(why) = widened {
        return Selection::Widened(why);
    }

    let crates: Vec<String> = cone.into_iter().collect();
    let any_lib = members.any_has_lib(&crates);
    Selection::Narrowed {
        seeds: seeds.into_iter().collect(),
        crates,
        any_lib,
    }
}

/// The `change scope` stage: the scope the rest of the run uses, and the ladder
/// entry that explains it.
///
/// A ladder OUTCOME, not just a NOTICE: a stage that records nothing cannot be
/// counted, and this driver refuses to reach a verdict when a planted stage
/// decided nothing. The outcome is `ok` in both branches — widening is the
/// selection working as designed, and it costs the run nothing but time, so it
/// must not masquerade as a skipped gate.
#[must_use]
pub fn stage_report(base: &str, sel: &Selection) -> (Scope, Report) {
    let mut r = Report::new(format!("change scope (--changed --base {base})"));
    match sel {
        Selection::Widened(why) => {
            r.raw(format!(
                "  NOTICE: --changed could NOT narrow honestly ({why}).\n\
                 \x20         Widening to the WHOLE workspace: a narrower that cannot compute its\n\
                 \x20         scope must do MORE work, never less."
            ));
            r.pass(format!(
                "change scope: WIDENED to the whole workspace ({why})"
            ));
            (Scope::workspace(), r)
        }
        Selection::Narrowed {
            seeds,
            crates,
            any_lib,
        } => {
            r.raw(format!("  changed crates:  {}", list_or_none(seeds)));
            r.raw(format!("  + dependents:    {}", list_or_none(crates)));
            r.pass(format!(
                "change scope: {} crate(s) selected against {base}",
                crates.len()
            ));
            (Scope::changed(base, crates.clone(), *any_lib), r)
        }
    }
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() {
        "<none>".to_string()
    } else {
        v.join(" ")
    }
}

// ---------------------------------------------------------------------------
// Fetching the inputs. The only judgement here is "I could not read this".
// ---------------------------------------------------------------------------

/// Run `--changed`'s selection against a real repo.
#[must_use]
pub fn resolve(root: &Path, tools: &Toolchain, path_env: &OsStr, base: &str) -> Selection {
    if !tools.have_targo() {
        return Selection::Widened(
            "targo is absent, so the dependency graph cannot be read".to_string(),
        );
    }
    let git = |args: &[&str]| stdout_of(Command::new("git").args(args), root, path_env);
    if git(&["rev-parse", "--git-dir"]).is_none() {
        return Selection::Widened(
            "this is not a git checkout, so there is no diff to scope by".to_string(),
        );
    }
    let merge_base = git(&["merge-base", "HEAD", base])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(mb) = merge_base else {
        return Selection::Widened(format!("no merge-base between HEAD and '{base}'"));
    };

    // Tracked changes since the merge-base INCLUDING the working tree (this tier
    // exists to be run BEFORE the commit), plus untracked non-ignored files.
    let (Some(tracked), Some(untracked)) = (
        git(&["diff", "--name-only", &mb]),
        git(&["ls-files", "--others", "--exclude-standard"]),
    ) else {
        return Selection::Widened("git could not list the changed files".to_string());
    };
    let paths: Vec<String> = tracked
        .lines()
        .chain(untracked.lines())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    let members = read_members(root, tools, path_env);
    let manifests = manifests_near(root, &paths);
    select(&paths, &manifests, &members, &|c| {
        invert(root, tools, path_env, c)
    })
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
            let name = line.split_whitespace().next()?;
            let at = line.find(&open)?;
            let rest = &line[at + 1..];
            let end = rest.find(')')?;
            let dir = rest[root_prefix.len()..end].to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The manifests this repo really has around a changed file, including the
    /// non-member ones that stop the ownership walk.
    fn manifests() -> Manifests {
        [
            ("crates/aterm-grid", Some("aterm-grid")),
            ("crates/aterm-gui", Some("aterm-gui")),
            ("crates/aterm-scrollback", Some("aterm-scrollback")),
            (
                "crates/aterm-scrollback/fuzz",
                Some("aterm-scrollback-fuzz"),
            ),
            ("crates/xtask", Some("xtask")),
            ("tools/freeze-safety-gate", Some("freeze-safety-gate")),
        ]
        .into_iter()
        .collect()
    }

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
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

    // -- seeds from paths ---------------------------------------------------

    #[test]
    fn a_changed_file_seeds_the_member_that_owns_it() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-scrollback", "xtask"]);
        let names = m.names();
        let mf = manifests();
        assert_eq!(
            owning_member("crates/aterm-grid/src/lib.rs", &mf, &names),
            Some("aterm-grid")
        );
        assert_eq!(
            owning_member("crates/aterm-grid/tests/deep/nested.rs", &mf, &names),
            Some("aterm-grid")
        );
    }

    #[test]
    fn a_path_no_member_owns_contributes_no_crate() {
        // docs/, tools/, scripts/ and the repo root are covered by the whole-tree
        // guard stages, which run unchanged in every mode.
        let m = members(&["aterm-grid"]);
        let names = m.names();
        let mf = manifests();
        for p in [
            "docs/PROCESS.md",
            "tools/grep_guard.sh",
            "scripts/verify-kani-proofs.sh",
            "README.md",
            ".githooks/pre-push",
        ] {
            assert_eq!(owning_member(p, &mf, &names), None, "{p} owned nothing");
        }
    }

    #[test]
    fn a_non_member_manifest_stops_the_walk_instead_of_charging_its_parent() {
        // crates/aterm-scrollback/fuzz/ is a real non-member manifest in this
        // repo. Walking PAST it would seed aterm-scrollback for a change that
        // cargo does not even build in this workspace.
        let m = members(&["aterm-scrollback"]);
        let names = m.names();
        let mf = manifests();
        assert_eq!(
            owning_member(
                "crates/aterm-scrollback/fuzz/fuzz_targets/a.rs",
                &mf,
                &names
            ),
            None
        );
        assert_eq!(
            owning_member("crates/aterm-scrollback/src/lib.rs", &mf, &names),
            Some("aterm-scrollback")
        );
        // …and the same rule covers the non-member workspaces under tools/.
        assert_eq!(
            owning_member("tools/freeze-safety-gate/src/main.rs", &mf, &names),
            None
        );
    }

    #[test]
    fn the_root_manifest_is_never_an_owner() {
        // It is virtual, and a root-level change that matters widens instead.
        let mut mf = manifests();
        mf.insert("", None);
        let names = members(&["aterm-grid"]).names();
        assert_eq!(owning_member("AGENTS.md", &mf, &names), None);
    }

    // -- the reverse-dependency closure ------------------------------------

    #[test]
    fn a_seed_drags_in_every_crate_that_depends_on_it() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &m,
            &graph(EDGES),
        );
        let (seeds, crates) = narrowed(&sel);
        assert_eq!(seeds, ["aterm-grid"]);
        assert_eq!(
            crates,
            ["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"],
            "direct AND transitive dependents, plus the seed itself"
        );
    }

    #[test]
    fn the_cone_keeps_the_seed_even_if_the_graph_forgets_it() {
        // `targo tree --invert X` prints X first, so this is belt and braces —
        // but a selection that could DROP a changed crate is the one failure
        // this tier may never have, so the seed is unioned in unconditionally.
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &m,
            &|_| Some(vec![]),
        );
        assert_eq!(narrowed(&sel).1, ["aterm-grid"]);
    }

    #[test]
    fn a_dependent_outside_the_workspace_is_not_selectable_and_is_dropped() {
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &m,
            &|_| Some(vec!["aterm-grid".into(), "some-registry-crate".into()]),
        );
        assert_eq!(narrowed(&sel).1, ["aterm-grid"]);
    }

    #[test]
    fn two_seeds_merge_into_one_cone_without_duplicates() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let sel = select(
            &paths(&[
                "crates/aterm-grid/src/lib.rs",
                "crates/aterm-gui/src/app.rs",
                "crates/aterm-gui/src/other.rs",
            ]),
            &manifests(),
            &m,
            &graph(EDGES),
        );
        let (seeds, crates) = narrowed(&sel);
        assert_eq!(seeds, ["aterm-grid", "aterm-gui"]);
        assert_eq!(
            crates,
            ["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"]
        );
    }

    #[test]
    fn a_diff_that_touches_no_crate_selects_nothing_and_does_not_widen() {
        // Legitimately empty: docs-only branches build and test nothing, and the
        // whole-tree guard stages still run. This is NOT a widening — pretending
        // it were would make the cheap case the expensive one.
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["docs/PROCESS.md", "tools/verify.sh"]),
            &manifests(),
            &m,
            &graph(EDGES),
        );
        let (seeds, crates) = narrowed(&sel);
        assert!(seeds.is_empty() && crates.is_empty());
    }

    // -- every widening trigger --------------------------------------------

    #[test]
    fn a_manifest_level_change_widens_and_names_the_file() {
        let m = members(&["aterm-grid"]);
        for f in REPLANNING_PATHS {
            let sel = select(
                &paths(&[f, "crates/aterm-grid/src/lib.rs"]),
                &manifests(),
                &m,
                &graph(EDGES),
            );
            assert_eq!(
                sel,
                Selection::Widened(format!(
                    "{f} changed, which can change how every crate is built"
                )),
                "{f} must widen"
            );
        }
    }

    #[test]
    fn a_crates_own_manifest_is_owned_by_it_and_does_not_widen() {
        // The widening list is root-relative and exact on purpose: a crate's own
        // Cargo.toml re-plans that crate, which is what the seed already says.
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["crates/aterm-grid/Cargo.toml"]),
            &manifests(),
            &m,
            &graph(EDGES),
        );
        assert_eq!(narrowed(&sel).0, ["aterm-grid"]);
        assert!(!replans_everything("crates/aterm-grid/Cargo.toml"));
    }

    #[test]
    fn an_unreadable_graph_widens_rather_than_guessing_no_dependents() {
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &m,
            &|_| None,
        );
        assert_eq!(
            sel,
            Selection::Widened(
                "`targo tree --invert aterm-grid` failed, so its dependents are unknown"
                    .to_string()
            )
        );
    }

    #[test]
    fn an_empty_member_list_widens() {
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &Members::default(),
            &graph(EDGES),
        );
        assert_eq!(
            sel,
            Selection::Widened("`targo tree` returned no workspace members".to_string())
        );
    }

    #[test]
    fn a_widening_never_reports_a_narrower_set_underneath_it() {
        // Whatever seeds were found before the trigger, the answer is the whole
        // workspace: the reason is text for the reader, never a smaller scope.
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs", "Cargo.lock"]),
            &manifests(),
            &m,
            &graph(EDGES),
        );
        let (scope, _) = stage_report("main", &sel);
        assert!(scope.is_workspace());
        assert_eq!(scope.args(), ["--workspace"]);
    }

    // -- the stage's own report --------------------------------------------

    #[test]
    fn the_stage_reports_the_selection_and_always_records_an_outcome() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &m,
            &graph(EDGES),
        );
        let (scope, report) = stage_report("main", &sel);
        let text = report.render();
        assert!(text.starts_with("\n=== change scope (--changed --base main) ===\n"));
        assert!(text.contains("  changed crates:  aterm-grid\n"));
        assert!(
            text.contains("  + dependents:    aterm-bench aterm-cli aterm-grid aterm-gui\n"),
            "{text}"
        );
        assert!(text.contains("  ok    change scope: 4 crate(s) selected against main\n"));
        assert_eq!(report.outcomes().count(), 1, "never a silent stage");
        assert_eq!(
            scope.args(),
            [
                "-p",
                "aterm-bench",
                "-p",
                "aterm-cli",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui"
            ]
        );
    }

    #[test]
    fn the_stage_says_none_rather_than_printing_an_empty_line() {
        let m = members(&["aterm-grid"]);
        let sel = select(&paths(&["docs/x.md"]), &manifests(), &m, &graph(EDGES));
        let (scope, report) = stage_report("origin/main", &sel);
        let text = report.render();
        assert!(text.contains("  changed crates:  <none>\n"));
        assert!(text.contains("  + dependents:    <none>\n"));
        assert!(scope.selects_nothing());
    }

    #[test]
    fn a_widened_stage_prints_the_notice_and_hands_back_the_whole_workspace() {
        let (scope, report) = stage_report("main", &Selection::Widened("because".into()));
        let text = report.render();
        assert_eq!(
            text,
            "\n=== change scope (--changed --base main) ===\n\
             \x20 NOTICE: --changed could NOT narrow honestly (because).\n\
             \x20         Widening to the WHOLE workspace: a narrower that cannot compute its\n\
             \x20         scope must do MORE work, never less.\n\
             \x20 ok    change scope: WIDENED to the whole workspace (because)\n"
        );
        assert!(scope.is_workspace());
        // A widening is not a skip: the run does MORE, so it forfeits nothing.
        assert_eq!(report.outcomes().count(), 1);
        assert!(!text.contains("  skip"));
    }

    // -- the doctest lib-target question -----------------------------------

    #[test]
    fn an_all_binary_selection_reports_that_it_has_no_library_target() {
        // `cargo test --doc -p xtask` is a hard ERROR ("no library targets found
        // in package `xtask`"), and a branch that edits only crates/xtask selects
        // exactly {xtask}. A tier that cries wolf is worse than no tier.
        let m = Members::new(vec![
            Member {
                name: "xtask".into(),
                dir: "crates/xtask".into(),
                has_lib: false,
            },
            Member {
                name: "aterm-grid".into(),
                dir: "crates/aterm-grid".into(),
                has_lib: true,
            },
        ]);
        assert!(!m.any_has_lib(&["xtask".to_string()]));
        assert!(m.any_has_lib(&["xtask".to_string(), "aterm-grid".to_string()]));
        // Not believable => run the stage and let cargo answer.
        let none = Members::new(vec![Member {
            name: "xtask".into(),
            dir: "crates/xtask".into(),
            has_lib: false,
        }]);
        assert!(none.any_has_lib(&["xtask".to_string()]));
    }

    // -- parsing the graph and the manifests --------------------------------

    #[test]
    fn graph_lines_are_parsed_by_name_and_by_being_inside_the_repo() {
        let text = "\
aterm-grid v0.10.0 (/repo/crates/aterm-grid)
aterm-error-derive v0.10.0 (proc-macro) (/repo/crates/aterm-error-derive)
aterm-gui v0.10.0 (/repo/crates/aterm-gui) (*)
libc v0.2.0
winit v0.30.0 (/repo/vendor/winit)
";
        assert_eq!(
            parse_graph_entries(text, "/repo/"),
            [
                ("aterm-grid".to_string(), "crates/aterm-grid".to_string()),
                (
                    "aterm-error-derive".to_string(),
                    "crates/aterm-error-derive".to_string()
                ),
                ("aterm-gui".to_string(), "crates/aterm-gui".to_string()),
                ("winit".to_string(), "vendor/winit".to_string()),
            ],
            "registry crates have no path and are not workspace members"
        );
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

    #[test]
    fn an_explicit_lib_table_counts_as_a_library_target() {
        assert!(declares_lib_target(
            "[package]\nname=\"x\"\n[lib]\npath=\"s.rs\"\n"
        ));
        assert!(!declares_lib_target(
            "[package]\nname=\"x\"\n[[bin]]\nname=\"x\"\n"
        ));
    }

    #[test]
    fn parent_dir_walks_to_the_top_and_stops() {
        assert_eq!(parent_dir("crates/a/src/lib.rs"), "crates/a/src");
        assert_eq!(parent_dir("crates/a"), "crates");
        assert_eq!(parent_dir("crates"), "");
        assert_eq!(parent_dir("README.md"), "");
        assert_eq!(parent_dir(""), "");
    }

    // -- the impure half, on a repo that is not one -------------------------

    #[test]
    fn an_absent_targo_widens_before_anything_else_is_asked() {
        let tools = Toolchain::discover(Some(Path::new("/nonexistent")), Path::new("/nonexistent"));
        assert_eq!(
            resolve(Path::new("/"), &tools, OsStr::new(""), "main"),
            Selection::Widened(
                "targo is absent, so the dependency graph cannot be read".to_string()
            )
        );
    }
}
