// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Repository release listing, lookup, and asset download operations.

use crate::client::{Accept, Answered, Call, Github};
use crate::download::{self, Downloaded, Expect, Sink};
use crate::error::{Error, Forbid, Kind, Result, Sub};
use crate::github::{How, limit_after_sort, repo_root};
use crate::journal::{Intent, IntentId, IntentStore, Settled};
use crate::model::{AssetObject, ReleaseObject, Sha256Digest, from_json};
use crate::page::{LinkScope, ListOpts, Listing};
use crate::path;
use crate::policy::{Access, Permit, RouteTarget};
use crate::slug::Slug;
use crate::write::{Deleted, Ensured, Outcome};

/// Returns `/repos/{o}/{r}/releases/tags/{tag}` with `/` in `tag` preserved.
pub(crate) fn tag_path(slug: &Slug, tag: &str) -> String {
    format!("{}/releases/tags/{}", repo_root(slug), path::ref_path(tag))
}

/// What a one-release read's NotFound says.
#[derive(Clone, Copy)]
enum Absent<'s> {
    /// The thing read (`release {id}`), named in the push check's NotFound; any other
    /// NotFound keeps the client's why.
    Named(&'s str),
    /// This why for every NotFound, the push check's included.
    Why(&'s str),
}

/// The releases of one repository.
pub struct Releases<'a> {
    gh: &'a Github,
    slug: Slug,
    how: How,
}

/// What `objects_for_tag` found.
#[derive(Clone, Debug, PartialEq)]
pub enum TagObjects {
    /// No release has the tag.
    None,
    /// One release has it (boxed, so the enum stays small).
    One(Box<ReleaseObject>),
    /// Several have it (drafts included).
    Many(Vec<ReleaseObject>),
}

impl<'a> Releases<'a> {
    /// `How::read()` over `slug`.
    pub(crate) fn new(gh: &'a Github, slug: &Slug) -> Releases<'a> {
        Releases {
            gh,
            slug: slug.clone(),
            how: How::read(),
        }
    }

    /// The same releases under `how`.
    #[must_use]
    pub fn how(self, how: How) -> Releases<'a> {
        Releases { how, ..self }
    }

    /// Whether this resource reads as the writer.
    fn writer(&self) -> bool {
        self.how.access == Access::WriterRead
    }

    /// The push check (plan §4.6.20): under WriterRead, `GET /repos/{o}/{r}` as the writer, which
    /// must hold `push`.
    fn push_check(&self) -> Result<()> {
        if !self.writer() {
            return Ok(());
        }
        let (_, permissions) = self
            .gh
            .repos()
            .how(self.how.clone())
            .permissions(&self.slug, Access::WriterRead)?;
        if permissions.push {
            return Ok(());
        }
        let meter = self.gh.meter();
        let label = meter.label();
        Err(Error::new(
            Kind::Forbidden,
            format!("as {label}: push is not granted, and drafts are visible only to push"),
        )
        .with_sub(Sub::Forbid(Forbid::NoPush))
        .with_repo(self.slug.to_string()))
    }

    /// A 404 after the push check passed: the identity has push, so the thing is absent, not
    /// hidden.
    fn absent_after_check(&self, what: &str, error: Error) -> Error {
        self.absent_as(error, || {
            format!(
                "{what} is not found (the push check saw {} with push, so it is absent, not \
                 hidden)",
                self.slug
            )
        })
    }

    /// [`Self::absent_after_check`]'s recast, with `why()` as the NotFound's why.
    fn absent_as(&self, error: Error, why: impl FnOnce() -> String) -> Error {
        if self.writer()
            && error.kind == Kind::NotVisible
            && error.http.as_ref().is_some_and(|http| http.status == 404)
        {
            return error
                .recast(Kind::NotFound, why())
                .with_next(Kind::NotFound.next_action());
        }
        error
    }

