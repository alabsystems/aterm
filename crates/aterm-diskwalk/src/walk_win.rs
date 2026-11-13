// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOWS CURSOR: the walk's handle-relative arm. A root is opened from its drive's root one
//! name at a time, each as itself: a junction or a symbolic link on the way is a refusal, never a
//! hop, and a volume mounted at a folder is entered (at its own root) only where the mount table
//! names it there as a local fixed volume. The cursor keeps one open directory handle per level,
//! whose listing the kernel resumes where the last slice left it, until its host releases them
//! ([`Walk::release`]): an open handle fails a rename of every directory above it.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::OwnedHandle;
use std::path::{Component, Path, PathBuf, Prefix};

use super::{LevelRow, Link, LinkTotals, Policy, SizeFull, Status, Walk};
use crate::refuse::{Refusal, safe_mount_at, unsafe_mounts_in};
use crate::tree::{DirId, Sums, Tree};
use crate::win::{self, Entry, Info, Kind, LinkReads, Lister, Stat};

/// The listed size from which a sizing walk reads a file by name (its link count, and its size
/// and allocation as they are now); it reads a file listed as empty too, which is what a file
/// created and still open is listed as. Any other file is counted as one link at the size the
/// directory's index holds, so a sizing walk's sum is off by:
/// - over, by less than this per name, for each hard link smaller than this (it counts whole at
///   each of its names);
/// - under, by what it grew since it was last closed, for a file that is open and growing and
///   was neither empty nor this big at its last close, and for a hard link's other name when the
///   file grew through the first. NTFS brings a name's index record up to date when a handle
///   opened through that name closes (or the name is read by name, as an exact walk reads every
///   one), so this lasts as long as the file stays open and nothing else looks at it.
///
/// A walk with [`Policy::links_out`] or [`Policy::private`] reads every file and has neither.
pub(crate) const LINK_READ_FLOOR: u64 = 1 << 20;

/// Which files a walk under `policy` reads by name: every one when the walk is asked what
/// deleting the tree frees ([`Policy::links_out`], [`Policy::private`]), else the big and the
/// empty-listed ones.
const fn link_reads(policy: &Policy) -> LinkReads {
    if policy.links_out || policy.private {
        LinkReads::Every
    } else {
        LinkReads::From(LINK_READ_FLOOR)
    }
}

/// One directory of the cursor.
pub(super) struct Frame {
    /// `None` while released ([`Walk::release`]): opened again by path when the cursor is back
    /// at this directory.
    handle: Option<OwnedHandle>,
    /// Its file id as its first open read it: what a handle opened again must read.
    ino: u64,
    dir: DirId,
    path: PathBuf,
    /// `None` once its listing ended (the buffer went back to the spares).
    lister: Option<Lister>,
    /// Child directories listed and not yet entered.
    pending: Vec<(Box<[u16]>, DirId)>,
}

pub(super) enum Unit {
    /// A listing call or a child open: the clock is read after it.
    Called,
    /// A finished directory folded into its parent: no syscall.
    Folded,
    /// Nothing left.
    Empty,
}

fn wide(name: &OsStr) -> Vec<u16> {
    name.encode_wide().collect()
}

/// A look's failure as a refusal: a Windows without the by-name read has no looks.
pub(crate) const fn stat_refusal(code: u32) -> Refusal {
    match code {
        win::ERROR_NOT_SUPPORTED => Refusal::Unsupported,
        c => win::code_refusal(c),
    }
}

/// Open the directory `name` inside `parent` as itself, and read what it is.
fn open_child(parent: &OwnedHandle, name: &[u16]) -> Result<(OwnedHandle, Info), Refusal> {
    let handle = win::open_dir_at(parent, name).map_err(|code| match code {
        win::ERROR_DIRECTORY => link_or_file(parent, name),
        c => win::code_refusal(c),
    })?;
    let info = win::info_of(&handle).map_err(win::code_refusal)?;
    if info.link {
        return Err(Refusal::Link);
    }
    Ok((handle, info))
}

