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
        && !fetch::archive_supported(&download_basename(cask))
        && !fetch::is_dmg(&download_basename(cask)))
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
            let short = super::formula::Formula::short_name(dep).to_string();
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
/// Brew's cask tab `runtime_dependencies` hash: the cask's
/// recursive formula/cask dependency graph as installed.
/// Direct entries come from the cask's declared deps; each
/// direct dep's own recorded closure folds in transitively.
/// Only non-empty kinds appear (brew writes `{}` when bare).
fn cask_runtime_deps_json(prefix: &Prefix, cask: &Cask) -> String {
    let mut formulae: Vec<(String, bool)> = Vec::new();
    let mut casks: Vec<(String, bool)> = Vec::new();
    let push = |list: &mut Vec<(String, bool)>, full: &str, declared: bool| {
        if !list.iter().any(|(n, _)| n == full) {
            list.push((full.to_string(), declared));
        }
    };
    for dep in &cask.depends_formula {
        push(&mut formulae, dep, true);
        let short = super::formula::Formula::short_name(dep);
        if let Some(inst) = prefix.installed_one(Kind::Formula, short)
            && let Some(names) =
                super::prefix::receipt_runtime_deps(&prefix.keg(&inst.name, &inst.version))
        {
            for n in &names {
                push(&mut formulae, n, false);
            }
        }
    }
    for dep in &cask.depends_cask {
        push(&mut casks, dep, true);
        if let Some(inst) = prefix.installed_one(Kind::Cask, dep) {
            let (sub_formulae, sub_casks) = super::prefix::receipt_cask_runtime_deps(
                &prefix.cask_dir(&inst.name, &inst.version),
            );
            for n in &sub_formulae {
                push(&mut formulae, n, false);
            }
            for n in &sub_casks {
                push(&mut casks, n, false);
            }
        }
    }
    let entry = |full: &str, declared: bool, version: &str| {
        format!(
            "{{\"full_name\": \"{}\", \"version\": \"{}\", \"declared_directly\": {declared}}}",
            super::prefix::json_escape(full),
            super::prefix::json_escape(version),
        )
    };
    let mut kinds: Vec<String> = Vec::new();
    if !casks.is_empty() {
        let items: Vec<String> = casks
            .iter()
            .map(|(full, declared)| {
                let version = prefix
                    .installed_one(Kind::Cask, super::formula::Formula::short_name(full))
                    .map_or_else(String::new, |i| i.version);
                entry(full, *declared, &version)
            })
            .collect();
        kinds.push(format!("\"cask\": [{}]", items.join(", ")));
    }
    if !formulae.is_empty() {
        let items: Vec<String> = formulae
            .iter()
            .map(|(full, declared)| {
                let version = prefix
                    .installed_one(Kind::Formula, super::formula::Formula::short_name(full))
                    .map_or_else(String::new, |i| i.version);
                entry(full, *declared, &version)
            })
            .collect();
        kinds.push(format!("\"formula\": [{}]", items.join(", ")));
    }
    format!("{{{}}}", kinds.join(", "))
}

