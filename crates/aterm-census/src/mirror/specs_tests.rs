// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_clause_is_its_row_and_its_notes_cell_is_apart() {
    let specs = "intro\n\nid\tkind\tfunctions\tstatement\tgaps\n\
                 a-b\ttable\tf\tf(x) =  x\tProved via mirror: Clean R1 row `r`\n\nprose\n";
    let found = clauses(specs);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].line, 4);
    let respaced =
        clauses("id\tkind\tfunctions\tstatement\tgaps\na-b\ttable\tf\tf(x) = x\tother\n");
    assert_eq!(
        found[0].digest(),
        respaced[0].digest(),
        "notes are not the clause"
    );
    let refunctioned = clauses("id\tkind\tfunctions\tstatement\tgaps\na-b\ttable\tg\tf(x) = x\t\n");
    assert_ne!(
        found[0].digest(),
        refunctioned[0].digest(),
        "the functions column is"
    );
    let misnamed = clauses(
        "id\tkind\tfunctions\tstatement\tblocking_gaps\ttests\tdifficulty\n\
         a-b\ttable\tf\tf(x) = x\t1,3\t3/3\tProved via mirror: Clean R1 row `r`\n",
    );
    let notes: Vec<bool> = misnamed[0].cells.iter().map(|c| c.notes).collect();
    assert_eq!(
        notes,
        [false, false, false, false, false, false, true],
        "the last cell holds the notes whatever its header says"
    );
    let short = clauses("id\tkind\tfunctions\tstatement\na-b\ttable\tf\tf(x) = x\n");
    assert!(
        short[0].cells.iter().all(|c| !c.notes),
        "the statement is never a note"
    );
    let retested = clauses(
        "id\tkind\tfunctions\tstatement\tblocking_gaps\ttests\tdifficulty\n\
         a-b\ttable\tf\tf(x) = x\t2\tother_test\tProved via mirror: Clean R1 row `r`\n",
    );
    assert_eq!(
        misnamed[0].digest(),
        retested[0].digest(),
        "the tests and difficulty cells are not what the clause says"
    );
}

#[test]
fn a_pipe_table_and_a_repeated_id_are_read() {
    let specs = "| id | fn | statement | notes |\n| --- | --- | --- | --- |\n\
                 | `a` | `f` | `x | y` | Proved via mirror: Clean R1 row `r` |\n\
                 | `a` | `f` | other | |\n\ntext\n";
    let found = clauses(specs);
    assert_eq!(found.len(), 2);
    assert_eq!((found[0].id.as_str(), found[1].line), ("a", 4));
    assert_eq!(found[0].cells[2].text, "`x | y`");
    assert!(found[0].cells[3].notes);
    assert_ne!(found[0].digest(), found[1].digest());
}

#[test]
fn notes_name_their_rows() {
    let (found, bad) = notes(
        "3; Proved via mirror: Clean R213 rows `a_b`, `c` and `d` (`RowProvedB3`); \
         stale proof: Clean R212 row `e` (moved)",
    );
    assert_eq!(bad, Vec::<String>::new());
    assert_eq!(
        found,
        [
            Note {
                stale: false,
                clean: "R213".into(),
                rows: vec!["a_b".into(), "c".into(), "d".into()]
            },
            Note {
                stale: true,
                clean: "R212".into(),
                rows: vec!["e".into()]
            }
        ]
    );
}

#[test]
fn a_claim_in_any_other_wording_is_unreadable() {
    for text in [
        "Proved  via mirror: Clean R1 row `a`",
        "Proved\u{a0}via mirror: Clean R1 row `a`",
        "Mirror-proved in Clean R1",
        "proved (via mirror)",
        "proved-via-mirror",
        "Proved via mirror: Clean R1, row 4",
        "Proved via mirror: R1 row `a`",
    ] {
        assert!(!notes(text).1.is_empty(), "{text}");
    }
    for text in [
        "not yet proved via mirror",
        "atlas-mirrors-cpu-glyph-cache",
        "the mirror word",
        "Accepting a stale proof is rejected",
    ] {
        assert_eq!(notes(text), (Vec::new(), Vec::new()), "{text}");
    }
}
