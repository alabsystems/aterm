// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux release asset verification through the signed roster and appcast chain.

use aterm_update_core::linux::LinuxTarget;
use aterm_update_core::{Manifest, Roster, verify_roster};
use atpkg::signed_artifact_cache::{SignedArtifactCache, SignedReleaseKey};
use atpkg::store::Layout;

use super::releases::{LinuxDownload, LinuxKind, LinuxProver, Release, linux_appcast_name};

const APPCAST_LIMIT: u64 = 5_000_000;
const SIG_LIMIT: u64 = 64;
const ROSTER_LIMIT: u64 = 65_536;

/// A [`LinuxProver`] over the published proof assets, read through `get`.
pub struct Chain<'a> {
    pub slug: &'a str,
    /// The paper master keyset (`pins::PAPER_MASTER_PUBKEYS`; a test pins its own).
    pub masters: &'a [&'a str],
    pub now_unix: i64,
    /// The manager layout that keeps exact, already-verified signed release pairs.
    pub artifact_layout: Option<&'a Layout>,
    /// A release asset's bytes at `url`, at most `limit` of them.
    pub get: &'a mut super::Fetch<'a>,
}

impl Chain<'_> {
    fn fetch(&mut self, r: &Release, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        if r.named(name).is_none() {
            return Err(format!("missing or duplicate {name}"));
        }
        let bytes = (self.get)(&r.download_url(self.slug, name), limit)?;
        if bytes.len() as u64 > limit {
            return Err(format!("oversized {name}"));
        }
        Ok(bytes)
    }

    fn pair_is_listed(&self, r: &Release, name: &str) -> Result<(), String> {
        if r.named(name).is_none() {
            return Err(format!("missing or duplicate {name}"));
        }
        let sig = format!("{name}.sig");
        if r.named(&sig).is_none() {
            return Err(format!("missing or duplicate {sig}"));
        }
        Ok(())
    }

    fn cached_pair(&self, r: &Release, name: &str, limit: u64) -> Option<(Vec<u8>, Vec<u8>)> {
        let layout = self.artifact_layout?;
        let source = format!("github:{}", self.slug);
        let key = SignedReleaseKey::asset(&source, &r.tag, name)?;
        SignedArtifactCache::for_layout(layout).load_pair(
            &key,
            usize::try_from(limit).ok()?,
            usize::try_from(SIG_LIMIT).ok()?,
        )
    }

    fn store_pair(&self, r: &Release, name: &str, limit: u64, bytes: &[u8], sig: &[u8]) {
        let Some(layout) = self.artifact_layout else {
            return;
        };
        let Ok(document_cap) = usize::try_from(limit) else {
            return;
        };
        let Ok(signature_cap) = usize::try_from(SIG_LIMIT) else {
            return;
        };
        let source = format!("github:{}", self.slug);
        let Some(key) = SignedReleaseKey::asset(&source, &r.tag, name) else {
            return;
        };
        let _ = SignedArtifactCache::for_layout(layout).store_verified_pair(
            &key,
            bytes,
            sig,
            document_cap,
            signature_cap,
        );
    }

    fn fetch_pair(
        &mut self,
        r: &Release,
        name: &str,
        limit: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let sig_name = format!("{name}.sig");
        Ok((
            self.fetch(r, name, limit)?,
            self.fetch(r, &sig_name, SIG_LIMIT)?,
        ))
    }

    fn roster(&mut self, authority: &Release) -> Result<Roster, String> {
        const NAME: &str = "aterm-machines.toml";
        self.pair_is_listed(authority, NAME)?;
        if let Some((bytes, sig)) = self.cached_pair(authority, NAME, ROSTER_LIMIT)
            && let Ok(roster) = admitted_roster(self.masters, self.now_unix, bytes, &sig)
        {
            return Ok(roster);
        }
        let (bytes, sig) = self.fetch_pair(authority, NAME, ROSTER_LIMIT)?;
        let roster = admitted_roster(self.masters, self.now_unix, bytes.clone(), &sig)?;
        self.store_pair(authority, NAME, ROSTER_LIMIT, &bytes, &sig);
        Ok(roster)
    }

    fn release_appcast(
        &mut self,
        roster: &Roster,
        release: &Release,
        name: &str,
    ) -> Result<Manifest, String> {
        self.pair_is_listed(release, name)?;
        if let Some((bytes, sig)) = self.cached_pair(release, name, APPCAST_LIMIT)
            && let Ok(manifest) = self.appcast(roster, &bytes, &sig, &release.tag)
        {
            return Ok(manifest);
        }
        let (bytes, sig) = self.fetch_pair(release, name, APPCAST_LIMIT)?;
        let manifest = self.appcast(roster, &bytes, &sig, &release.tag)?;
        self.store_pair(release, name, APPCAST_LIMIT, &bytes, &sig);
        Ok(manifest)
    }

    fn appcast(
        &self,
        roster: &Roster,
        bytes: &[u8],
        sig: &[u8],
        tag: &str,
    ) -> Result<Manifest, String> {
        signed_release_appcast(self.slug, roster, bytes, sig, tag, self.now_unix)
    }
}

