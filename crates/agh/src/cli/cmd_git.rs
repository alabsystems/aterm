// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The history verbs (design §10.2): `rev`, `log`, `compare`, `branches` and `tags`.

use aterm_json::{Value, json};

use super::args::Invocation;
use super::out::OutputOpts;
use super::project;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::client::Github;
use crate::error::{Error, Kind, Result, Sub};
use crate::github::CompareResult;
use crate::page::{End, Listing};
use crate::policy::Access;
use crate::slug::Slug;

/// `log`'s default `--limit`.
const LOG_LIMIT: usize = 30;

/// A listing's Items answer, each item its raw JSON, with the listing's pages and
/// consistency on the receipt.
fn items<T>(
    inv: &Invocation,
    gh: &Github,
    slug: &Slug,
    noun: Noun,
    listing: Listing<T>,
    raw: fn(T) -> Value,
    completeness: Completeness,
) -> Outcome {
    let mut receipt = Receipt::for_repo(inv, gh, slug);
    receipt.pages = listing.pages;
    receipt.consistency = Some(listing.consistency);
    Outcome {
        answer: Answer::Items {
            noun,
            items: listing.items.into_iter().map(raw).collect(),
            completeness,
        },
        receipt,
    }
}

/// `agh rev REPO@REF`: the commit the ref names.
///
/// # Errors
///
/// Usage for no `@REF` or a `:PATH`; NotFound for no such commit.
pub(crate) fn rev(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named, at) = inv.content(gh.router())?;
    let Some(reference) = at.reference.as_deref() else {
        return Err(inv.usage("rev needs REPO@REF"));
    };
    if at.path.is_some() {
        return Err(inv.usage("rev takes REPO@REF"));
    }
    let commit = gh
        .git(&slug)
        .how(inv.how(Access::Read, named))
        .resolve(reference)?;
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Commit,
            value: commit.raw,
        },
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

/// `agh log REPO[@REF] [--path P] [--since T] [--author L] [--limit N]`: the newest N commits.
///
/// # Errors
///
/// Usage for a `:PATH` or a bad `--since`; the walk's error.
pub(crate) fn log(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named, at) = inv.content(gh.router())?;
    if at.path.is_some() {
        return Err(inv.usage("log takes REPO[@REF]; name a path with --path"));
    }
    let since = inv.flag("--since");
    if since.is_some_and(|since| aterm_types::rfc3339::parse_utc(since).is_none()) {
        return Err(inv.usage("--since wants an RFC 3339 UTC time (2026-10-01T00:00:00Z)"));
    }
    let limit = inv.global.limit.unwrap_or(LOG_LIMIT);
    let listing = gh.git(&slug).how(inv.how(Access::Read, named)).log(
        at.reference.as_deref(),
        inv.flag("--path"),
        since,
        inv.flag("--author"),
        limit,
    )?;
    let completeness = match listing.end {
        End::Limited { .. } => Completeness::Complete,
        end => Completeness::of(end),
    };
    Ok(items(
        inv,
        &gh,
        &slug,
        Noun::Commit,
        listing,
        |commit| commit.raw,
        completeness,
    ))
}

/// The answer for refs that share no history.
fn unrelated(base: &str, head: &str) -> Value {
    json!({
        "ahead_by": 0u64,
        "base": base,
        "behind_by": 0u64,
        "head": head,
        "merge_base": null,
        "status": "unrelated",
        "total_commits": 0u64,
    })
}

/// `agh compare REPO BASE...HEAD`: compares `base` and `head`.
///
/// # Errors
///
/// Usage for a range without `...`; Incomplete `CompareCap`; the read's error.
pub(crate) fn compare(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let range = inv.rest().first().map(String::as_str).unwrap_or_default();
    let (base, head) = match range.split_once("...") {
        Some((base, head)) if !base.is_empty() && !head.is_empty() => (base, head),
        _ => return Err(inv.usage("compare wants BASE...HEAD (three dots)")),
    };
    let result = gh
        .git(&slug)
        .how(inv.how(Access::Read, named))
        .compare(base, head)?;
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    let value = match result {
        CompareResult::NoMergeBase { base, head } => unrelated(&base, &head),
        CompareResult::Compared(compared) => {
            let opts = OutputOpts::from_global(&inv.global);
            let mut caps = Vec::new();
            if compared.capped_commits() && project::wants(Noun::Compare, &opts, "commits") {
                caps.push(format!(
                    "GitHub lists at most 250 commits of a comparison; this one has {}",
                    compared.total_commits
                ));
            }
            if compared.capped_files() && project::wants(Noun::Compare, &opts, "files") {
                caps.push(format!(
                    "GitHub lists at most 300 files of a comparison, and this one lists {}",
                    compared.files.len()
                ));
            }
            if let Some(why) = caps.first() {
                if !inv.global.partial {
                    return Err(Error::new(Kind::Incomplete, why.clone())
                        .with_sub(Sub::CompareCap)
                        .with_repo(slug.to_string()));
                }
                receipt.partial(why.clone());
            }
            let mut value = compared.raw;
            if let Some(object) = value.as_object_mut() {
                object.insert("base".into(), Value::from(base));
                object.insert("head".into(), Value::from(head));
            }
            value
        }
    };
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Compare,
            value,
        },
        receipt,
    })
}

/// `agh branches REPO`: every branch, sorted by name.
///
/// # Errors
///
/// The walk's error.
pub(crate) fn branches(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let listing = gh
        .git(&slug)
        .how(inv.how(Access::Read, named))
        .branches(&inv.list_opts())?;
    let completeness = Completeness::of(listing.end);
    Ok(items(
        inv,
        &gh,
        &slug,
        Noun::Branch,
        listing,
        |branch| branch.raw,
        completeness,
    ))
}

/// `agh tags REPO`: every tag, sorted by name.
///
/// # Errors
///
/// The walk's error.
pub(crate) fn tags(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let listing = gh
        .git(&slug)
        .how(inv.how(Access::Read, named))
        .tags(&inv.list_opts())?;
    let completeness = Completeness::of(listing.end);
    Ok(items(
        inv,
        &gh,
        &slug,
        Noun::Tag,
        listing,
        |tag| tag.raw,
        completeness,
    ))
}
