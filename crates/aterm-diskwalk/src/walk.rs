// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALK: a resumable cursor over one root, advanced in time slices.

#[cfg(unix)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_sysprobe::volumes::Table;

use crate::list::ListerChoice;
use crate::refuse::{Deny, Refusal, refuse};
#[cfg(unix)]
use crate::tree::DirId;
use crate::tree::{Sums, Tree};

/// A slice's budget (design §10.5 rule 10).
pub const SLICE: Duration = Duration::from_millis(4);
/// The deepest a cursor goes; deeper folders are `too deep` rows.
pub const MAX_DEPTH: u16 = 128;

/// A root to walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSpec {
    /// The canonical spelling.
    pub path: PathBuf,
    /// The logical spelling, when it differs.
    pub logical: Option<PathBuf>,
}

impl RootSpec {
    /// A root named once.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            logical: None,
        }
    }
}

/// How a walk runs.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The deny set and allowlist.
    pub deny: Deny,
    /// The mount table (read with `MNT_NOWAIT`).
    pub mounts: Table,
    /// Read each file's private (unshared-clone) size: about 2.5× the cost.
    pub private: bool,
    /// Keep a `` map of multiply linked files for the walk's first-seen and shared-out figures.
    pub links_out: bool,
    /// A slice's budget.
    pub slice: Duration,
    /// The deepest level entered.
    pub max_depth: u16,
    /// Which lister.
    pub lister: ListerChoice,
}

impl Policy {
    /// The default policy.
    #[must_use]
    pub const fn new(deny: Deny, mounts: Table) -> Self {
        Self {
            deny,
            mounts,
            private: false,
            links_out: false,
            slice: SLICE,
            max_depth: MAX_DEPTH,
            lister: ListerChoice::Native,
        }
    }
}

/// What a slice did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The slice is spent and the walk goes on; `walked` entries so far.
    More {
        /// Entries listed so far.
        walked: u64,
    },
    /// The walk reached its end.
    Done,
    /// The walk was cancelled.
    Cancelled,
}

/// The hard-link figures of a walk with [`Policy::links_out`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkTotals {
    /// Each inode's allocation counted once, at its first link seen.
    pub alloc_first_seen: u64,
    /// The allocation of inodes with a link outside the root.
    pub shared_out: u64,
    /// What deleting the tree could free at most (clones aside): `alloc_first_seen − shared_out`.
    pub frees_at_most: u64,
    /// Multiply linked inodes seen.
    pub linked_inodes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Walking,
    Done,
    Cancelled,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct Link {
    nlink: u32,
    seen: u32,
    alloc: u64,
}

/// One root's walk: the cursor and what it has recorded.
pub struct Walk {
    generation: u64,
    tree: Tree,
    #[cfg(unix)]
    stack: Vec<imp::Frame>,
    #[cfg(unix)]
    spare: Vec<Vec<u8>>,
    root_dev: u64,
    #[cfg(unix)]
    policy: Policy,
    #[cfg(unix)]
    links: Option<HashMap<(u64, u64), Link>>,
    link_totals: Option<LinkTotals>,
    walked: u64,
    calls: u64,
    slices: u64,
    status: Status,
}

