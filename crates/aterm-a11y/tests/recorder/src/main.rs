// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The semantics oracle's recorder (A11Y.5 of
//! docs/AUDIT-third-party-2026-10-15.md, unit a11y), built outside the aterm
//! workspace (see Cargo.toml): registry accesskit_atspi_common 0.19.0, over
//! accesskit_consumer 0.37.0 and accesskit 0.24.1, plays the corpus of
//! `../oracle/corpus.rs` and prints the transcript of `../oracle/transcript.rs`,
//! the file aterm-a11y's own semantics are held to.
//!
//!   (no argument)   the recording: events as text, the other blocks digested
//!   --full          every block's text
//!   --blank-lines   the blank-line table: every text offset and granularity
//!                   of the corpus's blank-line documents, `hang` where the
//!                   registry crates never answered

use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The model the corpus builds: the registry crate's.
mod model {
    pub(crate) use accesskit::*;
}

/// The semantics the transcript plays: the registry crate's.
mod sem {
    pub(crate) use accesskit_atspi_common::*;

    /// The application context's handle.
    pub(crate) type SharedApp = std::sync::Arc<std::sync::RwLock<AppContext>>;
}

#[path = "../../oracle/corpus.rs"]
mod corpus;
#[path = "../../oracle/transcript.rs"]
mod transcript;

const TRANSCRIPT_HEADER: &str = "\
# The AT-SPI semantics oracle (docs/AUDIT-third-party-2026-10-15.md, unit a11y,
# A11Y.5): registry accesskit_atspi_common 0.19.0 over accesskit_consumer 0.37.0
# and accesskit 0.24.1, recorded once by tests/recorder from the corpus of
# tests/oracle/corpus.rs through tests/oracle/transcript.rs (main @ c1b839fd73).
# Per step: the events as text (sorted: the registry crates emit them in hash
# order), then the tree, text and actions blocks as FNV-1a digest and line count.
# aterm-a11y's semantics replay it in src/atspi/oracle.rs. Never edit by hand.
";

const BLANK_LINES_HEADER: &str = "\
# The AT-SPI semantics oracle's blank-line table (docs/AUDIT-third-party-2026-10-15.md,
# unit a11y, A11Y.5): registry accesskit_atspi_common 0.19.0 over accesskit_consumer
# 0.37.0 and accesskit 0.24.1, recorded by `tests/recorder -- --blank-lines` from the
# blank-line documents of tests/oracle/corpus.rs through
# tests/oracle/transcript.rs's blank_line_answer. One row per document, offset and
# granularity: GetStringAtOffset's answer, or `hang` where the registry crates had
# not answered a minute after the first question (accesskit_consumer's paragraph
# search loops forever after a blank line). aterm-a11y's semantics replay it in
# src/atspi/oracle.rs. Never edit by hand.
";

/// A minute: every answer the registry crates give comes in microseconds, so
/// one still missing then never comes.
const HANG: Duration = Duration::from_secs(60);

fn blank_lines() {
    let (tx, rx) = mpsc::channel();
    let mut keys = Vec::new();
    for document in corpus::blank_line_documents() {
        for offset in 0..=document.len() {
            for g in transcript::BLANK_LINE_GRANULARITIES {
                let row = keys.len();
                keys.push(transcript::blank_line_key(&document, offset, g));
                let tx = tx.clone();
                let document = document.clone();
                // A question that loops keeps its thread spinning until the
                // process exits; every other one ends at once.
                std::thread::spawn(move || {
                    let _ = tx.send((row, transcript::blank_line_answer(&document, offset, g)));
                });
            }
        }
    }
    drop(tx);
    let deadline = Instant::now() + HANG;
    let mut answers: Vec<Option<String>> = vec![None; keys.len()];
    while let Ok((row, answer)) =
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        answers[row] = Some(answer);
    }
    print!("{BLANK_LINES_HEADER}");
    for (key, answer) in keys.iter().zip(answers) {
        println!("{key} {}", answer.as_deref().unwrap_or("hang"));
    }
    // The spinning threads never end; the process does.
    std::process::exit(0);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--blank-lines") {
        blank_lines();
    }
    let full = args.iter().any(|a| a == "--full");
    if !full {
        print!("{TRANSCRIPT_HEADER}");
    }
    for scenario in corpus::scenarios() {
        print!("{}", transcript::play(scenario, full));
    }
}
