// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formula installation, dependency resolution, bottle staging, and keg linking.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::api::{Api, Catalog};
use super::fetch;
use super::formula::{Formula, Requirement, version_cmp};
use super::machine::Machine;
use super::prefix::{Installed, Kind, Prefix, symlink_dir, symlink_file};

#[cfg(unix)]
mod shared_namespace;
#[cfg(unix)]
use shared_namespace::with_shared_namespaces;

#[cfg(not(unix))]
fn with_shared_namespaces<T>(
    _prefix: &Prefix,
    _payload: Option<&Path>,
    action: impl FnOnce() -> Result<T, InstallError>,
) -> Result<T, InstallError> {
    action()
}

/// Formats the formula's keg version string (including revision when non-zero).
pub(super) fn keg_version(f: &Formula) -> String {
    if f.revision == 0 {
        f.version.clone()
    } else {
        format!("{}_{}", f.version, f.revision)
    }
}

/// One entry of a receipt's `runtime_dependencies` (brew's tab
/// dep hash): the recursive runtime closure as the formula
/// definitions stood at pour time.
struct RuntimeDep {
    full_name: String,
    version: String,
    revision: u64,
    bottle_rebuild: u64,
    pkg_version: String,
    declared: bool,
    compatibility_version: Option<u64>,
}

/// Brew's tab closure for `f`: every recursive runtime
/// dependency, depth-first in declared order, first occurrence
/// kept. `read` resolves a short name to its record (the plan
/// first, then the catalog); an unresolvable name records
/// nothing, as its keg can never be poured either.
fn runtime_closure(f: &Formula, read: &dyn Fn(&str) -> Option<Formula>) -> Vec<RuntimeDep> {
    fn walk(
        deps: &[String],
        direct: &[String],
        read: &dyn Fn(&str) -> Option<Formula>,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<RuntimeDep>,
    ) {
        for dep in deps {
            let short = Formula::short_name(dep);
            if !seen.insert(short.to_string()) {
                continue;
            }
            let Some(record) = read(short) else { continue };
            let declared = direct
                .iter()
                .any(|d| d == &record.full_name || Formula::short_name(d) == record.name);
            // Brew's Dependency.expand is post-order: a dep's
            // own dependencies are recorded before it (the
            // order real tabs carry).
            let entry = RuntimeDep {
                full_name: record.full_name.clone(),
                version: record.version.clone(),
                revision: record.revision,
                bottle_rebuild: record.bottle_rebuild,
                pkg_version: keg_version(&record),
                declared,
                compatibility_version: record.compatibility_version,
            };
            walk(&record.dependencies.clone(), direct, read, seen, out);
            out.push(entry);
        }
    }
    let mut seen: BTreeSet<String> = [f.name.clone()].into_iter().collect();
    let mut out = Vec::new();
    walk(&f.dependencies, &f.dependencies, read, &mut seen, &mut out);
    out
}

/// Encodes a runtime closure as the receipt's JSON array.
fn runtime_deps_json(deps: &[RuntimeDep]) -> String {
    let mut s = String::from("[");
    for (i, d) in deps.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!(
            "{{\"full_name\": \"{}\", \"version\": \"{}\", \"revision\": {}, \"bottle_rebuild\": {}, \"pkg_version\": \"{}\", \"declared_directly\": {}",
            super::prefix::json_escape(&d.full_name),
            super::prefix::json_escape(&d.version),
            d.revision,
            d.bottle_rebuild,
            super::prefix::json_escape(&d.pkg_version),
            d.declared,
        ));
        if let Some(cv) = d.compatibility_version {
            s.push_str(&format!(", \"compatibility_version\": {cv}"));
        }
        s.push('}');
    }
    s.push(']');
    s
}

fn preflight(
    prefix: &Prefix,
    f: &Formula,
    opts: &InstallOpts,
    machine: &Machine,
    is_top: bool,
) -> Result<bool, InstallError> {
    let version = keg_version(f);
    if !super::valid_component(&f.name)
        || !super::valid_cask_version(&version)
        || f.full_name != f.name
    {
        return Err(InstallError::Unsupported(
            "unsupported formula name/version".to_string(),
        ));
    }
    let mut link = !f.keg_only;
    let mut same_keg_reinstall = false;
    if let Some(inst) = prefix.installed_one(Kind::Formula, &f.name) {
        let older = version_cmp(&inst.version, &version) == std::cmp::Ordering::Less;
        same_keg_reinstall = opts.reinstall && inst.version == version;
        // A dependency reaches the plan only when brew's
        // expansion would pour it (missing or below the
        // parent's bottle-tab minimum), so its replacement is
        // admitted in every mode; the top's own replacement
        // stays gated on upgrade/reinstall.
        let admitted_replacement = if older {
            opts.upgrade || opts.reinstall || !is_top
        } else {
            opts.reinstall && inst.version == version
        };
        if !admitted_replacement && (opts.force || older) {
            return Err(InstallError::Unsupported(format!(
                "replacement of {} is not implemented natively",
                f.name
            )));
        }
        if !admitted_replacement {
            return Ok(link);
        }
        if !f.keg_only {
            link = keg_was_linked(prefix, &f.name, &inst.version)?;
        }
    }
    if let Err(needs) = f.native_bottle(&machine.bottle_tags()) {
        return Err(InstallError::Unsupported(format!(
            "{} needs its Homebrew build/relocation recipe ({needs})",
            f.name
        )));
    }
    if let Some(short) = prefix.installed_conflict(&f.conflicts)? {
        return Err(InstallError::Failed(format!(
            "abrew: {} conflicts with installed {short}",
            f.name
        )));
    }
    if f.pour_requires_clt && !machine.has_clt() {
        return Err(InstallError::Unsupported(format!(
            "{} pours only with the Command Line Tools installed",
            f.name
        )));
    }
    let at_least = |have: Option<&str>, floor: &Option<String>| {
        have.is_some_and(|v| {
            floor
                .as_deref()
                .is_none_or(|floor| version_cmp(v, floor) != std::cmp::Ordering::Less)
        })
    };
    for requirement in &f.requirements {
        let (met, what) = match requirement {
            Requirement::MacosMin(floor) => {
                (at_least(machine.macos_version(), floor), "a newer macOS")
            }
            Requirement::MacosMax(ceiling) => (
                machine
                    .macos_version()
                    .is_some_and(|v| version_cmp(v, ceiling) != std::cmp::Ordering::Greater),
                "an older macOS",
            ),
            Requirement::Arch(want) => (want == machine.arch(), "another architecture"),
            Requirement::XcodeMin(floor) => (
                at_least(machine.xcode_version(), floor),
                if floor.is_some() {
                    "a newer Xcode"
                } else {
                    "Xcode"
                },
            ),
        };
        if !met {
            return Err(InstallError::Unsupported(format!(
                "{} needs {what}",
                f.name
            )));
        }
    }
    if !same_keg_reinstall && std::fs::symlink_metadata(prefix.keg(&f.name, &version)).is_ok() {
        return Err(InstallError::Failed(format!(
            "abrew: unrecognized keg already exists for {}",
            f.name
        )));
    }
    Ok(link)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Local installation failure message.
    Failed(String),
    /// Recipe or arguments need a native capability that is not implemented.
    Unsupported(String),
    /// The user answered no at brew's confirmation prompt.
    Aborted,
}

impl InstallError {
    /// The failure or refusal text (an aborted prompt has none
    /// of its own; callers that only ever see planning errors
    /// use this for those).
    pub fn message(&self) -> String {
        match self {
            InstallError::Failed(m) | InstallError::Unsupported(m) => m.clone(),
            InstallError::Aborted => "aborted".to_string(),
        }
    }
}

impl From<String> for InstallError {
    fn from(s: String) -> Self {
        InstallError::Failed(s)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOpts {
    pub ignore_dependencies: bool,
    pub dry_run: bool,
    pub force: bool,
    pub quiet: bool,
    /// Whether this install is an upgrade replacing an older owned keg.
    pub upgrade: bool,
    /// Whether this install is a same-version reinstall.
    pub reinstall: bool,
    /// Whether this formula is installed as another package's dependency.
    pub as_dependency: bool,
    /// Brew's ask mode: prompt before proceeding when planned
    /// dependencies will pour (cleared by --no-ask or the env).
    pub ask: bool,
}

/// Resolves `name`'s runtime dependency install plan in dependency-first order.
pub fn plan(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<Vec<Formula>, InstallError> {
    // Brew's `Dependency#satisfied?` against the pouring
    // parent's bottle tab: an installed dep is kept when it is
    // at the latest version, pinned, or at/above the version
    // the parent's bottle was built against; below that
    // minimum it upgrades with the parent. No tab (unreadable
    // manifest) keeps the installed dep.
    let mut minimums: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, String>,
    > = Default::default();
    dependency_plan(
        name,
        opts.ignore_dependencies,
        false,
        |n| catalog.formula(n).map_err(InstallError::from),
        |parent: &Formula, dep: &Formula| {
            let Some(inst) = prefix.installed_one(Kind::Formula, &dep.name) else {
                return false;
            };
            if version_cmp(&inst.version, &keg_version(dep)) != std::cmp::Ordering::Less {
                return true;
            }
            if prefix.is_pinned(&dep.name) {
                return true;
            }
            let tab = minimums
                .entry(parent.name.clone())
                .or_insert_with(|| bottle_tab_minimums(prefix, parent));
            match tab.get(&dep.name) {
                Some(minimum) => version_cmp(&inst.version, minimum) != std::cmp::Ordering::Less,
                None => true,
            }
        },
    )
}

/// One row of a dry-run install plan: brew's `VersionChange` — the
/// name, the installed version when replacing one, and the version
/// that would be poured.
#[derive(Clone)]
pub(super) struct DryRow {
    pub name: String,
    pub old_version: Option<String>,
    pub new_version: String,
}

/// Brew's already-installed notice for a named formula install
/// (`Install::Check`): the up-to-date wording with its reinstall
/// hint when the current keg is linked, the just-not-linked wording
/// with its link hint otherwise. Printed on stderr, only the first
/// line carrying the `Warning:` prefix.
pub(super) fn warn_already_installed(prefix: &Prefix, inst: &Installed, version: &str) {
    let full = prefix.specified_name(inst);
    if is_linked(prefix, &inst.name, version) {
        super::tty::opoo!("Warning: {full} {version} is already installed and up-to-date.");
        eprintln!("To reinstall {version}, run:");
        eprintln!("  brew reinstall {}", inst.name);
    } else {
        super::tty::opoo!("Warning: {full} {version} is already installed, it's just not linked.");
        eprintln!("To link this version, run:");
        eprintln!("  brew link {full}");
    }
}

/// Plans and preflights a dry install of `name`, returning the
/// requested formula's row (absent when already current, which is
/// warned about exactly as a real install warns) and its dependency
/// rows in plan order. Gating is identical to `install`: anything
/// the native engine would delegate still delegates here, before the
/// caller prints any summary.
pub(super) fn dry_rows(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<(Option<DryRow>, Vec<DryRow>), InstallError> {
    let planned = plan(api, prefix, name, opts)?;
    let top_f = planned.iter().rfind(|f| f.matches_name(name));
    let top_name = top_f.map(|f| f.name.clone());
    // A plain install of an already-current top has no
    // installer in brew: warn and plan nothing, deps included.
    // Brew decides this before any per-formula checks.
    if !opts.force
        && !opts.upgrade
        && !opts.reinstall
        && let Some(top_f) = top_f
        && let Some(inst) = prefix.installed_one(Kind::Formula, &top_f.name)
        && version_cmp(&inst.version, &keg_version(top_f)) != std::cmp::Ordering::Less
    {
        if !opts.quiet {
            let version = inst.version.clone();
            warn_already_installed(prefix, &inst, &version);
        }
        return Ok((None, Vec::new()));
    }
    for f in &planned {
        preflight(
            prefix,
            f,
            opts,
            Machine::this(),
            top_name.as_deref() == Some(f.name.as_str()),
        )?;
    }
    let row = |f: &Formula| -> Option<DryRow> {
        let new_version = keg_version(f);
        match prefix.installed_one(Kind::Formula, &f.name) {
            Some(inst)
                if !opts.force
                    && version_cmp(&inst.version, &new_version) != std::cmp::Ordering::Less =>
            {
                None
            }
            Some(inst) => Some(DryRow {
                name: f.name.clone(),
                old_version: (inst.version != new_version).then_some(inst.version),
                new_version,
            }),
            None => Some(DryRow {
                name: f.name.clone(),
                old_version: None,
                new_version,
            }),
        }
    };
    let mut rows: Vec<DryRow> = planned.iter().filter_map(row).collect();
    let top = if top_name
        .as_deref()
        .is_some_and(|t| rows.last().is_some_and(|r| r.name == t))
    {
        rows.pop()
    } else {
        if !opts.quiet
            && let Some(t) = top_name.as_deref()
            && let Some(inst) = prefix.installed_one(Kind::Formula, t)
        {
            let version = inst.version.clone();
            warn_already_installed(prefix, &inst, &version);
        }
        None
    };
    Ok((top, rows))
}

/// Resolves the full dependency closure for `fetch` regardless of
/// installed state. Brew's `fetch --deps` expands build
/// dependencies too (they are downloaded, never installed), so
/// the fetch plan walks both lists where the install plan walks
/// runtime dependencies only.
pub(super) fn fetch_plan(api: &Api, name: &str) -> Result<Vec<Formula>, InstallError> {
    dependency_plan(
        name,
        false,
        true,
        |n| api.formula(n).map_err(InstallError::from),
        |_, _| false,
    )
}

fn dependency_plan(
    name: &str,
    ignore_dependencies: bool,
    include_build: bool,
    mut read: impl FnMut(&str) -> Result<Formula, InstallError>,
    mut satisfied: impl FnMut(&Formula, &Formula) -> bool,
) -> Result<Vec<Formula>, InstallError> {
    struct PlanState {
        out: Vec<Formula>,
        visiting: Vec<String>,
        seen: BTreeSet<String>,
        pruned: BTreeSet<String>,
        canon_of: BTreeMap<String, String>,
    }
    fn walk(
        name: &str,
        parent: Option<&Formula>,
        ignore_dependencies: bool,
        include_build: bool,
        read: &mut impl FnMut(&str) -> Result<Formula, InstallError>,
        satisfied: &mut impl FnMut(&Formula, &Formula) -> bool,
        state: &mut PlanState,
    ) -> Result<(), InstallError> {
        let name = name.strip_prefix("homebrew/core/").unwrap_or(name);
        if !super::valid_component(name) {
            return Err(InstallError::Unsupported(format!(
                "qualified dependency {name} needs its tap recipe"
            )));
        }
        let short = Formula::short_name(name).to_string();
        if state.visiting.contains(&short) {
            return Err(InstallError::Failed(format!(
                "abrew: dependency cycle: {} -> {short}",
                state.visiting.join(" -> ")
            )));
        }
        if state.seen.contains(&short) && !state.pruned.contains(&short) {
            return Ok(());
        }
        let f = read(&short)?;
        if f.disable {
            return Err(InstallError::Failed(format!(
                "abrew: {short} is disabled upstream"
            )));
        }
        let canon = f.name.clone();
        state.canon_of.insert(short.clone(), canon.clone());
        if canon != short {
            if state.visiting.contains(&canon) {
                return Err(InstallError::Failed(format!(
                    "abrew: dependency cycle: {} -> {canon}",
                    state.visiting.join(" -> ")
                )));
            }
            if state.seen.contains(&canon) && !state.pruned.contains(&canon) {
                state.seen.insert(short);
                return Ok(());
            }
        }
        // A dep another parent already kept can still be pulled
        // in when this parent's bottle was built against a newer
        // version than the installed one (brew's kept-dependency
        // re-expansion).
        if let Some(parent) = parent
            && satisfied(parent, &f)
        {
            state.seen.insert(short.clone());
            state.pruned.insert(short);
            state.seen.insert(canon.clone());
            state.pruned.insert(canon);
            return Ok(());
        }
        state.pruned.remove(&short);
        state.pruned.remove(&canon);
        state.seen.insert(short);
        state.seen.insert(canon.clone());
        if !ignore_dependencies {
            state.visiting.push(canon);
            let deps: Vec<String> = f
                .dependencies
                .iter()
                .chain(
                    include_build
                        .then_some(&f.build_dependencies)
                        .into_iter()
                        .flatten(),
                )
                .cloned()
                .collect();
            for d in &deps {
                walk(
                    d,
                    Some(&f),
                    ignore_dependencies,
                    include_build,
                    read,
                    satisfied,
                    state,
                )?;
            }
            state.visiting.pop();
        }
        state.out.push(f);
        Ok(())
    }
    let mut state = PlanState {
        out: Vec::new(),
        visiting: Vec::new(),
        seen: BTreeSet::new(),
        pruned: BTreeSet::new(),
        canon_of: BTreeMap::new(),
    };
    walk(
        name,
        None,
        ignore_dependencies,
        include_build,
        &mut read,
        &mut satisfied,
        &mut state,
    )?;
    // A dep first kept for one parent and later pulled in by
    // another lands after the parent that kept it; restore
    // deps-before-dependents order (stable for the common case,
    // where the walk already produced it).
    let canon_of = state.canon_of;
    let mut out = state.out;
    let names: BTreeSet<String> = out.iter().map(|f| f.name.clone()).collect();
    let mut ordered: Vec<Formula> = Vec::with_capacity(out.len());
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    while !out.is_empty() {
        let Some(pos) = out.iter().position(|f| {
            f.dependencies.iter().all(|d| {
                let short = Formula::short_name(d);
                let canon = canon_of.get(short).map_or(short, String::as_str);
                !names.contains(canon) || emitted.contains(canon)
            })
        }) else {
            break;
        };
        let f = out.remove(pos);
        emitted.insert(f.name.clone());
        ordered.push(f);
    }
    ordered.extend(out);
    Ok(ordered)
}

/// Resolves and preflights `name`'s install plan without side effects beyond metadata caching.
pub(super) fn preflight_plan(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<(), InstallError> {
    preflight_refusals(catalog, prefix, name, opts)
        .into_iter()
        .next()
        .map_or(Ok(()), Err)
}

/// Collects all preflight refusals for `name` in plan order.
pub(super) fn preflight_refusals(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Vec<InstallError> {
    match plan(catalog, prefix, name, opts) {
        Err(walk) => vec![walk],
        Ok(planned) => {
            let top_f = planned.iter().rfind(|f| f.matches_name(name));
            let top_name = top_f.map(|f| f.name.clone());
            // Brew has no installer for an already-current top
            // outside upgrade/reinstall, so nothing in its plan
            // needs gating (mirrors the early return in install).
            let top_current = !opts.force
                && !opts.upgrade
                && !opts.reinstall
                && !opts.as_dependency
                && top_f.is_some_and(|f| {
                    prefix
                        .installed_one(Kind::Formula, &f.name)
                        .is_some_and(|inst| {
                            version_cmp(&inst.version, &keg_version(f)) != std::cmp::Ordering::Less
                        })
                });
            if top_current {
                return Vec::new();
            }
            planned
                .iter()
                .filter_map(|f| {
                    preflight(
                        prefix,
                        f,
                        opts,
                        catalog.machine(),
                        top_name.as_deref() == Some(f.name.as_str()),
                    )
                    .err()
                })
                .collect()
        }
    }
}

/// The tap a formula display names: `user/repo` from a qualified
/// full name, else the core tap every API formula belongs to.
fn tap_of(f: &Formula) -> String {
    let parts: Vec<&str> = f.full_name.split('/').collect();
    if parts.len() == 3 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        "homebrew/core".to_string()
    }
}

/// Brew's specified form of a full name: official taps omitted.
fn specified_of(full_name: &str) -> String {
    full_name
        .strip_prefix("homebrew/core/")
        .or_else(|| full_name.strip_prefix("homebrew/cask/"))
        .unwrap_or(full_name)
        .to_string()
}

/// Each planned dependency's immediate parent within the plan, for
/// brew's `Installing <parent> dependency: <dep>` headlines.
fn dep_parents(planned: &[Formula], top: &str) -> BTreeMap<String, String> {
    let by_name: BTreeMap<&str, &Formula> = planned.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut parents = BTreeMap::new();
    let mut seen = BTreeSet::from([top.to_string()]);
    let mut queue = vec![top.to_string()];
    while let Some(cur) = queue.pop() {
        if let Some(f) = by_name.get(cur.as_str()) {
            for dep in &f.dependencies {
                let dep = Formula::short_name(dep).to_string();
                if by_name.contains_key(dep.as_str()) && seen.insert(dep.clone()) {
                    parents.insert(dep.clone(), cur.clone());
                    queue.push(dep);
                }
            }
        }
    }
    parents
}

/// Brew's bottle filename: `<name>--<pkg_version>.<tag>.bottle[.N].tar.gz`.
fn bottle_filename(f: &Formula, tag: &str) -> String {
    let rebuild = if f.bottle_rebuild > 0 {
        format!(".{}", f.bottle_rebuild)
    } else {
        String::new()
    };
    format!(
        "{}--{}.{tag}.bottle{rebuild}.tar.gz",
        f.name,
        keg_version(f)
    )
}

/// Brew's cached-download path: `downloads/<sha256(url)>--<file>`
/// under the cache (the name `brew --cache` prints).
pub(super) fn download_dest(prefix: &Prefix, url: &str, filename: &str) -> PathBuf {
    prefix.cache().join("downloads").join(format!(
        "{}--{filename}",
        super::fetch::sha256_bytes(url.as_bytes())
    ))
}

/// The cached bottle file for one of `f`'s bottle entries.
pub(super) fn bottle_dest(
    prefix: &Prefix,
    f: &Formula,
    bottle: &super::formula::Bottle,
) -> PathBuf {
    download_dest(prefix, &bottle.url, &bottle_filename(f, &bottle.tag))
}

/// Brew's `check_install_sanity` pinned-dependency refusal: any
/// pinned runtime dep in `top`'s closure that is not at its
/// latest version blocks the install outright (brew checks
/// `dep.satisfied?` with no bottle minimum here, so even a dep
/// above the bottle-tab minimum counts). Brew fires this at
/// real install time, never for a dry run.
fn check_pinned_dependencies(
    api: &Api,
    prefix: &Prefix,
    top: &Formula,
) -> Result<(), InstallError> {
    let mut seen: BTreeSet<String> = [top.name.clone()].into_iter().collect();
    let mut bad: Vec<String> = Vec::new();
    let mut stack: Vec<String> = top.dependencies.clone();
    while let Some(declared) = stack.pop() {
        let short = Formula::short_name(&declared).to_string();
        if !seen.insert(short.clone()) {
            continue;
        }
        let Some(df) = api
            .formula(&short)
            .ok()
            .or_else(|| super::tap::lookup(prefix, &short))
        else {
            continue;
        };
        if prefix.is_pinned(&short) {
            let satisfied = prefix
                .installed_one(Kind::Formula, &short)
                .is_some_and(|inst| {
                    version_cmp(&inst.version, &keg_version(&df)) != std::cmp::Ordering::Less
                });
            if !satisfied {
                bad.push(declared.clone());
            }
        }
        stack.extend(df.dependencies.iter().cloned());
    }
    if bad.is_empty() {
        return Ok(());
    }
    bad.sort();
    bad.dedup();
    Err(InstallError::Failed(format!(
        "Error: {}: You must `brew unpin {}` as installing {} requires the latest version of pinned dependencies.",
        specified_of(&top.full_name),
        bad.join(" "),
        top.full_name
    )))
}

/// Installs formula `name` and its missing dependencies, returning the installed version.
pub fn install(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<String, InstallError> {
    let planned = plan(api, prefix, name, opts)?;
    // Brew narrates a plain install: the tap-action headline, the
    // dependency roll, and a headline per dependency. Upgrade and
    // reinstall have their own banners at the command layer, and an
    // already-current top is partitioned out before any of it (it
    // only warns), so the narration runs only for a plain install
    // whose top will actually pour.
    let top = planned.iter().rfind(|f| f.matches_name(name));
    let top_short = top.map_or_else(|| Formula::short_name(name).to_string(), |f| f.name.clone());
    let top_current = !opts.force
        && top.is_some_and(|f| {
            prefix
                .installed_one(Kind::Formula, &f.name)
                .is_some_and(|inst| {
                    version_cmp(&inst.version, &keg_version(f)) != std::cmp::Ordering::Less
                })
        });
    // Brew has no installer for an already-current top outside
    // upgrade/reinstall: it only warns (and marks the keg
    // on-request for a named install), pouring no dependencies.
    // Brew decides this when building its installer list, before
    // any per-formula checks, so a planned dependency's own
    // gating must not fire either.
    if top_current
        && !opts.upgrade
        && !opts.reinstall
        && !opts.as_dependency
        && let Some(top_f) = top
        && let Some(inst) = prefix.installed_one(Kind::Formula, &top_f.name)
    {
        if !opts.quiet {
            warn_already_installed(prefix, &inst, &keg_version(top_f));
        }
        if !opts.dry_run {
            prefix.ensure()?;
            let _ = super::prefix::set_installed_on_request(
                &prefix.keg(&inst.name, &inst.version),
                true,
            );
            prefix.rebuild_state()?;
        }
        return Ok(inst.version);
    }
    let link_states = planned
        .iter()
        .map(|f| preflight(prefix, f, opts, Machine::this(), f.name == top_short))
        .collect::<Result<Vec<_>, _>>()?;
    let prior = prior_requests(prefix, planned.iter().map(|f| f.name.as_str()))?;
    if !opts.dry_run {
        prefix.ensure()?;
    }
    let narrate = !opts.quiet
        && !opts.dry_run
        && !opts.upgrade
        && !opts.reinstall
        && !opts.as_dependency
        && !top_current;
    let parents = dep_parents(&planned, &top_short);
    // Brew's plain-install narration: the requested formula's
    // manifest first, then the plan (only when dependencies will
    // pour), then every download under one heading, then the
    // pours. The phases below run in that order.
    let will_pour = |f: &Formula| -> bool {
        opts.force
            || prefix
                .installed_one(Kind::Formula, &f.name)
                .is_none_or(|inst| {
                    version_cmp(&inst.version, &keg_version(f)) == std::cmp::Ordering::Less
                })
    };
    let pouring_deps: Vec<&Formula> = planned
        .iter()
        .filter(|f| f.name != top_short && will_pour(f))
        .collect();
    if !opts.dry_run
        && !opts.ignore_dependencies
        && let Some(top_f) = top
        && will_pour(top_f)
    {
        check_pinned_dependencies(api, prefix, top_f)?;
    }
    if narrate && let Some(top_f) = top {
        if !bottle_manifest_cached(prefix, top_f) {
            eprintln!("{}", super::tty::oh1("Downloading bottle manifests"));
            if fetch_bottle_manifest(prefix, top_f)? {
                eprintln!("✔︎ Bottle Manifest {} ({})", top_f.name, keg_version(top_f));
            }
        }
        if !pouring_deps.is_empty() {
            let row = |f: &Formula| DryRow {
                name: specified_of(&f.full_name),
                old_version: prefix
                    .installed_one(Kind::Formula, &f.name)
                    .filter(|inst| inst.version != keg_version(f))
                    .map(|inst| inst.version),
                new_version: keg_version(f),
            };
            let dep_rows: Vec<DryRow> = pouring_deps.iter().map(|f| row(f)).collect();
            super::commands::print_dry_sections(
                &[(top_short.clone(), Some(row(top_f)), dep_rows)],
                &[],
            );
            if opts.ask {
                ask_to_proceed(if opts.reinstall {
                    "reinstallation"
                } else if opts.upgrade {
                    "upgrade"
                } else {
                    "installation"
                })?;
            }
        }
        eprintln!(
            "{}",
            super::tty::oh1(&format!(
                "Fetching downloads for: {}",
                specified_of(&top_f.full_name)
            ))
        );
        for f in planned.iter().filter(|f| will_pour(f)) {
            fetch_bottle(prefix, f, opts.quiet, true)?;
        }
        let tap = tap_of(top_f);
        if tap != "homebrew/core" {
            println!(
                "{}",
                super::tty::ohai(&format!("Installing {} from {}", top_f.name, tap))
            );
        }
    }
    // Children that will pour, per parent, for brew's
    // more-than-one dependency roll headline.
    let mut rolls: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for dep in &pouring_deps {
        let parent = parents
            .get(&dep.name)
            .map_or(top_short.as_str(), String::as_str);
        rolls
            .entry(parent.to_string())
            .or_default()
            .push(dep.name.clone());
    }
    let mut rolls_printed: BTreeSet<String> = Default::default();
    let mut caveat_notes: Vec<(String, String)> = Vec::new();
    let mut completion_notes: Vec<String> = Vec::new();
    let mut last = String::new();
    let mut installed_now: Vec<String> = Vec::new();
    let mut completed = false;
    for (f, link) in planned.iter().zip(link_states) {
        let version = keg_version(f);
        let mut replacing = ReplaceMode::None;
        let mut old_version: Option<String> = None;
        if let Some(inst) = prefix.installed_one(Kind::Formula, &f.name) {
            if !opts.force && version_cmp(&inst.version, &version) != std::cmp::Ordering::Less {
                if !opts.quiet {
                    warn_already_installed(prefix, &inst, &version);
                }
                last = inst.version;
                continue;
            }
            replacing = if inst.version == version {
                ReplaceMode::SameKeg
            } else {
                old_version = Some(inst.version.clone());
                ReplaceMode::NewKeg
            };
        }
        if opts.dry_run {
            // The command layer prints brew's grouped dry-run summary
            // from dry_rows; a dry install() itself stays silent.
        } else {
            let preserve = opts.upgrade || opts.reinstall;
            let on_request = if preserve {
                prior.get(&f.name).copied().unwrap_or(false)
            } else {
                (!opts.as_dependency && f.name == top_short)
                    || prior.get(&f.name).copied().unwrap_or(false)
            };
            if narrate && f.name != top_short {
                let parent = parents
                    .get(&f.name)
                    .map_or(top_short.as_str(), String::as_str);
                if let Some(children) = rolls.get(parent)
                    && children.len() > 1
                    && rolls_printed.insert(parent.to_string())
                {
                    let parent_full = planned
                        .iter()
                        .find(|p| p.name == parent)
                        .map_or(parent, |p| p.full_name.as_str());
                    let names: Vec<String> =
                        children.iter().map(|n| super::tty::ident(n)).collect();
                    println!(
                        "{}",
                        super::tty::oh1(&format!(
                            "Installing dependencies for {}: {}",
                            specified_of(parent_full),
                            super::commands::to_sentence(&names)
                        ))
                    );
                }
                let from = old_version
                    .as_ref()
                    .map_or_else(String::new, |old| format!("{old} -> "));
                println!(
                    "{}",
                    super::tty::oh1(&format!(
                        "Installing {parent} dependency: {} ({from}{version})",
                        super::tty::ident(&f.name)
                    ))
                );
            }
            if narrate && f.name == top_short && !pouring_deps.is_empty() {
                println!(
                    "{}",
                    super::tty::oh1(&format!("Installing {}", specified_of(&f.full_name)))
                );
            }
            let closure = runtime_closure(f, &|short| {
                planned
                    .iter()
                    .find(|p| p.name == short)
                    .cloned()
                    .or_else(|| api.formula(short).ok())
            });
            match install_one(
                prefix,
                f,
                &closure,
                PourFlags {
                    on_request,
                    link,
                    quiet: opts.quiet,
                    prefetched: narrate,
                },
                replacing,
            ) {
                Ok(()) => {
                    completed = true;
                    if !prior.contains_key(&f.name) {
                        installed_now.push(f.name.clone());
                    }
                    if !opts.quiet {
                        let keg = prefix.keg(&f.name, &version);
                        let shown = std::fs::canonicalize(&keg).unwrap_or_else(|_| keg.clone());
                        let summary =
                            format!("{}: {}", shown.display(), super::commands::keg_abv(&keg));
                        match super::install_badge() {
                            Some(badge) => println!("{badge}  {summary}"),
                            None => println!("{summary}"),
                        }
                        // Brew records caveats for requested
                        // formulae and displays them after every
                        // pour; the same for the generated
                        // completion lines.
                        if on_request {
                            completion_notes.extend(super::commands::completions_and_elisp(
                                prefix,
                                f,
                                &prefix.keg(&f.name, &version),
                            ));
                            let caveats = super::commands::formula_caveats_text(
                                prefix,
                                f,
                                Some(&prefix.keg(&f.name, &version)),
                            );
                            if !caveats.is_empty() {
                                caveat_notes.push((specified_of(&f.full_name), caveats));
                            }
                        }
                    }
                }
                Err(e) => {
                    for done in installed_now.iter().rev() {
                        eprintln!("abrew: rolling back {done} (installed for this failed plan)");
                        let _ = uninstall(api, prefix, done, true, true, false);
                    }
                    return Err(super::after_native_progress(e, completed));
                }
            }
        }
        last = version;
    }
    if !opts.quiet && (!completion_notes.is_empty() || !caveat_notes.is_empty()) {
        println!("{}", super::tty::oh1("Caveats"));
        for note in &completion_notes {
            print!("{note}");
        }
        for (package, text) in &caveat_notes {
            println!("{}", super::tty::ohai(package));
            print!("{text}");
        }
    }
    if !opts.dry_run {
        prefix.rebuild_state()?;
        if completed {
            note_bin_not_on_path(prefix, opts.quiet);
        }
    }
    Ok(last)
}

/// Prints a caveat when the prefix `bin` directory is not on `PATH`.
pub(super) fn note_bin_not_on_path(prefix: &Prefix, quiet: bool) {
    if quiet || super::no_env_hints() {
        return;
    }
    let bin = prefix.bin();
    let on_path =
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == bin));
    if !on_path {
        println!(
            "abrew: note: {} is not on PATH — run: eval \"$(abrew shellenv)\"",
            bin.display()
        );
    }
}

fn prior_requests<'a>(
    prefix: &Prefix,
    names: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, bool>, InstallError> {
    let names: BTreeSet<_> = names.collect();
    prefix
        .installed_checked(Kind::Formula)?
        .into_iter()
        .filter(|installed| names.contains(installed.name.as_str()))
        .map(|installed| {
            let requested = super::commands::requested(prefix, &installed)?;
            Ok((installed.name, requested))
        })
        .collect()
}

fn linkage_leftovers(staged: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_symlink()) {
                // A placeholder a relativization could not map (e.g.
                // @@HOMEBREW_JAVA@@ with no openjdk dependency) must
                // still delegate rather than pour a dangling link.
                if let Ok(target) = std::fs::read_link(&path)
                    && target.to_string_lossy().contains("@@HOMEBREW")
                {
                    out.push(format!("{} (placeholder symlink)", path.display()));
                }
                continue;
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, out);
                continue;
            }
            let Ok(bytes) = crate::metadata_io::read_bounded_regular(&path, 128 * 1024 * 1024)
            else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            if text.contains("@@HOMEBREW") {
                out.push(format!("{} (placeholder)", path.display()));
                continue;
            }
            if cfg!(target_os = "macos") {
                out.extend(macos_leftovers(&path, &bytes));
            }
        }
    }
    walk(staged, &mut out);
    out
}

