// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write journal (design §6.8): one append-only `astream_wire::steps` file per
//! repository, `<state>/agh/journal/<owner>/<repo>.steps`, both the audit ledger and the
//! intent store. Unix only (plan §1, rule 12).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_json::{Map, Value};

use crate::config::Dirs;
use crate::error::{Error, Kind, Result, Sub};
use crate::fx::Fx;
use crate::policy::Permit;
use crate::redact::Redactor;
use crate::slug::Slug;
use crate::write::WriteOp;

/// The largest journal agh reads or appends to: 64 MiB, which is about 16,000 releases.
/// Past it `append` refuses with Refused `journal_full`, and the file is moved aside once
/// no intent in it is open.
pub const JOURNAL_MAX: u64 = 64 << 20;
/// The tail of `JOURNAL_MAX` that only the records closing an intent may use: 1 MiB, room
/// for more than 250 records of agh's 4 KiB bound. Those records are an intent's first
/// `Settled`, and a `Retracted` or an `Observed` that found its object while its intent is
/// open. `append` refuses any other record (an Intent, an `Observed` that found nothing, or
/// one of an intent already closed) once the file is within one largest frame (a 4 KiB body
/// and its headers) of `JOURNAL_MAX - JOURNAL_RESERVE`, whatever that record's own size. So
/// once one Intent is refused every later one is, and an empty `agh intent list --open`
/// stays empty until the file is moved aside. A full journal can still settle, observe and
/// retract every intent it holds open, and a discovery rerun cannot use the reserve up.
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
        let hex = aterm_codec::hex::is_lower(hash, 12);
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
    /// (`create_step`'s `CreateStep::Begin`, or `Github::one_shot`) before `begin`. `slug` is
    /// the repository the permit was minted for (`Permit::target`), whose profile and grant
    /// the draft records; `begin` refuses a draft whose slug is not its journal's.
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
        /// Its `html_url`, or its `url` where it has none (a ref); empty when `settle` found
        /// it too long to journal.
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
    /// How the attempt ended: one per Intent, after the answer or its absence. `settle`
    /// refuses a second, and a view reads the first.
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
    /// `agh intent retract`: the intent was closed after a look at GitHub.
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
    /// A damaged frame in the middle. Nothing is appended until the file is looked at.
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
    /// `agh intent show {repo} {id}`. When the journal holds no intent of the op and key but
    /// `draft.after` names one, it is Conflict (exit 9), why `{path} holds no intent of {op}
    /// {key}, but its caller judged one`, and nothing is appended. A draft whose slug is not
    /// the journal's is Usage (exit 2), why `a {op} draft for {slug} cannot begin in the
    /// journal of {repo}`. A credential-shaped `key` or `target_commitish` is Usage (exit 2),
    /// why `the {op}'s {key|target} is shaped like a credential; agh journals it as written
    /// or not at all`, and nothing is appended.
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId>;
    /// Record how the attempt ended, once. An `Unknown`'s why is redacted and cut to 512
    /// bytes (`WHY_MAX`) and `…` first, and a `Created`'s url whose JSON text passes 2048
    /// bytes (`URL_MAX`) is journaled empty, since the created-intent rule reads the id. So
    /// the outcome of a write that was sent always fits a record.
    /// A closing outcome (Created, NeverSent, Refused) clears the intent's stale need.
    ///
    /// # Errors
    ///
    /// The journal's refusals. An intent that has a Settled record already is Usage
    /// (exit 2), why `intent {id} is settled already: agh settles an intent once`, and
    /// nothing is appended: what a later look finds is `observe`'s.
    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()>;
    /// Record what a discovery found. A found observation of an intent already closed is
    /// journaled, but it may not use `JOURNAL_RESERVE`.
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
    /// The journal's refusals. A damaged journal is Refused `journal_fault`, as `open`'s,
    /// because a record past the fault may have settled or closed the intent.
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
    /// Whether an append through this value has made the owner directory durable: a run that
    /// created the file may have died before its own directory sync (plan §8, body 5).
    dir_synced: AtomicBool,
    /// Under a tape: the records this run appended, at their steps; `None` for a file journal.
    taped: Option<Mutex<Vec<(u64, JournalRecord)>>>,
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
    /// off unix `write::unix_only()`, under a tape too (plan §8, body 3).
    pub fn open(dirs: &Dirs, slug: &Slug, fx: &Arc<Fx>) -> Result<Journal> {
        #[cfg(unix)]
        {
            if fx.mode().is_taped() {
                return Ok(Journal::taped(slug, fx));
            }
            let path = path_of(dirs, slug);
            let owner_dir = dirs.journal().join(slug.owner_key());
            crate::config::make_state_subdir(dirs, &owner_dir).map_err(|e| {
                Error::local(format!(
                    "the write journal {} could not be made: {e}",
                    path.display()
                ))
            })?;
            Ok(Journal {
                path,
                slug: slug.clone(),
                fx: fx.clone(),
                lock_wait: LOCK_WAIT,
                dirs: Some(dirs.clone()),
                dir_synced: AtomicBool::new(false),
                taped: None,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (dirs, slug, fx);
            Err(crate::write::unix_only())
        }
    }

    /// The journal of `slug` under a tape, with no file and no directory: every append is
    /// `Fx::emit_journal`, its step the tape's own counter, and it raises no need. Its
    /// records are this value's: a run keeps one value per repository, so each `begin`
    /// judges what the run journaled before it. `Journal::open` under a tape is this on
    /// unix, and `Github::journal` calls it when a taped run has no `Dirs`. Off unix it is
    /// made, but its `read` and every append answer `write::unix_only()` (plan §1, rule 12),
    /// so a replayed write is could_not_run there as a live one is.
    #[must_use]
    pub(crate) fn taped(slug: &Slug, fx: &Arc<Fx>) -> Journal {
        Journal {
            path: PathBuf::new(),
            slug: slug.clone(),
            fx: fx.clone(),
            lock_wait: LOCK_WAIT,
            dirs: None,
            dir_synced: AtomicBool::new(false),
            taped: Some(Mutex::new(Vec::new())),
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
    ///   `look at {path}; agh never truncates a fault`;
    /// - Refused `journal_full` (`Sub::JournalFull { len }`), for a record that may not use
    ///   `JOURNAL_RESERVE` once the file is within one largest frame of
    ///   `JOURNAL_MAX - JOURNAL_RESERVE`, or any record that would take it past `JOURNAL_MAX`,
    ///   why `the write journal {path} holds {n} bytes, at its 64 MiB bound`, next
    ///   `agh intent list {repo} --open; once each is closed (a rerun of its write observes
    ///   it, or agh intent retract), move {path} aside and agh starts a new one`.
    ///   A journal moved aside with an intent open drops the open-intent rule for its key,
    ///   and under listing lag a rerun could then create a second draft;
    /// - Usage for an Intent record whose `slug` (as written, lowercased), `key` or
    ///   `target_commitish` has a credential's shape, which the journal would otherwise hold
    ///   redacted (plan §1, rule 11);
    /// - Usage, why `the {type} record would not read back: {why}; agh journals it as
    ///   written or not at all`, for any record whose encoded body `decode` refuses at its
    ///   step (an Intent whose id names another step, say), so the journal never writes a
    ///   record its own scan would read as a fault;
    /// - could_not_run for a failed write or flush, after cutting the file back to the
    ///   length it had.
    ///
    /// A refused or failed append to a journal file that did not exist removes the file its
    /// scan made, so a refused call leaves no journal behind (its `.lock` file stays).
    pub fn append(&self, record: &JournalRecord) -> Result<u64> {
        self.append_with(|_, _| Ok(record.clone()))
    }

    /// Every record in step order, and how the file ended. A torn tail is repaired, under
    /// the lock, before answering. A journal file that does not exist reads as no records
    /// and `Clean`, and nothing is made: `read` looks for the file (`symlink_metadata`)
    /// before it takes the lock, whose `.lock` file it would otherwise create. Under a tape:
    /// the records this run appended. Off unix, a taped journal too is `write::unix_only()`
    /// (plan §1, rule 12).
    ///
    /// # Errors
    ///
    /// As `append`, but a Fault is answered as `JournalEnd::Fault`, with every record before it.
    pub fn read(&self) -> Result<(Vec<(u64, JournalRecord)>, JournalEnd)> {
        if let Some(taped) = &self.taped {
            #[cfg(unix)]
            {
                return Ok((lock(taped).clone(), JournalEnd::Clean));
            }
            #[cfg(not(unix))]
            {
                let _ = taped;
                return Err(crate::write::unix_only());
            }
        }
        self.read_file()
    }

    /// The intent `id` and everything recorded about it since.
    ///
    /// # Errors
    ///
    /// As `read`, and Refused `journal_fault` for a damaged journal, as `open` and `latest`
    /// are: a record past the fault may have closed the intent, or be the intent itself.
    pub fn view(&self, id: &IntentId) -> Result<Option<IntentView>> {
        let (records, end) = self.read()?;
        if let JournalEnd::Fault { at, why } = &end {
            return Err(self.fault(*at, why));
        }
        Ok(view_of(&records, id))
    }

    /// Append `Retracted` for an open intent.
    ///
    /// # Errors
    ///
    /// Refused `not_open`, why `intent {id} is not open: {state}`, for an intent that is
    /// settled, found or retracted already; NotFound for an id the journal does not hold;
    /// `append`'s errors.
    ///
    /// Both refusals still clear `agh.intent.{id}.stale`: the need's one action is this
    /// retract, so a need its intent outlived (a crash between a closing record and its
    /// clear, or a journal moved aside) goes when its own `then` is run. A need of an intent
    /// still open in another journal is raised again by that journal's next scan.
    pub fn retract(&self, id: &IntentId) -> Result<()> {
        let at_secs = self.fx.wall_secs()?;
        let mut moot = false;
        let retracted = self.append_with(|records, _| {
            let Some(view) = view_of(records, id) else {
                moot = true;
                return Err(Error::new(Kind::NotFound, self.not_in(id)).with_repo(self.repo()));
            };
            if let Some(state) = closed_state(&view) {
                moot = true;
                return Err(Error::refused(format!("intent {id} is not open: {state}"))
                    .with_sub(Sub::NotOpen {
                        state: state.to_string(),
                    })
                    .with_repo(self.repo()));
            }
            Ok(JournalRecord::Retracted {
                intent: id.clone(),
                at_secs,
            })
        });
        if retracted.is_ok() || moot {
            self.clear_stale(id);
        }
        retracted.map(|_| ())
    }

    /// Tests: the same journal with another lock wait.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_lock_wait_for_tests(mut self, wait: Duration) -> Journal {
        self.lock_wait = wait;
        self
    }

    /// The lowercased slug, as refusals and needs name the repository.
    fn repo(&self) -> String {
        self.slug.key()
    }

    /// `intent {id} is not in {path}`.
    fn not_in(&self, id: &IntentId) -> String {
        format!("intent {id} is not in {}", self.path.display())
    }

    /// Drop the stale need of `id`; a failure is ignored, as the rate book ignores its own.
    fn clear_stale(&self, id: &IntentId) {
        if let Some(dirs) = &self.dirs {
            let _ = crate::needs::clear(dirs, &stale_key(id));
        }
    }

    /// Append the record `build` makes from the records already there and the step it will
    /// take, under the lock (plan §8, body 5): so `begin` mints its id and `retract` judges
    /// its intent where no other appender can move the journal in between.
    fn append_with(
        &self,
        build: impl FnOnce(&[(u64, JournalRecord)], u64) -> Result<JournalRecord>,
    ) -> Result<u64> {
        if let Some(taped) = &self.taped {
            return self.append_taped(taped, build);
        }
        self.append_file(build)
    }

    /// A taped journal's append: built against this run's records, capped as a file's is,
    /// and emitted through the journal door; no lock and no file.
    #[cfg(unix)]
    fn append_taped(
        &self,
        taped: &Mutex<Vec<(u64, JournalRecord)>>,
        build: impl FnOnce(&[(u64, JournalRecord)], u64) -> Result<JournalRecord>,
    ) -> Result<u64> {
        let mut records = lock(taped);
        let step = u64::try_from(records.len())
            .map_err(|_| Error::local("the taped journal holds more records than a step counts"))?;
        let record = build(&records, step)?;
        refuse_shaped(&record)?;
        let body = encode(&record);
        if body.len() > RECORD_MAX {
            return Err(too_long(body.len(), 0));
        }
        reads_back(&record, 0, step, &body)?;
        let Some(taped_step) = self.fx.emit_journal(&body)? else {
            return Err(Error::local("the tape gave the journal no step"));
        };
        records.push((step, record));
        Ok(taped_step)
    }

    /// Off unix a taped journal records nothing, as a file journal does: every write is
    /// `write::unix_only()` (plan §1, rule 12), replayed or not.
    #[cfg(not(unix))]
    fn append_taped(
        &self,
        taped: &Mutex<Vec<(u64, JournalRecord)>>,
        build: impl FnOnce(&[(u64, JournalRecord)], u64) -> Result<JournalRecord>,
    ) -> Result<u64> {
        let _ = (taped, build);
        Err(crate::write::unix_only())
    }

    /// A file journal's read: the locked scan, which cuts a torn tail back, or no records
    /// and `Clean` for a file that is not there, with nothing made.
    #[cfg(unix)]
    fn read_file(&self) -> Result<(Vec<(u64, JournalRecord)>, JournalEnd)> {
        if missing(&self.path) {
            return Ok((Vec::new(), JournalEnd::Clean));
        }
        let Some(scan) = self.scan(false)? else {
            return Ok((Vec::new(), JournalEnd::Clean));
        };
        // Under the lock, so no other agh closes an intent between this scan and its need.
        self.raise_stale(&scan.records, &scan.end);
        Ok((scan.records, scan.end))
    }

    /// Off unix a file journal is never made (`Journal::open` is `write::unix_only()`).
    #[cfg(not(unix))]
    fn read_file(&self) -> Result<(Vec<(u64, JournalRecord)>, JournalEnd)> {
        let _ = (self.lock_wait, &self.dirs, &self.dir_synced);
        Err(crate::write::unix_only())
    }

    /// A file journal's append (plan §8, body 5).
    #[cfg(unix)]
    fn append_file(
        &self,
        build: impl FnOnce(&[(u64, JournalRecord)], u64) -> Result<JournalRecord>,
    ) -> Result<u64> {
        let Some(mut scan) = self.scan(true)? else {
            return Err(Error::local(format!(
                "the write journal {} vanished while it was locked",
                self.path.display()
            )));
        };
        let answer = self.append_locked(&mut scan, build);
        if answer.is_err() && scan.created {
            // Nothing this call made was answered: the file its scan created goes, under
            // the lock, so a refused call leaves no journal for `journals` to list. The
            // `.lock` file stays, because another agh may be waiting on it.
            if let Some(name) = self.path.file_name().and_then(|n| n.to_str()) {
                let _ = aterm_uds::private_file::remove_private(&scan.owner_dir, name);
            }
        }
        // Under the lock, over the records the append left (the one it added among them),
        // so a record that closed its intent raises nothing, and no other agh closes one
        // between this scan and its need.
        self.raise_stale(&scan.records, &scan.end);
        answer
    }

    /// Body 5's steps 1 to 6 over a locked scan; on success the record joins `scan.records`.
    #[cfg(unix)]
    fn append_locked(
        &self,
        scan: &mut Scan,
        build: impl FnOnce(&[(u64, JournalRecord)], u64) -> Result<JournalRecord>,
    ) -> Result<u64> {
        if let JournalEnd::Fault { at, why } = &scan.end {
            return Err(self.fault(*at, why));
        }
        let step = u64::try_from(scan.records.len()).map_err(|_| self.too_far())?;
        let record = build(&scan.records, step)?;
        refuse_shaped(&record)?;
        let body = encode(&record);
        if body.len() > RECORD_MAX {
            return Err(too_long(body.len(), scan.valid_len).with_repo(self.repo()));
        }
        reads_back(&record, scan.valid_len, step, &body).map_err(|e| e.with_repo(self.repo()))?;
        let frame_len =
            u64::try_from(FRAME_OVERHEAD.saturating_add(body.len())).map_err(|_| self.too_far())?;
        let worst =
            u64::try_from(FRAME_OVERHEAD.saturating_add(RECORD_MAX)).map_err(|_| self.too_far())?;
        let end = scan.valid_len.saturating_add(frame_len);
        // Judged by the file's length and the largest frame, not this record's own: once one
        // Intent is refused, every later one is, whatever its size.
        let near = scan.valid_len.saturating_add(worst) > JOURNAL_MAX - JOURNAL_RESERVE;
        if end > JOURNAL_MAX || (near && !may_use_reserve(&record, &scan.records)) {
            return Err(self.full(scan.valid_len));
        }
        let frame = astream_wire::steps::encode_step(step, &body)
            .map_err(|e| Error::local(format!("the journal record cannot be framed: {e}")))?;
        let written = self.write_frame(scan, &frame);
        if let Err(e) = written {
            // Best effort: no caller is told the record was written, so its bytes go.
            let _ = scan.file.set_len(scan.valid_len);
            return Err(Error::local(format!(
                "the write journal {} could not be written: {e}",
                self.path.display()
            )));
        }
        scan.records.push((step, record));
        Ok(step)
    }

    /// Off unix a file journal is never made (`Journal::open` is `write::unix_only()`).
    #[cfg(not(unix))]
    fn append_file(
        &self,
        build: impl FnOnce(&[(u64, JournalRecord)], u64) -> Result<JournalRecord>,
    ) -> Result<u64> {
        let _ = (build, self.lock_wait, &self.dirs, &self.dir_synced);
        Err(crate::write::unix_only())
    }

    /// Body 5's steps 4 and 5: the frame at the valid length, the data made durable, then the
    /// owner directory when this scan created the file or this value has not synced it yet.
    #[cfg(unix)]
    fn write_frame(&self, scan: &Scan, frame: &[u8]) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt as _;
        scan.file.write_all_at(frame, scan.valid_len)?;
        aterm_durable::durable_data(&scan.file)?;
        if scan.created || !self.dir_synced.load(Ordering::Acquire) {
            aterm_durable::durable_dir(&scan.owner_dir)?;
            self.dir_synced.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// The locked scan (plan §8, body 4), which `append` and `read` share. `None` when `read`
    /// (`create` false) finds the file gone once it holds the lock: it answers as a missing
    /// file does, and makes none. Its step 8, the needs, is `raise_stale`, which each caller
    /// runs while it still holds the scan, and the lock with it.
    #[cfg(unix)]
    fn scan(&self, create: bool) -> Result<Option<Scan>> {
        use std::io::Read as _;

        let owner_dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let lock_name = format!("{name}.lock");
        let held = aterm_uds::private_file::PrivateLock::take(
            &owner_dir,
            &lock_name,
            self.lock_wait,
            "busy",
        );
        let lock = held.map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Error::local(format!(
                    "another agh is writing {}; run this again",
                    self.path.display()
                ))
            } else {
                Error::local(format!(
                    "the write journal {} could not be locked: {e}",
                    self.path.display()
                ))
            }
        })?;
        let created = missing(&self.path);
        if created && !create {
            return Ok(None);
        }
        let file = aterm_uds::private_file::open_private_rw(&owner_dir, &name).map_err(|e| {
            Error::local(format!(
                "the write journal {} cannot be opened: {e}",
                self.path.display()
            ))
        })?;
        let len = file.metadata().map_err(|e| self.io(&e))?.len();
        if len > JOURNAL_MAX {
            return Err(self.full(len));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(len.saturating_add(1)).unwrap_or(0));
        (&file)
            .take(JOURNAL_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| self.io(&e))?;
        let scanned = astream_wire::steps::scan_steps_from(&bytes, 0);
        let mut valid_len = u64::try_from(scanned.valid_len).map_err(|_| self.too_far())?;
        let mut end = match scanned.end {
            astream_wire::steps::StepEnd::Clean => JournalEnd::Clean,
            astream_wire::steps::StepEnd::Torn => {
                file.set_len(valid_len).map_err(|e| self.io(&e))?;
                aterm_durable::durable_data(&file).map_err(|e| self.io(&e))?;
                JournalEnd::TornRepaired { at: valid_len }
            }
            astream_wire::steps::StepEnd::Fault { at, kind } => JournalEnd::Fault {
                at: u64::try_from(at).map_err(|_| self.too_far())?,
                why: kind.to_string(),
            },
        };
        let mut records = Vec::with_capacity(scanned.steps.len());
        for (step, range) in scanned.steps {
            let start = range.start.saturating_sub(FRAME_OVERHEAD);
            let at = u64::try_from(start).map_err(|_| self.too_far())?;
            let decoded = bytes
                .get(range)
                .ok_or_else(|| self.too_far())
                .and_then(|body| decode(at, step, body));
            match decoded {
                Ok(record) => records.push((step, record)),
                Err(e) => {
                    valid_len = at;
                    end = JournalEnd::Fault {
                        at,
                        why: e.into_data().why,
                    };
                    break;
                }
            }
        }
        Ok(Some(Scan {
            _lock: lock,
            file,
            owner_dir,
            records,
            valid_len,
            end,
            created,
        }))
    }

    /// Body 8 of the scan: every intent the records leave open past `SETTLE_WINDOW` raises
    /// its need. `read` and `append` call it under the scan's lock, over the records the scan
    /// read and the one an append added, so a record that closed its intent raises nothing,
    /// and every closer clears its need after a raise it could race. A need already raised
    /// is left as it is (one `symlink_metadata`), so each stale intent's need is written once,
    /// at the time it was first seen, not on every scan other agh processes wait for. A
    /// damaged file (`JournalEnd::Fault`) raises nothing: a record past the fault may have
    /// closed the intent, as `open` and `latest` refuse to judge it. A raise that fails is
    /// ignored, as the rate book ignores its failures.
    #[cfg(unix)]
    fn raise_stale(&self, records: &[(u64, JournalRecord)], end: &JournalEnd) {
        let Some(dirs) = &self.dirs else {
            return;
        };
        if matches!(end, JournalEnd::Fault { .. }) {
            return;
        }
        let Ok(now) = self.fx.wall_secs() else {
            return;
        };
        for view in views(records).iter().filter(|v| v.open) {
            let due = view.intent.at_secs.saturating_add(SETTLE_WINDOW.as_secs());
            if due < now && !crate::needs::raised(dirs, &stale_key(&view.intent.id)) {
                let _ = crate::needs::raise(dirs, &self.stale_need(&view.intent, now));
            }
        }
    }

    /// `agh.intent.{id}.stale`, with plan §4.6.15's texts.
    #[cfg(unix)]
    fn stale_need(&self, intent: &Intent, now: u64) -> crate::needs::Need {
        let id = &intent.id;
        let repo = self.repo();
        crate::needs::Need::new(
            &stale_key(id),
            &format!(
                "an agh write never settled: {} {} on {repo}",
                intent.op.slug(),
                intent.key
            ),
            &format!(
                "intent {id} has been open since {}; agh never posts it again and found no \
                 object for it",
                aterm_types::rfc3339::format_rfc3339(intent.at_secs)
            ),
            &format!("look, then run: agh intent retract {repo} {id} --confirm {id}"),
            now,
        )
    }

    /// Refused `journal_fault` for a damaged journal.
    pub(crate) fn fault(&self, at: u64, why: &str) -> Error {
        let path = self.path.display();
        Error::refused(format!(
            "the write journal {path} is damaged at byte {at}: {why}"
        ))
        .with_sub(Sub::JournalFault { at })
        .with_next(format!("look at {path}; agh never truncates a fault"))
        .with_repo(self.repo())
    }

    #[cfg(unix)]
    /// Refused `journal_full` for a journal of `len` bytes.
    fn full(&self, len: u64) -> Error {
        let path = self.path.display();
        let repo = self.repo();
        Error::refused(format!(
            "the write journal {path} holds {len} bytes, at its 64 MiB bound"
        ))
        .with_sub(Sub::JournalFull { len })
        .with_next(format!(
            "agh intent list {repo} --open; once each is closed (a rerun of its write observes \
             it, or agh intent retract), move {path} aside and agh starts a new one"
        ))
        .with_repo(repo)
    }

    #[cfg(unix)]
    /// could_not_run for an offset this platform does not address.
    fn too_far(&self) -> Error {
        Error::local(format!(
            "the write journal {} is longer than this platform addresses",
            self.path.display()
        ))
    }

    /// could_not_run for an I/O error on the journal.
    #[cfg(unix)]
    fn io(&self, e: &std::io::Error) -> Error {
        Error::local(format!(
            "the write journal {} could not be read or repaired: {e}",
            self.path.display()
        ))
    }

    /// The re-judging's refusal (`IntentStore::begin`): OutcomeUnknown `intent_open`.
    fn began_first(&self, id: &IntentId, key: &str) -> Error {
        let repo = self.repo();
        Error::new(
            Kind::OutcomeUnknown,
            format!("intent {id} for {key} began first: agh never posts twice"),
        )
        .with_sub(Sub::IntentOpen {
            intent: id.to_string(),
        })
        .with_next(format!("agh intent show {repo} {id}"))
        .with_repo(repo)
    }

    /// The record of `id`'s intent judged under the lock: Usage when no Intent holds it.
    fn holds(&self, records: &[(u64, JournalRecord)], id: &IntentId) -> Result<()> {
        let held = records
            .iter()
            .any(|(_, r)| matches!(r, JournalRecord::Intent(i) if &i.id == id));
        if held {
            Ok(())
        } else {
            Err(Error::usage(self.not_in(id)).with_repo(self.repo()))
        }
    }
}

