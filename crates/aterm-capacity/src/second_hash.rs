// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHY A UNIT GOT A SECOND HASH, FOR THE WHOLE MACHINE (`docs/DESIGN-disk-custody-2026-11-12.md`
//! §3.1, "Find what mints the variants"; DC-03b). Each time a verify compile builds a unit of a
//! workspace package beside another unit of its package and kind in the same profile, the run
//! reads both fingerprints and names the first input that moved
//! (`atpkg::build_cache::stale::SecondHashes`); it keeps one row of it here, beside the stages'
//! memory ([`crate::book::VERIFY_RECORD`]) and the disk footprints of every checkout's runs. A
//! pair of units is one row however often a run builds either of them again, whichever of the
//! two it built ([`Row::same`]), and the record keeps the newest [`ROWS`], so it stays bounded. [`SecondHashes::listing`] counts the
//! rows by cause: what `aterm pkg machine cache second-hashes` prints.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use aterm_json::{Map, Value};

/// The record, under this user's aterm state directory (`aterm_types::dirs::account_state_dir`).
pub const RECORD: &str = "verify/second-hashes.json";

/// The rows the record keeps: past this, the oldest goes.
pub const ROWS: usize = 4096;

/// The record's schema.
pub const SCHEMA: u64 = 1;

/// The most bytes [`SecondHashes::load`] reads and [`SecondHashes::keep`] writes: [`ROWS`] rows of
/// a few hundred bytes each.
const FILE_MAX: u64 = 8 << 20;

/// The longest detail a row keeps, in characters.
const DETAIL_MAX: usize = 200;

/// ONE SECOND HASH: a unit a compile built beside another of its package and kind, and the input
/// that moved between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// When the run that built it recorded it, milliseconds since the epoch.
    pub at_ms: u64,
    /// The unit's package.
    pub package: String,
    /// The unit's kind (`atpkg::build_cache::stale::Unit::kind`).
    pub kind: String,
    /// The unit built.
    pub hash: String,
    /// The other unit of its package and kind it was compared with.
    pub other: String,
    /// The package whose own input moved: the unit's, or the dependency's its `deps` led to.
    pub cause: String,
    /// The input that moved (`features`, `deps`, `folder`, …).
    pub input: String,
    /// How it moved, in a few words.
    pub detail: String,
}

impl Row {
    /// Whether `other` is this row's pair of units again: the same package, kind and two hashes,
    /// whichever of the two was built (a run that builds `B` beside `A` and a later one that
    /// builds `A` beside `B` found one second hash).
    #[must_use]
    pub fn same(&self, other: &Row) -> bool {
        (&self.package, &self.kind) == (&other.package, &other.kind)
            && ((&self.hash, &self.other) == (&other.hash, &other.other)
                || (&self.hash, &self.other) == (&other.other, &other.hash))
    }

    fn value(&self) -> Value {
        let mut o = Map::new();
        o.insert("at_ms".to_owned(), Value::from(self.at_ms));
        for (key, text) in [
            ("package", &self.package),
            ("kind", &self.kind),
            ("hash", &self.hash),
            ("other", &self.other),
            ("cause", &self.cause),
            ("input", &self.input),
            ("detail", &self.detail),
        ] {
            o.insert(key.to_owned(), Value::from(text.as_str()));
        }
        Value::Object(o)
    }

    fn of_value(v: &Value) -> Option<Row> {
        let text = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Row {
            at_ms: v.get("at_ms")?.as_u64()?,
            package: text("package")?,
            kind: text("kind")?,
            hash: text("hash")?,
            other: text("other")?,
            cause: text("cause")?,
            input: text("input")?,
            detail: text("detail")?,
        })
    }
}

/// THE MACHINE'S RECORD OF SECOND HASHES ([`RECORD`]): its rows, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecondHashes {
    rows: Vec<Row>,
}