    /// One release read from `path`; `absent` says what its NotFound says.
    fn release_at(&self, absent: Absent<'_>, path: String) -> Result<ReleaseObject> {
        let call = self.how.get(path, RouteTarget::Repo(self.slug.clone()));
        let answered = self.gh.call(&call).map_err(|e| match absent {
            Absent::Named(what) => self.absent_after_check(what, e),
            Absent::Why(why) => self
                .absent_as(e, || why.to_string())
                .why_if_not_found(|| why.to_string()),
        })?;
        answered
            .json()
            .and_then(|value| from_json(&value))
            .map_err(|e| e.with_repo(self.slug.to_string()))
    }

    /// `/repos/{o}/{r}/releases`.
    fn releases_path(&self) -> String {
        format!("{}/releases", repo_root(&self.slug))
    }

    /// Lists releases newest first by `(created_at, id)`, dropping drafts under Read.
    ///
    /// # Errors
    ///
    /// The walk's error, or the push check's under WriterRead.
    pub fn list(&self, opts: &ListOpts) -> Result<Listing<ReleaseObject>> {
        self.push_check()?;
        self.list_checked(opts)
    }

    /// `list`, once the push check has run.
    fn list_checked(&self, opts: &ListOpts) -> Result<Listing<ReleaseObject>> {
        let first = self
            .how
            .get(self.releases_path(), RouteTarget::Repo(self.slug.clone()));
        let walked = crate::page::walk(
            self.gh,
            first,
            &opts.with_limit(None),
            &LinkScope::Repo(self.slug.clone()),
        )?;
        let mut listing = walked
            .map(|value| from_json::<ReleaseObject>(&value))
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        if !self.writer() {
            listing.items.retain(|release| !release.draft);
        }
        listing
            .items
            .sort_by(|a, b| (&b.created_at, b.id).cmp(&(&a.created_at, a.id)));
        limit_after_sort(&mut listing, opts.limit);
        Ok(listing)
    }

    /// The release with this id. Under Read a draft is NotFound.
    ///
    /// # Errors
    ///
    /// NotFound; Forbidden `no_push` under WriterRead.
    pub fn get(&self, id: u64) -> Result<ReleaseObject> {
        self.push_check()?;
        let release = self.release_at(
            Absent::Named(&format!("release {id}")),
            format!("{}/{id}", self.releases_path()),
        )?;
        if release.draft && !self.writer() {
            return Err(Error::new(
                Kind::NotFound,
                format!(
                    "release {id} is a draft; drafts are read as the writer (--include-drafts)"
                ),
            )
            .with_repo(self.slug.to_string()));
        }
        Ok(release)
    }

    /// The published release with this tag.
    ///
    /// # Errors
    ///
    /// Usage if `path::bad_tag` refuses `tag`; NotFound when no published release matches.
    pub fn published_by_tag(&self, tag: &str) -> Result<ReleaseObject> {
        path::refuse_bad_tag(tag).map_err(|e| e.with_repo(self.slug.to_string()))?;
        self.push_check()?;
        let path = tag_path(&self.slug, tag);
        let why = format!(
            "no published release has tag {tag}; drafts are seen only with --include-drafts, \
             as the writer"
        );
        self.release_at(Absent::Why(&why), path)
    }

    /// Returns all releases with `tag` (including drafts) read under WriterRead.
    ///
    /// # Errors
    ///
    /// `list`'s errors.
    pub fn objects_for_tag(&self, tag: &str) -> Result<TagObjects> {
        let writer = Releases {
            gh: self.gh,
            slug: self.slug.clone(),
            how: self.how.with_access(Access::WriterRead),
        };
        let mut found = writer.list(&ListOpts::CAS)?.items;
        found.retain(|release| release.tag_name == tag);
        Ok(match found.len() {
            0 => TagObjects::None,
            1 => TagObjects::One(Box::new(found.remove(0))),
            _ => TagObjects::Many(found),
        })
    }

