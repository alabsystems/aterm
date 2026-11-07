// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Repository releases: listing, lookup and asset download, and the release write protocol
//! beside them (create, upload, clobber, publish, edit, delete, ensure), a journaled write
//! settled through the write journal.

use aterm_json::{Value, json};

use crate::client::{Accept, Answered, Call, CallBody, ConvergingAttempts, Ended, Github, Look};
use crate::download::{self, Downloaded, Expect, Sink};
use crate::error::{Error, Forbid, Kind, Result, Sub};
use crate::github::{How, limit_after_sort, repo_root};
use crate::journal::{Intent, IntentId, IntentStore, IntentView, Settled};
use crate::model::{AssetObject, ReleaseObject, Sha256Digest, from_json_owned};
use crate::notify::Notice;
use crate::page::{End, LinkScope, ListOpts, Listing};
use crate::path;
use crate::policy::{Access, Permit, RouteRequest, RouteTarget};
use crate::retry::Idempotency;
use crate::slug::Slug;
use crate::wire::{Deadlines, Method, OriginRole};
use crate::write::{Deleted, Effect, Ensured, Outcome, WriteOp};

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
            .and_then(from_json_owned)
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
            .map(from_json_owned::<ReleaseObject>)
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        if !self.writer() {
            listing.items.retain(|release| !release.draft);
        }
        listing
            .items
            .sort_unstable_by(|a, b| (&b.created_at, b.id).cmp(&(&a.created_at, a.id)));
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
    /// `list`'s errors. Usage when `tag_regex` outruns its step budget on a tag.
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
        let mut best: Option<ReleaseObject> = None;
        for release in listing
            .items
            .into_iter()
            .filter(|release| !release.draft)
            .filter(|release| prerelease.is_none_or(|want| release.prerelease == want))
        {
            let matched = match tag_regex {
                None => true,
                Some(regex) => regex.try_is_match(&release.tag_name).map_err(|e| {
                    Error::usage(format!(
                        "--tag-regex {:?} gave up on a {}-byte tag: {e}",
                        regex.as_str(),
                        release.tag_name.len()
                    ))
                })?,
            };
            if matched
                && best.as_ref().is_none_or(|cur| {
                    (&release.created_at, release.id) >= (&cur.created_at, cur.id)
                })
            {
                best = Some(release);
            }
        }
        Ok(best)
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
        .and_then(from_json_owned)
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
    /// - a name of 1 to 255 bytes in `[A-Za-z0-9._+-]`, not starting or ending with `.`,
    ///   holding no `..` (GitHub renames anything else, and agh refuses rather than be
    ///   renamed).
    ///
    /// # Errors
    ///
    /// Invalid (exit 10, design §7.1's client-side limits), why `{path}: {what}`, naming the
    /// rule; could_not_run for a read that failed.
    pub fn open(path: &std::path::Path, name: Option<&str>) -> Result<UploadFile> {
        use std::io::Read as _;
        let shown = path.display().to_string();
        let invalid = |what: String| Error::new(Kind::Invalid, format!("{shown}: {what}"));
        let unread = |e: std::io::Error| Error::local(format!("{shown} could not be read: {e}"));
        let name = match name {
            Some(name) => name.to_string(),
            None => path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .ok_or_else(|| invalid("its file name is not UTF-8; name the asset".to_string()))?
                .to_string(),
        };
        if let Some(what) = asset_name_fault(&name) {
            return Err(invalid(what));
        }
        // The open never waits (on unix `O_NONBLOCK`: a blocking open of a FIFO with no writer
        // never returns), and the type is judged on the opened handle, so whatever the path
        // names at the open is what is judged.
        let mut file = crate::download::open_nonblocking(path).map_err(unread)?;
        let meta = file.metadata().map_err(unread)?;
        if !meta.is_file() {
            return Err(invalid("not a regular file".to_string()));
        }
        if meta.len() > ASSET_BYTES_MAX {
            return Err(invalid(format!(
                "{} bytes; GitHub takes at most 2 GiB for one asset",
                meta.len()
            )));
        }
        let mut hasher = aterm_digest::Sha256::new();
        let mut buf = [0u8; 16 * 1024];
        let mut len = 0u64;
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    len = len.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                    if len > meta.len() {
                        return Err(Error::local(format!("{shown} changed while it was hashed")));
                    }
                    hasher.update(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(unread(e)),
            }
        }
        if len != meta.len() {
            return Err(Error::local(format!("{shown} changed while it was hashed")));
        }
        Ok(UploadFile {
            path: path.to_path_buf(),
            name,
            content_type: "application/octet-stream".to_string(),
            len,
            sha256: Sha256Digest::from_bytes(hasher.finalize()),
        })
    }
}

/// GitHub's most for one asset: 2 GiB.
const ASSET_BYTES_MAX: u64 = 2 << 30;
/// GitHub's most assets on one release.
pub(crate) const ASSETS_MAX: usize = 1000;
/// The longest asset name GitHub keeps.
pub(crate) const ASSET_NAME_MAX: usize = 255;
/// GitHub's most characters of release notes.
pub(crate) const NOTES_MAX: usize = 125_000;

/// The temporary asset name a clobber of `name` with the bytes `digest` uploads first (design
/// §11.2, step 1): `{name}.agh-` and the first 8 hex digits of the digest.
#[must_use]
pub fn clobber_temporary(name: &str, digest: &Sha256Digest) -> String {
    let mut out = String::with_capacity(name.len() + 13);
    out.push_str(name);
    out.push_str(".agh-");
    aterm_codec::hex::push_lower(&mut out, &digest.as_bytes()[..4]);
    out
}

/// Whether `asset` is the temporary name of some clobber of `name` ([`clobber_temporary`] of
/// any bytes): `{name}.agh-` and 8 lowercase hex digits.
#[must_use]
pub fn is_clobber_temporary_of(asset: &str, name: &str) -> bool {
    asset
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(".agh-"))
        .is_some_and(|hex| aterm_codec::hex::is_lower(hex, 8))
}

