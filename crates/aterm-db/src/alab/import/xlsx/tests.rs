// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An xlsx tab read in memory, over synthetic workbooks built here part by part (the layout Excel
//! and Numbers write: `xl/workbook.xml`, its relationships, shared strings, worksheets), every
//! address `example.invalid` and every password `synthetic-pass-<n>`.

use super::*;
use crate::alab::import::{Sheet, SheetKind};
use aterm_codec::crc32::Crc32;
use aterm_digest::Sha256;

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// `data` as DEFLATE stored blocks, so the reader's inflate path runs.
fn deflated(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks = data.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        return vec![0x01, 0, 0, 0xFF, 0xFF];
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let n = chunk.len() as u16;
        le16(&mut out, n);
        le16(&mut out, !n);
        out.extend_from_slice(chunk);
    }
    out
}

/// A ZIP archive of `parts`, every other one deflated.
fn zip(parts: &[(&str, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (n, (name, text)) in parts.iter().enumerate() {
        let deflate = n % 2 == 1;
        let body = if deflate {
            deflated(text.as_bytes())
        } else {
            text.as_bytes().to_vec()
        };
        let offset = out.len() as u32;
        let crc = Crc32::of(text.as_bytes());
        let method: u16 = if deflate { 8 } else { 0 };
        le32(&mut out, 0x0403_4b50);
        le16(&mut out, 20);
        le16(&mut out, 0);
        le16(&mut out, method);
        le32(&mut out, 0);
        le32(&mut out, crc);
        le32(&mut out, body.len() as u32);
        le32(&mut out, text.len() as u32);
        le16(&mut out, name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, 20);
        le16(&mut central, 20);
        le16(&mut central, 0);
        le16(&mut central, method);
        le32(&mut central, 0);
        le32(&mut central, crc);
        le32(&mut central, body.len() as u32);
        le32(&mut central, text.len() as u32);
        le16(&mut central, name.len() as u16);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, 0);
        le32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, parts.len() as u16);
    le16(&mut out, parts.len() as u16);
    le32(&mut out, central.len() as u32);
    le32(&mut out, cd_offset);
    le16(&mut out, 0);
    out
}

const SHARED: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="9" uniqueCount="9">
<si><t>Email Address</t></si>
<si><t>Root Account</t></si>
<si><t>Password</t></si>
<si><t>Status</t></si>
<si><t>Notes</t></si>
<si><t>ann@example.invalid</t></si>
<si><t>synthetic-pass-01</t></si>
<si><r><rPr><b/></rPr><t xml:space="preserve">key fob </t></r><r><t>in the drawer</t></r><rPh sb="0" eb="1"><t>phonetic</t></rPh></si>
<si><t>Tom &amp; Jerry&#x27;s</t></si>
</sst>"#;

/// The Emails tab: a header, ann's row with a password, a styled empty cell and a rich note, and
/// bob's row with an inline string, a number and a gap where Root Account is.
fn emails_sheet(password_index: usize) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:E3"/><sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>2</v></c><c r="D1" t="s"><v>3</v></c><c r="E1" t="s"><v>4</v></c></row>
<row r="2"><c r="A2" t="s"><v>5</v></c><c r="B2" s="3"/><c r="C2" t="s"><v>{password_index}</v></c><c r="D2" t="inlineStr"><is><t>active</t></is></c><c r="E2" t="s"><v>7</v></c></row>
<row r="4"><c r="A4" t="inlineStr"><is><t>bob@example.invalid</t></is></c><c r="C4"><v>1</v></c><c r="D4" t="b"><v>1</v></c><c r="E4" t="s"><v>8</v></c><c r="Z4" s="2"/></row>
</sheetData></worksheet>"#
    )
}

fn workbook(emails: String) -> Vec<u8> {
    zip(&[
        (
            "[Content_Types].xml",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>".to_owned(),
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Notes" sheetId="1" r:id="rId1"/><sheet name="Emails" sheetId="2" r:id="rId2"/></sheets></workbook>"#.to_owned(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="worksheet" Target="/xl/worksheets/../worksheets/sheet2.xml"/><Relationship Id="rId9" Type="sharedStrings" Target="sharedStrings.xml"/></Relationships>"#.to_owned(),
        ),
        ("xl/sharedStrings.xml", SHARED.to_owned()),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>not this tab</t></is></c></row></sheetData></worksheet>"#.to_owned(),
        ),
        ("xl/worksheets/sheet2.xml", emails),
    ])
}

