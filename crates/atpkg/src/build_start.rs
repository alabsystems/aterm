// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE BUILD-START RECORD: one line for each build started in an aterm tab, naming the tab's
//! session, the workspace the build compiles and its build folder.
//!
//! aterm's window reads the record ([`read`]) to give each tab the worktrees its builds ran in,
//! however the build reached them (`cd`, `--manifest-path`, `-C`). On macOS the front a `targo`
//! or `targo-tippy` shim hops to writes the line before the build starts
//! ([`crate::build_admit::run_hidden`]); aterm writes none on any other system, and none for a
//! build that never reaches that front: a `cargo` call a reroute runs (it execs the upstream
//! `cargo`, or the store's `targo` by its path), and a build that runs a `targo` by its path
//! (`$(trustc --print sysroot)/bin/targo`, verify's own compiles). A tool that does not link
//! aterm (a build driver of another repository) writes the same line by this contract:
//!
//! - **Place.** [`RECORD`] in aterm's state folder ([`record`]): on macOS
//!   `~/Library/Application Support/aterm/build-starts.log`; on Linux
//!   `$XDG_STATE_HOME/aterm/build-starts.log` where that variable is an absolute path, else
//!   `~/.local/state/aterm/build-starts.log`; on Windows `%LOCALAPPDATA%\aterm\build-starts.log`.
//!   A writer never makes the folder: where aterm has made none, no window reads a line. A
//!   writer whose effective user does not own the folder (a `sudo -E` build) writes nothing, so
//!   no record is ever another user's file. A debug build reads `ATERM_STATE_HOME` as the state
//!   folder and a release build does not, so a debug window (`lab instance`) reads the lines of
//!   a debug front alone, and a release front under a scratch `ATERM_STATE_HOME` writes into the
//!   account's own record, under a session no window of that account holds.
//! - **Line.** One JSON object and a `\n`, at most [`MAX_LINE`] bytes with the `\n`. A writer
//!   opens the record for appending (`O_APPEND`, and `O_CREAT` with mode 0600), writes the line
//!   with one `write` and closes it, for each line: lines written at once by several builds so
//!   stay whole, and a writer that kept its descriptor would write past a rewrite into a file no
//!   reader opens. No lock and no flush:
//!
//!   ```text
//!   {"v":1,"session":"s-0123456789abcdef0123","workspace":"/w","build":"/b","pid":1,"at_ms":1}
//!   ```
//!
//!   - `v` is 1. A reader skips a line of any other version.
//!   - `session` is the tab's session: the value of `ATERM_PARENT_SESSION_ID` in the build's
//!     environment, `s-` and 20 lower-case hex digits ([`session_of`]). A build with no such
//!     variable was not started in a tab and writes no line. `ATERM_SESSION_ID` is not the tab's
//!     session: it is a fresh id for an aterm started inside the tab, which no window holds.
//!   - `workspace` is the absolute path of the Cargo workspace root the build compiles, its links
//!     resolved. A writer that cannot name that root (no manifest named or found, a root it
//!     cannot resolve) writes no line rather than a folder that may hold other workspaces.
//!   - `build` is the absolute path of the folder its build output goes to, where the writer
//!     knows one; the key is left out, or `null`, where it does not.
//!   - `pid` is the writer's process id and `at_ms` the time, in milliseconds since the Unix
//!     epoch.
//!   - The keys come in any order. A reader ignores a key it does not know and skips a line it
//!     cannot read.
//! - **Never in the build's way.** A line that cannot be written is dropped, with nothing
//!   printed, and the build starts as it would have. A writer reads nothing a line names: it
//!   neither opens nor stats a recorded path, so a line naming a dead mount stalls no build.
//! - **Bound.** A writer that finds the record over [`CAP`] bytes after its own line may rewrite
//!   it ([`append`]), and only while it holds the lock of [`LOCK`] beside the record, taken
//!   without waiting: a writer that finds it held leaves the rewrite to its holder. The rewrite
//!   keeps lines by what they say alone: of the record's last [`READ_MAX`] bytes, the newest line
//!   of each workspace, and the newest of each session and workspace that is younger than
//!   [`KEEP_PAIR_FOR`] (a line dated after the rewrite's clock counts as old); then, oldest first,
//!   it sheds the second kind and after it the first until what is kept fits in [`KEEP_MAX`]
//!   bytes, half the cap, so the next rewrite is a cap's growth away. The newest line of a
//!   workspace so outlives its session, whether or not the workspace is still there; whether a
//!   path still exists is its reader's question. The rewrite stages the kept lines in [`STAGE`]
//!   and renames it over the record, then appends the whole lines written to the old file after
//!   its read. A line written to the old file after that is lost: that build's worktree is
//!   missing from its tab's reading until the tab's next build there. Lines the rewriting build
//!   cannot read go with it. A writer that never rewrites is correct too; the record then stays
//!   within what aterm's own writers keep.
//!
//! A line is unauthenticated: any process of the account can append one under any session id
//! (`aterm ctl ls` lists them), with any path, process id and time. It attributes bytes to a tab
//! and grants nothing: the window reads a line only for a session it holds, names from it only
//! roots a pane's own directory could name, and nothing reads the line as authority. A total
//! built from lines (a tab's bytes, a session's worktrees) must never drive a removal, a quota or
//! a hold.
//!
//! aterm's front reads what a call builds with a reader of its own
//! ([`crate::build_cache::registered::observe_shim_call`]), beside the one that registers the
//! call's build cache, and only for a call started in a tab whose state folder this user owns
//! ([`tab`]): the registration resolves no owner for a build in the shared cache, which is the
//! common one, so it has no workspace to hand on.

