// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The graph folders each window publishes its tabs in, recorded beside what a sweep collects
//! by those tabs: `<aterm state>/drive/.registries/` beside the drive journals drive collection
//! removes ([`drive_folder`]) and `<harness state>/upgrade/.registries/` beside the agent records
//! the record prunes remove ([`agent_folder`]), one `<pid>-<start>.json` (mode 0600) in each,
//! naming the window by pid, boot and kernel start ([`crate::release_progress::identity`]) and
//! listing the folders. A window records itself under every folder its own environment resolves
//! ([`folders`]): its aterm state root's, the default harness state's under that root (the
//! window's own sweep's), and the harness state the CLI's sweep resolves (`$ATERM_HARNESS_STATE`
//! where set, [`super::cli::harness_state_dir`]), so two windows of different aterm state roots
//! that share a harness state read each other's records beside the agent records they share.
//!
//! Windows of one state root need not publish in one graph: an elevated and an unelevated
//! Windows process publish under different temp folders, and on Unix a window started with
//! `XDG_RUNTIME_DIR` publishes under it while sharing the default state root. A collector reads
//! every recorded window's graph before it judges any tab gone from them all ([`live`]).
//!
//! A record also names the machine ([`aterm_uds::process::host_id`]) and the pid namespace
//! ([`aterm_uds::process::pid_namespace`], Linux) its process runs in: a state root shared with
//! another machine (a network home) or another pid namespace (a container that mounts this
//! home) holds records whose pids this kernel cannot judge, so a reader takes them for neither
//! running nor ended, and collects nothing while the record is fresh. A window refreshes its
//! standing records every keep pass ([`refresh`]), so any record a reader cannot judge, for that
//! or any other reason, that went a week unwritten is abandoned and removed ([`read`]).
//!
//! A window writes its records before it publishes a graph entry, wherever one is missing or
//! does not name that entry's folders yet, and removes them when it quits; a record a crash
//! left names a process this kernel proves ended, and its next reader removes it. Where this host cannot name a
//! process's start, its machine or its pid namespace (Windows), no record is written and a
//! reader can tell nothing ([`live`]).

use std::path::{Path, PathBuf};

/// The name of a records folder, in the folder whose files it speaks for.
pub const FOLDER: &str = ".registries";

/// The most bytes one record is read to.
const RECORD_BYTES: u64 = 64 * 1024;

/// The records beside the drive journals of the aterm state root `aterm_state`
/// (`<aterm_state>/drive`, the folder drive collection removes journals from).
#[must_use]
pub fn drive_folder(aterm_state: &Path) -> PathBuf {
    aterm_state.join("drive").join(FOLDER)
}

/// The records beside the agent records of the harness state `state` (`<state>/upgrade`, the
/// folder the record prunes remove records from).
#[must_use]
pub fn agent_folder(state: &Path) -> PathBuf {
    state.join("upgrade").join(FOLDER)
}

/// Every records folder a sweep of this process's environment reads ([`folders_of`]): those of
/// its aterm state root ([`aterm_types::dirs::state_dir`]) and of the harness state the CLI
/// resolves ([`super::cli::harness_state_dir`]), each override (`$ATERM_STATE_HOME`,
/// `$ATERM_HARNESS_STATE`, a scratch world) applied as the sweeps apply it. In a Cargo test
/// binary ([`aterm_uds::test_binary::is_test_binary`]) only the folders under its redirected
/// state root: `$ATERM_HARNESS_STATE` is read as it is set, which no test binary redirects, so a
/// test run from a shell that names the person's harness state never writes there.
#[must_use]
pub fn folders() -> Vec<PathBuf> {
    static IN_TEST_BINARY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let in_test_binary = *IN_TEST_BINARY.get_or_init(|| {
        std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe))
    });
    let harness_state = if in_test_binary {
        None
    } else {
        super::cli::harness_state_dir().ok()
    };
    folders_of(
        aterm_types::dirs::state_dir().as_deref(),
        harness_state.as_deref(),
    )
}

/// The records folders of the aterm state root `aterm_state` and the harness state
/// `harness_state`, each named once: the drive folder's ([`drive_folder`]) and the default
/// harness state's under `aterm_state` ([`super::cli::harness_state_in`], a window's own sweep's),
/// then `harness_state`'s ([`agent_folder`]).
#[must_use]
pub fn folders_of(aterm_state: Option<&Path>, harness_state: Option<&Path>) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(root) = aterm_state {
        folders.push(drive_folder(root));
        folders.push(agent_folder(&super::cli::harness_state_in(root)));
    }
    if let Some(folder) = harness_state.map(agent_folder)
        && !folders.contains(&folder)
    {
        folders.push(folder);
    }
    folders
}

/// The record name of the process `pid` started at kernel `start`.
fn name_of(pid: u32, start: u64) -> String {
    format!("{pid}-{start}.json")
}

