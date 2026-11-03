// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh schema` (design §9.3): the exit table, the nouns and the verbs, as JSON.

use aterm_json::{Value, json};

use super::project::{default_fields, derived_fields};
use super::verbs::{Class, Noun, Takes, VERBS, Verb};
use crate::error::{Error, Result, exit_rows};

/// `[{"exit","meaning","next","slug"}]`, exit 0 first.
#[must_use]
pub fn exit_table() -> Value {
    Value::Array(
        exit_rows()
            .into_iter()
            .map(|row| {
                json!({
                    "exit": row.exit,
                    "meaning": row.meaning,
                    "next": row.next,
                    "slug": row.slug,
                })
            })
            .collect(),
    )
}

const fn order(noun: Noun) -> Option<&'static str> {
    match noun {
        Noun::ReleaseList => Some("created_at desc, then id desc"),
        Noun::Asset | Noun::ContentEntry | Noun::Branch | Noun::Tag => Some("name asc"),
        Noun::TreeEntry => Some("path asc"),
        Noun::Commit => Some("GitHub's: newest first"),
        Noun::Repo => Some("full_name asc, case-insensitive"),
        Noun::Identity => Some("profile asc, then the ambient variables"),
        Noun::Rate => Some("profile asc, then anonymous; resource asc"),
        Noun::Intent => Some("repo asc, then step asc"),
        Noun::Release
        | Noun::User
        | Noun::Policy
        | Noun::Route
        | Noun::Compare
        | Noun::Raw
        | Noun::Download
        | Noun::Wait
        | Noun::Permission
        | Noun::Meta
        | Noun::Write
        | Noun::Credential => None,
    }
}

/// The keys of the documents agh itself writes, each with its JSON type: a write's report
/// (`write_out::WriteReport`), a journal intent (`agh intent show`) and the git helper's
/// answer (`agh whoami --git`). `None` for a noun whose document is GitHub's.
const fn keys(noun: Noun) -> Option<&'static [(&'static str, &'static str)]> {
    match noun {
        Noun::Write => Some(&[
            ("answer", "any|null"),
            ("dry_run", "boolean"),
            ("id", "number|null"),
            ("intent", "string|null"),
            ("key", "string"),
            ("op", "string"),
            ("outcome", "string"),
            ("repo", "string"),
            ("url", "string|null"),
        ]),
        Noun::Intent => Some(&[
            ("at_secs", "number"),
            ("grant", "string|null"),
            ("id", "string"),
            ("identity", "string"),
            ("key", "string"),
            ("observed", "array"),
            ("op", "string"),
            ("open", "boolean"),
            ("repo", "string"),
            ("request", "string"),
            ("retracted", "number|null"),
            ("settled", "string|null"),
            ("target_commitish", "string|null"),
        ]),
        Noun::Credential => Some(&[
            ("answer", "string"),
            ("grant", "string|null"),
            ("profile", "string|null"),
            ("repo", "string|null"),
            ("tier", "string|null"),
            ("why", "string|null"),
        ]),
        Noun::Release
        | Noun::ReleaseList
        | Noun::Asset
        | Noun::ContentEntry
        | Noun::TreeEntry
        | Noun::Commit
        | Noun::Branch
        | Noun::Tag
        | Noun::Repo
        | Noun::User
        | Noun::Identity
        | Noun::Rate
        | Noun::Policy
        | Noun::Route
        | Noun::Compare
        | Noun::Raw
        | Noun::Download
        | Noun::Wait
        | Noun::Permission
        | Noun::Meta => None,
    }
}

fn noun_row(noun: Noun) -> Value {
    let keys = keys(noun).map_or(Value::Null, |keys| {
        Value::Object(
            keys.iter()
                .map(|(name, kind)| ((*name).to_string(), Value::from(*kind)))
                .collect(),
        )
    });
    json!({
        "default_fields": default_fields(noun),
        "derived": derived_fields(noun),
        "keys": keys,
        "name": noun.slug(),
        "order": order(noun),
    })
}

/// `[{"default_fields","derived","keys","name","order"}]`.
#[must_use]
pub fn nouns() -> Value {
    Value::Array(Noun::ALL.into_iter().map(noun_row).collect())
}

const fn class_slug(class: Class) -> &'static str {
    match class {
        Class::Read => "read",
        Class::List => "list",
        Class::Local => "local",
        Class::Provision => "provision",
        Class::ByMethod => "by-method",
        Class::Write => "write",
        Class::Destructive => "destructive",
        Class::Git => "git",
    }
}

const fn takes_slug(takes: Takes) -> &'static str {
    match takes {
        Takes::None => "none",
        Takes::Value => "value",
        Takes::Repeat => "repeat",
    }
}

fn verb_row(verb: &Verb) -> Value {
    let flags = verb
        .flags
        .iter()
        .map(|flag| {
            json!({
                "name": flag.name,
                "short": flag.short.map(|c| c.to_string()),
                "takes": takes_slug(flag.takes),
            })
        })
        .collect::<Vec<Value>>();
    json!({
        "class": class_slug(verb.class),
        "flags": flags,
        "gh": verb.gh,
        "milestone": verb.milestone,
        "noun": verb.noun.slug(),
        "path": verb.path.join("."),
        "positionals": verb.positionals,
        "usage": verb.usage,
    })
}

/// Returns the verb schema rows as a JSON array.
#[must_use]
pub fn verbs() -> Value {
    Value::Array(VERBS.iter().map(verb_row).collect())
}

/// None → `{"exit","nouns","verbs"}`; `exit`, `nouns`, `verbs`; a noun's slug → its row.
///
/// # Errors
///
/// Usage: `no schema for {what}`, next `agh schema`.
pub fn schema(what: Option<&str>) -> Result<Value> {
    match what {
        None => Ok(json!({
            "exit": exit_table(),
            "nouns": nouns(),
            "verbs": verbs(),
        })),
        Some("exit") => Ok(exit_table()),
        Some("nouns") => Ok(nouns()),
        Some("verbs") => Ok(verbs()),
        Some(text) => match Noun::parse(text) {
            Some(noun) => Ok(noun_row(noun)),
            None => Err(Error::usage(format!("no schema for {text}")).with_next("agh schema")),
        },
    }
}
