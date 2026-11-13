// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Offline transport for the ordinary authenticated updater. No local trust anchor.

use super::*;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// A private canonical cut, checked by default. Publishing makes it eligible for host automation.
pub struct LocalStageOptions {
    /// Directory containing the cutter's unchanged canonical assets.
    pub assets: PathBuf,
    /// Running release host; its kernel image determines the actual installed bundle.
    pub host_pid: i32,
    /// Expected installed build, used only as an additional race fence.
    pub expected_build: u64,
    /// Expected installed source commit, used only as an additional race fence.
    pub expected_commit: String,
    /// Explicitly publish the verified candidate into the existing updater stage.
    pub stage: bool,
}

fn regular(path: &Path) -> Result<(std::fs::File, std::fs::Metadata), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    Ok((file, meta))
}

fn read_asset(dir: &Path, name: &str, cap: u64) -> Result<Vec<u8>, String> {
    leaf(name)?;
    let (file, meta) = regular(&dir.join(name))?;
    if meta.len() > cap {
        return Err(format!("{name} exceeds its {cap}-byte bound"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
        return Err(format!("{name} exceeds its {cap}-byte bound"));
    }
    Ok(bytes)
}

fn leaf(name: &str) -> Result<(), String> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return Err(format!("asset name is not one leaf: {name:?}"));
    }
    Ok(())
}

struct HostBinding {
    identity: aterm_uds::process::Identity,
    image: aterm_uds::process::LoadedImage,
    app: PathBuf,
}

impl HostBinding {
    fn capture(pid: i32) -> Result<Self, String> {
        let identity =
            aterm_uds::process::read_identity(pid).ok_or("release host identity unavailable")?;
        let image =
            aterm_uds::process::LoadedImage::read(pid).ok_or("release host image unavailable")?;
        let app = bundle::resolve_from(image.launch_path())
            .ok_or("host is not an installed application bundle")?
            .app_root;
        if bundle::is_dev_marked(&app) {
            return Err("host is a development bundle".into());
        }
        let binding = Self {
            identity,
            image,
            app,
        };
        binding.current()?;
        Ok(binding)
    }

    fn current(&self) -> Result<(), String> {
        let now = aterm_uds::process::LoadedImage::read(self.identity.pid)
            .ok_or("release host image disappeared")?;
        if aterm_uds::process::read_identity(self.identity.pid) != Some(self.identity)
            || now.launch_path() != self.image.launch_path()
            || self.image.matches_file(now.launch_path()) != Some(true)
            || now.matches_file(self.image.launch_path()) != Some(true)
            || bundle::resolve_from(now.launch_path()).map(|b| b.app_root) != Some(self.app.clone())
        {
            return Err("actual release host changed during candidate verification".into());
        }
        Ok(())
    }

    fn installed(&self, options: &LocalStageOptions) -> Result<(), String> {
        self.current()?;
        let checks =
            crate::verify::check_bundle(&self.app, aterm_update_core::pins::APPLE_TEAM_ID, false);
        checks.policy?;
        if checks.build? != options.expected_build || checks.commit? != options.expected_commit {
            return Err("actual installed release changed; refusing the local stage".into());
        }
        self.current()
    }
}

fn at(root: PathBuf) -> Staging {
    Staging {
        apply_lock: root.join("apply.lock"),
        stage_lock: root.join("stage.lock"),
        download: root.join("download"),
        staged_app: root.join("staged/aterm.app"),
        ready: root.join("ready.toml"),
        status: root.join("status.toml"),
        root,
    }
}

fn stage_scratch(live_root: Option<&Path>) -> std::io::Result<aterm_tempfile::TempDir> {
    let builder = aterm_tempfile::Builder::new().suffix(".noindex");
    match live_root {
        Some(root) => builder.tempdir_in(root),
        None => builder.tempdir(),
    }
}

