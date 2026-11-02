// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write journal's verbs (design §6.8): `intent list`, `intent show` and
//! `intent retract`. Local: no network, no credential, no router. Unix only, as the journal
//! is (plan §1, rule 12).

use super::args::Invocation;
use super::{Ctx, Outcome};
use crate::error::Result;

/// `agh intent list [REPO] [--open]`: every journal's intents, ordered by repository, then
/// step; with REPO, that journal's alone.
///
/// # Errors
///
/// could_not_run with no state directory; Usage for a bad REPO; a journal's read errors. A
/// journal whose read ends in a fault ends the listing as `FailedWith`: the items read so far,
/// then Refused `journal_fault` (exit 1).
pub(crate) fn intent_list(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    #[cfg(unix)]
    {
        unix::list(ctx, inv)
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, inv);
        Err(crate::write::unix_only())
    }
}

/// `agh intent show REPO ID`: one intent's document.
///
/// # Errors
///
/// could_not_run with no state directory; Usage for a bad REPO or ID; NotFound (exit 6) for an
/// id the journal does not hold; Refused `journal_fault` for a damaged journal.
pub(crate) fn intent_show(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    #[cfg(unix)]
    {
        unix::show(ctx, inv)
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, inv);
        Err(crate::write::unix_only())
    }
}

/// `agh intent retract REPO ID --confirm ID`: an open intent retracted, its id typed again.
/// It takes no terminal step (owner ruling 2026-10-26 (a)): an agent retracts after
/// `agh intent show` and a look at GitHub, with or without a terminal.
///
/// # Errors
///
/// Refused `a retract needs --confirm {id}` when `--confirm` is not ID; `Journal::retract`'s
/// errors; `intent show`'s.
pub(crate) fn intent_retract(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    #[cfg(unix)]
    {
        unix::retract(ctx, inv)
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, inv);
        Err(crate::write::unix_only())
    }
}

/// The verbs' bodies, over the unix-only journal.
#[cfg(unix)]
mod unix {
    use aterm_json::{Map, Value};

    use super::super::args::Invocation;
    use super::super::verbs::Noun;
    use super::super::{Answer, Completeness, Ctx, Outcome, Receipt};
    use crate::config::Dirs;
    use crate::error::{Error, Kind, Result};
    use crate::journal::{self, IntentId, IntentView, Journal, JournalEnd, Settled};
    use crate::slug::Slug;

    /// `intent list`.
    pub(super) fn list(ctx: &Ctx, inv: &Invocation) -> Result<Outcome> {
        let dirs = state_dirs(ctx)?;
        let named = repo_text(inv).map(|text| repo(inv, text)).transpose()?;
        let slugs: Vec<Slug> = match &named {
            Some(slug) => vec![slug.clone()],
            None => journal::journals(&dirs)?
                .into_iter()
                .map(|(slug, _path)| slug)
                .collect(),
        };
        let only_open = inv.has("--open");
        let mut items = Vec::new();
        let mut receipt = Receipt::new(&inv.verb_name());
        // With REPO the listing is one repository's, and its receipt names it, as `show`'s
        // and `retract`'s do.
        receipt.repo = named.as_ref().map(Slug::key);
        for slug in &slugs {
            let journal = Journal::open(&dirs, slug, &ctx.fx)?;
            let (records, end) = journal.read()?;
            items.extend(
                journal::views(&records)
                    .iter()
                    .filter(|view| view.open || !only_open)
                    .map(document),
            );
            if let JournalEnd::Fault { at, why } = &end {
                receipt.complete = false;
                return Ok(Outcome {
                    answer: Answer::FailedWith {
                        value: Value::Array(items),
                        error: journal.fault(*at, why),
                    },
                    receipt,
                });
            }
        }
        Ok(Outcome {
            answer: Answer::Items {
                noun: Noun::Intent,
                items,
                completeness: Completeness::Complete,
            },
            receipt,
        })
    }

    /// `intent show`.
    pub(super) fn show(ctx: &Ctx, inv: &Invocation) -> Result<Outcome> {
        let dirs = state_dirs(ctx)?;
        let (slug, id) = repo_and_id(inv)?;
        let journal = Journal::open(&dirs, &slug, &ctx.fx)?;
        let view = held(&journal, &slug, &id)?;
        Ok(object(inv, &slug, &view))
    }

