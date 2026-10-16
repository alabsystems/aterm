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
//! false green, one that guesses high is only slow.
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

/// The paths that re-plan or re-flag EVERY unit in the graph. Narrowing past one
/// of these would be a claim about crates whose build inputs just changed.
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
/// input to its library.
pub const TESTED_BY: [(&str, &str); 3] = [
    ("tools/install.sh", "aterm-update"),
    // the export policy the engine reads: aterm-release's `it` tests judge it
    ("publish/manifest.txt", "aterm-release"),
    ("publish/config.sh", "aterm-release"),
];

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
#[must_use]
pub fn select(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
    path_packages: &dyn Fn() -> Option<Vec<(String, String)>>,
    dependents: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Selection {
    if members.is_empty() {
        return Selection::Widened("`targo tree` returned no workspace members".to_string());
    }
    let names = members.names();

    let (seeds, replanned) = seeds(paths, manifests, &names);
    // Already decided to widen: the cone below would cost a `targo tree --invert`
    // per seed crate and change nothing.
    if let Some(p) = replanned {
        return Selection::Widened(format!(
            "{p} changed, which can change how every crate is built"
        ));
    }
    let Some(packages) = path_packages() else {
        return Selection::Widened(
            "`targo tree --workspace` failed, so the path packages outside the workspace \
             are unknown"
                .to_string(),
        );
    };
    let mut cone: BTreeSet<String> = seeds.clone();
    let mut seeds = seeds;
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
            .filter_map(|p| TESTED_BY.iter().find(|(file, _)| file == p))
            .map(|(_, member)| (*member).to_string())
            .filter(|n| names.contains(n)),
    );

    let crates: Vec<String> = cone.into_iter().collect();
    let any_lib = members.any_has_lib(&crates);
    Selection::Narrowed {
        seeds: seeds.into_iter().collect(),
        crates,
        any_lib,
    }
}

/// The SEED half of [`select`], on its own: the workspace members whose own
/// files changed, and the first [`REPLANNING_PATHS`] entry among the paths, if
/// any. No reverse cone — for the one stage that must act on exactly what a
/// change touched (the Trust advisory lane: a library's obligations come from
/// its own MIR, and the lane excludes every dependency unit).
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

/// What a change touched, library by library — [`touched_since`]'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touched {
    /// The ref the diff was taken against, as given.
    pub base: String,
    /// The merge-base commit the diff started from.
    pub merge_base: String,
    /// Seed members with a library target and the ADMITTED vendored forks
    /// whose files changed ([`forks_touched`]), sorted: what the Trust lane
    /// verifies (a bin-only seed has nothing the library lane verifies).
    pub libraries: Vec<String>,
    /// The first changed path that re-plans every unit, if any.
    pub replanned: Option<String>,
    /// Members with a library whose changed files are all outside what its
    /// library's verification compiles ([`lib_inputs`]: its own `tests/`,
    /// `benches/`, `examples/`, or a `src/**/<x>_tests.rs` declared only under
    /// `cfg(test)`), sorted: the lane leaves them out and says so.
    pub tests_only: Vec<String>,
}

/// A member's directories that hold other targets, never its library: its
/// integration tests, benches and examples are crates of their own, so a change
/// confined to them cannot move what `targo trust check --lib` verifies.
pub const NON_LIB_DIRS: [&str; 3] = ["tests", "benches", "examples"];

