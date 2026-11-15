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

/// The most UTF-16 code units of a `/goal` condition Claude Code takes: its own limit (`Goal
/// condition is limited to …` past `length > 4000`, read in the 2.1.325 bundle), and so the
/// longest condition the harness types back ([`typable`]).
pub const CONDITION_UNITS: usize = 4000;

/// The most bytes of a condition aterm keeps as Claude Code holds it ([`GoalRead::Active`]): three
/// to a UTF-16 code unit, so every condition Claude Code takes fits whole.
pub const CONDITION_BYTES: usize = 3 * CONDITION_UNITS;

/// The name a transcript's command row gives `/goal` (`<command-name>/goal</command-name>`, then
/// its `<command-args>`), measured on 2.1.323.
const GOAL_COMMAND: &str = "<command-name>/goal</command-name>";

/// WHY A CONDITION CLAUDE CODE CLEARED IS NOT TYPED BACK ([`typable`]): `/goal <condition>` would
/// set another goal than the one it held, so none is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Untypable {
    /// Nothing is left of it once its ends are trimmed.
    Empty,
    /// It is longer than [`CONDITION_UNITS`]: Claude Code would refuse it.
    Long,
    /// It holds a line break or another control character: typed, it would be sent cut, or
    /// changed.
    Breaks,
}

impl Untypable {
    /// Every reason, in the declaration's order.
    pub const ALL: [Self; 3] = [Self::Empty, Self::Long, Self::Breaks];

    /// The reason's word, as a tape and a ledger row hold it ([`Self::from_word`] reads it back).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Long => "long",
            Self::Breaks => "breaks",
        }
    }

    /// The reason [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.word() == word)
    }

    /// The reason as the journal and the ledger say it.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Empty => "the cleared condition is empty",
            Self::Long => "the cleared condition is longer than Claude Code's /goal takes",
            Self::Breaks => "the cleared condition holds a line break or a control character",
        }
    }
}

/// `raw` as one `/goal` line sets it again exactly: its ends trimmed (as Claude Code reads a
/// command's arguments), at most [`CONDITION_UNITS`] UTF-16 code units, with no control
/// character and no line or paragraph separator. Else why not ([`Untypable`]): the harness never
/// types a changed goal.
///
/// # Errors
/// [`Untypable`]: empty, longer than Claude Code takes, or holding a break.
pub fn typable(raw: &str) -> Result<String, Untypable> {
    let line = raw.trim();
    if line.is_empty() {
        return Err(Untypable::Empty);
    }
    if line.len() > CONDITION_BYTES || line.encode_utf16().count() > CONDITION_UNITS {
        return Err(Untypable::Long);
    }
    if line
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(Untypable::Breaks);
    }
    Ok(line.to_owned())
}

/// `raw` as Claude Code holds it, for the harness's comparisons ([`GoalRead::Active`]): its ends
/// trimmed, at most [`CONDITION_BYTES`] (cut on a character boundary past that).
fn held(raw: &str) -> String {
    super::truncate_bytes(raw.trim(), CONDITION_BYTES).to_owned()
}

/// The arguments of the `/goal` a command row's `content` names, its ends trimmed (empty for a
/// bare `/goal`); `None` where it names another command or none.
fn goal_args(content: &str) -> Option<&str> {
    let rest = content.trim_start().strip_prefix(GOAL_COMMAND)?;
    let args = rest
        .split_once("<command-args>")
        .and_then(|(_, a)| a.split_once("</command-args>"))
        .map_or("", |(a, _)| a);
    Some(args.trim())
}

/// The most bytes of an error's cause aterm keeps from Claude Code's notice that it cleared a goal
/// ([`cleared_label`], [`GoalEnd::ByError`]).
pub const LABEL_BYTES: usize = 120;

/// The most bytes of a transcript row's `uuid` aterm keeps ([`Ended::row`]): a longer id is none.
pub const ROW_ID_BYTES: usize = 64;

/// The cause Claude Code names in its notice that it cleared a goal after an unrecoverable error
/// (`authentication failed` in `Goal cleared after an unrecoverable error (authentication
/// failed): "<condition>". Run /goal again to continue.`), wherever the notice stands in `text`: the
/// `informational` system row it writes into the transcript, or the row it draws, read into a
/// wall's message. `None` where `text` holds no such notice or it names no cause.
#[must_use]
pub fn cleared_label(text: &str) -> Option<&str> {
    let head = aterm_phase::anchor!("wall.goal_cleared");
    let at = text.find(head)? + head.len();
    let rest = text[at..].trim_start().strip_prefix('(')?;
    rest.split(')').next().filter(|l| !l.is_empty())
}

