// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! GitHub repository metadata, permissions, and owner repository listings.

use crate::client::Github;
use crate::error::{Error, Forbid, Kind, Result, Sub};
use crate::github::{How, limit_after_sort, repo_root};
use crate::model::{Permissions, RepoObject, UserObject, from_json};
use crate::page::{LinkScope, ListOpts, Listing};
use crate::path;
use crate::policy::{Access, Decision, RouteTarget};
use crate::slug::Slug;

/// Repositories.
pub struct Repos<'a> {
    gh: &'a Github,
    how: How,
}

/// `repo list --type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepoListKind {
    /// Every repository.
    #[default]
    All,
    /// Public ones.
    Public,
    /// Private ones.
    Private,
    /// Forks.
    Forks,
    /// Not forks.
    Sources,
}

impl RepoListKind {
    /// `all`, `public`, `private`, `forks`, `sources`.
    #[must_use]
    pub fn parse(text: &str) -> Option<RepoListKind> {
        match text {
            "all" => Some(RepoListKind::All),
            "public" => Some(RepoListKind::Public),
            "private" => Some(RepoListKind::Private),
            "forks" => Some(RepoListKind::Forks),
            "sources" => Some(RepoListKind::Sources),
            _ => None,
        }
    }

    /// The kind as `parse` reads it.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            RepoListKind::All => "all",
            RepoListKind::Public => "public",
            RepoListKind::Private => "private",
            RepoListKind::Forks => "forks",
            RepoListKind::Sources => "sources",
        }
    }

    const fn keeps(self, repo: &RepoObject) -> bool {
        match self {
            RepoListKind::All => true,
            RepoListKind::Public => !repo.private,
            RepoListKind::Private => repo.private,
            RepoListKind::Forks => repo.fork,
            RepoListKind::Sources => !repo.fork,
        }
    }
}

impl<'a> Repos<'a> {
    /// `How::read()`.
    pub(crate) fn new(gh: &'a Github) -> Repos<'a> {
        Repos {
            gh,
            how: How::read(),
        }
    }

    /// The same repositories under `how`.
    #[must_use]
    pub fn how(self, how: How) -> Repos<'a> {
        Repos { how, ..self }
    }

    /// `GET /repos/{o}/{r}`.
    /// # Errors
    /// The call's error.
    pub fn get(&self, slug: &Slug) -> Result<RepoObject> {
        self.read(slug, &self.how)
    }

    fn read(&self, slug: &Slug, how: &How) -> Result<RepoObject> {
        let mut call = how.get(repo_root(slug), RouteTarget::Repo(slug.clone()));
        if how.access != Access::Read {
            call = call.follow_moved(false);
        }
        let answered = self.gh.call(&call)?;
        let repo: RepoObject = answered
            .json()
            .and_then(|value| from_json(&value))
            .map_err(|e| e.with_repo(slug.to_string()))?;
        self.gh.remember_repo_id(slug, repo.id);
        Ok(repo)
    }

    /// Reads the repository and the identity's permissions on it under `access`.
    /// # Errors
    /// The call's error; Forbidden `drafts_unproven` for an unvouched fine-grained writer;
    /// Forbidden `other` when a non-read answer carries no permissions block.
    pub fn permissions(&self, slug: &Slug, access: Access) -> Result<(RepoObject, Permissions)> {
        let how = self.how.with_access(access);
        if access == Access::WriterRead
            && let Decision::Send { profile, tier } =
                self.gh.decide(&how.route(RouteTarget::Repo(slug.clone())))
            && !self.gh.drafts_vouched(&profile)
        {
            return Err(Error::new(
                Kind::Forbidden,
                format!("as {profile}: a fine-grained token's drafts are unproven (design §6.4)"),
            )
            .with_sub(Sub::Forbid(Forbid::DraftsUnproven))
            .with_next(format!(
                "a person runs: agh auth add {profile} --file PATH --tier {} --owner {} --write --drafts",
                tier.slug(),
                slug.owner()
            ))
            .with_repo(slug.to_string()));
        }
        let repo = self.read(slug, &how)?;
        let permissions = match (&repo.permissions, access) {
            (Some(permissions), _) => permissions.clone(),
            (None, Access::Read) => Permissions {
                pull: true,
                ..Permissions::default()
            },
            (None, _) => {
                return Err(
                    Error::new(Kind::Forbidden, "the answer carries no permissions block")
                        .with_sub(Sub::Forbid(Forbid::Other))
                        .with_repo(slug.to_string()),
                );
            }
        };
        Ok((repo, permissions))
    }

    /// Lists an owner's repositories sorted case-insensitively by full name.
    /// # Errors
    /// Usage error for an invalid `owner` before any request, or request failure.
    pub fn list_for_owner(
        &self,
        owner: &str,
        kind: RepoListKind,
        opts: &ListOpts,
    ) -> Result<Listing<RepoObject>> {
        Slug::new(owner, "repo")?;
        let target = RouteTarget::Owner(owner.to_string());
        let who = self.gh.call(
            &self
                .how
                .as_read()
                .get(format!("/users/{}", path::segment(owner)), target.clone()),
        )?;
        let user: UserObject = from_json(&who.json()?)?;
        let walk_opts = opts.with_limit(None);
        let listing = if user.kind == "Organization" {
            let first = self
                .how
                .get(format!("/orgs/{}/repos", path::segment(owner)), target)
                .query("type", kind.slug());
            crate::page::walk(
                self.gh,
                first,
                &walk_opts,
                &LinkScope::Owner { id: user.id },
            )?
        } else if let Some(profile) = self.own_login(owner, &target) {
            let how = self.how.with_access(Access::Read).as_profile(Some(profile));
            let mut first = how
                .get("/user/repos", RouteTarget::Identity)
                .query("affiliation", "owner");
            match kind {
                RepoListKind::Public => first = first.query("visibility", "public"),
                RepoListKind::Private => first = first.query("visibility", "private"),
                RepoListKind::All | RepoListKind::Forks | RepoListKind::Sources => {}
            }
            crate::page::walk(self.gh, first, &walk_opts, &LinkScope::Exact)?
        } else {
            let first = self
                .how
                .get(format!("/users/{}/repos", path::segment(owner)), target)
                .query("type", "owner");
            crate::page::walk(
                self.gh,
                first,
                &walk_opts,
                &LinkScope::Owner { id: user.id },
            )?
        };
        let mut listing = listing.map(|value| from_json::<RepoObject>(&value))?;
        listing.items.retain(|repo| kind.keeps(repo));
        listing
            .items
            .sort_by_cached_key(|repo| repo.full_name.to_ascii_lowercase());
        limit_after_sort(&mut listing, opts.limit);
        Ok(listing)
    }

    fn own_login(&self, owner: &str, target: &RouteTarget) -> Option<String> {
        let Decision::Send { profile, .. } = self.gh.decide(&self.how.route(target.clone())) else {
            return None;
        };
        let login = self.gh.router().profile(&profile)?.login.clone()?;
        login.eq_ignore_ascii_case(owner).then_some(profile)
    }
}
