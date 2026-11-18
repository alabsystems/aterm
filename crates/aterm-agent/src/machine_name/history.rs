// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's name History and Undo, read from its own `lab/machines` registry row (tracker
//! package FL-171, decision FL-144-REGISTRY): every write to the row's `label` and `nickname`,
//! each with the Mac that wrote it, its kind and its time as the registry keeps them
//! ([`aterm_db::att::Handle::writes`]), so nothing here records an author, a time or an earlier
//! name. `aterm name history` and Settings ▸ This Mac list it in one spelling ([`History::lines`]),
//! and Undo takes back the row's newest rename by a new owner write of the values it replaced
//! ([`undo_of`]): an undo is a rename like any other, so undoing it again redoes the rename.
//!
//! The read only reads: it opens the registry under a bound the caller gives, gives no held
//! write way ([`super::registry`]'s `here` does that) and makes no name, and a name kept anywhere
//! but this account's live state (a test's scratch folder, a private state root) has no row and
//! so no History. An undo writes only while the same read, made again under the naming locks,
//! gives the undo it was offered ([`fence`]), so it never takes back a rename its caller did not
//! show.

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_db::att::{EntryKind, Held};
use aterm_db::file::Renamed;
use aterm_types::machine_name::{Nickname, Number};

use super::{Cause, NameTo, Refused, UndoTo};

/// What History says where no registry row is this Mac's: `aterm name history`'s words, which
/// Settings ▸ This Mac shows as a sentence.
pub const NO_ROW: &str = "no lab registry row is this Mac's, and its History is the row's";

/// What History says where this Mac's row holds no write to its number or nickname.
pub const NO_HISTORY: &str = "the lab registry keeps no change to this Mac's number or nickname";

/// The most writes Settings ▸ This Mac lists ([`History::lines`]); `aterm name history` lists
/// every one.
pub const PAGE_ROWS: usize = 16;

/// How long [`fence`]'s read of the registry, made under the naming locks, may take.
pub const FENCE_WAIT: Duration = Duration::from_secs(10);

/// The cells History reads, in the order it reads them.
const CELLS: [Renamed; 2] = [Renamed::Label, Renamed::Nickname];

/// This Mac's History: its registry row, what a rename of this Mac is still sending to it, and
/// every write to the row's number and nickname, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    /// The row's key; `None` where no registry row is this Mac's.
    pub row: Option<String>,
    /// The cells a write of this Mac's own has not reached yet (queued, or published and not yet
    /// in the snapshot, or held back).
    pub pending: Vec<Renamed>,
    /// Every write to the row's `label` and `nickname`, newest first.
    pub writes: Vec<NameWrite>,
}

/// One write to this Mac's row's number or nickname, as the registry keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameWrite {
    /// The operation that wrote it.
    pub op: [u8; 32],
    /// The signed writer clock claim (ms).
    pub at_ms: u64,
    /// The Mac that wrote it, as the registry's history names it.
    pub mac: String,
    /// What kind of write it is: an owner's (`explicit`, a revert included), aterm's own or an
    /// import's.
    pub kind: EntryKind,
    /// The operation it reverted, where it is a revert.
    pub reverts: Option<[u8; 32]>,
    /// A registry roll carried it from an earlier generation, whose history keeps no value it
    /// replaced.
    pub carried: bool,
    /// Each of the two cells it wrote, `label` first.
    pub cells: Vec<NameCell>,
}

/// What one write left in one cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameCell {
    pub cell: Renamed,
    /// The text it wrote; `None` where it cleared the cell.
    pub value: Option<String>,
    /// Its value stands now.
    pub stands: bool,
    /// The values it replaced, each once (`None`: the cell was unset); none for the cell's
    /// first write and for a carried write.
    pub replaced: Vec<Option<String>>,
    /// The writes of the cell it had not seen, which wrote at once with it.
    pub unseen: Vec<[u8; 32]>,
}

impl NameWrite {
    /// What it left in `cell`, where it wrote it.
    #[must_use]
    pub fn cell(&self, cell: Renamed) -> Option<&NameCell> {
        self.cells.iter().find(|written| written.cell == cell)
    }
}

