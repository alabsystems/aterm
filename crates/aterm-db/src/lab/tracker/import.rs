// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE IMPORT (docs/DESIGN-lab-tracker-2026-11-14.md §3, §5 milestone 3; package TK-11): the lab
//! tracker's TSVs, publication's `goals/data` at a named commit, read into `lab/tracker`'s rows
//! and log entries, with every cell the tracker cannot hold as the TSV says it named by its file,
//! line, column and rule.
//!
//! Nothing is lost on the way. Each TSV row becomes one tracker row, or for `metrics.tsv` one
//! sample of the `measure` log, keyed as its name makes it ([`super::mapped`]). Each non-empty
//! cell lands in a column of that row ([`Place`]), or is a [`Finding`]: a cell no column holds,
//! one the tracker computes, a reference that names no row, a value its column's type refuses.
//! A finding says where the cell's words went ([`Kept`]): into a column of the row that keeps
//! what was said (`state_said`, a principal's `said`), into a note of
//! the `note` log about the row, quoting the cell and citing the commit, or nowhere, where the
//! tracker refuses to hold them at all (a credential's shape), or the line makes no row, and
//! then the TSV at the named commit is where they stay. A row the import cannot make is no
//! row any other row may name: the import runs again without it until every reference it
//! writes names a row it makes ([`import`]).
//!
//! The import is pure: it reads text and writes nothing. The rows it makes are the owner's
//! write of a new table ([`Import::changes`], one record each, saying [`Import::said`]); its
//! samples and notes are entries the importing machine appends to its own logs.

use super::tables::{
    COMPETITION, DECISION, GOAL, GROUP, LANE, METRIC, PACKAGE, PLAN, PROJECT, REPO, RULING, STOP,
};
use super::{MEASURE, NOTE, mapped};
use crate::database::{Change, MAX_WRITES};
use crate::said::Said;
use crate::typed::{
    Collection, Ty,
    cell::{self, CellKey, CellValue},
    grid, guard, owned, pk_grammar,
    values::{self, Evidence, MAX_EVIDENCE, Principal},
};
use crate::{Error, Result};
use aterm_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The repository the TSVs are read from, as evidence names it.
pub const REPOSITORY: &str = "publication";
/// Where the TSVs sit in it.
pub const DATA: &str = "goals/data";

/// A day in milliseconds: a sample's day as the time its `at` column holds, midnight UTC.
const MS_DAY: u64 = 86_400_000;

/// Each TSV the import reads and the columns it reads of it.
pub const FILES: &[(&str, &[&str])] = &[
    (
        "groups.tsv",
        &[
            "id",
            "horizon",
            "name",
            "summary",
            "coordinator",
            "scoreboard",
            "value",
            "of",
        ],
    ),
    (
        "goals.tsv",
        &[
            "id",
            "group",
            "title",
            "detail",
            "metric",
            "baseline",
            "target",
            "target_by",
            "state",
        ],
    ),
    (
        "metrics.tsv",
        &[
            "date", "goal", "metric", "value", "unit", "machine", "source", "note",
        ],
    ),
    (
        "lanes.tsv",
        &[
            "id",
            "name",
            "prefix",
            "repos",
            "goals",
            "bottleneck",
            "order",
        ],
    ),
    (
        "packages.tsv",
        &[
            "id",
            "lane",
            "order",
            "goal",
            "title",
            "outcome",
            "repo",
            "owns",
            "depends",
            "exit_command",
            "exit_expect",
            "size",
            "state",
            "claimed_by",
            "claimed_on",
            "done_on",
            "evidence",
        ],
    ),
    (
        "decisions.tsv",
        &[
            "id",
            "lane",
            "decision",
            "decided_by",
            "date",
            "reason",
            "state",
        ],
    ),
    ("rulings.tsv", &["date", "about", "quote", "repo", "source"]),
    ("stops.tsv", &["lane", "item"]),
    (
        "competitions.tsv",
        &[
            "id", "name", "kind", "edition", "tracks", "catalog", "scoring", "limits", "engine",
            "next", "today", "state", "url",
        ],
    ),
    ("projects.tsv", &["project", "size", "real", "gap", "score"]),
    ("plan.tsv", &["key", "value"]),
];

/// Why a cell is not where its TSV column would put it: each a word, a sentence and a heading.
/// The set is closed: a cell the import cannot place by one of the others is [`Rule::Refused`],
/// whose cause no import rule names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// The tracker keeps no column for what the TSV column says.
    NoColumn,
    /// The tracker computes the value; it stores none.
    Computed,
    /// A reference, or a member of a list of them, names no row the TSVs hold: words, or an id
    /// no row has.
    NamesNoRow,
    /// A member of a list of paths is no path in a repository.
    NotAPath,
    /// A number's cell holds no number written exactly.
    NotANumber,
    /// A date's cell holds no `YYYY-MM-DD` date.
    NotADay,
    /// A cell of the column's own words holds another.
    NotAWord,
    /// A URL's cell holds no https URL without query, fragment or user part.
    NotAUrl,
    /// A state word the tracker does not have, kept in `state_said` beside the nearest one.
    StateMapped,
    /// Who acted, named in words: kept as what the principal said, with no session or program.
    SaidInWords,
    /// A sample's machine named in words: the `measure` log names one by its machine uuid.
    MachineInWords,
    /// A sample's unit is not its metric's, which its first sample stated.
    UnitDiffers,
    /// A cell holds part of a value whose other part is empty: a claim's day with no holder, an
    /// exit's expectation with no command.
    Incomplete,
    /// A dependency that would close a circle of packages.
    DependsCycle,
    /// Longer than the column takes.
    TooLong,
    /// Words the tracker never stores: a credential's shape, a secret named.
    SecretShaped,
    /// A row lacks a value its table needs, so it is not made.
    NeedsAValue,
    /// A row whose key an earlier row of its file already has.
    DuplicateRow,
    /// A line with more or fewer cells than its header: its cells are read by their place, as
    /// `goals.py` reads them, and a cell past the header is kept nowhere.
    WrongWidth,
    /// A header column the import does not read.
    UnknownColumn,
    /// A column the import reads that the header does not have.
    MissingColumn,
    /// A TSV the import does not read.
    UnknownFile,
    /// A TSV the import reads that the commit does not have.
    MissingFile,
    /// The cell holds only white space.
    Blank,
    /// The line makes no row (another finding on it says why), so the cell is kept nowhere.
    RowNotMade,
    /// The column's type refused the value by a rule no other import rule names (a control
    /// character, a principal's word outside its grammar).
    Refused,
}

