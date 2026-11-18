// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_db::att::EntryKind;
use aterm_db::file::Renamed;
use aterm_types::machine_name::{Nickname, Number};

use super::{History, NO_HISTORY, NO_ROW, NameCell, NameWrite, read, undo_of};
use crate::machine_name::{Cause, NameTo, UndoTo};

const ROW: &str = "mc-0123456789abcdef";

/// What one write left in `cell`: `value`, standing or not, over `replaced`.
fn cell(cell: Renamed, value: Option<&str>, stands: bool, replaced: &[Option<&str>]) -> NameCell {
    NameCell {
        cell,
        value: value.map(str::to_owned),
        stands,
        replaced: replaced
            .iter()
            .map(|value| value.map(str::to_owned))
            .collect(),
        unseen: Vec::new(),
    }
}

/// Write `op` of `kind` by Mac `m8`, authored at `op` seconds, leaving `cells`.
fn write(op: u8, kind: EntryKind, cells: Vec<NameCell>) -> NameWrite {
    NameWrite {
        op: [op; 32],
        at_ms: u64::from(op) * 1_000,
        mac: "m8".to_owned(),
        kind,
        reverts: None,
        carried: false,
        cells,
    }
}

/// This Mac's row holding `writes`, newest first.
fn history(writes: Vec<NameWrite>) -> History {
    History {
        row: Some(ROW.to_owned()),
        pending: Vec::new(),
        writes,
    }
}

fn num(word: &str) -> Number {
    Number::parse(word).expect("a number")
}

fn nick(word: &str) -> Nickname {
    Nickname::parse(word).expect("a nickname")
}

/// The owner rename of `m8 Heron` to `m86 Marigold` over registration's automatic `m8 Heron`.
fn renamed() -> NameWrite {
    write(
        2,
        EntryKind::Explicit,
        vec![
            cell(Renamed::Label, Some("m86"), true, &[Some("m8")]),
            cell(Renamed::Nickname, Some("Marigold"), true, &[Some("Heron")]),
        ],
    )
}

/// Undo takes back the row's newest owner write, from the values it replaced, even where aterm
/// wrote the same label again after it, and the name it leaves is the number and nickname it
/// gives back: the KI-1114-532 case, an owner's label restored over registration's automatic
/// one, writes the replaced label as a new owner write, which registration leaves alone.
/// NEGATIVE CONTROL: with `undo_of` taking the newest write of any kind (its `EntryKind::Explicit`
/// filter removed), the later automatic `m86` is taken, which replaced its own text, and the undo
/// is refused: `` nothing to undo: the last rename left this Mac's number and nickname as they
/// were ``.
#[test]
fn undo_takes_back_the_rows_newest_owner_write() {
    let born = write(
        1,
        EntryKind::Aterm,
        vec![
            cell(Renamed::Label, Some("m8"), false, &[]),
            cell(Renamed::Nickname, Some("Heron"), false, &[]),
        ],
    );
    let mut rename = renamed();
    rename.cells[0].stands = false;
    let again = write(
        3,
        EntryKind::Aterm,
        vec![cell(Renamed::Label, Some("m86"), true, &[Some("m86")])],
    );
    let undo = undo_of(&history(vec![again, rename, born])).unwrap();
    assert_eq!(undo.op, [2; 32]);
    assert_eq!(
        undo.shown,
        [
            (Renamed::Label, "m86".to_owned()),
            (Renamed::Nickname, "Marigold".to_owned())
        ]
    );
    assert_eq!(
        undo.cells,
        [
            (Renamed::Label, "m8".to_owned()),
            (Renamed::Nickname, "Heron".to_owned())
        ]
    );
    assert_eq!(
        undo.to,
        UndoTo::Name(NameTo {
            num: num("m8"),
            nick: Some(nick("Heron")),
        })
    );
}

/// An undo is a rename like any other: the newest owner write is the undo, so undoing it again
/// gives back what the undo replaced, the rename. A nickname rename goes back alone.
/// NEGATIVE CONTROL: with `undo_of` refusing an owner write that wrote what an earlier one
/// replaced (an undo, as the file's History did), the second undo is refused.
#[test]
fn an_undo_of_an_undo_redoes_the_rename() {
    let rename = write(
        1,
        EntryKind::Explicit,
        vec![cell(Renamed::Label, Some("m8"), false, &[Some("m7")])],
    );
    let undo = write(
        2,
        EntryKind::Explicit,
        vec![cell(Renamed::Label, Some("m7"), true, &[Some("m8")])],
    );
    let redo = undo_of(&history(vec![undo, rename])).unwrap();
    assert_eq!(redo.cells, [(Renamed::Label, "m8".to_owned())]);
    assert_eq!(
        redo.to,
        UndoTo::Name(NameTo {
            num: num("m8"),
            nick: None,
        })
    );

    let nickname = write(
        3,
        EntryKind::Explicit,
        vec![cell(
            Renamed::Nickname,
            Some("Otter"),
            true,
            &[Some("Heron")],
        )],
    );
    assert_eq!(
        undo_of(&history(vec![nickname])).unwrap().to,
        UndoTo::Nickname(nick("Heron"))
    );
}