fn link_or_file(parent: &OwnedHandle, name: &[u16]) -> Refusal {
    match win::stat_at(parent, name) {
        Ok(st) if st.kind() == Kind::Link => Refusal::Link,
        Err(code) if win::is_gone(code) => Refusal::Gone,
        _ => Refusal::NotDirectory,
    }
}

/// The root of the volume mounted at the folder `name` inside `parent`, which is `at`: entered
/// only when three things agree. The mount table names a local, fixed volume at exactly `at`;
/// the folder's own reparse data is a mount point whose target is a volume's root (a junction
/// to a directory is not); and that volume is the one the table named. Anything else is the
/// link it is, never followed.
fn mounted_volume(
    parent: &OwnedHandle,
    name: &[u16],
    at: &Path,
    policy: &Policy,
) -> Result<(OwnedHandle, Info), Refusal> {
    let vouched = safe_mount_at(at, &policy.mounts, policy.deny.fold_case())
        .and_then(|m| m.container.as_deref())
        .ok_or(Refusal::Link)?;
    let volume = win::volume_mount_at(parent, name).ok_or(Refusal::Link)?;
    if !volume.eq_ignore_ascii_case(vouched) {
        return Err(Refusal::Link);
    }
    let handle = win::open_volume(&volume)?;
    let info = win::info_of(&handle).map_err(win::code_refusal)?;
    if info.link {
        return Err(Refusal::Link);
    }
    Ok((handle, info))
}

/// Open `path` one component at a time from its drive's root, each as itself relative to its
/// parent, and never look a name up in a cloud placeholder on the way: the lookup could fetch it.
fn open_chain(path: &Path, policy: &Policy) -> Result<(OwnedHandle, Info), Refusal> {
    #[cfg(any(test, debug_assertions))]
    crate::touched::note(path);
    let mut parts = path.components();
    let letter = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => char::from(letter),
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return Err(Refusal::Remote),
            Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return Err(Refusal::Relative),
        },
        _ => return Err(Refusal::Relative),
    };
    if let Some(why) = win::drive_refusal(letter) {
        return Err(why);
    }
    let mut handle = win::open_drive(letter)?;
    let mut info = win::info_of(&handle).map_err(win::code_refusal)?;
    let mut at = PathBuf::from(format!("{letter}:\\"));
    for c in parts {
        match c {
            Component::RootDir => {}
            Component::Normal(name) => {
                if info.dataless {
                    return Err(Refusal::Dataless);
                }
                at.push(name);
                let name = wide(name);
                (handle, info) = match open_child(&handle, &name) {
                    Err(Refusal::Link) => mounted_volume(&handle, &name, &at, policy)?,
                    opened => opened?,
                };
            }
            _ => return Err(Refusal::Relative),
        }
    }
    Ok((handle, info))
}

/// [`open_checked_stat`]'s handle alone: what a marker or link look, a file root's directory and
/// an entry count open.
pub(crate) fn open_checked(path: &Path, policy: &Policy) -> Result<OwnedHandle, Refusal> {
    open_checked_stat(path, policy).map(|(handle, _)| handle)
}

/// [`open_chain`], then the cloud-placeholder test on the open handle, which every look into a
/// directory and every listing of one passes first; with the directory's volume and file id.
fn open_checked_stat(path: &Path, policy: &Policy) -> Result<(OwnedHandle, Info), Refusal> {
    let (handle, info) = open_chain(path, policy)?;
    if info.dataless {
        return Err(Refusal::Dataless);
    }
    Ok((handle, info))
}

pub(crate) fn directory_identity(path: &Path, policy: &Policy) -> Result<(u64, u64), Refusal> {
    let (_, info) = open_checked_stat(path, policy)?;
    Ok((info.dev, info.ino))
}