impl IntentStore for Journal {
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId> {
        let op = draft.op.slug();
        if draft.slug != self.slug {
            // Another repository's intent here would be missed by its own journal's
            // open-intent rule, so a rerun through that journal would post again.
            let repo = self.repo();
            return Err(Error::usage(format!(
                "a {op} draft for {} cannot begin in the journal of {repo}",
                draft.slug.key(),
            ))
            .with_repo(repo));
        }
        for (field, value) in [
            ("key", Some(draft.key.as_str())),
            ("target", draft.target_commitish.as_deref()),
        ] {
            if value.is_some_and(shaped) {
                return Err(Error::usage(format!(
                    "the {op}'s {field} is shaped like a credential; agh journals it as written \
                     or not at all"
                ))
                .with_repo(self.repo()));
            }
        }
        let at_secs = self.fx.wall_secs()?;
        let mut minted = None;
        self.append_with(|records, step| {
            let of_key = views_filtered(records, |i| i.op == draft.op && i.key == draft.key);
            if draft.op != WriteOp::RefCreate
                && let Some(open) = of_key.iter().rev().find(|v| v.open)
            {
                return Err(self.began_first(&open.intent.id, &draft.key));
            }
            let latest = of_key.last().map(|v| &v.intent.id);
            if latest != draft.after.as_ref() {
                return Err(match latest {
                    Some(latest) => self.began_first(latest, &draft.key),
                    None => Error::new(
                        Kind::Conflict,
                        format!(
                            "{} holds no intent of {op} {}, but its caller judged one",
                            self.path.display(),
                            draft.key
                        ),
                    )
                    .with_repo(self.repo()),
                });
            }
            let id = mint(step, &draft.slug, draft.op, &draft.key);
            minted = Some(id.clone());
            Ok(JournalRecord::Intent(Box::new(Intent {
                id,
                op: draft.op,
                slug: draft.slug.clone(),
                identity: draft.identity.clone(),
                fingerprint: draft.fingerprint.clone(),
                key: draft.key.clone(),
                request: draft.request.clone(),
                target_commitish: draft.target_commitish.clone(),
                body_digest: draft.body_digest.clone(),
                grant: draft.grant.clone(),
                at_secs,
            })))
        })?;
        minted.ok_or_else(|| Error::local("the journal appended an intent and minted no id"))
    }

    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()> {
        let outcome = bounded(outcome);
        self.append_with(|records, _| {
            let Some(view) = view_of(records, id) else {
                return Err(Error::usage(self.not_in(id)).with_repo(self.repo()));
            };
            if view.settled.is_some() {
                return Err(Error::usage(format!(
                    "intent {id} is settled already: agh settles an intent once"
                ))
                .with_repo(self.repo()));
            }
            Ok(JournalRecord::Settled {
                intent: id.clone(),
                outcome: outcome.clone(),
            })
        })?;
        if settles_closed(&outcome) {
            self.clear_stale(id);
        }
        Ok(())
    }