    /// The latest release: GitHub's own pointer.
    ///
    /// # Errors
    ///
    /// NotFound when there is none.
    pub fn latest(&self) -> Result<ReleaseObject> {
        self.push_check()?;
        self.latest_checked()
    }

    /// `latest`, once the push check has run.
    fn latest_checked(&self) -> Result<ReleaseObject> {
        self.release_at(
            Absent::Why("the repository has no latest release"),
            format!("{}/latest", self.releases_path()),
        )
    }

    /// The latest release's id, or `None`.
    ///
    /// # Errors
    ///
    /// `latest`'s errors but NotFound.
    pub fn latest_id(&self) -> Result<Option<u64>> {
        self.push_check()?;
        self.latest_id_checked()
    }

    /// Returns `latest_id` without a second push check.
    ///
    /// # Errors
    ///
    /// `latest`'s errors but NotFound.
    pub(crate) fn latest_id_checked(&self) -> Result<Option<u64>> {
        match self.latest_checked() {
            Ok(release) => Ok(Some(release.id)),
            Err(error) if error.kind == Kind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Returns the newest published release matching the filters.
    ///
    /// # Errors
    ///
    /// `list`'s errors.
    pub fn newest(
        &self,
        prerelease: Option<bool>,
        tag_regex: Option<&aterm_regex::Regex>,
    ) -> Result<Option<ReleaseObject>> {
        let reader = Releases {
            gh: self.gh,
            slug: self.slug.clone(),
            how: self.how.as_read(),
        };
        let listing = reader.list(&ListOpts::STANDARD)?;
        Ok(listing
            .items
            .into_iter()
            .filter(|release| !release.draft)
            .filter(|release| prerelease.is_none_or(|want| release.prerelease == want))
            .filter(|release| tag_regex.is_none_or(|regex| regex.is_match(&release.tag_name)))
            .max_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id))))
    }

    /// The asset with this id.
    ///
    /// # Errors
    ///
    /// NotFound; Forbidden `no_push` under WriterRead.
    pub fn asset(&self, id: u64) -> Result<AssetObject> {
        self.push_check()?;
        asset_object(self.gh, &self.slug, &self.how, id)
            .map_err(|e| self.absent_after_check(&format!("asset {id}"), e))
    }

    /// Download `asset` into `sink`, checked against `expect` (`download::fetch`).
    ///
    /// # Errors
    ///
    /// `download::fetch`'s errors, or the push check's under WriterRead.
    pub fn download(
        &self,
        asset: &AssetObject,
        sink: Sink<'_>,
        expect: Expect,
    ) -> Result<Downloaded> {
        self.push_check()?;
        download::fetch(self.gh, &self.slug, &self.how, asset, sink, expect)
            .map_err(|e| self.absent_after_check(&format!("asset {}", asset.id), e))
    }
}

/// Fetches `/repos/{o}/{r}/releases/assets/{id}` with Accept Json and no push check.
pub(crate) fn asset_object(gh: &Github, slug: &Slug, how: &How, id: u64) -> Result<AssetObject> {
    asset_object_counted(gh, slug, how, id).map(|(asset, _)| asset)
}

/// Fetches `asset_object` along with its total request count.
pub(crate) fn asset_object_counted(
    gh: &Github,
    slug: &Slug,
    how: &How,
    id: u64,
) -> Result<(AssetObject, u32)> {
    let call = how
        .get(
            format!("{}/releases/assets/{id}", repo_root(slug)),
            RouteTarget::Repo(slug.clone()),
        )
        .accept(Accept::Json);
    let answered = gh.call(&call)?;
    let asset = answered
        .json()
        .and_then(|value| from_json(&value))
        .map_err(|e| e.with_repo(slug.to_string()))?;
    Ok((asset, answered.requests))
}

/// `make_latest` (design §11.2): the string enum GitHub takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MakeLatest {
    /// `"true"`.
    True,
    /// `"false"`: what agh's verbs always send.
    False,
    /// `"legacy"`.
    Legacy,
}