use std::collections::BTreeMap;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The record's file name inside aterm's state folder.
pub const RECORD: &str = "build-starts.log";

/// The file beside the record whose lock a rewrite holds ([`append`]).
pub const LOCK: &str = "build-starts.log.lock";

/// The file beside the record a rewrite stages its lines in before it renames it over the record.
pub const STAGE: &str = "build-starts.log.new";

/// The longest line, its `\n` included: one a single `write` appends whole.
pub const MAX_LINE: usize = 4096;

/// The size past which a writer rewrites the record ([`append`]).
pub const CAP: u64 = 256 << 10;

/// The most a rewrite keeps: half of [`CAP`], so rewrites are at least that much growth apart.
pub const KEEP_MAX: u64 = CAP / 2;

/// How much of the record's end a reader reads, and a rewrite keeps from.
pub const READ_MAX: u64 = 1 << 20;

/// How long a rewrite keeps a session's newest line of a workspace that a later session built
/// in too.
pub const KEEP_PAIR_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The one version of the line this module writes and reads.
const VERSION: u64 = 1;

/// A session id's length: `s-` and 20 hex digits.
const SESSION_BYTES: usize = 22;

/// Whether `text` is a session id as aterm mints one: `s-` and 20 lower-case hex digits.
fn is_session(text: &str) -> bool {
    text.len() == SESSION_BYTES && aterm_types::control_request::Sid::parse(text).is_some()
}

/// `path` as the line spells it: `None` for a relative path or one that is not UTF-8.
fn absolute_text(path: &Path) -> Option<&str> {
    path.to_str().filter(|_| path.is_absolute())
}

/// The path a line's `text` names: `None` for a relative one.
fn absolute_path(text: &str) -> Option<PathBuf> {
    let path = PathBuf::from(text);
    path.is_absolute().then_some(path)
}

/// One build start: what one line of the record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Start {
    /// The session of the tab the build was started in.
    pub session: String,
    /// The workspace root the build compiles.
    pub workspace: PathBuf,
    /// The folder its build output goes to, where the writer knew one.
    pub build: Option<PathBuf>,
    /// The writer's process id.
    pub pid: u32,
    /// When it was written, in milliseconds since the Unix epoch.
    pub at_ms: u64,
}

