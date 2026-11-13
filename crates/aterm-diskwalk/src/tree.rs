// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Directory tree records (`DirRec`), per-directory and subtree [`Sums`], and tree queries.

use std::path::{Path, PathBuf};

use crate::refuse::Refusal;

/// Direct-entry threshold for a huge-directory note.
pub const HUGE_NOTE: u32 = 75_000;
/// Direct-entry threshold for a huge-directory warning.
pub const HUGE_WARN: u32 = 100_000;

/// Sizes and counts over a set of entries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sums {
    /// Σ allocated / link count (`du`-equivalent when hard links stay inside the root).
    pub alloc_fair: u64,
    /// Σ logical size (`TOTALSIZE`, `st_size`).
    pub logical: u64,
    /// Σ allocated of single-link files.
    pub released_floor: u64,
    /// Σ private size / link count (macOS clones), or `None` when not sampled, or when it cannot
    /// be told: a Windows volume whose files can share blocks (ReFS) says nothing of how much
    /// of a file's allocation is its own.
    pub private: Option<u64>,
    /// Σ logical size of cloud placeholders (0 allocated).
    pub dataless_logical: u64,
    /// Files and other non-directories.
    pub files: u64,
    /// Directories (the record's own directory not counted in `own`).
    pub dirs: u64,
    /// Newest modification time seen, in unix seconds (`0` when none).
    pub newest_mtime_s: i64,
    /// Entries that could not be read.
    pub unreadable: u32,
    /// Entries that vanished between listing and reading.
    pub gone: u32,
}

impl Sums {
    /// Fold `other` into `self`.
    pub fn add(&mut self, other: &Sums) {
        self.alloc_fair = self.alloc_fair.saturating_add(other.alloc_fair);
        self.logical = self.logical.saturating_add(other.logical);
        self.released_floor = self.released_floor.saturating_add(other.released_floor);
        self.private = match (self.private, other.private) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (a, b) => a.or(b),
        };
        self.dataless_logical = self.dataless_logical.saturating_add(other.dataless_logical);
        self.files = self.files.saturating_add(other.files);
        self.dirs = self.dirs.saturating_add(other.dirs);
        self.newest_mtime_s = self.newest_mtime_s.max(other.newest_mtime_s);
        self.unreadable = self.unreadable.saturating_add(other.unreadable);
        self.gone = self.gone.saturating_add(other.gone);
    }

    /// Entries counted.
    #[inline]
    #[must_use]
    pub const fn entries(&self) -> u64 {
        self.files.saturating_add(self.dirs)
    }
}

/// A directory's index in its [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirId(pub u32);

impl DirId {
    /// The root's id.
    pub const ROOT: DirId = DirId(0);

    #[inline]
    const fn ix(self) -> usize {
        self.0 as usize
    }
}

/// One directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirRec {
    /// Its parent; `None` for the root.
    pub parent: Option<DirId>,
    name_at: u32,
    name_len: u32,
    /// Its inode number.
    pub ino: u64,
    /// Its depth below the root (the root is 0).
    pub depth: u16,
    /// The files directly in it, and its own allocation.
    pub own: Sums,
    /// It and everything under it; a lower bound while not [`Self::complete`].
    pub subtree: Sums,
    /// Entries directly in the directory.
    pub direct_entries: u32,
    /// Executable regular files directly in it.
    pub direct_exec: u32,
    /// Its subtree is walked to the end.
    pub complete: bool,
    /// Reason it was not entered, or `None` when walked.
    pub refused: Option<Refusal>,
    /// A mount point: a different file system lives here and is never entered.
    pub mount_boundary: bool,
}

/// The directories one walk recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    root: PathBuf,
    dirs: Vec<DirRec>,
    names: Vec<u8>,
}

/// A directory at or past [`HUGE_NOTE`] direct entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HugeDir {
    /// The directory, absolute.
    pub path: PathBuf,
    /// Its direct entries.
    pub entries: u32,
    /// It holds executables (the tccd/WindowServer hazard applies).
    pub executables: bool,
    /// At or past [`HUGE_WARN`] (else past [`HUGE_NOTE`]).
    pub warning: bool,
}

impl HugeDir {
    /// The line `sys disk` prints and the exceptions entry carries.
    #[must_use]
    pub fn sentence(&self) -> String {
        let kind = if self.warning { "warning" } else { "note" };
        let path = self.path.display();
        let n = group(u64::from(self.entries));
        let line = group(u64::from(if self.warning { HUGE_WARN } else { HUGE_NOTE }));
        if self.executables {
            format!(
                "{kind}: {path} has {n} entries (line {line}): a binary launched from it makes \
                 tccd read the whole directory when it opens its WindowServer connection; remove \
                 old build outputs there (targo clean, or delete entries older than a few days)"
            )
        } else {
            format!(
                "{kind}: {path} has {n} entries (line {line}): huge directory: slow for anything \
                 that lists it"
            )
        }
    }
}