    fn observe(&self, id: &IntentId, found: Option<u64>) -> Result<()> {
        let at_secs = self.fx.wall_secs()?;
        self.append_with(|records, _| {
            self.holds(records, id)?;
            Ok(JournalRecord::Observed {
                intent: id.clone(),
                found,
                at_secs,
            })
        })?;
        if found.is_some() {
            self.clear_stale(id);
        }
        Ok(())
    }

    fn open(&self) -> Result<Vec<Intent>> {
        let (records, end) = self.read()?;
        if let JournalEnd::Fault { at, why } = &end {
            return Err(self.fault(*at, why));
        }
        Ok(views(&records)
            .into_iter()
            .filter(|v| v.open)
            .map(|v| v.intent)
            .collect())
    }

    fn latest(&self, op: WriteOp, key: &str) -> Result<Option<IntentView>> {
        let (records, end) = self.read()?;
        if let JournalEnd::Fault { at, why } = &end {
            return Err(self.fault(*at, why));
        }
        let Some(id) = records.iter().rev().find_map(|(_, r)| match r {
            JournalRecord::Intent(i) if i.op == op && i.key == key => Some(&i.id),
            _ => None,
        }) else {
            return Ok(None);
        };
        Ok(view_of(&records, id))
    }
}

/// What the locked scan holds for `append`: the lock, the file, the records it decoded, the
/// length they end at, how the file ended, and whether this scan created the file.
#[cfg(unix)]
struct Scan {
    /// Held for the whole append; released by `PrivateLock`'s `LOCK_UN` when dropped.
    _lock: aterm_uds::private_file::PrivateLock,
    /// The journal, read and write.
    file: std::fs::File,
    /// The directory that names it.
    owner_dir: PathBuf,
    /// The records, in step order.
    records: Vec<(u64, JournalRecord)>,
    /// Where the last accepted record ends.
    valid_len: u64,
    /// How the file ended.
    end: JournalEnd,
    /// Whether the file was missing before this scan opened it.
    created: bool,
}

