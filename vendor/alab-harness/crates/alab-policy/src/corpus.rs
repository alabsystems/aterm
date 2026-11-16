// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CORPORA THE READERS ARE JUDGED ON, each read by its one parser: the go-ahead's table
//! ([`go_ahead`]), the irreversible guard's base record ([`guard_base`]: its generator and its
//! fields), the open-work reader's three corpora ([`open_work`]) and the needs table
//! ([`needs_table`]). This crate's tests and the engines' tests that hold a whole turn end to the
//! same rows read them alike.
//!
//! Each parser takes its corpus's text, and the test that reads a corpus `include_str!`s its file
//! from this crate's `fixtures/`: no fixture is an input of this module, which every debug build
//! of the library compiles and every dependent's debug and test builds link, so a row added to a
//! fixture rebuilds only the tests that read it.

pub mod go_ahead;
pub mod guard_base;
pub mod open_work;

/// What a row of the needs table (`fixtures/needs-table.txt`) says its report must read as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedsRow {
    /// No need stands: no need named, and a clean defense lands.
    None,
    /// A need, carrying these words: named, and nothing lands.
    Need(String),
    /// An ask with no head, these words in it: no landing, and the nudge names what it read open.
    Headless(String),
}

/// The rows of the needs table, from its text (`fixtures/needs-table.txt`).
pub fn needs_table(table: &str) -> Vec<(NeedsRow, String)> {
    table
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let (label, report) = line
                .split_once(" || ")
                .unwrap_or_else(|| panic!("a row with no ` || `: {line:?}"));
            let row = match label.split_once(' ') {
                None if label == "NONE" => NeedsRow::None,
                Some(("NEED", words)) => NeedsRow::Need(words.to_string()),
                Some(("HEADLESS", words)) => NeedsRow::Headless(words.to_string()),
                _ => panic!("a row with no label: {line:?}"),
            };
            (row, report.replace("\\n", "\n"))
        })
        .collect()
}