    /// `intent retract`.
    pub(super) fn retract(ctx: &Ctx, inv: &Invocation) -> Result<Outcome> {
        let dirs = state_dirs(ctx)?;
        let (slug, id) = repo_and_id(inv)?;
        if inv.flag("--confirm") != Some(id.as_str()) {
            return Err(Error::refused(format!("a retract needs --confirm {id}"))
                .with_next(format!(
                    "agh intent retract {} {id} --confirm {id}",
                    slug.key()
                ))
                .with_repo(slug.key()));
        }
        let journal = Journal::open(&dirs, &slug, &ctx.fx)?;
        journal.retract(&id)?;
        let view = held(&journal, &slug, &id)?;
        Ok(object(inv, &slug, &view))
    }

    /// agh's state directory, which every journal lives under.
    fn state_dirs(ctx: &Ctx) -> Result<Dirs> {
        ctx.dirs.clone().ok_or_else(|| {
            Error::could_not_run(
                "agh intent reads the write journals under agh's state directory, and there is \
                 none",
            )
        })
    }

    /// REPO's text: `-R`, or the first positional.
    fn repo_text(inv: &Invocation) -> Option<&str> {
        inv.global.repo.as_deref().or_else(|| inv.positional(0))
    }

    /// REPO, through `Slug::parse`; a bad one is Usage, next `agh help {verb}`.
    fn repo(inv: &Invocation, text: &str) -> Result<Slug> {
        Slug::parse(text).map_err(|e| inv.usage(e.into_data().why))
    }

    /// REPO and ID, each required; a bad one is Usage.
    fn repo_and_id(inv: &Invocation) -> Result<(Slug, IntentId)> {
        let text = repo_text(inv).ok_or_else(|| inv.usage("REPO is required"))?;
        let slug = repo(inv, text)?;
        let id_text = inv
            .rest()
            .first()
            .ok_or_else(|| inv.usage("ID is required"))?;
        let id = IntentId::parse(id_text).ok_or_else(|| {
            inv.usage(format!(
                "not an intent id: {id_text:?} (want STEP-HEX, as agh intent list prints it)"
            ))
        })?;
        Ok((slug, id))
    }

    /// The view of `id` (`Journal::view`, which refuses a damaged journal `journal_fault`):
    /// NotFound (exit 6), naming `slug` as `Journal::retract`'s does, when the journal holds
    /// no such intent.
    fn held(journal: &Journal, slug: &Slug, id: &IntentId) -> Result<IntentView> {
        journal.view(id)?.ok_or_else(|| {
            Error::new(
                Kind::NotFound,
                format!("intent {id} is not in {}", journal.path().display()),
            )
            .with_repo(slug.key())
        })
    }

    /// One intent's document as the answer.
    fn object(inv: &Invocation, slug: &Slug, view: &IntentView) -> Outcome {
        let mut receipt = Receipt::new(&inv.verb_name());
        receipt.repo = Some(slug.key());
        Outcome {
            answer: Answer::Object {
                noun: Noun::Intent,
                value: document(view),
            },
            receipt,
        }
    }

    /// The intent document (plan §8, body 12): thirteen sorted keys. The fingerprint and the
    /// body digest are not printed.
    pub(super) fn document(view: &IntentView) -> Value {
        let intent = &view.intent;
        let settled = view.settled.as_ref().map(|settled| match settled {
            Settled::Created { .. } => "created",
            Settled::NeverSent => "never_sent",
            Settled::Refused { .. } => "refused",
            Settled::Unknown { .. } => "unknown",
        });
        let observed: Vec<Value> = view
            .observed
            .iter()
            .map(|(found, at_secs)| {
                let mut row = Map::new();
                row.insert("at_secs".into(), Value::from(*at_secs));
                row.insert("found".into(), Value::from(*found));
                Value::Object(row)
            })
            .collect();
        let mut doc = Map::new();
        doc.insert("at_secs".into(), Value::from(intent.at_secs));
        doc.insert("grant".into(), Value::from(intent.grant.clone()));
        doc.insert("id".into(), Value::from(intent.id.as_str()));
        doc.insert("identity".into(), Value::from(intent.identity.as_str()));
        doc.insert("key".into(), Value::from(intent.key.as_str()));
        doc.insert("observed".into(), Value::Array(observed));
        doc.insert("op".into(), Value::from(intent.op.slug()));
        doc.insert("open".into(), Value::from(view.open));
        doc.insert("repo".into(), Value::from(intent.slug.key()));
        doc.insert("request".into(), Value::from(intent.request.as_str()));
        doc.insert("retracted".into(), Value::from(view.retracted));
        doc.insert("settled".into(), Value::from(settled));
        doc.insert(
            "target_commitish".into(),
            Value::from(intent.target_commitish.clone()),
        );
        Value::Object(doc)
    }
}

#[cfg(all(test, unix))]
#[path = "cmd_intent_tests.rs"]
mod tests;
