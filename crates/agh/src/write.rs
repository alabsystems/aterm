// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! What a write did (design §9.4, §10.2): the outcome every write method returns, the effect
//! an ensure-style write had, and the operations the journal names.

use crate::error::{Error, Kind, Result, SideEffect, Sub};
use crate::journal::{IntentId, Settled};

/// How a one-shot write ended (design §10.2's settle table).
#[derive(Debug)]
pub enum Outcome<T> {
    /// Done: the object as GitHub now holds it.
    Done(T),
    /// Nothing was sent (`Phase::NotSent`). Running it again is safe.
    NeverSent(Error),
    /// GitHub answered a 4xx. Nothing was created, the intent is closed, and a later run
    /// posts again.
    Refused(Error),
    /// Sent, and the answer was a 5xx or never came. The intent stays open, and only a
    /// WriterRead discovery or a person's `agh intent retract` closes it.
    Unknown {
        /// The open intent.
        intent: IntentId,
        /// What happened on the wire.
        error: Error,
    },
}

impl<T> Outcome<T> {
    /// `done`, `not_sent`, `refused` or `unknown`.
    #[must_use]
    pub fn slug(&self) -> &'static str {
        match self {
            Outcome::Done(_) => "done",
            Outcome::NeverSent(_) => "not_sent",
            Outcome::Refused(_) => "refused",
            Outcome::Unknown { .. } => "unknown",
        }
    }

    /// The object, when `Done`.
    #[must_use]
    pub fn done(self) -> Option<T> {
        match self {
            Outcome::Done(object) => Some(object),
            Outcome::NeverSent(_) | Outcome::Refused(_) | Outcome::Unknown { .. } => None,
        }
    }

    /// `Done(t)` → `Ok(t)`. `NeverSent(e)` and `Refused(e)` → `Err(e)`. `Unknown` → `Err` of
    /// kind OutcomeUnknown (exit 13), `sub` the wire error's, why
    /// `outcome unknown; intent {intent} open: {error.why}`, next
    /// `agh intent show {repo} {intent}`, or `agh intent list --open` when the error names no
    /// repo (`intent list` with no REPO reads every journal, so it finds the intent).
    ///
    /// # Errors
    ///
    /// As above.
    pub fn into_result(self) -> Result<T> {
        match self {
            Outcome::Done(object) => Ok(object),
            Outcome::NeverSent(error) | Outcome::Refused(error) => Err(error),
            Outcome::Unknown { intent, error } => {
                let why = format!("outcome unknown; intent {intent} open: {}", error.why);
                let next = match &error.repo {
                    Some(repo) => format!("agh intent show {repo} {intent}"),
                    None => "agh intent list --open".to_string(),
                };
                Err(error.recast(Kind::OutcomeUnknown, why).with_next(next))
            }
        }
    }

    /// The same outcome over `f(T)`.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Outcome<U> {
        match self {
            Outcome::Done(object) => Outcome::Done(f(object)),
            Outcome::NeverSent(error) => Outcome::NeverSent(error),
            Outcome::Refused(error) => Outcome::Refused(error),
            Outcome::Unknown { intent, error } => Outcome::Unknown { intent, error },
        }
    }
}

/// What an ensure-style write did to reach its object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// This call's write changed GitHub: it made, uploaded, published, edited or deleted.
    Created,
    /// GitHub already held what an earlier attempt of the same write made: the open intent's
    /// own draft, an asset with the same digest, or a publish a lost answer had made. Nothing
    /// was sent again.
    Converged,
    /// The state was as asked before anything was sent: a clobber of identical bytes, an
    /// edit to the values already there.
    Unchanged,
}

impl Effect {
    /// `created`, `converged`, `unchanged`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Effect::Created => "created",
            Effect::Converged => "converged",
            Effect::Unchanged => "unchanged",
        }
    }
}

/// An object and what the write did to reach it.
#[derive(Clone, Debug, PartialEq)]
pub struct Ensured<T> {
    /// The object as GitHub holds it.
    pub object: T,
    /// What the write did.
    pub effect: Effect,
}

/// What a delete did (design §10.2's key delete).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deleted {
    /// This call's DELETE removed it, and a re-read shows it gone.
    Removed,
    /// It was gone before: a 404, and a re-read that lists it nowhere.
    Gone,
}

impl Deleted {
    /// `deleted` for `Removed` and `unchanged` for `Gone`: the outcome slugs the delete verbs
    /// print. Design §9.4 lists `created`, `converged`, `unchanged` and `refused`; a removal is
    /// printed `deleted`, because `created` would read as its opposite.
    #[must_use]
    pub const fn outcome_slug(self) -> &'static str {
        match self {
            Deleted::Removed => "deleted",
            Deleted::Gone => "unchanged",
        }
    }
}

/// What a write does, as the journal names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum WriteOp {
    /// POST a draft release. One-shot.
    ReleaseCreate,
    /// PATCH a release's name, body or prerelease flag.
    ReleaseUpdate,
    /// PATCH a draft to published.
    ReleasePublish,
    /// DELETE a release.
    ReleaseDelete,
    /// POST an asset to the uploads origin.
    AssetUpload,
    /// DELETE an asset.
    AssetDelete,
    /// PATCH an asset's name (clobber's third step).
    AssetRename,
    /// POST a git ref. One-shot.
    RefCreate,
    /// Delete a ref through `git push --force-with-lease`.
    RefDelete,
    /// POST an SSH key to `/user/keys`.
    KeyAdd,
    /// DELETE an SSH key.
    KeyDelete,
    /// `agh api` with a write method. One-shot when the method is POST.
    Api,
}