fn install_inner(
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    force: bool,
    // An outdated install is replaced whichever verb brought us
    // here; `upgrade` only says the command layer already printed
    // brew's Upgrading headline.
    upgrade: bool,
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
    let mut old_version: Option<String> = None;
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
            old_version = Some(inst.version.clone());
        } else if force || reinstall {
            return Err(InstallError::Unsupported(format!(
                "replacement of cask {} needs brew",
                cask.token
            )));
        } else {
            // Brew's dry run says nothing for an up-to-date cask.
            if !dry_run {
                super::tty::opoo!(
                    "Warning: Not upgrading {}, the latest version is already installed",
                    cask.token
                );
            }
            return Ok(inst.version);
        }
    }
    let file = download_basename(cask);
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
    let dest = download_dest(prefix, cask);
    // Brew's download-queue narration: one heading for the named
    // cask and a tick once its download is in hand (cached counts).
    // Dependency casks get the tick only; brew fetches them under a
    // separate dependency heading this inline walk does not print.
    if on_request {
        eprintln!(
            "{}",
            super::tty::oh1(&format!(
                "Fetching downloads for: {}",
                super::tty::ident(&cask.token)
            ))
        );
    }
    if git_plan.is_none() {
        let expected = cask.sha256.as_deref().unwrap_or("no_check");
        let reusable = cask.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
        if !reusable {
            fetch::curl_to_file(&cask.url, &dest, &[])?;
        }
        fetch::verify_sha256(&dest, expected)?;
        eprintln!("✔︎ Cask {} ({})", cask.token, cask.version);
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
        eprintln!("✔︎ Cask {} ({})", cask.token, cask.version);
    } else if naked {
        std::fs::create_dir_all(&payload).map_err(|e| format!("abrew: {e}"))?;
        let base = download_basename(cask);
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
            crate::metadata_io::set_regular_mode(&staged_file, 0o755)
                .map_err(|e| format!("abrew: {e}"))?;
            for (relative, _, _, exec) in &artifacts {
                if *exec {
                    let f = payload.join(relative);
                    if f.is_file() {
                        crate::metadata_io::set_regular_mode(&f, 0o755)
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
    if let Some(old) = &old_version {
        // Brew routes an outdated cask through its upgrade flow:
        // an Upgrading headline (printed by the upgrade command
        // itself, so only narrated here on the install path), the
        // old artifacts backed up and removed, then the new ones.
        if !upgrade {
            println!(
                "{}",
                super::tty::oh1(&format!("Upgrading {}", super::tty::ident(&cask.token)))
            );
            println!("  {old} -> {}", cask.version);
        }
        if let Ok(Some(destinations)) =
            recorded_artifacts(prefix, &prefix.cask_dir(&cask.token, old))
        {
            narrate_recorded_removal(prefix, &destinations);
        }
    } else {
        if same_version_reinstall {
            // Brew's reinstall uninstalls the existing cask first:
            // headline, artifact removal, purge, then the install.
            println!(
                "{}",
                super::tty::oh1(&format!(
                    "Uninstalling Cask {}",
                    super::tty::ident(&cask.token)
                ))
            );
            if let Ok(Some(destinations)) = recorded_artifacts(prefix, &staged) {
                narrate_recorded_removal(prefix, &destinations);
            }
            println!(
                "==> Purging files for version {} of Cask {}",
                cask.version, cask.token
            );
        }
        println!(
            "{}",
            super::tty::oh1(&format!(
                "Installing Cask {}",
                super::tty::ident(&cask.token)
            ))
        );
    }
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
    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .to_string();
    let mut receipt_fields: Vec<(&str, &str)> = vec![
        ("token", &cask.token),
        ("version", &cask.version),
        ("installed_by", "abrew"),
        ("installed_on_request", on_request_word),
        ("full_name", &cask.token),
        ("time", &installed_at),
    ];
    for (k, v) in &artifact_fields {
        receipt_fields.push((k, v));
    }
    let runtime_deps = cask_runtime_deps_json(prefix, cask);
    prefix.write_receipt_raw(
        &payload,
        &receipt_fields,
        &[("runtime_dependencies", &runtime_deps)],
    )?;
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
    if let Some(old) = &old_version {
        // Brew's upgrade finalizes by purging the predecessor's
        // staged files; the new links already point at the new
        // staged path, so the old directory can go.
        println!("==> Purging files for version {old} of Cask {}", cask.token);
        let old_dir = prefix.cask_dir(&cask.token, old);
        match std::fs::remove_dir_all(&old_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("abrew: {e}").into()),
        }
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
/// Brew's per-artifact removal narration for recorded destinations:
/// moved artifacts are backed up to their staged path, then every
/// artifact is removed or unlinked.
fn narrate_recorded_removal(prefix: &Prefix, destinations: &BTreeSet<PathBuf>) {
    for dst in destinations {
        if std::fs::symlink_metadata(dst).is_err() {
            continue;
        }
        let (label, linked) = destination_label(prefix, dst);
        if !linked && let Ok(target) = std::fs::read_link(dst) {
            let joined = if target.is_absolute() {
                target.clone()
            } else {
                dst.parent()
                    .map_or_else(|| target.clone(), |p| p.join(&target))
            };
            let source = std::fs::canonicalize(&joined).unwrap_or(joined);
            let base = dst
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!("==> Backing up {label} '{base}' to '{}'", source.display());
        }
        println!(
            "==> {} {label} '{}'",
            if linked { "Unlinking" } else { "Removing" },
            dst.display()
        );
    }
}

fn recorded_artifacts(
    prefix: &Prefix,
    payload: &Path,
) -> Result<Option<BTreeSet<PathBuf>>, InstallError> {
    let bytes = crate::metadata_io::read_bounded_regular(
        &payload.join("INSTALL_RECEIPT.json"),
        1024 * 1024,
    )
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
    if !super::single_normal_segment(branch)
        || only_path.is_empty()
        || !only_path.split('/').all(super::single_normal_segment)
    {
        return None;
    }
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
        if !super::single_normal_segment(source) {
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
        let head = crate::metadata_io::read_head(&dest, 64).map_err(|e| format!("abrew: {e}"))?;
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
    let file = download_basename(cask);
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
        if !super::single_normal_segment(s) {
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
    let Ok(mut f) = crate::metadata_io::open_regular(path) else {
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

/// The cask's download URL with `#{version}` interpolated
/// (the string brew hashes for its cache name).
fn substituted_url(cask: &Cask) -> String {
    cask.url.replace("#{version}", &cask.version)
}

/// Percent-decode (`URI.unescape`): `%XX` bytes, lossy UTF-8.
fn uri_unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Brew `AbstractFileDownloadStrategy#parse_basename` +
/// `Utils.safe_filename`: a `response-content-disposition`
/// filename wins, else the last path/query component with
/// an extension, else the last path component; control
/// bytes and separators are stripped. An empty result
/// falls back to "download" so staging always has a name.
fn parse_download_basename(url: &str) -> String {
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    let mut query_values: Vec<String> = Vec::new();
    if let Some(query) = query {
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = uri_unescape(&value.replace('+', " "));
            if key == "response-content-disposition"
                && let Some(name) = value
                    .split("filename=")
                    .nth(1)
                    .map(|n| n.trim().trim_matches(['"', '\'']))
                && !name.is_empty()
            {
                let base = name.rsplit('/').next().unwrap_or(name);
                return safe_filename(base);
            }
            query_values.push(value);
        }
    }
    // Only the URI path's segments count, never the host.
    let path = head.split('#').next().unwrap_or(head);
    let path = match path.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(at) => &rest[at..],
            None => "",
        },
        None => path,
    };
    let segments: Vec<String> = path
        .split('/')
        .map(uri_unescape)
        .filter(|s| !s.is_empty())
        .collect();
    let has_ext = |s: &str| match s.rfind('.') {
        Some(at) => at > 0 && at + 1 < s.len(),
        None => false,
    };
    for candidate in segments.iter().chain(&query_values).rev() {
        let base = candidate.rsplit('/').next().unwrap_or(candidate);
        if has_ext(base) {
            return safe_filename(base);
        }
    }
    safe_filename(segments.last().map_or("", String::as_str))
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/')
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "download".to_string()
    } else {
        cleaned
    }
}

/// The download's display basename (brew `download_name`).
pub(super) fn download_basename(cask: &Cask) -> String {
    parse_download_basename(&substituted_url(cask))
}

/// The cask payload's cached-download path (brew
/// `downloads/<sha256(url)>--<basename>`).
pub(super) fn download_dest(prefix: &Prefix, cask: &Cask) -> PathBuf {
    super::install::download_dest(prefix, &substituted_url(cask), &download_basename(cask))
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
        let mut writer = crate::platform::open_create_new_write(&directory.join(file), 0o755)
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

fn link_file(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks are not supported",
        ))
    }
}

/// One symlink-installed cask artifact (brew's `Symlinked`
/// kinds: binaries, command wrappers, manpages, completions).
pub(super) struct CaskLink {
    pub(super) source: PathBuf,
    pub(super) target: PathBuf,
    english: &'static str,
}

/// The cask's linkable artifacts staged at `version`.
pub(super) fn linkable_artifacts(
    prefix: &Prefix,
    cask: &Cask,
    version: &str,
) -> Result<Vec<CaskLink>, InstallError> {
    let paths = artifact_paths(prefix, cask)?;
    let staged = prefix.cask_dir(&cask.token, version);
    let mut out = Vec::new();
    for (artifact, (relative, target, _, _)) in cask.artifacts.iter().zip(paths) {
        let english = match artifact {
            Artifact::Binary(..) => "Binary",
            Artifact::Wrapper(..) => "Command Wrapper",
            Artifact::Manpage(..) => "Manpage",
            Artifact::Library(_, _, dir) if dir == "share/bash-completion/completions" => {
                "Bash Completion"
            }
            Artifact::Library(_, _, dir) if dir == "share/zsh/site-functions" => "Zsh Completion",
            Artifact::Library(_, _, dir) if dir == "share/fish/vendor_completions.d" => {
                "Fish Completion"
            }
            _ => continue,
        };
        out.push(CaskLink {
            source: staged.join(&relative),
            target,
            english,
        });
    }
    Ok(out)
}

fn target_links_to_source(target: &Path, source: &Path) -> bool {
    if !target
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return false;
    }
    if std::fs::read_link(target).is_ok_and(|read| read == source) {
        return true;
    }
    std::fs::canonicalize(target).ok() == std::fs::canonicalize(source).ok()
}

/// The target is a symlink into the same-named formula's keg.
fn conflicting_formula(prefix: &Prefix, token: &str, target: &Path) -> bool {
    if !target
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return false;
    }
    let resolved = target
        .parent()
        .zip(std::fs::read_link(target).ok())
        .map(|(dir, read)| dir.join(read));
    resolved.is_some_and(|path| path.starts_with(prefix.cellar().join(token)))
}

