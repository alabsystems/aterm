// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `--fields` and the default projections (design §8.2, §8.3).

use std::collections::BTreeMap;

use aterm_json::{Map, Value};

use super::out::OutputOpts;
use super::verbs::Noun;
use crate::error::{Error, Result};

/// One field path: `tag_name`, `parent.full_name`, `assets[].name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldPath {
    /// Its steps, in order.
    pub steps: Vec<Step>,
}

/// One step of a field path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// An object's key.
    Key(String),
    /// Every item of an array (`[]`).
    Each,
}

/// Whether `text` is a KEY: `[A-Za-z0-9_]+`.
fn is_key(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl FieldPath {
    /// The path `text` writes: `KEY ("[]")? ("." KEY ("[]")?)*`, with KEY `[A-Za-z0-9_]+`.
    /// # Errors
    /// Usage: `not a field path: {text}`.
    pub fn parse(text: &str) -> Result<FieldPath> {
        let bad = || Error::usage(format!("not a field path: {text}"));
        let mut steps = Vec::new();
        for part in text.split('.') {
            let (key, each) = match part.strip_suffix("[]") {
                Some(key) => (key, true),
                None => (part, false),
            };
            if !is_key(key) {
                return Err(bad());
            }
            steps.push(Step::Key(key.to_string()));
            if each {
                steps.push(Step::Each);
            }
        }
        Ok(FieldPath { steps })
    }

    /// The path as written; the empty path, which only the output door builds, as `.`.
    #[must_use]
    pub fn text(&self) -> String {
        if self.steps.is_empty() {
            return ".".to_string();
        }
        let mut out = String::new();
        for step in &self.steps {
            match step {
                Step::Key(key) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(key);
                }
                Step::Each => out.push_str("[]"),
            }
        }
        out
    }

    /// The first key: the top-level name the path selects.
    fn head(&self) -> Option<&str> {
        match self.steps.first() {
            Some(Step::Key(key)) => Some(key),
            _ => None,
        }
    }
}

/// A comma-separated `--fields` list.
/// # Errors
/// `FieldPath::parse`'s: an empty piece is not a field path.
pub fn parse_fields(text: &str) -> Result<Vec<FieldPath>> {
    text.split(',')
        .map(|piece| FieldPath::parse(piece.trim()))
        .collect()
}

/// The nouns printed whole: no default projection.
pub(crate) fn is_whole(noun: Noun) -> bool {
    matches!(
        noun,
        Noun::Route | Noun::Raw | Noun::Wait | Noun::Meta | Noun::Policy
    )
}

/// The noun's default projection (design §8.3, and the plan's table).
#[must_use]
pub fn default_fields(noun: Noun) -> &'static [&'static str] {
    match noun {
        Noun::Release => &[
            "id",
            "tag_name",
            "name",
            "draft",
            "prerelease",
            "latest",
            "immutable",
            "target_commitish",
            "created_at",
            "published_at",
            "html_url",
            "repo",
            "assets[].id",
            "assets[].name",
            "assets[].size",
            "assets[].digest",
            "assets[].state",
            "assets[].content_type",
            "assets[].updated_at",
        ],
        Noun::ReleaseList => &[
            "tag_name",
            "draft",
            "prerelease",
            "latest",
            "published_at",
            "assets[].name",
        ],
        Noun::Asset => &[
            "id",
            "name",
            "size",
            "digest",
            "state",
            "content_type",
            "updated_at",
        ],
        Noun::ContentEntry => &["type", "name", "size", "sha"],
        Noun::TreeEntry => &["path", "type", "size"],
        Noun::Commit => &["sha", "date", "subject", "author"],
        Noun::Repo => &[
            "full_name",
            "default_branch",
            "private",
            "visibility",
            "fork",
            "parent.full_name",
            "pushed_at",
            "description",
        ],
        Noun::Branch | Noun::Tag => &["name", "sha"],
        Noun::Compare => &[
            "status",
            "ahead_by",
            "behind_by",
            "total_commits",
            "merge_base",
            "base",
            "head",
        ],
        Noun::Download => DOWNLOAD,
        Noun::Rate => RATE,
        Noun::Permission => PERMISSION,
        Noun::Identity => IDENTITY,
        Noun::User => &["login", "id", "type", "name"],
        Noun::Route | Noun::Raw | Noun::Wait | Noun::Meta | Noun::Policy => &[],
    }
}

