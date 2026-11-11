// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn id(c: char) -> String {
    std::iter::repeat_n(c, 40).collect()
}

fn judged(tree: char, base: char) -> Checked {
    Checked {
        tree: id(tree),
        rows: vec![
            Row {
                check: "fmt".into(),
                word: "ok".into(),
            },
            Row {
                check: "export-scan".into(),
                word: "skip".into(),
            },
            Row {
                check: "test".into(),
                word: "FAIL".into(),
            },
        ],
        base: id(base),
        secs: 2712,
    }
}

/// A receipt reads back as written: two judgments in order, the composed-onto commit and
/// each inherited item, with other trailers and prose passed over.
#[test]
fn a_stated_receipt_reads_back_as_written() {
    let stated = Stated {
        checked: vec![judged('a', 'b'), judged('c', 'd')],
        composed_onto: Some(id('e')),
        inherited: vec!["-p other --lib -- flaky_case".into()],
    };
    let block = stated.trailers();
    assert_eq!(
        block.lines().next(),
        Some(format!(
            "Aterm-Checked: {} rows=fmt:ok,export-scan:skip,test:FAIL base={} secs=2712",
            id('a'),
            id('b')
        ))
        .as_deref()
    );
    let text = format!("Signed-off-by: someone\n{block}Reviewed: yes\n");
    assert_eq!(Stated::of_trailers(&text), Some(stated.clone()));
    assert_eq!(stated.rejudges(), 1);
}

/// NEGATIVE CONTROLS: no `Aterm-Checked` line is no receipt, and one malformed line of any
/// key spoils the whole receipt rather than reading as part of one.
#[test]
fn a_receipt_is_read_whole_or_not_at_all() {
    let good = format!("{CHECKED}: {}\n", judged('a', 'b').value());
    assert!(Stated::of_trailers(&good).is_some());
    assert_eq!(Stated::of_trailers(""), None);
    assert_eq!(
        Stated::of_trailers(&format!("{COMPOSED_ONTO}: {}\n", id('e'))),
        None,
        "no judgment"
    );
    for bad in [
        format!("{CHECKED}: {} rows=fmt:ok base={} secs=1", id('A'), id('b')),
        format!(
            "{CHECKED}: {} rows=fmt:green base={} secs=1",
            id('a'),
            id('b')
        ),
        format!("{CHECKED}: {} rows=Fmt:ok base={} secs=1", id('a'), id('b')),
        format!("{CHECKED}: {} rows=fmt:ok base={} secs=x", id('a'), id('b')),
        format!("{CHECKED}: {} rows=fmt:ok base={}", id('a'), id('b')),
        format!(
            "{CHECKED}: {} rows=fmt:ok base={} secs=1 extra",
            id('a'),
            id('b')
        ),
        format!("{CHECKED}: {} base={} rows=fmt:ok secs=1", id('a'), id('b')),
        format!("{COMPOSED_ONTO}: abc"),
        format!("{COMPOSED_ONTO}: {}\n{COMPOSED_ONTO}: {}", id('e'), id('f')),
        format!("{INHERITED}:  padded"),
        format!("{INHERITED}: twice\n{INHERITED}: twice"),
    ] {
        assert_eq!(
            Stated::of_trailers(&format!("{good}{bad}\n")),
            None,
            "{bad}"
        );
    }
    let sha256 = std::iter::repeat_n('f', 64).collect::<String>();
    assert!(object_id(&sha256) && !object_id(&sha256[..63]));
}