pub(super) fn begin(canon: PathBuf, policy: &Policy) -> Result<Walk, Refusal> {
    let (handle, info) = open_checked_stat(&canon, policy)?;
    let tree = Tree::new(canon.clone(), info.ino);
    let skip = unsafe_mounts_in(&canon, &policy.mounts, policy.deny.fold_case());
    let lister = Lister::new(Vec::new(), skip, link_reads(policy));
    let shares_blocks = win::volume_shares_blocks(&handle);
    Ok(Walk {
        generation: 0,
        tree,
        stack: vec![Frame {
            handle: Some(handle),
            ino: info.ino,
            dir: DirId::ROOT,
            path: canon,
            lister: Some(lister),
            pending: Vec::new(),
        }],
        spare: Vec::new(),
        root_dev: info.dev,
        shares_blocks,
        policy: policy.clone(),
        links: policy.links_out.then(HashMap::new),
        dirs_alloc: 0,
        link_totals: None,
        walked: 0,
        calls: 0,
        slices: 0,
        status: Status::Walking,
    })
}

/// Whether a walk asked for private sizes ([`Policy::private`]) claims one: only on a volume
/// that shares no blocks between files (NTFS), where a file's allocation is its own. On one that
/// can (ReFS block cloning), a clone lists its whole allocation with one link and nothing read
/// here says how much of it is shared, so no private size is given rather than an overstated one.
pub(crate) const fn claims_private(asked: bool, shares_blocks: bool) -> bool {
    asked && !shares_blocks
}

/// A file's sums as a walk and a file root count it: its fair share of a multiply linked file's
/// allocation, and a cloud placeholder's logical size apart.
fn file_sums(e: &Entry<'_>, private: bool) -> Sums {
    let nlink = u64::from(e.nlink.max(1));
    let mut s = Sums {
        files: 1,
        logical: e.logical,
        newest_mtime_s: e.mtime_s,
        ..Sums::default()
    };
    if e.dataless {
        s.dataless_logical = e.logical;
    } else {
        s.alloc_fair = e.alloc / nlink;
        if nlink <= 1 {
            s.released_floor = e.alloc;
        }
    }
    if private {
        // No blocks are shared between files here ([`claims_private`]): a file's private size
        // is its allocation.
        s.private = Some(if e.dataless { 0 } else { e.alloc / nlink });
    }
    s
}

struct Sink<'a> {
    tree: &'a mut Tree,
    links: Option<&'a mut HashMap<(u64, u64), Link>>,
    dirs_alloc: &'a mut u64,
    policy: &'a Policy,
    private: bool,
    walked: &'a mut u64,
    root_dev: u64,
    dir: DirId,
    path: &'a Path,
    pending: &'a mut Vec<(Box<[u16]>, DirId)>,
}

impl Sink<'_> {
    fn add_own(&mut self, s: &Sums) {
        let r = self.tree.rec_mut(self.dir);
        r.own.add(s);
        r.subtree.add(s);
    }

    fn entry(&mut self, e: &Entry<'_>) {
        *self.walked += 1;
        {
            let r = self.tree.rec_mut(self.dir);
            r.direct_entries = r.direct_entries.saturating_add(1);
        }
        if e.gone {
            self.add_own(&Sums {
                gone: 1,
                ..Sums::default()
            });
            return;
        }
        if e.kind == Kind::Dir {
            self.dir_entry(e);
            return;
        }
        if let Some(links) = self.links.as_deref_mut()
            && !e.dataless
            && e.nlink > 1
        {
            let l = links.entry((self.root_dev, e.ino)).or_insert(Link {
                nlink: e.nlink,
                seen: 0,
                alloc: e.alloc,
            });
            l.seen = l.seen.saturating_add(1);
        }
        self.add_own(&file_sums(e, self.private));
    }

    fn dir_entry(&mut self, e: &Entry<'_>) {
        self.add_own(&Sums {
            dirs: 1,
            newest_mtime_s: e.mtime_s,
            ..Sums::default()
        });
        let parent_depth = self.tree.get(self.dir).map_or(0, |r| r.depth);
        let name = OsString::from_wide(e.name);
        let child = self
            .tree
            .push_child(self.dir, name.as_encoded_bytes(), e.ino);
        let boundary = e.mount_point;
        let refused = if boundary {
            None
        } else if e.dataless {
            Some(Refusal::Dataless)
        } else if parent_depth.saturating_add(1) > self.policy.max_depth {
            Some(Refusal::TooDeep)
        } else {
            self.policy.deny.denies(&self.path.join(&name))
        };
        *self.dirs_alloc = self.dirs_alloc.saturating_add(e.alloc);
        let r = self.tree.rec_mut(child);
        r.own.alloc_fair = e.alloc;
        r.subtree.alloc_fair = e.alloc;
        r.own.newest_mtime_s = e.mtime_s;
        r.subtree.newest_mtime_s = e.mtime_s;
        if boundary || refused.is_some() {
            r.mount_boundary = boundary;
            r.refused = refused;
            r.complete = true;
            let sub = r.subtree;
            self.tree.rec_mut(self.dir).subtree.add(&sub);
        } else {
            self.pending.push((e.name.into(), child));
        }
    }
}