impl Rule {
    /// Every rule, in order.
    pub const ALL: &'static [Self] = &[
        Self::NoColumn,
        Self::Computed,
        Self::NamesNoRow,
        Self::NotAPath,
        Self::NotANumber,
        Self::NotADay,
        Self::NotAWord,
        Self::NotAUrl,
        Self::StateMapped,
        Self::SaidInWords,
        Self::MachineInWords,
        Self::UnitDiffers,
        Self::Incomplete,
        Self::DependsCycle,
        Self::TooLong,
        Self::SecretShaped,
        Self::NeedsAValue,
        Self::DuplicateRow,
        Self::WrongWidth,
        Self::UnknownColumn,
        Self::MissingColumn,
        Self::UnknownFile,
        Self::MissingFile,
        Self::Blank,
        Self::RowNotMade,
        Self::Refused,
    ];

    /// The rule's word, as a finding of the tracker's checks is named.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::NoColumn => "no-column",
            Self::Computed => "computed",
            Self::NamesNoRow => "names-no-row",
            Self::NotAPath => "not-a-path",
            Self::NotANumber => "not-a-number",
            Self::NotADay => "not-a-day",
            Self::NotAWord => "not-a-word",
            Self::NotAUrl => "not-a-url",
            Self::StateMapped => "state-mapped",
            Self::SaidInWords => "said-in-words",
            Self::MachineInWords => "machine-in-words",
            Self::UnitDiffers => "unit-differs",
            Self::Incomplete => "incomplete",
            Self::DependsCycle => "depends-cycle",
            Self::TooLong => "too-long",
            Self::SecretShaped => "secret-shaped",
            Self::NeedsAValue => "needs-a-value",
            Self::DuplicateRow => "duplicate-row",
            Self::WrongWidth => "wrong-width",
            Self::UnknownColumn => "unknown-column",
            Self::MissingColumn => "missing-column",
            Self::UnknownFile => "unknown-file",
            Self::MissingFile => "missing-file",
            Self::Blank => "blank",
            Self::RowNotMade => "row-not-made",
            Self::Refused => "refused",
        }
    }

    /// What the rule means, in a person's words; it names no value.
    #[must_use]
    pub const fn sentence(self) -> &'static str {
        match self {
            Self::NoColumn => "The tracker keeps no column for what this TSV column says",
            Self::Computed => "The tracker computes this value from its rows and stores none",
            Self::NamesNoRow => {
                "A reference names no row the TSVs hold: words, or an id no row has"
            }
            Self::NotAPath => "A member of a list of paths is no path inside a repository",
            Self::NotANumber => "A number's cell holds no number written exactly",
            Self::NotADay => "A date's cell holds no date written as 2026-11-14",
            Self::NotAWord => "The cell holds none of its column's own words",
            Self::NotAUrl => "The cell holds no https URL without a query, fragment or user part",
            Self::StateMapped => {
                "A state word the tracker does not have: the row states the nearest of its own \
                 and keeps the word"
            }
            Self::SaidInWords => {
                "Who acted is named in words, not as a session or program aterm stated"
            }
            Self::MachineInWords => {
                "A sample names its machine in words; a sample names one by its machine uuid"
            }
            Self::UnitDiffers => "A sample's unit is not the unit its metric's first sample stated",
            Self::Incomplete => "The cell holds part of a value whose other part is empty",
            Self::DependsCycle => "The dependency would close a circle of packages",
            Self::TooLong => "The cell is longer than its column takes",
            Self::SecretShaped => "The words have a credential's shape, which is never stored",
            Self::NeedsAValue => "The row lacks a value its table needs, so it is not made",
            Self::DuplicateRow => "An earlier row of the file has this row's key",
            Self::WrongWidth => "The line has more or fewer cells than its header",
            Self::UnknownColumn => "The import reads no column of this name",
            Self::MissingColumn => "The header lacks a column the import reads",
            Self::UnknownFile => "The import reads no TSV of this name",
            Self::MissingFile => "The commit lacks a TSV the import reads",
            Self::Blank => "The cell holds only white space",
            Self::RowNotMade => {
                "The line makes no row, as another finding on it says, so the cell is kept nowhere"
            }
            Self::Refused => {
                "The column's type refused the value by a rule the import does not name"
            }
        }
    }

    /// A few words in sentence case.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::NoColumn => "No column for it",
            Self::Computed => "Computed by the tracker",
            Self::NamesNoRow => "Names no row",
            Self::NotAPath => "Not a path",
            Self::NotANumber => "Not a number",
            Self::NotADay => "Not a date",
            Self::NotAWord => "Not a word of its column",
            Self::NotAUrl => "Not a URL",
            Self::StateMapped => "State mapped from another word",
            Self::SaidInWords => "Named in words",
            Self::MachineInWords => "Machine named in words",
            Self::UnitDiffers => "Unit differs from its metric's",
            Self::Incomplete => "Part of a value",
            Self::DependsCycle => "Dependency closes a circle",
            Self::TooLong => "Too long for its column",
            Self::SecretShaped => "Shaped like a credential",
            Self::NeedsAValue => "Row lacks a needed value",
            Self::DuplicateRow => "Key already taken",
            Self::WrongWidth => "Line of the wrong width",
            Self::UnknownColumn => "Column not read",
            Self::MissingColumn => "Column missing",
            Self::UnknownFile => "File not read",
            Self::MissingFile => "File missing",
            Self::Blank => "Only white space",
            Self::RowNotMade => "Row not made",
            Self::Refused => "Refused by its type",
        }
    }
}

/// Where a finding's words went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kept {
    /// In a column of the row that keeps what was said: `state_said`, a principal's `said`.
    Column,
    /// In a note of the `note` log about the row, which quotes the cell and cites the commit.
    Note,
    /// Nowhere in the tracker: the TSV at the named commit holds them.
    Not,
}

/// One cell, line or file the tracker does not hold as its TSV says.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// The TSV (`packages.tsv`).
    pub file: String,
    /// Its line, counted from 1, the header's; 0 for the whole file.
    pub line: usize,
    /// The key of the tracker row the line is, where it has one.
    pub row: Option<String>,
    /// The TSV column; empty for a whole line or file.
    pub column: String,
    pub rule: Rule,
    /// How many members of a list the rule names: 1 for a cell that is not a list.
    pub members: usize,
    pub kept: Kept,
}

/// A TSV cell: its file, its line and its column.
pub type At = (String, usize, String);

/// Where a cell landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// In a column of a tracker row.
    Row {
        collection: &'static str,
        key: String,
        column: &'static str,
    },
    /// In a column of a sample of the `measure` log: [`Import::measures`]`[index]`.
    Measure { index: usize, column: &'static str },
    /// Quoted in a note: [`Import::notes`]`[index]`.
    Note { index: usize },
}

/// One tracker row the import makes: its scalar columns' values and its sets' members, typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub collection: &'static Collection,
    pub key: String,
    pub values: BTreeMap<&'static str, Value>,
    pub members: BTreeMap<&'static str, BTreeSet<String>>,
    /// The TSV line it is made from; a repository's, the first that names it.
    pub file: &'static str,
    pub line: usize,
}

/// What the TSVs at one commit are in the tracker.
#[derive(Clone, Debug, Default)]
pub struct Import {
    /// The publication commit the TSVs were read at.
    pub commit: String,
    /// The rows each TSV holds, by its name.
    pub read: BTreeMap<String, usize>,
    /// The tracker's rows, those a row refers to first.
    pub rows: Vec<Row>,
    /// The `measure` log's samples, without the counter the append gives each.
    pub measures: Vec<Map>,
    /// The `note` log's notes, without the counter the append gives each.
    pub notes: Vec<Map>,
    pub findings: Vec<Finding>,
    /// Where each non-empty cell landed, by its file, line and column: a column, a sample's
    /// column or a note, and for a list some of whose members a finding names, both the column
    /// that holds the others and the note that quotes the cell.
    pub placed: BTreeMap<At, Vec<Place>>,
}

