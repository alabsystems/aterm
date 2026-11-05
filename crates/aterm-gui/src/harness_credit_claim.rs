// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The claim act (`[harness] claim_credits`): where Claude Code's cached offers name a free
//! credit live for a supervised session's account ([`credit_offers`]), the window's host starts
//! one worker that claims it in the person's own Chrome ([`credit_offers::claim_task`]).
//!
//! The trigger is the supervised Claude Code session's decided idle turn end, where the host
//! already reads its usage tap ([`super::WorkerIdle`]'s `usage_tap`): no timer of its own. A look
//! there costs one `stat` of the profile while its size and modification time stand
//! ([`ProfileCache`]); the file is parsed only when they move.
//!
//! Each offer is tried once, then at most once more, no sooner than [`RETRY_AFTER_MS`] after
//! the first try and only while it is still live. Every try is recorded durably ([`Ledger`],
//! a small owner-only file in the harness's state) before its worker is asked for, so a crash
//! or a failed spawn never makes a second worker of the same try; a full ledger drops a row
//! whose tries are spent, else the least recently tried, and never one tried at that look.
//!
//! A try is spent only where a worker can do the claim, so nothing a person must clear up is
//! made: never from a session that is one of aterm's own workers ([`Gate::own_worker`]: a
//! claim worker's own idle turn end starts no other); never on a link a claim worker asked for or
//! running already acts on in the person's one Chrome, whatever account the profile names now
//! ([`Claims`], [`Stepped::Covered`]: the ledger's key names the account, so a login change
//! makes the same link a new key); never while the login the worker would start on is at a usage
//! limit ([`Stepped::Walled`]); and never before Chrome and its Claude extension are in place
//! ([`ready`], [`Stepped::NotReady`]), which the look installs what it can of and records once
//! what is left for a person. A look after any of these tries again.
//!
//! The worker is fenced by where it starts: its folder is
//! [`credit_offers::CLAIM_FOLDER`] under the harness state directory ([`worker_folder_under`]),
//! the supervisor knows it by that trusted folder, and its approval rule (`claim-worker@v1`)
//! approves only its Chrome actions on the vendor's claim pages and declines every other box, a
//! question or another Chrome action on its own Esc, so none waits on a person. It starts in auto
//! mode, so it runs with nobody at its boxes, or in `default` where `[harness] approve = "none"`
//! keeps every box the person's ([`crate::task_launch::claim_worker_mode`]), and with no built-in
//! tool but `ToolSearch` (`--tools`, [`credit_offers::CLAIM_TOOLS`]), so its Chrome tools are
//! what it acts with in any mode; that rule judges any box Claude Code still raises. Its tab is
//! named for its offer ([`worker_role`]) and its task ends in one of the two replies a managed
//! Autopilot Land worker concludes on: claimed, which lands, or `Could not finish: NOT CLAIMED:`,
//! which fails. Its tab is ephemeral, as every worker's is, so the reaper closes it once its task
//! concluded ([`crate::worker_reaper`]); one whose task never concludes is ended once it has run
//! [`WORKER_FOR`] since it started or a person last typed in its tab ([`ran_out`]): its program,
//! where the program in front names itself the agent its record names, then its task, marked
//! `exited`, which concludes it. A seamless update carries the task record that clock reads; any
//! other restore does not, so the layout names each worker's folder (and an agent in a claim
//! folder that is no worker's) and a restore starts a new record for its tab: `working` for a
//! worker it brings back or one in a shell the PTY keeper hands back, `exited` where the worker
//! had ended or is not coming back (a plain quit forgets a running one), so a program found in
//! that tab is not timed as it and the tab still closes ([`note_restored_worker`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::credit_offers::{self, Offer, OfferId, OfferKey, Stamp};
use aterm_agent::harness::profile::NoProfile;
use aterm_agent::supervise::Autopilot;

use crate::task_record::{TaskRecord, TaskState};

#[cfg(any(target_os = "macos", test))]
#[path = "harness_claim_ready.rs"]
pub(crate) mod ready;

