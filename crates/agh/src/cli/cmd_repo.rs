// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Repository views, cloning and private main snapshots.

use std::path::PathBuf;

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::Result;
use crate::git::GitRun;
use crate::github::RepoListKind;
use crate::policy::{Access, Named};
use crate::slug::Slug;

/// Create a public User-owned repository after literal/grant routing and owner proof.
pub(crate) fn repo_create(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    use super::cmd_release_write::{Write, journaled, one_shot_answer};
    use super::write_out::done;
    use crate::github::repos::{CREATE_KEY, RepoVisibility};
    use crate::write::WriteOp;

    if !inv.has("--public") {
        return Err(inv.usage("--public is required; only public User-owned creation is supported"));
    }
    let dry = inv.has("--dry-run");
    let gh = if dry {
        ctx.github_recorded()?
    } else {
        ctx.github()?
    };
    let (slug, named) = inv.repo(gh.router())?;
    journaled(inv, WriteOp::RepoCreate, "repo", &slug.key())?;
    let how = inv.how(Access::Write, named);
    let w = Write {
        inv,
        gh,
        slug,
        how,
        dry,
    };
    let report = w.report(WriteOp::RepoCreate, CREATE_KEY);
    w.finish(report, |report| {
        let repos = w.gh.repos();
        let permit = repos.permit_create(
            &w.how
                .route(crate::policy::RouteTarget::Repo(w.slug.clone())),
            RepoVisibility::Public,
        )?;
        if dry {
            #[cfg(unix)]
            let latest = if w.gh.fx().mode().is_taped() {
                use crate::journal::IntentStore as _;
                w.gh.journal(&w.slug)?
                    .latest(WriteOp::RepoCreate, CREATE_KEY)?
            } else {
                w.gh.dirs()
                    .map(|dirs| {
                        crate::journal::peek_latest(dirs, &w.slug, WriteOp::RepoCreate, CREATE_KEY)
                    })
                    .transpose()?
                    .flatten()
            };
            #[cfg(not(unix))]
            let latest = None;
            let found = repos.preview_create(&permit, latest.as_ref())?;
            report.outcome = found
                .as_ref()
                .map_or("would_send", |found| found.effect.slug())
                .into();
            if let Some(found) = found {
                let repo = found.object;
                report.id = Some(repo.id);
                report.url = repo
                    .raw
                    .get("html_url")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            return Ok(done(report.clone()));
        }
        let journal = w.gh.journal(&w.slug)?;
        let outcome = repos.create(&permit, &journal)?;
        Ok(one_shot_answer(
            report,
            WriteOp::RepoCreate,
            &journal,
            outcome,
            |repo| {
                (
                    Some(repo.id),
                    repo.raw
                        .get("html_url")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                )
            },
        ))
    })
}

/// `agh repo view REPO`.
///
/// # Errors
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

/// `agh repo list OWNER [--type all|public|private|forks|sources]`.
///
/// # Errors
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

/// The most `--depth` takes.
const DEPTH_MAX: u32 = 1_000_000;

/// `agh repo clone REPO [DIR] [--depth N]`: a clone through the isolated git runner. `DIR`
/// defaults to the repository's name in the current directory, and must be absent or empty.
/// It prints nothing; the receipt (`--meta`) notes `as={profile}`, or `as=anonymous`: the
/// identity the git run spoke as, its one `as=` key. The run routes as agh.toml says (the read
/// chain, or the writer for a release-tier owner), so `--anonymous` and `--as` are Usage
/// rather than flags it would silently not honour.
///
/// # Errors
///
/// Usage for `--anonymous` or `--as`, the repository, a `DIR` that is not empty, or a
/// `--depth` outside 1 to 1,000,000 or combined with `--mirror`;
/// the runner's refusals and git's failure.
pub(crate) fn repo_clone(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let mirror = inv.has("--mirror");
    if mirror && inv.flag("--depth").is_some() {
        return Err(inv.usage("--mirror needs complete history: drop --depth"));
    }
    if inv.global.anonymous || inv.global.as_profile.is_some() {
        return Err(inv.usage(
            "repo clone speaks as agh.toml routes it (the read chain, or a release-tier owner's \
             writer): drop --anonymous and --as",
        ));
    }
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let depth = match inv.flag("--depth") {
        None => None,
        Some(text) => Some(
            crate::path::digits::<u32>(text)
                .filter(|n| (1..=DEPTH_MAX).contains(n))
                .ok_or_else(|| inv.usage("--depth wants 1 to 1000000"))?,
        ),
    };
    let dir = inv.rest().first().map_or_else(
        || {
            PathBuf::from(if mirror {
                format!("{}.git", slug.repo())
            } else {
                slug.repo().to_string()
            })
        },
        PathBuf::from,
    );
    let dir = std::path::absolute(&dir).map_err(|e| inv.usage(format!("DIR: {e}")))?;
    let empty = match std::fs::read_dir(&dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    if !empty {
        return Err(inv.usage(format!(
            "{} is there and not an empty directory",
            dir.display()
        )));
    }
    let mut run = if mirror {
        GitRun::new_mirror(&slug, &dir)
    } else {
        GitRun::new_clone(&slug, &dir)
    }
    .with_named(named);
    if let Some(depth) = depth {
        run = run.with_depth(depth);
    }
    let out = gh.run_git(&run)?;
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    // The meter's identity is what the client's own requests spoke as (vault adoption's
    // `GET /user`, say), not the git run's: the note below is the run's, and the one `as=`.
    receipt.as_ = None;
    receipt.notes.push((
        "as".to_string(),
        out.as_.unwrap_or_else(|| "anonymous".to_string()),
    ));
    Ok(Outcome {
        answer: Answer::Nothing,
        receipt,
    })
}

/// `agh repo fetch-main REPO DIR`: refresh agh's private main snapshot, with a reflog.
/// Branches, tags, remote-tracking refs, stash and working files are not moved.
///
/// # Errors
/// Usage for an identity override or absent directory; repository and runner refusals.
pub(crate) fn repo_fetch_main(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    const INTO: &str = "refs/agh/fetched-main";
    if inv.global.anonymous || inv.global.as_profile.is_some() {
        return Err(
            inv.usage("repo fetch-main speaks as agh.toml routes it: drop --anonymous and --as")
        );
    }
    let dir = inv
        .rest()
        .first()
        .ok_or_else(|| inv.usage("DIR is required"))?;
    let dir = std::path::absolute(dir).map_err(|e| inv.usage(format!("DIR: {e}")))?;
    if !dir.is_dir() {
        return Err(inv.usage("DIR must be an existing Git repository directory"));
    }
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let out = gh.run_git(&GitRun::fetch_main(&slug, &dir, INTO).with_named(named))?;
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    receipt.as_ = None;
    receipt
        .notes
        .push(("as".into(), out.as_.unwrap_or_else(|| "anonymous".into())));
    receipt.notes.push(("ref".into(), INTO.into()));
    Ok(Outcome {
        answer: Answer::Nothing,
        receipt,
    })
}

/// Fetch one exact source into an agh-owned local ref; anonymous mode is a
/// credential-free read, not a fallback from a failed authenticated operation.
pub(crate) fn repo_fetch(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    if inv.global.as_profile.is_some() {
        return Err(inv.usage("repo fetch uses routed credentials or --anonymous, not --as"));
    }
    let reference = inv
        .flag("--ref")
        .ok_or_else(|| inv.usage("--ref is required"))?;
    let into = inv
        .flag("--into")
        .ok_or_else(|| inv.usage("--into is required"))?;
    if !(crate::path::is_sha(reference)
        || (reference.starts_with("refs/") && crate::path::ref_name_ok(reference)))
    {
        return Err(inv.usage("--ref must be a full ref or forty lowercase hex digits"));
    }
    if !into.starts_with("refs/agh/") || !crate::path::ref_name_ok(into) {
        return Err(inv.usage("--into must be a valid ref beneath refs/agh/"));
    }
    let dir = inv
        .rest()
        .first()
        .ok_or_else(|| inv.usage("DIR is required"))?;
    let dir = std::path::absolute(dir).map_err(|e| inv.usage(format!("DIR: {e}")))?;
    if !dir.is_dir() {
        return Err(inv.usage("DIR must be an existing Git repository directory"));
    }
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let out = gh.run_git(
        &GitRun::fetch_ref(&slug, &dir, reference, into, inv.global.anonymous).with_named(named),
    )?;
    let mut receipt = Receipt::for_repo(inv, &gh, &slug);
    receipt.as_ = None;
    receipt
        .notes
        .push(("as".into(), out.as_.unwrap_or_else(|| "anonymous".into())));
    receipt.notes.push(("ref".into(), into.into()));
    receipt.notes.push(("source".into(), reference.into()));
    Ok(Outcome {
        answer: Answer::Nothing,
        receipt,
    })
}
