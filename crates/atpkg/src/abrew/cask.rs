// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native cask installation from bounded archives and read-only disk images.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::api::Catalog;
use super::fetch;
use super::formula::{Artifact, Cask, version_cmp};
use super::install::{
    InstallError, InstallLink, check_directory, link_target_points_into, publish_payload,
    same_path_identity,
};
use super::machine::Machine;
use super::prefix::{Kind, Prefix, owned_receipt};

/// Apply the variation for `machine`, when the cask carries any.
pub(super) fn resolve<'c>(
    cask: &'c Cask,
    machine: &Machine,
) -> Result<std::borrow::Cow<'c, Cask>, InstallError> {
    if let Some(what) = cask.unsupported_constraints {
        return Err(InstallError::Unsupported(format!(
            "cask {} has unsupported constraints ({what})",
            cask.token
        )));
    }
    if cask.variations.is_empty() {
        return Ok(std::borrow::Cow::Borrowed(cask));
    }
    if machine.macos_major().is_none() {
        return Err(InstallError::Unsupported(format!(
            "cask {} needs Homebrew variation selection",
            cask.token
        )));
    }
    let Some(variation) = cask.variation_for(machine) else {
        return Ok(std::borrow::Cow::Borrowed(cask));
    };
    if variation.unsupported {
        return Err(InstallError::Unsupported(format!(
            "cask {} needs Homebrew variation selection",
            cask.token
        )));
    }
    let mut resolved = cask.clone();
    if let Some(url) = &variation.url {
        resolved.url = url.clone();
    }
    if let Some(sha) = &variation.sha256 {
        resolved.sha256 = sha.clone();
    }
    if let Some(version) = &variation.version {
        resolved.version = version.clone();
    }
    if let Some(artifacts) = &variation.artifacts {
        resolved.artifacts = artifacts.clone();
    }
    resolved.variations.clear();
    Ok(std::borrow::Cow::Owned(resolved))
}

/// The pre-lock native admission check for one cask on `machine`.
pub(super) fn preflight(
    prefix: &Prefix,
    cask: &Cask,
    machine: &Machine,
) -> Result<(), InstallError> {
    let resolved = resolve(cask, machine)?;
    let cask: &Cask = &resolved;
    if let Some(floor) = &cask.macos_min
        && !machine
            .macos_version()
            .is_some_and(|v| version_cmp(v, floor) != std::cmp::Ordering::Less)
    {
        return Err(InstallError::Unsupported(format!(
            "cask {} needs macOS >= {floor}",
            cask.token
        )));
    }
    if let Some(exact) = &cask.macos_exact {
        let met = machine.macos_version().is_some_and(|v| {
            let bound: Vec<&str> = exact.split('.').collect();
            let here: Vec<&str> = v.split('.').collect();
            here.len() >= bound.len() && here[..bound.len()] == bound[..]
        });
        if !met {
            return Err(InstallError::Unsupported(format!(
                "cask {} needs exactly macOS {exact}",
                cask.token
            )));
        }
    }
    if let Some(arch) = &cask.arch {
        let here = machine.arch();
        if arch != here {
            return Err(InstallError::Unsupported(format!(
                "cask {} needs {arch} (this machine is {here})",
                cask.token
            )));
        }
    }
    if let Some(short) = prefix.installed_conflict(&cask.conflicts)? {
        return Err(InstallError::Failed(format!(
            "abrew: cask {} conflicts with installed {short}",
            cask.token
        )));
    }
    if cask.disabled {
        return Err(InstallError::Unsupported(format!(
            "cask {} is disabled upstream",
            cask.token
        )));
    }
    if cask.artifacts.iter().any(|a| matches!(a, Artifact::Pkg(_))) {
        return Err(InstallError::Unsupported(format!(
            "cask {} ships a pkg installer (root work abrew never does itself)",
            cask.token
        )));
    }
    if (!cask.naked
        && !autodetect_naked(cask)
        && git_raw_plan(cask).is_none()
        && !fetch::archive_supported(&download_name(cask))
        && !fetch::is_dmg(&download_name(cask)))
        || cask.artifacts.is_empty()
    {
        return Err(InstallError::Unsupported(format!(
            "cask {} needs its Homebrew extraction recipe",
            cask.token
        )));
    }
    artifact_paths(prefix, cask)?;
    Ok(())
}

/// The dependency half of preflight.
pub(super) fn preflight_deps(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    cask: &Cask,
) -> Result<(), InstallError> {
    let mut seen = BTreeSet::new();
    preflight_deps_inner(catalog, prefix, cask, &mut seen, 0)
}

fn preflight_deps_inner(
    catalog: &dyn Catalog,
    prefix: &Prefix,
    cask: &Cask,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), InstallError> {
    let resolved = resolve(cask, catalog.machine())?;
    let cask: &Cask = &resolved;
    if depth > 8 {
        return Err(InstallError::Unsupported(format!(
            "cask {} dependency chain is too deep",
            cask.token
        )));
    }
    let dep_opts = super::install::InstallOpts {
        quiet: true,
        as_dependency: true,
        ..super::install::InstallOpts::default()
    };
    for dep in &cask.depends_formula {
        if prefix.installed_one(Kind::Formula, dep).is_none() {
            super::install::preflight_plan(catalog, prefix, dep, &dep_opts)?;
        }
    }
    for dep in &cask.depends_cask {
        if prefix.installed_one(Kind::Cask, dep).is_some() || !seen.insert(dep.clone()) {
            continue;
        }
        let dc = catalog.cask(dep).map_err(InstallError::Failed)?;
        preflight(prefix, &dc, catalog.machine())?;
        preflight_deps_inner(catalog, prefix, &dc, seen, depth + 1)?;
    }
    Ok(())
}

/// Install a cask after its `depends_on` dependencies, brew's order.
#[allow(clippy::too_many_arguments)]
pub(super) fn install_with_deps(
    api: &super::api::Api,
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    force: bool,
    upgrade: bool,
    reinstall: bool,
) -> Result<String, InstallError> {
    let mut seen = BTreeSet::new();
    let mut possible_progress = false;
    if let Err(error) = install_deps_inner(
        api,
        prefix,
        cask,
        dry_run,
        &mut seen,
        0,
        &mut possible_progress,
    ) {
        return super::after_native_attempt(&mut possible_progress, Err(error));
    }
    let result = install(prefix, cask, dry_run, force, upgrade, reinstall);
    super::after_native_attempt(&mut possible_progress, result)
}

fn install_deps_inner(
    api: &super::api::Api,
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    seen: &mut BTreeSet<String>,
    depth: usize,
    possible_progress: &mut bool,
) -> Result<(), InstallError> {
    let resolved = resolve(cask, Machine::this())?;
    let cask: &Cask = &resolved;
    if depth > 8 {
        return Err(InstallError::Unsupported(format!(
            "cask {} dependency chain is too deep",
            cask.token
        )));
    }
    let dep_opts = super::install::InstallOpts {
        dry_run,
        as_dependency: true,
        ..super::install::InstallOpts::default()
    };
    for dep in &cask.depends_formula {
        if prefix.installed_one(Kind::Formula, dep).is_none() {
            let result = super::install::install(api, prefix, dep, &dep_opts);
            super::after_native_attempt(possible_progress, result)?;
        }
    }
    for dep in &cask.depends_cask {
        if prefix.installed_one(Kind::Cask, dep).is_some() || !seen.insert(dep.clone()) {
            continue;
        }
        let dc = api.cask(dep).map_err(InstallError::Failed)?;
        install_deps_inner(
            api,
            prefix,
            &dc,
            dry_run,
            seen,
            depth + 1,
            possible_progress,
        )?;
        let result = install_inner(prefix, &dc, dry_run, false, false, false, false);
        super::after_native_attempt(possible_progress, result)?;
    }
    Ok(())
}

/// Names a dry run would still have to install for `cask`: its
/// uninstalled `depends_on` casks (each resolved like the real walk)
/// followed by its uninstalled `depends_on` formulae — the membership
/// of brew's `Would install N dependencies for <token>` line. Read
/// only; admission runs separately through the dry install itself.
pub(super) fn dry_dep_names(api: &super::api::Api, prefix: &Prefix, cask: &Cask) -> Vec<String> {
    fn walk(api: &super::api::Api, prefix: &Prefix, cask: &Cask, out: &mut Vec<String>) {
        let Ok(resolved) = resolve(cask, Machine::this()) else {
            return;
        };
        for dep in &resolved.depends_cask {
            if prefix.installed_one(Kind::Cask, dep).is_none() && !out.contains(dep) {
                out.push(dep.clone());
                if let Ok(dc) = api.cask(dep) {
                    walk(api, prefix, &dc, out);
                }
            }
        }
        for dep in &resolved.depends_formula {
            let short = super::Formula::short_name(dep).to_string();
            if prefix.installed_one(Kind::Formula, &short).is_none() && !out.contains(&short) {
                out.push(short);
            }
        }
    }
    let mut out = Vec::new();
    walk(api, prefix, cask, &mut out);
    out
}

/// Install one cask the user named (on request).
pub fn install(
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    force: bool,
    upgrade: bool,
    reinstall: bool,
) -> Result<String, InstallError> {
    install_inner(prefix, cask, dry_run, force, upgrade, reinstall, true)
}

