// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn repeated_identifiers_name_each_later_heading_and_the_first_definition() {
    let text = "## ATERM-KI-1110-113 — vault\n\n\
                ## ATERM-KI-1110-113 — main lane\n\n\
                ## ATERM-KI-1110-113 — another problem\n";
    let found = run(&ISSUE_ID, &[(INDEX, text)]);
    assert_eq!(
        spots(&found),
        ["docs/KNOWN_ISSUES.md:3", "docs/KNOWN_ISSUES.md:5"]
    );
    assert!(found.iter().all(|finding| {
        finding
            .detail
            .contains("ATERM-KI-1110-113 already appears at docs/KNOWN_ISSUES.md:1")
    }));
    let repaired = text
        .replace("113 — main lane", "147 — main lane")
        .replace("113 — another", "148 — another");
    assert!(run(&ISSUE_ID, &[(INDEX, &repaired)]).is_empty());
}

#[test]
fn references_and_headings_in_other_documents_do_not_define_open_issues() {
    let text = "## ATERM-KI-1110-113 — vault\n\
                See ATERM-KI-1110-113 and `ATERM-KI-1110-113`.\n\
                ### ATERM-KI-1110-113 — details\n\
                ## ATERM-KI-1110-147 — main lane\n";
    assert!(run(&ISSUE_ID, &[(INDEX, text), ("docs/design.md", text)]).is_empty());
    assert!(run(&ISSUE_ID, &[("docs/design.md", text)]).is_empty());
}

#[test]
fn fenced_and_indented_examples_are_not_issue_definitions() {
    for (open, close, inside) in [
        ("```md", "```", ""),
        ("   ~~~md", "   ~~~~", ""),
        ("````md", "````", "```\n"),
        ("~~~md", "~~~", "~~~ still an example\n"),
    ] {
        let text = format!(
            "## ATERM-KI-1110-113 — actual\n{open}\n{inside}\
             ## ATERM-KI-1110-113 — example\n{close}\n\
             \x20\x20\x20\x20## ATERM-KI-1110-113 — indented example\n\
             ## ATERM-KI-1110-147 — another actual\n"
        );
        assert!(run(&ISSUE_ID, &[(INDEX, &text)]).is_empty(), "{text}");
        let duplicate = format!("{text}## ATERM-KI-1110-113 — duplicate\n");
        assert_eq!(run(&ISSUE_ID, &[(INDEX, &duplicate)]).len(), 1);
    }
}

#[test]
fn markdown_heading_whitespace_and_crlf_keep_exact_definition_lines() {
    for indent in ["", " ", "  ", "   "] {
        let text = format!(
            "##\tATERM-KI-1110-113 — first\r\n\
             {indent}## ATERM-KI-1110-113 — second ###\r\n"
        );
        let found = run(&ISSUE_ID, &[(INDEX, &text)]);
        assert_eq!(spots(&found), ["docs/KNOWN_ISSUES.md:2"]);
        assert!(found[0].detail.contains("docs/KNOWN_ISSUES.md:1"));
    }
}

#[test]
fn comment_blocks_hide_drafts_and_cannot_open_code_fences() {
    for indent in ["", " ", "  ", "   "] {
        let text = format!(
            "## ATERM-KI-1110-113 — actual\n\
             {indent}<!-- draft\n\
             ## ATERM-KI-1110-113 — hidden draft\n\
             ```md\n\
             -->\n\
             ## ATERM-KI-1110-147 — another actual\n"
        );
        assert!(run(&ISSUE_ID, &[(INDEX, &text)]).is_empty());
        let repeated = format!("{text}## ATERM-KI-1110-113 — real duplicate\n");
        let found = run(&ISSUE_ID, &[(INDEX, &repeated)]);
        assert_eq!(spots(&found), ["docs/KNOWN_ISSUES.md:7"]);
    }
}

#[test]
fn comment_like_code_and_inline_text_do_not_hide_real_definitions() {
    for example in [
        "<!-- one line -->\n",
        "    <!-- indented code\n",
        "```md\n<!-- fenced code\n```\n",
        "`<!--` is an inline example.\n",
        "\\<!-- is escaped.\n",
    ] {
        let text = format!(
            "## ATERM-KI-1110-113 — first\n{example}\
             ## ATERM-KI-1110-113 — second\n"
        );
        let found = run(&ISSUE_ID, &[(INDEX, &text)]);
        assert_eq!(found.len(), 1, "{text}");
        assert!(found[0].detail.contains("docs/KNOWN_ISSUES.md:1"));
    }
}
