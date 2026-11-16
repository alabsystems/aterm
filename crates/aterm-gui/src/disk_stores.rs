// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable stores aterm itself keeps on this machine, for `sys disk`'s `store` lines:
//! where each is (from the path function its own code uses), the bound
//! its code holds it to (from the constant that code enforces) with what that code does at it
//! (refuse the write, or discard its oldest), and its size as the disk walker last read it. The walker sizes each from the walk of the own root around it; nothing here
//! walks, and a store no walk has read is unread, or stands at the last size the walker's kept
//! history holds, with that size's age.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_vitals::sys::stores::{AtBound, Growth, SAMPLE_KEEP_S, StoreBound};
use aterm_vitals::sys::{StoreFact, StoreHistory, StoreSample, StoreSize, StoreWord};

use crate::diskwalk_host::{
    OwnDirs, Published, StoreAsk, StoreBytes, StoreSeen, history_key, read_history,
};

/// One store: its word, where its code puts it, and the bound that code enforces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreSpec {
    /// Its fixed word.
    pub(crate) word: StoreWord,
    /// Its number among the stores of its word, where the word names several fixed ones.
    pub(crate) n: Option<u32>,
    /// Its folder or file.
    pub(crate) path: PathBuf,
    /// Each folder directly in `path` is one store of this word, numbered in name order.
    pub(crate) each_child: bool,
    /// The bound its code holds one store to, and what that code does at it.
    pub(crate) bound: Option<StoreBound>,
}

/// Every store aterm keeps under `dirs`, in the order the lines print. Each path is its own
/// code's (the vault's `Paths`, the Commons catalog's table root, the journal's spool, the
/// account stores, atpkg's prefix, the logs folder, the control folder's images, the telemetry
/// log) and each bound the constant that code enforces, so neither is a second copy.
///
/// How each bound is held is read off the same code. The vault's records log and the journal's
/// outbox refuse a write past theirs. The heard archive drops its oldest days and the telemetry
/// log its oldest file, so each stands at its bound once it has filled. A Commons table refuses
/// a write past the operations it holds, in count and in wire bytes, and neither is a size of
/// its files (the journal on disk also keeps what was discarded): its line names the byte bound
/// and its share is unread until something reads the table itself.
#[cfg(unix)]
pub(crate) fn specs(dirs: &OwnDirs) -> Vec<StoreSpec> {
    use aterm_agent::ai::connector::Cli;
    use aterm_db::alab::journal::spool::{MAX_SPOOL, Spool};
    let one = |word, path: PathBuf, bound: Option<StoreBound>| StoreSpec {
        word,
        n: None,
        path,
        each_child: false,
        bound,
    };
    let files = |bytes: u64, at: AtBound| {
        Some(StoreBound {
            bytes,
            at,
            walked: true,
        })
    };
    let mut out = Vec::new();
    if let Some(home) = dirs.home.as_deref() {
        let vault = aterm_secrets::client::Paths::locate_for_home(home);
        out.push(one(
            StoreWord::VaultRecords,
            vault.records_file(),
            files(
                aterm_secrets::membership_log::MAX_RECORDS_STREAM as u64,
                AtBound::Refuses,
            ),
        ));
        out.push(one(StoreWord::VaultServed, vault.served(), None));
        out.push(StoreSpec {
            word: StoreWord::CommonsTable,
            n: None,
            path: aterm_db::catalog::tables_root(&vault),
            each_child: true,
            bound: Some(StoreBound {
                bytes: aterm_db::table::store::operation_bytes_bound(),
                at: AtBound::Refuses,
                walked: false,
            }),
        });
    }
    if let Some(state) = dirs.state.as_deref() {
        let spool = Spool::under(state);
        let (heard, heard_max) = spool.heard_archive();
        out.push(one(
            StoreWord::JournalHeard,
            heard,
            files(heard_max, AtBound::Discards),
        ));
        out.push(one(
            StoreWord::JournalOutbox,
            spool.outbox_file(),
            files(MAX_SPOOL, AtBound::Refuses),
        ));
    }
    if let (Some(home), Some(state)) = (dirs.home.as_deref(), dirs.state.as_deref()) {
        let harness = aterm_agent::harness::cli::harness_state_in(state);
        for (i, cli) in Cli::ALL.into_iter().enumerate() {
            out.push(StoreSpec {
                n: u32::try_from(i + 1).ok(),
                ..one(
                    StoreWord::HarnessAccounts,
                    aterm_agent::harness::accounts::Store::at_for(&harness, cli, home).dir,
                    None,
                )
            });
        }
    }
    if let Some(home) = dirs.home.as_deref() {
        out.push(one(
            StoreWord::Packages,
            atpkg::platform::default_prefix(home),
            None,
        ));
    }
    if let Some(logs) = dirs.logs.clone() {
        out.push(one(StoreWord::Logs, logs, None));
    }
    if let Some(control) = dirs.control.as_deref() {
        out.push(one(
            StoreWord::Images,
            control.join(crate::control_auth::IMAGES_DIR),
            None,
        ));
    }
    if let Some(state) = dirs.state.as_deref() {
        let limits = aterm_telemetry::log::Limits::DEFAULT;
        out.push(one(
            StoreWord::Telemetry,
            aterm_telemetry::log::dir_in(state),
            files(
                limits.file_bytes.saturating_mul(u64::from(limits.files)),
                AtBound::Discards,
            ),
        ));
    }
    out
}

