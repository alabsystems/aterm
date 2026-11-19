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
//! - [`upd`]: this Mac's one update record (`aterm_update_core::update_record`), written as its
//!   `upd` row; and the reader of every Mac's live rows.
//! - [`mine`]: this Mac's own `login` rows (the harness's [`mine::Login`]s) and `usage` rows (the
//!   book's readings), the value-free guard every one passes, and what in them is urgent.
//!
//! The ring's collections are compiled ATT/1 schemas ([`EV`], [`USE`], [`SUM`], [`RELAY`],
//! [`PLACE`], [`UPD`], [`LOGIN`], [`USAGE`]); every row lives at its writer's own Commons machine
//! uuid and is inert at any other key (`typed::owned`). `LOGIN` and `USAGE` are the rows of the
//! shared files `ai/logins` and `ai/usage`, and `EV` the entries of `ai/events`
//! (`crate::file::types`); an older build reads none of the two new collections and still reads
//! every other row of a record that carries them. The flusher writes the `login` and `usage`
//! rows beside the `use` and `place` rows, which it keeps writing.

pub mod event;
pub mod mine;
pub mod place;
pub mod seen;
pub mod spool;
pub mod upd;

pub(crate) mod book;
pub(crate) mod flush;
mod heard;
mod relay;

pub use book::{AccountFacts, BookFacts, PlaceFacts, Presence};
pub use event::{Event, Kind, ModelBucket, Pk, Res, Source, SubKey, Via, Window, Word};
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
        authority: None,
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
        authority: None,
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
        authority: None,
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
        authority: None,
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
        authority: None,
    }
);

crate::att_collection!(
    /// This Mac's update record (`aterm_update_core::update_record::UpdateRecord`, [`upd`]):
    /// `<uuid32>`, at most 2 KiB: the build it runs and whether it proved itself, the newest
    /// build offered, the trial under way, the one hold and how the last one ended. `until` is
    /// when it lapses unless restated.
    pub UPD = Collection {
        name: "upd",
        version: 1,
        kind: Merge::Owned,
        writers: Writers::SelfKey,
        prefix: "",
        derive: Derive::SignerUuid,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("period", Ty::Uint).writer(Writer::Mine),
            Column::new("os", Ty::Enum(&["macos", "linux", "windows", "other"]))
                .writer(Writer::Mine),
            Column::new("arch", Ty::Enum(&["aarch64", "x86_64", "other"])).writer(Writer::Mine),
            Column::new("running", Ty::Uint).writer(Writer::Mine),
            Column::new("running_sha", Ty::Hex(64))
                .null()
                .writer(Writer::Mine),
            Column::new("proven", Ty::Bool).writer(Writer::Mine),
            Column::new("offered", Ty::Uint).null().writer(Writer::Mine),
            Column::new("trial", Ty::Json(512)).null().writer(Writer::Mine),
            Column::new("hold", Ty::Json(768)).null().writer(Writer::Mine),
            Column::new("last", Ty::Json(128)).null().writer(Writer::Mine),
            Column::new("until", Ty::Uint).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: caps::UPD_ROW,
        authority: None,
    }
);

/// The CLI words a `login` row names (`Event::cli`'s words, [`mine::Cli`]).
pub const CLIS: &[&str] = &mine::Cli::WORDS;

/// The reading-source words a `usage` row names: [`Source`]'s words a reading carries.
pub const READING_SOURCES: &[&str] = &[
    "statusline",
    "rollout",
    "session-usage",
    "cli-output",
    "signin-flow",
    "error",
    "relay",
    "probe",
];