fn macos_leftovers(path: &Path, bytes: &[u8]) -> Vec<String> {
    if !is_mach_o(bytes) {
        return Vec::new();
    }
    let names = match crate::macho::load_names(bytes) {
        Ok(names) => names.once(),
        Err(why) => {
            return vec![format!(
                "{}: its load commands could not be checked ({why})",
                path.display()
            )];
        }
    };
    names
        .id
        .iter()
        .chain(&names.loads)
        .filter(|dep| {
            dep.starts_with("/opt/homebrew/")
                || dep.starts_with("/usr/local/Cellar/")
                || dep.starts_with("/usr/local/opt/")
        })
        .map(|dep| format!("{} -> {dep}", path.display()))
        .collect()
}

fn is_mach_o(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0xFE, 0xED, 0xFA, 0xCF])
            | Some([0xCF, 0xFA, 0xED, 0xFE])
            | Some([0xFE, 0xED, 0xFA, 0xCE])
            | Some([0xCE, 0xFA, 0xED, 0xFE])
            | Some([0xCA, 0xFE, 0xBA, 0xBE])
            | Some([0xCA, 0xFE, 0xBA, 0xBF])
    ) && !crate::macho::is_java_class(bytes)
}

/// The `install_name_tool` arguments that move a Mach-O's placeholder names to this prefix.
/// # Errors
/// The slices of a universal file name different install names, which one
/// `-id` cannot move.
#[cfg(target_os = "macos")]
fn relocation_args(
    names: &crate::macho::LoadNames,
    relocate: impl Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    let names = names.once();
    let mut args: Vec<String> = Vec::new();
    match names.id.as_slice() {
        [] => {}
        [id] => {
            if let Some(new) = relocate(id) {
                args.extend(["-id".into(), new]);
            }
        }
        ids => {
            return Err(format!(
                "its slices name {} different install names",
                ids.len()
            ));
        }
    }
    for dep in names.loads {
        if let Some(new) = relocate(&dep) {
            args.extend(["-change".into(), dep, new]);
        }
    }
    for rpath in names.rpaths {
        if let Some(new) = relocate(&rpath) {
            args.extend(["-rpath".into(), rpath, new]);
        }
    }
    Ok(args)
}

/// The `@@HOMEBREW_*@@` placeholder → location pairs brew
/// substitutes at pour time. `None` when the prefix path cannot be
/// spelled into a file (a NUL byte), which delegates.
fn placeholder_pairs(prefix: &Prefix, f: &Formula) -> Option<Vec<(&'static str, String)>> {
    let root = prefix.root.to_string_lossy().into_owned();
    let cellar = prefix.cellar().to_string_lossy().into_owned();
    if root.contains('\0') || cellar.contains('\0') {
        return None;
    }
    let java = f
        .dependencies
        .iter()
        .map(|d| Formula::short_name(d))
        .find(|d| *d == "openjdk" || d.starts_with("openjdk@"))
        .map(|d| format!("{root}/opt/{d}/libexec/openjdk.jdk/Contents/Home"));
    let mut pairs: Vec<(&str, String)> = vec![
        ("@@HOMEBREW_CELLAR@@", cellar),
        ("@@HOMEBREW_REPOSITORY@@", root.clone()),
        ("@@HOMEBREW_LIBRARY@@", format!("{root}/Library")),
        ("@@HOMEBREW_PERL@@", format!("{root}/opt/perl/bin/perl")),
        ("@@HOMEBREW_PREFIX@@", root),
    ];
    if let Some(java) = java {
        pairs.push(("@@HOMEBREW_JAVA@@", java));
    }
    Some(pairs)
}