/// The least time between an offer's first try and its one retry.
pub(crate) const RETRY_AFTER_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The most tries an offer gets: the first and one retry.
pub(crate) const TRIES_MAX: u32 = 2;

/// The folder under the harness's state root the ledger is kept in, owner-only.
const LEDGER_DIR: &str = "credit-claims";

/// The ledger's file in [`LEDGER_DIR`].
#[cfg(unix)]
const LEDGER_FILE: &str = "attempts";

/// The ledger's first line.
#[cfg(any(unix, test))]
const LEDGER_HEAD: &str = "aterm-credit-claims v1";

/// The most offers the ledger keeps (the most recently tried kept).
const LEDGER_ROWS: usize = 16;

/// The most bytes the ledger is read to.
#[cfg(any(unix, test))]
const LEDGER_MAX: usize = 4096;

/// The most profiles [`ProfileCache`] keeps a reading of.
const CACHE_ROWS: usize = 8;

/// How long a claim worker whose task never concludes may run with nobody typing in its tab
/// ([`ran_out`], [`crate::App::reap_workers`]).
pub(crate) const WORKER_FOR: Duration = Duration::from_secs(30 * 60);

/// One offer's tries ([`Ledger`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tried {
    pub(crate) key: OfferKey,
    /// How many tries were made.
    pub(crate) count: u32,
    /// The first try, unix milliseconds.
    pub(crate) first_ms: i64,
    /// The latest try, unix milliseconds.
    pub(crate) last_ms: i64,
}

/// The tries made, by offer: the act's durable record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ledger {
    pub(crate) rows: Vec<Tried>,
}

impl Ledger {
    /// The ledger's text back: `None` for text that is not one.
    #[cfg(any(unix, test))]
    pub(crate) fn parse(text: &str) -> Option<Ledger> {
        let mut lines = text.lines();
        if lines.next()? != LEDGER_HEAD {
            return None;
        }
        let mut rows = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let mut words = line.split(' ');
            let key = OfferKey::parse(words.next()?)?;
            let mut next_digits = || {
                words
                    .next()
                    .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
            };
            let row = Tried {
                key,
                count: next_digits()?.parse().ok()?,
                first_ms: next_digits()?.parse().ok()?,
                last_ms: next_digits()?.parse().ok()?,
            };
            if words.next().is_some() || rows.len() >= LEDGER_ROWS {
                return None;
            }
            rows.push(row);
        }
        Some(Ledger { rows })
    }

    /// The ledger as its file holds it.
    #[cfg(any(unix, test))]
    pub(crate) fn text(&self) -> String {
        let mut out = format!("{LEDGER_HEAD}\n");
        for row in &self.rows {
            out.push_str(&format!(
                "{} {} {} {}\n",
                row.key, row.count, row.first_ms, row.last_ms
            ));
        }
        out
    }

    /// Whether `key` may be tried at `now_ms`: never tried, or tried once at least
    /// [`RETRY_AFTER_MS`] ago.
    pub(crate) fn due(&self, key: &OfferKey, now_ms: i64) -> bool {
        match self.rows.iter().find(|row| &row.key == key) {
            None => true,
            Some(row) => {
                row.count < TRIES_MAX && now_ms.saturating_sub(row.first_ms) >= RETRY_AFTER_MS
            }
        }
    }

    /// A try of each of `keys` at `now_ms` recorded (at most [`LEDGER_ROWS`] of them, which
    /// [`step`] keeps to), then rows dropped past [`LEDGER_ROWS`]: never one of `keys`, so no try
    /// is unrecorded as it is made, whatever the clock says; first a row whose tries are spent
    /// ([`TRIES_MAX`]), which [`Self::due`] would never try again, then the least recently tried.
    pub(crate) fn tried(&mut self, keys: &[OfferKey], now_ms: i64) {
        for key in keys {
            match self.rows.iter_mut().find(|row| &row.key == key) {
                Some(row) => {
                    row.count = row.count.saturating_add(1);
                    row.last_ms = now_ms;
                }
                None => self.rows.push(Tried {
                    key: key.clone(),
                    count: 1,
                    first_ms: now_ms,
                    last_ms: now_ms,
                }),
            }
        }
        while self.rows.len() > LEDGER_ROWS {
            let Some(dropped) = self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| !keys.contains(&row.key))
                .min_by_key(|(_, row)| (row.count < TRIES_MAX, row.last_ms))
                .map(|(at, _)| at)
            else {
                break;
            };
            self.rows.remove(dropped);
        }
    }
}