/// What is wrong with asset `name` (design §11.2), or `None`: 1 to 255 bytes in
/// `[A-Za-z0-9._+-]`, not starting or ending with `.`, holding no `..`.
fn asset_name_fault(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > ASSET_NAME_MAX {
        return Some(format!(
            "asset name {name} holds {} bytes; GitHub takes 1 to {ASSET_NAME_MAX}",
            name.len()
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return Some(format!(
            "asset name {name} holds a character outside [A-Za-z0-9._+-], which GitHub renames"
        ));
    }
    if name.starts_with('.') || name.ends_with('.') || name.contains("..") {
        return Some(format!(
            "asset name {name} starts or ends with . or holds .., which GitHub renames"
        ));
    }
    None
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
    /// what clears a cut upload's asset without a person. Between the DELETE of the old asset
    /// and the PATCH that renames the temporary one, the name's download answers 404 for one
    /// round trip (design §11.2).
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

impl ReleaseEdit {
    /// Whether `release` already holds every field this edit names (a missing body reads as
    /// empty): the edit guard's rule, which `release edit --dry-run` asks too.
    #[must_use]
    pub(crate) fn holds(&self, release: &ReleaseObject) -> bool {
        self.name
            .as_ref()
            .is_none_or(|name| release.name.as_ref() == Some(name))
            && self
                .body
                .as_ref()
                .is_none_or(|notes| release.body.as_deref().unwrap_or("") == notes)
            && self
                .prerelease
                .is_none_or(|prerelease| release.prerelease == prerelease)
    }
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
/// (`recent_objects_for_tag`), so its cost is bounded whatever the repository holds: 12
/// requests on a quiet listing (the push check, the tag read and ten listing requests), at
/// most 22.
pub const DRAFT_WINDOW_PAGES: u32 = 5;

impl<'a> Releases<'a> {
    /// What a write must see for `tag`, read as the writer (WriterRead, Snapshot, uncached),
    /// in a bounded number of requests, whatever the repository holds:
    /// - the push check (design §6.4) reads `GET /repos/{o}/{r}` as the writer first, in this
    ///   call and never memoized: `permissions.push` must hold, and a fine-grained writer
    ///   must be vouched (`DraftsUnproven`), so a listing that cannot see drafts is refused
    ///   rather than read as empty;
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
    /// `2 + 2 × DRAFT_WINDOW_PAGES` = 12 requests with the push check, and a shorter one
    /// fewer (a one-page listing is not revalidated). The push check's answer also gives M1's
    /// walk the repository's id, against which it checks GitHub's numeric next links
    /// (`/repositories/{id}/releases`). A listing that changes while it is read is rescanned
    /// once, at most 22 requests, and one that changes again is Incomplete `Torn`: the write
    /// is refused before anything is sent, safe but spurious, and a rerun reads again. A
    /// repository another builder is releasing into can cause that. The window stays
    /// Snapshot, not AsRead: AsRead would save the revalidation, but a release deleted
    /// mid-walk shifts the later pages up, so it could miss the very draft the pre-check
    /// exists to see.
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
    /// The reads' errors: the push check's Forbidden `no_push` or `drafts_unproven`, and its
    /// not_visible for a repository the writer cannot see, before any release is read.
    pub fn recent_objects_for_tag(&self, tag: &str) -> Result<TagObjects> {
        self.recent_objects_and_cut(tag).map(|(objects, _)| objects)
    }

    /// `recent_objects_for_tag`, and whether its window was cut: the listing went on past
    /// `DRAFT_WINDOW_PAGES`, so a draft older than the newest 500 releases may hold the tag.
    ///
    /// # Errors
    ///
    /// As `recent_objects_for_tag`.
    pub(crate) fn recent_objects_and_cut(&self, tag: &str) -> Result<(TagObjects, bool)> {
        path::refuse_bad_tag(tag).map_err(|e| e.with_repo(self.slug.to_string()))?;
        let writer = self.guard_view(None);
        writer.push_check()?;
        let tagged = match writer.read_release(tag_path(&self.slug, tag)) {
            Ok(release) => Some(release),
            Err(error) if is_404(&error) => None,
            Err(error) => return Err(error),
        };
        let window = ListOpts::CAS
            .with_max_pages(DRAFT_WINDOW_PAGES)
            .with_partial(true);
        let listing = writer.list_checked(&window)?;
        let cut = matches!(listing.end, End::Capped { .. });
        let mut found: Vec<ReleaseObject> = tagged
            .into_iter()
            .filter(|release| release.tag_name == tag)
            .collect();
        for release in listing.items {
            if release.draft && release.tag_name == tag && !found.iter().any(|f| f.id == release.id)
            {
                found.push(release);
            }
        }
        found.sort_unstable_by_key(|release| release.id);
        let objects = match found.len() {
            0 => TagObjects::None,
            1 => TagObjects::One(Box::new(found.remove(0))),
            _ => TagObjects::Many(found),
        };
        Ok((objects, cut))
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
    /// none → `Unknown`, with `observe(.., None)`; several → `AmbiguousTag`. A discovery that
    /// fails (its reads or the journal's) → `Unknown`, the intent open, its why appended to
    /// the lost answer's.
    ///
    /// # Errors
    ///
    /// Usage for a permit on another slug; Invalid for a body over 125,000 characters;
    /// `write::unix_only()` off unix; `begin`'s refusal (nothing is sent); the push check's
    /// refusals (`recent_objects_for_tag`), before anything is sent. After a lost answer,
    /// several releases with the tag are Conflict `AmbiguousTag`, `side_effect` unknown, next
    /// `agh intent show {repo} {id}`.
    pub fn create(
        &self,
        permit: &Permit,
        req: &DraftRequest,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<Ensured<ReleaseObject>>> {
        self.write_checks(permit)?;
        let tag = req.tag_name.as_str();
        path::refuse_bad_tag(tag).map_err(|e| e.with_repo(self.slug.to_string()))?;
        notes_bound(req.body.as_str()).map_err(|e| e.with_repo(self.slug.to_string()))?;
        let writer = self.guard_view(Some(permit));
        let (release, intent, effect) = match writer.create_step(req, intents)? {
            CreateStep::Refuse(error) => return Err(error),
            CreateStep::StillOpen { intent } => {
                let error = self.still_open(&intent, tag);
                return Ok(Outcome::Unknown { intent, error });
            }
            CreateStep::Converge {
                release,
                intent,
                observe,
            } => {
                if observe {
                    intents.observe(&intent, Some(release.id))?;
                }
                (*release, intent, Effect::Converged)
            }
            CreateStep::Begin { after } => {
                let call = self.create_call(req)?;
                let draft = self
                    .gh
                    .intent_draft(WriteOp::ReleaseCreate, &self.slug, &call, permit, tag)?
                    .with_target(req.target_commitish.as_deref())
                    .with_body_digest(Some(Sha256Digest::of(req.body.as_bytes()).to_string()))
                    .with_after(after);
                let id = intents.begin(&draft)?;
                let result = writer.send_create(&call, permit);
                match writer.settle_create(&result, &id, intents)? {
                    Settled::Created { .. } => {
                        let answered = result?;
                        let release = answered
                            .json()
                            .and_then(from_json_owned::<ReleaseObject>)
                            .map_err(|e| {
                                e.with_side_effect(crate::SideEffect::Applied)
                                    .with_repo(self.slug.to_string())
                            })?;
                        (release, id, Effect::Created)
                    }
                    Settled::NeverSent => return Ok(Outcome::NeverSent(self.wire_error(result))),
                    Settled::Refused { .. } => {
                        return Ok(Outcome::Refused(self.wire_error(result)));
                    }
                    Settled::Unknown { why } => {
                        let error = match result {
                            Err(error) => error,
                            Ok(_) => Error::new(Kind::OutcomeUnknown, why)
                                .with_side_effect(crate::SideEffect::Applied),
                        }
                        .with_repo(self.slug.to_string());
                        let discovered = intents
                            .latest(WriteOp::ReleaseCreate, tag)
                            .and_then(|latest| {
                                latest
                                    .map(|view| view.intent)
                                    .filter(|intent| intent.id == id)
                                    .ok_or_else(|| {
                                        Error::local(format!(
                                            "the journal of {} lost intent {id} it just began",
                                            self.slug
                                        ))
                                    })
                            })
                            .and_then(|begun| writer.discover(&begun, intents));
                        match discovered {
                            Ok(Discovered::Found(release)) => (*release, id, Effect::Converged),
                            Ok(Discovered::NoneSeen) => {
                                return Ok(Outcome::Unknown { intent: id, error });
                            }
                            // The POST may have landed, so a discovery that failed leaves the
                            // outcome unknown and the intent open: never a refusal.
                            Err(failed) => {
                                let mut error = error;
                                let why = format!(
                                    "{}; the discovery after it failed: {}",
                                    error.why, failed.why
                                );
                                error.why = self.gh.redactor().apply(&why);
                                return Ok(Outcome::Unknown { intent: id, error });
                            }
                            Ok(Discovered::Several) => {
                                return Err(self
                                    .conflict(
                                        Sub::AmbiguousTag { ids: Vec::new() },
                                        format!(
                                            "several releases hold tag {tag}, and intent {id} \
                                             stays open: agh never adopts one of them"
                                        ),
                                    )
                                    .with_side_effect(crate::SideEffect::Unknown)
                                    .with_next(format!("agh intent show {} {id}", self.slug)));
                            }
                        }
                    }
                }
            }
        };
        crate::notify::settled(
            self.gh,
            &self.how,
            &Notice::intent(&self.slug, tag, "created", &intent, Some(release.id)),
        );
        Ok(Outcome::Done(Ensured {
            object: release,
            effect,
        }))
    }

    /// Upload `file` to `release` (design §11.2): Converging, its guard re-reading the asset
    /// list before every attempt. Under `Replace::Clobber`, between the DELETE of the old
    /// asset and the PATCH that renames the temporary one, `file.name`'s download answers 404
    /// for one round trip (design §11.2).
    ///
    /// # Errors
    ///
    /// Refused `immutable` (`Sub::Immutable`) on an immutable release; Invalid for 1000 assets
    /// already there; Conflict `AlreadyExists` under `Never` for another digest; Conflict
    /// `DigestMismatch` when the answer's digest is not the file's, `side_effect` applied;
    /// NotFound when the release is gone; the push check's refusals (design §6.4), before
    /// any asset is read. A lost answer no guard read settles is OutcomeUnknown (§4.6.7).
    pub fn upload(
        &self,
        permit: &Permit,
        release: &ReleaseObject,
        file: &UploadFile,
        replace: Replace,
    ) -> Result<Ensured<AssetObject>> {
        self.write_checks(permit)?;
        if release.immutable {
            return Err(self.immutable(release.id));
        }
        let (ensured, count) = match replace {
            Replace::Never => {
                self.guard_view(Some(permit)).push_check()?;
                self.upload_as(permit, release, file, &file.name)?
            }
            Replace::Clobber => self.clobber(permit, release, file)?,
        };
        crate::notify::settled(
            self.gh,
            &self.how,
            &Notice::converged_for(
                &self.slug,
                &release.tag_name,
                "uploaded",
                Some(count),
                Some(release.id),
                ensured.object.id,
            ),
        );
        Ok(ensured)
    }

    /// Publish draft `id` (design §11.2): Converging, its guard re-reading the release
    /// before every attempt. A release already published with the guard's assets →
    /// `Converged` (a lost answer's publish), with no PATCH.
    ///
    /// # Errors
    ///
    /// Conflict `GuardFailed { what }` when the release is no longer a draft holding the
    /// expected assets, or still a draft after a 2xx (then `side_effect` applied); NotFound
    /// when it is gone; the push check's refusals (design §6.4), before the release is read.
    /// A lost answer no guard read settles is OutcomeUnknown (§4.6.7).
    pub fn publish(
        &self,
        permit: &Permit,
        id: u64,
        guard: &PublishGuard,
        patch: &PublishPatch,
    ) -> Result<Ensured<ReleaseObject>> {
        self.write_checks(permit)?;
        let writer = self.guard_view(Some(permit));
        writer.push_check()?;
        let mut expected: Option<AssetSet> = guard
            .assets
            .as_ref()
            .map(|assets| assets.iter().map(|(n, d)| (n.clone(), Some(*d))).collect());
        let body = json_body(&json!({
            "draft": false,
            "make_latest": patch.make_latest.as_str(),
            "prerelease": patch.prerelease,
        }))?;
        let call = self.write_call(
            Method::Patch,
            format!("{}/{id}", self.releases_path()),
            body,
        );
        let mut look = |_: Look| -> Result<Option<ReleaseObject>> {
            let release = writer.release_by_id(id)?;
            let seen = asset_set(&release)?;
            let want = expected.get_or_insert_with(|| seen.clone());
            self.publish_judgement(release, &seen, want, patch)
        };
        let release = match self.converge(WriteOp::ReleasePublish, &call, permit, &mut look)? {
            Settles::Found(release, _) => Ensured {
                object: release,
                effect: Effect::Converged,
            },
            Settles::Answered(answered) => {
                let release = self
                    .release_of(&answered)
                    .map_err(|e| e.with_side_effect(crate::SideEffect::Applied))?;
                if release.draft {
                    return Err(applied(self.guard_failed(format!(
                        "release {id} is still a draft after its publish"
                    ))));
                }
                Ensured {
                    object: release,
                    effect: Effect::Created,
                }
            }
        };
        crate::notify::settled(
            self.gh,
            &self.how,
            &Notice::converged(
                &self.slug,
                &release.object.tag_name,
                "published",
                Some(count_of(release.object.assets.len())),
                Some(id),
            ),
        );
        Ok(release)
    }

    /// Edit release `id`: Converging. Values already there → `Unchanged`, with no PATCH.
    ///
    /// # Errors
    ///
    /// NotFound when it is gone; Invalid for notes over 125,000 characters; the push check's
    /// refusals (design §6.4), before the release is read. A 2xx answer that does not parse
    /// carries `side_effect` applied. A lost answer no guard read settles is OutcomeUnknown
    /// (§4.6.7).
    pub fn edit(
        &self,
        permit: &Permit,
        id: u64,
        edit: &ReleaseEdit,
    ) -> Result<Ensured<ReleaseObject>> {
        self.write_checks(permit)?;
        if let Some(notes) = &edit.body {
            notes_bound(notes).map_err(|e| e.with_repo(self.slug.to_string()))?;
        }
        let writer = self.guard_view(Some(permit));
        writer.push_check()?;
        let mut fields = aterm_json::Map::new();
        if let Some(name) = &edit.name {
            fields.insert("name".into(), Value::from(name.as_str()));
        }
        if let Some(notes) = &edit.body {
            fields.insert("body".into(), Value::from(notes.as_str()));
        }
        if let Some(prerelease) = edit.prerelease {
            fields.insert("prerelease".into(), Value::from(prerelease));
        }
        let body = json_body(&Value::Object(fields))?;
        let call = self.write_call(
            Method::Patch,
            format!("{}/{id}", self.releases_path()),
            body,
        );
        let mut look = |_: Look| -> Result<Option<ReleaseObject>> {
            let release = writer.release_by_id(id)?;
            Ok(edit.holds(&release).then_some(release))
        };
        Ok(
            match self.converge(WriteOp::ReleaseUpdate, &call, permit, &mut look)? {
                Settles::Found(release, attempt) => Ensured {
                    object: release,
                    effect: if attempt == 1 {
                        Effect::Unchanged
                    } else {
                        Effect::Converged
                    },
                },
                Settles::Answered(answered) => Ensured {
                    object: self
                        .release_of(&answered)
                        .map_err(|e| e.with_side_effect(crate::SideEffect::Applied))?,
                    effect: Effect::Created,
                },
            },
        )
    }

    /// Delete release `id`: Converging, a guard read by id before every attempt. A 404 before
    /// the first DELETE → `Gone`, with nothing sent; a 404 answering the DELETE itself, with no
    /// earlier answer lost → `Gone`, as another DELETE removed it; a 404 before a later
    /// attempt, or on the re-read after a 204 → `Removed`.
    ///
    /// # Errors
    ///
    /// Conflict `GuardFailed` when it is still there after a 204, and a failed re-read after
    /// it, each `side_effect` applied; the push check's refusals (design §6.4), before the
    /// release is read, so a 404 is `Gone` only for a repository the writer sees with push. A
    /// lost answer no guard read settles is OutcomeUnknown (§4.6.7).
    pub fn delete(&self, permit: &Permit, id: u64) -> Result<Deleted> {
        self.write_checks(permit)?;
        let writer = self.guard_view(Some(permit));
        writer.push_check()?;
        let call = self.write_call(
            Method::Delete,
            format!("{}/{id}", self.releases_path()),
            CallBody::Empty,
        );
        let mut tag: Option<String> = None;
        let mut look = |look: Look| -> Result<Option<Deleted>> {
            match writer.release_by_id(id) {
                Ok(release) => {
                    tag.get_or_insert(release.tag_name);
                    Ok(None)
                }
                Err(error) if error.kind == Kind::NotFound => Ok(Some(if look.attempt == 1 {
                    Deleted::Gone
                } else {
                    Deleted::Removed
                })),
                Err(error) => Err(error),
            }
        };
        let deleted = match self.converge(WriteOp::ReleaseDelete, &call, permit, &mut look) {
            // The push check passed in this call, so a 404 on the DELETE is the absence the
            // guard would have read: another DELETE won, and this call removed nothing. (After
            // a lost answer the last look reads it instead, and answers `Removed`.)
            Err(error) if is_404(&error) && error.side_effect == crate::SideEffect::None => {
                Deleted::Gone
            }
            Err(error) => return Err(error),
            Ok(Settles::Found(deleted, _)) => deleted,
            // GitHub answered the DELETE with a 2xx, so every error after it is `applied`.
            Ok(Settles::Answered(_)) => match writer.release_by_id(id) {
                Err(error) if error.kind == Kind::NotFound => Deleted::Removed,
                Err(error) => return Err(applied(error)),
                Ok(_) => {
                    return Err(applied(self.guard_failed(format!(
                        "release {id} is still there after its DELETE"
                    ))));
                }
            },
        };
        if deleted == Deleted::Removed
            && let Some(tag) = &tag
        {
            crate::notify::settled(
                self.gh,
                &self.how,
                &Notice::converged(&self.slug, tag, "deleted", None, Some(id)),
            );
        }
        Ok(deleted)
    }

    /// Delete `release`'s asset `name`, then re-list. The DELETE targets the id the first
    /// listing gave the name, and every later look judges that id: an asset of the name that
    /// appears after it is another asset. A 404 answering the DELETE, with no earlier answer
    /// lost, is `Gone`, as another DELETE removed it.
    ///
    /// # Errors
    ///
    /// Refused `immutable` on an immutable release. A name the first listing does not hold,
    /// or a release that is gone, is `Gone`, with no DELETE, so a rerun after a lost answer
    /// converges. The push check's refusals (design §6.4), before the assets are listed.
    /// Conflict `GuardFailed`, `side_effect` applied, when the asset's id is still listed after
    /// a 204, and a failed listing after it carries `side_effect` applied too. A lost answer no
    /// guard read settles is OutcomeUnknown (§4.6.7).
    pub fn delete_asset(
        &self,
        permit: &Permit,
        release: &ReleaseObject,
        name: &str,
    ) -> Result<Deleted> {
        self.write_checks(permit)?;
        if release.immutable {
            return Err(self.immutable(release.id));
        }
        let writer = self.guard_view(Some(permit));
        writer.push_check()?;
        // A release that is gone holds no asset of the name.
        let first = match writer.assets_of(release.id) {
            Err(error) if error.kind == Kind::NotFound => return Ok(Deleted::Gone),
            listed => listed?,
        };
        let Some(asset) = first.iter().find(|a| a.name == name) else {
            return Ok(Deleted::Gone);
        };
        let asset_id = asset.id;
        let call = self.write_call(
            Method::Delete,
            format!("{}/releases/assets/{asset_id}", repo_root(&self.slug)),
            CallBody::Empty,
        );
        let mut left = count_of(first.len().saturating_sub(1));
        let mut look = |look: Look| -> Result<Option<Deleted>> {
            if look.attempt == 1 && !look.last {
                return Ok(None);
            }
            let assets = match writer.assets_of(release.id) {
                Err(error) if error.kind == Kind::NotFound => Vec::new(),
                listed => listed?,
            };
            left = count_of(assets.iter().filter(|a| a.id != asset_id).count());
            Ok((!assets.iter().any(|a| a.id == asset_id)).then_some(Deleted::Removed))
        };
        let deleted = match self.converge(WriteOp::AssetDelete, &call, permit, &mut look) {
            // The push check passed in this call, so a 404 on the DELETE is the absence the
            // guard would have read: another DELETE won, and this call removed nothing.
            Err(error) if is_404(&error) && error.side_effect == crate::SideEffect::None => {
                return Ok(Deleted::Gone);
            }
            Err(error) => return Err(error),
            Ok(Settles::Found(deleted, _)) => deleted,
            // GitHub answered the DELETE with a 2xx, so every error after it is `applied`.
            Ok(Settles::Answered(_)) => {
                let assets = match writer.assets_of(release.id) {
                    Err(error) if error.kind == Kind::NotFound => Vec::new(),
                    listed => listed.map_err(applied)?,
                };
                if assets.iter().any(|a| a.id == asset_id) {
                    return Err(applied(self.guard_failed(format!(
                        "{name} (asset {asset_id}) is still listed after its DELETE"
                    ))));
                }
                left = count_of(assets.len());
                Deleted::Removed
            }
        };
        crate::notify::settled(
            self.gh,
            &self.how,
            &Notice::converged_for(
                &self.slug,
                &release.tag_name,
                "deleted",
                Some(left),
                Some(release.id),
                asset_id,
            ),
        );
        Ok(deleted)
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
        let created = match self.create(permit, &spec.draft, intents)? {
            Outcome::Done(created) => created,
            other => {
                return Ok(other.map(|release| EnsureReport {
                    release,
                    assets: Vec::new(),
                    published: None,
                }));
            }
        };
        let release = &created.object;
        let mut assets = Vec::with_capacity(spec.assets.len());
        for file in &spec.assets {
            if release.draft {
                assets.push(self.upload(permit, release, file, Replace::Never)?);
                continue;
            }
            // Published already: only this ensure's earlier run can have published it (the
            // created-intent rule found it), so each file must be there as it was uploaded.
            let there = release.assets.iter().find(|a| a.name == file.name);
            match there {
                Some(asset) if asset.holds(file.sha256)? => {
                    assets.push(Ensured {
                        object: asset.clone(),
                        effect: Effect::Converged,
                    });
                }
                _ => {
                    return Err(self.guard_failed(format!(
                        "release {} is published without {} as {}",
                        release.id, file.name, file.sha256
                    )));
                }
            }
        }
        let published = match &spec.publish {
            Some(patch) => {
                let expected = spec
                    .assets
                    .iter()
                    .map(|file| (file.name.clone(), file.sha256))
                    .collect();
                Some(self.publish(
                    permit,
                    release.id,
                    &PublishGuard::expecting(expected),
                    patch,
                )?)
            }
            None => None,
        };
        Ok(Outcome::Done(EnsureReport {
            release: created,
            assets,
            published,
        }))
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
        if seen_as != Access::WriterRead {
            return Err(Error::usage(
                "observe_open needs a WriterRead listing: a Read listing can miss a draft",
            )
            .with_repo(self.slug.to_string()));
        }
        let Some(intent) = newest_open(intents, &self.slug, tag)? else {
            return Ok(None);
        };
        let matching: Vec<&ReleaseObject> = tag_objects(seen)
            .iter()
            .filter(|release| matches(release, &intent))
            .collect();
        let [release] = matching.as_slice() else {
            return Ok(None);
        };
        intents.observe(&intent.id, Some(release.id))?;
        Ok(Some(intent.id))
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AghEnsureOnce",
            action = "Begin",
            project = "agh::github::releases::project_create_step"
        )
    )]
    pub(crate) fn create_step(
        &self,
        req: &DraftRequest,
        intents: &dyn IntentStore,
    ) -> Result<CreateStep> {
        let tag = req.tag_name.as_str();
        let writer = self.guard_view(None);
        let open = newest_open(intents, &self.slug, tag)?;
        let objects = writer.recent_objects_for_tag(tag)?;
        // The open-intent rule: an open create of this tag is converged onto, or waited for,
        // and never posted again.
        if let Some(open) = open {
            if !agrees(req, &open) {
                return Ok(CreateStep::Refuse(
                    self.guard_failed(format!(
                        "intent {} for {tag} asked for another target or body",
                        open.id
                    ))
                    .with_next(format!("agh intent show {} {}", self.slug, open.id)),
                ));
            }
            let matching: Vec<&ReleaseObject> = tag_objects(&objects)
                .iter()
                .filter(|release| matches(release, &open))
                .collect();
            return Ok(match matching.as_slice() {
                [] => CreateStep::StillOpen { intent: open.id },
                [release] => CreateStep::Converge {
                    release: Box::new((*release).clone()),
                    intent: open.id,
                    observe: true,
                },
                several => {
                    CreateStep::Refuse(self.ambiguous(tag, several.iter().map(|r| r.id).collect()))
                }
            });
        }
        // The created-intent rule: the tag's latest closed intent that made an object is read
        // by id, which a lagging listing cannot hide.
        let latest = intents
            .latest(WriteOp::ReleaseCreate, tag)?
            .filter(|view| view.intent.slug == self.slug);
        if let Some(view) = &latest
            && !view.open
            && let Some(made) = made_object(view)
        {
            match writer.read_release(format!("{}/{made}", writer.releases_path())) {
                Ok(release) if release.tag_name == tag => {
                    if agrees(req, &view.intent) {
                        return Ok(CreateStep::Converge {
                            release: Box::new(release),
                            intent: view.intent.id.clone(),
                            observe: false,
                        });
                    }
                    return Ok(CreateStep::Refuse(self.conflict(
                        Sub::AlreadyExists,
                        format!(
                            "release {made} for {tag} was made by intent {} with another target \
                             or body",
                            view.intent.id
                        ),
                    )));
                }
                Ok(_) => {}
                Err(error) if is_404(&error) => {}
                Err(error) => return Err(error),
            }
        }
        // The pre-check.
        Ok(match objects {
            TagObjects::None => CreateStep::Begin {
                after: latest.map(|view| view.intent.id),
            },
            TagObjects::One(release) if release.draft => CreateStep::Refuse(
                self.conflict(
                    Sub::ForeignDraft { id: release.id },
                    format!(
                        "draft release {} holds tag {tag}, and no open intent of this journal \
                         made it",
                        release.id
                    ),
                )
                .with_next(format!(
                    "agh never adopts a draft it did not make: a person deletes release {} or \
                     names another tag",
                    release.id
                )),
            ),
            TagObjects::One(release) => CreateStep::Refuse(self.conflict(
                Sub::AlreadyExists,
                format!("published release {} holds tag {tag}", release.id),
            )),
            TagObjects::Many(several) => {
                CreateStep::Refuse(self.ambiguous(tag, several.iter().map(|r| r.id).collect()))
            }
        })
    }

    /// Send's step: the one POST, `call_write` with `Idempotency::OneShot`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AghEnsureOnce",
            action = "Send",
            project = "agh::github::releases::project_sent"
        )
    )]
    pub(crate) fn send_create(&self, call: &Call, permit: &Permit) -> Result<Answered> {
        self.gh.call_write(call, permit, &How::write())
    }

    /// Settle's step: `write::settle_table` of `result`, an `Unknown`'s why passed through
    /// the client's `redactor().apply`, then `intents.settle`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AghEnsureOnce",
            action = "Settle",
            project = "agh::github::releases::project_settled"
        )
    )]
    pub(crate) fn settle_create(
        &self,
        result: &Result<Answered>,
        intent: &IntentId,
        intents: &dyn IntentStore,
    ) -> Result<Settled> {
        let made = result.as_ref().ok().and_then(created_release);
        let redactor = self.gh.redactor();
        let settled = match crate::write::settle_table(result, made) {
            Settled::Unknown { why } => Settled::Unknown {
                why: redactor.apply(&why),
            },
            Settled::Created { id, url } => Settled::Created {
                id,
                url: redactor.apply(&url),
            },
            settled => settled,
        };
        if let Err(e) = intents.settle(intent, &settled) {
            let (status, side_effect) = match result {
                Ok(answered) => (answered.status.to_string(), crate::SideEffect::Applied),
                Err(error) => (
                    error
                        .http
                        .as_ref()
                        .map_or_else(|| "nothing".to_string(), |http| http.status.to_string()),
                    crate::SideEffect::Unknown,
                ),
            };
            let why = format!(
                "GitHub answered {status}, and the journal could not record it: {}; intent \
                 {intent} stays open",
                e.why
            );
            return Err(Error::local(redactor.apply(&why))
                .with_side_effect(side_effect)
                .with_next(format!("agh intent show {} {intent}", self.slug))
                .with_repo(self.slug.to_string()));
        }
        Ok(settled)
    }

    /// Discover's step: one WriterRead `recent_objects_for_tag` (Snapshot), then
    /// `intents.observe` with what it found.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AghEnsureOnce",
            action = "Discover",
            project = "agh::github::releases::project_discovered"
        )
    )]
    pub(crate) fn discover(
        &self,
        intent: &Intent,
        intents: &dyn IntentStore,
    ) -> Result<Discovered> {
        let objects = self.guard_view(None).recent_objects_for_tag(&intent.key)?;
        let matching: Vec<&ReleaseObject> = tag_objects(&objects)
            .iter()
            .filter(|release| matches(release, intent))
            .collect();
        Ok(match matching.as_slice() {
            [release] => {
                intents.observe(&intent.id, Some(release.id))?;
                Discovered::Found(Box::new((*release).clone()))
            }
            [] => {
                intents.observe(&intent.id, None)?;
                Discovered::NoneSeen
            }
            _ => {
                intents.observe(&intent.id, None)?;
                Discovered::Several
            }
        })
    }
}