impl Start {
    /// The line that says this start, its `\n` included. `None` for a session that is no
    /// session id, a workspace or build folder that is relative or not UTF-8, and a line longer
    /// than [`MAX_LINE`].
    #[must_use]
    pub fn line(&self) -> Option<String> {
        if !is_session(&self.session) {
            return None;
        }
        let mut object = aterm_json::Map::new();
        object.insert("v".to_owned(), aterm_json::Value::from(VERSION));
        object.insert(
            "session".to_owned(),
            aterm_json::Value::from(self.session.as_str()),
        );
        object.insert(
            "workspace".to_owned(),
            aterm_json::Value::from(absolute_text(&self.workspace)?),
        );
        if let Some(build) = &self.build {
            object.insert(
                "build".to_owned(),
                aterm_json::Value::from(absolute_text(build)?),
            );
        }
        object.insert("pid".to_owned(), aterm_json::Value::from(self.pid));
        object.insert("at_ms".to_owned(), aterm_json::Value::from(self.at_ms));
        let mut line = aterm_json::to_string(&aterm_json::Value::Object(object)).ok()?;
        line.push('\n');
        (line.len() <= MAX_LINE).then_some(line)
    }

    /// What `line` says, with or without its `\n`. `None` for a line longer than [`MAX_LINE`],
    /// one that is no JSON object, one of another version, and one whose session is no session
    /// id, whose workspace or build folder is relative, or that lacks the session, the
    /// workspace, the process id or the time. A `build` that is absent or `null` is none.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if line.len() >= MAX_LINE {
            return None;
        }
        let value: aterm_json::Value = aterm_json::from_str(line).ok()?;
        if value.get("v")?.as_u64()? != VERSION {
            return None;
        }
        let session = value.get("session")?.as_str()?;
        if !is_session(session) {
            return None;
        }
        let build = match value.get("build") {
            None | Some(aterm_json::Value::Null) => None,
            Some(named) => Some(absolute_path(named.as_str()?)?),
        };
        Some(Self {
            session: session.to_owned(),
            workspace: absolute_path(value.get("workspace")?.as_str()?)?,
            build,
            pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
            at_ms: value.get("at_ms")?.as_u64()?,
        })
    }
}

/// Where the record is: [`RECORD`] in aterm's state folder. `None` where no state folder
/// resolves.
#[must_use]
pub fn record() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|state| state.join(RECORD))
}

/// The session of the tab a process with the environment `env` was started in: its
/// `ATERM_PARENT_SESSION_ID`, where that is a session id. `None` outside a tab.
#[must_use]
pub fn session_of(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    env(aterm_types::domain::ENV_PARENT_SESSION_ID).filter(|session| is_session(session))
}

/// Where the builds this process starts leave their lines ([`tab`]).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    /// The session of the tab this process was started in.
    session: String,
    /// The record, in a state folder this process's effective user owns.
    record: PathBuf,
}

/// Where a build this process starts leaves its line: the session of the tab it was started in
/// ([`session_of`]) and the record ([`record`]). `None` outside a tab, where no state folder
/// resolves, where it is no folder, and where this process's effective user does not own it
/// (a `sudo -E` build in a tab, whose record would be root's and so no window's). One `stat` of
/// the state folder; nothing a line names is read.
#[cfg(unix)]
#[must_use]
pub(crate) fn tab() -> Option<Tab> {
    use std::os::unix::fs::MetadataExt as _;
    let session = session_of(&|name| std::env::var(name).ok())?;
    let state = aterm_types::dirs::state_dir()?;
    let folder = std::fs::metadata(&state).ok()?;
    (folder.is_dir() && folder.uid() == aterm_uds::peer::our_uid()).then(|| Tab {
        session,
        record: state.join(RECORD),
    })
}

/// `path` opened for writing, made private where it is new, never through a link and never
/// waiting on a FIFO; `append` for appending, else truncated. `InvalidInput` for anything but a
/// regular file.
#[cfg(unix)]
fn open_private(path: &Path, append: bool) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .append(append)
        .truncate(!append)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

/// `path` opened for writing, never where its name is a link or anything but a file; `append`
/// for appending, else truncated.
#[cfg(not(unix))]
fn open_private(path: &Path, append: bool) -> io::Result<std::fs::File> {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && !meta.file_type().is_file()
    {
        return Err(not_regular());
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .append(append)
        .truncate(!append)
        .create(true)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "the build-start record is not a regular file",
    )
}