crate::att_collection!(
    /// One login this Mac holds for one account (docs/DESIGN-shared-files-2026-11-11.md §10
    /// file 3, the shared file `ai/logins`): `<account pk>.<uuid32>`, at most 768 B. Every value
    /// is a closed word, a count or a time, so it can carry no address, path, URL or secret. The
    /// times are Unix milliseconds of the writing Mac's clock, an author's claim. Since version 2
    /// it states the credential its sign-in runs on (SF-34, decision SF-D3, [`mine::Credential`]):
    /// `cred_kind`, `cred_kept`, `cred_issued_at`, `cred_expires_at`, `cred_refreshed_at`,
    /// `cred_last_used` and `cred_state`, facts and never a value, each time bounded to a span a
    /// real credential's falls in (`mine::checked`). An account's key names its
    /// provider's realm, so one Mac's two pooled CLIs never sign in to one account (codex-alab
    /// runs on Codex's own login), so each row states one credential.
    pub LOGIN = Collection {
        name: "login",
        version: 2,
        kind: Merge::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("account", Ty::Ref("account")).writer(Writer::Mine),
            Column::new("cli", Ty::Enum(CLIS)).writer(Writer::Mine),
            Column::new("place", Ty::Enum(&mine::Place::WORDS)).writer(Writer::Mine),
            Column::new("state", Ty::Enum(&mine::State::WORDS)).writer(Writer::Mine),
            Column::new("proof", Ty::Enum(&mine::Proof::WORDS)).writer(Writer::Mine),
            Column::new("src", Ty::Enum(&mine::Src::WORDS)).writer(Writer::Mine),
            Column::new("use", Ty::Enum(&mine::Use::WORDS)).writer(Writer::Mine),
            Column::new("tabs", Ty::Uint).writer(Writer::Mine),
            Column::new("held", Ty::Enum(&mine::Held::WORDS)).writer(Writer::Mine),
            Column::new("since", Ty::Time).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
            Column::new("served_at", Ty::Time).null().writer(Writer::Mine),
            Column::new("ends_by", Ty::Time).null().writer(Writer::Mine),
            Column::new("ended_at", Ty::Time).null().writer(Writer::Mine),
            Column::new("attempt", Ty::Enum(&mine::Attempt::WORDS)).writer(Writer::Mine),
            Column::new("attempt_at", Ty::Time).null().writer(Writer::Mine),
            Column::new("attempt_route", Ty::Enum(&mine::Route::WORDS))
                .null()
                .writer(Writer::Mine),
            Column::new("plan", Ty::Text(16)).null().writer(Writer::Mine),
            Column::new("cred_kind", Ty::Enum(&mine::CredKind::WORDS))
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new("cred_kept", Ty::Enum(&mine::Kept::WORDS))
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new("cred_issued_at", Ty::Time)
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new("cred_expires_at", Ty::Time)
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new("cred_refreshed_at", Ty::Time)
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new("cred_last_used", Ty::Time)
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new("cred_state", Ty::Enum(&mine::CredState::WORDS))
                .null()
                .writer(Writer::Mine)
                .since(2),
        ],
        unique: &[],
        max_row: caps::LOGIN_ROW,
        authority: None,
    }
);

/// The most bytes a `usage` row's `model_walls` holds (FL-269): a list of each model's own
/// standing weekly wall, `{"bucket": <a ModelBucket word>, "until": <Unix ms>}` with `until`
/// only where the wall states an end. This build writes at most one entry of each of
/// [`ModelBucket`]'s four words (about 250 B); the room past them is a newer build's, and the
/// cell is JSON within this size, never a typed list, so a newer build's bucket, member or
/// entry is read entry by entry (`file::limits::model_walls_of`) and costs no other entry its
/// wall.
pub const MODEL_WALLS_MAX: usize = 384;

