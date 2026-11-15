// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A hand-laid `[Unreleased]` in the layout the roll ships: every kind, multi-line entries with
/// two-space continuations, a code span whose inner spaces matter, and an entry whose second
/// paragraph follows a blank line.
const HAND: &str = "# Changelog\n\nHead prose.\n\n## [Unreleased]\n\n\
### Added\n\n- **A1.** First added.\n\n- **A2.** Second added,\n  continued.\n\n\
### Changed\n\n- **C1.** `a  b` keeps two spaces.\n\n\
### Deprecated\n\n- **D1.** Deprecated.\n\n\
### Removed\n\n- **R1.** Removed.\n\n\
### Fixed\n\n- **F1.** One paragraph.\n\n  A second paragraph.\n\n- **F2.** Fixed.\n\n\
### Security\n\n- **S1.** Secured.\n\n\
## [0.1.0] - 2026-11-01\n\n- Old.\n";

/// The scaffold `HAND` leaves once its entries are files.
fn scaffold(text: &str) -> String {
    let start = text.find("## [Unreleased]\n\n").unwrap() + "## [Unreleased]\n\n".len();
    let end = text.find("## [0.1.0]").unwrap();
    format!("{}{}", &text[..start], &text[end..])
}

fn fragment(path: &str, text: &str) -> Fragment {
    Fragment {
        path: path.into(),
        text: text.into(),
    }
}

/// HAND's entries as fragments, each named so name order is file order.
fn hand_fragments() -> Vec<Fragment> {
    vec![
        fragment("changes/security/s1.md", "- **S1.** Secured.\n"),
        fragment(
            "changes/added/a2.md",
            "- **A2.** Second added,\n  continued.\n",
        ),
        fragment("changes/fixed/f2.md", "- **F2.** Fixed.\n"),
        fragment("changes/added/a1.md", "- **A1.** First added.\n"),
        fragment(
            "changes/changed/c1.md",
            "- **C1.** `a  b` keeps two spaces.\n",
        ),
        fragment(
            "changes/fixed/f1.md",
            "- **F1.** One paragraph.\n\n  A second paragraph.\n",
        ),
        fragment("changes/removed/r1.md", "- **R1.** Removed.\n"),
        fragment("changes/deprecated/d1.md", "- **D1.** Deprecated.\n"),
    ]
}

/// THE ASSEMBLED `[Unreleased]` IS THE HAND LAYOUT BYTE FOR BYTE: the entries as files, given
/// in any order, render exactly the hand-laid section; the same section as raw entries (the
/// drain of entries written before fragments) renders itself; the derived announcement leads
/// `### Changed`, a raw harness line is dropped in its favour, and raw entries precede the
/// fragments of their kind, which follow in byte order of name.
/// NEGATIVE CONTROL: a scaffold with no fragment renders an empty `[Unreleased]`, not HAND.
#[test]
fn assembled_unreleased_is_the_hand_layout_byte_for_byte() {
    let scaffold = scaffold(HAND);
    assert_eq!(assemble(&scaffold, &hand_fragments(), None).unwrap(), HAND);
    assert_eq!(assemble(HAND, &[], None).unwrap(), HAND);
    assert_ne!(assemble(&scaffold, &[], None).unwrap(), HAND);
    assert_eq!(assemble(&scaffold, &[], None).unwrap(), scaffold);

    let line = format!("{HARNESS_ENTRY_HEAD}0.1.0: `alab claude harness 0.32`.**");
    let stale = format!("{HARNESS_ENTRY_HEAD}0.0.9: `alab claude harness 0.31`.**");
    let raw = HAND.replace(
        "### Changed\n\n- **C1.**",
        &format!("### Changed\n\n- **C0.** Raw.\n\n{stale}\n\n- **C1.**"),
    );
    let extra = [
        fragment("changes/changed/b-peer.md", "- **Peer B.**\n"),
        fragment("changes/changed/a-peer.md", "- **Peer A.**\n"),
    ];
    let assembled = assemble(&raw, &extra, Some(&line)).unwrap();
    assert!(
        assembled.contains(&format!(
            "### Changed\n\n{line}\n\n- **C0.** Raw.\n\n- **C1.** `a  b` keeps two spaces.\n\n\
             - **Peer A.**\n\n- **Peer B.**\n\n### Deprecated"
        )),
        "{assembled}"
    );
    assert!(
        !assembled.contains("0.0.9"),
        "the raw harness line is dropped"
    );
    let none = assemble(&raw, &[], None).unwrap();
    assert!(
        !none.contains(HARNESS_ENTRY_HEAD),
        "no raised version, no line"
    );
    // Lines above [Unreleased] and every released section stay where they were.
    assert!(assembled.starts_with("# Changelog\n\nHead prose.\n\n## [Unreleased]\n\n"));
    assert!(assembled.ends_with("## [0.1.0] - 2026-11-01\n\n- Old.\n"));
}

