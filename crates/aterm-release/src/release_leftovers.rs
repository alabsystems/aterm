// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE RELEASE TOOL ITSELF LEAVES, BOUNDED: each run's log folder, each frozen tool set,
//! each pump controller copy and each clone's cutter checkout are written once per run, per
//! cutter build, per `--pump --install` and per clone, and nothing removed them.
//!
//! THE BOUND keeps the newest release's and whatever a live record names, and removes the rest:
//!
//! - A RUN'S LOG FOLDER (`<state>/.ubd/logs.noindex/<stamp>-<pid>`, and `<state>/logs/` from
//!   before the quiet folder) says which clone's release folder it belongs to ([`OWNER`],
//!   written by [`own`] as the run begins). It stays while that clone's `running` claim or its
//!   `latest-run` pointer names it, and while it is no older than that clone's newest release
//!   (`release.toml` and `candidate.toml`, their `started`). A folder whose clone is gone goes;
//!   one that names no clone (written before runs recorded theirs) goes once [`UNOWNED`] old and
//!   the sweeping clone names it nowhere.
//! - A FROZEN TOOL SET (`<release>/.ubd/tools/<set sha256>`) stays while a record of the release
//!   folder (`cutter.toml`, `tool.toml`, any `.toml` there or in its `.ubd`) names it.
//! - A PUMP CONTROLLER COPY (`<state>/.ubd/pump/controllers/<token>`) stays while it is the one
//!   just installed, a launch agent names it or a process runs from it.
//! - A CUTTER CHECKOUT (`<state>/.ubd/cutter-<key>.noindex`) stays while the cutter cache it was
//!   made for ([`cutter_owner`]) exists; one with no such record stays.
//!
//! NOTHING WRITTEN WITHIN [`QUIET`] GOES, whatever names it: a run that has made its folder and
//! not yet its claim, and a second install that has copied its controller and not yet written
//! the agent, are both younger. An executable's folder goes only through
//! [`crate::artifact::remove_unused_dir`], which keeps what a process runs from or a managed
//! service names; a record that cannot be read keeps everything it could have named.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Nothing written within this long is removed.
pub(crate) const QUIET: Duration = Duration::from_secs(24 * 60 * 60);

/// A run's log folder that names no clone goes once unwritten this long.
pub(crate) const UNOWNED: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// The file in a run's log folder that names its clone's release folder, absolute.
pub(crate) const OWNER: &str = "release-dir";

/// The longest record this reads.
const RECORD_MAX: u64 = 1024 * 1024;

/// Record in the run's log folder `logs` that it belongs to the clone whose release folder is
/// `release`.
pub(crate) fn own(logs: &Path, release: &Path) {
    let _ = std::fs::write(logs.join(OWNER), release.as_os_str().as_encoded_bytes());
}

/// The absolute path `file` holds, when it is a regular file holding exactly one.
fn path_in(file: &Path) -> Option<PathBuf> {
    let text = crate::durable::read_regular_text(file, 8192).ok()?;
    let text = text.trim_end_matches(['\r', '\n']);
    let path = PathBuf::from(text);
    (!text.is_empty() && !text.chars().any(char::is_control) && path.is_absolute()).then_some(path)
}

/// Whether `a` and `b` are one folder, by spelling or resolved.
fn same(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

/// A real directory's entries that are themselves real directories, sorted.
fn dirs_in(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    out.sort();
    out
}

/// When `dir` or an entry directly in it was last written; `None` when either cannot be read.
fn newest_write(dir: &Path) -> Option<SystemTime> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let mut newest = meta.modified().ok()?;
    for entry in std::fs::read_dir(dir).ok()? {
        newest = newest.max(entry.ok()?.metadata().ok()?.modified().ok()?);
    }
    Some(newest)
}

