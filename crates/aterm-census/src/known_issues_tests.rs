// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const HEAD: &str = "# Known issues\n\nOpen issues.\n\n";
const A: &str = "## ATERM-KI-1114-1 — a\n\nA's text.\nLeft: a fix.\n";
const B: &str = "## ATERM-KI-1114-2 — b\n\nB's text.\n";
const C: &str = "## ATERM-KI-1114-3 — c\n\nC's text.\n";
const D: &str = "## ATERM-KI-1114-4 — d\n\nD's text.\n";
const E: &str = "## ATERM-KI-1114-5 — e\n\nE's text.\n";
const F: &str = "## ATERM-KI-1114-6 — f\n\nF's text.\n";
const KI1: &str = "ATERM-KI-1114-1";
const KI2: &str = "ATERM-KI-1114-2";

fn file(entries: &[&str]) -> String {
    format!("{HEAD}{}", entries.join("\n"))
}

fn ids(text: &str) -> Vec<Option<&str>> {
    entries(text).iter().map(|entry| entry.id).collect()
}

/// AN ENTRY RUNS FROM ITS LEVEL-TWO HEADING TO THE NEXT HEADING OF LEVEL ONE OR TWO: a
/// level-three heading, a heading in fenced code or an HTML comment, an indented one and one
/// with no space after its marks are its own lines, and its body drops the blank lines after it.
#[test]
fn an_entry_runs_to_the_next_heading_of_level_one_or_two() {
    let text = "# Known issues\n\n\
                ## ATERM-KI-1114-1 — a\n\
                ### details\n\
                ```md\n## ATERM-KI-1114-9 — example\n```\n\
                <!-- a draft\n## ATERM-KI-1114-8 — hidden\n-->\n\
                \x20\x20\x20\x20## ATERM-KI-1114-7 — indented\n\
                ##ATERM-KI-1114-6 no space\n\n\n\
                ## Not an issue\n\
                text\n\
                # A section\n\
                outside\n\
                ## ATERM-KI-1114-2 — b\r\n\
                last\r\n";
    let found = entries(text);
    assert_eq!(
        found.iter().map(|e| (e.id, e.line)).collect::<Vec<_>>(),
        [
            (Some("ATERM-KI-1114-1"), 3),
            (None, 15),
            (Some("ATERM-KI-1114-2"), 19)
        ]
    );
    assert!(
        found[0]
            .body
            .starts_with("## ATERM-KI-1114-1 — a\n### details\n```md")
    );
    assert!(found[0].body.ends_with("##ATERM-KI-1114-6 no space"));
    assert!(text[found[0].span.clone()].ends_with("no space\n\n\n"));
    assert_eq!(found[1].body, "## Not an issue\ntext");
    assert_eq!(found[2].body, "## ATERM-KI-1114-2 — b\r\nlast");
    assert_eq!(found[2].span.end, text.len());
    assert!(entries("no heading at all\n").is_empty());
}

/// A TEXT'S PARTS ARE THE TEXT ABOVE ITS FIRST ENTRY AND EACH ENTRY BY ITS IDENTIFIER, a
/// heading in a closed fence its entry's own line, on CRLF text too, in the order the text holds
/// them. CONTROLS: no parts where an entry names no identifier or one another names, where a
/// level-one heading follows an entry (the last included), or where a fence or an HTML comment is
/// open at the text's end.
#[test]
fn a_texts_parts_are_its_head_and_each_entry_by_its_identifier() {
    let text = file(&[A, B]);
    assert_eq!(
        parts(&text),
        Some(BTreeMap::from([
            (super::HEAD, HEAD.trim_end()),
            ("ATERM-KI-1114-1", A.trim_end()),
            ("ATERM-KI-1114-2", B.trim_end()),
        ]))
    );
    assert_eq!(
        ordered(&file(&[B, A])).unwrap(),
        [
            (super::HEAD, HEAD.trim_end()),
            ("ATERM-KI-1114-2", B.trim_end()),
            ("ATERM-KI-1114-1", A.trim_end()),
        ]
    );
    assert_eq!(
        parts("no entry\n"),
        Some(BTreeMap::from([(super::HEAD, "no entry")]))
    );
    let crlf = "# Known issues\r\n\r\n## ATERM-KI-1114-1 — a\r\n```\r\n## ATERM-KI-1114-9 — example\r\n\
                ```\r\n\r\n## ATERM-KI-1114-2 — b\r\nlast\r\n";
    let held = parts(crlf).unwrap();
    assert_eq!(
        held.keys().copied().collect::<Vec<_>>(),
        [super::HEAD, "ATERM-KI-1114-1", "ATERM-KI-1114-2"]
    );
    assert!(held["ATERM-KI-1114-1"].contains("ATERM-KI-1114-9"));

    for unread in [
        file(&[A, "## Not an issue\n\ntext\n"]),
        file(&[A, B, A]),
        file(&[A, "# A section\n\noutside\n", B]),
        file(&[A, B, "# Closed\n\ntext\n"]),
        file(&[A, "```\n## ATERM-KI-1114-9 — example\n", B]),
        file(&[A, "<!-- a draft\n", B]),
    ] {
        assert_eq!(parts(&unread), None, "{unread}");
    }
}