/// HOW THE GOAL THE NEWEST GOAL ROW ENDED WAS ENDED ([`Ended::end`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalEnd {
    /// Its Stop hook found the condition holds: `met: true` with no `sentinel`.
    Met,
    /// Its Stop hook gave it up: `failed: true`, with the hook's `reason` and counts.
    Failed,
    /// `/goal clear` dropped it: the sentinel row `met: true, sentinel: true`, and no notice of an
    /// error after it. Claude Code writes the same row when a person types `/goal clear`.
    Cleared,
    /// Claude Code dropped it after an unrecoverable error: that same sentinel row, and as the very
    /// next row (its `parentUuid` names the sentinel row's `uuid`) the `informational` system row
    /// `Goal cleared after an unrecoverable error (<label>): "<condition>". Run /goal again to
    /// continue.` (Claude Code 2.1.323, measured on m2 2026-11-13 at a Cloudflare 403).
    ByError {
        /// The error's cause as the notice names it ([`cleared_label`]), at most [`LABEL_BYTES`].
        label: String,
    },
}

/// THE GOAL THE NEWEST GOAL ROW ENDED ([`ClaudeGoal::ended`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    /// Its condition, as [`shown`] keeps it.
    pub condition: String,
    /// Its condition as one `/goal` line sets it again exactly, or why it cannot ([`typable`]).
    pub line: Result<String, Untypable>,
    /// How it ended.
    pub end: GoalEnd,
    /// The ending row's `uuid`, at most [`ROW_ID_BYTES`] (empty where the row names none or a
    /// longer one): what tells this end apart from another end of the same goal.
    pub row: String,
    /// A response the API served in the main conversation came after this end
    /// ([`ClaudeGoal::line`]): the error, where one ended it, has passed.
    pub served: bool,
    /// A `/goal` of the person's came after this end and set no goal: a clear word (`/goal
    /// clear`, which answers `No goal set` and writes no goal row once no goal is in force), a
    /// condition Claude Code refused, or a bare `/goal`; any but the line that sets this goal
    /// again ([`Self::line`], the harness's own restore). The goal is theirs from there.
    pub taken: bool,
}

/// THE GOAL CLAUDE CODE CLEARED AFTER AN UNRECOVERABLE ERROR ([`GoalRead::Lost`]): an [`Ended`]
/// whose end is [`GoalEnd::ByError`], no goal row after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lost {
    /// The condition it held, as [`shown`] keeps it: one line of at most [`GOAL_BYTES`], for the
    /// journal alone.
    pub condition: String,
    /// The condition as one `/goal` line sets it again exactly ([`typable`]): what the restore
    /// types, or why none is typed.
    pub line: Result<String, Untypable>,
    /// The error's cause the notice named.
    pub label: String,
    /// The clearing row's `uuid` ([`Ended::row`]), never empty: one clear apart from the next.
    pub clear: String,
    /// The API served a response in the main conversation after the clear.
    pub served: bool,
}

/// WHAT A SESSION'S TRANSCRIPT SAYS OF ITS `/goal` for the harness ([`ClaudeGoal::read`],
/// [`GoalRecord::read`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GoalRead {
    /// Nothing was read: no transcript, one not yet read to its end, or a program whose goal
    /// this read does not keep.
    #[default]
    Unread,
    /// No goal is in force and none was lost: none was set, or the last one was met, given up
    /// or dropped by `/goal clear`.
    Off,
    /// The goal in force: its condition as Claude Code holds it, its ends trimmed, at most
    /// [`CONDITION_BYTES`].
    Active(String),
    /// Claude Code cleared the goal after an unrecoverable error, and neither a goal row nor a
    /// person's `/goal` came after ([`Ended::taken`]).
    Lost(Lost),
}