crate::att_collection!(
    /// This Mac's latest reading of one account's limit windows (docs/DESIGN-shared-files-2026-11-11.md
    /// §10 file 4, the shared file `ai/usage`): `<account or sub-account pk>.<uuid32>`, at most
    /// 1,024 B. A percent is tenths of a percent used. A reset is the provider's stated instant,
    /// Unix milliseconds: the one time two Macs' readings are compared by (`ai/limits`), since it
    /// is the provider's, never a Mac's clock. `read_at` and `at` are the writing Mac's clock, an
    /// author's claim that orders nothing. `wall` is the account's own standing wall; since
    /// version 2 each model's own standing weekly wall is in `model_walls`, by its bucket
    /// ([`MODEL_WALLS_MAX`], FL-269), and no longer in `wall`, where a version-1 row's
    /// `7d-model` names no model. Since version 3 each window's reset names how exactly it is
    /// known (`w5h_res`, `w7d_res`, `w7dm_res`: the reading's [`Res`]), which `ai/limits` judges
    /// two Macs' resets of one instance by (`file::limits::SAME_RESET_MS`).
    pub USAGE = Collection {
        name: "usage",
        version: 3,
        kind: Merge::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("account", Ty::Ref("account")).writer(Writer::Mine),
            Column::new("sub", Ty::Hex(12)).null().writer(Writer::Mine),
            Column::new("w5h", Ty::Uint).null().writer(Writer::Mine),
            Column::new("w7d", Ty::Uint).null().writer(Writer::Mine),
            Column::new("w7dm", Ty::Uint).null().writer(Writer::Mine),
            Column::new("w5h_reset", Ty::Time).null().writer(Writer::Mine),
            Column::new("w7d_reset", Ty::Time).null().writer(Writer::Mine),
            Column::new("w7dm_reset", Ty::Time).null().writer(Writer::Mine),
            Column::new("w5h_res", Ty::Enum(Res::WORDS))
                .null()
                .writer(Writer::Mine)
                .since(3),
            Column::new("w7d_res", Ty::Enum(Res::WORDS))
                .null()
                .writer(Writer::Mine)
                .since(3),
            Column::new("w7dm_res", Ty::Enum(Res::WORDS))
                .null()
                .writer(Writer::Mine)
                .since(3),
            Column::new("wall", Ty::Enum(&mine::Wall::WORDS)).writer(Writer::Mine),
            Column::new("wall_until", Ty::Time).null().writer(Writer::Mine),
            Column::new("model_walls", Ty::Json(MODEL_WALLS_MAX))
                .null()
                .writer(Writer::Mine)
                .since(2),
            Column::new(
                "credits",
                Ty::Int {
                    min: i64::MIN,
                    max: i64::MAX,
                },
            )
            .null()
            .writer(Writer::Mine),
            Column::new("plan", Ty::Text(16)).null().writer(Writer::Mine),
            Column::new("src", Ty::Enum(READING_SOURCES)).writer(Writer::Mine),
            Column::new("read_at", Ty::Time).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
            Column::new("sessions_7d", Ty::Uint).writer(Writer::Mine),
            Column::new("last_start", Ty::Time).null().writer(Writer::Mine),
            Column::new("last_end", Ty::Time).null().writer(Writer::Mine),
        ],
        unique: &[],
        max_row: caps::USAGE_ROW,
        authority: None,
    }
);

/// The journal ring's collections, as one typed table. Its records are automatic by nature;
/// the ring's fixed shares bound them, not a quota.
pub const JOURNAL: Table = Table {
    application: APPLICATION,
    role: ROLE,
    collections: &[&EV, &USE, &SUM, &RELAY, &PLACE, &UPD, &LOGIN, &USAGE],
    self_quota: usize::MAX,
    ends: &[],
    faces: &[],
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

/// What this Mac's book knows of the accounts it used and the places it holds, as of `now`
/// (Unix seconds): the per-account facts the metrics publisher puts in each quota row, and the
/// places it publishes (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14). It reads
/// `<spool>/book.json`, which the flusher saves, whichever process runs it; a book that is
/// missing or does not read is an empty one. It reads no table and no network.
#[must_use]
pub fn book_facts(spool: &Spool, now: u64) -> BookFacts {
    book::Book::load(spool).facts(now)
}

/// The sign-in word two folds hold of one account after what `spool` took in
/// ([`fold_signin_for_test`]).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldedSignIn {
    /// This Mac's book: what its `use` row and its metrics quota row state.
    pub book: Option<&'static str>,
    /// A peer that holds this Mac's events and not yet the `use` row that follows them.
    pub peer_events: Option<&'static str>,
    /// A peer that holds the events and the `use` row.
    pub peer: Option<&'static str>,
}

