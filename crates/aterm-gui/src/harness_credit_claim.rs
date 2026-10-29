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
//! The worker is fenced by where it starts: its folder is
//! [`credit_offers::CLAIM_FOLDER`] under the harness state directory ([`worker_folder_under`]),
//! the supervisor knows it by that trusted folder, and its approval rule (`claim-worker@v1`)
//! approves only its Chrome actions on the vendor's claim pages, hands its questions and any
//! other Chrome action to a person, and declines every other tool's box. It starts with every box
//! raised (`--permission-mode default`, [`crate::task_launch::worker_fresh`]), its task ends in the
//! reply Autopilot Land lands on, and a worker this process started that still runs
//! [`WORKER_FOR`] after its start has its Autopilot turned Off ([`Running`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::credit_offers::{self, Offer, OfferKey, Stamp};
use aterm_agent::harness::profile::NoProfile;
use aterm_agent::supervise::Autopilot;

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

/// How long a claim worker runs under its Autopilot: past it, a worker this process started has
/// its Autopilot turned Off ([`Running::expired`]), so Land types nothing more into it.
pub(crate) const WORKER_FOR: Duration = Duration::from_secs(30 * 60);

/// The most claim workers [`Running`] keeps; past it the earliest started is taken as expired.
const RUNNING_ROWS: usize = 16;

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
            let row = Tried {
                key: OfferKey::parse(words.next()?)?,
                count: words.next()?.parse().ok()?,
                first_ms: words.next()?.parse().ok()?,
                last_ms: words.next()?.parse().ok()?,
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

/// Held while the ledger is read, decided and written, so two tabs of one account make one try.
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
}

impl Gate {
    /// Every condition holds.
    pub(crate) fn open(&self) -> bool {
        self.on && self.claude && self.autopilot != Autopilot::Off && self.claim_credits
    }
}

/// What one look did ([`step`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stepped {
    /// The gate is shut: nothing was looked at.
    Shut,
    /// No offer is live.
    NothingLive,
    /// Every live offer has had its tries.
    NothingDue,
    /// The ledger could not be read or written: nothing was started.
    Unrecorded(String),
    /// These offers were recorded and their workers asked for.
    Started(Vec<Offer>),
}

/// One look of the act: under an open `gate`, the live offers in `profile` (through `cache`
/// and `read`), each due one recorded in the ledger in `ledger` first, then handed to `start`.
pub(crate) fn step(
    gate: Gate,
    profile: &Path,
    cache: &ProfileCache,
    read: &dyn Fn(&Path) -> Result<Vec<Offer>, NoProfile>,
    ledger: &Path,
    now_ms: i64,
    start: &mut dyn FnMut(Offer),
) -> Stepped {
    if !gate.open() {
        return Stepped::Shut;
    }
    let offers = cache.offers(profile, read);
    if offers.is_empty() {
        return Stepped::NothingLive;
    }
    let due = {
        let _held = LEDGER_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let mut book = match read_ledger(ledger) {
            Ok(book) => book,
            Err(why) => return Stepped::Unrecorded(why),
        };
        let due: Vec<Offer> = offers
            .into_iter()
            .filter(|offer| book.due(&offer.key, now_ms))
            .take(LEDGER_ROWS)
            .collect();
        if due.is_empty() {
            return Stepped::NothingDue;
        }
        let keys: Vec<OfferKey> = due.iter().map(|offer| offer.key.clone()).collect();
        book.tried(&keys, now_ms);
        if let Err(why) = write_ledger(ledger, &book) {
            return Stepped::Unrecorded(why);
        }
        due
    };
    for offer in &due {
        start(offer.clone());
    }
    Stepped::Started(due)
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
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir)
}

/// The claim workers this process started, each with when it started, until each ran out of its
/// time ([`WORKER_FOR`]).
#[derive(Default)]
pub(crate) struct Running {
    rows: Mutex<Vec<(String, Instant)>>,
}

impl Running {
    pub(crate) const fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
        }
    }

    /// The claim worker `sid` started at `at`.
    pub(crate) fn started(&self, sid: &str, at: Instant) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.retain(|(s, _)| s != sid);
        rows.push((sid.to_owned(), at));
    }

    /// The claim workers that have run [`WORKER_FOR`] by `now`, taken off the list, so each is
    /// told once; past [`RUNNING_ROWS`] the earliest started are taken too, never dropped
    /// untold. Whether each still runs is the window's to read.
    pub(crate) fn expired(&self, now: Instant) -> Vec<String> {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        rows.sort_by_key(|(_, at)| *at);
        let over = rows.len().saturating_sub(RUNNING_ROWS);
        let mut out = Vec::new();
        let mut k = 0;
        rows.retain(|(sid, at)| {
            let gone = k < over || now.saturating_duration_since(*at) >= WORKER_FOR;
            k += 1;
            if gone {
                out.push(sid.clone());
            }
            !gone
        });
        out
    }
}

/// The claim workers this process started ([`Running`]).
pub(crate) static RUNNING: Running = Running::new();

impl crate::App {
    /// Start a claim worker for `offer`, found by the supervised session `from`
    /// ([`super::HostNote::CreditClaim`]), the way the disk worker starts: a background Claude
    /// Code tab under Autopilot Land, trusted the claim folder ([`worker_folder_under`]), so the
    /// harness fences its boxes. The try is already recorded ([`step`]); the tab it opens, or
    /// why none opened, is a fact for later in the digest, and the tab joins [`RUNNING`].
    pub(crate) fn start_credit_claim(&mut self, from: &str, offer: Offer) {
        let folder = super::harness_state().and_then(|state| worker_folder_under(&state));
        let Some(folder) = folder else {
            let why = "no folder of aterm's own to start it in (no state directory)";
            aterm_log::warn!(
                "harness @{from}: credit claim {}: not started: {why}",
                offer.id.word()
            );
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
                self.except(&crate::message_reporters::credit_claim_not_started_entry(
                    &offer, &why,
                ));
                return;
            }
        };
        let from = from.to_string();
        self.book_spawn(
            plan,
            booking,
            Box::new(move |app, spawned| {
                match spawned {
                    Ok(spawned) => {
                        let sid = app.start_spawned(spawned);
                        RUNNING.started(&sid, Instant::now());
                        aterm_log::info!(
                            "harness @{from}: credit claim {}: worker runs as @{sid}",
                            offer.id.word()
                        );
                        app.except(&crate::message_reporters::credit_claim_started_entry(
                            &offer, &sid,
                        ));
                    }
                    Err(why) => {
                        aterm_log::warn!(
                            "harness @{from}: credit claim {}: not started: {why}",
                            offer.id.word()
                        );
                        app.except(&crate::message_reporters::credit_claim_not_started_entry(
                            &offer, &why,
                        ));
                    }
                }
                app.mark_status_dirty();
            }),
        );
    }

    /// The claim worker `sid` ran out of its time ([`HostNote::CreditClaimExpired`],
    /// [`WORKER_FOR`]): its Autopilot goes Off, written by aterm, so Land types nothing more
    /// into it.
    ///
    /// [`HostNote::CreditClaimExpired`]: super::HostNote::CreditClaimExpired
    pub(crate) fn credit_claim_expired(&mut self, sid: &str) {
        if self.worker_autopilot_off(sid) {
            aterm_log::info!(
                "harness: the credit claim worker @{sid} ran out of its time; its Autopilot is Off"
            );
        } else {
            aterm_log::debug!(
                "harness: the credit claim worker @{sid} ran out of its time and no longer runs \
                 here"
            );
        }
    }
}

#[cfg(test)]
#[path = "harness_credit_claim_tests.rs"]
mod tests;
