// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The outcome a write verb prints (design §9.4), and the one rule every write verb answers
//! by: `done`, `failed` and `outcome_of`.

use aterm_json::{Map, Value};

use super::Answer;
use super::verbs::Noun;
use crate::error::{Error, Kind, SideEffect};
use crate::journal::IntentId;
use crate::slug::Slug;
use crate::write::{self, Deleted, Effect, Ensured, WriteOp};

/// What a write verb prints: one object, keys sorted. No `Eq`: `aterm_json::Value` has none,
/// because a JSON number may be a float.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct WriteReport {
    /// `created`, `converged`, `unchanged`, `deleted`, `refused`, `unknown` or `not_sent`;
    /// `would_send` under `--dry-run`.
    pub outcome: String,
    /// `WriteOp::slug`.
    pub op: String,
    /// `{owner}/{repo}`.
    pub repo: String,
    /// The idempotency key: the tag, the asset's name, the ref, or `api {METHOD} {path}`.
    pub key: String,
    /// The object's id, when it has one.
    pub id: Option<u64>,
    /// Its `html_url`, or its API URL for a ref.
    pub url: Option<String>,
    /// The intent, for a one-shot write.
    pub intent: Option<String>,
    /// Whether `--dry-run` was given.
    pub dry_run: bool,
    /// GitHub's answer, for `agh api` only: its JSON body, or `{body, headers, status}` under
    /// `--include`. `None` (printed `null`) for every other verb, whose object is its id and url.
    pub answer: Option<Value>,
}

impl WriteReport {
    /// The report of `outcome` on `op` for `repo` and `key`.
    #[must_use]
    pub fn new(outcome: &str, op: WriteOp, repo: &Slug, key: &str) -> WriteReport {
        WriteReport {
            outcome: outcome.to_string(),
            op: op.slug().to_string(),
            repo: repo.to_string(),
            key: key.to_string(),
            id: None,
            url: None,
            intent: None,
            dry_run: false,
            answer: None,
        }
    }

    /// The same report with the object's id and URL.
    #[must_use]
    pub fn with_object(mut self, id: Option<u64>, url: Option<&str>) -> WriteReport {
        self.id = id;
        self.url = url.map(str::to_string);
        self
    }

    /// The same report with the intent.
    #[must_use]
    pub fn with_intent(mut self, intent: Option<&IntentId>) -> WriteReport {
        self.intent = intent.map(IntentId::to_string);
        self
    }

    /// The same report, marked `--dry-run`.
    #[must_use]
    pub fn dry_run(mut self) -> WriteReport {
        self.dry_run = true;
        self
    }

    /// The same report carrying GitHub's answer (`agh api`).
    #[must_use]
    pub fn with_answer(mut self, answer: Option<Value>) -> WriteReport {
        self.answer = answer;
        self
    }

    /// `{"answer":null,"dry_run":false,"id":5000,"intent":"0-…","key":"v1","op":"release.create","outcome":"created","repo":"alabsystems/aterm","url":"…"}`;
    /// an absent value is `null`, never left out. Its compact bytes are `aterm_json::to_string` of
    /// this value.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let text = |value: &Option<String>| value.as_deref().map_or(Value::Null, Value::from);
        let mut object = Map::new();
        object.insert("answer".into(), self.answer.clone().unwrap_or(Value::Null));
        object.insert("dry_run".into(), Value::from(self.dry_run));
        object.insert("id".into(), self.id.map_or(Value::Null, Value::from));
        object.insert("intent".into(), text(&self.intent));
        object.insert("key".into(), Value::from(self.key.as_str()));
        object.insert("op".into(), Value::from(self.op.as_str()));
        object.insert("outcome".into(), Value::from(self.outcome.as_str()));
        object.insert("repo".into(), Value::from(self.repo.as_str()));
        object.insert("url".into(), text(&self.url));
        Value::Object(object)
    }
}

/// A done write's answer: its report as a Write object (exit 0).
pub(crate) fn done(report: WriteReport) -> Answer {
    Answer::Object {
        noun: Noun::Write,
        value: report.to_value(),
    }
}

/// A refused, unknown or not-sent write's answer: its report on stdout, then `error`'s two
/// lines, with `error`'s own exit.
pub(crate) fn failed(report: WriteReport, error: Error) -> Answer {
    Answer::FailedWith {
        value: report.to_value(),
        error,
    }
}