/// The changed paths a library's verification can read ([`touched_since`]):
/// every path but one under its owning manifest's [`NON_LIB_DIRS`] entry that
/// `names(manifest dir, entry)` says the member's own `src/` or `build.rs`
/// never names in an `include*!` or a `#[path]` (aterm-render includes its
/// font fixture from `tests/`, aterm-grapheme its Unicode tables).
///
/// WHY: a change to a crate's tests alone would re-verify its library, which
/// can take many minutes, to prove the same MIR again. A path no manifest owns
/// is kept as it is.
///
/// A UNIT-TEST MODULE FILE TOO: a `src/**/<x>_tests.rs` that
/// `test_module(manifest dir, path in it)` finds declared only under a
/// `cfg(test)` ([`src_declares_only_for_tests`]), which rustc leaves out of the
/// non-test lib's dep-info.
#[must_use]
pub fn lib_inputs(
    paths: &[String],
    manifests: &Manifests,
    names: &dyn Fn(&str, &str) -> bool,
    test_module: &dyn Fn(&str, &str) -> bool,
) -> Vec<String> {
    let mut named: BTreeMap<(String, &str), bool> = BTreeMap::new();
    paths
        .iter()
        .filter(|p| {
            let Some(dir) = owning_dir(p, manifests) else {
                return true;
            };
            let rel = p.strip_prefix(dir).and_then(|r| r.strip_prefix('/'));
            if let Some(rel) = rel
                && rel.starts_with("src/")
                && rel.ends_with("_tests.rs")
                && test_module(dir, rel)
            {
                return false;
            }
            let Some(entry) = rel.and_then(|r| {
                NON_LIB_DIRS
                    .into_iter()
                    .find(|d| r.strip_prefix(d).is_some_and(|rest| rest.starts_with('/')))
            }) else {
                return true;
            };
            *named
                .entry((dir.to_string(), entry))
                .or_insert_with(|| names(dir, entry))
        })
        .cloned()
        .collect()
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

/// Does the member at `root/dir` name its `entry` directory where its library
/// can read it: a line of `build.rs` or of a `.rs` under `src/` that holds
/// `<entry>/` and an `include` macro or a `#[path`. Lexical and generous: a
/// comment or a test-only include counts too, which only keeps a path in.
#[must_use]
pub fn src_names_dir(root: &Path, dir: &str, entry: &str) -> bool {
    let needle = format!("{entry}/");
    member_source_texts(root, dir).any(|text| {
        text.lines()
            .any(|l| l.contains(&needle) && (l.contains("include") || l.contains("#[path")))
    })
}

/// Readable text from build.rs and every Rust file below src/, shared by
/// both library-input probes.
fn member_source_texts(root: &Path, dir: &str) -> impl Iterator<Item = String> {
    fn walk(p: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(p) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }
    let member = root.join(dir);
    let mut files = vec![member.join("build.rs")];
    walk(&member.join("src"), &mut files);
    files
        .into_iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
}

/// Is `rel` (a `src/**/<x>_tests.rs` of the member at `root/dir`) declared only
/// as a unit-test module: every line of the member's `src/` and `build.rs` that
/// names it — `"<file name>"` anywhere, or `mod <stem>;` — is a `#[path = …]`
/// or a `mod` declaration with `#[cfg(test)]` or `#[cfg(all(test, …))]` on one
/// of the two lines above it, and none is an include. Lexical and strict: a
/// file named nowhere, an `any(…)` or a `not(…)` cfg, an include or anything
/// else keeps the path a library input.
#[must_use]
pub fn src_declares_only_for_tests(root: &Path, dir: &str, rel: &str) -> bool {
    let Some(name) = rel.rsplit('/').next() else {
        return false;
    };
    let stem = name.trim_end_matches(".rs");
    let quoted = format!("\"{name}\"");
    let test_cfg = |l: &str| {
        let l = l.trim();
        l.starts_with("#[cfg(test)]") || l.starts_with("#[cfg(all(test,")
    };
    let mut declared = false;
    for text in member_source_texts(root, dir) {
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim();
            let mod_line = t
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ")
                .strip_prefix("mod ")
                .is_some_and(|m| m.trim_end_matches(';').trim() == stem);
            if !l.contains(&quoted) && !mod_line {
                continue;
            }
            let path_attr = t.starts_with("#[path") && !t.contains("include");
            let guarded = lines[i.saturating_sub(2)..i].iter().any(|p| test_cfg(p));
            if !(path_attr || mod_line) || !guarded {
                return false;
            }
            declared = true;
        }
    }
    declared
}

