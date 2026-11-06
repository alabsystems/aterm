// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One named tab of an xlsx workbook as a sheet's records (docs/DESIGN-alab-one-mesh-2026-11-05.md
//! §4.7: "a CSV … or an xlsx's named tab"; §10: "read in place, as a CSV is"). The workbook is
//! read in memory, through the workspace's one ZIP reader (`aterm_codec::zip`), its DEFLATE
//! (`aterm_codec::inflate`) and its XML reader (`aterm_codec::xml`): no file is extracted and no
//! other program opens it.
//!
//! Four parts are read: `xl/workbook.xml` names each tab and its relationship id,
//! `xl/_rels/workbook.xml.rels` maps the id to the tab's worksheet part, `xl/sharedStrings.xml`
//! holds the text cells point at, and the worksheet's `sheetData` holds the rows. A cell's text
//! is its shared string, its inline string, or its stored value as written (a number, a
//! formula's cached result, `TRUE`/`FALSE` as `1`/`0`), placed by its `r` reference, so a
//! sparse row keeps its columns. The records then go through the same sheet reader a CSV's do
//! ([`super::Sheet::from_records`]), so a credential column is a has-password flag here too.
//!
//! **Bounded and value-free.** Every part is read whole only below [`MAX_PART`] uncompressed
//! bytes, and the tab below [`MAX_ROWS`] rows and [`MAX_COLS`] columns. An error names the part
//! and the rule, never a cell. The buffers that held the workbook's text are zeroed when
//! dropped: a tab may hold real passwords (m104's Emails tab does, in column C), which the sheet
//! reader turns into flags.

use crate::{
    Error, Result,
    import::{MAX_CELL as CSV_CELL, MAX_COLUMNS as CSV_COLUMNS, MAX_ROWS as CSV_ROWS},
};
use aterm_codec::{
    xml::{Event, Reader},
    zip::Archive,
};
use std::io::Cursor;
use zeroize::{Zeroize, Zeroizing};

/// The largest uncompressed part read.
pub const MAX_PART: u64 = 64 * 1024 * 1024;

/// The most rows a tab may hold.
pub const MAX_ROWS: usize = 100_000;

/// The most columns a tab may hold (Excel's own limit, `XFD`).
pub const MAX_COLS: usize = 16_384;

/// The most shared strings read.
const MAX_STRINGS: usize = 1_000_000;

/// The most non-empty cells read.
const MAX_CELLS: usize = 1_000_000;

/// A workbook-level error: the part and the rule, never a cell.
fn bad(what: &str) -> Error {
    Error::new(&format!("the workbook {what}"))
}

/// The part `name` of `archive`, uncompressed, as UTF-8 text; `None` where the workbook has no
/// such part.
fn part(archive: &mut Archive<Cursor<&[u8]>>, name: &str) -> Result<Option<Zeroizing<String>>> {
    let entry = archive
        .find(name.as_bytes())
        .map_err(|_| bad("is not a ZIP archive this build reads"))?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.encrypted() {
        return Err(bad("is encrypted"));
    }
    let bytes = archive
        .read(&entry, MAX_PART)
        .map_err(|_| bad(&format!("part {name} cannot be read within its bound")))?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Some(Zeroizing::new(text))),
        Err(err) => {
            let mut bytes = err.into_bytes();
            bytes.zeroize();
            Err(bad(&format!("part {name} is not UTF-8")))
        }
    }
}

/// The XML reader's next event, its error as the part's.
fn next<'a>(reader: &mut Reader<'a>, name: &str) -> Result<Option<Event<'a>>> {
    reader
        .next_event()
        .map_err(|_| bad(&format!("part {name} is not well-formed XML")))
}

/// A tag's local name: `x:row` is `row`.
fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

