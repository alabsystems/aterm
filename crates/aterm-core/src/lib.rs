// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// The anchor attributes are `macro_rules!` ones where Cargo resolved no proc macro
// under them (crates/aterm-spec-anchor/anchor_rules.rs).
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! # aterm-core
//!
//! Terminal emulation core.
//!
//! ## Verification
//!
//! - **Derived models** (`ty_model!`) for key invariants: `crates/aterm-models-core`
//!   (the alternate-screen round trip, `tests/conformance_altscreen.rs`)
//! - **Fuzz-shaped tests** are modules of the `core_suite` integration binary
//!   (`tests/fuzz_process_never_panics.rs`, `tests/fuzz_alt_screen.rs`).
//!   The workspace's `cargo-fuzz` targets are `crates/aterm-font/fuzz`,
//!   `crates/aterm-zstd/fuzz` and `crates/aterm-dbus/fuzz`.
//!
//! ## Components
//!
//! - [`terminal`] - Terminal emulation state machine
//! - [`config`] - Terminal configuration and hot-reload types
//! - [`render`] - The engine-owned render snapshot a renderer consumes
//! - [`search`] - Trigram-indexed search (core logic in `aterm-search`)
//! - [`shell_integration`] - Shell integration injection (zsh, bash, fish)
//! - [`bell`] - Bell presentation state machines
//! - [`fault`] - Deterministic fault injection for fail-closed-path testing
//! - [`grapheme`], [`grid`], [`selection`], [`scrollback`] - re-exports of the
//!   `aterm-grapheme`, `aterm-grid`, `aterm-selection` and `aterm-scrollback` crates
//!
//! Semantic state (perception, regions, highlighting) is not an engine
//! primitive; consumers such as `aterm-observe` and `aterm-agent` read it over
//! the engine.
//!
//! Internal: `parser` (`aterm-parser`), `domain` value types, `sixel` (decodes
//! a sixel DCS into an inline image; DA1 advertises code 4) and `vt_level`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
// Test code (compiled via --all-targets) is exempt from strict clippy.
// Production code is still checked: `verify lint` (tippy -D warnings) enforces all + pedantic.
#![cfg_attr(test, allow(clippy::all, clippy::pedantic))]
#![allow(
    unexpected_cfgs,
    reason = "a verification cfg added behind a feature gate must not trip the workspace lint"
)]
// Crate-wide style policy.
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
// Documentation lints, relaxed crate-wide.
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
// High-count style lints kept crate-wide.
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

/// Bell presentation state: the pure flash/beep decision logic
/// (timestamps injected) behind a host's BEL handling. The engine fires
/// [`terminal::Terminal::set_bell_callback`]; the host feeds these state
/// machines and does the actual painting/beeping.
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
/// The engine-owned render snapshot: [`render::RenderInput`] is the
/// `Terminal`-free value a renderer consumes for one frame; the engine builds it via
/// [`terminal::Terminal::cell_frame_into`]. Hosted here so the engine can emit
/// it without a dependency cycle; `aterm_render::render_api` re-exports it.
pub mod render;
/// The fx-layer primitive, re-exported from [`render`] beside `GlowQuad`.
mod render_fx;
pub mod scrollback;
pub mod search;

/// Sixel graphics decoder (extracted to `aterm-sixel` crate).
pub(crate) use aterm_sixel as sixel;
pub mod terminal;
pub(crate) mod vt_level;

/// Shell integration injection (zsh, bash, fish).
///
/// Re-exported from the standalone `aterm-shell-integration` crate.
pub use aterm_shell_integration as shell_integration;
