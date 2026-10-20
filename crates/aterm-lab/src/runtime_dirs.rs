// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-user runtime directories an agent under test writes in the shared
//! `/tmp`, which none of the variables tried moves, and what `down` takes
//! back from them.
//!
//! Muse Code 1.4.10 keeps its local Session registry in
//! `/private/tmp/tbh-<uid>-rt/muse` whatever `TMPDIR`, `TMP`, `TEMP`,
//! `XDG_RUNTIME_DIR` or `HOME` say (measured: each pointed into the scratch,
//! the registry stayed there; `TMPDIR` moves only its other temporary
//! files). Each process makes a lease `ms-<hex>.sock.lease` (`{"pid": <pid>,
//! …}`), the socket `ms-<hex>.sock` beside it and a session record
//! `sessions/<id>.json` (`"process_generation_hint": "pid=<pid>"`). Every Muse
//! Code of this user shares the directory, the owner's and other labs' among
//! them; a clean exit usually removes its own three, but not always (leases
//! of cleanly exited processes were seen left), and a TERM, a HUP or a KILL
//! (which `down` sends) leaves them: `down` takes back what the record names
//! whichever way the process ended.
//!
//! The witness ([`crate::sampler`]) asks `lsof` each second which entries under
//! each root the jail's processes hold, and records each with its identity in
//! `<run>/runtime.txt` (`pid TAB device TAB inode TAB path`); `down` asks once
//! more before it ends them. Once the jail is empty, `down` removes an entry
//! when ALL of these hold ([`claims`]), and nothing else: never a directory,
//! the registry's shared lock, or an entry of a process of another jail.
//!
//! * a process of this jail held it (the record);
//! * it names that process: a lease's `pid`, a session record's
//!   `process_generation_hint`, or it is the socket of such a lease;
//! * it was made after the lab came up (its birth time is not before the run
//!   directory's); and
//! * the same file (device and inode) of this user still stands at that path,
//!   reached through no link.
//!
//! What the witness never saw (an entry of another shape, a process it missed)
//! is not taken back, so `muse-selftest` checks from outside the record: a
//! [`listing`] of the roots before `up` and after `down`, whose difference
//! ([`new_since`]) may hold only entries naming a live Muse Code outside the
//! jail and the registry's shared directories and lock (which a first run
//! makes, and `down` leaves to every Muse Code of this user).

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
    /// A failed directory read, including any failed entry, is unknown.
    fn children(&self, _path: &str) -> std::io::Result<Option<Scan<Vec<String>>>> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// The real filesystem, read through no link ([`crate::sys::entry_stat`],
/// [`crate::sys::scratch_read`]).
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

/// The `(pid, path)` pairs of `lsof -F pn` output whose path lies strictly below `root`.
#[must_use]
pub fn held_below(lsof_f: &str, root: &str) -> Vec<(i32, String)> {
    let prefix = format!("{root}/");
    let mut pid = 0;
    let mut out = Vec::new();
    for l in lsof_f.lines() {
        if let Some(p) = l.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(path) = l.strip_prefix('n')
            && pid > 1
            && path.len() > prefix.len()
            && path.starts_with(&prefix)
        {
            out.push((pid, path.to_string()));
        }
    }
    out
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

/// What `down` takes back: the recorded entries that are this run's ([module
/// docs](self)), each once, in path order. `uid` is this user, `since` the
/// birth time of the run directory.
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
                sockets.push((format!("{root}/{}", rel.trim_end_matches(".lease")), h.pid));
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
    let set: BTreeSet<Held> = std::fs::read_to_string(run_dir.join(RECORD))
        .unwrap_or_default()
        .lines()
        .filter_map(Held::parse)
        .collect();
    set.into_iter().collect()
}