/// Each tab's name and relationship id, in workbook order.
fn tabs(workbook: &str) -> Result<Vec<(String, String)>> {
    let name = "xl/workbook.xml";
    let mut reader = Reader::new(workbook);
    let mut out = Vec::new();
    while let Some(event) = next(&mut reader, name)? {
        if let Event::Start(tag) = event
            && local(tag.name) == "sheet"
        {
            // The relationship id, under whatever prefix names the relationships namespace.
            let id = tag
                .attrs()
                .find(|(name, _)| {
                    name.rsplit_once(':')
                        .is_some_and(|(_, local)| local == "id")
                })
                .map(|(_, value)| value);
            let (Some(tab), Some(id)) = (tag.attr("name"), id) else {
                return Err(bad("names a tab without its name or relationship"));
            };
            out.push((tab.to_owned(), id.to_owned()));
        }
    }
    Ok(out)
}

/// The part a relationship id names in `xl/_rels/workbook.xml.rels`, as an archive path.
fn target(rels: &str, id: &str) -> Result<Option<String>> {
    let name = "xl/_rels/workbook.xml.rels";
    let mut reader = Reader::new(rels);
    while let Some(event) = next(&mut reader, name)? {
        if let Event::Start(tag) = event
            && local(tag.name) == "Relationship"
            && tag.attr("Id") == Some(id)
        {
            let Some(to) = tag.attr("Target") else {
                return Err(bad("names a relationship without its target"));
            };
            if tag.attr("TargetMode") == Some("External") {
                return Err(bad("points a tab outside itself"));
            }
            let path = match to.strip_prefix('/') {
                Some(absolute) => absolute.to_owned(),
                None => format!("xl/{to}"),
            };
            // `xl/worksheets/../worksheets/sheet1.xml` resolves inside the archive or not at all.
            let mut parts: Vec<&str> = Vec::new();
            for segment in path.split('/') {
                match segment {
                    "" | "." => {}
                    ".." => {
                        if parts.pop().is_none() {
                            return Err(bad("points a tab outside itself"));
                        }
                    }
                    other => parts.push(other),
                }
            }
            return Ok(Some(parts.join("/")));
        }
    }
    Ok(None)
}

/// The shared strings, each the concatenation of its `t` runs (phonetic runs, `rPh`, skipped).
fn shared(strings: &str) -> Result<Vec<String>> {
    let name = "xl/sharedStrings.xml";
    let mut reader = Reader::new(strings);
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut in_t = false;
    let mut phonetic = 0usize;
    while let Some(event) = next(&mut reader, name)? {
        match event {
            Event::Start(tag) => match local(tag.name) {
                "si" if !tag.empty => current = Some(String::new()),
                "si" => out.push(String::new()),
                "rPh" if !tag.empty => phonetic += 1,
                "t" if !tag.empty => in_t = true,
                _ => {}
            },
            Event::End(end) => match local(end) {
                "si" => {
                    out.push(current.take().unwrap_or_default());
                    if out.len() > MAX_STRINGS {
                        out.iter_mut().for_each(Zeroize::zeroize);
                        return Err(bad("holds more shared strings than this import reads"));
                    }
                }
                "rPh" => phonetic = phonetic.saturating_sub(1),
                "t" => in_t = false,
                _ => {}
            },
            Event::Text(mut text) => {
                if in_t
                    && phonetic == 0
                    && let Some(current) = current.as_mut()
                {
                    current.push_str(&text);
                }
                text.zeroize();
            }
        }
    }
    Ok(out)
}

/// The 0-based column of a cell reference (`C7` is 2), where it names one within [`MAX_COLS`].
fn column_of(reference: &str) -> Option<usize> {
    let bytes = reference.as_bytes();
    let len = bytes
        .iter()
        .position(|b| !b.is_ascii_alphabetic())
        .unwrap_or(bytes.len());
    let letters = &bytes[..len];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut col = 0usize;
    for &letter in letters {
        col = col
            .checked_mul(26)?
            .checked_add(usize::from(letter.to_ascii_uppercase() - b'A') + 1)?;
    }
    let col = col.checked_sub(1)?;
    (col < MAX_COLS).then_some(col)
}