/// Download's fields, every one of them derived.
const DOWNLOAD: &[&str] = &["path", "bytes", "sha256", "written", "digest", "asset_id"];
/// Rate's fields.
const RATE: &[&str] = &[
    "as",
    "source",
    "resource",
    "limit",
    "remaining",
    "reset",
    "used",
];
/// Permission's fields, every one of them derived.
const PERMISSION: &[&str] = &["repo", "as", "op", "granted", "permissions"];
/// Identity's fields, every one of them derived.
const IDENTITY: &[&str] = &[
    "profile",
    "tier",
    "source",
    "presence",
    "why",
    "fingerprint",
    "kind",
    "login",
    "remedy",
];

/// The noun's derived fields (computed by the verb, not GitHub's).
#[must_use]
pub fn derived_fields(noun: Noun) -> &'static [&'static str] {
    match noun {
        Noun::Release | Noun::ReleaseList => &["latest", "repo"],
        Noun::Commit => &["date", "subject", "author"],
        Noun::Branch | Noun::Tag => &["sha"],
        Noun::Compare => &["merge_base", "base", "head"],
        Noun::Download => DOWNLOAD,
        Noun::Rate => &["as", "source"],
        Noun::Permission => PERMISSION,
        Noun::Identity => IDENTITY,
        Noun::Asset
        | Noun::ContentEntry
        | Noun::TreeEntry
        | Noun::Repo
        | Noun::User
        | Noun::Route
        | Noun::Raw
        | Noun::Wait
        | Noun::Meta
        | Noun::Policy => &[],
    }
}

/// The default projection, parsed.
pub(crate) fn defaults(noun: Noun) -> Vec<FieldPath> {
    default_fields(noun)
        .iter()
        .filter_map(|text| FieldPath::parse(text).ok())
        .collect()
}

