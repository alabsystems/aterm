// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A session's ACTIVE GOAL: the objective its agent was told to keep working toward until it
//! holds, read from where the agent itself keeps it and shown as one line
//! (`docs/DESIGN-harness-goal-2026-10-24.md`).

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

/// The most bytes of a goal aterm keeps ([`shown`]).
pub const GOAL_BYTES: usize = 2048;

/// `text` as aterm shows a goal: one line ([`super::one_line`]: line breaks and controls a
/// space, runs of spaces one, the ends trimmed) of at most [`GOAL_BYTES`]; `None` when nothing
/// is left.
#[must_use]
pub fn shown(text: &str) -> Option<String> {
    let line = super::one_line(text, GOAL_BYTES);
    (!line.is_empty()).then_some(line)
}

/// What Claude Code's transcript says of its `/goal`, folded one row at a time ([`Self::row`]).
///
/// Claude Code writes a `{"type":"attachment","attachment":{"type":"goal_status",…}}` row when
/// `/goal <condition>` sets a goal (`met: false`, `sentinel: true`), again after a compaction
/// (the same sentinel), at each check its Stop hook makes (`met: false` with a `reason`, or
/// `met: true` once the condition holds) and when `/goal clear` drops it (`met: true`,
/// `sentinel: true`); a goal it gave up after an unrecoverable error says `failed`. The goal is
/// active while the newest such row is neither met nor failed, and its text is that row's
/// `condition`: the reading Claude Code makes itself when it resumes a conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeGoal {
    active: Option<String>,
}

impl ClaudeGoal {
    /// Whether `line` may hold a goal row: the cheap look before any parse (the attachment's
    /// type has nothing JSON escapes).
    #[must_use]
    pub fn may_be(line: &str) -> bool {
        line.contains("goal_status")
    }

    /// Fold one transcript row (its [`super::transcript::goal`] projection): whether it was a
    /// goal row of the main conversation.
    pub fn row(&mut self, row: &Map) -> bool {
        if row.get("type").and_then(Value::as_str) != Some("attachment")
            || row.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            return false;
        }
        let Some(attachment) = row.get("attachment").and_then(Value::as_object) else {
            return false;
        };
        if attachment.get("type").and_then(Value::as_str) != Some("goal_status") {
            return false;
        }
        let ended = |key: &str| attachment.get(key).and_then(Value::as_bool) == Some(true);
        self.active = if ended("met") || ended("failed") {
            None
        } else {
            attachment
                .get("condition")
                .and_then(Value::as_str)
                .and_then(shown)
        };
        true
    }

    /// The goal in force after the rows folded so far.
    #[must_use]
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }
}

/// Muse Code's goal store in a session's directory: `goals`, one row per session.
pub const MUSE_GOALS: &str = "goals.db";

/// Which agent's SQLite goal store a [`StoreGoal`] reads.
///
/// Muse Code keeps one row per session whose `objective` is the goal and whose `status` is
/// `active` while the agent pursues it (`paused`, `blocked`, `usage_limited`, `budget_limited`
/// and `complete` are not pursued); clearing the goal deletes the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    /// Muse Code's [`MUSE_GOALS`], keyed by the session id its session directory names.
    Muse,
}

/// The most bytes of an objective one read takes from a store, before [`shown`] cuts it.
const STORE_TEXT_BYTES: usize = 4 * GOAL_BYTES;

/// Maximum byte size of a goal database or its write-ahead log read into memory.
const GOAL_DB_MAX_BYTES: u64 = 64 << 20;

/// Whether `id` is a UUID in its text form (`8-4-4-4-12` hex digits).
#[must_use]
pub fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Truncate `text` to [`STORE_TEXT_BYTES`] on a character boundary and format via [`shown`].
#[must_use]
pub fn objective_of(text: &str) -> Option<String> {
    shown(super::truncate_bytes(text, STORE_TEXT_BYTES))
}

/// What identifies one file's content between two reads.
type FileStamp = (u64, u64, std::time::SystemTime);

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt as _;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    Some((ino, meta.len(), meta.modified().ok()?))
}

/// A database's files: itself and its write-ahead log.
fn store_stamp(db: &Path) -> [Option<FileStamp>; 2] {
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    [file_stamp(db), file_stamp(Path::new(&wal))]
}

/// The active goal an agent's SQLite goal store holds for one thread or session, read again
/// only when the store's files move ([`Self::read`]).
#[derive(Debug, Default)]
pub struct StoreGoal {
    /// What the last read was of: the store, the id and the files' stamps.
    seen: Option<(PathBuf, String, [Option<FileStamp>; 2])>,
    goal: Option<String>,
}

impl StoreGoal {
    /// The goal `store`'s database `db` holds for `id`: read now when `db`, `id` or the files
    /// moved since the last read, else the last read's answer. No database is no goal; a read
    /// that failed keeps the last answer until the files move again.
    pub fn read(&mut self, store: Store, db: &Path, id: &str) -> Option<&str> {
        let stamp = store_stamp(db);
        if stamp[0].is_none() {
            self.seen = None;
            self.goal = None;
            return None;
        }
        let fresh = self
            .seen
            .as_ref()
            .is_some_and(|(path, held, at)| path == db && held == id && *at == stamp);
        if !fresh {
            if self
                .seen
                .as_ref()
                .is_some_and(|(path, held, _)| path != db || held != id)
            {
                self.goal = None;
            }
            match read_store(store, db, id) {
                Ok(goal) => self.goal = goal,
                Err(aterm_uds::foreign_db::Unread::Missing) => self.goal = None,
                Err(_) => {}
            }
            self.seen = Some((db.to_path_buf(), id.to_owned(), stamp));
        }
        self.goal.as_deref()
    }
}

/// One read of `store`'s database `db` for `id`'s active goal.
///
/// # Errors
/// [`aterm_uds::foreign_db::Unread`]: no database, one being written or changed during the read,
/// or a reader that gave no answer (an `id` that is no UUID among them).
pub fn read_store(
    store: Store,
    db: &Path,
    id: &str,
) -> Result<Option<String>, aterm_uds::foreign_db::Unread> {
    if !is_uuid(id) {
        return Err(aterm_uds::foreign_db::Unread::Failed);
    }
    let (table, key) = match store {
        Store::Muse => ("goals", "session_id"),
    };
    aterm_uds::foreign_db::read_bytes(db, GOAL_DB_MAX_BYTES, |main, wal| {
        let matches = crate::harness::sqlite_read::scan_table_with(
            main,
            wal,
            table,
            |schema| {
                Some((
                    schema.column_index(key)?,
                    schema.column_index("objective")?,
                    schema.column_index("status")?,
                ))
            },
            |&(k_i, obj_i, st_i), row| {
                if row.get(k_i)?.as_str() == Some(id) && row.get(st_i)?.as_str() == Some("active") {
                    Some(row.get(obj_i)?.to_list_field())
                } else {
                    None
                }
            },
        )?;
        Some(matches.into_iter().next().and_then(|s| objective_of(&s)))
    })
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
