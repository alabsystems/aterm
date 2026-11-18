// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A staged label another row claims, said before Write, on a scratch registry
//! (`aterm_db::table::fixture::Lab`: an isolated home, no live vault, fleet or Commons): what
//! the cell, the cell bar, the history drawer's line and the pane say, the label the row reads
//! as once written, and when nothing is said. Every value is synthetic.

use super::super::native_tables_tests::{
    act, cell_says, key_in, painted_words, put_in, render, said, settle, settle_write,
    shipped_themes, style_of, tree, two_macs, view_of,
};
use super::super::rows::ADD;
use super::super::{EDIT, WRITE, Work, Wrote};
use super::*;
use crate::App;
use crate::native_app::{SemanticInput, TextInputEvent};
use aterm_db::alab::Registry;
use aterm_db::alab::registry::MACHINE;
use aterm_db::att::{Handle, REGISTRY};
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_json::json;

/// The machine that holds `m22`.
const H: &str = "mc-00000000000000a1";
/// The machine labelled `m30`, given `m22`.
const S: &str = "mc-00000000000000b2";
/// Two machines given one label nobody holds.
const C: &str = "mc-00000000000000c3";
const D: &str = "mc-00000000000000d4";

/// Machines written on the lab's first Mac beside the fixture's own (`m9`), each live under
/// its label in a record of its own, and carried to the second.
fn labelled(lab: &Lab, rows: &[(&str, &str)]) {
    Registry::at(lab.paths[0].clone())
        .write(
            rows.iter()
                .map(|(pk, label)| {
                    vec![
                        put_in(&MACHINE, pk, "label", json!(label)),
                        put_in(&MACHINE, pk, "status", json!("active")),
                        put_in(&MACHINE, pk, "row", json!("live")),
                    ]
                })
                .collect(),
        )
        .unwrap();
    repair_commons(lab, 4);
}

/// How many operations the registry holds on the lab's first Mac.
fn operations(lab: &Lab) -> usize {
    Handle::at(lab.paths[0].clone(), &REGISTRY)
        .history()
        .unwrap()
        .len()
}

/// What row `pk`'s label reads as on Mac `mac`, as the registry reads it.
fn reads(lab: &Lab, mac: usize, pk: &str) -> Option<String> {
    Handle::at(lab.paths[mac].clone(), &REGISTRY)
        .view()
        .unwrap()
        .project(&MACHINE)
        .rows
        .get(pk)
        .and_then(|row| row.get("label"))
        .and_then(aterm_json::Value::as_str)
        .map(str::to_owned)
}

/// The page on the lab's first Mac, at Machines.
fn page(lab: &Lab) -> App {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    app
}

/// Type `text` into row `pk`'s `column`: a press selects it, a second opens it (one opens the
/// cell already selected), Return stages it.
fn type_in(app: &mut App, pk: &str, column: &str, text: &str) {
    let cell = format!("tables/cell/{pk}/{column}");
    act(app, &cell, None);
    if said(&render(app), EDIT).is_none() {
        act(app, &cell, None);
    }
    assert!(said(&render(app), EDIT).is_some(), "{pk} {column} opened");
    act(app, EDIT, Some(SemanticInput::Text(text.into())));
    key_in(app, TextInputEvent::Submit);
}

/// What the page says of the label staged in row `pk`, where it says another row claims it.
fn claimed(app: &mut App, pk: &str) -> Option<Claimed> {
    let tables = app.native_runtime.tables_mut().expect("the Tables page");
    tables
        .claimed
        .get(&("machine".to_owned(), pk.to_owned(), "label".to_owned()))
        .cloned()
}

/// The page reads the registry again.
fn read_again(app: &mut App) {
    let tables = app.native_runtime.tables_mut().expect("the Tables page");
    tables.work.send(Work::Read).unwrap();
    tables.settle();
}

