// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write journal (design §6.8): one append-only `astream_wire::steps` file per
//! repository, `<state>/agh/journal/<owner>/<repo>.steps`, both the audit ledger and the
//! intent store. Unix only (plan §1, rule 12).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Dirs;
use crate::error::{Error, Result};
use crate::fx::Fx;
use crate::policy::Permit;
use crate::slug::Slug;
use crate::write::WriteOp;

/// The largest journal agh reads or appends to: 64 MiB, which is about 16,000 releases.
/// Past it `append` refuses with Refused `journal_full`, and a person rotates the file once
/// no intent in it is open.
pub const JOURNAL_MAX: u64 = 64 << 20;
/// The tail of `JOURNAL_MAX` that only the records closing an intent may use: 1 MiB, room
/// for more than 250 records of agh's 4 KiB bound. `begin` refuses an Intent record that
/// would take the file past `JOURNAL_MAX - JOURNAL_RESERVE`, so a full journal can still
/// settle, observe and retract every intent it holds open before a person moves it aside.
/// Moving a journal aside with an intent open would drop the open-intent rule for its key.
pub const JOURNAL_RESERVE: u64 = 1 << 20;
/// How long `append` waits for the lock: 10 s (design §6.8).
pub const LOCK_WAIT: Duration = Duration::from_secs(10);
/// How long an intent may stay open before it becomes a need (`needs::raise`): 6 h. That is
/// longer than the longest lane between a create and its discovery (a 20-asset upload at
/// 12 KB/s), and short enough to be seen the same day.
pub const SETTLE_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// An intent's id: `<step>-<first 12 lowercase hex of sha256(slug ‖ 0x00 ‖ op ‖ 0x00 ‖ key)>`,
/// where `<step>` is the Intent record's own step and the slug is lowercased. Ids order by
/// step, then by text: `10-…` sorts after `9-…`, so a `max()`, a sort or a `BTreeMap` of ids
/// follows the journal's order. The step is a function of the text, so the order agrees with
/// the derived `Eq` and `Hash`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IntentId(String);

impl Ord for IntentId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.step(), &self.0).cmp(&(other.step(), &other.0))
    }
}

