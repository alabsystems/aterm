// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Repository release listing, lookup, and asset download operations.

use crate::client::{Accept, Github};
use crate::download::{self, Downloaded, Expect, Sink};
use crate::error::{Error, Forbid, Kind, Result, Sub};
use crate::github::{How, limit_after_sort, repo_root};
use crate::model::{AssetObject, ReleaseObject, from_json};
use crate::page::{LinkScope, ListOpts, Listing};
use crate::path;
use crate::policy::{Access, RouteTarget};
use crate::slug::Slug;

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