/// No store is listed where the vault, the Commons tables and the journal do not build.
#[cfg(not(unix))]
pub(crate) fn specs(_dirs: &OwnDirs) -> Vec<StoreSpec> {
    Vec::new()
}

/// What the walker is asked to size for `specs`.
pub(crate) fn asks(specs: &[StoreSpec]) -> Vec<StoreAsk> {
    specs
        .iter()
        .map(|s| StoreAsk {
            path: s.path.clone(),
            each_child: s.each_child,
        })
        .collect()
}

/// `spec`'s line with `size` and `growth`: where it lies and, under the home, its path there.
/// A child store prints the folder that holds it, never its own name.
fn fact(
    spec: &StoreSpec,
    n: Option<u32>,
    home: Option<&Path>,
    size: Option<StoreSize>,
    growth: Option<Growth>,
) -> StoreFact {
    let home_text = home.map(|h| h.display().to_string());
    StoreFact {
        word: spec.word,
        n,
        under: aterm_vitals::sys::under(&spec.path.display().to_string(), home_text.as_deref()),
        rel: home
            .and_then(|h| spec.path.strip_prefix(h).ok())
            .map(|rel| rel.display().to_string()),
        size,
        bound: spec.bound,
        growth_per_h: growth.map(|g| g.alloc),
        fill_per_h: growth.map(|g| g.logical),
    }
}

/// A walked store's growth, where the history gave it one.
fn walked_growth(bytes: &StoreBytes) -> Option<Growth> {
    Some(Growth {
        alloc: bytes.growth_per_h?,
        logical: bytes.fill_per_h?,
    })
}

fn walked(bytes: &StoreBytes, age_s: u64) -> StoreSize {
    StoreSize {
        alloc: bytes.alloc,
        logical: bytes.logical,
        files: bytes.files,
        age_s: Some(age_s),
    }
}

/// A kept sample as a size, where it is no older than the history keeps one.
fn kept_size(sample: StoreSample, unix_s: u64) -> Option<StoreSize> {
    let age_s = unix_s.saturating_sub(sample.at_s);
    (age_s <= SAMPLE_KEEP_S).then_some(StoreSize {
        alloc: sample.alloc,
        logical: sample.logical,
        files: sample.files,
        age_s: Some(age_s),
    })
}

/// `specs` as store lines: each store as `board` holds it (the walk of the root around it), a
/// store this machine does not keep left out; and for one no kept walk holds, its last size in
/// `kept` (the walker's history, read only when one is needed) with its age, else unread. A
/// store that is one of a folder's several carries the number the history keeps for it, and the
/// lines of one folder stand in that number's order: a store that comes or goes renumbers none.
pub(crate) fn facts(
    specs: &[StoreSpec],
    board: &Published,
    home: Option<&Path>,
    kept: &dyn Fn() -> Option<StoreHistory>,
    now: Instant,
    unix_s: u64,
) -> Vec<StoreFact> {
    let history: std::cell::OnceCell<Option<StoreHistory>> = std::cell::OnceCell::new();
    let history = || history.get_or_init(kept).as_ref();
    let mut out = Vec::new();
    for spec in specs {
        if let Some(read) = board.stores.get(&spec.path) {
            let age_s = now.saturating_duration_since(read.at).as_secs();
            match &read.seen {
                StoreSeen::Absent => {}
                StoreSeen::One(bytes) => out.push(fact(
                    spec,
                    spec.n,
                    home,
                    Some(walked(bytes, age_s)),
                    walked_growth(bytes),
                )),
                StoreSeen::Children(children) => {
                    let mut numbered: Vec<&StoreBytes> =
                        children.iter().map(|(_, bytes)| bytes).collect();
                    numbered.sort_by_key(|bytes| bytes.n);
                    for bytes in numbered {
                        out.push(fact(
                            spec,
                            bytes.n,
                            home,
                            Some(walked(bytes, age_s)),
                            walked_growth(bytes),
                        ));
                    }
                }
            }
            continue;
        }
        if !spec.each_child {
            let key = history_key(&spec.path, None);
            let last = history().and_then(|h| Some((h.latest(&key)?, h.growth(&key))));
            let size = last.and_then(|(sample, _)| kept_size(sample, unix_s));
            let growth = last
                .and_then(|(_, growth)| growth)
                .filter(|_| size.is_some());
            out.push(fact(spec, spec.n, home, size, growth));
            continue;
        }
        // The children of the last walk that read the folder: those measured at its newest
        // time, each by the number the history keeps for it.
        let prefix = history_key(&spec.path, None);
        let children: Vec<(&str, StoreSample)> = history()
            .map(|h| h.under(&prefix).collect())
            .unwrap_or_default();
        let newest = children.iter().map(|(_, s)| s.at_s).max();
        let mut current: Vec<(u32, &str, StoreSize)> = children
            .iter()
            .filter(|(_, s)| Some(s.at_s) == newest)
            .filter_map(|(key, s)| {
                let n = history()?.number_of(key)?;
                Some((n, *key, kept_size(*s, unix_s)?))
            })
            .collect();
        current.sort_by_key(|(n, _, _)| *n);
        if current.is_empty() {
            out.push(fact(spec, None, home, None, None));
        }
        for (n, key, size) in current {
            let growth = history().and_then(|h| h.growth(key));
            out.push(fact(spec, Some(n), home, Some(size), growth));
        }
    }
    out
}