/// The longest body a record may have: agh's records are under 4 KiB (design §6.8).
#[cfg(unix)]
const RECORD_MAX: usize = 4096;
/// A frame's bytes before its body: the frame header and the step header.
#[cfg(unix)]
const FRAME_OVERHEAD: usize = astream_wire::frame::HEADER_SIZE + astream_wire::steps::STEP_HEADER;

/// The lock of a taped journal's records; a poisoned lock still holds whole records.
#[cfg(unix)]
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether nothing is at `path` (`symlink_metadata`'s `NotFound`).
#[cfg(unix)]
fn missing(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// Refused `journal_fault` for a body of `n` bytes, at `at`.
#[cfg(unix)]
fn too_long(n: usize, at: u64) -> Error {
    Error::refused(format!(
        "a record of {n} bytes: agh's records are under 4 KiB"
    ))
    .with_sub(Sub::JournalFault { at })
}

/// Whether `Redactor::shapes` would change `text`.
fn shaped(text: &str) -> bool {
    Redactor::shapes(text) != text
}

/// Usage for an Intent record whose match fields or slug have a credential's shape: the
/// journal holds them as written or not at all (plan §1, rule 11).
#[cfg(unix)]
fn refuse_shaped(record: &JournalRecord) -> Result<()> {
    let JournalRecord::Intent(intent) = record else {
        return Ok(());
    };
    let op = intent.op.slug();
    // The bytes `encode` writes: the lowercased slug, whose shape can differ from the slug as
    // the caller typed it (`GHS_…` reads as no token, its lowercased `ghs_…` as one).
    let slug = intent.slug.key();
    for (field, value) in [
        ("repo", Some(slug.as_str())),
        ("key", Some(intent.key.as_str())),
        ("target", intent.target_commitish.as_deref()),
    ] {
        if value.is_some_and(shaped) {
            return Err(Error::usage(format!(
                "the {op}'s {field} is shaped like a credential; agh journals it as written or \
                 not at all"
            )));
        }
    }
    Ok(())
}

/// Usage for a record whose encoded `body` `decode` refuses at `step`: the journal writes
/// only what its own scan reads back as a record, never a fault (plan §1, rule 11).
#[cfg(unix)]
fn reads_back(record: &JournalRecord, at: u64, step: u64, body: &[u8]) -> Result<()> {
    decode(at, step, body).map(|_| ()).map_err(|e| {
        Error::usage(format!(
            "the {} record would not read back: {}; agh journals it as written or not at all",
            type_name(record),
            e.into_data().why
        ))
    })
}

/// The record's `type`, as `encode` writes it.
#[cfg(unix)]
const fn type_name(record: &JournalRecord) -> &'static str {
    match record {
        JournalRecord::Intent(_) => "intent",
        JournalRecord::Settled { .. } => "settled",
        JournalRecord::Observed { .. } => "observed",
        JournalRecord::Retracted { .. } => "retracted",
    }
}

/// Whether `record` may use `JOURNAL_RESERVE`, judged over the `records` before it: an
/// intent's first Settled, or a Retracted or an Observed that found its object while its
/// intent is open. Anything else closes no intent: an Intent, an Observed that found
/// nothing (a rerun discovery could append those without bound), or a record of an intent
/// already closed or not in the journal.
#[cfg(unix)]
fn may_use_reserve(record: &JournalRecord, records: &[(u64, JournalRecord)]) -> bool {
    let (intent, while_open) = match record {
        JournalRecord::Settled { intent, .. } => (intent, false),
        JournalRecord::Retracted { intent, .. }
        | JournalRecord::Observed {
            intent,
            found: Some(_),
            ..
        } => (intent, true),
        JournalRecord::Intent(_) | JournalRecord::Observed { found: None, .. } => return false,
    };
    view_of(records, intent).is_some_and(|v| {
        if while_open {
            v.open
        } else {
            v.settled.is_none()
        }
    })
}

/// `settle` cuts an `Unknown`'s why to `WHY_MAX` (512) bytes on a character boundary and
/// appends `…`. JSON writes a control byte as six, so even the worst why's record stays under
/// `RECORD_MAX`.
const WHY_MAX: usize = 512;

/// The longest `Settled::Created` url `settle` journals, as JSON text (escapes included):
/// 2048 bytes. A longer one is journaled empty, so the record stays under `RECORD_MAX`.
const URL_MAX: usize = 2048;

/// `outcome` as `settle` journals it: an `Unknown`'s why redacted, then cut on a character
/// boundary to `WHY_MAX` bytes and `…`; a `Created`'s url emptied when its redacted JSON text
/// is over `URL_MAX` bytes, since a cut url would name another page and the created-intent
/// rule reads the id. Redacting before the cut means a cut never leaves part of a token the
/// Redactor would no longer see.
fn bounded(outcome: &Settled) -> Settled {
    if let Settled::Created { id, url } = outcome {
        let text = aterm_json::to_string(&Value::from(Redactor::shapes(url)));
        if !text.is_ok_and(|t| t.len() <= URL_MAX) {
            return Settled::Created {
                id: *id,
                url: String::new(),
            };
        }
    }
    let Settled::Unknown { why } = outcome else {
        return outcome.clone();
    };
    let shaped = Redactor::shapes(why);
    if shaped.len() <= WHY_MAX {
        return Settled::Unknown { why: shaped };
    }
    let mut end = WHY_MAX;
    while !shaped.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let head = shaped.get(..end).unwrap_or_default();
    Settled::Unknown {
        why: format!("{head}…"),
    }
}

/// `agh.intent.{id}.stale`.
fn stale_key(id: &IntentId) -> String {
    format!("agh.intent.{id}.stale")
}

/// The id of an intent at `step`: `{step}-{first 12 hex of sha256(slug ‖ 0 ‖ op ‖ 0 ‖ key)}`,
/// the slug lowercased (plan §8, body 1).
fn mint(step: u64, slug: &Slug, op: WriteOp, key: &str) -> IntentId {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(slug.key().as_bytes());
    hasher.update([0u8]);
    hasher.update(op.slug().as_bytes());
    hasher.update([0u8]);
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let short = aterm_codec::hex::encode_lower(&digest[..6]);
    IntentId(format!("{step}-{short}"))
}

/// What closed `view`, or `None` while it is open: `retracted`, `found` or `settled`.
fn closed_state(view: &IntentView) -> Option<&'static str> {
    if view.retracted.is_some() {
        Some("retracted")
    } else if view.observed.iter().any(|(found, _)| found.is_some()) {
        Some("found")
    } else if view.settled.as_ref().is_some_and(settles_closed) {
        Some("settled")
    } else {
        None
    }
}

