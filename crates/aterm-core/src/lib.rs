// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! # aterm-core
//!
//! High-performance terminal emulation core with formal verification in progress.
//!
//! ## Verification
//!
//! Verification is in progress and coverage varies by module:
//! - **TLA+ specs** define key invariants (see `tla/`)
//! - **Fuzz targets** live in `crates/aterm-core/fuzz`
//! - **Property-based tests** live in `src/tests/proptest/`
//!
//! ## Components
//!
//! ### Core Engine
//!
//! - [`terminal`] - Terminal emulation state machine
//! - [`checkpoint`] - Crash recovery checkpoints and restore
//! - [`config`] - Terminal configuration and hot-reload types
//! - [`search`] - Trigram-indexed search (core logic in `aterm-search`)
//! - [`shell_integration`] - Shell integration injection (zsh, bash, fish)
//! - [`ui`] - Verified UI bridge state machine
//!
//! ### Perception & AI
//!
//! - [`perception`] - Structured screen reading for AI agents
//! - [`semantic`] - Semantic region detection (prompts, commands, errors)
//!
//! ### Graphics
//!
//! - `syntax` - Tree-sitter syntax highlighting infrastructure
//!
//! ### Extracted Crates
//!
//! - [`grapheme`] - Unicode grapheme cluster segmentation (extracted to `aterm-grapheme`)
//! - [`grid`] - Terminal grid storage and page model (extracted to `aterm-grid`)
//! - [`selection`] - Text selection and smart semantic selection (extracted to `aterm-selection`)
//! - [`scrollback`] - Tiered storage (extracted to `aterm-scrollback`)
//! - `aterm-vi` - Vi mode keyboard navigation used internally by [`terminal`]
//! - `aterm-jsonrpc` (`jsonrpc`) - JSON-RPC 2.0 protocol used internally by
//!   daemon integrations
//!
//! ### Internal (`pub(crate)`)
//!
//! - `parser` - VT100/ANSI parser (extracted to `aterm-parser`, re-exported)
//! - `domain` - Domain value types
//! - `iterm_image` - Terminal inline image protocol
//! - `platform` - Platform abstraction (fonts, clipboard, notifications)
//! - `security` - Security primitives for AI agent interaction
//! - `session` - Terminal session management (test only)
//! - `sixel` - Sixel graphics decoder (off-by-default `sixel` feature; when
//!   enabled, decodes a sixel DCS into an inline image via the OSC 1337 path
//!   and advertises DA1 code 4)
//! - `text_shaping_config` - Text shaping configuration
//! - `vt_level` - VT compatibility level tracking
//!
//! ## Code Guidelines
//!
//! See `.AI Model/rules/rust_excellence.md` for error handling, API design, and style.
//!

// =============================================================================
// LINT CONFIGURATION
// =============================================================================
//
// This crate uses strict linting with explicit exceptions documented below.
// Allows are grouped by category. Module-specific allows should be placed
// in the module file, not here. Only crate-wide policy decisions belong here.

#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
// Test code (compiled via --all-targets) is exempt from strict clippy.
// Production code is still checked: `cargo clippy -p aterm-core --lib` enforces all + pedantic.
#![cfg_attr(test, allow(clippy::all, clippy::pedantic))]
#![allow(
    unexpected_cfgs,
    reason = "defensive: the `sixel` feature gates the off-by-default decode/render path (declared in Cargo.toml); kept allowing unexpected_cfgs so a future verification cfg added behind the same gate cannot trip the workspace lint"
)]
// -----------------------------------------------------------------------------
// CRATE-WIDE STYLE POLICY: These are intentional style choices for consistency
// -----------------------------------------------------------------------------
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
// struct_excessive_bools: Narrowed to per-struct in #2088
// wildcard_imports: narrowed to per-module in #5780 (grid, ffi, gpu/ffi, etc.)
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
// needless_return: Eliminated (0 occurrences remaining)
// borrow_as_ptr: Narrowed to per-module in #2088 (FFI modules only)

// -----------------------------------------------------------------------------
// DOCUMENTATION: Will be addressed incrementally, does not affect correctness
// -----------------------------------------------------------------------------
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
// -----------------------------------------------------------------------------
// STYLE: High-count lints kept crate-wide (57+ occurrences each)
// -----------------------------------------------------------------------------
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

// -----------------------------------------------------------------------------
// NARROWED IN #2088: The following were crate-wide, now per-module/per-function:
//   cast_possible_truncation  → per-module (grid, ffi, terminal, security)
//   borrow_as_ptr             → per-module (FFI modules only)
//   large_stack_arrays        → grid/page.rs only
//   should_implement_trait    → per-item (3 types)
//   inherent_to_string*       → per-item (2 types)
//   needless_pass_by_value    → per-function (6 functions)
//   struct_excessive_bools    → per-struct (8 structs)
//   too_many_lines            → per-function (37 functions, in earlier commit)
// LEGACY PATTERNS (migrated in #2074):
//   manual_let_else, uninlined_format_args, unused_self, ptr_as_ptr,
//   derivable_impls, iter_without_into_iter, elidable_lifetime_names
// -----------------------------------------------------------------------------