/// This machine's store lines now: the walker's newest board, and its kept history for a store
/// no kept walk holds. It demands no scope: `sys disk` demands aterm's, whose walk sizes the
/// stores, and a reader that demands none reads what is there.
pub(crate) fn facts_now() -> Vec<StoreFact> {
    let dirs = OwnDirs::live();
    let board = crate::diskwalk_host::DISKWALK.board();
    let unix_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    facts(
        &specs(&dirs),
        &board,
        dirs.home.as_deref(),
        &|| dirs.state.as_deref().and_then(read_history),
        Instant::now(),
        unix_s,
    )
}

/// The most entries of a store's own folder [`sized_now`] reads.
const FLAT_ENTRIES: usize = 64;

/// A store's logical bytes and files as they are now, by one `lstat` of its file or one
/// listing of its own folder, never a walk: `None` where it is not there, cannot be read, or is
/// no flat store (its folder holds a folder, or more than [`FLAT_ENTRIES`] entries).
fn sized_now(path: &Path) -> Option<StoreSize> {
    let flat = |logical: u64, files: u64| StoreSize {
        alloc: logical,
        logical,
        files,
        age_s: Some(0),
    };
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_file() {
        return Some(flat(meta.len(), 1));
    }
    if !meta.file_type().is_dir() {
        return None;
    }
    let (mut logical, mut files) = (0u64, 0u64);
    for (seen, entry) in std::fs::read_dir(path).ok()?.enumerate() {
        if seen >= FLAT_ENTRIES {
            return None;
        }
        let meta = std::fs::symlink_metadata(entry.ok()?.path()).ok()?;
        if meta.file_type().is_dir() {
            return None;
        }
        if meta.file_type().is_file() {
            logical = logical.saturating_add(meta.len());
            files += 1;
        }
    }
    Some(flat(logical, files))
}

/// The fullest of `specs`' stores that refuse writes at a bound over their files, each sized
/// by `sized`: the one a survey row names. A store that discards its oldest at its bound, one
/// whose bound no file size reads (a Commons table) and one that is not there are never it.
fn worst_of(
    specs: &[StoreSpec],
    sized: &dyn Fn(&Path) -> Option<StoreSize>,
) -> Option<aterm_agent::survey_facts::StoreWorst> {
    let facts: Vec<StoreFact> = specs
        .iter()
        .filter(|spec| {
            !spec.each_child
                && spec
                    .bound
                    .is_some_and(|b| b.walked && b.at == AtBound::Refuses)
        })
        .filter_map(|spec| Some(fact(spec, spec.n, None, Some(sized(&spec.path)?), None)))
        .collect();
    let worst = aterm_vitals::sys::stores::worst(&facts)?;
    aterm_agent::survey_facts::StoreWorst::new(worst.word.word(), worst.used_pct()?)
}

/// This machine's fullest store that refuses writes at a bound, for its survey facts
/// (`store_worst=`, `store_worst_pct=`): each such store is one file or one flat folder, sized
/// here by its own `lstat`s ([`sized_now`]). So the figure is this moment's on a Mac no reader
/// has asked `sys disk` of, and in a node job, which runs no walker; it costs no walk and waits
/// on none.
pub(crate) fn worst_now() -> Option<aterm_agent::survey_facts::StoreWorst> {
    worst_of(&specs(&OwnDirs::live()), &sized_now)
}

#[cfg(all(test, unix))]
#[path = "disk_stores_tests.rs"]
mod tests;