/// Test seam for the harness's side (`crates/aterm`, which links both): run the local half of two
/// flusher passes over `spool` at `now` (the first takes the spool, the second absorbs it) (the spool's lines merged into the outbox and folded into
/// the book, as [`run`] does before it reads a table) and say which sign-in word of `account`
/// each fold ends on: this Mac's book, and a peer's reading of what this Mac would publish (the
/// outbox as an `ev` batch, with and without the book's `use` row), through the folds
/// themselves ([`book::Book::fold`], [`seen::Seen`]).
///
/// # Errors
/// The spool, outbox or book cannot be read or written, or a `use` row cannot be built.
#[cfg(any(test, debug_assertions))]
pub fn fold_signin_for_test(spool: &Spool, account: &Pk, now: u64) -> crate::Result<FoldedSignIn> {
    const PEER: &str = "00000000000000000000000000000001";
    // A pass absorbs the file the pass before it took: the second takes in what was spooled.
    flush::take_in(spool, false, now)?;
    let (book, outbox) = flush::take_in(spool, false, now)?;
    let mut uses = Vec::new();
    for (pk, facts) in &book.accounts {
        if pk.as_str().starts_with("ac-") {
            let row = facts.use_row(1, now / book::WEEK, now, caps::USE_ROW)?;
            uses.push((*pk, row));
        }
    }
    let read = |uses: &[(Pk, aterm_json::Value)]| {
        seen::Seen::of_published(PEER, &outbox, uses, now)
            .signins
            .get(account.as_str())
            .and_then(|by_mac| by_mac.get(PEER))
            .map(|signed| signed.state)
    };
    Ok(FoldedSignIn {
        book: book
            .facts(now)
            .accounts
            .get(account)
            .and_then(|facts| facts.signin)
            .map(|(state, _)| state),
        peer_events: read(&[]),
        peer: read(&uses),
    })
}

/// What the fleet's shared files say of the limits each Mac's spool took in
/// ([`fold_limits_for_test`]).
#[cfg(all(unix, any(test, debug_assertions)))]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FoldedLimits {
    /// Each `ai/usage` row by its key (`<pk>.<uuid32>`): each Mac's as its flusher states it,
    /// and each older build's as given.
    pub usage: std::collections::BTreeMap<String, aterm_json::Value>,
    /// Each `ai/limits` row the fold makes of them, by its key, as its values.
    pub limits: std::collections::BTreeMap<String, aterm_json::Value>,
    /// What rotation reads of those `ai/limits` rows, an older build's row in place of its key's
    /// ([`fold_limits_for_test`]'s `older_limits`).
    pub rotation: crate::file::Rotation,
}

/// Test seam for the harness's side (`aterm-agent`'s accounts, whose rotation reads `ai/limits`
/// through `aterm_db::file::Rotation`): run the local half of two flusher passes over each Mac's
/// spool (`macs`, each by its Commons uuid, 32 hex) at `now`, as [`fold_signin_for_test`] does;
/// state each account's `usage` row as that Mac's book does ([`book::Book::usages_due`]), held
/// to the guard the flusher writes through ([`mine::checked`]); project each, and each row of
/// `older` (rows an older build wrote, by key), as a pass projects the ring's rows
/// (`file::materialize`); fold them into `ai/limits` as a pass does
/// ([`crate::file::limits::fold`]), each computed cell held to the guards a pass writes them
/// through (`file::materialize::guard_computed`); and read those rows as rotation does
/// ([`crate::file::Rotation`]'s walls), each row of `older_limits` (an `ai/limits` row an older
/// build's materializer wrote, by key, as its values) in place of this build's row of that key,
/// as rotation reads the file whichever build last wrote it.
///
/// # Errors
/// A spool, outbox or book cannot be read or written, or a Mac's `usage` row breaks the guard.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn fold_limits_for_test(
    macs: &[(&str, &Spool)],
    older: &[(&str, aterm_json::Value)],
    older_limits: &[(&str, aterm_json::Value)],
    now: u64,
) -> crate::Result<FoldedLimits> {
    use crate::file::limits;
    let mut folded = FoldedLimits::default();
    for (uuid, spool) in macs {
        flush::take_in(spool, false, now)?;
        let (book, _) = flush::take_in(spool, false, now)?;
        for (pk, row) in book.usages_due(1, now / book::WEEK, now) {
            mine::checked(&USAGE, &row)?;
            folded.usage.insert(format!("{}.{uuid}", pk.as_str()), row);
        }
    }
    for (key, row) in older {
        folded.usage.insert((*key).to_owned(), row.clone());
    }
    let readings: Vec<limits::Reading> = folded
        .usage
        .iter()
        .filter_map(|(key, row)| crate::file::materialize::ring_row(&USAGE, key, row))
        .filter_map(|row| limits::Reading::of_row(&row))
        .collect();
    let rows: Vec<aterm_file::Row> = limits::fold(&readings, &limits::Seats::default())
        .iter()
        .map(|(pk, limit)| {
            let mut row = limit.row(pk, None, None);
            crate::file::materialize::guard_computed(&mut row);
            row
        })
        .collect();
    let read: Vec<aterm_file::Row> = rows
        .iter()
        .filter(|row| older_limits.iter().all(|(pk, _)| *pk != row.pk))
        .cloned()
        .chain(older_limits.iter().map(|(pk, values)| aterm_file::Row {
            pk: (*pk).to_owned(),
            live: true,
            values: values.as_object().cloned().unwrap_or_default(),
            ..aterm_file::Row::default()
        }))
        .collect();
    folded.rotation.limits(&read);
    folded.limits = rows
        .into_iter()
        .map(|row| (row.pk, aterm_json::Value::Object(row.values)))
        .collect();
    Ok(folded)
}