/// The ledger's folder under the harness's state root `state`.
pub(crate) fn ledger_dir(state: &Path) -> PathBuf {
    state.join(LEDGER_DIR)
}

/// The ledger in `dir`: empty where no file is there yet.
/// # Errors
/// Why it could not be read: an unreadable ledger tries nothing.
#[cfg(unix)]
pub(crate) fn read_ledger(dir: &Path) -> Result<Ledger, String> {
    use aterm_uds::private_file::{PrivateFileError, read_private};
    match read_private(dir, LEDGER_FILE, LEDGER_MAX) {
        Ok(bytes) => std::str::from_utf8(&bytes)
            .ok()
            .and_then(Ledger::parse)
            .ok_or_else(|| "the claims ledger is not one this build wrote".to_string()),
        Err(PrivateFileError::Missing) => Ok(Ledger::default()),
        Err(e) => Err(format!("the claims ledger could not be read ({e})")),
    }
}

/// [`read_ledger`] where no private file can be kept: nothing is tried.
#[cfg(not(unix))]
pub(crate) fn read_ledger(_dir: &Path) -> Result<Ledger, String> {
    Err("no owner-only ledger on this platform".to_string())
}

/// `ledger` written to `dir` (made owner-only), atomically, `0600`.
/// # Errors
/// Why it was not written: nothing is tried.
#[cfg(unix)]
pub(crate) fn write_ledger(dir: &Path, ledger: &Ledger) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("the harness state folder could not be made ({e})"))?;
    }
    aterm_uds::private_file::create_private_dir(dir)
        .map_err(|e| format!("the claims ledger's folder is not owner-only ({e})"))?;
    aterm_uds::private_file::write_private(dir, LEDGER_FILE, ledger.text().as_bytes())
        .map_err(|e| format!("the claims ledger could not be written ({e})"))
}

/// [`write_ledger`] where no private file can be kept.
#[cfg(not(unix))]
pub(crate) fn write_ledger(_dir: &Path, _ledger: &Ledger) -> Result<(), String> {
    Err("no owner-only ledger on this platform".to_string())
}

/// What the last parse of each profile found, by its [`Stamp`]: a look whose stamp stands
/// parses nothing.
#[derive(Default)]
pub(crate) struct ProfileCache {
    rows: Mutex<Vec<(PathBuf, Stamp, Vec<Offer>)>>,
}

impl ProfileCache {
    pub(crate) const fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
        }
    }

    /// The live offers in the profile at `path`: the kept reading while its stamp stands, else
    /// `read`'s (an unread file has none).
    pub(crate) fn offers(
        &self,
        path: &Path,
        read: &dyn Fn(&Path) -> Result<Vec<Offer>, NoProfile>,
    ) -> Vec<Offer> {
        let Some(stamp) = credit_offers::stamp(path) else {
            return Vec::new();
        };
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, _, offers)) = rows.iter().find(|(p, s, _)| p == path && *s == stamp) {
            return offers.clone();
        }
        let offers = read(path).unwrap_or_default();
        rows.retain(|(p, _, _)| p != path);
        rows.push((path.to_path_buf(), stamp, offers.clone()));
        if rows.len() > CACHE_ROWS {
            rows.remove(0);
        }
        offers
    }
}

/// The host's one [`ProfileCache`].
pub(crate) static PROFILES: ProfileCache = ProfileCache::new();

/// Held while the ledger is read, decided and written, and the claims asked for are noted
/// ([`Claims::asked`]), so two tabs of one account make one try and two tabs of any accounts
/// one worker per link.
static LEDGER_LOCK: Mutex<()> = Mutex::new(());