fn push_name(p: &mut PathBuf, name: &[u8]) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        p.push(std::ffi::OsStr::from_bytes(name));
    }
    #[cfg(not(unix))]
    {
        p.push(String::from_utf8_lossy(name).as_ref());
    }
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

impl Tree {
    /// A tree holding only its root record.
    #[must_use]
    pub fn new(root: PathBuf, ino: u64) -> Self {
        let mut t = Self {
            root,
            dirs: Vec::new(),
            names: Vec::new(),
        };
        t.dirs.push(DirRec {
            parent: None,
            name_at: 0,
            name_len: 0,
            ino,
            depth: 0,
            own: Sums::default(),
            subtree: Sums::default(),
            direct_entries: 0,
            direct_exec: 0,
            complete: false,
            refused: None,
            mount_boundary: false,
        });
        t
    }

    /// Add a child directory record (not yet walked).
    #[cfg(any(unix, windows, test))]
    pub(crate) fn push_child(&mut self, parent: DirId, name: &[u8], ino: u64) -> DirId {
        let name_at = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
        self.names.extend_from_slice(name);
        let depth = self.dirs[parent.ix()].depth.saturating_add(1);
        let id = DirId(u32::try_from(self.dirs.len()).unwrap_or(u32::MAX));
        self.dirs.push(DirRec {
            parent: Some(parent),
            name_at,
            name_len: u32::try_from(name.len()).unwrap_or(u32::MAX),
            ino,
            depth,
            own: Sums::default(),
            subtree: Sums::default(),
            direct_entries: 0,
            direct_exec: 0,
            complete: false,
            refused: None,
            mount_boundary: false,
        });
        id
    }

    #[cfg(any(unix, windows, test))]
    #[inline]
    pub(crate) fn rec_mut(&mut self, id: DirId) -> &mut DirRec {
        &mut self.dirs[id.ix()]
    }

    /// The root path the tree was walked from.
    #[inline]
    #[must_use]
    pub fn root_path(&self) -> &Path {
        &self.root
    }

    /// Every directory record, the root first.
    #[inline]
    #[must_use]
    pub fn dirs(&self) -> &[DirRec] {
        &self.dirs
    }

    /// One record.
    #[inline]
    #[must_use]
    pub fn get(&self, id: DirId) -> Option<&DirRec> {
        self.dirs.get(id.ix())
    }

    /// The root's record.
    #[inline]
    #[must_use]
    pub fn root(&self) -> &DirRec {
        &self.dirs[0]
    }

    /// A record's name (empty for the root).
    #[inline]
    #[must_use]
    pub fn name(&self, id: DirId) -> &[u8] {
        self.get(id).map_or(&[][..], |r| {
            let at = r.name_at as usize;
            at.checked_add(r.name_len as usize)
                .and_then(|end| self.names.get(at..end))
                .unwrap_or(&[])
        })
    }