enum LinkAction {
    Link,
    Overwrite,
    AlreadyLinked,
    SkipFormula,
    Conflict,
}

fn link_action(
    prefix: &Prefix,
    token: &str,
    item: &CaskLink,
    force: bool,
    overwrite: bool,
) -> LinkAction {
    if !item.target.exists() {
        return LinkAction::Link;
    }
    let same_cask = target_links_to_source(&item.target, &item.source)
        || std::fs::canonicalize(&item.target)
            .is_ok_and(|p| p.starts_with(prefix.caskroom().join(token)));
    if overwrite || (force && item.target.is_symlink() && same_cask) {
        LinkAction::Overwrite
    } else if target_links_to_source(&item.target, &item.source) {
        LinkAction::AlreadyLinked
    } else if conflicting_formula(prefix, token, &item.target) {
        LinkAction::SkipFormula
    } else {
        LinkAction::Conflict
    }
}

fn english_article(english: &str) -> &'static str {
    if english
        .chars()
        .next()
        .is_some_and(|c| "aeiou".contains(c.to_ascii_lowercase()))
    {
        "an"
    } else {
        "a"
    }
}

/// `brew link --cask`: pre-check conflicts the way brew does,
/// then link each artifact with brew's lines.
pub(super) fn link_cask(
    prefix: &Prefix,
    token: &str,
    items: &[CaskLink],
    force: bool,
    overwrite: bool,
    dry_run: bool,
) -> Result<(), InstallError> {
    if let Some(item) = items.iter().find(|item| {
        matches!(
            link_action(prefix, token, item, force, overwrite),
            LinkAction::Conflict
        )
    }) {
        return Err(InstallError::Failed(format!(
            "Error: Could not link {token}: {} already exists.\nTo force the link and overwrite all conflicting files:\n  brew link --cask --overwrite {token}",
            item.target.display()
        )));
    }
    if dry_run {
        println!(
            "{}",
            if overwrite {
                "Would remove:"
            } else {
                "Would link:"
            }
        );
    }
    for item in items {
        let base = item
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let message = format!(
            "It seems there is already {} {} at '{}'",
            english_article(item.english),
            item.english,
            item.target.display()
        );
        match link_action(prefix, token, item, force, overwrite) {
            LinkAction::Overwrite => {
                if dry_run {
                    println!("{}", item.target.display());
                    continue;
                }
                super::tty::opoo!("Warning: {message}; overwriting.");
                std::fs::remove_file(&item.target)
                    .map_err(|e| format!("abrew: cannot remove {}: {e}", item.target.display()))?;
            }
            LinkAction::AlreadyLinked => {
                if !dry_run {
                    println!(
                        "{}",
                        super::tty::ohai(&format!(
                            "{} '{base}' is already linked to '{}'",
                            item.english,
                            item.target.display()
                        ))
                    );
                }
                continue;
            }
            LinkAction::SkipFormula => {
                super::tty::opoo!("Warning: {message} from formula {token}; skipping link.");
                continue;
            }
            LinkAction::Conflict => {
                return Err(InstallError::Failed(format!("Error: {message}.")));
            }
            LinkAction::Link => {}
        }
        if dry_run {
            if !overwrite || item.target.is_symlink() {
                println!("{}", item.target.display());
            }
            continue;
        }
        if !item.source.exists() {
            return Err(InstallError::Failed(format!(
                "Error: It seems the {} source '{}' is not there.",
                item.english.to_ascii_lowercase(),
                item.source.display()
            )));
        }
        println!(
            "{}",
            super::tty::ohai(&format!(
                "Linking {} '{base}' to '{}'",
                item.english,
                item.target.display()
            ))
        );
        if let Some(parent) = item.target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("abrew: cannot create {}: {e}", parent.display()))?;
        }
        // brew creates the link with `ln --force`: a dangling
        // symlink at the target is replaced.
        if item.target.is_symlink() {
            std::fs::remove_file(&item.target)
                .map_err(|e| format!("abrew: cannot remove {}: {e}", item.target.display()))?;
        }
        link_file(&item.source, &item.target)
            .map_err(|e| format!("abrew: cannot link {}: {e}", item.target.display()))?;
    }
    Ok(())
}