impl std::fmt::Debug for Walk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Walk")
            .field("generation", &self.generation)
            .field("root", &self.tree.root_path())
            .field("walked", &self.walked)
            .field("calls", &self.calls)
            .field("slices", &self.slices)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Walk {
    /// Test `root` with [`refuse`].
    /// # Errors
    /// The [`Refusal`] the path test, the open or the volume check gave.
    pub fn begin(root: &RootSpec, policy: &Policy) -> Result<Walk, Refusal> {
        let canon = crate::refuse::canonical(&root.path).ok_or(Refusal::Relative)?;
        if let Some(why) = refuse(
            root.logical.as_deref(),
            &canon,
            &policy.mounts,
            &policy.deny,
        ) {
            return Err(why);
        }
        #[cfg(unix)]
        {
            imp::begin(canon, policy)
        }
        #[cfg(not(unix))]
        {
            let _ = canon;
            Err(Refusal::Unsupported)
        }
    }

    /// One slice: list and descend within [`Policy::slice`].
    // COST: UNBOUNDED(entries) — one call's batch is bounded (16 KiB).
    pub fn diskwalk_slice(&mut self, now: &mut dyn FnMut() -> Instant) -> Step {
        match self.status {
            Status::Done => return Step::Done,
            Status::Cancelled => return Step::Cancelled,
            Status::Walking => {}
        }
        #[cfg(unix)]
        {
            let start = now();
            self.slices += 1;
            loop {
                match self.unit() {
                    imp::Unit::Empty => {
                        self.finish();
                        return Step::Done;
                    }
                    imp::Unit::Folded => {}
                    imp::Unit::Called => {
                        self.calls += 1;
                        if now().saturating_duration_since(start) >= self.policy.slice {
                            return Step::More {
                                walked: self.walked,
                            };
                        }
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = now;
            self.status = Status::Done;
            Step::Done
        }
    }

    /// Stop: every open directory is closed now.
    pub fn cancel(&mut self) {
        if self.status == Status::Walking {
            self.status = Status::Cancelled;
        }
        #[cfg(unix)]
        self.stack.clear();
    }

    /// The generation [`Walker`] minted for this walk (0 for a bare walk).
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// What it recorded.
    #[must_use]
    pub const fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The tree, taking it.
    #[must_use]
    pub fn into_tree(self) -> Tree {
        self.tree
    }

    /// Entries listed so far.
    #[must_use]
    pub const fn walked(&self) -> u64 {
        self.walked
    }

    /// Listing calls and child opens made so far.
    #[must_use]
    pub const fn calls(&self) -> u64 {
        self.calls
    }

    /// Slices run so far.
    #[must_use]
    pub const fn slices(&self) -> u64 {
        self.slices
    }

    /// The walk reached its end.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.status == Status::Done
    }

    /// The walk was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.status == Status::Cancelled
    }

    /// The root's device.
    #[must_use]
    pub const fn root_dev(&self) -> u64 {
        self.root_dev
    }

    /// Everything listed so far, summed.
    #[must_use]
    pub fn partial(&self) -> Sums {
        if self.status == Status::Done {
            return self.tree.root().subtree;
        }
        let mut s = Sums::default();
        for r in self.tree.dirs() {
            s.add(&r.own);
        }
        s
    }

    /// The hard-link figures, once a walk with [`Policy::links_out`] is done.
    #[must_use]
    pub const fn links(&self) -> Option<LinkTotals> {
        self.link_totals
    }

    #[cfg(unix)]
    fn finish(&mut self) {
        self.status = Status::Done;
        let root = self.tree.rec_mut(DirId::ROOT);
        root.complete = true;
        if let Some(map) = self.links.take() {
            let floor = self.tree.root().subtree.released_floor;
            let mut t = LinkTotals {
                alloc_first_seen: floor,
                ..LinkTotals::default()
            };
            for l in map.values() {
                t.linked_inodes += 1;
                t.alloc_first_seen = t.alloc_first_seen.saturating_add(l.alloc);
                if l.seen < l.nlink {
                    t.shared_out = t.shared_out.saturating_add(l.alloc);
                }
            }
            t.frees_at_most = t.alloc_first_seen.saturating_sub(t.shared_out);
            self.link_totals = Some(t);
        }
    }
}

/// A [`Walker`] generation, handed to whoever will ask for its slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket {
    generation: u64,
}

impl Ticket {
    /// The generation it names.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// What [`Walker::slice`] answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sliced {
    /// The current walk took the slice.
    Step(Step),
    /// The ticket names a superseded generation: nothing was walked.
    Stale,
}

/// One walk in flight, its generation and whether its host owes it a slice (`armed`).
#[derive(Debug, Default)]
pub struct Walker {
    minted: u64,
    walk: Option<Walk>,
    armed: bool,
}