/// What lets the act run for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gate {
    /// The harness supervises this session's agent here, its worker not stopping.
    pub(crate) on: bool,
    /// The session's agent is Claude Code.
    pub(crate) claude: bool,
    /// The session's Autopilot.
    pub(crate) autopilot: Autopilot,
    /// `[harness] claim_credits` under the master switch.
    pub(crate) claim_credits: bool,
    /// The session is a worker aterm started for its own ends (a credit claim's or the disk
    /// worker's, by its task record's trusted folder,
    /// [`crate::task_record::reports_its_own_end`]), or one the store no longer holds
    /// ([`own_worker`]): its own idle turn end starts no claim.
    pub(crate) own_worker: bool,
}

impl Gate {
    /// Every condition holds.
    pub(crate) fn open(&self) -> bool {
        self.on
            && self.claude
            && self.autopilot != Autopilot::Off
            && self.claim_credits
            && !self.own_worker
    }
}

/// What one look did ([`step`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stepped {
    /// The gate is shut: nothing was looked at.
    Shut,
    /// No offer is live.
    NothingLive,
    /// Every live offer's link is one a claim worker asked for or running already acts on
    /// ([`Claims::covers`]): no try is spent and nothing is recorded.
    Covered,
    /// Every live offer has had its tries.
    NothingDue,
    /// The login a claim worker would start on is at a usage limit until this instant, unix
    /// seconds ([`Seams::walled`]): no try is spent, so a look after it tries.
    Walled(i64),
    /// What the claim needs is not all in place: what is, and what is left ([`Seams::left`]).
    /// No try is spent and no tab opens; the next look checks again.
    NotReady(String),
    /// The ledger could not be read or written: nothing was started.
    Unrecorded(String),
    /// These offers were recorded and their workers asked for.
    Started(Vec<Offer>),
}

/// A live claim worker, as a look reads it ([`claim_worker`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClaimWorker {
    /// Its session.
    pub(crate) session: u64,
    /// Its task is marked `exited`: it acts in Chrome no more.
    pub(crate) ended: bool,
}

/// Who holds a link ([`Claims`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holder {
    /// A worker asked for at this instant, whose tab has not landed.
    Asked(Instant),
    /// The worker running in this session.
    Runs(u64),
}

/// One link a claim worker holds ([`Claims`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Claim {
    id: OfferId,
    url: String,
    holder: Holder,
}

/// The claim workers asked for or running, by the offer and link each claims: what keeps a
/// second worker off a link one already acts on in the person's one Chrome, whatever account the
/// profile names now. In this process's memory only, so a worker a restore or a seamless update
/// brought back is no row's, and covers every link while it runs ([`Self::covers`]).
pub(crate) struct Claims {
    rows: Mutex<Vec<Claim>>,
}

impl Claims {
    pub(crate) const fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
        }
    }

    /// Whether offer `id`'s link `url` is covered at `now`, with `workers` the live claim
    /// workers: a worker asked for it or runs for it, or a running worker no row names (one this
    /// process did not ask for) runs, which may be acting on any link. Rows of a worker that
    /// ended or is gone go first, and so does an ask whose tab never landed within
    /// [`WORKER_FOR`].
    pub(crate) fn covers(
        &self,
        id: OfferId,
        url: &str,
        workers: &[ClaimWorker],
        now: Instant,
    ) -> bool {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.retain(|row| match row.holder {
            Holder::Runs(session) => workers.iter().any(|w| w.session == session && !w.ended),
            Holder::Asked(at) => now.saturating_duration_since(at) < WORKER_FOR,
        });
        rows.iter().any(|row| row.id == id && row.url == url)
            || workers
                .iter()
                .any(|w| !w.ended && !rows.iter().any(|row| row.holder == Holder::Runs(w.session)))
    }

    /// A worker for offer `id`'s link `url` was asked for at `now`.
    pub(crate) fn asked(&self, id: OfferId, url: &str, now: Instant) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.retain(|row| !(row.id == id && row.url == url));
        rows.push(Claim {
            id,
            url: url.to_owned(),
            holder: Holder::Asked(now),
        });
    }

    /// That worker's tab landed as session `session`.
    pub(crate) fn spawned(&self, id: OfferId, url: &str, session: u64) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.retain(|row| !(row.id == id && row.url == url));
        rows.push(Claim {
            id,
            url: url.to_owned(),
            holder: Holder::Runs(session),
        });
    }

    /// That worker did not start: the link is free again (the ledger still holds its try).
    pub(crate) fn refused(&self, id: OfferId, url: &str) {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|row| {
                !(row.id == id && row.url == url && matches!(row.holder, Holder::Asked(_)))
            });
    }
}