/// The text one stored cell value stands for: its typed value's string, `None` where it cleared
/// the cell.
fn one(value: &aterm_json::Value) -> Option<String> {
    let x = aterm_db::typed::cell::CellValue::decode(value)
        .map_or_else(|_| value.clone(), |cell| cell.x);
    match x {
        aterm_json::Value::Null => None,
        aterm_json::Value::String(text) => Some(text),
        other => Some(aterm_json::to_string(&other).unwrap_or_default()),
    }
}

/// The text a held value stands for, `None` where it cleared the cell or holds no value: a
/// register of several candidates, which a revert restored, says each, ` / ` between them, as a
/// line says values written at once.
fn text(held: &Held) -> Option<String> {
    stored(held.value.as_ref()?)
}

/// The text a stored value stands for ([`text`]).
fn stored(value: &aterm_json::Value) -> Option<String> {
    let Some(candidates) = aterm_db::typed::cell::carried(value) else {
        return one(value);
    };
    let said: Vec<String> = candidates
        .iter()
        .map(|(_, candidate)| said(one(candidate).as_deref()).to_owned())
        .collect();
    Some(said.join(" / "))
}

/// `writes`, the registry's writes to `keys` (the row's `label` and `nickname` registers, in
/// [`CELLS`]' order), as History lists them, newest first.
fn of(writes: Vec<aterm_db::att::Written>, keys: &[String]) -> Vec<NameWrite> {
    let mut out: Vec<NameWrite> = writes
        .into_iter()
        .map(|written| {
            let cells = CELLS
                .iter()
                .zip(keys)
                .filter_map(|(cell, key)| {
                    let wrote = written.cells.iter().find(|wrote| wrote.key == *key)?;
                    let held = wrote.wrote.first()?;
                    let mut replaced: Vec<Option<String>> = Vec::new();
                    for value in wrote.replaced.iter().map(text) {
                        if !replaced.contains(&value) {
                            replaced.push(value);
                        }
                    }
                    Some(NameCell {
                        cell: *cell,
                        value: text(held),
                        stands: wrote.wrote.iter().any(|held| held.stands),
                        replaced,
                        unseen: wrote.unseen.iter().map(|held| held.operation).collect(),
                    })
                })
                .collect();
            NameWrite {
                op: written.operation,
                at_ms: written.authored_ms,
                mac: written.mac.name(),
                kind: written.kind,
                reverts: written.reverts,
                carried: !matches!(written.source, aterm_db::att::Source::Log),
                cells,
            }
        })
        .collect();
    out.reverse();
    out
}

/// This Mac's History, as the registry kept in this account's live vault home holds it for the
/// name kept in `state` (`None`'s row for a name kept anywhere else): the row the last
/// snapshot here marks this Mac's ([`aterm_db::file::here_published`]), the cells a write of this
/// Mac's is still changing, and every write to its `label` and `nickname`, read in one
/// observation that gives up at `read_by` ([`aterm_db::att::Handle::writes`]). It only reads.
///
/// # Errors
/// The registry cannot be read by `read_by`: busy past it, or unreadable.
pub fn read(state: &Path, read_by: Instant) -> Result<History, String> {
    #[cfg(any(test, debug_assertions))]
    if let Some(seam) = HISTORY.with(|seam| seam.borrow().clone()) {
        return seam;
    }
    let Some(paths) = super::registry::live(state) else {
        return Ok(History::default());
    };
    let Some(here) = aterm_db::file::here_published(&paths) else {
        return Ok(History::default());
    };
    let row = here.pk().to_owned();
    let pending = CELLS
        .into_iter()
        .filter(|cell| here.pending(*cell))
        .collect();
    let keys = CELLS
        .iter()
        .map(|cell| {
            aterm_db::typed::cell::CellKey {
                pk: &row,
                col: cell.column(),
                elem: None,
            }
            .encode()
        })
        .collect::<Result<Vec<String>, _>>()
        .map_err(|why| why.to_string())?;
    let asked: Vec<&str> = keys.iter().map(String::as_str).collect();
    let handle = aterm_db::att::Handle::at_until(paths, &aterm_db::att::REGISTRY, read_by)
        .map_err(|why| why.to_string())?;
    let writes = handle
        .writes(aterm_db::alab::registry::MACHINE.name, &asked, read_by)
        .map_err(|why| why.to_string())?;
    Ok(History {
        row: Some(row),
        pending,
        writes: of(writes, &keys),
    })
}

