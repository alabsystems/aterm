// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! [`AghHost`]: the release host over agh (docs/DESIGN-agh-2026-10-19.md §10.3). M1 is the
//! read half, used by tests only; production runs `RestHost` (init-system S15) until M2
//! swaps both halves in one commit.
//!
//! Three rules hold for every operation:
//! * a host serves one owner: a slug of another owner is refused before anything is sent,
//!   because the shared Router would otherwise route the call as that owner's identity;
//! * every draft-inclusive read is WriterRead, so the push check runs on each call;
//! * agh's errors never reach `?` bare: each goes through the one error map, which keeps a
//!   repository this identity cannot see ([`agh::Kind::NotVisible`]) from reading as absent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agh::download::{Expect, Sink};
use agh::github::TagObjects;
use agh::model::ReleaseObject;
use agh::page::ListOpts;
use agh::policy::{Presence, RefusalCode};
use agh::retry::RetryPolicy;
use agh::{How, Kind, Sub};

use super::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

/// A release host over one agh client. `home` is the slug this host serves; every operation
/// refuses a slug of another owner.
pub struct AghHost {
    github: agh::Github,
    home: agh::Slug,
    retry_pause: Duration,
    /// Every warning line written, for tests.
    #[cfg(any(test, debug_assertions))]
    warned: std::sync::Mutex<Vec<String>>,
}