/// Where a process runs, as its record names it and a reader compares: the machine and, on a
/// kernel with pid namespaces, the namespace its pids are read in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    host: String,
    namespace: Option<String>,
}

/// This process's [`Place`]: `None` where this host names no machine, or a kernel with pid
/// namespaces does not name this process's.
fn here() -> Option<Place> {
    let namespace = aterm_uds::process::pid_namespace();
    if cfg!(any(target_os = "linux", target_os = "android")) && namespace.is_none() {
        return None;
    }
    Some(Place {
        host: aterm_uds::process::host_id()?,
        namespace,
    })
}

/// Record this process in the records folder `folder` ([`folders`]) as publishing its tabs in
/// each folder of `graphs`, replacing its earlier record there whole; missing folders on the way
/// are made private (0700 on Unix). A graph folder whose path is not UTF-8 is recorded as its
/// lossy spelling, which names nothing a reader can open, so that reader collects nothing
/// ([`live`]). The record's path.
///
/// # Errors
/// This host names no start, machine or pid namespace for this process (`Unsupported`), or the
/// folder could not be made or the record written.
pub fn record(folder: &Path, graphs: &[PathBuf]) -> std::io::Result<PathBuf> {
    let pid = std::process::id();
    let unnamed = || {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this host names no start, machine or pid namespace for this process",
        )
    };
    let (boot, start) = crate::release_progress::identity(pid).ok_or_else(unnamed)?;
    let place = here().ok_or_else(unnamed)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(folder)?;
    let graphs: Vec<String> = graphs
        .iter()
        .map(|graph| graph.to_string_lossy().into_owned())
        .collect();
    let body = aterm_json::json!({"pid": pid, "boot": boot, "start": start,
        "host": place.host, "namespace": place.namespace, "graphs": graphs});
    let path = folder.join(name_of(pid, start));
    super::atomic_write::write_atomic_ordered_mode(&path, &body.to_string(), Some(0o600))?;
    Ok(path)
}

/// Remove the record at `path`, a regular file only (never a link or anything else there).
pub fn forget(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Move the modification time of this process's record at `path` to `at` without changing its
/// bytes, opened without following a link, as a window's keep pass does to every record of its
/// that still stands, so no reader takes it for abandoned while this process runs ([`read`]).
///
/// # Errors
/// The record cannot be opened, is no regular file, or its time cannot be set.
pub fn refresh(path: &Path, at: std::time::SystemTime) -> std::io::Result<()> {
    let mut open = std::fs::OpenOptions::new();
    open.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = open.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the record is not a regular file",
        ));
    }
    file.set_modified(at)
}

/// What the records of one folder say ([`read`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// The graph folders of every recorded process this kernel proves still running: `None`
    /// where any record cannot be told.
    pub graphs: Option<Vec<PathBuf>>,
    /// The records nothing reads again: those of processes this kernel proves ended, and those
    /// abandoned ([`read`]).
    pub spent: Vec<PathBuf>,
}

/// What one record says ([`read`]).
enum Told {
    /// Its process runs, publishing in these graph folders.
    Running(Vec<PathBuf>),
    /// Nothing reads it again.
    Spent,
    /// It was removed since the listing: its process quit.
    Gone,
}

/// The records in the records folder `folder` ([`drive_folder`], [`agent_folder`]) at wall
/// second `now`: the graph folders of each recorded process this kernel proves running, by pid,
/// boot and start ([`crate::release_progress::judge`]), and the spent records, of processes it
/// proves ended and those abandoned. No folder is no records. A name that is not a record's (a
/// writer's temporary) is not read. Every entry is read, so the spent records are named whatever
/// else the folder holds.
///
/// A record cannot be told where it does not read (unreadable, no JSON, not a regular file), is
/// no record of its own name, names a process this kernel neither proves running nor proves
/// ended, names a running process whose graph folder is no folder, or names another machine or
/// pid namespace than this process's, whose pids this kernel cannot judge, or no place at all
/// (written before records named one). Such a record whose entry went
/// [`STATE_KEEP_S`](super::upgrade_drive::STATE_KEEP_S) unwritten before `now`, by its own
/// modification time (`lstat`, a link's own), is abandoned: spent and skipped, where it is a
/// regular file or a link; a folder or anything else there is never removed and keeps the reading
/// unknown. Any other such record makes the graphs `None`, as do this process failing to record
/// itself ([`record`], so no other could either), the folder or an entry not reading, and an entry
/// whose own time cannot be read. A record that can be told is judged whatever its age.
///
/// A week is safe: every running window refreshes each of its records every ten minutes, so a
/// record a week unwritten is no running window's, unless its window slept that week (its
/// machine, its container), and that one refreshes or writes its record again within ten minutes
/// of waking; meanwhile its tabs are judged gone only from the removal on, so they still need
/// their own week as gone before drive collection removes their journals, and a prune removes an
/// agent record only once that too went a week unwritten.
#[must_use]
pub fn read(folder: &Path, now: u64) -> Reading {
    let mut reading = Reading::default();
    if crate::release_progress::identity(std::process::id()).is_none() {
        return reading;
    }
    let Some(here) = here() else {
        return reading;
    };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            reading.graphs = Some(Vec::new());
            return reading;
        }
        Err(_) => return reading,
    };
    let mut graphs = Some(Vec::new());
    for entry in entries {
        let Ok(entry) = entry else {
            graphs = None;
            break;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| record_named(name)) else {
            continue;
        };
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            // Removed since the listing: its process quit.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                graphs = None;
                continue;
            }
        };
        let told = if meta.is_file() {
            tell(&path, name, &here)
        } else {
            None
        };
        match told {
            Some(Told::Running(found)) => {
                if let Some(graphs) = graphs.as_mut() {
                    graphs.extend(found);
                }
            }
            Some(Told::Spent) => reading.spent.push(path),
            Some(Told::Gone) => {}
            None if removable(&meta) && abandoned(&meta, now) => reading.spent.push(path),
            None => graphs = None,
        }
    }
    reading.graphs = graphs;
    reading
}

