// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::store::{DocumentStore, DocumentTxnOutcome};

fn at(text: &str, line: usize) -> String {
    Anchor::of_line(text, line).unwrap().to_string()
}

fn apply(text: &str, script: &str) -> Result<String, ScriptError> {
    apply_with(text, script, &PlanOptions::default())
}

fn apply_with(text: &str, script: &str, options: &PlanOptions<'_>) -> Result<String, ScriptError> {
    let plan = Script::parse(script)?.plan(text, options)?;
    Ok(plan.apply(text))
}

const TEXT: &str = "one\ntwo\nthree\nfour\nfive\n";

#[test]
fn each_op_applies_exactly() {
    let t = TEXT;
    let cases = [
        (
            format!("replace {} {} <<END\n2a\n2b\nEND\n", at(t, 2), at(t, 3)),
            "one\n2a\n2b\nfour\nfive\n",
        ),
        (
            format!("replace {} <<END\nTWO\nEND\n", at(t, 2)),
            "one\nTWO\nthree\nfour\nfive\n",
        ),
        (
            format!("replace {} <<END\nEND\n", at(t, 2)),
            "one\nthree\nfour\nfive\n",
        ),
        (
            format!("insert after {} <<X\nnew\nX\n", at(t, 5)),
            "one\ntwo\nthree\nfour\nfive\nnew\n",
        ),
        (
            format!("insert before {} <<X\nnew\nX\n", at(t, 1)),
            "new\none\ntwo\nthree\nfour\nfive\n",
        ),
        (
            "insert at-start <<X\nhead\nX\n".to_string(),
            "head\none\ntwo\nthree\nfour\nfive\n",
        ),
        (
            "insert at-end <<X\ntail\nX\n".to_string(),
            "one\ntwo\nthree\nfour\nfive\ntail\n",
        ),
        (
            format!("delete {} {}\n", at(t, 4), at(t, 5)),
            "one\ntwo\nthree\n",
        ),
        (format!("delete {}\n", at(t, 1)), "two\nthree\nfour\nfive\n"),
        (
            "substitute <<OLD\nthree\nOLD\nwith <<NEW\n3\nNEW\n".to_string(),
            "one\ntwo\n3\nfour\nfive\n",
        ),
        (
            "substitute <<OLD\nwo\nthr\nOLD\nwith <<NEW\nW\nNEW\n".to_string(),
            "one\ntWee\nfour\nfive\n",
        ),
    ];
    for (script, expected) in cases {
        assert_eq!(apply(t, &script).as_deref(), Ok(expected), "{script}");
    }

    let script = format!(
        "# rename and grow\ninsert at-start <<A\nzero\nA\n\nreplace {} <<B # one line\nTWO\nB\ndelete {}\ninsert after {} <<C\nfive and a half\nC\n",
        at(t, 2),
        at(t, 4),
        at(t, 5)
    );
    assert_eq!(
        apply(t, &script).as_deref(),
        Ok("zero\none\nTWO\nthree\nfive\nfive and a half\n")
    );

    let open = "a\nb";
    assert_eq!(
        apply(open, &format!("insert after {} <<X\nc\nX\n", at(open, 2))).as_deref(),
        Ok("a\nb\nc")
    );
    assert_eq!(
        apply(open, "insert at-end <<X\nc\nX\n").as_deref(),
        Ok("a\nb\nc")
    );
    assert_eq!(
        apply(open, &format!("replace {} <<X\nB\nX\n", at(open, 2))).as_deref(),
        Ok("a\nB")
    );
    let crlf = "a\r\nb\r\n";
    assert_eq!(
        apply(crlf, &format!("replace {} <<X\nx\ny\nX\n", at(crlf, 1))).as_deref(),
        Ok("x\r\ny\r\nb\r\n")
    );
    let bom = "\u{feff}a\nb\n";
    assert_eq!(
        apply(bom, &format!("delete {}\n", at(bom, 1))).as_deref(),
        Ok("\u{feff}b\n")
    );
    assert_eq!(
        apply(bom, "insert at-start <<X\nz\nX\n").as_deref(),
        Ok("\u{feff}z\na\nb\n")
    );
    assert_eq!(apply("", "insert at-end <<X\nz\nX\n").as_deref(), Ok("z\n"));
}

#[test]
fn delimiter_word_inside_a_body_line_is_text() {
    let t = TEXT;
    let script = format!(
        "replace {} <<END\nEND of story\n  END\nEND.\n#END\n\nENDEND\nEND\n",
        at(t, 1)
    );
    assert_eq!(
        apply(t, &script).as_deref(),
        Ok("END of story\n  END\nEND.\n#END\n\nENDEND\ntwo\nthree\nfour\nfive\n")
    );
    let script = format!(
        "insert after {} << 'EOF'\n# not a comment\na <<b\nEOF\n",
        at(t, 5)
    );
    assert_eq!(
        apply(t, &script).as_deref(),
        Ok("one\ntwo\nthree\nfour\nfive\n# not a comment\na <<b\n")
    );
    let script = format!("replace {} <<END\r\nTWO\r\nEND\r\n", at(t, 2));
    assert_eq!(
        apply(t, &script).as_deref(),
        Ok("one\nTWO\nthree\nfour\nfive\n")
    );
    let script = format!(
        "delete {}\nreplace {} <<END\nnever closed\n",
        at(t, 1),
        at(t, 2)
    );
    assert!(matches!(
        Script::parse(&script),
        Err(ScriptError::Parse { line: 2, .. })
    ));
}

