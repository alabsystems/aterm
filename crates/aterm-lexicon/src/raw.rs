// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Deserialization types for `data/lexicon.toml`, `data/exceptions.toml` and
//! `data/tricks.toml`, shared between `build.rs` and run-time decoding.

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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTricks {
    #[serde(default)]
    pub(crate) trick: Vec<RawTrickRow>,
    #[serde(default)]
    pub(crate) vocative: Vec<RawWordRow>,
    #[serde(default)]
    pub(crate) filler: Vec<RawWordRow>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTrickRow {
    pub(crate) id: String,
    pub(crate) lang: String,
    #[serde(default)]
    pub(crate) words: Vec<String>,
    #[serde(default)]
    pub(crate) address: Vec<String>,
    #[serde(default)]
    pub(crate) gated: bool,
    #[serde(default)]
    pub(crate) cjk: bool,
    #[serde(default)]
    pub(crate) unglued: Vec<String>,
    /// Reviewer prose in the TOML; type-checked (`deny_unknown_fields`), never read.
    #[serde(default, rename = "notes")]
    pub(crate) _notes: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWordRow {
    pub(crate) lang: String,
    #[serde(default)]
    pub(crate) words: Vec<String>,
    #[serde(default)]
    pub(crate) gated: bool,
    #[serde(default)]
    pub(crate) cjk: bool,
    #[serde(default)]
    pub(crate) unglued: Vec<String>,
    /// Reviewer prose in the TOML; type-checked (`deny_unknown_fields`), never read.
    #[serde(default, rename = "notes")]
    pub(crate) _notes: String,
}