/// `brew unlink --cask`: remove prefix links that point at
/// this cask's staged artifacts.
pub(super) fn unlink_cask(
    prefix: &Prefix,
    token: &str,
    items: &[CaskLink],
    dry_run: bool,
) -> Result<(), InstallError> {
    if dry_run {
        println!("Would remove:");
    }
    for item in items {
        if !item
            .target
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
            || conflicting_formula(prefix, token, &item.target)
            || !target_links_to_source(&item.target, &item.source)
        {
            continue;
        }
        if dry_run {
            println!("{}", item.target.display());
            continue;
        }
        println!(
            "{}",
            super::tty::ohai(&format!(
                "Unlinking {} '{}'",
                item.english,
                item.target.display()
            ))
        );
        std::fs::remove_file(&item.target)
            .map_err(|e| format!("abrew: cannot remove {}: {e}", item.target.display()))?;
    }
    Ok(())
}

/// Remove links into receipt-admitted versions and only those payloads.
/// When the cask definition is known, its `uninstall` stanza's
/// file-only directives run in brew's order: `trash:` before the
/// artifacts come out, `rmdir:` deferred until after. Other
/// directives (quit, launchctl, pkgutil, script, sudo-bound
/// delete) are not run natively.
pub fn uninstall(
    prefix: &Prefix,
    token: &str,
    dry_run: bool,
    cask: Option<&super::formula::Cask>,
) -> Result<(), InstallError> {
    let directories = artifact_directories(prefix, PathBuf::from("/Applications"))?;
    uninstall_from(prefix, token, dry_run, &directories, cask, &BTreeSet::new())
}