impl MakeLatest {
    /// `true`, `false`, `legacy`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MakeLatest::True => "true",
            MakeLatest::False => "false",
            MakeLatest::Legacy => "legacy",
        }
    }
}

/// A draft to create (design §10.2, as written there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRequest {
    /// The tag.
    pub tag_name: String,
    /// `None`: not sent, so GitHub uses the default branch, as aterm-release's curl lane does.
    pub target_commitish: Option<String>,
    /// The release's title.
    pub name: String,
    /// The notes, at most 125,000 characters.
    pub body: String,
    /// The prerelease flag.
    pub prerelease: bool,
    /// `None`: not sent. agh's own verbs always send `"false"`; a draft is never latest, and
    /// the publish PATCH always sends it.
    pub make_latest: Option<MakeLatest>,
}

impl DraftRequest {
    /// A draft of `tag`: titled `tag`, no notes, no target, not a prerelease,
    /// `make_latest: Some(MakeLatest::False)`.
    #[must_use]
    pub fn new(tag: &str) -> DraftRequest {
        DraftRequest {
            tag_name: tag.to_string(),
            target_commitish: None,
            name: tag.to_string(),
            body: String::new(),
            prerelease: false,
            make_latest: Some(MakeLatest::False),
        }
    }
}

/// One file to upload, hashed once when it is opened.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UploadFile {
    /// The file.
    pub path: std::path::PathBuf,
    /// The asset's name.
    pub name: String,
    /// `Content-Type`: `application/octet-stream`.
    pub content_type: String,
    /// Its length when hashed. The upload refuses a file that has changed since.
    pub len: u64,
    /// Its sha256, read once through `aterm_digest::Sha256`.
    pub sha256: Sha256Digest,
}

impl UploadFile {
    /// `path` as asset `name` (default: its file name), validated as design §11.2 says:
    /// - at most 2 GiB;
    /// - a name of 1 to 255 bytes in `[A-Za-z0-9._+-]`, not starting with `.`, holding no `..`
    ///   (GitHub renames anything else, and agh refuses rather than be renamed).
    ///
    /// # Errors
    ///
    /// Invalid (exit 10, design §7.1's client-side limits), why `{path}: {what}`, naming the
    /// rule; could_not_run for a read that failed.
    pub fn open(path: &std::path::Path, name: Option<&str>) -> Result<UploadFile> {
        let _ = (path, name);
        Err(Error::not_yet("the upload file", "WP5"))
    }
}

/// Whether an upload may replace an asset with the same name (design §11.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replace {
    /// Never: an asset with the name and the same digest converges; another digest is
    /// Conflict `AlreadyExists`.
    Never,
    /// Replace it, in clobber's three steps. The same digest is `Unchanged`, with no write.
    /// An old asset in state `starter` or `open` is replaced too, though it may be another
    /// uploader's upload in progress, which `Never` refuses to delete: that upload then
    /// fails. `--clobber` is the caller's word that any asset of the name may go, and it is
    /// what clears a cut upload's asset without a person.
    Clobber,
}

/// What the publish guard checks before every attempt (design §11.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PublishGuard {
    /// The assets the draft must hold, by name and digest. `None`: the set the first guard
    /// read saw, which must not change before the PATCH.
    pub assets: Option<std::collections::BTreeMap<String, Sha256Digest>>,
}

impl PublishGuard {
    /// A guard expecting exactly these assets.
    #[must_use]
    pub fn expecting(assets: std::collections::BTreeMap<String, Sha256Digest>) -> PublishGuard {
        PublishGuard {
            assets: Some(assets),
        }
    }
}

/// The publish PATCH's body: `{draft:false, prerelease, make_latest}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishPatch {
    /// The prerelease flag to publish with.
    pub prerelease: bool,
    /// `make_latest`, always sent.
    pub make_latest: MakeLatest,
}

