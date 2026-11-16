// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a finished walk says of the stores aterm keeps under its root (`sys disk`'s `store`
//! lines): each store's bytes and files are read from the walk's own tree, so sizing them costs
//! no second walk, and the walker's history of those sizes gives each its growth per hour.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_diskwalk::{DirId, Tree};
use aterm_vitals::sys::{StoreHistory, StoreSample};

/// The history's file in the aterm state folder. Unix only: it is a strict private file
/// (`aterm_uds::private_file`), and no store is listed elsewhere (`crate::disk_stores::specs`),
/// so there is nothing to keep.
#[cfg(unix)]
pub(crate) const STORE_HISTORY_FILE: &str = "disk-stores.history";
/// The history is written at most this often, and at the first measurement of a run.
const STORE_HISTORY_EVERY: Duration = Duration::from_secs(600);

/// One store the loop asks the walker to size: a folder, a single file, or (`each_child`) a
/// folder whose every child folder is a store of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreAsk {
    /// Where it is.
    pub(crate) path: PathBuf,
    /// Each folder directly in it is one store.
    pub(crate) each_child: bool,
}

/// What a walk measured of one store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StoreBytes {
    /// Allocated bytes, hard links shared fairly.
    pub(crate) alloc: u64,
    /// Logical bytes.
    pub(crate) logical: u64,
    /// Files in it.
    pub(crate) files: u64,
    /// Its allocation's move over the walker's history, bytes per hour; `None` until two
    /// measurements far enough apart exist.
    pub(crate) growth_per_h: Option<i64>,
    /// Its logical bytes' move over the same span, bytes per hour.
    pub(crate) fill_per_h: Option<i64>,
    /// The number the history keeps for it among the stores of its folder
    /// (`StoreHistory::number`); `None` for a store that is not one of several.
    pub(crate) n: Option<u32>,
}

/// What a walk found where a store is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StoreSeen {
    /// Nothing is there: this machine keeps no such store.
    Absent,
    /// The store.
    One(StoreBytes),
    /// The stores in an `each_child` folder, by their folder's name, in name order. Each
    /// carries the number the history keeps for it once the walker has noted it.
    Children(Vec<(String, StoreBytes)>),
}

/// One store as the walk around it last read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreRead {
    /// When that walk ended.
    pub(crate) at: Instant,
    /// What it found.
    pub(crate) seen: StoreSeen,
}

/// A history key: the store's asked path, and for a child store its folder's name after it.
pub(crate) fn history_key(path: &Path, child: Option<&str>) -> String {
    match child {
        Some(name) => format!("{}/{name}", path.display()),
        None => path.display().to_string(),
    }
}

fn bytes_of(sums: &aterm_diskwalk::Sums) -> StoreBytes {
    StoreBytes {
        alloc: sums.alloc_fair,
        logical: sums.logical,
        files: sums.files,
        growth_per_h: None,
        fill_per_h: None,
        n: None,
    }
}

/// A single file's bytes, or `None` for anything that is not a regular file.
fn file_bytes(meta: &std::fs::Metadata) -> Option<StoreBytes> {
    if !meta.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    let alloc = {
        use std::os::unix::fs::MetadataExt as _;
        meta.blocks().saturating_mul(512)
    };
    #[cfg(not(unix))]
    let alloc = meta.len();
    Some(StoreBytes {
        alloc,
        logical: meta.len(),
        files: 1,
        growth_per_h: None,
        fill_per_h: None,
        n: None,
    })
}