impl PartialOrd for IntentId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl IntentId {
    /// An id `<digits>-<12 lowercase hex>`, with no leading zero but in `0`.
    #[must_use]
    pub fn parse(text: &str) -> Option<IntentId> {
        let (step, hash) = text.split_once('-')?;
        let digits = !step.is_empty() && step.bytes().all(|b| b.is_ascii_digit());
        let no_leading_zero = step == "0" || !step.starts_with('0');
        let hex = hash.len() == 12
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        (digits && no_leading_zero && hex && step.parse::<u64>().is_ok())
            .then(|| IntentId(text.to_string()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The step.
    #[must_use]
    pub fn step(&self) -> u64 {
        self.0
            .split_once('-')
            .and_then(|(step, _)| step.parse().ok())
            .unwrap_or(0)
    }
}

impl std::fmt::Display for IntentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a write asks `begin` to record. `begin` adds the id and the time.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct IntentDraft {
    /// What the write does.
    pub op: WriteOp,
    /// Its repository.
    pub slug: Slug,
    /// The permit's profile.
    pub identity: String,
    /// The credential's fingerprint (`identity::fingerprint`).
    pub fingerprint: String,
    /// The caller's idempotency key: the tag, the asset name, the ref, or `api POST {path}`.
    pub key: String,
    /// `sha256:<hex> <size>` of the canonical request bytes (`fx::canon::request_digest`).
    pub request: String,
    /// The draft's `target_commitish`, when it sent one.
    pub target_commitish: Option<String>,
    /// `sha256:<hex>` of the draft's body text, for a release create.
    pub body_digest: Option<String>,
    /// The `[[writers]]` caller, when `Router::grant` made the target (`Permit::grant`).
    pub grant: Option<String>,
    /// The newest intent of this op and key that the caller judged before it chose to begin
    /// (`IntentStore::latest`), or `None` when it saw none. `begin` judges again under the
    /// lock and refuses when the journal has moved past it. It is not recorded: the Intent
    /// record's body is unchanged.
    pub after: Option<IntentId>,
}

impl IntentDraft {
    /// A draft for `op` on `slug` under `permit`, with no target, body digest, request or
    /// `after` yet. `Github::intent_draft` calls it and fills `request`; the resource adds the
    /// target and the body digest, and `after` comes from whoever judged the journal
    /// (`create_step`'s `CreateStep::Begin`, or `Github::one_shot`) before `begin`.
    #[must_use]
    pub fn new(
        op: WriteOp,
        slug: &Slug,
        permit: &Permit,
        fingerprint: &str,
        key: &str,
    ) -> IntentDraft {
        IntentDraft {
            op,
            slug: slug.clone(),
            identity: permit.profile().to_string(),
            fingerprint: fingerprint.to_string(),
            key: key.to_string(),
            request: String::new(),
            target_commitish: None,
            body_digest: None,
            grant: permit.grant().map(str::to_string),
            after: None,
        }
    }

    /// The same draft with `target_commitish`.
    #[must_use]
    pub fn with_target(mut self, target: Option<&str>) -> IntentDraft {
        self.target_commitish = target.map(str::to_string);
        self
    }

    /// The same draft with the body's digest.
    #[must_use]
    pub fn with_body_digest(mut self, digest: Option<String>) -> IntentDraft {
        self.body_digest = digest;
        self
    }

    /// The same draft with the request's digest.
    #[must_use]
    pub fn with_request(mut self, request: String) -> IntentDraft {
        self.request = request;
        self
    }

    /// The same draft with `after`.
    #[must_use]
    pub fn with_after(mut self, after: Option<IntentId>) -> IntentDraft {
        self.after = after;
        self
    }
}

/// A recorded intent (design §6.8): the draft, its id and its time.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Intent {
    /// The id.
    pub id: IntentId,
    /// What the write does.
    pub op: WriteOp,
    /// Its repository.
    pub slug: Slug,
    /// The profile's name.
    pub identity: String,
    /// The credential's fingerprint.
    pub fingerprint: String,
    /// The idempotency key.
    pub key: String,
    /// `sha256:<hex> <size>` of the canonical request.
    pub request: String,
    /// The draft's `target_commitish`.
    pub target_commitish: Option<String>,
    /// The draft body's digest.
    pub body_digest: Option<String>,
    /// The `[[writers]]` caller.
    pub grant: Option<String>,
    /// `Fx::wall_secs` at `begin`.
    pub at_secs: u64,
}

/// How an attempt ended (design §10.2's table).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Settled {
    /// Nothing was sent.
    NeverSent,
    /// GitHub answered this 4xx: closed, nothing created.
    Refused {
        /// The status.
        status: u16,
    },
    /// GitHub created it.
    Created {
        /// The object's id.
        id: u64,
        /// Its `html_url`, or its `url` where it has none (a ref).
        url: String,
    },
    /// No answer, or a 5xx: the intent stays open.
    Unknown {
        /// Why, redacted.
        why: String,
    },
}

/// One journal record (design §6.8, and `Retracted`, which the plan adds).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JournalRecord {
    /// Made durable before the first byte of the write is sent. Boxed: an `Intent` is about
    /// 256 bytes against about 56 for the next variant, right at clippy's
    /// `large_enum_variant` line (200), so one more field would fail `-D warnings`.
    Intent(Box<Intent>),
    /// How the attempt ended: one per Intent, after the answer or its absence.
    Settled {
        /// The intent.
        intent: IntentId,
        /// How it ended.
        outcome: Settled,
    },
    /// What a WriterRead discovery saw for an intent left Unknown.
    Observed {
        /// The intent.
        intent: IntentId,
        /// The object found, or `None`, which leaves the intent open.
        found: Option<u64>,
        /// `Fx::wall_secs`.
        at_secs: u64,
    },
    /// `agh intent retract`: a person closed the intent after looking.
    Retracted {
        /// The intent.
        intent: IntentId,
        /// `Fx::wall_secs`.
        at_secs: u64,
    },
}

