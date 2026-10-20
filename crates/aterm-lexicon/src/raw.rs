// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The TOML shapes of a lexicon document and an exceptions document: what the
//! build script decodes the embedded `data/lexicon.toml` and
//! `data/exceptions.toml` through (`build.rs` includes this file), and what
//! [`crate::Lexicon::from_sources`] and a user's override decode through at run
//! time. One set of shapes, so the two decodings cannot disagree about what a
//! row is.
//!
//! `deny_unknown_fields` on all four, the kitty-command loader's law verbatim
//! (see `tricks.rs`): a typo'd key (`form = [...]` for `forms`, `stem` for
//! `stems`) must be a parse error the build — and the user's own stderr —
//! trips over, not a list that silently never loads. The host logs a rejected
//! override and falls back to the builtin.

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
    /// [`crate::Class::Animal`] only: the species id naming the authored sprite for
    /// every surface in this entry (e.g. `species = "monkey"`). Ignored (with
    /// a conflicts-channel note) on other classes; its absence on an animal
    /// entry is likewise recorded and the entry skipped.
    #[serde(default)]
    pub(crate) species: String,
    /// Reviewer prose in the TOML; type-checked (`deny_unknown_fields`), never read.
    #[serde(default, rename = "notes")]
    pub(crate) _notes: String,
    /// NOT a TOML field: set by [`crate::Lexicon::with_languages_and_override`] on
    /// every entry that came from the user's override TOML, so the build can
    /// record its surfaces in `Lexicon::user_surfaces` (v3 §6 short-word
    /// exemption).
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
    /// Reviewer annotations in the TOML; type-checked (`deny_unknown_fields`), never read.
    #[serde(default, rename = "lang")]
    pub(crate) _lang: String,
    #[serde(default, rename = "meaning")]
    pub(crate) _meaning: String,
}