impl Walker {
    /// No walk.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin walking `root`, superseding the walk in flight (its ticket goes stale), and arm.
    /// # Errors
    /// The [`Refusal`] [`Walk::begin`] gave.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_walk",
            action = "Start",
            project = "conformance_budgeted_walk::project"
        )
    )]
    pub fn start(&mut self, root: &RootSpec, policy: &Policy) -> Result<Ticket, Refusal> {
        let mut walk = Walk::begin(root, policy)?;
        self.minted += 1;
        walk.generation = self.minted;
        self.walk = Some(walk);
        self.armed = true;
        Ok(Ticket {
            generation: self.minted,
        })
    }

    /// One slice of the walk `ticket` names.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_walk",
            action = "Slice",
            project = "conformance_budgeted_walk::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_walk",
            action = "FinalSlice",
            project = "conformance_budgeted_walk::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_walk",
            action = "StaleSlice",
            project = "conformance_budgeted_walk::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "budgeted_walk",
            action = "OverBudget",
            reason = "Buggy=1 negative control only: a slice reads the injected clock after \
                      every listing call and child open and returns at the first boundary at or \
                      past its budget; the Tier-1 replays a clock that never charges and the \
                      healthy model rejects the slice it produces"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "budgeted_walk",
            action = "ResumeOldGeneration",
            reason = "Buggy=1 negative control only: a ticket whose generation is not the walk \
                      in flight answers Stale before any listing"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "budgeted_walk",
            action = "CursorRestartsDirectory",
            reason = "Buggy=1 liveness negative control only: the cursor keeps each listing's \
                      offset in its open descriptor, so a slice never lists a directory twice"
        )
    )]
    pub fn slice(&mut self, ticket: Ticket, now: &mut dyn FnMut() -> Instant) -> Sliced {
        let Some(walk) = self
            .walk
            .as_mut()
            .filter(|w| w.generation == ticket.generation)
        else {
            return Sliced::Stale;
        };
        if !self.armed {
            return Sliced::Step(if walk.is_cancelled() {
                Step::Cancelled
            } else {
                Step::Done
            });
        }
        let step = walk.diskwalk_slice(now);
        if !matches!(step, Step::More { .. }) {
            self.armed = false;
        }
        Sliced::Step(step)
    }

    /// Cancel the walk in flight: its directories close and nothing stays armed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "budgeted_walk",
            action = "Cancel",
            project = "conformance_budgeted_walk::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "budgeted_walk",
            action = "TimerAfterCancel",
            reason = "Buggy=1 negative control only: cancel clears `armed` in the same call \
                      and only `start` sets it again"
        )
    )]
    pub fn cancel(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(w) = self.walk.as_mut() {
            w.cancel();
        }
        self.armed = false;
    }

    /// Whether the host owes the walk in flight another slice.
    #[must_use]
    pub const fn armed(&self) -> bool {
        self.armed
    }

    /// The walk in flight (or the last one, ended).
    #[must_use]
    pub const fn walk(&self) -> Option<&Walk> {
        self.walk.as_ref()
    }

    /// The ticket of the walk in flight.
    #[must_use]
    pub fn ticket(&self) -> Option<Ticket> {
        self.walk.as_ref().map(|w| Ticket {
            generation: w.generation,
        })
    }

    /// Take the walk out (its ticket goes stale; nothing stays armed).
    pub const fn take(&mut self) -> Option<Walk> {
        self.armed = false;
        self.walk.take()
    }
}

/// A whole walk's result ([`diskwalk_size_full`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeFull {
    /// The root's sums (a lower bound when not complete).
    pub sums: Sums,
    /// The walk reached its end inside the budget.
    pub complete: bool,
    /// Entries listed.
    pub walked: u64,
    /// The hard-link figures, with [`Policy::links_out`] and a complete walk.
    pub links: Option<LinkTotals>,
}

/// A whole walk in one call for a one-shot survey (design §10.7).
/// # Errors
/// The [`Refusal`] the root earned.
// COST: UNBOUNDED(entries) — a whole walk; the harness's survey and the CLI call it on their own.
pub fn diskwalk_size_full(
    root: &RootSpec,
    policy: &Policy,
    budget: Duration,
    now: &mut dyn FnMut() -> Instant,
) -> Result<SizeFull, Refusal> {
    let mut walk = match Walk::begin(root, policy) {
        Ok(w) => w,
        Err(Refusal::NotDirectory) => return file_root(root, policy),
        Err(e) => return Err(e),
    };
    let start = now();
    loop {
        match walk.diskwalk_slice(now) {
            Step::Done | Step::Cancelled => break,
            Step::More { .. } => {
                if now().saturating_duration_since(start) >= budget {
                    break;
                }
            }
        }
    }
    Ok(SizeFull {
        sums: walk.partial(),
        complete: walk.is_done(),
        walked: walk.walked(),
        links: walk.links(),
    })
}

