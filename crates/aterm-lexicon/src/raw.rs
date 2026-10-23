// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Deserialization types for `data/lexicon.toml` and `data/exceptions.toml`,
//! shared between `build.rs` and runtime override loading.

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLexicon {
    #[serde(default)]
    pub(crate) entry: Vec<RawEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEntry {
    pub(crate) class: String,
    #[serde(default)]
    pub(crate) lang: String,
    #[serde(default = "default_mode")]
    pub(crate) mode: String,
    #[serde(default)]
    pub(crate) stems: Vec<String>,
    #[serde(default)]
    pub(crate) suffixes: Vec<String>,
    #[serde(default)]
    pub(crate) forms: Vec<String>,
    #[serde(default)]
    pub(crate) cjk: bool,
    #[serde(default)]
    pub(crate) ambiguous: bool,
    /// Animal species identifier for [`crate::Class::Animal`] entries.
    #[serde(default)]
    pub(crate) species: String,
    /// Reviewer notes in the TOML, checked by `deny_unknown_fields` and ignored at runtime.
    #[serde(default, rename = "notes")]
    pub(crate) _notes: String,
    /// Set at runtime on entries loaded from a user override TOML.
    #[serde(skip)]
    pub(crate) user: bool,
}

pub(crate) fn default_mode() -> String {
    "suffix".to_string()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawExceptions {
    #[serde(default)]
    pub(crate) exception: Vec<RawException>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawException {
    pub(crate) word: String,
    /// Reviewer annotations in the TOML, checked by `deny_unknown_fields` and ignored at runtime.
    #[serde(default, rename = "lang")]
    pub(crate) _lang: String,
    #[serde(default, rename = "meaning")]
    pub(crate) _meaning: String,
}