/// What `release edit` changes. `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleaseEdit {
    /// The title.
    pub name: Option<String>,
    /// The notes.
    pub body: Option<String>,
    /// The prerelease flag.
    pub prerelease: Option<bool>,
}

/// What `release ensure` makes true.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnsureSpec {
    /// The draft, created or converged on.
    pub draft: DraftRequest,
    /// The files, each uploaded with `Replace::Never`.
    pub assets: Vec<UploadFile>,
    /// Publish once every asset is there, guarded by exactly those assets.
    pub publish: Option<PublishPatch>,
}

impl EnsureSpec {
    /// The spec of `draft`, with no assets and no publish.
    #[must_use]
    pub fn new(draft: DraftRequest) -> EnsureSpec {
        EnsureSpec {
            draft,
            assets: Vec::new(),
            publish: None,
        }
    }

    /// The same spec with `files`.
    #[must_use]
    pub fn with_assets(mut self, files: Vec<UploadFile>) -> EnsureSpec {
        self.assets = files;
        self
    }

    /// The same spec, publishing with `patch`.
    #[must_use]
    pub fn with_publish(mut self, patch: PublishPatch) -> EnsureSpec {
        self.publish = Some(patch);
        self
    }
}

/// What `ensure` did, step by step.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct EnsureReport {
    /// The draft (or, when it was published already by this ensure's earlier run, the release).
    pub release: Ensured<ReleaseObject>,
    /// Each asset, in the spec's order.
    pub assets: Vec<Ensured<AssetObject>>,
    /// The publish, when asked.
    pub published: Option<Ensured<ReleaseObject>>,
}

/// How many listing pages of 100, newest first, a write's tag read walks for drafts: the
/// newest 500 releases. A draft a run made is among the newest, because the listing is
/// newest first. Past the bound the listing is a window, not a wall
/// (`recent_objects_for_tag`), so its cost is bounded whatever the repository holds: 11
/// requests on a quiet listing, at most 21.
pub const DRAFT_WINDOW_PAGES: u32 = 5;

impl<'a> Releases<'a> {
    /// What a write must see for `tag`, read as the writer (WriterRead, Snapshot, uncached),
    /// in a bounded number of requests, whatever the repository holds:
    /// - `GET /releases/tags/{tag}` finds the published release, at any depth. A 404 there
    ///   means no published release, not an error;
    /// - the `/releases` listing (newest first, 100 a page) stops after `DRAFT_WINDOW_PAGES`
    ///   pages, keeping the drafts whose `tag_name` is `tag`. It is M1's walk under
    ///   `ListOpts { max_pages: DRAFT_WINDOW_PAGES, partial: true, ..ListOpts::CAS }`, so
    ///   `End::Capped` at the bound is not Incomplete. A page that fails is still the error
    ///   it is.
    ///
    /// **The cost.** At most `DRAFT_WINDOW_PAGES` distinct listing pages are read, and M1's
    /// Snapshot revalidation (`page.rs`'s `changed`) asks each once more with its ETag, a 304
    /// when it has not changed. So a quiet listing of five or more pages costs
    /// `1 + 2 × DRAFT_WINDOW_PAGES` = 11 requests, and a shorter one fewer (a one-page listing
    /// is not revalidated). A listing that changes while it is read is rescanned once, at most
    /// 21 requests, and one that changes again is Incomplete `Torn`: the write is refused
    /// before anything is sent, safe but spurious, and a rerun reads again. A repository
    /// another builder is releasing into can cause that. The window stays Snapshot, not
    /// AsRead: AsRead would save the revalidation, but a release deleted mid-walk shifts the
    /// later pages up, so it could miss the very draft the pre-check exists to see.
    ///
    /// The union, by id, is the `TagObjects`. A draft older than the newest 500 releases is
    /// not seen:
    /// - a foreign one is not refused, and the duplicate reads as `AmbiguousTag` later;
    /// - a lost create's own draft leaves its intent open, which is safe.
    ///
    /// M1's `objects_for_tag` walks every page under `ListOpts::CAS` and is Incomplete past
    /// 20, which a staging repository with one release per build outgrows. So every write path
    /// reads through this.
    ///
    /// # Errors
    ///
    /// The reads' errors.
    pub fn recent_objects_for_tag(&self, tag: &str) -> Result<TagObjects> {
        let _ = tag;
        Err(Error::not_yet("the recent tag read", "WP5"))
    }

