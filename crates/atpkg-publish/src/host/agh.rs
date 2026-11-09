// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! [`AghHost`]: the release host over agh (docs/DESIGN-agh-2026-10-19.md §10.3, §11.2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agh::download::{Expect, Sink};
use agh::git::GitRun;
use agh::github::TagObjects;
use agh::github::releases::{
    DraftRequest, MakeLatest, PublishGuard, PublishPatch, Replace as AghReplace, UploadFile,
};
use agh::model::ReleaseObject;
use agh::page::ListOpts;
use agh::policy::{
    Access, Decision, Named, Presence, RefusalCode, RouteRequest, RouteTarget, Tier,
};
use agh::retry::RetryPolicy;
use agh::{How, Kind, Outcome, Permit, Sub};

use super::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

/// The `[[writers]]` caller atpkg-publish's release-tier writes are granted to (design §10.2),
/// which every lane write and helper grant names.
pub(crate) const CALLER: &str = "atpkg-publish";

/// How long an authenticated lane clone or fetch through agh's isolated git may take: four hours,
/// the Linux unit's `TimeoutStartSec`, in place of agh's ten-minute `GIT_BUDGET`. Anonymous reads
/// instead run while transfer progress continues, with no whole-transfer latency budget; a
/// release-tier grant still lives at most `GRANT_MAX`.
pub const LANE_GIT_BUDGET: Duration = Duration::from_secs(4 * 60 * 60);

/// A release host over one agh client.
pub struct AghHost {
    github: agh::Github,
    home: agh::Slug,
    retry_pause: Duration,
    #[cfg(any(test, debug_assertions))]
    warned: std::sync::Mutex<Vec<String>>,
}