/// How a Converging write ended.
enum Settles<T> {
    /// A guard read found the state already right, before the attempt it names.
    Found(T, u32),
    /// GitHub answered an attempt with a 2xx (boxed: an answer carries its headers and body).
    Answered(Box<Answered>),
}

/// One asset set: name → digest (`None` for an asset with no digest).
pub type AssetSet = std::collections::BTreeMap<String, Option<Sha256Digest>>;

impl<'a> Releases<'a> {
    /// The checks every write method makes first (§9 body 1): unix only, and a permit for this
    /// resource's slug (ASCII case-insensitive), else Usage with nothing sent.
    fn write_checks(&self, permit: &Permit) -> Result<()> {
        super::permit_covers(permit, &self.slug, "releases")
    }

    /// The same releases read as a write's guard reads (design §11.3: WriterRead, Snapshot and
    /// uncached), the rate book not consulted, and, for a write, the naming its permit was
    /// routed with, so a release-tier guard reads as the writer too. There is no 404 probe:
    /// every write runs the push check (design §6.4) first, in the same call, and its
    /// `GET /repos/{o}/{r}` as this writer is the probe's own read, so a later 404 on a
    /// release or asset is the thing's absence (design §6.4: not_found only after that
    /// check passed in the same call). A read through this view before the push check is a
    /// bug.
    fn guard_view(&self, permit: Option<&Permit>) -> Releases<'a> {
        let mut how = self
            .how
            .with_access(Access::WriterRead)
            .rate_book(false)
            .no_probe(true);
        if let Some(permit) = permit {
            how = how.named(permit.named());
        }
        Releases {
            gh: self.gh,
            slug: self.slug.clone(),
            how,
        }
    }

    /// One release read from `path` under this resource's routing.
    fn read_release(&self, path: String) -> Result<ReleaseObject> {
        let call = self.how.get(path, RouteTarget::Repo(self.slug.clone()));
        let answered = self
            .gh
            .call(&call)
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        self.release_of(&answered)
    }

    /// The release an answer holds.
    fn release_of(&self, answered: &Answered) -> Result<ReleaseObject> {
        answered
            .json()
            .and_then(from_json_owned)
            .map_err(|e| e.with_repo(self.slug.to_string()))
    }

    /// Release `id`, a 404 recast as NotFound: every caller ran the push check first in the
    /// same call (`guard_view`), so the repository is visible and the release is absent.
    fn release_by_id(&self, id: u64) -> Result<ReleaseObject> {
        self.read_release(format!("{}/{id}", self.releases_path()))
            .map_err(|error| {
                if is_404(&error) {
                    error
                        .recast(Kind::NotFound, format!("release {id} is not found"))
                        .with_next(Kind::NotFound.next_action())
                } else {
                    error
                }
            })
    }

    /// Every asset of release `id`: `GET …/releases/{id}/assets`, `per_page=100`, every page.
    /// A 404 is NotFound, as `release_by_id`'s is: every caller ran the push check first in
    /// the same call (`guard_view`), so the release is absent.
    fn assets_of(&self, id: u64) -> Result<Vec<AssetObject>> {
        let first = self.how.get(
            format!("{}/{id}/assets", self.releases_path()),
            RouteTarget::Repo(self.slug.clone()),
        );
        let walked = crate::page::walk(
            self.gh,
            first,
            &ListOpts::CAS,
            &LinkScope::Repo(self.slug.clone()),
        )
        .map_err(|error| {
            if is_404(&error) {
                error
                    .recast(Kind::NotFound, format!("release {id} is not found"))
                    .with_next(Kind::NotFound.next_action())
                    .with_repo(self.slug.to_string())
            } else {
                error
            }
        })?;
        walked
            .items
            .into_iter()
            .map(from_json_owned::<AssetObject>)
            .collect::<Result<Vec<_>>>()
            .map_err(|e| e.with_repo(self.slug.to_string()))
    }

    /// A write call on this repository's api origin, Converging.
    fn write_call(&self, method: Method, path: String, body: CallBody) -> Call {
        Call::write(
            method,
            path,
            RouteRequest::write(RouteTarget::Repo(self.slug.clone())),
            Idempotency::Converging,
            body,
        )
    }

    /// The create's one POST (§9 body 3.2): `POST /repos/{o}/{r}/releases`, OneShot, the
    /// draft as compact JSON with sorted keys, `target_commitish` and `make_latest` left out
    /// when `None`.
    fn create_call(&self, req: &DraftRequest) -> Result<Call> {
        let mut fields = aterm_json::Map::new();
        fields.insert("body".into(), Value::from(req.body.as_str()));
        fields.insert("draft".into(), Value::from(true));
        if let Some(latest) = req.make_latest {
            fields.insert("make_latest".into(), Value::from(latest.as_str()));
        }
        fields.insert("name".into(), Value::from(req.name.as_str()));
        fields.insert("prerelease".into(), Value::from(req.prerelease));
        fields.insert("tag_name".into(), Value::from(req.tag_name.as_str()));
        if let Some(target) = &req.target_commitish {
            fields.insert("target_commitish".into(), Value::from(target.as_str()));
        }
        Ok(Call::write(
            Method::Post,
            self.releases_path(),
            RouteRequest::write(RouteTarget::Repo(self.slug.clone())),
            Idempotency::OneShot,
            json_body(&Value::Object(fields))?,
        ))
    }

    /// A Converging write (design §7.4): the guard reads before every attempt, and a found
    /// state ends the loop with nothing sent. The attempts are `Github::converging`'s, one at a
    /// time under the client's retry policy, so the guard knows whether an earlier attempt of
    /// this call lost its answer. A lost answer is kept for the whole call, so a later attempt
    /// that is answered, or never sent, does not forget that the first may have landed. A loop
    /// that ends after a lost answer asks the guard once more with nothing sent after it; a
    /// state still not right is OutcomeUnknown, built on the lost attempt's error. A guard
    /// read, or a pause, that fails after a lost answer is OutcomeUnknown too (`unsettled`),
    /// never its own error with no side effect.
    fn converge<T>(
        &self,
        op: WriteOp,
        call: &Call,
        permit: &Permit,
        guard: &mut dyn FnMut(Look) -> Result<Option<T>>,
    ) -> Result<Settles<T>> {
        let mut found = None;
        let attempts = self.gh.converging(call, permit, &self.how, &mut |look| {
            Ok(guard(look)?.map(|state| {
                found = Some(state);
                guard_answer()
            }))
        });
        let ConvergingAttempts {
            result,
            ended,
            lost,
            attempt,
            ..
        } = attempts;
        let failure = match (result, ended) {
            (Ok(answered), _) => {
                return Ok(match found {
                    Some(state) => Settles::Found(state, attempt),
                    None => Settles::Answered(Box::new(answered)),
                });
            }
            (Err(read), Ended::Guard | Ended::Pause) => {
                return Err(match lost {
                    Some(sent) => self.unsettled(op, sent, Some(read)),
                    None => read,
                });
            }
            (Err(failure), Ended::Attempt | Ended::Answered) => failure,
        };
        // The kept lost answer, not `failure`: a later 422's sub and status would read as
        // GitHub's answer to this call.
        let Some(sent) = lost else {
            return Err(failure);
        };
        let last = Look {
            attempt: attempt.saturating_add(1),
            lost: true,
            last: true,
        };
        match guard(last) {
            Ok(Some(state)) => Ok(Settles::Found(state, last.attempt)),
            Ok(None) => Err(self.unsettled(op, sent, None)),
            Err(read) => Err(self.unsettled(op, sent, Some(read))),
        }
    }

    /// A Converging write's error once an attempt lost its answer (`sent`, the write may have
    /// landed) and the guard read after it did not find the state right (§4.6.7). The guard's
    /// own verdict (`read` of kind Conflict, NotFound, Invalid, OutcomeUnknown or Interrupted)
    /// keeps its kind, `side_effect` unknown. Anything else (no `read`, or a read that
    /// failed) is OutcomeUnknown (exit 13) built on `sent`, why `outcome unknown: {op} on
    /// {repo}; the guard read could not settle it`, and the failed read's why after it.
    fn unsettled(&self, op: WriteOp, sent: Error, read: Option<Error>) -> Error {
        let mut why = format!(
            "outcome unknown: {} on {}; the guard read could not settle it",
            op.slug(),
            self.slug
        );
        if let Some(read) = read {
            if matches!(
                read.kind,
                Kind::Conflict
                    | Kind::NotFound
                    | Kind::Invalid
                    | Kind::OutcomeUnknown
                    | Kind::Interrupted
            ) {
                return read.with_side_effect(crate::SideEffect::Unknown);
            }
            why = self.gh.redactor().apply(&format!("{why} ({})", read.why));
        }
        sent.recast(Kind::OutcomeUnknown, why)
            .with_side_effect(crate::SideEffect::Unknown)
            .with_next(Kind::OutcomeUnknown.next_action())
            .with_repo(self.slug.to_string())
    }

    /// Upload `file` as asset `name` under `Replace::Never`'s rules (§9 body 6), and the
    /// release's asset count after it.
    fn upload_as(
        &self,
        permit: &Permit,
        release: &ReleaseObject,
        file: &UploadFile,
        name: &str,
    ) -> Result<(Ensured<AssetObject>, u32)> {
        let writer = self.guard_view(Some(permit));
        let id = release.id;
        let call = Call::write(
            Method::Post,
            format!("{}/{id}/assets", self.releases_path()),
            RouteRequest::write(RouteTarget::Repo(self.slug.clone())),
            Idempotency::Converging,
            CallBody::File {
                path: file.path.clone(),
                len: file.len,
                content_type: file.content_type.clone(),
            },
        )
        .origin(OriginRole::Uploads)
        .query("name", name)
        .deadlines(Deadlines::for_transfer(file.len));
        let ours = file.sha256;
        let mut others = 0u32;
        // What the guard's DELETE of this call's own left asset may have done: every error
        // after it says at least that (`took`).
        let deleted = std::cell::Cell::new(crate::SideEffect::None);
        // Whether the guard has cleared a left asset once (its DELETE answered, or a 404).
        let mut cleared = false;
        let mut look = |look: Look| -> Result<Option<AssetObject>> {
            let assets = writer.assets_of(id)?;
            others = count_of(assets.iter().filter(|a| a.name != name).count());
            let Some(asset) = assets.iter().find(|a| a.name == name) else {
                if look.attempt == 1 && !look.last && assets.len() >= ASSETS_MAX {
                    return Err(self.full(id));
                }
                return Ok(None);
            };
            if asset.is_uploaded() {
                let theirs = asset.sha256()?;
                if theirs == Some(ours) {
                    return Ok(Some(asset.clone()));
                }
                if look.last {
                    return Ok(None);
                }
                return Err(self.held_otherwise(name, id, theirs, ours));
            }
            if look.last {
                return Ok(None);
            }
            if !look.lost {
                // Most likely another uploader's upload in progress: a DELETE would kill it.
                return Err(self.upload_in_progress(name, release, &asset.state));
            }
            if cleared {
                return Err(Error::new(
                    Kind::OutcomeUnknown,
                    format!("{name} was left {} twice on release {id}", asset.state),
                )
                .with_side_effect(crate::SideEffect::Unknown)
                .with_repo(self.slug.to_string()));
            }
            // This call's own lost answer left it: design §11.2 deletes it and sends again.
            deleted.set(self.delete_asset_id(permit, asset.id)?);
            cleared = true;
            Ok(None)
        };
        let settled = self.converge(WriteOp::AssetUpload, &call, permit, &mut look);
        // After the guard's own DELETE, an error says at least what that DELETE may have done.
        let after = |error: Error| took(error, deleted.get());
        let (asset, effect) = match settled {
            Ok(Settles::Found(asset, _)) => (asset, Effect::Converged),
            Ok(Settles::Answered(answered)) => {
                let asset: AssetObject =
                    answered.json().and_then(from_json_owned).map_err(|e| {
                        e.with_side_effect(crate::SideEffect::Applied)
                            .with_repo(self.slug.to_string())
                    })?;
                let got = asset.digest.as_deref().unwrap_or_default();
                if Sha256Digest::parse(got) != Some(ours) {
                    // GitHub answered 201: the asset with those bytes is on the release.
                    return Err(applied(self.conflict(
                        Sub::DigestMismatch {
                            expected: ours.to_string(),
                            got: got.to_string(),
                        },
                        format!("GitHub holds {name} as {got}, not {ours}"),
                    )));
                }
                (asset, Effect::Created)
            }
            // GitHub answered 422 `already_exists`: one more read, with no send after it.
            Err(error)
                if error.sub == Sub::AlreadyExists
                    && error.http.as_ref().is_some_and(|http| http.status == 422) =>
            {
                let again = Look {
                    attempt: 2,
                    lost: false,
                    last: false,
                };
                match look(again).map_err(after)? {
                    Some(asset) => (asset, Effect::Converged),
                    None => return Err(after(error)),
                }
            }
            Err(error) => return Err(after(error)),
        };
        Ok((
            Ensured {
                object: asset,
                effect,
            },
            others.saturating_add(1),
        ))
    }

    /// `DELETE …/releases/assets/{id}`, Converging; a 404 is accepted. What the DELETE may
    /// have done: `Applied` after a 2xx; after a 404 the 404's own side effect, `Unknown`
    /// when an earlier attempt's answer was lost (it may have removed the asset) and `None`
    /// when this call removed nothing.
    fn delete_asset_id(&self, permit: &Permit, id: u64) -> Result<crate::SideEffect> {
        let call = self.write_call(
            Method::Delete,
            format!("{}/releases/assets/{id}", repo_root(&self.slug)),
            CallBody::Empty,
        );
        match self.gh.call_write(&call, permit, &How::write()) {
            Ok(_) => Ok(crate::SideEffect::Applied),
            Err(error) if is_404(&error) => Ok(error.side_effect),
            Err(error) => Err(error),
        }
    }

    /// `upload(.., Clobber)` (§9 body 7): design §11.2's three steps, and the asset count
    /// after them.
    fn clobber(
        &self,
        permit: &Permit,
        release: &ReleaseObject,
        file: &UploadFile,
    ) -> Result<(Ensured<AssetObject>, u32)> {
        let writer = self.guard_view(Some(permit));
        let id = release.id;
        let name = file.name.as_str();
        let ours = file.sha256;
        let temporary = clobber_temporary(name, &ours);
        writer.push_check()?;
        let first = writer.assets_of(id)?;
        let old = first.iter().find(|a| a.name == name);
        if let Some(asset) = old
            && asset.holds(ours)?
        {
            let others = count_of(first.iter().filter(|a| a.name != name).count());
            return Ok((
                Ensured {
                    object: asset.clone(),
                    effect: Effect::Unchanged,
                },
                others.saturating_add(1),
            ));
        }
        // The temporary name is judged by the asset-name rule as a whole, its length included,
        // once the bytes are known not to be there already and before anything is written.
        if let Some(error) = self.temporary_fault(&temporary) {
            return Err(error);
        }
        let mut parked = first.iter().find(|a| a.name == temporary);
        // What this call's DELETE of a cut temporary asset may have done: every error after it
        // says at least that (`took`).
        let mut cleared = crate::SideEffect::None;
        if let Some(cut) = parked.filter(|asset| !asset.is_uploaded()) {
            // A temporary asset of these bytes that never finished: a clobber of these bytes
            // cut in step 1, or one running beside this one, which N84 lets a clobber replace.
            // `upload_as` would read it as another upload in progress and refuse every rerun,
            // so it goes first, as the old asset does in step 2.
            cleared = self.delete_asset_id(permit, cut.id)?;
            self.await_gone(permit, id, cut)
                .map_err(|e| took(e, cleared))?;
            parked = None;
        }
        let after_clear = |e: Error| took(e, cleared);
        let ours_parked = match parked {
            Some(asset) if asset.holds(ours)? => Some(asset),
            _ => None,
        };
        let (renamed, count) = match (old, ours_parked) {
            (None, None) if parked.is_none() => {
                return self
                    .upload_as(permit, release, file, name)
                    .map_err(after_clear);
            }
            (None, Some(parked)) => self.rename(permit, id, parked, name, ours)?,
            _ => {
                let (staged, _) = self
                    .upload_as(permit, release, file, &temporary)
                    .map_err(after_clear)?;
                // The temporary asset is on the release now, so every error after it says a
                // write took effect (`applied`, unless it says unknown).
                if let Some(old) = old {
                    self.delete_asset_id(permit, old.id).map_err(applied)?;
                    self.await_gone(permit, id, old).map_err(applied)?;
                }
                self.rename(permit, id, &staged.object, name, ours)
                    .map_err(applied)?
            }
        };
        Ok((
            Ensured {
                object: renamed,
                effect: Effect::Created,
            },
            count,
        ))
    }

    /// Clobber's step 2 after its DELETE: list at most three times, a second apart, until
    /// `old` is gone; still listed on the third is Conflict `GuardFailed`.
    fn await_gone(&self, permit: &Permit, release: u64, old: &AssetObject) -> Result<()> {
        let writer = self.guard_view(Some(permit));
        for listing in 0..3 {
            if listing > 0 {
                self.gh.fx().sleep(std::time::Duration::from_secs(1))?;
            }
            let listed = match writer.assets_of(release) {
                // A release that is gone holds no asset.
                Err(error) if error.kind == Kind::NotFound => return Ok(()),
                listed => listed?,
            };
            if !listed.iter().any(|a| a.id == old.id) {
                return Ok(());
            }
        }
        Err(self.guard_failed(format!(
            "{} (asset {}) is still listed after its DELETE",
            old.name, old.id
        )))
    }

    /// Clobber's step 3: `PATCH …/releases/assets/{id}` to `{"name":name}`, Converging, its
    /// guard re-listing; an asset `name` with `ours` → Converged. The asset and the count.
    fn rename(
        &self,
        permit: &Permit,
        release: u64,
        staged: &AssetObject,
        name: &str,
        ours: Sha256Digest,
    ) -> Result<(AssetObject, u32)> {
        let writer = self.guard_view(Some(permit));
        let call = self.write_call(
            Method::Patch,
            format!("{}/releases/assets/{}", repo_root(&self.slug), staged.id),
            json_body(&json!({ "name": name }))?,
        );
        let mut others = 0u32;
        let mut look = |_: Look| -> Result<Option<AssetObject>> {
            let assets = writer.assets_of(release)?;
            others = count_of(
                assets
                    .iter()
                    .filter(|a| a.name != name && a.id != staged.id)
                    .count(),
            );
            for asset in assets.iter().filter(|a| a.name == name) {
                if asset.holds(ours)? {
                    return Ok(Some(asset.clone()));
                }
            }
            Ok(None)
        };
        let asset = match self.converge(WriteOp::AssetRename, &call, permit, &mut look)? {
            Settles::Found(asset, _) => asset,
            Settles::Answered(answered) => {
                answered.json().and_then(from_json_owned).map_err(|e| {
                    e.with_side_effect(crate::SideEffect::Applied)
                        .with_repo(self.slug.to_string())
                })?
            }
        };
        Ok((asset, others.saturating_add(1)))
    }

    /// Conflict `sub` with `why`, naming this repository.
    fn conflict(&self, sub: Sub, why: String) -> Error {
        Error::new(Kind::Conflict, why)
            .with_sub(sub)
            .with_repo(self.slug.to_string())
    }

    /// Conflict `GuardFailed { what }`, `what` redacted.
    pub(crate) fn guard_failed(&self, what: String) -> Error {
        let what = self.gh.redactor().apply(&what);
        self.conflict(Sub::GuardFailed { what: what.clone() }, what)
    }

    /// Conflict `AmbiguousTag` over `ids`.
    fn ambiguous(&self, tag: &str, ids: Vec<u64>) -> Error {
        use std::fmt::Write as _;
        let mut why = format!("{} releases hold tag {tag}: ", ids.len());
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                why.push_str(", ");
            }
            let _ = write!(why, "{id}");
        }
        self.conflict(Sub::AmbiguousTag { ids }, why)
    }

    /// The publish guard's judgement of `release`, whose assets are `seen`, against `want`
    /// (`publish`'s guard, and `--dry-run`'s prediction of it): published as `patch` would
    /// leave it → `Some(release)`, converged; a draft holding exactly `want`, every asset
    /// uploaded → `None`, send.
    ///
    /// # Errors
    ///
    /// Conflict `GuardFailed { what }` for a release published otherwise, a draft whose
    /// assets are not `want` (what was added, is missing or changed), or one holding an asset
    /// not yet uploaded.
    pub(crate) fn publish_judgement(
        &self,
        release: ReleaseObject,
        seen: &AssetSet,
        want: &AssetSet,
        patch: &PublishPatch,
    ) -> Result<Option<ReleaseObject>> {
        let id = release.id;
        if !release.draft {
            if seen == want && release.prerelease == patch.prerelease {
                return Ok(Some(release));
            }
            return Err(self.guard_failed(format!(
                "release {id} is published, not as this publish would leave it"
            )));
        }
        if seen != want {
            return Err(self.guard_failed(set_change(id, want, seen)));
        }
        if let Some(asset) = release.assets.iter().find(|a| !a.is_uploaded()) {
            return Err(self.guard_failed(format!("{} is {}", asset.name, asset.state)));
        }
        Ok(None)
    }

    /// Invalid: release `id` holds `ASSETS_MAX` assets, so an upload adding one is refused
    /// by attempt 1's guard.
    pub(crate) fn full(&self, id: u64) -> Error {
        Error::new(
            Kind::Invalid,
            format!("release {id} holds {ASSETS_MAX} assets, GitHub's most"),
        )
        .with_repo(self.slug.to_string())
    }

    /// Conflict `AlreadyExists`: `name` is on release `id` with the digest `theirs`, not
    /// `ours` (`Replace::Never`).
    pub(crate) fn held_otherwise(
        &self,
        name: &str,
        id: u64,
        theirs: Option<Sha256Digest>,
        ours: Sha256Digest,
    ) -> Error {
        let theirs = theirs.map_or_else(|| "no digest".to_string(), |d| d.to_string());
        self.conflict(
            Sub::AlreadyExists,
            format!("{name} is on release {id} with {theirs}, not {ours}"),
        )
    }

    /// Conflict `GuardFailed`: an asset `name` of release `release` is still `state`, most
    /// likely another uploader's upload in progress, with the step that clears it once none
    /// runs.
    pub(crate) fn upload_in_progress(
        &self,
        name: &str,
        release: &ReleaseObject,
        state: &str,
    ) -> Error {
        self.guard_failed(format!(
            "another upload of {name} is in progress on release {} ({state})",
            release.id
        ))
        .with_next(format!(
            "once no upload of {name} is running, run: agh asset delete {} {} {name} --confirm \
             {name}",
            self.slug, release.tag_name
        ))
    }

    /// Invalid when a clobber's `temporary` name breaks the asset-name rule (its length
    /// included), which the clobber judges before it writes.
    pub(crate) fn temporary_fault(&self, temporary: &str) -> Option<Error> {
        asset_name_fault(temporary).map(|what| {
            Error::new(
                Kind::Invalid,
                format!("{temporary}: the clobber's temporary name: {what}"),
            )
            .with_repo(self.slug.to_string())
        })
    }

    /// Refused `immutable` for release `id` (design §11.2).
    pub(crate) fn immutable(&self, id: u64) -> Error {
        Error::refused(format!(
            "release {id} is immutable: agh never changes its assets"
        ))
        .with_sub(Sub::Immutable { id })
        .with_next("publish the files under a new tag")
        .with_repo(self.slug.to_string())
    }

    /// The open-intent rule's `Outcome::Unknown` error for `intent` of `tag`.
    pub(crate) fn still_open(&self, intent: &IntentId, tag: &str) -> Error {
        Error::new(
            Kind::OutcomeUnknown,
            format!("intent {intent} for {tag} is still open: agh never posts twice"),
        )
        .with_sub(Sub::IntentOpen {
            intent: intent.to_string(),
        })
        .with_next(format!("agh intent show {} {intent}", self.slug))
        .with_repo(self.slug.to_string())
    }

    /// The error a settled `NeverSent` or `Refused` came from.
    fn wire_error(&self, result: Result<Answered>) -> Error {
        match result {
            Err(error) => error,
            Ok(answered) => Error::protocol(format!(
                "GitHub answered {} and the settle table read no answer",
                answered.status
            )),
        }
        .with_repo(self.slug.to_string())
    }
}