impl AghHost {
    /// A host over `github` for `home`, pausing `super::RETRY_PAUSE` (5 s) between
    /// unanswered attempts, as `RestHost` does.
    #[must_use]
    pub fn new(github: agh::Github, home: agh::Slug) -> AghHost {
        AghHost {
            github,
            home,
            retry_pause: super::RETRY_PAUSE,
            #[cfg(any(test, debug_assertions))]
            warned: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Tests: another pause (the contract uses zero).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_retry_pause(self, pause: Duration) -> AghHost {
        AghHost {
            retry_pause: pause,
            ..self
        }
    }

    /// Tests: every warning line this host has written to stderr, in order.
    #[cfg(any(test, debug_assertions))]
    pub fn warnings(&self) -> Vec<String> {
        self.warned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// One warning line on stderr (kept for `warnings()` in a test or debug build).
    fn warn(&self, line: String) {
        let line = self.github.redactor().apply(&line);
        eprintln!("{line}");
        #[cfg(any(test, debug_assertions))]
        self.warned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line);
    }

    /// The text of an error this host builds, redacted.
    fn redacted(&self, text: &str) -> String {
        self.github.redactor().apply(text)
    }

    /// The error map (design §10.3). Every text passes the client's redactor.
    fn host_error(&self, e: &agh::Error) -> HostError {
        if e.sub == Sub::Policy(RefusalCode::WriterAbsent) {
            return HostError::Unreachable(self.redacted(&format!(
                "a draft-inclusive read needs the writer identity for {}, which is absent",
                self.home.owner()
            )));
        }
        let why = self.redacted(&e.why);
        match e.kind {
            Kind::NotFound => HostError::NotFound,
            Kind::Unauthenticated
            | Kind::Forbidden
            | Kind::Refused
            | Kind::Usage
            | Kind::Invalid
            | Kind::Conflict => HostError::Refused(why),
            Kind::NotVisible
            | Kind::CouldNotRun
            | Kind::RateLimited
            | Kind::Server
            | Kind::Incomplete
            | Kind::Pending
            | Kind::Failed
            | Kind::Interrupted
            | Kind::OutcomeUnknown => HostError::Unreachable(why),
        }
    }

    /// `slug` as agh's, once it is shown to be of this host's owner: nothing is sent for a
    /// slug of another owner, which the Router would route as that owner's identity.
    fn own(&self, slug: &Slug) -> Result<agh::Slug, String> {
        let s = agh::Slug::new(&slug.owner, &slug.repo).map_err(|e| self.redacted(&e.why))?;
        if s.owner().eq_ignore_ascii_case(self.home.owner()) {
            Ok(s)
        } else {
            Err(self.redacted(&format!(
                "this host serves {}; {slug} belongs to another owner",
                self.home
            )))
        }
    }

    /// [`Self::own`] for an operation answering a [`HostError`].
    fn own_or_refuse(&self, slug: &Slug) -> Result<agh::Slug, HostError> {
        self.own(slug).map_err(HostError::Refused)
    }

    /// The release `tag` names on `s` (design §10.3): exactly one published release is it,
    /// whatever drafts stand beside it (one warning line, never an error); with no published
    /// release, one draft is it and several are ambiguous; none is `NotFound`.
    ///
    /// A release the listing showed but its read by id no longer finds (deleted between the
    /// two reads, as a person removing a race loser's draft does), or whose tag changed in
    /// that window, is no longer beside the tag: the rules run over what remains, so it never
    /// turns a published release that is still there into `NotFound`.
    ///
    /// `how` is WriterRead, and every call of the resolution goes with it: `download` passes
    /// its own retry policy (none), so agh never retries inside one of the lane's attempts.
    fn resolve(&self, s: &agh::Slug, tag: &str, how: How) -> Result<ReleaseObject, HostError> {
        let releases = self.github.releases(s).how(how);
        let found = releases
            .objects_for_tag(tag)
            .map_err(|e| self.host_error(&e))?;
        let objects = match found {
            TagObjects::None => return Err(HostError::NotFound),
            TagObjects::One(release) => return Ok(*release),
            TagObjects::Many(objects) => objects,
        };
        // Each one read by id as the writer, so the push check runs on every call.
        let mut read = Vec::with_capacity(objects.len());
        for object in &objects {
            match releases.get(object.id) {
                Ok(release) if release.tag_name == tag => read.push(release),
                Ok(_) => {}
                Err(e) if e.kind == Kind::NotFound => {}
                Err(e) => return Err(self.host_error(&e)),
            }
        }
        let (mut published, mut drafts): (Vec<ReleaseObject>, Vec<ReleaseObject>) =
            read.into_iter().partition(|release| !release.draft);
        match (published.len(), drafts.len()) {
            (0, 0) => Err(HostError::NotFound),
            (1, n) => {
                if n > 0 {
                    self.warn(format!(
                        "atpkg-publish: {s} tag {tag} has {n} draft(s) beside its published release; the published one is used"
                    ));
                }
                Ok(published.remove(0))
            }
            (0, 1) => Ok(drafts.remove(0)),
            (0, n) => Err(HostError::Refused(
                self.redacted(&format!("ambiguous: {n} drafts have tag {tag}")),
            )),
            (n, _) => Err(HostError::Refused(self.redacted(&format!(
                "ambiguous: {n} published releases have tag {tag}"
            )))),
        }
    }

    /// `fetch_public` on a slug of this host's owner, before a `dest` it does not answer
    /// present is removed.
    fn fetch_public_own(
        &self,
        s: &agh::Slug,
        tag: &str,
        asset: &str,
        dest: &Path,
        cap: u64,
    ) -> Probe {
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Probe::Unknown(self.redacted(&format!("mkdir {}: {e}", parent.display())));
        }
        match self.github.public().fetch(s, tag, asset, dest, cap) {
            Ok(_) => Probe::Present,
            Err(e) if e.kind == Kind::NotFound => Probe::Absent,
            Err(e) => Probe::Unknown(self.redacted(&e.why)),
        }
    }
}

/// agh's release object as the trait's: its asset names in GitHub's order.
fn release_of(object: &ReleaseObject) -> Release {
    Release {
        tag: object.tag_name.clone(),
        draft: object.draft,
        prerelease: object.prerelease,
        assets: object.assets.iter().map(|a| a.name.clone()).collect(),
    }
}

impl ReleaseHost for AghHost {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        let s = self.own_or_refuse(slug)?;
        let listing = self
            .github
            .releases(&s)
            .how(How::writer())
            .list(&ListOpts::CAS)
            .and_then(agh::page::Listing::complete)
            .map_err(|e| self.host_error(&e))?;
        Ok(listing.items.iter().map(release_of).collect())
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        let s = self.own_or_refuse(slug)?;
        Ok(release_of(&self.resolve(&s, tag, How::writer())?))
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        let s = self.own_or_refuse(slug)?;
        // One attempt of the lane's (`channel::download_retried`) is one attempt of every call
        // it makes, the tag's resolution included: agh's own retry never compounds the lane's.
        let how = How::writer().retry_policy(RetryPolicy::NONE);
        let release = self.resolve(&s, tag, how.clone())?;
        std::fs::create_dir_all(dir).map_err(|e| {
            HostError::Refused(self.redacted(&format!("mkdir {}: {e}", dir.display())))
        })?;
        // Every name an asset's exact name, and one plain file name that is no part file's,
        // before any byte moves: a listed name is the reply's text, and `dir.join` of `../x`
        // or of an absolute name would write outside `dir`.
        let mut picked = Vec::with_capacity(assets.len());
        for name in assets {
            let asset = release
                .assets
                .iter()
                .find(|a| a.name == *name)
                .ok_or(HostError::NotFound)?;
            if !agh::download::plain_file_name(&asset.name)
                || agh::download::is_part_name(Path::new(&asset.name))
            {
                return Err(HostError::Refused(self.redacted(&format!(
                    "asset {:?} is not a plain file name that {} can hold",
                    asset.name,
                    dir.display()
                ))));
            }
            picked.push(asset);
        }
        let releases = self.github.releases(&s).how(how);
        for asset in picked {
            let expect = Expect::from_asset(asset).map_err(|e| self.host_error(&e))?;
            let sink = Sink::File {
                path: dir.join(&asset.name),
                clobber: true,
            };
            releases
                .download(asset, sink, expect)
                .map_err(|e| self.host_error(&e))?;
        }
        Ok(())
    }

