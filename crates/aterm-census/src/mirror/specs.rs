// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SPECS.md as the mirror registry reads it: its clause rows (tab tables and `|` tables whose
//! header has an `id` and a `statement` column), each clause's digest, and the mirror-proof
//! notes its notes cells carry. What a clause says is its id, its `functions` (`fn`) cell and
//! its `statement` cell: the code it is about and what it claims of it. The other cells
//! (tests, difficulty, what it subsumes) can change without changing the clause. A clause
//! row's notes cell is its last, after the statement: the blocking-gaps column, where every
//! crate's table keeps what a proof stands on (its header's name and place vary from crate to
//! crate; its place in the row does not).
//!
//! A note names the rows it speaks for, in one of two forms, so that a note can neither claim a
//! proof nobody recorded nor excuse a row it does not name:
//!
//! ```text
//! Proved via mirror: Clean R213 rows `mouse_button_code`, `wheel_dir_code` (…)
//! Stale proof: Clean R212 row `canonical_bracket` (…)
//! ```
//!
//! Anything else that reads as a mirror-proof claim ("mirror-proved", "proved (via mirror)",
//! a doubled space) is an unreadable note, and so is either form outside a clause's notes
//! column; "not (yet) proved via mirror" claims nothing.

use super::fnv;

/// The note that claims a row's mirror proof.
pub const CLAIM: &str = "Proved via mirror";

/// The note that withdraws a row's mirror proof: the code or the clause moved since.
pub const STALE: &str = "Stale proof";

/// One cell of a clause row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell<'a> {
    /// The cell's text.
    pub text: &'a str,
    /// The notes cell (the row's last, after its statement): what the proof stands on, not
    /// what the clause says.
    pub notes: bool,
    /// The id, `functions` or `statement` cell: what the clause says.
    pub said: bool,
}

/// One clause row of a SPECS.md.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause<'a> {
    /// The clause id (the row's first cell, backticks dropped).
    pub id: String,
    /// 1-based.
    pub line: usize,
    /// The row's cells, in column order.
    pub cells: Vec<Cell<'a>>,
}

impl Clause<'_> {
    /// The digest of what the clause says: its id, functions and statement cells, each read as
    /// its words single-spaced.
    #[must_use]
    pub fn digest(&self) -> String {
        let said: Vec<String> = self
            .cells
            .iter()
            .filter(|c| c.said)
            .map(|c| c.text.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        fnv(&said.join("\t"))
    }
}

