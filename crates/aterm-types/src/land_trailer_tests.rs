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

fn rechecked(tree: char, onto: char, after: usize) -> Rechecked {
    Rechecked {
        tree: id(tree),
        rows: ["fmt", "census", "export-scan"]
            .into_iter()
            .map(|check| Row {
                check: check.into(),
                word: "ok".into(),
            })
            .collect(),
        base: id(onto),
        onto: id(onto),
        secs: 95,
        after,
    }
}

fn interrupted(tree: char, base: char, tip: char) -> Interrupted {
    Interrupted {
        tree: id(tree),
        base: id(base),
        tip: id(tip),
        secs: 420,
    }
}

/// A receipt reads back as written: two judgments in order, the interrupted one between them,
/// the composed-onto commit and each inherited item, with other trailers and prose passed over.
/// Its re-judges and seconds count the interrupted judgment too.
#[test]
fn a_stated_receipt_reads_back_as_written() {
    let stated = Stated {
        checked: vec![judged('a', 'b'), judged('c', 'd')],
        interrupted: vec![interrupted('a', 'f', 'd')],
        rechecked: Vec::new(),
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
    assert!(
        block.contains(&format!(
            "\nAterm-Interrupted: {} base={} tip={} secs=420\n",
            id('a'),
            id('f'),
            id('d')
        )),
        "{block}"
    );
    let text = format!("Signed-off-by: someone\n{block}Reviewed: yes\n");
    assert_eq!(Stated::of_trailers(&text), Some(stated.clone()));
    assert_eq!(stated.rejudges(), 2);
    assert_eq!(stated.secs(), 2712 + 2712 + 420);
    let uninterrupted = Stated {
        interrupted: Vec::new(),
        ..stated
    };
    assert_eq!(
        (uninterrupted.rejudges(), uninterrupted.secs()),
        (1, 2712 + 2712)
    );
}

/// NEGATIVE CONTROLS: no `Aterm-Checked` line is no receipt, though an `Aterm-Interrupted` one
/// is there, and one malformed line of any key spoils the whole receipt rather than reading as
/// part of one.
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
    assert_eq!(
        Stated::of_trailers(&format!(
            "{INTERRUPTED}: {}\n",
            interrupted('a', 'b', 'c').value()
        )),
        None,
        "an interrupted judgment alone is no verdict"
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
        format!("{INTERRUPTED}: {} base={} tip=abc secs=1", id('a'), id('b')),
        format!(
            "{INTERRUPTED}: {} tip={} base={} secs=1",
            id('a'),
            id('b'),
            id('c')
        ),
        format!(
            "{INTERRUPTED}: {} base={} tip={} secs=-1",
            id('a'),
            id('b'),
            id('c')
        ),
        format!(
            "{INTERRUPTED}: {} base={} tip={}",
            id('a'),
            id('b'),
            id('c')
        ),
        format!(
            "{INTERRUPTED}: {} base={} tip={} secs=1 extra",
            id('a'),
            id('b'),
            id('c')
        ),
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

/// A recheck reads back as written, in its place: its line follows the judgment it extends, so
/// the landing's `onto` is the last recheck's tip only while no judgment ran after it. Rechecks
/// add their seconds and are no re-judge.
#[test]
fn a_recheck_reads_back_in_its_place_and_is_no_rejudge() {
    let stated = Stated {
        checked: vec![judged('a', 'b')],
        rechecked: vec![rechecked('c', 'd', 1), rechecked('e', 'f', 1)],
        ..Stated::default()
    };
    let block = stated.trailers();
    assert_eq!(
        block.lines().nth(1),
        Some(format!(
            "Aterm-Rechecked: {} rows=fmt:ok,census:ok,export-scan:ok base={} onto={} secs=95",
            id('c'),
            id('d'),
            id('d')
        ))
        .as_deref(),
        "{block}"
    );
    assert_eq!(Stated::of_trailers(&block), Some(stated.clone()));
    assert_eq!((stated.rejudges(), stated.secs()), (0, 2712 + 95 + 95));
    assert_eq!(stated.onto(), Some(id('f').as_str()));
    // A judgment after the rechecks (a later tip that did not compose) is the last judgment:
    // the landing was judged where it lands, and the earlier rechecks still count their time.
    let rejudged = Stated {
        checked: vec![judged('a', 'b'), judged('7', '8')],
        ..stated.clone()
    };
    let block = rejudged.trailers();
    let order: Vec<&str> = block
        .lines()
        .map(|line| line.split_once(':').unwrap().0)
        .collect();
    assert_eq!(order, [CHECKED, RECHECKED, RECHECKED, CHECKED], "{block}");
    assert_eq!(Stated::of_trailers(&block), Some(rejudged.clone()));
    assert_eq!(rejudged.onto(), None);
    assert!(rejudged.rechecks_since_judged().is_empty());
    assert_eq!(
        (rejudged.rejudges(), rejudged.secs()),
        (1, 2712 * 2 + 95 * 2)
    );
    // A run-free composition after a recheck names the commit the landing is composed onto.
    let composed = Stated {
        composed_onto: Some(id('9')),
        ..stated
    };
    assert_eq!(composed.onto(), Some(id('9').as_str()));
}

/// NEGATIVE CONTROLS: a malformed recheck spoils the receipt like any key's, a recheck alone is
/// no receipt, and a reader that knows no `Aterm-Rechecked` key (one built before it) reads the
/// rest of the block exactly as it reads the block without that line.
#[test]
fn a_recheck_is_read_whole_and_older_readers_pass_it_over() {
    let good = format!("{CHECKED}: {}\n", judged('a', 'b').value());
    let recheck = format!("{RECHECKED}: {}\n", rechecked('c', 'd', 1).value());
    assert_eq!(
        Stated::of_trailers(&recheck),
        None,
        "a recheck is no judgment"
    );
    for bad in [
        format!(
            "{RECHECKED}: {} rows=fmt:ok base={} onto={} secs=1 extra",
            id('c'),
            id('d'),
            id('d')
        ),
        format!(
            "{RECHECKED}: {} rows=fmt:ok base={} onto=abc secs=1",
            id('c'),
            id('d')
        ),
        format!(
            "{RECHECKED}: {} rows=fmt:green base={} onto={} secs=1",
            id('c'),
            id('d'),
            id('d')
        ),
        format!(
            "{RECHECKED}: {} rows=fmt:ok base={} secs=1",
            id('c'),
            id('d')
        ),
        format!(
            "{RECHECKED}: {} rows=fmt:ok onto={} base={} secs=1",
            id('c'),
            id('d'),
            id('d')
        ),
    ] {
        assert_eq!(
            Stated::of_trailers(&format!("{good}{bad}\n")),
            None,
            "{bad}"
        );
    }
    // The golden: a landing's block as main's landings wrote it before rechecks, byte for byte.
    let golden = format!(
        "{CHECKED}: {a} rows=fmt:ok,export-scan:skip,test:FAIL base={b} secs=2712\n\
         {INTERRUPTED}: {a} base={f} tip={d} secs=420\n\
         {COMPOSED_ONTO}: {e}\n\
         {INHERITED}: -p other --lib -- flaky_case\n",
        a = id('a'),
        b = id('b'),
        d = id('d'),
        e = id('e'),
        f = id('f'),
    );
    let before = Stated::of_trailers(&golden).unwrap();
    assert_eq!(before.trailers(), golden);
    let with = format!("{good}{recheck}{}", &golden[good.len()..]);
    assert_eq!(with.lines().next(), golden.lines().next());
    // An older reader is this one with the key unknown: it passes the line over.
    let older = with.replace(RECHECKED, "Aterm-Unknown-Key");
    assert_eq!(Stated::of_trailers(&older), Some(before.clone()));
    let read = Stated::of_trailers(&with).unwrap();
    assert_eq!(
        Stated {
            rechecked: Vec::new(),
            ..read
        },
        before
    );
}
