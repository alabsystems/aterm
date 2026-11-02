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
//! raised (`--permission-mode default`, [`crate::task_launch::worker_fresh`]) and its task ends in
//! the reply Autopilot Land lands on. A worker that has run [`WORKER_FOR`] since it started or a
//! person last typed in its tab ends, whatever it is doing, while the program in front names
//! itself the agent its record names; anything else running there, a tab on screen in a focused
//! window, and one a hold or a driver has, is left alone ([`crate::App::end_claim_workers`]). Its
//! tab closes [`WORKER_FOR`] after its task is marked `exited`, or after a program someone ran
//! there since was last seen running in front, unless it is its window's last tab, which asks
//! before it goes (a job left in the background or stopped there ends with the tab, as it does at
//! a person's own close, whose guard also asks only for the program in front): a box nobody
//! answered does not stand on, and the ledger says when the offer may be tried again. A seamless
//! update carries the task record that clock reads and that last sighting; any other restore does
//! not, so the layout names each worker's folder (and an agent in a claim folder that is no
//! worker's) and a restore starts a new record for its tab: `working` for a worker it brings back
//! or one in a shell the PTY keeper hands back, `exited` where the worker had ended or is not
//! coming back (a plain quit forgets a running one), so a program found in that tab is not timed
//! as it and the tab still closes ([`note_restored_worker`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::credit_offers::{self, Offer, OfferKey, Stamp};
use aterm_agent::harness::profile::NoProfile;
use aterm_agent::supervise::Autopilot;

use crate::task_record::{TaskRecord, TaskState};

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

/// How long a claim worker may run with nobody typing in its tab, and how long its tab then stays
/// for its last reply to be read ([`crate::App::end_claim_workers`]).
pub(crate) const WORKER_FOR: Duration = Duration::from_secs(30 * 60);

/// How long a claim worker's program has to end after the first `SIGTERM` the look delivered to
/// it ([`note_term`]): one still running then gets `SIGKILL`, a program that ignores the signal or
/// stopped reading its terminal.
pub(crate) const DEAF_FOR: Duration = Duration::from_secs(2 * 60);

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
    #[cfg(unix)]
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// How long the task `record` has stood by `now`, `typed` after a person last typed in its tab:
/// from its start while its program runs and from its exit once that ended. Monotonic time, so a
/// Mac asleep spends none of it.
pub(crate) fn stood(record: &TaskRecord, now: Instant, typed: Option<Duration>) -> Duration {
    let from = if record.state == TaskState::Exited {
        record.since
    } else {
        record.started
    };
    let stood = now.saturating_duration_since(from);
    typed.map_or(stood, |typed| stood.min(typed))
}

/// Whether a task that trusted `folder` and has `stood` that long is a claim worker's that ran out
/// of its time. All of it is the task record's, which a seamless update carries, so a worker an
/// earlier process started is judged as this one's is.
pub(crate) fn ran_out(folder: Option<&str>, stood: Duration) -> bool {
    stood >= WORKER_FOR
        && folder.is_some_and(|folder| credit_offers::is_claim_folder(Path::new(folder)))
}

/// What a look does with a claim worker that ran out of its time ([`ending`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Leave it for a later look.
    Leave,
    /// Ask its program to end.
    Term,
    /// End a program that did not.
    Kill,
    /// Mark its task `exited`, its program gone, which starts the time its tab stays open to be
    /// read ([`stood`]).
    MarkExited,
    /// Close its pane.
    ClosePane,
}

/// What a look reads of a claim worker that ran out of its time ([`ending`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stand {
    /// Its tab is on screen in a focused window, or a hold or a driver has it.
    pub(crate) in_hand: bool,
    /// Its task is marked `exited`, so a job in its tab now is not the worker's.
    pub(crate) exited: bool,
    /// A foreground job runs in its tab.
    pub(crate) job: bool,
    /// The program in front names itself the agent the worker's record names (its own argv, as
    /// a native, atpkg or `versions/<n>` install of Claude Code does), so a job there is the
    /// worker's own; any other program is someone else's, a runtime such as `node` among them,
    /// whatever its screen shows (an exited worker's last frame can stand on it).
    pub(crate) worker: bool,
    /// Closing its pane would close its window.
    pub(crate) ends_window: bool,
    /// A seamless update is under way, which a close would call off.
    pub(crate) updating: bool,
    /// The program in front, its process group, has had [`DEAF_FOR`] to end since the first
    /// `SIGTERM` delivered to it ([`deaf`]).
    pub(crate) deaf: bool,
    /// A program ran in the tab since its task was marked `exited` (someone's, never the
    /// worker's), seen by a look less than [`WORKER_FOR`] ago, so its output is still to be read.
    pub(crate) reading: bool,
}