impl Import {
    /// The owner's write of [`Self::rows`] into a table that holds none of them, as the records
    /// it appends: each row's changes a record of its own, and a row of more changes than a
    /// record takes ([`MAX_WRITES`]: a package that depends on 165 others) several records of
    /// that row alone, its birth first. Every record says the same ([`Self::said`]), so a record
    /// still holds one row and one reason.
    ///
    /// # Errors
    /// A value the import made breaks its column's rules: a defect of the import.
    pub fn changes(&self) -> Result<Vec<Vec<Change>>> {
        let mut records = Vec::new();
        for row in &self.rows {
            let changes = changes(row)?;
            records.extend(changes.chunks(MAX_WRITES).map(<[Change]>::to_vec));
        }
        Ok(records)
    }

    /// What each record of the import says: who imported it, why, and the commit it read.
    #[must_use]
    pub fn said(&self, principal: Principal) -> Said {
        Said {
            principal,
            reason: Some(format!(
                "the lab tracker's TSVs at {REPOSITORY} {}",
                self.commit
            )),
            evidence: vec![self.evidence()],
        }
    }

    /// The commit the TSVs were read at, as evidence.
    #[must_use]
    pub fn evidence(&self) -> Evidence {
        Evidence::Commit {
            repo: REPOSITORY.to_owned(),
            commit: self.commit.clone(),
        }
    }
}

/// One row's changes: each scalar value and the lifecycle of a born row, then each set member.
fn changes(row: &Row) -> Result<Vec<Change>> {
    let collection = row.collection;
    let owner = |x| CellValue::owner(collection.version, x);
    let scalar = |column: &str, x: &Value| {
        cell::change(
            collection,
            CellKey {
                pk: &row.key,
                col: column,
                elem: None,
            },
            &owner(x.clone()),
        )
    };
    let mut out = Vec::new();
    for (column, x) in &row.values {
        out.push(scalar(column, x)?);
    }
    out.push(scalar("row", &Value::String("live".to_owned()))?);
    for (column, members) in &row.members {
        for member in members {
            out.push(cell::member_change(
                collection, &row.key, column, member, true, owner,
            )?);
        }
    }
    Ok(out)
}

/// The TSVs a directory holds, by name: the files of `goals/data` checked out at a commit.
///
/// # Errors
/// The directory or a TSV in it is unreadable, or a TSV is not UTF-8.
pub fn read_dir(dir: &std::path::Path) -> std::io::Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(".tsv") {
            files.insert(name.to_owned(), std::fs::read_to_string(&path)?);
        }
    }
    Ok(files)
}

/// The words a package's `depends` cell holds as ids, split as the lab tracker's `goals.py`
/// splits them: at commas, semicolons and spaces.
#[must_use]
pub fn split_ids(text: &str) -> Vec<&str> {
    text.split([',', ';', ' '])
        .filter(|part| !part.is_empty())
        .collect()
}