#[allow(clippy::too_many_arguments)]
fn install_inner(
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    force: bool,
    // Kept for the install/upgrade/reinstall caller shape; an
    // outdated install is replaced whichever verb brought us here.
    _upgrade: bool,
    reinstall: bool,
    on_request: bool,
) -> Result<String, InstallError> {
    if cask.disabled {
        return Err(InstallError::Unsupported(format!(
            "cask {} is disabled upstream",
            cask.token
        )));
    }
    let resolved = resolve(cask, Machine::this())?;
    let cask: &Cask = &resolved;
    preflight(prefix, cask, Machine::this())?;
    if cask.deprecated {
        eprintln!("abrew: warning: cask {} is deprecated upstream", cask.token);
    }
    if !super::valid_component(&cask.token) || !super::valid_cask_version(&cask.version) {
        return Err(InstallError::Unsupported(
            "unsupported cask token/version".to_string(),
        ));
    }
    let mut replacing = false;
    let mut same_version_reinstall = false;
    let mut effective_on_request = on_request;
    if let Some(inst) = prefix.installed_one(Kind::Cask, &cask.token) {
        if let Some(old) = super::prefix::receipt_word(
            &prefix.cask_dir(&cask.token, &inst.version),
            "installed_on_request",
        ) {
            effective_on_request = old == "true";
        }
        let older =
            super::formula::version_cmp(&inst.version, &cask.version) == std::cmp::Ordering::Less;
        if reinstall && inst.version == cask.version {
            replacing = true;
            same_version_reinstall = true;
        } else if older {
            // Brew's plain cask install upgrades an outdated cask
            // (its "Not upgrading" notice below is the current case),
            // preserving the receipt's on-request intent read above.
            replacing = true;
        } else if force || reinstall {
            return Err(InstallError::Unsupported(format!(
                "replacement of cask {} needs brew",
                cask.token
            )));
        } else {
            eprintln!(
                "Warning: Not upgrading {}, the latest version is already installed",
                cask.token
            );
            return Ok(inst.version);
        }
    }
    let file = download_name(cask);
    if (!cask.naked
        && !autodetect_naked(cask)
        && git_raw_plan(cask).is_none()
        && !fetch::archive_supported(&file)
        && !fetch::is_dmg(&file))
        || cask.artifacts.is_empty()
    {
        return Err(InstallError::Unsupported(format!(
            "cask {} needs its Homebrew extraction recipe",
            cask.token
        )));
    }
    let artifacts = artifact_paths(prefix, cask)?;
    let staged = prefix.cask_dir(&cask.token, &cask.version);
    if !same_version_reinstall && std::fs::symlink_metadata(&staged).is_ok() {
        return Err(InstallError::Failed(format!(
            "abrew: unrecognized cask directory {}",
            staged.display()
        )));
    }
    for (_, dst, _, _) in &artifacts {
        if std::fs::symlink_metadata(dst).is_ok() {
            let own = replacing
                && std::fs::read_link(dst).is_ok_and(|target| {
                    super::install::replacement_target_owned(
                        prefix,
                        dst,
                        &target,
                        &prefix.caskroom().join(&cask.token),
                    )
                });
            if !own {
                return Err(InstallError::Failed(format!(
                    "abrew: refusing existing artifact {}",
                    dst.display()
                )));
            }
        }
    }
    if dry_run {
        // The command layer prints brew's grouped dry-run summary; a
        // dry cask install itself stays silent.
        return Ok(cask.version.clone());
    }
    prefix.ensure()?;
    let git_plan = git_raw_plan(cask);
    let dest = prefix.cache().join(&file);
    if git_plan.is_none() {
        let expected = cask.sha256.as_deref().unwrap_or("no_check");
        let reusable = cask.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
        if !reusable {
            eprintln!("==> Fetching cask {} ({})", cask.token, cask.version);
            fetch::curl_to_file(&cask.url, &dest, &[])?;
        }
        fetch::verify_sha256(&dest, expected)?;
    } else {
        eprintln!("==> Fetching cask {} ({})", cask.token, cask.version);
    }
    let work =
        aterm_tempfile::TempDir::new_in(prefix.caskroom()).map_err(|e| format!("abrew: {e}"))?;
    let payload = work.path().join("payload");
    // Explicit naked always stages the file itself; an
    // autodetected naked download whose bytes say archive is
    // extracted (or delegated) instead.
    let naked = cask.naked || (autodetect_naked(cask) && !archive_magic(&dest));
    if let Some(plan) = &git_plan {
        std::fs::create_dir_all(&payload).map_err(|e| format!("abrew: {e}"))?;
        for (source, cached) in fetch_git_raw(prefix, cask, plan)? {
            fetch::copy_regular(&cached, &payload.join(&source))?;
        }
    } else if naked {
        std::fs::create_dir_all(&payload).map_err(|e| format!("abrew: {e}"))?;
        let cached_name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let base = cached_name
            .strip_prefix(&format!("{}-", cask.token))
            .unwrap_or(&cached_name)
            .to_string();
        let staged_file = payload.join(&base);
        fetch::copy_regular(&dest, &staged_file)?;
        for (relative, _, _, _) in &artifacts {
            if relative.components().count() == 1
                && relative.to_string_lossy() != base.as_str()
                && !relative.starts_with(".abrew-wrappers")
            {
                fetch::copy_regular(&dest, &payload.join(relative))?;
            }
        }
        let wants_exec = artifacts.iter().any(|(_, _, _, exec)| *exec);
        #[cfg(unix)]
        if wants_exec {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged_file, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("abrew: {e}"))?;
            for (relative, _, _, exec) in &artifacts {
                if *exec {
                    let f = payload.join(relative);
                    if f.is_file() {
                        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755))
                            .map_err(|e| format!("abrew: {e}"))?;
                    }
                }
            }
        }
        let _ = wants_exec;
    }
    if !naked && git_plan.is_none() {
        let extraction_error = |e: String| extraction_error(&cask.token, e);
        if let Some(nested) = &cask.nested {
            let outer = work.path().join("outer");
            std::fs::create_dir_all(&outer).map_err(|e| format!("abrew: {e}"))?;
            fetch::untar(&dest, &outer).map_err(extraction_error)?;
            let inner = outer.join(nested);
            if !std::fs::symlink_metadata(&inner)
                .is_ok_and(|m| m.file_type().is_file() && !m.file_type().is_symlink())
            {
                return Err(InstallError::Failed(format!(
                    "abrew: cask {} has no nested container {nested}",
                    cask.token
                )));
            }
            fetch::untar(&inner, &payload).map_err(extraction_error)?;
        } else if fetch::is_dmg(&file) {
            fetch::extract_dmg(&dest, &payload).map_err(|e| dmg_error(&cask.token, e))?;
        } else {
            fetch::untar(&dest, &payload).map_err(extraction_error)?;
        }
        extract_nested_tars(&payload, &artifacts, &cask.token)?;
    }
    println!("==> Installing Cask {}", cask.token);
    stage_wrappers(prefix, cask, &payload, &staged)?;
    for (relative, _, app, exec) in &artifacts {
        let src = payload.join(relative);
        let valid = if *app {
            src.is_dir()
        } else if *exec {
            src.is_file()
        } else {
            src.is_file() || src.is_dir()
        };
        #[cfg(unix)]
        let valid = {
            use std::os::unix::fs::PermissionsExt;
            valid
                && (!*exec
                    || src
                        .metadata()
                        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
        };
        #[cfg(not(unix))]
        let _ = exec;
        if !valid {
            return Err(InstallError::Failed(format!(
                "abrew: cask {} has no artifact {}",
                cask.token,
                relative.display()
            )));
        }
    }
    let links: Vec<InstallLink> = artifacts
        .iter()
        .map(|(relative, destination, _directory, _exec)| InstallLink {
            source: staged.join(relative),
            destination: destination.clone(),
            directory: payload.join(relative).is_dir(),
        })
        .collect();
    let artifact_fields = complete_artifact_receipt_fields(prefix, &links)?;
    let on_request_word = if effective_on_request {
        "true"
    } else {
        "false"
    };
    let mut receipt_fields: Vec<(&str, &str)> = vec![
        ("token", &cask.token),
        ("version", &cask.version),
        ("installed_by", "abrew"),
        ("installed_on_request", on_request_word),
        ("full_name", &cask.token),
    ];
    for (k, v) in &artifact_fields {
        receipt_fields.push((k, v));
    }
    prefix.write_receipt(&payload, &receipt_fields)?;
    for (artifact, (path, dst, _, _)) in cask.artifacts.iter().zip(&artifacts) {
        let (label, linked) = artifact_label(artifact);
        let base = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!(
            "==> {} {label} '{base}' to '{}'",
            if linked { "Linking" } else { "Moving" },
            dst.display()
        );
    }
    if same_version_reinstall {
        super::install::publish_payload_reinstall(
            prefix,
            &payload,
            &staged,
            &links,
            &prefix.caskroom().join(&cask.token),
        )?;
    } else if replacing {
        super::install::publish_payload_replacing(
            prefix,
            &payload,
            &staged,
            &links,
            &prefix.caskroom().join(&cask.token),
        )?;
    } else {
        publish_payload(prefix, &payload, &staged, &links)?;
    }
    prefix.rebuild_state()?;
    let verb = if replacing && !same_version_reinstall {
        "upgraded"
    } else {
        "installed"
    };
    match super::install_badge() {
        Some(badge) => println!("{badge}  {} was successfully {verb}!", cask.token),
        None => println!("{} was successfully {verb}!", cask.token),
    }
    super::install::note_bin_not_on_path(prefix, false);
    Ok(cask.version.clone())
}

/// Record the complete destination set only when every path is lossless UTF-8.
fn artifact_receipt_fields(links: &[InstallLink]) -> Vec<(String, String)> {
    links
        .iter()
        .enumerate()
        .map(|(i, link)| {
            Some((
                format!("artifact_{i}"),
                link.destination.to_str()?.to_string(),
            ))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// New writes record prefix-relative names too.
fn complete_artifact_receipt_fields(
    prefix: &Prefix,
    links: &[InstallLink],
) -> Result<Vec<(String, String)>, InstallError> {
    let mut fields = artifact_receipt_fields(links);
    for (index, link) in links.iter().enumerate() {
        let relative = link
            .destination
            .strip_prefix(&prefix.root)
            .map_err(|_| "abrew: artifact destination escapes prefix".to_string())?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("abrew: invalid relative artifact destination"
                .to_string()
                .into());
        }
        let name = relative
            .to_str()
            .ok_or_else(|| "abrew: artifact destination is not lossless UTF-8".to_string())?;
        fields.push((format!("relative_artifact_{index}"), name.to_string()));
    }
    Ok(fields)
}

fn recorded_artifact_paths(
    prefix: &Prefix,
    value: &aterm_json::Value,
) -> Result<Option<BTreeSet<PathBuf>>, InstallError> {
    let aterm_json::Value::Object(map) = value else {
        return Err("abrew: invalid cask artifact receipt".to_string().into());
    };
    let mut relative = BTreeSet::new();
    for (key, value) in map
        .iter()
        .filter(|(key, _)| key.starts_with("relative_artifact_"))
    {
        let name = value
            .as_str()
            .ok_or_else(|| format!("abrew: invalid cask artifact {key}"))?;
        let path = Path::new(name);
        if name.is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(format!("abrew: invalid relative cask artifact {key}").into());
        }
        relative.insert(prefix.root.join(path));
    }
    if relative.is_empty() && prefix.root.to_str().is_none() {
        return Ok(None);
    }
    let mut absolute = BTreeSet::new();
    for (key, value) in map.iter().filter(|(key, _)| key.starts_with("artifact_")) {
        let name = value
            .as_str()
            .ok_or_else(|| format!("abrew: invalid cask artifact {key}"))?;
        let path = PathBuf::from(name);
        if !path.is_absolute()
            || path.file_name().is_none()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(format!("abrew: invalid absolute cask artifact {key}").into());
        }
        absolute.insert(path);
    }
    if !relative.is_empty() {
        if prefix.root.to_str().is_some() && !absolute.is_empty() && absolute != relative {
            return Err("abrew: conflicting cask artifact records"
                .to_string()
                .into());
        }
        return Ok(Some(relative));
    }
    Ok((!absolute.is_empty()).then_some(absolute))
}

/// Missing, unreadable and malformed receipts are not an absent restriction.
fn recorded_artifacts(
    prefix: &Prefix,
    payload: &Path,
) -> Result<Option<BTreeSet<PathBuf>>, InstallError> {
    let bytes =
        crate::metadata_io::read_bounded_regular(&payload.join("INSTALL_RECEIPT.json"), 64 * 1024)
            .map_err(|e| format!("abrew: cannot read cask artifacts: {e}"))?;
    let value = aterm_json::from_slice(&bytes)
        .map_err(|e| format!("abrew: invalid cask artifacts: {e}"))?;
    recorded_artifact_paths(prefix, &value)
}

/// Percent-encode one URL path segment (raw-blob URLs name the
/// file exactly; brackets and spaces in font names must escape).
fn pct_segment(seg: &str) -> String {
    let mut out = String::new();
    for b in seg.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The (source, raw URL) fetch plan for a GitHub git-checkout
/// cask, when it is exactly raw-fetchable: a github.com
/// owner/repo.git URL, a parsed branch+only_path, `no_check`
/// (per-file bytes cannot carry the cask checksum), and every
/// artifact source a single file inside only_path. The bytes
/// are the same repo blobs brew's clone would stage; only the
/// transport differs.
pub(super) fn git_raw_plan(cask: &Cask) -> Option<Vec<(String, String)>> {
    if cask.sha256.is_some() || cask.nested.is_some() {
        return None;
    }
    let (branch, only_path) = cask.git_branch_path.as_ref()?;
    let rest = cask.url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo_git) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || !super::valid_component(owner) {
        return None;
    }
    let repo = repo_git.strip_suffix(".git")?;
    if !super::valid_component(repo) {
        return None;
    }
    let mut plan: Vec<(String, String)> = Vec::new();
    for artifact in &cask.artifacts {
        let source = artifact_source(artifact)?;
        if Path::new(source).components().count() != 1 {
            return None;
        }
        let source = source.to_string();
        if plan.iter().any(|(s, _)| *s == source) {
            continue;
        }
        let mut url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            pct_segment(owner),
            pct_segment(repo),
            pct_segment(branch)
        );
        for seg in only_path.split('/') {
            url.push('/');
            url.push_str(&pct_segment(seg));
        }
        url.push('/');
        url.push_str(&pct_segment(&source));
        plan.push((source, url));
    }
    if plan.is_empty() { None } else { Some(plan) }
}

/// Fetch a git-checkout cask's files next to each other under
/// the cache name `token-<source>`, refusing Git-LFS pointer
/// files (their bytes are not the payload). Returns each
/// source's cached path.
pub(super) fn fetch_git_raw(
    prefix: &Prefix,
    cask: &Cask,
    plan: &[(String, String)],
) -> Result<Vec<(String, PathBuf)>, InstallError> {
    let mut out = Vec::new();
    for (source, url) in plan {
        let dest = prefix.cache().join(format!("{}-{}", cask.token, source));
        fetch::curl_to_file(url, &dest, &[])?;
        let head = std::fs::read(&dest).map_err(|e| format!("abrew: {e}"))?;
        if head.starts_with(b"version https://git-lfs.github.com") {
            let _ = std::fs::remove_file(&dest);
            return Err(InstallError::Unsupported(format!(
                "cask {} ships Git-LFS payloads, which need a real git checkout",
                cask.token
            )));
        }
        out.push((source.clone(), dest));
    }
    Ok(out)
}