/// The answer `call_write_guarded` ends with when a guard found the state already right;
/// `converge` hands back the guard's own object instead.
fn guard_answer() -> Answered {
    Answered {
        status: 200,
        headers: Vec::new(),
        body: Vec::new(),
        as_: crate::error::IdentityRef::Anonymous,
        request_id: None,
        etag: None,
        moved: None,
        requests: 0,
        rate: None,
    }
}

/// Whether `error` came from a 404.
fn is_404(error: &Error) -> bool {
    error.http.as_ref().is_some_and(|http| http.status == 404)
}

/// `error` raised after GitHub answered a write with a 2xx: `side_effect` applied, unless it
/// already says a write may have taken effect (error.rs: `Applied` after a write that landed).
fn applied(error: Error) -> Error {
    if error.side_effect == crate::SideEffect::None {
        error.with_side_effect(crate::SideEffect::Applied)
    } else {
        error
    }
}

/// `error`, saying at least `effect`, what an earlier write of the same call may have done:
/// `Applied` makes it `applied`, `Unknown` makes a `None` unknown, and `None` leaves it.
fn took(error: Error, effect: crate::SideEffect) -> Error {
    match effect {
        crate::SideEffect::Applied => applied(error),
        crate::SideEffect::Unknown if error.side_effect == crate::SideEffect::None => {
            error.with_side_effect(crate::SideEffect::Unknown)
        }
        crate::SideEffect::Unknown | crate::SideEffect::None => error,
    }
}