/// What to do with a worker that ran out of its time: nothing to a tab in someone's hand or to a
/// job that is not the worker's (its task already `exited`, or another program in front); its
/// program first, which leaves a pane that closes with no confirmation to raise; then its task
/// marked `exited`, unless something already marked it, so
/// its tab stays open [`WORKER_FOR`] more for its last reply to be read, whatever ended its
/// program, and as long again after a program someone ran there since; and its pane only where
/// that leaves its window standing, since a window's last tab asks before it goes. A seamless
/// update under way gets neither a mark, which would wait on its fence, nor a close, which would
/// call it off.
pub(crate) const fn ending(stand: Stand) -> Ending {
    if stand.in_hand || (stand.job && (stand.exited || !stand.worker)) {
        Ending::Leave
    } else if stand.job {
        if stand.deaf {
            Ending::Kill
        } else {
            Ending::Term
        }
    } else if stand.updating {
        Ending::Leave
    } else if !stand.exited {
        Ending::MarkExited
    } else if stand.ends_window || stand.reading {
        Ending::Leave
    } else {
        Ending::ClosePane
    }
}

impl crate::App {
    /// Start a claim worker for `offer`, found by the supervised session `from`
    /// ([`super::HostNote::CreditClaim`]), the way the disk worker starts: a background Claude
    /// Code tab under Autopilot Land, trusted the claim folder ([`worker_folder_under`]), so the
    /// harness fences its boxes. The try is already recorded ([`step`]); the tab it opens, or
    /// why none opened, is a fact for later in the digest.
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

    /// The disk watch's look ([`HostNote::DiskLooked`], on a timer of its own, whatever any
    /// session does): every live claim worker that ran out of its time ([`ran_out`]) is moved on
    /// by one step of [`ending`], so its program ends at one look, its task is marked `exited` at
    /// a later one, and its pane closes once that mark has stood [`WORKER_FOR`]. Nothing here
    /// raises a confirmation: a pane closes only with no job in it and with its window left
    /// standing. Returns each such worker's session and the step taken.
    ///
    /// [`HostNote::DiskLooked`]: super::HostNote::DiskLooked
    pub(crate) fn end_claim_workers(&mut self, now: Instant) -> Vec<(u64, Ending)> {
        let handles: Vec<_> = {
            let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
            store.live_handles().cloned().collect()
        };
        let now_us = crate::metrics::now_us();
        let mut steps = Vec::new();
        let mut ended = Vec::new();
        for handle in handles {
            // A program running in the tab of a worker that ended is someone's: seen at every
            // look, not only once the tab's time is out, so its output keeps the tab open its
            // reading time after it ends ([`Stand::reading`]).
            if claim_worker(&handle.ctx).is_some_and(|(_, done)| done) {
                ended.push(handle.local_id);
                if self.session_foreground_busy(handle.local_id) {
                    self.claim_job_seen.insert(handle.local_id, now);
                }
            }
            // Typing alone, in awake time as `now` is, a seamless update's carried age too: a
            // click or a scroll in the tab is no person taking it over.
            let typed = handle.ctx.human_input.typed_awake_ms(now_us);
            let over = {
                let timeline = handle
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                timeline.task().and_then(|task| {
                    let stood = stood(task, now, typed.map(Duration::from_millis));
                    ran_out(task.trusted_cwd.as_deref(), stood).then(|| Look {
                        worker: task.agent.is_some_and(|agent| {
                            timeline
                                .agent()
                                .program
                                .as_deref()
                                .and_then(aterm_phase::program_of)
                                == Some(agent)
                        }),
                        record: task.clone(),
                    })
                })
            };
            if let Some(look) = over {
                steps.push((handle.local_id, self.end_claim_worker(&handle, &look, now)));
            }
        }
        // A worker typed back into its time, or gone, owes no signal: its next run out starts
        // with `SIGTERM` again. (One marked `exited` is never signalled.)
        self.claim_termed
            .retain(|session, _| steps.iter().any(|(over, _)| over == session));
        self.claim_job_seen
            .retain(|session, _| ended.contains(session));
        steps
    }