/// A label another machine reads as, staged in a row, is said before Write and never after: the
/// cell wears `in use` in the note's ink, and the cell, the cell bar, the history drawer's
/// `NOT WRITTEN` line each say, after their visible words, that m22 already names that machine
/// (its words and its key's ending) and the clash label this row reads as once written; the
/// pane offers that machine, a press away. Written, the row reads as exactly that clash label,
/// and the note is gone.
/// NEGATIVE CONTROL: with `claims::said` asking the settlement what the rows read as now
/// (`settlement.reads()`) in place of once written (`settlement.written(&staged)`), the row is
/// said to read as the label it holds today: `` m30 with Reads("m30") `` at the clash label's
/// `starts_with("m22-")`.
#[test]
fn a_label_another_row_claims_is_said_before_write_with_the_label_the_row_will_read_as() {
    let lab = two_macs();
    labelled(&lab, &[(H, "m22"), (S, "m30")]);
    let mut app = page(&lab);
    type_in(&mut app, S, "label", "m22");

    let said_of = claimed(&mut app, S).expect("the label is said");
    assert_eq!(said_of.other, H);
    assert_eq!(said_of.by, By::Names);
    let After::Reads(clash) = said_of.this.clone() else {
        panic!("{said_of:?}")
    };
    assert!(clash.starts_with("m22-"), "{clash} with {:?}", said_of.this);
    let words =
        format!("m22 already names m22 \u{b7} \u{2026}00a1; written, this row reads as {clash}");
    assert_eq!(said_of.words, words);

    // The cell: its words, then the note's.
    let ui = render(&app);
    let cell = format!("tables/cell/{S}/label");
    assert_eq!(
        cell_says(&ui, S, "label"),
        format!("m22, not written, {IN_USE}: {words}")
    );
    assert_eq!(
        painted_words(&ui, &format!("{cell}/tag")).as_deref(),
        Some(IN_USE)
    );
    let drawn = tree(&app);
    assert_eq!(
        style_of(&drawn, &format!("{cell}/tag")),
        Some(StyleRef::Note),
        "a claim is no refusal: the note's ink, never the danger ink"
    );
    // The cell bar: the grid's word, the note, what the cell holds.
    assert_eq!(
        said(&ui, "tables/cellbar/state"),
        Some(format!(
            "{IN_USE} \u{b7} {words} \u{b7} not written \u{b7} holds m30"
        ))
    );
    // The pane: the other machine, a press away.
    let to = "tables/pane/row/claim/label";
    assert_eq!(
        said(&ui, to).as_deref(),
        Some("In use: m22 \u{2192} m22 \u{b7} \u{2026}00a1, show it in Machines")
    );

    // The drawer's line.
    act(&mut app, "tables/history/show", None);
    let ui = render(&app);
    let line = format!("tables/history/staged/{S}/label");
    assert_eq!(
        said(&ui, &line),
        Some(format!(
            "m30 \u{b7} Label: m22, not written, {IN_USE}: {words}"
        ))
    );
    assert_eq!(
        painted_words(&ui, &format!("{line}/claimed")).as_deref(),
        Some(IN_USE)
    );
    assert_eq!(
        style_of(&tree(&app), &format!("{line}/claimed")),
        Some(StyleRef::Note)
    );

    act(&mut app, to, None);
    assert_eq!(view_of(&app).row.as_deref(), Some(H), "the press shows it");

    // Written, the row reads as the clash label said, and the note is gone.
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        reads(&lab, 0, S),
        Some(clash.clone()),
        "as said before Write"
    );
    assert_eq!(reads(&lab, 0, H).as_deref(), Some("m22"));
    assert_eq!(claimed(&mut app, S), None);
    let ui = render(&app);
    assert_eq!(cell_says(&ui, S, "label"), clash);
    assert_eq!(painted_words(&ui, &format!("{cell}/tag")), None);
}

/// Two machines given one label nobody holds are each said: the one the read's tie-break keeps
/// it for says the other reads as its clash label, the other that it does; written, they read
/// so.
/// NEGATIVE CONTROL: with `claims::said` saying nothing where this row keeps the label (a
/// `continue` first in that arm), the row that keeps it is not said: `` the first row is said ``.
#[test]
fn two_rows_given_one_label_are_each_said() {
    let lab = two_macs();
    labelled(&lab, &[(C, "m40"), (D, "m41")]);
    let mut app = page(&lab);
    type_in(&mut app, C, "label", "m50");
    type_in(&mut app, D, "label", "m50");

    let lost = claimed(&mut app, D).expect("the second row is said");
    let After::Reads(clash) = lost.this.clone() else {
        panic!("{lost:?}")
    };
    assert!(clash.starts_with("m50-"), "{clash}");
    let kept = claimed(&mut app, C).expect("the first row is said");
    assert_eq!((kept.by, lost.by), (By::Staged, By::Staged));
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, C, "label"),
        format!(
            "m50, not written, {IN_USE}: m50 is staged for m41 \u{b7} \u{2026}00d4 too; written, \
             this row keeps it and that row reads as {clash}"
        )
    );
    assert_eq!(
        cell_says(&ui, D, "label"),
        format!(
            "m50, not written, {IN_USE}: m50 is staged for m40 \u{b7} \u{2026}00c3 too; written, \
             this row reads as {clash}"
        )
    );

    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(reads(&lab, 0, C).as_deref(), Some("m50"));
    assert_eq!(reads(&lab, 0, D), Some(clash));
}