/// A count as the notice carries it.
fn count_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// `value` as compact JSON with sorted keys.
fn json_body(value: &Value) -> Result<CallBody> {
    super::encode_body(value).map(CallBody::Json)
}

/// Invalid for notes over 125,000 characters.
pub(crate) fn notes_bound(notes: &str) -> Result<()> {
    let n = notes.chars().count();
    if n > NOTES_MAX {
        return Err(Error::new(
            Kind::Invalid,
            format!("the notes hold {n} characters; GitHub takes at most {NOTES_MAX}"),
        ));
    }
    Ok(())
}

/// The created release's id and `html_url`, which `settle_table` records.
fn created_release(answered: &Answered) -> Option<(u64, String)> {
    let value = answered.json().ok()?;
    let id = value.get("id")?.as_u64()?;
    let url = value.get("html_url")?.as_str()?.to_string();
    Some((id, url))
}

/// The releases a `TagObjects` holds.
fn tag_objects(objects: &TagObjects) -> &[ReleaseObject] {
    match objects {
        TagObjects::None => &[],
        TagObjects::One(release) => std::slice::from_ref(release.as_ref()),
        TagObjects::Many(several) => several.as_slice(),
    }
}

/// The newest open `ReleaseCreate` intent of `slug` (ASCII case-insensitive, as `begin`
/// compares it) whose key is `tag`. A store that holds another repository's intents never
/// lends one to this repository's rules.
fn newest_open(intents: &dyn IntentStore, slug: &Slug, tag: &str) -> Result<Option<Intent>> {
    Ok(intents
        .open()?
        .into_iter()
        .filter(|intent| {
            intent.op == WriteOp::ReleaseCreate && intent.key == tag && intent.slug == *slug
        })
        .max_by(|a, b| a.id.cmp(&b.id)))
}