    /// Create a draft (design §11.2's four steps). The pre-check, a WriterRead
    /// `recent_objects_for_tag`, decides:
    /// - one draft matching an open intent of this journal for the tag →
    ///   `Done(Ensured { effect: Converged })`, with `observe(intent, Some(id))`;
    /// - one draft with no matching open intent → Conflict, `Sub::ForeignDraft { id }`, next
    ///   `agh never adopts a draft it did not make: a person deletes release {id} or names
    ///   another tag`;
    /// - a published release → Conflict, `Sub::AlreadyExists`;
    /// - several → Conflict, `Sub::AmbiguousTag`.
    ///
    /// A rerun converges only onto what it asks for. `req` must agree with the intent it
    /// would converge onto: the same body digest, and the same target, where an intent with
    /// no target agrees with any. Otherwise the open-intent rule's answer is Conflict
    /// `GuardFailed`, and the created-intent rule's is Conflict `AlreadyExists`, with nothing
    /// sent.
    ///
    /// Otherwise `begin`, one POST (OneShot), and `settle` by design §10.2's table. After
    /// `Unknown`, discovery runs once (WriterRead, Snapshot): one release with the tag whose
    /// target and body digest match the intent → `Done(Converged)` and `observe(.., Some)`;
    /// none → `Unknown`, with `observe(.., None)`; several → `AmbiguousTag`.
    ///
    /// # Errors
    ///
    /// Usage for a permit on another slug; Invalid for a body over 125,000 characters;
    /// `write::unix_only()` off unix; `begin`'s refusal (nothing is sent).
    pub fn create(
        &self,
        permit: &Permit,
        req: &DraftRequest,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<ReleaseObject>>> {
        let _ = (permit, req, intents);
        Err(Error::not_yet("release create", "WP5"))
    }

    /// Upload `file` to `release` (design §11.2): Converging, its guard re-reading the asset
    /// list before every attempt.
    ///
    /// # Errors
    ///
    /// Refused `immutable` (`Sub::Immutable`) on an immutable release; Invalid for 1000 assets
    /// already there; Conflict `AlreadyExists` under `Never` for another digest; Conflict
    /// `DigestMismatch` when the answer's digest is not the file's.
    pub fn upload(
        &self,
        permit: &Permit,
        release: &ReleaseObject,
        file: &UploadFile,
        replace: Replace,
    ) -> Result<Ensured<AssetObject>> {
        let _ = (permit, release, file, replace);
        Err(Error::not_yet("release upload", "WP5"))
    }

    /// Publish draft `id` (design §11.2): Converging, its guard re-reading the release
    /// before every attempt. A release already published with the guard's assets →
    /// `Converged` (a lost answer's publish), with no PATCH.
    ///
    /// # Errors
    ///
    /// Conflict `GuardFailed { what }` when the release is no longer a draft holding the
    /// expected assets; NotFound when it is gone.
    pub fn publish(
        &self,
        permit: &Permit,
        id: u64,
        guard: &PublishGuard,
        patch: &PublishPatch,
    ) -> Result<Ensured<ReleaseObject>> {
        let _ = (permit, id, guard, patch);
        Err(Error::not_yet("release publish", "WP5"))
    }

    /// Edit release `id`: Converging. Values already there → `Unchanged`, with no PATCH.
    ///
    /// # Errors
    ///
    /// NotFound when it is gone; Invalid for notes over 125,000 characters.
    pub fn edit(
        &self,
        permit: &Permit,
        id: u64,
        edit: &ReleaseEdit,
    ) -> Result<Ensured<ReleaseObject>> {
        let _ = (permit, id, edit);
        Err(Error::not_yet("release edit", "WP5"))
    }

    /// Delete release `id`: Converging, a guard read by id before every attempt. A 404 before
    /// the first DELETE → `Gone`, with nothing sent; a 404 before a later attempt, or on the
    /// re-read after a 204 → `Removed`.
    ///
    /// # Errors
    ///
    /// Conflict `GuardFailed` when it is still there after a 204.
    pub fn delete(&self, permit: &Permit, id: u64) -> Result<Deleted> {
        let _ = (permit, id);
        Err(Error::not_yet("release delete", "WP5"))
    }

    /// Delete `release`'s asset `name`, then re-list.
    ///
    /// # Errors
    ///
    /// Refused `immutable` on an immutable release. A name the first listing does not hold is
    /// `Gone`, with no DELETE, so a rerun after a lost answer converges.
    pub fn delete_asset(
        &self,
        permit: &Permit,
        release: &ReleaseObject,
        name: &str,
    ) -> Result<Deleted> {
        let _ = (permit, release, name);
        Err(Error::not_yet("asset delete", "WP5"))
    }

    /// Create or converge the draft, upload each asset with `Replace::Never`, then publish
    /// guarded by exactly those assets when asked. Each step's own rules hold. The first
    /// step that is not `Done` ends it: the draft's `Outcome` when the create is not
    /// `Done`, and an `Err` from a later step.
    ///
    /// # Errors
    ///
    /// The steps' errors.
    pub fn ensure(
        &self,
        permit: &Permit,
        spec: &EnsureSpec,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<EnsureReport>> {
        let _ = (permit, spec, intents);
        Err(Error::not_yet("release ensure", "WP5"))
    }

    /// Close an open create intent for `tag` from what a WriterRead `recent_objects_for_tag`
    /// (or `objects_for_tag`) just saw (design §10.2's discovery, run by a caller that read the
    /// tag anyway: AghHost's `release`, finding F4). Exactly one object whose target and body
    /// digest match the open intent → `observe(intent, Some(id))` and `Some(intent)`. None,
    /// several, or no open intent for the tag → `None`, and nothing is appended. `seen` must
    /// come from a WriterRead: a Read listing is refused Usage, because it can miss a draft.
    ///
    /// # Errors
    ///
    /// The journal's refusals; Usage as above.
    pub fn observe_open(
        &self,
        tag: &str,
        seen: &TagObjects,
        seen_as: Access,
        intents: &dyn IntentStore,
    ) -> Result<Option<IntentId>> {
        let _ = (tag, seen, seen_as, intents);
        Err(Error::not_yet("the open intent's discovery", "WP5"))
    }
}

/// What a create does next (design §11.2's pre-check, the open-intent rule and the
/// created-intent rule). AghEnsureOnce's `Begin` is enabled exactly when this is `Begin { .. }`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CreateStep {
    /// Begin an intent, then POST.
    Begin {
        /// The newest `ReleaseCreate` intent of the tag this step judged (`IntentStore::latest`),
        /// or `None`: the draft's `after`, which `begin` judges again under the lock.
        after: Option<IntentId>,
    },
    /// GitHub holds the draft an intent of this journal made: no POST.
    Converge {
        /// The draft, or the release it was published as (boxed, so the enum stays small, as
        /// M1's `TagObjects::One` is).
        release: Box<ReleaseObject>,
        /// The intent that made it.
        intent: IntentId,
        /// Whether an `Observed` record is appended: true for an open intent.
        observe: bool,
    },
    /// An intent for the tag is open and no draft is seen: no POST (`Outcome::Unknown`).
    StillOpen {
        /// The open intent.
        intent: IntentId,
    },
    /// The step refuses: a foreign draft, a published release or several releases (the
    /// pre-check), or a request that does not agree with the intent it would converge onto.
    Refuse(Error),
}

/// What a discovery found for an open create intent (design §10.2).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Discovered {
    /// One release with the tag, whose target and body digest match the intent (boxed, as
    /// `CreateStep::Converge`'s is).
    Found(Box<ReleaseObject>),
    /// None seen: the intent stays open.
    NoneSeen,
    /// Several: Conflict `AmbiguousTag`, and the intent stays open.
    Several,
}