/// Whether `settled` closes its intent: Created, NeverSent or Refused.
const fn settles_closed(settled: &Settled) -> bool {
    matches!(
        settled,
        Settled::Created { .. } | Settled::NeverSent | Settled::Refused { .. }
    )
}

/// Every intent's view, in step order, from one pass over `records` (plan §8, body 7).
pub(crate) fn views(records: &[(u64, JournalRecord)]) -> Vec<IntentView> {
    views_filtered(records, |_| true)
}

fn views_filtered(
    records: &[(u64, JournalRecord)],
    keep: impl Fn(&Intent) -> bool,
) -> Vec<IntentView> {
    let mut views: Vec<IntentView> = Vec::new();
    let mut index: HashMap<&IntentId, usize> = HashMap::new();
    for (_, record) in records {
        match record {
            JournalRecord::Intent(intent) if keep(intent) => {
                index.insert(&intent.id, views.len());
                views.push(IntentView {
                    intent: (**intent).clone(),
                    settled: None,
                    observed: Vec::new(),
                    retracted: None,
                    open: true,
                });
            }
            JournalRecord::Intent(_) => {}
            JournalRecord::Settled { intent, outcome } => {
                // The first counts: `settle` refuses a second, and a spliced one must not
                // close an intent left Unknown.
                if let Some(view) = index.get(intent).and_then(|&i| views.get_mut(i)) {
                    view.settled.get_or_insert_with(|| outcome.clone());
                }
            }
            JournalRecord::Observed {
                intent,
                found,
                at_secs,
            } => {
                if let Some(view) = index.get(intent).and_then(|&i| views.get_mut(i)) {
                    view.observed.push((*found, *at_secs));
                }
            }
            JournalRecord::Retracted { intent, at_secs } => {
                if let Some(view) = index.get(intent).and_then(|&i| views.get_mut(i)) {
                    view.retracted.get_or_insert(*at_secs);
                }
            }
        }
    }
    for view in &mut views {
        view.open = closed_state(view).is_none();
    }
    views
}