/// What this Mac would publish of one account's plan after what `spool` took in
/// ([`fold_plan_for_test`]): each as `(word, when)`.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldedPlan {
    /// This Mac's book: the plan its metrics row states.
    pub book: Option<(String, u64)>,
    /// The `plan` (`{word, t}`) of the account's `use` row, as a peer reads it.
    pub use_row: Option<(String, u64)>,
    /// The account's `plan-seen` events in the `ev` batch this Mac would append, oldest first.
    pub events: Vec<(String, u64)>,
    /// What a peer that holds the `ev` batch and the `use` row reads as the plan this Mac saw
    /// ([`seen::Seen::plan`]): the plan the fleet's accounts relation states as `plan_observed`
    /// (`<provider>-<word>`) and `plan_at`.
    pub peer: Option<(String, u64)>,
}

/// Test seam for the harness's side (`crates/aterm`): run the local half of two flusher passes
/// over `spool` at `now`, as [`fold_signin_for_test`] does, and say what each record this Mac
/// publishes states of `account`'s plan: its book, its `use` row and its `ev` batch, which are
/// what a reader of the fleet's accounts takes the observed plan from.
///
/// # Errors
/// The spool, outbox or book cannot be read or written, or a `use` row cannot be built.
#[cfg(any(test, debug_assertions))]
pub fn fold_plan_for_test(spool: &Spool, account: &Pk, now: u64) -> crate::Result<FoldedPlan> {
    const PEER: &str = "00000000000000000000000000000001";
    flush::take_in(spool, false, now)?;
    let (book, outbox) = flush::take_in(spool, false, now)?;
    let row = match book.accounts.get(account) {
        Some(facts) => Some(facts.use_row(1, now / book::WEEK, now, caps::USE_ROW)?),
        None => None,
    };
    let use_row = row.as_ref().and_then(|row| {
        let plan = row.get("plan")?;
        let word = plan.get("word")?.as_str()?.to_owned();
        Some((word, plan.get("t")?.as_u64()?))
    });
    let uses: Vec<_> = row.into_iter().map(|row| (*account, row)).collect();
    let peer = seen::Seen::of_published(PEER, &outbox, &uses, now)
        .plan(account.as_str())
        .map(|plan| (plan.word.clone(), plan.t));
    let events = outbox
        .iter()
        .filter(|event| event.kind == Kind::PlanSeen && event.account.as_ref() == Some(account))
        .filter_map(|event| Some((event.plan?.as_str().to_owned(), event.t)))
        .collect();
    Ok(FoldedPlan {
        book: book
            .facts(now)
            .accounts
            .get(account)
            .and_then(|facts| facts.plan.clone()),
        use_row,
        events,
        peer,
    })
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