/// The first eight hex digits of `op`, as `aterm file history` names an operation.
fn short(op: &[u8; 32]) -> String {
    let mut hex = aterm_codec::hex::encode_lower(op);
    hex.truncate(8);
    hex
}

/// A cell's value as a line says it.
fn said(value: Option<&str>) -> &str {
    value.unwrap_or("(unset)")
}

impl NameWrite {
    /// The write as `aterm name history` and Settings ▸ This Mac list it, one line with no line
    /// end, such as `2026-11-13T10:22:01Z  m8  explicit  label m7 -> m86, nickname Otter ->
    /// Marigold`: the time in UTC to the second, the Mac that wrote it, its kind (with the
    /// operation a revert reverted), and each cell it wrote as what it replaced and what it
    /// wrote, `(replaced)` after a value that no longer stands and `(concurrent with <op>)`
    /// after one written at once with another.
    #[must_use]
    pub fn line(&self) -> String {
        let at = aterm_types::rfc3339::format_rfc3339(self.at_ms / 1_000);
        let mut kind = self.kind.word().to_owned();
        if let Some(reverts) = &self.reverts {
            kind.push_str(" revert of ");
            kind.push_str(&short(reverts));
        }
        if self.carried {
            kind.push_str(" (carried)");
        }
        let cells: Vec<String> = self
            .cells
            .iter()
            .map(|cell| {
                let mut said_cell = cell.cell.column().to_owned();
                if !cell.replaced.is_empty() {
                    let replaced: Vec<&str> = cell
                        .replaced
                        .iter()
                        .map(|value| said(value.as_deref()))
                        .collect();
                    said_cell.push(' ');
                    said_cell.push_str(&replaced.join(" / "));
                    said_cell.push_str(" ->");
                }
                said_cell.push(' ');
                said_cell.push_str(said(cell.value.as_deref()));
                if !cell.stands {
                    said_cell.push_str(" (replaced)");
                }
                for op in &cell.unseen {
                    said_cell.push_str(" (concurrent with ");
                    said_cell.push_str(&short(op));
                    said_cell.push(')');
                }
                said_cell
            })
            .collect();
        format!("{at}  {}  {kind}  {}", self.mac, cells.join(", "))
    }
}

impl History {
    /// History as `aterm name history` (every write, `limit` `None`) and Settings ▸ This Mac
    /// (the newest [`PAGE_ROWS`]) list it: a line for each cell a write of this Mac's is still
    /// sending, then one per write, newest first ([`NameWrite::line`]); or the one line
    /// [`NO_ROW`] or [`NO_HISTORY`] says.
    #[must_use]
    pub fn lines(&self, limit: Option<usize>) -> Vec<String> {
        if self.row.is_none() {
            return vec![NO_ROW.to_owned()];
        }
        let mut lines: Vec<String> = self
            .pending
            .iter()
            .map(|cell| {
                format!(
                    "pending  a write of this Mac's {} is on its way to the row",
                    cell.column()
                )
            })
            .collect();
        if self.writes.is_empty() {
            lines.push(NO_HISTORY.to_owned());
        }
        let writes = self.writes.iter().take(limit.unwrap_or(usize::MAX));
        lines.extend(writes.map(NameWrite::line));
        lines
    }

