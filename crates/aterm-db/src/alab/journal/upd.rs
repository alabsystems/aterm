// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `upd` rows: each Mac's one update record (`aterm_update_core::update_record`), keyed by its
//! Commons machine uuid, so the lab sees which build every Mac runs, whether it proved itself,
//! the newest build its updater was offered, the trial under way and the one hold, with when it
//! lapses. The updater keeps the same record in its own folder (`Updates/record.json`), which its
//! launch-time revert and boot gate read before any Commons runs; this row is that record
//! published, never a second ledger.
//!
//! The `aterm` executable reports the record through [`install_record`]; the flusher reads it
//! each pass (a small file read, so another process's edit, the node's or a window's, is seen
//! within a pass) and writes the row inside the journal's fixed shares whenever it says
//! something the last row did not, as promptly as a sharp event, and once in each new week while
//! a hold or a trial stands. [`nudge`] starts this process's next pass at once. A row lapses at
//! its `until` (the end of the next week, or the hold's end if later) unless restated, so a Mac
//! that went away leaves no row after two weeks. [`read`] is the reader's half: every live row in
//! the window, by the Mac that wrote it.

use super::book::{VERSION, WEEK};
use crate::typed::owned::{OwnedRead, caps};
use aterm_json::Value;
use aterm_update_core::update_record::UpdateRecord;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// What reports this Mac's update record: `None` where it has none (no updater ran here yet),
/// which leaves the last row as it was.
pub type Records = dyn Fn() -> Option<UpdateRecord> + Send + Sync;

static RECORD: OnceLock<Box<Records>> = OnceLock::new();
static NUDGED: AtomicBool = AtomicBool::new(false);

/// Install this process's record report (the `aterm` executable's, which reads the updater's
/// own copy); the first install wins. With none installed the flusher writes no `upd` row.
pub fn install_record(report: Box<Records>) -> bool {
    RECORD.set(report).is_ok()
}

/// The installed report's record now; `None` with none installed.
pub(crate) fn record() -> Option<UpdateRecord> {
    (RECORD.get()?)()
}

/// Ask this process's flusher for a pass now rather than at its next minute (or its retry ten
/// minutes after a pass that failed): the updater calls it after a hold was set or ended or a
/// trial missed (`update_record::install_notify`). A pass still keeps to the pace line.
pub fn nudge() {
    NUDGED.store(true, Ordering::Release);
}

/// Whether a [`nudge`] came since the last ask, clearing it.
pub(crate) fn nudged() -> bool {
    NUDGED.swap(false, Ordering::AcqRel)
}

/// When a row stated in week `period` lapses unless restated: the end of the next week (as a
/// `place` row's), or the hold's end where that is later.
pub(crate) fn until(record: &UpdateRecord, period: u64) -> u64 {
    let carried = period.saturating_add(2).saturating_mul(WEEK);
    record
        .hold
        .as_ref()
        .map_or(carried, |hold| hold.until.max(carried))
}

/// The row `record` is published as, at counter `n` in week `period`, within [`caps::UPD_ROW`]:
/// over it, the trial's and then the hold's reason go first; `None` if even that row passes it.
/// What is cut is judged on the row with the widest counter, week and `until` there are, so it
/// is the same at every `n` and `period`: the book compares rows spelled at different counters
/// ([`content`]), and a cut that depended on their digits would make a row that never matches.
pub(crate) fn row(record: &UpdateRecord, n: u64, period: u64) -> Option<Value> {
    let mut record = record.clone();
    let fits = |value: &Value| aterm_json::to_vec(value).is_ok_and(|b| b.len() <= caps::UPD_ROW);
    loop {
        if fits(&spell(&record, u64::MAX, u64::MAX)?) {
            return spell(&record, n, period);
        }
        if let Some(trial) = record.trial.as_mut().filter(|trial| !trial.why.is_empty()) {
            trial.why.clear();
        } else {
            record
                .hold
                .as_mut()
                .filter(|hold| !hold.why.is_empty())?
                .why
                .clear();
        }
    }
}