/// AN ENTRY-LEVEL MERGE TAKES EACH PART FROM THE SIDE THAT CHANGED IT: a new entry, a changed
/// one, a deleted one and one both sides changed the same way, from either side (a change of one
/// side alone from each), the part neither changed from both. CONTROLS: open where both changed it unlike each other, a
/// deletion against a change included.
#[test]
fn an_entry_level_merge_takes_each_part_from_the_side_that_changed_it() {
    let changed = |entry: &str| entry.replace("text.", "new text.");
    let (a2, b2, c2, c3) = (
        changed(A),
        changed(B),
        changed(C),
        C.replace("text.", "other text."),
    );
    let base = file(&[A, B, C, D]);
    let ours = file(&[E, A, &b2, &c3]);
    let theirs = file(&[&a2, &b2, &c2, F]);
    let deleted = file(&[A, C]);
    let [base, ours, theirs, deleted] =
        [&base, &ours, &theirs, &deleted].map(|text| parts(text).unwrap());
    use Merged::{Gone, Holds, Open};
    use Side::{Both, Ours, Theirs};
    assert_eq!(
        merge(&base, &ours, &theirs),
        BTreeMap::from([
            (super::HEAD, Holds(HEAD.trim_end(), Both)),
            ("ATERM-KI-1114-1", Holds(a2.trim_end(), Theirs)),
            ("ATERM-KI-1114-2", Holds(b2.trim_end(), Both)),
            ("ATERM-KI-1114-3", Open),
            ("ATERM-KI-1114-4", Gone(Both)),
            ("ATERM-KI-1114-5", Holds(E.trim_end(), Ours)),
            ("ATERM-KI-1114-6", Holds(F.trim_end(), Theirs)),
        ])
    );
    let alone = file(&[A, &b2, C, D]);
    let alone = parts(&alone).unwrap();
    assert_eq!(
        merge(&base, &alone, &base)["ATERM-KI-1114-2"],
        Holds(b2.trim_end(), Ours)
    );
    assert_eq!(
        merge(&base, &base, &alone)["ATERM-KI-1114-2"],
        Holds(b2.trim_end(), Theirs)
    );
    let merged = merge(&base, &deleted, &theirs);
    assert_eq!(merged["ATERM-KI-1114-2"], Open);
    assert_eq!(merged["ATERM-KI-1114-4"], Gone(Both));
    let kept = file(&[A, B, C, D]);
    let kept = parts(&kept).unwrap();
    assert_eq!(merge(&base, &deleted, &kept)["ATERM-KI-1114-2"], Gone(Ours));
    assert_eq!(
        merge(&base, &kept, &deleted)["ATERM-KI-1114-4"],
        Gone(Theirs)
    );
}