impl SecondHashes {
    /// The rows, oldest first.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Keep `row` unless it is a row's pair of units again ([`Row::same`]), its detail cut to
    /// [`DETAIL_MAX`] characters; the oldest go past [`ROWS`]. Whether it was kept.
    pub fn push(&mut self, mut row: Row) -> bool {
        if self.rows.iter().any(|kept| kept.same(&row)) {
            return false;
        }
        if let Some((at, _)) = row.detail.char_indices().nth(DETAIL_MAX) {
            row.detail.truncate(at);
        }
        let at = self.rows.partition_point(|kept| kept.at_ms <= row.at_ms);
        self.rows.insert(at, row);
        let over = self.rows.len().saturating_sub(ROWS);
        self.rows.drain(..over);
        true
    }

    /// The rows by cause: how many of each input that moved (`features`, `deps`, …), and of each
    /// package and input.
    #[must_use]
    pub fn by_cause(&self) -> (BTreeMap<String, usize>, BTreeMap<(String, String), usize>) {
        let mut inputs = BTreeMap::new();
        let mut causes = BTreeMap::new();
        for row in &self.rows {
            *inputs.entry(row.input.clone()).or_insert(0) += 1;
            *causes
                .entry((row.cause.clone(), row.input.clone()))
                .or_insert(0) += 1;
        }
        (inputs, causes)
    }

    /// What `aterm pkg machine cache second-hashes` prints: the count of second hashes and, by
    /// count, each input that moved, then each package and input.
    #[must_use]
    pub fn listing(&self) -> String {
        let (inputs, causes) = self.by_cause();
        let since = self.rows.first().map_or_else(String::new, |row| {
            format!(" since {} ms after the epoch", row.at_ms)
        });
        let mut out = format!("second hashes: {}{since}\n", self.rows.len());
        let mut inputs: Vec<(String, usize)> = inputs.into_iter().collect();
        inputs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (input, count) in inputs {
            out.push_str(&format!("  {count} {input}\n"));
        }
        let mut causes: Vec<((String, String), usize)> = causes.into_iter().collect();
        causes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for ((package, input), count) in causes {
            out.push_str(&format!("  {count} {package} {input}\n"));
        }
        out
    }

    /// The record as the file holds it, schema [`SCHEMA`].
    #[must_use]
    pub fn encode(&self) -> String {
        let mut file = Map::new();
        file.insert("schema".to_owned(), Value::from(SCHEMA));
        file.insert(
            "rows".to_owned(),
            Value::Array(self.rows.iter().map(Row::value).collect()),
        );
        aterm_json::to_string(&Value::Object(file)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The record `text` holds: none for text that is not [`Self::encode`]'s or of another schema;
    /// a row it cannot read whole is left out.
    #[must_use]
    pub fn decode(text: &str) -> SecondHashes {
        let Ok(file) = aterm_json::from_str::<Value>(text) else {
            return SecondHashes::default();
        };
        if file.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
            return SecondHashes::default();
        }
        let mut out = SecondHashes::default();
        for row in file
            .get("rows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Row::of_value)
        {
            out.push(row);
        }
        out
    }

    /// The record kept at `path`; none where it is absent or unreadable, a file past its bound
    /// among them.
    #[must_use]
    pub fn load(path: &Path) -> SecondHashes {
        crate::book::read_text(path, FILE_MAX)
            .ok()
            .flatten()
            .map(|text| Self::decode(&text))
            .unwrap_or_default()
    }

    /// Keep `rows` in the record at `path`: read, pushed ([`Self::push`]) and written back whole,
    /// holding the record's lock from the read to the write ([`crate::book::update_text`]), so two
    /// runs ending at once each keep theirs. How many were new.
    ///
    /// # Errors
    /// As [`crate::book::update_text`]: an existing record that could not be read is left as it
    /// is.
    pub fn keep(path: &Path, rows: Vec<Row>) -> io::Result<usize> {
        let mut added = 0;
        crate::book::update_text(path, FILE_MAX, ".second-hashes-", |text| {
            let mut record = text.map(Self::decode).unwrap_or_default();
            for row in rows {
                added += usize::from(record.push(row));
            }
            (added > 0).then(|| record.encode())
        })?;
        Ok(added)
    }
}

#[cfg(test)]
#[path = "second_hash_tests.rs"]
mod tests;