/// The view of `id` from one pass over `records`, without cloning unrelated intents.
pub(crate) fn view_of(records: &[(u64, JournalRecord)], id: &IntentId) -> Option<IntentView> {
    let mut view: Option<IntentView> = None;
    for (_, record) in records {
        match record {
            JournalRecord::Intent(intent) if &intent.id == id && view.is_none() => {
                view = Some(IntentView {
                    intent: (**intent).clone(),
                    settled: None,
                    observed: Vec::new(),
                    retracted: None,
                    open: true,
                });
            }
            JournalRecord::Settled { intent, outcome } if intent == id => {
                if let Some(view) = &mut view {
                    view.settled.get_or_insert_with(|| outcome.clone());
                }
            }
            JournalRecord::Observed {
                intent,
                found,
                at_secs,
            } if intent == id => {
                if let Some(view) = &mut view {
                    view.observed.push((*found, *at_secs));
                }
            }
            JournalRecord::Retracted { intent, at_secs } if intent == id => {
                if let Some(view) = &mut view {
                    view.retracted.get_or_insert(*at_secs);
                }
            }
            _ => {}
        }
    }
    if let Some(view) = &mut view {
        view.open = closed_state(view).is_none();
    }
    view
}

/// An intent and what the journal recorded of it since.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct IntentView {
    /// The intent.
    pub intent: Intent,
    /// Its first Settled record, when there is one.
    pub settled: Option<Settled>,
    /// Its Observed records, in order: what each found, and when.
    pub observed: Vec<(Option<u64>, u64)>,
    /// When it was retracted.
    pub retracted: Option<u64>,
    /// Whether it is open (`IntentStore::open`'s rule).
    pub open: bool,
}