/// The projection of `value` onto the path tails in `paths` (each a path's steps from here).
fn project_at(value: &Value, paths: &[&[Step]]) -> Value {
    if paths.iter().any(|steps| steps.is_empty()) {
        return value.clone();
    }
    let each: Vec<&[Step]> = paths
        .iter()
        .filter_map(|steps| match steps.split_first() {
            Some((Step::Each, rest)) => Some(rest),
            _ => None,
        })
        .collect();
    let mut keyed: BTreeMap<&str, Vec<&[Step]>> = BTreeMap::new();
    for steps in paths {
        if let Some((Step::Key(key), rest)) = steps.split_first() {
            keyed.entry(key.as_str()).or_default().push(rest);
        }
    }
    match value {
        Value::Array(items) if !each.is_empty() => {
            Value::Array(items.iter().map(|item| project_at(item, &each)).collect())
        }
        Value::Object(map) if !keyed.is_empty() => Value::Object(
            keyed
                .into_iter()
                .map(|(key, rest)| {
                    let at = map.get(key).unwrap_or(&Value::Null);
                    (key.to_string(), project_at(at, &rest))
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

/// Project `value` onto `fields`.
#[must_use]
pub fn project(value: &Value, fields: &[FieldPath]) -> Value {
    let paths: Vec<&[Step]> = fields.iter().map(|f| f.steps.as_slice()).collect();
    match value {
        Value::Object(_) => project_at(value, &paths),
        _ => project_at(&Value::Object(Map::new()), &paths),
    }
}

/// The value at `path`: `Each` maps over an array, and a missing point is null.
pub(crate) fn value_at(value: &Value, path: &FieldPath) -> Value {
    value_at_steps(value, &path.steps)
}

fn value_at_steps(value: &Value, steps: &[Step]) -> Value {
    match steps.split_first() {
        None => value.clone(),
        Some((Step::Key(key), rest)) => match value.get(key) {
            Some(inner) => value_at_steps(inner, rest),
            None => Value::Null,
        },
        Some((Step::Each, rest)) => match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| value_at_steps(item, rest))
                    .collect(),
            ),
            _ => Value::Null,
        },
    }
}

/// Usage when a top-level name is in neither the noun's schema (its default and derived fields) nor
/// `sample`'s keys: `no field {name} in {noun}; fields: {sorted list}`.
/// # Errors
/// As above.
pub fn validate_fields(noun: Noun, fields: &[FieldPath], sample: Option<&Value>) -> Result<()> {
    let Some(sample) = sample else {
        return Ok(());
    };
    let mut names: Vec<String> = default_fields(noun)
        .iter()
        .chain(derived_fields(noun))
        .filter_map(|text| FieldPath::parse(text).ok())
        .filter_map(|path| path.head().map(str::to_string))
        .collect();
    if let Some(map) = sample.as_object() {
        names.extend(map.keys().cloned());
    }
    names.sort();
    names.dedup();
    for field in fields {
        let Some(head) = field.head() else { continue };
        if !names.iter().any(|name| name == head) {
            return Err(Error::usage(format!(
                "no field {head} in {}; fields: {}",
                noun.slug(),
                names.join(", ")
            )));
        }
    }
    Ok(())
}

/// Whether the verb must compute `field` (a derived field that costs a request).
#[must_use]
pub fn wants(noun: Noun, opts: &OutputOpts, field: &str) -> bool {
    let names = |text: &str| {
        FieldPath::parse(text.trim())
            .ok()
            .is_some_and(|path| path.head() == Some(field))
    };
    let in_fields = opts
        .fields
        .as_deref()
        .is_some_and(|fields| fields.split(',').any(names));
    let by_default = opts.fields.is_none()
        && opts.jq.is_none()
        && default_fields(noun).iter().any(|text| names(text));
    let in_jq = opts.jq.as_deref().is_some_and(|jq| jq.contains(field));
    in_fields || by_default || in_jq
}

/// The noun's pure derived fields, computed from GitHub's own keys.
fn pure_derived(noun: Noun, value: &Map) -> Vec<(&'static str, Value)> {
    let at = |path: &[&str]| -> Value {
        let mut here = value.get(path[0]);
        for key in &path[1..] {
            here = here.and_then(|v| v.get(key));
        }
        here.cloned().unwrap_or(Value::Null)
    };
    let string = |v: Value| match v {
        Value::String(_) => v,
        _ => Value::Null,
    };
    match noun {
        Noun::Commit => {
            let subject = match at(&["commit", "message"]) {
                Value::String(message) => {
                    Value::String(message.split('\n').next().unwrap_or("").to_string())
                }
                _ => Value::Null,
            };
            let author = match string(at(&["author", "login"])) {
                Value::Null => string(at(&["commit", "author", "name"])),
                login => login,
            };
            vec![
                ("date", string(at(&["commit", "committer", "date"]))),
                ("subject", subject),
                ("author", author),
            ]
        }
        Noun::Branch | Noun::Tag => vec![("sha", string(at(&["commit", "sha"])))],
        Noun::Compare => vec![("merge_base", string(at(&["merge_base_commit", "sha"])))],
        _ => Vec::new(),
    }
}

/// What `--fields` and the default projection read.
#[must_use]
pub fn with_derived(noun: Noun, value: &Value) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };
    let mut out = map.clone();
    for (key, derived) in pure_derived(noun, map) {
        out.insert(key.to_string(), derived);
    }
    Value::Object(out)
}

/// What `--jq` reads without `--fields`.
#[must_use]
pub fn jq_input(noun: Noun, value: &Value) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };
    let mut out = map.clone();
    for (key, derived) in pure_derived(noun, map) {
        out.entry(key.to_string()).or_insert(derived);
    }
    Value::Object(out)
}