/// One pass of the witness: the entries below each root (one that is there)
/// that `members` hold, appended to the record once each (`seen` holds what
/// this caller recorded before).
pub fn witness(run_dir: &Path, members: &[i32], seen: &mut BTreeSet<Held>) {
    let live: Vec<String> = members
        .iter()
        .filter(|&&p| p > 1)
        .map(ToString::to_string)
        .collect();
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
        let Ok(out) = aterm_uds::spawnfd::command("/usr/sbin/lsof")
            .args(["-nP", "-a", "-p", &live.join(","), "+D", &root, "-F", "pn"])
            .output()
        else {
            continue;
        };
        let mut lines = String::new();
        for (pid, path) in held_below(&String::from_utf8_lossy(&out.stdout), &root) {
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
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(run_dir.join(RECORD))
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

/// Remove what [`claimed`] finds (`down`, once the jail is empty), each only
/// while it is still the same file; the paths removed.
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

/// Every observed entry under the roots, without following links; at most
/// four directory levels are entered. Unknown reads remain distinguishable.
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

/// What a listing of the roots taken after `down` holds that one taken before
/// `up` did not ([`new_since`]), by path.
#[derive(Debug, PartialEq, Eq)]
pub struct Since {
    /// What nothing excuses: an entry left behind.
    pub left: Vec<String>,
    /// A lease or session record that names a live process `alive` counts as
    /// another's (the owner's Muse Code, another lab's), or the socket of
    /// such a lease.
    pub theirs: Vec<String>,
    /// The registry's shared structure a first run makes when it was not
    /// there: a root, its `sessions` directory and its mutation lock, which
    /// every Muse Code of this user shares (so `down` leaves them).
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

/// Sort every entry of `after` that is not in `before` (by path and identity)
/// below one of `roots` into [`Since`]: `alive` says whether a pid an entry
/// names is a live process outside the jail.
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
        // gone since the listing: nothing is left there
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

/// The post-down observation: keep known leftovers even when another read
/// was unknown. Both listings, every recheck, and claimed-file reads matter.
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const ROOT: &str = "/private/tmp/tbh-501-rt/muse";
    const UID: u32 = 501;
    const UP: i64 = 1_000;

    /// A filesystem of entries and contents.
    #[derive(Default)]
    struct Fake {
        at: BTreeMap<String, (Stat, String)>,
        dirs: BTreeSet<String>,
        stat_errors: BTreeSet<String>,
        dir_errors: BTreeSet<String>,
        entry_errors: BTreeSet<String>,
    }

    impl Fake {
        fn put(&mut self, path: &str, ino: u64, kind: Kind, text: &str) {
            let st = Stat {
                dev: 1,
                ino,
                kind,
                uid: UID,
                born: UP + 5,
            };
            self.at.insert(path.to_string(), (st, text.to_string()));
        }
    }

    impl View for Fake {
        fn stat(&self, path: &str) -> Option<Stat> {
            self.at.get(path).map(|(s, _)| *s)
        }
        fn read(&self, path: &str) -> Option<String> {
            self.at.get(path).map(|(_, t)| t.clone())
        }
        fn inspect(&self, path: &str) -> std::io::Result<Option<Stat>> {
            if self.stat_errors.contains(path) {
                Err(std::io::Error::other("injected unavailable observation"))
            } else {
                Ok(self.stat(path))
            }
        }
        fn children(&self, path: &str) -> std::io::Result<Option<Scan<Vec<String>>>> {
            if self.dir_errors.contains(path) {
                return Err(std::io::Error::other("injected unavailable observation"));
            }
            if !self.dirs.contains(path) {
                return Ok(None);
            }
            let prefix = format!("{path}/");
            Ok(Some(Scan {
                entries: self
                    .at
                    .keys()
                    .filter(|p| {
                        p.strip_prefix(prefix.as_str())
                            .is_some_and(|rest| !rest.contains('/'))
                    })
                    .cloned()
                    .collect(),
                complete: !self.entry_errors.contains(path),
            }))
        }
    }

    fn held(pid: i32, ino: u64, rel: &str) -> Held {
        Held {
            pid,
            dev: 1,
            ino,
            path: format!("{ROOT}/{rel}"),
        }
    }

    /// One Muse Code's three entries, as 1.4.10 writes them.
    fn muse(fs: &mut Fake, pid: i32, hex: &str, id: &str, base: u64) -> Vec<Held> {
        fs.put(
            &format!("{ROOT}/ms-{hex}.sock.lease"),
            base,
            Kind::File,
            &format!(
                "{{\"schema_version\":1,\"endpoint_hint\":\"ms-{hex}.sock\",\"process_generation_hint\":\"pid={pid}\",\"pid\":{pid}}}"
            ),
        );
        fs.put(&format!("{ROOT}/ms-{hex}.sock"), base + 1, Kind::Socket, "");
        fs.put(
            &format!("{ROOT}/sessions/{id}.json"),
            base + 2,
            Kind::File,
            &format!(
                "{{\"schema_version\":1,\"session_id\":\"{id}\",\"endpoint_hint\":\"ms-{hex}.sock\",\"process_generation_hint\":\"pid={pid}\"}}"
            ),
        );
        vec![
            held(pid, base, &format!("ms-{hex}.sock.lease")),
            held(pid, base + 1, &format!("ms-{hex}.sock")),
            held(pid, base + 2, &format!("sessions/{id}.json")),
        ]
    }

    fn paths(v: &[Held]) -> Vec<String> {
        v.iter()
            .map(|h| h.path.trim_start_matches(ROOT).to_string())
            .collect()
    }

    /// The witness's record keeps a line per entry, and only its own shape.
    #[test]
    fn a_record_line_reads_back_and_lsof_names_what_lies_below_the_root() {
        let h = held(4242, 7, "ms-00ff.sock.lease");
        assert_eq!(Held::parse(h.line().trim_end()), Some(h));
        assert_eq!(
            Held::parse("1\t1\t1\t/x"),
            None,
            "launchd is never a holder"
        );
        assert_eq!(Held::parse("7\t1\t1\trelative"), None);
        let lsof = format!(
            "p4242\nn{ROOT}\nn{ROOT}/ms-00ff.sock.lease\nn{ROOT}/sessions\np17\nn{ROOT}-other/x\nn{ROOT}/ms-00ff.sock\n"
        );
        assert_eq!(
            held_below(&lsof, ROOT),
            [
                (4242, format!("{ROOT}/ms-00ff.sock.lease")),
                (4242, format!("{ROOT}/sessions")),
                (17, format!("{ROOT}/ms-00ff.sock")),
            ]
        );
    }

    /// THE LAB TAKES BACK ITS OWN MUSE CODE'S LEASE, SOCKET AND SESSION
    /// RECORD, AND NOTHING ELSE. NEGATIVE CONTROLS: another process's entries
    /// that a process of the jail held (a scan of the registry) and that name
    /// that other process, the registry's shared lock and its directories, an
    /// entry made before the lab came up, an entry another file replaced since
    /// the witness saw it, a link in an entry's place, another user's file,
    /// and a socket whose lease is not this run's all stay.
    #[test]
    fn the_lab_takes_back_its_own_muse_code_entries_and_nothing_else() {
        let mut fs = Fake::default();
        let mut rec = muse(&mut fs, 4242, "aa11", "01a1-mine", 10);
        // the owner's live Muse Code, whose lease and record the jail's process opened while scanning the registry
        let theirs = muse(&mut fs, 777, "bb22", "01a1-theirs", 20);
        rec.extend(theirs.iter().map(|h| Held {
            pid: 4242,
            ..h.clone()
        }));
        // the registry's own lock and directories
        fs.put(
            &format!("{ROOT}/.session-registry.mutation.lock"),
            30,
            Kind::File,
            "",
        );
        rec.push(held(4242, 30, ".session-registry.mutation.lock"));
        fs.put(&format!("{ROOT}/sessions"), 31, Kind::Other, "");
        rec.push(held(4242, 31, "sessions"));
        // a stale lease of an earlier process that had this pid, made before the lab came up
        fs.put(
            &format!("{ROOT}/ms-cc33.sock.lease"),
            40,
            Kind::File,
            "{\"pid\":4243}",
        );
        fs.at
            .get_mut(&format!("{ROOT}/ms-cc33.sock.lease"))
            .unwrap()
            .0
            .born = UP - 60;
        rec.push(held(4243, 40, "ms-cc33.sock.lease"));
        // a second Muse Code of this jail, whose lease was since replaced by another file of the same name
        rec.extend(muse(&mut fs, 5151, "dd44", "01a1-second", 50));
        fs.at
            .get_mut(&format!("{ROOT}/ms-dd44.sock.lease"))
            .unwrap()
            .0
            .ino = 99;
        // a third whose session record is another user's, and a fourth whose lease is now a link
        rec.extend(muse(&mut fs, 6161, "ee55", "01a1-third", 60));
        fs.at
            .get_mut(&format!("{ROOT}/sessions/01a1-third.json"))
            .unwrap()
            .0
            .uid = 0;
        rec.extend(muse(&mut fs, 7171, "ff66", "01a1-fourth", 70));
        fs.at
            .get_mut(&format!("{ROOT}/ms-ff66.sock.lease"))
            .unwrap()
            .0
            .kind = Kind::Other;
        // four more whose lease and record are this run's but whose socket is not the one the witness saw: another
        // file replaced it, it is another user's, it is no socket, or it was made before the lab came up
        rec.extend(muse(&mut fs, 8181, "a1a1", "01a1-fifth", 80));
        rec.extend(muse(&mut fs, 9191, "b2b2", "01a1-sixth", 90));
        rec.extend(muse(&mut fs, 3131, "c3c3", "01a1-seventh", 100));
        rec.extend(muse(&mut fs, 2121, "d4d4", "01a1-eighth", 110));
        for (hex, edit) in [
            ("a1a1", (|s: &mut Stat| s.ino = 199) as fn(&mut Stat)),
            ("b2b2", |s: &mut Stat| s.uid = 0),
            ("c3c3", |s: &mut Stat| s.kind = Kind::File),
            ("d4d4", |s: &mut Stat| s.born = UP - 60),
        ] {
            edit(&mut fs.at.get_mut(&format!("{ROOT}/ms-{hex}.sock")).unwrap().0);
        }
        let got = claims(&rec, &[ROOT.to_string()], UID, UP, &fs);
        assert_eq!(
            paths(&got),
            [
                "/ms-a1a1.sock.lease",
                "/ms-aa11.sock",
                "/ms-aa11.sock.lease",
                "/ms-b2b2.sock.lease",
                "/ms-c3c3.sock.lease",
                "/ms-d4d4.sock.lease",
                "/ms-ee55.sock",
                "/ms-ee55.sock.lease",
                "/sessions/01a1-eighth.json",
                "/sessions/01a1-fifth.json",
                "/sessions/01a1-fourth.json",
                "/sessions/01a1-mine.json",
                "/sessions/01a1-second.json",
                "/sessions/01a1-seventh.json",
                "/sessions/01a1-sixth.json",
            ]
        );
        assert!(
            got.iter()
                .all(|h| [4242, 5151, 6161, 7171, 8181, 9191, 3131, 2121].contains(&h.pid))
        );
        // the control: with no record, nothing is claimed
        assert!(claims(&[], &[ROOT.to_string()], UID, UP, &fs).is_empty());
        // and an entry outside every root is never claimed, whatever it names
        let outside = Held {
            path: "/private/tmp/other/ms-aa11.sock.lease".into(),
            ..rec[0].clone()
        };
        fs.at.insert(
            outside.path.clone(),
            fs.at[&format!("{ROOT}/ms-aa11.sock.lease")].clone(),
        );
        assert!(claims(&[outside], &[ROOT.to_string()], UID, UP, &fs).is_empty());
    }

    fn ent(rel: &str, ino: u64) -> Entry {
        Entry {
            path: if rel.is_empty() {
                ROOT.to_string()
            } else {
                format!("{ROOT}/{rel}")
            },
            dev: 1,
            ino,
        }
    }

    fn rels(v: &[String]) -> Vec<&str> {
        v.iter()
            .map(|p| p.trim_start_matches(ROOT).trim_start_matches('/'))
            .collect()
    }

    /// A LISTING BEFORE UP AND AFTER DOWN SHOWS WHAT IS LEFT, whatever the
    /// witness saw: an entry new since up is left unless it names a live Muse
    /// Code outside the jail (a lease's `pid`, a record's
    /// `process_generation_hint`, the socket of such a lease) or is the
    /// registry's shared structure a first run makes. NEGATIVE CONTROLS: an
    /// entry of no known shape, a socket whose lease is gone, entries naming
    /// a dead pid, a `sessions` that is no directory and a lock that is no
    /// file are left; an entry gone since the listing, an entry outside the
    /// root and one unchanged since up are not counted.
    #[test]
    fn a_listing_before_up_and_after_down_shows_what_is_left() {
        let mut fs = Fake::default();
        let mut before = BTreeSet::new();
        // the registry as it stood before up: its structure and the owner's live Muse Code (pid 777)
        fs.put(ROOT, 1, Kind::Other, "");
        fs.put(&format!("{ROOT}/sessions"), 2, Kind::Other, "");
        fs.put(&format!("{ROOT}/{LOCK}"), 3, Kind::File, "");
        for (rel, ino) in [("", 1), ("sessions", 2), (LOCK, 3)] {
            before.insert(ent(rel, ino));
        }
        let theirs = muse(&mut fs, 777, "bb22", "theirs", 20);
        before.extend(theirs.iter().map(|h| Entry {
            path: h.path.clone(),
            dev: h.dev,
            ino: h.ino,
        }));
        let mut after = before.clone();
        // left by this run's Muse Code (pid 4242, dead), and a temporary file of no known shape
        for h in muse(&mut fs, 4242, "aa11", "mine", 10) {
            after.insert(Entry {
                path: h.path,
                dev: h.dev,
                ino: h.ino,
            });
        }
        fs.put(&format!("{ROOT}/ms-aa11.sock.tmp"), 40, Kind::File, "");
        after.insert(ent("ms-aa11.sock.tmp", 40));
        // another lab's live Muse Code (pid 778), and the owner's record rewritten in place (a new inode)
        for h in muse(&mut fs, 778, "cc33", "other", 30) {
            after.insert(Entry {
                path: h.path,
                dev: h.dev,
                ino: h.ino,
            });
        }
        fs.at
            .get_mut(&format!("{ROOT}/sessions/theirs.json"))
            .unwrap()
            .0
            .ino = 23;
        after.remove(&ent("sessions/theirs.json", 22));
        after.insert(ent("sessions/theirs.json", 23));
        // a socket whose lease is gone, an entry gone since the listing, one outside the root
        fs.put(&format!("{ROOT}/ms-ee55.sock"), 50, Kind::Socket, "");
        after.insert(ent("ms-ee55.sock", 50));
        after.insert(ent("ms-ff66.sock.lease", 60));
        let outside = "/private/tmp/tbh-501-rt/muse-other/ms-aa11.sock.lease";
        fs.put(outside, 70, Kind::File, "{\"pid\":4242}");
        after.insert(Entry {
            path: outside.into(),
            dev: 1,
            ino: 70,
        });
        let alive = |p: i32| p == 777 || p == 778;
        let got = new_since(&before, &after, &[ROOT.to_string()], &fs, &alive);
        assert_eq!(
            rels(&got.left),
            [
                "ms-aa11.sock",
                "ms-aa11.sock.lease",
                "ms-aa11.sock.tmp",
                "ms-ee55.sock",
                "sessions/mine.json",
            ]
        );
        assert_eq!(
            rels(&got.theirs),
            [
                "ms-cc33.sock",
                "ms-cc33.sock.lease",
                "sessions/other.json",
                "sessions/theirs.json",
            ]
        );
        assert!(got.shared.is_empty());
        // the control: nothing new, nothing counted
        assert_eq!(
            new_since(&before, &before, &[ROOT.to_string()], &fs, &alive),
            Since::default()
        );
        // a first run made the registry: its root, `sessions` and the lock are shared structure, unless of another kind
        let none = BTreeSet::new();
        let first: BTreeSet<Entry> = [ent("", 1), ent("sessions", 2), ent(LOCK, 3)].into();
        let got = new_since(&none, &first, &[ROOT.to_string()], &fs, &alive);
        assert_eq!(rels(&got.shared), ["", LOCK, "sessions"]);
        assert!(got.left.is_empty() && got.theirs.is_empty());
        fs.at.get_mut(&format!("{ROOT}/sessions")).unwrap().0.kind = Kind::File;
        fs.at.get_mut(&format!("{ROOT}/{LOCK}")).unwrap().0.kind = Kind::Other;
        let got = new_since(&none, &first, &[ROOT.to_string()], &fs, &alive);
        assert_eq!(rels(&got.shared), [""]);
        assert_eq!(rels(&got.left), [LOCK, "sessions"]);
    }

    /// Only the three shapes Muse Code makes per process are ever taken: a
    /// name with a path too many, a dot file or a non-hex endpoint is not.
    #[test]
    fn only_a_lease_its_socket_and_a_session_record_have_a_shape() {
        assert_eq!(shape("ms-0a1b.sock.lease"), Some(Shape::Lease));
        assert_eq!(shape("ms-0a1b.sock"), Some(Shape::Socket));
        assert_eq!(
            shape("sessions/01a14930-f573-7ce1.json"),
            Some(Shape::Session)
        );
        for not in [
            "",
            "sessions",
            "sessions/",
            "sessions/.json",
            "sessions/a/b.json",
            "sessions/../x.json",
            ".session-registry.mutation.lock",
            "ms-.sock",
            "ms-xyz.sock",
            "ms-0a1b.sock.lease/x",
            "sub/ms-0a1b.sock",
        ] {
            assert_eq!(shape(not), None, "{not}");
        }
    }
    /// Actual traversal/recheck/post-down verdict: clean and absent are known;
    /// stat, directory and claimed-file errors cannot certify an absence.
    #[test]
    fn a_runtime_absence_needs_every_listing_and_recheck() {
        use crate::codex_proof::Verdict;
        let roots = [ROOT.to_string()];
        let mut fs = Fake::default();
        let absent = listing_with(&roots, &fs);
        assert!(absent.complete && absent.entries.is_empty());
        fs.put(ROOT, 1, Kind::Other, "");
        fs.dirs.insert(ROOT.to_string());
        let clean = listing_with(&roots, &fs);
        let made: Vec<String> = ["ms-aa.sock", "ms-aa.sock.lease", "sessions/mine.json"]
            .iter()
            .map(|p| format!("{ROOT}/{p}"))
            .collect();
        let judge = |before: &Scan<BTreeSet<Entry>>,
                     after: &Scan<BTreeSet<Entry>>,
                     held: &[Held],
                     fs: &Fake| {
            let got = after_down(before, after, held, &roots, fs, &|_| false);
            crate::muse_proof::runtime_verdict(&made, &got.left, got.complete)
        };
        assert_eq!(judge(&absent, &clean, &[], &fs), Verdict::Ok);
        let h = held(7, 2, "ms-aa.sock");
        fs.put(&h.path, h.ino, Kind::Socket, "");
        let dirty = listing_with(&roots, &fs);
        assert_eq!(
            judge(&clean, &dirty, std::slice::from_ref(&h), &fs),
            Verdict::Fail
        );
        // A genuinely absent claimed file does not count as an unknown read.
        let missing = held(7, 3, "ms-bb.sock");
        assert!(standing_with(std::slice::from_ref(&missing), &fs).complete);
        let mut unexpected = Vec::new();
        // Each error used to disappear from an ordinary empty/clean result.
        fs.stat_errors.insert(ROOT.to_string());
        let unknown = listing_with(&roots, &fs);
        if judge(&clean, &unknown, &[], &fs) != Verdict::Unproven {
            unexpected.push("root stat");
        }
        if judge(&unknown, &clean, &[], &fs) != Verdict::Unproven {
            unexpected.push("before snapshot");
        }
        fs.stat_errors.clear();
        fs.dir_errors.insert(ROOT.to_string());
        let unknown = listing_with(&roots, &fs);
        if judge(&clean, &unknown, &[], &fs) != Verdict::Unproven {
            unexpected.push("directory entries");
        }
        fs.dir_errors.clear();
        fs.stat_errors.insert(h.path.clone());
        if judge(&clean, &dirty, &[], &fs) != Verdict::Unproven {
            unexpected.push("entry recheck");
        }
        if judge(&clean, &clean, std::slice::from_ref(&h), &fs) != Verdict::Unproven {
            unexpected.push("claimed file");
        }
        // A directory iterator can keep positive entries and still report an error.
        fs.stat_errors.clear();
        let saved = fs.at.remove(&h.path).unwrap();
        fs.entry_errors.insert(ROOT.to_string());
        let unknown = listing_with(&roots, &fs);
        if judge(&clean, &unknown, &[], &fs) != Verdict::Unproven {
            unexpected.push("directory entry");
        }
        fs.at.insert(h.path.clone(), saved);
        let partial = listing_with(&roots, &fs);
        assert_eq!(
            judge(&clean, &partial, std::slice::from_ref(&h), &fs),
            Verdict::Fail
        );
        fs.entry_errors.clear();
        // Known positive evidence still fails even with an unrelated unknown.
        fs.stat_errors.clear();
        fs.stat_errors.insert(missing.path.clone());
        let both = [h, missing];
        assert_eq!(judge(&clean, &dirty, &both, &fs), Verdict::Fail);
        assert!(
            unexpected.is_empty(),
            "LAB-ABSENCE-COMPLETENESS: {unexpected:?}"
        );
    }
}
