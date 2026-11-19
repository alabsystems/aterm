// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent retention for stage measurements and weaker compiler-shaping observations.
//! Neither file is the successful-wrapper admission-unit book.

use super::{Key, Learned, Records, Row, Wider};
use std::io;
use std::path::{Path, PathBuf};

/// Read-only evidence for stage estimates and shaping floors. A composed view cannot be passed
/// to a compiler-unit admission reader, which requires a raw, separately proven `Records` book.
pub trait ShapingSource: std::fmt::Debug + Send + Sync {
    /// Retained stage and, for a shaping view, weaker unit rows.
    fn rows(&self) -> &[Row];
    /// The exact or wider measurement in this checkout.
    fn learned(&self, key: &Key) -> Option<Learned>;
    /// The exact work measured in another checkout.
    fn elsewhere(&self, key: &Key) -> Option<Learned>;
    /// A wider named measurement that is not outweighed by narrower work or `floor`.
    fn wider_elsewhere(&self, key: &Key, floor: u64) -> Option<Wider>;
    /// The heaviest shaping unit inside this test compile.
    fn unit_floor(&self, key: &Key) -> u64;
    /// The heaviest narrower stage measurement.
    fn narrower(&self, key: &Key) -> u64;
}

impl ShapingSource for Records {
    fn rows(&self) -> &[Row] {
        self.rows()
    }
    fn learned(&self, key: &Key) -> Option<Learned> {
        self.learned(key)
    }
    fn elsewhere(&self, key: &Key) -> Option<Learned> {
        self.elsewhere(key)
    }
    fn wider_elsewhere(&self, key: &Key, floor: u64) -> Option<Wider> {
        self.wider_elsewhere(key, floor)
    }
    fn unit_floor(&self, key: &Key) -> u64 {
        self.unit_floor(key)
    }
    fn narrower(&self, key: &Key) -> u64 {
        self.narrower(key)
    }
}

/// The two independently bounded files composed only for estimates and shaping. There is no
/// conversion or dereference to `Records`, and nothing here can publish admission evidence.
#[derive(Clone, Debug, Default)]
pub struct ShapingHistory {
    records: Records,
}

impl ShapingSource for ShapingHistory {
    fn rows(&self) -> &[Row] {
        self.records.rows()
    }
    fn learned(&self, key: &Key) -> Option<Learned> {
        self.records.learned(key)
    }
    fn elsewhere(&self, key: &Key) -> Option<Learned> {
        self.records.elsewhere(key)
    }
    fn wider_elsewhere(&self, key: &Key, floor: u64) -> Option<Wider> {
        self.records.wider_elsewhere(key, floor)
    }
    fn unit_floor(&self, key: &Key) -> u64 {
        self.records.unit_floor(key)
    }
    fn narrower(&self, key: &Key) -> u64 {
        self.records.narrower(key)
    }
}

impl ShapingHistory {
    /// Read aggregate `stage_path` and its weaker shaping sibling. Missing new files may read
    /// the old mixed book; an existing malformed or unreadable new file never falls back.
    #[must_use]
    pub fn load(stage_path: &Path) -> Self {
        let mut stages = Domain::Stages.load(stage_path);
        let units = Domain::Units.load(stage_path);
        // Each source already has its own 512-row bound. Do not push through the mixed-book
        // eviction policy while composing a view: that would discard aggregate history again.
        stages.rows.extend(units.rows);
        Self { records: stages }
    }
}

#[derive(Clone, Copy)]
enum Domain {
    Stages,
    Units,
}

impl Domain {
    fn accepts(self, key: &Key) -> bool {
        (key.stage == super::UNIT_STAGE) == matches!(self, Self::Units)
    }

    fn path(self, stage_path: &Path) -> PathBuf {
        match self {
            Self::Stages => stage_path.to_owned(),
            Self::Units => stage_path.with_file_name("shaping-unit-memory-v1.json"),
        }
    }