/// Catch panics at an FFI boundary, returning `$default` on unwind.
///
/// Thin wrapper over [`aterm_ffi_types::aterm_ffi_catch_panic`] with the
/// crate's standard `"[aterm-ffi]"` log prefix. Used by the test-only
#[cfg(test)]
#[macro_export]
macro_rules! ffi_catch_panic {
    ($default:expr_2021, $fn_name:literal, $body:expr_2021) => {
        ::aterm_ffi_types::aterm_ffi_catch_panic!("[aterm-ffi]", $default, $fn_name, $body)
    };
}

// Used by test code in feature-gated modules (media, gpu).
#[cfg(test)]
#[allow(
    unused_imports,
    reason = "used by test code in feature-gated modules (media, gpu)"
)]
pub(crate) use aterm_ffi_types::MAX_FFI_BUFFER_SIZE;

/// Origin-aware clipboard policy (Phase 3 of #7874 escape-sequence
/// hardening). Decides whether a clipboard action is allowed, denied,
/// or requires user confirmation, keyed by the origin of the request
/// (user-initiated, PTY-origin OSC 52, paste-injection,
/// checkpoint-restore). Orthogonal to the `ClipboardAuth` capability
/// tokens in `terminal::clipboard_auth`.
pub mod clipboard_policy;

/// Bell presentation state: the pure flash/beep decision logic
/// (timestamps injected) behind a host's BEL handling. The engine fires
/// [`terminal::Terminal::set_bell_callback`]; the host feeds these state
/// machines and does the actual painting/beeping.
pub mod bell;
pub mod config;
pub(crate) mod domain;
/// Deterministic fault injection for fail-closed-path testing (M7 FAULT-INJECT).
pub mod fault;
pub mod grapheme;
pub mod grid;
/// VT100/ANSI escape sequence parser.
///
/// Re-exported from the standalone [`aterm_parser`] crate for backward
/// compatibility. New consumers should depend on `aterm-parser` directly.
pub(crate) use aterm_parser as parser;
/// Text selection and smart semantic selection (re-exported from `aterm-selection`).
pub use aterm_selection as selection;
/// Parser FFI bridge (depends on aterm-core FFI infrastructure).
/// The engine-owned render snapshot (`read_image` boundary, REARCH A-3):
/// [`render::RenderInput`] is the `Terminal`-free value a renderer consumes for
/// one frame; the engine builds it via
/// [`terminal::Terminal::cell_frame_into`]. Hosted here (not in
/// `aterm-render-api`) so the engine can emit it without a dependency cycle;
/// `aterm-render-api` re-exports it.
pub mod render;
pub mod scrollback;
pub mod search;

// Security primitives for AI agent terminal interaction (defense-in-depth: buffer
// history tracking for temporal integrity, ANSI sanitization for hidden/deceptive
// content). #2584 extraction never occurred — narrowed back to crate-internal (#6671).

// AI perception layer for structured terminal screen reading (semantic
// understanding of terminal content: text mode = raw lines/full text; layout mode
// = cell positions/styles; semantic mode = typed regions like prompts, commands,
// errors, code). Crate-internal — these are notes, not item docs.

/// Platform abstraction traits for portable terminal integration.
///
/// Defines platform-specific functionality (fonts, clipboard, notifications)
/// that varies across operating systems. Each platform provides its own
/// implementation via native Rust code or FFI callbacks.
///
/// Public so `aterm-core-ffi` can own the GPU glyph-instance FFI without
/// duplicating text-shaping types and stub implementations (#6555).
pub mod platform;
/// Sixel graphics decoder (extracted to `aterm-sixel` crate).
#[cfg(feature = "sixel")]
pub(crate) use aterm_sixel as sixel;
pub mod terminal;
pub(crate) mod text_shaping_config;
pub mod ui;
/// Vi mode navigation: cursor movement, marks, inline search.
///
/// Re-exported from the `aterm-vi` crate. Provides vim-style keyboard
/// navigation as a state machine owned by [`terminal::Terminal`].
pub(crate) use aterm_vi as vi_mode;
/// The vi-mode value types are part of the `Terminal` public API (the `vi_motion` /
/// `vi_cursor_point` / … accessors take/return them), so re-export them publicly for
/// GUI callers driving keyboard copy-mode (VI-1) — otherwise those methods take a
/// crate-private type no downstream crate can name.
pub use aterm_vi::{InlineSearchKind, ViBoundary, ViMotion, ViPoint, ViVisualType};
pub(crate) mod vt_level;

/// Test-only helper re-exports for in-crate tests.
#[cfg(test)]
pub mod testing;

/// Shell integration injection (zsh, bash, fish).
///
/// Re-exported from the standalone `aterm-shell-integration` crate.
pub use aterm_shell_integration as shell_integration;

// Trigger evaluation is test-only support after the production engine removal.
// Physical files relocated to test_support/triggers/ (Part of #6814).
#[cfg(test)]
#[path = "../test_support/triggers/mod.rs"]
pub(crate) mod triggers;

// Property tests module (only compiled when testing)
#[cfg(test)]
mod tests;

// ffi_tests/ fully migrated to aterm-core-ffi (#5760) — module removed.

pub mod prelude;