/// `to` expressed relative to the directory `from_dir` (both
/// absolute), brew's `relative_path_from`.
fn relative_path_from(from_dir: &Path, to: &Path) -> PathBuf {
    use std::path::Component;
    let from: Vec<_> = from_dir.components().collect();
    let dest: Vec<_> = to.components().collect();
    let mut common = 0;
    while common < from.len()
        && common < dest.len()
        && from[common] == dest[common]
        && matches!(from[common], Component::Normal(_) | Component::RootDir)
    {
        common += 1;
    }
    let mut out = PathBuf::new();
    for part in &from[common..] {
        if matches!(part, Component::Normal(_)) {
            out.push("..");
        }
    }
    for part in &dest[common..] {
        out.push(part.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Brew's `relativize_prefix_symlinks!`: an absolute symlink into
/// the bottle's built prefix or cellar is rewritten relative to
/// its parent, so it resolves wherever the keg lands. Relative
/// links and absolute links outside the built locations are left
/// alone. Runs against the staged payload but computes paths as
/// they will sit at the keg's final location.
fn relativize_prefix_symlinks(
    prefix: &Prefix,
    f: &Formula,
    staged: &Path,
    keg: &Path,
) -> Result<(), InstallError> {
    let root = prefix.root.to_string_lossy().into_owned();
    let cellar = prefix.cellar().to_string_lossy().into_owned();
    let mut built: Vec<(String, String)> = vec![
        ("@@HOMEBREW_PREFIX@@".into(), "@@HOMEBREW_CELLAR@@".into()),
        ("/opt/homebrew".into(), "/opt/homebrew/Cellar".into()),
        ("/usr/local".into(), "/usr/local/Cellar".into()),
        (
            "/home/linuxbrew/.linuxbrew".into(),
            "/home/linuxbrew/.linuxbrew/Cellar".into(),
        ),
    ];
    if let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags())
        && bottle.cellar.starts_with('/')
        && let Some(parent) = Path::new(&bottle.cellar).parent()
    {
        built.push((parent.to_string_lossy().into_owned(), bottle.cellar.clone()));
    }
    built.retain(|(p, c)| *p != root && *c != cellar);
    fn walk(
        dir: &Path,
        staged: &Path,
        keg: &Path,
        built: &[(String, String)],
        root: &str,
        cellar: &str,
        failures: &mut Vec<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() && !kind.is_symlink() {
                walk(&path, staged, keg, built, root, cellar, failures);
                continue;
            }
            if !kind.is_symlink() {
                continue;
            }
            let Ok(target) = std::fs::read_link(&path) else {
                continue;
            };
            let target_str = target.to_string_lossy();
            let mapped = built.iter().find_map(|(bp, bc)| {
                target_str
                    .strip_prefix(&format!("{bc}/"))
                    .map(|rest| format!("{cellar}/{rest}"))
                    .or_else(|| {
                        target_str
                            .strip_prefix(&format!("{bp}/"))
                            .map(|rest| format!("{root}/{rest}"))
                    })
            });
            let Some(new_abs) = mapped else { continue };
            let Ok(rel_parent) = path.parent().unwrap_or(staged).strip_prefix(staged) else {
                continue;
            };
            let final_parent = keg.join(rel_parent);
            let new_target = relative_path_from(&final_parent, Path::new(&new_abs));
            if let Err(error) = std::fs::remove_file(&path)
                .map_err(|e| e.to_string())
                .and_then(|()| symlink_file(&new_target, &path))
            {
                failures.push(format!("{}: {error}", path.display()));
            }
        }
    }
    let mut failures = Vec::new();
    walk(staged, staged, keg, &built, &root, &cellar, &mut failures);
    if let Some(first) = failures.first() {
        return Err(InstallError::Failed(format!(
            "abrew: {} bottle symlink could not be relativized ({first})",
            f.name
        )));
    }
    Ok(())
}

/// Brew's pour-time text pass (`replace_placeholders_with_locations`
/// for text files): substitute the placeholders in every NUL-free
/// regular file of the staged payload. Runs for every native
/// bottle the Mach-O relocation pass does not already cover —
/// `:any_skip_relocation` bottles still carry placeholders in
/// their scripts and config files. Hardlinks share one rewrite.
fn replace_text_placeholders(
    prefix: &Prefix,
    f: &Formula,
    staged: &Path,
) -> Result<(), InstallError> {
    let Some(pairs) = placeholder_pairs(prefix, f) else {
        return Err(InstallError::Unsupported(format!(
            "{} bottle relocation needs Homebrew (prefix path is not spellable)",
            f.name
        )));
    };
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let path = e.path();
            if kind.is_dir() {
                collect(&path, out);
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    collect(staged, &mut files);
    #[cfg(unix)]
    let mut seen: std::collections::BTreeSet<(u64, u64)> = std::collections::BTreeSet::new();
    for path in files {
        let Ok((file, meta)) = crate::metadata_io::open_regular_with_meta(&path) else {
            continue;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
        }
        let Ok(bytes) =
            crate::metadata_io::read_bounded_regular_file(file, &meta, 128 * 1024 * 1024)
        else {
            continue;
        };
        if bytes.contains(&0) || !bytes.windows(10).any(|w| w == b"@@HOMEBREW") {
            continue;
        }
        let mut text = bytes;
        let mut changed = false;
        for (old, new) in &pairs {
            let old = old.as_bytes();
            if text.windows(old.len()).any(|w| w == old) {
                let replaced = replace_bytes(&text, old, new.as_bytes());
                if replaced != text {
                    changed = true;
                    text = replaced;
                }
            }
        }
        if changed {
            rewrite_staged_regular(&path, &meta, &text)?;
        }
    }
    Ok(())
}

fn rewrite_staged_regular(
    path: &Path,
    meta: &std::fs::Metadata,
    bytes: &[u8],
) -> Result<(), String> {
    use std::io::Write as _;
    let mode = meta.permissions();
    #[cfg(unix)]
    {
        let _ = crate::metadata_io::set_regular_mode(path, 0o644);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut f = opts.open(path).map_err(|e| format!("abrew: {e}"))?;
    if !f.metadata().map_err(|e| format!("abrew: {e}"))?.is_file() {
        return Err(format!("abrew: {} is not a regular file", path.display()));
    }
    f.set_len(0).map_err(|e| format!("abrew: {e}"))?;
    f.write_all(bytes).map_err(|e| format!("abrew: {e}"))?;
    let _ = f.set_permissions(mode);
    Ok(())
}

/// Relocate a staged `:any` bottle payload from placeholder paths
/// to this prefix, brew's two pour-time passes: install names / dylib ids /
/// rpaths rewritten by `install_name_tool`, placeholders in NUL-free (text)
/// files replaced as bytes, and every modified binary ad-hoc re-signed
/// (`codesign`, metadata preserved) since the edits invalidate its signature.
#[cfg(target_os = "macos")]
fn relocate_staged(
    prefix: &Prefix,
    f: &Formula,
    staged: &Path,
    install_name_tool: Option<&Path>,
) -> Result<(), InstallError> {
    use std::os::unix::fs::MetadataExt;
    let unsupported = |what: String| {
        InstallError::Unsupported(format!(
            "{} bottle relocation needs Homebrew ({what})",
            f.name
        ))
    };
    let Some(pairs) = placeholder_pairs(prefix, f) else {
        return Err(unsupported("prefix path is not spellable".into()));
    };
    let relocate_name = |name: &str, pairs: &[(&str, String)]| -> Option<String> {
        for (old, new) in pairs {
            if let Some(rest) = name.strip_prefix(old) {
                return Some(format!("{new}{rest}"));
            }
        }
        None
    };
    let mut files: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::BTreeSet<(u64, u64)> = std::collections::BTreeSet::new();
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let path = e.path();
            if kind.is_dir() {
                collect(&path, out);
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }
    collect(staged, &mut files);
    for path in files {
        let Ok((file, meta)) = crate::metadata_io::open_regular_with_meta(&path) else {
            continue;
        };
        if !seen.insert((meta.dev(), meta.ino())) {
            continue;
        }
        let Ok(bytes) =
            crate::metadata_io::read_bounded_regular_file(file, &meta, 128 * 1024 * 1024)
        else {
            continue;
        };
        if !bytes.windows(10).any(|w| w == b"@@HOMEBREW") {
            continue;
        }
        let shown = path.to_string_lossy().into_owned();
        if is_mach_o(&bytes) {
            let names = crate::macho::load_names(&bytes)
                .map_err(|e| unsupported(format!("{shown}: {e}")))?;
            let mut args = relocation_args(&names, |name| relocate_name(name, &pairs))
                .map_err(|e| unsupported(format!("{shown}: {e}")))?;
            if args.is_empty() {
                continue;
            }
            let Some(install_name_tool) = install_name_tool else {
                return Err(unsupported(format!(
                    "{shown} needs install_name_tool, and no developer tools are installed"
                )));
            };
            let install_name_tool = install_name_tool.to_string_lossy();
            let mode = meta.permissions();
            let _ = crate::metadata_io::set_regular_mode(&path, 0o755);
            args.push(shown.clone());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = fetch::run_capture(&install_name_tool, &refs)
                .map_err(|e| unsupported(e.clone()))?;
            if !out.status.success() {
                return Err(unsupported(format!(
                    "install_name_tool: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let out = fetch::run_capture(
                aterm_uds::os_path!("codesign"),
                &[
                    "--sign",
                    "-",
                    "--force",
                    "--preserve-metadata=entitlements,requirements,flags,runtime",
                    &shown,
                ],
            )
            .map_err(|e| unsupported(e.clone()))?;
            if !out.status.success() {
                return Err(unsupported(format!(
                    "codesign: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            if let Ok(file) = crate::metadata_io::open_regular(&path) {
                let _ = file.set_permissions(mode);
            }
        } else if !bytes.contains(&0) {
            let mut text = bytes;
            let mut changed = false;
            for (old, new) in &pairs {
                let old = old.as_bytes();
                if text.windows(old.len()).any(|w| w == old) {
                    let replaced = replace_bytes(&text, old, new.as_bytes());
                    if replaced != text {
                        changed = true;
                        text = replaced;
                    }
                }
            }
            if changed {
                rewrite_staged_regular(&path, &meta, &text)?;
            }
        }
    }
    Ok(())
}

fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = rest.windows(needle.len()).position(|w| w == needle) {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(replacement);
        rest = &rest[pos + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplaceMode {
    /// First install: every destination must be new.
    None,
    /// Upgrade (or outdated reinstall): publish a NEW keg directory,
    /// swapping the same package's own links.
    NewKeg,
    /// Same-version reinstall: set the live keg aside, publish in its
    /// place, restore it on any failure.
    SameKeg,
}

fn keg_was_linked(prefix: &Prefix, name: &str, version: &str) -> Result<bool, InstallError> {
    let keg = prefix.keg(name, version);
    let links = keg_link_plan(prefix, &keg)?;
    let uncertain = || {
        InstallError::Failed(format!(
            "abrew: cannot preserve {name} {version} activation: links are partial or unreadable, or the payload has no activation files"
        ))
    };
    let mut state = None;
    for link in links {
        if let Some(parent) = link.destination.parent() {
            check_directory(parent, &prefix.root)?;
        }
        let linked = link_points_to(&link.destination, &link.source).ok_or_else(uncertain)?;
        if state.is_some_and(|prior| prior != linked) {
            return Err(uncertain());
        }
        state = Some(linked);
    }
    state.ok_or_else(uncertain)
}

/// The per-keg switches of one pour.
struct PourFlags {
    on_request: bool,
    link: bool,
    quiet: bool,
    prefetched: bool,
}

fn install_one(
    prefix: &Prefix,
    f: &Formula,
    runtime_deps: &[RuntimeDep],
    flags: PourFlags,
    replace: ReplaceMode,
) -> Result<(), InstallError> {
    let PourFlags {
        on_request,
        link,
        quiet,
        prefetched,
    } = flags;
    let version = keg_version(f);
    let keg = prefix.keg(&f.name, &version);
    let work =
        aterm_tempfile::TempDir::new_in(prefix.cellar()).map_err(|e| format!("abrew: {e}"))?;
    let poured = pour_bottle(prefix, f, work.path(), quiet, prefetched)?;
    // Brew substitutes the @@HOMEBREW_*@@ placeholders in text files
    // for EVERY bottle pour, `:any_skip_relocation` included (only
    // linkage fixups are skipped there). The macOS `:any` path does
    // text and Mach-O together in relocate_staged; every other
    // native bottle gets the text pass here, and the leftover scan
    // below still delegates any binary that carries a placeholder.
    let any_cellar = f
        .bottle_for(&Machine::this().bottle_tags())
        .is_some_and(|b| b.cellar == ":any");
    if !(cfg!(target_os = "macos") && any_cellar) {
        replace_text_placeholders(prefix, f, &poured)?;
    }
    #[cfg(target_os = "macos")]
    if any_cellar {
        let install_name_tool = super::machine::DeveloperDir::this().tool("install_name_tool");
        relocate_staged(prefix, f, &poured, install_name_tool.as_deref())?;
    }
    relativize_prefix_symlinks(prefix, f, &poured, &keg)?;
    let leftovers = linkage_leftovers(&poured);
    if !leftovers.is_empty() {
        return Err(InstallError::Unsupported(format!(
            "{} bottle may still reference its build prefix ({}); needs Homebrew relocation",
            f.name,
            leftovers.first().cloned().unwrap_or_default()
        )));
    }
    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .to_string();
    let version_scheme = f.version_scheme.to_string();
    let runtime_deps = runtime_deps_json(runtime_deps);
    prefix.write_receipt_raw(
        &poured,
        &[
            ("name", &f.name),
            ("version", &version),
            ("version_scheme", &version_scheme),
            ("source", "bottle"),
            ("installed_by", "abrew"),
            ("time", &installed_at),
            (
                "installed_on_request",
                if on_request { "true" } else { "false" },
            ),
            ("keg_only", if f.keg_only { "true" } else { "false" }),
            ("full_name", &f.full_name),
        ],
        &[("runtime_dependencies", &runtime_deps)],
    )?;
    with_shared_namespaces(
        prefix,
        (!f.keg_only && link).then_some(poured.as_path()),
        || {
            let mut links = Vec::new();
            if !f.keg_only && link {
                plan_links(&poured, &keg, &prefix.root, &mut links)?;
            }
            links.push(InstallLink {
                source: keg.clone(),
                destination: prefix.opt(&f.name),
                directory: true,
            });
            let keg_base = prefix.cellar().join(&f.name);
            match replace {
                ReplaceMode::None => publish_payload(prefix, &poured, &keg, &links),
                ReplaceMode::NewKeg => {
                    publish_payload_replacing(prefix, &poured, &keg, &links, &keg_base)
                }
                ReplaceMode::SameKeg => {
                    publish_payload_reinstall(prefix, &poured, &keg, &links, &keg_base)
                }
            }
        },
    )
}

/// Planned symlink in a new payload's activation plan.
pub(super) struct InstallLink {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub directory: bool,
}

/// What brew's `Keg#link_dir` does with a keg directory:
/// descend past real prefix dirs (and create them for
/// database directories), symlink the directory whole, or
/// prune it entirely.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DirMode {
    Mkpath,
    Link,
    Skip,
}

fn postgresql_dir(rel: &str) -> bool {
    rel.strip_prefix("postgresql@")
        .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

/// Brew's `LOCALEDIR_RX` over a share-relative path
/// (`locale/` or `man/` + a locale segment).
fn locale_dir(rel: &str) -> bool {
    let mut segments = rel.split('/');
    if !matches!(segments.next(), Some("locale" | "man")) {
        return false;
    }
    let Some(locale) = segments.next() else {
        return false;
    };
    let (head, tail) = if locale == "C" || locale.starts_with("POSIX") {
        (true, locale.strip_prefix("POSIX").unwrap_or(""))
    } else if locale.len() >= 2 && locale[..2].chars().all(|c| c.is_ascii_lowercase()) {
        (true, &locale[2..])
    } else {
        (false, "")
    };
    head && (tail.is_empty() || tail.starts_with('_') || tail.starts_with('.'))
}

fn link_dir_mode(sub: &str, rel: &str) -> DirMode {
    const SHARE_MKPATH: &[&str] = &[
        "aclocal",
        "cps",
        "doc",
        "info",
        "java",
        "locale",
        "man",
        "man/man1",
        "man/man2",
        "man/man3",
        "man/man4",
        "man/man5",
        "man/man6",
        "man/man7",
        "man/man8",
        "man/cat1",
        "man/cat2",
        "man/cat3",
        "man/cat4",
        "man/cat5",
        "man/cat6",
        "man/cat7",
        "man/cat8",
        "applications",
        "gnome",
        "gnome/help",
        "icons",
        "mime",
        "mime/packages",
        "mime-info",
        "pixmaps",
        "postgresql",
        "sounds",
    ];
    match sub {
        "etc" => DirMode::Mkpath,
        "bin" | "sbin" => DirMode::Skip,
        "include" => {
            if postgresql_dir(rel) {
                DirMode::Mkpath
            } else {
                DirMode::Link
            }
        }
        "share" => {
            if locale_dir(rel)
                || rel.starts_with("icons/")
                || rel.starts_with("zsh")
                || rel.starts_with("fish")
                || rel.starts_with("pwsh")
                || rel.starts_with("lua/")
                || rel.starts_with("guile/")
                || postgresql_dir(rel)
                || rel.starts_with("pypy")
                || SHARE_MKPATH.contains(&rel)
            {
                DirMode::Mkpath
            } else {
                DirMode::Link
            }
        }
        "lib" => {
            let python = rel.len() > 7
                && rel.starts_with("python")
                && matches!(rel.as_bytes()[6], b'2' | b'3')
                && rel.as_bytes()[7] == b'.'
                && rel[8..].chars().next().is_some_and(|c| c.is_ascii_digit());
            if matches!(
                rel,
                "cps" | "pkgconfig" | "cmake" | "dtrace" | "ghc" | "php"
            ) || rel.starts_with("gdk-pixbuf")
                || rel.starts_with("gio")
                || rel.starts_with("lua")
                || rel.starts_with("mecab")
                || rel.starts_with("node")
                || rel.starts_with("ocaml")
                || rel.starts_with("perl5/")
                || postgresql_dir(rel)
                || rel.starts_with("pypy")
                || python
                || rel.starts_with('R')
                || rel.starts_with("ruby")
            {
                DirMode::Mkpath
            } else {
                DirMode::Link
            }
        }
        "Frameworks" => {
            let framework = rel
                .strip_suffix("/Versions")
                .unwrap_or(rel)
                .ends_with(".framework")
                && !rel.strip_suffix("/Versions").unwrap_or(rel).contains('/');
            if framework {
                DirMode::Mkpath
            } else {
                DirMode::Link
            }
        }
        _ => DirMode::Link,
    }
}

/// Whether brew's file branch skips this keg file outright
/// (`.DS_Store`, `charset.alias`, icon caches, the info
/// `dir` index, bytecode under site-packages, and symlinks
/// that already point at their destination or into another
/// keg's opt tree at the same relative path).
fn link_file_skipped(
    sub: &str,
    rel: &str,
    src: &Path,
    dst: &Path,
    root: &Path,
    keg_rel: &str,
) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    if base == ".DS_Store" {
        return true;
    }
    if matches!(sub, "lib") && rel == "charset.alias" {
        return true;
    }
    if sub == "share" {
        if rel == "locale/locale.alias"
            || (rel.starts_with("icons/") && rel.ends_with("/icon-theme.cache"))
        {
            return true;
        }
        if rel.starts_with("info/") && base == "dir" {
            return true;
        }
    }
    if (base.ends_with(".pyc") || base.ends_with(".pyo")) && keg_rel.contains("/site-packages/") {
        return true;
    }
    if src.is_symlink()
        && let Ok(resolved) = std::fs::canonicalize(src)
    {
        if resolved == dst {
            return true;
        }
        let opt = root.join("opt");
        if let Ok(under) = resolved.strip_prefix(&opt)
            && let Some((_, tail)) = under.to_string_lossy().split_once('/')
            && tail == keg_rel
        {
            return true;
        }
    }
    false
}

fn plan_links(
    staged: &Path,
    keg: &Path,
    root: &Path,
    links: &mut Vec<InstallLink>,
) -> Result<(), InstallError> {
    // These two values remain fixed throughout one recursive tree plan.
    struct LinkContext<'a> {
        sub: &'a str,
        root: &'a Path,
    }

    fn walk(
        context: &LinkContext<'_>,
        rel: &str,
        staged: &Path,
        live: &Path,
        dst: &Path,
        view: Option<&Path>,
        links: &mut Vec<InstallLink>,
    ) -> Result<(), InstallError> {
        let sub = context.sub;
        let root = context.root;
        let mut entries: Vec<_> = std::fs::read_dir(staged)
            .map_err(|e| format!("abrew: {e}"))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("abrew: {e}"))?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
            let name = entry.file_name();
            let rel = if rel.is_empty() {
                name.to_string_lossy().into_owned()
            } else {
                format!("{rel}/{}", name.to_string_lossy())
            };
            let destination = dst.join(&name);
            let source = live.join(&name);
            let viewed_destination = view.map(|path| path.join(&name));
            let observed = viewed_destination.as_deref().unwrap_or(&destination);
            if kind.is_symlink() || kind.is_file() {
                if link_file_skipped(
                    sub,
                    &rel,
                    &entry.path(),
                    &destination,
                    root,
                    &format!("{sub}/{rel}"),
                ) {
                    continue;
                }
                links.push(InstallLink {
                    source,
                    destination,
                    directory: false,
                });
            } else if kind.is_dir() {
                if name.to_string_lossy().ends_with(".app") {
                    continue;
                }
                // Unowned directory aliases are never traversed. An owned
                // alias can be projected for a dry run; actual activation
                // expands it transactionally before planning writes. The
                // exact current-keg alias remains useful to state probes.
                if std::fs::symlink_metadata(observed).is_ok_and(|m| m.file_type().is_symlink()) {
                    if view.is_some() {
                        return Err(format!(
                            "abrew: refusing foreign link destination {}",
                            destination.display()
                        )
                        .into());
                    }
                    let own = link_points_to(&destination, &source) == Some(true);
                    if !own {
                        #[cfg(unix)]
                        {
                            // Dry-run projection only: activation expands this admitted
                            // namespace transactionally before planning real writes.
                            let owned =
                                shared_namespace::owned_directory(&Prefix::at(root), &destination)?;
                            walk(
                                context,
                                &rel,
                                &entry.path(),
                                &source,
                                &destination,
                                Some(&owned),
                                links,
                            )?;
                            continue;
                        }
                        #[cfg(not(unix))]
                        return Err(format!(
                            "abrew: refusing foreign link destination {}",
                            destination.display()
                        )
                        .into());
                    }
                    links.push(InstallLink {
                        source,
                        destination,
                        directory: true,
                    });
                    continue;
                }
                // An existing real prefix directory is walked
                // into, never replaced by a symlink.
                let real_dst_dir = std::fs::symlink_metadata(observed)
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
                if real_dst_dir {
                    walk(
                        context,
                        &rel,
                        &entry.path(),
                        &source,
                        &destination,
                        viewed_destination.as_deref(),
                        links,
                    )?;
                    continue;
                }
                match link_dir_mode(sub, &rel) {
                    DirMode::Skip => {}
                    DirMode::Mkpath => {
                        walk(
                            context,
                            &rel,
                            &entry.path(),
                            &source,
                            &destination,
                            viewed_destination.as_deref(),
                            links,
                        )?;
                    }
                    DirMode::Link => links.push(InstallLink {
                        source,
                        destination,
                        directory: true,
                    }),
                }
            }
            // Other file types are never linked (brew's walk
            // has no branch for them).
        }
        Ok(())
    }
    for sub in [
        "bin",
        "sbin",
        "lib",
        "include",
        "share",
        "etc",
        "Frameworks",
    ] {
        let src = staged.join(sub);
        match std::fs::symlink_metadata(&src) {
            Ok(m) if m.file_type().is_dir() => {
                walk(
                    &LinkContext { sub, root },
                    "",
                    &src,
                    &keg.join(sub),
                    &root.join(sub),
                    None,
                    links,
                )?;
            }
            Ok(_) => {
                return Err(format!("abrew: not a physical directory: {}", src.display()).into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("abrew: {e}").into()),
        }
    }
    Ok(())
}

pub(super) fn check_directory(path: &Path, root: &Path) -> Result<(), InstallError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "abrew: link destination escapes prefix".to_string())?;
    let mut current = root.to_path_buf();
    for part in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(part) = part {
            if !matches!(part, std::path::Component::Normal(_)) {
                return Err(InstallError::Failed(
                    "abrew: invalid link directory".to_string(),
                ));
            }
            current.push(part.as_os_str());
        }
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => {
                return Err(InstallError::Failed(format!(
                    "abrew: refusing non-directory {}",
                    current.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(InstallError::Failed(format!("abrew: {e}"))),
        }
    }
    Ok(())
}

/// Publishes a new keg payload and writes its receipt after all links succeed.
pub(super) fn publish_payload(
    prefix: &Prefix,
    staged: &Path,
    live: &Path,
    links: &[InstallLink],
) -> Result<(), InstallError> {
    publish_payload_with(prefix, staged, live, links, None, None, |link| {
        if link.directory {
            symlink_dir(&link.source, &link.destination)
        } else {
            symlink_file(&link.source, &link.destination)
        }
    })
}

/// Publishes a keg payload replacing the same package's receipt-owned links.
pub(super) fn publish_payload_replacing(
    prefix: &Prefix,
    staged: &Path,
    live: &Path,
    links: &[InstallLink],
    replace_root: &Path,
) -> Result<(), InstallError> {
    publish_payload_with(
        prefix,
        staged,
        live,
        links,
        Some(replace_root),
        None,
        |link| {
            if link.directory {
                symlink_dir(&link.source, &link.destination)
            } else {
                symlink_file(&link.source, &link.destination)
            }
        },
    )
}

pub(super) fn publish_payload_reinstall(
    prefix: &Prefix,
    staged: &Path,
    live: &Path,
    links: &[InstallLink],
    root: &Path,
) -> Result<(), InstallError> {
    reinstall_payload_with(prefix, staged, live, links, root, |link| {
        symlink_file(&link.source, &link.destination)
    })
}

struct RelocatedKeg {
    live: PathBuf,
    aside: PathBuf,
    identity: std::fs::Metadata,
}

fn reinstall_payload_with(
    prefix: &Prefix,
    staged: &Path,
    live: &Path,
    links: &[InstallLink],
    root: &Path,
    place: impl FnMut(&InstallLink) -> Result<(), String>,
) -> Result<(), InstallError> {
    // Establish receipt authority before moving the live version. Keep the
    // aside on restore failure: a temporary-directory Drop must not delete it.
    if !replacement_target_owned(prefix, live, live, root) {
        return Err("abrew: reinstall requires an owned physical keg"
            .to_string()
            .into());
    }
    for link in links {
        if std::fs::symlink_metadata(&link.destination).is_ok()
            && !std::fs::read_link(&link.destination).is_ok_and(|target| {
                replacement_target_owned(prefix, &link.destination, &target, root)
            })
        {
            return Err(format!(
                "abrew: refusing existing artifact {}",
                link.destination.display()
            )
            .into());
        }
    }
    let identity = std::fs::symlink_metadata(live).map_err(|e| format!("abrew: {e}"))?;
    let reserved = aterm_tempfile::TempDir::new_in(
        root.parent()
            .ok_or_else(|| "abrew: package root has no parent".to_string())?,
    )
    .map_err(|e| format!("abrew: {e}"))?
    .keep();
    let old = RelocatedKeg {
        live: live.to_path_buf(),
        aside: reserved.join("payload"),
        identity,
    };
    let result = (|| {
        check_directory(root, &prefix.root)?;
        if !same_path_identity(live, &old.identity)
            || !replacement_target_owned(prefix, live, live, root)
        {
            return Err("abrew: reinstall keg changed".to_string().into());
        }
        super::prefix::publish_directory(live, &old.aside, false)
            .map_err(|e| format!("abrew: cannot preserve old keg: {e}"))?;
        publish_payload_with(prefix, staged, live, links, Some(root), Some(&old), place)
    })();
    if result.is_ok() {
        if check_directory(&reserved, &prefix.root).is_ok()
            && same_path_identity(&old.aside, &old.identity)
            && let Err(e) = std::fs::remove_dir_all(&old.aside)
        {
            eprintln!("abrew: old keg kept at {}: {e}", old.aside.display());
        }
    } else if check_directory(root, &prefix.root).is_ok()
        && same_path_identity(&old.aside, &old.identity)
        && std::fs::symlink_metadata(live).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        && let Err(e) = super::prefix::publish_directory(&old.aside, live, false)
    {
        eprintln!("abrew: old keg kept at {}: {e}", old.aside.display());
    }
    let _ = std::fs::remove_dir(&reserved);
    result
}

fn publish_payload_with(
    prefix: &Prefix,
    staged: &Path,
    live: &Path,
    links: &[InstallLink],
    replace_root: Option<&Path>,
    relocated: Option<&RelocatedKeg>,
    mut place: impl FnMut(&InstallLink) -> Result<(), String>,
) -> Result<(), InstallError> {
    let mut directories = CreatedDirectories::new(prefix);
    let parent = live
        .parent()
        .ok_or_else(|| "abrew: payload has no parent".to_string())?;
    check_directory(parent, &prefix.root)?;
    let mut replaced: Vec<(&InstallLink, PathBuf, std::fs::Metadata)> = Vec::new();
    for link in links {
        if let Ok(identity) = std::fs::symlink_metadata(&link.destination) {
            let swap = replace_root.and_then(|root| {
                let target = std::fs::read_link(&link.destination).ok()?;
                replacement_target_owned_at(prefix, &link.destination, &target, root, relocated)
                    .then_some(target)
            });
            match swap {
                Some(old_target) => replaced.push((link, old_target, identity)),
                None => {
                    return Err(InstallError::Failed(format!(
                        "abrew: refusing existing artifact {}",
                        link.destination.display()
                    )));
                }
            }
        }
        if let Some(parent) = link.destination.parent() {
            check_directory(parent, &prefix.root)?;
        }
    }
    let receipt_name = "INSTALL_RECEIPT.json";
    let receipt = staged.join(receipt_name);
    if !std::fs::symlink_metadata(&receipt).is_ok_and(|m| m.file_type().is_file()) {
        return Err(InstallError::Failed(
            "abrew: staged receipt is not a regular file".to_string(),
        ));
    }
    let stash = aterm_tempfile::TempDir::new_in(
        staged
            .parent()
            .ok_or_else(|| "abrew: stage has no parent".to_string())?,
    )
    .map_err(|e| format!("abrew: {e}"))?;
    let prepared_receipt = stash.path().join(receipt_name);
    std::fs::rename(receipt, &prepared_receipt).map_err(|e| format!("abrew: {e}"))?;
    let identity = std::fs::symlink_metadata(staged).map_err(|e| format!("abrew: {e}"))?;
    directories.ensure(parent)?;
    super::prefix::publish_directory(staged, live, false)
        .map_err(|e| format!("abrew: cannot publish payload: {e}"))?;
    let mut placed: Vec<(&InstallLink, std::fs::Metadata)> = Vec::new();
    let mut removed = Vec::new();
    let result = (|| {
        for link in links {
            if let Some(parent) = link.destination.parent() {
                directories.ensure(parent)?;
                check_directory(parent, &prefix.root)?;
            }
            if let Some((_, old_target, expected)) = replaced
                .iter()
                .find(|(l, _, _)| l.destination == link.destination)
            {
                if !same_path_identity(&link.destination, expected)
                    || !std::fs::read_link(&link.destination).is_ok_and(|now| now == *old_target)
                    || !replace_root.is_some_and(|root| {
                        replacement_target_owned_at(
                            prefix,
                            &link.destination,
                            old_target,
                            root,
                            relocated,
                        )
                    })
                {
                    return Err(
                        format!("abrew: artifact changed: {}", link.destination.display()).into(),
                    );
                }
                std::fs::remove_file(&link.destination).map_err(|e| format!("abrew: {e}"))?;
                removed.push((link, old_target));
            }
            place(link)?;
            let identity =
                std::fs::symlink_metadata(&link.destination).map_err(|e| format!("abrew: {e}"))?;
            placed.push((link, identity));
        }
        std::fs::hard_link(&prepared_receipt, live.join(receipt_name))
            .map_err(|e| format!("abrew: cannot publish receipt: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        for (link, identity) in placed.into_iter().rev() {
            if link
                .destination
                .parent()
                .is_some_and(|parent| check_directory(parent, &prefix.root).is_ok())
                && same_path_identity(&link.destination, &identity)
                && std::fs::read_link(&link.destination).is_ok_and(|target| target == link.source)
            {
                let _ = std::fs::remove_file(&link.destination);
            }
        }
        for (link, old_target) in removed.into_iter().rev() {
            if link
                .destination
                .parent()
                .is_some_and(|parent| check_directory(parent, &prefix.root).is_ok())
                && std::fs::symlink_metadata(&link.destination)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                let _ = symlink_file(old_target, &link.destination);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if std::fs::symlink_metadata(live)
                .is_ok_and(|now| now.dev() == identity.dev() && now.ino() == identity.ino())
            {
                let _ = super::prefix::publish_directory(live, staged, false);
            }
        }
        #[cfg(not(unix))]
        let _ = identity;
    }
    directories.committed = result.is_ok();
    result
}

/// Removes only empty directories created by this activation when it fails.
/// Existing containers and intervening user data are never removed.
struct CreatedDirectories<'a> {
    prefix: &'a Prefix,
    paths: Vec<(PathBuf, std::fs::Metadata)>,
    committed: bool,
}

impl<'a> CreatedDirectories<'a> {
    fn new(prefix: &'a Prefix) -> Self {
        Self {
            prefix,
            paths: Vec::new(),
            committed: false,
        }
    }

    fn ensure(&mut self, path: &Path) -> Result<(), InstallError> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(format!("abrew: refusing non-directory {}", path.display()).into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    self.ensure(parent)?;
                    check_directory(parent, &self.prefix.root)?;
                }
                std::fs::create_dir(path).map_err(|e| format!("abrew: {e}"))?;
                let identity =
                    std::fs::symlink_metadata(path).map_err(|e| format!("abrew: {e}"))?;
                self.paths.push((path.to_path_buf(), identity));
                Ok(())
            }
            Err(error) => Err(format!("abrew: {error}").into()),
        }
    }
}

impl Drop for CreatedDirectories<'_> {
    fn drop(&mut self) {
        if !self.committed {
            for (path, identity) in self.paths.iter().rev() {
                if path
                    .parent()
                    .is_some_and(|p| check_directory(p, &self.prefix.root).is_ok())
                    && same_path_identity(path, identity)
                {
                    // remove_dir refuses a nonempty directory atomically.
                    let _ = std::fs::remove_dir(path);
                }
            }
        }
    }
}

fn ensure_directory(path: &Path) -> Result<(), InstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(InstallError::Failed(format!(
            "abrew: refusing non-directory {}",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                ensure_directory(parent)?;
            }
            std::fs::create_dir(path).map_err(|e| format!("abrew: {e}"))?;
            Ok(())
        }
        Err(e) => Err(InstallError::Failed(format!("abrew: {e}"))),
    }
}

/// Returns whether `link` resolves lexically under `root` without following foreign symlinks.
pub(super) fn link_points_into(link: &Path, root: &Path) -> bool {
    std::fs::read_link(link).is_ok_and(|target| link_target_points_into(link, &target, root))
}

/// Returns whether `link` resolves lexically to `expected`.
pub(super) fn link_points_to(link: &Path, expected: &Path) -> Option<bool> {
    match std::fs::read_link(link) {
        Ok(target) => Some(
            resolved_link_target(link, &target)
                .zip(normalize_link_path(expected))
                .is_some_and(|(target, expected)| target == expected),
        ),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Some(false)
        }
        Err(_) => None,
    }
}

/// Returns whether `link`'s target `target` resolves lexically under `root`.
pub(super) fn link_target_points_into(link: &Path, target: &Path, root: &Path) -> bool {
    resolved_link_target(link, target)
        .zip(normalize_link_path(root))
        .is_some_and(|(target, root)| target.starts_with(root))
}

fn normalize_link_path(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

fn resolved_link_target(link: &Path, target: &Path) -> Option<PathBuf> {
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    normalize_link_path(&target)
}

/// Returns whether `target` under `cellar` is an admitted receipt-owned keg of `name`.
pub(super) fn replacement_target_owned(
    prefix: &Prefix,
    link: &Path,
    target: &Path,
    root: &Path,
) -> bool {
    replacement_target_owned_at(prefix, link, target, root, None)
}

fn replacement_target_owned_at(
    prefix: &Prefix,
    link: &Path,
    target: &Path,
    root: &Path,
    relocated: Option<&RelocatedKeg>,
) -> bool {
    let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !super::valid_component(name) {
        return false;
    }
    let kind = if root == prefix.cellar().join(name) {
        Kind::Formula
    } else if root == prefix.caskroom().join(name) {
        Kind::Cask
    } else {
        return false;
    };
    let Some(target) = resolved_link_target(link, target) else {
        return false;
    };
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    let Some(std::path::Component::Normal(version)) = relative.components().next() else {
        return false;
    };
    let Some(version) = version.to_str().filter(|version| match kind {
        Kind::Formula => super::valid_component(version),
        Kind::Cask => super::valid_cask_version(version),
    }) else {
        return false;
    };
    let version_dir = root.join(version);
    let receipt_dir = match relocated.filter(|old| old.live == version_dir) {
        Some(old) if same_path_identity(&old.aside, &old.identity) => &old.aside,
        Some(_) => return false,
        None => &version_dir,
    };
    check_directory(receipt_dir, &prefix.root).is_ok()
        && check_directory(&version_dir, &prefix.root).is_ok()
        && target
            .parent()
            .is_some_and(|parent| check_directory(parent, &prefix.root).is_ok())
        && super::prefix::owned_receipt(receipt_dir, kind, name, version)
}

/// Points `opt/<name>` at the newest admitted keg and links its files unless keg-only.
pub fn relink(prefix: &Prefix, f: &Formula) -> Result<usize, InstallError> {
    let inst = prefix
        .installed_one(Kind::Formula, &f.name)
        .ok_or_else(|| InstallError::Failed(format!("abrew: {} is not installed", f.name)))?;
    let payload = prefix.keg(&f.name, &inst.version);
    admit_keg(prefix, &payload)?;
    with_shared_namespaces(prefix, (!f.keg_only).then_some(payload.as_path()), || {
        let (keg, links) = activation_links(prefix, &f.name, &inst.version, f.keg_only)?;
        // Brew's count is every keg symlink placed; the plan's
        // leading entry is the opt link, which brew does not count.
        let created = links.len().saturating_sub(1);
        activate_keg(prefix, &keg, &links, false)?;
        Ok(created)
    })
}

/// Returns whether all files in the keg's link plan already point into the keg.
pub fn is_linked(prefix: &Prefix, name: &str, version: &str) -> bool {
    keg_was_linked(prefix, name, version).unwrap_or(false)
}

/// Returns the destination paths that linking `f` would create (dry-run).
pub fn link_destinations(prefix: &Prefix, f: &Formula) -> Result<Vec<PathBuf>, InstallError> {
    let inst = prefix
        .installed_one(Kind::Formula, &f.name)
        .ok_or_else(|| InstallError::Failed(format!("abrew: {} is not installed", f.name)))?;
    if f.keg_only {
        return Ok(Vec::new());
    }
    let keg = prefix.keg(&f.name, &inst.version);
    Ok(keg_link_plan(prefix, &keg)?
        .into_iter()
        .map(|l| l.destination)
        .collect())
}

/// Returns the prefix links that unlinking `name` would remove (dry-run).
pub fn unlink_targets(prefix: &Prefix, name: &str) -> Result<Vec<PathBuf>, String> {
    owned_keg_links(prefix, name, false)
}

/// Switches `opt/<name>` and prefix links to `version`.
pub(super) fn switch_keg(
    prefix: &Prefix,
    name: &str,
    version: &str,
    keg_only: bool,
) -> Result<(), InstallError> {
    let payload = prefix.keg(name, version);
    admit_keg(prefix, &payload)?;
    with_shared_namespaces(prefix, (!keg_only).then_some(payload.as_path()), || {
        let (keg, links) = activation_links(prefix, name, version, keg_only)?;
        activate_keg(prefix, &keg, &links, true)
    })
}

fn activation_links(
    prefix: &Prefix,
    name: &str,
    version: &str,
    keg_only: bool,
) -> Result<(PathBuf, Vec<InstallLink>), InstallError> {
    let (keg, opt) = prepare_opt(prefix, name, version)?;
    let mut links = vec![InstallLink {
        source: keg.clone(),
        destination: opt,
        directory: true,
    }];
    if !keg_only {
        links.extend(keg_link_plan(prefix, &keg)?);
    }
    Ok((keg, links))
}

#[cfg(all(test, unix))]
fn point_opt(prefix: &Prefix, name: &str, version: &str) -> Result<(), InstallError> {
    let (keg, links) = activation_links(prefix, name, version, true)?;
    activate_keg(prefix, &keg, &links, false)
}

fn prepare_opt(
    prefix: &Prefix,
    name: &str,
    version: &str,
) -> Result<(PathBuf, PathBuf), InstallError> {
    let keg = prefix.keg(name, version);
    admit_keg(prefix, &keg)?;
    let opt = prefix.opt(name);
    let parent = prefix.root.join("opt");
    check_directory(&parent, &prefix.root)?;
    ensure_directory(&parent)?;
    match std::fs::symlink_metadata(&opt) {
        Ok(_) if !prefix_owned_target(&opt, &prefix.cellar().join(name)) || !opt.is_dir() => {
            return Err(InstallError::Failed(format!(
                "abrew: refusing foreign opt link {}",
                opt.display()
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("abrew: {e}").into()),
    }
    Ok((keg, opt))
}

fn admit_keg(prefix: &Prefix, keg: &Path) -> Result<(), InstallError> {
    let cellar = prefix.cellar();
    let relative = keg
        .strip_prefix(&cellar)
        .map_err(|_| "abrew: keg escapes cellar".to_string())?;
    let mut parts = relative.components();
    let (
        Some(std::path::Component::Normal(name)),
        Some(std::path::Component::Normal(version)),
        None,
    ) = (parts.next(), parts.next(), parts.next())
    else {
        return Err("abrew: invalid keg path".to_string().into());
    };
    let (Some(name), Some(version)) = (name.to_str(), version.to_str()) else {
        return Err("abrew: invalid keg name or version".to_string().into());
    };
    if !super::valid_component(name)
        || !super::valid_cask_version(version)
        || prefix.keg(name, version).as_os_str() != keg.as_os_str()
        || !prefix
            .installed_one(Kind::Formula, name)
            .is_some_and(|inst| inst.versions.iter().any(|v| v == version))
    {
        return Err("abrew: keg has no owned install receipt".to_string().into());
    }
    check_directory(keg, &prefix.root)
}

/// Links a keg's payload directories into the prefix, returning the number of links created.
pub fn link_keg(prefix: &Prefix, keg: &Path, _overwrite: bool) -> Result<(), InstallError> {
    admit_keg(prefix, keg)?;
    with_shared_namespaces(prefix, Some(keg), || {
        let links = keg_link_plan(prefix, keg)?;
        activate_keg(prefix, keg, &links, false)
    })
}

fn keg_link_plan(prefix: &Prefix, keg: &Path) -> Result<Vec<InstallLink>, InstallError> {
    admit_keg(prefix, keg)?;
    let mut links = Vec::new();
    plan_links(keg, keg, &prefix.root, &mut links)?;
    Ok(links)
}

fn snapshot_keg_link(
    prefix: &Prefix,
    keg: &Path,
    destination: &Path,
) -> Result<Option<(PathBuf, std::fs::Metadata)>, InstallError> {
    if let Some(parent) = destination.parent() {
        check_directory(parent, &prefix.root)?;
    }
    match std::fs::symlink_metadata(destination) {
        Ok(_) if !prefix_owned_target(destination, keg.parent().unwrap_or(keg)) => {
            Err(InstallError::Failed(format!(
                "abrew: refusing foreign link destination {}",
                destination.display()
            )))
        }
        Ok(identity) => {
            let target = std::fs::read_link(destination).map_err(|e| format!("abrew: {e}"))?;
            Ok(Some((target, identity)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("abrew: {e}").into()),
    }
}

fn activate_keg(
    prefix: &Prefix,
    keg: &Path,
    links: &[InstallLink],
    remove_old: bool,
) -> Result<(), InstallError> {
    activate_keg_with(prefix, keg, links, remove_old, |link| {
        if link.directory {
            symlink_dir(&link.source, &link.destination)
        } else {
            symlink_file(&link.source, &link.destination)
        }
    })
}

fn activate_keg_with(
    prefix: &Prefix,
    keg: &Path,
    links: &[InstallLink],
    remove_old: bool,
    mut place: impl FnMut(&InstallLink) -> Result<(), String>,
) -> Result<(), InstallError> {
    let mut directories = CreatedDirectories::new(prefix);
    struct Change<'a> {
        before: Option<(PathBuf, std::fs::Metadata)>,
        after: Option<&'a InstallLink>,
    }
    let mut changes = BTreeMap::new();
    if remove_old {
        let name = keg
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| "abrew: invalid keg name".to_string())?;
        for path in owned_keg_links(prefix, name, true)? {
            changes.insert(
                path,
                Change {
                    before: None,
                    after: None,
                },
            );
        }
    }
    for link in links {
        changes
            .entry(link.destination.clone())
            .or_insert(Change {
                before: None,
                after: None,
            })
            .after = Some(link);
    }
    for (path, change) in &mut changes {
        change.before = snapshot_keg_link(prefix, keg, path)?;
        if let Some(parent) = path.parent() {
            directories.ensure(parent)?;
        }
    }
    let mut applied = Vec::new();
    let result: Result<(), InstallError> = (|| {
        for (path, change) in &changes {
            if let Some(parent) = path.parent() {
                check_directory(parent, &prefix.root)?;
            }
            if let Some((target, identity)) = &change.before {
                if !(same_path_identity(path, identity)
                    && std::fs::read_link(path).is_ok_and(|actual| &actual == target))
                {
                    return Err(format!(
                        "abrew: link changed before activation: {}",
                        path.display()
                    )
                    .into());
                }
                std::fs::remove_file(path).map_err(|e| format!("abrew: {e}"))?;
            } else {
                match std::fs::symlink_metadata(path) {
                    Ok(_) => {
                        return Err(format!(
                            "abrew: link appeared before activation: {}",
                            path.display()
                        )
                        .into());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("abrew: {e}").into()),
                }
            }
            applied.push((path, change, None));
            if let Some(link) = change.after {
                place(link)?;
                let identity =
                    std::fs::symlink_metadata(path).map_err(|e| format!("abrew: {e}"))?;
                if std::fs::read_link(path).is_ok_and(|target| target == link.source) {
                    if let Some((_, _, placed)) = applied.last_mut() {
                        *placed = Some(identity);
                    }
                } else {
                    return Err(format!("abrew: placed link changed: {}", path.display()).into());
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut refused = Vec::new();
        for (path, change, placed) in applied.into_iter().rev() {
            let restored: Result<(), InstallError> = (|| {
                if let Some(parent) = path.parent() {
                    check_directory(parent, &prefix.root)?;
                }
                match std::fs::symlink_metadata(path) {
                    Ok(_)
                        if placed
                            .as_ref()
                            .is_some_and(|identity| same_path_identity(path, identity))
                            && change.after.is_some_and(|link| {
                                std::fs::read_link(path).is_ok_and(|actual| actual == link.source)
                            }) =>
                    {
                        std::fs::remove_file(path).map_err(|e| format!("abrew: {e}"))?;
                    }
                    Ok(_) => return Err("abrew: alias changed during rollback".to_string().into()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("abrew: {e}").into()),
                }
                if let Some((target, _)) = &change.before {
                    symlink_dir(target, path)?;
                }
                Ok(())
            })();
            if restored.is_err() {
                refused.push(path.display().to_string());
            }
        }
        return if refused.is_empty() {
            Err(error)
        } else {
            let message = error.message();
            Err(InstallError::Failed(format!(
                "{message}; could not restore aliases: {}",
                refused.join(", ")
            )))
        };
    }
    directories.committed = true;
    Ok(())
}

/// Returns whether `path` still has the same device, inode, and file type as `expected`.
pub(super) fn same_path_identity(path: &Path, expected: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).is_ok_and(|actual| {
            actual.dev() == expected.dev()
                && actual.ino() == expected.ino()
                && actual.file_type() == expected.file_type()
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected);
        false
    }
}

/// Removes all prefix symlinks owned by `name`'s kegs, returning the count removed.
pub fn unlink_keg(prefix: &Prefix, name: &str) -> Result<usize, String> {
    let links = owned_keg_links(prefix, name, false)?;
    let removed = links.len();
    for link in links {
        std::fs::remove_file(link).map_err(|e| format!("abrew: {e}"))?;
    }
    Ok(removed)
}

/// Removes `opt/<name>` if it is an abrew-owned link into `name`'s kegs.
pub fn remove_owned_opt_link(prefix: &Prefix, name: &str) -> Result<(), String> {
    if !super::valid_component(name) {
        return Err("abrew: invalid formula name".to_string());
    }
    let opt = prefix.opt(name);
    if prefix_owned_target(&opt, &prefix.cellar().join(name)) {
        std::fs::remove_file(&opt).map_err(|e| format!("abrew: {e}"))?;
    }
    Ok(())
}

fn owned_keg_links(prefix: &Prefix, name: &str, include_opt: bool) -> Result<Vec<PathBuf>, String> {
    let versions: Vec<String> = prefix
        .installed_one(Kind::Formula, name)
        .map(|inst| inst.versions)
        .unwrap_or_default();
    owned_keg_links_for(prefix, name, &versions, include_opt)
}

/// The prefix symlinks owned by specific installed versions
/// of `name`, in brew's `Keg.keg_link_directories` order
/// (sorted within each directory, as `Find` yields them).
pub(super) fn owned_keg_links_for(
    prefix: &Prefix,
    name: &str,
    versions: &[String],
    include_opt: bool,
) -> Result<Vec<PathBuf>, String> {
    if !super::valid_component(name) {
        return Err("abrew: invalid formula name".to_string());
    }
    check_directory(&prefix.cellar().join(name), &prefix.root).map_err(|e| e.message())?;
    let cellar = prefix.cellar().join(name);
    let versions: Vec<PathBuf> = versions.iter().map(|v| cellar.join(v)).collect();
    let mut links = Vec::new();
    for sub in [
        "bin",
        "etc",
        "include",
        "lib",
        "sbin",
        "share",
        "var",
        "Frameworks",
    ] {
        let mut found = Vec::new();
        owned_links(&prefix.root.join(sub), &cellar, &versions, &mut found)?;
        found.sort();
        links.extend(found);
    }
    if include_opt {
        let opt = prefix.opt(name);
        if prefix_owned_target_with(&opt, &cellar, &versions) {
            links.push(opt);
        }
    }
    Ok(links)
}

fn owned_links(
    dir: &Path,
    cellar: &Path,
    versions: &[PathBuf],
    links: &mut Vec<PathBuf>,
) -> Result<(), String> {
    // Do not traverse a foreign directory symlink, including the top-level root.
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()) {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("abrew: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("abrew: {e}"))?;
        let p = entry.path();
        let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
        if kind.is_symlink() && prefix_owned_target_with(&p, cellar, versions) {
            links.push(p);
        } else if kind.is_dir() {
            owned_links(&p, cellar, versions, links)?;
        }
    }
    Ok(())
}

fn prefix_owned_target_with(link: &Path, package: &Path, versions: &[PathBuf]) -> bool {
    let Ok(target) = std::fs::read_link(link) else {
        return false;
    };
    link_target_points_into(link, &target, package)
        && versions
            .iter()
            .any(|version| link_target_points_into(link, &target, version))
}

fn prefix_owned_target(link: &Path, package: &Path) -> bool {
    let Ok(target) = std::fs::read_link(link) else {
        return false;
    };
    if !link_target_points_into(link, &target, package) {
        return false;
    }
    let Some(name) = package.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(root) = package.parent().and_then(Path::parent) else {
        return false;
    };
    Prefix::at(root)
        .installed_one(Kind::Formula, name)
        .is_some_and(|inst| {
            inst.versions
                .iter()
                .any(|version| link_target_points_into(link, &target, &package.join(version)))
        })
}

/// Removes dangling prefix symlinks pointing into `name`'s missing kegs.
pub fn remove_dangling_links(prefix: &Prefix, name: &str) -> Result<usize, String> {
    if !super::valid_component(name) {
        return Err("abrew: invalid formula name".to_string());
    }
    let cellar_name = prefix.cellar().join(name);
    let linked = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    if linked(&prefix.cellar()) || linked(&cellar_name) {
        return Ok(0);
    }
    fn walk(dir: &Path, cellar_name: &Path, removed: &mut usize) -> Result<(), String> {
        if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()) {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir).map_err(|e| format!("abrew: {e}"))? {
            let entry = entry.map_err(|e| format!("abrew: {e}"))?;
            let p = entry.path();
            let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
            if kind.is_symlink() {
                if link_points_into(&p, cellar_name) && !p.exists() {
                    std::fs::remove_file(&p).map_err(|e| format!("abrew: {e}"))?;
                    *removed += 1;
                }
            } else if kind.is_dir() {
                walk(&p, cellar_name, removed)?;
            }
        }
        Ok(())
    }
    let mut removed = 0;
    for sub in ["bin", "sbin", "lib", "include", "share", "etc", "opt"] {
        walk(&prefix.root.join(sub), &cellar_name, &mut removed)?;
    }
    Ok(removed)
}

/// Download (or reuse) the manifest and bottle. When
/// `announce`, each is reported like brew's download queue (a
/// manifest only when this call fetched it, the bottle always).
fn fetch_bottle(
    prefix: &Prefix,
    f: &Formula,
    quiet: bool,
    announce: bool,
) -> Result<(PathBuf, String), InstallError> {
    let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags()).cloned() else {
        return Err(InstallError::Unsupported(format!(
            "no bottle for {} on this platform",
            f.name
        )));
    };
    if fetch_bottle_manifest(prefix, f)? && announce && !quiet {
        eprintln!("✔︎ Bottle Manifest {} ({})", f.name, keg_version(f));
    }
    let bottle_tag = bottle.tag.clone();
    let dest = bottle_dest(prefix, f, &bottle);
    let (url, sha) = (bottle.url, bottle.sha256);
    let version = keg_version(f);
    if fetch::verify_digest_strict(&dest, &sha).is_err() {
        let mut headers = Vec::new();
        if let Some(repo) = super::api::ghcr_repo_of(&url) {
            headers.push(format!(
                "Authorization: Bearer {}",
                super::api::ghcr_token(&repo)?
            ));
        }
        let hrefs: Vec<&str> = headers.iter().map(String::as_str).collect();
        fetch::curl_to_file(&url, &dest, &hrefs)?;
    }
    fetch::verify_digest_strict(&dest, &sha)?;
    if announce && !quiet {
        eprintln!("✔︎ Bottle {} ({version})", f.name);
    }
    Ok((dest, bottle_tag))
}

fn pour_bottle(
    prefix: &Prefix,
    f: &Formula,
    staging: &Path,
    quiet: bool,
    prefetched: bool,
) -> Result<PathBuf, InstallError> {
    let (dest, bottle_tag) = fetch_bottle(prefix, f, quiet, !prefetched)?;
    let version = keg_version(f);
    if !quiet {
        println!(
            "{}",
            super::tty::ohai(&format!("Pouring {}", bottle_filename(f, &bottle_tag)))
        );
    }
    fetch::untar(&dest, staging)?;
    selected_bottle_payload(staging, &f.name, &version)
}

fn selected_bottle_payload(
    staging: &Path,
    name: &str,
    version: &str,
) -> Result<PathBuf, InstallError> {
    if !super::valid_component(name) || !super::valid_cask_version(version) {
        return Err("abrew: invalid bottle subtree".to_string().into());
    }
    let inner = staging.join(name).join(version);
    for path in [staging.to_path_buf(), staging.join(name), inner.clone()] {
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
            return Err(InstallError::Failed(format!(
                "abrew: bottle subtree is not a physical directory: {}",
                path.display()
            )));
        }
    }
    let mut pending = vec![inner.clone()];
    let mut seen = 0u64;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("abrew: {e}"))? {
            let entry = entry.map_err(|e| format!("abrew: {e}"))?;
            seen = seen.saturating_add(1);
            if seen > crate::install::MAX_ENTRIES {
                return Err("abrew: bottle subtree entry limit exceeded"
                    .to_string()
                    .into());
            }
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path).map_err(|e| format!("abrew: {e}"))?;
                let relative = path
                    .strip_prefix(&inner)
                    .map_err(|e| format!("abrew: {e}"))?;
                if !matches!(
                    crate::extract::vet_symlink(&inner, relative, &target, 0),
                    Ok(Some(_))
                ) {
                    return Err(InstallError::Failed(format!(
                        "abrew: bottle link escapes the selected payload: {}",
                        path.display()
                    )));
                }
            } else if !kind.is_file() {
                return Err(InstallError::Failed(format!(
                    "abrew: bottle contains a nonregular entry: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(inner)
}

/// Uninstalls `inst`'s receipt-admitted kegs and removes `opt/<name>` and any pin.
pub fn uninstall_versions(prefix: &Prefix, inst: &Installed, force: bool) -> Vec<String> {
    if force {
        return inst.versions.clone();
    }
    let default = prefix
        .optlinked_version(&inst.name)
        .filter(|v| inst.versions.contains(v))
        .unwrap_or_else(|| inst.version.clone());
    vec![default]
}

fn unlink_keg_version(prefix: &Prefix, name: &str, version: &str) -> Result<(), String> {
    let keg = prefix.keg(name, version);
    for link in owned_keg_links(prefix, name, false)? {
        if link_points_into(&link, &keg) {
            std::fs::remove_file(&link).map_err(|e| format!("abrew: {e}"))?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn uninstall(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    ignore_dependencies: bool,
    force: bool,
    dry_run: bool,
) -> Result<(), InstallError> {
    // An alias names its canonical formula's keg, as in brew.
    let canon =
        super::commands::canonical_installed_name(prefix, api, Formula::short_name(name), false);
    let short = canon.as_str();
    let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
        // Brew's resolution order: a known formula is a missing
        // keg, a known cask is its not-installed error, and a
        // name nothing answers to is unavailable.
        let sentence = if api.formula(name).is_ok() || super::tap::lookup(prefix, name).is_some() {
            format!("No such keg: {}", prefix.cellar().join(short).display())
        } else if api.cask(short).is_ok() {
            format!("Cask '{short}' is not installed.")
        } else {
            super::commands::formula_unavailable(api, name)
        };
        return Err(InstallError::Failed(format!("Error: {sentence}")));
    };
    if !ignore_dependencies {
        let deps_on = dependents(api, prefix, short)?;
        if !deps_on.is_empty() {
            return Err(InstallError::Failed(format!(
                "Error: Refusing to uninstall {}\nbecause it is required by {}, which {} currently installed.\nYou can override this and force removal with:\n  brew uninstall --ignore-dependencies {short}",
                prefix.keg(short, &inst.version).display(),
                super::commands::to_sentence(&deps_on),
                if deps_on.len() == 1 { "is" } else { "are" }
            )));
        }
    }
    let pinned = prefix.pinned_version(short);
    if pinned.is_some() && !force {
        return Err(InstallError::Failed(format!(
            "Error: {short} is pinned. You must unpin it to uninstall."
        )));
    }
    let targets = uninstall_versions(prefix, &inst, force);
    if dry_run {
        // Brew: one line per keg, then the links its unlink
        // would remove.
        for version in &targets {
            println!("Would uninstall {short} {version}");
            let links = owned_keg_links_for(prefix, short, std::slice::from_ref(version), false)?;
            for link in links {
                println!("{}", link.display());
            }
        }
        return Ok(());
    }
    let opt_version = prefix.optlinked_version(short);
    for version in &targets {
        unlink_keg_version(prefix, short, version)?;
    }
    if force || opt_version.is_some_and(|v| targets.contains(&v)) {
        remove_owned_opt_link(prefix, short)?;
    }
    let base = prefix.cellar().join(short);
    if (force || targets.len() >= inst.versions.len())
        && (pinned.is_some() || prefix.pin_link_dangling_ours(short))
    {
        prefix.unpin(short)?;
    }
    for version in &targets {
        std::fs::remove_dir_all(base.join(version)).map_err(|e| format!("abrew: {e}"))?;
    }
    let _ = std::fs::remove_dir(&base);
    prefix.rebuild_state()?;
    Ok(())
}

/// Downloads and verifies `f`'s bottle or source tarball into the cache without installing.
/// The OCI bottle manifest brew downloads beside every GHCR
/// bottle (its annotations carry the tab attributes). Cached as
/// `{name}-{keg_version}.bottle_manifest.json`; `Ok(false)` when
/// the bottle has no GHCR manifest resource (non-GHCR root).
/// Brew `Homebrew::Ask.confirm?`: on a terminal, show the
/// proceed prompt and read single characters; only `y`
/// proceeds. Without a terminal it never prompts.
pub(super) fn ask_to_proceed(action: &str) -> Result<(), InstallError> {
    ask_confirm(action).unwrap_or(Ok(()))
}

/// `Ask.confirm?` as a value: `None` without a terminal (no
/// prompt shown), `Some(Ok(()))` on `y`, `Some(Err(..))` on
/// decline/EOF (brew exits 1).
pub(super) fn ask_confirm(action: &str) -> Option<Result<(), InstallError>> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    Some(ask_confirm_tty(action))
}

fn ask_confirm_tty(action: &str) -> Result<(), InstallError> {
    println!(
        "{}",
        super::tty::ohai(&format!("Do you want to proceed with the {action}? [y/n]"))
    );
    // Single-character reads, like IO#getch: raw mode for the
    // prompt's duration so y/n register without Enter.
    #[cfg(unix)]
    struct RawMode(libc::termios);
    #[cfg(unix)]
    impl RawMode {
        fn engage() -> Option<Self> {
            unsafe {
                let mut prior: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &raw mut prior) != 0 {
                    return None;
                }
                let mut raw = prior;
                libc::cfmakeraw(&raw mut raw);
                if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const raw) == 0 {
                    Some(Self(prior))
                } else {
                    None
                }
            }
        }
    }
    #[cfg(unix)]
    impl Drop for RawMode {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const self.0);
            }
        }
    }
    // No termios off unix: reads stay line-buffered there.
    #[cfg(not(unix))]
    struct RawMode;
    #[cfg(not(unix))]
    impl RawMode {
        fn engage() -> Option<Self> {
            Some(Self)
        }
    }
    let mut stdin = std::io::stdin().lock();
    use std::io::Read as _;
    loop {
        // Raw mode per read, restored between reads like IO#getch.
        let _raw = RawMode::engage();
        let mut byte = [0u8; 1];
        let read = stdin.read(&mut byte);
        drop(_raw);
        match read {
            Ok(1) => match byte[0] {
                b'y' | b'Y' => return Ok(()),
                b'n' | b'N' | 0x1b | 0x03 | 0x04 => return Err(InstallError::Aborted),
                _ => println!("Invalid input. Please press 'y' to proceed, or 'n' to abort."),
            },
            _ => return Err(InstallError::Aborted),
        }
    }
}

pub(super) fn bottle_manifest_dest(prefix: &Prefix, f: &Formula) -> PathBuf {
    let filename = format!("{}-{}.bottle_manifest.json", f.name, keg_version(f));
    let url = f
        .bottle_for(&Machine::this().bottle_tags())
        .and_then(|bottle| super::api::ghcr_repo_of(&bottle.url))
        .map(|repo| {
            let version = keg_version(f);
            let tag = if f.bottle_rebuild > 0 {
                format!("{version}-{}", f.bottle_rebuild)
            } else {
                version
            };
            format!("https://ghcr.io/v2/{repo}/manifests/{tag}")
        });
    match url {
        Some(url) => download_dest(prefix, &url, &filename),
        None => prefix.cache().join(filename),
    }
}

/// Whether a usable bottle manifest is already in the cache
/// (brew only downloads — and only reports — the ones missing).
/// The bottle tab's runtime-dependency minimums for `f`'s
/// bottle on this machine: short name -> built-against
/// `pkg_version`. Brew keeps an installed dep only when it is
/// at the latest version or at/above this minimum. Empty when
/// no tab can be read (callers then keep installed deps).
pub(super) fn bottle_tab_minimums(
    prefix: &Prefix,
    f: &Formula,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags()).cloned() else {
        return out;
    };
    if !bottle_manifest_cached(prefix, f) {
        let _ = fetch_bottle_manifest(prefix, f);
    }
    let Ok(bytes) =
        crate::metadata_io::read_bounded_regular(&bottle_manifest_dest(prefix, f), 8 * 1024 * 1024)
    else {
        return out;
    };
    let Ok(index) = aterm_json::from_slice::<aterm_json::Value>(&bytes) else {
        return out;
    };
    let Some(manifests) = index.get("manifests").and_then(aterm_json::Value::as_array) else {
        return out;
    };
    let tab_text = manifests.iter().find_map(|m| {
        let annotations = m.get("annotations")?;
        let digest = annotations
            .get("sh.brew.bottle.digest")
            .and_then(aterm_json::Value::as_str)?;
        (digest == bottle.sha256).then(|| {
            annotations
                .get("sh.brew.tab")
                .and_then(aterm_json::Value::as_str)
                .map(str::to_owned)
        })?
    });
    let Some(tab_text) = tab_text else {
        return out;
    };
    let Ok(tab) = aterm_json::from_slice::<aterm_json::Value>(tab_text.as_bytes()) else {
        return out;
    };
    if let Some(deps) = tab
        .get("runtime_dependencies")
        .and_then(aterm_json::Value::as_array)
    {
        for dep in deps {
            if let (Some(full), Some(pkg)) = (
                dep.get("full_name").and_then(aterm_json::Value::as_str),
                dep.get("pkg_version").and_then(aterm_json::Value::as_str),
            ) {
                out.insert(Formula::short_name(full).to_string(), pkg.to_string());
            }
        }
    }
    out
}

pub fn bottle_manifest_cached(prefix: &Prefix, f: &Formula) -> bool {
    if !super::valid_component(&f.name) || !super::valid_cask_version(&keg_version(f)) {
        return false;
    }
    crate::metadata_io::read_head(&bottle_manifest_dest(prefix, f), 1)
        .is_ok_and(|b| b.starts_with(b"{"))
}

/// Fetch the bottle manifest when missing; `true` when this call
/// downloaded it (the case brew reports with a ✔ line).
pub fn fetch_bottle_manifest(prefix: &Prefix, f: &Formula) -> Result<bool, InstallError> {
    if !super::valid_component(&f.name) || !super::valid_cask_version(&keg_version(f)) {
        return Err(InstallError::Unsupported(
            "unsupported formula name/version".to_string(),
        ));
    }
    let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags()).cloned() else {
        return Ok(false);
    };
    let Some(repo) = super::api::ghcr_repo_of(&bottle.url) else {
        return Ok(false);
    };
    let version = keg_version(f);
    let tag = if f.bottle_rebuild > 0 {
        format!("{version}-{}", f.bottle_rebuild)
    } else {
        version.clone()
    };
    let dest = bottle_manifest_dest(prefix, f);
    if bottle_manifest_cached(prefix, f) {
        return Ok(false);
    }
    prefix.ensure()?;
    let headers = [
        format!("Authorization: Bearer {}", super::api::ghcr_token(&repo)?),
        "Accept: application/vnd.oci.image.index.v1+json".to_string(),
    ];
    let hrefs: Vec<&str> = headers.iter().map(String::as_str).collect();
    let url = format!("https://ghcr.io/v2/{repo}/manifests/{tag}");
    fetch::curl_to_file(&url, &dest, &hrefs)?;
    Ok(true)
}

pub fn fetch_only(prefix: &Prefix, f: &Formula) -> Result<PathBuf, InstallError> {
    if !super::valid_component(&f.name) || !super::valid_cask_version(&keg_version(f)) {
        return Err(InstallError::Unsupported(
            "unsupported formula name/version".to_string(),
        ));
    }
    prefix.ensure()?;
    fetch_bottle_manifest(prefix, f)?;
    let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags()).cloned() else {
        return Err(InstallError::Unsupported(format!(
            "no bottle for {} on this platform; fetch -s for source",
            f.name
        )));
    };
    let dest = bottle_dest(prefix, f, &bottle);
    let (url, sha) = (bottle.url, bottle.sha256);
    if !(dest.exists() && fetch::verify_digest_strict(&dest, &sha).is_ok()) {
        let mut headers: Vec<String> = Vec::new();
        if let Some(repo) = super::api::ghcr_repo_of(&url) {
            headers.push(format!(
                "Authorization: Bearer {}",
                super::api::ghcr_token(&repo)?
            ));
        }
        let hrefs: Vec<&str> = headers.iter().map(String::as_str).collect();
        fetch::curl_to_file(&url, &dest, &hrefs)?;
    }
    fetch::verify_digest_strict(&dest, &sha)?;
    Ok(dest)
}

/// Loads runtime dependency lists for all installed formulae.
pub(super) fn dependency_books(
    installed: &[Installed],
    mut read: impl FnMut(&str) -> Result<Formula, String>,
) -> Result<Vec<(Installed, Formula)>, String> {
    installed.iter().map(|i| {
        let formula = read(&i.name)?;
        if formula.name != i.name {
            return Err(format!("abrew: dependency metadata does not name {}", i.name));
        }
        if !super::valid_cask_version(&formula.version) {
            return Err("abrew: invalid formula version".to_string());
        }
        let version = keg_version(&formula);
        if i.versions.is_empty() || i.versions.iter().any(|installed| *installed != version) {
            return Err(format!("abrew: dependency metadata for {} covers {version}, not every installed version; automatic deletion refused (explicit uninstall --ignore-dependencies is separate)", i.name));
        }
        Ok((i.clone(), formula))
    }).collect()
}

/// Returns installed formulae that declare `name` as a runtime dependency.
pub fn dependents(api: &Api, prefix: &Prefix, name: &str) -> Result<Vec<String>, InstallError> {
    let mut installed = prefix.installed_checked(Kind::Formula)?;
    installed.retain(|inst| inst.name != name);
    let books = dependency_books(&installed, |n| api.dependency_formula(n))?;
    Ok(books
        .into_iter()
        .filter(|(_, f)| {
            f.dependencies
                .iter()
                .any(|d| Formula::short_name(d) == name)
        })
        .map(|(inst, _)| inst.name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::extract::fixtures::{gzip_bytes, tar_bytes};

    #[test]
    #[cfg(unix)]
    fn prefix_symlinks_are_relativized_and_strays_still_delegate() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(staged.join("lib")).unwrap();
        symlink_file(
            Path::new("@@HOMEBREW_CELLAR@@/demo/1/lib/libdemo.dylib"),
            &staged.join("lib/libdemo.1.dylib"),
        )
        .unwrap();
        symlink_file(
            Path::new("/opt/homebrew/opt/helper/bin/helper"),
            &staged.join("lib/helper"),
        )
        .unwrap();
        symlink_file(
            Path::new("@@HOMEBREW_JAVA@@/bin/java"),
            &staged.join("lib/java"),
        )
        .unwrap();
        symlink_file(
            Path::new("/usr/lib/libSystem.B.dylib"),
            &staged.join("lib/sys"),
        )
        .unwrap();
        let f = Formula {
            name: "demo".to_string(),
            ..Formula::default()
        };
        let keg = prefix.keg("demo", "1");
        relativize_prefix_symlinks(&prefix, &f, &staged, &keg).unwrap();
        // From <keg>/lib into the same directory: a bare name.
        assert_eq!(
            std::fs::read_link(staged.join("lib/libdemo.1.dylib")).unwrap(),
            Path::new("libdemo.dylib")
        );
        // From <keg>/lib to <prefix>/opt/helper/bin/helper: four `..`.
        assert_eq!(
            std::fs::read_link(staged.join("lib/helper")).unwrap(),
            Path::new("../../../../opt/helper/bin/helper")
        );
        // Unmapped system links are left alone; the java placeholder
        // remains for the leftover scan to delegate.
        assert_eq!(
            std::fs::read_link(staged.join("lib/sys")).unwrap(),
            Path::new("/usr/lib/libSystem.B.dylib")
        );
        let leftovers = linkage_leftovers(&staged);
        assert_eq!(leftovers.len(), 1);
        assert!(leftovers[0].contains("placeholder symlink"));
    }

    #[test]
    fn text_placeholders_are_substituted_in_text_files_only() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(staged.join("etc/profile.d")).unwrap();
        let script = staged.join("etc/profile.d/demo.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n. @@HOMEBREW_CELLAR@@/demo/1/etc/demo\n",
        )
        .unwrap();
        let binary = staged.join("blob.bin");
        let mut raw = b"@@HOMEBREW_PREFIX@@".to_vec();
        raw.push(0);
        std::fs::write(&binary, &raw).unwrap();
        let f = Formula {
            name: "demo".to_string(),
            ..Formula::default()
        };
        replace_text_placeholders(&prefix, &f, &staged).unwrap();
        let out = std::fs::read_to_string(&script).unwrap();
        assert_eq!(
            out,
            format!(
                "#!/bin/sh\n. {}/demo/1/etc/demo\n",
                prefix.cellar().display()
            )
        );
        // A NUL-bearing file is not text: left byte-identical for the
        // leftover scan to delegate.
        assert_eq!(std::fs::read(&binary).unwrap(), raw);
    }

    #[cfg(unix)]
    #[test]
    fn shared_namespace_preserves_payloads_and_unlink_independence() {
        use std::os::unix::fs::MetadataExt as _;

        for namespace in [
            "share/vim",
            "lib/opaque-widget",
            "include/opaque-widget",
            "Frameworks/opaque-widget",
        ] {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let prefix = Prefix::at(&tmp.path().join("prefix"));
            prefix.ensure().unwrap();
            let make = |name: &str| {
                let keg = prefix.keg(name, "1.0");
                let path = keg.join(namespace).join("nested").join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, name).unwrap();
                prefix
                    .write_receipt(
                        &keg,
                        &[
                            ("name", name),
                            ("version", "1.0"),
                            ("installed_by", "abrew"),
                        ],
                    )
                    .unwrap();
                (keg, path)
            };
            let (left, left_file) = make("left");
            let (right, right_file) = make("right");
            let left_identity = std::fs::metadata(&left_file).unwrap();
            link_keg(&prefix, &left, false).unwrap();
            let shared = prefix.root.join(namespace);
            assert!(
                shared.is_symlink(),
                "first installation retains the directory policy"
            );
            link_keg(&prefix, &right, false).unwrap();
            assert!(!shared.is_symlink());
            assert!(shared.is_dir());
            assert_eq!(std::fs::read(shared.join("nested/left")).unwrap(), b"left");
            assert_eq!(
                std::fs::read(shared.join("nested/right")).unwrap(),
                b"right"
            );
            assert!(is_linked(&prefix, "left", "1.0"));
            assert!(is_linked(&prefix, "right", "1.0"));
            unlink_keg(&prefix, "left").unwrap();
            assert!(!shared.join("nested/left").exists());
            assert_eq!(
                std::fs::read(shared.join("nested/right")).unwrap(),
                b"right"
            );
            link_keg(&prefix, &left, false).unwrap();
            unlink_keg(&prefix, "right").unwrap();
            assert_eq!(std::fs::read(shared.join("nested/left")).unwrap(), b"left");
            assert!(!shared.join("nested/right").exists());
            assert_eq!(std::fs::read(&left_file).unwrap(), b"left");
            assert_eq!(std::fs::read(&right_file).unwrap(), b"right");
            assert_eq!(
                std::fs::metadata(&left_file).unwrap().ino(),
                left_identity.ino()
            );
        }
    }

    #[cfg(unix)]
    fn shared_namespace_keg(
        prefix: &Prefix,
        name: &str,
        version: &str,
        files: &[(&str, &[u8])],
    ) -> PathBuf {
        let keg = prefix.keg(name, version);
        for (rel, bytes) in files {
            let path = keg.join("share/vim").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        prefix
            .write_receipt(
                &keg,
                &[
                    ("name", name),
                    ("version", version),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        keg
    }

    #[cfg(unix)]
    #[test]
    fn shared_namespace_rolls_back_activation_and_empty_parent_creation() {
        use std::os::unix::fs::MetadataExt as _;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let left = shared_namespace_keg(&prefix, "left", "1.0", &[("nested/left", b"left")]);
        let right =
            shared_namespace_keg(&prefix, "right", "1.0", &[("new/a", b"a"), ("new/b", b"b")]);
        // Model a previously published whole-directory alias beneath a
        // namespace whose current policy creates parents for each leaf.
        for keg in [&left, &right] {
            std::fs::create_dir_all(keg.join("etc")).unwrap();
            std::fs::rename(keg.join("share/vim"), keg.join("etc/shared")).unwrap();
        }
        let namespace = prefix.root.join("etc/shared");
        symlink_dir(&left.join("etc/shared"), &namespace).unwrap();
        let identity = std::fs::symlink_metadata(&namespace).unwrap();
        let target = std::fs::read_link(&namespace).unwrap();
        let result = with_shared_namespaces(&prefix, Some(&right), || {
            let links = keg_link_plan(&prefix, &right)?;
            let mut placed = 0;
            activate_keg_with(&prefix, &right, &links, false, |link| {
                placed += 1;
                if placed == 2 {
                    return Err("injected later activation failure".into());
                }
                symlink_file(&link.source, &link.destination)
            })
        });
        assert_eq!(
            result,
            Err(InstallError::Failed(
                "injected later activation failure".into()
            ))
        );
        assert_eq!(std::fs::read_link(&namespace).unwrap(), target);
        assert_eq!(
            std::fs::symlink_metadata(&namespace).unwrap().ino(),
            identity.ino()
        );
        assert_eq!(
            std::fs::read(namespace.join("nested/left")).unwrap(),
            b"left"
        );
        assert!(!namespace.join("new").exists());
        assert_eq!(std::fs::read(right.join("etc/shared/new/a")).unwrap(), b"a");
    }

    #[cfg(unix)]
    #[test]
    fn shared_namespace_restores_install_upgrade_and_reinstall_failures() {
        use std::os::unix::fs::MetadataExt as _;
        for mode in [ReplaceMode::None, ReplaceMode::NewKeg, ReplaceMode::SameKeg] {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let prefix = Prefix::at(&tmp.path().join("prefix"));
            prefix.ensure().unwrap();
            let old = shared_namespace_keg(&prefix, "left", "1.0", &[("nested/left", b"old")]);
            link_keg(&prefix, &old, false).unwrap();
            point_opt(&prefix, "left", "1.0").unwrap();
            let old_receipt = std::fs::read(old.join("INSTALL_RECEIPT.json")).unwrap();
            let namespace = prefix.root.join("share/vim");
            let identity = std::fs::symlink_metadata(&namespace).unwrap();
            let owner = if mode == ReplaceMode::None {
                "right"
            } else {
                "left"
            };
            let version = if mode == ReplaceMode::NewKeg {
                "2.0"
            } else {
                "1.0"
            };
            let live = prefix.keg(owner, version);
            let work = aterm_tempfile::TempDir::new_in(prefix.cellar()).unwrap();
            let stage = work.path().join("payload");
            std::fs::create_dir_all(stage.join("share/vim/new")).unwrap();
            std::fs::write(stage.join("share/vim/new/a"), b"a").unwrap();
            std::fs::create_dir_all(stage.join("share/vim/nested")).unwrap();
            std::fs::write(stage.join("share/vim/nested/left"), b"new").unwrap();
            // A fresh install must use a distinct leaf; a same-name leaf is a
            // genuine conflict owned by the first formula.
            if mode == ReplaceMode::None {
                std::fs::rename(
                    stage.join("share/vim/nested/left"),
                    stage.join("share/vim/nested/right"),
                )
                .unwrap();
            }
            prefix
                .write_receipt(
                    &stage,
                    &[
                        ("name", owner),
                        ("version", version),
                        ("installed_by", "abrew"),
                    ],
                )
                .unwrap();
            let result = with_shared_namespaces(&prefix, Some(&stage), || {
                let mut links = Vec::new();
                plan_links(&stage, &live, &prefix.root, &mut links)?;
                let mut count = 0;
                let place = |link: &InstallLink| {
                    count += 1;
                    if count == 2 {
                        return Err("injected publication failure".to_string());
                    }
                    symlink_file(&link.source, &link.destination)
                };
                if mode == ReplaceMode::SameKeg {
                    reinstall_payload_with(
                        &prefix,
                        &stage,
                        &live,
                        &links,
                        &prefix.cellar().join(owner),
                        place,
                    )
                } else {
                    publish_payload_with(
                        &prefix,
                        &stage,
                        &live,
                        &links,
                        (mode == ReplaceMode::NewKeg)
                            .then_some(prefix.cellar().join(owner))
                            .as_deref(),
                        None,
                        place,
                    )
                }
            });
            assert_eq!(
                result,
                Err(InstallError::Failed("injected publication failure".into()))
            );
            assert_eq!(
                std::fs::read_link(&namespace).unwrap(),
                old.join("share/vim")
            );
            assert_eq!(
                std::fs::symlink_metadata(&namespace).unwrap().ino(),
                identity.ino()
            );
            assert_eq!(
                std::fs::read(old.join("share/vim/nested/left")).unwrap(),
                b"old"
            );
            assert_eq!(
                std::fs::read(old.join("INSTALL_RECEIPT.json")).unwrap(),
                old_receipt
            );
            assert_eq!(std::fs::read_link(prefix.opt("left")).unwrap(), old);
            if mode != ReplaceMode::SameKeg {
                assert!(!live.exists());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn shared_namespace_refuses_foreign_aliases_and_real_leaf_conflicts() {
        for case in 0..4 {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let prefix = Prefix::at(&tmp.path().join("prefix"));
            prefix.ensure().unwrap();
            let left = shared_namespace_keg(&prefix, "left", "1.0", &[("nested/item", b"left")]);
            let right = shared_namespace_keg(&prefix, "right", "1.0", &[("nested/item", b"right")]);
            let outside = tmp.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("keep"), b"user bytes").unwrap();
            let namespace = prefix.root.join("share/vim");
            match case {
                0 => {
                    symlink_dir(&outside, &namespace).unwrap();
                }
                1 => {
                    symlink_dir(&left.join("share"), &namespace).unwrap();
                }
                2 => {
                    std::fs::remove_dir_all(left.join("share/vim")).unwrap();
                    symlink_dir(&outside, &left.join("share/vim")).unwrap();
                    symlink_dir(&left.join("share/vim"), &namespace).unwrap();
                }
                _ => {
                    link_keg(&prefix, &left, false).unwrap();
                }
            }
            let target = std::fs::read_link(&namespace).unwrap();
            assert!(link_keg(&prefix, &right, true).is_err());
            assert_eq!(std::fs::read_link(&namespace).unwrap(), target);
            assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
            assert_eq!(
                std::fs::read(right.join("share/vim/nested/item")).unwrap(),
                b"right"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn shared_namespace_never_traverses_payload_symlinks_or_deletes_foreign_rollback_data() {
        for conflict in [false, true] {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let prefix = Prefix::at(&tmp.path().join("prefix"));
            prefix.ensure().unwrap();
            let left = shared_namespace_keg(&prefix, "left", "1.0", &[("left", b"left")]);
            let right = shared_namespace_keg(&prefix, "right", "1.0", &[("alias/new", b"right")]);
            let outside = tmp.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("keep"), b"user bytes").unwrap();
            symlink_dir(&outside, &left.join("share/vim/alias")).unwrap();
            link_keg(&prefix, &left, false).unwrap();
            let namespace = prefix.root.join("share/vim");
            if conflict {
                let error = with_shared_namespaces::<()>(&prefix, Some(&right), || {
                    std::fs::write(namespace.join("user-file"), b"retained").unwrap();
                    Err(InstallError::Failed(
                        "injected failure with foreign data".into(),
                    ))
                })
                .unwrap_err();
                assert!(error.message().contains("original alias retained"));
                assert_eq!(
                    std::fs::read(namespace.join("user-file")).unwrap(),
                    b"retained"
                );
            } else {
                assert!(link_keg(&prefix, &right, false).is_err());
                assert_eq!(
                    std::fs::read_link(&namespace).unwrap(),
                    left.join("share/vim")
                );
                assert!(!outside.join("new").exists());
            }
            assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
            assert_eq!(std::fs::read(left.join("share/vim/left")).unwrap(), b"left");
        }
    }

    #[test]
    fn plan_links_follows_brew_link_dir_modes() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let keg = tmp.path().join("keg");
        let root = tmp.path().join("prefix");
        let put = |rel: &str| {
            let path = keg.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        };
        put("bin/tool");
        put("bin/sub/hidden");
        put("share/doc/demo/readme.md");
        put("share/man/man1/demo.1");
        put("share/locale/en/LC_MESSAGES/demo.mo");
        put("lib/pkgconfig/demo.pc");
        put("lib/node/demo/index.js");
        put("include/postgresql@16/pg.h");
        put("lib/.DS_Store");
        let mut links = Vec::new();
        plan_links(&keg, &keg, &root, &mut links).unwrap();
        let got: Vec<String> = links
            .iter()
            .map(|l| {
                format!(
                    "{}:{}",
                    l.destination.strip_prefix(&root).unwrap().display(),
                    if l.directory { "dir" } else { "file" }
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                "bin/tool:file",
                "lib/node/demo/index.js:file",
                "lib/pkgconfig/demo.pc:file",
                "include/postgresql@16/pg.h:file",
                "share/doc/demo:dir",
                "share/locale/en/LC_MESSAGES/demo.mo:file",
                "share/man/man1/demo.1:file",
            ]
        );
    }

    #[test]
    fn bottle_dest_matches_brew_downloads_naming() {
        // Oracle: `brew --cache jq` prints this exact tail
        // (sha256 of the bottle URL, then the bottle filename).
        let prefix = Prefix::at(std::path::Path::new("/p"));
        let f = Formula {
            name: "jq".to_string(),
            version: "1.8.2".to_string(),
            bottle_rebuild: 1,
            ..Formula::default()
        };
        let bottle = super::super::formula::Bottle {
            tag: "arm64_golden_gate".to_string(),
            url: "https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:d048dd4c28d867e939065a3bc8209f37e2a5be3e2c18d18c8c7da8f4d1629a4a"
                .to_string(),
            sha256: "d048dd4c28d867e939065a3bc8209f37e2a5be3e2c18d18c8c7da8f4d1629a4a"
                .to_string(),
            cellar: ":any".to_string(),
        };
        assert_eq!(
            bottle_dest(&prefix, &f, &bottle),
            std::path::PathBuf::from(
                "/p/cache/downloads/da850b1765922f36315ec227ef66d8780c3508b916b9dde242025e9828e67b26--jq--1.8.2.arm64_golden_gate.bottle.1.tar.gz"
            )
        );
    }

    #[test]
    fn bottle_filenames_and_dep_parents_match_brew_narration() {
        let mut lz4 = Formula {
            name: "lz4".to_string(),
            version: "1.10.0".to_string(),
            bottle_rebuild: 3,
            ..Formula::default()
        };
        assert_eq!(
            bottle_filename(&lz4, "arm64_golden_gate"),
            "lz4--1.10.0.arm64_golden_gate.bottle.3.tar.gz"
        );
        lz4.bottle_rebuild = 0;
        lz4.revision = 1;
        assert_eq!(
            bottle_filename(&lz4, "arm64_tahoe"),
            "lz4--1.10.0_1.arm64_tahoe.bottle.tar.gz"
        );
        let mk = |name: &str, deps: &[&str]| Formula {
            name: name.to_string(),
            dependencies: deps.iter().map(ToString::to_string).collect(),
            ..Formula::default()
        };
        let planned = vec![mk("leaf", &[]), mk("mid", &["leaf"]), mk("top", &["mid"])];
        let parents = dep_parents(&planned, "top");
        assert_eq!(parents.get("mid").map(String::as_str), Some("top"));
        assert_eq!(parents.get("leaf").map(String::as_str), Some("mid"));
        assert_eq!(specified_of("homebrew/core/zstd"), "zstd");
        assert_eq!(tap_of(&mk("x", &[])), "homebrew/core");
    }

    #[test]
    fn runtime_closure_records_the_recursive_graph_in_declared_order() {
        let mk = |name: &str, deps: &[&str]| Formula {
            name: name.to_string(),
            full_name: name.to_string(),
            version: "1".to_string(),
            dependencies: deps.iter().map(ToString::to_string).collect(),
            ..Formula::default()
        };
        let records = [
            mk("top", &["mid", "shared"]),
            mk("mid", &["leaf", "shared"]),
            mk("leaf", &[]),
            mk("shared", &[]),
        ];
        let read = |short: &str| records.iter().find(|r| r.name == short).cloned();
        let closure = runtime_closure(&records[0], &read);
        let names: Vec<(&str, bool)> = closure
            .iter()
            .map(|d| (d.full_name.as_str(), d.declared))
            .collect();
        assert_eq!(names, [("leaf", false), ("shared", true), ("mid", true),]);
        let json = runtime_deps_json(&closure);
        assert!(json.contains("\"full_name\": \"mid\", \"version\": \"1\""));
        assert!(json.starts_with('[') && json.ends_with(']'));
        assert_eq!(runtime_deps_json(&[]), "[]");
    }

    #[test]
    fn a_fetch_plan_keeps_installed_dependencies_and_their_descendants() {
        let read = |name: &str| -> Result<Formula, InstallError> {
            let dependencies = match name {
                "app" => vec!["dep", "shared"],
                "dep" => vec!["leaf", "shared"],
                "leaf" | "shared" | "tool" => vec![],
                _ => return Err(InstallError::Failed("captured unknown".into())),
            };
            Ok(Formula {
                name: name.into(),
                dependencies: dependencies.into_iter().map(str::to_string).collect(),
                build_dependencies: if name == "app" {
                    vec!["tool".to_string()]
                } else {
                    Vec::new()
                },
                ..Formula::default()
            })
        };
        let names = |plan: Vec<Formula>| plan.into_iter().map(|f| f.name).collect::<Vec<_>>();
        let mut install_reads = Vec::new();
        let install = dependency_plan(
            "app",
            false,
            false,
            |n| {
                install_reads.push(n.to_string());
                read(n)
            },
            |_, dep| dep.name == "dep",
        )
        .unwrap();
        assert_eq!(names(install), ["shared", "app"]);
        assert_eq!(install_reads, ["app", "dep", "shared"]);
        let mut fetch_reads = Vec::new();
        let fetch = dependency_plan(
            "homebrew/core/app",
            false,
            true,
            |n| {
                fetch_reads.push(n.to_string());
                read(n)
            },
            |_, _| false,
        )
        .unwrap();
        assert_eq!(names(fetch), ["leaf", "shared", "dep", "tool", "app"]);
        assert_eq!(fetch_reads, ["app", "dep", "leaf", "shared", "tool"]);
        assert_eq!(
            names(dependency_plan("app", true, false, read, |_, _| false).unwrap()),
            ["app"]
        );
        assert!(
            dependency_plan(
                "other/tap/app",
                false,
                false,
                |_| panic!("qualified name must be refused before reading"),
                |_, _| false
            )
            .is_err()
        );
        let cycle = |n: &str| {
            Ok(Formula {
                name: n.into(),
                dependencies: vec!["app".into()],
                ..Formula::default()
            })
        };
        assert!(
            matches!(dependency_plan("app", false, false, cycle, |_, _| false), Err(InstallError::Failed(message)) if message.contains("dependency cycle"))
        );
        assert!(
            matches!(dependency_plan("app", false, false, |_| Ok(Formula { disable: true, ..Formula::default() }), |_, _| false), Err(InstallError::Failed(message)) if message.contains("disabled"))
        );
        assert_eq!(
            dependency_plan(
                "app",
                false,
                false,
                |_| Err(InstallError::Failed("captured read failure".into())),
                |_, _| false
            ),
            Err(InstallError::Failed("captured read failure".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn keg_was_linked_judges_resolving_bin_links() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let p = Prefix::at(&tmp.path().join("prefix"));
        p.ensure().unwrap();
        let make = |name: &str, files: &[&str]| {
            let keg = p.keg(name, "1.0");
            std::fs::create_dir_all(&keg).unwrap();
            for file in files {
                let path = keg.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"owned payload").unwrap();
            }
            p.write_receipt(
                &keg,
                &[
                    ("name", name),
                    ("version", "1.0"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
            symlink_dir(&keg, &p.opt(name)).unwrap();
            keg
        };
        let keg = make("demo", &["bin/tool"]);
        let dest = p.bin().join("tool");
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
        link_keg(&p, &keg, false).unwrap();
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(true));
        std::fs::remove_file(&dest).unwrap();
        symlink_file(Path::new("../Cellar/demo/1.0/bin/tool"), &dest).unwrap();
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(true));
        std::fs::remove_file(&dest).unwrap();
        symlink_file(&keg.join("bin/other"), &dest).unwrap();
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&dest, b"foreign regular").unwrap();
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
        assert_eq!(std::fs::read(&dest).unwrap(), b"foreign regular");
        std::fs::remove_file(&dest).unwrap();
        assert!(p.opt("demo").symlink_metadata().is_ok());
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));

        let library = make("libdemo", &["lib/libdemo.a", "include/libdemo.h"]);
        assert_eq!(keg_was_linked(&p, "libdemo", "1.0"), Ok(false));
        link_keg(&p, &library, false).unwrap();
        assert_eq!(keg_was_linked(&p, "libdemo", "1.0"), Ok(true));
        std::fs::remove_file(p.root.join("lib/libdemo.a")).unwrap();
        assert!(matches!(
            keg_was_linked(&p, "libdemo", "1.0"),
            Err(InstallError::Failed(_))
        ));
        let next = Formula {
            name: "libdemo".into(),
            full_name: "libdemo".into(),
            version: "2.0".into(),
            ..Formula::default()
        };
        assert!(matches!(
            preflight(
                &p,
                &next,
                &InstallOpts {
                    upgrade: true,
                    ..InstallOpts::default()
                },
                Machine::this(),
                true,
            ),
            Err(InstallError::Failed(_))
        ));
        assert_eq!(
            std::fs::read(library.join("lib/libdemo.a")).unwrap(),
            b"owned payload"
        );
        assert!(!p.keg("libdemo", "2.0").exists());
        unlink_keg(&p, "libdemo").unwrap();
        assert_eq!(keg_was_linked(&p, "libdemo", "1.0"), Ok(false));
        assert!(p.opt("libdemo").symlink_metadata().is_ok());

        let layout = make(
            "layout",
            &[
                "bin/layout-tool",
                "sbin/layout-admin",
                "lib/layout.a",
                "include/layout.h",
                "share/doc/layout/readme",
                "etc/layout.conf",
            ],
        );
        assert_eq!(keg_was_linked(&p, "layout", "1.0"), Ok(false));
        link_keg(&p, &layout, false).unwrap();
        assert_eq!(keg_was_linked(&p, "layout", "1.0"), Ok(true));
        unlink_keg(&p, "layout").unwrap();
        assert_eq!(keg_was_linked(&p, "layout", "1.0"), Ok(false));

        make("empty", &[]);
        assert!(matches!(
            keg_was_linked(&p, "empty", "1.0"),
            Err(InstallError::Failed(_))
        ));
        let bad = make("bad", &[]);
        std::fs::write(bad.join("bin"), b"not a directory").unwrap();
        assert!(matches!(
            keg_was_linked(&p, "bad", "1.0"),
            Err(InstallError::Failed(_))
        ));
        assert_eq!(std::fs::read(bad.join("bin")).unwrap(), b"not a directory");
        let nested = make("nested", &["share/guarded/file"]);
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("file"), b"outside retained").unwrap();
        symlink_dir(&outside, &p.root.join("share/guarded")).unwrap();
        assert!(matches!(
            keg_was_linked(&p, "nested", "1.0"),
            Err(InstallError::Failed(_))
        ));
        assert_eq!(
            std::fs::read(outside.join("file")).unwrap(),
            b"outside retained"
        );
        assert_eq!(
            std::fs::read(nested.join("share/guarded/file")).unwrap(),
            b"owned payload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unlink_keeps_the_opt_pointer_and_its_removal_is_separate() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let p = Prefix::at(&tmp.path().join("prefix"));
        p.ensure().unwrap();
        let keg = p.keg("demo", "1.0");
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::write(keg.join("bin/tool"), b"1.0").unwrap();
        p.write_receipt(
            &keg,
            &[
                ("name", "demo"),
                ("version", "1.0"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
        point_opt(&p, "demo", "1.0").unwrap();
        link_keg(&p, &keg, false).unwrap();
        assert!(std::fs::symlink_metadata(p.bin().join("tool")).is_ok());

        unlink_keg(&p, "demo").unwrap();
        assert!(std::fs::symlink_metadata(p.bin().join("tool")).is_err());
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), keg);
        assert_eq!(p.optlinked_version("demo").as_deref(), Some("1.0"));

        remove_owned_opt_link(&p, "demo").unwrap();
        assert!(std::fs::symlink_metadata(p.opt("demo")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_opt_update_retains_unowned_entries_and_refuses_a_directory_alias() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let p = Prefix::at(&tmp.path().join("prefix"));
        p.ensure().unwrap();
        let old = p.keg("demo", "0.9");
        let new = p.keg("demo", "1.0");
        let other = p.keg("other", "1.0");
        for (name, version) in [("demo", "0.9"), ("demo", "1.0"), ("other", "1.0")] {
            let keg = p.keg(name, version);
            std::fs::create_dir_all(keg.join("bin")).unwrap();
            std::fs::write(keg.join("bin/tool"), version).unwrap();
            let run = keg.join("bin/run");
            std::fs::write(&run, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
            std::fs::set_permissions(run, std::fs::Permissions::from_mode(0o755)).unwrap();
            if name == "demo" && version == "0.9" {
                std::fs::write(keg.join("bin/old-only"), b"old artifact").unwrap();
            }
            p.write_receipt(
                &keg,
                &[
                    ("name", name),
                    ("version", version),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        }
        point_opt(&p, "demo", "0.9").unwrap();
        link_keg(&p, &old, false).unwrap();
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
        let formula = Formula {
            name: "demo".to_string(),
            ..Formula::default()
        };
        for linked_parent in [false, true] {
            let target = if linked_parent {
                new.join("share/new/tool")
            } else {
                new.join("bin/conflict")
            };
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, b"new artifact").unwrap();
            let foreign = if linked_parent {
                let outside = tmp.path().join("external-share");
                std::fs::create_dir(&outside).unwrap();
                std::fs::write(outside.join("keep"), b"owner bytes").unwrap();
                let link = p.root.join("share/new");
                symlink_dir(&outside, &link).unwrap();
                link
            } else {
                let file = p.bin().join("conflict");
                std::fs::write(&file, b"owner bytes").unwrap();
                file
            };
            for action in 0..3 {
                let result = match action {
                    0 => switch_keg(&p, "demo", "1.0", false),
                    1 => relink(&p, &formula).map(|_| ()),
                    _ => link_keg(&p, &new, false),
                };
                assert!(
                    result.is_err(),
                    "activation {action}, alias={linked_parent}"
                );
                assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
                assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"0.9");
                assert_eq!(
                    std::fs::read(p.bin().join("old-only")).unwrap(),
                    b"old artifact"
                );
                let output = aterm_uds::spawnfd::command(p.bin().join("run"))
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(output.stdout, b"0.9\n");
                let bytes = if linked_parent {
                    std::fs::read(foreign.join("keep")).unwrap()
                } else {
                    std::fs::read(&foreign).unwrap()
                };
                assert_eq!(bytes, b"owner bytes");
            }
            std::fs::remove_file(foreign).unwrap();
            std::fs::remove_file(target).unwrap();
        }
        for remove_old in [false, true] {
            let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
            for fail_at in [2, links.len()] {
                let mut placed = 0;
                let error = activate_keg_with(&p, &keg, &links, remove_old, |link| {
                    placed += 1;
                    if placed == fail_at {
                        return Err("injected later link failure".to_string());
                    }
                    symlink_dir(&link.source, &link.destination)
                })
                .unwrap_err();
                assert_eq!(
                    error,
                    InstallError::Failed("injected later link failure".to_string())
                );
                assert_eq!(placed, fail_at);
                assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
                assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"0.9");
                assert_eq!(
                    std::fs::read(p.bin().join("old-only")).unwrap(),
                    b"old artifact"
                );
                let output = aterm_uds::spawnfd::command(p.bin().join("run"))
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(output.stdout, b"0.9\n");
            }
        }
        let foreign_target = tmp.path().join("foreign-target");
        for foreign_link in [false, true] {
            let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
            let mut first: Option<PathBuf> = None;
            let error = activate_keg_with(&p, &keg, &links, true, |link| {
                if let Some(path) = &first {
                    std::fs::remove_file(path).unwrap();
                    if foreign_link {
                        symlink_file(&foreign_target, path).unwrap();
                    } else {
                        std::fs::write(path, b"foreign replacement").unwrap();
                    }
                    return Err("injected replacement".to_string());
                }
                symlink_dir(&link.source, &link.destination)?;
                first = Some(link.destination.clone());
                Ok(())
            })
            .unwrap_err();
            assert!(
                matches!(error, InstallError::Failed(ref message) if message.contains("could not restore aliases")),
                "foreign_link={foreign_link}: {error:?}"
            );
            let path = first.unwrap();
            if foreign_link {
                assert_eq!(std::fs::read_link(&path).unwrap(), foreign_target);
            } else {
                assert_eq!(std::fs::read(&path).unwrap(), b"foreign replacement");
            }
            assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
            std::fs::remove_file(&path).unwrap();
            let old_target = old.join("bin").join(path.file_name().unwrap());
            symlink_file(&old_target, &path).unwrap();
        }
        let (run, tool) = (p.bin().join("run"), p.bin().join("tool"));
        for foreign_link in [false, true] {
            let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
            let error = activate_keg_with(&p, &keg, &links, false, |link| {
                symlink_dir(&link.source, &link.destination)?;
                if link.destination == run {
                    std::fs::remove_file(&tool).unwrap();
                    if foreign_link {
                        symlink_file(&foreign_target, &tool).unwrap();
                    } else {
                        std::fs::write(&tool, b"foreign replacement").unwrap();
                    }
                }
                Ok(())
            })
            .unwrap_err();
            assert_eq!(
                error,
                InstallError::Failed(format!(
                    "abrew: link changed before activation: {}",
                    tool.display()
                )),
                "foreign_link={foreign_link}"
            );
            if foreign_link {
                assert_eq!(std::fs::read_link(&tool).unwrap(), foreign_target);
            } else {
                assert_eq!(std::fs::read(&tool).unwrap(), b"foreign replacement");
            }
            assert_eq!(std::fs::read_link(&run).unwrap(), old.join("bin/run"));
            assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
            std::fs::remove_file(&tool).unwrap();
            symlink_file(&old.join("bin/tool"), &tool).unwrap();
        }
        point_opt(&p, "demo", "1.0").unwrap();
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), new);
        std::fs::remove_file(p.opt("demo")).unwrap();
        symlink_dir(&other, &p.opt("demo")).unwrap();
        assert!(point_opt(&p, "demo", "1.0").is_err());
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), other);
        std::fs::remove_file(p.opt("demo")).unwrap();
        std::fs::write(p.opt("demo"), b"foreign regular bytes").unwrap();
        assert!(point_opt(&p, "demo", "1.0").is_err());
        assert_eq!(
            std::fs::read(p.opt("demo")).unwrap(),
            b"foreign regular bytes"
        );
        std::fs::remove_file(p.opt("demo")).unwrap();
        symlink_dir(Path::new("../Cellar/demo/missing"), &p.opt("demo")).unwrap();
        assert!(point_opt(&p, "demo", "1.0").is_err());
        assert_eq!(
            std::fs::read_link(p.opt("demo")).unwrap(),
            Path::new("../Cellar/demo/missing")
        );
        std::fs::remove_file(p.opt("demo")).unwrap();
        std::fs::create_dir(p.opt("demo")).unwrap();
        std::fs::write(p.opt("demo").join("keep"), b"directory bytes").unwrap();
        assert!(point_opt(&p, "demo", "1.0").is_err());
        assert_eq!(
            std::fs::read(p.opt("demo").join("keep")).unwrap(),
            b"directory bytes"
        );
        std::fs::remove_file(p.opt("demo").join("keep")).unwrap();
        std::fs::remove_dir(p.opt("demo")).unwrap();
        std::fs::remove_dir(p.root.join("opt")).unwrap();
        let outside = tmp.path().join("external-opt");
        std::fs::create_dir(&outside).unwrap();
        symlink_dir(&old, &outside.join("demo")).unwrap();
        symlink_dir(&outside, &p.root.join("opt")).unwrap();
        assert!(point_opt(&p, "demo", "1.0").is_err());
        assert!(switch_keg(&p, "demo", "1.0", false).is_err());
        assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"0.9");
        assert_eq!(std::fs::read_link(outside.join("demo")).unwrap(), old);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        assert!(point_opt(&p, "../demo", "1.0").is_err());
        assert!(point_opt(&p, "demo", "../1.0").is_err());
        std::fs::remove_file(p.root.join("opt")).unwrap();
        switch_keg(&p, "demo", "1.0", false).unwrap();
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), new);
        assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"1.0");
        assert!(std::fs::symlink_metadata(p.bin().join("old-only")).is_err());
        for bad in [
            p.cellar().join("demo/../other/1.0"),
            p.cellar().join("demo/./1.0"),
            p.cellar().join("demo/1.0/"),
        ] {
            assert!(link_keg(&p, &bad, true).is_err(), "{bad:?}");
        }
        assert!(unlink_keg(&p, "..").is_err());
        let dead = p.bin().join("deadtool");
        symlink_dir(&p.keg("gone", "1.0").join("bin/deadtool"), &dead).unwrap();
        let foreign = p.bin().join("foreigntool");
        symlink_dir(Path::new("/nonexistent/foreigntool"), &foreign).unwrap();
        assert_eq!(remove_dangling_links(&p, "gone").unwrap(), 1);
        assert!(std::fs::symlink_metadata(&dead).is_err());
        assert!(std::fs::symlink_metadata(&foreign).is_ok());
        assert_eq!(remove_dangling_links(&p, "gone").unwrap(), 0);
        assert!(remove_dangling_links(&p, "..").is_err());
        let alias = p.keg("alias", "1.0");
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink_dir(&new, &alias).unwrap();
        assert!(link_keg(&p, &alias, true).is_err());
        assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"1.0");
    }

    /// An entry of another kind is never the entry an identity was read from, even
    /// on that entry's inode number, which ext4 hands to the next entry made: a file
    /// where an admitted link was, or a link where a cask's directory was, is a
    /// change for every caller (cask's parents and payloads, the prefix's ancestor
    /// chain), whether or not the caller also compares a link's target.
    #[cfg(unix)]
    #[test]
    fn an_entry_of_another_kind_is_not_the_same_path_identity() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let path = tmp.path().join("entry");
        let make = |kind: &str| match kind {
            "link" => symlink_dir(&target, &path).unwrap(),
            "file" => std::fs::write(&path, b"foreign bytes").unwrap(),
            _ => std::fs::create_dir(&path).unwrap(),
        };
        let remove = |kind: &str| match kind {
            "directory" => std::fs::remove_dir(&path).unwrap(),
            _ => std::fs::remove_file(&path).unwrap(),
        };
        for (was, now) in [("link", "file"), ("directory", "link"), ("file", "link")] {
            make(was);
            let identity = std::fs::symlink_metadata(&path).unwrap();
            assert!(same_path_identity(&path, &identity), "an untouched {was}");
            remove(was);
            make(now);
            assert!(
                !same_path_identity(&path, &identity),
                "a {now} where a {was} was"
            );
            remove(now);
        }
    }

    fn cache_formula(
        prefix: &Prefix,
        revision: u64,
        url: &str,
        sha: &str,
        cellar: &str,
        deps: &[&str],
    ) {
        let value = aterm_json::json!({
            "name": "demo", "versions": { "stable": "1.0" }, "revision": revision,
            "dependencies": deps,
            "bottle": { "stable": { "files": { "all": { "url": url, "sha256": sha, "cellar": cellar } } } }
        });
        let dir = prefix.cache().join("api");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("formula-demo.json"),
            aterm_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn already_current_top_pours_no_dependencies_like_brew() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        // demo installed current (as a dependency: not on
        // request); its dep is not installed at all. Brew has no
        // installer for a current top: it only warns and marks
        // the keg on-request, pouring nothing.
        let demo_keg = prefix.keg("demo", "1.0");
        std::fs::create_dir_all(&demo_keg).unwrap();
        prefix
            .write_receipt(
                &demo_keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", "demo"),
                    ("version", "1.0"),
                    ("installed_on_request", "false"),
                ],
            )
            .unwrap();
        cache_formula(
            &prefix,
            0,
            "file:///nonexistent/demo.tar.gz",
            "abc",
            ":any_skip_relocation",
            &["dep"],
        );
        let archive = tmp.path().join("dep.tar.gz");
        std::fs::write(
            &archive,
            gzip_bytes(&tar_bytes(&[(
                "dep/3/bin/dep",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'dep\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let dep_record = aterm_json::json!({
            "name": "dep", "versions": { "stable": "3" },
            "bottle": { "stable": { "files": { "all": {
                "url": format!("file://{}", archive.display()),
                "sha256": fetch::sha256_file(&archive).unwrap(),
                "cellar": ":any_skip_relocation"
            } } } }
        });
        std::fs::write(
            prefix.cache().join("api/formula-dep.json"),
            aterm_json::to_vec(&dep_record).unwrap(),
        )
        .unwrap();
        let api = Api::new(prefix.clone());
        assert_eq!(
            install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
            "1.0"
        );
        assert!(
            prefix.installed_one(Kind::Formula, "dep").is_none(),
            "dep must not pour for an already-current top"
        );
        let receipt = std::fs::read_to_string(demo_keg.join("INSTALL_RECEIPT.json")).unwrap();
        assert!(
            receipt.contains("\"installed_on_request\": \"true\""),
            "named install flips on-request: {receipt}"
        );
        // The dry plan likewise has no rows at all.
        let (top, rows) = dry_rows(&api, &prefix, "demo", &InstallOpts::default()).unwrap();
        assert!(
            top.is_none() && rows.is_empty(),
            "dry rows: {} row(s)",
            rows.len()
        );
    }

    #[test]
    #[cfg(unix)]
    fn fresh_install_upgrades_below_minimum_dep_like_brew() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        // dep installed at 2, below the 3 demo's bottle was
        // built against. Brew's installer expansion (shared by
        // install and upgrade) prunes only satisfied deps, so a
        // fresh plain install of demo upgrades dep to 3.
        let dep_keg = prefix.keg("dep", "2");
        std::fs::create_dir_all(dep_keg.join("bin")).unwrap();
        std::fs::write(dep_keg.join("bin/dep"), b"#!/bin/sh\nprintf 'dep2\\n'\n").unwrap();
        prefix
            .write_receipt(
                &dep_keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", "dep"),
                    ("version", "2"),
                    ("installed_on_request", "false"),
                ],
            )
            .unwrap();
        let demo_archive = tmp.path().join("demo.tar.gz");
        std::fs::write(
            &demo_archive,
            gzip_bytes(&tar_bytes(&[(
                "demo/1.0/bin/demo",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'demo\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let demo_sha = fetch::sha256_file(&demo_archive).unwrap();
        cache_formula(
            &prefix,
            0,
            &format!("file://{}", demo_archive.display()),
            &demo_sha,
            ":any_skip_relocation",
            &["dep"],
        );
        let dep_archive = tmp.path().join("dep.tar.gz");
        std::fs::write(
            &dep_archive,
            gzip_bytes(&tar_bytes(&[(
                "dep/3/bin/dep",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'dep\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let dep_record = aterm_json::json!({
            "name": "dep", "versions": { "stable": "3" },
            "bottle": { "stable": { "files": { "all": {
                "url": format!("file://{}", dep_archive.display()),
                "sha256": fetch::sha256_file(&dep_archive).unwrap(),
                "cellar": ":any_skip_relocation"
            } } } }
        });
        std::fs::write(
            prefix.cache().join("api/formula-dep.json"),
            aterm_json::to_vec(&dep_record).unwrap(),
        )
        .unwrap();
        let api = Api::new(prefix.clone());
        let demo = api.formula("demo").unwrap();
        let tab = aterm_json::json!({
            "runtime_dependencies": [{ "full_name": "dep", "pkg_version": "3" }]
        });
        let manifest = aterm_json::json!({
            "manifests": [{ "annotations": {
                "sh.brew.bottle.digest": demo_sha,
                "sh.brew.tab": aterm_json::to_string(&tab).unwrap()
            } }]
        });
        std::fs::write(
            bottle_manifest_dest(&prefix, &demo),
            aterm_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
            "1.0"
        );
        assert_eq!(
            prefix
                .installed_one(Kind::Formula, "dep")
                .map(|inst| inst.version)
                .as_deref(),
            Some("3"),
            "fresh install must upgrade the below-minimum dep"
        );
    }

    #[test]
    #[cfg(unix)]
    fn already_current_top_below_minimum_dep_is_not_gated() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        // demo current; dep installed at 2, below the 3 its
        // bottle was built against. Admission must not gate the
        // dep replacement brew never attempts for a current top.
        let demo_keg = prefix.keg("demo", "1.0");
        std::fs::create_dir_all(&demo_keg).unwrap();
        prefix
            .write_receipt(
                &demo_keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", "demo"),
                    ("version", "1.0"),
                    ("installed_on_request", "false"),
                ],
            )
            .unwrap();
        let dep_keg = prefix.keg("dep", "2");
        std::fs::create_dir_all(&dep_keg).unwrap();
        prefix
            .write_receipt(
                &dep_keg,
                &[("installed_by", "abrew"), ("name", "dep"), ("version", "2")],
            )
            .unwrap();
        cache_formula(
            &prefix,
            0,
            "file:///nonexistent/demo.tar.gz",
            "abc",
            ":any_skip_relocation",
            &["dep"],
        );
        let dep_record = aterm_json::json!({
            "name": "dep", "versions": { "stable": "3" },
            "bottle": { "stable": { "files": { "all": {
                "url": "file:///nonexistent/dep.tar.gz", "sha256": "abc",
                "cellar": ":any_skip_relocation"
            } } } }
        });
        std::fs::write(
            prefix.cache().join("api/formula-dep.json"),
            aterm_json::to_vec(&dep_record).unwrap(),
        )
        .unwrap();
        let api = Api::new(prefix.clone());
        let demo = api.formula("demo").unwrap();
        let tab = aterm_json::json!({
            "runtime_dependencies": [{ "full_name": "dep", "pkg_version": "3" }]
        });
        let manifest = aterm_json::json!({
            "manifests": [{ "annotations": {
                "sh.brew.bottle.digest": "abc",
                "sh.brew.tab": aterm_json::to_string(&tab).unwrap()
            } }]
        });
        std::fs::write(
            bottle_manifest_dest(&prefix, &demo),
            aterm_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(
            preflight_plan(&api, &prefix, "demo", &InstallOpts::default()).is_ok(),
            "current top must not gate its planned dep replacement"
        );
        assert_eq!(
            install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
            "1.0"
        );
        assert_eq!(
            prefix
                .installed_one(Kind::Formula, "dep")
                .map(|inst| inst.version)
                .as_deref(),
            Some("2"),
            "below-minimum dep must not upgrade for a current top"
        );
    }

    #[test]
    #[cfg(unix)]
    fn alias_and_oldname_install_treats_resolved_formula_as_top() {
        let aliased_read = |n: &str| {
            Ok(match n {
                "app-alias" | "app" => Formula {
                    name: "app".into(),
                    aliases: vec!["app-alias".into()],
                    dependencies: vec!["left".into(), "right".into()],
                    ..Formula::default()
                },
                "left" => Formula {
                    name: "left".into(),
                    dependencies: vec!["shared-old".into()],
                    ..Formula::default()
                },
                "right" => Formula {
                    name: "right".into(),
                    dependencies: vec!["shared".into()],
                    ..Formula::default()
                },
                "shared-old" | "shared" => Formula {
                    name: "shared".into(),
                    oldnames: vec!["shared-old".into()],
                    ..Formula::default()
                },
                other => Formula {
                    name: other.into(),
                    ..Formula::default()
                },
            })
        };
        let planned_names: Vec<String> =
            dependency_plan("app-alias", false, false, aliased_read, |_, _| false)
                .unwrap()
                .into_iter()
                .map(|f| f.name)
                .collect();
        assert_eq!(planned_names, ["shared", "left", "right", "app"]);
        let aliased_cycle = |n: &str| {
            Ok(match n {
                "app-alias" => Formula {
                    name: "app".into(),
                    aliases: vec!["app-alias".into()],
                    dependencies: vec!["dep".into()],
                    ..Formula::default()
                },
                _ => Formula {
                    name: "dep".into(),
                    dependencies: vec!["app".into()],
                    ..Formula::default()
                },
            })
        };
        assert!(matches!(
            dependency_plan("app-alias", false, false, aliased_cycle, |_, _| false),
            Err(InstallError::Failed(msg)) if msg.contains("dependency cycle")
        ));

        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        std::fs::create_dir_all(prefix.cache().join("api")).unwrap();
        let demo_archive = tmp.path().join("demo.tar.gz");
        std::fs::write(
            &demo_archive,
            gzip_bytes(&tar_bytes(&[(
                "demo/1.0/bin/demo",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'demo\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let demo_sha = fetch::sha256_file(&demo_archive).unwrap();
        let dep_archive = tmp.path().join("dep.tar.gz");
        std::fs::write(
            &dep_archive,
            gzip_bytes(&tar_bytes(&[(
                "dep/3/bin/dep",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'dep\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let demo_record = aterm_json::json!({
            "name": "demo",
            "full_name": "demo",
            "aliases": ["demo-alias"],
            "oldnames": ["demo-legacy"],
            "versions": { "stable": "1.0" },
            "dependencies": ["dep"],
            "bottle": { "stable": { "files": { "all": {
                "url": format!("file://{}", demo_archive.display()),
                "sha256": demo_sha,
                "cellar": ":any_skip_relocation"
            } } } }
        });
        for cache_key in [
            "formula-demo.json",
            "formula-demo-alias.json",
            "formula-demo-legacy.json",
        ] {
            std::fs::write(
                prefix.cache().join("api").join(cache_key),
                aterm_json::to_vec(&demo_record).unwrap(),
            )
            .unwrap();
        }
        let dep_record = aterm_json::json!({
            "name": "dep",
            "full_name": "dep",
            "versions": { "stable": "3" },
            "bottle": { "stable": { "files": { "all": {
                "url": format!("file://{}", dep_archive.display()),
                "sha256": fetch::sha256_file(&dep_archive).unwrap(),
                "cellar": ":any_skip_relocation"
            } } } }
        });
        std::fs::write(
            prefix.cache().join("api/formula-dep.json"),
            aterm_json::to_vec(&dep_record).unwrap(),
        )
        .unwrap();
        let api = Api::new(prefix.clone());
        let (dry_top, dry_deps) = dry_rows(
            &api,
            &prefix,
            "demo-alias",
            &InstallOpts {
                quiet: true,
                ..InstallOpts::default()
            },
        )
        .unwrap();
        assert_eq!(dry_top.as_ref().map(|r| r.name.as_str()), Some("demo"));
        assert_eq!(
            dry_deps.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["dep"]
        );

        assert_eq!(
            install(
                &api,
                &prefix,
                "demo-alias",
                &InstallOpts {
                    quiet: true,
                    ..InstallOpts::default()
                },
            )
            .unwrap(),
            "1.0"
        );
        let demo_inst = prefix.installed_one(Kind::Formula, "demo").unwrap();
        let dep_inst = prefix.installed_one(Kind::Formula, "dep").unwrap();
        assert!(
            super::super::commands::requested(&prefix, &demo_inst).unwrap(),
            "alias install must mark the resolved formula installed_on_request"
        );
        assert!(
            !super::super::commands::requested(&prefix, &dep_inst).unwrap(),
            "dependency stays installed_on_request = false"
        );

        // With the resolved top already current, a plain alias install must short-circuit
        // without gating or upgrading a below-minimum dependency, while marking on-request.
        super::super::prefix::set_installed_on_request(&prefix.keg("demo", "1.0"), false).unwrap();
        remove_owned_opt_link(&prefix, "dep").unwrap();
        let _ = std::fs::remove_file(prefix.bin().join("dep"));
        std::fs::remove_dir_all(prefix.keg("dep", "3")).unwrap();
        let dep_old = prefix.keg("dep", "2");
        std::fs::create_dir_all(dep_old.join("bin")).unwrap();
        std::fs::write(dep_old.join("bin/dep"), b"old-dep").unwrap();
        prefix
            .write_receipt(
                &dep_old,
                &[
                    ("installed_by", "abrew"),
                    ("name", "dep"),
                    ("version", "2"),
                    ("installed_on_request", "false"),
                ],
            )
            .unwrap();
        let demo = api.formula("demo-alias").unwrap();
        let tab = aterm_json::json!({
            "runtime_dependencies": [{ "full_name": "dep", "pkg_version": "3" }]
        });
        let manifest = aterm_json::json!({
            "manifests": [{ "annotations": {
                "sh.brew.bottle.digest": demo_sha,
                "sh.brew.tab": aterm_json::to_string(&tab).unwrap()
            } }]
        });
        std::fs::write(
            bottle_manifest_dest(&prefix, &demo),
            aterm_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(preflight_plan(&api, &prefix, "demo-legacy", &InstallOpts::default()).is_ok());
        assert_eq!(
            dry_rows(
                &api,
                &prefix,
                "demo-legacy",
                &InstallOpts {
                    quiet: true,
                    ..InstallOpts::default()
                },
            )
            .unwrap()
            .0
            .map(|r| r.name),
            None
        );
        assert_eq!(
            install(
                &api,
                &prefix,
                "demo-legacy",
                &InstallOpts {
                    quiet: true,
                    ..InstallOpts::default()
                },
            )
            .unwrap(),
            "1.0"
        );
        assert_eq!(
            prefix.installed_one(Kind::Formula, "dep").unwrap().version,
            "2"
        );
        let demo_inst = prefix.installed_one(Kind::Formula, "demo").unwrap();
        assert!(super::super::commands::requested(&prefix, &demo_inst).unwrap());

        // An outdated keg reached via alias stays gated as top in plain preflight,
        // and outdated_linked_formula resolves it when opt-linked.
        remove_owned_opt_link(&prefix, "demo").unwrap();
        let _ = std::fs::remove_file(prefix.bin().join("demo"));
        std::fs::remove_dir_all(prefix.keg("demo", "1.0")).unwrap();
        let old_demo = prefix.keg("demo", "0.9");
        std::fs::create_dir_all(old_demo.join("bin")).unwrap();
        std::fs::write(old_demo.join("bin/demo"), b"old").unwrap();
        prefix
            .write_receipt(
                &old_demo,
                &[
                    ("installed_by", "abrew"),
                    ("name", "demo"),
                    ("version", "0.9"),
                    ("installed_on_request", "true"),
                ],
            )
            .unwrap();
        assert!(matches!(
            preflight_plan(&api, &prefix, "demo-alias", &InstallOpts::default()),
            Err(InstallError::Unsupported(_))
        ));
        point_opt(&prefix, "demo", "0.9").unwrap();
        let (resolved, have) =
            super::super::commands::outdated_linked_formula(&prefix, &api, "demo-alias").unwrap();
        assert_eq!(resolved.name, "demo");
        assert_eq!(have, "0.9");
    }

    #[test]
    #[cfg(unix)]
    fn pinned_outdated_dependency_blocks_install_like_brew() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        // dep installed at 2 (latest 3) and pinned there.
        let dep_keg = prefix.keg("dep", "2");
        std::fs::create_dir_all(&dep_keg).unwrap();
        prefix
            .write_receipt(
                &dep_keg,
                &[("installed_by", "abrew"), ("name", "dep"), ("version", "2")],
            )
            .unwrap();
        prefix.pin("dep").unwrap();
        cache_formula(
            &prefix,
            0,
            "file:///nonexistent/demo.tar.gz",
            "abc",
            ":any_skip_relocation",
            &["dep"],
        );
        std::fs::write(
            prefix.cache().join("api/formula-dep.json"),
            br#"{"name":"dep","versions":{"stable":"3"}}"#,
        )
        .unwrap();
        let api = Api::new(prefix.clone());
        let err = install(&api, &prefix, "demo", &InstallOpts::default()).unwrap_err();
        assert_eq!(
            err.message(),
            "Error: demo: You must `brew unpin dep` as installing demo requires the latest version of pinned dependencies."
        );
        // Brew never sanity-errors a dry run, and
        // --ignore-dependencies skips the check entirely (both
        // then fail later for the missing bottle file, if at all).
        let dry = install(
            &api,
            &prefix,
            "demo",
            &InstallOpts {
                dry_run: true,
                ..InstallOpts::default()
            },
        );
        assert!(dry.is_ok(), "dry run: {dry:?}");
        let ignoring = install(
            &api,
            &prefix,
            "demo",
            &InstallOpts {
                ignore_dependencies: true,
                ..InstallOpts::default()
            },
        );
        assert!(
            !format!("{ignoring:?}").contains("unpin"),
            "ignore-dependencies: {ignoring:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn bottle_install_runs_revised_keg_and_preserves_foreign_files() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let archive = tmp.path().join("demo.tar.gz");
        std::fs::write(
            &archive,
            gzip_bytes(&tar_bytes(&[(
                "demo/1.0_2/bin/demo",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'ready\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let url = format!("file://{}", archive.display());
        let sha = fetch::sha256_file(&archive).unwrap();
        cache_formula(&prefix, 2, &url, &sha, ":any_skip_relocation", &[]);
        let api = Api::new(prefix.clone());
        assert_eq!(
            install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
            "1.0_2"
        );
        let output = aterm_uds::spawnfd::command(prefix.bin().join("demo"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ready\n");
        assert_eq!(
            prefix.installed_one(Kind::Formula, "demo").unwrap().version,
            "1.0_2"
        );

        let archive3 = tmp.path().join("demo3.tar.gz");
        std::fs::write(
            &archive3,
            gzip_bytes(&tar_bytes(&[(
                "demo/1.0_3/bin/demo",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'ready3\\n'\n",
                0o755,
            )])),
        )
        .unwrap();
        let url3 = format!("file://{}", archive3.display());
        let sha3 = fetch::sha256_file(&archive3).unwrap();
        cache_formula(&prefix, 3, &url3, &sha3, ":any_skip_relocation", &[]);
        assert!(matches!(
            install(&api, &prefix, "demo", &InstallOpts::default()),
            Err(InstallError::Unsupported(_))
        ));
        assert!(matches!(
            install(
                &api,
                &prefix,
                "demo",
                &InstallOpts {
                    force: true,
                    ..InstallOpts::default()
                }
            ),
            Err(InstallError::Unsupported(_))
        ));
        assert_eq!(
            aterm_uds::spawnfd::command(prefix.bin().join("demo"))
                .output()
                .unwrap()
                .stdout,
            b"ready\n"
        );
        assert_eq!(
            install(
                &api,
                &prefix,
                "demo",
                &InstallOpts {
                    upgrade: true,
                    ..InstallOpts::default()
                }
            )
            .unwrap(),
            "1.0_3"
        );
        assert_eq!(
            prefix.installed_one(Kind::Formula, "demo").unwrap().version,
            "1.0_3"
        );
        assert_eq!(
            aterm_uds::spawnfd::command(prefix.bin().join("demo"))
                .output()
                .unwrap()
                .stdout,
            b"ready3\n"
        );
        assert_eq!(
            install(
                &api,
                &prefix,
                "demo",
                &InstallOpts {
                    force: true,
                    reinstall: true,
                    ..InstallOpts::default()
                }
            )
            .unwrap(),
            "1.0_3"
        );
        assert_eq!(
            aterm_uds::spawnfd::command(prefix.bin().join("demo"))
                .output()
                .unwrap()
                .stdout,
            b"ready3\n"
        );
        assert!(
            std::fs::symlink_metadata(prefix.cellar().join(".abrew-reinstall-demo-1.0_3")).is_err()
        );

        let foreign = prefix.keg("demo", "personal");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("notes"), b"keep").unwrap();
        let other = prefix.keg("other", "1");
        std::fs::create_dir_all(other.join("bin")).unwrap();
        std::fs::write(other.join("bin/other"), b"other").unwrap();
        prefix
            .write_receipt(
                &other,
                &[
                    ("name", "other"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        symlink_file(&other.join("bin/other"), &prefix.bin().join("other")).unwrap();
        std::fs::remove_file(prefix.bin().join("demo")).unwrap();
        symlink_file(Path::new("../owner/tool"), &prefix.bin().join("demo")).unwrap();
        let foreign_link = std::fs::read_link(prefix.bin().join("demo")).unwrap();
        assert!(link_keg(&prefix, &prefix.keg("demo", "1.0_2"), true).is_err());
        assert_eq!(
            std::fs::read_link(prefix.bin().join("demo")).unwrap(),
            foreign_link
        );
        uninstall(&api, &prefix, "demo", true, true, false).unwrap();
        assert!(!prefix.keg("demo", "1.0_2").exists());
        assert_eq!(std::fs::read(foreign.join("notes")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read_link(prefix.bin().join("demo")).unwrap(),
            foreign_link
        );
        assert!(prefix.bin().join("other").exists());
    }

    /// A placeholder dylib's install name, loads and run paths become the
    /// `install_name_tool` arguments that move them to this prefix, each name
    /// once however many slices carry it; `@loader_path` and libSystem stay.
    #[cfg(target_os = "macos")]
    #[test]
    fn relocation_moves_every_placeholder_name_the_load_commands_carry() {
        use crate::macho::fixtures::{
            ID_DYLIB, LOAD_DYLIB, dylib_command, fat, other_command, rpath_command, thin,
        };
        let placeholder = |id: &str| {
            thin(
                &[
                    dylib_command(ID_DYLIB, id, true),
                    other_command(0x1B, 24, true),
                    dylib_command(
                        LOAD_DYLIB,
                        "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib",
                        true,
                    ),
                    rpath_command("@@HOMEBREW_PREFIX@@/lib", true),
                    dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
                    rpath_command("@loader_path/../lib", true),
                ],
                true,
            )
        };
        let relocate = |name: &str| -> Option<String> {
            [
                ("@@HOMEBREW_CELLAR@@", "/p/Cellar"),
                ("@@HOMEBREW_PREFIX@@", "/p"),
            ]
            .iter()
            .find_map(|(old, new)| name.strip_prefix(old).map(|rest| format!("{new}{rest}")))
        };
        let id = "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib";
        let want = [
            "-id",
            "/p/opt/x/lib/libx.1.dylib",
            "-change",
            "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib",
            "/p/Cellar/xz/1/lib/liblzma.5.dylib",
            "-rpath",
            "@@HOMEBREW_PREFIX@@/lib",
            "/p/lib",
        ];
        for bytes in [
            placeholder(id),
            fat(&[placeholder(id), placeholder(id)], false),
        ] {
            let names = crate::macho::load_names(&bytes).unwrap();
            assert_eq!(relocation_args(&names, relocate).unwrap(), want);
        }
        let system = thin(
            &[dylib_command(
                LOAD_DYLIB,
                "/usr/lib/libSystem.B.dylib",
                true,
            )],
            true,
        );
        let names = crate::macho::load_names(&system).unwrap();
        assert!(relocation_args(&names, relocate).unwrap().is_empty());
        let split = fat(
            &[
                placeholder(id),
                placeholder("@@HOMEBREW_PREFIX@@/opt/y/lib/liby.dylib"),
            ],
            false,
        );
        let names = crate::macho::load_names(&split).unwrap();
        let refused = relocation_args(&names, relocate).unwrap_err();
        assert!(refused.contains("2 different install names"), "{refused}");
        assert_eq!(
            replace_bytes(
                b"a@@HOMEBREW_PREFIX@@/b@@HOMEBREW_PREFIX@@",
                b"@@HOMEBREW_PREFIX@@",
                b"/p"
            ),
            b"a/p/b/p".to_vec()
        );
    }

    /// `:any` relocation runs the developer directory's own
    /// `install_name_tool`, never the `/usr/bin` shim, which raises Apple's
    /// install dialog on a Mac without the developer tools: with none, a
    /// bottle whose Mach-O carries a placeholder name is refused as unsupported
    /// before anything runs, its bytes untouched. NEGATIVE CONTROL:
    /// with a tool given, that tool runs on the file with the names'
    /// arguments (a stand-in that records them and refuses, so nothing is
    /// signed), and a bottle with no placeholder Mach-O needs no tool at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn relocation_without_the_developer_tools_is_refused_before_running_anything() {
        use crate::macho::fixtures::{ID_DYLIB, LOAD_DYLIB, dylib_command, thin};
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let f = Formula {
            name: "demo".into(),
            ..Formula::default()
        };
        let staged = tmp.path().join("staged");
        let lib = staged.join("lib/libx.1.dylib");
        std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
        let bytes = thin(
            &[
                dylib_command(ID_DYLIB, "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib", true),
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
            ],
            true,
        );
        std::fs::write(&lib, &bytes).unwrap();
        match relocate_staged(&prefix, &f, &staged, None) {
            Err(InstallError::Unsupported(why)) => assert!(
                why.contains("needs install_name_tool, and no developer tools are installed"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(&lib).unwrap(), bytes, "nothing changed");

        let argv = tmp.path().join("argv");
        let stand_in = tmp.path().join("bin/install_name_tool");
        std::fs::create_dir_all(stand_in.parent().unwrap()).unwrap();
        std::fs::write(
            &stand_in,
            format!(
                "#!/bin/sh\necho \"$@\" > '{}'\necho 'stand-in refuses' >&2\nexit 1\n",
                argv.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        match relocate_staged(&prefix, &f, &staged, Some(&stand_in)) {
            Err(InstallError::Unsupported(why)) => {
                assert!(why.contains("install_name_tool: stand-in refuses"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        let root = prefix.root.display();
        assert_eq!(
            std::fs::read_to_string(&argv).expect("the given tool ran"),
            format!("-id {root}/opt/x/lib/libx.1.dylib {}\n", lib.display())
        );

        std::fs::write(
            &lib,
            thin(
                &[dylib_command(
                    LOAD_DYLIB,
                    "/usr/lib/libSystem.B.dylib",
                    true,
                )],
                true,
            ),
        )
        .unwrap();
        assert!(relocate_staged(&prefix, &f, &staged, None).is_ok());
    }

    #[test]
    fn the_leftover_scan_reads_load_commands_without_otool() {
        use crate::macho::fixtures::{
            ID_DYLIB, LOAD_DYLIB, archive, dylib_command, dylib_command32, fat, thin, thin32,
        };
        let p = Path::new("/k/bin/tool");
        let homebrew = thin(
            &[
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
                dylib_command(LOAD_DYLIB, "/opt/homebrew/opt/xz/lib/liblzma.5.dylib", true),
            ],
            true,
        );
        assert_eq!(
            macos_leftovers(p, &homebrew),
            ["/k/bin/tool -> /opt/homebrew/opt/xz/lib/liblzma.5.dylib"]
        );
        assert_eq!(
            macos_leftovers(p, &fat(&[homebrew.clone(), homebrew.clone()], true)),
            ["/k/bin/tool -> /opt/homebrew/opt/xz/lib/liblzma.5.dylib"],
            "a universal file's slices are listed once"
        );
        let id = thin(
            &[dylib_command(
                ID_DYLIB,
                "/usr/local/opt/x/lib/libx.dylib",
                false,
            )],
            false,
        );
        assert_eq!(
            macos_leftovers(p, &id),
            ["/k/bin/tool -> /usr/local/opt/x/lib/libx.dylib"]
        );
        let watch = thin32(
            &[dylib_command32(
                LOAD_DYLIB,
                "/opt/homebrew/opt/xz/lib/liblzma.5.dylib",
                false,
            )],
            false,
        );
        assert_eq!(
            macos_leftovers(p, &watch),
            ["/k/bin/tool -> /opt/homebrew/opt/xz/lib/liblzma.5.dylib"],
            "a 32-bit Mach-O is read too"
        );
        let mut truncated = homebrew.clone();
        truncated.truncate(40);
        let listed = macos_leftovers(p, &truncated);
        assert!(
            listed.len() == 1
                && listed[0].starts_with("/k/bin/tool: its load commands could not be checked ("),
            "{listed:?}"
        );
        assert!(
            macos_leftovers(p, &fat(&[archive(), archive()], false)).is_empty(),
            "a universal static archive loads nothing"
        );
        let clean = thin(
            &[dylib_command(
                LOAD_DYLIB,
                "/usr/lib/libSystem.B.dylib",
                true,
            )],
            true,
        );
        assert!(macos_leftovers(p, &clean).is_empty());
        assert!(macos_leftovers(p, b"#!/bin/sh\necho /opt/homebrew/bin\n").is_empty());
        let class = [0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52, 0, 0];
        assert!(crate::macho::is_java_class(&class));
        assert!(macos_leftovers(p, &class).is_empty());
        assert!(!crate::macho::is_java_class(&fat(&[clean], false)));
    }

    #[test]
    fn source_and_relocation_recipes_fall_back_before_dependencies() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        cache_formula(
            &prefix,
            0,
            "unused",
            &"0".repeat(64),
            "/opt/homebrew/Cellar",
            &[],
        );
        let api = Api::new(prefix.clone());
        assert!(matches!(
            install(&api, &prefix, "demo", &InstallOpts::default()),
            Err(InstallError::Unsupported(_))
        ));
        assert!(!prefix.cellar().exists());
        assert!(!prefix.cellar().exists());
    }

    #[test]
    #[cfg(unix)]
    fn publication_conflicts_and_link_failures_leave_no_installed_marker() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let staged = tmp.path().join("stage");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("a"), b"a").unwrap();
        std::fs::write(staged.join("b"), b"b").unwrap();
        let live = prefix.keg("demo", "1");
        let receipt = [
            ("name", "demo"),
            ("version", "1"),
            ("installed_by", "abrew"),
        ];
        prefix.write_receipt(&staged, &receipt).unwrap();
        let links = [
            InstallLink {
                source: live.join("a"),
                destination: prefix.bin().join("a"),
                directory: false,
            },
            InstallLink {
                source: live.join("b"),
                destination: prefix.bin().join("b"),
                directory: false,
            },
        ];
        std::fs::write(&links[1].destination, b"owner").unwrap();
        assert!(publish_payload(&prefix, &staged, &live, &links).is_err());
        assert!(!live.exists());
        assert_eq!(std::fs::read(&links[1].destination).unwrap(), b"owner");
        assert!(!links[0].destination.exists());
        std::fs::remove_file(&links[1].destination).unwrap();

        let mut calls = 0;
        let error = publish_payload_with(&prefix, &staged, &live, &links, None, None, |link| {
            calls += 1;
            assert!(
                prefix.installed(Kind::Formula).is_empty(),
                "receipt preceded successful placement"
            );
            if calls == 2 {
                return Err("injected symlink refusal".to_string());
            }
            symlink_file(&link.source, &link.destination)
        })
        .unwrap_err();
        assert_eq!(
            error,
            InstallError::Failed("injected symlink refusal".to_string())
        );
        assert_eq!(calls, 2);
        assert!(prefix.installed(Kind::Formula).is_empty());
        assert!(!live.exists());
        assert!(
            links
                .iter()
                .all(|link| std::fs::symlink_metadata(&link.destination).is_err())
        );
        assert!(staged.exists());

        prefix.write_receipt(&staged, &receipt).unwrap();
        publish_payload(&prefix, &staged, &live, &links).unwrap();
        assert_eq!(
            prefix.installed_one(Kind::Formula, "demo").unwrap().version,
            "1"
        );
        assert_eq!(std::fs::read(&links[0].destination).unwrap(), b"a");

        let next = prefix.keg("demo", "2");
        let upgrade = tmp.path().join("upgrade");
        std::fs::create_dir(&upgrade).unwrap();
        for name in ["a", "b"] {
            std::fs::write(upgrade.join(name), b"new").unwrap();
        }
        let next_receipt = [
            ("name", "demo"),
            ("version", "2"),
            ("installed_by", "abrew"),
        ];
        prefix.write_receipt(&upgrade, &next_receipt).unwrap();
        let next_links: Vec<_> = links
            .iter()
            .map(|link| InstallLink {
                source: next.join(link.source.file_name().unwrap()),
                destination: link.destination.clone(),
                directory: false,
            })
            .collect();
        let root = prefix.cellar().join("demo");
        let personal = root.join("personal");
        std::fs::create_dir(&personal).unwrap();
        std::fs::write(personal.join("b"), b"personal").unwrap();
        std::fs::remove_file(&links[1].destination).unwrap();
        symlink_file(&personal.join("b"), &links[1].destination).unwrap();
        assert!(publish_payload_replacing(&prefix, &upgrade, &next, &next_links, &root).is_err());
        assert!(!next.exists());
        assert_eq!(std::fs::read(&links[1].destination).unwrap(), b"personal");
        std::fs::remove_file(&links[1].destination).unwrap();
        symlink_file(&links[1].source, &links[1].destination).unwrap();

        let saved = tmp.path().join("saved-link");
        let mut calls = 0;
        assert!(
            publish_payload_with(
                &prefix,
                &upgrade,
                &next,
                &next_links,
                Some(&root),
                None,
                |link| {
                    calls += 1;
                    symlink_file(&link.source, &link.destination)?;
                    if calls == 1 {
                        std::fs::rename(&links[1].destination, &saved).unwrap();
                        std::fs::write(&links[1].destination, b"replacement owner").unwrap();
                    }
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(calls, 1);
        assert_eq!(
            std::fs::read(&links[1].destination).unwrap(),
            b"replacement owner"
        );
        assert_eq!(
            std::fs::read_link(&links[0].destination).unwrap(),
            links[0].source
        );
        assert!(live.exists());
        assert!(!next.exists());
        std::fs::remove_file(&links[1].destination).unwrap();
        std::fs::rename(saved, &links[1].destination).unwrap();
        prefix.write_receipt(&upgrade, &next_receipt).unwrap();
        publish_payload_replacing(&prefix, &upgrade, &next, &next_links, &root).unwrap();
        assert_eq!(std::fs::read(&links[0].destination).unwrap(), b"new");
        assert!(live.exists(), "upgrades retain the preceding version");
        assert_eq!(
            prefix.installed_one(Kind::Formula, "demo").unwrap().version,
            "2"
        );

        let reinstall = tmp.path().join("reinstall");
        std::fs::create_dir(&reinstall).unwrap();
        for name in ["a", "b"] {
            std::fs::write(reinstall.join(name), b"reinstalled").unwrap();
        }
        prefix.write_receipt(&reinstall, &next_receipt).unwrap();
        let saved_receipt = std::fs::read(next.join("INSTALL_RECEIPT.json")).unwrap();
        let mut calls = 0;
        assert!(
            reinstall_payload_with(&prefix, &reinstall, &next, &next_links, &root, |link| {
                calls += 1;
                if calls == 2 {
                    return Err("injected reinstall refusal".to_string());
                }
                symlink_file(&link.source, &link.destination)
            })
            .is_err()
        );
        assert_eq!(calls, 2);
        assert_eq!(
            std::fs::read(next.join("INSTALL_RECEIPT.json")).unwrap(),
            saved_receipt
        );
        for link in &next_links {
            assert_eq!(std::fs::read_link(&link.destination).unwrap(), link.source);
            assert_eq!(std::fs::read(&link.destination).unwrap(), b"new");
        }
        prefix.write_receipt(&reinstall, &next_receipt).unwrap();
        reinstall_payload_with(&prefix, &reinstall, &next, &next_links, &root, |link| {
            symlink_file(&link.source, &link.destination)
        })
        .unwrap();
        assert_eq!(
            std::fs::read(&next_links[0].destination).unwrap(),
            b"reinstalled"
        );
        assert!(live.exists());
        assert_eq!(std::fs::read(personal.join("b")).unwrap(), b"personal");
    }

    #[test]
    #[cfg(unix)]
    fn owned_cask_versions_with_commas_reinstall_through_the_saved_receipt() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let prefix = Prefix::at(tmp.path());
        prefix.ensure().unwrap();
        let version = "4.2.1,20260531.1";
        let live = prefix.cask_dir("demo", version);
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("tool"), b"old cask").unwrap();
        let receipt = [
            ("token", "demo"),
            ("version", version),
            ("installed_by", "abrew"),
        ];
        prefix.write_receipt(&live, &receipt).unwrap();
        let links = [InstallLink {
            source: live.join("tool"),
            destination: prefix.bin().join("demo"),
            directory: false,
        }];
        symlink_file(&links[0].source, &links[0].destination).unwrap();
        let root = prefix.caskroom().join("demo");
        assert!(replacement_target_owned(
            &prefix,
            &links[0].destination,
            &links[0].source,
            &root
        ));
        let stage = tmp.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        std::fs::write(stage.join("tool"), b"new cask").unwrap();
        prefix.write_receipt(&stage, &receipt).unwrap();
        publish_payload_reinstall(&prefix, &stage, &live, &links, &root).unwrap();
        assert_eq!(std::fs::read(&links[0].destination).unwrap(), b"new cask");
        assert!(super::super::prefix::owned_receipt(
            &live,
            Kind::Cask,
            "demo",
            version
        ));
    }

    #[test]
    fn upgrade_request_inputs_preserve_roots_and_refuse_unknown_status() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let prefix = Prefix::at(tmp.path());
        for (name, requested) in [("app", "true"), ("dep", "false")] {
            let keg = prefix.keg(name, "1");
            std::fs::create_dir_all(&keg).unwrap();
            prefix
                .write_receipt(
                    &keg,
                    &[
                        ("name", name),
                        ("version", "1"),
                        ("installed_by", "abrew"),
                        ("installed_on_request", requested),
                    ],
                )
                .unwrap();
        }
        let prior = prior_requests(&prefix, ["app", "dep", "new"].into_iter()).unwrap();
        assert_eq!(prior.get("app"), Some(&true));
        assert_eq!(prior.get("dep"), Some(&false));
        assert!(!prior.contains_key("new"));
        let old = prefix.keg("dep", "0");
        std::fs::create_dir(&old).unwrap();
        prefix
            .write_receipt(
                &old,
                &[
                    ("name", "dep"),
                    ("version", "0"),
                    ("installed_by", "abrew"),
                    ("installed_on_request", "true"),
                ],
            )
            .unwrap();
        assert_eq!(
            prior_requests(&prefix, ["dep"].into_iter())
                .unwrap()
                .get("dep"),
            Some(&true)
        );
        prefix
            .write_receipt(
                &old,
                &[("name", "dep"), ("version", "0"), ("installed_by", "abrew")],
            )
            .unwrap();
        assert!(prior_requests(&prefix, ["dep"].into_iter()).is_err());
        assert!(prefix.keg("dep", "1").exists());
        assert!(old.exists());
    }

    #[test]
    #[cfg(unix)]
    fn plain_uninstall_removes_only_default_keg_and_refuses_pinned() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        for v in ["1", "2"] {
            let keg = prefix.keg("demo", v);
            std::fs::create_dir_all(keg.join("bin")).unwrap();
            std::fs::write(keg.join("bin/tool"), b"x").unwrap();
            prefix
                .write_receipt(
                    &keg,
                    &[("name", "demo"), ("version", v), ("installed_by", "abrew")],
                )
                .unwrap();
        }
        crate::abrew::prefix::symlink_dir(&prefix.keg("demo", "2"), &prefix.opt("demo")).unwrap();
        std::fs::create_dir_all(prefix.bin()).unwrap();
        crate::abrew::prefix::symlink_file(
            &prefix.keg("demo", "2").join("bin/tool"),
            &prefix.bin().join("tool"),
        )
        .unwrap();
        let api = Api::new(prefix.clone());
        uninstall(&api, &prefix, "demo", true, false, false).unwrap();
        assert!(!prefix.keg("demo", "2").exists(), "default keg removed");
        assert!(prefix.keg("demo", "1").exists(), "other version kept");
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
        assert!(std::fs::symlink_metadata(prefix.opt("demo")).is_err());
        prefix.pin("demo").unwrap();
        let err = uninstall(&api, &prefix, "demo", true, false, false).unwrap_err();
        assert!(matches!(err, InstallError::Failed(m) if m.contains("is pinned")));
        assert!(prefix.keg("demo", "1").exists());
        assert!(prefix.pinned_version("demo").is_some());
        uninstall(&api, &prefix, "demo", true, true, false).unwrap();
        assert!(!prefix.keg("demo", "1").exists());
        assert!(prefix.pinned_version("demo").is_none());
    }

    #[test]
    fn unknown_dependency_metadata_refuses_removal_unless_explicitly_ignored() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        for name in ["demo", "other"] {
            let keg = prefix.keg(name, "1");
            std::fs::create_dir_all(&keg).unwrap();
            prefix
                .write_receipt(
                    &keg,
                    &[("name", name), ("version", "1"), ("installed_by", "abrew")],
                )
                .unwrap();
        }
        let cache = prefix.cache().join("api");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("formula-other.json"), b"{}").unwrap();
        let api = Api::new(prefix.clone());
        assert!(uninstall(&api, &prefix, "demo", false, true, false).is_err());
        assert!(prefix.keg("demo", "1").exists());
        uninstall(&api, &prefix, "demo", true, true, false).unwrap();
        assert!(!prefix.keg("demo", "1").exists());
        let keg = prefix.keg("demo", "1");
        std::fs::create_dir_all(&keg).unwrap();
        prefix
            .write_receipt(
                &keg,
                &[
                    ("name", "demo"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        std::fs::write(
            cache.join("formula-other.json"),
            br#"{"name":"other","versions":{"stable":"1"},"revision":0,"dependencies":[]}"#,
        )
        .unwrap();
        uninstall(&api, &prefix, "demo", false, true, false).unwrap();
        assert!(!keg.exists());
    }

    #[test]
    #[cfg(unix)]
    fn bottle_links_are_admitted_against_the_subtree_that_is_published() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        for (label, target, allowed) in [
            ("ordinary", None, true),
            ("local-link", Some("tool"), true),
            ("subtree-escape", Some("../../../outside"), false),
        ] {
            let mut members = vec![
                ("outside", b'0', "", b"outside bytes".as_slice(), 0o644),
                (
                    "demo/1.0/bin/tool",
                    b'0',
                    "",
                    b"tool bytes".as_slice(),
                    0o644,
                ),
            ];
            if let Some(target) = target {
                members.push(("demo/1.0/bin/alias", b'2', target, b"".as_slice(), 0o777));
            }
            let archive = tmp.path().join(format!("{label}.tar"));
            std::fs::write(&archive, tar_bytes(&members)).unwrap();
            let staging = tmp.path().join(label);
            fetch::untar(&archive, &staging).unwrap();
            if let Some(target) = target {
                assert_eq!(
                    std::fs::read_link(staging.join("demo/1.0/bin/alias")).unwrap(),
                    Path::new(target)
                );
            }
            let selected = selected_bottle_payload(&staging, "demo", "1.0");
            assert_eq!(selected.is_ok(), allowed, "{label}");
            assert_eq!(
                std::fs::read(staging.join("outside")).unwrap(),
                b"outside bytes"
            );
            assert_eq!(
                std::fs::read(staging.join("demo/1.0/bin/tool")).unwrap(),
                b"tool bytes"
            );
        }
        let staging = tmp.path().join("aliased-name");
        std::fs::create_dir_all(staging.join("other/1.0")).unwrap();
        symlink_dir(Path::new("other"), &staging.join("demo")).unwrap();
        assert!(selected_bottle_payload(&staging, "demo", "1.0").is_err());
        assert_eq!(
            std::fs::read_link(staging.join("demo")).unwrap(),
            Path::new("other")
        );
    }
}