/// What Claude Code's transcript says of its `/goal`, folded one row at a time ([`Self::row`]).
///
/// Claude Code writes a `{"type":"attachment","attachment":{"type":"goal_status",…}}` row when
/// `/goal <condition>` sets a goal (`met: false`, `sentinel: true`), again after a compaction
/// (the same sentinel), at each check its Stop hook makes (`met: false` with a `reason`; `met:
/// true` once the condition holds; `failed: true`, with its reason and counts, once the hook
/// gives the goal up) and when `/goal clear` drops it (`met: true`, `sentinel: true`). A goal it
/// drops after an unrecoverable error gets that same `/goal clear` row, followed at once by an
/// `informational` system row naming the error ([`GoalEnd::ByError`]; 2.1.323 writes no
/// `failed` there). The goal is active while the newest such row is neither met nor failed, and
/// its text is that row's `condition`: the reading Claude Code makes itself when it resumes a
/// conversation. How the newest row ended a goal is kept beside it ([`Self::ended`]).
///
/// Every `/goal` typed is also a command row: a `user` row whose content opens
/// `<command-name>/goal</command-name>` and carries its `<command-args>` (2.1.323; a `system`
/// row of subtype `local_command` with the same text, for one typed while a turn runs). Only
/// those two tags are read of it. A `/goal` that sets no goal writes no goal row (a clear word
/// with no goal in force answers `No goal set`; a condition Claude Code refuses, a bare `/goal`),
/// so its command row is what tells that a person took up a goal cleared after an error
/// ([`Ended::taken`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeGoal {
    /// The goal in force: as [`shown`] keeps it, and as Claude Code holds it ([`held`]).
    active: Option<(String, String)>,
    ended: Option<Ended>,
}

impl ClaudeGoal {
    /// Whether `line` may hold a goal row, the notice of a goal cleared after an error, or a
    /// `/goal` command row: the cheap look before any parse (neither the attachment's type, the
    /// notice's words nor the command's name have anything JSON escapes).
    #[must_use]
    pub fn may_be(line: &str) -> bool {
        line.contains("goal_status")
            || line.contains(aterm_phase::anchor!("wall.goal_cleared"))
            || line.contains(GOAL_COMMAND)
    }