#[test]
fn substitute_unique_all_and_nth() {
    let text = "let a = 1;\nlet b = a + a;\n";
    let script = |which: &str| format!("substitute <<OLD\na\nOLD\nwith <<NEW\nx\nNEW\n{which}");
    assert_eq!(
        apply(text, &script("")),
        Err(ScriptError::Ambiguous { op: 1, matches: 3 })
    );
    assert_eq!(
        apply(text, &script("all\n")).as_deref(),
        Ok("let x = 1;\nlet b = x + x;\n")
    );
    assert_eq!(
        apply(text, &script("nth=2\n")).as_deref(),
        Ok("let a = 1;\nlet b = x + a;\n")
    );
    assert_eq!(
        apply(text, &script("nth=4\n")),
        Err(ScriptError::Missing { op: 1 })
    );
    let unique = "substitute <<OLD\nb =\nOLD\nwith <<NEW\nc =\nNEW\n";
    assert_eq!(
        apply(text, unique).as_deref(),
        Ok("let a = 1;\nlet c = a + a;\n")
    );
    let none = "substitute <<OLD\nzzz\nOLD\nwith <<NEW\nc\nNEW\nall\n";
    assert_eq!(apply(text, none), Err(ScriptError::Missing { op: 1 }));
    assert_eq!(
        apply(text, "substitute <<OLD\na\nOLD\nwith <<NEW all\nx\nNEW\n").as_deref(),
        Ok("let x = 1;\nlet b = x + x;\n")
    );
    assert!(matches!(
        Script::parse("substitute all <<OLD\na\nOLD\nwith <<NEW\nx\nNEW\nnth=2\n"),
        Err(ScriptError::Parse { line: 7, .. })
    ));
    assert!(matches!(
        Script::parse("substitute <<OLD\nOLD\nwith <<NEW\nx\nNEW\n"),
        Err(ScriptError::Parse { line: 1, .. })
    ));
    assert_eq!(
        apply("a.b\naxb\n", "substitute <<O\n.\nO\nwith <<N\n!\nN\n").as_deref(),
        Ok("a!b\naxb\n")
    );
}

#[test]
fn multi_line_substitute_matches_any_end_of_line() {
    let join = "substitute <<O\nb\nc\nO\nwith <<N\nB\nN\n";
    for (text, expected) in [
        ("a\nb\nc\n", "a\nB\n"),
        ("a\r\nb\r\nc\r\n", "a\r\nB\r\n"),
        ("a\nb\r\nc\r\n", "a\nB\r\n"),
        ("a\r\nb\nc\n", "a\r\nB\n"),
    ] {
        assert_eq!(apply(text, join).as_deref(), Ok(expected), "{text:?}");
    }
    let mixed = "x = 1\r\nif x {\nrun()\r\n} else\n";
    assert_eq!(
        apply(
            mixed,
            "substitute <<O\nx {\nrun()\n}\nO\nwith <<N\nx {\n  run()\n}\nN\n"
        )
        .as_deref(),
        Ok("x = 1\r\nif x {\r\n  run()\r\n} else\n")
    );
    assert_eq!(
        apply(mixed, "substitute <<O\nif\nrun()\n}\nO\nwith <<N\nz\nN\n"),
        Err(ScriptError::Missing { op: 1 })
    );
    assert_eq!(
        apply("a\nb\r\nc\n", "substitute <<O\n\nc\nO\nwith <<N\n!\nN\n").as_deref(),
        Ok("a\nb!\n")
    );
    assert_eq!(
        apply("a\r\nb\n", "substitute <<O\nb\n\nO\nwith <<N\nB\nN\n").as_deref(),
        Ok("a\r\nB")
    );
    assert_eq!(
        apply(
            "a\na\r\na\na\n",
            "substitute <<O\na\na\nO\nwith <<N\nb\nN\nall\n"
        )
        .as_deref(),
        Ok("b\r\nb\n")
    );
    assert_eq!(
        apply("a\nb", "substitute <<O\nb\n\nO\nwith <<N\nB\nN\n"),
        Err(ScriptError::Missing { op: 1 })
    );
}