/// A list of names: split at commas and at `|`, each trimmed.
fn split_names(text: &str) -> Vec<&str> {
    text.split([',', '|'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// A list of paths: split at commas, each trimmed.
fn split_paths(text: &str) -> Vec<&str> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// The day a `YYYY-MM-DD` date names, as a day cell holds it; `None` for any other text.
fn day(text: &str) -> Option<u64> {
    let day = grid::read(&Ty::Day, text).ok()?.as_u64()?;
    (grid::day_date(day).as_deref() == Some(text)).then_some(day)
}

/// The rule a column of type `ty` refused a value by, as the import names it.
fn classify(ty: &Ty, rule: &str) -> Rule {
    if rule == guard::Refusal::Length.rule() {
        return Rule::TooLong;
    }
    if rule == guard::Refusal::CredentialShape.rule() || rule == guard::Refusal::SecretWord.rule() {
        return Rule::SecretShaped;
    }
    match ty {
        Ty::Decimal | Ty::Uint | Ty::Int { .. } => Rule::NotANumber,
        Ty::Day | Ty::Time => Rule::NotADay,
        Ty::Enum(_) => Rule::NotAWord,
        Ty::Path => Rule::NotAPath,
        Ty::Url => Rule::NotAUrl,
        Ty::Ref(_) => Rule::NamesNoRow,
        Ty::Set(member) | Ty::List(member, _) => classify(member, rule),
        _ => Rule::Refused,
    }
}

/// One TSV line: its number and its cells by column.
struct Line {
    n: usize,
    cells: BTreeMap<String, String>,
}

impl Line {
    fn get(&self, column: &str) -> &str {
        self.cells.get(column).map_or("", String::as_str)
    }
}

/// One TSV, read.
struct Tsv {
    name: &'static str,
    lines: Vec<Line>,
}

/// A row being made, with what it adds once it is: its notes and findings wait on it, since a
/// note is about a row the tracker holds.
struct Making {
    row: Row,
    /// Each note, with the cells it quotes.
    notes: Vec<(Map, Vec<At>)>,
    findings: Vec<Finding>,
    placed: Vec<(At, Place)>,
}

impl Making {
    /// Begin a row of `collection` keyed `key`, made from line `line` of `file`.
    fn new(collection: &'static Collection, key: String, file: &'static str, line: usize) -> Self {
        Self {
            row: Row {
                collection,
                key,
                values: BTreeMap::new(),
                members: BTreeMap::new(),
                file,
                line,
            },
            notes: Vec::new(),
            findings: Vec::new(),
            placed: Vec::new(),
        }
    }

    /// A finding about this row whose words a column of it keeps.
    fn kept(&mut self, column: &str, rule: Rule) {
        self.findings.push(Finding {
            file: self.row.file.to_owned(),
            line: self.row.line,
            row: Some(self.row.key.clone()),
            column: column.to_owned(),
            rule,
            members: 1,
            kept: Kept::Column,
        });
    }

    /// The cell `tsv` of `line` landed in this row's column `column`.
    fn place(&mut self, line: &Line, tsv: &str, column: &'static str) {
        self.placed.push((
            (self.row.file.to_owned(), line.n, tsv.to_owned()),
            Place::Row {
                collection: self.row.collection.name,
                key: self.row.key.clone(),
                column,
            },
        ));
    }
}

/// The import's state: the keys of every row it will make, and what it has made and found.
struct Importer {
    commit: String,
    at_ms: u64,
    /// The keys of the rows each cells table will hold.
    keys: BTreeMap<&'static str, BTreeSet<String>>,
    /// The rows already begun, by table and key.
    seen: BTreeSet<(&'static str, String)>,
    /// The rows this pass could not make, by table and key.
    unmade: BTreeSet<(&'static str, String)>,
    out: Import,
}

impl Importer {
    /// The key of the row named `id` where the table `collection` will hold it.
    fn holds(&self, collection: &str, id: &str) -> Option<String> {
        let key = mapped::key(id);
        self.keys
            .get(collection)
            .is_some_and(|keys| keys.contains(&key))
            .then_some(key)
    }

    /// A finding about no row the tracker holds: a file's, a header's, or a line's whose row is
    /// not made.
    fn found(&mut self, file: &str, line: usize, column: &str, rule: Rule) {
        self.out.findings.push(Finding {
            file: file.to_owned(),
            line,
            row: None,
            column: column.to_owned(),
            rule,
            members: 1,
            kept: Kept::Not,
        });
    }

    /// Whether the row of `collection` keyed `key` is one no earlier line began; a finding
    /// where one did.
    fn fresh(
        &mut self,
        file: &str,
        line: usize,
        collection: &'static Collection,
        key: &str,
    ) -> bool {
        if self.seen.insert((collection.name, key.to_owned())) {
            return true;
        }
        self.found(file, line, "", Rule::DuplicateRow);
        false
    }

    /// A note about the row `about` of `on` quoting `pieces` (`<column>: <words>` each) of line
    /// `line` of `file` and citing the commit; `None` where the note's own rules refuse it.
    fn note(
        &self,
        (on, about): (&str, &str),
        (file, line): (&str, usize),
        rule: Rule,
        pieces: &[(&str, &str)],
    ) -> Option<Map> {
        if rule == Rule::SecretShaped {
            return None;
        }
        let mut evidence: Vec<Value> = pieces
            .iter()
            .map(|(column, words)| Evidence::note(&format!("{column}: {words}")).to_value())
            .collect();
        evidence.push(
            Evidence::Commit {
                repo: REPOSITORY.to_owned(),
                commit: self.commit.clone(),
            }
            .to_value(),
        );
        if evidence.len() > MAX_EVIDENCE || evidence.iter().any(|x| values::evidence(x).is_err()) {
            return None;
        }
        let columns: Vec<&str> = pieces.iter().map(|(column, _)| *column).collect();
        let mut note = Map::new();
        note.insert("on".into(), Value::String(on.to_owned()));
        note.insert("about".into(), Value::String(about.to_owned()));
        note.insert(
            "text".into(),
            Value::String(format!(
                "{DATA}/{file} line {line}, {}: {}",
                columns.join(", "),
                rule.heading()
            )),
        );
        note.insert("evidence".into(), Value::Array(evidence));
        note.insert("at".into(), Value::from(self.at_ms));
        owned::log_row(&NOTE, &note, 1).ok().map(|_| note)
    }

    /// A finding about the row `making`, its words (`pieces`, each `<column>: <words>`, read
    /// from `line` of the row's file) quoted in a note about the row where the note's rules
    /// take them, else kept nowhere.
    fn noted(
        &self,
        making: &mut Making,
        line: usize,
        column: &str,
        (rule, members): (Rule, usize),
        pieces: &[(&str, &str)],
    ) {
        let file = making.row.file;
        let note = self.note(
            (making.row.collection.name, &making.row.key),
            (file, line),
            rule,
            pieces,
        );
        let kept = if let Some(note) = note {
            let cells = pieces
                .iter()
                .map(|(column, _)| (file.to_owned(), line, (*column).to_owned()))
                .collect();
            making.notes.push((note, cells));
            Kept::Note
        } else {
            Kept::Not
        };
        making.findings.push(Finding {
            file: file.to_owned(),
            line,
            row: Some(making.row.key.clone()),
            column: column.to_owned(),
            rule,
            members,
            kept,
        });
    }

    /// `text`, non-empty, as a value of `collection`'s column `column`: read as the grid reads
    /// what a person types, a date only as `YYYY-MM-DD`, a reference keyed as its name makes it
    /// and held to the rows the TSVs make; then held to the column's type and guards. The rule
    /// it breaks where it is none.
    fn value(
        &self,
        collection: &Collection,
        column: &str,
        text: &str,
    ) -> std::result::Result<Value, Rule> {
        let column = collection.column(column).ok_or(Rule::Refused)?;
        if text.trim().is_empty() {
            return Err(Rule::Blank);
        }
        let x = match column.ty {
            Ty::Day => Value::from(day(text).ok_or(Rule::NotADay)?),
            Ty::Ref(target) => Value::String(self.holds(target, text).ok_or(Rule::NamesNoRow)?),
            ty => grid::read(&ty, text).map_err(|rule| classify(&ty, rule))?,
        };
        cell::rule(&column.ty, &x, column.null).map_err(|rule| classify(&column.ty, rule))?;
        Ok(x)
    }

    /// The cell `tsv` of `line` as `making`'s scalar column `column` ([`Self::value`]); a cell
    /// that is none quoted in a note by the rule it breaks.
    fn scalar(&self, making: &mut Making, line: &Line, tsv: &str, column: &'static str) {
        let text = line.get(tsv);
        if text.is_empty() {
            return;
        }
        match self.value(making.row.collection, column, text) {
            Ok(x) => {
                making.row.values.insert(column, x);
                making.place(line, tsv, column);
            }
            Err(rule) => self.noted(making, line.n, tsv, (rule, 1), &[(tsv, text)]),
        }
    }

    /// The cell `tsv` of `line` as members of `making`'s set column `column`, split by `split`:
    /// a reference keyed as its name makes it and held to the rows the TSVs make, any other
    /// member held to the set's member type. The members that fail are one finding, the whole
    /// cell quoted in a note; the others are members.
    fn set(
        &self,
        making: &mut Making,
        line: &Line,
        (tsv, column): (&str, &'static str),
        split: fn(&str) -> Vec<&str>,
    ) {
        let text = line.get(tsv);
        let Some(held) = making.row.collection.column(column) else {
            return;
        };
        let mut failed: Vec<Rule> = Vec::new();
        let mut members = BTreeSet::new();
        if !text.is_empty() && split(text).is_empty() {
            failed.push(Rule::Blank);
        }
        for member in split(text) {
            let made = match held.ty {
                Ty::Set(Ty::Ref(target)) => self.holds(target, member).ok_or(Rule::NamesNoRow),
                ty => cell::element(held, member)
                    .map(|(_, x)| x.as_str().map_or_else(|| member.to_owned(), str::to_owned))
                    .map_err(|rule| classify(&ty, rule)),
            };
            match made {
                Ok(member) => {
                    members.insert(member);
                }
                Err(rule) => failed.push(rule),
            }
        }
        if !members.is_empty() {
            making
                .row
                .members
                .entry(column)
                .or_default()
                .extend(members);
            making.place(line, tsv, column);
        }
        if let Some(&first) = failed.first() {
            // Words the tracker never stores keep the whole cell out of a note.
            let rule = if failed.contains(&Rule::SecretShaped) {
                Rule::SecretShaped
            } else {
                first
            };
            self.noted(making, line.n, tsv, (rule, failed.len()), &[(tsv, text)]);
        }
    }

    /// The cell `tsv` of `line`, which no column of the row holds, quoted in a note by `rule`.
    fn unheld(&self, making: &mut Making, line: &Line, tsv: &str, rule: Rule) {
        let text = line.get(tsv);
        if !text.is_empty() {
            self.noted(making, line.n, tsv, (rule, 1), &[(tsv, text)]);
        }
    }

    /// The cell at `cell` landed at `place`.
    fn place(&mut self, cell: At, place: Place) {
        self.out.placed.entry(cell).or_default().push(place);
    }

    /// Add `notes`, each with the cells it quotes.
    fn add_notes(&mut self, notes: Vec<(Map, Vec<At>)>) {
        for (note, cells) in notes {
            let index = self.out.notes.len();
            self.out.notes.push(note);
            for cell in cells {
                self.place(cell, Place::Note { index });
            }
        }
    }

    /// Finish `making`: the row is made where it holds every column its table needs, with its
    /// notes, findings and places. Else it is not: its findings keep their words nowhere, and
    /// each column it lacks is a finding.
    fn finish(&mut self, making: Making) {
        let Making {
            mut row,
            notes,
            mut findings,
            placed,
        } = making;
        let lacks: Vec<&str> = row
            .collection
            .columns
            .iter()
            .filter(|column| {
                !column.null
                    && column.name != "row"
                    && !matches!(column.ty, Ty::Set(_))
                    && !row.values.contains_key(column.name)
            })
            .map(|column| column.name)
            .collect();
        if !lacks.is_empty() {
            for finding in &mut findings {
                finding.kept = Kept::Not;
                finding.row = None;
            }
            self.out.findings.extend(findings);
            for column in lacks {
                self.found(row.file, row.line, column, Rule::NeedsAValue);
            }
            self.unmade.insert((row.collection.name, row.key));
            return;
        }
        self.add_notes(notes);
        self.out.findings.extend(findings);
        for (cell, place) in placed {
            self.place(cell, place);
        }
        row.members.retain(|_, members| !members.is_empty());
        self.out.rows.push(row);
    }

    /// Begin the row of `collection` that `line` of `file` names by its column `id`: keyed as
    /// that name makes it, with the name as its `id`. `None` where the line names no row or one
    /// an earlier line began, each a finding.
    fn named(
        &mut self,
        (file, line): (&'static str, &Line),
        id: &str,
        collection: &'static Collection,
    ) -> Option<Making> {
        let name = line.get(id);
        if name.is_empty() || name.trim() != name {
            self.found(file, line.n, id, Rule::NeedsAValue);
            return None;
        }
        let held = match self.value(collection, "id", name) {
            Ok(held) => held,
            Err(rule) => {
                self.found(file, line.n, id, rule);
                return None;
            }
        };
        let key = mapped::key(name);
        if !self.fresh(file, line.n, collection, &key) {
            return None;
        }
        let mut making = Making::new(collection, key, file, line.n);
        making.row.values.insert("id", held);
        making.place(line, id, "id");
        Some(making)
    }
}

/// The rule a principal's `said` text breaks: it is the tracker's prose of at most 256 bytes.
fn said_rule(text: &str) -> Rule {
    match guard::prose(text, values::MAX_SAID) {
        Err(guard::Refusal::Length) => Rule::TooLong,
        Err(guard::Refusal::CredentialShape | guard::Refusal::SecretWord) => Rule::SecretShaped,
        _ => Rule::Refused,
    }
}

/// Read every TSV: its header against the columns the import reads, and each line's cells.
fn read_files(files: &BTreeMap<String, String>, out: &mut Import) -> BTreeMap<&'static str, Tsv> {
    let finding = |file: &str, line, column: &str, rule| Finding {
        file: file.to_owned(),
        line,
        row: None,
        column: column.to_owned(),
        rule,
        members: 1,
        kept: Kept::Not,
    };
    for name in files.keys() {
        if !FILES.iter().any(|(file, _)| file == name) {
            out.findings.push(finding(name, 0, "", Rule::UnknownFile));
        }
    }
    let mut tsvs = BTreeMap::new();
    for &(name, columns) in FILES {
        let Some(text) = files.get(name) else {
            out.findings.push(finding(name, 0, "", Rule::MissingFile));
            continue;
        };
        let mut lines = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .enumerate()
            .filter(|(_, line)| !line.is_empty());
        let head: Vec<&str> = lines
            .next()
            .map(|(_, line)| line.split('\t').collect())
            .unwrap_or_default();
        for column in columns {
            if !head.contains(column) {
                out.findings
                    .push(finding(name, 1, column, Rule::MissingColumn));
            }
        }
        let mut read = Vec::new();
        for (index, line) in lines {
            let n = index + 1;
            let cells: Vec<&str> = line.split('\t').collect();
            if cells.len() != head.len() {
                out.findings.push(finding(name, n, "", Rule::WrongWidth));
            }
            let cells = head
                .iter()
                .zip(&cells)
                .map(|(column, cell)| ((*column).to_owned(), (*cell).to_owned()))
                .collect();
            read.push(Line { n, cells });
        }
        out.read.insert(name.to_owned(), read.len());
        tsvs.insert(name, Tsv { name, lines: read });
    }
    tsvs
}

/// Import the lab tracker's TSVs (`files`, by name, as `goals/data` holds them at `commit`)
/// into `lab/tracker`'s rows, samples and notes, each note written at `at_ms`.
///
/// # Errors
/// `commit` is no commit.
pub fn import(commit: &str, at_ms: u64, files: &BTreeMap<String, String>) -> Result<Import> {
    values::commit(commit).map_err(Error::new)?;
    // A row the import cannot make is named by no other row: each pass leaves out the keys of
    // the rows an earlier one could not make, so a reference to one is a finding of its own,
    // until a pass makes every row it keys. Each pass leaves out one row more, so it ends.
    let mut unmade = BTreeSet::new();
    loop {
        let (out, more) = pass(commit, at_ms, files, &unmade);
        if more.is_subset(&unmade) {
            return Ok(out);
        }
        unmade.extend(more);
    }
}

/// One pass of [`import`], with the rows `unmade` names held by no table, and the rows this
/// pass could not make.
fn pass(
    commit: &str,
    at_ms: u64,
    files: &BTreeMap<String, String>,
    unmade: &BTreeSet<(&'static str, String)>,
) -> (Import, BTreeSet<(&'static str, String)>) {
    let mut out = Import {
        commit: commit.to_owned(),
        ..Import::default()
    };
    let tsvs = read_files(files, &mut out);
    let empty = Tsv {
        name: "",
        lines: Vec::new(),
    };
    let tsv = |name: &str| tsvs.get(name).unwrap_or(&empty);
    let mut importer = Importer {
        commit: commit.to_owned(),
        at_ms,
        keys: keys(&tsv, unmade),
        seen: BTreeSet::new(),
        unmade: BTreeSet::new(),
        out,
    };

    // A repository is a row of its own name wherever a package, a lane or a ruling names it
    // by a word a key takes: the TSVs keep no file of repositories.
    for (name, (file, line)) in repositories(&tsv) {
        let mut making = Making::new(&REPO, mapped::key(&name), file, line);
        making.row.values.insert("id", Value::String(name));
        importer.seen.insert((REPO.name, making.row.key.clone()));
        importer.finish(making);
    }
    for line in &tsv("groups.tsv").lines {
        let Some(mut making) = importer.named(("groups.tsv", line), "id", &GROUP) else {
            continue;
        };
        for column in ["horizon", "name", "summary", "scoreboard"] {
            importer.scalar(&mut making, line, column, column);
        }
        importer.unheld(&mut making, line, "coordinator", Rule::NoColumn);
        importer.unheld(&mut making, line, "value", Rule::Computed);
        importer.unheld(&mut making, line, "of", Rule::Computed);
        importer.finish(making);
    }
    for line in &tsv("goals.tsv").lines {
        let Some(mut making) = importer.named(("goals.tsv", line), "id", &GOAL) else {
            continue;
        };
        for column in ["group", "title", "detail", "metric", "target_by"] {
            importer.scalar(&mut making, line, column, column);
        }
        // A baseline or a target is a number in its metric's unit where it is one, else the
        // lab tracker's prose.
        for (tsv, number, prose) in [
            ("baseline", "baseline", "baseline_text"),
            ("target", "target", "target_text"),
        ] {
            let column = if values::decimal(line.get(tsv)).is_ok() {
                number
            } else {
                prose
            };
            importer.scalar(&mut making, line, tsv, column);
        }
        importer.unheld(&mut making, line, "state", Rule::Computed);
        importer.finish(making);
    }
    samples(&mut importer, tsv("metrics.tsv"));
    for line in &tsv("lanes.tsv").lines {
        let Some(mut making) = importer.named(("lanes.tsv", line), "id", &LANE) else {
            continue;
        };
        for (tsv, column) in [
            ("name", "name"),
            ("prefix", "prefix"),
            ("bottleneck", "bottleneck"),
            ("order", "rank"),
        ] {
            importer.scalar(&mut making, line, tsv, column);
        }
        importer.set(&mut making, line, ("repos", "repos"), split_names);
        importer.set(&mut making, line, ("goals", "goals"), split_names);
        importer.finish(making);
    }
    let mut packages = Vec::new();
    for line in &tsv("packages.tsv").lines {
        let Some(mut making) = importer.named(("packages.tsv", line), "id", &PACKAGE) else {
            continue;
        };
        for (tsv, column) in [
            ("lane", "lane"),
            ("order", "rank"),
            ("goal", "goal"),
            ("title", "title"),
            ("outcome", "outcome"),
            ("size", "size"),
        ] {
            importer.scalar(&mut making, line, tsv, column);
        }
        importer.set(&mut making, line, ("repo", "repos"), split_names);
        importer.set(&mut making, line, ("owns", "owns"), split_paths);
        importer.set(&mut making, line, ("depends", "depends"), split_ids);
        exit(&importer, &mut making, line);
        state(&importer, &mut making, line);
        claim(&importer, &mut making, line);
        importer.unheld(&mut making, line, "done_on", Rule::NoColumn);
        importer.unheld(&mut making, line, "evidence", Rule::NoColumn);
        packages.push((making, line.get("depends")));
    }
    acyclic(&importer, &mut packages);
    for (making, _) in packages {
        importer.finish(making);
    }
    for line in &tsv("decisions.tsv").lines {
        let Some(mut making) = importer.named(("decisions.tsv", line), "id", &DECISION) else {
            continue;
        };
        for (tsv, column) in [
            ("lane", "lane"),
            ("decision", "decision"),
            ("date", "decided_on"),
            ("reason", "reason"),
        ] {
            importer.scalar(&mut making, line, tsv, column);
        }
        said(&importer, &mut making, line, "decided_by");
        state(&importer, &mut making, line);
        importer.finish(making);
    }
    for line in &tsv("rulings.tsv").lines {
        // A ruling is keyed by its day and its words.
        let (date, quote) = (line.get("date"), line.get("quote"));
        let Some(on) = day(date).filter(|_| !quote.is_empty()) else {
            let (column, rule) = match (date.is_empty(), quote.is_empty()) {
                (false, false) => ("date", Rule::NotADay),
                (true, _) => ("date", Rule::NeedsAValue),
                (false, true) => ("quote", Rule::NeedsAValue),
            };
            importer.found("rulings.tsv", line.n, column, rule);
            continue;
        };
        let key = mapped::ruling_key(on, quote);
        if !importer.fresh("rulings.tsv", line.n, &RULING, &key) {
            continue;
        }
        let mut making = Making::new(&RULING, key, "rulings.tsv", line.n);
        for column in ["date", "about", "quote", "source"] {
            importer.scalar(&mut making, line, column, column);
        }
        importer.set(&mut making, line, ("repo", "repos"), split_names);
        making
            .row
            .values
            .insert("grade", Value::String("carried".to_owned()));
        importer.finish(making);
    }
    for line in &tsv("stops.tsv").lines {
        // A stop is keyed by its lane and its words.
        let (lane, item) = (line.get("lane"), line.get("item"));
        if lane.is_empty() || item.is_empty() {
            let column = if lane.is_empty() { "lane" } else { "item" };
            importer.found("stops.tsv", line.n, column, Rule::NeedsAValue);
            continue;
        }
        let key = mapped::stop_key(&mapped::key(lane), item);
        if !importer.fresh("stops.tsv", line.n, &STOP, &key) {
            continue;
        }
        let mut making = Making::new(&STOP, key, "stops.tsv", line.n);
        importer.scalar(&mut making, line, "lane", "lane");
        importer.scalar(&mut making, line, "item", "item");
        importer.finish(making);
    }
    // Competitions, projects and the plan: a column each.
    for (file, id, collection, columns) in [
        (
            "competitions.tsv",
            "id",
            &COMPETITION,
            &[
                "name", "kind", "edition", "tracks", "catalog", "scoring", "limits", "engine",
                "next", "today", "state", "url",
            ][..],
        ),
        (
            "projects.tsv",
            "project",
            &PROJECT,
            &["size", "real", "gap", "score"][..],
        ),
        ("plan.tsv", "key", &PLAN, &["value"][..]),
    ] {
        for line in &tsv(file).lines {
            let Some(mut making) = importer.named((file, line), id, collection) else {
                continue;
            };
            for column in columns {
                importer.scalar(&mut making, line, column, column);
            }
            importer.finish(making);
        }
    }
    unknown_columns(&mut importer, &tsvs);
    unplaced(&mut importer, &tsvs);
    let mut out = importer.out;
    out.findings.sort();
    (out, importer.unmade)
}

/// Each non-empty cell of a column the import reads that has no place and no finding: a cell of
/// a line whose row is not made.
fn unplaced(importer: &mut Importer, tsvs: &BTreeMap<&'static str, Tsv>) {
    let named: BTreeSet<At> = importer
        .out
        .findings
        .iter()
        .map(|f| (f.file.clone(), f.line, f.column.clone()))
        .collect();
    for &(name, columns) in FILES {
        let Some(tsv) = tsvs.get(name) else {
            continue;
        };
        for line in &tsv.lines {
            for column in columns {
                let at = (name.to_owned(), line.n, (*column).to_owned());
                if !line.get(column).is_empty()
                    && !importer.out.placed.contains_key(&at)
                    && !named.contains(&at)
                {
                    importer.found(name, line.n, column, Rule::RowNotMade);
                }
            }
        }
    }
}

/// The keys of the rows every table will hold, so a reference to a row a later file makes is
/// held: each named row's, each repository's ([`repositories`]) and each metric's, a goal's
/// `<goal>.<metric>` that a sample names.
fn keys<'t>(
    tsv: &impl Fn(&str) -> &'t Tsv,
    unmade: &BTreeSet<(&'static str, String)>,
) -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut keys: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    for (file, column, collection) in [
        ("groups.tsv", "id", &GROUP),
        ("goals.tsv", "id", &GOAL),
        ("lanes.tsv", "id", &LANE),
        ("packages.tsv", "id", &PACKAGE),
        ("decisions.tsv", "id", &DECISION),
        ("competitions.tsv", "id", &COMPETITION),
        ("projects.tsv", "project", &PROJECT),
        ("plan.tsv", "key", &PLAN),
    ] {
        let held = keys.entry(collection.name).or_default();
        for line in &tsv(file).lines {
            let id = line.get(column);
            if !id.is_empty() && id.trim() == id {
                held.insert(mapped::key(id));
            }
        }
    }
    keys.insert(
        REPO.name,
        repositories(tsv)
            .keys()
            .map(|name| mapped::key(name))
            .collect(),
    );
    let leave_out = |keys: &mut BTreeMap<&'static str, BTreeSet<String>>| {
        for (collection, key) in unmade {
            if let Some(held) = keys.get_mut(collection) {
                held.remove(key);
            }
        }
    };
    leave_out(&mut keys);
    let goals = keys.get(GOAL.name).cloned().unwrap_or_default();
    keys.insert(
        METRIC.name,
        tsv("metrics.tsv")
            .lines
            .iter()
            .filter(|line| goals.contains(&mapped::key(line.get("goal"))))
            .map(|line| mapped::key(&format!("{}.{}", line.get("goal"), line.get("metric"))))
            .collect(),
    );
    leave_out(&mut keys);
    keys
}

/// Each repository a lane, a package or a ruling names by a word a key takes, with the first
/// line that names it.
fn repositories<'t>(tsv: &impl Fn(&str) -> &'t Tsv) -> BTreeMap<String, (&'static str, usize)> {
    let mut repos = BTreeMap::new();
    for (file, column) in [
        ("lanes.tsv", "repos"),
        ("packages.tsv", "repo"),
        ("rulings.tsv", "repo"),
    ] {
        let file = tsv(file);
        for line in &file.lines {
            for name in split_names(line.get(column)) {
                if pk_grammar(name) && guard::word(name).is_ok() {
                    repos.entry(name.to_owned()).or_insert((file.name, line.n));
                }
            }
        }
    }
    repos
}

/// A package's exit test from `exit_command` and `exit_expect`: the command, and what the lab
/// tracker said passing means, in its own words.
fn exit(importer: &Importer, making: &mut Making, line: &Line) {
    let (command, expect) = (line.get("exit_command"), line.get("exit_expect"));
    let member = |name: &str| {
        super::tables::EXIT
            .iter()
            .find(|member| member.name == name)
            .map_or(Ty::Bool, |member| member.ty)
    };
    let check = |name: &str, text: &str| {
        let ty = member(name);
        cell::rule(&ty, &Value::String(text.to_owned()), false).map_err(|rule| classify(&ty, rule))
    };
    let incomplete = |importer: &Importer, making: &mut Making| {
        if !expect.is_empty() {
            importer.noted(
                making,
                line.n,
                "exit_expect",
                (Rule::Incomplete, 1),
                &[("exit_expect", expect)],
            );
        }
    };
    if command.is_empty() {
        incomplete(importer, making);
        return;
    }
    if let Err(rule) = check("command", command) {
        importer.noted(
            making,
            line.n,
            "exit_command",
            (rule, 1),
            &[("exit_command", command)],
        );
        incomplete(importer, making);
        return;
    }
    let mut exit = Map::new();
    exit.insert("command".into(), Value::String(command.to_owned()));
    making.place(line, "exit_command", "exit");
    if !expect.is_empty() {
        match check("expect", expect) {
            Ok(()) => {
                exit.insert("expect".into(), Value::String(expect.to_owned()));
                making.place(line, "exit_expect", "exit");
            }
            Err(rule) => importer.noted(
                making,
                line.n,
                "exit_expect",
                (rule, 1),
                &[("exit_expect", expect)],
            ),
        }
    }
    making.row.values.insert("exit", Value::Object(exit));
}

/// A package's or a decision's state: one of the tracker's words as itself, and another word
/// of the lab tracker's as the nearest ([`mapped`]), what it said kept in `state_said`. A word
/// the tracker cannot map holds the row until someone gives it a state (a package `blocked`, a
/// decision `open`), the word kept likewise.
fn state(importer: &Importer, making: &mut Making, line: &Line) {
    let said = line.get("state");
    if said.is_empty() {
        return;
    }
    let (word, holds) = if making.row.collection.name == PACKAGE.name {
        (mapped::package_state(said), "blocked")
    } else {
        (mapped::decision_state(said), "open")
    };
    let (state, rule) = match word {
        Some(state) => (state.state, state.said.map(|_| Rule::StateMapped)),
        None => (holds, Some(Rule::NotAWord)),
    };
    if let Some(rule) = rule {
        let x = Value::String(said.to_owned());
        let kept = making
            .row
            .collection
            .column("state_said")
            .is_some_and(|column| cell::rule(&column.ty, &x, true).is_ok());
        if kept {
            making.row.values.insert("state_said", x);
            making.kept("state", rule);
        } else {
            // The row still states the word its state maps to; the note keeps what was said.
            importer.noted(making, line.n, "state", (rule, 1), &[("state", said)]);
        }
    }
    making
        .row
        .values
        .insert("state", Value::String(state.to_owned()));
    making.place(line, "state", "state");
}

/// Who decided, from the lab tracker's free text in `tsv`: a principal that says only that, a
/// finding until a principal aterm stated replaces it.
fn said(importer: &Importer, making: &mut Making, line: &Line, tsv: &'static str) {
    let text = line.get(tsv);
    if text.is_empty() {
        return;
    }
    let principal = mapped::decided_by(text).to_value();
    if values::principal(&principal).is_err() {
        importer.noted(making, line.n, tsv, (said_rule(text), 1), &[(tsv, text)]);
        return;
    }
    making.row.values.insert(tsv, principal);
    making.kept(tsv, Rule::SaidInWords);
    making.place(line, tsv, tsv);
}

/// A package's claim from `claimed_by` and `claimed_on`: who held it, in words, and the day.
fn claim(importer: &Importer, making: &mut Making, line: &Line) {
    let (by, on) = (line.get("claimed_by"), line.get("claimed_on"));
    let principal = mapped::decided_by(by).to_value();
    if by.is_empty() || values::principal(&principal).is_err() {
        if !by.is_empty() {
            importer.noted(
                making,
                line.n,
                "claimed_by",
                (said_rule(by), 1),
                &[("claimed_by", by)],
            );
        }
        if !on.is_empty() {
            importer.noted(
                making,
                line.n,
                "claimed_on",
                (Rule::Incomplete, 1),
                &[("claimed_on", on)],
            );
        }
        return;
    }
    let mut claim = Map::new();
    claim.insert("by".into(), principal);
    making.kept("claimed_by", Rule::SaidInWords);
    making.place(line, "claimed_by", "claim");
    if !on.is_empty() {
        if let Some(on) = day(on) {
            claim.insert("on".into(), Value::from(on));
            making.place(line, "claimed_on", "claim");
        } else {
            importer.noted(
                making,
                line.n,
                "claimed_on",
                (Rule::NotADay, 1),
                &[("claimed_on", on)],
            );
        }
    }
    making.row.values.insert("claim", Value::Object(claim));
}

/// Drop each dependency that would close a circle of packages, in the order the TSV holds them:
/// the guard refuses a write that closes one ([`super::checks::guard`]).
fn acyclic(importer: &Importer, packages: &mut [(Making, &str)]) {
    fn reaches(graph: &BTreeMap<String, BTreeSet<String>>, from: &str, to: &str) -> bool {
        let mut seen = BTreeSet::new();
        let mut walk = vec![from];
        while let Some(at) = walk.pop() {
            if at == to {
                return true;
            }
            if seen.insert(at) {
                walk.extend(graph.get(at).into_iter().flatten().map(String::as_str));
            }
        }
        false
    }
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (making, text) in packages.iter_mut() {
        let key = making.row.key.clone();
        let Some(depends) = making.row.members.get_mut("depends") else {
            continue;
        };
        let mut cut = 0;
        for dependency in depends.clone() {
            if reaches(&graph, &dependency, &key) {
                cut += 1;
                depends.remove(&dependency);
            } else {
                graph.entry(key.clone()).or_default().insert(dependency);
            }
        }
        if depends.is_empty() {
            making
                .placed
                .retain(|((_, _, column), _)| column != "depends");
        }
        if cut > 0 {
            let line = making.row.line;
            importer.noted(
                making,
                line,
                "depends",
                (Rule::DependsCycle, cut),
                &[("depends", text)],
            );
        }
    }
}

/// The columns of `metrics.tsv`, a sample each line.
const SAMPLE: [&str; 8] = [
    "date", "goal", "metric", "value", "unit", "machine", "source", "note",
];

/// `metrics.tsv`: each sample a `measure` entry of the metric `<goal>.<metric>`, which the first
/// sample of it makes, with that sample's unit. What a sample says that the log has no column
/// for (its machine in words, a note, a unit unlike its metric's) is a note about the metric. A
/// sample of a group's number, which the tracker computes, is a note about the group; one whose
/// value or day the log cannot hold is a note about its metric; each such note quotes every
/// cell of the sample.
fn samples(importer: &mut Importer, tsv: &Tsv) {
    let mut metrics: BTreeMap<String, usize> = BTreeMap::new();
    let mut made: Vec<Making> = Vec::new();
    for line in &tsv.lines {
        let (goal, name) = (line.get("goal"), line.get("metric"));
        let cells: Vec<(&str, &str)> = SAMPLE
            .into_iter()
            .map(|column| (column, line.get(column)))
            .filter(|(_, text)| !text.is_empty())
            .collect();
        if importer.holds(GOAL.name, goal).is_none() {
            let Some(group) = importer.holds(GROUP.name, goal) else {
                importer.found(tsv.name, line.n, "goal", Rule::NamesNoRow);
                continue;
            };
            let mut making = Making::new(&GROUP, group, tsv.name, line.n);
            importer.noted(&mut making, line.n, "goal", (Rule::Computed, 1), &cells);
            importer.add_notes(making.notes);
            importer.out.findings.extend(making.findings);
            continue;
        }
        let id = format!("{goal}.{name}");
        let key = mapped::key(&id);
        let at = *metrics.entry(key.clone()).or_insert_with(|| {
            let mut making = Making::new(&METRIC, key.clone(), tsv.name, line.n);
            if let Ok(id) = importer.value(&METRIC, "id", &id) {
                making.row.values.insert("id", id);
            }
            if let Ok(goal) = importer.value(&METRIC, "goal", goal) {
                making.row.values.insert("goal", goal);
            }
            let unit = line.get("unit");
            if let Some(unit) = (!unit.is_empty())
                .then(|| importer.value(&METRIC, "unit", unit).ok())
                .flatten()
            {
                making.row.values.insert("unit", unit);
            }
            made.push(making);
            made.len() - 1
        });
        let making = &mut made[at];
        let value = line.get("value");
        let on = day(line.get("date"));
        let (Ok(()), Some(on)) = (values::decimal(value), on) else {
            let (column, rule) = if on.is_none() {
                ("date", Rule::NotADay)
            } else {
                ("value", Rule::NotANumber)
            };
            importer.noted(making, line.n, column, (rule, 1), &cells);
            continue;
        };
        let mut entry = Map::new();
        entry.insert("metric".into(), Value::String(key.clone()));
        entry.insert("value".into(), Value::String(value.to_owned()));
        entry.insert("at".into(), Value::from(on * MS_DAY));
        let mut placed = vec!["date", "goal", "metric", "value"];
        let source = line.get("source");
        if !source.is_empty() {
            let x = Value::String(source.to_owned());
            let ty = MEASURE
                .column("source")
                .map_or(Ty::Bool, |column| column.ty);
            match cell::rule(&ty, &x, true) {
                Ok(()) => {
                    entry.insert("source".into(), x);
                    placed.push("source");
                }
                Err(rule) => importer.noted(
                    making,
                    line.n,
                    "source",
                    (classify(&ty, rule), 1),
                    &[("source", source)],
                ),
            }
        }
        if owned::log_row(&MEASURE, &entry, 1).is_err() {
            importer.noted(making, line.n, "value", (Rule::Refused, 1), &cells);
            continue;
        }
        let index = importer.out.measures.len();
        importer.out.measures.push(entry);
        for column in placed {
            importer.place(
                (tsv.name.to_owned(), line.n, column.to_owned()),
                Place::Measure { index, column },
            );
        }
        let unit = line.get("unit");
        if !unit.is_empty() {
            if making.row.values.get("unit").and_then(Value::as_str) == Some(unit) {
                making.place(line, "unit", "unit");
            } else {
                importer.noted(
                    making,
                    line.n,
                    "unit",
                    (Rule::UnitDiffers, 1),
                    &[("unit", unit)],
                );
            }
        }
        for (column, rule) in [("machine", Rule::MachineInWords), ("note", Rule::NoColumn)] {
            let text = line.get(column);
            if !text.is_empty() {
                importer.noted(making, line.n, column, (rule, 1), &[(column, text)]);
            }
        }
    }
    for making in made {
        importer.finish(making);
    }
}

/// Each non-empty cell of a header column the import does not read: quoted in a note about its
/// line's row where the import made one.
fn unknown_columns(importer: &mut Importer, tsvs: &BTreeMap<&'static str, Tsv>) {
    // A line's own row: never a repository, which many lines name, nor a metric, which its
    // first sample makes; a sample's row is its metric's, or its group's.
    let rows: BTreeMap<(&str, usize), (&'static str, String)> = importer
        .out
        .rows
        .iter()
        .filter(|row| row.collection.name != REPO.name && row.collection.name != METRIC.name)
        .map(|row| ((row.file, row.line), (row.collection.name, row.key.clone())))
        .collect();
    let made: BTreeSet<(&str, String)> = importer
        .out
        .rows
        .iter()
        .map(|row| (row.collection.name, row.key.clone()))
        .collect();
    let sample = |line: &Line| {
        let (goal, metric) = (line.get("goal"), line.get("metric"));
        [
            (METRIC.name, mapped::key(&format!("{goal}.{metric}"))),
            (GROUP.name, mapped::key(goal)),
        ]
        .into_iter()
        .find(|row| made.contains(row))
    };
    for &(name, columns) in FILES {
        let Some(tsv) = tsvs.get(name) else {
            continue;
        };
        for line in &tsv.lines {
            for (column, text) in &line.cells {
                if columns.contains(&column.as_str()) || text.is_empty() {
                    continue;
                }
                let row = if name == "metrics.tsv" {
                    sample(line)
                } else {
                    rows.get(&(name, line.n)).cloned()
                };
                let Some((on, key)) = row else {
                    importer.found(name, line.n, column, Rule::UnknownColumn);
                    continue;
                };
                let note = importer.note(
                    (on, &key),
                    (name, line.n),
                    Rule::UnknownColumn,
                    &[(column, text)],
                );
                let kept = if let Some(note) = note {
                    importer.add_notes(vec![(
                        note,
                        vec![(name.to_owned(), line.n, column.clone())],
                    )]);
                    Kept::Note
                } else {
                    Kept::Not
                };
                importer.out.findings.push(Finding {
                    file: name.to_owned(),
                    line: line.n,
                    row: Some(key),
                    column: column.clone(),
                    rule: Rule::UnknownColumn,
                    members: 1,
                    kept,
                });
            }
        }
    }
}