    /// History as `aterm name history --json` prints it, one object: `row` (its key or
    /// `null`), `pending` (each column) and `writes`, newest first, each with `op`, `at`,
    /// `at_ms`, `mac`, `kind`, `reverts`, `carried` and `cells`, each cell with `column`,
    /// `value`, `stands`, `replaced` and `unseen`.
    #[must_use]
    pub fn json(&self) -> aterm_json::Value {
        use aterm_json::{Map, Value};
        let text = |value: &Option<String>| value.clone().map_or(Value::Null, Value::String);
        let hex = |op: &[u8; 32]| Value::String(aterm_codec::hex::encode_lower(op));
        let writes = self
            .writes
            .iter()
            .map(|write| {
                let cells = write
                    .cells
                    .iter()
                    .map(|cell| {
                        let mut object = Map::new();
                        object.insert(
                            "column".to_owned(),
                            Value::String(cell.cell.column().to_owned()),
                        );
                        object.insert("value".to_owned(), text(&cell.value));
                        object.insert("stands".to_owned(), Value::Bool(cell.stands));
                        object.insert(
                            "replaced".to_owned(),
                            Value::Array(cell.replaced.iter().map(text).collect()),
                        );
                        object.insert(
                            "unseen".to_owned(),
                            Value::Array(cell.unseen.iter().map(hex).collect()),
                        );
                        Value::Object(object)
                    })
                    .collect();
                let mut object = Map::new();
                object.insert("op".to_owned(), hex(&write.op));
                object.insert(
                    "at".to_owned(),
                    Value::String(aterm_types::rfc3339::format_rfc3339(write.at_ms / 1_000)),
                );
                object.insert("at_ms".to_owned(), Value::from(write.at_ms));
                object.insert("mac".to_owned(), Value::String(write.mac.clone()));
                object.insert(
                    "kind".to_owned(),
                    Value::String(write.kind.word().to_owned()),
                );
                object.insert(
                    "reverts".to_owned(),
                    write.reverts.as_ref().map_or(Value::Null, hex),
                );
                object.insert("carried".to_owned(), Value::Bool(write.carried));
                object.insert("cells".to_owned(), Value::Array(cells));
                Value::Object(object)
            })
            .collect();
        let mut object = Map::new();
        object.insert(
            "row".to_owned(),
            self.row.clone().map_or(Value::Null, Value::String),
        );
        object.insert(
            "pending".to_owned(),
            Value::Array(
                self.pending
                    .iter()
                    .map(|cell| Value::String(cell.column().to_owned()))
                    .collect(),
            ),
        );
        object.insert("writes".to_owned(), Value::Array(writes));
        Value::Object(object)
    }
}

/// What Undo takes back and writes ([`undo_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undo {
    /// The write it takes back: the row's newest rename.
    pub op: [u8; 32],
    /// The row's name as the undo read it: each cell's one standing value, `label` first, as the
    /// row's text (`""` for an unset or cleared cell). The undo writes only while the row reads
    /// the same ([`fence`]).
    pub shown: Vec<(Renamed, String)>,
    /// What the undo writes: each cell's value the write replaced, `label` first.
    pub cells: Vec<(Renamed, String)>,
    /// The name the undo leaves this Mac.
    pub to: UndoTo,
}

/// The value `cell` of the write `op` replaced, as the undo writes it: `None` for a nickname the
/// undo leaves out (it replaced none, or an empty one).
///
/// # Errors
/// It replaced two values at once ([`Cause::Nothing`]); a number it replaced was none
/// ([`Cause::Unnumbered`]), or is no number, or a nickname is no nickname ([`Cause::Nothing`]).
fn replaced(op: &[u8; 32], cell: &NameCell) -> Result<Option<String>, Refused> {
    let column = cell.cell.column();
    let value = match cell.replaced.as_slice() {
        [] => None,
        [one] => one.clone(),
        several => {
            let values: Vec<&str> = several.iter().map(|value| said(value.as_deref())).collect();
            return Err(Refused::new(
                Cause::Nothing,
                format!(
                    "the last rename settled this Mac's {column} over {} written at once, so \
                     there is no one value to give back: set the one you want",
                    values.join(" and ")
                ),
            ));
        }
    };
    let revert = || {
        format!(
            "`aterm file revert {}` restores the row as it was",
            short(op)
        )
    };
    match (cell.cell, value) {
        (Renamed::Label, None) => Err(Refused::new(
            Cause::Unnumbered,
            "the last rename gave this Mac its first number, and an undo leaves no Mac \
             unnumbered"
                .to_owned(),
        )),
        (Renamed::Label, Some(text)) => {
            let num = Number::parse(&text)
                .ok()
                .filter(|num| num.to_string() == text);
            match num {
                Some(_) => Ok(Some(text)),
                None => Err(Refused::new(
                    Cause::Nothing,
                    format!(
                        "the label the last rename replaced, {text}, is no number this Mac can \
                         take; {}",
                        revert()
                    ),
                )),
            }
        }
        (Renamed::Nickname, None) => Ok(None),
        (Renamed::Nickname, Some(text)) if text.is_empty() => Ok(None),
        (Renamed::Nickname, Some(text)) => match Nickname::parse(&text) {
            Ok(_) => Ok(Some(text)),
            Err(why) => Err(Refused::new(
                Cause::Nothing,
                format!(
                    "the nickname the last rename replaced, {text}, is no nickname ({why}); {}",
                    revert()
                ),
            )),
        },
    }
}