/// The members whose OWN files changed since the merge-base of HEAD and
/// `base` — working tree and untracked files included, exactly the paths the
/// cone diffs — the libraries among them, plus every admitted vendored
/// fork the change touched ([`forks_touched`]). `Err` names what could not be
/// read; the caller decides what that costs (this never widens).
///
/// # Errors
/// No targo, not a git checkout, no merge-base, an unlistable diff, or a
/// member table `targo tree` would not give.
pub fn touched_since(
    root: &Path,
    tools: &Toolchain,
    path_env: &OsStr,
    base: &str,
) -> Result<Touched, String> {
    if !tools.have_targo() {
        return Err("targo is absent, so the workspace members cannot be read".to_string());
    }
    let (merge_base, paths) = changed_paths(root, path_env, base)?;
    let members = read_members(root, tools, path_env);
    if members.is_empty() {
        return Err("`targo tree` returned no workspace members".to_string());
    }
    let paths = with_shared_inputs(root, path_env, paths, &members)?;
    let manifests = manifests_near(root, &paths);
    let names = members.names();
    let has_lib = |name: &String| members.0.iter().any(|m| &m.name == name && m.has_lib);
    let inputs = lib_inputs(
        &paths,
        &manifests,
        &|dir, entry| src_names_dir(root, dir, entry),
        &|dir, rel| src_declares_only_for_tests(root, dir, rel),
    );
    let (seeds, replanned) = seeds(&inputs, &manifests, &names);
    let mut libraries: BTreeSet<String> = seeds.into_iter().filter(has_lib).collect();
    let tests_only: Vec<String> = self::seeds(&paths, &manifests, &names)
        .0
        .into_iter()
        .filter(|name| has_lib(name) && !libraries.contains(name))
        .collect();
    libraries.extend(forks_touched(&paths, &admitted_forks(root)));
    Ok(Touched {
        base: base.to_string(),
        merge_base,
        libraries: libraries.into_iter().collect(),
        replanned,
        tests_only,
    })
}

/// The Trust lane's floors, relative to the repository root: one row per
/// library it holds to a floor, the vendored forks among them.
pub const TRUST_FLOORS: &str = "crates/aterm-release/trust/floors.tsv";

/// The ADMITTED vendored forks — the forks with a row in [`TRUST_FLOORS`],
/// which the Trust lane verifies beside the workspace members — each with the
/// directory of the manifest under `vendor/` that declares it. A queued fork has
/// no floor; a floor no manifest declares is left out, since the lane itself
/// reports it (`FORK-VANISHED`) and a diff cannot touch what is not there.
#[must_use]
pub fn admitted_forks(root: &Path) -> Vec<(String, String)> {
    let Ok(floors) = std::fs::read_to_string(root.join(TRUST_FLOORS)) else {
        return Vec::new();
    };
    let admitted: BTreeSet<&str> = floors
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next().map(str::trim))
        .filter(|name| !name.is_empty())
        .collect();
    let mut found = Vec::new();
    vendor_manifests(root, "vendor", 0, &mut found);
    found
        .into_iter()
        .filter(|(name, _)| admitted.contains(name.as_str()))
        .collect()
}

/// Every `(package, dir)` a `Cargo.toml` declares under `dir`, four levels deep
/// (vendor/astream/crates/<crate> is the deepest fork), skipping build output.
fn vendor_manifests(root: &Path, dir: &str, depth: usize, out: &mut Vec<(String, String)>) {
    if let Some(name) = std::fs::read_to_string(root.join(dir).join("Cargo.toml"))
        .ok()
        .and_then(|t| manifest_package_name(&t))
    {
        out.push((name, dir.to_string()));
    }
    if depth >= 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return;
    };
    let mut subdirs: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && n != "target" && !n.starts_with("target-"))
        .collect();
    subdirs.sort();
    for sub in subdirs {
        vendor_manifests(root, &format!("{dir}/{sub}"), depth + 1, out);
    }
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
    let paths = match changed_paths(root, path_env, base) {
        Ok((_, paths)) => paths,
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
        &|c| invert(root, tools, path_env, c),
    )
}

