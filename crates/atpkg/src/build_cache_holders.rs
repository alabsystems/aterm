// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHO HOLDS THE DISK: the largest folders of build output, caches, campaign scopes, worktree
//! roots and checkouts, measured within a budget and kept, so a record written when free space
//! crosses the disk watch's floor names them and not only the free figure.
//!
//! One measure walks a folder's whole tree, and the folders together hold terabytes: no look
//! can walk them all. [`measure`] therefore keeps what it measured ([`ledger`]) and spends each
//! call's budget on what was never measured and then on what was measured longest ago, so
//! successive calls converge and every figure says how old it is ([`Holder::measured`]). A walk
//! the budget ends keeps the folder's earlier whole figure where there is one, and otherwise
//! says the bytes it counted as a lower bound ([`Holder::complete`]). A whole figure younger than
//! [`FRESH`] is not walked again, so a volume that stays below its floor is measured once an
//! hour and not at every look.
//!
//! WHAT IS MEASURED ([`candidates`]): each unit of the leftover places
//! ([`super::leftovers::places`]), the shared and the tooling cache, each folder directly in
//! `$HOME` outside the folders never looked into (`checkout::HOME_SKIP`), and what the caller
//! adds (the temporary folder's build scratch, a shared folder's entries). Nothing here removes
//! anything: a checkout in `$HOME` is listed for its owner and never collected.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// The ledger's place under `home`: `Library/Caches/aterm/disk-holders.json`.
const LEDGER: &str = "Library/Caches/aterm/disk-holders.json";

/// The holders a record or a listing names.
pub const NAMED: usize = 20;

/// A folder measured whole within this long is not walked again.
pub const FRESH: Duration = Duration::from_secs(60 * 60);

/// One measured folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    /// The folder.
    pub path: PathBuf,
    /// What it is, as [`candidates`] named it.
    pub kind: String,
    /// The bytes its tree holds on disk; a lower bound unless [`Self::complete`].
    pub bytes: u64,
    /// Whether the walk that counted [`Self::bytes`] read the whole tree.
    pub complete: bool,
    /// When it was measured, in seconds since the epoch.
    pub measured: u64,
    /// When the folder itself was last written, in seconds since the epoch, when readable.
    pub written: Option<u64>,
}

/// What one [`measure`] knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Survey {
    /// Every measured folder, largest first.
    pub holders: Vec<Holder>,
    /// Candidate folders no call has measured yet.
    pub unmeasured: usize,
}

impl Survey {
    /// The sum of every measured folder's bytes. Folders are disjoint, so no byte is counted
    /// twice but for files hard-linked across two of them.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.holders
            .iter()
            .fold(0u64, |sum, holder| sum.saturating_add(holder.bytes))
    }
}

/// The ledger file under `home`.
#[must_use]
pub fn ledger(home: &Path) -> PathBuf {
    home.join(LEDGER)
}

