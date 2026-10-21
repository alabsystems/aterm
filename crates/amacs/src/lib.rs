// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs, aterm's editor engine (docs/DESIGN-amacs-2026-10-19.md).
//!
//! The headless reducer, document store and rope behind aterm's native Editor.
//! Editing commands commit through the document store; the workspace owns
//! keymaps, selections, undo, search, kill/yank and keyboard macros.
//!
//! This is an engine crate (docs/ATERM_DESIGN.md §2.7, the census's
//! `engine-platform` rule): its shipped code names no filesystem, socket, raw
//! fd, libc, thread or native link. The native GUI performs I/O and hands the
//! engine values.
//!
//! The editor reducer (`editor`), the document store (`store`) and the rope
//! (`text`) moved here from aterm-gui-documents, which re-exports them under
//! their old names (`native_editor`, `document_store`, `native_text`). The
//! input reducer (`input`) is the one place a face's key, text or paste
//! becomes an editor command; aterm's native Editor drives it today. The
//! `protocol` module has the `edit` request and ticket replies and the
//! file-URI codec, which aterm-gui's `edit` and `await edit` verbs serve, and
//! the edit script with its `line:h6` anchors and anchored reads, which the
//! `doc` verb and `amacs --apply` are to serve. The document I/O both faces
//! share is the `amacs-files` crate, and the `amacs` program's client is the
//! `amacs-term` crate. The `language` module is the closed language table:
//! line lexers, indent and fill rules, and commit diagnostics. The `config`
//! module is a placeholder for planned work.

// A contracted fn is written once, through `trust_contract_fn!`: under Trust's
// verifying session (`cfg(trust_verify)`) it carries its native `ensures`
// clauses, and every other compiler (stock rustc in the foreign cells) gets the
// plain fn and never parses them. Each clause cites a theorem stated over
// `trust_import_amacs__<module path>__<fn>`, the kernel definition Trust mints
// from the compiled body, so the proof is about this code, not a copy of it.
// The arms are crates/aterm-log/src/lib.rs's; SPECS.md lists every contract.
// Defined before the modules so every module can use them.
#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

// A `clean { … }` proof island: only Trust lexes it, so every other compiler
// drops its tokens.
#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

pub mod config;
/// The editor's pure reducer: commands, keymaps, views and the workspace.
pub mod editor;
/// The shared input reducer: one key-to-command semantics for every face.
pub mod input;
pub mod language;
pub mod protocol;
/// The store of open documents and their views.
pub mod store;
/// The rope every document's text lives in.
pub mod text;