#[derive(Clone, Copy, Default)]
enum CellKind {
    Shared,
    Inline,
    Bool,
    Error,
    #[default]
    Other,
}

impl CellKind {
    fn from_attr(attr: Option<&str>) -> Self {
        match attr.unwrap_or("n") {
            "s" => Self::Shared,
            "inlineStr" => Self::Inline,
            "b" => Self::Bool,
            "e" => Self::Error,
            _ => Self::Other,
        }
    }
}

/// One cell being read.
#[derive(Default)]
struct CellRead {
    col: usize,
    kind: CellKind,
    value: String,
    inline: String,
}

/// Every non-empty cell of a worksheet part: its 0-based row, column and text. Only cells with
/// text are kept, so a styled but empty range costs nothing.
fn cells(sheet: &str, strings: &[String], name: &str) -> Result<Vec<(usize, usize, String)>> {
    let mut reader = Reader::new(sheet);
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    // The row being read, and the next column a cell with no reference takes.
    let mut row: Option<(usize, usize)> = None;
    let mut next_row = 0usize;
    let mut cell: Option<CellRead> = None;
    // Where the text inside a cell goes: its `v`, or an inline string's `t`.
    let mut into_v = false;
    let mut into_t = false;
    let mut in_data = false;
    while let Some(event) = next(&mut reader, name)? {
        match event {
            Event::Start(tag) => match local(tag.name) {
                "sheetData" => in_data = !tag.empty,
                "row" if in_data => {
                    let at = match tag.attr("r") {
                        Some(r) => r
                            .parse::<usize>()
                            .ok()
                            .and_then(|r| r.checked_sub(1))
                            .ok_or_else(|| bad("holds a row with a bad number"))?,
                        None => next_row,
                    };
                    if at >= MAX_ROWS || at < next_row {
                        return Err(bad("holds rows out of order or beyond this import's bound"));
                    }
                    next_row = at + 1;
                    if !tag.empty {
                        row = Some((at, 0));
                    }
                }
                "c" => {
                    let Some((_, next_col)) = row else {
                        continue;
                    };
                    let col = match tag.attr("r") {
                        Some(r) => column_of(r).ok_or_else(|| bad("holds a bad cell reference"))?,
                        None => next_col,
                    };
                    if col < next_col || col >= MAX_COLS {
                        return Err(bad(
                            "holds cells out of order or beyond this import's bound",
                        ));
                    }
                    if let Some((_, next)) = row.as_mut() {
                        *next = col + 1;
                    }
                    let read = CellRead {
                        col,
                        kind: CellKind::from_attr(tag.attr("t")),
                        ..CellRead::default()
                    };
                    if !tag.empty {
                        cell = Some(read);
                    }
                }
                "v" if cell.is_some() && !tag.empty => into_v = true,
                "t" if cell.is_some() && !tag.empty => into_t = true,
                _ => {}
            },
            Event::End(end) => match local(end) {
                "v" => into_v = false,
                "t" => into_t = false,
                "c" => {
                    if let (Some(read), Some((at, _))) = (cell.take(), row) {
                        let col = read.col;
                        let text = text_of(read, strings)?;
                        if text.is_empty() {
                            continue;
                        }
                        if out.len() >= MAX_CELLS {
                            out.iter_mut().for_each(|(_, _, t)| t.zeroize());
                            return Err(bad("holds more cells than this import reads"));
                        }
                        out.push((at, col, text));
                    }
                }
                "row" => row = None,
                "sheetData" => in_data = false,
                _ => {}
            },
            Event::Text(mut text) => {
                if let Some(read) = cell.as_mut() {
                    if into_v {
                        read.value.push_str(&text);
                    } else if into_t {
                        read.inline.push_str(&text);
                    }
                }
                text.zeroize();
            }
        }
    }
    Ok(out)
}