/// EACH RULE NAMES ITS FILE AND LINE: every way a fragment would ship other bytes than its
/// author meant, or bypass the roll's layout, is refused by its own message.
/// NEGATIVE CONTROL: HAND's fragments pass.
#[test]
fn assemble_refuses_what_would_change_shipped_bytes() {
    for f in hand_fragments() {
        assert!(fragment_findings(&f.path, &f.text).is_empty(), "{f:?}");
    }
    let good = "- **Ok.** Fine.\n";
    for (path, text, expected) in [
        ("changes/fixes/x.md", good, "changes/<kind>/<name>.md"),
        ("changes/x.md", good, "changes/<kind>/<name>.md"),
        ("changes/fixed/deep/x.md", good, "changes/<kind>/<name>.md"),
        ("changes/fixed/x.txt", good, "changes/<kind>/<name>.md"),
        ("changes/fixed/README", good, "changes/<kind>/<name>.md"),
        ("changes/Fixed/x.md", good, "changes/<kind>/<name>.md"),
        ("changes/fixed/X-Upper.md", good, "changes/<kind>/<name>.md"),
        ("changes/fixed/-lead.md", good, "changes/<kind>/<name>.md"),
        (
            "changes/fixed/x.md",
            "- **Ok.**\n## Heading\n",
            "line 2: is a heading",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**\n### Fixed\n",
            "line 2: is a heading",
        ),
        (
            "changes/fixed/x.md",
            "- **One.**\n- **Two.**\n",
            "line 2: starts a second entry",
        ),
        (
            "changes/fixed/x.md",
            "**No lead.**\n",
            "line 1: does not start with `- `",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**\nlazy\n",
            "line 2: indent continuation",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**\r\n",
            "line 1: contains a carriage return",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**",
            "does not end in a newline",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**\n\n",
            "line 2: is a trailing blank line",
        ),
        ("changes/fixed/x.md", "\n- **Ok.**\n", "line 1: is blank"),
        (
            "changes/fixed/x.md",
            "- **Ok.** <!-- note -->\n",
            "line 1: contains an HTML comment",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.** '''\n",
            "line 1: contains '''",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.** \u{1}\n",
            "line 1: contains control character",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**\n=======\n",
            "line 2: contains git conflict marker `=======`",
        ),
        (
            "changes/fixed/x.md",
            "- **Ok.**\n<<<<<<< ours\n",
            "line 2: contains git conflict marker `<<<<<<<`",
        ),
        ("changes/fixed/x.md", "", "is empty"),
        (
            "changes/changed/x.md",
            "- **Harness versions since v0.1.0: `alab claude harness 0.2`.**\n",
            "harness-versions announcement",
        ),
    ] {
        let found = fragment_findings(path, text).join("\n");
        assert!(found.contains(expected), "{path} {text:?}: {found}");
        assert!(found.starts_with(path), "{found}");
        let refused = assemble(&scaffold(HAND), &[fragment(path, text)], None).unwrap_err();
        assert!(refused.iter().any(|f| f.contains(expected)), "{refused:?}");
    }
    let long = format!("- {}\n", "x".repeat(MAX_FRAGMENT_BYTES));
    assert!(fragment_findings("changes/fixed/x.md", &long)[0].contains("over the"));
    let name = format!("changes/fixed/{}.md", "a".repeat(MAX_NAME_BYTES + 1));
    assert!(!fragment_findings(&name, good).is_empty());
    let name = format!("changes/fixed/{}.md", "a".repeat(MAX_NAME_BYTES));
    assert!(fragment_findings(&name, good).is_empty());
    let many: Vec<_> = (0..=MAX_FRAGMENTS)
        .map(|i| fragment(&format!("changes/fixed/f{i}.md"), good))
        .collect();
    assert!(
        assemble(&scaffold(HAND), &many, None)
            .unwrap_err()
            .iter()
            .any(|f| f.contains("one release may ship"))
    );
    // The raw [Unreleased] arm refuses what it cannot place.
    for (raw, expected) in [
        (
            "## [Unreleased]\n\nProse under no heading.\n",
            "holds text that is no entry",
        ),
        (
            "## [Unreleased]\n\n### Fixed\n\nProse.\n",
            "holds text that is no entry",
        ),
        (
            "## [Unreleased]\n\n### Surprise\n\n- x\n",
            "unknown subsection",
        ),
        ("# Changelog\n", "no \"## [Unreleased]\""),
    ] {
        let refused = assemble(raw, &[], None).unwrap_err().join("\n");
        assert!(refused.contains(expected), "{raw:?}: {refused}");
    }
    // It ships an entry under no heading first, where the roll always placed it, and an HTML
    // comment nowhere.
    let raw = "## [Unreleased]\n\n- Under no heading.\n\n<!-- a note\n- not an entry -->\n\n\
               ### Fixed\n\n- **F.**\n";
    assert_eq!(
        assemble(raw, &[fragment("changes/added/a.md", "- **A.**\n")], None).unwrap(),
        "## [Unreleased]\n\n- Under no heading.\n\n### Added\n\n- **A.**\n\n### Fixed\n\n- **F.**\n"
    );
}