/// How the file ended when it was scanned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalEnd {
    /// Every frame is whole.
    Clean,
    /// The frame at byte `at` was torn and the file was cut back to it. No caller was told
    /// that frame was written.
    TornRepaired {
        /// The byte offset of the torn frame.
        at: u64,
    },
    /// A damaged frame in the middle. Nothing is appended, and a person looks.
    Fault {
        /// The byte offset of the fault.
        at: u64,
        /// What `astream_wire::steps` says is wrong.
        why: String,
    },
}

/// The intent store (design §10.2): `Journal` is agh's. aterm-release's `JournalIntents` is
/// the cutter's in M3.
pub trait IntentStore: Send + Sync {
    /// Record an intent durably, before any byte of its write is sent, and answer its id.
    ///
    /// Under the same lock as the append, `begin` first judges again what its caller judged.
    /// So two agh processes on one journal (a lane, and a person's `agh release create`) never
    /// both begin one write. It refuses, and appends nothing, when either holds:
    /// - an intent of the draft's op and key is open. A `WriteOp::RefCreate` is the one
    ///   exception: GitHub itself refuses a second POST of a ref that exists, with 422
    ///   `Reference already exists`, and `Refs::create` resolves that;
    /// - the newest intent of the draft's op and key is not `draft.after`, so another run
    ///   began, or closed, one since the caller looked.
    ///
    /// Before either, it refuses a draft whose `key` or `target_commitish` `Redactor::shapes`
    /// would change. The journal passes every string through the Redactor, and `latest`,
    /// `open` and the agreeing rule match these two as written, so a redacted one would never
    /// match on a rerun, and the open-intent rule would not hold for its key. They are
    /// refused, never journaled redacted.
    ///
    /// # Errors
    ///
    /// The journal's refusals. The write is then refused, and nothing is sent. The re-judging's
    /// refusal is OutcomeUnknown, `Sub::IntentOpen { intent }` (exit 13), naming the intent it
    /// found, why `intent {id} for {key} began first: agh never posts twice`, next
    /// `agh intent show {repo} {id}`. A credential-shaped `key` or `target_commitish` is
    /// Usage (exit 2), why `the {op}'s {key|target} is shaped like a credential; agh journals
    /// it as written or not at all`, and nothing is appended.
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId>;
    /// Record how the attempt ended.
    ///
    /// # Errors
    ///
    /// The journal's refusals.
    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()>;
    /// Record what a discovery found.
    ///
    /// # Errors
    ///
    /// The journal's refusals.
    fn observe(&self, id: &IntentId, found: Option<u64>) -> Result<()>;
    /// Every intent that is neither settled (Created, NeverSent, Refused), nor observed found,
    /// nor retracted.
    ///
    /// # Errors
    ///
    /// The journal's refusals.
    fn open(&self) -> Result<Vec<Intent>>;
    /// The latest intent of `op` whose key is `key`, open or closed, with what the journal
    /// recorded of it since. `Releases::create` reads it, so a draft a lagging listing hides
    /// is found again by the id its intent settled with (the created-intent rule).
    ///
    /// # Errors
    ///
    /// The journal's refusals.
    fn latest(&self, op: WriteOp, key: &str) -> Result<Option<IntentView>>;
}

/// One repository's journal.
pub struct Journal {
    /// The journal file, `<journal>/<owner>/<repo>.steps`; empty for a taped journal.
    path: PathBuf,
    /// Its repository.
    slug: Slug,
    /// The effect seam: the clock, and under a tape the journal door.
    fx: Arc<Fx>,
    /// How long `append` waits for the lock.
    lock_wait: Duration,
    /// The directories, for the stale need; `None` when taped.
    dirs: Option<Dirs>,
    /// Under a tape: the step the next append answers.
    taped_step: std::sync::atomic::AtomicU64,
}