/// What Undo takes back over `history` and writes: the row's newest rename, the newest write an
/// owner made (`explicit`, an undo and an `aterm file revert` included; never one aterm or an
/// import made), from any Mac, and each value it replaced in the cells it wrote, as one new owner
/// write ([`super::undo_last`]). An undo is such a rename, so undoing it again redoes the rename.
/// A cell whose value the rename left as it was is no part of the undo, nor is a nickname the
/// rename gave where the row had none. The row's whole name as read, both cells, is kept beside
/// it ([`Undo::shown`]), so a later write of either, an automatic one included, stops the undo.
/// Who holds the number now is [`super::judge`]'s to say, as for any rename.
///
/// # Errors
/// No registry row is this Mac's, the row holds no rename, its newest predates the registry's
/// roll, it settled two values at once, a value it replaced is no number or nickname, or it
/// left both cells as they were ([`Cause::Nothing`]); a write of this Mac's own is still on its
/// way to the row, or a value the rename left no longer stands, a later write or one made at
/// once with it standing instead, or the row holds two values of a cell the rename did not write
/// ([`Cause::Superseded`]); the rename gave this Mac its first number ([`Cause::Unnumbered`]).
pub fn undo_of(history: &History) -> Result<Undo, Refused> {
    if history.row.is_none() {
        return Err(Refused::new(Cause::Nothing, NO_ROW.to_owned()));
    }
    if let Some(cell) = history.pending.first() {
        return Err(Refused::new(
            Cause::Superseded,
            format!(
                "a write of this Mac's {} is still on its way to its lab registry row: undo once \
                 it lands",
                cell.column()
            ),
        ));
    }
    let Some(rename) = history
        .writes
        .iter()
        .find(|write| write.kind == EntryKind::Explicit)
    else {
        return Err(Refused::new(
            Cause::Nothing,
            "nothing to undo: this Mac's lab registry row holds no rename of its number or \
             nickname"
                .to_owned(),
        ));
    };
    if rename.carried {
        return Err(Refused::new(
            Cause::Nothing,
            "nothing to undo: the last rename predates the lab registry's roll, whose history \
             keeps no value it replaced"
                .to_owned(),
        ));
    }
    let mut shown = Vec::with_capacity(CELLS.len());
    for column in CELLS {
        let standing: Vec<Option<&str>> = history
            .writes
            .iter()
            .filter_map(|write| write.cell(column))
            .filter(|other| other.stands)
            .map(|other| other.value.as_deref())
            .fold(Vec::new(), |mut all, value| {
                if !all.contains(&value) {
                    all.push(value);
                }
                all
            });
        let Some(cell) = rename.cell(column) else {
            // A cell the rename did not write: the row's one value there, or none.
            match standing.as_slice() {
                [] => shown.push((column, String::new())),
                [one] => shown.push((column, one.unwrap_or_default().to_owned())),
                several => {
                    return Err(Refused::new(
                        Cause::Superseded,
                        format!(
                            "this Mac's lab registry row holds two {}s at once ({}): set the one \
                             you want",
                            column.column(),
                            several
                                .iter()
                                .map(|value| said(*value))
                                .collect::<Vec<_>>()
                                .join(" and ")
                        ),
                    ));
                }
            }
            continue;
        };
        let left = cell.value.clone();
        if standing != [left.as_deref()] {
            let column = cell.cell.column();
            let why = if standing.len() > 1 {
                format!(
                    "this Mac's lab registry row holds two {column}s at once ({}), so the last \
                     rename no longer stands as it left the row",
                    standing
                        .iter()
                        .map(|value| said(*value))
                        .collect::<Vec<_>>()
                        .join(" and ")
                )
            } else {
                format!(
                    "this Mac's {column} changed in its lab registry row after the last rename, \
                     which left it {}: there is no rename of its to undo",
                    said(left.as_deref())
                )
            };
            return Err(Refused::new(Cause::Superseded, why));
        }
        shown.push((cell.cell, left.unwrap_or_default()));
    }
    let mut cells = Vec::with_capacity(rename.cells.len());
    for cell in &rename.cells {
        if let Some(back) = replaced(&rename.op, cell)?
            && Some(&back) != cell.value.as_ref()
        {
            cells.push((cell.cell, back));
        }
    }
    let value = |wanted: Renamed| {
        cells
            .iter()
            .find(|(cell, _)| *cell == wanted)
            .map(|(_, value)| value.as_str())
    };
    let nick = value(Renamed::Nickname).and_then(|word| Nickname::parse(word).ok());
    let to = match value(Renamed::Label).and_then(|text| Number::parse(text).ok()) {
        Some(num) => UndoTo::Name(NameTo { num, nick }),
        None => match nick {
            Some(nick) => UndoTo::Nickname(nick),
            None => {
                return Err(Refused::new(
                    Cause::Nothing,
                    "nothing to undo: the last rename left this Mac's number and nickname as \
                     they were"
                        .to_owned(),
                ));
            }
        },
    };
    Ok(Undo {
        op: rename.op,
        shown,
        cells,
        to,
    })
}

