// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The ref verbs (design §9.4): `ref create` over the API, and `ref delete` as a leased
//! push through the git runner.

use super::args::Invocation;
use super::cmd_release_write::{
    Write, as_outcome, confirm, journaled, latest_intent, naming_intent, one_shot_answer,
};
use super::dry_run;
use super::write_out::done;
use super::{Ctx, Outcome};
use crate::error::Result;
use crate::model::RefObject;
use crate::write::WriteOp;

/// REF, under the reference rule (`refs/heads/…` or `refs/tags/…`, by a name git accepts).
fn reference(inv: &Invocation) -> Result<&str> {
    let reference = inv
        .rest()
        .first()
        .map(String::as_str)
        .ok_or_else(|| inv.usage("REF is required"))?;
    if crate::path::reference_ok(reference) {
        return Ok(reference);
    }
    Err(inv.usage(crate::path::reference_refusal(reference)))
}

/// `sha`, which must be 40 lowercase hex digits.
fn sha<'s>(inv: &Invocation, sha: &'s str) -> Result<&'s str> {
    if crate::path::is_sha(sha) {
        return Ok(sha);
    }
    Err(inv.usage(crate::path::sha_refusal(sha)))
}

/// `agh ref create REPO REF SHA` (plan §10, body 8): one journaled POST, a rerun at the same
/// sha converging. The report is op `ref.create`, key the reference, `id` null, `url` the
/// ref's API URL, and the intent.
///
/// # Errors
///
/// Usage for the reference and sha rules, and for a REF shaped like a credential
/// (`journaled`), and `Ctx::github`'s refusals (agh.toml, vault adoption), before the
/// repository is routed, with no report; Usage, with no report and nothing read, for a routed
/// repository whose lowercased key is shaped like a credential (`journaled`); every other
/// error after the repository is routed prints its report.
pub(crate) fn ref_create(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let reference = reference(inv)?;
    journaled(inv, WriteOp::RefCreate, "key", reference)?;
    let typed = inv
        .rest()
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| inv.usage("SHA is required"))?;
    let sha = sha(inv, typed)?;
    let w = Write::begin(ctx, inv)?;
    journaled(inv, WriteOp::RefCreate, "repo", &w.slug.key())?;
    let report = w.report(WriteOp::RefCreate, reference);
    w.finish(report, |report| {
        let permit = w.permit()?;
        if w.dry {
            return dry_run::ref_create(&w, reference, sha, report);
        }
        let journal = w.gh.journal(&w.slug)?;
        let before = latest_intent(&journal, WriteOp::RefCreate, reference);
        let outcome = w
            .gh
            .refs(&w.slug)
            .how(w.how.clone())
            .create(&permit, reference, sha, &journal)
            .map_err(|e| naming_intent(report, &journal, WriteOp::RefCreate, before.as_ref(), e))?;
        Ok(one_shot_answer(
            report,
            WriteOp::RefCreate,
            &journal,
            outcome,
            |found: &RefObject| (None, Some(found.url.clone())),
        ))
    })
}

/// `agh ref delete REPO REF --expect SHA --confirm REF` (plan §10, body 8): a push through
/// the git runner that deletes the ref only while it still names `--expect`. The report is
/// op `ref.delete`, key the reference, `id` and `url` null.
///
/// # Errors
///
/// Usage without `--expect`, and for the reference and sha rules; Refused for a `--confirm`
/// absent or not the REF, before anything is read; `Ctx::github`'s refusals (agh.toml, vault
/// adoption), before the repository is routed, with no report; every error after the
/// repository is routed prints its report, a repository that is not there (`Refs::push_check`,
/// which `Refs::delete_expect` and the dry run both make) included: it is refused, never a
/// delete with nothing to do.
pub(crate) fn ref_delete(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let reference = reference(inv)?;
    let Some(typed) = inv.flag("--expect") else {
        return Err(inv
            .usage("ref delete needs --expect SHA: agh deletes a ref only where it still points"));
    };
    let expect = sha(inv, typed)?;
    confirm(inv, reference)?;
    let w = Write::begin(ctx, inv)?;
    let report = w.report(WriteOp::RefDelete, reference);
    w.finish(report, |report| {
        let permit = w.permit()?;
        if w.dry {
            return dry_run::ref_delete(&w, reference, expect, report);
        }
        // `Refs::delete_expect` reads the repository first (design §6.4's push check), so a
        // repository that is not there is refused, never `Gone`.
        let deleted =
            w.gh.refs(&w.slug)
                .how(w.how.clone())
                .delete_expect(&permit, reference, expect)?;
        Ok(done(as_outcome(report.clone(), deleted)))
    })
}
