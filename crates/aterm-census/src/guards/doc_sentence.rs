// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! doc-sentence: a doc comment does not end mid-sentence.
//!
//! A doc comment whose last line ends on a joining word (`and`, `the`, `of`, …) or a comma
//! was cut short: a prune that keeps only each paragraph's first line leaves exactly that,
//! and rustdoc shows the fragment as the item's summary. One such prune left about 17,000
//! across the workspace on 2026-10-22, every one passing fmt, lint and doc. Read over
//! every Rust file's `///` and `//!` blocks; a heading (`# …`) names a section, not a
//! sentence.

use super::rules::PRODUCT;
use super::{Finding, Rule, Tree};

pub(super) const DOC_SENTENCE: Rule = Rule {
    id: "doc-sentence",
    why: "a doc comment that ends on a joining word or a comma was cut mid-sentence, and \
          rustdoc shows the fragment as the item's summary; finish the sentence",
    scope: &[],
    check: doc_sentence,
};

/// Words a sentence never ends on. `in`, `on` and `so` are left out: a sentence can end on
/// one as a particle ("zoomed in", "turned on", "does so").
const JOINING: &[&str] = &[
    "a", "an", "and", "as", "but", "by", "for", "from", "is", "its", "of", "or", "that", "the",
    "this", "to", "when", "where", "which", "whose", "with",
];

fn doc_sentence(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !file.mentions(&["///", "//!"]) {
            continue;
        }
        let raw: Vec<&str> = file.raw.lines().collect();
        let marker = |no: usize| {
            let line = raw.get(no.wrapping_sub(1))?.trim_start();
            ["///", "//!"]
                .into_iter()
                .find(|marker| line.starts_with(marker) && !line.starts_with("////"))
        };
        for line in file.lines() {
            if !line.text.trim().is_empty() {
                continue;
            }
            let Some(own) = marker(line.no) else {
                continue;
            };
            if marker(line.no + 1) == Some(own) {
                continue;
            }
            let body = raw[line.no - 1].trim_start()[own.len()..].trim();
            if cut(body) {
                let tail = &body[body.char_indices().rev().nth(47).map_or(0, |(at, _)| at)..];
                out.push(file.finding(
                    DOC_SENTENCE.id,
                    line.no,
                    format!("a doc comment ends mid-sentence: `…{tail}`"),
                ));
            }
        }
    }
}

/// Whether a doc comment's last line, `body`, stops mid-sentence.
fn cut(body: &str) -> bool {
    if body.starts_with('#') {
        return false;
    }
    body.ends_with(',')
        || body
            .rsplit(|c: char| !c.is_ascii_alphabetic())
            .next()
            .is_some_and(|word| JOINING.contains(&word))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn a_doc_comment_does_not_end_mid_sentence() {
        let rust = "//! A module that\n\
                    //! does one thing.\n\
                    \n\
                    /// Reads the vault and\n\
                    fn cut() {}\n\
                    /// The current index,\n\
                    struct Comma;\n\
                    /// Reads the vault.\n\
                    fn whole() {}\n\
                    /// Each cell, except that\n\
                    ///\n\
                    /// * a zero-width one joins the cell before it.\n\
                    fn list() {}\n\
                    /// # Where it comes from\n\
                    fn heading() {}\n\
                    // A plain comment ends on the\n\
                    fn plain() {}\n\
                    const S: &str = \"\n\
                    /// a string's text and\n\
                    \";\n\
                    /// The band's base\n\
                    fn noun() {}\n\
                    /// Something can be zoomed in\n\
                    const ZOOM_IN: u32 = 0x21;\n\
                    /// Built in the\n\
                    //! module doc after an item doc\n\
                    fn mixed() {}\n";
        let found = run(&DOC_SENTENCE, &[("crates/aterm-x/src/lib.rs", rust)]);
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-x/src/lib.rs:4",
                "crates/aterm-x/src/lib.rs:6",
                "crates/aterm-x/src/lib.rs:25",
            ]
        );
        assert_eq!(
            found[0].detail,
            "a doc comment ends mid-sentence: `…Reads the vault and`"
        );
    }
}