impl Journal {
    /// The journal of `slug` under `dirs.journal()`: `<owner>/<repo>.steps`, owner and repo
    /// lowercased, its directories made 0700 by `config::make_state_subdir(dirs,
    /// &dirs.journal().join(owner))`, which follows each directory it makes with
    /// `aterm_durable::durable_dir` of its parent, so the new entry survives a power loss.
    /// Nothing is read until the first call. Under a tape (`fx.mode()` is Recording,
    /// Replaying or Rerecording) no directory is made, and every append is
    /// `Fx::emit_journal`.
    ///
    /// # Errors
    ///
    /// could_not_run, `Sub::Local`, why `the write journal {path} could not be made: {e}`;
    /// off unix `write::unix_only()`.
    pub fn open(dirs: &Dirs, slug: &Slug, fx: &Arc<Fx>) -> Result<Journal> {
        let _ = (dirs, slug, fx);
        Err(Error::not_yet("the write journal", "WP4"))
    }

    /// The journal of `slug` under a tape, with no file and no directory: every append is
    /// `Fx::emit_journal`, its step the tape's own counter, and it raises no need.
    /// `Journal::open` under a tape is this, and `Github::journal` calls it when a taped run
    /// has no `Dirs`.
    #[must_use]
    pub(crate) fn taped(slug: &Slug, fx: &Arc<Fx>) -> Journal {
        Journal {
            path: PathBuf::new(),
            slug: slug.clone(),
            fx: fx.clone(),
            lock_wait: LOCK_WAIT,
            dirs: None,
            taped_step: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The file; an empty path for a taped journal.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record and make it durable before answering its step: design §6.8's five
    /// steps, with the scan bounded by `JOURNAL_MAX`.
    ///
    /// # Errors
    ///
    /// - could_not_run, `Sub::Local`, why `another agh is writing {path}; run this again`,
    ///   when the lock was held for `LOCK_WAIT`;
    /// - Refused `journal_fault` (`Sub::JournalFault { at }`), why
    ///   `the write journal {path} is damaged at byte {at}: {why}`, next
    ///   `a person looks at {path}; agh never truncates a fault`;
    /// - Refused `journal_full` (`Sub::JournalFull { len }`), for an Intent record that would
    ///   take the file past `JOURNAL_MAX - JOURNAL_RESERVE` or any record past `JOURNAL_MAX`,
    ///   why `the write journal {path} holds {n} bytes, at its 64 MiB bound`, next
    ///   `agh intent list {repo} --open; once each is closed (a rerun of its write observes
    ///   it, or agh intent retract), a person moves {path} aside and agh starts a new one`.
    ///   A journal moved aside with an intent open drops the open-intent rule for its key,
    ///   and under listing lag a rerun could then create a second draft;
    /// - could_not_run for a failed write or flush, after cutting the file back to the
    ///   length it had.
    pub fn append(&self, record: &JournalRecord) -> Result<u64> {
        let _ = record;
        Err(self.not_yet("Journal::append"))
    }

    /// Every record in step order, and how the file ended. A torn tail is repaired, under
    /// the lock, before answering. A journal file that does not exist reads as no records
    /// and `Clean`, and nothing is made: `read` looks for the file (`symlink_metadata`)
    /// before it takes the lock, whose `.lock` file it would otherwise create. Under a tape:
    /// the records this run appended.
    ///
    /// # Errors
    ///
    /// As `append`, but a Fault is answered as `JournalEnd::Fault`, with every record before it.
    pub fn read(&self) -> Result<(Vec<(u64, JournalRecord)>, JournalEnd)> {
        Err(self.not_yet("Journal::read"))
    }

    /// The intent `id` and everything recorded about it since.
    ///
    /// # Errors
    ///
    /// As `read`.
    pub fn view(&self, id: &IntentId) -> Result<Option<IntentView>> {
        let _ = id;
        Err(self.not_yet("Journal::view"))
    }

    /// Append `Retracted` for an open intent.
    ///
    /// # Errors
    ///
    /// Refused `not_open`, why `intent {id} is not open: {state}`, for an intent that is
    /// settled, found or retracted already; NotFound for an id the journal does not hold;
    /// `append`'s errors.
    pub fn retract(&self, id: &IntentId) -> Result<()> {
        let _ = id;
        Err(self.not_yet("Journal::retract"))
    }

    /// Tests: the same journal with another lock wait.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_lock_wait_for_tests(mut self, wait: Duration) -> Journal {
        self.lock_wait = wait;
        self
    }

    /// A stub's refusal, reading every field so none is dead until WP4 writes the bodies
    /// (plan §1, rule 14).
    fn not_yet(&self, what: &str) -> Error {
        let _ = (
            &self.path,
            &self.slug,
            &self.fx,
            self.lock_wait,
            &self.dirs,
            &self.taped_step,
        );
        Error::not_yet(what, "WP4")
    }
}

impl IntentStore for Journal {
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId> {
        let _ = draft;
        Err(self.not_yet("IntentStore::begin"))
    }

    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()> {
        let _ = (id, outcome);
        Err(self.not_yet("IntentStore::settle"))
    }

