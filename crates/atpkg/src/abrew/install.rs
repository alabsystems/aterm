// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Installing: dependency resolution, relocation-free bottle staging,
//! linking and owned-version removal
//! (design §3). Non-native recipes return [`InstallError::PassThrough`]
//! before successful native progress; later refusals remain local failures
//! rather than replaying completed work through real brew (decision 3A).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::api::{Api, Catalog};
use super::fetch;
use super::formula::{Formula, Requirement, version_cmp};
use super::machine::Machine;
use super::prefix::{Installed, Kind, Prefix, symlink_dir, symlink_file};

/// Homebrew keg versions include the formula revision.
pub(super) fn keg_version(f: &Formula) -> String {
    if f.revision == 0 {
        f.version.clone()
    } else {
        format!("{}_{}", f.version, f.revision)
    }
}

/// One plan member's admission on `machine`: Ok(whether to link it), a
/// PassThrough for a recipe that needs Homebrew, or a refusal.
fn preflight(
    prefix: &Prefix,
    f: &Formula,
    opts: &InstallOpts,
    machine: &Machine,
) -> Result<bool, InstallError> {
    let version = keg_version(f);
    if !super::valid_component(&f.name)
        || !super::valid_cask_version(&version)
        || f.full_name != f.name
    {
        return Err(InstallError::PassThrough(
            "unsupported formula name/version".to_string(),
        ));
    }
    let mut link = !f.keg_only;
    let mut same_keg_reinstall = false;
    if let Some(inst) = prefix.installed_one(Kind::Formula, &f.name) {
        let older = version_cmp(&inst.version, &version) == std::cmp::Ordering::Less;
        same_keg_reinstall = opts.reinstall && inst.version == version;
        let admitted_replacement = if older {
            opts.upgrade || opts.reinstall
        } else {
            opts.reinstall && inst.version == version
        };
        if !admitted_replacement && (opts.force || older) {
            return Err(InstallError::PassThrough(format!(
                "replacement of {} is handled by brew",
                f.name
            )));
        }
        if !admitted_replacement {
            return Ok(link);
        }
        // Resolve the complete old activation plan before a new bottle can pour.
        // Keg-only metadata already forbids all activation links.
        if !f.keg_only {
            link = keg_was_linked(prefix, &f.name, &inst.version)?;
        }
        // An admitted replacement still needs the NEW bottle to qualify
        // natively; fall through to the checks below.
    }
    if let Err(needs) = f.native_bottle(&machine.bottle_tags()) {
        return Err(InstallError::PassThrough(format!(
            "{} needs its Homebrew build/relocation recipe ({needs})",
            f.name
        )));
    }
    // Data-evaluable requirements and conflicts, enforced here so the
    // pre-lock pass and the locked install agree.
    if let Some(short) = prefix.installed_conflict(&f.conflicts)? {
        return Err(InstallError::Failed(format!(
            "abrew: {} conflicts with installed {short}",
            f.name
        )));
    }
    if f.pour_requires_clt && !machine.has_clt() {
        return Err(InstallError::PassThrough(format!(
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
            return Err(InstallError::PassThrough(format!(
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
    /// A real failure (message for stderr).
    Failed(String),
    /// Not expressible natively; delegate to real brew (reason).
    PassThrough(String),
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
    /// This install is an upgrade: replacing an older owned keg with a
    /// newer native bottle is admitted (plain installs still delegate
    /// replacements).
    pub upgrade: bool,
    /// This install is a reinstall: replacing the SAME version's owned
    /// keg is admitted (old keg set aside, restored on any failure,
    /// deleted only after the new keg is receipted and linked).
    pub reinstall: bool,
    /// This formula is being installed as another package's
    /// dependency (a cask's `depends_on formula:`), so its receipt
    /// records `installed_on_request=false` like brew's.
    pub as_dependency: bool,
}

/// Resolve `name`'s full install plan: the formula and every runtime
/// dependency not already installed, dependencies first. Cycles and
/// disabled formulae are refusals naming the loop/package.
pub fn plan(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<Vec<Formula>, InstallError> {
    dependency_plan(
        name,
        opts.ignore_dependencies,
        |n| catalog.formula(n).map_err(InstallError::from),
        |n| prefix.installed_one(Kind::Formula, n).is_some(),
    )
}

/// Fetch needs the complete runtime closure even when a dependency is installed:
/// an installed keg does not mean its bottle is present in the download cache.
pub(super) fn fetch_plan(api: &Api, name: &str) -> Result<Vec<Formula>, InstallError> {
    dependency_plan(
        name,
        false,
        |n| api.formula(n).map_err(InstallError::from),
        |_| false,
    )
}

/// One walk, with the caller deciding which non-root dependencies can be skipped.
fn dependency_plan(
    name: &str,
    ignore_dependencies: bool,
    mut read: impl FnMut(&str) -> Result<Formula, InstallError>,
    mut skip: impl FnMut(&str) -> bool,
) -> Result<Vec<Formula>, InstallError> {
    /// The recursion's mutable state, one argument.
    struct PlanState {
        out: Vec<Formula>,
        visiting: Vec<String>,
        seen: BTreeSet<String>,
    }
    fn walk(
        name: &str,
        ignore_dependencies: bool,
        read: &mut impl FnMut(&str) -> Result<Formula, InstallError>,
        skip: &mut impl FnMut(&str) -> bool,
        state: &mut PlanState,
        top: bool,
    ) -> Result<(), InstallError> {
        // homebrew/core/<f> is the API's own name with its home tap
        // spelled out; any other qualification needs its tap recipe.
        let name = name.strip_prefix("homebrew/core/").unwrap_or(name);
        if !super::valid_component(name) {
            return Err(InstallError::PassThrough(format!(
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
        if state.seen.contains(&short) {
            return Ok(());
        }
        state.seen.insert(short.clone());
        if !top && skip(&short) {
            return Ok(());
        }
        let f = read(&short)?;
        if f.disable {
            return Err(InstallError::Failed(format!(
                "abrew: {short} is disabled upstream"
            )));
        }
        if !ignore_dependencies {
            state.visiting.push(short.clone());
            for d in &f.dependencies {
                walk(d, ignore_dependencies, read, skip, state, false)?;
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
    };
    walk(
        name,
        ignore_dependencies,
        &mut read,
        &mut skip,
        &mut state,
        true,
    )?;
    Ok(state.out)
}

/// Plan + preflight with no effects beyond the metadata cache: the
/// dispatcher's pre-lock delegation check. A PassThrough here must
/// delegate before the writer lock or prefix skeleton exists.
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

/// Every refusal [`preflight_plan`] meets, in plan order (it reports the
/// first): the walk's own, which ends the walk (a cycle, a disabled or
/// unknown dependency), or each planned member's. Empty: admitted.
pub(super) fn preflight_refusals(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Vec<InstallError> {
    match plan(catalog, prefix, name, opts) {
        Err(walk) => vec![walk],
        Ok(planned) => planned
            .iter()
            .filter_map(|f| preflight(prefix, f, opts, catalog.machine()).err())
            .collect(),
    }
}

/// Install one formula (its plan first). Returns the installed version.
pub fn install(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<String, InstallError> {
    let planned = plan(api, prefix, name, opts)?;
    // A later fallback must not replay an already installed dependency plan.
    let link_states = planned
        .iter()
        .map(|f| preflight(prefix, f, opts, Machine::this()))
        .collect::<Result<Vec<_>, _>>()?;
    // Capture the standing packages before any pour. Request uncertainty must
    // not become false, and rollback must never uninstall an earlier version.
    let prior = prior_requests(prefix, planned.iter().map(|f| f.name.as_str()))?;
    if !opts.dry_run {
        prefix.ensure()?;
    }
    let mut last = String::new();
    let mut installed_now: Vec<String> = Vec::new();
    let mut completed = false;
    for (f, link) in planned.iter().zip(link_states) {
        let version = keg_version(f);
        let mut replacing = ReplaceMode::None;
        if let Some(inst) = prefix.installed_one(Kind::Formula, &f.name) {
            if !opts.force && version_cmp(&inst.version, &version) != std::cmp::Ordering::Less {
                if !opts.quiet {
                    eprintln!("Warning: {} {} is already installed", f.name, inst.version);
                }
                last = inst.version;
                continue;
            }
            replacing = if inst.version == version {
                ReplaceMode::SameKeg
            } else {
                ReplaceMode::NewKeg
            };
        }
        if opts.dry_run {
            println!("Would install {} {version}", f.name);
        } else {
            // brew's upgrade/reinstall preserve the standing tab:
            // request status and linkedness carry over to the new
            // keg (a dependency stays a dependency, an unlinked keg
            // stays unlinked). A plain install of the target is the
            // only path that claims on-request.
            let preserve = opts.upgrade || opts.reinstall;
            let on_request = if preserve {
                prior.get(&f.name).copied().unwrap_or(false)
            } else {
                (!opts.as_dependency && f.name == Formula::short_name(name))
                    || prior.get(&f.name).copied().unwrap_or(false)
            };
            match install_one(prefix, f, on_request, link, replacing) {
                Ok(()) => {
                    completed = true;
                    if !prior.contains_key(&f.name) {
                        installed_now.push(f.name.clone());
                    }
                    if !opts.quiet {
                        match super::install_badge() {
                            Some(badge) => println!("{badge}  {}/{version}: installed", f.name),
                            None => println!("{}/{version}: installed", f.name),
                        }
                    }
                }
                Err(e) => {
                    // Remove only packages absent before this plan. Completed
                    // upgrades retain their old and new versions on later failure.
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
    if !opts.dry_run {
        prefix.rebuild_state()?;
        if completed {
            note_bin_not_on_path(prefix, opts.quiet);
        }
    }
    Ok(last)
}

/// One caveat line when the prefix bin is not on PATH: the install
/// landed, but bare command names will not resolve until shellenv is
/// evaluated (found live: an aterm session's PATH carries pkg/bin,
/// not abrew's prefix bin).
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

/// Build-prefix references left in a staged keg: placeholders in text
/// files and, on macOS, Mach-O dependency paths still naming the build
/// prefix. A native-tag pour must be relocation-free; anything listed
/// here delegates BEFORE receipt/publication instead of installing a
/// binary that cannot run.
fn linkage_leftovers(staged: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, out);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            if text.contains("@@HOMEBREW") {
                out.push(format!("{} (placeholder)", path.display()));
                continue;
            }
            if cfg!(target_os = "macos")
                && matches!(
                    bytes.get(..4),
                    Some([0xFE, 0xED, 0xFA, 0xCF])
                        | Some([0xCF, 0xFA, 0xED, 0xFE])
                        | Some([0xCA, 0xFE, 0xBA, 0xBE])
                )
                && let Ok(o) = fetch::run_capture("otool", &["-L", &path.to_string_lossy()])
            {
                for line in String::from_utf8_lossy(&o.stdout).lines().skip(1) {
                    let dep = line.trim().split(" (").next().unwrap_or("").trim();
                    if dep.starts_with("/opt/homebrew/")
                        || dep.starts_with("/usr/local/Cellar/")
                        || dep.starts_with("/usr/local/opt/")
                    {
                        out.push(format!("{} -> {dep}", path.display()));
                    }
                }
            }
        }
    }
    walk(staged, &mut out);
    out
}

/// `otool -l` LC_RPATH paths, in load order.
#[cfg(target_os = "macos")]
fn rpaths_from_otool(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_rpath = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("cmd ") {
            in_rpath = line == "cmd LC_RPATH";
        } else if in_rpath && let Some(rest) = line.strip_prefix("path ") {
            out.push(rest.split(" (offset").next().unwrap_or(rest).to_string());
            in_rpath = false;
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn is_mach_o(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0xFE, 0xED, 0xFA, 0xCF])
            | Some([0xCF, 0xFA, 0xED, 0xFE])
            | Some([0xFE, 0xED, 0xFA, 0xCE])
            | Some([0xCE, 0xFA, 0xED, 0xFE])
            | Some([0xCA, 0xFE, 0xBA, 0xBE])
    )
}

/// Relocate a staged `:any` bottle payload from placeholder paths
/// to this prefix, brew's two pour-time passes with the system
/// Mach-O tools: install names / dylib ids / rpaths rewritten by
/// `install_name_tool`, placeholders in NUL-free (text) files
/// replaced as bytes, and every modified binary ad-hoc re-signed
/// (`codesign`, metadata preserved) since the edits invalidate
/// its signature. Any tool failure delegates: the caller's
/// `linkage_leftovers` scan then judges the result before any
/// receipt or publication, so a half-relocated keg cannot land.
#[cfg(target_os = "macos")]
fn relocate_staged(prefix: &Prefix, f: &Formula, staged: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::MetadataExt;
    let passthrough = |what: String| {
        InstallError::PassThrough(format!(
            "{} bottle relocation needs Homebrew ({what})",
            f.name
        ))
    };
    let root = prefix.root.to_string_lossy().into_owned();
    let cellar = prefix.cellar().to_string_lossy().into_owned();
    if root.contains('\0') || cellar.contains('\0') {
        return Err(passthrough("prefix path is not spellable".into()));
    }
    let java = f
        .dependencies
        .iter()
        .map(|d| Formula::short_name(d))
        .find(|d| *d == "openjdk" || d.starts_with("openjdk@"))
        .map(|d| format!("{root}/opt/{d}/libexec/openjdk.jdk/Contents/Home"));
    let mut pairs: Vec<(&str, String)> = vec![
        ("@@HOMEBREW_CELLAR@@", cellar.clone()),
        ("@@HOMEBREW_REPOSITORY@@", root.clone()),
        ("@@HOMEBREW_LIBRARY@@", format!("{root}/Library")),
        ("@@HOMEBREW_PERL@@", format!("{root}/opt/perl/bin/perl")),
        ("@@HOMEBREW_PREFIX@@", root.clone()),
    ];
    if let Some(java) = &java {
        pairs.push(("@@HOMEBREW_JAVA@@", java.clone()));
    }
    let relocate_name = |name: &str, pairs: &[(&str, String)]| -> Option<String> {
        for (old, new) in pairs {
            if let Some(rest) = name.strip_prefix(old) {
                return Some(format!("{new}{rest}"));
            }
        }
        None
    };
    // Collect regular files once, hardlink-deduped, like brew.
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
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !seen.insert((meta.dev(), meta.ino())) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if !bytes.windows(10).any(|w| w == b"@@HOMEBREW") {
            continue;
        }
        let shown = path.to_string_lossy().into_owned();
        if is_mach_o(&bytes) {
            let mut args: Vec<String> = Vec::new();
            // Dylib id (otool -D prints it as the only entry).
            let out =
                fetch::run_capture("otool", &["-D", &shown]).map_err(|e| passthrough(e.clone()))?;
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(id) = text.lines().nth(1).map(str::trim)
                && let Some(new) = relocate_name(id, &pairs)
            {
                args.push("-id".into());
                args.push(new);
            }
            let out =
                fetch::run_capture("otool", &["-L", &shown]).map_err(|e| passthrough(e.clone()))?;
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines().skip(1) {
                let dep = line.trim().split(" (").next().unwrap_or("").trim();
                if let Some(new) = relocate_name(dep, &pairs) {
                    args.push("-change".into());
                    args.push(dep.to_string());
                    args.push(new);
                }
            }
            let out =
                fetch::run_capture("otool", &["-l", &shown]).map_err(|e| passthrough(e.clone()))?;
            let text = String::from_utf8_lossy(&out.stdout);
            for rpath in rpaths_from_otool(&text) {
                if let Some(new) = relocate_name(&rpath, &pairs) {
                    args.push("-rpath".into());
                    args.push(rpath);
                    args.push(new);
                }
            }
            if args.is_empty() {
                continue;
            }
            let mode = meta.permissions();
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
            args.push(shown.clone());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = fetch::run_capture("install_name_tool", &refs)
                .map_err(|e| passthrough(e.clone()))?;
            if !out.status.success() {
                return Err(passthrough(format!(
                    "install_name_tool: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let out = fetch::run_capture(
                "codesign",
                &[
                    "--sign",
                    "-",
                    "--force",
                    "--preserve-metadata=entitlements,requirements,flags,runtime",
                    &shown,
                ],
            )
            .map_err(|e| passthrough(e.clone()))?;
            if !out.status.success() {
                return Err(passthrough(format!(
                    "codesign: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let _ = std::fs::set_permissions(&path, mode);
        } else if !bytes.contains(&0) {
            // Text payload: byte-replace every placeholder pair.
            let mut text = bytes;
            let mut changed = false;
            for (old, new) in &pairs {
                let replaced = replace_bytes(&text, old.as_bytes(), new.as_bytes());
                if replaced != text {
                    changed = true;
                    text = replaced;
                }
            }
            if changed {
                let mode = meta.permissions();
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
                }
                std::fs::write(&path, &text).map_err(|e| format!("abrew: {e}"))?;
                let _ = std::fs::set_permissions(&path, mode);
            }
        }
        // A binary that is not Mach-O but still carries placeholders
        // is left for the leftover scan to delegate.
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(replacement);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
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

/// Read the same six-directory payload plan that activation would use, without
/// its independent opt pointer. Only uniformly exact links or uniformly absent
/// matches have a single state. Partial, empty and unreadable plans refuse.
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

fn install_one(
    prefix: &Prefix,
    f: &Formula,
    on_request: bool,
    link: bool,
    replace: ReplaceMode,
) -> Result<(), InstallError> {
    let version = keg_version(f);
    let keg = prefix.keg(&f.name, &version);
    let work =
        aterm_tempfile::TempDir::new_in(prefix.cellar()).map_err(|e| format!("abrew: {e}"))?;
    let poured = pour_bottle(prefix, f, work.path())?;
    #[cfg(target_os = "macos")]
    if f.bottle_for(&Machine::this().bottle_tags())
        .is_some_and(|b| b.cellar == ":any")
    {
        relocate_staged(prefix, f, &poured)?;
    }
    let leftovers = linkage_leftovers(&poured);
    if !leftovers.is_empty() {
        return Err(InstallError::PassThrough(format!(
            "{} bottle still references its build prefix ({}); needs Homebrew relocation",
            f.name,
            leftovers.first().cloned().unwrap_or_default()
        )));
    }
    prefix.write_receipt(
        &poured,
        &[
            ("name", &f.name),
            ("version", &version),
            ("source", "bottle"),
            ("installed_by", "abrew"),
            (
                "installed_on_request",
                if on_request { "true" } else { "false" },
            ),
            // Recorded so doctor can tell an unlinked keg from a
            // keg-only one offline, from receipt authority alone.
            ("keg_only", if f.keg_only { "true" } else { "false" }),
            // The tap-qualified identity for `list --full-name`
            // (equals the short name for homebrew/core API rows).
            ("full_name", &f.full_name),
        ],
    )?;
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
        ReplaceMode::NewKeg => publish_payload_replacing(prefix, &poured, &keg, &links, &keg_base),
        ReplaceMode::SameKeg => publish_payload_reinstall(prefix, &poured, &keg, &links, &keg_base),
    }
}

/// A link in a new payload's activation plan. Destinations are exclusively created.
pub(super) struct InstallLink {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub directory: bool,
}

fn plan_links(
    staged: &Path,
    keg: &Path,
    root: &Path,
    links: &mut Vec<InstallLink>,
) -> Result<(), InstallError> {
    fn walk(
        staged: &Path,
        live: &Path,
        dst: &Path,
        links: &mut Vec<InstallLink>,
    ) -> Result<(), InstallError> {
        for entry in std::fs::read_dir(staged).map_err(|e| format!("abrew: {e}"))? {
            let entry = entry.map_err(|e| format!("abrew: {e}"))?;
            let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
            let destination = dst.join(entry.file_name());
            let source = live.join(entry.file_name());
            if kind.is_dir() {
                walk(&entry.path(), &source, &destination, links)?;
            } else {
                links.push(InstallLink {
                    source,
                    destination,
                    directory: false,
                });
            }
        }
        Ok(())
    }
    for sub in ["bin", "sbin", "lib", "include", "share", "etc"] {
        let src = staged.join(sub);
        match std::fs::symlink_metadata(&src) {
            Ok(m) if m.file_type().is_dir() => {
                walk(&src, &keg.join(sub), &root.join(sub), links)?;
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

/// Publish a new payload without replacement and admit its receipt only after
/// every link succeeds. Refusals leave no installed marker or foreign deletion.
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

/// Publish replacing the same package's own live links (an upgrade):
/// only links into receipt-owned versions under `replace_root` may be swapped.
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
    // Only an empty reservation is removed; an unrestored keg is retained.
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
    let parent = live
        .parent()
        .ok_or_else(|| "abrew: payload has no parent".to_string())?;
    check_directory(parent, &prefix.root)?;
    // Record both identity and target before granting replacement authority.
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
    // Preparation is private; only successful placement publishes the installed marker.
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
    let identity = std::fs::metadata(staged).map_err(|e| format!("abrew: {e}"))?;
    ensure_directory(parent)?;
    super::prefix::publish_directory(staged, live, false)
        .map_err(|e| format!("abrew: cannot publish payload: {e}"))?;
    let mut placed: Vec<(&InstallLink, std::fs::Metadata)> = Vec::new();
    let mut removed = Vec::new();
    let result = (|| {
        for link in links {
            if let Some(parent) = link.destination.parent() {
                ensure_directory(parent)?;
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
        // Hard-link creation refuses an unexpected destination atomically.
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
        // Restore swapped-out links of the previous version exactly.
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
                // Failed rollback leaves an unreceipted orphan, never a false install.
                let _ = super::prefix::publish_directory(live, staged, false);
            }
        }
        #[cfg(not(unix))]
        let _ = identity;
    }
    result
}

/// Create a destination directory without traversing an existing symlink.
fn ensure_directory(path: &Path) -> Result<(), InstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(InstallError::Failed(format!(
            "abrew: refusing non-directory {}",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                ensure_directory(parent)?;
            }
            std::fs::create_dir(path).map_err(|e| format!("abrew: {e}"))?;
            Ok(())
        }
        Err(e) => Err(InstallError::Failed(format!("abrew: {e}"))),
    }
}

/// Resolve a link lexically, without following a foreign chain on disk.
pub(super) fn link_points_into(link: &Path, root: &Path) -> bool {
    std::fs::read_link(link).is_ok_and(|target| link_target_points_into(link, &target, root))
}

/// Whether a link names exactly the expected payload, without following a
/// foreign chain. Missing names and non-links are negative; other read failures
/// are unknown, rather than evidence that a command is unlinked.
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

/// Classify an already-read link target without a second filesystem lookup.
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

/// Lexical containment alone never authorizes replacing a personal version.
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

/// Point `opt/<name>` at the newest admitted keg and link its files unless keg-only.
/// Relink an installed keg; returns the file symlinks in the
/// activation plan (brew's "N symlinks created" — the opt record
/// is not counted).
pub fn relink(prefix: &Prefix, f: &Formula) -> Result<usize, InstallError> {
    let inst = prefix
        .installed_one(Kind::Formula, &f.name)
        .ok_or_else(|| InstallError::Failed(format!("abrew: {} is not installed", f.name)))?;
    let (keg, links) = activation_links(prefix, &f.name, &inst.version, f.keg_only)?;
    let created = links.iter().filter(|l| !l.directory).count();
    activate_keg(prefix, &keg, &links, false)?;
    Ok(created)
}

/// Whether every file in the keg's link plan already links into
/// the keg (brew's already-linked warning). Partial or unreadable
/// link state reads as not linked, so `link` repairs it.
pub fn is_linked(prefix: &Prefix, name: &str, version: &str) -> bool {
    keg_was_linked(prefix, name, version).unwrap_or(false)
}

/// The file-link destinations a `link` would create, plan only:
/// nothing is written (dry-run).
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
        .filter(|l| !l.directory)
        .map(|l| l.destination)
        .collect())
}

/// The prefix links an `unlink` would remove, plan only (dry-run).
pub fn unlink_targets(prefix: &Prefix, name: &str) -> Result<Vec<PathBuf>, String> {
    owned_keg_links(prefix, name, false)
}

/// Switch only after the target, opt entry and every link destination are admitted.
pub(super) fn switch_keg(
    prefix: &Prefix,
    name: &str,
    version: &str,
    keg_only: bool,
) -> Result<(), InstallError> {
    let (keg, links) = activation_links(prefix, name, version, keg_only)?;
    activate_keg(prefix, &keg, &links, true)
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

/// Only an exactly spelled, receipt-admitted physical keg is link authority.
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

/// Link a keg's `bin/sbin/lib/include/share/etc` files into the prefix.
/// Only links into this package's admitted kegs may be replaced. Foreign
/// files and directory symlinks are explicit refusals, including `--overwrite`.
pub fn link_keg(prefix: &Prefix, keg: &Path, _overwrite: bool) -> Result<(), InstallError> {
    let links = keg_link_plan(prefix, keg)?;
    activate_keg(prefix, keg, &links, false)
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
    // Snapshot the complete admitted alias set before replacing any entry.
    for (path, change) in &mut changes {
        change.before = snapshot_keg_link(prefix, keg, path)?;
        if let Some(parent) = path.parent() {
            ensure_directory(parent)?;
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
            let (InstallError::Failed(message) | InstallError::PassThrough(message)) = error;
            Err(InstallError::Failed(format!(
                "{message}; could not restore aliases: {}",
                refused.join(", ")
            )))
        };
    }
    Ok(())
}

/// Whether `path` is still the entry `expected` was read from: the same device, inode
/// and kind. The kind is part of it because a Linux file system hands a freed inode
/// number straight to the next entry made (ext4 does, between an unlink and the next
/// create), so a file put where a link was can carry the link's number. A link cannot
/// be retargeted in place, so a caller that holds one's target compares it too.
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

/// Remove every prefix symlink pointing into `keg`.
/// Remove every prefix link owned by `name`'s kegs; returns how
/// many were removed (a hand-deleted keg leaves dangling owned
/// links that only this can clear).
/// Remove a keg's file links. The `opt/` pointer stays: in brew
/// it is the version record independent of linking (keg-only
/// formulae hold one without ever being linked), and `list
/// --json`'s optlinked_version reads it. Uninstall removes it
/// separately via [`remove_owned_opt_link`].
pub fn unlink_keg(prefix: &Prefix, name: &str) -> Result<usize, String> {
    let links = owned_keg_links(prefix, name, false)?;
    let removed = links.len();
    for link in links {
        std::fs::remove_file(link).map_err(|e| format!("abrew: {e}"))?;
    }
    Ok(removed)
}

/// Remove `opt/<name>` when it is abrew's own pointer into the
/// name's kegs (uninstall's last link step).
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
    if !super::valid_component(name) {
        return Err("abrew: invalid formula name".to_string());
    }
    check_directory(&prefix.cellar().join(name), &prefix.root).map_err(|e| match e {
        InstallError::Failed(message) | InstallError::PassThrough(message) => message,
    })?;
    let cellar = prefix.cellar().join(name);
    let mut links = Vec::new();
    for sub in ["bin", "sbin", "lib", "include", "share", "etc"] {
        owned_links(&prefix.root.join(sub), &cellar, &mut links)?;
    }
    if include_opt {
        let opt = prefix.opt(name);
        if prefix_owned_target(&opt, &cellar) {
            links.push(opt);
        }
    }
    Ok(links)
}

fn owned_links(dir: &Path, cellar: &Path, links: &mut Vec<PathBuf>) -> Result<(), String> {
    // Do not traverse a foreign directory symlink, including the top-level root.
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()) {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("abrew: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("abrew: {e}"))?;
        let p = entry.path();
        let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
        if kind.is_symlink() && prefix_owned_target(&p, cellar) {
            links.push(p);
        } else if kind.is_dir() {
            owned_links(&p, cellar, links)?;
        }
    }
    Ok(())
}

// A matching path alone is insufficient: the target version must carry our receipt.
fn prefix_owned_target(link: &Path, package: &Path) -> bool {
    if !link_points_into(link, package) {
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
                .any(|version| link_points_into(link, &package.join(version)))
        })
}

/// Remove DANGLING prefix links that spell into `name`'s missing
/// kegs — the recovery path for a hand-deleted keg, whose links
/// the receipt-proven [`unlink_keg`] can no longer claim (the
/// receipt died with the keg) and which make `install` refuse the
/// name. Only links whose target does not exist are removed, and
/// only when Cellar/<name> is a real layout (a linked Cellar
/// could spell into a foreign tree). The explicit `unlink <name>`
/// command is the user's consent; uninstall never comes here.
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

/// Download, verify and safely extract a bottle into private staging.
fn pour_bottle(prefix: &Prefix, f: &Formula, staging: &Path) -> Result<PathBuf, InstallError> {
    let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags()).cloned() else {
        return Err(InstallError::PassThrough(format!(
            "no bottle for {} on this platform",
            f.name
        )));
    };
    let (url, sha) = (bottle.url, bottle.sha256);
    let version = keg_version(f);
    let dest = prefix
        .cache()
        .join(format!("{}-{version}.bottle.tar.gz", f.name));
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
    fetch::untar(&dest, staging)?;
    selected_bottle_payload(staging, &f.name, &version)
}

// Extraction admits links under the whole archive root. Only this exact subtree
// moves into Cellar, so its links must be admitted again against the smaller root.
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

/// Uninstall one formula: unlink and remove only receipt-admitted kegs, drop the
/// opt link and any pin. Refuses while installed dependents remain
/// (receipt dep scan) unless `ignore_dependencies`.
/// The versions an uninstall removes: every receipted version
/// under `--force`, else the one default keg — the opt-linked
/// version when it stands, else the latest (brew's
/// `resolve_default_keg`).
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

/// Remove prefix links that point into one version's keg only, so
/// removing the default keg never strips another version's links.
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
    let short = Formula::short_name(name);
    let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
        return Err(InstallError::Failed(format!(
            "abrew: {short} is not installed"
        )));
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
    // Brew refuses a pinned formula without --force and leaves
    // everything standing; --force removes the pin with the kegs.
    let pinned = prefix.pinned_version(short);
    if pinned.is_some() && !force {
        return Err(InstallError::Failed(format!(
            "Error: {short} is pinned. You must unpin it to uninstall."
        )));
    }
    let targets = uninstall_versions(prefix, &inst, force);
    if dry_run {
        if force {
            println!("Would uninstall {short} (all versions)");
        } else {
            println!("Would uninstall {short} {}", targets[0]);
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
    if force && pinned.is_some() {
        prefix.unpin(short)?;
    }
    for version in &targets {
        std::fs::remove_dir_all(base.join(version)).map_err(|e| format!("abrew: {e}"))?;
    }
    let _ = std::fs::remove_dir(&base);
    prefix.rebuild_state()?;
    Ok(())
}

/// `abrew fetch`: download (and verify) the bottle or source tarball
/// into the cache without installing.
pub fn fetch_only(prefix: &Prefix, f: &Formula) -> Result<(), InstallError> {
    if !super::valid_component(&f.name) || !super::valid_cask_version(&keg_version(f)) {
        return Err(InstallError::PassThrough(
            "unsupported formula name/version".to_string(),
        ));
    }
    prefix.ensure()?;
    let Some(bottle) = f.bottle_for(&Machine::this().bottle_tags()).cloned() else {
        return Err(InstallError::PassThrough(format!(
            "no bottle for {} on this platform; fetch -s for source",
            f.name
        )));
    };
    let (url, sha) = (bottle.url, bottle.sha256);
    let dest = prefix
        .cache()
        .join(format!("{}-{}.bottle.tar.gz", f.name, keg_version(f)));
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
    println!("Downloaded to {}", dest.display());
    Ok(())
}

/// Complete installed dependency metadata, shared by both destructive paths.
/// The reader is the actual API method in production; captures can refuse it without
/// network work. Unknown rows or stale version coverage never become an empty dependency set.
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

/// Installed formulae declaring `name` as a runtime dependency. Unknown
/// metadata is a refusal; it cannot establish that removal is safe.
pub fn dependents(api: &Api, prefix: &Prefix, name: &str) -> Result<Vec<String>, InstallError> {
    let mut installed = prefix.installed_checked(Kind::Formula)?;
    // Removing this package cannot break its own dependency requirements.
    // Only the other installed packages can be dependents of it.
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
    fn a_fetch_plan_keeps_installed_dependencies_and_their_descendants() {
        let read = |name: &str| -> Result<Formula, InstallError> {
            let dependencies = match name {
                "app" => vec!["dep", "shared"],
                "dep" => vec!["leaf", "shared"],
                "leaf" | "shared" => vec![],
                _ => return Err(InstallError::Failed("captured unknown".into())),
            };
            Ok(Formula {
                name: name.into(),
                dependencies: dependencies.into_iter().map(str::to_string).collect(),
                ..Formula::default()
            })
        };
        let names = |plan: Vec<Formula>| plan.into_iter().map(|f| f.name).collect::<Vec<_>>();
        let mut install_reads = Vec::new();
        let install = dependency_plan(
            "app",
            false,
            |n| {
                install_reads.push(n.to_string());
                read(n)
            },
            |n| n == "dep",
        )
        .unwrap();
        assert_eq!(names(install), ["shared", "app"]);
        assert_eq!(install_reads, ["app", "shared"]);
        let mut fetch_reads = Vec::new();
        let fetch = dependency_plan(
            "homebrew/core/app",
            false,
            |n| {
                fetch_reads.push(n.to_string());
                read(n)
            },
            |_| false,
        )
        .unwrap();
        assert_eq!(names(fetch), ["leaf", "shared", "dep", "app"]);
        assert_eq!(fetch_reads, ["app", "dep", "leaf", "shared"]);
        assert_eq!(
            names(dependency_plan("app", true, read, |_| false).unwrap()),
            ["app"]
        );
        assert!(
            dependency_plan(
                "other/tap/app",
                false,
                |_| panic!("qualified name must be refused before reading"),
                |_| false
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
            matches!(dependency_plan("app", false, cycle, |_| false), Err(InstallError::Failed(message)) if message.contains("dependency cycle"))
        );
        assert!(
            matches!(dependency_plan("app", false, |_| Ok(Formula { disable: true, ..Formula::default() }), |_| false), Err(InstallError::Failed(message)) if message.contains("disabled"))
        );
        assert_eq!(
            dependency_plan(
                "app",
                false,
                |_| Err(InstallError::Failed("captured read failure".into())),
                |_| false
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
            // The independent opt pointer is present even when every activation link is absent.
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
        // Merely pointing somewhere inside the keg is not this payload link.
        symlink_file(&keg.join("bin/other"), &dest).unwrap();
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&dest, b"foreign regular").unwrap();
        assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
        assert_eq!(std::fs::read(&dest).unwrap(), b"foreign regular");
        std::fs::remove_file(&dest).unwrap();
        // opt survives unlink; it must not make a uniformly unlinked plan positive.
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
        // No bottle exists in this captured record: reaching bottle selection would
        // PassThrough. Partial activation must instead fail in the earlier preflight.
        assert!(matches!(
            preflight(
                &p,
                &next,
                &InstallOpts {
                    upgrade: true,
                    ..InstallOpts::default()
                },
                Machine::this(),
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

        // No activation files and a roster that cannot be read are uncertain.
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
        // A directory alias is not authority to inspect activation through its target.
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
        // File links go; the opt version pointer stays, as in brew.
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
        // A new version's foreign leaf or linked parent must be refused before
        // replacing the old executable links or their opt entry.
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
        // A foreign file or link put where a placed link was survives the rollback, even
        // when the file system hands it the placed link's inode number (ext4 does).
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
        // A foreign file or link put where an admitted link was, after the snapshot and
        // before activation reaches it, is refused and survives, even on the admitted
        // link's inode number. The rollback puts back what activation had replaced.
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
        // Dangling-link recovery: a dead link into a missing keg
        // is removed; a live link and a foreign dangling link stay.
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

        // Replacement refuses before touching the working keg or links.
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
            Err(InstallError::PassThrough(_))
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
            Err(InstallError::PassThrough(_))
        ));
        assert_eq!(
            aterm_uds::spawnfd::command(prefix.bin().join("demo"))
                .output()
                .unwrap()
                .stdout,
            b"ready\n"
        );
        // The same replacement IS admitted as an upgrade.
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
        // A same-version reinstall swaps the keg in place and leaves
        // no aside behind; a plain forced install still delegates.
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
        // Restore the 1.0_2 working keg's link expectations below by
        // pointing the census back: the foreign-link checks use 1.0_2,
        // which still exists beside 1.0_3.

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

    #[cfg(target_os = "macos")]
    #[test]
    fn rpaths_parse_from_otool_load_commands() {
        let text = "\
Load command 10\n      cmd LC_LOAD_DYLIB\n      name @@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib (offset 24)\nLoad command 11\n      cmd LC_RPATH\n      cmdsize 48\n      path @@HOMEBREW_PREFIX@@/lib (offset 12)\nLoad command 12\n      cmd LC_RPATH\n      cmdsize 40\n      path @loader_path/../lib (offset 12)\n";
        assert_eq!(
            rpaths_from_otool(text),
            ["@@HOMEBREW_PREFIX@@/lib", "@loader_path/../lib"]
        );
        assert_eq!(
            replace_bytes(
                b"a@@HOMEBREW_PREFIX@@/b@@HOMEBREW_PREFIX@@",
                b"@@HOMEBREW_PREFIX@@",
                b"/p"
            ),
            b"a/p/b/p".to_vec()
        );
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
            Err(InstallError::PassThrough(_))
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

        // A later destination changes after admission, while its original inode
        // stays alive at another name. Refuse it and restore only our own edit.
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
        // opt points at 2 (the default keg); bin links into 2.
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
        // Pinned: plain uninstall refuses and leaves everything;
        // --force removes keg and pin.
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
        // Valid cached JSON reaches the metadata refusal without any network request.
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
            // All three archives pass the actual whole-archive InRoot writer.
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