/// Cargo's dependency graph cannot see source and assets included across a
/// package boundary (`#[path]`, `include!`, `include_str!`, `include_bytes!`).
/// Seed each consumer too. Inspect relative path literals conservatively:
/// comments and test-only references can add work, never remove a check.
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
    if paths.is_empty() || paths.iter().any(|p| replans_everything(p)) {
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
        let mut target = std::path::PathBuf::new();
        for component in parent.join(literal).components() {
            match component {
                std::path::Component::ParentDir => {
                    target.pop();
                }
                std::path::Component::CurDir => {}
                component => target.push(component.as_os_str()),
            }
        }
        !target.starts_with(member) && changed.iter().any(|p| target.starts_with(p))
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

/// The merge-base of HEAD and `base`, and every path changed since it: tracked
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

    /// THE SEED HALF, on its own: the owners of the changed paths and the first
    /// re-planning path — no reverse cone, no widening, and a non-member manifest
    /// still owns its subtree.
    #[test]
    fn seeds_are_the_owners_of_the_changed_paths_and_nothing_downstream() {
        let manifests: Manifests = [
            ("crates/aterm-grid", Some("aterm-grid")),
            ("crates/aterm-gui", Some("aterm-gui")),
            (
                "crates/aterm-scrollback/fuzz",
                Some("aterm-scrollback-fuzz"),
            ),
        ]
        .into_iter()
        .collect();
        let members: BTreeSet<String> = ["aterm-grid", "aterm-gui", "aterm-scrollback"]
            .into_iter()
            .map(String::from)
            .collect();
        let paths: Vec<String> = [
            "crates/aterm-grid/src/lib.rs",
            "crates/aterm-scrollback/fuzz/src/main.rs",
            "docs/x.md",
            "Cargo.lock",
            "rust-toolchain.toml",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (owners, replanned) = seeds(&paths, &manifests, &members);
        assert_eq!(owners.into_iter().collect::<Vec<_>>(), ["aterm-grid"]);
        assert_eq!(replanned.as_deref(), Some("Cargo.lock"));
        let (none, clean) = seeds(&["docs/x.md".to_string()], &manifests, &members);
        assert!(none.is_empty() && clean.is_none());
    }

    /// A LIBRARY'S INPUTS: a change under a member's own `tests/`,
    /// `benches/` or `examples/` is no input of its library unless the member
    /// names that directory in an include; `src/`, the manifest, a look-alike
    /// directory (`tests_util/`, a nested `src/tests/`) and a path no manifest
    /// owns always are. The answer is asked once per directory.
    #[test]
    fn a_change_under_tests_benches_or_examples_is_no_library_input_unless_included() {
        let manifests: Manifests = [("crates/a", Some("a")), ("crates/b", Some("b"))]
            .into_iter()
            .collect();
        let paths: Vec<String> = [
            "crates/a/tests/it.rs",
            "crates/a/tests/common/mod.rs",
            "crates/a/benches/x.rs",
            "crates/a/examples/e.rs",
            "crates/a/src/lib.rs",
            "crates/a/src/tests/mod.rs",
            "crates/a/tests_util/x.rs",
            "crates/a/Cargo.toml",
            "crates/b/tests/fixtures/font.ttf",
            "docs/x.md",
            "crates/a/src/moved_tests.rs",
            "crates/a/src/kept_tests.rs",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let asked = std::cell::RefCell::new(Vec::new());
        let kept = lib_inputs(
            &paths,
            &manifests,
            &|dir, entry| {
                asked.borrow_mut().push(format!("{dir} {entry}"));
                dir == "crates/b" && entry == "tests"
            },
            &|_, rel| rel == "src/moved_tests.rs",
        );
        assert_eq!(
            kept,
            [
                "crates/a/src/lib.rs",
                "crates/a/src/tests/mod.rs",
                "crates/a/tests_util/x.rs",
                "crates/a/Cargo.toml",
                "crates/b/tests/fixtures/font.ttf",
                "docs/x.md",
                "crates/a/src/kept_tests.rs",
            ]
        );
        let mut asked = asked.into_inner();
        asked.sort();
        assert_eq!(
            asked,
            [
                "crates/a benches",
                "crates/a examples",
                "crates/a tests",
                "crates/b tests"
            ]
        );
    }

    /// A UNIT-TEST MODULE FILE: left out only when every line naming it is a
    /// `#[path]` or `mod` declaration under `cfg(test)` / `cfg(all(test, …))`;
    /// a file named nowhere, an include, a `not(…)` or `any(…)` cfg, or an
    /// unguarded `mod` keeps it in.
    #[test]
    fn a_tests_file_is_left_out_only_when_declared_for_tests_alone() {
        let root = crate::mktemp_dir("atv-testmod").expect("mktemp");
        let src = root.join("crates/m/src/sub");
        std::fs::create_dir_all(&src).expect("mkdir");
        let decl = |body: &str| {
            std::fs::write(src.join("parent.rs"), body).expect("write");
            src_declares_only_for_tests(&root, "crates/m", "src/sub/x_tests.rs")
        };
        assert!(!decl("pub fn f() {}\n"), "named nowhere");
        assert!(decl("#[cfg(test)]\n#[path = \"x_tests.rs\"]\nmod tests;\n"));
        assert!(decl(
            "#[cfg(all(test, unix))]\n#[path = \"x_tests.rs\"]\nmod tests;\n"
        ));
        assert!(decl("#[cfg(test)]\nmod x_tests;\n"));
        assert!(!decl(
            "#[cfg(not(test))]\n#[path = \"x_tests.rs\"]\nmod tests;\n"
        ));
        assert!(!decl(
            "#[cfg(any(test, feature = \"t\"))]\n#[path = \"x_tests.rs\"]\nmod tests;\n"
        ));
        assert!(!decl("#[path = \"x_tests.rs\"]\nmod tests;\n"), "unguarded");
        assert!(!decl("mod x_tests;\n"), "an unguarded mod");
        assert!(
            !decl(
                "#[cfg(test)]\n#[path = \"x_tests.rs\"]\nmod tests;\nconst T: &str = include_str!(\"x_tests.rs\");\n"
            ),
            "also included"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The include probe reads `build.rs` and every `.rs` under `src/`, and only
    /// a line that names the directory beside an include or a `#[path` counts.
    #[test]
    fn the_include_probe_finds_a_named_directory_and_ignores_prose() {
        let root = crate::mktemp_dir("atv-libinputs").expect("mktemp");
        let src = root.join("crates/m/src/deep");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("x.rs"), "// see tests/ for more\n").expect("write");
        assert!(
            !src_names_dir(&root, "crates/m", "tests"),
            "prose is no include"
        );
        std::fs::write(
            src.join("y.rs"),
            "const F: &[u8] = include_bytes!(\"../../tests/f.ttf\");\n",
        )
        .expect("write");
        assert!(src_names_dir(&root, "crates/m", "tests"));
        assert!(!src_names_dir(&root, "crates/m", "examples"));
        std::fs::write(
            root.join("crates/m/build.rs"),
            "#[path = \"examples/support.rs\"] mod s;\n",
        )
        .expect("write");
        assert!(src_names_dir(&root, "crates/m", "examples"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// AN ADMITTED FORK IS A LIBRARY THE LANE VERIFIES:
    /// a change inside its directory touches it, and so does a submodule bump,
    /// which lists only the gitlink ABOVE the crates. Negative controls: a
    /// sibling directory sharing a prefix, and an unrelated path, touch nothing.
    #[test]
    fn a_fork_is_touched_by_its_files_and_by_the_gitlink_above_it() {
        let forks = vec![
            ("smol_str".to_string(), "vendor/smol_str".to_string()),
            (
                "astream-wire".to_string(),
                "vendor/astream/crates/astream-wire".to_string(),
            ),
        ];
        let touched = |paths: &[&str]| {
            let paths: Vec<String> = paths.iter().map(ToString::to_string).collect();
            forks_touched(&paths, &forks)
                .into_iter()
                .collect::<Vec<_>>()
        };
        assert_eq!(touched(&["vendor/smol_str/src/lib.rs"]), ["smol_str"]);
        assert_eq!(touched(&["vendor/astream"]), ["astream-wire"]);
        assert_eq!(
            touched(&["vendor/astream/crates/astream-wire/src/frame.rs"]),
            ["astream-wire"]
        );
        assert!(touched(&["vendor/smol_str_extra/src/lib.rs"]).is_empty());
        assert!(touched(&["vendor/astream-other", "crates/aterm-grid/src/lib.rs"]).is_empty());
    }

    /// WHAT A CHANGE TOUCHED, WHATEVER THE CALLER'S GIT CONFIG HIDES:
    /// a file moved out of crate `a` lists `a`'s side too (rename detection
    /// printed only `b`'s, so the cone never built the `a` it broke), a gitlink
    /// bump under `diff.ignoreSubmodules = all` is still listed (it listed
    /// nothing, and the Trust lane selected no fork), and a non-ASCII path is
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
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        git(&["branch", "shared"]);
        git(&["update-index", "--assume-unchanged", "crates/a/src/lib.rs"]);
        git(&["update-index", "--skip-worktree", "crates/b/src/lib.rs"]);
        git(&["update-index", "--skip-worktree", "crates/c/src/lib.rs"]);
        std::fs::write(repo.join("crates/a/src/lib.rs"), "pub fn a() {}\n").expect("write");
        std::fs::write(repo.join("crates/b/src/lib.rs"), "pub fn b() {}\n").expect("write");
        let listed = changed_paths(&repo, &path_env, "shared").map(|(_, paths)| paths);
        let _ = std::fs::remove_dir_all(&repo);
        assert_eq!(
            listed.expect("listed"),
            ["crates/a/src/lib.rs", "crates/b/src/lib.rs"]
        );
    }

    #[test]
    fn flagged_paths_are_hashed_in_batches_without_a_line_quoting_protocol() {
        let repo = crate::mktemp_dir("atv-flagged-batches").expect("mktemp");
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let git = |args: &[&str]| {
            let out = git(&repo, &path_env)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        let names: Vec<_> = (0..257).map(|i| format!("source-{i:04}.rs")).collect();
        // Legal Unix filenames that --stdin-paths would split or try to unquote.
        #[cfg(unix)]
        let names = {
            let mut names = names;
            names.extend(["line\nbreak.rs".into(), "\"quoted.rs".into()]);
            names
        };
        for name in &names {
            std::fs::write(repo.join(name), "unchanged\n").unwrap();
        }
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let mut args = vec!["update-index", "--assume-unchanged", "--"];
        args.extend(names.iter().map(String::as_str));
        git(&args);
        let mut edited: Vec<_> = names.iter().skip(256).cloned().collect();
        edited.push(names[0].clone());
        for name in &edited {
            std::fs::write(repo.join(name), "changed\n").unwrap();
        }
        edited.sort();
        assert_eq!(changed_paths(&repo, &path_env, "HEAD").unwrap().1, edited);
        std::fs::remove_dir_all(repo).unwrap();
    }

    /// The forks with a floor, each found by the manifest that declares it
    /// anywhere under `vendor/` (four levels: vendor/astream/crates/<crate>); a
    /// queued fork has no floor and is not the lane's.
    #[test]
    fn admitted_forks_are_the_forks_with_a_floor_found_under_vendor() {
        let tmp = std::env::temp_dir().join(format!("aterm-verify-forks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let manifest = |dir: &str, name: &str| {
            std::fs::create_dir_all(tmp.join(dir)).expect("mkdir");
            std::fs::write(
                tmp.join(dir).join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            )
            .expect("manifest");
        };
        manifest("vendor/smol_str", "smol_str");
        manifest("vendor/astream/crates/astream-wire", "astream-wire");
        manifest("vendor/libm", "libm");
        std::fs::create_dir_all(tmp.join("crates/aterm-release/trust")).expect("mkdir");
        std::fs::write(
            tmp.join(TRUST_FLOORS),
            "# crate\tproved\ttotal\tprover\nsmol_str\t1\t2\tp\nastream-wire\t1\t2\tp\n\
             aterm-grid\t1\t2\tp\nghost\t1\t2\tp\n",
        )
        .expect("floors");
        let got = admitted_forks(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            got,
            [
                (
                    "astream-wire".to_string(),
                    "vendor/astream/crates/astream-wire".to_string()
                ),
                ("smol_str".to_string(), "vendor/smol_str".to_string()),
            ]
        );
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

    // -- seeds from paths ---------------------------------------------------

    #[test]
    fn a_changed_file_seeds_the_member_that_owns_it() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-scrollback", "aterm-lab"]);
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
        // docs/, tools/ and the repo root are covered by the whole-tree
        // guard stages, which run unchanged in every mode.
        let m = members(&["aterm-grid"]);
        let names = m.names();
        let mf = manifests();
        for p in [
            "docs/PROCESS.md",
            "tools/install.sh",
            "README.md",
            ".gitignore",
        ] {
            assert_eq!(owning_member(p, &mf, &names), None, "{p} owned nothing");
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
        let sel = select(
            &paths(&["publish/manifest.txt"]),
            &manifests(),
            &m,
            &no_packages,
            &graph(&[]),
        );
        assert_eq!(narrowed(&sel), (vec![], vec!["aterm-release".to_string()]));
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
        // …and the same rule covers a non-member workspace (the libc oracle's).
        assert_eq!(owning_member("libc-oracle/src/lib.rs", &mf, &names), None);
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
            &no_packages,
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
            &no_packages,
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
            &no_packages,
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
            &no_packages,
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
            &paths(&["docs/PROCESS.md", "tools/install.sh"]),
            &manifests(),
            &m,
            &no_packages,
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
                &no_packages,
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
            cone("origin/main", &bump).1,
            "the change cone since origin/main: astream-broker astream-cap + 2 dependent(s)"
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

    /// Unknown is not none: a graph that lists no path packages, or a touched
    /// package whose dependents cannot be read, widens and says why.
    #[test]
    fn an_unreadable_graph_for_a_vendored_change_widens() {
        let m = members(&["aterm-link"]);
        let unread = select(
            &paths(&["vendor/astream"]),
            &manifests(),
            &m,
            &|| None,
            &graph(EDGES),
        );
        assert!(
            matches!(&unread, Selection::Widened(why) if why.contains("--workspace")),
            "{unread:?}"
        );
        let no_dependents = select(
            &paths(&["vendor/astream"]),
            &manifests(),
            &m,
            &|| {
                Some(vec![(
                    "astream-wire".to_string(),
                    "vendor/astream/crates/astream-wire".to_string(),
                )])
            },
            &|c| (c != "astream-wire").then(Vec::new),
        );
        assert!(
            matches!(&no_dependents, Selection::Widened(why) if why.contains("astream-wire")),
            "{no_dependents:?}"
        );
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

    #[test]
    fn a_crates_own_manifest_is_owned_by_it_and_does_not_widen() {
        // The widening list is root-relative and exact on purpose: a crate's own
        // Cargo.toml re-plans that crate, which is what the seed already says.
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["crates/aterm-grid/Cargo.toml"]),
            &manifests(),
            &m,
            &no_packages,
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
            &no_packages,
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
            &no_packages,
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
            &no_packages,
            &graph(EDGES),
        );
        let (scope, _) = cone("main", &sel);
        assert!(scope.is_workspace());
        assert_eq!(scope.args(), ["--workspace"]);
    }

    // -- the cone the run's header names ------------------------------------

    #[test]
    fn the_cone_names_the_changed_crates_and_counts_their_dependents() {
        let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
        let sel = select(
            &paths(&["crates/aterm-grid/src/lib.rs"]),
            &manifests(),
            &m,
            &no_packages,
            &graph(EDGES),
        );
        let (scope, said) = cone("origin/main", &sel);
        assert_eq!(
            said,
            "the change cone since origin/main: aterm-grid + 3 dependent(s)"
        );
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
    fn an_empty_cone_says_so_and_selects_nothing() {
        let m = members(&["aterm-grid"]);
        let sel = select(
            &paths(&["docs/x.md"]),
            &manifests(),
            &m,
            &no_packages,
            &graph(EDGES),
        );
        let (scope, said) = cone("origin/main", &sel);
        assert_eq!(
            said,
            "the change cone since origin/main is empty: no workspace crate changed"
        );
        assert!(scope.selects_nothing());
    }

    #[test]
    fn a_widened_cone_says_why_and_hands_back_the_whole_workspace() {
        let (scope, said) = cone("main", &Selection::Widened("because".into()));
        assert_eq!(
            said,
            "the whole workspace (the change cone widened: because)"
        );
        assert!(scope.is_workspace());
    }

    // -- the doctest lib-target question -----------------------------------

    #[test]
    fn a_scoped_crate_answers_the_lib_question_from_its_own_manifest_or_yes() {
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
    }

    #[test]
    fn an_all_binary_selection_reports_that_it_has_no_library_target() {
        // `cargo test --doc -p aterm-lab` is a hard ERROR ("no library targets found
        // in package `aterm-lab`"), and a branch that edits only crates/aterm-lab selects
        // exactly {aterm-lab}. A tier that cries wolf is worse than no tier.
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
    }

    // -- parsing the graph and the manifests --------------------------------

    #[test]
    fn graph_lines_are_parsed_by_name_and_by_being_inside_the_repo() {
        let text = "\
aterm-grid v0.10.0 (/repo/crates/aterm-grid)
aterm-error-derive v0.10.0 (proc-macro) (/repo/crates/aterm-error-derive)
aterm-gui v0.10.0 (/repo/crates/aterm-gui) (*)
libc v0.2.0
indexmap v2.14.0 (/repo/vendor/indexmap)
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
                ("indexmap".to_string(), "vendor/indexmap".to_string()),
            ],
            "registry crates have no path and are not workspace members"
        );
    }

    #[test]
    fn graph_paths_preserve_parentheses_in_the_checkout_and_package_directory() {
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
        let tools = Toolchain::discover(
            Some(Path::new("/nonexistent")),
            Path::new("/nonexistent"),
            std::ffi::OsStr::new(""),
            None,
        );
        assert_eq!(
            resolve(Path::new("/"), &tools, OsStr::new(""), "main"),
            Selection::Widened(
                "targo is absent, so the dependency graph cannot be read".to_string()
            )
        );
    }
}