    /// Fold one transcript row (its [`super::transcript::goal`] projection): whether it was a
    /// goal row of the main conversation, the notice that names the error a goal was just
    /// cleared after, or a `/goal` command row.
    pub fn row(&mut self, row: &Map) -> bool {
        if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return false;
        }
        fn text(v: Option<&Value>) -> Option<&str> {
            v.and_then(Value::as_str)
        }
        match row.get("type").and_then(Value::as_str) {
            Some("attachment") => self.goal_row(row),
            Some("system") => match row.get("subtype").and_then(Value::as_str) {
                Some("informational") => self.notice_row(row),
                Some("local_command") => text(row.get("content"))
                    .and_then(goal_args)
                    .is_some_and(|args| self.command_row(args)),
                _ => false,
            },
            Some("user") => text(row.get("message").and_then(|m| m.get("content")))
                .and_then(goal_args)
                .is_some_and(|args| self.command_row(args)),
            _ => false,
        }
    }

    /// A `/goal` typed with `args`: where the goal stands cleared after an error, any `/goal` but
    /// the line that sets it again takes it ([`Ended::taken`]). Elsewhere it changes nothing: a
    /// goal it set or cleared has its own goal row.
    fn command_row(&mut self, args: &str) -> bool {
        if let Some(ended) = self.ended.as_mut()
            && matches!(ended.end, GoalEnd::ByError { .. })
            && ended.line.as_deref() != Ok(args)
        {
            ended.taken = true;
        }
        true
    }

    fn goal_row(&mut self, row: &Map) -> bool {
        let Some(attachment) = row.get("attachment").and_then(Value::as_object) else {
            return false;
        };
        if attachment.get("type").and_then(Value::as_str) != Some("goal_status") {
            return false;
        }
        let flag = |key: &str| attachment.get(key).and_then(Value::as_bool) == Some(true);
        let raw = attachment.get("condition").and_then(Value::as_str);
        let condition = raw.and_then(shown);
        if flag("met") || flag("failed") {
            let end = if flag("failed") {
                GoalEnd::Failed
            } else if flag("sentinel") {
                GoalEnd::Cleared
            } else {
                GoalEnd::Met
            };
            self.active = None;
            self.ended = condition.map(|condition| Ended {
                condition,
                line: typable(raw.unwrap_or_default()),
                end,
                row: row_id(row, "uuid"),
                served: false,
                taken: false,
            });
        } else {
            self.active = condition.map(|shown| (shown, held(raw.unwrap_or_default())));
            self.ended = None;
        }
        true
    }

    /// The `informational` notice of a goal cleared after an unrecoverable error, read only as
    /// the child of the clear's own row ([`GoalEnd::ByError`]).
    fn notice_row(&mut self, row: &Map) -> bool {
        let Some(label) = row
            .get("content")
            .and_then(Value::as_str)
            .and_then(cleared_label)
        else {
            return false;
        };
        let parent = row_id(row, "parentUuid");
        match self.ended.as_mut() {
            Some(ended)
                if ended.end == GoalEnd::Cleared
                    && !ended.row.is_empty()
                    && ended.row == parent =>
            {
                ended.end = GoalEnd::ByError {
                    label: super::one_line(label, LABEL_BYTES),
                };
                true
            }
            _ => false,
        }
    }

    /// A response the API served in the main conversation ([`Self::line`]'s): the end the rows
    /// folded so far leave, if any, came before it.
    fn served(&mut self) {
        if let Some(ended) = self.ended.as_mut() {
            ended.served = true;
        }
    }

    /// Fold one raw transcript line: a goal row or the notice of an error clear ([`Self::row`]),
    /// or a response the API served after the goal ended ([`Self::served`]). Only a line that
    /// may hold one is parsed, and a served answer is looked for only while an end waits for one.
    pub fn line(&mut self, line: &str) {
        if Self::may_be(line)
            && let Ok(value) = super::transcript::goal(line)
            && let Some(row) = value.as_object()
            && self.row(row)
        {
            return;
        }
        if self.ended.as_ref().is_some_and(|e| !e.served)
            && line.contains(r#""type":"assistant""#)
            && let Ok(value) = super::transcript::usage(line)
            && let Some(row) = value.as_object()
            && answered(row)
        {
            self.served();
        }
    }

    /// The goal in force after the rows folded so far, as [`shown`] keeps it.
    #[must_use]
    pub fn active(&self) -> Option<&str> {
        self.active.as_ref().map(|(shown, _)| shown.as_str())
    }

    /// How the newest goal row ended a goal, where it ended one.
    #[must_use]
    pub fn ended(&self) -> Option<&Ended> {
        self.ended.as_ref()
    }

    /// What the rows folded so far say for the harness: the goal in force (as Claude Code holds
    /// it), one lost to an error that no person's `/goal` has taken up since, or none.
    #[must_use]
    pub fn read(&self) -> GoalRead {
        if let Some((_, held)) = &self.active {
            return GoalRead::Active(held.clone());
        }
        match &self.ended {
            Some(Ended {
                condition,
                line,
                end: GoalEnd::ByError { label },
                row,
                served,
                taken: false,
            }) => GoalRead::Lost(Lost {
                condition: condition.clone(),
                line: line.clone(),
                label: label.clone(),
                clear: row.clone(),
                served: *served,
            }),
            _ => GoalRead::Off,
        }
    }
}

/// `row`'s id under `key` (`uuid`, `parentUuid`): empty where it names none or one longer than
/// [`ROW_ID_BYTES`].
fn row_id(row: &Map, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|id| id.len() <= ROW_ID_BYTES)
        .unwrap_or_default()
        .to_owned()
}

/// Whether a row's [`super::transcript::usage`] projection is an answer the API served in the main
/// conversation: an assistant row with usage, no API error, and a real model's id (never
/// Claude Code's `<synthetic>` answer), as the usage fold counts one served.
fn answered(row: &Map) -> bool {
    let message = row.get("message").and_then(Value::as_object);
    row.get("type").and_then(Value::as_str) == Some("assistant")
        && row.get("isSidechain").and_then(Value::as_bool) != Some(true)
        && row.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true)
        && message
            .and_then(|m| m.get("usage"))
            .is_some_and(Value::is_object)
        && message
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .is_some_and(super::upgrade::is_model_id)
}

/// The most transcript bytes one [`GoalRecord::read`] folds: a longer transcript is read on at the
/// next ask, and reads [`GoalRead::Unread`] until its end is reached.
pub const RECORD_READ_BYTES: u64 = 64 << 20;