/// Whether `dir` was last written at least `span` before `now`; an unreadable folder was not.
fn quiet(dir: &Path, now: SystemTime, span: Duration) -> bool {
    newest_write(dir).is_some_and(|at| now.duration_since(at).is_ok_and(|age| age >= span))
}

/// What a clone's release folder says of its runs' logs.
struct Names {
    /// The log folders its `running` claim and its `latest-run` pointer name.
    named: Vec<PathBuf>,
    /// When its newest release began: the later `started` of its two journals. `None` with no
    /// journal.
    since: Option<SystemTime>,
}

/// What `release` names, or `Err` when a record there cannot be read, so that nothing it could
/// have named is removed.
fn names(release: &Path) -> Result<Names, String> {
    let mut named = Vec::new();
    if let Some(holder) = crate::release::read_holder(&release.join(crate::release::RUNNING_FILE))
        && !holder.logs.is_empty()
    {
        named.push(PathBuf::from(holder.logs));
    }
    let latest = release.join("latest-run");
    match std::fs::symlink_metadata(&latest) {
        Ok(_) => named.push(path_in(&latest).ok_or("latest-run cannot be read")?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut since = None;
    for journal in ["release.toml", "candidate.toml"] {
        if let Some(journal) = crate::release::Journal::load(&release.join(journal))? {
            let started = SystemTime::UNIX_EPOCH + Duration::from_secs(journal.started);
            since = Some(since.map_or(started, |known: SystemTime| known.max(started)));
        }
    }
    Ok(Names { named, since })
}

/// The process id a run's log folder was named for (`<stamp>-<pid>`).
fn pid_of(logs: &Path) -> Option<u32> {
    logs.file_name()?.to_str()?.rsplit_once('-')?.1.parse().ok()
}

/// Whether the run's log folder `logs` goes at `now`, seen from the clone whose release folder
/// is `release`; `alive` says whether a process id still runs.
fn log_goes(logs: &Path, release: &Path, now: SystemTime, alive: &dyn Fn(u32) -> bool) -> bool {
    if !quiet(logs, now, QUIET) {
        return false;
    }
    let Some(newest) = newest_write(logs) else {
        return false;
    };
    let kept_by = |release: &Path| match names(release) {
        Err(_) => true,
        Ok(names) => {
            names.named.iter().any(|named| same(named, logs))
                || names.since.is_some_and(|since| newest >= since)
        }
    };
    let owner_file = logs.join(OWNER);
    let goes = match std::fs::symlink_metadata(&owner_file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            quiet(logs, now, UNOWNED) && !kept_by(release)
        }
        Err(_) => false,
        Ok(_) => match path_in(&owner_file) {
            None => false,
            Some(owner) => match std::fs::symlink_metadata(&owner) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
                Ok(_) => !kept_by(&owner),
            },
        },
    };
    goes && !pid_of(logs).is_some_and(alive)
}

/// Remove every run's log folder under `state` that no release names any more ([`log_goes`]),
/// and return the folders removed.
pub(crate) fn sweep_logs(
    state: &Path,
    release: &Path,
    now: SystemTime,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<PathBuf> {
    let roots = [
        state.join(aterm_tempfile::QUIET).join("logs.noindex"),
        state.join("logs"),
    ];
    let mut removed = Vec::new();
    for logs in roots.iter().flat_map(|root| dirs_in(root)) {
        if log_goes(&logs, release, now, alive) && std::fs::remove_dir_all(&logs).is_ok() {
            removed.push(logs);
        }
    }
    removed
}

/// The record beside a cutter checkout `source` (`cutter-<key>.noindex`) that names the cutter
/// cache it was made for: `cutter-<key>.owner`.
pub(crate) fn cutter_owner(source: &Path) -> PathBuf {
    source.with_extension("owner")
}

/// Record that the cutter checkout `source` was made for the cutter cache `cache`.
pub(crate) fn own_cutter(source: &Path, cache: &Path) {
    let record = cutter_owner(source);
    if path_in(&record).as_deref() != Some(cache) {
        let _ = std::fs::write(record, cache.as_os_str().as_encoded_bytes());
    }
}

/// Remove every cutter checkout under `state` whose recorded cutter cache is gone, through
/// `remove`, and return the checkouts removed.
pub(crate) fn sweep_cutters(
    state: &Path,
    now: SystemTime,
    remove: &dyn Fn(&Path) -> Result<bool, String>,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for source in dirs_in(&state.join(aterm_tempfile::QUIET)) {
        let named = source
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("cutter-") && name.ends_with(".noindex"));
        let record = cutter_owner(&source);
        let orphan = path_in(&record).is_some_and(|cache| {
            std::fs::symlink_metadata(&cache)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        });
        if named && orphan && quiet(&source, now, QUIET) && remove(&source) == Ok(true) {
            let _ = std::fs::remove_file(record);
            removed.push(source);
        }
    }
    removed
}