/// The journal file of `slug` under `dirs`: `<journal>/<owner>/<repo>.steps`, owner and repo
/// lowercased. `Journal::open` writes there and a dry run looks there, so the two never part.
#[must_use]
pub(crate) fn path_of(dirs: &Dirs, slug: &Slug) -> PathBuf {
    dirs.journal()
        .join(slug.owner_key())
        .join(format!("{}.steps", slug.repo().to_ascii_lowercase()))
}

/// Every journal under `dirs.journal()`, by slug: the `*.steps` files two levels down, in
/// slug order. A name that is not a slug is skipped.
///
/// # Errors
///
/// could_not_run when the directory cannot be listed. A missing directory is an empty list.
pub fn journals(dirs: &Dirs) -> Result<Vec<(Slug, PathBuf)>> {
    let root = dirs.journal();
    let listed = |dir: &Path, e: &std::io::Error| {
        Error::local(format!(
            "the write journals under {} cannot be listed: {e}",
            dir.display()
        ))
    };
    let owners = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(listed(&root, &e)),
    };
    let mut found = Vec::new();
    for owner in owners {
        let owner = owner.map_err(|e| listed(&root, &e))?;
        let name = owner.file_name();
        let Some(owner_name) = name.to_str().filter(|n| crate::config::is_owner_name(n)) else {
            continue;
        };
        if !owner.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = owner.path();
        let files = std::fs::read_dir(&dir).map_err(|e| listed(&dir, &e))?;
        for file in files {
            let file = file.map_err(|e| listed(&dir, &e))?;
            let name = file.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".steps")) else {
                continue;
            };
            let Ok(slug) = Slug::new(owner_name, stem) else {
                continue;
            };
            if file.file_type().is_ok_and(|t| t.is_file()) {
                found.push((slug, file.path()));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// The record's body: one compact JSON object, sorted keys, `"v":1`.
#[must_use]
pub fn encode(record: &JournalRecord) -> Vec<u8> {
    let text = |s: &str| Value::from(Redactor::shapes(s));
    let maybe = |s: &Option<String>| s.as_deref().map_or(Value::Null, text);
    let mut object = Map::new();
    object.insert("v".into(), Value::from(1u64));
    match record {
        JournalRecord::Intent(intent) => {
            object.insert("type".into(), Value::from("intent"));
            object.insert("at_secs".into(), Value::from(intent.at_secs));
            object.insert("body_digest".into(), maybe(&intent.body_digest));
            object.insert("fingerprint".into(), text(&intent.fingerprint));
            object.insert("grant".into(), maybe(&intent.grant));
            object.insert("id".into(), Value::from(intent.id.as_str()));
            object.insert("identity".into(), text(&intent.identity));
            object.insert("key".into(), text(&intent.key));
            object.insert("op".into(), Value::from(intent.op.slug()));
            object.insert("request".into(), text(&intent.request));
            object.insert("slug".into(), text(&intent.slug.key()));
            object.insert("target_commitish".into(), maybe(&intent.target_commitish));
        }
        JournalRecord::Settled { intent, outcome } => {
            object.insert("type".into(), Value::from("settled"));
            object.insert("intent".into(), Value::from(intent.as_str()));
            match outcome {
                Settled::NeverSent => {
                    object.insert("outcome".into(), Value::from("never_sent"));
                }
                Settled::Refused { status } => {
                    object.insert("outcome".into(), Value::from("refused"));
                    object.insert("status".into(), Value::from(*status));
                }
                Settled::Created { id, url } => {
                    object.insert("outcome".into(), Value::from("created"));
                    object.insert("id".into(), Value::from(*id));
                    object.insert("url".into(), text(url));
                }
                Settled::Unknown { why } => {
                    object.insert("outcome".into(), Value::from("unknown"));
                    object.insert("why".into(), text(why));
                }
            }
        }
        JournalRecord::Observed {
            intent,
            found,
            at_secs,
        } => {
            object.insert("type".into(), Value::from("observed"));
            object.insert("at_secs".into(), Value::from(*at_secs));
            object.insert("found".into(), Value::from(*found));
            object.insert("intent".into(), Value::from(intent.as_str()));
        }
        JournalRecord::Retracted { intent, at_secs } => {
            object.insert("type".into(), Value::from("retracted"));
            object.insert("at_secs".into(), Value::from(*at_secs));
            object.insert("intent".into(), Value::from(intent.as_str()));
        }
    }
    // A `Value` built of strings, integers and null always renders.
    aterm_json::to_string(&Value::Object(object))
        .unwrap_or_default()
        .into_bytes()
}

/// The record a body holds; `at` is its frame's byte offset, which a refusal names.
///
/// # Errors
///
/// Refused `journal_fault` (`Sub::JournalFault { at }`), why `step {step}: {what}`, for a body
/// that is not a v1 record.
pub fn decode(at: u64, step: u64, body: &[u8]) -> Result<JournalRecord> {
    let fault = |what: String| {
        Error::refused(format!("step {step}: {what}")).with_sub(Sub::JournalFault { at })
    };
    let value: Value = aterm_json::from_slice(body).map_err(|_| fault("not JSON".into()))?;
    if value.as_object().is_none() {
        return Err(fault("not a JSON object".into()));
    }
    let fields = Fields {
        value: &value,
        fault: &fault,
    };
    let v = fields.number("v")?;
    if v != 1 {
        return Err(fault(format!("v is {v}")));
    }
    let kind = fields.string("type")?;
    match kind {
        "intent" => {
            let id_text = fields.string("id")?;
            let id = IntentId::parse(id_text)
                .ok_or_else(|| fault(format!("intent id {id_text:?} does not parse")))?;
            if id.step() != step {
                return Err(fault(format!("intent id {id} names step {}", id.step())));
            }
            let op_text = fields.string("op")?;
            let op = WriteOp::parse(op_text).ok_or_else(|| fault(format!("op {op_text:?}")))?;
            let slug_text = fields.string("slug")?;
            let slug = Slug::parse(slug_text)
                .map_err(|_| fault(format!("slug {slug_text:?} is not a repository")))?;
            Ok(JournalRecord::Intent(Box::new(Intent {
                id,
                op,
                slug,
                identity: fields.string("identity")?.to_string(),
                fingerprint: fields.string("fingerprint")?.to_string(),
                key: fields.string("key")?.to_string(),
                request: fields.string("request")?.to_string(),
                target_commitish: fields.maybe_string("target_commitish")?,
                body_digest: fields.maybe_string("body_digest")?,
                grant: fields.maybe_string("grant")?,
                at_secs: fields.number("at_secs")?,
            })))
        }
        "settled" => {
            let intent = fields.intent()?;
            let outcome = match fields.string("outcome")? {
                "never_sent" => Settled::NeverSent,
                "refused" => {
                    let status = fields.number("status")?;
                    let status = u16::try_from(status)
                        .map_err(|_| fault(format!("status {status} is no HTTP status")))?;
                    Settled::Refused { status }
                }
                "created" => Settled::Created {
                    id: fields.number("id")?,
                    url: fields.string("url")?.to_string(),
                },
                "unknown" => Settled::Unknown {
                    why: fields.string("why")?.to_string(),
                },
                other => return Err(fault(format!("outcome {other:?}"))),
            };
            Ok(JournalRecord::Settled { intent, outcome })
        }
        "observed" => Ok(JournalRecord::Observed {
            intent: fields.intent()?,
            found: fields.maybe_number("found")?,
            at_secs: fields.number("at_secs")?,
        }),
        "retracted" => Ok(JournalRecord::Retracted {
            intent: fields.intent()?,
            at_secs: fields.number("at_secs")?,
        }),
        other => Err(fault(format!("type {other:?}"))),
    }
}

/// A body's fields, each read with its kind checked; a missing or mistyped one is `decode`'s
/// fault `{key} is missing or not a {kind}`.
struct Fields<'a, F: Fn(String) -> Error> {
    /// The body.
    value: &'a Value,
    /// `decode`'s fault for this body.
    fault: &'a F,
}

