// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The repository verbs (design §10.2): `repo view` and `repo list`.

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::Result;
use crate::github::RepoListKind;
use crate::policy::{Access, Named};
use crate::slug::Slug;

/// `agh repo view REPO`: the repository.
///
/// # Errors
///
/// The repository argument's, then the read's.
pub(crate) fn repo_view(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let how = inv.how(Access::Read, named);
    let repo = gh.repos().how(how).get(&slug)?;
    let receipt = Receipt::for_repo(inv, &gh, &slug);
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Repo,
            value: repo.raw,
        },
        receipt,
    })
}

/// `agh repo list OWNER [--type all|public|private|forks|sources]`: the owner's
/// repositories, sorted by full name.
///
/// # Errors
///
/// Usage for a bad owner or `--type`; the listing's error.
pub(crate) fn repo_list(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let owner = inv.require(0, "OWNER")?;
    Slug::new(owner, "repo")?;
    let kind = match inv.flag("--type") {
        None => RepoListKind::All,
        Some(text) => RepoListKind::parse(text)
            .ok_or_else(|| inv.usage("--type wants all, public, private, forks or sources"))?,
    };
    let gh = ctx.github()?;
    // An owner listing names no repository, so it carries no literal; a read never looks at
    // `named` (plan §4.6.13).
    let how = inv.how(Access::Read, Named::Default);
    let listing = gh
        .repos()
        .how(how)
        .list_for_owner(owner, kind, &inv.list_opts())?;
    let mut receipt = Receipt::new(&inv.verb_name()).metered(&gh.meter());
    receipt.pages = listing.pages;
    receipt.consistency = Some(listing.consistency);
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::Repo,
            completeness: Completeness::of(listing.end),
            items: listing.items.into_iter().map(|repo| repo.raw).collect(),
        },
        receipt,
    })
}