enum Next {
    Nothing,
    /// Boxed: a frame is large, and one allocation per directory entered is nothing beside its
    /// open.
    Push(Box<Frame>),
    Pop,
}

/// What a directory that could not be read (`why`) adds to the counts of what was missed.
fn missed(why: Refusal) -> Sums {
    match why {
        Refusal::Gone => Sums {
            gone: 1,
            ..Sums::default()
        },
        Refusal::Unreadable(_) => Sums {
            unreadable: 1,
            ..Sums::default()
        },
        _ => Sums::default(),
    }
}

impl Walk {
    /// [`Walk::release`]: close every handle of the cursor. Only its top directory can be part
    /// way through its listing, and it stays open when its place is too far in to find again
    /// ([`Lister::release`]).
    pub(super) fn release_handles(&mut self) {
        let Some((top, below)) = self.stack.split_last_mut() else {
            return;
        };
        for frame in below {
            frame.handle = None;
        }
        if top.lister.as_mut().is_none_or(Lister::release) {
            top.handle = None;
        }
    }

    /// Open the cursor's top directory again if a release closed it: by its path, one name at a
    /// time as the root was, and only if it is the directory it was (its volume and file id). One
    /// that is gone, or is another directory of that name, is recorded as such with what was
    /// counted in it, and the cursor goes back to its parent. Leaves the top directory open, or
    /// no directory at all.
    fn ensure_open(&mut self) {
        while let Some(top) = self.stack.last_mut() {
            if top.handle.is_some() {
                return;
            }
            let opened = open_checked_stat(&top.path, &self.policy).and_then(|(handle, info)| {
                if info.dev == self.root_dev && info.ino == top.ino {
                    Ok(handle)
                } else {
                    Err(Refusal::Gone)
                }
            });
            match opened {
                Ok(handle) => {
                    top.handle = Some(handle);
                    return;
                }
                Err(why) => self.lose_top(why),
            }
        }
    }

    /// The cursor's top directory cannot be opened again (`why`): it and the children it had
    /// listed and not entered are refused, and what was counted in it folds into its parent.
    fn lose_top(&mut self, why: Refusal) {
        let Some(lost) = self.stack.pop() else {
            return;
        };
        if let Some(lister) = lost.lister {
            self.spare.push(lister.into_buffer());
        }
        let miss = missed(why);
        for (_, child) in lost.pending {
            let r = self.tree.rec_mut(child);
            r.refused = Some(why);
            r.complete = true;
            let mut sub = r.subtree;
            sub.add(&miss);
            self.tree.rec_mut(lost.dir).subtree.add(&sub);
        }
        let r = self.tree.rec_mut(lost.dir);
        r.own.add(&miss);
        r.subtree.add(&miss);
        r.refused.get_or_insert(why);
        r.complete = true;
        let sub = r.subtree;
        if let Some(p) = r.parent {
            self.tree.rec_mut(p).subtree.add(&sub);
        }
    }