/// The destinations `token`'s installed receipts record that
/// `new_cask` would also install, with brew's artifact display
/// (`Caffeine.app (App)`): the shared set a cask migration keeps.
pub(super) fn migration_shared_artifacts(
    prefix: &Prefix,
    token: &str,
    new_cask: &super::formula::Cask,
) -> Result<Vec<(PathBuf, String)>, InstallError> {
    let Some(inst) = prefix.installed_one(Kind::Cask, token) else {
        return Ok(Vec::new());
    };
    let mut recorded: BTreeSet<PathBuf> = BTreeSet::new();
    for version in &inst.versions {
        if let Some(paths) = recorded_artifacts(prefix, &prefix.cask_dir(token, version))? {
            recorded.extend(paths);
        }
    }
    let new_destinations: BTreeSet<PathBuf> = artifact_paths(prefix, new_cask)?
        .into_iter()
        .map(|(_, destination, _, _)| destination)
        .collect();
    Ok(recorded
        .intersection(&new_destinations)
        .map(|destination| {
            let (label, _) = destination_label(prefix, destination);
            let base = destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (destination.clone(), format!("{base} ({label})"))
        })
        .collect())
}

/// Migration's uninstall of the old cask: receipt-driven like an
/// ordinary uninstall, minus the stanza directives (the old
/// definition is renamed away) and minus shared destinations
/// the new cask installs too.
pub(super) fn uninstall_for_migration(
    prefix: &Prefix,
    token: &str,
    keep: &BTreeSet<PathBuf>,
) -> Result<(), InstallError> {
    let directories = artifact_directories(prefix, PathBuf::from("/Applications"))?;
    uninstall_from(prefix, token, false, &directories, None, keep)
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
            let kind = entry
                .file_type()
                .map_err(|error| format!("abrew: cannot inspect legacy artifact entry: {error}"))?;
            if !kind.is_dir() {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|error| format!("abrew: cannot inspect legacy artifact entry: {error}"))?;
            if !crate::platform::is_reparse(&metadata) {
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

/// Segments match with fnmatch rules (`*`, `?`, `[...]`), the
/// subset brew's zap path globs use. A leading dot only matches
/// when the pattern itself starts with one (no FNM_DOTMATCH).
fn segment_matches(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    fn inner(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        match p[0] {
            b'*' => (0..=s.len()).any(|i| inner(&p[1..], &s[i..])),
            b'?' => !s.is_empty() && inner(&p[1..], &s[1..]),
            b'[' => {
                if s.is_empty() {
                    return false;
                }
                let (negate, mut i) = if p.get(1) == Some(&b'!') || p.get(1) == Some(&b'^') {
                    (true, 2)
                } else {
                    (false, 1)
                };
                let mut hit = false;
                while i < p.len() && p[i] != b']' {
                    if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
                        if (p[i]..=p[i + 2]).contains(&s[0]) {
                            hit = true;
                        }
                        i += 3;
                    } else {
                        if p[i] == s[0] {
                            hit = true;
                        }
                        i += 1;
                    }
                }
                if i >= p.len() {
                    return false;
                }
                (hit != negate) && inner(&p[i + 1..], &s[1..])
            }
            c => s.first() == Some(&c) && inner(&p[1..], &s[1..]),
        }
    }
    inner(pattern.as_bytes(), name.as_bytes())
}

fn glob_walk(base: &Path, segments: &[&str], out: &mut Vec<PathBuf>) {
    let Some((first, rest)) = segments.split_first() else {
        if base.symlink_metadata().is_ok() {
            out.push(base.to_path_buf());
        }
        return;
    };
    if first.contains(['*', '?', '[']) {
        if let Ok(entries) = std::fs::read_dir(base) {
            let mut names: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| segment_matches(first, &e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect();
            names.sort();
            for path in names {
                glob_walk(&path, rest, out);
            }
        }
    } else {
        glob_walk(&base.join(first), rest, out);
    }
}

/// Brew's `each_resolved_path`: expand `~`, refuse relative or
/// dot-segment paths and undeletable ones with a warning, glob
/// the rest. Yields (stanza path, existing resolved paths).
/// The API placeholders `$APPDIR` and `$HOMEBREW_PREFIX` are
/// substituted first, as brew's API loader substitutes them.
fn resolve_stanza_paths(
    prefix: &Prefix,
    action: &str,
    paths: &[String],
) -> Vec<(String, Vec<PathBuf>)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for raw in paths {
        let display = expand_stanza_placeholders(prefix, raw);
        let shown = display.as_deref().unwrap_or(raw);
        let expanded: PathBuf = if shown == "~" {
            home.clone().unwrap_or_default()
        } else if let Some(rest) = shown.strip_prefix("~/") {
            home.clone().unwrap_or_default().join(rest)
        } else {
            PathBuf::from(shown)
        };
        if expanded.as_os_str().is_empty() || expanded.is_relative() {
            super::tty::opoo!("Warning: Skipping `{action}` for relative path '{shown}'.");
            continue;
        }
        let text = expanded.to_string_lossy();
        if text.split('/').any(|segment| matches!(segment, "." | "..")) {
            super::tty::opoo!(
                "Warning: Skipping `{action}` for path with relative segments '{shown}'."
            );
            continue;
        }
        let mut resolved = Vec::new();
        for variant in brace_expand(&text) {
            let segments: Vec<&str> = variant.split('/').filter(|s| !s.is_empty()).collect();
            glob_walk(Path::new("/"), &segments, &mut resolved);
        }
        resolved.sort();
        resolved.dedup();
        resolved.retain(|target| {
            let parent_ok = target
                .parent()
                .is_some_and(crate::platform::dir_writable_by_caller);
            if !parent_ok {
                super::tty::opoo!(
                    "Warning: Skipping `{action}` for undeletable path '{}'.",
                    target.display()
                );
            }
            parent_ok
        });
        out.push((shown.to_string(), resolved));
    }
    out
}

/// Ruby `Dir.glob` brace expansion: `{a,b}` alternatives,
/// nestable, at any point in the path. A group without a
/// top-level comma expands to its sole contents, as in Ruby.
fn brace_expand(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut commas: Vec<usize> = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                    commas.clear();
                }
                depth += 1;
            }
            b',' if depth == 1 => commas.push(i),
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let open = start.expect("brace group start");
                    let mut alts = Vec::new();
                    let mut prev = open + 1;
                    for &comma in commas.iter().chain([i].iter()) {
                        alts.push(&text[prev..comma]);
                        prev = comma + 1;
                    }
                    let mut out = Vec::new();
                    for alt in alts {
                        out.extend(brace_expand(&format!(
                            "{}{}{}",
                            &text[..open],
                            alt,
                            &text[i + 1..]
                        )));
                    }
                    return out;
                }
            }
            _ => {}
        }
    }
    vec![text.to_string()]
}