/// A CLAUDE CODE TRANSCRIPT'S GOAL ([`ClaudeGoal`]), read on from where the last read stopped:
/// the whole file at the first ask (at most [`RECORD_READ_BYTES`] an ask), then only what was
/// written since. Another file, or the same path holding another file or a shorter one, starts it
/// over.
#[derive(Debug, Default)]
pub struct GoalRecord {
    /// The file the reads are of: its path and its inode.
    file: Option<(PathBuf, u64)>,
    /// The byte after the last whole line folded.
    read_to: u64,
    /// The bytes from [`Self::read_to`] are the rest of a line longer than one read: passed over
    /// through its end.
    skipping: bool,
    goal: ClaudeGoal,
}

impl GoalRecord {
    /// What the transcript at `path` says of its goal ([`ClaudeGoal::read`]), once read to its
    /// end; [`GoalRead::Unread`] where it cannot be opened or is not read to its end yet. A line
    /// still being written at the end is folded at a later ask.
    pub fn read(&mut self, path: &Path) -> GoalRead {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let Ok((mut f, meta)) = aterm_uds::regular_file::open(path) else {
            *self = Self::default();
            return GoalRead::Unread;
        };
        let ino = inode(&meta);
        let len = meta.len();
        if self
            .file
            .as_ref()
            .is_none_or(|(held, at)| held != path || *at != ino)
            || len < self.read_to
        {
            *self = Self {
                file: Some((path.to_path_buf(), ino)),
                ..Self::default()
            };
        }
        let start = self.read_to;
        let left = len.saturating_sub(start);
        let want = left.min(RECORD_READ_BYTES);
        let mut buf = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
        if f.seek(SeekFrom::Start(start)).is_err()
            || f.by_ref().take(want).read_to_end(&mut buf).is_err()
        {
            return GoalRead::Unread;
        }
        let mut from = 0;
        if self.skipping {
            match buf.iter().position(|b| *b == b'\n') {
                Some(at) => {
                    from = at + 1;
                    self.skipping = false;
                }
                None => from = buf.len(),
            }
        }
        let whole = buf
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(from, |at| at + 1)
            .max(from);
        for line in String::from_utf8_lossy(&buf[from..whole]).lines() {
            self.goal.line(line);
        }
        let reached_end = want == left;
        self.read_to = start.saturating_add(whole as u64);
        if !reached_end && whole == 0 {
            // A full read with no line end in it: one line longer than a read, passed over
            // through its end.
            self.skipping = true;
            self.read_to = start.saturating_add(buf.len() as u64);
        }
        if reached_end {
            self.goal.read()
        } else {
            GoalRead::Unread
        }
    }
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.ino()
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> u64 {
    0
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
    seen: Option<(Store, PathBuf, String, [Option<FileStamp>; 2])>,
    goal: Option<String>,
}

impl StoreGoal {
    /// The goal `store`'s database `db` holds for `id`: read now when `db`, `id` or the files
    /// moved since the last read, else the last read's answer. No database is no goal; a read
    /// that failed keeps the last answer until the files move again. An in-flight write keeps the
    /// last answer without recording the stamp so the next call retries once the write settles.
    pub fn read(&mut self, store: Store, db: &Path, id: &str) -> Option<&str> {
        let stamp = store_stamp(db);
        if stamp[0].is_none() {
            self.seen = None;
            self.goal = None;
            return None;
        }
        let fresh = self.seen.as_ref().is_some_and(|(kind, path, held, at)| {
            *kind == store && path == db && held == id && *at == stamp
        });
        if !fresh {
            if self
                .seen
                .as_ref()
                .is_some_and(|(kind, path, held, _)| *kind != store || path != db || held != id)
            {
                self.goal = None;
            }
            match read_store(store, db, id) {
                Ok(goal) => {
                    self.goal = goal;
                    self.seen = Some((store, db.to_path_buf(), id.to_owned(), stamp));
                }
                Err(aterm_uds::foreign_db::Unread::Missing) => {
                    self.goal = None;
                    self.seen = None;
                }
                Err(
                    aterm_uds::foreign_db::Unread::Writing | aterm_uds::foreign_db::Unread::Changed,
                ) => {
                    self.seen = None;
                }
                Err(aterm_uds::foreign_db::Unread::Failed) => {
                    self.seen = Some((store, db.to_path_buf(), id.to_owned(), stamp));
                }
            }
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