    /// One unit of work on the cursor's top directory.
    pub(super) fn unit(&mut self) -> Unit {
        self.ensure_open();
        let private = claims_private(self.policy.private, self.shares_blocks);
        let Walk {
            stack,
            tree,
            links,
            dirs_alloc,
            policy,
            walked,
            root_dev,
            spare,
            ..
        } = self;
        let Some(top) = stack.last_mut() else {
            return Unit::Empty;
        };
        let Frame {
            handle,
            dir,
            path,
            lister,
            pending,
            ..
        } = top;
        // `ensure_open` left the top directory open.
        let Some(handle) = handle.as_ref() else {
            return Unit::Empty;
        };
        let dir = *dir;
        let (unit, next) = if let Some(l) = lister.as_mut() {
            let mut sink = Sink {
                tree,
                links: links.as_mut(),
                dirs_alloc,
                policy,
                private,
                walked,
                root_dev: *root_dev,
                dir,
                path,
                pending,
            };
            match l.next_batch(handle, &mut |e| sink.entry(e)) {
                Ok(true) => {}
                Ok(false) => {
                    if let Some(l) = lister.take() {
                        spare.push(l.into_buffer());
                    }
                }
                Err(code) => {
                    let s = if win::is_gone(code) {
                        Sums {
                            gone: 1,
                            ..Sums::default()
                        }
                    } else {
                        Sums {
                            unreadable: 1,
                            ..Sums::default()
                        }
                    };
                    let r = tree.rec_mut(dir);
                    r.own.add(&s);
                    r.subtree.add(&s);
                    r.refused.get_or_insert(win::code_refusal(code));
                    if let Some(l) = lister.take() {
                        spare.push(l.into_buffer());
                    }
                }
            }
            (Unit::Called, Next::Nothing)
        } else if let Some((name, child)) = pending.pop() {
            let opened = open_child(handle, &name).and_then(|(chandle, info)| {
                if info.dataless {
                    return Err(Refusal::Dataless);
                }
                Ok((chandle, info))
            });
            let next = match opened {
                Ok((chandle, info)) if info.dev == *root_dev => {
                    let cpath = path.join(OsString::from_wide(&name));
                    let skip = unsafe_mounts_in(&cpath, &policy.mounts, policy.deny.fold_case());
                    let buf = spare.pop().unwrap_or_default();
                    Next::Push(Box::new(Frame {
                        handle: Some(chandle),
                        ino: info.ino,
                        dir: child,
                        path: cpath,
                        lister: Some(Lister::new(buf, skip, link_reads(policy))),
                        pending: Vec::new(),
                    }))
                }
                other => {
                    let why = other.err();
                    let r = tree.rec_mut(child);
                    match why {
                        None => r.mount_boundary = true,
                        Some(w) => r.refused = Some(w),
                    }
                    r.complete = true;
                    let mut sub = r.subtree;
                    match why {
                        Some(Refusal::Gone) => sub.gone += 1,
                        Some(Refusal::Unreadable(_)) => sub.unreadable += 1,
                        _ => {}
                    }
                    tree.rec_mut(dir).subtree.add(&sub);
                    Next::Nothing
                }
            };
            (Unit::Called, next)
        } else {
            (Unit::Folded, Next::Pop)
        };
        match next {
            Next::Nothing => {}
            Next::Push(frame) => {
                let r = tree.rec_mut(frame.dir);
                r.direct_entries = 0;
                r.direct_exec = 0;
                stack.push(*frame);
            }
            Next::Pop => {
                if let Some(done) = stack.pop() {
                    let r = tree.rec_mut(done.dir);
                    r.complete = true;
                    let sub = r.subtree;
                    if let Some(p) = r.parent {
                        tree.rec_mut(p).subtree.add(&sub);
                    }
                }
            }
        }
        unit
    }
}

