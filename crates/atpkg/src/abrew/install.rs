// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Installing: dependency resolution, relocation-free bottle staging,
//! linking and owned-version removal
//! (design §3). Anything the data cannot express returns
//! [`InstallError::PassThrough`] and the caller delegates to real
//! brew with a notice (decision 3A).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::api::Api;
use super::fetch;
use super::formula::{Formula, bottle_tags, version_cmp};
use super::prefix::{Installed, Kind, Prefix, symlink_dir, symlink_file};

/// Homebrew keg versions include the formula revision.
pub(super) fn keg_version(f: &Formula) -> String {
    if f.revision == 0 {
        f.version.clone()
    } else {
        format!("{}_{}", f.version, f.revision)
    }
}

fn preflight(prefix: &Prefix, f: &Formula, opts: &InstallOpts) -> Result<(), InstallError> {
    let version = keg_version(f);
    if !super::valid_component(&f.name)
        || !super::valid_component(&version)
        || f.full_name != f.name
    {
        return Err(InstallError::PassThrough(
            "unsupported formula name/version".to_string(),
        ));
    }
    if let Some(inst) = prefix.installed_one(Kind::Formula, &f.name) {
        if opts.force || version_cmp(&inst.version, &version) == std::cmp::Ordering::Less {
            return Err(InstallError::PassThrough(format!(
                "replacement of {} is handled by brew",
                f.name
            )));
        }
        return Ok(());
    }
    let native = f
        .bottle_for(&bottle_tags())
        .is_some_and(|(tag, _, _)| f.native_bottle_tags.contains(tag));
    if !native {
        return Err(InstallError::PassThrough(format!(
            "{} needs its Homebrew build/relocation recipe",
            f.name
        )));
    }
    if std::fs::symlink_metadata(prefix.keg(&f.name, &version)).is_ok() {
        return Err(InstallError::Failed(format!(
            "abrew: unrecognized keg already exists for {}",
            f.name
        )));
    }
    Ok(())
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
}