/// The graph folders [`read`] finds running in `folder` at wall second `now`, once the spent
/// records it names are removed (each a regular file or a link, the link itself, never what it
/// names), whether or not the rest reads whole.
#[must_use]
pub fn live(folder: &Path, now: u64) -> Option<Vec<PathBuf>> {
    let reading = read(folder, now);
    for spent in &reading.spent {
        if std::fs::symlink_metadata(spent).is_ok_and(|meta| removable(&meta)) {
            let _ = std::fs::remove_file(spent);
        }
    }
    reading.graphs
}

/// What the regular file `path`, named `name`, says as a record to a reader at `here`
/// ([`read`]); `None` where it cannot be told.
fn tell(path: &Path, name: &str, here: &Place) -> Option<Told> {
    let Some(text) = super::accounts::read_bounded(path, RECORD_BYTES).ok()? else {
        return Some(Told::Gone);
    };
    let record: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    let pid = u32::try_from(record["pid"].as_u64()?).ok()?;
    let start = record["start"].as_u64()?;
    if name != name_of(pid, start) {
        return None;
    }
    let place = match (record.get("host"), record.get("namespace")) {
        (Some(host), Some(namespace)) => Some(Place {
            host: host.as_str()?.to_owned(),
            namespace: match namespace {
                aterm_json::Value::Null => None,
                namespace => Some(namespace.as_str()?.to_owned()),
            },
        }),
        _ => None,
    };
    if place.as_ref() != Some(here) {
        return None;
    }
    match crate::release_progress::judge(pid, record["boot"].as_str()?, start) {
        crate::release_progress::Judged::Alive => {
            let mut graphs = Vec::new();
            for graph in record["graphs"].as_array()? {
                let graph = PathBuf::from(graph.as_str()?);
                if !std::fs::symlink_metadata(&graph).is_ok_and(|meta| meta.is_dir()) {
                    return None;
                }
                graphs.push(graph);
            }
            Some(Told::Running(graphs))
        }
        crate::release_progress::Judged::Dead => Some(Told::Spent),
        crate::release_progress::Judged::Unknown => None,
    }
}

/// Whether an entry of `meta` (its `lstat`) may be removed as a record: a regular file or a link.
fn removable(meta: &std::fs::Metadata) -> bool {
    meta.is_file() || meta.file_type().is_symlink()
}

/// Whether an entry of `meta` (its `lstat`) was last written at least
/// [`STATE_KEEP_S`](super::upgrade_drive::STATE_KEEP_S) before wall second `now`; not where its
/// time does not read.
fn abandoned(meta: &std::fs::Metadata, now: u64) -> bool {
    meta.modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|at| now.saturating_sub(at.as_secs()) >= super::upgrade_drive::STATE_KEEP_S)
}

/// Whether `name` is a record's: `<pid>-<start>.json`, both decimal.
fn record_named(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    name.strip_suffix(".json")
        .and_then(|stem| stem.split_once('-'))
        .is_some_and(|(pid, start)| digits(pid) && digits(start))
}

/// Whether the graph folder `graph` holds an entry for the tab `sid`: `None` where that cannot
/// be read, or `sid` names no session ([`aterm_types::control_request::Sid::parse`]).
#[must_use]
pub fn holds(graph: &Path, sid: &str) -> Option<bool> {
    aterm_types::control_request::Sid::parse(sid)?;
    match std::fs::symlink_metadata(graph.join(sid)) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "graph_records_tests.rs"]
mod tests;