/// The host's one [`Claims`].
pub(crate) static CLAIMS: Claims = Claims::new();

/// What a look reads besides the profile and the ledger ([`step`]): live, the host's own
/// ([`PROFILES`], [`CLAIMS`], its store's claim workers, its accounts and this machine), a
/// test's own in tests.
pub(crate) struct Seams<'a> {
    /// The profiles' kept readings.
    pub(crate) cache: &'a ProfileCache,
    /// A profile's parse.
    pub(crate) read: &'a dyn Fn(&Path) -> Result<Vec<Offer>, NoProfile>,
    /// The claim workers asked for or running.
    pub(crate) claims: &'a Claims,
    /// Every live claim worker ([`claim_workers`]).
    pub(crate) workers: &'a dyn Fn() -> Vec<ClaimWorker>,
    /// Until when, unix seconds, the login a claim worker would start on is at a usage limit
    /// ([`crate::task_launch::claude_worker_wall`]), or `None` where it can run.
    pub(crate) walled: &'a dyn Fn() -> Option<i64>,
    /// What is in place for the claim and what is left, where something is ([`ready`]), or
    /// `None` where all of it is.
    pub(crate) left: &'a dyn Fn() -> Option<String>,
}

/// One look of the act: under an open `gate`, the live offers in `profile` (through
/// `seams.cache` and `seams.read`); of those no claim worker covers ([`Claims::covers`]) and the
/// ledger in `ledger` holds due, each recorded there first, where the worker's login runs and
/// what the claim needs is in place, then noted asked for and handed to `start`. The walled and
/// readiness reads are made only where an offer is due.
pub(crate) fn step(
    gate: Gate,
    profile: &Path,
    ledger: &Path,
    now_ms: i64,
    now: Instant,
    seams: &Seams<'_>,
    start: &mut dyn FnMut(Offer),
) -> Stepped {
    if !gate.open() {
        return Stepped::Shut;
    }
    let offers = seams.cache.offers(profile, seams.read);
    if offers.is_empty() {
        return Stepped::NothingLive;
    }
    let due = {
        let _held = LEDGER_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let workers = (seams.workers)();
        let open: Vec<Offer> = offers
            .into_iter()
            .filter(|offer| !seams.claims.covers(offer.id, &offer.url, &workers, now))
            .collect();
        if open.is_empty() {
            return Stepped::Covered;
        }
        let mut book = match read_ledger(ledger) {
            Ok(book) => book,
            Err(why) => return Stepped::Unrecorded(why),
        };
        let due: Vec<Offer> = open
            .into_iter()
            .filter(|offer| book.due(&offer.key, now_ms))
            .take(LEDGER_ROWS)
            .collect();
        if due.is_empty() {
            return Stepped::NothingDue;
        }
        if let Some(until) = (seams.walled)() {
            return Stepped::Walled(until);
        }
        if let Some(left) = (seams.left)() {
            return Stepped::NotReady(left);
        }
        let keys: Vec<OfferKey> = due.iter().map(|offer| offer.key.clone()).collect();
        book.tried(&keys, now_ms);
        if let Err(why) = write_ledger(ledger, &book) {
            return Stepped::Unrecorded(why);
        }
        for offer in &due {
            seams.claims.asked(offer.id, &offer.url, now);
        }
        due
    };
    for offer in &due {
        start(offer.clone());
    }
    Stepped::Started(due)
}