/// Append `start`'s line to `record` with one write, then rewrite the record where it has grown
/// past [`CAP`] ([`compact`], at the clock's time now). It makes no folder, flushes nothing,
/// prints nothing and never waits on a lock; every descriptor it opens is closed before it
/// returns.
///
/// # Errors
/// `InvalidInput` for a start that has no line ([`Start::line`]) and for a record that is not a
/// regular file; else the open's or the write's own error (no state folder, a folder that
/// cannot be written). A rewrite that fails or is skipped is no error: the line is written.
pub fn append(record: &Path, start: &Start) -> io::Result<()> {
    let line = start.line().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a build start with no session id, a relative or non-UTF-8 path, or too long a line",
        )
    })?;
    let mut file = open_private(record, true)?;
    file.write_all(line.as_bytes())?;
    let over = file.metadata().is_ok_and(|meta| meta.len() > CAP);
    drop(file);
    if over {
        compact(record, aterm_time::unix_millis());
    }
    Ok(())
}

/// The lines this module reads among the last [`READ_MAX`] bytes before `end` of `file`, oldest
/// first, each with what it says. Where `end` is past that, the first line of those bytes goes
/// unread: the start of the bytes may have cut it.
fn lines_before(file: &mut std::fs::File, end: u64) -> io::Result<Vec<(String, Start)>> {
    let from = end.saturating_sub(READ_MAX);
    file.seek(io::SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take(end - from).read_to_end(&mut bytes)?;
    let mut text = bytes.as_slice();
    if from > 0 {
        let first = text
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |end| end + 1);
        text = &text[first..];
    }
    Ok(text
        .split(|byte| *byte == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|line| Start::parse(line).map(|start| (line.to_owned(), start)))
        .collect())
}

/// Which of `lines` (oldest first) a rewrite at the clock's `now` keeps, by what they say alone:
/// the newest line of each workspace, and the newest of each session and workspace whose time
/// is at or before `now` and less than [`KEEP_PAIR_FOR`] before it (none such where the clock
/// reads before 1970); then, oldest first, it drops lines of the second kind only, then any,
/// until the kept lines with their `\n`s fit in [`KEEP_MAX`] bytes.
fn kept(lines: &[(String, Start)], now: Option<u64>) -> Vec<bool> {
    let keep_ms = u64::try_from(KEEP_PAIR_FOR.as_millis()).unwrap_or(u64::MAX);
    let mut newest_of_workspace: BTreeMap<&Path, usize> = BTreeMap::new();
    let mut newest_of_pair: BTreeMap<(&str, &Path), usize> = BTreeMap::new();
    for (at, (_, start)) in lines.iter().enumerate() {
        newest_of_workspace.insert(start.workspace.as_path(), at);
        newest_of_pair.insert((start.session.as_str(), start.workspace.as_path()), at);
    }
    let last_of_workspace: Vec<bool> = lines
        .iter()
        .enumerate()
        .map(|(at, (_, start))| newest_of_workspace.get(start.workspace.as_path()) == Some(&at))
        .collect();
    let mut keep: Vec<bool> = lines
        .iter()
        .enumerate()
        .map(|(at, (_, start))| {
            let young = now.is_some_and(|now| start.at_ms <= now && now - start.at_ms < keep_ms);
            last_of_workspace[at]
                || (young
                    && newest_of_pair.get(&(start.session.as_str(), start.workspace.as_path()))
                        == Some(&at))
        })
        .collect();
    let size = |at: usize| lines[at].0.len() as u64 + 1;
    let mut total: u64 = (0..lines.len()).filter(|&at| keep[at]).map(size).sum();
    for workspaces_too in [false, true] {
        for at in 0..lines.len() {
            if total <= KEEP_MAX {
                return keep;
            }
            if keep[at] && (workspaces_too || !last_of_workspace[at]) {
                keep[at] = false;
                total -= size(at);
            }
        }
    }
    keep
}

