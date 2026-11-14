// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REBUILDABLE BUDGET'S SIZING (`aterm ctl sys disk`'s `budget` record): which looks may
//! begin one ([`admitted`], [`due`]), the bytes it counts ([`rebuildable`], [`size`]), and the
//! record of the last one every process of the account shares ([`Record`], [`claim`],
//! [`keep`]). A reading: nothing here removes, journals, raises or moves a level, and the
//! sizing takes no build's lock (each tree is sized by [`target::total_bytes_until`], never
//! judged for removal).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Map, Value};

use super::{Cap, Hog, Level, Rebuildable, collapse_nested_holders, discover, target};

/// The least time, in seconds, from the start of one sizing to the start of the next, across
/// every process of the account ([`due`], [`claim`]).
pub const EVERY_S: i64 = 3600;

/// The one wall-clock bound on a whole sizing: the search ([`discover::Budget::SHIPPED`]'s own
/// wall within it) and every size walk. A sizing that meets it stops and is a floor.
pub const WALL: Duration = Duration::from_secs(120);

/// The record of the last sizing, in the harness state directory ([`Record`]).
pub const FILE: &str = "disk-budget.json";

/// The most bytes of [`FILE`] read: one record of a few numbers.
const FILE_MAX: u64 = 4096;

/// Whether a look of the disk watch may begin a sizing: the volume reads `ok`, this process is
/// the account's own watch (`owner`), no agent's demand and no fill projection stands (those
/// looks belong to a pass), and the budget is not off. The hourly rule is [`due`]'s.
#[must_use]
pub fn admitted(level: Level, owner: bool, demand: bool, projected: bool, cap: Cap) -> bool {
    level == Level::Ok && owner && !demand && !projected && cap != Cap::Off
}

/// Whether a sizing is due at unix second `now`: none began (`began` is `None`), the last
/// began [`EVERY_S`] or more ago, or the clock reads before it (a clock set back does not hold
/// the budget unsized for as long as it was set back).
#[must_use]
pub fn due(began: Option<i64>, now: i64) -> bool {
    began.is_none_or(|at| now < at || now.saturating_sub(at) >= EVERY_S)
}

/// The rebuildable bytes the volume holds now: each registered build cache under `roots`'s
/// home and each build directory a search from `roots` finds (under `search`), those
/// `same_volume` places on the volume, sized by [`size`] before `deadline`. The figure is a
/// floor (`partial`) where the search was cut short, too.
#[must_use]
pub fn rebuildable(
    roots: &discover::Roots,
    search: discover::Budget,
    deadline: Instant,
    same_volume: &dyn Fn(&Path) -> bool,
) -> Rebuildable {
    let _exposed = discover::expose_placeholders().ok();
    let wall = deadline
        .saturating_duration_since(Instant::now())
        .min(search.wall);
    let found = discover::discover(roots, discover::Budget { wall, ..search });
    let caches: Vec<PathBuf> = roots.home.as_deref().map_or_else(Vec::new, |home| {
        atpkg::build_cache::registered::roots(home)
            .into_iter()
            .filter(|cache| cache.is_current(home))
            .map(|cache| cache.root)
            .collect()
    });
    let dirs: Vec<PathBuf> = found.found.into_iter().map(|f| f.path).collect();
    let mut sized = size(&caches, &dirs, same_volume, deadline);
    sized.partial |= found.cut.is_some();
    sized
}

/// The bytes under `caches` and `dirs` that `same_volume` places on the volume, each tree
/// sized once ([`target::total_bytes_until`], on the tree's own device) before `deadline`:
/// the caches first, a path inside a tree already sized whole is not walked again, and a
/// tree nested in another is counted in the outer one ([`collapse_nested_holders`]). The
/// figure is a floor where a walk or the whole met `deadline` or its entry bound.
#[must_use]
pub fn size(
    caches: &[PathBuf],
    dirs: &[PathBuf],
    same_volume: &dyn Fn(&Path) -> bool,
    deadline: Instant,
) -> Rebuildable {
    let mut by: BTreeMap<PathBuf, Hog> = BTreeMap::new();
    let mut partial = false;
    for path in caches.iter().chain(dirs) {
        if by.contains_key(path)
            || by
                .values()
                .any(|h| !h.bytes_partial && path.starts_with(&h.path))
            || !same_volume(path)
        {
            continue;
        }
        if Instant::now() >= deadline {
            partial = true;
            break;
        }
        let (bytes, bytes_partial) = target::total_bytes_until(path, deadline);
        by.insert(
            path.clone(),
            Hog {
                path: path.clone(),
                bytes,
                bytes_partial,
                why: String::new(),
                serves: None,
                built_s_ago: None,
            },
        );
    }
    let mut sized = Rebuildable { bytes: 0, partial };
    for root in collapse_nested_holders(by) {
        sized.bytes = sized.bytes.saturating_add(root.bytes);
        sized.partial |= root.bytes_partial;
    }
    sized
}

/// One sizing: the volume's figures at the instant the rebuildable bytes were counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizing {
    /// The volume sized, by its spelled key (`aterm_sysprobe::volumes::VolumeKey::spelled`).
    pub volume: String,
    /// Its size, bytes.
    pub total: u64,
    /// Its used bytes: its size less what this user may still write.
    pub used: u64,
    /// The rebuildable bytes counted, and whether that figure is a floor.
    pub sized: Rebuildable,
    /// Unix second the sizing ended.
    pub sized_unix: i64,
}

