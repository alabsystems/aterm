// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! GitHub repository metadata, permissions, and owner repository listings.

mod create;
pub(crate) use create::RepoCreateWrite;
pub use create::{CREATE_KEY, RepoCreatePermit, RepoVisibility};

use crate::client::Github;
use crate::error::{Error, Forbid, Kind, Result, Sub};
use crate::github::{How, limit_after_sort, repo_root};
use crate::model::{Permissions, RepoObject, UserObject, from_json};
use crate::page::{LinkScope, ListOpts, Listing};
use crate::policy::{Access, Decision, RouteTarget};
use crate::slug::Slug;

/// The route door's fact naming a profile's drafts verdict (`Fx::fact`).
const DRAFTS: &str = "drafts";
/// The routing fact `own_login` asks: the profile's login, empty when it has none.
const LOGIN: &str = "login";

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
            && let Decision::Send { profile, tier } = self.writer_decision(slug, &how)?
            && let Drafts::Unproven { vault } = self.drafts_verdict(&profile)?
        {
            return Err(Error::new(
                Kind::Forbidden,
                format!("as {profile}: a fine-grained token's drafts are unproven (design §6.4)"),
            )
            .with_sub(Sub::Forbid(Forbid::DraftsUnproven))
            .with_next(drafts_next(&vault, &profile, tier, slug))
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
                .get(format!("/users/{owner}"), target.clone()),
        )?;
        let user: UserObject = from_json(&who.json()?)?;
        let walk_opts = opts.with_limit(None);
        let listing = if user.kind == "Organization" {
            let first = self
                .how
                .get(format!("/orgs/{owner}/repos"), target)
                .query("type", kind.slug());
            crate::page::walk(
                self.gh,
                first,
                &walk_opts,
                &LinkScope::Owner { id: user.id },
            )?
        } else if let Some(profile) = self.own_login(owner, &target)? {
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
                .get(format!("/users/{owner}/repos"), target)
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
        listing.items.sort_by(|a, b| {
            a.full_name
                .bytes()
                .map(|c| c.to_ascii_lowercase())
                .cmp(b.full_name.bytes().map(|c| c.to_ascii_lowercase()))
        });
        limit_after_sort(&mut listing, opts.limit);
        Ok(listing)
    }

    /// The drafts gate's decision for `slug` under `how`, through the route door, so a tape
    /// holds it and a replay meets it.
    fn writer_decision(&self, slug: &Slug, how: &How) -> Result<Decision> {
        let route = how.route(RouteTarget::Repo(slug.clone()));
        self.gh.fx().route(&route, &mut || self.gh.decide(&route))
    }

    /// `profile`'s drafts verdict, through the route door's facts: a replay has no credential
    /// to judge and no router to read, so it reads the verdict the recording taped, the
    /// vault's profiles its step names included. The answer is `vouched`, `unproven`, or
    /// `unproven vault` and the sorted names of the profiles that name a vault token file
    /// when `profile` is one of them.
    ///
    /// # Errors
    ///
    /// `Fx::fact`'s (a replay's divergence, or a recording refused for a token shape).
    fn drafts_verdict(&self, profile: &str) -> Result<Drafts> {
        let verdict = self.gh.fx().fact(DRAFTS, profile, &mut || {
            if self.gh.drafts_vouched(profile) {
                return "vouched".to_string();
            }
            let names = vault_profiles(self.gh.router(), profile);
            if names.is_empty() {
                "unproven".to_string()
            } else {
                format!("unproven vault {}", names.join(" "))
            }
        })?;
        // Any answer but `vouched` is unproven: the gate fails closed.
        if verdict == "vouched" {
            return Ok(Drafts::Vouched);
        }
        let vault = verdict
            .strip_prefix("unproven vault ")
            .map(|names| names.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        Ok(Drafts::Unproven { vault })
    }

    /// The profile `owner`'s repositories are listed as through `/user/repos`: the routed
    /// profile whose login is `owner`. The decision comes through the route door and the
    /// profile's login through its facts, so a recording lists as the untaped run does and a
    /// replay, whose router holds no login, reads both off the tape.
    fn own_login(&self, owner: &str, target: &RouteTarget) -> Result<Option<String>> {
        let route = self.how.route(target.clone());
        let Decision::Send { profile, .. } =
            self.gh.fx().route(&route, &mut || self.gh.decide(&route))?
        else {
            return Ok(None);
        };
        let login = self.gh.fx().fact(LOGIN, &profile, &mut || {
            self.gh
                .router()
                .profile(&profile)
                .and_then(|p| p.login.clone())
                .unwrap_or_default()
        })?;
        Ok((!login.is_empty() && login.eq_ignore_ascii_case(owner)).then_some(profile))
    }
}

/// A profile's drafts verdict, as `Repos::drafts_verdict` reads it off the route door.
enum Drafts {
    /// Its drafts are vouched.
    Vouched,
    /// Unproven; `vault` names, sorted, every profile that names one of the vault's token
    /// files when this one does, else it is empty.
    Unproven { vault: Vec<String> },
}

/// The profiles of `router` that name one of the vault's token files, sorted, when `profile`
/// is one of them; else none.
fn vault_profiles(router: &crate::policy::Router, profile: &str) -> Vec<String> {
    use crate::identity::{Source, vault};
    let names_vault = |p: &crate::identity::Profile| match &p.source {
        Source::File(path) => vault::names_vault_path(path),
        Source::Env(_) | Source::Inline(_) => false,
    };
    if !router.profile(profile).is_some_and(names_vault) {
        return Vec::new();
    }
    let mut names: Vec<String> = router
        .profiles()
        .into_iter()
        .filter(|p| names_vault(p))
        .map(|p| p.name.clone())
        .collect();
    names.sort_unstable();
    names
}

/// The step for `profile`'s unvouched drafts: re-add it from its token file with `--drafts`
/// (plan §20 item 2). A profile that names one of the vault's token files (`vault`, every
/// such profile, empty when `profile` is not one) may be a builder lanes' agh.toml, which
/// atpkg-publish requires to route alone and refuses once it says `vault = true`, or an
/// agh.toml written before vault adoption. So the re-add comes first, and removing every such
/// profile, after which agh.toml says `vault = true` and adoption vouches each rotation's
/// token (design §6.10), is named only as the step for a file no lane reads.
fn drafts_next(vault: &[String], profile: &str, tier: crate::policy::Tier, slug: &Slug) -> String {
    let add = format!(
        "agh auth add {profile} --file PATH --tier {} --owner {} --write --drafts",
        tier.slug(),
        slug.owner()
    );
    if vault.is_empty() {
        return add;
    }
    let removes: Vec<String> = vault
        .iter()
        .map(|name| format!("agh auth remove {name}"))
        .collect();
    format!(
        "{add}; where no builder's lanes read this agh.toml, {} instead: agh.toml then says \
         vault = true, and agh adopts the vault's tokens, vouching each rotation's",
        removes.join("; ")
    )
}
