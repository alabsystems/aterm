// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Terminal emulation core: VT state machine, render snapshots, search, and configuration.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![cfg_attr(test, allow(clippy::all, clippy::pedantic))]
#![allow(
    unexpected_cfgs,
    reason = "a verification cfg added behind a feature gate must not trip the workspace lint"
)]
#![allow(
    clippy::must_use_candidate,
    reason = "not all functions need #[must_use]"
)]
#![allow(
    clippy::module_name_repetitions,
    reason = "e.g. parser::Parser is idiomatic"
)]
#![allow(
    clippy::similar_names,
    reason = "fg/bg, row/col are domain-standard pairs"
)]
#![allow(
    clippy::match_same_arms,
    reason = "explicit match arms aid readability"
)]
#![allow(
    clippy::match_bool,
    reason = "match on bool can be clearer than if/else"
)]
#![allow(
    clippy::single_match_else,
    reason = "FFI error handling uses match for clarity"
)]
#![allow(
    clippy::collapsible_if,
    reason = "nested ifs can be clearer than combined conditions"
)]
#![allow(
    clippy::items_after_statements,
    reason = "helper fns near usage site aids readability"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "documentation coverage is incremental"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "documentation coverage is incremental"
)]
#![allow(
    clippy::doc_markdown,
    reason = "technical terms like VT100 don't need backticks"
)]
#![allow(
    clippy::missing_fields_in_debug,
    reason = "large structs omit fields for readability"
)]
#![allow(
    clippy::explicit_iter_loop,
    reason = "for x in vec.iter() can be clearer than for x in &vec"
)]
#![allow(
    clippy::map_unwrap_or,
    reason = ".map().unwrap_or() clearer than .map_or() in this crate"
)]
#![allow(
    clippy::manual_let_else,
    reason = "FFI match-and-return pattern clearer than let-else"
)]

/// Bell presentation state machines for visual flash and audio alert decisions.
pub mod bell;
pub mod config;
pub(crate) mod domain;
/// Deterministic fault injection for fail-closed-path testing.
pub mod fault;
pub mod grapheme;
pub mod grid;
/// VT100/ANSI escape sequence parser (the [`aterm_parser`] crate).
pub(crate) use aterm_parser as parser;
/// Text selection and smart semantic selection (re-exported from `aterm-selection`).
pub use aterm_selection as selection;
/// Engine-owned [`render::RenderInput`] snapshot consumed by renderers.
pub mod render;
/// The fx-layer primitive, re-exported from [`render`] beside `GlowQuad`.
mod render_fx;
/// A glow stream's ramps, re-exported from [`render`] beside `GlowQuad`.
mod render_ramps;
pub mod scrollback;
pub mod search;

/// Sixel graphics decoder (the `aterm-sixel` crate).
pub(crate) use aterm_sixel as sixel;
pub mod terminal;
pub(crate) mod vt_level;

/// Shell integration injection (the `aterm-shell-integration` crate).
pub use aterm_shell_integration as shell_integration;
