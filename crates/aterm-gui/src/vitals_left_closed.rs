// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closed-tab record (`docs/DESIGN-leftover-processes-2026-10-16.md` §3.2): the closed tabs
//! the leftover pass knows, kept in the state home, so an aterm that starts again in the same boot
//! (after an update, a quit or a relaunch) judges a closed tab's leftover as the aterm that hosted
//! the tab would have. A record keeps that aterm's window (its shell's unique id and its horizon),
//! so the judge gives the same tier, proof words and token, and nothing on the way widens it.

use std::path::Path;
use std::time::Duration;

use aterm_vitals::left::{AGENT_SPANS_KEPT, AgentSpan, ClosedTab};

use crate::vitals_left::CLOSED_KEPT;

/// The record's name in the state home, beside the vitals files.
pub(crate) const FILE: &str = "vitals-closed-tabs.json";
/// The lock beside [`FILE`] that every writer holds across its read, merge and replace, so two
/// aterms of one state home (an update's overlap) lose neither's closes.
pub(crate) const LOCK: &str = "vitals-closed-tabs.lock";
const SCHEMA: u32 = 1;
/// A file larger than this reads as none: [`CLOSED_KEPT`] records of the largest a writer makes
/// fit well under it.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// The longest label kept, bytes, cut at a character's boundary: every word made of a label
/// prints 40 characters of it at most.
const LABEL_BYTES: usize = 256;
/// How long a writer waits for another's lock before it leaves this write to the next one, which
/// writes the whole table again.
const LOCK_WAIT: Duration = Duration::from_millis(250);

/// The longest a closed tab is kept after its close, ms (30 days), in memory as on disk: an aterm
/// that started again and one that kept running hold the same closes.
pub(crate) const CLOSED_KEPT_MS: u64 = 30 * 24 * 3_600_000;

/// Whether a tab closed at `closed_ms` is kept at `now_ms` (both unix ms): closed at most
/// [`CLOSED_KEPT_MS`] before it. One dated after it (a clock set back since) is kept.
pub(crate) const fn within(closed_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(closed_ms) <= CLOSED_KEPT_MS
}

/// One closed tab as the pass keeps it: the view's record, and what the file keeps beside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kept {
    /// The view's record.
    pub(crate) tab: ClosedTab,
    /// Its shell's pid, which a record of a quit is held to ([`Self::at_quit`]).
    pub(crate) shell_pid: u32,
    /// Closed by its aterm's quit, not by a close of its own: an aterm that starts again takes it
    /// only once its shell reads gone, since while that shell runs (one that outlived the quit's
    /// hangup) the session it names is not gone.
    pub(crate) at_quit: bool,
    /// This process's host told what it left (a `proc.left` record,
    /// [`crate::message_reporters::newly_left`]) and said so to the pass
    /// ([`crate::vitals_left::LeftPass::keep_told`]): the record keeps it told.
    pub(crate) told_here: bool,
}

/// The file.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Doc {
    schema: u32,
    /// The boot its unique ids were read in ([`aterm_uds::process::boot_id`]): they start again
    /// at every boot, so a file of another boot reads as none.
    boot: String,
    /// In the order each close was first written, oldest first.
    tabs: Vec<Rec>,
}

/// One closed tab.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Rec {
    sid: String,
    label: String,
    number: Option<usize>,
    shell_pid: u32,
    shell_uniqueid: u64,
    horizon: u64,
    closed_ms: u64,
    /// `(since, until)` of each stretch an agent ran in it, oldest first.
    agent_spans: Vec<(u64, Option<u64>)>,
    at_quit: bool,
    /// A `proc.left` record told what it left, as its host said: the aterm that reads it tells
    /// none again.
    told: bool,
}

/// `label` cut to at most [`LABEL_BYTES`] at a character's boundary.
fn cut(label: &str) -> String {
    let mut end = label.len().min(LABEL_BYTES);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    label[..end].to_owned()
}

impl Rec {
    /// What the file keeps of `kept`: told once its own record says so, or once this process's
    /// host told it ([`Kept::told_here`]).
    fn of(kept: &Kept) -> Self {
        let t = &kept.tab;
        Self {
            sid: t.sid.clone(),
            label: cut(&t.label),
            number: t.number,
            shell_pid: kept.shell_pid,
            shell_uniqueid: t.shell_uniqueid,
            horizon: t.horizon,
            closed_ms: t.closed_ms,
            agent_spans: t.agent_spans.iter().map(|s| (s.since, s.until)).collect(),
            at_quit: kept.at_quit,
            told: t.told || kept.told_here,
        }
    }

    /// The pass's record of it: restored, with no key yet and nothing counted left.
    fn kept(self) -> Kept {
        Kept {
            tab: ClosedTab {
                key: 0,
                sid: self.sid,
                label: self.label,
                number: self.number,
                shell_uniqueid: self.shell_uniqueid,
                horizon: self.horizon,
                closed_ms: self.closed_ms,
                agent_spans: self
                    .agent_spans
                    .into_iter()
                    .map(|(since, until)| AgentSpan { since, until })
                    .collect(),
                left: 0,
                restored: true,
                told: self.told,
            },
            shell_pid: self.shell_pid,
            at_quit: self.at_quit,
            told_here: false,
        }
    }