/// The named tab's records, cell by cell: shared strings (rich runs joined, phonetic runs
/// skipped, entities read), inline strings, a number as stored, a boolean as `1`, a gap kept
/// at its column, an empty row kept at its place, and styled empty cells costing no column.
/// The tab is found by name in any case, through a relationship target with `..`.
/// NEGATIVE CONTROL: with `cells` ignoring each cell's `r` reference (every cell at the next
/// column), bob's password flag lands under Root Account: `` left: ["bob@example.invalid", "1",
/// … ``.
#[test]
fn a_named_tab_reads_cell_by_cell() {
    let bytes = workbook(emails_sheet(6));
    let records = tab(&bytes, "emails").unwrap();
    assert_eq!(
        records,
        vec![
            vec![
                "Email Address",
                "Root Account",
                "Password",
                "Status",
                "Notes"
            ],
            vec![
                "ann@example.invalid",
                "",
                "synthetic-pass-01",
                "active",
                "key fob in the drawer"
            ],
            vec!["", "", "", "", ""],
            vec!["bob@example.invalid", "", "1", "1", "Tom & Jerry's"],
        ]
    );
    assert_eq!(tab(&bytes, "Notes").unwrap(), vec![vec!["not this tab"]]);
}

/// Read as a CSV is: its Password column becomes a has-password flag, so the real password is
/// no sheet cell, and the source is named by the flagged records, not the workbook's bytes, so a
/// changed password names the same source and no SHA-256 over a password reaches it.
/// NEGATIVE CONTROL: with `Sheet::xlsx` naming its source by the workbook's bytes, the two
/// workbooks' sources differ: `` assertion `left == right` failed ``.
#[test]
fn a_tab_reads_as_a_csv_and_its_source_holds_no_password() {
    let one = workbook(emails_sheet(6));
    let sheet = Sheet::xlsx(
        SheetKind::Emails,
        "/somewhere/AI Email Accounting.xlsx",
        "Emails",
        &one,
    )
    .unwrap();
    assert_eq!(sheet.name, "AI Email Accounting.xlsx#Emails");
    assert_eq!(sheet.rows[0][2], "1", "the password is a flag");
    assert_eq!(sheet.rows[2][2], "1");
    assert!(
        !sheet
            .rows
            .iter()
            .flatten()
            .any(|cell| cell.contains("synthetic")),
        "no password is a cell"
    );
    // ann's password changed: same flags, same source.
    let two = workbook(emails_sheet(8));
    let again = Sheet::xlsx(
        SheetKind::Emails,
        "AI Email Accounting.xlsx",
        "Emails",
        &two,
    )
    .unwrap();
    assert_eq!(sheet.source, again.source);
    assert_eq!(sheet.sha256, again.sha256);
    assert_ne!(
        sheet.sha256,
        aterm_codec::hex::encode_lower(&Sha256::digest(&one))
    );
}

/// A missing tab names the tabs the workbook holds and no cell; a relationship leading out of
/// the archive, a tab wider than a CSV may be, and bytes that are no ZIP archive are refused.
/// NEGATIVE CONTROL: with `target` keeping `..` past the root, the escaping target reads as a
/// part name and the refusal is `lacks the tab's part`: `` assertion failed: err.contains("outside") ``.
#[test]
fn a_workbook_beyond_what_is_read_is_refused_by_rule() {
    let bytes = workbook(emails_sheet(6));
    let err = tab(&bytes, "Phones").unwrap_err().to_string();
    assert!(
        err.contains("\"Notes\"") && err.contains("\"Emails\""),
        "{err}"
    );
    assert!(
        !err.contains("synthetic") && !err.contains("example.invalid"),
        "{err}"
    );

    let escaping = zip(&[
        (
            "xl/workbook.xml",
            r#"<workbook><sheets><sheet name="Emails" r:id="rId1"/></sheets></workbook>"#.to_owned(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships><Relationship Id="rId1" Target="../../../etc/sheet.xml"/></Relationships>"#.to_owned(),
        ),
    ]);
    let err = tab(&escaping, "Emails").unwrap_err().to_string();
    assert!(err.contains("outside"), "{err}");

    let wide: String = (0..70)
        .map(|n| {
            let letter = |k: usize| char::from(b'A' + k as u8);
            let col = if n < 26 {
                letter(n).to_string()
            } else {
                format!("{}{}", letter(n / 26 - 1), letter(n % 26))
            };
            format!(r#"<c r="{col}1" t="inlineStr"><is><t>h{n}</t></is></c>"#)
        })
        .collect();
    let wide = workbook(format!(
        "<worksheet><sheetData><row r=\"1\">{wide}</row></sheetData></worksheet>"
    ));
    let err = tab(&wide, "Emails").unwrap_err().to_string();
    assert!(err.contains("capacity"), "{err}");

    assert!(tab(b"Email Address,Password\n", "Emails").is_err());
}

#[test]
fn column_references_read_as_excel_numbers_them() {
    assert_eq!(column_of("A1"), Some(0));
    assert_eq!(column_of("Z9"), Some(25));
    assert_eq!(column_of("AA10"), Some(26));
    assert_eq!(column_of("xfd1"), Some(16_383));
    assert_eq!(column_of("XFE1"), None);
    assert_eq!(column_of("1"), None);
}