/// Substitute the API path placeholders in one stanza path.
/// Returns `None` when the path carries no placeholder.
fn expand_stanza_placeholders(prefix: &Prefix, raw: &str) -> Option<String> {
    for (placeholder, base) in [
        ("$APPDIR", prefix.applications()),
        ("$HOMEBREW_PREFIX", prefix.root.clone()),
    ] {
        if raw == placeholder {
            return Some(base.display().to_string());
        }
        if let Some(rest) = raw
            .strip_prefix(placeholder)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            return Some(format!("{}/{rest}", base.display()));
        }
    }
    None
}

/// One `trash:` block, moved with `/usr/bin/trash` (the same
/// Foundation trashing brew's Mac impl performs).
fn zap_trash(prefix: &Prefix, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let resolved = resolve_stanza_paths(prefix, "trash", paths);
    println!("{}", super::tty::ohai("Trashing files:"));
    for (raw, _) in &resolved {
        println!("{raw}");
    }
    let existing: Vec<PathBuf> = resolved.into_iter().flat_map(|(_, paths)| paths).collect();
    if existing.is_empty() {
        return;
    }
    let args: Vec<String> = existing
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = fetch::run_inherit("/usr/bin/trash", &arg_refs);
    if code != 0 {
        super::tty::opoo!(
            "Warning: The following files could not be trashed, please do so manually:"
        );
        for path in &existing {
            if path.symlink_metadata().is_ok() {
                eprintln!("{}", path.display());
            }
        }
    }
}

