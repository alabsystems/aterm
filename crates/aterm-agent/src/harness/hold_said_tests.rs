// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE HOLD'S OWN WORDS (§14.17 item 1, slice 4b): the agent's list item by item, bounded, and the
// files git found. The slice's As built note names the mutant each test was seen failing under.

use super::*;

/// THE QUOTE IS THE AGENT'S LIST, ITEM BY ITEM, as Land's prompt quotes it: a list's marks are
/// dropped, a wrapped row joins its item, a blank line is none, curly quotes are made straight,
/// and the items past the fourth are counted, not quoted.
/// Negative controls: the list kept whole as one item fails the item count; the reader's family
/// word in place of the sentence fails the first item.
#[test]
fn the_quote_is_the_agents_own_list_item_by_item() {
    let list = "- The docs PR is still open.\n\
                - The changelog entry for the \u{201c}parser\u{201d} is not\n\
                written yet.\n\
                \n\
                3. Bump the version.\n\
                * Tag the release.\n\
                - Close the milestone.\n\
                - Archive the old branch.";
    let said = HoldSaid::of(Some(list), &[]);
    assert_eq!(
        said.open,
        [
            "The docs PR is still open.",
            "The changelog entry for the \"parser\" is not written yet.",
            "Bump the version.",
            "Tag the release.",
        ]
    );
    assert_eq!(said.more, 2, "{said:?}");
    assert_eq!(said.past_first(), 5);
    assert_eq!(
        land_check::listed_words(list),
        said.open.join("; ") + "; Close the milestone.; Archive the old branch.",
        "the prompt quotes the same items, one source"
    );
    assert!(HoldSaid::of(None, &[]).is_empty());
    assert!(HoldSaid::of(Some("\n  \n"), &[]).is_empty());
}

/// A HEADING IS NOT AN ITEM, AND A WRAPPED ROW JOINS ITS ITEM WHATEVER ITS CASE: an answer that
/// mirrors the check (`Not landed:` above its list, or a markdown heading) quotes the list, not
/// the heading; a list item, or a full row, the screen wrapped before a capital (`GitHub`) is one
/// item; a plain list's short lines starting with a capital stay apart.
/// Negative controls: the heading kept as the first item fails the first quote; a wrap joined only
/// where it starts in lower case fails the marked and the full row; every capital row joined fails
/// the plain list.
#[test]
fn a_heading_is_not_an_item_and_a_wrapped_row_joins_its_item() {
    let headed = "Not landed:\n- The docs PR is still open.\n- Bump the version.";
    assert_eq!(
        HoldSaid::of(Some(headed), &[]).open,
        ["The docs PR is still open.", "Bump the version."]
    );
    assert_eq!(
        land_check::listed_items("## Left\nthe docs PR is still open."),
        ["the docs PR is still open."]
    );
    assert_eq!(
        land_check::listed_items("Not landed:"),
        ["Not landed:"],
        "a lone line is the answer"
    );
    let marked = "- The docs pull request for the parser is still open on\n  \
                  GitHub and needs a review.\n- Bump the version.";
    assert_eq!(
        land_check::listed_items(marked),
        [
            "The docs pull request for the parser is still open on GitHub and needs a review.",
            "Bump the version.",
        ]
    );
    let row = "The docs pull request for the parser crate is still open on the\n\
               GitHub mirror and needs a review.";
    assert_eq!(
        land_check::listed_items(row),
        [
            "The docs pull request for the parser crate is still open on the GitHub mirror and \
          needs a review."
        ]
    );
    assert_eq!(
        land_check::listed_items("The docs PR is still open\nThe version is not bumped"),
        ["The docs PR is still open", "The version is not bumped"]
    );
}

/// A QUOTED ITEM IS BOUNDED: at most [`ITEM_BYTES`] bytes, cut at a word's edge with `…`; the
/// short form cuts the first item to [`SHORT_CHARS`] characters the same way.
/// Negative control: an item kept uncut fails the byte bound.
#[test]
fn a_quoted_item_is_bounded_at_a_words_edge() {
    let long = "word ".repeat(80);
    let said = HoldSaid::of(Some(&long), &[]);
    let item = &said.open[0];
    assert!(item.len() <= ITEM_BYTES, "{} bytes", item.len());
    assert!(item.ends_with("word\u{2026}"), "{item}");
    let short = cut_chars(item, SHORT_CHARS);
    assert!(short.chars().count() <= SHORT_CHARS, "{short}");
    assert!(short.ends_with("word\u{2026}"), "{short}");
    assert_eq!(
        cut_chars("short", SHORT_CHARS),
        "short",
        "a short item is whole"
    );
    let wide = "\u{00e9}".repeat(200);
    let cut = cut_chars(&wide, SHORT_CHARS);
    assert_eq!(
        cut.chars().count(),
        SHORT_CHARS,
        "one word, cut at a character: {cut}"
    );
}

/// THE FILES GIT FOUND are named after the counts, with `…` where it found more than it named,
/// in the list Land's prompt names them in (one source).
/// Negative controls: the paths left out of [`HoldSaid::of`] fail the names; the glass's own list
/// in place of [`super::super::repo_work::files_list`] fails the prompt's.
#[test]
fn the_files_git_found_are_named() {
    let paths = vec!["wip-notes.txt".to_string(), "README.md".to_string()];
    let said = HoldSaid::of(None, &paths);
    assert_eq!(said.paths, paths);
    assert!(!said.is_empty());
    assert_eq!(files_words(&said.paths, 2), ": wip-notes.txt, README.md");
    assert_eq!(
        files_words(&said.paths, 5),
        ": wip-notes.txt, README.md, \u{2026}"
    );
    assert_eq!(files_words(&[], 5), "");
    let work = super::super::repo_work::RepoWork {
        repo: "/r".to_string(),
        changed: 4,
        untracked: 1,
        paths: paths.clone(),
        ..super::super::repo_work::RepoWork::default()
    };
    let summary = work.summary();
    let listed = files_words(&paths, 5);
    let listed = listed.trim_start_matches(": ");
    assert!(
        summary.contains(&format!("({listed})")),
        "the prompt and the glass list the files alike: {summary} / {listed}"
    );
}