impl Releases<'_> {
    /// Begin's step: `intents.open()`, one WriterRead `recent_objects_for_tag(&req.tag_name)`,
    /// and, for the tag's latest closed intent that made an object, one WriterRead of that
    /// object by id. It takes the whole request, because it converges only onto an intent the
    /// request agrees with.
    pub(crate) fn create_step(
        &self,
        req: &DraftRequest,
        intents: &dyn IntentStore,
    ) -> Result<CreateStep> {
        let _ = (req, intents);
        Err(Error::not_yet("the create's begin step", "WP5"))
    }

    /// Send's step: the one POST, `call_write` with `Idempotency::OneShot`.
    pub(crate) fn send_create(&self, call: &Call, permit: &Permit) -> Result<Answered> {
        let _ = (call, permit);
        Err(Error::not_yet("the create's send step", "WP5"))
    }

    /// Settle's step: `write::settle_table` of `result`, an `Unknown`'s why passed through
    /// the client's `redactor().apply`, then `intents.settle`.
    pub(crate) fn settle_create(
        &self,
        result: &Result<Answered>,
        intent: &IntentId,
        intents: &dyn IntentStore,
    ) -> Result<Settled> {
        let _ = (result, intent, intents);
        Err(Error::not_yet("the create's settle step", "WP5"))
    }

    /// Discover's step: one WriterRead `recent_objects_for_tag` (Snapshot), then
    /// `intents.observe` with what it found.
    pub(crate) fn discover(
        &self,
        intent: &Intent,
        intents: &dyn IntentStore,
    ) -> Result<Discovered> {
        let _ = (intent, intents);
        Err(Error::not_yet("the create's discover step", "WP5"))
    }
}