/// Entries outside released sections and inside them, as the dev rules read them.
#[test]
fn entries_name_their_section_and_whole_text() {
    let text = "- Stray.\n\n## [Unreleased]\n\n### Fixed\n\n- **A.** One.\n  More.\n\n  Para.\n\n\
                <!--\n- commented\n-->\n## [0.2.0] - d\n\n- **B.** Shipped.\n\n## Appendix\n\n- C.\n";
    let found: Vec<_> = entries(text)
        .into_iter()
        .map(|e| (e.section, e.line, e.text))
        .collect();
    assert_eq!(
        found,
        [
            (Section::Unreleased, 1, "- Stray.".to_string()),
            (
                Section::Unreleased,
                7,
                "- **A.** One.\n  More.\n\n  Para.".to_string()
            ),
            (
                Section::Released("0.2.0"),
                17,
                "- **B.** Shipped.".to_string()
            ),
            (Section::Unreleased, 21, "- C.".to_string()),
        ]
    );
}

/// AN ENTRY THE WINDOW HOLDS TWICE IS REFUSED, NAMING BOTH PLACES: two files, or a file and a
/// raw `[Unreleased]` entry, whose entries are the same words (whitespace folded), so the release
/// would ship it twice; two branches' copies under different names reach main without either
/// judging the other.
/// NEGATIVE CONTROL: the same bold lead with other words assembles.
#[test]
fn an_entry_the_window_holds_twice_is_refused_naming_both() {
    let raw = "## [Unreleased]\n\n### Fixed\n\n- **Raw.** Before\n  fragments.\n";
    let twins = [
        fragment("changes/fixed/a.md", "- **A.** One.\n"),
        fragment("changes/fixed/b.md", "- **A.**  One.\n"),
    ];
    let refused = assemble(raw, &twins, None).unwrap_err().join("\n");
    assert!(
        refused.contains("changes/fixed/b.md: repeats the entry changes/fixed/a.md holds"),
        "{refused}"
    );
    let copy = [fragment(
        "changes/fixed/r.md",
        "- **Raw.** Before fragments.\n",
    )];
    let refused = assemble(raw, &copy, None).unwrap_err().join("\n");
    assert!(
        refused.contains("changes/fixed/r.md: repeats the entry CHANGELOG.md line 5 holds"),
        "{refused}"
    );
    let control = [
        fragment("changes/fixed/a.md", "- **A.** One.\n"),
        fragment("changes/fixed/b.md", "- **A.** Two.\n"),
        fragment("changes/fixed/r.md", "- **Raw.** After fragments.\n"),
    ];
    assemble(raw, &control, None).unwrap();
}

/// AN HTML COMMENT THAT NEVER CLOSES UNDER `[Unreleased]` IS REFUSED AT ITS LINE, rather than
/// swallowing every raw entry after it from the notes (and, by the landing, from main).
/// NEGATIVE CONTROL: a closed comment ships nowhere and the entries after it ship.
#[test]
fn an_unclosed_comment_under_unreleased_is_refused() {
    let raw = "## [Unreleased]\n\n### Fixed\n\n<!-- note\n\n- **A.** one.\n\n- **B.** two.\n\n\
               ## [0.1.0] - d\n\n- Old. -->\n";
    let refused = assemble(raw, &[], None).unwrap_err().join("\n");
    assert!(
        refused.contains("CHANGELOG.md line 5: an HTML comment that never closes"),
        "{refused}"
    );
    let closed = raw.replace("<!-- note", "<!-- note -->");
    assert_eq!(
        assemble(&closed, &[], None).unwrap(),
        "## [Unreleased]\n\n### Fixed\n\n- **A.** one.\n\n- **B.** two.\n\n## [0.1.0] - d\n\n\
         - Old. -->\n"
    );
}

/// A ROOT ENTRY THAT IS ANOTHER CASE OF `changes`, OR A FILE NAMED `changes`, IS NAMED AS THE
/// RELEASE'S STRICT READ NAMES IT.
/// NEGATIVE CONTROL: the exact spelling passes.
#[test]
fn a_case_variant_of_the_entry_directory_is_named() {
    for path in ["Changes/fixed/x.md", "CHANGES/fixed/x.md", "changes"] {
        let found = path_findings(path).join("\n");
        assert!(
            found.starts_with(&format!(
                "{path}: changelog entries live in a directory spelled exactly changes/"
            )),
            "{path}: {found}"
        );
    }
    assert!(path_findings("changes/fixed/x.md").is_empty());
}
