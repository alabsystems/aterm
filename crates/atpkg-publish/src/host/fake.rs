// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`FakeHost`]: an in-memory [`ReleaseHost`] for the producer's tests, with the failure
//! modes seen in the field: a draft (invisible to the download host), a one-shot HTTP 500
//! on one asset, an unreachable owner, a private repo the download host serves nothing
//! for, and a create that races another publisher. Every call is logged, so a test can
//! say "a dry run made no host call".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

/// One release's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FakeRelease {
    /// A draft: the download host answers 404 for its assets.
    pub draft: bool,
    /// A prerelease.
    pub prerelease: bool,
    /// Asset name -> bytes.
    pub assets: BTreeMap<String, Vec<u8>>,
}

#[derive(Default)]
struct State {
    repos: BTreeMap<Slug, BTreeMap<String, FakeRelease>>,
    clone_sources: BTreeMap<Slug, PathBuf>,
    fail_once: Vec<(Slug, String, String)>,
    unreachable_owners: BTreeSet<String>,
    private: BTreeSet<Slug>,
    anonymous: bool,
    calls: Vec<String>,
}

/// The in-memory release host.
#[derive(Default)]
pub struct FakeHost {
    state: Mutex<State>,
}

impl FakeHost {
    /// An empty host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lay release `tag` on `slug` as given.
    pub fn put(&self, slug: &Slug, tag: &str, release: FakeRelease) {
        self.state()
            .repos
            .entry(slug.clone())
            .or_default()
            .insert(tag.to_string(), release);
    }

    /// Lay one asset, creating a published prerelease `tag` when there is none.
    pub fn put_asset(&self, slug: &Slug, tag: &str, name: &str, bytes: impl Into<Vec<u8>>) {
        self.state()
            .repos
            .entry(slug.clone())
            .or_default()
            .entry(tag.to_string())
            .or_insert_with(|| FakeRelease {
                prerelease: true,
                ..FakeRelease::default()
            })
            .assets
            .insert(name.to_string(), bytes.into());
    }

    /// Mark `tag` a draft (or not).
    pub fn set_draft(&self, slug: &Slug, tag: &str, draft: bool) {
        if let Some(r) = self
            .state()
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
        {
            r.draft = draft;
        }
    }

    /// The release as it stands, if any.
    #[must_use]
    pub fn release_of(&self, slug: &Slug, tag: &str) -> Option<FakeRelease> {
        self.state()
            .repos
            .get(slug)
            .and_then(|tags| tags.get(tag))
            .cloned()
    }

    /// One asset's bytes, if present.
    #[must_use]
    pub fn asset(&self, slug: &Slug, tag: &str, name: &str) -> Option<Vec<u8>> {
        self.release_of(slug, tag)
            .and_then(|r| r.assets.get(name).cloned())
    }

    /// The next download of this one asset fails as unreachable (an HTTP 500), once.
    pub fn fail_once(&self, slug: &Slug, tag: &str, asset: &str) {
        self.state()
            .fail_once
            .push((slug.clone(), tag.to_string(), asset.to_string()));
    }

    /// Every authenticated call on `owner`'s repos answers unreachable (or stops).
    pub fn set_unreachable(&self, owner: &str, unreachable: bool) {
        let mut s = self.state();
        if unreachable {
            s.unreachable_owners.insert(owner.to_string());
        } else {
            s.unreachable_owners.remove(owner);
        }
    }

    /// The download host serves nothing for `slug` (a private repo), or serves it again.
    pub fn set_private(&self, slug: &Slug, private: bool) {
        let mut s = self.state();
        if private {
            s.private.insert(slug.clone());
        } else {
            s.private.remove(slug);
        }
    }

    /// A keyless builder: no token and no `gh` login, so every authenticated read
    /// (`releases`, `release`, `download`) is unreachable and only the anonymous download
    /// host answers ([`ReleaseHost::reads_anonymously`]).
    pub fn set_anonymous_reads(&self, anonymous: bool) {
        self.state().anonymous = anonymous;
    }

    /// `clone_repo(slug)` clones this local repository.
    pub fn add_clone_source(&self, slug: &Slug, path: &Path) {
        self.state()
            .clone_sources
            .insert(slug.clone(), path.to_path_buf());
    }

    /// Every call so far, one line each (`<op> <owner>/<repo> [<tag> [<assets>]]`).
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    fn log(&self, line: String) {
        self.state().calls.push(line);
    }

    fn reachable(&self, slug: &Slug) -> Result<(), HostError> {
        if self.state().anonymous {
            return Err(HostError::Unreachable(
                "gh has no login and there is no token file on this keyless builder".into(),
            ));
        }
        if self.state().unreachable_owners.contains(&slug.owner) {
            Err(HostError::Unreachable(format!(
                "{} did not answer (HTTP 500)",
                slug.owner
            )))
        } else {
            Ok(())
        }
    }

    fn file_name(path: &Path) -> Result<String, HostError> {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| HostError::Refused(format!("{} names no file", path.display())))
    }

    fn read_files(files: &[PathBuf]) -> Result<Vec<(String, Vec<u8>)>, HostError> {
        files
            .iter()
            .map(|f| {
                let bytes = std::fs::read(f)
                    .map_err(|e| HostError::Refused(format!("read {}: {e}", f.display())))?;
                Ok((Self::file_name(f)?, bytes))
            })
            .collect()
    }

    fn public(&self, slug: &Slug, tag: &str, asset: &str) -> Option<Vec<u8>> {
        let s = self.state();
        if s.private.contains(slug) {
            return None;
        }
        s.repos
            .get(slug)
            .and_then(|tags| tags.get(tag))
            .filter(|r| !r.draft)
            .and_then(|r| r.assets.get(asset).cloned())
    }
}