/// A REWRITE HOLDS THE TEXT TO THE PARTS IT WANTS, KEEPING ONE BLANK LINE BETWEEN THEM: the text
/// of an entry the union left as a bare heading, glued to the one after it, put back; an entry
/// glued to the one before it, two together and the last taken out, after which the text ends
/// with one line end; an entry the text does not hold put in where it is wanted; two entries
/// swapped; a changed entry of CRLF text keeps the CRLF blank line after it. CONTROLS: no change,
/// or one of a key the text does not name, is the text byte for byte; a text `parts` does not
/// read is not rewritten.
#[test]
fn a_rewrite_keeps_one_blank_line_between_the_parts_it_leaves() {
    const KI3: &str = "ATERM-KI-1114-3";
    let bare = format!("{HEAD}## ATERM-KI-1114-1 — a\n{B}");
    assert_eq!(
        rewritten(&bare, &[(KI1, Some(A.trim_end()))]),
        file(&[A, B])
    );
    let glued = format!("{HEAD}{A}{B}\n{C}");
    assert_eq!(rewritten(&glued, &[(KI2, None)]), file(&[A, C]));
    assert_eq!(
        rewritten(&file(&[A, B, C, D]), &[(KI2, None), (KI3, None)]),
        file(&[A, D])
    );
    assert_eq!(rewritten(&file(&[A, B, C]), &[(KI3, None)]), file(&[A, B]));
    let none = rewritten(&file(&[A]), &[(KI1, None)]);
    assert_eq!(ids(&none), []);
    assert!(none.ends_with("Open issues.\n"));
    let wanted = |entries: &[&'static str]| -> Vec<(&'static str, &'static str)> {
        let mut want = vec![(super::HEAD, HEAD.trim_end())];
        want.extend(entries.iter().map(|entry| {
            let key = &entry[3..18];
            (key, entry.trim_end())
        }));
        want
    };
    assert_eq!(
        rewrite(&file(&[A, C]), &wanted(&[A, B, C])),
        Some(file(&[A, B, C]))
    );
    assert_eq!(
        rewrite(&file(&[A, B]), &wanted(&[B, A])),
        Some(file(&[B, A]))
    );
    let crlf = "# K\r\n\r\n## ATERM-KI-1114-1 — a\r\nold\r\n\r\n## ATERM-KI-1114-2 — b\r\nlast\r\n";
    assert_eq!(
        rewritten(crlf, &[(KI1, Some("## ATERM-KI-1114-1 — a\r\nnew"))]),
        crlf.replace("old", "new")
    );

    let text = format!("{HEAD}{A}\n\n\n{B}");
    assert_eq!(rewritten(&text, &[]), text);
    assert_eq!(rewritten(&text, &[("ATERM-KI-1114-9", None)]), text);
    assert_eq!(rewrite(&file(&[A, A]), &wanted(&[A])), None);
}

/// `text` rewritten with each part `changes` names as it says: `Some` text in place of the
/// part's, `None` the part taken out.
fn rewritten(text: &str, changes: &[(&str, Option<&str>)]) -> String {
    let changes: BTreeMap<&str, Option<&str>> = changes.iter().copied().collect();
    let want: Vec<(&str, &str)> = ordered(text)
        .unwrap()
        .into_iter()
        .filter_map(|(key, body)| match changes.get(key) {
            None => Some((key, body)),
            Some(now) => now.map(|now| (key, now)),
        })
        .collect();
    rewrite(text, &want).unwrap()
}

/// A REWRITE OF CRLF TEXT WRITES CRLF LINE ENDS: a part it puts in is followed by a CRLF blank
/// line, and as the last by a CRLF.
#[test]
fn a_rewrite_of_crlf_text_writes_crlf_line_ends() {
    let text = "# K\r\n\r\n## ATERM-KI-1114-1 — a\r\n\r\nold\r\n";
    let a = "## ATERM-KI-1114-1 — a\r\n\r\nold";
    let b = "## ATERM-KI-1114-2 — b\r\n\r\nnew";
    assert_eq!(
        rewrite(text, &[(super::HEAD, "# K"), (KI2, b), (KI1, a)]).as_deref(),
        Some(
            "# K\r\n\r\n## ATERM-KI-1114-2 — b\r\n\r\nnew\r\n\r\n## ATERM-KI-1114-1 — a\r\n\r\nold\r\n"
        )
    );
    assert_eq!(
        rewrite(
            text,
            &[
                (super::HEAD, "# K"),
                (KI1, "## ATERM-KI-1114-1 — a"),
                (KI2, b)
            ]
        )
        .as_deref(),
        Some("# K\r\n\r\n## ATERM-KI-1114-1 — a\r\n\r\n## ATERM-KI-1114-2 — b\r\n\r\nnew\r\n")
    );
}