/// Every op, in declaration order: `parse` reads its slugs from here.
const WRITE_OPS: [WriteOp; 12] = [
    WriteOp::ReleaseCreate,
    WriteOp::ReleaseUpdate,
    WriteOp::ReleasePublish,
    WriteOp::ReleaseDelete,
    WriteOp::AssetUpload,
    WriteOp::AssetDelete,
    WriteOp::AssetRename,
    WriteOp::RefCreate,
    WriteOp::RefDelete,
    WriteOp::KeyAdd,
    WriteOp::KeyDelete,
    WriteOp::Api,
];

impl WriteOp {
    /// `release.create`, `release.update`, `release.publish`, `release.delete`,
    /// `asset.upload`, `asset.delete`, `asset.rename`, `ref.create`, `ref.delete`,
    /// `key.add`, `key.delete`, `api`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            WriteOp::ReleaseCreate => "release.create",
            WriteOp::ReleaseUpdate => "release.update",
            WriteOp::ReleasePublish => "release.publish",
            WriteOp::ReleaseDelete => "release.delete",
            WriteOp::AssetUpload => "asset.upload",
            WriteOp::AssetDelete => "asset.delete",
            WriteOp::AssetRename => "asset.rename",
            WriteOp::RefCreate => "ref.create",
            WriteOp::RefDelete => "ref.delete",
            WriteOp::KeyAdd => "key.add",
            WriteOp::KeyDelete => "key.delete",
            WriteOp::Api => "api",
        }
    }

    /// The op `slug` names.
    #[must_use]
    pub fn parse(slug: &str) -> Option<WriteOp> {
        WRITE_OPS.into_iter().find(|op| op.slug() == slug)
    }
}

/// Every write's answer off unix (plan §1, rule 12): could_not_run (exit 3), `Sub::Local`, why
/// `agh writes need the write journal, which is unix-only in M2`. It is no refusal and has
/// no refusal code. Every write method's first step off unix.
#[must_use]
pub fn unix_only() -> Error {
    Error::could_not_run("agh writes need the write journal, which is unix-only in M2")
        .with_sub(Sub::Local)
}

/// Design §10.2's settle table, over one one-shot attempt's result. The one table every
/// one-shot write settles by (`Github::one_shot`, `Releases::create`):
/// - `Err` with `side_effect` none and no `http` (nothing was sent) → `Settled::NeverSent`;
/// - `Ok` → `Settled::Created` with `created`, or `Settled::Unknown`, why
///   `GitHub answered {status} with no object id`, when `created` is `None`;
/// - `Err` whose `http.status` is 400–499 → `Settled::Refused { status }`;
/// - any other `Err` (a 5xx, or `side_effect` unknown) → `Settled::Unknown { why }`, the
///   error's why through `Redactor::shapes`. M1 builds a why raw from GitHub's `message` and
///   the transport's detail and redacts only where it renders one, so this is where a why
///   bound for the journal is redacted. A caller holding a redactor (`Github::one_shot`,
///   `Releases::settle_create`) also passes it through `Redactor::apply` before
///   `intents.settle`.
#[must_use]
pub(crate) fn settle_table(
    result: &Result<crate::client::Answered>,
    created: Option<(u64, String)>,
) -> Settled {
    match result {
        Ok(answered) => match created {
            Some((id, url)) => Settled::Created { id, url },
            None => Settled::Unknown {
                why: format!("GitHub answered {} with no object id", answered.status),
            },
        },
        Err(error) if error.side_effect == SideEffect::None && error.http.is_none() => {
            Settled::NeverSent
        }
        Err(error) => match &error.http {
            Some(http) if (400..=499).contains(&http.status) => Settled::Refused {
                status: http.status,
            },
            _ => Settled::Unknown {
                why: crate::redact::Redactor::shapes(&error.why),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HttpRef;

    /// A 502 whose why carries `token`, as classify builds one from GitHub's `message`.
    fn bad_gateway(token: &str) -> Error {
        Error::new(
            Kind::OutcomeUnknown,
            format!("GitHub answered 502: {token}"),
        )
        .with_side_effect(SideEffect::Unknown)
        .with_http(HttpRef {
            status: 502,
            method: "POST",
            path: "/repos/fixture-dev/app/releases".to_string(),
            request_id: None,
        })
    }

    #[test]
    fn an_unknown_settle_never_carries_a_token_shaped_why() {
        for token in [agh_fake::token("7"), agh_fake::fine_grained_token("7")] {
            let settled = settle_table(&Err(bad_gateway(&token)), None);
            let Settled::Unknown { why } = settled else {
                panic!("a 502 settles Unknown: {settled:?}");
            };
            assert!(!why.contains(&token), "the journal's why holds the token");
            assert_eq!(why, crate::redact::Redactor::shapes(&why));
        }
    }

    #[test]
    fn an_unknown_outcome_without_a_repo_names_a_command_that_runs() {
        let intent = IntentId::parse("3-0123456789ab").expect("an id");
        let error = Error::new(Kind::OutcomeUnknown, "GitHub answered 502");
        let unknown = Outcome::<()>::Unknown {
            intent: intent.clone(),
            error: error.clone(),
        };
        let without = unknown.into_result().expect_err("unknown is an error");
        assert_eq!(without.next, "agh intent list --open");

        let with = Outcome::<()>::Unknown {
            intent,
            error: error.with_repo("fixture-dev/app"),
        }
        .into_result()
        .expect_err("unknown is an error");
        assert_eq!(with.next, "agh intent show fixture-dev/app 3-0123456789ab");
    }
}