#[test]
fn overlapping_ops_refused() {
    let t = TEXT;
    let refused = [
        format!(
            "replace {} {} <<A\nx\nA\nreplace {} {} <<B\ny\nB\n",
            at(t, 2),
            at(t, 3),
            at(t, 3),
            at(t, 4)
        ),
        format!(
            "insert after {} <<A\nx\nA\ndelete {} {}\n",
            at(t, 3),
            at(t, 2),
            at(t, 4)
        ),
        format!(
            "replace {} <<A\nx\nA\nsubstitute <<O\nhre\nO\nwith <<N\nHRE\nN\n",
            at(t, 3)
        ),
        format!(
            "insert before {} <<A\nx\nA\nsubstitute <<O\nhre\nO\nwith <<N\nHRE\nN\n",
            at(t, 3)
        ),
        format!(
            "replace {} {} <<A\nx\nA\ninsert after {} <<B\ny\nB\n",
            at(t, 1),
            at(t, 2),
            at(t, 2)
        ),
    ];
    for script in refused {
        assert_eq!(
            apply(t, &script),
            Err(ScriptError::Overlap { op: 2 }),
            "{script}"
        );
    }
    let script = format!(
        "insert after {} <<A\nA1\nA\nreplace {} <<B\nTWO\nB\nreplace {} <<C\nTHREE\nC\ninsert before {} <<D\nB4\nD\ninsert after {} <<E\nA4\nE\ninsert before {} <<F\nB5\nF\n",
        at(t, 1),
        at(t, 2),
        at(t, 3),
        at(t, 4),
        at(t, 4),
        at(t, 5)
    );
    assert_eq!(
        apply(t, &script).as_deref(),
        Ok("one\nA1\nTWO\nTHREE\nB4\nfour\nA4\nB5\nfive\n")
    );

    for open in ["a\nb\nc", "a\r\nb\r\nc"] {
        let at_end = "insert at-end <<X\nX1\nX\n";
        for (changes_c, also) in [
            (format!("delete {}\n", at(open, 3)), true),
            (format!("replace {} <<R\nR\n", at(open, 3)), true),
            ("substitute <<O\nc\nO\nwith <<N\n\nN\n".to_string(), true),
            (
                "substitute <<O\nb\nc\nO\nwith <<N\nb\nN\n".to_string(),
                true,
            ),
            (format!("replace {} <<R\nC\nR\n", at(open, 3)), true),
            (format!("insert after {} <<Y\nnew\nY\n", at(open, 3)), false),
        ] {
            for script in [
                format!("{changes_c}{at_end}"),
                format!("{at_end}{changes_c}"),
            ] {
                let planned = apply(open, &script);
                if also {
                    assert_eq!(planned, Err(ScriptError::Overlap { op: 2 }), "{script}");
                } else {
                    assert!(planned.is_ok(), "{script}: {planned:?}");
                }
            }
        }
    }
    let closed = "a\nb\nc\n";
    assert_eq!(
        apply(
            closed,
            &format!("delete {}\ninsert at-end <<X\nX1\nX\n", at(closed, 3))
        )
        .as_deref(),
        Ok("a\nb\nX1\n")
    );
}

#[test]
fn first_failing_op_in_script_order_names_the_error() {
    let t = TEXT;
    let stale = format!("4:{}", Hash6::of("not four"));
    let script = format!(
        "replace {} <<X\nA\nX\nreplace {} <<X\nB\nX\ndelete {stale}\n",
        at(t, 3),
        at(t, 3)
    );
    assert_eq!(apply(t, &script), Err(ScriptError::Overlap { op: 2 }));
    let script = format!(
        "replace {} <<X\nA\nX\ndelete {stale}\nreplace {} <<X\nB\nX\n",
        at(t, 3),
        at(t, 3)
    );
    assert!(matches!(
        apply(t, &script),
        Err(ScriptError::Conflict { op: 2, line: 4, .. })
    ));
    let script = format!(
        "replace {} {} <<X\nA\nX\nreplace {} <<X\nB\nX\nreplace {} <<X\nC\nX\n",
        at(t, 3),
        at(t, 5),
        at(t, 1),
        at(t, 4)
    );
    assert_eq!(apply(t, &script), Err(ScriptError::Overlap { op: 3 }));
    let script = format!(
        "insert after {} <<X\nA\nX\nsubstitute <<O\ne\nO\nwith <<N\nE\nN\nall\n",
        at(t, 2)
    );
    assert_eq!(
        apply(t, &script).as_deref(),
        Ok("onE\ntwo\nA\nthrEE\nfour\nfivE\n")
    );
    let script = format!(
        "insert after {} <<X\nA\nX\nsubstitute <<O\no\nO\nwith <<N\nO\nN\nall\n",
        at(t, 2)
    );
    assert_eq!(apply(t, &script), Err(ScriptError::Overlap { op: 2 }));
}

#[test]
fn failing_third_op_leaves_text_untouched() {
    let t = TEXT;
    let mut store = DocumentStore::for_test();
    let document = store.open("mem://script".into(), t.into());
    let before = store.snapshot(document).unwrap();
    let stale_anchor = Anchor {
        line: 4,
        h6: Hash6::of("not four"),
    };
    let script = format!(
        "replace {} <<A\nONE\nA\ninsert at-end <<B\nsix\nB\ndelete {stale_anchor}\n",
        at(t, 1)
    );
    let script = Script::parse(&script).unwrap();
    let error = script
        .plan(&before.text, &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        error,
        ScriptError::Conflict {
            op: 3,
            line: 4,
            want: Hash6::of("not four"),
            have: Some(Hash6::of("four")),
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "conflict op=3 line=4 want={} have={}",
            Hash6::of("not four"),
            Hash6::of("four")
        )
    );
    let after = store.snapshot(document).unwrap();
    assert_eq!(after.seq, before.seq);
    assert_eq!(after.text.as_bytes(), t.as_bytes());

    let script = format!(
        "replace {} <<A\nONE\nA\ninsert at-end <<B\nsix\nB\ndelete {}\n",
        at(t, 1),
        at(t, 4)
    );
    let plan = Script::parse(&script)
        .unwrap()
        .plan(&before.text, &PlanOptions::default())
        .unwrap();
    let outcome = store.transact(document, before.seq, plan.edits.clone());
    assert!(
        matches!(outcome, DocumentTxnOutcome::Committed { .. }),
        "{outcome:?}"
    );
    let committed = store.snapshot(document).unwrap();
    assert_eq!(&*committed.text, plan.apply(t));
    assert_eq!(&*committed.text, "ONE\ntwo\nthree\nfive\nsix\n");
}