/// Brew's `recursive_rmdir`: remove directories holding only
/// directories and `.DS_Store` files, bottom-up.
fn zap_rmdir(prefix: &Prefix, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    println!("{}", super::tty::ohai("Removing directories if empty:"));
    fn recursive(path: &Path, home: &Path) -> bool {
        let shown = path.strip_prefix(home).map_or_else(
            |_| path.display().to_string(),
            |rel| format!("~/{}", rel.display()),
        );
        println!("{shown}");
        if path.is_symlink() || !path.is_dir() {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        let mut children: Vec<(PathBuf, Option<std::fs::FileType>)> = entries
            .flatten()
            .map(|e| (e.path(), e.file_type().ok()))
            .collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));
        if !children.iter().all(|(child, ft)| {
            ft.is_some_and(|ft| ft.is_dir()) || child.file_name().is_some_and(|n| n == ".DS_Store")
        }) {
            return false;
        }
        if let Some(ds) = children
            .iter()
            .position(|(child, _)| child.file_name().is_some_and(|n| n == ".DS_Store"))
        {
            let (ds, _) = children.remove(ds);
            if std::fs::remove_file(&ds).is_err() {
                return false;
            }
        }
        if !children.iter().all(|(child, _)| recursive(child, home)) {
            return false;
        }
        std::fs::remove_dir(path).is_ok()
    }
    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
    for (_, resolved) in resolve_stanza_paths(prefix, "rmdir", paths) {
        if resolved.iter().all(|p| p.is_dir()) {
            for dir in &resolved {
                recursive(dir, &home);
            }
        }
    }
}

