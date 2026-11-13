// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tracking and cleanup of per-user agent runtime directories in `/tmp`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

use crate::pyjson::J;

/// The record's name in the run directory.
pub const RECORD: &str = "runtime.txt";

/// The roots for the user `uid`: Muse Code's registry (physical: `/tmp` is a link).
#[must_use]
pub fn roots(uid: u32) -> Vec<String> {
    vec![format!("/private/tmp/tbh-{uid}-rt/muse")]
}

/// One entry a process of the jail held, as the witness recorded it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Held {
    /// The process that held it.
    pub pid: i32,
    /// Its device then.
    pub dev: u64,
    /// Its inode then.
    pub ino: u64,
    /// Its path.
    pub path: String,
}

impl Held {
    /// Its record line.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{}\t{}\t{}\t{}\n", self.pid, self.dev, self.ino, self.path)
    }

    /// A record line read back (None: not one).
    #[must_use]
    pub fn parse(line: &str) -> Option<Held> {
        let mut it = line.splitn(4, '\t');
        let pid = it.next()?.parse().ok()?;
        let dev = it.next()?.parse().ok()?;
        let ino = it.next()?.parse().ok()?;
        let path = it.next()?.to_string();
        (pid > 1 && path.starts_with('/')).then_some(Held {
            pid,
            dev,
            ino,
            path,
        })
    }
}

/// What kind of file stands at a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A unix-domain socket.
    Socket,
    /// Anything else: a directory, a link, …
    Other,
}

/// What [`View::stat`] reads of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    /// Its device.
    pub dev: u64,
    /// Its inode.
    pub ino: u64,
    /// Its kind.
    pub kind: Kind,
    /// Its owner.
    pub uid: u32,
    /// Its birth time, in seconds.
    pub born: i64,
}

/// The filesystem as [`claims`] reads it ([`Disk`]: the real one).
pub trait View {
    /// What stands at `path`, not followed (None: nothing, or unreadable).
    fn stat(&self, path: &str) -> Option<Stat>;
    /// The contents of the regular file at `path` (None: unreadable).
    fn read(&self, path: &str) -> Option<String>;
    /// Absence-sensitive entry read: `Err` is unknown, never missing.
    fn inspect(&self, path: &str) -> std::io::Result<Option<Stat>>;
    /// Immediate child paths of a directory; `None` for a non-directory.
    fn children(&self, _path: &str) -> std::io::Result<Option<Scan<Vec<String>>>> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// Real filesystem view that does not follow links.
pub struct Disk;

impl View for Disk {
    fn stat(&self, path: &str) -> Option<Stat> {
        self.inspect(path).ok().flatten()
    }

    fn inspect(&self, path: &str) -> std::io::Result<Option<Stat>> {
        let Some(st) = crate::sys::entry_stat(Path::new(path))? else {
            return Ok(None);
        };
        let kind = match st.kind {
            libc::S_IFREG => Kind::File,
            libc::S_IFSOCK => Kind::Socket,
            _ => Kind::Other,
        };
        Ok(Some(Stat {
            dev: st.dev,
            ino: st.ino,
            kind,
            uid: st.uid,
            born: st.born,
        }))
    }

    fn read(&self, path: &str) -> Option<String> {
        let b = crate::sys::scratch_read(Path::new(path)).ok()??;
        String::from_utf8(b).ok()
    }