/// Seconds since the epoch at `at`; 0 before it.
fn secs(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The folders [`measure`] sizes under `home`, each with what it is, and `extra` after them. No
/// folder is another's ancestor: a place's units stand for the place, and the places' own roots
/// and the folders never looked into are left out of `$HOME`'s children.
#[must_use]
pub fn candidates(home: &Path, extra: &[(PathBuf, &'static str)]) -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();
    let places = super::leftovers::places(home);
    for place in &places {
        out.extend(place.units().into_iter().map(|unit| (unit, place.kind)));
    }
    for (root, kind) in [
        (super::cache_root(home), "shared build cache"),
        (super::tooling::root(home), "tooling cache"),
    ] {
        if std::fs::symlink_metadata(&root).is_ok_and(|meta| meta.is_dir()) {
            out.push((root, kind));
        }
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|child| {
            super::checkout::never_looked_at(child, Some(home)).is_none()
                && child.file_name().is_some_and(|name| name != ".Trash")
                && !places.iter().any(|place| place.root.starts_with(child))
        })
        .collect();
    children.sort();
    out.extend(children.into_iter().map(|child| (child, "folder in $HOME")));
    out.extend(
        extra
            .iter()
            .filter(|(path, _)| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
            .cloned(),
    );
    out
}

/// What the live calls add to [`candidates`]: the temporary folder's `.ubd` build scratch, and
/// on macOS each folder of `/Users//Shared`, where campaigns also leave roots.
#[must_use]
pub fn extra_here() -> Vec<(PathBuf, &'static str)> {
    let mut extra = vec![(std::env::temp_dir().join(".ubd"), "temporary build scratch")];
    if cfg!(target_os = "macos") {
        let mut shared: Vec<PathBuf> = std::fs::read_dir("/Users//Shared")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        shared.sort();
        extra.extend(shared.into_iter().map(|path| (path, "shared folder")));
    }
    extra
}

/// [`extra_here`] when `home` is this process's own home, and nothing for any other folder: a
/// fixture home is measured alone.
#[must_use]
pub fn extra_for(home: &Path) -> Vec<(PathBuf, &'static str)> {
    if aterm_types::dirs::home_dir().as_deref() == Some(home) {
        extra_here()
    } else {
        Vec::new()
    }
}

/// The ledger at `path` as it stands: every folder an earlier [`measure`] kept. Missing or
/// unreadable, it is empty.
#[must_use]
pub fn read(path: &Path) -> Vec<Holder> {
    crate::metadata_io::open_regular(path)
        .ok()
        .and_then(|mut file| {
            use std::io::Read as _;
            let mut text = String::new();
            file.read_to_string(&mut text).ok()?;
            aterm_json::from_str::<Vec<Holder>>(&text).ok()
        })
        .unwrap_or_default()
}

/// Write the ledger at `path`, through a sibling renamed over it.
fn write(path: &Path, holders: &[Holder]) {
    let Ok(text) = aterm_json::to_string(holders) else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    let _ = std::fs::create_dir_all(parent);
    let staged = path.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&staged, text).is_ok() && std::fs::rename(&staged, path).is_err() {
        let _ = std::fs::remove_file(&staged);
    }
}

/// Measure `candidates` at `now` until `stopped` says so, the never measured first and then the
/// longest ago, keep the result in the ledger at `ledger`, and return all it knows. A folder
/// measured whole within [`FRESH`] is not walked; one that is no longer a candidate leaves the
/// ledger; a walk `stopped` ended keeps the earlier whole figure, or says what it counted as a
/// lower bound.
pub fn measure_in(
    ledger: &Path,
    candidates: &[(PathBuf, &'static str)],
    now: SystemTime,
    stopped: &dyn Fn() -> bool,
) -> Survey {
    let mut prior: BTreeMap<PathBuf, Holder> = read(ledger)
        .into_iter()
        .map(|holder| (holder.path.clone(), holder))
        .collect();
    let mut order: Vec<(u64, usize)> = candidates
        .iter()
        .enumerate()
        .map(|(index, (path, _))| {
            (
                prior.get(path).map_or(0, |known| known.measured.max(1)),
                index,
            )
        })
        .collect();
    order.sort();
    let mut known: BTreeMap<PathBuf, Holder> = BTreeMap::new();
    for (path, kind) in candidates {
        if let Some(mut holder) = prior.remove(path) {
            (*kind).clone_into(&mut holder.kind);
            known.insert(path.clone(), holder);
        }
    }
    for (_, index) in order {
        let (path, kind) = &candidates[index];
        if known.get(path).is_some_and(|earlier| {
            earlier.complete && secs(now).saturating_sub(earlier.measured) < FRESH.as_secs()
        }) {
            continue;
        }
        if stopped() {
            break;
        }
        aterm_uds::progress::tick();
        let cut = std::cell::Cell::new(false);
        let bytes = aterm_capacity::footprint::allocated_until(std::slice::from_ref(path), &|| {
            let stop = stopped();
            cut.set(cut.get() || stop);
            stop
        });
        let complete = !cut.get();
        if !complete && known.get(path).is_some_and(|earlier| earlier.complete) {
            continue;
        }
        known.insert(
            path.clone(),
            Holder {
                path: path.clone(),
                kind: (*kind).to_owned(),
                bytes,
                complete,
                measured: secs(now),
                written: super::last_written(path).map(secs),
            },
        );
    }
    let mut holders: Vec<Holder> = known.into_values().collect();
    write(ledger, &holders);
    holders.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    Survey {
        unmeasured: candidates.len().saturating_sub(holders.len()),
        holders,
    }
}

/// [`measure_in`] over `home`'s own candidates and `extra`, for at most `budget`, kept in
/// `home`'s ledger ([`ledger`]).
pub fn measure(
    home: &Path,
    extra: &[(PathBuf, &'static str)],
    now: SystemTime,
    budget: Duration,
) -> Survey {
    let began = std::time::Instant::now();
    measure_in(&ledger(home), &candidates(home, extra), now, &|| {
        began.elapsed() >= budget
    })
}

/// [`measure`] over this user's home and [`extra_here`], now, for at most `budget`; `None`
/// without a home.
pub fn measure_home(budget: Duration) -> Option<Survey> {
    let home = aterm_types::dirs::home_dir()?;
    Some(measure(&home, &extra_here(), SystemTime::now(), budget))
}

/// A span in the largest unit that fits: `3d`, `5h`, `12m`, `40s`.
fn span(secs: u64) -> String {
    match secs {
        s if s >= 2 * 86_400 => format!("{}d", s / 86_400),
        s if s >= 2 * 3_600 => format!("{}h", s / 3_600),
        s if s >= 120 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

impl Holder {
    /// One line of a listing: its size (`at least` for a lower bound), path, kind, when the
    /// folder itself was last written and how old the measure is, at `now`.
    #[must_use]
    pub fn line(&self, now: SystemTime) -> String {
        let now = secs(now);
        let written = self.written.map_or_else(String::new, |at| {
            format!("; last written {} ago", span(now.saturating_sub(at)))
        });
        format!(
            "{}{:>10}  {}\n            {}{written}; measured {} ago",
            if self.complete { "" } else { "at least " },
            crate::cost::human_bytes(self.bytes),
            self.path.display(),
            self.kind,
            span(now.saturating_sub(self.measured)),
        )
    }
}

#[cfg(all(test, unix))]
#[path = "build_cache_holders_tests.rs"]
mod tests;