/// `line`'s cells as a `|` table row: split at each `|` outside backticks, the outer two
/// dropped.
fn pipe_cells(line: &str) -> Vec<&str> {
    let mut cells = Vec::new();
    let mut code = false;
    let mut start = 0;
    for (i, c) in line.char_indices() {
        match c {
            '`' => code = !code,
            '|' if !code => {
                cells.push(line[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    cells.push(line[start..].trim());
    if cells.first() == Some(&"") {
        cells.remove(0);
    }
    if cells.last() == Some(&"") {
        cells.pop();
    }
    cells
}

/// The statement column and the functions column, when `cells` is a clause table's header (an
/// `id` column first and a `statement` column).
fn header_of(cells: &[&str]) -> Option<(usize, Option<usize>)> {
    if cells.first() != Some(&"id") {
        return None;
    }
    let statement = cells
        .iter()
        .position(|c| c.to_ascii_lowercase().starts_with("statement"))?;
    let functions = cells.iter().position(|c| matches!(*c, "functions" | "fn"));
    Some((statement, functions))
}

/// Every clause row of `specs`, in file order, a repeated id included.
#[must_use]
pub fn clauses(specs: &str) -> Vec<Clause<'_>> {
    let mut out = Vec::new();
    let mut tab: Option<(usize, Option<usize>)> = None;
    let mut pipe: Option<(usize, Option<usize>)> = None;
    for (index, line) in specs.lines().enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        if let Some(header) = header_of(&fields) {
            tab = Some(header);
            continue;
        }
        let piped = line.trim_start().starts_with('|');
        let cells = if piped { pipe_cells(line) } else { Vec::new() };
        if piped && let Some(header) = header_of(&cells) {
            pipe = Some(header);
            continue;
        }
        let (row, (statement, functions)) = if let Some(header) = tab.filter(|_| fields.len() > 1) {
            (fields, header)
        } else if let Some(header) = pipe.filter(|_| piped) {
            if cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')))
            {
                continue;
            }
            (cells, header)
        } else {
            tab = None;
            pipe = pipe.filter(|_| piped);
            continue;
        };
        let id = row[0].trim().trim_matches('`');
        if id.is_empty() {
            continue;
        }
        out.push(Clause {
            id: id.to_string(),
            line: index + 1,
            cells: row
                .iter()
                .enumerate()
                .map(|(i, text)| Cell {
                    text,
                    notes: i > statement && i + 1 == row.len(),
                    said: i == 0 || i == statement || Some(i) == functions,
                })
                .collect(),
        });
    }
    out
}

/// A note in one of the two forms: which Clean entry, which rows, and whether it withdraws
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// A [`STALE`] note rather than a [`CLAIM`].
    pub stale: bool,
    /// The Clean entry (`R213`).
    pub clean: String,
    /// The rows it names.
    pub rows: Vec<String>,
}

/// The notes `text` carries, and each passage of it that reads as a mirror-proof claim in
/// neither form.
#[must_use]
pub fn notes(text: &str) -> (Vec<Note>, Vec<String>) {
    let low = text.to_ascii_lowercase();
    let mut rest: Vec<u8> = text.as_bytes().to_vec();
    let mut found = Vec::new();
    let mut bad = Vec::new();
    for (phrase, stale) in [(CLAIM, false), (STALE, true)] {
        let key = format!("{}:", phrase.to_ascii_lowercase());
        for (at, _) in low.match_indices(&key) {
            rest[at..at + key.len()].fill(b' ');
            match note_body(&text[at + key.len()..]) {
                Ok((clean, rows)) => found.push(Note { stale, clean, rows }),
                Err(why) => bad.push(format!("\"{}\": {why}", excerpt(&text[at..]))),
            }
        }
    }
    bad.extend(loose(&String::from_utf8_lossy(&rest)));
    (found, bad)
}

/// The start of `text`, for a finding.
fn excerpt(text: &str) -> String {
    let cut: String = text.chars().take(60).collect();
    if cut.len() < text.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// What follows a note's `:`: ` Clean R<n> row `a`` or ` Clean R<n> rows `a`, `b` and `c``.
fn note_body(text: &str) -> Result<(String, Vec<String>), String> {
    const FORM: &str = "write it ` Clean R<n> row `<row>`` (rows `<a>`, `<b>`)";
    let after = text.strip_prefix(" Clean R").ok_or(FORM)?;
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Err(FORM.to_string());
    }
    let clean = format!("R{}", &after[..digits]);
    let mut rest = after[digits..]
        .strip_prefix(" rows ")
        .or_else(|| after[digits..].strip_prefix(" row "))
        .ok_or(FORM)?;
    let mut rows = Vec::new();
    loop {
        let name = rest.strip_prefix('`').ok_or(FORM)?;
        let len = name
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if len == 0 || name.as_bytes().get(len) != Some(&b'`') {
            return Err(FORM.to_string());
        }
        rows.push(name[..len].to_string());
        rest = &name[len + 1..];
        match [", and `", " and `", ", `"]
            .iter()
            .find(|sep| rest.starts_with(**sep))
        {
            Some(sep) => rest = &rest[sep.len() - 1..],
            None => break,
        }
    }
    Ok((clean, rows))
}

/// Each passage of `text` where "mirror" stands within three words of a proof word, read as
/// words (case, punctuation and odd spaces aside), unless a "not" just before it negates it.
fn loose(text: &str) -> Vec<String> {
    let norm: String = text
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = norm.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if !matches!(*w, "mirror" | "mirrored") {
            continue;
        }
        let lo = i.saturating_sub(3);
        let hi = (i + 3).min(words.len() - 1);
        let Some(proof) = (lo..=hi)
            .find(|&j| j != i && (words[j].starts_with("prov") || words[j].starts_with("proof")))
        else {
            continue;
        };
        let first = proof.min(i);
        if words[first.saturating_sub(2)..first].contains(&"not") {
            continue;
        }
        out.push(format!(
            "\"{}\" reads as a mirror-proof claim but is neither \"{CLAIM}: Clean R<n> row \
             `<row>`\" nor \"{STALE}: …\"",
            words[lo..=hi].join(" ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
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
        let refunctioned =
            clauses("id\tkind\tfunctions\tstatement\tgaps\na-b\ttable\tg\tf(x) = x\t\n");
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
}