    fn children(&self, path: &str) -> std::io::Result<Option<Scan<Vec<String>>>> {
        let Some(st) = crate::sys::entry_stat(Path::new(path))? else {
            return Ok(None);
        };
        if st.kind != libc::S_IFDIR {
            return Ok(None);
        }
        let mut out = Scan {
            entries: Vec::new(),
            complete: true,
        };
        for entry in std::fs::read_dir(path)? {
            match entry {
                Ok(entry) => out
                    .entries
                    .push(entry.path().to_string_lossy().into_owned()),
                Err(_) => out.complete = false,
            }
        }
        Ok(Some(out))
    }
}

/// The `(pid, path)` pairs of `held` whose path lies strictly below `root`, launchd's never.
#[must_use]
pub fn held_below(held: &[(i32, String)], root: &str) -> Vec<(i32, String)> {
    let prefix = format!("{root}/");
    held.iter()
        .filter(|(pid, path)| *pid > 1 && path.len() > prefix.len() && path.starts_with(&prefix))
        .cloned()
        .collect()
}

/// What `pids` hold below `root`, read in this process with no reference taken on any socket
/// or kqueue of theirs: each file one holds open through a descriptor
/// ([`aterm_uds::holders::open_files`]), and each socket directly below `root` whose listener
/// one bound, by the kernel's table of unix-domain listeners
/// ([`aterm_uds::holders::scan_listener_holders`]). A scan that does not answer (a table that
/// does not read, a connection that does not answer in time) adds nothing: the record keeps only
/// what is known held.
fn held_by(pids: &[i32], root: &str) -> Vec<(i32, String)> {
    use std::os::unix::fs::FileTypeExt as _;
    let mut held = Vec::new();
    for &pid in pids {
        let Ok(n) = u32::try_from(pid) else { continue };
        for path in aterm_uds::holders::open_files(n).unwrap_or_default() {
            held.push((pid, path.to_string_lossy().into_owned()));
        }
    }
    let sockets = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
        .map(|e| e.path());
    for socket in sockets {
        let Ok(scan) = aterm_uds::holders::scan_listener_holders(&socket, &[]) else {
            continue;
        };
        for pid in scan.holders {
            if let Ok(pid) = i32::try_from(pid)
                && pids.contains(&pid)
            {
                held.push((pid, socket.to_string_lossy().into_owned()));
            }
        }
    }
    held
}

/// What an entry is, by its path below its root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `ms-<hex>.sock.lease`.
    Lease,
    /// `ms-<hex>.sock`.
    Socket,
    /// `sessions/<id>.json`.
    Session,
}

fn shape(rel: &str) -> Option<Shape> {
    let hex = |h: &str| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit());
    if let Some(name) = rel.strip_prefix("sessions/") {
        let id = name.strip_suffix(".json")?;
        return (!id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .then_some(Shape::Session);
    }
    let stem = rel.strip_prefix("ms-")?;
    if let Some(h) = stem.strip_suffix(".sock.lease") {
        return hex(h).then_some(Shape::Lease);
    }
    stem.strip_suffix(".sock")
        .filter(|h| hex(h))
        .map(|_| Shape::Socket)
}

/// The root `path` lies below, its path below that root and its shape (None:
/// below no root, or of no shape).
fn below<'a>(path: &'a str, roots: &'a [String]) -> Option<(&'a str, &'a str, Shape)> {
    roots.iter().find_map(|r| {
        path.strip_prefix(r.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rel| shape(rel).map(|s| (r.as_str(), rel, s)))
    })
}

/// Recorded entries belonging to this run (`uid`, born at or after `since`), in path order.
#[must_use]
pub fn claims(held: &[Held], roots: &[String], uid: u32, since: i64, fs: &dyn View) -> Vec<Held> {
    let same = |h: &Held, kind: Kind| {
        fs.stat(&h.path).is_some_and(|s| {
            s.dev == h.dev && s.ino == h.ino && s.kind == kind && s.uid == uid && s.born >= since
        })
    };
    let names = |h: &Held, key: &str, want: J| {
        fs.read(&h.path)
            .and_then(|t| J::parse(&t).ok())
            .is_some_and(|j| j.get(key) == Some(&want))
    };
    let mut out: Vec<Held> = Vec::new();
    let mut sockets: Vec<(String, i32)> = Vec::new();
    for h in held {
        match below(&h.path, roots) {
            Some((root, rel, Shape::Lease))
                if same(h, Kind::File) && names(h, "pid", J::Int(i64::from(h.pid))) =>
            {
                sockets.push((
                    format!("{root}/{}", rel.strip_suffix(".lease").unwrap_or(rel)),
                    h.pid,
                ));
                out.push(h.clone());
            }
            Some((_, _, Shape::Session))
                if same(h, Kind::File)
                    && names(
                        h,
                        "process_generation_hint",
                        J::Str(format!("pid={}", h.pid)),
                    ) =>
            {
                out.push(h.clone());
            }
            _ => {}
        }
    }
    for h in held {
        if matches!(below(&h.path, roots), Some((_, _, Shape::Socket)))
            && sockets.contains(&(h.path.clone(), h.pid))
            && same(h, Kind::Socket)
        {
            out.push(h.clone());
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.pid.cmp(&b.pid)));
    out.dedup_by(|a, b| a.path == b.path && a.dev == b.dev && a.ino == b.ino);
    out
}