#[test]
fn near_relocates_only_when_given() {
    let read = TEXT;
    let script = format!("replace {} <<X\nTHREE\nX\n", at(read, 3));
    let moved = "zero\nhalf\none\ntwo\nthree\nfour\nfive\n";
    assert_eq!(
        apply(moved, &script),
        Err(ScriptError::Conflict {
            op: 1,
            line: 3,
            want: Hash6::of("three"),
            have: Some(Hash6::of("one")),
        })
    );
    let near = |n| PlanOptions {
        near: Some(n),
        base_map: None,
    };
    assert_eq!(
        apply_with(moved, &script, &near(1)),
        Err(ScriptError::Conflict {
            op: 1,
            line: 3,
            want: Hash6::of("three"),
            have: Some(Hash6::of("one")),
        })
    );
    assert_eq!(
        apply_with(moved, &script, &near(2)).as_deref(),
        Ok("zero\nhalf\none\ntwo\nTHREE\nfour\nfive\n")
    );
    let span = format!("delete {} {}\n", at(read, 2), at(read, 3));
    assert_eq!(
        apply_with(moved, &span, &near(2)).as_deref(),
        Ok("zero\nhalf\none\nfour\nfive\n")
    );
    let twins = "x\nthree\ny\nthree\n";
    let script = format!("replace 3:{} <<X\nT\nX\n", Hash6::of("three"));
    assert_eq!(
        apply_with(twins, &script, &near(1)),
        Err(ScriptError::Ambiguous { op: 1, matches: 2 })
    );
    let script = format!("replace {} <<X\nT\nX\n", at(twins, 2));
    assert_eq!(
        apply_with(twins, &script, &near(3)).as_deref(),
        Ok("x\nT\ny\nthree\n")
    );

    let read: String = (1..=10).map(|n| format!("line {n}\n")).collect();
    let shrunk: String = (7..=10).map(|n| format!("line {n}\n")).collect();
    let script = format!("replace {} <<X\nTEN\nX\n", at(&read, 10));
    assert_eq!(
        apply_with(&shrunk, &script, &near(6)).as_deref(),
        Ok("line 7\nline 8\nline 9\nTEN\n")
    );
    assert_eq!(
        apply_with(&shrunk, &script, &near(100)).as_deref(),
        Ok("line 7\nline 8\nline 9\nTEN\n")
    );
    assert_eq!(
        apply_with(&shrunk, &script, &near(5)),
        Err(ScriptError::Conflict {
            op: 1,
            line: 10,
            want: Hash6::of("line 10"),
            have: None,
        })
    );
    let hunk = Script::parse_patch("@@ -10 +10 @@\n-line 10\n+TEN\n").unwrap();
    assert_eq!(
        hunk.plan(&shrunk, &near(6)).map(|plan| plan.apply(&shrunk)),
        Ok("line 7\nline 8\nline 9\nTEN\n".to_string())
    );
    assert!(matches!(
        hunk.plan(&shrunk, &near(5)),
        Err(ScriptError::Conflict {
            op: 1,
            line: 10,
            ..
        })
    ));
    let far = |content: &str| format!("replace {}:{} <<X\nT\nX\n", usize::MAX, Hash6::of(content));
    assert_eq!(
        apply_with(&shrunk, &far("gone"), &near(usize::MAX)),
        Err(ScriptError::Conflict {
            op: 1,
            line: usize::MAX,
            want: Hash6::of("gone"),
            have: None,
        })
    );
    assert_eq!(
        apply_with(&shrunk, &far("line 8"), &near(usize::MAX)).as_deref(),
        Ok("line 7\nT\nline 9\nline 10\n")
    );
    let script = format!("replace {} <<X\nT\nX\n", at(&read, 9));
    assert_eq!(
        apply_with(&shrunk, &script, &near(usize::MAX)).as_deref(),
        Ok("line 7\nline 8\nT\nline 10\n")
    );
}

#[test]
fn base_maps_anchors_through_what_changed_since_the_read() {
    let read = TEXT;
    let other = format!(
        "insert at-start <<A\nzero\nA\nreplace {} <<B\nFIVE\nB\n",
        at(read, 5)
    );
    let other = Script::parse(&other)
        .unwrap()
        .plan(read, &PlanOptions::default())
        .unwrap();
    let now = other.apply(read);
    let mut map = LineMap::new(7);
    map.record(other.line_changes(read));
    let options = PlanOptions {
        near: None,
        base_map: Some(&map),
    };
    let script = format!("replace {} <<X\nTHREE\nX\n", at(read, 3));
    assert_eq!(
        apply_with(&now, &script, &options).as_deref(),
        Ok("zero\none\ntwo\nTHREE\nfour\nFIVE\n")
    );
    let script = format!("delete {}\n", at(read, 5));
    assert_eq!(
        apply_with(&now, &script, &options),
        Err(ScriptError::Conflict {
            op: 1,
            line: 5,
            want: Hash6::of("five"),
            have: None,
        })
    );
    let mut inside = LineMap::new(7);
    inside.record([LineChange {
        at: 3,
        removed: 0,
        inserted: 1,
    }]);
    let span = format!("delete {} {}\n", at(read, 2), at(read, 3));
    let options = PlanOptions {
        near: None,
        base_map: Some(&inside),
    };
    assert!(matches!(
        apply_with("one\ntwo\nnew\nthree\nfour\nfive\n", &span, &options),
        Err(ScriptError::Conflict { op: 1, line: 2, .. })
    ));
    let gone = LineMap::unreachable(3);
    let options = PlanOptions {
        near: None,
        base_map: Some(&gone),
    };
    assert_eq!(
        apply_with(read, &script, &options),
        Err(ScriptError::Stale { base: 3 })
    );
    assert_eq!(ScriptError::Stale { base: 3 }.to_string(), "stale base=3");
}