/// A regular-file root: its own allocation, read through its parent.
fn file_root(root: &RootSpec, policy: &Policy) -> Result<SizeFull, Refusal> {
    let canon = crate::refuse::canonical(&root.path).ok_or(Refusal::Relative)?;
    let parent = canon.parent().ok_or(Refusal::TooBroad)?;
    let name = canon.file_name().ok_or(Refusal::Relative)?;
    #[cfg(unix)]
    {
        let _ = policy;
        let pfd = imp::open_chain(parent)?;
        use std::os::fd::AsFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        let e = crate::list::portable::stat_at(pfd.as_fd(), name.as_bytes());
        if let Some(errno) = e.error {
            return Err(errno_refusal(errno));
        }
        let mut sums = Sums {
            files: 1,
            logical: e.logical,
            newest_mtime_s: e.mtime_s,
            ..Sums::default()
        };
        if e.dataless {
            sums.dataless_logical = e.logical;
        } else {
            sums.alloc_fair = e.alloc / u64::from(e.nlink.max(1));
            if e.nlink <= 1 {
                sums.released_floor = e.alloc;
            }
        }
        Ok(SizeFull {
            sums,
            complete: true,
            walked: 1,
            links: None,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (policy, parent, name);
        Err(Refusal::Unsupported)
    }
}

/// An open's errno as a refusal.
#[cfg(unix)]
pub(crate) const fn errno_refusal(errno: i32) -> Refusal {
    match errno {
        libc::ENOENT => Refusal::Gone,
        libc::ELOOP => Refusal::Link,
        libc::ENOTDIR => Refusal::NotDirectory,
        e => Refusal::Unreadable(e),
    }
}

/// One child of a too-broad root ([`list_one_level`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelRow {
    /// Its name.
    pub name: std::ffi::OsString,
    /// A directory.
    pub dir: bool,
    /// A directory's direct entry count, where the platform reads it without listing (macOS).
    pub direct_entries: Option<u32>,
    /// A file's allocation.
    pub alloc: u64,
    /// Why it was not looked at (a protected folder is named, never stat'ed).
    pub refused: Option<Refusal>,
    /// Another file system is mounted here.
    pub mount_boundary: bool,
}

/// A too-broad root's one-level listing: names, kinds and direct entry counts, nothing walked.
/// # Errors
/// The [`Refusal`] the root earned.
pub fn list_one_level(root: &RootSpec, policy: &Policy) -> Result<Vec<LevelRow>, Refusal> {
    let canon = crate::refuse::canonical(&root.path).ok_or(Refusal::Relative)?;
    match refuse(
        root.logical.as_deref(),
        &canon,
        &policy.mounts,
        &policy.deny,
    ) {
        None | Some(Refusal::TooBroad) => {}
        Some(why) => return Err(why),
    }
    #[cfg(unix)]
    {
        imp::one_level(&canon, policy)
    }
    #[cfg(not(unix))]
    {
        Err(Refusal::Unsupported)
    }
}

/// One directory's direct entry count without listing it where the platform can.
/// # Errors
/// The [`Refusal`] the path earned.
pub fn entry_count_of(path: &Path, policy: &Policy) -> Result<u32, Refusal> {
    let canon = crate::refuse::canonical(path).ok_or(Refusal::Relative)?;
    if let Some(why) = refuse(None, &canon, &policy.mounts, &policy.deny) {
        return Err(why);
    }
    #[cfg(unix)]
    {
        imp::entry_count(&canon)
    }
    #[cfg(not(unix))]
    {
        Err(Refusal::Unsupported)
    }
}

#[cfg(unix)]
pub(crate) mod imp {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Component, Path, PathBuf};

    use aterm_dirfd::{Mode, OFlags};

    use super::{LevelRow, Link, Policy, Status, Walk, errno_refusal};
    use crate::list::{EntryRef, Kind, Lister};
    use crate::refuse::{Refusal, unsafe_mounts_in};
    use crate::tree::{DirId, Sums, Tree};

    /// One open directory of the cursor.
    pub(super) struct Frame {
        fd: OwnedFd,
        dir: DirId,
        path: PathBuf,
        /// `None` once its listing ended (the buffer went back to the spares).
        lister: Option<Lister>,
        /// Child directories listed and not yet entered.
        pending: Vec<(Box<[u8]>, DirId)>,
    }

    pub(super) enum Unit {
        /// A listing call or a child open: the clock is read after it.
        Called,
        /// A finished directory folded into its parent: no syscall.
        Folded,
        /// Nothing left.
        Empty,
    }

    const DIR_FLAGS: OFlags = OFlags::RDONLY;

    fn dir_flags() -> OFlags {
        DIR_FLAGS | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    /// `/`, opened.
    fn open_slash() -> Result<OwnedFd, Refusal> {
        // SAFETY: a NUL-terminated literal path; the flags open a directory.
        let fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(errno_refusal(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            ));
        }
        // SAFETY: a fresh descriptor this function owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Open `path` one component at a time from `/`, each with `O_NOFOLLOW` relative to its parent.
    pub(super) fn open_chain(path: &Path) -> Result<OwnedFd, Refusal> {
        let mut fd = open_slash()?;
        for c in path.components() {
            match c {
                Component::RootDir => {}
                Component::Normal(name) => fd = open_child(&fd, name)?,
                _ => return Err(Refusal::Relative),
            }
        }
        Ok(fd)
    }

    /// [`open_chain`], then the volume test on the open descriptor: what a marker look opens.
    pub(crate) fn open_checked(path: &Path) -> Result<OwnedFd, Refusal> {
        let fd = open_chain(path)?;
        if let Some(why) = volume_refusal_of(&fd) {
            return Err(why);
        }
        Ok(fd)
    }

    /// Open the directory `name` in `parent`, following no link.
    fn open_child(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Refusal> {
        aterm_dirfd::openat(parent, name, dir_flags(), Mode::empty()).map_err(|e| {
            match e.raw_os_error() {
                libc::ENOTDIR | libc::ELOOP => link_or_file(parent, name),
                errno => errno_refusal(errno),
            }
        })
    }

    fn link_or_file(parent: &OwnedFd, name: &OsStr) -> Refusal {
        let e = crate::list::portable::stat_at(parent.as_fd(), name.as_bytes());
        match (e.error, e.kind) {
            (Some(errno), _) => errno_refusal(errno),
            (None, Kind::Link) => Refusal::Link,
            _ => Refusal::NotDirectory,
        }
    }

    /// The root's volume: never remote, removable or automounted, whatever
    /// the table said when the walk was planned.
    #[cfg(target_vendor = "apple")]
    fn volume_refusal_of(fd: &OwnedFd) -> Option<Refusal> {
        const MNT_LOCAL: u32 = 0x0000_1000;
        const MNT_REMOVABLE: u32 = 0x0000_0200;
        const MNT_AUTOMOUNTED: u32 = 0x0040_0000;
        // SAFETY: `statfs` is integers and arrays: all-zero is a value.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor and a writable `statfs`.
        if unsafe { libc::fstatfs(fd.as_raw_fd(), &raw mut st) } != 0 {
            return Some(errno_refusal(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            ));
        }
        let fstype: Vec<u8> = st
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c.to_ne_bytes()[0])
            .collect();
        let fstype = String::from_utf8_lossy(&fstype).to_ascii_lowercase();
        if st.f_flags & MNT_AUTOMOUNTED != 0 || fstype == "autofs" {
            return Some(Refusal::Automounted);
        }
        if st.f_flags & MNT_LOCAL == 0
            || crate::refuse::REMOTE_FSTYPES.contains(&fstype.as_str())
            || fstype.starts_with("fuse")
        {
            return Some(Refusal::Remote);
        }
        if st.f_flags & MNT_REMOVABLE != 0 {
            return Some(Refusal::Removable);
        }
        None
    }

    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unnecessary_cast,
        reason = "statfs's f_type is __fsword_t, whose width varies by arch"
    )]
    fn volume_refusal_of(fd: &OwnedFd) -> Option<Refusal> {
        const AUTOFS_SUPER_MAGIC: i64 = 0x0187;
        const SMB_SUPER_MAGIC: i64 = 0x517B;
        const CIFS_MAGIC_NUMBER: i64 = 0xFF53_4D42;
        const SMB2_MAGIC_NUMBER: i64 = 0xFE53_4D42;
        const CEPH_SUPER_MAGIC: i64 = 0x00C3_6400;
        const V9FS_MAGIC: i64 = 0x0102_1997;
        // SAFETY: `statfs` is integers: all-zero is a value.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor and a writable `statfs`.
        if unsafe { libc::fstatfs(fd.as_raw_fd(), &raw mut st) } != 0 {
            return Some(errno_refusal(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            ));
        }
        let t = st.f_type as i64;
        if t == AUTOFS_SUPER_MAGIC {
            return Some(Refusal::Automounted);
        }
        if [
            libc::NFS_SUPER_MAGIC as i64,
            libc::FUSE_SUPER_MAGIC as i64,
            SMB_SUPER_MAGIC,
            CIFS_MAGIC_NUMBER,
            SMB2_MAGIC_NUMBER,
            CEPH_SUPER_MAGIC,
            V9FS_MAGIC,
        ]
        .contains(&t)
        {
            return Some(Refusal::Remote);
        }
        None
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    fn volume_refusal_of(_fd: &OwnedFd) -> Option<Refusal> {
        None
    }

    /// `fstat` of an open descriptor: (dev, ino, allocated bytes, flags).
    fn fstat_of(fd: &OwnedFd) -> Result<(u64, u64, u64, u32), Refusal> {
        // SAFETY: `stat` is integers: all-zero is a value.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor and a writable `stat`.
        if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut st) } != 0 {
            return Err(errno_refusal(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            ));
        }
        #[cfg(target_vendor = "apple")]
        let flags = st.st_flags;
        #[cfg(not(target_vendor = "apple"))]
        let flags = 0u32;
        Ok((
            crate::list::dev_of(st.st_dev),
            st.st_ino as u64,
            u64::try_from(st.st_blocks as i64)
                .unwrap_or(0)
                .saturating_mul(512),
            flags,
        ))
    }

    /// Whether `st_flags` mark a cloud placeholder (`SF_DATALESS`), which only
    /// Apple's file systems have.
    #[cfg(target_vendor = "apple")]
    const fn dataless(flags: u32) -> bool {
        flags & crate::list::bulk::SF_DATALESS != 0
    }
    #[cfg(not(target_vendor = "apple"))]
    const fn dataless(_: u32) -> bool {
        false
    }

    pub(super) fn begin(canon: PathBuf, policy: &Policy) -> Result<Walk, Refusal> {
        let fd = open_chain(&canon)?;
        if let Some(why) = volume_refusal_of(&fd) {
            return Err(why);
        }
        let (dev, ino, alloc, flags) = fstat_of(&fd)?;
        if dataless(flags) {
            return Err(Refusal::Dataless);
        }
        let mut tree = Tree::new(canon.clone(), ino);
        {
            let root = tree.rec_mut(DirId::ROOT);
            root.own.alloc_fair = alloc;
            root.subtree.alloc_fair = alloc;
        }
        let skip = unsafe_mounts_in(&canon, &policy.mounts, policy.deny.fold_case());
        let lister = Lister::new(policy.lister, Vec::new(), skip, policy.private);
        Ok(Walk {
            generation: 0,
            tree,
            stack: vec![Frame {
                fd,
                dir: DirId::ROOT,
                path: canon,
                lister: Some(lister),
                pending: Vec::new(),
            }],
            spare: Vec::new(),
            root_dev: dev,
            policy: policy.clone(),
            links: policy.links_out.then(HashMap::new),
            link_totals: None,
            walked: 0,
            calls: 0,
            slices: 0,
            status: Status::Walking,
        })
    }

    /// Where one entry lands.
    struct Sink<'a> {
        tree: &'a mut Tree,
        links: Option<&'a mut HashMap<(u64, u64), Link>>,
        policy: &'a Policy,
        walked: &'a mut u64,
        root_dev: u64,
        dir: DirId,
        path: &'a Path,
        pending: &'a mut Vec<(Box<[u8]>, DirId)>,
    }

    impl Sink<'_> {
        fn add_own(&mut self, s: &Sums) {
            let r = self.tree.rec_mut(self.dir);
            r.own.add(s);
            r.subtree.add(s);
        }

        fn entry(&mut self, e: &EntryRef<'_>) {
            if e.name.is_empty() || e.name == b"." || e.name == b".." {
                return;
            }
            *self.walked += 1;
            {
                let r = self.tree.rec_mut(self.dir);
                r.direct_entries = r.direct_entries.saturating_add(1);
                if e.exec {
                    r.direct_exec = r.direct_exec.saturating_add(1);
                }
            }
            if let Some(errno) = e.error {
                let s = if errno == libc::ENOENT {
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
                self.add_own(&s);
                return;
            }
            if e.kind == Kind::Dir {
                self.dir_entry(e);
                return;
            }
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
                if let Some(links) = self.links.as_deref_mut()
                    && nlink > 1
                {
                    let l = links.entry((e.dev, e.ino)).or_insert(Link {
                        nlink: e.nlink,
                        seen: 0,
                        alloc: e.alloc,
                    });
                    l.seen = l.seen.saturating_add(1);
                }
            }
            if self.policy.private {
                s.private = Some(if e.dataless {
                    0
                } else {
                    e.private.unwrap_or(e.alloc) / nlink
                });
            }
            self.add_own(&s);
        }

        fn dir_entry(&mut self, e: &EntryRef<'_>) {
            self.add_own(&Sums {
                dirs: 1,
                newest_mtime_s: e.mtime_s,
                ..Sums::default()
            });
            let parent_depth = self.tree.get(self.dir).map_or(0, |r| r.depth);
            let child = self.tree.push_child(self.dir, e.name, e.ino);
            let boundary = e.mount_point || (e.dev != 0 && e.dev != self.root_dev);
            let refused = if boundary {
                None
            } else if e.dataless {
                Some(Refusal::Dataless)
            } else if parent_depth.saturating_add(1) > self.policy.max_depth {
                Some(Refusal::TooDeep)
            } else {
                self.policy
                    .deny
                    .denies(&self.path.join(OsStr::from_bytes(e.name)))
            };
            let r = self.tree.rec_mut(child);
            r.direct_entries = e.dir_entries.unwrap_or(0);
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

    /// What a step decided to do with the cursor's top.
    enum Next {
        Nothing,
        Push(Frame),
        Pop,
    }

    impl Walk {
        /// One unit of work on the cursor's top directory.
        pub(super) fn unit(&mut self) -> Unit {
            let Walk {
                stack,
                tree,
                links,
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
                fd,
                dir,
                path,
                lister,
                pending,
            } = top;
            let dir = *dir;
            let (unit, next) = if let Some(l) = lister.as_mut() {
                let mut sink = Sink {
                    tree,
                    links: links.as_mut(),
                    policy,
                    walked,
                    root_dev: *root_dev,
                    dir,
                    path,
                    pending,
                };
                match l.next_batch(fd.as_fd(), &mut |e| sink.entry(e)) {
                    Ok(true) => {}
                    Ok(false) => {
                        if let Some(l) = lister.take() {
                            spare.push(l.into_buffer());
                        }
                    }
                    Err(errno) => {
                        let s = if errno == libc::ENOENT {
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
                        r.refused.get_or_insert(errno_refusal(errno));
                        if let Some(l) = lister.take() {
                            spare.push(l.into_buffer());
                        }
                    }
                }
                (Unit::Called, Next::Nothing)
            } else if let Some((name, child)) = pending.pop() {
                let name = OsStr::from_bytes(&name);
                let opened = open_child(fd, name).and_then(|cfd| {
                    let (dev, ..) = fstat_of(&cfd)?;
                    Ok((cfd, dev))
                });
                let next = match opened {
                    Ok((cfd, dev)) if dev == *root_dev => {
                        let cpath = path.join(name);
                        let skip =
                            unsafe_mounts_in(&cpath, &policy.mounts, policy.deny.fold_case());
                        let buf = spare.pop().unwrap_or_default();
                        Next::Push(Frame {
                            fd: cfd,
                            dir: child,
                            path: cpath,
                            lister: Some(Lister::new(policy.lister, buf, skip, policy.private)),
                            pending: Vec::new(),
                        })
                    }
                    other => {
                        // A different device (the open crossed a mount), or a
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
                    // Its parent's listing gave the count `ATTR_DIR_ENTRYCOUNT`
                    let r = tree.rec_mut(frame.dir);
                    r.direct_entries = 0;
                    r.direct_exec = 0;
                    stack.push(frame);
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

    pub(super) fn one_level(canon: &Path, policy: &Policy) -> Result<Vec<LevelRow>, Refusal> {
        let fd = open_chain(canon)?;
        if let Some(why) = volume_refusal_of(&fd) {
            return Err(why);
        }
        let (root_dev, ..) = fstat_of(&fd)?;
        let skip = unsafe_mounts_in(canon, &policy.mounts, policy.deny.fold_case());
        let dir = aterm_dirfd::Dir::read_from(&fd).map_err(|e| errno_refusal(e.raw_os_error()))?;
        let mut rows = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|e| errno_refusal(e.raw_os_error()))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let os = OsStr::from_bytes(name).to_os_string();
            let child = canon.join(&os);
            if let Some(why) = policy.deny.denies(&child) {
                rows.push(LevelRow {
                    name: os,
                    dir: true,
                    direct_entries: None,
                    alloc: 0,
                    refused: Some(why),
                    mount_boundary: false,
                });
                continue;
            }
            if skip.iter().any(|s| s.as_bytes() == name) {
                rows.push(LevelRow {
                    name: os,
                    dir: true,
                    direct_entries: None,
                    alloc: 0,
                    refused: None,
                    mount_boundary: true,
                });
                continue;
            }
            let e = crate::list::portable::stat_at(fd.as_fd(), name);
            if let Some(errno) = e.error {
                rows.push(LevelRow {
                    name: os,
                    dir: false,
                    direct_entries: None,
                    alloc: 0,
                    refused: Some(errno_refusal(errno)),
                    mount_boundary: false,
                });
                continue;
            }
            let is_dir = e.kind == Kind::Dir;
            let boundary = is_dir && e.dev != root_dev;
            let direct_entries = if is_dir && !boundary && !e.dataless {
                count_at(&fd, name)
            } else {
                None
            };
            rows.push(LevelRow {
                name: os,
                dir: is_dir,
                direct_entries,
                alloc: if is_dir { 0 } else { e.alloc },
                refused: (is_dir && e.dataless).then_some(Refusal::Dataless),
                mount_boundary: boundary,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    #[cfg(target_vendor = "apple")]
    fn count_at(fd: &OwnedFd, name: &[u8]) -> Option<u32> {
        let c = std::ffi::CString::new(name).ok()?;
        crate::list::bulk::entry_count_at(fd.as_fd(), &c).ok()
    }

    #[cfg(not(target_vendor = "apple"))]
    fn count_at(_fd: &OwnedFd, _name: &[u8]) -> Option<u32> {
        None
    }

    pub(super) fn entry_count(canon: &Path) -> Result<u32, Refusal> {
        let fd = open_chain(canon)?;
        if let Some(why) = volume_refusal_of(&fd) {
            return Err(why);
        }
        #[cfg(target_vendor = "apple")]
        {
            crate::list::bulk::entry_count(fd.as_fd()).map_err(errno_refusal)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let dir =
                aterm_dirfd::Dir::read_from(&fd).map_err(|e| errno_refusal(e.raw_os_error()))?;
            let mut n = 0u32;
            for entry in dir {
                let entry = entry.map_err(|e| errno_refusal(e.raw_os_error()))?;
                let name = entry.file_name().to_bytes();
                if name != b"." && name != b".." {
                    n = n.saturating_add(1);
                }
            }
            Ok(n)
        }
    }
}