impl AghHost {
    /// A host over `github` for `home`, pausing `super::RETRY_PAUSE` between unanswered attempts.
    #[must_use]
    pub const fn new(github: agh::Github, home: agh::Slug) -> AghHost {
        AghHost {
            github,
            home,
            retry_pause: super::RETRY_PAUSE,
            #[cfg(any(test, debug_assertions))]
            warned: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The production host serving `home`, using agh's shared configured identity snapshot.
    ///
    /// # Errors
    ///
    /// Refusal of explicit configuration or adopted identity. Missing configuration adopts
    /// the vault's identities but grants no release-tier publication permission.
    pub fn production(dirs: &agh::config::Dirs, home: agh::Slug) -> Result<AghHost, String> {
        Self::configured_client(dirs, true).map(|client| AghHost::new(client, home))
    }

    /// One client snapshot shared by a run's staging and public hosts. `learn = false`
    /// reads only recorded identities, with no request or state change.
    pub(crate) fn configured_client(
        dirs: &agh::config::Dirs,
        learn: bool,
    ) -> Result<agh::Github, String> {
        let env = Arc::new(agh::identity::ProcessEnv);
        let base = agh::GithubBuilder::new(agh::policy::Router::empty())
            .transport(Arc::new(agh::wire::http::HttpTransport::new(env.clone())))
            .endpoints(agh::wire::Endpoints::github_com())
            .env(env)
            .fx(Arc::new(agh::Fx::live()))
            .retry(RetryPolicy::STANDARD)
            .caller(CALLER)
            .journal(dirs.clone())
            .rate_book(agh::rate::RateBook::at(dirs.rate()))
            .build();
        let resolved = if learn {
            agh::routing::adopt_for_host(&base, dirs)
        } else {
            agh::routing::view_for_host(dirs, base.env())
        };
        resolved
            .and_then(|resolved| resolved.bind(&base))
            .map_err(|error| {
                base.redactor()
                    .apply(&format!("agh identity routing refused: {}", error.why))
            })
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

    fn warn(&self, line: String) {
        let line = self.github.redactor().apply(&line);
        eprintln!("{line}");
        #[cfg(any(test, debug_assertions))]
        self.warned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line);
    }

    fn redacted(&self, text: &str) -> String {
        self.github.redactor().apply(text)
    }

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

    /// The error map of a write (design §10.3's table): `host_error`, with an open intent named
    /// as such, and the next step kept where it is the one that unblocks a lane (a guard that
    /// failed, a grant that is missing). A missing writer refuses the write by its owner.
    fn write_error(&self, e: &agh::Error) -> HostError {
        match &e.sub {
            Sub::IntentOpen { intent } => {
                return HostError::Unreachable(Self::outcome_unknown(intent));
            }
            Sub::GuardFailed { .. } | Sub::Policy(RefusalCode::NotGranted) => {
                // A guard that set no next of its own carries its kind's generic one, which
                // names agh's JSON fields; only a step of the error's own is worth the line.
                if e.next == e.kind.next_action() {
                    return HostError::Refused(self.redacted(&e.why));
                }
                return HostError::Refused(self.redacted(&format!("{}; next: {}", e.why, e.next)));
            }
            Sub::Policy(RefusalCode::WriterAbsent) => {
                return HostError::Refused(self.redacted(&format!(
                    "a write needs the writer identity for {}, which is absent: {}",
                    self.home.owner(),
                    e.why
                )));
            }
            _ => {}
        }
        self.host_error(e)
    }

    /// `Unreachable`'s why for an open intent.
    fn outcome_unknown(intent: &dyn std::fmt::Display) -> String {
        format!("outcome unknown; intent {intent} open")
    }

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

    fn own_or_refuse(&self, slug: &Slug) -> Result<agh::Slug, HostError> {
        self.own(slug).map_err(HostError::Refused)
    }

    /// How a write names `s` (design §10.2): a release-tier repository by the `[[writers]]`
    /// grant of atpkg-publish, which the router refuses `not_granted` without; any other by
    /// the library's default.
    fn named(&self, s: &agh::Slug) -> Result<Named, HostError> {
        let router = self.github.router();
        if router.tier_of(s.owner()) != Tier::Release {
            return Ok(Named::Default);
        }
        router
            .grant(CALLER, s)
            .map(Named::Literal)
            .map_err(|e| self.write_error(&e))
    }

    /// The one-owner check, then a write permit for `slug`, named as `named` says.
    fn permit(&self, slug: &Slug) -> Result<(agh::Slug, Permit), HostError> {
        let s = self.own_or_refuse(slug)?;
        let named = self.named(&s)?;
        let req = RouteRequest::write(RouteTarget::Repo(s.clone())).with_named(named);
        let permit = self.github.permit(&req).map_err(|e| self.write_error(&e))?;
        Ok((s, permit))
    }

    /// What a WriterRead of `tag` sees, in a bounded number of requests whatever the repository
    /// holds (`recent_objects_for_tag`, so a draft is seen: finding F11).
    fn tag_objects(&self, s: &agh::Slug, tag: &str, how: How) -> Result<TagObjects, HostError> {
        self.github
            .releases(s)
            .how(how)
            .recent_objects_for_tag(tag)
            .map_err(|e| self.host_error(&e))
    }

    /// M1's resolution of what `tag_objects` saw: each object read by id, the published
    /// release first, drafts beside it a warning.
    fn pick(
        &self,
        s: &agh::Slug,
        tag: &str,
        how: How,
        found: TagObjects,
    ) -> Result<ReleaseObject, HostError> {
        let objects = match found {
            TagObjects::None => return Err(HostError::NotFound),
            TagObjects::One(release) => return Ok(*release),
            TagObjects::Many(objects) => objects,
        };
        let releases = self.github.releases(s).how(how);
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

    fn resolve(&self, s: &agh::Slug, tag: &str, how: How) -> Result<ReleaseObject, HostError> {
        let found = self.tag_objects(s, tag, how.clone())?;
        self.pick(s, tag, how, found)
    }

    /// Each of `files` opened and hashed before anything is sent.
    fn open_files(&self, files: &[PathBuf]) -> Result<Vec<UploadFile>, HostError> {
        files
            .iter()
            .map(|path| {
                UploadFile::open(path, None).map_err(|e| HostError::Refused(self.redacted(&e.why)))
            })
            .collect()
    }

    /// Closes an open create intent for `tag` from what `release` just read (finding F4). An
    /// error is one warning line, never a `HostError`; a client with no journal has none open.
    fn observe(&self, s: &agh::Slug, tag: &str, seen: &TagObjects) {
        if self.github.dirs().is_none() {
            return;
        }
        let observed = self.github.journal(s).and_then(|journal| {
            self.github
                .releases(s)
                .observe_open(tag, seen, Access::WriterRead, &journal)
        });
        if let Err(e) = observed {
            self.warn(format!(
                "atpkg-publish: {s} tag {tag}: an open intent could not be observed: {}",
                e.why
            ));
        }
    }

    /// Body 2's published-`Done` rule: the release a lost answer's publish left holds every
    /// file with its sha256 as its digest, and nothing else, or `create` is refused by name.
    fn converged_published(
        &self,
        s: &agh::Slug,
        tag: &str,
        release: &ReleaseObject,
        files: &[UploadFile],
    ) -> Result<(), HostError> {
        let want: agh::github::releases::AssetSet = files
            .iter()
            .map(|file| (file.name.clone(), Some(file.sha256)))
            .collect();
        // An asset not yet uploaded, or whose digest is malformed, holds no file's bytes
        // (`AssetObject::holds`), so it is seen with no digest: changed, or added.
        let seen: agh::github::releases::AssetSet = release
            .assets
            .iter()
            .map(|asset| {
                let digest = asset.sha256().ok().flatten();
                (asset.name.clone(), digest.filter(|_| asset.is_uploaded()))
            })
            .collect();
        let change = agh::github::releases::asset_change(&want, &seen);
        if change.is_empty() {
            // The trait's create leaves a prerelease, and agh's publish guard converges only a
            // release published as its patch would leave it, so `publish_draft` refuses one
            // made a full release since; `create` refuses it too.
            if !release.prerelease {
                return Err(HostError::Refused(self.redacted(&format!(
                    "{tag} on {s} is published as a full release, not a prerelease"
                ))));
            }
            return Ok(());
        }
        let list = |names: &[String]| {
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        };
        Err(HostError::Refused(self.redacted(&format!(
            "{tag} on {s} is published with other files: added {}; missing {}; changed {}",
            list(&change.added),
            list(&change.missing),
            list(&change.changed)
        ))))
    }

    /// Before a clobber of `files`: every temporary asset an earlier clobber of the same name
    /// left with other bytes (`{name}.agh-{8 hex}` not of this file's digest, which agh's
    /// clobber neither renames nor deletes) is deleted, and so is the staged copy a cut
    /// `RestHost` replace of the same name left (`{name}.atpkg-staged`), as `RestHost` drops
    /// both before every clobber: during the swap both hosts write the same staging releases.
    /// A temporary of this file's own bytes is the clobber's to finish, and a release holding
    /// none sends nothing here. Left behind, either would be an asset the mirror refuses to
    /// republish.
    fn drop_stale_temporaries(
        &self,
        releases: &agh::github::releases::Releases<'_>,
        permit: &Permit,
        release: &ReleaseObject,
        files: &[UploadFile],
    ) -> Result<(), HostError> {
        for file in files {
            let ours = agh::github::releases::clobber_temporary(&file.name, &file.sha256);
            let staged = format!("{}{}", file.name, crate::host::rest::STAGED);
            let stale = release.assets.iter().filter(|asset| {
                asset.name == staged
                    || (asset.name != ours
                        && agh::github::releases::is_clobber_temporary_of(&asset.name, &file.name))
            });
            for asset in stale {
                releases
                    .delete_asset(permit, release, &asset.name)
                    .map_err(|e| self.write_error(&e))?;
            }
        }
        Ok(())
    }

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

fn release_of(object: &ReleaseObject) -> Release {
    Release {
        tag: object.tag_name.clone(),
        draft: object.draft,
        prerelease: object.prerelease,
        assets: object.assets.iter().map(|a| a.name.clone()).collect(),
    }
}

/// What every publish of atpkg-publish leaves: a prerelease, never `latest`.
const PRERELEASE: PublishPatch = PublishPatch {
    prerelease: true,
    make_latest: MakeLatest::False,
};

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
        let seen = self.tag_objects(&s, tag, How::writer())?;
        self.observe(&s, tag, &seen);
        Ok(release_of(&self.pick(&s, tag, How::writer(), seen)?))
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        let s = self.own_or_refuse(slug)?;
        let how = How::writer().retry_policy(RetryPolicy::STANDARD);
        let release = self.resolve(&s, tag, how.clone())?;
        std::fs::create_dir_all(dir).map_err(|e| {
            HostError::Refused(self.redacted(&format!("mkdir {}: {e}", dir.display())))
        })?;
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
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        let (s, permit) = self.permit(slug)?;
        let journal = self.github.journal(&s).map_err(|e| self.write_error(&e))?;
        let files = self.open_files(files)?;
        let req = DraftRequest {
            tag_name: tag.to_string(),
            target_commitish: None,
            name: title.to_string(),
            body: notes.to_string(),
            prerelease: true,
            make_latest: Some(MakeLatest::False),
        };
        let releases = self.github.releases(&s);
        let release = match releases.create(&permit, &req, &journal) {
            Ok(Outcome::Done(ensured)) => ensured.object,
            Ok(Outcome::NeverSent(e)) => return Err(self.write_error(&e)),
            // A rate limit settles as a refusal (nothing was created, so a rerun POSTs once
            // more), but it is `Unreachable` as on every other write (design §10.3, F14).
            Ok(Outcome::Refused(e)) if e.kind == Kind::RateLimited => {
                return Err(HostError::Unreachable(self.redacted(&e.why)));
            }
            Ok(Outcome::Refused(e)) => return Err(HostError::Refused(self.redacted(&e.why))),
            Ok(Outcome::Unknown { intent, .. }) => {
                return Err(HostError::Unreachable(Self::outcome_unknown(&intent)));
            }
            Err(e) => return Err(self.write_error(&e)),
        };
        if !release.draft {
            // The created-intent rule found the release this journal's draft was published
            // as: an earlier run's publish landed and its answer was lost. Nothing is uploaded
            // and no PATCH is sent: `ensure` runs its publish step here, whose guard converges
            // without one on a release published with the same files as a prerelease, and
            // refuses any other. So an immutable release converges, and one made a full
            // release since is refused.
            return self.converged_published(&s, tag, &release, &files);
        }
        for file in &files {
            releases
                .upload(&permit, &release, file, AghReplace::Never)
                .map_err(|e| self.write_error(&e))?;
        }
        let expected: BTreeMap<String, agh::model::Sha256Digest> = files
            .iter()
            .map(|file| (file.name.clone(), file.sha256))
            .collect();
        releases
            .publish(
                &permit,
                release.id,
                &PublishGuard::expecting(expected),
                &PRERELEASE,
            )
            .map_err(|e| self.write_error(&e))?;
        Ok(())
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        let (s, permit) = self.permit(slug)?;
        let release = self.resolve(&s, tag, How::writer())?;
        let files = self.open_files(files)?;
        let replace = match replace {
            Replace::Never => AghReplace::Never,
            Replace::Clobber => AghReplace::Clobber,
        };
        let releases = self.github.releases(&s);
        if replace == AghReplace::Clobber {
            self.drop_stale_temporaries(&releases, &permit, &release, &files)?;
        }
        for file in &files {
            releases
                .upload(&permit, &release, file, replace)
                .map_err(|e| self.write_error(&e))?;
        }
        Ok(())
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        let (s, permit) = self.permit(slug)?;
        let release = self.resolve(&s, tag, How::writer())?;
        self.github
            .releases(&s)
            .publish(&permit, release.id, &PublishGuard::default(), &PRERELEASE)
            .map_err(|e| self.write_error(&e))?;
        Ok(())
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
        match self.own(slug) {
            Ok(s) => self.fetch_public_own(&s, tag, asset, dest, cap),
            Err(why) => Probe::Unknown(why),
        }
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        let s = self.own_or_refuse(slug)?;
        let named = self.named(&s)?;
        self.github
            .run_git(
                &GitRun::new_clone(&s, dest)
                    .with_named(named)
                    .with_budget(LANE_GIT_BUDGET),
            )
            .map(|_| ())
            .map_err(|error| self.write_error(&error))
    }

    fn retry_pause(&self) -> Duration {
        self.retry_pause
    }

    fn download_owns_retry(&self) -> bool {
        true
    }

    fn reads_anonymously(&self) -> bool {
        self.github
            .router()
            .writer_of(self.home.owner())
            .is_none_or(|profile| self.github.presence(profile) != Presence::Present)
    }

    /// `GET /repos/{o}/{r}` through the read chain, then anonymously: `Ok(false)` only when
    /// both answer 404 (agh's `not_found` or `not_visible` for the repository itself), as
    /// `RestHost` asks it, so the D50 arm stages a pack the public owner cannot be listed for.
    fn repo_seen(&self, slug: &Slug) -> Result<bool, HostError> {
        let s = self.own_or_refuse(slug)?;
        let unseen = |e: &agh::Error| {
            matches!(e.kind, Kind::NotFound | Kind::NotVisible)
                && !matches!(e.sub, Sub::Moved { .. })
        };
        let route = RouteRequest::read(RouteTarget::Repo(s.clone()));
        let routed_anonymous = matches!(self.github.decide(&route), Decision::Anonymous { .. });
        let hows = if routed_anonymous {
            vec![How::read()]
        } else {
            vec![How::read(), How::read().anonymous(true)]
        };
        for how in hows {
            match self.github.repos().how(how).get(&s) {
                Ok(_) => return Ok(true),
                Err(e) if unseen(&e) => {}
                Err(e) => return Err(self.host_error(&e)),
            }
        }
        Ok(false)
    }

    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        let s = self.own_or_refuse(slug)?;
        let named = self.named(&s)?;
        self.github
            .run_git(
                &GitRun::fetch(&s, dir)
                    .with_named(named)
                    .with_budget(LANE_GIT_BUDGET),
            )
            .map_err(|e| self.write_error(&e))?;
        Ok(())
    }

    /// `GET /repos/{o}/{r}/git/ref/heads/main` for main's sha, then agh's isolated
    /// `GitOp::FetchMain` of `+refs/heads/main:{into}` into `dir`, then `into` held to that sha
    /// (`rev-parse --verify`), as `RestHost` does: on a mismatch `into` is deleted and the fetch
    /// is `Unreachable`, so only the API names the main of record.
    fn fetch_main(&self, slug: &Slug, dir: &Path, into: &str) -> Result<(), HostError> {
        let s = self.own_or_refuse(slug)?;
        let named = self.named(&s)?;
        let main = self.github.refs(&s).get("refs/heads/main").map_err(|e| {
            HostError::Unreachable(
                self.redacted(&format!("read {s}'s main from the host API: {}", e.why)),
            )
        })?;
        let expected = main.object.sha;
        self.github
            .run_git(
                &GitRun::fetch_main(&s, dir, into)
                    .with_named(named)
                    .with_budget(LANE_GIT_BUDGET),
            )
            .map_err(|e| match self.write_error(&e) {
                HostError::Unreachable(why) => {
                    HostError::Unreachable(format!("fetch of {s}'s main: {why}"))
                }
                other => other,
            })?;
        let fetched = crate::git::run(dir, &["rev-parse", "--verify", into]);
        let checked = match fetched {
            Ok(actual) if actual.trim() == expected => return Ok(()),
            Ok(actual) => format!(
                "fetched {s}'s main ({}) differs from the host API ({expected}); main moved or \
                 git transported another main",
                actual.trim()
            ),
            Err(e) => format!("{into} cannot be read after the fetch: {e}"),
        };
        let dropped = crate::git::run(dir, &["update-ref", "-d", into]);
        Err(HostError::Unreachable(self.redacted(&match dropped {
            Ok(_) => format!("could not bind {s}'s main of record: {checked}"),
            Err(drop) => format!(
                "could not bind {s}'s main of record: {checked}; removing {into} failed: {drop}"
            ),
        })))
    }

    fn published_releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        let s = self.own_or_refuse(slug)?;
        let listing = self
            .github
            .releases(&s)
            .how(How::read())
            .list(&ListOpts::CAS)
            .and_then(agh::page::Listing::complete)
            .map_err(|e| self.host_error(&e))?;
        Ok(listing
            .items
            .iter()
            .filter(|release| !release.draft)
            .map(release_of)
            .collect())
    }