/// The note goes as soon as the label changes, and once the other row lets the label go: here
/// another Mac gives it another label, and the page reads that.
/// NEGATIVE CONTROL: with `TablesApp::recheck` working the claims out again only where what is
/// staged moved, not where the read did, the read the holder let go in leaves the note: ``
/// left: "m22, not written, in use: m22 already names m22 · …00a1; written, this row reads as
/// m22-df77"  right: "m22, not written" ``.
#[test]
fn the_note_goes_when_the_label_changes_or_the_other_row_lets_it_go() {
    let lab = two_macs();
    labelled(&lab, &[(H, "m22"), (S, "m30")]);
    let mut app = page(&lab);
    type_in(&mut app, S, "label", "m22");
    assert!(claimed(&mut app, S).is_some());

    type_in(&mut app, S, "label", "m31");
    assert_eq!(
        cell_says(&render(&app), S, "label"),
        "m31, not written",
        "a label nobody holds says nothing"
    );
    assert_eq!(claimed(&mut app, S), None);

    type_in(&mut app, S, "label", "m22");
    assert!(claimed(&mut app, S).is_some());
    // The other Mac moves the holder off the label; read again here, nothing claims it.
    Registry::at(lab.paths[1].clone())
        .write(vec![vec![put_in(&MACHINE, H, "label", json!("m23"))]])
        .unwrap();
    repair_commons(&lab, 4);
    assert_eq!(reads(&lab, 0, H).as_deref(), Some("m23"));
    read_again(&mut app);
    assert_eq!(cell_says(&render(&app), S, "label"), "m22, not written");
    assert_eq!(claimed(&mut app, S), None);
}

/// Nothing is said of a label no other row reads as (the row keeps the label it is given), of
/// one whose holder the same Write retires (the holder lets it go), of a value staged in a
/// column that claims nothing (another machine's location), or in a table that claims no labels.
/// NEGATIVE CONTROL: with `claims::said` taking any row that reads as the label now for the one
/// this row takes it from, retired by the Write or not (its `is_some_and` on the row's reading
/// once written made `is_none_or`), the retired holder is said to show no label: `` left: "m22,
/// not written, in use: m22 already names m22 · …00a1; written, this row keeps it and that row
/// shows no label until its claim settles"  right: "m22, not written" ``.
#[test]
fn a_label_nobody_else_keeps_and_a_column_that_claims_none_say_nothing() {
    let lab = two_macs();
    labelled(&lab, &[(H, "m22"), (S, "m30")]);
    let mut app = page(&lab);
    type_in(&mut app, S, "label", "m31");
    type_in(&mut app, S, "location", "rack a");
    let ui = render(&app);
    assert_eq!(cell_says(&ui, S, "label"), "m31, not written");
    assert_eq!(cell_says(&ui, S, "location"), "rack a, not written");
    assert_eq!(
        said(&ui, "tables/cellbar/state").as_deref(),
        Some("not written \u{b7} holds empty")
    );

    // The holder retired in the same Write lets the label go.
    act(&mut app, &format!("tables/cell/{H}/label"), None);
    act(&mut app, "tables/pane/row/retire", None);
    type_in(&mut app, S, "label", "m22");
    assert_eq!(cell_says(&render(&app), S, "label"), "m22, not written");
    let tables = app.native_runtime.tables_mut().expect("the Tables page");
    assert!(tables.claimed.is_empty(), "{:?}", tables.claimed);
    // A table that claims no labels keeps no settlement to ask.
    let snapshot = tables.snapshot.as_ref().unwrap();
    for sheet in snapshot.sheets.iter().filter(|s| s.name != "machine") {
        if let Some(schema) = &sheet.schema {
            assert!(schema.settled.is_empty(), "{}", sheet.name);
        }
    }
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        reads(&lab, 0, S).as_deref(),
        Some("m22"),
        "it took the label"
    );
}

