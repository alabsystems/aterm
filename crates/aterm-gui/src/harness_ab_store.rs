// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seven-day per-process A/B book store ([`DAYS`], [`read_others`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::harness_ab::{AbArmTotals, AbDay, AbEnds, AbRing, BUCKETS, DAYS, Holds};

/// The write order owned by one live book.
#[derive(Debug, Default)]
pub(crate) struct AbWrites {
    next: AtomicU64,
    installed: Mutex<Option<u64>>,
}

/// One copied ring and its capture order, tied to the book that captured it.
#[derive(Debug)]
pub(crate) struct AbSnapshot {
    writes: Arc<AbWrites>,
    generation: u64,
    ring: (AbRing, i64),
}

impl AbWrites {
    /// Capture a ring on the App thread, before it is handed to a worker.
    pub(crate) fn snapshot(self: &Arc<Self>, ring: (AbRing, i64)) -> std::io::Result<AbSnapshot> {
        let generation = self
            .next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| std::io::Error::other("A/B book snapshot generation exhausted"))?;
        Ok(AbSnapshot {
            writes: Arc::clone(self),
            generation,
            ring,
        })
    }
}

impl AbSnapshot {
    /// Write the copied ring through the same path used by a page reading and the exit seam.
    pub(crate) fn write_if_newer(
        &self,
        harness_state: &Path,
        name: &str,
        now_unix_ms: i64,
    ) -> std::io::Result<()> {
        let mut installed = self
            .writes
            .installed
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if installed.is_some_and(|generation| self.generation <= generation) {
            return Ok(());
        }
        write_own(harness_state, name, &self.ring.0, self.ring.1, now_unix_ms)?;
        *installed = Some(self.generation);
        Ok(())
    }
}

/// The files' schema ([`EndsFile`]).
pub(crate) const SCHEMA: u32 = 1;

/// The book's directory under the harness's state directory.
pub(crate) const DIR: &str = "ab";

/// The files' mode: the owner's alone.
const MODE: u32 = 0o600;

/// The oldest day kept, as days before today: today and the seven before.
const KEPT_BEFORE: i64 = DAYS as i64 - 1;

/// One process's book on disk.
#[derive(Debug, Serialize, Deserialize)]
struct BookFile {
    /// [`SCHEMA`].
    schema: u32,
    /// The writing process's pid.
    pid: u32,
    /// When it was written, unix ms.
    written_unix_ms: i64,
    /// Its days, oldest first.
    days: Vec<DayFile>,
}

/// One day on disk.
#[derive(Debug, Serialize, Deserialize)]
struct DayFile {
    /// The local day number.
    day: i64,
    /// When its first counted stretch began, unix ms.
    first_unix_ms: i64,
    /// Per arm (Cruise, Land, Off, Unsupervised).
    arms: [[u64; 6]; 4],
    /// Per compared arm (Cruise, Land, Off) and kind (box, question): the hold histogram.
    holds: [[[u32; BUCKETS]; 2]; 3],
    /// Count of completed tasks.
    #[serde(default, skip_serializing_if = "EndsFile::is_none")]
    ends: EndsFile,
}

/// One day's ends on disk ([`AbEnds`]).
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct EndsFile {
    /// Land's landings.
    #[serde(default)]
    landed: u32,
    /// The time from Land to landed: the histogram.
    #[serde(default)]
    landing: [u32; BUCKETS],
}

impl EndsFile {
    const fn of(ends: &AbEnds) -> Self {
        Self {
            landed: ends.landed,
            landing: ends.landing,
        }
    }

    const fn ends(&self) -> AbEnds {
        AbEnds {
            landed: self.landed,
            landing: self.landing,
        }
    }

    fn is_none(&self) -> bool {
        *self == Self::default()
    }
}

/// Only the schema, read first: a newer file's other fields may not parse.
#[derive(Deserialize)]
struct SchemaOnly {
    schema: u32,
}

impl DayFile {
    fn of(day: &AbDay) -> Self {
        let arm = |t: AbArmTotals| {
            [
                t.busy_ms,
                t.at_box_ms,
                t.asked_ms,
                t.at_prompt_ms,
                t.landed_ms,
                t.pausing_ms,
            ]
        };
        Self {
            day: day.day,
            first_unix_ms: day.first_unix_ms,
            arms: day.arms.map(arm),
            holds: day.holds,
            ends: EndsFile::of(&day.ends),
        }
    }

