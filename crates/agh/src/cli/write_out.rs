// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The outcome a write verb prints (design §9.4).

use aterm_json::{Map, Value};

use crate::journal::IntentId;
use crate::slug::Slug;
use crate::write::WriteOp;

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