/// Brew autodetects the container from the download: a URL
/// naming no known archive is a naked download — the file itself
/// is the payload (most font casks ship one raw .ttf). Admission
/// stays narrow: no explicit container, no archive or dmg name,
/// no VCS (.git) or pkg URL, and exactly one distinct
/// single-component artifact source for the one file to carry.
/// Archive magic in the downloaded bytes still overrides this at
/// staging (see `archive_magic`), exactly like brew's autodetect.
fn autodetect_naked(cask: &Cask) -> bool {
    if cask.naked || cask.nested.is_some() {
        return false;
    }
    let file = download_name(cask);
    if fetch::archive_supported(&file) || fetch::is_dmg(&file) {
        return false;
    }
    let url_path = cask.url.split('?').next().unwrap_or(&cask.url);
    if url_path.ends_with(".git") || url_path.to_ascii_lowercase().ends_with(".pkg") {
        return false;
    }
    let mut source: Option<&str> = None;
    for artifact in &cask.artifacts {
        let Some(s) = artifact_source(artifact) else {
            return false;
        };
        if Path::new(s).components().count() != 1 {
            return false;
        }
        match source {
            None => source = Some(s),
            Some(prev) if prev != s => return false,
            _ => {}
        }
    }
    source.is_some()
}

/// One artifact's payload source, where it has one.
fn artifact_source(a: &Artifact) -> Option<&str> {
    match a {
        Artifact::App(s, _)
        | Artifact::Binary(s, _)
        | Artifact::Font(s, _)
        | Artifact::Pkg(s)
        | Artifact::Manpage(s, _)
        | Artifact::Suite(s, _)
        | Artifact::KeyboardLayout(s, _)
        | Artifact::ScreenSaver(s, _)
        | Artifact::PrefPane(s, _)
        | Artifact::Service(s, _)
        | Artifact::Generic(s, _)
        | Artifact::Library(s, _, _) => Some(s),
        Artifact::Wrapper(_, exe, _, _) => Some(exe),
        Artifact::Other(_) => None,
    }
}

/// Whether a downloaded file's magic says archive (gzip/zstd/zip
/// the vetted extractor speaks, or bzip2/xz it refuses): an
/// autodetected-naked download with archive magic is extracted
/// (or delegated) instead, like brew's container autodetect.
fn archive_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 6];
    if f.read_exact(&mut magic).is_err() {
        return false;
    }
    magic.starts_with(&[0x1f, 0x8b])
        || magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || magic.starts_with(b"PK")
        || magic.starts_with(b"BZh")
        || magic == *b"\xfd7zXZ\0"
}

pub(super) fn download_name(cask: &Cask) -> String {
    let base = cask
        .url
        .split('?')
        .next()
        .unwrap_or(&cask.url)
        .rsplit('/')
        .next()
        .unwrap_or("download");
    format!(
        "{}-{}",
        cask.token,
        base.replace("#{version}", &cask.version)
    )
}

/// Validate the program mapping independently of payload IO, before download.
fn wrapper_program(
    prefix: &Prefix,
    cask: &Cask,
    executable: &str,
) -> Result<Option<PathBuf>, InstallError> {
    if prefix.root.to_str().is_none() {
        return Err(InstallError::Unsupported(
            "command wrappers need a lossless prefix path".to_string(),
        ));
    }
    if !executable.contains('/') {
        if !super::valid_component(executable) || executable.starts_with('-') {
            return Err(InstallError::Unsupported(
                "unsupported wrapper program name".to_string(),
            ));
        }
        return Ok(None);
    }
    payload_source(cask, executable)
        .map(Some)
        .ok_or_else(|| unmapped_source(cask))
}

/// Unpack a single same-named nested tar if present.
fn dmg_error(token: &str, e: fetch::DmgError) -> InstallError {
    match e {
        fetch::DmgError::Attach(_) => InstallError::Unsupported(format!(
            "cask {token} container format needs Homebrew extraction"
        )),
        #[cfg(any(target_os = "macos", test))]
        other => InstallError::Failed(other.to_string()),
    }
}

/// Extract a zip archive using the system `unzip` when the strict reader refuses it.
fn extraction_error(token: &str, e: String) -> InstallError {
    if e.contains("unsupported archive") || e.contains("io: zip: ") {
        InstallError::Unsupported(format!(
            "cask {token} container format needs Homebrew extraction"
        ))
    } else {
        InstallError::Failed(e)
    }
}

fn extract_nested_tars(
    payload: &Path,
    artifacts: &[(PathBuf, PathBuf, bool, bool)],
    token: &str,
) -> Result<(), InstallError> {
    for (relative, _, _, _) in artifacts {
        if payload.join(relative).exists() {
            continue;
        }
        let nested = payload.join(format!("{}.tar", relative.display()));
        if std::fs::symlink_metadata(&nested)
            .is_ok_and(|m| m.file_type().is_file() && !m.file_type().is_symlink())
        {
            let staging = aterm_tempfile::TempDir::new_in(payload)
                .map_err(|e| InstallError::Failed(format!("abrew: {e}")))?;
            fetch::untar(&nested, staging.path()).map_err(|e| extraction_error(token, e))?;
            let entries = std::fs::read_dir(staging.path())
                .map_err(|e| InstallError::Failed(format!("abrew: {e}")))?;
            for entry in entries {
                let entry = entry.map_err(|e| InstallError::Failed(format!("abrew: {e}")))?;
                let dest = payload.join(entry.file_name());
                if std::fs::symlink_metadata(&dest).is_ok() {
                    return Err(InstallError::Failed(format!(
                        "abrew: cask {token} nested archive would overwrite {}",
                        dest.display()
                    )));
                }
                std::fs::rename(entry.path(), &dest)
                    .map_err(|e| InstallError::Failed(format!("abrew: {e}")))?;
            }
        }
    }
    Ok(())
}

/// Reserve a new private namespace; an archive-owned entry is never a writer target.
fn stage_wrappers(
    prefix: &Prefix,
    cask: &Cask,
    payload: &Path,
    staged: &Path,
) -> Result<(), InstallError> {
    let wrappers: Vec<_> = cask
        .artifacts
        .iter()
        .filter_map(|artifact| match artifact {
            Artifact::Wrapper(name, exe, args, dest) => Some((name, exe, args, dest)),
            _ => None,
        })
        .collect();
    if wrappers.is_empty() {
        return Ok(());
    }
    let directory = payload.join(".abrew-wrappers");
    std::fs::create_dir(&directory)
        .map_err(|error| format!("abrew: cannot reserve wrapper directory: {error}"))?;
    for (name, executable, args, destination) in wrappers {
        let command = match wrapper_program(prefix, cask, executable)? {
            None => executable.clone(),
            Some(relative) => {
                let source = payload.join(&relative);
                let metadata = std::fs::metadata(&source)
                    .map_err(|error| format!("abrew: missing wrapper program: {error}"))?;
                if !metadata.is_file() {
                    return Err("abrew: wrapper program is not a file".to_string().into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 {
                        return Err("abrew: wrapper program is not executable"
                            .to_string()
                            .into());
                    }
                }
                staged
                    .join(relative)
                    .to_str()
                    .ok_or_else(|| "abrew: wrapper path is not UTF-8".to_string())?
                    .to_string()
            }
        };
        let file = destination
            .as_deref()
            .or_else(|| Path::new(name).file_name().and_then(|name| name.to_str()))
            .filter(|name| super::valid_component(name))
            .ok_or_else(|| "abrew: invalid wrapper name".to_string())?;
        let mut script = format!(
            "#!/bin/sh\n# abrew command wrapper for {}\nexec {}",
            cask.token,
            shell_quote(&command)
        );
        for argument in args {
            let argument = argument
                .replace("$HOMEBREW_PREFIX/", &format!("{}/", prefix.root.display()))
                .replace("$APPDIR/", &format!("{}/", prefix.applications().display()));
            script.push(' ');
            script.push_str(&shell_quote(&argument));
        }
        script.push_str(" \"$@\"\n");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o755);
        }
        let mut writer = options
            .open(directory.join(file))
            .map_err(|error| format!("abrew: cannot stage wrapper: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            writer
                .set_permissions(std::fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("abrew: cannot set wrapper mode: {error}"))?;
        }
        use std::io::Write as _;
        writer
            .write_all(script.as_bytes())
            .map_err(|error| format!("abrew: cannot write wrapper: {error}"))?;
    }
    Ok(())
}

/// The payload-relative path an artifact source (or a wrapper's executable)
/// names, or `None` when it names nothing in this cask's payload.
fn payload_source(cask: &Cask, name: &str) -> Option<PathBuf> {
    let own = format!("$HOMEBREW_PREFIX/Caskroom/{}/{}/", cask.token, cask.version);
    let staged = |name: &str| -> Option<String> {
        match name.strip_prefix(own.as_str()) {
            Some(tail) => Some(tail.to_string()),
            None => (!name.starts_with("$HOMEBREW_PREFIX/")).then(|| name.to_string()),
        }
    };
    let prefixed = |name: &str| -> Option<String> {
        let Some(rest) = name
            .strip_prefix("$HOMEBREW_PREFIX/")
            .filter(|_| !name.starts_with(own.as_str()))
        else {
            return staged(name);
        };
        cask.artifacts.iter().find_map(|artifact| {
            let Artifact::Generic(src, Some(target)) = artifact else {
                return None;
            };
            let tail = target
                .strip_prefix("$HOMEBREW_PREFIX/")
                .and_then(|t| rest.strip_prefix(t))
                .and_then(|t| t.strip_prefix('/'))?;
            Some(format!("{}/{tail}", staged(src)?))
        })
    };
    let source = match name.strip_prefix("/Applications/") {
        None => prefixed(name)?,
        Some(rest) => cask.artifacts.iter().find_map(|artifact| {
            let (installed, payload) = match artifact {
                Artifact::App(src, dest) | Artifact::Suite(src, dest) => (
                    dest.as_deref()
                        .or_else(|| Path::new(src).file_name()?.to_str())?,
                    prefixed(src)?,
                ),
                Artifact::Generic(src, Some(target)) => {
                    (target.strip_prefix("$APPDIR/")?, staged(src)?)
                }
                _ => return None,
            };
            let (head, tail) = rest.split_at_checked(installed.len())?;
            let tail = tail
                .strip_prefix('/')
                .filter(|_| head.eq_ignore_ascii_case(installed))?;
            Some(format!("{payload}/{tail}"))
        })?,
    };
    let path = PathBuf::from(&source);
    (!source.is_empty()
        && !source.starts_with('$')
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_))))
    .then_some(path)
}

/// The refusal for an artifact source [`payload_source`] cannot map.
fn unmapped_source(cask: &Cask) -> InstallError {
    InstallError::Unsupported(format!(
        "cask {} artifact sources from outside its payload",
        cask.token
    ))
}

/// One single-quoted POSIX shell word.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The payload-relative path a command wrapper is generated at.
fn wrapper_rel(file: &str) -> PathBuf {
    PathBuf::from(".abrew-wrappers").join(file)
}

/// Map a generic artifact's absolute stated target into the prefix.
fn map_generic_target(prefix: &Prefix, target: &str) -> Option<PathBuf> {
    super::formula::generic_target_relative(target).map(|relative| prefix.root.join(&*relative))
}

/// (payload-relative source, destination, is-directory, must-be-executable).
/// Brew's per-artifact display class: its `english_name` and whether
/// the class links (binary, manpage, completions — brew's Symlinked)
/// rather than moves (everything else) its payload. Drives the
/// `==> Moving/Linking …` and `==> Removing/Unlinking …` lines.
fn artifact_label(artifact: &Artifact) -> (&'static str, bool) {
    match artifact {
        Artifact::App(..) => ("App", false),
        Artifact::Binary(..) | Artifact::Wrapper(..) => ("Binary", true),
        Artifact::Font(..) => ("Font", false),
        Artifact::Manpage(..) => ("Manpage", true),
        Artifact::Suite(..) => ("App Suite", false),
        Artifact::KeyboardLayout(..) => ("Keyboard Layout", false),
        Artifact::ScreenSaver(..) => ("Screen Saver", false),
        Artifact::PrefPane(..) => ("Preference Pane", false),
        Artifact::Service(..) => ("Service", false),
        Artifact::Generic(..) => ("Generic Artifact", false),
        Artifact::Library(_, _, dir) => library_artifact_label(dir),
        _ => ("Generic Artifact", false),
    }
}

fn library_artifact_label(dir: &str) -> (&'static str, bool) {
    match dir {
        "Library/QuickLook" => ("Quick Look Plugin", false),
        "Library/ColorPickers" => ("Colorpicker", false),
        "Library/Dictionaries" => ("Dictionary", false),
        "Library/Input Methods" => ("Input Method", false),
        "Library/Internet Plug-Ins" => ("Internet Plugin", false),
        "Library/Spotlight" => ("Spotlight metadata importer", false),
        "Library/Audio/Plug-Ins/Components" => ("Audio Unit Plugin", false),
        "Library/Audio/Plug-Ins/VST" => ("VST Plugin", false),
        "Library/Audio/Plug-Ins/VST3" => ("VST3 Plugin", false),
        "share/bash-completion/completions" => ("Bash Completion", true),
        "share/zsh/site-functions" => ("Zsh Completion", true),
        "share/fish/vendor_completions.d" => ("Fish Completion", true),
        _ => ("Generic Artifact", false),
    }
}