impl<'a, F: Fn(String) -> Error> Fields<'a, F> {
    /// `{key} is missing or not a {kind}`.
    fn mistyped(&self, key: &str, kind: &str) -> Error {
        (self.fault)(format!("{key} is missing or not a {kind}"))
    }

    /// A string.
    fn string(&self, key: &str) -> Result<&'a str> {
        self.value
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| self.mistyped(key, "string"))
    }

    /// A string or `null`.
    fn maybe_string(&self, key: &str) -> Result<Option<String>> {
        match self.value.get(key) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            _ => Err(self.mistyped(key, "string or null")),
        }
    }

    /// A non-negative integer.
    fn number(&self, key: &str) -> Result<u64> {
        self.value
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| self.mistyped(key, "number"))
    }

    /// A non-negative integer or `null`.
    fn maybe_number(&self, key: &str) -> Result<Option<u64>> {
        match self.value.get(key) {
            Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .map(Some)
                .ok_or_else(|| self.mistyped(key, "number or null")),
            None => Err(self.mistyped(key, "number or null")),
        }
    }

    /// The `intent` an outcome record names.
    fn intent(&self) -> Result<IntentId> {
        let text = self.string("intent")?;
        IntentId::parse(text).ok_or_else(|| (self.fault)(format!("intent {text:?} is no id")))
    }
}

#[cfg(all(test, unix))]
#[path = "journal_tests.rs"]
mod tests;

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