#[test]
fn line_changes_count_whole_lines() {
    let edit = |range: Range<usize>, insert: &str| TextEdit {
        range,
        insert: insert.into(),
    };
    let text = "a\nb\nc\n";
    assert_eq!(
        line_changes(text, &[edit(2..2, "x\ny\n")]),
        [LineChange {
            at: 2,
            removed: 0,
            inserted: 2
        }]
    );
    assert_eq!(
        line_changes(text, &[edit(2..3, "B")]),
        [LineChange {
            at: 2,
            removed: 1,
            inserted: 1
        }]
    );
    assert_eq!(
        line_changes(text, &[edit(1..2, "")]),
        [LineChange {
            at: 1,
            removed: 2,
            inserted: 1
        }]
    );
    assert_eq!(
        line_changes(
            text,
            &[
                edit(6..6, "d\n"),
                edit(0..1, "A"),
                edit(4..5, "C"),
                edit(5..5, "!")
            ]
        ),
        [
            LineChange {
                at: 4,
                removed: 0,
                inserted: 1
            },
            LineChange {
                at: 3,
                removed: 1,
                inserted: 1
            },
            LineChange {
                at: 1,
                removed: 1,
                inserted: 1
            },
        ]
    );
}

#[test]
fn reply_echoes_fresh_anchors_in_script_order() {
    let t = TEXT;
    let script = format!(
        "replace {} <<A\nFOUR\n4b\nA\ninsert at-start <<B\nzero\nB\ndelete {}\n",
        at(t, 4),
        at(t, 2)
    );
    let plan = Script::parse(&script)
        .unwrap()
        .plan(t, &PlanOptions::default())
        .unwrap();
    let post = plan.apply(t);
    assert_eq!(post, "zero\none\nthree\nFOUR\n4b\nfive\n");
    let reply = plan.reply(&post, 9);
    let expected = format!(
        "OK applied hunks=3 seq=9\nhunk 1 {} {}\nhunk 2 {} {}\nhunk 3 {} {}\n",
        at(&post, 4),
        at(&post, 5),
        at(&post, 1),
        at(&post, 1),
        at(&post, 2),
        at(&post, 3)
    );
    assert_eq!(reply, expected);
    let next = format!("delete {}\n", at(&post, 5));
    assert_eq!(
        apply(&post, &next).as_deref(),
        Ok("zero\none\nthree\nFOUR\nfive\n")
    );
    let open = "a\nb";
    let plan = Script::parse(&format!("insert after {} <<X\nc\nX\n", at(open, 2)))
        .unwrap()
        .plan(open, &PlanOptions::default())
        .unwrap();
    let post = plan.apply(open);
    assert_eq!(
        plan.reply(&post, 1),
        format!(
            "OK applied hunks=1 seq=1\nhunk 1 {} {}\n",
            at(&post, 3),
            at(&post, 3)
        )
    );
    let plan = Script::parse(&format!("delete {}\n", at("x\n", 1)))
        .unwrap()
        .plan("x\n", &PlanOptions::default())
        .unwrap();
    assert_eq!(plan.reply("", 2), "OK applied hunks=1 seq=2\nhunk 1 none\n");
}