/// A read cell's text.
fn text_of(mut read: CellRead, strings: &[String]) -> Result<String> {
    let text = match read.kind {
        CellKind::Shared => {
            let index = read
                .value
                .trim()
                .parse::<usize>()
                .ok()
                .ok_or_else(|| bad("holds a bad shared string index"))?;
            strings
                .get(index)
                .cloned()
                .ok_or_else(|| bad("points a cell past its shared strings"))?
        }
        CellKind::Inline => std::mem::take(&mut read.inline),
        CellKind::Bool => match read.value.trim() {
            "1" => "1".to_owned(),
            _ => "0".to_owned(),
        },
        // An error cell (`#N/A`) says nothing the import reads.
        CellKind::Error => String::new(),
        // A number, a date serial or a formula's cached string, as stored.
        CellKind::Other => std::mem::take(&mut read.value),
    };
    read.value.zeroize();
    read.inline.zeroize();
    Ok(text)
}

/// The records of the tab named `tab` (any case) of the workbook `bytes`, every row padded to the
/// widest, empty rows kept so a row's number is its place, within a CSV's rows, columns and cell
/// bytes (`crate::import`): what [`super::Sheet::from_records`] reads. A tab the workbook does not hold is refused with the names it does hold, which are
/// the owner's tab titles, never a cell.
///
/// # Errors
/// The bytes are not an xlsx workbook this build reads, the tab is missing, or a bound is
/// passed.
pub fn tab(bytes: &[u8], tab: &str) -> Result<Vec<Vec<String>>> {
    let mut archive = Archive::open(Cursor::new(bytes))
        .map_err(|_| bad("is not a ZIP archive this build reads"))?;
    let workbook =
        part(&mut archive, "xl/workbook.xml")?.ok_or_else(|| bad("has no xl/workbook.xml"))?;
    let tabs = tabs(&workbook)?;
    let Some((_, id)) = tabs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(tab.trim()))
    else {
        let names: Vec<&str> = tabs.iter().map(|(name, _)| name.as_str()).collect();
        return Err(Error::new(&format!(
            "the workbook has no tab named {tab:?}; its tabs are {names:?}"
        )));
    };
    let rels = part(&mut archive, "xl/_rels/workbook.xml.rels")?
        .ok_or_else(|| bad("has no xl/_rels/workbook.xml.rels"))?;
    let path = target(&rels, id)?.ok_or_else(|| bad("names no part for the tab"))?;
    let mut strings = match part(&mut archive, "xl/sharedStrings.xml")? {
        Some(text) => shared(&text)?,
        None => Vec::new(),
    };
    let sheet = part(&mut archive, &path)?.ok_or_else(|| bad("lacks the tab's part"))?;
    let read = cells(&sheet, &strings, &path);
    strings.iter_mut().for_each(Zeroize::zeroize);
    let mut cells = read?;
    // The tab's records, as a CSV's: within its rows, columns and cell bytes.
    let height = cells.iter().map(|(row, _, _)| row + 1).max().unwrap_or(0);
    let width = cells.iter().map(|(_, col, _)| col + 1).max().unwrap_or(0);
    let over = height > CSV_ROWS
        || width > CSV_COLUMNS
        || cells.iter().any(|(_, _, text)| text.len() > CSV_CELL);
    if over {
        cells.iter_mut().for_each(|(_, _, text)| text.zeroize());
        return Err(Error::new(
            "the tab exceeds the import's capacity (the rows, columns and cell size a CSV may hold)",
        ));
    }
    let mut records = vec![vec![String::new(); width]; height];
    for (row, col, text) in cells {
        if let Some(slot) = records.get_mut(row).and_then(|record| record.get_mut(col)) {
            *slot = text;
        }
    }
    Ok(records)
}

#[cfg(test)]
pub(crate) mod tests;