    fn legacy(stage_path: &Path) -> Option<PathBuf> {
        // Only the canonical versioned namespace has a legacy predecessor. Arbitrary caller
        // books (including fixtures) must not import a neighbouring unrelated file.
        match stage_path.file_name()?.to_str()? {
            "stage-memory-v3.json" => Some(stage_path.with_file_name("stage-memory.json")),
            // An explicitly selected old book can still supply a read-only shaping view.
            "stage-memory.json" => Some(stage_path.to_owned()),
            _ => None,
        }
    }

    fn partition(self, records: Records) -> Records {
        let mut kept = Records::default();
        for row in records.rows.into_iter().filter(|r| self.accepts(&r.key)) {
            for peak in row.peaks {
                // Keep the original key, bytes and timestamp; do not freshen imported evidence.
                kept.push(row.key.clone(), peak.bytes, peak.at_ms);
            }
        }
        kept
    }

    fn read(self, stage_path: &Path, text: Option<&str>) -> io::Result<Records> {
        let legacy = match text {
            Some(_) => None,
            None => Self::legacy(stage_path)
                .map(|p| super::read_text(&p, super::BOOK_MAX_BYTES))
                .transpose()?
                .flatten(),
        };
        let records = match text.or(legacy.as_deref()) {
            Some(text) => Records::decode_checked(text).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid verify memory book; retained bytes stand",
                )
            })?,
            None => Records::default(),
        };
        Ok(self.partition(records))
    }

    fn load(self, stage_path: &Path) -> Records {
        super::read_text(&self.path(stage_path), super::BOOK_MAX_BYTES)
            .and_then(|text| self.read(stage_path, text.as_deref()))
            .unwrap_or_default()
    }

    fn keep(
        self,
        stage_path: &Path,
        key: Key,
        bytes: u64,
        at_ms: u64,
        light: bool,
    ) -> io::Result<bool> {
        if !self.accepts(&key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "wrong verify history domain",
            ));
        }
        let mut result = Ok(());
        let mut kept = false;
        super::update_text(
            &self.path(stage_path),
            super::BOOK_MAX_BYTES,
            ".stage-memory-",
            |text| {
                let mut records = match self.read(stage_path, text) {
                    Ok(records) => records,
                    Err(error) => {
                        result = Err(error);
                        return None;
                    }
                };
                if light && records.light(&key, bytes, at_ms) {
                    // A light run must still isolate imported heavy evidence from legacy writers.
                    return text.is_none().then(|| records.encode());
                }
                records.push(key, bytes, at_ms);
                kept = true;
                Some(records.encode())
            },
        )?;
        result?;
        Ok(kept)
    }
}

/// Aggregate-only history, with its own retention budget and read-only legacy import.
#[must_use]
pub fn stage_records(path: &Path) -> Records {
    Domain::Stages.load(path)
}

/// Read the weaker shaping book only for a selection that can have a unit floor.
pub(super) fn unit_floor(path: &Path, key: &Key) -> u64 {
    if key.unit_floor_selection().is_none() {
        return 0;
    }
    Domain::Units.load(path).unit_floor(key)
}

/// Keep a measured stage or planner refusal in the versioned aggregate book. Weak unit keys
/// are refused; the existing light-run guard and durable, bounded publication are retained.
pub fn keep_stage(
    path: &Path,
    key: Key,
    bytes: u64,
    at_ms: u64,
    unless_light: bool,
) -> io::Result<bool> {
    Domain::Stages.keep(path, key, bytes, at_ms, unless_light)
}

/// Keep a weaker meter-derived shaping unit, never in the aggregate or admission-unit book.
pub fn keep_shaping_unit(path: &Path, key: Key, bytes: u64, at_ms: u64) -> io::Result<bool> {
    Domain::Units.keep(path, key, bytes, at_ms, true)
}

#[cfg(test)]
#[path = "verify_history_tests.rs"]
mod tests;