#[test]
fn patch_format_applies_and_refuses_bad_context() {
    let t = TEXT;
    let patch = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -2,3 +2,3 @@ fn\n two\n-three\n+THREE\n four\n@@ -5 +5,2 @@\n five\n+six\n\n";
    let script = Script::parse_as(Format::Patch, patch).unwrap();
    assert_eq!(script.ops().len(), 2);
    let plan = script.plan(t, &PlanOptions::default()).unwrap();
    assert_eq!(plan.apply(t), "one\ntwo\nTHREE\nfour\nfive\nsix\n");

    let bad = "@@ -2,3 +2,3 @@\n two\n-three\n+THREE\n FOUR\n";
    assert_eq!(
        Script::parse_patch(bad)
            .unwrap()
            .plan(t, &PlanOptions::default()),
        Err(ScriptError::Conflict {
            op: 1,
            line: 4,
            want: Hash6::of("FOUR"),
            have: Some(Hash6::of("four")),
        })
    );
    let moved = "@@ -1,2 +1,2 @@\n two\n-three\n+3\n";
    let script = Script::parse_patch(moved).unwrap();
    assert!(matches!(
        script.plan(t, &PlanOptions::default()),
        Err(ScriptError::Conflict { op: 1, line: 1, .. })
    ));
    let near = PlanOptions {
        near: Some(1),
        base_map: None,
    };
    assert_eq!(
        script.plan(t, &near).unwrap().apply(t),
        "one\ntwo\n3\nfour\nfive\n"
    );
    let open = "@@ -5 +5 @@\n-five\n+FIVE\n\\ No newline at end of file\n@@ -0,0 +1 @@\n+zero\n";
    let plan = Script::parse_patch(open)
        .unwrap()
        .plan(t, &PlanOptions::default())
        .unwrap();
    assert_eq!(plan.apply(t), "zero\none\ntwo\nthree\nfour\nFIVE");
    for patch in [
        "@@ -1 +1 @@\n-a\n+X\n\\ No newline at end of file\n",
        "@@ -1,0 +2 @@\n+X\n\\ No newline at end of file\n",
    ] {
        assert_eq!(
            Script::parse_patch(patch)
                .unwrap()
                .plan("a\nb\n", &PlanOptions::default()),
            Err(ScriptError::Missing { op: 1 })
        );
    }
    let direct = PatchHunk {
        old_start: 1,
        old: vec!["a".into()],
        new: vec!["X".into()],
        old_open: false,
        new_open: true,
    };
    assert_eq!(
        resolve_hunk(1, &direct, &Lines::of("a\nb\n"), &PlanOptions::default()),
        Err(ScriptError::Missing { op: 1 })
    );
    let old_open = "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+X\n";
    let script = Script::parse_patch(old_open).unwrap();
    assert_eq!(
        script
            .plan("a", &PlanOptions::default())
            .unwrap()
            .apply("a"),
        "X\n"
    );
    for text in ["a\n", "a\nb\n"] {
        assert_eq!(
            script.plan(text, &PlanOptions::default()),
            Err(ScriptError::Missing { op: 1 })
        );
    }
    let near = PlanOptions {
        near: Some(1),
        base_map: None,
    };
    assert_eq!(script.plan("a\na", &near).unwrap().apply("a\na"), "a\nX\n");
    for patch in [
        "@@ -1 +1 @@\n a\n\\ No newline at end of file\n",
        "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+a\n\\ No newline at end of file\n",
    ] {
        assert_eq!(
            Script::parse_patch(patch)
                .unwrap()
                .plan("a", &PlanOptions::default())
                .unwrap()
                .apply("a"),
            "a"
        );
    }
    let open_then_more =
        "@@ -1,0 +2 @@\n+last\n\\ No newline at end of file\n@@ -1,0 +3 @@\n+more\n";
    assert_eq!(
        Script::parse_patch(open_then_more)
            .unwrap()
            .plan("a\n", &PlanOptions::default()),
        Err(ScriptError::Overlap { op: 2 })
    );
    let more_then_open =
        "@@ -1,0 +2 @@\n+more\n@@ -1,0 +3 @@\n+last\n\\ No newline at end of file\n";
    assert_eq!(
        Script::parse_patch(more_then_open)
            .unwrap()
            .plan("a\n", &PlanOptions::default())
            .unwrap()
            .apply("a\n"),
        "a\nmore\nlast"
    );
    for bad in [
        "\\ No newline at end of file\n@@ -1 +1 @@\n-a\n+X\n",
        "@@ -1 +1 @@\n\\ No newline at end of file\n-a\n+X\n",
        "@@ -1 +1 @@\n-a\n+X\n\\ not a newline marker\n",
        "@@ -1,2 +1,2 @@\n a\n\\ No newline at end of file\n-b\n+B\n",
        "@@ -1,2 +1 @@\n-a\n\\ No newline at end of file\n-b\n+X\n",
        "@@ -1 +1,2 @@\n-a\n+X\n\\ No newline at end of file\n+Y\n",
        "@@ -1 +1 @@\n-a\n+X\n\\ No newline at end of file\n\\ No newline at end of file\n",
    ] {
        assert!(
            matches!(Script::parse_patch(bad), Err(ScriptError::Parse { .. })),
            "{bad}"
        );
    }
    for (bad, line) in [
        ("@@ -1,2 +1,2 @@\n one\n", 1),
        ("@@ -1 +1 @@\n*one\n", 1),
        ("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-one\n+1\n--- a/g\n", 6),
        ("no hunk here\n", 1),
        ("@@ -x +1 @@\n", 1),
    ] {
        match Script::parse_patch(bad) {
            Err(ScriptError::Parse { line: got, .. }) => assert_eq!(got, line, "{bad}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
}

#[test]
fn parse_errors_name_the_script_line() {
    for (script, line) in [
        ("", 1),
        ("# only a comment\n", 1),
        ("frobnicate 1:abcdef\n", 1),
        ("\nreplace 1:abcdef\n", 2),
        ("replace 1:abcdef <<END extra\nx\nEND\n", 1),
        ("replace 2:abcdef 1:abcdef <<END\nEND\n", 1),
        ("replace 1:abcdef 2:abcdef 3:abcdef <<END\nEND\n", 1),
        ("replace 1:abcdefg <<END\nEND\n", 1),
        ("insert after <<END\nx\nEND\n", 1),
        ("insert at-end <<END\nEND\n", 1),
        ("insert sideways 1:abcdef <<END\nx\nEND\n", 1),
        ("delete 1:abcdef <<END\nEND\n", 1),
        ("delete\n", 1),
        ("substitute <<O\nx\nO\n", 1),
        ("substitute <<O\nx\nO\nwhat <<N\ny\nN\n", 4),
        ("substitute <<O\nx\nO\nwith <<N\ny\nN\nnth=0\n", 7),
    ] {
        match Script::parse(script) {
            Err(ScriptError::Parse { line: got, why }) => {
                assert_eq!(got, line, "{script:?}: {why}");
            }
            other => panic!("{script:?}: {other:?}"),
        }
    }
    assert_eq!("patch".parse::<Format>(), Ok(Format::Patch));
    assert!("diff".parse::<Format>().is_err());
}

#[test]
fn anchors_copied_from_a_read_apply_as_is() {
    use crate::protocol::read::{DOC_READ_CAP, LineRange, format_lines};

    let source = "fn main() {\r\n    let x = 1;\r\n    run(x);\r\n}\r\n";
    let mut store = DocumentStore::for_test();
    let document = store.open("mem://read".into(), source.into());
    let snapshot = store.snapshot(document).unwrap();
    let read = format_lines(&snapshot.text, snapshot.seq.0, LineRange::ALL, DOC_READ_CAP);
    let token = |line: &str| -> String {
        read.lines()
            .find_map(|row| {
                let (anchor, text) = row.split_once('|')?;
                (text == line).then(|| anchor.to_string())
            })
            .unwrap()
    };
    let seq: u64 = read
        .lines()
        .last()
        .and_then(|trailer| {
            trailer
                .split(' ')
                .find_map(|word| word.strip_prefix("seq="))
        })
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(seq, snapshot.seq.0);
    let script = format!(
        "replace {} <<END\n    let x = 2;\nEND\ninsert before {} <<END\n    log(x);\nEND\n",
        token("    let x = 1;"),
        token("}")
    );
    let plan = Script::parse(&script)
        .unwrap()
        .plan(&snapshot.text, &PlanOptions::default())
        .unwrap();
    let outcome = store.transact(document, aterm_buffer::Seq(seq), plan.edits.clone());
    assert!(
        matches!(outcome, DocumentTxnOutcome::Committed { .. }),
        "{outcome:?}"
    );
    let committed = store.snapshot(document).unwrap();
    assert_eq!(
        &*committed.text,
        "fn main() {\r\n    let x = 2;\r\n    run(x);\r\n    log(x);\r\n}\r\n"
    );
    let reply = plan.reply(&committed.text, committed.seq.0);
    let reread = format_lines(
        &committed.text,
        committed.seq.0,
        LineRange::ALL,
        DOC_READ_CAP,
    );
    for hunk in &plan.hunks {
        let (first, last) = hunk.anchors(&committed.text).unwrap();
        for anchor in [first, last] {
            assert!(
                reread.contains(&format!("{anchor}|")),
                "{anchor} not in\n{reread}"
            );
            assert!(reply.contains(&anchor.to_string()), "{reply}");
        }
    }
}

mod conformance {
    //! Tier 1: the planner refines `AmacsAnchor` (`aterm_models_native::models_amacs_anchor`).

    use super::super::*;
    use aterm_models_native::models_amacs_anchor::amacs_anchor_model;
    use aterm_spec::derive::Model;
    use aterm_spec::interp::admits;
    use std::collections::BTreeMap;

    type State = BTreeMap<&'static str, i64>;

    const LINES: usize = 3;
    const TARGET: &str = "target";

    #[derive(Clone, Copy, Debug)]
    struct Scenario {
        want: usize,
        at: usize,
        near: bool,
        matches: usize,
        mode: usize,
    }

    impl Scenario {
        fn all() -> Vec<Self> {
            let mut all = Vec::new();
            for want in 1..=LINES {
                for at in 0..=LINES {
                    for near in [false, true] {
                        for matches in 0..=3 {
                            for mode in 0..=2 {
                                all.push(Self {
                                    want,
                                    at,
                                    near,
                                    matches,
                                    mode,
                                });
                            }
                        }
                    }
                }
            }
            all
        }

        fn text(&self) -> String {
            let mut text = String::new();
            for line in 1..=LINES {
                if line == self.at {
                    text.push_str(TARGET);
                } else {
                    text.push_str(&format!("filler {line}"));
                }
                text.push('\n');
            }
            text.push_str(&"x".repeat(self.matches));
            text.push_str("\ntail\n");
            text
        }

        fn script(&self) -> Script {
            let anchor = Anchor {
                line: self.want,
                h6: Hash6::of(TARGET),
            };
            let which = ["", "all\n", "nth=2\n"][self.mode];
            Script::parse(&format!(
                "replace {anchor} {anchor} <<END\nONE\nEND\nsubstitute <<OLD\nx\nOLD\nwith <<NEW\nY\nNEW\n{which}"
            ))
            .unwrap()
        }

        fn options(&self) -> PlanOptions<'static> {
            PlanOptions {
                near: self.near.then_some(1),
                base_map: None,
            }
        }

        fn start(&self, model: &Model) -> State {
            let mut state = model.init_state();
            let mut fire = |action: &str, times: usize| {
                for _ in 0..times {
                    assert!(model.fire(action, &mut state), "{action} {self:?}");
                }
            };
            fire("MoveAnchor", self.want - 1);
            fire("PlaceContent", self.at);
            fire("GiveNear", usize::from(self.near));
            fire("AddMatch", self.matches);
            fire("ChooseMode", self.mode);
            fire("Start", 1);
            state
        }
    }

    fn int(value: usize) -> i64 {
        i64::try_from(value).unwrap()
    }

    fn applied_in(text: &str, landed: usize, replaced: usize) -> i64 {
        let lines = Lines::of(text);
        let one = landed > 0 && lines.content(landed) == Some("ONE");
        let two = replaced > 0 && text.matches('Y').count() == replaced;
        i64::from(one) + i64::from(two)
    }

    type Resolve = fn(&Script, usize, &str, &PlanOptions<'_>) -> Result<Vec<Region>, ScriptError>;

    fn real_resolve(
        script: &Script,
        op: usize,
        text: &str,
        options: &PlanOptions<'_>,
    ) -> Result<Vec<Region>, ScriptError> {
        resolve_op(op, &script.ops()[op - 1], &Lines::of(text), options)
    }

    fn run(
        model: &Model,
        scenario: &Scenario,
        resolve: Resolve,
        apply_as_resolved: bool,
    ) -> (Vec<State>, String) {
        let script = scenario.script();
        let options = scenario.options();
        let pre = scenario.text();
        let mut held = pre.clone();
        let mut states = vec![scenario.start(model)];
        let mut state = states[0].clone();
        let mut landed = 0;
        let mut replaced = 0;
        let mut staged = Vec::new();
        for op in 1..=2 {
            match resolve(&script, op, &held, &options) {
                Ok(regions) => {
                    if op == 1 {
                        let lines = Lines::of(&held);
                        landed = lines.line_at(regions[0].range.start).unwrap();
                    } else {
                        replaced = regions.len();
                    }
                    if apply_as_resolved {
                        held = plan_of(regions.clone()).apply(&held);
                    }
                    staged.extend(regions);
                    state.insert("landed", int(landed));
                    state.insert("replaced", int(replaced));
                    state.insert("staged", int(op));
                    state.insert("phase", int(op) + 1);
                    state.insert("applied", applied_in(&held, landed, replaced));
                    states.push(state.clone());
                }
                Err(_) => {
                    state.insert("applied", applied_in(&held, landed, replaced));
                    state.insert("outcome", 2);
                    state.insert("phase", 4);
                    states.push(state.clone());
                    return (states, held);
                }
            }
        }
        if !apply_as_resolved {
            let mut admitted = Admitted::default();
            for op in 1..=2 {
                let regions: Vec<Region> = staged
                    .iter()
                    .filter(|region| region.op == op)
                    .cloned()
                    .collect();
                admitted
                    .admit(op, &regions)
                    .expect("op 1 and op 2 touch different lines");
            }
            held = plan_of(staged).apply(&pre);
        }
        state.insert("applied", applied_in(&held, landed, replaced));
        state.insert("outcome", 1);
        state.insert("phase", 4);
        states.push(state);
        (states, held)
    }

    fn refused_step(model: &Model, states: &[State]) -> Option<usize> {
        states
            .windows(2)
            .position(|pair| admits(model, &pair[0], &pair[1]).is_none())
    }

    fn broken_law(model: &Model, states: &[State]) -> Option<&'static str> {
        ["AllOrNothing", "AnchorExact", "SubstituteUnique"]
            .into_iter()
            .find(|law| {
                states
                    .iter()
                    .any(|state| !model.check_invariant(law, state))
            })
    }

    #[test]
    fn conformance_refines_amacs_anchor() {
        let model = amacs_anchor_model();
        let fillers: Vec<Hash6> = (1..=LINES)
            .map(|line| Hash6::of(&format!("filler {line}")))
            .collect();
        assert!(!fillers.contains(&Hash6::of(TARGET)));
        let mut actions = std::collections::BTreeSet::new();
        for scenario in Scenario::all() {
            let (states, held) = run(&model, &scenario, real_resolve, false);
            for pair in states.windows(2) {
                let action = admits(&model, &pair[0], &pair[1]);
                assert!(
                    action.is_some(),
                    "{scenario:?}: the model admits no step {:?} -> {:?}",
                    pair[0],
                    pair[1]
                );
                actions.extend(action);
            }
            assert_eq!(broken_law(&model, &states), None, "{scenario:?}");
            let pre = scenario.text();
            match scenario.script().plan(&pre, &scenario.options()) {
                Ok(plan) => {
                    assert_eq!(states.last().unwrap()["outcome"], 1, "{scenario:?}");
                    assert_eq!(plan.apply(&pre), held, "{scenario:?}");
                }
                Err(_) => {
                    assert_eq!(states.last().unwrap()["outcome"], 2, "{scenario:?}");
                    assert_eq!(held, pre, "{scenario:?}");
                }
            }
        }
        for action in [
            "LandExact",
            "Relocate",
            "Conflict",
            "Substitute",
            "SubstituteRefused",
            "Commit",
        ] {
            assert!(actions.contains(action), "no scenario reached {action}");
        }

        let rigs: [(&str, Resolve, bool, &str); 3] = [
            ("PartialApply", real_resolve, true, "AllOrNothing"),
            ("OffByOne", off_by_one_resolve, false, "AnchorExact"),
            (
                "FirstMatchWins",
                first_match_resolve,
                false,
                "SubstituteUnique",
            ),
        ];
        for (mutant, resolve, apply_as_resolved, law) in rigs {
            let mut caught = 0;
            for scenario in Scenario::all() {
                let (states, _) = run(&model, &scenario, resolve, apply_as_resolved);
                if refused_step(&model, &states).is_some() {
                    caught += 1;
                    assert!(
                        states
                            .iter()
                            .any(|state| !model.check_invariant(law, state)),
                        "{mutant} {scenario:?}: refused without breaking {law}"
                    );
                }
            }
            assert!(caught > 0, "the {mutant} rig passed as a refinement");
        }
    }

    fn off_by_one_resolve(
        script: &Script,
        op: usize,
        text: &str,
        options: &PlanOptions<'_>,
    ) -> Result<Vec<Region>, ScriptError> {
        let lines = Lines::of(text);
        if let (1, Op::Replace { first, body, .. }) = (op, &script.ops()[0])
            && lines.h6(first.line + 1) == Some(first.h6)
        {
            let span = lines.span(first.line).unwrap();
            return Ok(vec![Region {
                op,
                range: span.start..span.end,
                insert: terminated(body, "\n"),
                holds: None,
                opens_tail: false,
            }]);
        }
        real_resolve(script, op, text, options)
    }

    fn first_match_resolve(
        script: &Script,
        op: usize,
        text: &str,
        options: &PlanOptions<'_>,
    ) -> Result<Vec<Region>, ScriptError> {
        match real_resolve(script, op, text, options) {
            Err(ScriptError::Ambiguous { .. }) => {
                let start = text.find('x').unwrap();
                Ok(vec![Region {
                    op,
                    range: start..start + 1,
                    insert: "Y".into(),
                    holds: None,
                    opens_tail: false,
                }])
            }
            other => other,
        }
    }
}