/// Every record of the release folder `release` that could name a frozen tool set, as one
/// text; `None` when one cannot be read.
fn records(release: &Path) -> Option<String> {
    let mut text = String::new();
    for dir in [release.to_path_buf(), release.join(aterm_tempfile::QUIET)] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        for entry in entries {
            let path = entry.ok()?.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                text.push_str(&crate::durable::read_regular_text(&path, RECORD_MAX).ok()?);
                text.push('\n');
            }
        }
    }
    Some(text)
}

/// Remove every frozen tool set of `release` but `frozen` (the one just chosen, which no record
/// names yet) that none of its records names, and every staging folder an interrupted freeze
/// left, through `remove`; return the folders removed.
pub(crate) fn sweep_tools(
    release: &Path,
    frozen: &Path,
    now: SystemTime,
    remove: &dyn Fn(&Path) -> Result<bool, String>,
) -> Vec<PathBuf> {
    let Some(records) = records(release) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for set in dirs_in(&release.join(aterm_tempfile::QUIET).join("tools")) {
        let Some(name) = set.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let a_set = name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit());
        let a_stage = name.starts_with(".tools-");
        if (a_set && !records.contains(name) || a_stage)
            && !same(&set, frozen)
            && quiet(&set, now, QUIET)
            && remove(&set) == Ok(true)
        {
            removed.push(set);
        }
    }
    removed
}

/// Remove every pump controller copy under `controllers` but `installed`, through `remove`, and
/// return the folders removed.
pub(crate) fn sweep_controllers(
    controllers: &Path,
    installed: &Path,
    now: SystemTime,
    remove: &dyn Fn(&Path) -> Result<bool, String>,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for copy in dirs_in(controllers) {
        if !same(&copy, installed) && quiet(&copy, now, QUIET) && remove(&copy) == Ok(true) {
            removed.push(copy);
        }
    }
    removed
}

/// As a run begins: record that its log folder `logs` is `release`'s, sweep the state's log
/// folders and cutter checkouts, and note what went in the run's own `leftovers.log`.
pub(crate) fn at_run_start(state: &Path, release: &Path, logs: &Path) {
    own(logs, release);
    let now = SystemTime::now();
    let mut removed = sweep_logs(state, release, now, &|pid| {
        crate::release_steps::started_of(pid).is_some()
    });
    removed.extend(sweep_cutters(
        state,
        now,
        &crate::artifact::remove_unused_dir,
    ));
    note(logs, &removed);
}

/// Append one line per removed folder to `logs`' `leftovers.log`.
pub(crate) fn note(logs: &Path, removed: &[PathBuf]) {
    use std::io::Write as _;
    if removed.is_empty() {
        return;
    }
    if let Ok(mut file) = crate::durable::open_append(&logs.join("leftovers.log"), true) {
        for path in removed {
            let _ = writeln!(file, "removed {}: no release names it", path.display());
        }
    }
}

#[cfg(test)]
#[path = "release_leftovers_tests.rs"]
mod tests;