/// Resolve `name`'s full install plan: the formula and every runtime
/// dependency not already installed, dependencies first. Cycles and
/// disabled formulae are refusals naming the loop/package.
pub fn plan(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<Vec<Formula>, InstallError> {
    /// The recursion's mutable state, one argument.
    struct PlanState {
        out: Vec<Formula>,
        visiting: Vec<String>,
        seen: BTreeSet<String>,
    }
    fn walk(
        api: &Api,
        prefix: &Prefix,
        name: &str,
        opts: &InstallOpts,
        state: &mut PlanState,
        top: bool,
    ) -> Result<(), InstallError> {
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
        if !top && prefix.installed_one(Kind::Formula, &short).is_some() {
            return Ok(());
        }
        let f = api.formula(&short)?;
        if f.disable {
            return Err(InstallError::Failed(format!(
                "abrew: {short} is disabled upstream"
            )));
        }
        if !opts.ignore_dependencies {
            state.visiting.push(short.clone());
            for d in &f.dependencies {
                walk(api, prefix, d, opts, state, false)?;
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
    walk(api, prefix, name, opts, &mut state, true)?;
    Ok(state.out)
}

/// Plan + preflight with no effects beyond the metadata cache: the
/// dispatcher's pre-lock delegation check. A PassThrough here must
/// delegate before the writer lock or prefix skeleton exists.
pub(super) fn preflight_plan(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    opts: &InstallOpts,
) -> Result<(), InstallError> {
    let planned = plan(api, prefix, name, opts)?;
    for f in &planned {
        preflight(prefix, f, opts)?;
    }
    Ok(())
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
    for f in &planned {
        preflight(prefix, f, opts)?;
    }
    if !opts.dry_run {
        prefix.ensure()?;
    }
    let mut last = String::new();
    let mut installed_now: Vec<String> = Vec::new();
    for f in &planned {
        let version = keg_version(f);
        if let Some(inst) = prefix.installed_one(Kind::Formula, &f.name)
            && !opts.force
            && version_cmp(&inst.version, &version) != std::cmp::Ordering::Less
        {
            if !opts.quiet {
                println!("abrew: {} {} already installed", f.name, inst.version);
            }
            last = inst.version;
            continue;
        }
        if opts.dry_run {
            println!("Would install {} {version}", f.name);
        } else {
            match install_one(prefix, f, f.name == Formula::short_name(name)) {
                Ok(()) => {
                    installed_now.push(f.name.clone());
                    if !opts.quiet {
                        println!("🍺  {}/{version}: installed", f.name);
                    }
                }
                Err(e) => {
                    // Roll this run back: dependencies poured for a plan
                    // that failed do not stay behind as orphans.
                    for done in installed_now.iter().rev() {
                        eprintln!("abrew: rolling back {done} (installed for this failed plan)");
                        let _ = uninstall(api, prefix, done, true, false);
                    }
                    return Err(e);
                }
            }
        }
        last = version;
    }
    if !opts.dry_run {
        prefix.rebuild_state()?;
    }
    Ok(last)
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

fn install_one(prefix: &Prefix, f: &Formula, on_request: bool) -> Result<(), InstallError> {
    let version = keg_version(f);
    let keg = prefix.keg(&f.name, &version);
    let work =
        aterm_tempfile::TempDir::new_in(prefix.cellar()).map_err(|e| format!("abrew: {e}"))?;
    let poured = pour_bottle(prefix, f, work.path())?;
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
        ],
    )?;
    let mut links = Vec::new();
    if !f.keg_only {
        plan_links(&poured, &keg, &prefix.root, &mut links)?;
    }
    links.push(InstallLink {
        source: keg.clone(),
        destination: prefix.opt(&f.name),
        directory: true,
    });
    publish_payload(prefix, &poured, &keg, &links)
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
    publish_payload_with(prefix, staged, live, links, |link| {
        if link.directory {
            symlink_dir(&link.source, &link.destination)
        } else {
            symlink_file(&link.source, &link.destination)
        }
    })
}

fn publish_payload_with(
    prefix: &Prefix,
    staged: &Path,
    live: &Path,
    links: &[InstallLink],
    mut place: impl FnMut(&InstallLink) -> Result<(), String>,
) -> Result<(), InstallError> {
    let parent = live
        .parent()
        .ok_or_else(|| "abrew: payload has no parent".to_string())?;
    check_directory(parent, &prefix.root)?;
    for link in links {
        if std::fs::symlink_metadata(&link.destination).is_ok() {
            return Err(InstallError::Failed(format!(
                "abrew: refusing existing artifact {}",
                link.destination.display()
            )));
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
    let mut placed: Vec<&InstallLink> = Vec::new();
    let result = (|| {
        for link in links {
            if let Some(parent) = link.destination.parent() {
                ensure_directory(parent)?;
            }
            place(link)?;
            placed.push(link);
        }
        // Hard-link creation refuses an unexpected destination atomically.
        std::fs::hard_link(&prepared_receipt, live.join(receipt_name))
            .map_err(|e| format!("abrew: cannot publish receipt: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        for link in placed.into_iter().rev() {
            if std::fs::read_link(&link.destination).is_ok_and(|target| target == link.source) {
                let _ = std::fs::remove_file(&link.destination);
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

/// Classify an already-read link target without a second filesystem lookup.
pub(super) fn link_target_points_into(link: &Path, target: &Path, root: &Path) -> bool {
    fn normalize(path: &Path) -> Option<PathBuf> {
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
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    normalize(&target)
        .zip(normalize(root))
        .is_some_and(|(target, root)| target.starts_with(root))
}

/// Point `opt/<name>` at the newest admitted keg and link its files unless keg-only.
pub fn relink(prefix: &Prefix, f: &Formula) -> Result<(), InstallError> {
    let inst = prefix
        .installed_one(Kind::Formula, &f.name)
        .ok_or_else(|| InstallError::Failed(format!("abrew: {} is not installed", f.name)))?;
    let (keg, links) = activation_links(prefix, &f.name, &inst.version, f.keg_only)?;
    activate_keg(prefix, &keg, &links, false)
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
        || !super::valid_component(version)
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
        for path in owned_keg_links(prefix, name)? {
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
            if let Some((_, identity)) = &change.before {
                if !same_path_identity(path, identity) {
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
                            .is_some_and(|identity| same_path_identity(path, identity)) =>
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

pub(super) fn same_path_identity(path: &Path, expected: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .is_ok_and(|actual| actual.dev() == expected.dev() && actual.ino() == expected.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected);
        false
    }
}

/// Remove every prefix symlink pointing into `keg`.
pub fn unlink_keg(prefix: &Prefix, name: &str) -> Result<(), String> {
    for link in owned_keg_links(prefix, name)? {
        std::fs::remove_file(link).map_err(|e| format!("abrew: {e}"))?;
    }
    Ok(())
}

fn owned_keg_links(prefix: &Prefix, name: &str) -> Result<Vec<PathBuf>, String> {
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
    let opt = prefix.opt(name);
    if prefix_owned_target(&opt, &cellar) {
        links.push(opt);
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

/// Download, verify and safely extract a bottle into private staging.
fn pour_bottle(prefix: &Prefix, f: &Formula, staging: &Path) -> Result<PathBuf, InstallError> {
    let tags = bottle_tags();
    let Some((_tag, url, sha)) = f.bottle_for(&tags).cloned() else {
        return Err(InstallError::PassThrough(format!(
            "no bottle for {} on this platform",
            f.name
        )));
    };
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
    if !super::valid_component(name) || !super::valid_component(version) {
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
pub fn uninstall(
    api: &Api,
    prefix: &Prefix,
    name: &str,
    ignore_dependencies: bool,
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
                "abrew: refusing to uninstall {short}: still needed by {} (use --ignore-dependencies to force)",
                deps_on.join(", ")
            )));
        }
    }
    if dry_run {
        println!("Would uninstall {short} (all versions)");
        return Ok(());
    }
    unlink_keg(prefix, short)?;
    let base = prefix.cellar().join(short);
    if prefix.pinned_version(short).is_some() {
        prefix.unpin(short)?;
    }
    for version in inst.versions {
        std::fs::remove_dir_all(base.join(version)).map_err(|e| format!("abrew: {e}"))?;
    }
    let _ = std::fs::remove_dir(&base);
    prefix.rebuild_state()?;
    Ok(())
}

/// `abrew fetch`: download (and verify) the bottle or source tarball
/// into the cache without installing.
pub fn fetch_only(prefix: &Prefix, f: &Formula) -> Result<(), InstallError> {
    if !super::valid_component(&f.name) || !super::valid_component(&keg_version(f)) {
        return Err(InstallError::PassThrough(
            "unsupported formula name/version".to_string(),
        ));
    }
    prefix.ensure()?;
    let tags = bottle_tags();
    let Some((_t, url, sha)) = f.bottle_for(&tags).cloned() else {
        return Err(InstallError::PassThrough(format!(
            "no bottle for {} on this platform; fetch -s for source",
            f.name
        )));
    };
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
        if !super::valid_component(&formula.version) {
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
                    1 => relink(&p, &formula),
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
        let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
        let mut first = None;
        let error = activate_keg_with(&p, &keg, &links, true, |link| {
            if let Some(path) = &first {
                std::fs::remove_file(path).unwrap();
                std::fs::write(path, b"foreign replacement").unwrap();
                return Err("injected replacement".to_string());
            }
            symlink_dir(&link.source, &link.destination)?;
            first = Some(link.destination.clone());
            Ok(())
        })
        .unwrap_err();
        assert!(
            matches!(error, InstallError::Failed(ref message) if message.contains("could not restore aliases"))
        );
        let path = first.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign replacement");
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
        std::fs::remove_file(&path).unwrap();
        let old_target = old.join("bin").join(path.file_name().unwrap());
        symlink_file(&old_target, &path).unwrap();
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
        let alias = p.keg("alias", "1.0");
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink_dir(&new, &alias).unwrap();
        assert!(link_keg(&p, &alias, true).is_err());
        assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"1.0");
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
        cache_formula(&prefix, 3, &url, &sha, ":any_skip_relocation", &[]);
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
        uninstall(&api, &prefix, "demo", true, false).unwrap();
        assert!(!prefix.keg("demo", "1.0_2").exists());
        assert_eq!(std::fs::read(foreign.join("notes")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read_link(prefix.bin().join("demo")).unwrap(),
            foreign_link
        );
        assert!(prefix.bin().join("other").exists());
    }

    #[test]
    fn source_and_relocation_recipes_fall_back_before_dependencies() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        cache_formula(&prefix, 0, "unused", &"0".repeat(64), ":any", &[]);
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
        let error = publish_payload_with(&prefix, &staged, &live, &links, |link| {
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
        assert!(uninstall(&api, &prefix, "demo", false, false).is_err());
        assert!(prefix.keg("demo", "1").exists());
        uninstall(&api, &prefix, "demo", true, false).unwrap();
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
        uninstall(&api, &prefix, "demo", false, false).unwrap();
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
