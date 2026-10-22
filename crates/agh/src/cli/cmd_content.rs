// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The content verbs (design §10.2): `cat`, `ls` and `find`.

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::{Error, Kind, Result, Sub};
use crate::page::End;
use crate::policy::Access;

/// `cat`'s default `--max-bytes`: 32 MiB.
const MAX_BYTES: u64 = 33_554_432;

/// `agh cat REPO[@REF]:PATH [--max-bytes N]`: the file's bytes, exactly.
///
/// # Errors
///
/// Usage for no `:PATH` or a bad `--max-bytes`; the read's error.
pub(crate) fn cat(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named, at) = inv.content(gh.router())?;
    let Some(path) = at.path.as_deref() else {
        return Err(inv.usage("cat needs :PATH (REPO[@REF]:PATH)"));
    };
    let max = match inv.flag("--max-bytes") {
        None => MAX_BYTES,
        Some(text) => text
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| inv.usage("--max-bytes wants a positive integer"))?,
    };
    let how = inv.how(Access::Read, named);
    let bytes = gh
        .contents(&slug)
        .how(how)
        .raw(at.reference.as_deref(), path, max)?;
    Ok(Outcome {
        answer: Answer::Bytes(bytes),
        receipt: Receipt::for_repo(inv, &gh, &slug),
    })
}

/// `agh ls REPO[@REF][:PATH]`: one directory's entries, sorted by name, `--limit` after the
/// sort.
///
/// # Errors
///
/// The listing's error.
pub(crate) fn ls(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named, at) = inv.content(gh.router())?;
    let how = inv.how(Access::Read, named);
    let listing = gh.contents(&slug).how(how).list(
        at.reference.as_deref(),
        at.path.as_deref().unwrap_or(""),
        inv.global.partial,
    )?;
    let mut items: Vec<_> = listing.items.into_iter().map(|entry| entry.raw).collect();
    let completeness = match crate::page::cut(listing.end, &mut items, inv.global.limit) {
        End::Capped { .. } => Completeness::Partial {
            why: crate::github::contents::DIRECTORY_CAP_WHY.to_string(),
        },
        end => Completeness::of(end),
    };
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    receipt.pages = listing.pages;
    receipt.consistency = Some(listing.consistency);
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::ContentEntry,
            items,
            completeness,
        },
        receipt,
    })
}

/// `agh find REPO[@REF] [PREFIX] [--type blob|tree] [--match RE]`: the recursive tree's
/// entries, filtered and sorted by path, `--limit` after the sort.
///
/// # Errors
///
/// Usage for a `:PATH`, a bad `--type` or `--match`; Incomplete `TruncatedTree` when GitHub
/// truncated the tree (a `Partial` answer under `--partial`); the read's error.
pub(crate) fn find(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let (slug, named, at) = inv.content(gh.router())?;
    if at.path.is_some() {
        return Err(inv.usage("find takes REPO[@REF]; the path prefix is its second argument"));
    }
    let kind = match inv.flag("--type") {
        None => None,
        Some(kind @ ("blob" | "tree")) => Some(kind.to_string()),
        Some(_) => return Err(inv.usage("--type wants blob or tree")),
    };
    let pattern = inv
        .flag("--match")
        .map(aterm_regex::Regex::new)
        .transpose()
        .map_err(|e| inv.usage(format!("--match: {e}")))?;
    let prefix = inv
        .rest()
        .first()
        .map(|prefix| prefix.trim_end_matches('/').to_string());
    let how = inv.how(Access::Read, named);
    let tree = gh.git(&slug).how(how).tree(at.reference.as_deref(), true)?;
    let truncated_why = "GitHub truncated the tree: it lists at most 100,000 entries or 7 MB";
    if tree.truncated && !inv.global.partial {
        return Err(Error::new(Kind::Incomplete, truncated_why)
            .with_sub(Sub::TruncatedTree)
            .with_next("agh ls REPO@REF:DIR for one directory, or --partial")
            .with_repo(slug.to_string()));
    }
    let mut entries: Vec<_> = tree
        .tree
        .into_iter()
        .filter(|entry| {
            prefix.as_deref().is_none_or(|prefix| {
                prefix.is_empty()
                    || entry.path == prefix
                    || entry
                        .path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .filter(|entry| kind.as_deref().is_none_or(|kind| entry.kind == kind))
        .filter(|entry| {
            pattern
                .as_ref()
                .is_none_or(|pattern| pattern.is_match(&entry.path))
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    // A truncated tree (under `--partial`) stays Partial whatever the cut.
    let end = crate::page::cut(End::Complete, &mut entries, inv.global.limit);
    let completeness = if tree.truncated {
        Completeness::Partial {
            why: truncated_why.to_string(),
        }
    } else {
        Completeness::of(end)
    };
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    receipt.pages = 1;
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::TreeEntry,
            items: entries.into_iter().map(|entry| entry.raw).collect(),
            completeness,
        },
        receipt,
    })
}