/// Rewrite `record`, past [`CAP`], to the lines a rewrite at the clock's `now` keeps ([`kept`]),
/// in their order, while this process holds the lock of [`LOCK`] beside it: a lock another
/// holds, or a record at or under the cap once it is held (a rewrite just ran), is no rewrite.
/// The kept lines are staged in [`STAGE`] and renamed over the record; then the whole lines
/// written to the old file after its read, up to [`READ_MAX`] bytes of them, are appended to the
/// new one. It reads nothing a line names, never waits, and leaves the record as it is where any
/// step fails.
fn compact(record: &Path, now: Option<u64>) {
    let Some(folder) = record.parent() else {
        return;
    };
    let Ok(lock) = open_private(&folder.join(LOCK), true) else {
        return;
    };
    let Ok(_held) = crate::lock::Flock::try_lock(lock) else {
        return;
    };
    let Ok((mut old, meta)) = crate::metadata_io::open_regular_with_meta(record) else {
        return;
    };
    let end = meta.len();
    if end <= CAP {
        return;
    }
    let Ok(lines) = lines_before(&mut old, end) else {
        return;
    };
    let mut text = String::new();
    for ((line, _), keep) in lines.iter().zip(kept(&lines, now)) {
        if keep {
            text.push_str(line);
            text.push('\n');
        }
    }
    let stage = folder.join(STAGE);
    let staged = open_private(&stage, false).and_then(|mut file| file.write_all(text.as_bytes()));
    if staged
        .and_then(|()| std::fs::rename(&stage, record))
        .is_err()
    {
        let _ = std::fs::remove_file(&stage);
        return;
    }
    let mut late = Vec::new();
    if old.seek(io::SeekFrom::Start(end)).is_err()
        || old.take(READ_MAX).read_to_end(&mut late).is_err()
    {
        return;
    }
    let whole = late
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    if whole > 0
        && let Ok(mut new) = open_private(record, true)
    {
        let _ = new.write_all(&late[..whole]);
    }
}

/// The newest start of each session and workspace among `starts` (oldest first), in their order.
fn newest_of_each_pair(starts: Vec<Start>) -> Vec<Start> {
    let mut newest: BTreeMap<(&str, &Path), usize> = BTreeMap::new();
    for (at, start) in starts.iter().enumerate() {
        newest.insert((start.session.as_str(), start.workspace.as_path()), at);
    }
    let keep: std::collections::BTreeSet<usize> = newest.into_values().collect();
    starts
        .into_iter()
        .enumerate()
        .filter(|(at, _)| keep.contains(at))
        .map(|(_, start)| start)
        .collect()
}

/// What `record` says: the newest line of each session and workspace among its last
/// [`READ_MAX`] bytes, oldest first. One bounded read of a regular file, never through a link;
/// empty where there is no record or it cannot be read.
#[must_use]
pub fn read(record: &Path) -> Vec<Start> {
    let Ok((mut file, meta)) = crate::metadata_io::open_regular_with_meta(record) else {
        return Vec::new();
    };
    let Ok(lines) = lines_before(&mut file, meta.len()) else {
        return Vec::new();
    };
    newest_of_each_pair(lines.into_iter().map(|(_, start)| start).collect())
}

/// Leave the start line of a build this process is about to become in `tab`'s record:
/// `workspace` and its `build` folder, under `tab`'s session, this process's id and the time
/// now. Nothing is written where the clock reads before 1970, and a line that cannot be written
/// is dropped: nothing here prints, waits or fails, so the build starts either way.
#[cfg(unix)]
pub(crate) fn note(tab: &Tab, workspace: &Path, build: Option<&Path>) {
    let Some(at_ms) = aterm_time::unix_millis() else {
        return;
    };
    let _ = append(
        &tab.record,
        &Start {
            session: tab.session.clone(),
            workspace: workspace.to_path_buf(),
            build: build.map(Path::to_path_buf),
            pid: std::process::id(),
            at_ms,
        },
    );
}

#[cfg(test)]
#[path = "build_start_tests.rs"]
mod tests;