/// The object a closed intent made: its settled `Created { id }`, else the last Observed
/// found `Some(id)`.
fn made_object(view: &IntentView) -> Option<u64> {
    if let Some(Settled::Created { id, .. }) = &view.settled {
        return Some(*id);
    }
    view.observed.iter().rev().find_map(|(found, _)| *found)
}

/// §9 body 2: `release` is what `intent` made: its tag, its body's digest, and its target
/// where the intent sent one (an intent with no target matches any, design §11.2).
fn matches(release: &ReleaseObject, intent: &Intent) -> bool {
    release.tag_name == intent.key
        && intent
            .target_commitish
            .as_ref()
            .is_none_or(|target| *target == release.target_commitish)
        && intent.body_digest.as_deref().and_then(Sha256Digest::parse)
            == Some(Sha256Digest::of(
                release.body.as_deref().unwrap_or("").as_bytes(),
            ))
}

/// §4.6.9's agreeing: `req` asks for what `intent` asked for: its tag, its body's digest, and
/// its target where the intent has one.
fn agrees(req: &DraftRequest, intent: &Intent) -> bool {
    req.tag_name == intent.key
        && intent
            .target_commitish
            .as_ref()
            .is_none_or(|target| req.target_commitish.as_ref() == Some(target))
        && intent.body_digest.as_deref().and_then(Sha256Digest::parse)
            == Some(Sha256Digest::of(req.body.as_bytes()))
}