/// Each History Undo cannot take back is refused by its cause, in words that say why, and a
/// nickname the rename gave where the row had none is left out of an undo that gives back its
/// number.
/// NEGATIVE CONTROL: with `replaced` taking any label `Number::parse` reads (its `to_string`
/// filter removed), the label `8` is given back as `m8`: the `bare digits` case is `Ok`.
#[test]
fn each_undo_refusal_names_its_cause() {
    let label = |value: &str, stands: bool, replaced: &[Option<&str>]| {
        cell(Renamed::Label, Some(value), stands, replaced)
    };
    let owner = |op: u8, cells: Vec<NameCell>| write(op, EntryKind::Explicit, cells);
    let auto = |op: u8, cells: Vec<NameCell>| write(op, EntryKind::Aterm, cells);
    let mut carried = renamed();
    carried.carried = true;
    let mut pending = history(vec![renamed()]);
    pending.pending = vec![Renamed::Label];
    let cases: Vec<(&str, History, Result<(), Cause>, &str)> = vec![
        ("no row", History::default(), Err(Cause::Nothing), NO_ROW),
        (
            "pending",
            pending,
            Err(Cause::Superseded),
            "still on its way",
        ),
        (
            "only automatic",
            history(vec![auto(1, vec![label("m8", true, &[])])]),
            Err(Cause::Nothing),
            "holds no rename",
        ),
        (
            "carried",
            history(vec![carried]),
            Err(Cause::Nothing),
            "predates the lab registry's roll",
        ),
        (
            "superseded",
            history(vec![
                auto(3, vec![label("m9", true, &[Some("m86")])]),
                owner(2, vec![label("m86", false, &[Some("m8")])]),
            ]),
            Err(Cause::Superseded),
            "changed in its lab registry row after the last rename",
        ),
        (
            "two at once",
            history(vec![
                auto(3, vec![label("m87", true, &[Some("m8")])]),
                owner(2, vec![label("m86", true, &[Some("m8")])]),
            ]),
            Err(Cause::Superseded),
            "holds two labels at once (m87 and m86)",
        ),
        (
            "settled two",
            history(vec![owner(
                2,
                vec![label("m86", true, &[Some("m7"), Some("m9")])],
            )]),
            Err(Cause::Nothing),
            "over m7 and m9 written at once",
        ),
        (
            "no number",
            history(vec![owner(
                2,
                vec![label("m86", true, &[Some("Mac-mini-M4")])],
            )]),
            Err(Cause::Nothing),
            "`aterm file revert 02020202` restores the row as it was",
        ),
        (
            "bare digits",
            history(vec![owner(2, vec![label("m86", true, &[Some("8")])])]),
            Err(Cause::Nothing),
            "replaced, 8, is no number",
        ),
        (
            "first number",
            history(vec![owner(2, vec![label("m86", true, &[])])]),
            Err(Cause::Unnumbered),
            "leaves no Mac unnumbered",
        ),
        (
            "unset number",
            history(vec![owner(2, vec![label("m86", true, &[None])])]),
            Err(Cause::Unnumbered),
            "leaves no Mac unnumbered",
        ),
        (
            "empty nickname",
            history(vec![owner(
                2,
                vec![
                    label("m86", true, &[Some("m8")]),
                    cell(Renamed::Nickname, Some("Marigold"), true, &[Some("")]),
                ],
            )]),
            Ok(()),
            "",
        ),
        (
            "kept both",
            history(vec![owner(
                2,
                vec![cell(
                    Renamed::Nickname,
                    Some("Heron"),
                    true,
                    &[Some("Heron")],
                )],
            )]),
            Err(Cause::Nothing),
            "left this Mac's number and nickname as they were",
        ),
    ];
    for (case, history, cause, words) in cases {
        let undone = undo_of(&history);
        match (&undone, cause) {
            (Ok(undo), Ok(())) => assert_eq!(
                undo.cells,
                [(Renamed::Label, "m8".to_owned())],
                "{case}: the empty nickname is left out"
            ),
            (Err(refused), Err(cause)) => {
                assert_eq!(refused.cause, cause, "{case}: {}", refused.why);
                assert!(refused.why.contains(words), "{case}: {}", refused.why);
            }
            _ => panic!("{case}: {undone:?}"),
        }
    }
}