    /// Whether it is one a writer makes: a minted session id, a label within [`LABEL_BYTES`], a
    /// shell, a window that does not close before it opens, and at most [`AGENT_SPANS_KEPT`]
    /// stretches as [`aterm_vitals::left::fold_agent`] keeps them: none ending before it starts,
    /// and each closed before the next starts, so only the last is open.
    fn valid(&self) -> bool {
        crate::spawn::is_valid_session_id(&self.sid)
            && self.label.len() <= LABEL_BYTES
            && self.shell_pid > 0
            && self.horizon >= self.shell_uniqueid
            && self.agent_spans.len() <= AGENT_SPANS_KEPT
            && self
                .agent_spans
                .iter()
                .all(|&(since, until)| until.is_none_or(|until| until >= since))
            && self
                .agent_spans
                .windows(2)
                .all(|pair| pair[0].1.is_some_and(|until| until < pair[1].0))
    }

    /// Whether it and `other` are one tab's close: one session, one shell.
    fn same(&self, other: &Self) -> bool {
        self.sid == other.sid && self.shell_uniqueid == other.shell_uniqueid
    }
}

/// The records `text` holds for `boot`, or `None` unless the whole file reads: JSON of this
/// schema and boot, at most [`CLOSED_KEPT`] records, each one a writer makes ([`Rec::valid`]). A
/// file that does not read whole proves nothing of any tab in it.
fn parse(text: &str, boot: &str) -> Option<Vec<Rec>> {
    let doc: Doc = aterm_json::from_str(text).ok()?;
    (doc.schema == SCHEMA
        && doc.boot == boot
        && doc.tabs.len() <= CLOSED_KEPT
        && doc.tabs.iter().all(Rec::valid))
    .then_some(doc.tabs)
}

/// `recs` within the bounds at `now_ms`: none closed more than [`CLOSED_KEPT_MS`] before it, and
/// of the rest the [`CLOSED_KEPT`] written last, in the order they were written. The pass drops
/// its oldest close first the same way ([`crate::vitals_left::LeftPass`]), by the order it saw
/// them close, never by the clock, which can be set back.
fn bounded(mut recs: Vec<Rec>, now_ms: u64) -> Vec<Rec> {
    recs.retain(|r| within(r.closed_ms, now_ms));
    let over = recs.len().saturating_sub(CLOSED_KEPT);
    recs.drain(..over);
    recs
}

/// `on_disk` with each of `ours` in it, one record per tab ([`Rec::same`]), a new one after the
/// rest. Where both hold one, ours stands, told where either says so: both are that tab's close as
/// one aterm recorded it, written again by that aterm or by one that restored it, since a shell
/// passes to another aterm only in a seamless update, whose predecessor writes nothing at its
/// Commit.
fn merge(mut on_disk: Vec<Rec>, ours: impl IntoIterator<Item = Rec>) -> Vec<Rec> {
    for rec in ours {
        match on_disk.iter_mut().find(|r| r.same(&rec)) {
            Some(held) => {
                let told = held.told || rec.told;
                *held = rec;
                held.told = told;
            }
            None => on_disk.push(rec),
        }
    }
    on_disk
}

/// The closed tabs the record at `path` keeps for `boot` at `now_ms` (unix ms), in the order they
/// were written and within the bounds ([`bounded`]), each restored ([`ClosedTab::restored`]) with
/// no key yet. None when the file is missing, larger than [`MAX_BYTES`], or does not read whole
/// ([`parse`]).
pub(crate) fn read(path: &Path, boot: &str, now_ms: u64) -> Vec<Kept> {
    crate::vitals_host::read_bounded(path, MAX_BYTES)
        .and_then(|text| parse(&text, boot))
        .map(|recs| bounded(recs, now_ms).into_iter().map(Rec::kept).collect())
        .unwrap_or_default()
}

/// Writes `table` into the record at `path` for `boot` at `now_ms` (unix ms), holding [`LOCK`]
/// beside it: the record as it reads now ([`parse`]; none when it does not read whole) merged
/// with each close of `table` a reader takes ([`Rec::valid`], [`merge`]), bounded ([`bounded`]),
/// and put in its place whole by a rename. A close no reader takes proves nothing of any process
/// (a window that closes before it opens, its horizon read where no process newer than its shell
/// outlived it; a tab whose session id was never read), and in the file it would spoil every
/// other.
///
/// # Errors
///
/// The lock could not be opened or was held past [`LOCK_WAIT`], or the write failed: the record is
/// as it was.
pub(crate) fn write<'a>(
    path: &Path,
    boot: &str,
    table: impl IntoIterator<Item = &'a Kept>,
    now_ms: u64,
) -> std::io::Result<()> {
    use crate::native_document_host::{AdvisoryLockRefusal, acquire_advisory_lock_within};
    let lock = crate::vitals_host::open_lock_file(&path.with_file_name(LOCK))?;
    let _held = acquire_advisory_lock_within(lock, LOCK_WAIT).map_err(|refusal| match refusal {
        AdvisoryLockRefusal::Busy => std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "another aterm held the closed-tab record's lock",
        ),
        AdvisoryLockRefusal::Failed(error) => error,
    })?;
    let on_disk = crate::vitals_host::read_bounded(path, MAX_BYTES)
        .and_then(|text| parse(&text, boot))
        .unwrap_or_default();
    let doc = Doc {
        schema: SCHEMA,
        boot: boot.to_owned(),
        tabs: bounded(
            merge(on_disk, table.into_iter().map(Rec::of).filter(Rec::valid)),
            now_ms,
        ),
    };
    let text = aterm_json::to_string(&doc).map_err(|e| std::io::Error::other(e.to_string()))?;
    aterm_agent::harness::atomic_write::write_atomic(path, &text)
}

#[cfg(test)]
#[path = "vitals_left_closed_tests.rs"]
mod tests;