/// The record of `run_dir`, each entry once.
#[must_use]
pub fn read_record(run_dir: &Path) -> Vec<Held> {
    let set: BTreeSet<Held> = crate::util::read_regular_text(&run_dir.join(RECORD))
        .unwrap_or_default()
        .lines()
        .filter_map(Held::parse)
        .collect();
    set.into_iter().collect()
}

/// Appends new entries below each root held by `members` ([`held_by`]) to `run_dir`'s record.
pub fn witness(run_dir: &Path, members: &[i32], seen: &mut BTreeSet<Held>) {
    let live: Vec<i32> = members.iter().copied().filter(|&p| p > 1).collect();
    if live.is_empty() {
        return;
    }
    for root in roots(crate::sys::getuid()) {
        let is_dir = crate::sys::entry_stat(Path::new(&root))
            .ok()
            .flatten()
            .is_some_and(|s| s.kind == libc::S_IFDIR);
        if !is_dir {
            continue;
        }
        let mut lines = String::new();
        for (pid, path) in held_below(&held_by(&live, &root), &root) {
            let Some(st) = Disk.stat(&path) else { continue };
            if !matches!(st.kind, Kind::File | Kind::Socket) {
                continue;
            }
            let h = Held {
                pid,
                dev: st.dev,
                ino: st.ino,
                path,
            };
            if seen.insert(h.clone()) {
                lines.push_str(&h.line());
            }
        }
        if !lines.is_empty()
            && let Ok(mut f) = crate::util::append_regular_file(&run_dir.join(RECORD))
        {
            let _ = f.write_all(lines.as_bytes());
        }
    }
}

/// The birth time of `run_dir`: when the lab came up (unreadable: the far
/// future, so that nothing is claimed).
fn run_born(run_dir: &Path) -> i64 {
    crate::sys::entry_stat(run_dir)
        .ok()
        .flatten()
        .map_or(i64::MAX, |s| s.born)
}

/// The entries of this run [`claims`] finds now in the record of `run_dir`.
#[must_use]
pub fn claimed(run_dir: &Path) -> Vec<Held> {
    let held = read_record(run_dir);
    if held.is_empty() {
        return Vec::new();
    }
    let uid = crate::sys::getuid();
    claims(&held, &roots(uid), uid, run_born(run_dir), &Disk)
}

/// Removes [`claimed`] entries while their file identity matches, returning removed paths.
#[must_use]
pub fn clean(run_dir: &Path) -> Vec<String> {
    claimed(run_dir)
        .into_iter()
        .filter(|h| crate::sys::remove_if_same(Path::new(&h.path), h.dev, h.ino).unwrap_or(false))
        .map(|h| h.path)
        .collect()
}

/// A partial observation and whether all of its reads succeeded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan<T> {
    /// Positive evidence remains useful even when another read failed.
    pub entries: T,
    /// Only a complete observation can establish an absence.
    pub complete: bool,
}

fn standing_with(claimed: &[Held], fs: &dyn View) -> Scan<Vec<String>> {
    let mut out = Scan {
        entries: Vec::new(),
        complete: true,
    };
    for h in claimed {
        match fs.inspect(&h.path) {
            Ok(Some(s)) if s.dev == h.dev && s.ino == h.ino => out.entries.push(h.path.clone()),
            Err(_) => out.complete = false,
            _ => {}
        }
    }
    out
}

/// One entry of a listing of the roots ([`listing`]): its path and identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    /// Its path.
    pub path: String,
    /// Its device.
    pub dev: u64,
    /// Its inode.
    pub ino: u64,
}

/// Lists every observed entry under `roots` up to four directory levels without following links.
#[must_use]
pub fn listing(roots: &[String]) -> Scan<BTreeSet<Entry>> {
    listing_with(roots, &Disk)
}