/// A History line says when (UTC), which Mac, the kind and each cell's change, newest first,
/// with a pending cell leading and the row's absence or emptiness said in one line; the JSON
/// holds every fact the line does, under the keys `aterm help name` lists (an object's keys
/// print sorted).
#[test]
fn history_lines_say_each_write_newest_first() {
    let mut undo = write(
        9,
        EntryKind::Explicit,
        vec![cell(Renamed::Label, Some("m8"), true, &[Some("m86")])],
    );
    undo.reverts = Some([2; 32]);
    undo.cells[0].unseen = vec![[7; 32]];
    let mut rename = renamed();
    rename.cells[0].stands = false;
    let mut read = history(vec![undo, rename]);
    read.pending = vec![Renamed::Nickname];
    assert_eq!(
        read.lines(None),
        [
            "pending  a write of this Mac's nickname is on its way to the row",
            "1970-01-01T00:00:09Z  m8  explicit revert of 02020202  label m86 -> m8 \
             (concurrent with 07070707)",
            "1970-01-01T00:00:02Z  m8  explicit  label m8 -> m86 (replaced), nickname Heron -> \
             Marigold",
        ]
    );
    assert_eq!(
        read.lines(Some(1)).len(),
        2,
        "the pending line and one write"
    );
    assert_eq!(History::default().lines(None), [NO_ROW]);
    assert_eq!(history(Vec::new()).lines(None), [NO_HISTORY]);

    let json = read.json();
    let keys = |value: &aterm_json::Value| -> Vec<String> {
        value.as_object().unwrap().keys().cloned().collect()
    };
    assert_eq!(keys(&json), ["pending", "row", "writes"]);
    assert_eq!(json["row"], aterm_json::json!(ROW));
    assert_eq!(json["pending"], aterm_json::json!(["nickname"]));
    let newest = &json["writes"][0];
    assert_eq!(
        keys(newest),
        [
            "at", "at_ms", "carried", "cells", "kind", "mac", "op", "reverts"
        ]
    );
    assert_eq!(newest["at"], aterm_json::json!("1970-01-01T00:00:09Z"));
    assert_eq!(newest["kind"], aterm_json::json!("explicit"));
    assert_eq!(
        newest["reverts"],
        aterm_json::json!(aterm_codec::hex::encode_lower(&[2; 32]))
    );
    assert_eq!(
        keys(&newest["cells"][0]),
        ["column", "replaced", "stands", "unseen", "value"]
    );
    assert_eq!(newest["cells"][0]["replaced"], aterm_json::json!(["m86"]));
}

/// A name kept anywhere but this account's live state has no registry row, so History reads
/// none and opens no store: a test's scratch folder, under a test binary's private state root.
/// A value a revert restored as a register of several candidates says each, ` / ` between them,
/// never its stored JSON, and an undo of a rename that replaced it is refused: no one number
/// stood there. A plain typed value says its string, and a cleared one nothing.
/// NEGATIVE CONTROL: with `stored` reading a register as one value (the stored JSON, as it did
/// before), the register says `{"k":[...]}`, not `m7 / m9`.
#[test]
fn a_restored_register_says_each_value() {
    use aterm_db::typed::cell::{CellValue, carry};
    let owner = |x: &str| CellValue::owner(1, aterm_json::Value::String(x.to_owned())).encode();
    let register = carry(&std::collections::BTreeMap::from([
        ([1; 32], owner("m7")),
        ([2; 32], owner("m9")),
    ]))
    .expect("a register");
    assert_eq!(super::stored(&register).as_deref(), Some("m7 / m9"));
    assert_eq!(super::stored(&owner("m7")).as_deref(), Some("m7"));
    assert_eq!(
        super::stored(&CellValue::owner(1, aterm_json::Value::Null).encode()),
        None
    );
    let refused = undo_of(&history(vec![write(
        2,
        EntryKind::Explicit,
        vec![cell(Renamed::Label, Some("m86"), true, &[Some("m7 / m9")])],
    )]))
    .expect_err("no one number was replaced");
    assert_eq!(refused.cause, Cause::Nothing, "{}", refused.why);
}

/// NEGATIVE CONTROL: with `registry::live`'s gate removed, `read` opens the vault home
/// `Paths::live` names, which `HOME` gives and a test's scratch root does not cover, and answers
/// what that home's registry holds. Run it only with `HOME` a stand-in home holding a copy of a
/// home's published `lab/machines` (`.aterm/files/lab/machines.tsv` and
/// `.aterm/files/.ubd/state.noindex/lab/machines.{snap,state.json,frontier}`), never the real
/// one: `read` finds the copy's `here` row and goes on to that home's store, which is not this
/// test's nothing (with no store opened there: `Err("this Mac has opened no Commons custody
/// yet: …")`).
#[test]
fn history_reads_nothing_under_a_private_state_root() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let read = read(
        scratch.path(),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    assert_eq!(read, Ok(History::default()));
}