/// What `tree`, the finished walk of `root`, says of the store at `path` (canonical): its
/// folder's subtree sums, each child folder's for an `each_child` ask, or, where the walk
/// listed the folder that would hold it and found no folder of that name, one `lstat` of a
/// single file (`lstat`: the tree records folders only). `None` where the walk does not speak
/// for it: it lies outside the root, or in a folder the walk refused or did not enter.
pub(crate) fn read_store(
    path: &Path,
    each_child: bool,
    root: &Path,
    tree: &Tree,
    fold: bool,
    lstat: &dyn Fn(&Path) -> std::io::Result<std::fs::Metadata>,
) -> Option<StoreSeen> {
    let rel = super::scopes::rel_in(path, root, fold)?;
    let entered = |id: DirId| {
        tree.get(id)
            .filter(|r| r.refused.is_none() && !r.mount_boundary)
    };
    if let Some(id) = tree.find_folded(&rel, fold) {
        let rec = entered(id)?;
        if !each_child {
            return Some(StoreSeen::One(bytes_of(&rec.subtree)));
        }
        let mut children: Vec<(String, StoreBytes)> = tree
            .children(id)
            .into_iter()
            .filter_map(|c| {
                let rec = entered(c)?;
                let name = String::from_utf8(tree.name(c).to_vec()).ok()?;
                Some((name, bytes_of(&rec.subtree)))
            })
            .collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));
        return Some(StoreSeen::Children(children));
    }
    // No folder of that name. The walk speaks for it only where it entered the folder that
    // would hold it: a parent it never listed (missing too, or refused) leaves a missing parent
    // absent and a refused one unread.
    let parent = rel.parent()?;
    match tree.find_folded(parent, fold) {
        Some(id) => {
            entered(id)?;
        }
        None => {
            let mut above = parent;
            loop {
                above = above.parent()?;
                if let Some(id) = tree.find_folded(above, fold) {
                    entered(id)?;
                    return Some(StoreSeen::Absent);
                }
            }
        }
    }
    if each_child {
        return Some(StoreSeen::Absent);
    }
    match lstat(path) {
        Ok(meta) => file_bytes(&meta).map(StoreSeen::One),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(StoreSeen::Absent),
        Err(_) => None,
    }
}

/// The walker's history of the stores' sizes: in memory for the run, and kept as text in the
/// state folder so a growth per hour survives a restart and a process with no kept walk of
/// aterm's folders reads the last sizes.
#[derive(Default)]
pub(crate) struct StoreKeep {
    history: StoreHistory,
    loaded: bool,
    written: Option<Instant>,
}

impl StoreKeep {
    /// Take one measurement of every store in `seen` at `now` (`unix_s`), set each one's growth
    /// and each child store's number from the history, and write the history to `dir` when it
    /// is due.
    pub(crate) fn note(
        &mut self,
        dir: Option<&Path>,
        seen: &mut [(&Path, &mut StoreSeen)],
        now: Instant,
        unix_s: u64,
    ) {
        if !self.loaded {
            self.loaded = true;
            if let Some(history) = dir.and_then(read_history) {
                self.history = history;
            }
        }
        let mut take = |key: String, child: bool, bytes: &mut StoreBytes| {
            self.history.note(
                &key,
                StoreSample {
                    at_s: unix_s,
                    alloc: bytes.alloc,
                    logical: bytes.logical,
                    files: bytes.files,
                },
            );
            let growth = self.history.growth(&key);
            bytes.growth_per_h = growth.map(|g| g.alloc);
            bytes.fill_per_h = growth.map(|g| g.logical);
            bytes.n = child.then(|| self.history.number(&key));
        };
        for (path, seen) in seen.iter_mut() {
            let path: &Path = path;
            match &mut **seen {
                StoreSeen::Absent => {}
                StoreSeen::One(bytes) => take(history_key(path, None), false, bytes),
                StoreSeen::Children(children) => {
                    for (name, bytes) in children {
                        take(history_key(path, Some(name)), true, bytes);
                    }
                }
            }
        }
        self.history.prune(unix_s);
        let due = self
            .written
            .is_none_or(|t| now.saturating_duration_since(t) >= STORE_HISTORY_EVERY);
        if let Some(dir) = dir
            && due
        {
            self.written = Some(now);
            if let Err(e) = write_history(dir, &self.history) {
                aterm_log::warn!("disk walker: the stores' history was not written: {e}");
            }
        }
    }
}

/// Write `history` as the state folder `dir`'s history file.
#[cfg(unix)]
fn write_history(dir: &Path, history: &StoreHistory) -> std::io::Result<()> {
    aterm_uds::private_file::write_private(dir, STORE_HISTORY_FILE, history.encode().as_bytes())
}

/// No history is kept off unix: no store is listed there.
#[cfg(not(unix))]
fn write_history(_dir: &Path, _history: &StoreHistory) -> std::io::Result<()> {
    Ok(())
}

/// The history kept in the state folder `dir`; `None` where there is none or it cannot be read.
#[cfg(unix)]
pub(crate) fn read_history(dir: &Path) -> Option<StoreHistory> {
    let bytes = aterm_uds::private_file::read_private(
        dir,
        STORE_HISTORY_FILE,
        aterm_vitals::sys::stores::HISTORY_MAX_BYTES,
    )
    .ok()?;
    Some(StoreHistory::decode(std::str::from_utf8(&bytes).ok()?))
}

/// No history is kept off unix: no store is listed there.
#[cfg(not(unix))]
pub(crate) fn read_history(_dir: &Path) -> Option<StoreHistory> {
    None
}