/// A label the same Write takes off the row that reads as it now settles no clash, so nothing
/// is said of it: the holder given another label (a rename), or the label it gives up (a swap).
/// Written, each row reads as the label staged in it.
/// NEGATIVE CONTROL: with `claims::said` taking for the row this one takes the label from any row
/// that reads as it now and claims some label once written (`before.is_some()` for
/// `Some(r.claims.as_str()) == before`), the rename says S takes it from the holder: `` left: "m22,
/// not written, in use: m22 already names m22 · …00a1; written, this row keeps it and that row
/// reads as m23"  right: "m22, not written" ``.
#[test]
fn a_label_the_same_write_takes_off_its_row_says_nothing() {
    let lab = two_macs();
    labelled(&lab, &[(H, "m22"), (S, "m30")]);
    let mut app = page(&lab);
    // A rename: the holder moves to m23, and S takes m22.
    type_in(&mut app, H, "label", "m23");
    type_in(&mut app, S, "label", "m22");
    let ui = render(&app);
    assert_eq!(cell_says(&ui, S, "label"), "m22, not written");
    assert_eq!(cell_says(&ui, H, "label"), "m23, not written");
    assert_eq!(claimed(&mut app, S), None);
    // A swap: the holder takes S's m30.
    type_in(&mut app, H, "label", "m30");
    let ui = render(&app);
    assert_eq!(cell_says(&ui, S, "label"), "m22, not written");
    assert_eq!(cell_says(&ui, H, "label"), "m30, not written");
    let tables = app.native_runtime.tables_mut().expect("the Tables page");
    assert!(tables.claimed.is_empty(), "{:?}", tables.claimed);

    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(reads(&lab, 0, S).as_deref(), Some("m22"));
    assert_eq!(reads(&lab, 0, H).as_deref(), Some("m30"));
}

/// A Write the save appends as more than one record (a record holds at most 64 changes) is said
/// by the order of its records: of two rows given one label nobody holds, the row an earlier
/// record carries keeps it, though the new row the last record carries (a new row's cells come
/// after every row the grid shows) has the smaller key. Written, they read so.
/// NEGATIVE CONTROL: with `claims::said` putting every row of the Write in its first record
/// (`Some(_) => 0` for `Some(record) => *record`), the new row is said to keep the label: ``
/// Claimed { label: "m50", other: "mc-ffffffffffffffff", named: "m40 · …ffff", by: Staged, this:
/// Keeps, that: Some(Reads("m50-5c32")), … } ``.
#[test]
fn a_write_of_several_records_is_said_by_their_order() {
    /// The row given the label in the Write's first record: the largest key a machine has.
    const E: &str = "mc-ffffffffffffffff";
    let lab = two_macs();
    let others: Vec<(String, String)> = (0..31)
        .map(|n| (format!("mc-{:016x}", 0x100 + n), format!("m{}", 100 + n)))
        .collect();
    let mut rows: Vec<(&str, &str)> = others
        .iter()
        .map(|(pk, label)| (pk.as_str(), label.as_str()))
        .collect();
    rows.push((E, "m40"));
    labelled(&lab, &rows);
    let mut app = page(&lab);
    // 62 cells in the rows before E's, then E's label: 63 changes, one short of a record.
    let tables = app.native_runtime.tables_mut().expect("the Tables page");
    for (pk, _) in &others {
        tables.stage("machine", pk, "location", "north rack".to_owned());
        tables.stage("machine", pk, "nickname", "spare box".to_owned());
    }
    assert_eq!(tables.staged.len(), 62);
    type_in(&mut app, E, "label", "m50");
    // A new row given the same label, whose changes go in a second record.
    act(&mut app, ADD, None);
    let new = format!("{}1", super::super::rows::NEW);
    type_in(&mut app, &new, "label", "m50");
    type_in(&mut app, &new, "status", "active");
    let born = aterm_db::typed::keys::machine_pk("m50");
    assert!(born.as_str() < E, "{born}: the new row has the smaller key");

    let kept = claimed(&mut app, E).expect("the earlier record's row is said");
    let lost = claimed(&mut app, &new).expect("the new row is said");
    let After::Reads(clash) = lost.this.clone() else {
        panic!("{lost:?}")
    };
    assert!(clash.starts_with("m50-"), "{clash}");
    assert_eq!(
        kept.words,
        format!(
            "m50 is staged for a new machine too; written, this row keeps it and that row \
             reads as {clash}"
        )
    );
    assert_eq!(
        lost.words,
        format!(
            "m50 is staged for m40 \u{b7} \u{2026}ffff too; written, this row reads as {clash}"
        )
    );

    let before = operations(&lab);
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(operations(&lab), before + 2, "the Write is two records");
    assert_eq!(
        reads(&lab, 0, E).as_deref(),
        Some("m50"),
        "as said before Write"
    );
    assert_eq!(reads(&lab, 0, &born), Some(clash), "as said before Write");
}