/// A release's assets by name and digest.
pub(crate) fn asset_set(release: &ReleaseObject) -> Result<AssetSet> {
    release
        .assets
        .iter()
        .map(|asset| Ok((asset.name.clone(), asset.sha256()?)))
        .collect()
}

/// How a release's asset set differs from the expected one: each list's names in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssetChange {
    /// Names the release holds that were not expected.
    pub added: Vec<String>,
    /// Names expected that the release lacks.
    pub missing: Vec<String>,
    /// Names both hold, the release's under another digest (or none).
    pub changed: Vec<String>,
}

impl AssetChange {
    /// Whether the release holds exactly the expected set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.missing.is_empty() && self.changed.is_empty()
    }
}

/// How `seen`, a release's asset set, differs from `want`, the expected one. agh's publish
/// guard and atpkg-publish's `AghHost` each word it in their own refusal.
#[must_use]
pub fn asset_change(want: &AssetSet, seen: &AssetSet) -> AssetChange {
    let names = |set: &AssetSet, keep: &dyn Fn(&String, &Option<Sha256Digest>) -> bool| {
        set.iter()
            .filter(|(name, digest)| keep(name, digest))
            .map(|(name, _)| name.clone())
            .collect()
    };
    AssetChange {
        added: names(seen, &|name, _| !want.contains_key(name)),
        missing: names(want, &|name, _| !seen.contains_key(name)),
        changed: names(seen, &|name, digest| {
            want.get(name).is_some_and(|w| w != digest)
        }),
    }
}