    fn observe(&self, id: &IntentId, found: Option<u64>) -> Result<()> {
        let _ = (id, found);
        Err(self.not_yet("IntentStore::observe"))
    }

    fn open(&self) -> Result<Vec<Intent>> {
        Err(self.not_yet("IntentStore::open"))
    }

    fn latest(&self, op: WriteOp, key: &str) -> Result<Option<IntentView>> {
        let _ = (op, key);
        Err(self.not_yet("IntentStore::latest"))
    }
}

/// An intent and what the journal recorded of it since.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct IntentView {
    /// The intent.
    pub intent: Intent,
    /// Its Settled record, when there is one.
    pub settled: Option<Settled>,
    /// Its Observed records, in order: what each found, and when.
    pub observed: Vec<(Option<u64>, u64)>,
    /// When it was retracted.
    pub retracted: Option<u64>,
    /// Whether it is open (`IntentStore::open`'s rule).
    pub open: bool,
}

/// Every journal under `dirs.journal()`, by slug: the `*.steps` files two levels down, in
/// slug order. A name that is not a slug is skipped.
///
/// # Errors
///
/// could_not_run when the directory cannot be listed. A missing directory is an empty list.
pub fn journals(dirs: &Dirs) -> Result<Vec<(Slug, PathBuf)>> {
    let _ = dirs;
    Err(Error::not_yet("the journals' listing", "WP4"))
}

/// The record's body: one compact JSON object, sorted keys, `"v":1`.
#[must_use]
pub fn encode(record: &JournalRecord) -> Vec<u8> {
    // WP4 writes the body (plan §8); until then no record has bytes.
    let _ = record;
    Vec::new()
}

/// The record a body holds; `at` is its frame's byte offset, which a refusal names.
///
/// # Errors
///
/// Refused `journal_fault` (`Sub::JournalFault { at }`), why `step {step}: {what}`, for a body
/// that is not a v1 record.
pub fn decode(at: u64, step: u64, body: &[u8]) -> Result<JournalRecord> {
    let _ = (at, step, body);
    Err(Error::not_yet("the journal's decode", "WP4"))
}

#[cfg(test)]
mod id_order_tests {
    use super::IntentId;

    /// Ids order by step: from step 10 on, text order would put `10-…` before `9-…`.
    #[test]
    fn intent_ids_order_by_step() {
        let id = |text: &str| IntentId::parse(text).expect("an id");
        let mut ids = [
            id("10-0123456789ab"),
            id("9-0123456789ab"),
            id("0-ffffffffffff"),
        ];
        ids.sort();
        let steps: Vec<u64> = ids.iter().map(IntentId::step).collect();
        assert_eq!(steps, [0, 9, 10]);
        assert!(id("10-0123456789ab") > id("9-ffffffffffff"));
        assert!(id("9-0123456789aa") < id("9-0123456789ab"));
    }
}