/// Verify the unchanged machine-signed canonical cut using the network updater's exact
/// authority verifier and bundle staging path. Check-only publishes into owned scratch;
/// explicit staging uses the existing old-host-readable atomic publisher. This tool never
/// invokes apply; a published stage is eligible for the existing host automation.
pub fn stage_local(options: &LocalStageOptions) -> Result<String, String> {
    if aterm_update_core::pins::PAPER_MASTER_PUBKEYS.is_empty() {
        return Err("offline staging requires the pinned paper-master roster tier".into());
    }
    let host = HostBinding::capture(options.host_pid)?;
    host.installed(options)?;
    let updates = aterm_update_core::seal_guard::updates_base()
        .ok_or("cannot resolve the installed updater state")?
        .join("Updates");
    let live = at(updates);
    // An audit never creates or rewrites the installed updater's state.
    let floor = crate::manifest::Floor::read_for_gate(&live.floor())?;
    let scratch = if options.stage {
        aterm_update_core::ensure_private_dir(&live.root).map_err(|e| e.to_string())?;
        stage_scratch(Some(&live.root))
    } else {
        stage_scratch(None)
    }
    .map_err(|e| e.to_string())?;
    let checked = at(scratch.path().join("checked"));
    aterm_update_core::ensure_private_dir(&checked.root).map_err(|e| e.to_string())?;
    // Preserve the actual local floor for the ordinary scratch publisher's late checks.
    if live.floor().exists() {
        use std::io::Write as _;
        let floor_bytes = read_asset(&live.root, "floor.toml", crate::MAX_LEDGER_BYTES)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        opts.open(checked.floor())
            .and_then(|mut f| f.write_all(&floor_bytes))
            .map_err(|e| e.to_string())?;
    }
    let refresh = || crate::manifest::Floor::read(&live.floor()).roster_seq;
    let policy = RosterPolicy {
        master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        floor_seq: floor.roster_seq,
        now_unix: aterm_update_core::roster::now_unix(),
        floor_refresh: Some(&refresh),
    };
    let mut fetched = authenticate(&options.assets, &policy, &floor)?;
    let observed_revocations = fetched.observed_revocations.clone();
    let observed_seq = fetched.observed_roster_seq.unwrap_or(0);
    let (manifest, _, artifact) = fetched.selected.take().ok_or_else(|| {
        fetched
            .rejection_reason
            .unwrap_or_else(|| "local authority refused".into())
    })?;
    if manifest.build_number <= options.expected_build {
        return Err("candidate is not newer than the installed release".into());
    }
    if let Some(why) = floor_hold(
        manifest.build_number,
        floor.min_build.max(manifest.min_build.unwrap_or(0)),
        floor.high_water,
    ) {
        return Err(why);
    }
    if let Some(hold) = crate::hold::held(
        &live,
        manifest.build_number,
        &manifest.sha256,
        crate::install::unix_now_secs(),
    ) {
        return Err(format!(
            "candidate is held by the installed updater: {}",
            crate::hold::refusal(&hold)
        ));
    }
    let copied = scratch.path().join(&artifact.name);
    let (mut input, _) = regular(&options.assets.join(&artifact.name))?;
    let mut output_opts = std::fs::OpenOptions::new();
    output_opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        output_opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut output = output_opts.open(&copied).map_err(|e| e.to_string())?;
    let cap = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;
    let size = std::io::copy(
        &mut Read::by_ref(&mut input).take(cap.saturating_add(1)),
        &mut output,
    )
    .map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())?;
    drop(output);
    if size > cap {
        return Err("candidate container exceeds the updater bound".into());
    }
    if container_sha256(&copied, CONTAINER_HASH_WAIT)? != artifact.sha256.to_ascii_lowercase() {
        return Err("candidate container digest differs from the authenticated manifest".into());
    }
    let confirmed_at = crate::install::now_rfc3339();
    match artifact.container {
        Container::Dmg => install::stage_from_dmg(
            &checked,
            &copied,
            &manifest,
            aterm_update_core::pins::APPLE_TEAM_ID,
            &confirmed_at,
        )?,
        Container::Zip => install::stage_from_zip(
            &checked,
            &copied,
            &manifest,
            aterm_update_core::pins::APPLE_TEAM_ID,
            &confirmed_at,
        )?,
    }
    let ready = Ready::read_publishable(&checked).ok_or("verification left no admissible stage")?;
    if options.stage {
        let _stage = take_stage_lock(&live, install::BACKGROUND_LOCK_WAIT)?;
        aterm_update_core::ensure_private_dir(&live.staged_dir()).map_err(|e| e.to_string())?;
        // A failed publication may have exchanged an old stage into this owned tree.
        // Retain it before mutation so no error path can delete uncertain custody.
        let retained = scratch.keep();
        let publication = install::publish_local_checked(
            &live,
            &checked.staged_app,
            &ready,
            || host.installed(options),
            &crate::manifest::FloorObservation {
                min_build: manifest.min_build.unwrap_or(0),
                high_water: manifest.build_number,
                roster_seq: observed_seq,
                machine: manifest.machine_id.as_deref(),
                revoked: &observed_revocations,
            },
        );
        if let Err(error) = publication {
            return Err(format!(
                "{error}; retained candidate/rollback custody: {}",
                retained.display()
            ));
        }
        std::fs::remove_dir_all(&retained).map_err(|e| {
            format!(
                "stage published; retained old-stage cleanup {}: {e}",
                retained.display()
            )
        })?;
    }
    Ok(format!(
        "{} build {} ({}) — {}",
        manifest.version,
        manifest.build_number,
        manifest.commit.as_deref().unwrap_or(""),
        if options.stage {
            "verified stage published; eligible for existing host automation (tool did not invoke apply)"
        } else {
            "verified in scratch; installed updater untouched"
        }
    ))
}

fn authenticate(
    dir: &Path,
    policy: &RosterPolicy<'_>,
    floor: &crate::manifest::Floor,
) -> Result<AuthoritativeFetch, String> {
    let appcast = read_asset(dir, APPCAST_ASSET, 5_000_000)?;
    // Untrusted parsing supplies only transport names. No field grants authority here.
    let hint = Manifest::parse(std::str::from_utf8(&appcast).map_err(|e| e.to_string())?)?;
    let mut names = vec![
        APPCAST_ASSET.to_string(),
        APPCAST_SIG_ASSET.to_string(),
        aterm_update_core::roster::ROSTER_ASSET.to_string(),
        aterm_update_core::roster::ROSTER_SIG_ASSET.to_string(),
        hint.dmg.clone(),
    ];
    if let Some(zip) = &hint.zip {
        names.push(zip.clone());
    }
    for name in &names {
        leaf(name)?;
    }
    // A local directory, not a repository: the reader's owner and repository name nothing.
    let release = Release {
        owner: String::new(),
        repo: String::new(),
        tag_name: format!("v{}", hint.version),
        assets: names.clone(),
    };
    let candidate = AuthoritativeRelease {
        version: hint.version.clone(),
        release,
        manifest_index: 0,
    };
    let mut fetched = fetch_authoritative_release(
        Some(candidate),
        &mut |asset, cap| {
            read_asset(dir, asset.name, cap)
                .map_err(aterm_update_core::release_asset::AssetError::Refused)
        },
        policy,
    );
    let source = Source::resolve_with(false, None, None);
    let tag = format!("v{}", hint.version);
    fetched.refuse_unbound_or_revoked(&source, floor, &tag);
    if fetched.attribution.is_none() {
        return Err("no authenticated machine attribution".into());
    }
    Ok(fetched)
}

#[cfg(test)]
#[path = "local_stage_tests.rs"]
mod tests;