/// `Begin`'s projection: 1 for `CreateStep::Begin { .. }` (`matches!`), else 0.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_create_step(step: &CreateStep) -> i64 {
    i64::from(matches!(step, CreateStep::Begin { .. }))
}

/// `Send`'s projection: the POSTs the attempt sent. `Ok` is its `requests`; an `Err` is 1
/// when it carries `http` or `side_effect` unknown, else 0.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_sent(result: &Result<Answered>) -> i64 {
    match result {
        Ok(answered) => i64::from(answered.requests),
        Err(error) => {
            i64::from(error.http.is_some() || error.side_effect == crate::SideEffect::Unknown)
        }
    }
}

/// `Settle`'s projection `(settled, intent)`: NeverSent (1, 2), Refused (2, 2), Created
/// (3, 2), Unknown (4, 1).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_settled(settled: &Settled) -> (i64, i64) {
    match settled {
        Settled::NeverSent => (1, 2),
        Settled::Refused { .. } => (2, 2),
        Settled::Created { .. } => (3, 2),
        Settled::Unknown { .. } => (4, 1),
    }
}

/// `Discover`'s projection `(found, intent, settled)`: Found (1, 2, 3); NoneSeen and Several
/// (2, 1, 4).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_discovered(found: &Discovered) -> (i64, i64, i64) {
    match found {
        Discovered::Found(_) => (1, 2, 3),
        Discovered::NoneSeen | Discovered::Several => (2, 1, 4),
    }
}

/// The host of `AghEnsureOnce`'s waivers: the world's actions no agh function performs (an
/// answer, a lost answer, a listing that reveals, a crash) and the negative controls. Empty,
/// as `policy::route_wall_environment` is; WP0b writes it bare, and WP5 puts the waivers on it
/// in the same commit as the anchors.
#[doc(hidden)]
pub fn ensure_once_environment() {}