fn listing_with(roots: &[String], fs: &dyn View) -> Scan<BTreeSet<Entry>> {
    fn walk(path: &str, depth: u32, fs: &dyn View, out: &mut Scan<BTreeSet<Entry>>) {
        let st = match fs.inspect(path) {
            Ok(Some(st)) => st,
            Ok(None) => return,
            Err(_) => {
                out.complete = false;
                return;
            }
        };
        out.entries.insert(Entry {
            path: path.to_string(),
            dev: st.dev,
            ino: st.ino,
        });
        match fs.children(path) {
            Ok(Some(children)) => {
                out.complete &= children.complete;
                if depth > 0 {
                    for child in children.entries {
                        walk(&child, depth - 1, fs, out);
                    }
                } else if !children.entries.is_empty() {
                    out.complete = false;
                }
            }
            Err(_) => out.complete = false,
            _ => {}
        }
    }
    let mut out = Scan {
        entries: BTreeSet::new(),
        complete: true,
    };
    for root in roots {
        walk(root, 4, fs, &mut out);
    }
    out
}

/// The registry's shared lock, below its root.
const LOCK: &str = ".session-registry.mutation.lock";

/// Entries present in a post-`down` listing of the roots that were absent before `up`.
#[derive(Debug, PartialEq, Eq)]
pub struct Since {
    /// Unexcused entries left behind.
    pub left: Vec<String>,
    /// Entries naming a live process outside the jail (`alive`), or sockets of such leases.
    pub theirs: Vec<String>,
    /// Shared registry directories and lock files created on first run.
    pub shared: Vec<String>,
    /// Every absence-sensitive entry recheck completed.
    pub complete: bool,
}

impl Default for Since {
    fn default() -> Self {
        Self {
            left: Vec::new(),
            theirs: Vec::new(),
            shared: Vec::new(),
            complete: true,
        }
    }
}

/// Classifies entries in `after` not present in `before` below `roots` into [`Since`].
#[must_use]
pub fn new_since(
    before: &BTreeSet<Entry>,
    after: &BTreeSet<Entry>,
    roots: &[String],
    fs: &dyn View,
    alive: &dyn Fn(i32) -> bool,
) -> Since {
    let names_live = |path: &str, key: &str| {
        fs.read(path)
            .and_then(|t| J::parse(&t).ok())
            .and_then(|j| match j.get(key)? {
                J::Int(p) => i32::try_from(*p).ok(),
                J::Str(s) => s.strip_prefix("pid=")?.parse().ok(),
                _ => None,
            })
            .is_some_and(|p| p > 1 && alive(p))
    };
    let mut out = Since::default();
    for e in after.difference(before) {
        let kind = match fs.inspect(&e.path) {
            Ok(Some(st)) => st.kind,
            Ok(None) => continue,
            Err(_) => {
                out.complete = false;
                continue;
            }
        };
        let rel = roots.iter().find_map(|r| {
            e.path
                .strip_prefix(r.as_str())
                .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                .map(|rest| rest.trim_start_matches('/'))
        });
        let Some(rel) = rel else { continue };
        let slot = match (rel, kind) {
            ("" | "sessions", Kind::Other) | (LOCK, Kind::File) => &mut out.shared,
            _ => match shape(rel) {
                Some(Shape::Lease) if names_live(&e.path, "pid") => &mut out.theirs,
                Some(Shape::Session) if names_live(&e.path, "process_generation_hint") => {
                    &mut out.theirs
                }
                Some(Shape::Socket) if names_live(&format!("{}.lease", e.path), "pid") => {
                    &mut out.theirs
                }
                _ => &mut out.left,
            },
        };
        slot.push(e.path.clone());
    }
    out
}

/// Post-`down` diff and standing claimed entries across `before`, `after`, and `claimed`.
pub fn after_down(
    before: &Scan<BTreeSet<Entry>>,
    after: &Scan<BTreeSet<Entry>>,
    claimed: &[Held],
    roots: &[String],
    fs: &dyn View,
    alive: &dyn Fn(i32) -> bool,
) -> Since {
    let mut out = new_since(&before.entries, &after.entries, roots, fs, alive);
    let standing = standing_with(claimed, fs);
    out.left.extend(standing.entries);
    out.left.sort();
    out.left.dedup();
    out.complete &= before.complete && after.complete && standing.complete;
    out
}

#[cfg(test)]
#[path = "runtime_dirs_tests.rs"]
mod tests;