/// Whether session `sid` is one of aterm's own workers ([`Gate::own_worker`]): its task record
/// trusts a claim folder or the disk worker's folder
/// ([`crate::task_record::reports_its_own_end`]). A session the store does not hold is read as
/// one, so a look of a session that went starts nothing.
pub(crate) fn own_worker(store: &crate::session_store::Store, sid: &str) -> bool {
    let Some(handle) = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .cloned()
    else {
        return true;
    };
    let timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    crate::task_record::reports_its_own_end(timeline.task().and_then(|t| t.trusted_cwd.as_deref()))
}

/// Every live claim worker in `store` ([`claim_worker`]), the claim look's [`Seams::workers`].
pub(crate) fn claim_workers(store: &crate::session_store::Store) -> Vec<ClaimWorker> {
    let handles: Vec<_> = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .live_handles()
        .cloned()
        .collect();
    handles
        .iter()
        .filter_map(|handle| {
            claim_worker(&handle.ctx).map(|(_, ended)| ClaimWorker {
                session: handle.local_id,
                ended,
            })
        })
        .collect()
}

/// [`Seams::walled`] on this machine: the claim worker starts as Claude Code with no starter's
/// login (no caller), on the row the launch's own accounts read would start it on
/// ([`crate::task_launch::claude_worker_wall`]).
pub(crate) fn walled_here() -> Option<i64> {
    let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Claude)?;
    let seat = crate::task_launch::PoolSeat::here()?;
    crate::task_launch::claude_worker_wall(&cli.unstarted(), &seat.seat())
}

/// [`Seams::left`] on this machine: Chrome and its Claude extension, put in place where aterm
/// can ([`ready::here`]).
#[cfg(target_os = "macos")]
pub(crate) fn left_here() -> Option<String> {
    ready::here(Instant::now())
}

/// [`Seams::left`] off macOS: the claim runs in Google Chrome on a Mac, which aterm sets up
/// there alone.
#[cfg(not(target_os = "macos"))]
pub(crate) fn left_here() -> Option<String> {
    Some("aterm sets up a credit claim's Chrome only on a Mac".to_string())
}

/// The name a claim worker's tab carries from its spawn (its `role`), for `offer`: the chip,
/// the Workers list and every message name it by its offer, never by its launch line.
pub(crate) const fn worker_role(offer: OfferId) -> &'static str {
    match offer {
        OfferId::ApiCredits => "Claim API credit",
        OfferId::CloudCredit => "Claim cloud credit",
    }
}

/// The order a claim worker is spawned with: Claude Code with the person's browser
/// (`--chrome`), its task [`credit_offers::claim_task`], under Autopilot Land, with no starting
/// session and no fact line, trusting `folder`.
pub(crate) fn claim_order(offer: &Offer, folder: Option<&Path>) -> crate::task_launch::TaskOrder {
    crate::task_launch::TaskOrder {
        task: credit_offers::claim_task(offer).into_bytes(),
        agent: aterm_phase::Program::Claude,
        model: None,
        caller: None,
        trusted_cwd: folder
            .and_then(Path::to_str)
            .and_then(crate::spawn_folder::canonical_folder),
        fact_line: false,
        autopilot: Autopilot::Land,
        chrome: true,
    }
}