    fn day(&self) -> AbDay {
        let arm = |[
            busy_ms,
            at_box_ms,
            asked_ms,
            at_prompt_ms,
            landed_ms,
            pausing_ms,
        ]: [u64; 6]| {
            AbArmTotals {
                busy_ms,
                at_box_ms,
                asked_ms,
                at_prompt_ms,
                landed_ms,
                pausing_ms,
            }
        };
        let holds: Holds = self.holds;
        AbDay {
            day: self.day,
            first_unix_ms: self.first_unix_ms,
            arms: self.arms.map(arm),
            holds,
            ends: self.ends.ends(),
        }
    }
}

/// This process's start on the wall, unix ms.
#[must_use]
pub(crate) fn process_start_unix_ms() -> i64 {
    static START: OnceLock<i64> = OnceLock::new();
    *START.get_or_init(|| {
        let since = i64::try_from(crate::turn_ledger::now_ms()).unwrap_or(0);
        crate::harness_screen::unix_ms_now().saturating_sub(since)
    })
}

/// This process's file's name: `<process start unix ms>-<pid>.json`.
#[must_use]
pub(crate) fn own_name() -> String {
    format!("{}-{}.json", process_start_unix_ms(), std::process::id())
}

/// The book's directory under `harness_state`.
#[must_use]
pub(crate) fn dir_of(harness_state: &Path) -> PathBuf {
    harness_state.join(DIR)
}

/// Whether `day` falls within the last [`DAYS`] days of `today`.
const fn kept(day: i64, today: i64) -> bool {
    day >= today.saturating_sub(KEPT_BEFORE)
}

/// WRITE THIS PROCESS'S BOOK, whole ([`DAYS`]).
///
/// # Errors
/// Returns an error if creating the directory, writing the file, or removing a stale book fails.
pub(crate) fn write_own(
    harness_state: &Path,
    name: &str,
    ring: &AbRing,
    today: i64,
    now_unix_ms: i64,
) -> std::io::Result<()> {
    let path = dir_of(harness_state).join(name);
    let mut days: Vec<DayFile> = ring
        .iter()
        .flatten()
        .filter(|day| kept(day.day, today))
        .map(DayFile::of)
        .collect();
    if days.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    days.sort_by_key(|day| day.day);
    let file = BookFile {
        schema: SCHEMA,
        pid: std::process::id(),
        written_unix_ms: now_unix_ms,
        days,
    };
    let text = aterm_json::to_string(&file).map_err(std::io::Error::other)?;
    aterm_agent::harness::upgrade_models::write_atomic_mode(&path, &text, Some(MODE))
}

/// Whether `name` is a book's file: `<digits>-<digits>.json`.
fn is_book_name(name: &str) -> bool {
    name.strip_suffix(".json")
        .and_then(|stem| stem.split_once('-'))
        .is_some_and(|(start, pid)| {
            !start.is_empty()
                && !pid.is_empty()
                && start.bytes().all(|b| b.is_ascii_digit())
                && pid.bytes().all(|b| b.is_ascii_digit())
        })
}

/// Read every other process's book under `harness_state/ab/`, skipping `own` ([`DAYS`]).
#[must_use]
pub(crate) fn read_others(harness_state: &Path, own: &str, today: i64) -> Vec<AbDay> {
    let dir = dir_of(harness_state);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut summed: Vec<AbDay> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == own || !is_book_name(name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(SchemaOnly { schema }) = aterm_json::from_str::<SchemaOnly>(&text) else {
            continue;
        };
        if schema != SCHEMA {
            continue;
        }
        let Ok(book) = aterm_json::from_str::<BookFile>(&text) else {
            continue;
        };
        if !book.days.iter().any(|day| kept(day.day, today)) {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        for day in book.days.iter().filter(|day| kept(day.day, today)) {
            let day = day.day();
            match summed.iter_mut().find(|sum| sum.day == day.day) {
                Some(sum) => sum.plus(&day),
                None => summed.push(day),
            }
        }
    }
    summed.sort_by_key(|day| day.day);
    summed
}

/// Persist this process's book and merge other processes' books ([`read_others`]).
#[must_use]
pub(crate) fn exchange(
    harness_state: &Path,
    own: &str,
    ring: Option<&AbSnapshot>,
    now_unix_ms: i64,
    offset_at: fn(i64) -> Option<i64>,
) -> Vec<AbDay> {
    let today = ring.map_or_else(
        || {
            let unix_s = now_unix_ms.div_euclid(1000);
            unix_s
                .saturating_add(offset_at(unix_s).unwrap_or(0))
                .div_euclid(86_400)
        },
        |snapshot| snapshot.ring.1,
    );
    if let Some(snapshot) = ring
        && let Err(error) = snapshot.write_if_newer(harness_state, own, now_unix_ms)
    {
        aterm_log::warn!("harness A/B book: not written at a reading: {error}");
    }
    read_others(harness_state, own, today)
}

#[cfg(test)]
#[path = "harness_ab_store_tests.rs"]
mod tests;