    /// One step of [`ending`] at `now` for the claim worker `handle`, read as `look`, with its
    /// Autopilot Off first: Land would otherwise keep typing in a tab a person left in front, or
    /// bring its program back.
    fn end_claim_worker(
        &mut self,
        handle: &crate::session_store::SessionHandle,
        look: &Look,
        now: Instant,
    ) -> Ending {
        let (session, sid) = (handle.local_id, handle.sid.as_str());
        let autopilot = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .autopilot_word();
        if autopilot != Some(Autopilot::Off) {
            self.worker_autopilot_off(sid);
        }
        let looked_at = self.windows.iter().any(|(wid, ws)| {
            crate::os_window_focused(ws) && self.visible_session_rect(*wid, session).is_some()
        });
        let driven = crate::input_stall::other_hand(&handle.ctx, crate::metrics::now_us());
        let step = ending(Stand {
            in_hand: looked_at || driven.is_some(),
            exited: look.record.state == TaskState::Exited,
            job: self.session_foreground_busy(session),
            worker: look.worker,
            ends_window: self.close_ends_window(session),
            updating: self.update_handoff_in_flight(),
            deaf: deaf(
                self.claim_termed.get(&session),
                look.record.started,
                crate::quit_safety::foreground_pgrp(handle.master),
                now,
            ),
            reading: look.record.state == TaskState::Exited
                && self
                    .claim_job_seen
                    .get(&session)
                    .is_some_and(|&seen| now.saturating_duration_since(seen) < WORKER_FOR),
        });
        match step {
            Ending::Leave => {}
            Ending::Term | Ending::Kill => {
                let word = if step == Ending::Kill { "kill" } else { "term" };
                let said = crate::control::control_input::signal_verb(
                    handle.master,
                    word,
                    &handle.ctx,
                    aterm_slog::Actor::Harness,
                );
                if step == Ending::Term {
                    note_term(
                        &mut self.claim_termed,
                        session,
                        look.record.started,
                        now,
                        &said,
                    );
                }
                aterm_log::info!(
                    "harness: the credit claim worker @{sid} ran out of its time; `signal {word}` \
                     to its program: {}",
                    said.trim_end()
                );
            }
            Ending::MarkExited => {
                let marked = mark_ended(&handle.ctx, session, &look.record);
                if let Ok(Some(marked)) = &marked
                    && marked.attention_moved
                {
                    crate::presence::post_meta_changed(session);
                }
                aterm_log::info!(
                    "harness: the credit claim worker @{sid} ran out of its time; its program \
                     ended, its task {}",
                    match marked {
                        Ok(Some(_)) => "marked exited: its tab closes after its reading time",
                        Ok(None) => "moved since the look: a later look marks it",
                        Err(crate::task_record::MarkFenced) => "waits for a later look to mark",
                    }
                );
            }
            Ending::ClosePane => {
                let _closing = crate::session_store::CloseAttribution::enter(
                    crate::session_store::ExitReason::CtlClose,
                    crate::session_store::ExitActor::Harness,
                );
                match self.close_session_by_id(session) {
                    Ok(_) => aterm_log::info!(
                        "harness: the credit claim worker @{sid} ran out of its time; its tab \
                         closed"
                    ),
                    Err(why) => aterm_log::info!(
                        "harness: the credit claim worker @{sid} ran out of its time; its tab \
                         stays for now: {why}"
                    ),
                }
            }
        }
        step
    }
}