/// A [`Stat`] as the entry a walk would have listed.
fn stat_entry<'a>(name: &'a [u16], st: &Stat) -> Entry<'a> {
    Entry {
        name,
        kind: st.kind(),
        ino: st.ino,
        nlink: if st.links == 0 { 1 } else { st.links },
        alloc: st.alloc,
        logical: st.logical,
        mtime_s: st.mtime_s,
        dataless: st.dataless(),
        mount_point: false,
        gone: false,
    }
}

/// The file `name` in the directory `parent`, sized as a walk sizes it; `None` when the name is a
/// directory.
pub(super) fn file_root(
    parent: &Path,
    name: &OsStr,
    policy: &Policy,
) -> Result<Option<SizeFull>, Refusal> {
    let handle = open_checked(parent, policy)?;
    let name = wide(name);
    let st = win::stat_at(&handle, &name).map_err(stat_refusal)?;
    let e = stat_entry(&name, &st);
    match e.kind {
        Kind::Link => return Err(Refusal::Link),
        Kind::Dir => return Ok(None),
        Kind::File => {}
    }
    let links = policy.links_out.then(|| {
        if e.dataless {
            LinkTotals::default()
        } else if e.nlink > 1 {
            LinkTotals {
                alloc_first_seen: e.alloc,
                shared_out: e.alloc,
                frees_at_most: 0,
                linked_inodes: 1,
            }
        } else {
            LinkTotals {
                alloc_first_seen: e.alloc,
                shared_out: 0,
                frees_at_most: e.alloc,
                linked_inodes: 0,
            }
        }
    });
    let private = claims_private(policy.private, win::volume_shares_blocks(&handle));
    Ok(Some(SizeFull {
        sums: file_sums(&e, private),
        complete: true,
        walked: 1,
        links,
    }))
}

fn level_row(handle: &OwnedHandle, canon: &Path, policy: &Policy, e: &Entry<'_>) -> LevelRow {
    let name = OsString::from_wide(e.name);
    let mut row = LevelRow {
        name,
        dir: true,
        direct_entries: None,
        alloc: 0,
        refused: None,
        mount_boundary: false,
    };
    if let Some(why) = policy.deny.denies(&canon.join(&row.name)) {
        row.refused = Some(why);
        return row;
    }
    if e.mount_point {
        row.mount_boundary = true;
        return row;
    }
    match e.kind {
        Kind::Dir => row.refused = e.dataless.then_some(Refusal::Dataless),
        Kind::Link => {
            row.dir = false;
            row.alloc = e.alloc;
        }
        Kind::File => {
            row.dir = false;
            row.alloc = e.alloc;
            if !e.dataless {
                match win::stat_at(handle, e.name) {
                    Ok(st) if st.kind() == Kind::File => row.alloc = st.alloc,
                    Err(code) if win::is_gone(code) => row.refused = Some(Refusal::Gone),
                    Ok(_) | Err(_) => {}
                }
            }
        }
    }
    row
}

pub(super) fn one_level(canon: &Path, policy: &Policy) -> Result<Vec<LevelRow>, Refusal> {
    let handle = open_checked(canon, policy)?;
    let skip = unsafe_mounts_in(canon, &policy.mounts, policy.deny.fold_case());
    // The listing reads no file's link count: a denied name is named, never looked at.
    let mut lister = Lister::new(Vec::new(), skip, LinkReads::None);
    let mut rows = Vec::new();
    loop {
        match lister.next_batch(&handle, &mut |e| {
            rows.push(level_row(&handle, canon, policy, e));
        }) {
            Ok(true) => {}
            Ok(false) => break,
            Err(code) => return Err(win::code_refusal(code)),
        }
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

pub(super) fn entry_count(canon: &Path, policy: &Policy) -> Result<u32, Refusal> {
    let handle = open_checked(canon, policy)?;
    let mut lister = Lister::new(Vec::new(), Vec::new(), LinkReads::None);
    let mut n = 0u32;
    loop {
        match lister.next_batch(&handle, &mut |_| n = n.saturating_add(1)) {
            Ok(true) => {}
            Ok(false) => break,
            Err(code) => return Err(win::code_refusal(code)),
        }
    }
    Ok(n)
}