/// Whether `seen`, the undo a caller read and showed, is still the one this Mac's History gives:
/// the same read ([`read`], bounded by [`FENCE_WAIT`]), made again while the undo holds the
/// naming locks, gives exactly `seen` ([`undo_of`]): the same newest rename, the row's name
/// standing as it was, and the same values to give back. Both reads are of the registry's store,
/// so a write that reached it from another Mac counts at once, before any snapshot shows it.
///
/// # Errors
/// Why not, in words that end a sentence: the registry was not read in time, no undo is there
/// now, or another rename or value took its place.
pub(super) fn fence(state: &Path, seen: &Undo) -> Result<(), String> {
    let fresh = read(state, Instant::now() + FENCE_WAIT)
        .map_err(|why| format!("its lab registry row was not read again ({why})"))?;
    let now = undo_of(&fresh).map_err(|refused| refused.why)?;
    if now.op != seen.op {
        return Err(format!(
            "its lab registry row's newest rename is now {}, not {}",
            short(&now.op),
            short(&seen.op)
        ));
    }
    if let Some(((cell, was), (_, is))) = seen
        .shown
        .iter()
        .zip(&now.shown)
        .find(|(was, is)| was != is)
    {
        return Err(format!(
            "its lab registry row's {} is now {}, not {}",
            cell.column(),
            said(Some(is).filter(|is| !is.is_empty()).map(String::as_str)),
            said(Some(was).filter(|was| !was.is_empty()).map(String::as_str))
        ));
    }
    if now != *seen {
        return Err("the values its newest rename replaced read otherwise now".to_owned());
    }
    Ok(())
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// What a test's [`read`] answers, while [`with_history`] runs.
    static HISTORY: std::cell::RefCell<Option<Result<History, String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `body` while [`read`] answers `history` on this thread: the seam this crate's tests and
/// aterm-gui's give an undo's [`fence`] and a page's read.
#[cfg(any(test, debug_assertions))]
pub fn with_history<T>(history: Result<History, String>, body: impl FnOnce() -> T) -> T {
    let held = HISTORY.with(|seam| seam.replace(Some(history)));
    let out = body();
    HISTORY.with(|seam| seam.replace(held));
    out
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