/// The folder a claim worker starts in, [`credit_offers::CLAIM_FOLDER`] under the harness state
/// directory `state` ([`super::harness_state`]), made owner-only: the supervisor fences the
/// session trusted it ([`credit_offers::is_claim_folder`]).
pub(crate) fn worker_folder_under(state: &Path) -> Option<PathBuf> {
    let dir = state.join(credit_offers::CLAIM_FOLDER);
    #[cfg(unix)]
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// How long the open task `record` has run by `now`, `typed` after a person last typed in its tab:
/// from its start, which no later state moves, or from that typing where it came later. Monotonic
/// time, so a Mac asleep spends none of it.
pub(crate) fn stood(record: &TaskRecord, now: Instant, typed: Option<Duration>) -> Duration {
    let stood = now.saturating_duration_since(record.started);
    typed.map_or(stood, |typed| stood.min(typed))
}

/// Whether a task that trusted `folder` and has `stood` that long is a claim worker's that ran out
/// of its time. All of it is the task record's, which a seamless update carries, so a worker an
/// earlier process started is judged as this one's is.
pub(crate) fn ran_out(folder: Option<&str>, stood: Duration) -> bool {
    stood >= WORKER_FOR
        && folder.is_some_and(|folder| credit_offers::is_claim_folder(Path::new(folder)))
}

impl crate::App {
    /// Start a claim worker for `offer`, found by the supervised session `from`
    /// ([`super::HostNote::CreditClaim`]), the way the disk worker starts: a background Claude
    /// Code tab under Autopilot Land, trusted the claim folder ([`worker_folder_under`]), so the
    /// harness fences its boxes, and named for its offer ([`worker_role`]). The try is already
    /// recorded and its link held ([`step`]): a landed tab holds it by its session, and a worker
    /// that did not start gives it back ([`Claims::refused`]). The tab it opens, or why none
    /// opened, is a fact for later in the digest.
    pub(crate) fn start_credit_claim(&mut self, from: &str, offer: Offer) {
        // What the claim waited on is in place now: a later wait is a new record.
        self.claim_not_ready = None;
        let folder = super::harness_state().and_then(|state| worker_folder_under(&state));
        let Some(folder) = folder else {
            let why = "no folder of aterm's own to start it in (no state directory)";
            aterm_log::warn!(
                "harness @{from}: credit claim {}: not started: {why}",
                offer.id.word()
            );
            CLAIMS.refused(offer.id, &offer.url);
            self.except(&crate::message_reporters::credit_claim_not_started_entry(
                &offer, why,
            ));
            return;
        };
        let order = claim_order(&offer, Some(&folder));
        let booking = crate::control::control_media::SpawnBooking {
            cwd: folder.to_str().map(str::to_string),
            split: None,
            raise: false,
            admission: crate::tab_spawn::SpawnAdmission::Plain,
        };
        let plan = match self.resolve_spawn(crate::control::SpawnAim::Front, Some(order)) {
            Ok(plan) => plan,
            Err(why) => {
                aterm_log::warn!(
                    "harness @{from}: credit claim {}: not started: {why}",
                    offer.id.word()
                );
                CLAIMS.refused(offer.id, &offer.url);
                self.except(&crate::message_reporters::credit_claim_not_started_entry(
                    &offer, &why,
                ));
                return;
            }
        };
        let from = from.to_string();
        let _act = crate::tab_marks::ActScope::enter(
            crate::session_store::ExitActor::Harness,
            "claim-credits",
        );
        self.book_spawn(
            plan,
            booking,
            Box::new(move |app, spawned| {
                match spawned {
                    Ok(spawned) => {
                        let sid = app.start_spawned(spawned);
                        aterm_log::info!(
                            "harness @{from}: credit claim {}: worker runs as @{sid}",
                            offer.id.word()
                        );
                        app.claim_worker_landed(&sid, &offer);
                        let spot = app.upgrade_spot(&sid);
                        app.except(&crate::message_reporters::credit_claim_started_entry(
                            &offer, &sid, &spot,
                        ));
                    }
                    Err(why) => {
                        aterm_log::warn!(
                            "harness @{from}: credit claim {}: not started: {why}",
                            offer.id.word()
                        );
                        CLAIMS.refused(offer.id, &offer.url);
                        app.except(&crate::message_reporters::credit_claim_not_started_entry(
                            &offer, &why,
                        ));
                    }
                }
                app.mark_status_dirty();
            }),
        );
    }

    /// A claim worker for `offer` landed as session `sid`: it holds its link by its session
    /// ([`Claims::spawned`]) and its tab is named for its offer ([`worker_role`]), before anything
    /// reads its place, so no chip or message names it by the line its shell is typing.
    pub(crate) fn claim_worker_landed(&mut self, sid: &str, offer: &Offer) {
        let handle = self
            .store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_sid(&aterm_session::SessionId::new(sid))
            .cloned();
        let Some(handle) = handle else {
            CLAIMS.refused(offer.id, &offer.url);
            return;
        };
        CLAIMS.spawned(offer.id, &offer.url, handle.local_id);
        let named = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set("role", Some(worker_role(offer.id).to_owned()));
        if named == Some(true) {
            crate::presence::post_meta_changed(handle.local_id);
        }
    }

    /// What a credit claim waits on, `why` ([`Stepped::NotReady`]), recorded in Messages once:
    /// the same words again record nothing, and new words reword the record standing, so the
    /// journal holds one line for the wait however many looks found it. Never an alert: it is
    /// a record of what aterm put in place and what is left.
    pub(crate) fn credit_claim_not_ready(&mut self, why: &str) {
        if self
            .claim_not_ready
            .as_ref()
            .is_some_and(|(said, _)| said == why)
        {
            return;
        }
        let msg = crate::message_reporters::credit_claim_not_ready_record(why);
        if let Some((_, id)) = self.claim_not_ready.take()
            && self.reword_record(id, &msg)
        {
            self.claim_not_ready = Some((why.to_owned(), id));
            return;
        }
        let id = self.record_message(msg);
        self.claim_not_ready = Some((why.to_owned(), id));
    }
}

/// The claim folder of session `ctx`'s task where that task is a claim worker's (its folder is a
/// claim folder), with whether it ended (marked `exited`). The layout carries both
/// ([`crate::restore::TerminalLeafRestore::claim_worker`], `claim_ended`), so a restore gives
/// that tab a record whether or not a program runs there, and times no program it brings back in
/// an ended worker's tab as the worker ([`note_restored_worker`]).
pub(crate) fn claim_worker(ctx: &crate::SessionCtx) -> Option<(String, bool)> {
    let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
    let task = timeline.task()?;
    let folder = task
        .trusted_cwd
        .clone()
        .filter(|folder| credit_offers::is_claim_folder(Path::new(folder)))?;
    Some((folder, task.state == TaskState::Exited))
}

/// Names session `local` (`ctx`) a claim worker where a restore other than a seamless update
/// brings back the agent that ran in folder `cwd`, or a tab whose worker is not coming back
/// (`ended`: it had ended, or a quit forgot its agent; [`claim_worker`]), if `cwd` is a claim
/// folder: the record that held its time did not cross, and a tab with none would never end
/// ([`crate::App::reap_workers`]). A new record is marked where none stands, so its time counts
/// from here; one already standing keeps its time and is named the worker. The new record is
/// `working why=restored`, which no startup or held deadline reads as a worker just spawned, or,
/// for a tab whose worker is not coming back, `exited why=exit`: a program found or brought back
/// in that tab is then not the worker's, and the tab closes once that mark has stood
/// [`crate::worker_reaper::LINGER`] and that program, if any, was last seen as long ago
/// ([`crate::worker_reaper::Stand::reading`]).
pub(crate) fn note_restored_worker(ctx: &crate::SessionCtx, local: u64, cwd: &str, ended: bool) {
    if !credit_offers::is_claim_folder(Path::new(cwd)) {
        return;
    }
    let mut mark = crate::task_record::TaskMark::new(if ended {
        TaskState::Exited
    } else {
        TaskState::Working
    });
    mark.why = Some(if ended {
        "exit"
    } else {
        crate::task_record::WHY_RESTORED
    });
    let marked = crate::task_record::mark_session_task_if(
        ctx,
        local,
        &mark,
        ctx.fabric.fence_wait(),
        |record| record.is_none(),
    );
    if marked.is_ok() {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_task_worker(aterm_phase::Program::Claude, Some(cwd.to_owned()));
    }
}

#[cfg(test)]
#[path = "harness_credit_claim_tests.rs"]
mod tests;