impl ReleaseHost for FakeHost {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        self.log(format!("releases {slug}"));
        self.reachable(slug)?;
        let s = self.state();
        let tags = s.repos.get(slug).ok_or(HostError::NotFound)?;
        Ok(tags
            .iter()
            .map(|(tag, r)| Release {
                tag: tag.clone(),
                draft: r.draft,
                prerelease: r.prerelease,
                assets: r.assets.keys().cloned().collect(),
            })
            .collect())
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        self.log(format!("release {slug} {tag}"));
        self.reachable(slug)?;
        let r = self.release_of(slug, tag).ok_or(HostError::NotFound)?;
        Ok(Release {
            tag: tag.to_string(),
            draft: r.draft,
            prerelease: r.prerelease,
            assets: r.assets.keys().cloned().collect(),
        })
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        self.log(format!("download {slug} {tag} {}", assets.join(" ")));
        self.reachable(slug)?;
        let release = self.release_of(slug, tag).ok_or(HostError::NotFound)?;
        std::fs::create_dir_all(dir)
            .map_err(|e| HostError::Refused(format!("mkdir {}: {e}", dir.display())))?;
        for name in assets {
            let mut s = self.state();
            let key = (slug.clone(), tag.to_string(), (*name).to_string());
            if let Some(at) = s.fail_once.iter().position(|k| *k == key) {
                s.fail_once.remove(at);
                return Err(HostError::Unreachable(format!("HTTP 500 on {name}")));
            }
            drop(s);
            let bytes = release.assets.get(*name).ok_or(HostError::NotFound)?;
            std::fs::write(dir.join(name), bytes)
                .map_err(|e| HostError::Refused(format!("write {name}: {e}")))?;
        }
        Ok(())
    }

    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        _title: &str,
        _notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.log(format!("create {slug} {tag}"));
        self.reachable(slug)?;
        if self.release_of(slug, tag).is_some() {
            return Err(HostError::Refused(format!(
                "{slug} already has a release {tag}"
            )));
        }
        let assets = Self::read_files(files)?.into_iter().collect();
        self.put(
            slug,
            tag,
            FakeRelease {
                draft: false,
                prerelease: true,
                assets,
            },
        );
        Ok(())
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        self.log(format!("upload {slug} {tag} {replace:?}"));
        self.reachable(slug)?;
        let incoming = Self::read_files(files)?;
        let mut s = self.state();
        let release = s
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
            .ok_or(HostError::NotFound)?;
        if replace == Replace::Never
            && let Some((name, _)) = incoming
                .iter()
                .find(|(n, _)| release.assets.contains_key(n))
        {
            return Err(HostError::Refused(format!(
                "{name} already exists on {slug} {tag}"
            )));
        }
        release.assets.extend(incoming);
        Ok(())
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        self.log(format!("publish-draft {slug} {tag}"));
        self.reachable(slug)?;
        if self.release_of(slug, tag).is_none() {
            return Err(HostError::NotFound);
        }
        self.set_draft(slug, tag, false);
        Ok(())
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        self.log(format!("probe {slug} {tag} {asset}"));
        if self.public(slug, tag, asset).is_some() {
            Probe::Present
        } else {
            Probe::Absent
        }
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        self.log(format!("fetch {slug} {tag} {asset}"));
        let Some(bytes) = self.public(slug, tag, asset) else {
            return Probe::Absent;
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
            return Probe::Unknown(format!("{asset} is over the {cap}-byte cap"));
        }
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Probe::Unknown(format!("mkdir {}: {e}", parent.display()));
        }
        match std::fs::write(dest, bytes) {
            Ok(()) => Probe::Present,
            Err(e) => Probe::Unknown(format!("write {}: {e}", dest.display())),
        }
    }

    fn reads_anonymously(&self) -> bool {
        self.state().anonymous
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        self.log(format!("clone {slug}"));
        self.reachable(slug)?;
        let source = self.clone_source(slug)?;
        git(aterm_uds::spawnfd::command("git")
            .arg("clone")
            .arg("--quiet")
            .arg(&source)
            .arg(dest))
    }

    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        self.log(format!("fetch-repo {slug}"));
        self.reachable(slug)?;
        let source = self.clone_source(slug)?;
        git(aterm_uds::spawnfd::command("git")
            .arg("-C")
            .arg(dir)
            .args(["fetch", "--quiet", "--tags", "--force"])
            .arg(&source)
            .arg("+refs/heads/*:refs/remotes/origin/*"))
    }
}

impl FakeHost {
    fn clone_source(&self, slug: &Slug) -> Result<PathBuf, HostError> {
        self.state()
            .clone_sources
            .get(slug)
            .cloned()
            .ok_or(HostError::NotFound)
    }
}

fn git(cmd: &mut aterm_uds::spawnfd::Command) -> Result<(), HostError> {
    let out = cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| HostError::Unreachable(format!("spawn git: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(HostError::Refused(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}
