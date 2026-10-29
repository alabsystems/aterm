// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless editor reducer, document store, rope, and protocol engine (`amacs`).

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
/// The smallest edits that turn one text into another.
pub mod diff;
/// The editor's pure reducer: commands, keymaps, views and the workspace.
pub mod editor;
/// The shared input reducer: one key-to-command semantics for every face.
pub mod input;
pub mod language;
pub mod protocol;
/// The store of open documents and their views.
pub mod store;
/// CSV and TSV as records and cells: the scan, values and re-encoding behind the grid face.
pub mod table;
/// The rope every document's text lives in.
pub mod text;
