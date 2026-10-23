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

/// Formats the formula's keg version string (including revision when non-zero).
pub(super) fn keg_version(f: &Formula) -> String {
    if f.revision == 0 {
        f.version.clone()
    } else {
        format!("{}_{}", f.version, f.revision)
    }
}

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
        if !f.keg_only {
            link = keg_was_linked(prefix, &f.name, &inst.version)?;
        }
    }
    if let Err(needs) = f.native_bottle(&machine.bottle_tags()) {
        return Err(InstallError::PassThrough(format!(
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
    /// Local installation failure message.
    Failed(String),
    /// Recipe requires delegating to real `brew`.
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
    /// Whether this install is an upgrade replacing an older owned keg.
    pub upgrade: bool,
    /// Whether this install is a same-version reinstall.
    pub reinstall: bool,
    /// Whether this formula is installed as another package's dependency.
    pub as_dependency: bool,
}

/// Resolves `name`'s runtime dependency install plan in dependency-first order.
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

/// Resolves the full runtime dependency closure for `fetch` regardless of installed state.
pub(super) fn fetch_plan(api: &Api, name: &str) -> Result<Vec<Formula>, InstallError> {
    dependency_plan(
        name,
        false,
        |n| api.formula(n).map_err(InstallError::from),
        |_| false,
    )
}

fn dependency_plan(
    name: &str,
    ignore_dependencies: bool,
    mut read: impl FnMut(&str) -> Result<Formula, InstallError>,
    mut skip: impl FnMut(&str) -> bool,
) -> Result<Vec<Formula>, InstallError> {
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
        Ok(planned) => planned
            .iter()
            .filter_map(|f| preflight(prefix, f, opts, catalog.machine()).err())
            .collect(),
    }
}

/// Installs formula `name` and its missing dependencies, returning the installed version.
pub fn install(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<String, InstallError> {
    let planned = plan(api, prefix, name, opts)?;
    let link_states = planned
        .iter()
        .map(|f| preflight(prefix, f, opts, Machine::this()))
        .collect::<Result<Vec<_>, _>>()?;
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
            let names = crate::macho::load_names(&bytes)
                .map_err(|e| passthrough(format!("{shown}: {e}")))?;
            let mut args = relocation_args(&names, |name| relocate_name(name, &pairs))
                .map_err(|e| passthrough(format!("{shown}: {e}")))?;
            if args.is_empty() {
                continue;
            }
            let Some(install_name_tool) = install_name_tool else {
                return Err(passthrough(format!(
                    "{shown} needs install_name_tool, and no developer tools are installed"
                )));
            };
            let install_name_tool = install_name_tool.to_string_lossy();
            let mode = meta.permissions();
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
            args.push(shown.clone());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = fetch::run_capture(&install_name_tool, &refs)
                .map_err(|e| passthrough(e.clone()))?;
            if !out.status.success() {
                return Err(passthrough(format!(
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
            .map_err(|e| passthrough(e.clone()))?;
            if !out.status.success() {
                return Err(passthrough(format!(
                    "codesign: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let _ = std::fs::set_permissions(&path, mode);
        } else if !bytes.contains(&0) {
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
        let install_name_tool = super::machine::DeveloperDir::this().tool("install_name_tool");
        relocate_staged(prefix, f, &poured, install_name_tool.as_deref())?;
    }
    let leftovers = linkage_leftovers(&poured);
    if !leftovers.is_empty() {
        return Err(InstallError::PassThrough(format!(
            "{} bottle may still reference its build prefix ({}); needs Homebrew relocation",
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
            ("keg_only", if f.keg_only { "true" } else { "false" }),
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

/// Planned symlink in a new payload's activation plan.
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
    result
}

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
    let (keg, links) = activation_links(prefix, &f.name, &inst.version, f.keg_only)?;
    let created = links.iter().filter(|l| !l.directory).count();
    activate_keg(prefix, &keg, &links, false)?;
    Ok(created)
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
        .filter(|l| !l.directory)
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

/// Downloads and verifies `f`'s bottle or source tarball into the cache without installing.
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
    /// bottle whose Mach-O carries a placeholder name is handed to real
    /// Homebrew before anything runs, its bytes untouched. NEGATIVE CONTROL:
    /// with a tool given, that tool runs on the file with the names'
    /// arguments (a stand-in that records them and refuses, so nothing is
    /// signed), and a bottle with no placeholder Mach-O needs no tool at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn relocation_without_the_developer_tools_delegates_before_running_anything() {
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
            Err(InstallError::PassThrough(why)) => assert!(
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
            Err(InstallError::PassThrough(why)) => {
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