    /// A record's path relative to the root (empty for the root).
    #[must_use]
    pub fn rel_path(&self, id: DirId) -> PathBuf {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == DirId::ROOT {
                break;
            }
            chain.push(c);
            cur = self.get(c).and_then(|r| r.parent);
        }
        let mut p = PathBuf::new();
        for c in chain.into_iter().rev() {
            push_name(&mut p, self.name(c));
        }
        p
    }

    /// A record's absolute path.
    #[must_use]
    pub fn path(&self, id: DirId) -> PathBuf {
        self.root.join(self.rel_path(id))
    }

    /// The direct child directories of `id`, largest subtree first.
    #[must_use]
    pub fn children(&self, id: DirId) -> Vec<DirId> {
        let mut out: Vec<DirId> = (1..self.dirs.len())
            .map(|i| DirId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|c| self.dirs[c.ix()].parent == Some(id))
            .collect();
        out.sort_by(|a, b| {
            self.dirs[b.ix()]
                .subtree
                .alloc_fair
                .cmp(&self.dirs[a.ix()].subtree.alloc_fair)
                .then(a.cmp(b))
        });
        out
    }

    /// Look up the record at root-relative path `rel`.
    #[must_use]
    pub fn find(&self, rel: &Path) -> Option<DirId> {
        self.find_folded(rel, false)
    }

    /// [`Self::find`], each name matched ignoring ASCII case when `fold` is set (as APFS
    /// matches names, [`crate::Deny::fold_case`]).
    #[must_use]
    pub fn find_folded(&self, rel: &Path, fold: bool) -> Option<DirId> {
        let same = |a: &[u8], b: &[u8]| {
            if fold {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        };
        let mut at = DirId::ROOT;
        for c in rel.components() {
            let name = match c {
                std::path::Component::Normal(name) => name.as_encoded_bytes(),
                std::path::Component::CurDir => continue,
                _ => return None,
            };
            at = (1..self.dirs.len())
                .map(|i| DirId(u32::try_from(i).unwrap_or(u32::MAX)))
                .find(|&id| self.dirs[id.ix()].parent == Some(at) && same(self.name(id), name))?;
        }
        Some(at)
    }

    /// Whether `id` is `within` or inside it.
    #[must_use]
    pub fn is_within(&self, id: DirId, within: DirId) -> bool {
        let mut at = Some(id);
        while let Some(cur) = at {
            if cur == within {
                return true;
            }
            at = self.dirs.get(cur.ix()).and_then(|r| r.parent);
        }
        false
    }

    /// [`Self::biggest`] over the whole tree.
    #[must_use]
    pub fn biggest(&self, n: usize) -> Vec<DirId> {
        self.biggest_in(DirId::ROOT, n)
    }

    /// Return the `n` largest subtrees inside `within` where no child holds at least half.
    #[must_use]
    pub fn biggest_in(&self, within: DirId, n: usize) -> Vec<DirId> {
        let len = self.dirs.len();
        let mut heaviest_child = vec![0u64; len];
        for r in self.dirs.iter().skip(1) {
            if let Some(p) = r.parent {
                let slot = &mut heaviest_child[p.ix()];
                *slot = (*slot).max(r.subtree.alloc_fair);
            }
        }
        let mut cands: Vec<DirId> = (1..len)
            .filter(|&i| {
                let r = &self.dirs[i];
                r.subtree.alloc_fair > 0
                    && heaviest_child[i].saturating_mul(2) < r.subtree.alloc_fair
            })
            .map(|i| DirId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|&id| id != within && self.is_within(id, within))
            .collect();
        cands.sort_by(|a, b| {
            self.dirs[b.ix()]
                .subtree
                .alloc_fair
                .cmp(&self.dirs[a.ix()].subtree.alloc_fair)
                .then(a.cmp(b))
        });
        cands.truncate(n);
        cands
    }

    /// The `n` directories with the most direct entries, most first.
    #[must_use]
    pub fn most_entries(&self, n: usize) -> Vec<DirId> {
        self.most_entries_in(DirId::ROOT, n)
    }

    /// [`Self::most_entries`] among `within` and the folders inside it.
    #[must_use]
    pub fn most_entries_in(&self, within: DirId, n: usize) -> Vec<DirId> {
        let mut ids: Vec<DirId> = (0..self.dirs.len())
            .map(|i| DirId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|&id| self.dirs[id.ix()].direct_entries > 0 && self.is_within(id, within))
            .collect();
        ids.sort_by(|a, b| {
            self.dirs[b.ix()]
                .direct_entries
                .cmp(&self.dirs[a.ix()].direct_entries)
                .then(a.cmp(b))
        });
        ids.truncate(n);
        ids
    }

    /// Every directory at or past [`HUGE_NOTE`] direct entries.
    #[must_use]
    pub fn huge_dirs(&self) -> Vec<HugeDir> {
        self.huge_dirs_in(DirId::ROOT)
    }

    /// [`Self::huge_dirs`] among `within` and the folders inside it.
    #[must_use]
    pub fn huge_dirs_in(&self, within: DirId) -> Vec<HugeDir> {
        (0..self.dirs.len())
            .map(|i| DirId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|&id| {
                self.dirs[id.ix()].direct_entries >= HUGE_NOTE && self.is_within(id, within)
            })
            .map(|id| {
                let r = &self.dirs[id.ix()];
                HugeDir {
                    path: self.path(id),
                    entries: r.direct_entries,
                    executables: r.direct_exec > 0,
                    warning: r.direct_entries >= HUGE_WARN,
                }
            })
            .collect()
    }

    /// The directories that were not entered, and why.
    #[must_use]
    pub fn refused(&self) -> Vec<(DirId, Refusal)> {
        self.refused_in(DirId::ROOT)
    }

    /// [`Self::refused`] among `within` and the folders inside it.
    #[must_use]
    pub fn refused_in(&self, within: DirId) -> Vec<(DirId, Refusal)> {
        (0..self.dirs.len())
            .filter_map(|i| {
                let id = DirId(u32::try_from(i).unwrap_or(u32::MAX));
                self.dirs[i]
                    .refused
                    .filter(|_| self.is_within(id, within))
                    .map(|r| (id, r))
            })
            .collect()
    }

    /// Largest direct entry count among `within` and folders inside it.
    #[must_use]
    pub fn direct_entries_max_in(&self, within: DirId) -> u32 {
        (0..self.dirs.len())
            .map(|i| DirId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|&id| self.is_within(id, within))
            .map(|id| self.dirs[id.ix()].direct_entries)
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
