// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB JOURNAL (docs/DESIGN-alab-lab-model-2026-11-01.md §4.17, §8, §11 step 8): what each
//! Mac sees while it uses an account anyway, published into the journal's week ring
//! (`catalog::ring::JOURNAL`) with no poller and no provider read, so "who last used this
//! account, on which Mac", "where is it signed in" and "its current quota" come from the Macs
//! that use the accounts.
//!
//! - [`event`]: one event in a closed vocabulary, `Copy`, no heap value.
//! - [`Spool`]: the harness notes events into `<state>/alab/spool.jsonl`, one bounded append
//!   each, with no lock and no allocation (the typing hook).
//! - `book`: the flusher's own state: per account it used, the facts of its `use` row; the
//!   pace that keeps its records within its fixed share of a week; record packing.
//! - `relay`: the 16 relay buckets, so an account's last use outlives the weeks that held it.
//! - `flush` (the Commons owner's `alab-journal` thread, `super::upkeep`): the spool into the
//!   outbox, the window's tables into the heard archive and the relays, one record when due.
//! - `heard`: this Mac's archive of every batch it read from the ring, 120 days or 64 MiB.
//! - [`place`]: what this Mac holds that no preference of its own names (the harness's
//!   [`place::Holding`]s), written as `place` rows; and the reader of every Mac's live rows.
//!
//! The ring's collections are compiled ATT/1 schemas ([`EV`], [`USE`], [`SUM`], [`RELAY`],
//! [`PLACE`]); every row lives at its writer's own Commons machine uuid and is inert at any
//! other key (`typed::owned`).

pub mod event;
pub mod place;
pub mod spool;

pub(crate) mod book;
pub(crate) mod flush;
mod heard;
mod relay;

pub use book::Presence;
pub use event::{Event, Kind, Pk, Res, Source, SubKey, Window, Word};
pub use spool::{Appended, Spool};

use crate::typed::owned::caps;
use crate::typed::{Collection, Column, Derive, Kind as Merge, Table, Ty, Writer, Writers};

/// The journal's application and role, as its ring names them.
pub const APPLICATION: &str = "alab";
pub const ROLE: &str = "journal";

crate::att_collection!(
    /// Events this Mac saw (design §4.17 `ev`): `<uuid32>.<n>`, one batch a record, at most
    /// 24 KiB, each event at most 192 B.
    pub EV = Collection {
        name: "ev",
        version: 1,
        kind: Merge::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("e", Ty::Json(caps::EV_BATCH)).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: caps::EV_BATCH,
    }
);

crate::att_collection!(
    /// This Mac's latest facts about one account (design §4.17 `use`):
    /// `<account or sub-account pk>.<uuid32>`, at most 1,536 B. A sub-account's row (one
    /// provider workspace's readings, §4.13) names its `AccountKey` in `key` and, since version
    /// 2, its account in `account`.
    pub USE = Collection {
        name: "use",
        version: 2,
        kind: Merge::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("period", Ty::Uint).writer(Writer::Mine),
            Column::new("first", Ty::Uint).writer(Writer::Mine),
            Column::new("last_start", Ty::Uint).writer(Writer::Mine),
            Column::new("last_end", Ty::Uint).writer(Writer::Mine),
            Column::new("sessions_7d", Ty::Uint).writer(Writer::Mine),
            Column::new("signin", Ty::Json(128)).null().writer(Writer::Mine),
            Column::new("quota", Ty::Json(1024)).writer(Writer::Mine),
            Column::new("wall", Ty::Json(512)).writer(Writer::Mine),
            Column::new("plan", Ty::Json(64)).null().writer(Writer::Mine),
            Column::new("key", Ty::Hex(12)).null().writer(Writer::Mine),
            Column::new("account", Ty::Ref("account"))
                .null()
                .writer(Writer::Mine)
                .since(2),
        ],
        unique: &[],
        max_row: caps::USE_ROW,
    }
);

crate::att_collection!(
    /// This Mac's presence and versions (design §4.17 `sum`): `<uuid32>`, at most 2 KiB,
    /// written in its first record of each week, even idle.
    pub SUM = Collection {
        name: "sum",
        version: 1,
        kind: Merge::Owned,
        writers: Writers::SelfKey,
        prefix: "",
        derive: Derive::SignerUuid,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("period", Ty::Uint).writer(Writer::Mine),
            Column::new("at", Ty::Uint).writer(Writer::Mine),
            Column::new("label", Ty::Label).null().writer(Writer::Mine),
            Column::new("aterm", Ty::Text(32)).writer(Writer::Mine),
            Column::new("os", Ty::Uint).null().writer(Writer::Mine),
            Column::new("clis", Ty::Json(512)).writer(Writer::Mine),
            Column::new("used", Ty::Uint).writer(Writer::Mine),
            Column::new("journal", Ty::Enum(&["on", "local", "off"])).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: caps::SUM_ROW,
    }
);

crate::att_collection!(
    /// The newest last-use news this Mac knows for one bucket of accounts (design §4.17
    /// `relay`): `rl-<bucket>.<uuid32>`, at most 12 KiB.
    pub RELAY = Collection {
        name: "relay",
        version: 1,
        kind: Merge::Owned,
        writers: Writers::SelfSuffix,
        prefix: "rl",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("period", Ty::Uint).writer(Writer::Mine),
            Column::new("b", Ty::Uint).writer(Writer::Mine),
            Column::new("e", Ty::Json(caps::RELAY_ROW)).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: caps::RELAY_ROW,
    }
);