fn admitted_roster(
    masters: &[&str],
    now_unix: i64,
    bytes: Vec<u8>,
    sig: &[u8],
) -> Result<Roster, String> {
    let verified = verify_roster(masters, bytes, sig)
        .map_err(|e| format!("roster is not signed by the pinned paper master ({e:?})"))?;
    let roster = Roster::parse(&verified).map_err(|e| format!("roster refused ({e:?})"))?;
    roster
        .admit(0, now_unix)
        .map_err(|e| format!("roster refused ({e:?})"))?;
    Ok(roster)
}

/// `bytes`, the appcast of `slug`'s release `tag`, held under `roster` to the rules install.sh's
/// `verify_manifest` also holds one to before its floors: signed (`sig`) by a machine of the
/// roster live at `now_unix` and naming that machine, for `tag`'s version, from a full source
/// commit, naming that release's DMG as its origin, with a nonzero build. The manifest it signs.
///
/// # Errors
/// Each of those that does not hold, or bytes that are not a manifest.
pub fn signed_release_appcast(
    slug: &str,
    roster: &Roster,
    bytes: &[u8],
    sig: &[u8],
    tag: &str,
    now_unix: i64,
) -> Result<Manifest, String> {
    let signer = roster
        .authorize_appcast(bytes, sig, now_unix)
        .map_err(|e| format!("appcast has no authorized live machine signature ({e:?})"))?;
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(text)?;
    signer
        .bind(manifest.machine_id.as_deref(), manifest.roster_seq)
        .map_err(|e| format!("appcast signer attribution mismatch ({e:?})"))?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if manifest.version != version {
        return Err("release tag/appcast version mismatch".into());
    }
    let commit_ok = manifest
        .commit
        .as_deref()
        .is_some_and(|c| aterm_codec::hex::is_lower(c, 40));
    if !commit_ok {
        return Err("invalid source commit".into());
    }
    let dmg = super::releases::dmg_name(version);
    let url = format!("https://github.com/{slug}/releases/download/{tag}/{dmg}");
    if manifest.dmg != dmg || manifest.url.as_deref() != Some(url.as_str()) {
        return Err("signed release origin mismatch".into());
    }
    if manifest.build_number == 0 {
        return Err("zero release build".into());
    }
    Ok(manifest)
}

impl LinuxProver for Chain<'_> {
    fn prove(
        &mut self,
        release: &Release,
        arch: &'static str,
        authority: &Release,
    ) -> Result<LinuxDownload, String> {
        let target = match arch {
            "x86_64" => LinuxTarget::X86_64,
            "aarch64" => LinuxTarget::Aarch64,
            other => return Err(format!("unsupported Linux architecture {other}")),
        };
        let proof = linux_appcast_name(release)
            .ok_or("native release has no current appcast or exact archived pair")?;
        if linux_appcast_name(authority).as_deref() != Some("aterm-appcast.toml") {
            return Err("an archived appcast cannot supply current policy".into());
        }
        if release.version_key() > authority.version_key() {
            return Err("native release is newer than the current appcast head".into());
        }
        let roster = self.roster(authority)?;
        let current = self.release_appcast(&roster, authority, "aterm-appcast.toml")?;
        let archived;
        let manifest = if release.tag == authority.tag {
            &current
        } else {
            archived = self.release_appcast(&roster, release, &proof)?;
            &archived
        };
        let floor = current.min_build.unwrap_or(0);
        if manifest.build_number < floor.max(manifest.min_build.unwrap_or(0)) {
            return Err("release build rollback or below apply floor".into());
        }
        let artifact = manifest
            .linux_artifact(target)?
            .ok_or_else(|| format!("signed release has no Linux artifact for {arch}"))?;
        let listed = release.named(artifact.name).and_then(|a| a.size);
        if listed != Some(artifact.size) {
            return Err("signed Linux name/size does not match release inventory".into());
        }
        Ok(LinuxDownload {
            tag: release.tag.clone(),
            version: release.version().into(),
            label: artifact.name.into(),
            url: release.download_url(self.slug, artifact.name),
            kind: LinuxKind::SignedRaw {
                sha256: artifact.sha256.into(),
                size: artifact.size,
            },
        })
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