    fn create(
        &self,
        slug: &Slug,
        _tag: &str,
        _title: &str,
        _notes: &str,
        _files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.own_or_refuse(slug)?;
        Err(HostError::Refused("agh writes arrive in M2".into()))
    }

    fn upload(
        &self,
        slug: &Slug,
        _tag: &str,
        _files: &[PathBuf],
        _replace: Replace,
    ) -> Result<(), HostError> {
        self.own_or_refuse(slug)?;
        Err(HostError::Refused("agh writes arrive in M2".into()))
    }

    fn publish_draft(&self, slug: &Slug, _tag: &str) -> Result<(), HostError> {
        self.own_or_refuse(slug)?;
        Err(HostError::Refused("agh writes arrive in M2".into()))
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        let s = match self.own(slug) {
            Ok(s) => s,
            Err(why) => return Probe::Unknown(why),
        };
        match self.github.public().probe(&s, tag, asset) {
            agh::github::Probe::Present => Probe::Present,
            agh::github::Probe::Absent => Probe::Absent,
            agh::github::Probe::Unknown(why) => Probe::Unknown(self.redacted(&why)),
        }
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        let answer = match self.own(slug) {
            Ok(s) => self.fetch_public_own(&s, tag, asset, dest, cap),
            Err(why) => Probe::Unknown(why),
        };
        // As the trait says and RestHost does: `dest` holds a file only when it is present (a
        // file an earlier call left, or one renamed into place before a later step failed),
        // whichever answer it is, another owner's refusal among them.
        if answer != Probe::Present {
            let _ = std::fs::remove_file(dest);
        }
        answer
    }

    fn clone_repo(&self, slug: &Slug, _dest: &Path) -> Result<(), HostError> {
        self.own_or_refuse(slug)?;
        Err(HostError::Refused("git through agh arrives in M2".into()))
    }

    fn retry_pause(&self) -> Duration {
        self.retry_pause
    }

    fn reads_anonymously(&self) -> bool {
        self.github
            .router()
            .writer_of(self.home.owner())
            .is_none_or(|profile| self.github.presence(profile) != Presence::Present)
    }

    fn fetch_repo(&self, slug: &Slug, _dir: &Path) -> Result<(), HostError> {
        self.own_or_refuse(slug)?;
        Err(HostError::Refused("git through agh arrives in M2".into()))
    }
}