/// A LONGEST COMMON SUBSEQUENCE MATCHES LINES THAT HOLD TEXT FIRST: two blank lines are not
/// matched in place of one line of text, and of equal matches the earliest is taken.
#[test]
fn a_match_of_lines_prefers_lines_that_hold_text() {
    assert_eq!(matched(&["", "", "x."], &["x.", "", ""]), [(2, 0)]);
    assert_eq!(
        matched(&["a.", "", "b."], &["a.", "", "b."]),
        [(0, 0), (1, 1), (2, 2)]
    );
    assert_eq!(matched(&["a.", "a."], &["a."]), [(0, 0)]);
    assert_eq!(matched(&[], &["a."]), []);
}

/// A PART'S THREE-WAY MERGE KEEPS EVERY LINE A SIDE WROTE AND NONE A SIDE TOOK OUT
/// ([`merge_part`]): where both sides added a line at one place ours comes first, a line both
/// added is held once, each side's deletion stands (two adjacent ones included), a change
/// against a deletion is the change, and a line one side changed keeps its place beside the
/// other's addition; an entry's heading is one value (a retitle against the text put back keeps
/// both, and of two retitles theirs stands), the text above the first entry is merged whole,
/// CRLF text keeps its line ends, and a part the base does not hold is merged from nothing.
#[test]
fn a_parts_merge_keeps_every_line_a_side_wrote_and_none_it_took_out() {
    let merged = |base: &str, ours: &str, theirs: &str| merge_part(KI1, base, ours, theirs);
    let x = |text: &str| format!("## ATERM-KI-1114-1 — x\n\n{text}");
    assert_eq!(
        merged(&x("base."), &x("base.\nours."), &x("base.\ntheirs.")),
        x("base.\nours.\ntheirs.")
    );
    assert_eq!(
        merged(&x("base."), &x("base.\nsame.\nours."), &x("base.\nsame.")),
        x("base.\nsame.\nours.")
    );
    assert_eq!(
        merged(
            &x("one.\ntwo.\nthree."),
            &x("two.\nthree."),
            &x("one.\nthree.")
        ),
        x("three.")
    );
    assert_eq!(
        merged(&x("old.\nkept."), &x("kept."), &x("new.\nkept.")),
        x("new.\nkept.")
    );
    assert_eq!(
        merged(
            &x("A.\nbranch: 3."),
            &x("A.\nbranch: 3.\nmain: 7."),
            &x("A.\nbranch: 4.")
        ),
        x("A.\nbranch: 4.\nmain: 7.")
    );
    assert_eq!(
        merged(
            "## ATERM-KI-1114-1 — e",
            "## ATERM-KI-1114-1 — e\n\nE's text.",
            "## ATERM-KI-1114-1 — e, narrowed"
        ),
        "## ATERM-KI-1114-1 — e, narrowed\n\nE's text."
    );
    assert_eq!(
        merged(
            "## ATERM-KI-1114-1 — e",
            "## ATERM-KI-1114-1 — mine",
            "## ATERM-KI-1114-1 — theirs"
        ),
        "## ATERM-KI-1114-1 — theirs"
    );
    assert_eq!(
        merge_part(
            super::HEAD,
            "# K\n\nintro.",
            "# K\n\nintro.\nmine.",
            "# K2\n\nintro."
        ),
        "# K2\n\nintro.\nmine."
    );
    let crlf = |text: &str| x(text).replace('\n', "\r\n");
    assert_eq!(
        merged(
            &crlf("base."),
            &crlf("base.\nours."),
            &crlf("base.\ntheirs.")
        ),
        crlf("base.\nours.\ntheirs.")
    );
    assert_eq!(merged("", &x("ours."), &x("theirs.")), x("ours.\ntheirs."));
}