/// The same classification from a placed destination alone, for
/// uninstall lines (the receipt records destinations, not kinds).
fn destination_label(prefix: &Prefix, dst: &Path) -> (&'static str, bool) {
    let rel = dst.strip_prefix(&prefix.root).unwrap_or(dst);
    let rel = rel.to_string_lossy();
    if rel.starts_with("bin/") {
        ("Binary", true)
    } else if rel.starts_with("share/man/") {
        ("Manpage", true)
    } else if rel.starts_with("Library/Fonts/") {
        ("Font", false)
    } else if rel.starts_with("Library/Keyboard Layouts/") {
        ("Keyboard Layout", false)
    } else if rel.starts_with("Library/Screen Savers/") {
        ("Screen Saver", false)
    } else if rel.starts_with("Library/PreferencePanes/") {
        ("Preference Pane", false)
    } else if rel.starts_with("Library/Services/") {
        ("Service", false)
    } else if let Some(dir) = rel.rsplit_once('/').map(|(d, _)| d) {
        let (label, linked) = library_artifact_label(dir);
        if label != "Generic Artifact" {
            (label, linked)
        } else if dst.starts_with(prefix.applications()) {
            ("App", false)
        } else {
            ("Generic Artifact", false)
        }
    } else {
        ("Generic Artifact", false)
    }
}

fn artifact_paths(
    prefix: &Prefix,
    cask: &Cask,
) -> Result<Vec<(PathBuf, PathBuf, bool, bool)>, InstallError> {
    let mut paths = Vec::new();
    let mut destinations = BTreeSet::new();
    for artifact in &cask.artifacts {
        let (name, dest_name, dir, app, exec) = match artifact {
            Artifact::App(name, dest) => (name, dest, prefix.applications(), true, false),
            Artifact::Binary(name, dest) => (name, dest, prefix.bin(), false, true),
            Artifact::Font(name, dest) => (
                name,
                dest,
                prefix.root.join("Library").join("Fonts"),
                false,
                false,
            ),
            Artifact::Manpage(name, dest) => {
                let section = Path::new(name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .filter(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric()));
                let Some(section) = section else {
                    return Err(InstallError::Unsupported(format!(
                        "cask {} manpage needs a Homebrew section mapping",
                        cask.token
                    )));
                };
                (
                    name,
                    dest,
                    prefix.root.join("share/man").join(format!("man{section}")),
                    false,
                    false,
                )
            }
            Artifact::Suite(name, dest) => (name, dest, prefix.applications(), true, false),
            Artifact::KeyboardLayout(name, dest) => (
                name,
                dest,
                prefix.root.join("Library").join("Keyboard Layouts"),
                false,
                false,
            ),
            Artifact::ScreenSaver(name, dest) => (
                name,
                dest,
                prefix.root.join("Library").join("Screen Savers"),
                false,
                false,
            ),
            Artifact::PrefPane(name, dest) => (
                name,
                dest,
                prefix.root.join("Library").join("PreferencePanes"),
                false,
                false,
            ),
            Artifact::Service(name, dest) => (
                name,
                dest,
                prefix.root.join("Library").join("Services"),
                false,
                false,
            ),
            Artifact::Library(name, dest, directory) => {
                if directory.is_empty()
                    || Path::new(directory)
                        .components()
                        .any(|part| !matches!(part, Component::Normal(_)))
                {
                    return Err(InstallError::Unsupported(
                        "invalid prefix-relative artifact directory".to_string(),
                    ));
                }
                (name, dest, prefix.root.join(directory), false, false)
            }
            Artifact::Wrapper(name, executable, args, dest) => {
                wrapper_program(prefix, cask, executable)?;
                if args.iter().any(|argument| argument.contains('\0')) {
                    return Err(InstallError::Unsupported(
                        "wrapper arguments cannot contain NUL".to_string(),
                    ));
                }
                let file = dest
                    .clone()
                    .or_else(|| {
                        Path::new(name)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                    })
                    .filter(|f| super::valid_component(f));
                let Some(file) = file else {
                    return Err(InstallError::Unsupported(
                        "cask command wrapper needs a Homebrew name mapping".to_string(),
                    ));
                };
                let dst = prefix.bin().join(&file);
                if !destinations.insert(dst.clone()) {
                    return Err(InstallError::Unsupported(
                        "cask has overlapping artifact destinations".to_string(),
                    ));
                }
                paths.push((wrapper_rel(&file), dst, false, true));
                continue;
            }
            Artifact::Generic(name, target) => {
                let Some(dst) = target
                    .as_deref()
                    .and_then(|t| map_generic_target(prefix, t))
                else {
                    return Err(InstallError::Unsupported(format!(
                        "cask {} artifact target is outside abrew's prefix mapping",
                        cask.token
                    )));
                };
                if !destinations.insert(dst.clone()) {
                    return Err(InstallError::Unsupported(
                        "cask has overlapping artifact destinations".to_string(),
                    ));
                }
                let path = payload_source(cask, name).ok_or_else(|| unmapped_source(cask))?;
                paths.push((path, dst, false, false));
                continue;
            }
            Artifact::Other(kind) => {
                return Err(InstallError::Unsupported(format!(
                    "cask {} has an unsupported artifact recipe ({kind})",
                    cask.token
                )));
            }
            _ => {
                return Err(InstallError::Unsupported(format!(
                    "cask {} has an unsupported artifact recipe",
                    cask.token
                )));
            }
        };
        let path = payload_source(cask, name).ok_or_else(|| unmapped_source(cask))?;
        let dst = match dest_name {
            Some(dest) => dir.join(dest),
            None => dir.join(
                path.file_name()
                    .ok_or_else(|| "abrew: empty cask artifact".to_string())?,
            ),
        };
        let relative = dst.strip_prefix(&prefix.root).ok().filter(|relative| {
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
        });
        if relative.is_none() {
            return Err(InstallError::Unsupported(
                "artifact destination escapes the prefix".to_string(),
            ));
        }
        if !destinations.insert(dst.clone()) {
            return Err(InstallError::Unsupported(
                "cask has overlapping artifact destinations".to_string(),
            ));
        }
        paths.push((path.to_path_buf(), dst, app, exec));
    }
    Ok(paths)
}

/// Remove links into receipt-admitted versions and only those payloads.
pub fn uninstall(prefix: &Prefix, token: &str, dry_run: bool) -> Result<(), InstallError> {
    let directories = artifact_directories(prefix, PathBuf::from("/Applications"))?;
    uninstall_from(prefix, token, dry_run, &directories)
}

/// Legacy unrecorded artifacts used these roots.
fn artifact_directories(
    prefix: &Prefix,
    applications: PathBuf,
) -> Result<Vec<PathBuf>, InstallError> {
    let mut directories = vec![
        prefix.applications(),
        prefix.bin(),
        prefix.root.join("Library/Fonts"),
        prefix.root.join("Library/Keyboard Layouts"),
        prefix.root.join("Library/Screen Savers"),
        prefix.root.join("Library/PreferencePanes"),
        prefix.root.join("Library/Services"),
        applications,
    ];
    let mut pending = vec![
        (prefix.root.join("share"), 3_u8),
        (prefix.root.join("Library"), 2_u8),
    ];
    while let Some((directory, depth)) = pending.pop() {
        check_directory(&directory, &prefix.root)?;
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("abrew: cannot survey legacy artifacts: {error}").into());
            }
        };
        directories.push(directory);
        if depth == 0 {
            continue;
        }
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("abrew: cannot survey legacy artifact entry: {error}"))?;
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| format!("abrew: cannot inspect legacy artifact entry: {error}"))?;
            if metadata.file_type().is_dir() && !crate::platform::is_reparse(&metadata) {
                pending.push((entry.path(), depth - 1));
            }
        }
    }
    let man = prefix.root.join("share/man");
    if std::fs::symlink_metadata(&man).is_ok_and(|metadata| {
        metadata.file_type().is_symlink() || crate::platform::is_reparse(&metadata)
    }) {
        directories.push(man);
        return Ok(directories);
    }
    check_directory(&man, &prefix.root)?;
    let entries = match std::fs::read_dir(&man) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(directories),
        Err(error) => {
            return Err(format!("abrew: cannot survey manpage directories: {error}").into());
        }
    };
    directories.push(man);
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("abrew: cannot survey manpage entry: {error}"))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("abrew: cannot inspect manpage entry: {error}"))?;
        if entry.file_name().to_string_lossy().starts_with("man") {
            // A final artifact link belongs to its physical parent survey;
            // it is never traversed as a manpage directory.
            if kind.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    Ok(directories)
}

fn uninstall_from(
    prefix: &Prefix,
    token: &str,
    dry_run: bool,
    directories: &[PathBuf],
) -> Result<(), InstallError> {
    let Some(inst) = prefix.installed_one(Kind::Cask, token) else {
        return Err(InstallError::Failed(format!(
            "abrew: cask {token} is not installed"
        )));
    };
    if dry_run {
        println!("Would uninstall cask {token}");
        return Ok(());
    }
    let plan = UninstallPlan::prepare(prefix, token, &inst.versions, directories)?;
    println!("==> Uninstalling Cask {token}");
    for link in plan.links.keys() {
        let (label, linked) = destination_label(prefix, link);
        println!(
            "==> {} {label} '{}'",
            if linked { "Unlinking" } else { "Removing" },
            link.display()
        );
    }
    plan.apply(prefix)
}

struct UninstallPlan {
    token: String,
    base: PathBuf,
    payloads: BTreeMap<String, std::fs::Metadata>,
    parents: BTreeMap<PathBuf, std::fs::Metadata>,
    links: BTreeMap<PathBuf, (PathBuf, std::fs::Metadata)>,
}

