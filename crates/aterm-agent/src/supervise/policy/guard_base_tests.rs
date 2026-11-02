// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE IRREVERSIBLE GUARD'S BASE, AS DATA: the generator of `fixtures/go-ahead-guard-base.txt`,
//! the cut (`needs::without_needs`) and the reading (`turn_end::classify_said`) the guard made of
//! each text of the go-ahead's corpus at 246d1ddc2f, before the go-ahead reader. The go-ahead's
//! tests hold today's guard's reading to it (`the_guard_reads_every_reply_as_the_base_did`): no
//! reading weaker than the base's, so a change to the cut or the classifier that weakens one shows
//! however today's own code reads it. The base's cut is kept in each record to say what it read,
//! not to pin today's: that today's cut takes no go-ahead join is
//! `needs::tests::the_guards_cut_takes_no_go_ahead_join`.
//!
//! It names only what 246d1ddc2f has, so it compiles there unchanged, and it must run there: run
//! here it would write today's guard, which is what the fixture checks. To write the fixture
//! again (a row added to the corpus):
//!
//! 1. Write the corpus here: `GA_CORPUS=<file> targo --unverified test -p aterm-agent --lib --
//!    --ignored write_guard_corpus`.
//! 2. Export 246d1ddc2f with its `vendor/astream` pin into a scratch folder (`git archive`, never
//!    a checkout of the shared one), copy this file into its `crates/aterm-agent/src/supervise/
//!    policy/`, and add `#[cfg(test)] mod guard_base_tests;` to that folder's `mod.rs`.
//! 3. There, `GA_CORPUS=<file> GA_OUT=<this crate>/src/supervise/policy/fixtures/
//!    go-ahead-guard-base.txt targo --unverified test -p aterm-agent --lib -- --ignored
//!    write_base_guard`.

use std::fmt::Write as _;

use super::turn_end::{Said, classify_said};

/// The fixture's head: what it holds and how it is written.
const HEAD: &str = concat!(
    "# GO-AHEAD GUARD BASE: the irreversible guard's cut and reading at 246d1ddc2f for each\n",
    "# text of the go-ahead's corpus, one `text<TAB>cut<TAB>reading` a line, each field escaped\n",
    "# (\\n, \\t, \\\\), the cut `\\=` where it is the text whole. Written by\n",
    "# supervise/policy/guard_base_tests.rs run at 246d1ddc2f (its head says how): rerun it\n",
    "# there, never edit by hand.\n",
);

/// `text` with its backslashes, line breaks and tabs escaped, one field of a record.
pub(crate) fn escaped(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// One field of a record read back ([`escaped`]'s inverse).
pub(crate) fn unescaped(field: &str) -> String {
    let mut out = String::new();
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some(other) => other,
            None => '\\',
        });
    }
    out
}

/// The cut field of a record whose cut is its text whole: no escaped text reads so.
pub(crate) const SAME: &str = "\\=";

/// A reading as a record writes it: its kind, and for a stop or an irreversible ask its reason.
pub(crate) fn reading(said: &Said) -> String {
    match said {
        Said::Plain => "plain".to_string(),
        Said::Offer => "offer".to_string(),
        Said::Question => "question".to_string(),
        Said::Stop(why) => format!("stop:{why}"),
        Said::Irreversible(why) => format!("irreversible:{why}"),
    }
}

/// WRITE THE FIXTURE from the corpus `GA_CORPUS` names (one escaped text a line) into the file
/// `GA_OUT` names, with this tree's guard: run at 246d1ddc2f only (the module's head says how).
#[test]
#[ignore = "writes fixtures/go-ahead-guard-base.txt; run at 246d1ddc2f only"]
fn write_base_guard() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is unset"));
    let corpus = std::fs::read_to_string(var("GA_CORPUS")).expect("the corpus reads");
    let mut out = HEAD.to_string();
    let mut seen = std::collections::HashSet::new();
    for line in corpus.lines().filter(|l| !l.is_empty() && seen.insert(*l)) {
        let text = unescaped(line);
        let cut = super::needs::without_needs(&text);
        let said = classify_said(Some(&cut));
        let cut = if cut == text {
            SAME.to_string()
        } else {
            escaped(&cut)
        };
        writeln!(
            out,
            "{}\t{cut}\t{}",
            escaped(&text),
            escaped(&reading(&said))
        )
        .expect("a String takes the record");
    }
    std::fs::write(var("GA_OUT"), out).expect("the fixture writes");
}