/// What [`FILE`] holds: when the last sizing began (the claim every process reads before it
/// begins one) and the last sizing that ended, which a process that starts prints with its
/// true age.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// Unix second the last sizing began.
    pub began_unix: Option<i64>,
    /// The last sizing that ended.
    pub last: Option<Sizing>,
}

impl Record {
    /// The record as [`FILE`] holds it.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        if let Some(began) = self.began_unix {
            o.insert("began_unix".to_owned(), Value::from(began));
        }
        if let Some(s) = &self.last {
            let mut l = Map::new();
            l.insert("volume".to_owned(), Value::from(s.volume.clone()));
            l.insert("total".to_owned(), Value::from(s.total));
            l.insert("used".to_owned(), Value::from(s.used));
            l.insert("rebuildable".to_owned(), Value::from(s.sized.bytes));
            l.insert("partial".to_owned(), Value::from(s.sized.partial));
            l.insert("sized_unix".to_owned(), Value::from(s.sized_unix));
            o.insert("last".to_owned(), Value::Object(l));
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The record `text` holds; `None` where it is not one ([`Self::encode`]'s schema 1).
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let v = aterm_json::from_str::<Value>(text).ok()?;
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let began_unix = match v.get("began_unix") {
            None => None,
            Some(b) => Some(b.as_i64()?),
        };
        let last = match v.get("last") {
            None => None,
            Some(l) => Some(Sizing {
                volume: l.get("volume")?.as_str()?.to_owned(),
                total: l.get("total")?.as_u64()?,
                used: l.get("used")?.as_u64()?,
                sized: Rebuildable {
                    bytes: l.get("rebuildable")?.as_u64()?,
                    partial: l.get("partial")?.as_bool()?,
                },
                sized_unix: l.get("sized_unix")?.as_i64()?,
            }),
        };
        Some(Self { began_unix, last })
    }

    /// The record in `dir`'s [`FILE`]: empty where there is none or it is not one.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        read(dir).0
    }
}

/// [`FILE`]'s record and its text as read (`None`: no file, or one that could not be read).
fn read(dir: &Path) -> (Record, Option<String>) {
    match aterm_uds::atomic_file::read_regular_text(&dir.join(FILE), FILE_MAX) {
        Ok(text) => (Record::decode(&text).unwrap_or_default(), Some(text)),
        Err(_) => (Record::default(), None),
    }
}

/// What [`claim`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// This process begins the sizing: [`FILE`] now says it began at the claim's second. The
    /// record as it was before.
    Claimed(Record),
    /// A sizing began within [`EVERY_S`] (in this process or another): none is begun. The
    /// record as read.
    NotDue(Record),
}

/// Claim the next sizing at unix second `now` for every process of the account, as the daily
/// cache maintenance stamps its claim (`maintenance::claim`): read [`FILE`] in `dir`, and
/// where a sizing is [`due`] write it back saying one began `now`, the write refused where
/// the file changed since it was read (another process claimed meanwhile). The stamp goes
/// before the walk, so a sizing that fails or is cut short is not begun again within the hour.
///
/// # Errors
///
/// The clock is unread (`now` not above 0), `dir` could not be made, or the write failed or
/// was refused.
pub fn claim(dir: &Path, now: i64) -> io::Result<Claim> {
    if now <= 0 {
        return Err(io::Error::other("the budget's clock is unread"));
    }
    std::fs::create_dir_all(dir)?;
    let (record, text) = read(dir);
    if !due(record.began_unix, now) {
        return Ok(Claim::NotDue(record));
    }
    let claimed = Record {
        began_unix: Some(now),
        ..record.clone()
    };
    write(dir, &claimed, text.as_deref())?;
    Ok(Claim::Claimed(record))
}

/// Keep `sizing`, begun at unix second `began` ([`claim`]), as [`FILE`]'s last sizing in `dir`.
///
/// # Errors
///
/// The file no longer says this sizing's claim (another process claimed since), or the write
/// failed.
pub fn keep(dir: &Path, began: i64, sizing: Sizing) -> io::Result<()> {
    let (record, text) = read(dir);
    if record.began_unix != Some(began) {
        return Err(io::Error::other(
            "another sizing was claimed since this one began",
        ));
    }
    write(
        dir,
        &Record {
            began_unix: Some(began),
            last: Some(sizing),
        },
        text.as_deref(),
    )
}

/// Write `record` to `dir`'s [`FILE`] where the file still reads `was` (`None`: unread).
fn write(dir: &Path, record: &Record, was: Option<&str>) -> io::Result<()> {
    aterm_uds::atomic_file::write_checked(
        &dir.join(FILE),
        record.encode().as_bytes(),
        0o600,
        || {
            if read(dir).1.as_deref() == was {
                Ok(())
            } else {
                Err(io::Error::other(
                    "the budget's record changed while it was written",
                ))
            }
        },
    )
}

#[cfg(test)]
#[path = "disk_budget_tests.rs"]
mod tests;