impl UninstallPlan {
    fn prepare(
        prefix: &Prefix,
        token: &str,
        versions: &[String],
        directories: &[PathBuf],
    ) -> Result<Self, InstallError> {
        let base = prefix.caskroom().join(token);
        check_directory(&base, &prefix.root)?;
        let mut plan = Self {
            token: token.to_string(),
            parents: BTreeMap::from([(
                base.clone(),
                std::fs::symlink_metadata(&base).map_err(|e| format!("abrew: {e}"))?,
            )]),
            base,
            payloads: BTreeMap::new(),
            links: BTreeMap::new(),
        };
        for version in versions {
            let payload = plan.base.join(version);
            check_directory(&payload, &prefix.root)?;
            let identity =
                std::fs::symlink_metadata(&payload).map_err(|e| format!("abrew: {e}"))?;
            plan.payloads.insert(version.clone(), identity);
            plan.check_payload(prefix, version)?;
        }
        let mut recorded = BTreeMap::new();
        for version in plan.payloads.keys() {
            if let Some(paths) = recorded_artifacts(prefix, &plan.base.join(version))? {
                recorded.insert(version.clone(), paths);
            }
        }
        let mut directories: BTreeSet<PathBuf> = directories.iter().cloned().collect();
        for paths in recorded.values() {
            for path in paths {
                let parent = path
                    .parent()
                    .ok_or_else(|| "abrew: artifact has no parent".to_string())?;
                if path.starts_with(&prefix.root) {
                    check_directory(parent, &prefix.root)?;
                    directories.insert(parent.to_path_buf());
                } else if !directories.contains(parent) {
                    return Err("abrew: recorded artifact is outside the admitted roots"
                        .to_string()
                        .into());
                }
            }
        }
        for dir in &directories {
            // A declared final directory link is removed from its checked parent,
            // never surveyed through. Other directory aliases still refuse below.
            if recorded.iter().any(|(version, paths)| {
                paths.contains(dir)
                    && std::fs::symlink_metadata(dir)
                        .is_ok_and(|metadata| metadata.file_type().is_symlink())
                    && std::fs::read_link(dir).is_ok_and(|target| {
                        link_target_points_into(dir, &target, &plan.base.join(version))
                    })
            }) {
                continue;
            }
            if dir.starts_with(&prefix.root) {
                check_directory(dir, &prefix.root)?;
            }
            let identity = match std::fs::symlink_metadata(dir) {
                Ok(m) if m.file_type().is_dir() && !crate::platform::is_reparse(&m) => m,
                Ok(_) => {
                    return Err(format!(
                        "abrew: refusing nonphysical artifact directory {}",
                        dir.display()
                    )
                    .into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(format!(
                        "abrew: cannot inspect artifact directory {}: {e}",
                        dir.display()
                    )
                    .into());
                }
            };
            plan.parents.insert(dir.clone(), identity);
            for entry in std::fs::read_dir(dir).map_err(|e| format!("abrew: {e}"))? {
                let entry = entry.map_err(|e| format!("abrew: {e}"))?;
                let path = entry.path();
                let identity =
                    std::fs::symlink_metadata(&path).map_err(|e| format!("abrew: {e}"))?;
                if !identity.file_type().is_symlink() {
                    continue;
                }
                let target = std::fs::read_link(&path).map_err(|e| format!("abrew: {e}"))?;
                if plan.payloads.keys().any(|version| {
                    link_target_points_into(&path, &target, &plan.base.join(version))
                        && recorded
                            .get(version)
                            .is_none_or(|paths| paths.contains(&path))
                }) {
                    plan.links.insert(path, (target, identity));
                }
            }
        }
        Ok(plan)
    }

    fn check_parent(&self, prefix: &Prefix, parent: &Path) -> Result<(), InstallError> {
        if parent.starts_with(&prefix.root) {
            check_directory(parent, &prefix.root)?;
        }
        if self
            .parents
            .get(parent)
            .is_some_and(|identity| same_path_identity(parent, identity))
        {
            Ok(())
        } else {
            Err(format!(
                "abrew: cask directory changed before removal: {}",
                parent.display()
            )
            .into())
        }
    }

    fn check_link(&self, prefix: &Prefix, path: &Path) -> Result<(), InstallError> {
        let parent = path
            .parent()
            .ok_or_else(|| "abrew: artifact has no parent".to_string())?;
        self.check_parent(prefix, parent)?;
        if self.links.get(path).is_some_and(|(target, identity)| {
            same_path_identity(path, identity)
                && std::fs::read_link(path).is_ok_and(|actual| &actual == target)
        }) {
            Ok(())
        } else {
            Err(format!(
                "abrew: cask link changed before removal: {}",
                path.display()
            )
            .into())
        }
    }

    fn check_payload(&self, prefix: &Prefix, version: &str) -> Result<(), InstallError> {
        self.check_parent(prefix, &self.base)?;
        let payload = self.base.join(version);
        check_directory(&payload, &prefix.root)?;
        if self
            .payloads
            .get(version)
            .is_some_and(|identity| same_path_identity(&payload, identity))
            && owned_receipt(&payload, Kind::Cask, &self.token, version)
        {
            Ok(())
        } else {
            Err(format!(
                "abrew: cask payload changed before removal: {}",
                payload.display()
            )
            .into())
        }
    }

    fn apply(self, prefix: &Prefix) -> Result<(), InstallError> {
        for parent in self.parents.keys() {
            self.check_parent(prefix, parent)?;
        }
        for version in self.payloads.keys() {
            self.check_payload(prefix, version)?;
        }
        for link in self.links.keys() {
            self.check_link(prefix, link)?;
        }
        for link in self.links.keys() {
            self.check_link(prefix, link)?;
            std::fs::remove_file(link).map_err(|e| format!("abrew: {e}"))?;
        }
        for version in self.payloads.keys() {
            self.check_payload(prefix, version)?;
            std::fs::remove_dir_all(self.base.join(version)).map_err(|e| format!("abrew: {e}"))?;
        }
        self.check_parent(prefix, &self.base)?;
        let _ = std::fs::remove_dir(&self.base);
        prefix.rebuild_state()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::extract::fixtures::{ZipMember, tar_bytes, zip_bytes};

    #[test]
    fn generic_targets_map_only_into_the_prefix() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(tmp.path());
        assert_eq!(
            map_generic_target(&prefix, "$HOMEBREW_PREFIX/share/x/tree"),
            Some(prefix.root.join("share/x/tree"))
        );
        assert_eq!(
            map_generic_target(&prefix, "/$HOME/Library/Services/W.workflow"),
            Some(prefix.root.join("Library/Services/W.workflow"))
        );
        assert_eq!(
            map_generic_target(&prefix, "$APPDIR/Demo.app"),
            Some(prefix.root.join("Applications/Demo.app"))
        );
        assert_eq!(map_generic_target(&prefix, "$APPDIR/../x"), None);
        assert_eq!(map_generic_target(&prefix, "~/Documents/x"), None);
        assert_eq!(map_generic_target(&prefix, "/usr/local/x"), None);
        assert_eq!(map_generic_target(&prefix, "$HOMEBREW_PREFIX/../x"), None);
        let mut library = Cask {
            token: "demo".to_string(),
            artifacts: vec![Artifact::Library(
                "Demo.qlgenerator".to_string(),
                None,
                "Library/QuickLook".to_string(),
            )],
            ..Cask::default()
        };
        assert_eq!(
            artifact_paths(&prefix, &library).unwrap()[0].1,
            prefix.root.join("Library/QuickLook/Demo.qlgenerator")
        );
        for directory in ["../outside", "/outside"] {
            library.artifacts = vec![Artifact::Library(
                "Demo.qlgenerator".to_string(),
                None,
                directory.to_string(),
            )];
            assert!(artifact_paths(&prefix, &library).is_err());
        }
        library.artifacts = vec![Artifact::Library(
            "Demo.qlgenerator".to_string(),
            Some("/outside".to_string()),
            "Library/QuickLook".to_string(),
        )];
        assert!(artifact_paths(&prefix, &library).is_err());
        let payload = tmp.path().join("payload");
        std::fs::create_dir(&payload).unwrap();
        let wrapper = Cask {
            token: "demo".to_string(),
            artifacts: vec![Artifact::Wrapper(
                "wrap".to_string(),
                "java".to_string(),
                vec!["a'b".to_string()],
                None,
            )],
            ..Cask::default()
        };
        stage_wrappers(&prefix, &wrapper, &payload, &prefix.cask_dir("demo", "1")).unwrap();
        assert_eq!(
            std::fs::read_to_string(payload.join(wrapper_rel("wrap"))).unwrap(),
            "#!/bin/sh\n# abrew command wrapper for demo\nexec 'java' 'a'\\''b' \"$@\"\n"
        );
        assert!(
            stage_wrappers(&prefix, &wrapper, &payload, &prefix.cask_dir("demo", "1")).is_err()
        );
        assert!(wrapper_program(&prefix, &wrapper, "-java").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(payload.join(wrapper_rel("wrap")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            use std::os::unix::ffi::OsStringExt as _;
            let non_utf8 = Prefix::at(&PathBuf::from(std::ffi::OsString::from_vec(
                b"/captured-\xff".to_vec(),
            )));
            assert!(wrapper_program(&non_utf8, &wrapper, "java").is_err());
            let captured_link = InstallLink {
                source: non_utf8.cask_dir("demo", "1").join("tree"),
                destination: non_utf8.root.join("var/demo"),
                directory: true,
            };
            let fields = complete_artifact_receipt_fields(&non_utf8, &[captured_link]).unwrap();
            let value = aterm_json::Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, aterm_json::Value::String(value)))
                    .collect(),
            );
            assert_eq!(
                recorded_artifact_paths(&non_utf8, &value).unwrap(),
                Some(BTreeSet::from([non_utf8.root.join("var/demo")]))
            );
            let aliased_payload = tmp.path().join("aliased-payload");
            let outside = tmp.path().join("owned-outside");
            std::fs::create_dir(&aliased_payload).unwrap();
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("wrap"), b"preserved").unwrap();
            super::super::prefix::symlink_dir(&outside, &aliased_payload.join(".abrew-wrappers"))
                .unwrap();
            assert!(
                stage_wrappers(
                    &prefix,
                    &wrapper,
                    &aliased_payload,
                    &prefix.cask_dir("demo", "1")
                )
                .is_err()
            );
            assert_eq!(std::fs::read(outside.join("wrap")).unwrap(), b"preserved");
        }
    }

    fn cask(url: String) -> Cask {
        Cask {
            token: "demo".to_string(),
            version: "1".to_string(),
            url,
            artifacts: vec![
                Artifact::Binary("tool".to_string(), None),
                Artifact::App("Demo.app".to_string(), None),
                Artifact::Font("fonts/F.ttf".to_string(), Some("Renamed.ttf".to_string())),
                Artifact::Wrapper(
                    "demo".to_string(),
                    "/Applications/Demo.app/Contents/MacOS/demo".to_string(),
                    vec!["--fixed".to_string()],
                    None,
                ),
            ],
            ..Cask::default()
        }
    }

    #[test]
    #[cfg(unix)]
    fn no_check_download_installs_real_artifacts_and_uninstall_preserves_unknown_versions() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let archive = tmp.path().join("demo.zip");
        std::fs::write(
            &archive,
            zip_bytes(
                &[
                    ZipMember {
                        name: "tool",
                        mode: 0o100755,
                        data: b"#!/bin/sh\nprintf 'cask ready\\n'\n",
                        deflate: false,
                    },
                    ZipMember {
                        name: "Demo.app/Contents/Info.plist",
                        mode: 0o100644,
                        data: b"app",
                        deflate: false,
                    },
                    ZipMember {
                        name: "Demo.app/Contents/MacOS/demo",
                        mode: 0o100755,
                        data: b"#!/bin/sh\nprintf 'wrapped %s\\n' \"$*\"\n",
                        deflate: false,
                    },
                    ZipMember {
                        name: "fonts/F.ttf",
                        mode: 0o100644,
                        data: b"font",
                        deflate: false,
                    },
                ],
                false,
            ),
        )
        .unwrap();
        let cask = cask(format!("file://{}", archive.display()));
        assert!(cask.sha256.is_none());
        assert!(!prefix.cache().join(download_name(&cask)).exists());
        assert_eq!(
            install(&prefix, &cask, false, false, false, false).unwrap(),
            "1"
        );
        let output = aterm_uds::spawnfd::command(prefix.bin().join("tool"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"cask ready\n");
        assert_eq!(
            std::fs::read(prefix.applications().join("Demo.app/Contents/Info.plist")).unwrap(),
            b"app"
        );
        assert_eq!(
            super::super::prefix::receipt_word(
                &prefix.cask_dir("demo", "1"),
                "installed_on_request"
            )
            .as_deref(),
            Some("true"),
            "a named install records on-request intent"
        );
        assert_eq!(
            std::fs::read(prefix.root.join("Library/Fonts/Renamed.ttf")).unwrap(),
            b"font"
        );
        let wrapped = aterm_uds::spawnfd::command(prefix.bin().join("demo"))
            .arg("user")
            .output()
            .unwrap();
        assert!(wrapped.status.success());
        assert_eq!(wrapped.stdout, b"wrapped --fixed user\n");
        assert!(matches!(
            install(&prefix, &cask, false, true, false, false),
            Err(InstallError::Unsupported(_))
        ));
        let unknown = prefix.cask_dir("demo", "personal");
        std::fs::create_dir_all(&unknown).unwrap();
        std::fs::write(unknown.join("notes"), b"keep").unwrap();
        super::super::prefix::symlink_file(&unknown.join("notes"), &prefix.bin().join("personal"))
            .unwrap();
        uninstall(&prefix, "demo", false).unwrap();
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
        assert!(std::fs::symlink_metadata(prefix.applications().join("Demo.app")).is_err());
        assert!(std::fs::symlink_metadata(prefix.root.join("Library/Fonts/Renamed.ttf")).is_err());
        assert!(std::fs::symlink_metadata(prefix.bin().join("demo")).is_err());
        assert_eq!(
            std::fs::read(prefix.bin().join("personal")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn git_raw_plan_maps_github_checkout_to_raw_blobs_only() {
        let mut c = cask("https://github.com/google/fonts.git".to_string());
        c.git_branch_path = Some(("main".into(), "ofl/abeezee".into()));
        c.artifacts = vec![
            Artifact::Font("ABeeZee-Regular.ttf".into(), None),
            Artifact::Font("ABeeZee-Italic.ttf".into(), None),
        ];
        let plan = git_raw_plan(&c).expect("raw plan");
        assert_eq!(
            plan[0].1,
            "https://raw.githubusercontent.com/google/fonts/main/ofl/abeezee/ABeeZee-Regular.ttf"
        );
        assert_eq!(plan.len(), 2);
        // Bracketed variable-font names percent-encode.
        c.artifacts = vec![Artifact::Font("42dotSans[wght].ttf".into(), None)];
        assert!(
            git_raw_plan(&c).unwrap()[0]
                .1
                .ends_with("42dotSans%5Bwght%5D.ttf")
        );
        // A cask checksum cannot cover per-file blobs.
        c.sha256 = Some("0".repeat(64));
        assert!(git_raw_plan(&c).is_none());
        c.sha256 = None;
        // Non-GitHub hosts and deeper paths delegate.
        c.url = "https://gitlab.com/google/fonts.git".into();
        assert!(git_raw_plan(&c).is_none());
        c.url = "https://github.com/google/fonts.git".into();
        c.artifacts = vec![Artifact::Font("sub/Foo.ttf".into(), None)];
        assert!(git_raw_plan(&c).is_none());
    }

    #[test]
    fn autodetect_naked_admits_single_raw_file_downloads_only() {
        let font = |url: &str, source: &str| {
            let mut c = cask(url.to_string());
            c.artifacts = vec![Artifact::Font(source.to_string(), None)];
            c
        };
        assert!(autodetect_naked(&font(
            "https://example.com/Foo.ttf",
            "Foo.ttf"
        )));
        // Known archives and images are not naked.
        assert!(!autodetect_naked(&font(
            "https://example.com/Foo.zip",
            "Foo.ttf"
        )));
        assert!(!autodetect_naked(&font(
            "https://example.com/Foo.dmg",
            "Foo.ttf"
        )));
        // VCS checkouts and pkg downloads delegate.
        assert!(!autodetect_naked(&font(
            "https://github.com/example/fonts.git",
            "Foo.ttf"
        )));
        assert!(!autodetect_naked(&font(
            "https://example.com/Foo.pkg",
            "Foo.ttf"
        )));
        // One file cannot carry a nested or second source.
        assert!(!autodetect_naked(&font(
            "https://example.com/Foo.ttf",
            "dir/Foo.ttf"
        )));
        let mut two = font("https://example.com/Foo.ttf", "Foo.ttf");
        two.artifacts
            .push(Artifact::Font("Bar.ttf".to_string(), None));
        assert!(!autodetect_naked(&two));
    }

    #[test]
    #[cfg(unix)]
    fn naked_cask_stages_under_artifact_source_name() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let download = tmp.path().join("payload.bin");
        std::fs::write(&download, b"#!/bin/sh\nprintf 'naked ready\\n'\n").unwrap();
        let mut c = cask(format!("file://{}", download.display()));
        c.naked = true;
        c.artifacts = vec![Artifact::Binary("staged-name".to_string(), None)];
        assert_eq!(
            install(&prefix, &c, false, false, false, false).unwrap(),
            "1"
        );
        let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"naked ready\n");
        std::fs::write(&download, b"#!/bin/sh\nprintf 'naked ready2\\n'\n").unwrap();
        c.version = "2".to_string();
        // Brew's plain cask install upgrades an outdated cask.
        assert_eq!(
            install(&prefix, &c, false, false, false, false).unwrap(),
            "2"
        );
        assert_eq!(
            install(&prefix, &c, false, false, true, false).unwrap(),
            "2"
        );
        let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
            .output()
            .unwrap();
        assert_eq!(output.stdout, b"naked ready2\n");
        std::fs::write(&download, b"#!/bin/sh\nprintf 'naked readyR\\n'\n").unwrap();
        assert_eq!(install(&prefix, &c, false, true, false, true).unwrap(), "2");
        let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
            .output()
            .unwrap();
        assert_eq!(output.stdout, b"naked readyR\n");
        assert!(
            std::fs::symlink_metadata(prefix.caskroom().join(".abrew-reinstall-demo-2")).is_err()
        );
        uninstall(&prefix, "demo", false).unwrap();
        std::fs::write(&download, b"#!/bin/sh\nprintf 'dep ready\\n'\n").unwrap();
        c.version = "1".to_string();
        assert_eq!(
            install_inner(&prefix, &c, false, false, false, false, false).unwrap(),
            "1"
        );
        let word = |v: &str| {
            super::super::prefix::receipt_word(&prefix.cask_dir("demo", v), "installed_on_request")
        };
        assert_eq!(word("1").as_deref(), Some("false"));
        std::fs::write(&download, b"#!/bin/sh\nprintf 'dep ready3\\n'\n").unwrap();
        c.version = "3".to_string();
        assert_eq!(
            install(&prefix, &c, false, false, true, false).unwrap(),
            "3"
        );
        assert_eq!(
            word("3").as_deref(),
            Some("false"),
            "upgrade preserves intent"
        );
        uninstall(&prefix, "demo", false).unwrap();
        assert!(std::fs::symlink_metadata(prefix.bin().join("staged-name")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn container_nested_archive_becomes_the_payload() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let inner = zip_bytes(
            &[ZipMember {
                name: "Demo.app/Contents/MacOS/demo",
                mode: 0o100755,
                data: b"#!/bin/sh\nprintf 'container nested\\n'\n",
                deflate: false,
            }],
            false,
        );
        let archive = tmp.path().join("outer.zip");
        std::fs::write(
            &archive,
            zip_bytes(
                &[ZipMember {
                    name: "wrap/Demo.app.zip",
                    mode: 0o100644,
                    data: &inner,
                    deflate: false,
                }],
                false,
            ),
        )
        .unwrap();
        let mut c = cask(format!("file://{}", archive.display()));
        c.artifacts = vec![Artifact::App("Demo.app".to_string(), None)];
        c.nested = Some("wrap/Demo.app.zip".to_string());
        assert_eq!(
            install(&prefix, &c, false, false, false, false).unwrap(),
            "1"
        );
        let out = aterm_uds::spawnfd::command(
            prefix
                .caskroom()
                .join("demo/1/Demo.app/Contents/MacOS/demo"),
        )
        .output()
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "container nested\n");
    }

    #[test]
    #[cfg(unix)]
    fn nested_app_tar_inside_the_zip_is_extracted() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let inner = tar_bytes(&[
            ("Demo.app/Contents/Info.plist", b'0', "", b"app", 0o644),
            (
                "Demo.app/Contents/MacOS/demo",
                b'0',
                "",
                b"#!/bin/sh\nprintf 'nested ready\\n'\n",
                0o755,
            ),
        ]);
        let archive = tmp.path().join("demo.zip");
        std::fs::write(
            &archive,
            zip_bytes(
                &[ZipMember {
                    name: "Demo.app.tar",
                    mode: 0o100644,
                    data: &inner,
                    deflate: false,
                }],
                false,
            ),
        )
        .unwrap();
        let mut c = cask(format!("file://{}", archive.display()));
        c.artifacts = vec![Artifact::App("Demo.app".to_string(), None)];
        assert_eq!(
            install(&prefix, &c, false, false, false, false).unwrap(),
            "1"
        );
        let out = aterm_uds::spawnfd::command(
            prefix
                .caskroom()
                .join("demo/1/Demo.app/Contents/MacOS/demo"),
        )
        .output()
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "nested ready\n");
    }

    #[test]
    fn extraction_structural_refusals_are_unsupported_vetting_refusals_fail() {
        assert!(matches!(
            dmg_error("x", fetch::DmgError::Attach("unsupported image".into())),
            InstallError::Unsupported(_)
        ));
        for name in ["dmg attach failed", "unsupported archive", "io: zip: "] {
            assert!(matches!(
                dmg_error(
                    "x",
                    fetch::DmgError::Payload(format!(
                        "abrew: dmg contains a nonregular entry: {name}"
                    ))
                ),
                InstallError::Failed(_)
            ));
        }
        assert!(matches!(
            dmg_error("x", fetch::DmgError::Detach),
            InstallError::Failed(_)
        ));
        for message in [
            "unsupported image",
            "dmg attach failed",
            "cleanup not mounted",
        ] {
            assert!(matches!(
                dmg_error("x", fetch::DmgError::Uncertain(message.into())),
                InstallError::Failed(_)
            ));
        }
        assert!(matches!(
            extraction_error(
                "empoche",
                "abrew: cannot extract /c/e.zip: io: zip: no end-of-central-directory record"
                    .to_string()
            ),
            InstallError::Unsupported(_)
        ));
        assert!(matches!(
            extraction_error(
                "x",
                "abrew: unsupported archive; use brew for this format".to_string()
            ),
            InstallError::Unsupported(_)
        ));
        assert!(matches!(
            extraction_error(
                "x",
                "abrew: cannot extract /c/e.zip: rejected entry \"../evil\": Slip".to_string()
            ),
            InstallError::Failed(_)
        ));
        assert!(matches!(
            extraction_error(
                "x",
                "abrew: No space left on device (os error 28)".to_string()
            ),
            InstallError::Failed(_)
        ));
    }

    #[test]
    fn caskroom_sourced_wrapper_rebases_onto_own_payload() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let c = cask("unused.zip".to_string());
        assert_eq!(
            wrapper_program(
                &prefix,
                &c,
                "$HOMEBREW_PREFIX/Caskroom/demo/1/staged/bin/tool"
            )
            .unwrap(),
            Some(PathBuf::from("staged/bin/tool"))
        );
        for exe in [
            "$HOMEBREW_PREFIX/Caskroom/other/1/bin/tool",
            "$HOMEBREW_PREFIX/Caskroom/demo/2/bin/tool",
            "$HOMEBREW_PREFIX/share/demo/bin/tool",
        ] {
            assert!(matches!(
                wrapper_program(&prefix, &c, exe),
                Err(InstallError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn caskroom_sourced_binary_rebases_onto_own_payload() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let mut c = cask("unused.zip".to_string());
        c.artifacts = vec![Artifact::Binary(
            "$HOMEBREW_PREFIX/Caskroom/demo/1/platform-tools/adb".to_string(),
            None,
        )];
        let paths = artifact_paths(&prefix, &c).unwrap();
        assert_eq!(
            paths,
            [(
                PathBuf::from("platform-tools/adb"),
                prefix.bin().join("adb"),
                false,
                true
            )]
        );
        c.artifacts = vec![
            Artifact::App(
                "$HOMEBREW_PREFIX/Caskroom/demo/1/Demo.app".to_string(),
                None,
            ),
            Artifact::Binary(
                "/Applications/Demo.app/Contents/MacOS/demo".to_string(),
                None,
            ),
            Artifact::Generic(
                "$HOMEBREW_PREFIX/Caskroom/demo/1/tree".to_string(),
                Some("$HOMEBREW_PREFIX/share/demo/tree".to_string()),
            ),
        ];
        assert_eq!(
            artifact_paths(&prefix, &c).unwrap(),
            [
                (
                    PathBuf::from("Demo.app"),
                    prefix.applications().join("Demo.app"),
                    true,
                    false
                ),
                (
                    PathBuf::from("Demo.app/Contents/MacOS/demo"),
                    prefix.bin().join("demo"),
                    false,
                    true
                ),
                (
                    PathBuf::from("tree"),
                    prefix.root.join("share/demo/tree"),
                    false,
                    false
                ),
            ]
        );
        for source in [
            "$HOMEBREW_PREFIX/Caskroom/other/1/platform-tools/adb",
            "$HOMEBREW_PREFIX/Caskroom/demo/2/platform-tools/adb",
            "$HOMEBREW_PREFIX/Caskroom/demo/1/../../other/1/adb",
            "$HOMEBREW_PREFIX/Caskroom/demo/1/",
            "$HOMEBREW_PREFIX/share/elsewhere/adb",
            "$HOME/adb",
        ] {
            for artifact in [
                Artifact::Binary(source.to_string(), None),
                Artifact::Generic(
                    source.to_string(),
                    Some("$HOMEBREW_PREFIX/share/demo/tree".to_string()),
                ),
            ] {
                c.artifacts = vec![artifact];
                assert!(
                    matches!(
                        artifact_paths(&prefix, &c),
                        Err(InstallError::Unsupported(_))
                    ),
                    "{:?}",
                    c.artifacts
                );
            }
        }
    }

    /// wezterm's shape: brew's `$APPDIR/WezTerm.app/...` binaries and
    /// command wrappers (blender's, firefox's) name the app the cask
    /// installs, which its zip keeps in a folder; and isabelle's: the app is
    /// a generic artifact installed into `$APPDIR/`.
    #[test]
    #[cfg(unix)]
    fn appdir_sources_resolve_to_the_app_the_cask_installs() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let machine = Machine::mac("arm64", "27.0", Some("27.0"), true);
        let binary = aterm_json::json!({"binary": ["$APPDIR/Demo.app/Contents/MacOS/demo"]});
        let wrapper = |executable: &str| {
            aterm_json::json!({"command_wrapper": [
                "wrapdemo", {"executable": executable, "args": ["--fixed"]}
            ]})
        };
        let installs = |shape: &str, root: &str, installer: aterm_json::Value| {
            let plist = format!("{root}Demo.app/Contents/Info.plist");
            let script = format!("{root}Demo.app/Contents/MacOS/demo");
            let members = [
                ZipMember {
                    name: &plist,
                    mode: 0o100644,
                    data: b"app",
                    deflate: false,
                },
                ZipMember {
                    name: &script,
                    mode: 0o100755,
                    data: b"#!/bin/sh\nprintf 'demo ready [%s]\\n' \"$*\"\n",
                    deflate: false,
                },
            ];
            let archive = tmp.path().join(format!("{shape}.zip"));
            std::fs::write(&archive, zip_bytes(&members, false)).unwrap();
            let prefix = Prefix::at(&tmp.path().join(shape));
            let cask = Cask::from_api(&aterm_json::json!({
                "token": "demo", "version": "1", "sha256": "no_check",
                "url": format!("file://{}", archive.display()),
                "artifacts": [installer, binary, wrapper("$APPDIR/Demo.app/Contents/MacOS/demo")],
            }))
            .unwrap();
            preflight(&prefix, &cask, &machine).unwrap();
            assert_eq!(
                install(&prefix, &cask, false, false, false, false).unwrap(),
                "1"
            );
            for (command, said) in [
                ("demo", "demo ready []\n"),
                ("wrapdemo", "demo ready [--fixed]\n"),
            ] {
                let output = aterm_uds::spawnfd::command(prefix.bin().join(command))
                    .output()
                    .unwrap();
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout),
                    said,
                    "{shape} {command}"
                );
            }
        };
        installs(
            "wezterm",
            "Top/",
            aterm_json::json!({"app": ["Top/Demo.app"]}),
        );
        for (shape, root) in [("isabelle", ""), ("isabelle-nested", "Top/")] {
            installs(
                shape,
                root,
                aterm_json::json!({"artifact": [format!("{root}Demo.app"), {"target": "$APPDIR/Demo.app"}]}),
            );
        }
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let renamed = Cask::from_api(&aterm_json::json!({
            "token": "demo", "version": "1", "sha256": "no_check", "url": "unused.zip",
            "artifacts": [
                {"app": ["Studio.app", {"target": "Studio Preview.app"}]},
                {"binary": ["$APPDIR/studio preview.app/Contents/MacOS/studio"]},
            ],
        }))
        .unwrap();
        assert_eq!(
            artifact_paths(&prefix, &renamed).unwrap()[1].0,
            PathBuf::from("Studio.app/Contents/MacOS/studio")
        );
        let app = aterm_json::json!({"app": ["Top/Demo.app"]});
        let tree =
            |target: &str| aterm_json::json!({"artifact": ["Top/Demo.app", {"target": target}]});
        for (installer, source) in [
            (app, "$APPDIR/Other.app/Contents/MacOS/demo"),
            (
                tree("$APPDIR/Demo.app"),
                "$APPDIR/Other.app/Contents/MacOS/demo",
            ),
            (
                tree("$APPDIR/Demo.app"),
                "$APPDIR/Demo.apple/Contents/MacOS/demo",
            ),
            (
                tree("$HOMEBREW_PREFIX/Demo.app"),
                "$APPDIR/Demo.app/Contents/MacOS/demo",
            ),
        ] {
            for artifact in [aterm_json::json!({"binary": [source]}), wrapper(source)] {
                let elsewhere = Cask::from_api(&aterm_json::json!({
                    "token": "demo", "version": "1", "sha256": "no_check", "url": "unused.zip",
                    "artifacts": [installer.clone(), artifact.clone()],
                }))
                .unwrap();
                assert!(
                    matches!(
                        preflight(&prefix, &elsewhere, &machine),
                        Err(InstallError::Unsupported(why)) if why.contains("outside its payload")
                    ),
                    "{installer} {artifact}"
                );
            }
        }
    }

    #[test]
    fn applications_sourced_binary_rebases_onto_the_app_payload() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let mut c = cask("unused.zip".to_string());
        c.artifacts = vec![
            Artifact::App("Demo.app".to_string(), None),
            Artifact::Binary(
                "/Applications/Demo.app/Contents/MacOS/tool".to_string(),
                None,
            ),
        ];
        let paths = artifact_paths(&prefix, &c).unwrap();
        assert!(paths.iter().any(|(src, dst, _, exec)| {
            src == &PathBuf::from("Demo.app/Contents/MacOS/tool")
                && dst == &prefix.bin().join("tool")
                && *exec
        }));
        c.artifacts = vec![
            Artifact::App(
                "build/Demo.app".to_string(),
                Some("Renamed.app".to_string()),
            ),
            Artifact::Binary(
                "/Applications/Renamed.app/Contents/MacOS/tool".to_string(),
                None,
            ),
        ];
        let paths = artifact_paths(&prefix, &c).unwrap();
        assert!(
            paths
                .iter()
                .any(|(src, _, _, _)| src == &PathBuf::from("build/Demo.app/Contents/MacOS/tool"))
        );
        c.artifacts = vec![Artifact::Binary(
            "/Applications/Other.app/Contents/MacOS/tool".to_string(),
            None,
        )];
        assert!(matches!(
            artifact_paths(&prefix, &c),
            Err(InstallError::Unsupported(_))
        ));
    }

    #[test]
    fn unsupported_casks_refuse_before_native_changes() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        for artifact in [
            Artifact::Pkg("x.pkg".to_string()),
            Artifact::Other("uninstall:x".to_string()),
        ] {
            let mut cask = cask("unused.zip".to_string());
            cask.artifacts.push(artifact);
            assert!(matches!(
                install(&prefix, &cask, false, false, false, false),
                Err(InstallError::Unsupported(_))
            ));
            assert!(!prefix.root.exists());
        }
        let mut named = cask("unused.zip".to_string());
        named.artifacts.push(Artifact::Other(
            "generate_completions_from_executable".to_string(),
        ));
        let Err(InstallError::Unsupported(msg)) =
            install(&prefix, &named, false, false, false, false)
        else {
            panic!("expected native refusal");
        };
        assert!(
            msg.contains("generate_completions_from_executable"),
            "{msg}"
        );
        assert!(!prefix.root.exists());
        let cask = cask("unused.xar".to_string());
        assert!(matches!(
            install(&prefix, &cask, false, false, false, false),
            Err(InstallError::Unsupported(_))
        ));
        assert!(!prefix.root.exists());
        let with_deps = aterm_json::from_str(
            r#"{"token":"demo","version":"1","url":"unused.zip","sha256":"no_check","depends_on":{"formula":["other"],"cask":["helper"],"macos":{}},"artifacts":[{"app":["Demo.app"]}]}"#,
        )
        .unwrap();
        let with_deps = Cask::from_api(&with_deps).unwrap();
        assert_eq!(with_deps.unsupported_constraints, None);
        assert_eq!(with_deps.depends_formula, ["other"]);
        assert_eq!(with_deps.depends_cask, ["helper"]);
        for constraint in [
            r#""container":{"type":"unknown"}"#,
            r#""depends_on":{"formula":[42]}"#,
            r#""conflicts_with":{"cask":[42]}"#,
        ] {
            let constrained = aterm_json::from_str(&format!(r#"{{"token":"demo","version":"1","url":"unused.zip","sha256":"no_check",{constraint},"artifacts":[{{"app":["Demo.app"]}}],"variations":{{"arm64_tahoe":{{"artifacts":[{{"app":["Demo.app"]}}]}}}}}}"#)).unwrap();
            let constrained = Cask::from_api(&constrained).unwrap();
            assert!(constrained.unsupported_constraints.is_some());
            assert!(matches!(
                install(&prefix, &constrained, false, false, false, false),
                Err(InstallError::Unsupported(_))
            ));
            assert!(!prefix.root.exists());
        }
        prefix.ensure().unwrap();
        std::fs::remove_dir(prefix.cellar()).unwrap();
        std::fs::write(prefix.cellar(), b"unknown census").unwrap();
        let mut conflicting = self::cask("unused.zip".to_string());
        conflicting.conflicts = vec!["other".to_string()];
        assert!(preflight(&prefix, &conflicting, Machine::this()).is_err());
        assert_eq!(std::fs::read(prefix.cellar()).unwrap(), b"unknown census");
    }

    #[test]
    #[cfg(unix)]
    fn a_missing_artifact_leaves_no_receipt_or_links() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let archive = tmp.path().join("demo.zip");
        std::fs::write(
            &archive,
            zip_bytes(
                &[ZipMember {
                    name: "other",
                    mode: 0o100644,
                    data: b"not the promised tool",
                    deflate: false,
                }],
                false,
            ),
        )
        .unwrap();
        let cask = cask(format!("file://{}", archive.display()));
        assert!(install(&prefix, &cask, false, false, false, false).is_err());
        assert!(prefix.installed(Kind::Cask).is_empty());
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
        std::fs::write(
            &archive,
            zip_bytes(
                &[
                    ZipMember {
                        name: "tool",
                        mode: 0o100644,
                        data: b"not executable",
                        deflate: false,
                    },
                    ZipMember {
                        name: "Demo.app/Contents/Info.plist",
                        mode: 0o100644,
                        data: b"app",
                        deflate: false,
                    },
                ],
                false,
            ),
        )
        .unwrap();
        std::fs::remove_file(prefix.cache().join(download_name(&cask))).unwrap();
        assert!(install(&prefix, &cask, false, false, false, false).is_err());
        assert!(prefix.installed(Kind::Cask).is_empty());
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn uninstall_rechecks_planned_links_parents_and_payloads() {
        use super::super::prefix::{symlink_dir, symlink_file};
        use std::os::unix::ffi::OsStringExt;

        for change in [
            "foreign-file",
            "same-target-link",
            "physical-parent",
            "aliased-parent",
            "payload",
            "receipt",
        ] {
            let tmp = aterm_tempfile::TempDir::new().unwrap();
            let prefix = Prefix::at(&tmp.path().join("prefix"));
            prefix.ensure().unwrap();
            let payload = prefix.cask_dir("demo", "1");
            std::fs::create_dir_all(&payload).unwrap();
            std::fs::write(payload.join("tool"), b"owned payload").unwrap();
            prefix
                .write_receipt(
                    &payload,
                    &[
                        ("token", "demo"),
                        ("version", "1"),
                        ("installed_by", "abrew"),
                    ],
                )
                .unwrap();
            let earlier = prefix.applications().join("also-owned");
            symlink_file(&payload.join("tool"), &earlier).unwrap();
            let applications = tmp.path().join("captured-applications");
            std::fs::create_dir(&applications).unwrap();
            let artifact = applications.join("tool");
            symlink_file(&payload.join("tool"), &artifact).unwrap();
            let plan = UninstallPlan::prepare(
                &prefix,
                "demo",
                &["1".to_string()],
                &[prefix.applications(), applications.clone()],
            )
            .unwrap();

            match change {
                "foreign-file" | "same-target-link" => {
                    std::fs::rename(&artifact, tmp.path().join("held-link")).unwrap();
                    if change == "foreign-file" {
                        std::fs::write(&artifact, b"foreign artifact").unwrap();
                    } else {
                        symlink_file(&payload.join("tool"), &artifact).unwrap();
                    }
                }
                "physical-parent" | "aliased-parent" => {
                    std::fs::rename(&applications, tmp.path().join("held-parent")).unwrap();
                    if change == "physical-parent" {
                        std::fs::create_dir(&applications).unwrap();
                        std::fs::write(&artifact, b"foreign artifact").unwrap();
                    } else {
                        let outside = tmp.path().join("foreign-parent");
                        std::fs::create_dir(&outside).unwrap();
                        std::fs::write(outside.join("tool"), b"foreign artifact").unwrap();
                        symlink_dir(&outside, &applications).unwrap();
                    }
                }
                "payload" => {
                    let held = tmp.path().join("held-payload");
                    std::fs::rename(&payload, &held).unwrap();
                    std::fs::create_dir(&payload).unwrap();
                    std::fs::write(payload.join("tool"), b"foreign payload").unwrap();
                    std::fs::copy(
                        held.join("INSTALL_RECEIPT.json"),
                        payload.join("INSTALL_RECEIPT.json"),
                    )
                    .unwrap();
                }
                "receipt" => {
                    std::fs::write(payload.join("INSTALL_RECEIPT.json"), b"foreign receipt")
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(plan.apply(&prefix).is_err(), "{change}");
            assert!(payload.is_dir(), "{change}");
            assert_eq!(
                std::fs::read_link(&earlier).unwrap(),
                payload.join("tool"),
                "{change}"
            );
            if matches!(
                change,
                "foreign-file" | "physical-parent" | "aliased-parent"
            ) {
                assert_eq!(
                    std::fs::read(&artifact).unwrap(),
                    b"foreign artifact",
                    "{change}"
                );
            } else {
                assert_eq!(
                    std::fs::read_link(&artifact).unwrap(),
                    payload.join("tool"),
                    "{change}"
                );
            }
            if change == "payload" {
                assert_eq!(
                    std::fs::read(payload.join("tool")).unwrap(),
                    b"foreign payload"
                );
            }
            if change == "receipt" {
                assert_eq!(
                    std::fs::read(payload.join("INSTALL_RECEIPT.json")).unwrap(),
                    b"foreign receipt"
                );
            }
        }
        let captured = Prefix::at(&PathBuf::from(std::ffi::OsString::from_vec(
            b"/captured-\xff".to_vec(),
        )));
        let link = InstallLink {
            source: captured.cask_dir("demo", "1").join("tool"),
            destination: captured.bin().join("tool"),
            directory: false,
        };
        assert!(artifact_receipt_fields(&[link]).is_empty());
        assert!(
            recorded_artifact_paths(&captured, &aterm_json::from_str("{}").unwrap())
                .unwrap()
                .is_none()
        );
        let spellings: &[&str] = if cfg!(target_os = "linux") {
            &["plain", "non-utf8", "old-lossy"]
        } else {
            &["plain"]
        };
        for &spelling in spellings {
            let non_utf8 = spelling != "plain";
            let tmp = aterm_tempfile::TempDir::new().unwrap();
            let name = if non_utf8 {
                std::ffi::OsString::from_vec(b"prefix-\xff".to_vec())
            } else {
                std::ffi::OsString::from("prefix")
            };
            let prefix = Prefix::at(&tmp.path().join(name));
            prefix.ensure().unwrap();
            let mut owned = Vec::new();
            for version in ["1", "2"] {
                let payload = prefix.cask_dir("demo", version);
                std::fs::create_dir_all(&payload).unwrap();
                std::fs::write(payload.join("tool"), version.as_bytes()).unwrap();
                let destination = prefix.bin().join(format!("tool-{version}"));
                let link = InstallLink {
                    source: payload.join("tool"),
                    destination: destination.clone(),
                    directory: false,
                };
                let fields = if version == "2" {
                    artifact_receipt_fields(std::slice::from_ref(&link))
                } else {
                    Vec::new()
                };
                assert_eq!(fields.is_empty(), version == "1" || non_utf8);
                let mut receipt = vec![
                    ("token", "demo"),
                    ("version", version),
                    ("installed_by", "abrew"),
                ];
                receipt.extend(
                    fields
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                );
                prefix.write_receipt(&payload, &receipt).unwrap();
                if version == "2" && spelling == "old-lossy" {
                    let old = destination.to_string_lossy();
                    prefix
                        .write_receipt(
                            &payload,
                            &[
                                ("token", "demo"),
                                ("version", version),
                                ("installed_by", "abrew"),
                                ("artifact_0", old.as_ref()),
                            ],
                        )
                        .unwrap();
                }
                symlink_file(&link.source, &link.destination).unwrap();
                owned.push(destination);
            }
            let extra = prefix.bin().join("not-recorded");
            let target = prefix.cask_dir("demo", "2").join("tool");
            symlink_file(&target, &extra).unwrap();
            let plan = UninstallPlan::prepare(
                &prefix,
                "demo",
                &["1".to_string(), "2".to_string()],
                &[prefix.bin()],
            )
            .unwrap();
            assert!(owned.iter().all(|path| plan.links.contains_key(path)));
            assert_eq!(plan.links.contains_key(&extra), non_utf8);
            plan.apply(&prefix).unwrap();
            assert!(
                owned
                    .iter()
                    .all(|path| std::fs::symlink_metadata(path).is_err())
            );
            assert!(!prefix.cask_dir("demo", "1").exists());
            assert!(!prefix.cask_dir("demo", "2").exists());
            if non_utf8 {
                assert!(std::fs::symlink_metadata(&extra).is_err());
            } else {
                assert_eq!(std::fs::read_link(&extra).unwrap(), target);
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn uninstall_refuses_aliased_artifact_directories_before_removing_anything() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let payload = prefix.cask_dir("demo", "1");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("tool"), b"owned payload").unwrap();
        prefix
            .write_receipt(
                &payload,
                &[
                    ("token", "demo"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        let unknown = prefix.cask_dir("demo", "personal");
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("notes"), b"owner notes").unwrap();
        let artifact = prefix.applications().join("tool");
        super::super::prefix::symlink_file(&payload.join("tool"), &artifact).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let external_link = outside.join("keep");
        super::super::prefix::symlink_file(&payload.join("tool"), &external_link).unwrap();
        std::fs::write(outside.join("marker"), b"external bytes").unwrap();
        std::fs::remove_dir(prefix.bin()).unwrap();
        super::super::prefix::symlink_dir(&outside, &prefix.bin()).unwrap();
        let local_applications = tmp.path().join("captured-applications");
        std::fs::create_dir(&local_applications).unwrap();
        let man = prefix.root.join("share/man");
        std::fs::write(&man, b"unknown man root").unwrap();
        assert!(artifact_directories(&prefix, local_applications.clone()).is_err());
        assert!(payload.join("INSTALL_RECEIPT.json").is_file());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
        std::fs::remove_file(&man).unwrap();
        let directories = [prefix.applications(), prefix.bin(), local_applications];
        assert!(uninstall_from(&prefix, "demo", false, &directories).is_err());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
        assert_eq!(
            std::fs::read_link(&external_link).unwrap(),
            payload.join("tool")
        );
        assert_eq!(
            std::fs::read(outside.join("marker")).unwrap(),
            b"external bytes"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
        assert!(payload.join("INSTALL_RECEIPT.json").is_file());
        std::fs::remove_file(prefix.bin()).unwrap();
        std::fs::write(prefix.bin(), b"foreign root file").unwrap();
        assert!(uninstall_from(&prefix, "demo", false, &directories).is_err());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
        assert_eq!(std::fs::read(prefix.bin()).unwrap(), b"foreign root file");
        std::fs::remove_file(prefix.bin()).unwrap();
        std::fs::create_dir(prefix.bin()).unwrap();
        let foreign = prefix.bin().join("foreign");
        super::super::prefix::symlink_file(&outside.join("marker"), &foreign).unwrap();
        uninstall_from(&prefix, "demo", false, &directories).unwrap();
        assert!(std::fs::symlink_metadata(&artifact).is_err());
        assert!(!payload.exists());
        assert_eq!(
            std::fs::read(unknown.join("notes")).unwrap(),
            b"owner notes"
        );
        assert_eq!(std::fs::read(&foreign).unwrap(), b"external bytes");
        assert_eq!(
            std::fs::read_link(&external_link).unwrap(),
            payload.join("tool")
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
        let generic_world = aterm_tempfile::TempDir::new().unwrap();
        let generic_prefix = Prefix::at(&generic_world.path().join("prefix"));
        generic_prefix.ensure().unwrap();
        let generic_payload = generic_prefix.cask_dir("demo", "1");
        std::fs::create_dir_all(&generic_payload).unwrap();
        std::fs::write(generic_payload.join("tree"), b"payload").unwrap();
        let destinations = [
            generic_prefix.root.join("var/demo"),
            generic_prefix.root.join("share/a/b/c/d/demo"),
        ];
        let mut links = Vec::new();
        for destination in &destinations {
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            super::super::prefix::symlink_file(&generic_payload.join("tree"), destination).unwrap();
            links.push(InstallLink {
                source: generic_payload.join("tree"),
                destination: destination.clone(),
                directory: false,
            });
        }
        let fields = complete_artifact_receipt_fields(&generic_prefix, &links).unwrap();
        let mut receipt = vec![
            ("token", "demo"),
            ("version", "1"),
            ("installed_by", "abrew"),
        ];
        receipt.extend(
            fields
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        for invalid in ["../outside", "/absolute", "var/../demo"] {
            generic_prefix
                .write_receipt(
                    &generic_payload,
                    &[
                        ("token", "demo"),
                        ("version", "1"),
                        ("installed_by", "abrew"),
                        ("relative_artifact_0", invalid),
                    ],
                )
                .unwrap();
            let saved = std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap();
            assert!(
                UninstallPlan::prepare(
                    &generic_prefix,
                    "demo",
                    &["1".to_string()],
                    &[generic_prefix.bin()]
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap(),
                saved
            );
            assert!(
                destinations
                    .iter()
                    .all(|path| std::fs::read_link(path).unwrap() == generic_payload.join("tree"))
            );
            assert_eq!(
                std::fs::read(generic_payload.join("tree")).unwrap(),
                b"payload"
            );
        }
        generic_prefix
            .write_receipt(&generic_payload, &receipt)
            .unwrap();
        let saved = std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap();
        let oversized = "x".repeat(64 * 1024);
        assert!(
            generic_prefix
                .write_receipt(&generic_payload, &[("oversized", &oversized)])
                .is_err()
        );
        assert_eq!(
            std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap(),
            saved
        );
        let plan = UninstallPlan::prepare(
            &generic_prefix,
            "demo",
            &["1".to_string()],
            &[generic_prefix.bin()],
        )
        .unwrap();
        assert!(
            destinations
                .iter()
                .all(|path| plan.links.contains_key(path))
        );
        plan.apply(&generic_prefix).unwrap();
        assert!(
            destinations
                .iter()
                .all(|path| std::fs::symlink_metadata(path).is_err())
        );
        assert!(!generic_payload.exists());
        let legacy_world = aterm_tempfile::TempDir::new().unwrap();
        let legacy_prefix = Prefix::at(&legacy_world.path().join("prefix"));
        legacy_prefix.ensure().unwrap();
        let legacy_payload = legacy_prefix.cask_dir("demo", "1");
        std::fs::create_dir_all(&legacy_payload).unwrap();
        std::fs::write(legacy_payload.join("tool"), b"legacy payload").unwrap();
        legacy_prefix
            .write_receipt(
                &legacy_payload,
                &[
                    ("token", "demo"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        let legacy_parent = legacy_prefix.root.join("share/x");
        std::fs::create_dir(&legacy_parent).unwrap();
        let legacy_link = legacy_parent.join("tool");
        super::super::prefix::symlink_file(&legacy_payload.join("tool"), &legacy_link).unwrap();
        let legacy_library_link = legacy_prefix.root.join("Library/QuickLook/tool");
        super::super::prefix::symlink_file(&legacy_payload.join("tool"), &legacy_library_link)
            .unwrap();
        let legacy_applications = legacy_world.path().join("captured-applications");
        std::fs::create_dir(&legacy_applications).unwrap();
        let directories = artifact_directories(&legacy_prefix, legacy_applications).unwrap();
        let plan = UninstallPlan::prepare(&legacy_prefix, "demo", &["1".to_string()], &directories)
            .unwrap();
        assert!(plan.links.contains_key(&legacy_link));
        assert!(plan.links.contains_key(&legacy_library_link));
        plan.apply(&legacy_prefix).unwrap();
        assert!(std::fs::symlink_metadata(&legacy_link).is_err());
        assert!(std::fs::symlink_metadata(&legacy_library_link).is_err());
        assert!(!legacy_payload.exists());
        for relative in ["share/man/man1", "share/man"] {
            let world = aterm_tempfile::TempDir::new().unwrap();
            let prefix = Prefix::at(&world.path().join("prefix"));
            prefix.ensure().unwrap();
            let payload = prefix.cask_dir("demo", "1");
            let tree = payload.join("tree");
            std::fs::create_dir_all(&tree).unwrap();
            std::fs::write(tree.join("page"), b"owned page").unwrap();
            let destination = prefix.root.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            super::super::prefix::symlink_dir(&tree, &destination).unwrap();
            let fields = complete_artifact_receipt_fields(
                &prefix,
                &[InstallLink {
                    source: tree.clone(),
                    destination: destination.clone(),
                    directory: true,
                }],
            )
            .unwrap();
            let mut receipt = vec![
                ("token", "demo"),
                ("version", "1"),
                ("installed_by", "abrew"),
            ];
            receipt.extend(
                fields
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            prefix.write_receipt(&payload, &receipt).unwrap();
            let applications = world.path().join("captured-applications");
            std::fs::create_dir(&applications).unwrap();
            if relative == "share/man" {
                let held = world.path().join("held-owned-link");
                std::fs::rename(&destination, &held).unwrap();
                let outside = world.path().join("outside");
                std::fs::create_dir(&outside).unwrap();
                std::fs::write(outside.join("keep"), b"preserved").unwrap();
                super::super::prefix::symlink_dir(&outside, &destination).unwrap();
                let directories = artifact_directories(&prefix, applications.clone()).unwrap();
                assert!(
                    UninstallPlan::prepare(&prefix, "demo", &["1".to_string()], &directories)
                        .is_err()
                );
                assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"preserved");
                assert_eq!(std::fs::read(tree.join("page")).unwrap(), b"owned page");
                std::fs::remove_file(&destination).unwrap();
                std::fs::rename(held, &destination).unwrap();
            }
            let directories = artifact_directories(&prefix, applications).unwrap();
            let plan =
                UninstallPlan::prepare(&prefix, "demo", &["1".to_string()], &directories).unwrap();
            assert!(plan.links.contains_key(&destination), "{relative}");
            plan.apply(&prefix).unwrap();
            assert!(
                std::fs::symlink_metadata(&destination).is_err(),
                "{relative}"
            );
            assert!(!payload.exists(), "{relative}");
        }
    }
}
