// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native casks install the plain-data artifact kinds (app, binary,
//! font, manpage, suites, plugins, generic artifacts, generated
//! wrappers) from bounded zip/tar payloads, including `container
//! nested:` archives and `depends_on` dependencies poured first.
//! Recipe-executing artifacts, disk images and pkg installers need
//! Homebrew's full recipe and pass through before any native
//! installation changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::fetch;
use super::formula::{Artifact, Cask};
use super::install::{
    InstallError, InstallLink, check_directory, link_target_points_into, publish_payload,
    same_path_identity,
};
use super::prefix::{Kind, Prefix, owned_receipt};

/// The running macOS version, probed with sw_vers. None off macOS or on
/// any probe failure (callers delegate rather than guess).
fn macos_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = aterm_uds::spawnfd::command("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let version = String::from_utf8(out.stdout).ok()?.trim().to_string();
        super::formula::numeric_macos_version(&version).then_some(version)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn arch_name() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

/// Whether the running macOS meets a `depends_on macos: ">= X"` floor.
fn macos_floor_met(floor: &str) -> bool {
    macos_version()
        .is_some_and(|v| super::formula::version_cmp(&v, floor) != std::cmp::Ordering::Less)
}

/// Apply the variation for this machine, when the cask carries any.
/// The resolved clone drops `variations` so re-resolving is a no-op.
pub(super) fn resolve(cask: &Cask) -> Result<std::borrow::Cow<'_, Cask>, InstallError> {
    if cask.unsupported_constraints {
        return Err(InstallError::PassThrough(format!(
            "cask {} has unsupported constraints",
            cask.token
        )));
    }
    if cask.variations.is_empty() {
        return Ok(std::borrow::Cow::Borrowed(cask));
    }
    let major = macos_version()
        .and_then(|v| v.split('.').next().and_then(|m| m.parse::<u64>().ok()))
        .ok_or_else(|| {
            InstallError::PassThrough(format!(
                "cask {} needs Homebrew variation selection",
                cask.token
            ))
        })?;
    let Some(variation) = cask.variation_for(major, arch_name()) else {
        return Ok(std::borrow::Cow::Borrowed(cask));
    };
    if variation.unsupported {
        return Err(InstallError::PassThrough(format!(
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

/// The pre-lock delegation check for one cask: every PassThrough the
/// install itself would raise before downloading.
pub(super) fn preflight(prefix: &Prefix, cask: &Cask) -> Result<(), InstallError> {
    let resolved = resolve(cask)?;
    let cask: &Cask = &resolved;
    if let Some(floor) = &cask.macos_min
        && !macos_floor_met(floor)
    {
        return Err(InstallError::PassThrough(format!(
            "cask {} needs macOS >= {floor}",
            cask.token
        )));
    }
    if let Some(exact) = &cask.macos_exact {
        let met = macos_version().is_some_and(|v| {
            let bound: Vec<&str> = exact.split('.').collect();
            let here: Vec<&str> = v.split('.').collect();
            here.len() >= bound.len() && here[..bound.len()] == bound[..]
        });
        if !met {
            return Err(InstallError::PassThrough(format!(
                "cask {} needs exactly macOS {exact}",
                cask.token
            )));
        }
    }
    if let Some(arch) = &cask.arch {
        let here = arch_name();
        if arch != here {
            return Err(InstallError::PassThrough(format!(
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
        return Err(InstallError::PassThrough(format!(
            "cask {} is disabled upstream",
            cask.token
        )));
    }
    if cask.artifacts.iter().any(|a| matches!(a, Artifact::Pkg(_))) {
        return Err(InstallError::PassThrough(format!(
            "cask {} ships a pkg installer (root work abrew never does itself)",
            cask.token
        )));
    }
    if (!cask.naked && !fetch::archive_supported(&download_name(cask))) || cask.artifacts.is_empty()
    {
        return Err(InstallError::PassThrough(format!(
            "cask {} needs its Homebrew extraction recipe",
            cask.token
        )));
    }
    artifact_paths(prefix, cask)?;
    Ok(())
}

/// The dependency half of preflight: every `depends_on` formula or
/// cask that is not already installed must itself plan natively
/// (cask deps recursively), so delegation still happens before any
/// effect — a cask never half-lands and then delegates its dep.
pub(super) fn preflight_deps(
    api: &super::api::Api,
    prefix: &Prefix,
    cask: &Cask,
) -> Result<(), InstallError> {
    let mut seen = BTreeSet::new();
    preflight_deps_inner(api, prefix, cask, &mut seen, 0)
}

fn preflight_deps_inner(
    api: &super::api::Api,
    prefix: &Prefix,
    cask: &Cask,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), InstallError> {
    let resolved = resolve(cask)?;
    let cask: &Cask = &resolved;
    if depth > 8 {
        return Err(InstallError::PassThrough(format!(
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
            super::install::preflight_plan(api, prefix, dep, &dep_opts)?;
        }
    }
    for dep in &cask.depends_cask {
        if prefix.installed_one(Kind::Cask, dep).is_some() || !seen.insert(dep.clone()) {
            continue;
        }
        let dc = api.cask(dep).map_err(InstallError::Failed)?;
        preflight(prefix, &dc)?;
        preflight_deps_inner(api, prefix, &dc, seen, depth + 1)?;
    }
    Ok(())
}

/// Install a cask after its `depends_on` dependencies, brew's
/// order: formula deps as dependencies (receipts record them as
/// not-on-request), cask deps recursively, then the cask itself.
/// Deps already installed are left exactly as they stand.
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
    install_deps_inner(api, prefix, cask, dry_run, &mut seen, 0)?;
    install(prefix, cask, dry_run, force, upgrade, reinstall)
}

fn install_deps_inner(
    api: &super::api::Api,
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), InstallError> {
    let resolved = resolve(cask)?;
    let cask: &Cask = &resolved;
    if depth > 8 {
        return Err(InstallError::PassThrough(format!(
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
            super::install::install(api, prefix, dep, &dep_opts)?;
        }
    }
    for dep in &cask.depends_cask {
        if prefix.installed_one(Kind::Cask, dep).is_some() || !seen.insert(dep.clone()) {
            continue;
        }
        let dc = api.cask(dep).map_err(InstallError::Failed)?;
        install_deps_inner(api, prefix, &dc, dry_run, seen, depth + 1)?;
        install(prefix, &dc, dry_run, false, false, false)?;
    }
    Ok(())
}

/// Install one cask. Unsupported recipes refuse before downloading or placing
/// artifacts. A missing `no_check` cache entry still has to be downloaded.
pub fn install(
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    force: bool,
    upgrade: bool,
    reinstall: bool,
) -> Result<String, InstallError> {
    if cask.disabled {
        return Err(InstallError::PassThrough(format!(
            "cask {} is disabled upstream",
            cask.token
        )));
    }
    let resolved = resolve(cask)?;
    let cask: &Cask = &resolved;
    preflight(prefix, cask)?;
    if cask.deprecated {
        eprintln!("abrew: warning: cask {} is deprecated upstream", cask.token);
    }
    if !super::valid_component(&cask.token) || !super::valid_cask_version(&cask.version) {
        return Err(InstallError::PassThrough(
            "unsupported cask token/version".to_string(),
        ));
    }
    let mut replacing = false;
    let mut same_version_reinstall = false;
    if let Some(inst) = prefix.installed_one(Kind::Cask, &cask.token) {
        let older =
            super::formula::version_cmp(&inst.version, &cask.version) == std::cmp::Ordering::Less;
        if reinstall && inst.version == cask.version {
            replacing = true;
            same_version_reinstall = true;
        } else if (upgrade || reinstall) && older {
            replacing = true;
        } else if force || reinstall {
            return Err(InstallError::PassThrough(format!(
                "replacement of cask {} needs brew",
                cask.token
            )));
        } else {
            println!("abrew: {} {} already installed", cask.token, inst.version);
            return Ok(inst.version);
        }
    }
    let file = download_name(cask);
    if (!cask.naked && !fetch::archive_supported(&file)) || cask.artifacts.is_empty() {
        return Err(InstallError::PassThrough(format!(
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
        println!("Would install cask {} {}", cask.token, cask.version);
        return Ok(cask.version.clone());
    }
    prefix.ensure()?;
    let dest = prefix.cache().join(file);
    let expected = cask.sha256.as_deref().unwrap_or("no_check");
    // A no_check payload is bound to neither version nor URL, so its
    // cache entry is never reused: always re-download, never receipt
    // stale bytes under the current version.
    let reusable = cask.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
    if !reusable {
        println!("==> Fetching cask {} ({})", cask.token, cask.version);
        fetch::curl_to_file(&cask.url, &dest, &[])?;
    }
    fetch::verify_sha256(&dest, expected)?;
    let work =
        aterm_tempfile::TempDir::new_in(prefix.caskroom()).map_err(|e| format!("abrew: {e}"))?;
    let payload = work.path().join("payload");
    if cask.naked {
        // The download is the single payload file; stage it under its
        // URL basename (template-expanded like the cache name minus the
        // token prefix), executable when a binary artifact names it.
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
        // Brew stages a naked download under the artifact's stated
        // source name, which need not be the URL basename (p4's binary
        // source is its URL's parent directory). Stage a copy under
        // every single-component artifact source name.
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
    if !cask.naked {
        let extraction_error = |e: String| extraction_error(&cask.token, e);
        if let Some(nested) = &cask.nested {
            // `container nested:` — the download only wraps the real
            // payload archive. Unpack the outer archive privately,
            // then the named inner archive becomes the payload.
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
        } else {
            fetch::untar(&dest, &payload).map_err(extraction_error)?;
        }
        extract_nested_tars(&payload, &artifacts, &cask.token)?;
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
            // Only binaries must be executable; fonts and manpages are data.
            valid
                && (!*exec
                    || src
                        .metadata()
                        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
        };
        // No mode bits off unix: an artifact there is valid by its kind alone.
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
    let mut receipt_fields: Vec<(&str, &str)> = vec![
        ("token", &cask.token),
        ("version", &cask.version),
        ("installed_by", "abrew"),
        // Casks resolve from the homebrew/cask API, where the token
        // is the full identity (`list --full-name` adds the tap).
        ("full_name", &cask.token),
    ];
    for (k, v) in &artifact_fields {
        receipt_fields.push((k, v));
    }
    prefix.write_receipt(&payload, &receipt_fields)?;
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
    super::install::note_bin_not_on_path(prefix, false);
    Ok(cask.version.clone())
}

/// Record the complete destination set only when every path is lossless UTF-8.
/// Otherwise retain the legacy, identity-checked target survey for this version;
/// a lossy spelling must never exclude an owned link while its payload is removed.
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

/// New writes record prefix-relative names too: the artifact path remains lossless
/// when the prefix itself is not UTF-8. Legacy absolute fields stay for old readers.
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
    // Absolute-only receipts under a non-UTF8 prefix may have been written lossily.
    // Preserve that version's legacy, identity-checked survey instead of trusting it.
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
        return Err(InstallError::PassThrough(
            "command wrappers need a lossless prefix path".to_string(),
        ));
    }
    if !executable.contains('/') {
        if !super::valid_component(executable) || executable.starts_with('-') {
            return Err(InstallError::PassThrough(
                "unsupported wrapper program name".to_string(),
            ));
        }
        return Ok(None);
    }
    let generic_bases: Vec<(&str, &str)> = cask
        .artifacts
        .iter()
        .filter_map(|artifact| match artifact {
            Artifact::Generic(source, Some(target)) => Some((target.as_str(), source.as_str())),
            _ => None,
        })
        .collect();
    let relative = if executable.starts_with("$HOMEBREW_PREFIX/") {
        // A wrapper may exec from inside its own staged Caskroom
        // tree (`$HOMEBREW_PREFIX/Caskroom/<token>/<version>/...`,
        // prince): that tree is this cask's payload, so the tail is
        // payload-relative. Anything else under the prefix must
        // rebase onto a generic artifact's install tree.
        rebase_caskroom_source(cask, executable)
            .or_else(|| rebase_source(&generic_bases, executable))
            .ok_or_else(|| {
                InstallError::PassThrough(
                    "wrapper executable is outside the admitted payload mapping".to_string(),
                )
            })?
    } else {
        executable.to_string()
    };
    if relative.is_empty()
        || Path::new(&relative)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(InstallError::PassThrough(
            "invalid wrapper payload program".to_string(),
        ));
    }
    Ok(Some(PathBuf::from(relative)))
}

/// Some casks ship an artifact inside a same-named nested tar
/// (restic-browser: the zip holds only `Restic-Browser.app.tar`,
/// and brew's container handling unpacks it to the app). When an
/// artifact source is missing but `<source>.tar` sits in the
/// payload as a regular file, extract it in place with the same
/// bounded extractor; the artifact census below still judges the
/// result. Nothing else about the payload changes.
/// Classify an extraction failure for cask payloads. Container
/// FORMATS the vetted readers do not speak (bzip2/xz magic) and zip
/// STRUCTURES the strict central-directory reader refuses (trailing
/// bytes after the end record, multi-disk, encryption, an unknown
/// method — real casks ship such zips, e.g. an appended signature
/// blob past the end record, and brew's `ditto` tolerates them)
/// delegate to real brew. The download's sha256 gate already proved
/// the bytes are upstream's, so a structural refusal is an upstream
/// shape, not corruption. Vetting refusals (slip entries, bomb caps)
/// render differently and stay hard failures.
fn extraction_error(token: &str, e: String) -> InstallError {
    if e.contains("unsupported archive") || e.contains("io: zip: ") {
        InstallError::PassThrough(format!(
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
            // The bounded extractor wants an empty destination:
            // unpack beside the payload trees, then rename the
            // top-level entries in. A name already in the payload
            // is a refusal, never an overwrite.
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

/// Rebase a `$HOMEBREW_PREFIX/Caskroom/<token>/<version>/...`
/// source onto this cask's payload: brew stages the payload in
/// exactly that tree, and so does abrew.
fn rebase_caskroom_source(cask: &Cask, name: &str) -> Option<String> {
    let rest = name.strip_prefix("$HOMEBREW_PREFIX/Caskroom/")?;
    let own = format!("{}/{}/", cask.token, cask.version);
    rest.strip_prefix(&own).map(str::to_string)
}

/// Rebase an `/Applications/<installed>/...` source onto the app
/// artifact's payload path, when this cask installs that app.
fn rebase_app_source(app_bases: &[(String, String)], name: &str) -> Option<String> {
    let rest = name.strip_prefix("/Applications/")?;
    let (installed, tail) = rest.split_once('/')?;
    let (_, src) = app_bases.iter().find(|(n, _)| n == installed)?;
    Some(format!("{src}/{tail}"))
}

/// Rebase a `$HOMEBREW_PREFIX/...` source onto the generic artifact
/// payload tree it names, when one matches.
fn rebase_source(generic_bases: &[(&str, &str)], name: &str) -> Option<String> {
    let rest = name.strip_prefix("$HOMEBREW_PREFIX/")?;
    for (target, src) in generic_bases {
        if let Some(tail) = target
            .strip_prefix("$HOMEBREW_PREFIX/")
            .and_then(|t| rest.strip_prefix(t))
            .and_then(|t| t.strip_prefix('/'))
        {
            return Some(format!("{src}/{tail}"));
        }
    }
    None
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
/// Only `$HOMEBREW_PREFIX/...`, home-`Library/...` and `$APPDIR/...`
/// targets have a prefix-local meaning; everything else delegates.
fn map_generic_target(prefix: &Prefix, target: &str) -> Option<PathBuf> {
    super::formula::generic_target_relative(target).map(|relative| prefix.root.join(&*relative))
}

/// (payload-relative source, destination, is-directory, must-be-executable)
fn artifact_paths(
    prefix: &Prefix,
    cask: &Cask,
) -> Result<Vec<(PathBuf, PathBuf, bool, bool)>, InstallError> {
    let mut paths = Vec::new();
    let mut destinations = BTreeSet::new();
    // A later artifact may source from inside a generic artifact's
    // install tree (`$HOMEBREW_PREFIX/share/<x>/.../bin/tool`); rebase
    // such sources onto that artifact's payload path.
    let generic_bases: Vec<(&str, &str)> = cask
        .artifacts
        .iter()
        .filter_map(|a| match a {
            Artifact::Generic(src, Some(target)) => Some((target.as_str(), src.as_str())),
            _ => None,
        })
        .collect();
    let rebase = |name: &str| rebase_source(&generic_bases, name);
    // A later artifact may also source from inside this cask's own
    // installed app (`/Applications/<App>.app/Contents/...` — brew
    // links such binaries into the installed app; abrew links into
    // the same app inside its payload tree).
    let app_bases: Vec<(String, String)> = cask
        .artifacts
        .iter()
        .filter_map(|a| match a {
            Artifact::App(src, dest) | Artifact::Suite(src, dest) => {
                let installed = dest.clone().or_else(|| {
                    Path::new(src)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })?;
                Some((installed, src.clone()))
            }
            _ => None,
        })
        .collect();
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
                    return Err(InstallError::PassThrough(format!(
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
                    return Err(InstallError::PassThrough(
                        "invalid prefix-relative artifact directory".to_string(),
                    ));
                }
                (name, dest, prefix.root.join(directory), false, false)
            }
            Artifact::Wrapper(name, executable, args, dest) => {
                wrapper_program(prefix, cask, executable)?;
                if args.iter().any(|argument| argument.contains('\0')) {
                    return Err(InstallError::PassThrough(
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
                    return Err(InstallError::PassThrough(
                        "cask command wrapper needs a Homebrew name mapping".to_string(),
                    ));
                };
                let dst = prefix.bin().join(&file);
                if !destinations.insert(dst.clone()) {
                    return Err(InstallError::PassThrough(
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
                    return Err(InstallError::PassThrough(format!(
                        "cask {} artifact target is outside abrew's prefix mapping",
                        cask.token
                    )));
                };
                if !destinations.insert(dst.clone()) {
                    return Err(InstallError::PassThrough(
                        "cask has overlapping artifact destinations".to_string(),
                    ));
                }
                let path = Path::new(name);
                if name.is_empty()
                    || path
                        .components()
                        .any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err(InstallError::PassThrough(
                        "cask artifact needs a Homebrew path mapping".to_string(),
                    ));
                }
                paths.push((path.to_path_buf(), dst, false, false));
                continue;
            }
            Artifact::Other(kind) => {
                // Name the artifact that forced delegation: a bare
                // "unsupported" makes every upstream recipe change
                // look like an abrew regression (1password-cli's
                // generate_completions_from_executable did).
                return Err(InstallError::PassThrough(format!(
                    "cask {} has an unsupported artifact recipe ({kind})",
                    cask.token
                )));
            }
            _ => {
                return Err(InstallError::PassThrough(format!(
                    "cask {} has an unsupported artifact recipe",
                    cask.token
                )));
            }
        };
        let rebased;
        let name: &str = if name.starts_with("$HOMEBREW_PREFIX/") {
            let Some(r) = rebase(name) else {
                return Err(InstallError::PassThrough(format!(
                    "cask {} artifact sources from an unmapped install tree",
                    cask.token
                )));
            };
            rebased = r;
            &rebased
        } else if let Some(r) = rebase_app_source(&app_bases, name) {
            rebased = r;
            &rebased
        } else {
            name
        };
        let path = Path::new(name);
        if name.is_empty()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(InstallError::PassThrough(
                "cask artifact needs a Homebrew path mapping".to_string(),
            ));
        }
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
            return Err(InstallError::PassThrough(
                "artifact destination escapes the prefix".to_string(),
            ));
        }
        if !destinations.insert(dst.clone()) {
            return Err(InstallError::PassThrough(
                "cask has overlapping artifact destinations".to_string(),
            ));
        }
        paths.push((path.to_path_buf(), dst, app, exec));
    }
    Ok(paths)
}

/// Remove links into receipt-admitted versions and only those payloads.
/// Unknown sibling versions and user data (`zap`) remain untouched.
pub fn uninstall(prefix: &Prefix, token: &str, dry_run: bool) -> Result<(), InstallError> {
    let directories = artifact_directories(prefix, PathBuf::from("/Applications"))?;
    uninstall_from(prefix, token, dry_run, &directories)
}

/// Legacy unrecorded artifacts used these roots. New complete receipts add their
/// exact checked parents in UninstallPlan, with no guessed depth-limited tree census.
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
    // Retain the discoverable legacy Generic subset. Bound depth as before,
    // propagate uncertainty, and never traverse an alias. Recorded new artifacts
    // add their exact parents separately, without depending on this depth limit.
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
        // A recorded Generic final artifact may be this name. Do not follow it;
        // the plan must admit that exact leaf or refuse it as a directory alias.
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

// The production directory roster is fixed above. Captures supply only their own
// directories, so the preservation regression never enumerates global applications.
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
    UninstallPlan::prepare(prefix, token, &inst.versions, directories)?.apply(prefix)
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
        // Survey every artifact directory before removing any entry or payload.
        // Only NotFound proves a directory absent; a planned pathname is not authority.
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
        // Admit the whole plan first, then recheck each entry immediately before
        // removal. Independent filesystem writers are not an atomic transaction.
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
        // $APPDIR is the cask appdir: brew's /Applications, abrew's
        // prefix Applications (electricbinary, liclipse).
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
        // The actual writer emits only into a namespace it just reserved.
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
                    "Demo.app/Contents/MacOS/demo".to_string(),
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
            Err(InstallError::PassThrough(_))
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
        // A plain reinstall of the same cask at a newer version is a
        // replacement (delegated); as an upgrade it swaps natively.
        std::fs::write(&download, b"#!/bin/sh\nprintf 'naked ready2\\n'\n").unwrap();
        c.version = "2".to_string();
        assert!(matches!(
            install(&prefix, &c, false, false, false, false),
            Ok(v) if v == "1"
        ));
        assert_eq!(
            install(&prefix, &c, false, false, true, false).unwrap(),
            "2"
        );
        let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
            .output()
            .unwrap();
        assert_eq!(output.stdout, b"naked ready2\n");
        // Same-version reinstall swaps the payload in place.
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
        assert!(std::fs::symlink_metadata(prefix.bin().join("staged-name")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn container_nested_archive_becomes_the_payload() {
        // gearboy's shape: the download wraps the real payload zip.
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
        // restic-browser's shape: the zip holds only Demo.app.tar.
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
    fn extraction_structural_refusals_delegate_vetting_refusals_fail() {
        // Zip structures the strict reader refuses (empoche ships
        // ~80KB past its end record) delegate to brew's extractor.
        assert!(matches!(
            extraction_error(
                "empoche",
                "abrew: cannot extract /c/e.zip: io: zip: no end-of-central-directory record"
                    .to_string()
            ),
            InstallError::PassThrough(_)
        ));
        assert!(matches!(
            extraction_error(
                "x",
                "abrew: unsupported archive; use brew for this format".to_string()
            ),
            InstallError::PassThrough(_)
        ));
        // Vetting refusals and plain IO failures stay hard failures.
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
        // prince's shape: the wrapper execs from its own staged
        // Caskroom tree.
        assert_eq!(
            wrapper_program(
                &prefix,
                &c,
                "$HOMEBREW_PREFIX/Caskroom/demo/1/staged/bin/tool"
            )
            .unwrap(),
            Some(PathBuf::from("staged/bin/tool"))
        );
        // Another cask's tree, or another version, does not rebase.
        for exe in [
            "$HOMEBREW_PREFIX/Caskroom/other/1/bin/tool",
            "$HOMEBREW_PREFIX/Caskroom/demo/2/bin/tool",
            "$HOMEBREW_PREFIX/share/demo/bin/tool",
        ] {
            assert!(matches!(
                wrapper_program(&prefix, &c, exe),
                Err(InstallError::PassThrough(_))
            ));
        }
    }

    #[test]
    fn applications_sourced_binary_rebases_onto_the_app_payload() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        // ruffle's shape: the binary sources from inside the app the
        // same cask installs, spelled by its installed /Applications
        // path.
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
        // Renamed app: the installed name is the rebase key.
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
        // No matching app artifact: still delegates.
        c.artifacts = vec![Artifact::Binary(
            "/Applications/Other.app/Contents/MacOS/tool".to_string(),
            None,
        )];
        assert!(matches!(
            artifact_paths(&prefix, &c),
            Err(InstallError::PassThrough(_))
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
                Err(InstallError::PassThrough(_))
            ));
            assert!(!prefix.root.exists());
        }
        // The delegation message names the artifact kind, so an
        // upstream recipe change reads as upstream, not as abrew
        // silently losing a cask it used to pour.
        let mut named = cask("unused.zip".to_string());
        named.artifacts.push(Artifact::Other(
            "generate_completions_from_executable".to_string(),
        ));
        let Err(InstallError::PassThrough(msg)) =
            install(&prefix, &named, false, false, false, false)
        else {
            panic!("expected passthrough");
        };
        assert!(
            msg.contains("generate_completions_from_executable"),
            "{msg}"
        );
        assert!(!prefix.root.exists());
        let cask = cask("unused.dmg".to_string());
        assert!(matches!(
            install(&prefix, &cask, false, false, false, false),
            Err(InstallError::PassThrough(_))
        ));
        assert!(!prefix.root.exists());
        // `depends_on` formula/cask names are data now: they parse
        // into dependency lists the dep preflight/install walks.
        let with_deps = aterm_json::from_str(
            r#"{"token":"demo","version":"1","url":"unused.zip","sha256":"no_check","depends_on":{"formula":["other"],"cask":["helper"],"macos":{}},"artifacts":[{"app":["Demo.app"]}]}"#,
        )
        .unwrap();
        let with_deps = Cask::from_api(&with_deps).unwrap();
        assert!(!with_deps.unsupported_constraints);
        assert_eq!(with_deps.depends_formula, ["other"]);
        assert_eq!(with_deps.depends_cask, ["helper"]);
        // Selecting artifacts must not erase an unsupported top-level constraint.
        for constraint in [
            r#""container":{"type":"unknown"}"#,
            r#""depends_on":{"formula":[42]}"#,
            r#""conflicts_with":{"cask":[42]}"#,
        ] {
            let constrained = aterm_json::from_str(&format!(r#"{{"token":"demo","version":"1","url":"unused.zip","sha256":"no_check",{constraint},"artifacts":[{{"app":["Demo.app"]}}],"variations":{{"arm64_tahoe":{{"artifacts":[{{"app":["Demo.app"]}}]}}}}}}"#)).unwrap();
            let constrained = Cask::from_api(&constrained).unwrap();
            assert!(constrained.unsupported_constraints);
            assert!(matches!(
                install(&prefix, &constrained, false, false, false, false),
                Err(InstallError::PassThrough(_))
            ));
            assert!(!prefix.root.exists());
        }
        // A conflict census that cannot be read must not say no conflict exists.
        prefix.ensure().unwrap();
        std::fs::remove_dir(prefix.cellar()).unwrap();
        std::fs::write(prefix.cellar(), b"unknown census").unwrap();
        let mut conflicting = self::cask("unused.zip".to_string());
        conflicting.conflicts = vec!["other".to_string()];
        assert!(preflight(&prefix, &conflicting).is_err());
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
        // A promised Binary with no executable mode is not a successful install.
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
            // Stand in for /Applications without reading or changing the real directory.
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
                    // Keep the old inode alive, so this control cannot depend on reuse.
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
                    // Even a copied valid receipt does not transfer the old inode's authority.
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
        // Legacy and recorded versions coexist. One version's receipt must not
        // exclude another version's owned link, including a non-UTF8 prefix.
        // Some filesystems (including this Mac's) reject non-UTF8 names.
        // Exercise the actual producer and reader's captured-path fallback with
        // captured paths everywhere; retain the full filesystem worlds on Linux.
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
                    // The predecessor wrote this lossy absolute path. Its owned
                    // target must still be found without comparing that spelling.
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
        // A non-directory survey root also refuses before the earlier eligible link.
        std::fs::remove_file(prefix.bin()).unwrap();
        std::fs::write(prefix.bin(), b"foreign root file").unwrap();
        assert!(uninstall_from(&prefix, "demo", false, &directories).is_err());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
        assert_eq!(std::fs::read(prefix.bin()).unwrap(), b"foreign root file");
        // Ordinary physical roots remove only admitted payload links and versions.
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
        // Old receipts without artifact fields retain the already-discoverable
        // share/x subset, rather than deleting its payload behind a dangling link.
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
        // Generic directory links at man traversal names are final leaves.
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