crate::att_collection!(
    /// This Mac's automatic use of an account (design §4.17 `place`, revision 2):
    /// `<account pk>.<uuid32>`, at most 512 B, with no seat: the key names the account and the
    /// Mac. The flusher writes one for each account the harness holds a row of that no preference
    /// here names ([`place`]); `until` is when it lapses unless restated.
    pub PLACE = Collection {
        name: "place",
        version: 1,
        kind: Merge::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("why", Ty::Enum(&["fill", "borrow", "stranded", "probation"]))
                .writer(Writer::Mine),
            Column::new("since", Ty::Uint).writer(Writer::Mine),
            Column::new("until", Ty::Uint).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: caps::PLACE_ROW,
    }
);

/// The journal ring's collections, as one typed table. Its records are automatic by nature;
/// the ring's fixed shares bound them, not a quota.
pub const JOURNAL: Table = Table {
    application: APPLICATION,
    role: ROLE,
    collections: &[&EV, &USE, &SUM, &RELAY, &PLACE],
    self_quota: usize::MAX,
    ends: &[],
};

/// What this Mac's journal is doing, as `aterm db alab check` reports it: `off` where
/// `[alab] journal = false` ([`publishing`]), `off: aterm.toml unreadable` where the file is
/// there and cannot be read as TOML, `local` with the reason its last flush was kept
/// (`journal: no storage slot`, a full share), else `on`.
pub fn status(spool: &Spool, config: Option<&std::path::Path>) -> String {
    match setting(config) {
        Setting::Off => return "off".to_owned(),
        Setting::Unread => return "off: aterm.toml unreadable".to_owned(),
        Setting::On => {}
    }
    match book::Book::load(spool).kept {
        Some(why) => format!("local: {why}"),
        None => "on".to_owned(),
    }
}

/// What the `aterm.toml` at `config` says of the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    On,
    /// `[alab] journal = false`.
    Off,
    /// The file is there and cannot be read, is over 1 MiB, is not UTF-8 or does not parse.
    Unread,
}

fn setting(config: Option<&std::path::Path>) -> Setting {
    let Some(config) = config else {
        return Setting::On;
    };
    let bytes = match aterm_secrets::fsx::read_aterm_config(config, 1024 * 1024) {
        Ok(None) => return Setting::On,
        Ok(Some(bytes)) => bytes,
        Err(_) => return Setting::Unread,
    };
    let Some(table) = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| aterm_toml::from_str::<aterm_toml::Table>(text).ok())
    else {
        return Setting::Unread;
    };
    match table
        .get("alab")
        .and_then(|alab| alab.get("journal")?.as_bool())
    {
        Some(false) => Setting::Off,
        _ => Setting::On,
    }
}

/// Whether this Mac publishes its journal (design §8, "Defaults"): `[alab] journal = false` in
/// the `aterm.toml` at `config` turns it off, and so does a file that is there but cannot be
/// read as TOML, so a typo elsewhere in it never cancels the owner's opt-out (`check` says
/// `off: aterm.toml unreadable`); no file, or one without the key, leaves it on. While it is
/// off the harness's hook notes nothing, what the spool and outbox already hold is dropped
/// unpublished (`flush::pass`, the file an off pass took included) and the backfill is marked
/// done without noting, so turning it on again publishes nothing from the time it was off; the
/// weekly `sum` row still says `journal: off`, so the lab can tell a Mac that stopped
/// publishing from one that is gone.
pub fn publishing(config: Option<&std::path::Path>) -> bool {
    setting(config) == Setting::On
}

/// [`publishing`] for a hook that asks on every note: the file is read and parsed again only
/// when its identity, length or times changed since the last ask (one `stat` otherwise), so a
/// note made while the journal is off is never appended, and one made once it is on again is.
#[derive(Debug)]
pub struct Publishing {
    config: Option<std::path::PathBuf>,
    /// The stamp of the file the last answer was read from (`None`: no file) and that answer.
    last: std::sync::Mutex<Option<(Option<ConfigStamp>, bool)>>,
}

/// What tells one `aterm.toml` from another without reading it: the file the path leads to
/// (through a link, as [`publishing`] reads it), its length and when its contents and inode
/// last changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConfigStamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl ConfigStamp {
    #[cfg(unix)]
    fn of(path: &std::path::Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        })
    }

    #[cfg(not(unix))]
    fn of(path: &std::path::Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let at = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or((0, 0), |d| {
                    (
                        i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                        i64::from(d.subsec_nanos()),
                    )
                })
        };
        Some(Self {
            dev: 0,
            ino: 0,
            len: meta.len(),
            mtime: at(meta.modified()),
            ctime: at(meta.created()),
        })
    }
}

impl Publishing {
    /// The gate for the `aterm.toml` at `config` (`None`: no config home, so on).
    #[must_use]
    pub fn new(config: Option<std::path::PathBuf>) -> Self {
        Self {
            config,
            last: std::sync::Mutex::new(None),
        }
    }

    /// [`publishing`] of the file as it is now.
    pub fn now(&self) -> bool {
        let Some(config) = &self.config else {
            return true;
        };
        let stamp = ConfigStamp::of(config);
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((held, on)) = *last
            && held == stamp
            && stamp.is_some()
        {
            return on;
        }
        let on = publishing(Some(config));
        *last = Some((stamp, on));
        on
    }
}

#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod tests;