/// What a look read of a claim worker that ran out of its time, under its timeline's lock.
pub(crate) struct Look {
    /// Its task record as the look read it.
    pub(crate) record: TaskRecord,
    /// The program in front names itself the agent its record names ([`Stand::worker`]).
    pub(crate) worker: bool,
}

/// The first `SIGTERM` the look delivered to a claim worker's program ([`note_term`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Termed {
    /// The start of the task record it was sent for.
    pub(crate) record: Instant,
    /// The process group it reached, the tab's foreground group then.
    pub(crate) group: i32,
    /// When.
    pub(crate) at: Instant,
}

/// Each claim worker's [`Termed`], by session ([`crate::App::end_claim_workers`]).
pub(crate) type TermedBySession = std::collections::HashMap<u64, Termed>;

/// Whether the program in front of a claim worker's tab, foreground process group `group` now,
/// for the task record that started at `started`, has had [`DEAF_FOR`] by `now` to end since the
/// first `SIGTERM` delivered to it (`sent`, [`note_term`]). One never sent one, sent one for an
/// earlier record, or another program that came to the front since, is not deaf, so `SIGKILL`
/// is never a program's first signal.
pub(crate) fn deaf(sent: Option<&Termed>, started: Instant, group: i32, now: Instant) -> bool {
    sent.is_some_and(|sent| {
        sent.record == started
            && group > 0
            && sent.group == group
            && now.saturating_duration_since(sent.at) >= DEAF_FOR
    })
}

/// Records in `termed` that session `session`'s claim worker, of the task record that started at
/// `started`, was sent `SIGTERM` at `now`, where `said` (the `signal` verb's reply) says which
/// process group it reached (`OK signalled pgrp <n>`): the first one delivered to that group for
/// that record stands, so [`DEAF_FOR`] counts from it. A reply that refused (`ERR …`, such as no
/// foreground process group) records nothing.
pub(crate) fn note_term(
    termed: &mut TermedBySession,
    session: u64,
    started: Instant,
    now: Instant,
    said: &str,
) {
    let Some(group) = said
        .strip_prefix("OK signalled pgrp ")
        .and_then(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<i32>()
                .ok()
        })
        .filter(|group| *group > 0)
    else {
        return;
    };
    let first = Termed {
        record: started,
        group,
        at: now,
    };
    let stands = termed
        .get(&session)
        .is_some_and(|sent| sent.record == started && sent.group == group);
    if !stands {
        termed.insert(session, first);
    }
}

/// Marks session `local`'s task `exited`, its claim worker's program gone, if its record is still
/// `read`: as the harness's own exit mark does (`aterm_agent::supervise::mark_task_exited`), a
/// landed task keeps its result, and the mark takes down the attention a deadline raised for the
/// worker (the held or startup ask), which a program that ended no longer owes. It waits on no
/// update's fence (`Err` until a later look). `Ok(None)` when the record moved since it was read.
fn mark_ended(
    ctx: &crate::SessionCtx,
    local: u64,
    read: &TaskRecord,
) -> Result<Option<crate::task_record::MarkedWith>, crate::task_record::MarkFenced> {
    let mut mark = crate::task_record::TaskMark::new(TaskState::Exited);
    mark.why = Some("exit");
    if read.state == TaskState::Done && read.result_path.is_some() {
        mark.result_bytes = read.result_bytes;
        mark.result_path.clone_from(&read.result_path);
    }
    crate::task_record::mark_session_task_with(
        ctx,
        local,
        Some(&mark),
        crate::task_record::AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands),
        Duration::ZERO,
        |record, _| {
            record.is_some_and(|record| record.rev == read.rev && record.state != TaskState::Exited)
        },
    )
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
/// ([`crate::App::end_claim_workers`]). A new record is marked where none stands, so its time
/// counts from here; one already standing keeps its time and is named the worker. The new record
/// is `working why=restored`, which no startup or held deadline reads as a worker just spawned,
/// or, for a tab whose worker is not coming back, `exited why=exit`: a program found or brought
/// back in that tab is then not the worker's, and the tab closes once that mark has stood
/// [`WORKER_FOR`] and that program, if any, was last seen [`WORKER_FOR`] ago ([`Stand::reading`]).
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