/// The outcome a write's report prints (`WriteReport::outcome`), named once: the slugs come
/// from here, and `side_effect_of` reads a printed report back through `from_slug`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReportOutcome {
    /// `created`: this call's write changed GitHub.
    Created,
    /// `converged`: GitHub already held what an earlier attempt made.
    Converged,
    /// `unchanged`: the state was as asked before anything was sent.
    Unchanged,
    /// `deleted`: this call's DELETE removed it.
    Deleted,
    /// `would_send`: a dry run's step the real run would send.
    WouldSend,
    /// `not_sent`: nothing was sent.
    NotSent,
    /// `refused`: GitHub's answer or agh's own refusal.
    Refused,
    /// `unknown`: sent, and whether it landed is not known.
    Unknown,
}

impl ReportOutcome {
    /// Every outcome, in declaration order.
    const ALL: [ReportOutcome; 8] = [
        ReportOutcome::Created,
        ReportOutcome::Converged,
        ReportOutcome::Unchanged,
        ReportOutcome::Deleted,
        ReportOutcome::WouldSend,
        ReportOutcome::NotSent,
        ReportOutcome::Refused,
        ReportOutcome::Unknown,
    ];

    /// The slug a report prints.
    #[must_use]
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            ReportOutcome::Created => "created",
            ReportOutcome::Converged => "converged",
            ReportOutcome::Unchanged => "unchanged",
            ReportOutcome::Deleted => "deleted",
            ReportOutcome::WouldSend => "would_send",
            ReportOutcome::NotSent => "not_sent",
            ReportOutcome::Refused => "refused",
            ReportOutcome::Unknown => "unknown",
        }
    }

    /// The outcome whose slug is `slug`, if any.
    #[must_use]
    pub(crate) fn from_slug(slug: &str) -> Option<ReportOutcome> {
        Self::ALL.into_iter().find(|outcome| outcome.slug() == slug)
    }

    /// What a report of this outcome, not a dry run's, says GitHub holds: `Applied` for
    /// `created` and `deleted`, `Unknown` for `unknown`, `None` for the rest.
    #[must_use]
    pub(crate) const fn side_effect(self) -> SideEffect {
        match self {
            ReportOutcome::Created | ReportOutcome::Deleted => SideEffect::Applied,
            ReportOutcome::Unknown => SideEffect::Unknown,
            ReportOutcome::Converged
            | ReportOutcome::Unchanged
            | ReportOutcome::WouldSend
            | ReportOutcome::NotSent
            | ReportOutcome::Refused => SideEffect::None,
        }
    }

    /// The outcome a write's error prints in its report (plan §10, body 1.5): `Unknown` when
    /// the kind is OutcomeUnknown or `side_effect` is unknown or applied (a settle that failed
    /// after a 2xx: GitHub took that write, so it is not refused); `NotSent` when agh could
    /// not run and nothing was written; `Refused` for anything else, GitHub's answer or agh's
    /// own refusal after a read.
    #[must_use]
    pub(crate) fn of(error: &Error) -> ReportOutcome {
        if error.kind == Kind::OutcomeUnknown
            || matches!(error.side_effect, SideEffect::Unknown | SideEffect::Applied)
        {
            ReportOutcome::Unknown
        } else if error.kind == Kind::CouldNotRun {
            ReportOutcome::NotSent
        } else {
            ReportOutcome::Refused
        }
    }
}

impl From<Effect> for ReportOutcome {
    fn from(effect: Effect) -> ReportOutcome {
        match effect {
            Effect::Created => ReportOutcome::Created,
            Effect::Converged => ReportOutcome::Converged,
            Effect::Unchanged => ReportOutcome::Unchanged,
        }
    }
}

impl From<Deleted> for ReportOutcome {
    fn from(deleted: Deleted) -> ReportOutcome {
        match deleted {
            Deleted::Removed => ReportOutcome::Deleted,
            Deleted::Gone => ReportOutcome::Unchanged,
        }
    }
}

impl<T> From<&write::Outcome<Ensured<T>>> for ReportOutcome {
    /// A one-shot write's outcome: `Done` → its effect's; `NeverSent` → `NotSent`;
    /// `Refused` → `Refused`; `Unknown` → `Unknown`.
    fn from(outcome: &write::Outcome<Ensured<T>>) -> ReportOutcome {
        match outcome {
            write::Outcome::Done(ensured) => ensured.effect.into(),
            write::Outcome::NeverSent(_) => ReportOutcome::NotSent,
            write::Outcome::Refused(_) => ReportOutcome::Refused,
            write::Outcome::Unknown { .. } => ReportOutcome::Unknown,
        }
    }
}

/// The outcome a write's error prints in its report: `ReportOutcome::of(error)`'s slug.
pub(crate) fn outcome_of(error: &Error) -> &'static str {
    ReportOutcome::of(error).slug()
}

#[cfg(test)]
#[path = "write_out_tests.rs"]
mod tests;