/// `brew zap` / `uninstall --zap` for the file-only stanza
/// subset (99% of zaps): trash and rmdir. Any other directive in
/// either stanza — quit, launchctl, pkgutil, script, sudo-bound
/// delete — delegates before any effect.
pub fn zap(
    prefix: &Prefix,
    token: &str,
    cask: &super::formula::Cask,
    dry_run: bool,
    force: bool,
) -> Result<(), InstallError> {
    if cask.zap_unsupported || !cask.zap_delete.is_empty() || cask.uninstall_unsupported {
        return Err(InstallError::Unsupported(format!(
            "zap for {token} uses directives abrew does not run natively"
        )));
    }
    let inst = prefix.installed_one(Kind::Cask, token);
    if inst.is_none() && !force {
        return Err(InstallError::Failed(format!(
            "Error: Cask '{token}' is not installed."
        )));
    }
    if dry_run {
        println!("Would uninstall Cask {token}");
        return Ok(());
    }
    // Brew's order: non-deferred directives (trash) run before the
    // artifacts come out; rmdir is deferred until after.
    let plan = match &inst {
        Some(inst) => {
            let directories = artifact_directories(prefix, PathBuf::from("/Applications"))?;
            Some(UninstallPlan::prepare(
                prefix,
                token,
                &inst.versions,
                &directories,
            )?)
        }
        None => None,
    };
    println!(
        "{}",
        super::tty::oh1(&format!("Zapping Cask {}", super::tty::ident(token)))
    );
    if !cask.zap_present {
        super::tty::opoo!("Warning: No zap stanza present for Cask '{token}'");
    }
    zap_trash(prefix, &cask.uninstall_trash);
    zap_trash(prefix, &cask.zap_trash);
    if let Some(plan) = plan {
        for link in plan.links.keys() {
            let (label, linked) = destination_label(prefix, link);
            println!(
                "==> {} {label} '{}'",
                if linked { "Unlinking" } else { "Removing" },
                link.display()
            );
        }
        plan.apply(prefix)?;
    } else if prefix.cask_pin_link_present(token) {
        prefix.unpin_cask(token)?;
    }
    zap_rmdir(prefix, &cask.uninstall_rmdir);
    zap_rmdir(prefix, &cask.zap_rmdir);
    println!(
        "{}",
        super::tty::ohai(&format!("Removing all staged versions of Cask '{token}'"))
    );
    Ok(())
}

fn uninstall_from(
    prefix: &Prefix,
    token: &str,
    dry_run: bool,
    directories: &[PathBuf],
    cask: Option<&super::formula::Cask>,
    keep: &BTreeSet<PathBuf>,
) -> Result<(), InstallError> {
    let Some(inst) = prefix.installed_one(Kind::Cask, token) else {
        return Err(InstallError::Failed(format!(
            "Error: Cask '{token}' is not installed."
        )));
    };
    if dry_run {
        println!("Would uninstall Cask {token}");
        return Ok(());
    }
    let mut plan = UninstallPlan::prepare(prefix, token, &inst.versions, directories)?;
    plan.links.retain(|path, _| !keep.contains(path));
    println!(
        "{}",
        super::tty::oh1(&format!("Uninstalling Cask {}", super::tty::ident(token)))
    );
    if let Some(cask) = cask {
        zap_trash(prefix, &cask.uninstall_trash);
    }
    for (link, (target, _)) in &plan.links {
        let (label, linked) = destination_label(prefix, link);
        if !linked {
            // Brew copies a moved artifact back into its staged
            // path before removing it; the payload never leaves the
            // Caskroom in abrew's symlink model, so the staged path
            // the link already points at is that destination.
            let joined = if target.is_absolute() {
                target.clone()
            } else {
                link.parent()
                    .map_or_else(|| target.clone(), |p| p.join(target))
            };
            let source = std::fs::canonicalize(&joined).unwrap_or(joined);
            let base = link
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!("==> Backing up {label} '{base}' to '{}'", source.display());
        }
        println!(
            "==> {} {label} '{}'",
            if linked { "Unlinking" } else { "Removing" },
            link.display()
        );
    }
    for version in &inst.versions {
        println!("==> Purging files for version {version} of Cask {token}");
    }
    plan.apply(prefix)?;
    if let Some(cask) = cask {
        zap_rmdir(prefix, &cask.uninstall_rmdir);
    }
    Ok(())
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
                let kind = entry.file_type().map_err(|e| format!("abrew: {e}"))?;
                if !kind.is_symlink() {
                    continue;
                }
                let path = entry.path();
                let identity = entry.metadata().map_err(|e| format!("abrew: {e}"))?;
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
        if prefix.cask_pin_link_present(&self.token) {
            prefix.unpin_cask(&self.token)?;
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
#[path = "cask_tests.rs"]
mod tests;