/// What changed between the expected asset set and the one seen.
fn set_change(id: u64, want: &AssetSet, seen: &AssetSet) -> String {
    use std::fmt::Write as _;
    let change = asset_change(want, seen);
    let mut out = format!("draft {id}'s assets are not the expected set: ");
    let mut first = true;
    for (label, names) in [
        ("added", &change.added),
        ("missing", &change.missing),
        ("changed", &change.changed),
    ] {
        if !names.is_empty() {
            if !first {
                out.push_str("; ");
            }
            first = false;
            let _ = write!(out, "{label} {}", names.join(", "));
        }
    }
    out
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

/// The host of `AghEnsureOnce`'s fifteen waivers: the world's actions no agh function
/// performs (an answer, a lost answer, a listing that reveals, a crash) and the negative
/// controls. Empty, as `policy::route_wall_environment` is.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "NotSent",
        reason = "Bounded nondeterministic environment selection: the wire refused before a byte left; settle_create records it as NeverSent"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "AnswerCreated",
        reason = "Bounded nondeterministic environment selection: what GitHub answered the one POST"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "AnswerRefused",
        reason = "Bounded nondeterministic environment selection: what GitHub answered the one POST"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "LandedLost",
        reason = "Bounded nondeterministic environment selection: what GitHub answered the one POST"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "FailedLost",
        reason = "Bounded nondeterministic environment selection: what GitHub answered the one POST"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "Reveal",
        reason = "Bounded nondeterministic environment selection: the listing caught up with a draft it hid"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "Rediscover",
        reason = "Bounded nondeterministic environment selection: a later run may discover again"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "Crash",
        reason = "Bounded nondeterministic environment selection: the run ended before it recorded the answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "LandedAfterCrash",
        reason = "Bounded nondeterministic environment selection: the POST landed after the run that sent it ended"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "BuggySendWithoutIntent",
        reason = "Buggy=1 negative control only; the release protocol has no such branch: SendNeedsIntent is held by Send"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "BuggyRepostOnUnknown",
        reason = "Buggy=1 negative control only; the release protocol has no such branch: OnePostPerIntent is held by Begin"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "BuggyRefusedStaysOpen",
        reason = "Buggy=1 negative control only; the release protocol has no such branch: RefusedCloses is held by Settle"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "BuggyLostIsCreated",
        reason = "Buggy=1 negative control only; the release protocol has no such branch: CreatedNeedsObject is held by Settle"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "BuggyDiscoverAsReader",
        reason = "Buggy=1 negative control only; the release protocol has no such branch: DiscoveryIsWriterRead is held by Discover"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghEnsureOnce",
        action = "BuggyNoneCloses",
        reason = "Buggy=1 negative control only; the release protocol has no such branch: NoneFoundStaysOpen is held by Discover"
    )
)]
#[doc(hidden)]
pub fn ensure_once_environment() {}

#[cfg(all(test, unix))]
#[path = "ensure_once_conformance_tests.rs"]
mod ensure_once_conformance;
