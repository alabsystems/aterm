// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab` with the harness's reading of the account sheet
//! (docs/DESIGN-alab-lab-model-2026-11-01.md §9): the lab importer lives in `aterm-db`, which
//! does not link the harness, so this executable hands it `accounts_sync::read_sheet_cell`. The
//! importer and every Mac's pool then read one sheet one way: Kimi Code's bare `1` held, Codex
//! and Grok Build's flow words, a disqualifying note pausing the vendor it names (or every
//! vendor when it names none) and, as Claude Code's pool reads it, Claude Code whatever it names.

use aterm_agent::harness::{
    accounts::Cli,
    accounts_sync::{SheetCell, SheetColumn, read_sheet_cell},
};
use aterm_db::{
    alab::import::{Mark, Terms, Vendors},
    typed::keys,
};
use std::{ffi::OsString, process::ExitCode};

/// The harness's reading of a provider cell.
pub(crate) struct Harness;

impl Vendors for Harness {
    fn mark(&self, provider: &str, cell: &str, notes: &str) -> Mark {
        let column = match provider {
            keys::ANTHROPIC => SheetColumn::Pool(Cli::Claude),
            keys::OPENAI => SheetColumn::Pool(Cli::Codex),
            keys::XAI => SheetColumn::Pool(Cli::Grok),
            keys::KIMI => SheetColumn::Pool(Cli::Kimi),
            keys::GOOGLE => SheetColumn::Google,
            keys::META => SheetColumn::Meta,
            _ => return Mark::Unread,
        };
        let terms = |vendor: aterm_agent::harness::accounts::Vendor| Terms {
            region: vendor.region.map(|region| region.as_str()),
            flow: vendor.flow.map(|flow| flow.as_str()),
        };
        match read_sheet_cell(column, cell, notes) {
            SheetCell::Absent => Mark::Absent,
            SheetCell::Member(vendor) => Mark::Member(terms(vendor)),
            SheetCell::Disqualified(vendor) => Mark::Paused(terms(vendor)),
            SheetCell::Held => Mark::Held,
            SheetCell::Unread => Mark::Unread,
        }
    }
}

/// `aterm db <args>`: `alab` with the harness's sheet reading, everything else as `aterm-db`
/// routes it.
pub(crate) fn db_entry(forwarded: Vec<OsString>) -> ExitCode {
    if forwarded.first().is_some_and(|word| word == "alab") {
        let Some(words) = forwarded[1..]
            .iter()
            .map(|word| word.clone().into_string().ok())
            .collect::<Option<Vec<String>>>()
        else {
            return aterm_uds::stdout::answer_exit("aterm db alab", "arguments must be UTF-8\n", 2);
        };
        return aterm_db::alab::command::main_entry(&words, Some(&Harness));
    }
    aterm_db::cli::main_entry(forwarded)
}