    fn published_by_tag(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        let s = self.own_or_refuse(slug)?;
        let release = self
            .github
            .releases(&s)
            .how(How::read())
            .published_by_tag(tag)
            .map_err(|e| self.host_error(&e))?;
        if release.draft {
            return Err(HostError::NotFound);
        }
        Ok(release_of(&release))
    }

    fn write_preflight(&self, slug: &Slug) -> Result<(), HostError> {
        let (s, permit) = self.permit(slug)?;
        // The write journal is the first local thing `create` needs: a state directory
        // `Journal::open` refuses is named here, before a build, not at the create after it.
        // It sends nothing; it makes or checks `<state>/journal/<owner>`.
        self.github.journal(&s).map_err(|e| self.write_error(&e))?;
        let (_, permissions) = self
            .github
            .repos()
            .permissions(&s, Access::WriterRead)
            .map_err(|e| self.write_error(&e))?;
        if permissions.push {
            return Ok(());
        }
        let writer = self
            .github
            .router()
            .profile(permit.profile())
            .and_then(|profile| profile.login.clone())
            .unwrap_or_else(|| permit.profile().to_string());
        Err(HostError::Refused(
            self.redacted(&format!("{writer} has no push on {s}")),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::AghHost;

    fn home() -> agh::Slug {
        agh::Slug::new("fixture-dev", "packs").expect("the home slug")
    }

    /// `dirs` under a fresh scratch root, with `text` as its agh.toml when one is given.
    fn production(text: Option<&str>) -> Result<AghHost, String> {
        let root = aterm_tempfile::Builder::new()
            .prefix("atpkg-publish-agh-production-")
            .tempdir()
            .expect("scratch dir");
        let dirs = agh::config::Dirs::under(root.path());
        if let Some(text) = text {
            std::fs::create_dir_all(dirs.config.parent().expect("a parent"))
                .expect("the config parent");
            aterm_uds::private_file::create_private_dir(&dirs.config)
                .expect("the config directory");
            aterm_uds::private_file::write_private(&dirs.config, "agh.toml", text.as_bytes())
                .expect("agh.toml");
        }
        AghHost::production(&dirs, home())
    }

    fn refusal(text: Option<&str>) -> String {
        match production(text) {
            Ok(_) => panic!("{text:?} built a host"),
            Err(why) => why,
        }
    }

    #[test]
    fn production_allows_missing_or_vault_config_but_refuses_invalid_config() {
        assert!(production(None).is_ok());
        assert!(production(Some("vault = true\n")).is_ok());
        let invalid = refusal(Some("[profiles.x]\ntier = \"dev\"\nbogus = true\n"));
        assert!(invalid.contains("L1:"), "{invalid}");
        assert!(invalid.contains("bogus"), "{invalid}");
    }

    #[test]
    fn production_builds_over_a_file_that_names_its_writers() {
        let text = "[profiles.dev]\ntoken_file = \"/nonexistent/dev.token\"\ntier = \"dev\"\n\
                    login = \"fixture-dev-bot\"\n\n[[owners]]\nname = \"fixture-dev\"\n\
                    tier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n";
        if let Err(why) = production(Some(text)) {
            panic!("{why}");
        }
    }
}
