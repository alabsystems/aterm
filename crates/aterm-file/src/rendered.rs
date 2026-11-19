// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A snapshot with what each of its rows renders to kept beside it (package TK-09,
//! docs/DESIGN-lab-tracker-2026-11-14.md §7, finding 16): the line each row takes in the
//! snapshot file, the bytes each of its cells takes in the face, and its face line. A pass that
//! replaces a few rows of the last snapshot ([`Rendered::patch`]) renders those rows alone, and
//! the snapshot's content, its file and its face are put together from the lines kept, so a
//! materializer pass after one record renders the rows that record changed and no other.
//!
//! The lines are private and change only with the rows they render, so they are always the
//! ones [`Snapshot::encode`], [`Snapshot::content`] and [`Snapshot::face`] would render: the
//! bytes are the same whichever way they were made.

use crate::{
    Snapshot,
    snapshot::{Row, column_widths, face_columns, face_line, row_line, row_widths},
};

/// A snapshot and its rows' renderings.
#[derive(Clone, Debug)]
pub struct Rendered {
    snapshot: Snapshot,
    /// Each row's line in the snapshot file, in row order.
    lines: Vec<String>,
    /// The bytes each column takes in the face over every row.
    widths: Vec<u64>,
    /// The columns the face shows ([`face_columns`]).
    face: Vec<usize>,
    /// Each row's face line over [`Self::face`], in row order.
    face_lines: Vec<String>,
    /// [`Snapshot::content`].
    content: [u8; 32],
}

impl Rendered {
    /// `snapshot`, every row rendered.
    #[must_use]
    pub fn of(snapshot: Snapshot) -> Self {
        let lines: Vec<String> = snapshot.rows.iter().map(row_line).collect();
        let widths = column_widths(&snapshot.columns, &snapshot.rows);
        let face = face_columns(&snapshot.columns, &widths, snapshot.rows.len());
        let face_lines = snapshot
            .rows
            .iter()
            .map(|row| face_line(&snapshot.columns, &face, row))
            .collect();
        let content = snapshot.content_of(lines.iter().map(String::as_str));
        Self {
            snapshot,
            lines,
            widths,
            face,
            face_lines,
            content,
        }
    }

    /// The snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The snapshot, its renderings dropped.
    #[must_use]
    pub fn into_snapshot(self) -> Snapshot {
        self.snapshot
    }

    /// [`Snapshot::content`], kept.
    #[must_use]
    pub const fn content(&self) -> [u8; 32] {
        self.content
    }

    /// [`Snapshot::encode`], from the lines kept.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.snapshot
            .encode_of(self.lines.iter().map(String::as_str))
    }

    /// [`Snapshot::face`], from the face lines kept.
    #[must_use]
    pub fn face(&self) -> String {
        self.snapshot
            .face_of(&self.face, self.face_lines.iter().map(String::as_str))
    }

    /// The names of the columns the face shows, in face order, the key last.
    #[must_use]
    pub fn face_names(&self) -> Vec<&str> {
        self.face
            .iter()
            .map(|at| self.snapshot.columns[*at].name.as_str())
            .collect()
    }

    /// Set the revision, which no rendering of a row holds.
    pub const fn set_rev(&mut self, rev: u64) {
        self.snapshot.rev = rev;
    }

    /// This snapshot with each of `rows` in place of the row of its key, or added in key order
    /// where it holds none, each row keyed in `gone` taken out, and the projection's
    /// completeness and time set: only those rows are rendered (each changed row's old cells are
    /// measured once more, to take its widths off the face's). `None` where the rows are not in
    /// key order (a log's or a ring's, which no pass patches): such a snapshot is projected
    /// whole. Where the rows changed move which columns the face shows, every face line is
    /// rendered again.
    #[must_use]
    pub fn patch(
        &self,
        rows: Vec<Row>,
        gone: &[String],
        complete: bool,
        at_ms: u64,
    ) -> Option<Self> {
        if !self
            .snapshot
            .rows
            .windows(2)
            .all(|pair| pair[0].pk < pair[1].pk)
        {
            return None;
        }
        let mut next = self.clone();
        next.snapshot.complete = complete;
        next.snapshot.at_ms = at_ms;
        for row in rows {
            next.put(row);
        }
        for pk in gone {
            next.take(pk);
        }
        let face = face_columns(
            &next.snapshot.columns,
            &next.widths,
            next.snapshot.rows.len(),
        );
        if face != next.face {
            next.face = face;
            next.face_lines = next
                .snapshot
                .rows
                .iter()
                .map(|row| face_line(&next.snapshot.columns, &next.face, row))
                .collect();
        }
        next.content = next
            .snapshot
            .content_of(next.lines.iter().map(String::as_str));
        Some(next)
    }

    /// Put `row` in place of the row of its key, or in key order.
    fn put(&mut self, row: Row) {
        let columns = &self.snapshot.columns;
        let line = row_line(&row);
        let face_line = face_line(columns, &self.face, &row);
        for (total, width) in self.widths.iter_mut().zip(row_widths(columns, &row)) {
            *total += width;
        }
        match self
            .snapshot
            .rows
            .binary_search_by(|held| held.pk.as_str().cmp(&row.pk))
        {
            Ok(at) => {
                let old = std::mem::replace(&mut self.snapshot.rows[at], row);
                for (total, width) in self.widths.iter_mut().zip(row_widths(columns, &old)) {
                    *total -= width;
                }
                self.lines[at] = line;
                self.face_lines[at] = face_line;
            }
            Err(at) => {
                self.snapshot.rows.insert(at, row);
                self.lines.insert(at, line);
                self.face_lines.insert(at, face_line);
            }
        }
    }

    /// Take out the row keyed `pk`, where one is.
    fn take(&mut self, pk: &str) {
        let Ok(at) = self
            .snapshot
            .rows
            .binary_search_by(|held| held.pk.as_str().cmp(pk))
        else {
            return;
        };
        let old = self.snapshot.rows.remove(at);
        for (total, width) in self
            .widths
            .iter_mut()
            .zip(row_widths(&self.snapshot.columns, &old))
        {
            *total -= width;
        }
        self.lines.remove(at);
        self.face_lines.remove(at);
    }
}

#[cfg(test)]
#[path = "rendered_tests.rs"]
mod tests;