fn spell(record: &UpdateRecord, n: u64, period: u64) -> Option<Value> {
    let mut value = aterm_json::to_value(record).ok()?;
    let object = value.as_object_mut()?;
    // The row's own counter and schema word stand for the file's: readers rank a writer's rows by
    // (week, counter), and the record's version is the codec's, restored on read.
    object.remove("v");
    object.insert("r".into(), Value::from(VERSION));
    object.insert("n".into(), Value::from(n));
    object.insert("period".into(), Value::from(period));
    object.insert("until".into(), Value::from(until(record, period)));
    Some(value)
}

/// What a row says, for telling whether a new one says anything the last did not: the row
/// without its counter, its week and its `until`, which move with every restatement.
pub(crate) fn content(row: &Value) -> Value {
    let mut row = row.as_object().cloned().unwrap_or_default();
    for key in ["n", "period", "until"] {
        row.remove(key);
    }
    Value::Object(row)
}

/// The record a row spells, through the record's own codec (so a hold's laws hold whatever a
/// writer put in it), and the row's `until`; `None` for a row that is no record. The record's
/// `n` is the row's counter.
pub(crate) fn of_row(value: &Value) -> Option<(UpdateRecord, u64)> {
    let mut object = value.as_object()?.clone();
    let until = object.remove("until")?.as_u64()?;
    object.remove("r");
    object.remove("period");
    object.insert(
        "v".into(),
        Value::from(aterm_update_core::update_record::VERSION),
    );
    let text = aterm_json::to_string(&Value::Object(object)).ok()?;
    let record = UpdateRecord::decode(&text).ok()?;
    Some((record, until))
}

/// One live `upd` row of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upd {
    /// The Commons machine uuid that wrote it (32 hex), which `machine-facts` binds to a machine.
    pub uuid: String,
    /// What that Mac says of its own updates.
    pub record: UpdateRecord,
    /// When the row lapses unless restated.
    pub until: u64,
    /// Whether it is this Mac's own row: no evidence about this Mac's holds, since it may lag
    /// the record it was published from.
    pub own: bool,
}

/// The live `upd` rows among `rows` (the window's owned read) at `now`, by writer, `me` (this
/// Mac's uuid, 32 hex) marked [`Upd::own`]: a row that is no record, or whose `until` has
/// passed, is no row.
pub(crate) fn live(rows: &OwnedRead, now: u64, me: Option<&str>) -> Vec<Upd> {
    let mut live: Vec<Upd> = rows
        .rows
        .iter()
        .filter_map(|(uuid, row)| {
            let (record, until) = of_row(&row.value)?;
            (until > now).then(|| Upd {
                uuid: uuid.clone(),
                record,
                until,
                own: me == Some(uuid.as_str()),
            })
        })
        .collect();
    live.sort_unstable_by(|a, b| a.uuid.cmp(&b.uuid));
    live
}

/// The journal window's live `upd` rows as this Mac holds them at `now` (Unix seconds), read as a
/// member, this Mac's own included and marked [`Upd::own`]; `None` where this Mac holds no
/// Commons it is a member of.
///
/// # Errors
/// A window table cannot be observed or read.
pub fn read(paths: &aterm_secrets::client::Paths, now: u64) -> crate::Result<Option<Vec<Upd>>> {
    use crate::{catalog::Directory, table::Session, typed::owned};
    use aterm_secrets::commons::bootstrap::Bootstrap;
    let selected = crate::generation::selected(paths)?;
    let deadline = crate::admission::deadline_at(std::time::Instant::now());
    let Ok((bootstrap, journal)) = Bootstrap::required_with_journal_until(&selected, deadline)
    else {
        return Ok(None);
    };
    let Ok(session) = Session::from_admitted_journal(&bootstrap, journal) else {
        return Ok(None);
    };
    let me = session.members().ok().and_then(|members| {
        members
            .iter()
            .find(|binding| binding.actor() == session.actor())
            .map(|binding| aterm_codec::hex::encode_lower(binding.machine()))
    });
    let directory = Directory::new(&selected, bootstrap.trust());
    let window = super::flush::window(&session, &directory, now)?;
    let resolve = owned::commons::historical(&session);
    let rows = owned::read_owned(&super::UPD, &window.ops, &resolve);
    Ok(Some(live(&rows, now, me.as_deref())))
}