/// A cell a Write the save refused there says that, and nothing of its claim: its pane offers
/// no row it is in use by. Its value pasted on it again, which takes the refusal back and moves
/// neither the read nor the staged cells, it says its claim again. The reply a refused Write brings (its
/// table's cells all kept staged, the registry unmoved) stands here for one: no rule of the
/// machines table refuses a label at the save.
/// NEGATIVE CONTROL: with `claims::said` skipping a refused cell as it works the claims out
/// (`if app.refusal(&at).is_some() { continue; }`), the claim skipped while the Write's refusal
/// stood stays unsaid once the paste takes it back: `` left: "m22, not written"  right: "m22, not
/// written, in use: m22 already names m22 · …00a1; written, this row reads as m22-df77" ``.
#[test]
fn a_cell_a_write_refused_says_no_claim_until_pasted_again() {
    let lab = two_macs();
    labelled(&lab, &[(H, "m22"), (S, "m30")]);
    let mut app = page(&lab);
    type_in(&mut app, S, "label", "m22");
    let said_of = claimed(&mut app, S).expect("the label is said");
    let to = "tables/pane/row/claim/label";
    assert!(said(&render(&app), to).is_some());

    let tables = app.native_runtime.tables_mut().expect("the Tables page");
    tables.wrote(Wrote {
        refused: vec![(
            "machine".to_owned(),
            S.to_owned(),
            "label".to_owned(),
            "a synthetic rule".to_owned(),
        )],
        ..Wrote::default()
    });
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, S, "label"),
        "m22, not written, refused: a synthetic rule"
    );
    assert_eq!(said(&ui, to), None, "no row is offered");
    assert_eq!(
        said(&ui, "tables/cellbar/state").as_deref(),
        Some("refused \u{b7} not written \u{b7} holds m30")
    );

    // The cell is still the one selected.
    assert!(app.native_input_event(
        super::super::native_tables_tests::WID,
        &crate::input::InputEvent::Paste("m22".into(), crate::input::PasteFraming::AtDrain)
    ));
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, S, "label"),
        format!("m22, not written, {IN_USE}: {}", said_of.words)
    );
    assert!(said(&ui, to).is_some(), "the row is offered again");
}

/// The `in use` tag reads on every shipped theme: its note ink holds
/// [`crate::native_appearance::SHEET_WORD_FLOOR`] on the staged cell's wash, in the grid and in
/// the drawer's line, as the wash is solved for ([`crate::native_appearance::NativeRoles::unwritten_wash`]).
/// NEGATIVE CONTROL: with the wash mixed its most whatever the inks
/// (`mix_srgb(elevated, unwritten, UNWRITTEN_WASH)` for `unwritten_wash`), 28 themes fail on both
/// grounds: `` 56 themes fail: ["Nord: the grid's tag on the staged wash reads 4.1", "Nord: the
/// drawer's tag on its line's wash reads 4.1", … `` (`Solarized Dark` at 3.2).
#[test]
fn the_in_use_tag_reads_on_every_theme() {
    use crate::widget::DrawPrim;
    let lab = two_macs();
    labelled(&lab, &[(H, "m22"), (S, "m30")]);
    let mut app = page(&lab);
    type_in(&mut app, S, "label", "m22");
    act(&mut app, "tables/history/show", None);
    let ui = render(&app);
    let cell = format!("tables/cell/{S}/label");
    let line = format!("tables/history/staged/{S}/label");
    let mut under = Vec::new();
    let themes = shipped_themes();
    assert!(themes.len() > 30, "{} themes swept", themes.len());
    for (name, theme) in themes {
        let roles = crate::settings::Roles::from_theme(theme);
        let painted = ui.painted(roles);
        let prims = |key: &str| {
            painted
                .iter()
                .find(|(painted, _)| painted.as_str() == key)
                .map(|(_, prims)| prims.clone())
                .unwrap_or_else(|| panic!("{key} is painted"))
        };
        let ground = |key: &str| {
            prims(key)
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} fills its ground"))
        };
        let ink = |key: &str| {
            prims(key)
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} draws words"))
        };
        for (what, ink, ground) in [
            (
                "the grid's tag on the staged wash",
                ink(&format!("{cell}/tag")),
                ground(&format!("{cell}/staged")),
            ),
            (
                "the drawer's tag on its line's wash",
                ink(&format!("{line}/claimed")),
                ground(&format!("{line}/seat")),
            ),
        ] {
            let ratio = crate::native_appearance::contrast_ratio(ink, ground);
            if ratio < crate::native_appearance::SHEET_WORD_FLOOR {
                under.push(format!("{name}: {what} reads {ratio:.1}"));
            }
        }
    }
    assert!(under.is_empty(), "{} themes fail: {under:#?}", under.len());
}
