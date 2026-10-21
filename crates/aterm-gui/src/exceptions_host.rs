// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S HALF OF THE EXCEPTIONS DIGEST (owner rule, 2026-09-28: *"a single
//! batched 'exceptions' [surface] that I can dismiss and they go to some log for
//! me to look at later"*; `aterm_messages::exceptions`).
//!
//! A reporter states an [`Entry`] and never touches the class ceremony. A
//! family has ONE producer and ONE mode ([`aterm_messages::exceptions::Family::mode`]):
//! a LEVEL family (the upgrade's rows, the resume loop's exceptions) gives its
//! WHOLE answer with [`App::except_replace`] at every look it takes (a fact it
//! stops naming ends ten minutes later, at a look or at the digest's own clock
//! — the upgrade's looks are sent only when its rows change, so "a second look"
//! is not something it can promise); an EVENT family (a macOS prompt observed,
//! a verified grant, the state folder the upgrade cannot use) uses
//! [`App::except`] and [`App::except_clear`] and is never swept. The host
//! refuses the wrong call, so a level pass cannot end an event-raised
//! sibling. The owner's `Dismiss all` is
//! [`App::dismiss_exceptions`]: every open entry goes to the log at one
//! stamp, and the same facts stay quiet for their span across restarts and
//! updates, because the LOG is their memory.
//!
//! The digest is a store beside the message center's live set, so nothing here
//! can ask the band for a row: it costs the terminal nothing and re-grids no
//! window. What it does move is the digest's own revision, which republishes
//! Settings ▸ Messages and rebuilds the menu-bar item.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use aterm_agent::supervise::policy::exception::Urgency;
use aterm_agent::supervise::{StallSeen, StuckSurvey};
use aterm_core::terminal::repaint_nudge::GiveUp;
use aterm_messages::exceptions::{Entry, Family, Mode};
use aterm_messages::need::Horizon;
use aterm_messages::{Message, PostOutcome};

use crate::App;
use crate::app_nudge::ADOPTION_EPOCH;
use crate::harness_host::{AccountsSaid, HostNote, RelaunchSay};
use crate::message_reporters;
use crate::messages_host::wall_stamp_now;

/// How long a rating survey the loop cannot close stands before it earns a line
/// of the digest: an hour (the same named horizon as the supervisor's, since the
/// card blocks nothing the worker does).
const SURVEY_ENTRY_AFTER: Duration = Horizon::SUPERVISOR_RESTARTS.duration();

#[cfg(test)]
thread_local! {
    /// How often the digest announced a change (the menu-bar item and Settings
    /// follow): a test seam, since a headless App has neither.
    static DIGEST_MOVES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// An entry that is not yet worth the owner's look: a fact that mends itself
/// (a supervisor that keeps stopping, a rating card, updates that fail) is a
/// RECORD until it has outlasted its horizon ([`App::except_after`]), and only
/// then a line of the digest.
#[derive(Debug)]
pub(crate) struct Ripening {
    key: String,
    due: Instant,
    entry: Entry,
}

/// How long a restarted supervisor must have run without failing again before
/// the failure is taken to be over ([`App::settle_supervisors`]): its pause and
/// this. A crash loop whose runs live longer than that is still a failure, and
/// says so at every failure it makes.
const SUPERVISOR_STEADY: Duration = Duration::from_secs(30 * 60);

/// How long after this process first looked at its exceptions the resume family
/// is handed its whole answer once, whatever the loops have told: the lines an
/// earlier process left open end with it if no loop of this one names them. Ten
/// minutes is long past the first point every loop reaches.
pub(crate) const RESUME_SETTLE: Duration = Duration::from_secs(10 * 60);

/// The `host_noted` marks that THIS process recorded a short volume, one for
/// each form of its line: the passes' ([`HostNote::DiskLow`]) and the one
/// opened while cleanup is off by fault ([`HostNote::DiskLowCleanupOff`]).
/// Each form is recorded once while its mark stands; the end of the form's
/// premise lifts it.
const DISK_NOTED: &str = "disk";
const DISK_CLEANUP_OFF_NOTED: &str = "disk:cleanup-off";

/// The `host_noted` marks of the usage tap's line ([`HostNote::UsageTap`]):
/// one per tab whose tap stands silent, and the day it was last raised.
const USAGE_TAP_NOTED: &str = "usage-tap:";
const USAGE_TAP_DAY: &str = "usage-tap-day:";

/// The instant this process first looked at its exceptions, and whether it has
/// since ended the supervisor lines an earlier process left open that nothing
/// has told of ([`App::sweep_adopted_supervisor_lines`]): once per process, at the
/// first sweep after the steady time (no wake of its own: the window sweeps at
/// every event).
static LOOKING_SINCE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static ADOPTED_SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One tab's supervisor that has failed past its restart budget and not yet
/// been seen steady ([`HostNote::SupervisorFailing`]).
#[derive(Debug)]
pub(crate) struct SupervisorFail {
    /// The first failure of this episode: the hour counts from here.
    pub(crate) first: Instant,
    /// The latest, and the pause the host waits before it starts the supervisor
    /// again.
    pub(crate) last: Instant,
    pub(crate) pause: Duration,
    pub(crate) why: String,
    /// The digest line is up.
    pub(crate) raised: bool,
}

/// A tab's login notices as ONE story (`App::raise_resume_ask`): the row or
/// record the story is told in now, which the loss's next notice replaces and
/// the login back closes fixed (`App::close_notice_story`), and the words it
/// is closed in (its notice's declared fix words; `None` for an ask that
/// declared none).
#[derive(Debug)]
pub(crate) struct NoticeStory {
    head: aterm_messages::MessageId,
    back: Option<String>,
    /// The wall clock (unix ms) of the story's last word: its latest notice
    /// told, or its ask's end. A story whose row has left the glass goes on
    /// for [`NOTICE_STORY_SPAN`] from here ([`Self::goes_on`]).
    pub(crate) last: u64,
    /// The notices of this story the owner read (folded or dismissed) whose
    /// rows a later notice replaced, by key, at the wall clock of the read.
    /// The replaced record no longer keeps its key quiet in the center
    /// ([`aterm_messages::MessageCenter::supersede_record`] retires it
    /// `Superseded`), so the story does: a notice the owner read is not raised
    /// again while its story stands, inside the standing quiet of its read.
    read: std::collections::BTreeMap<String, u64>,
}

/// How long a tab's login story waits for its next notice once its row has
/// left the glass and no login back was told: the standing quiet of a notice
/// the owner read ([`aterm_messages::need::STANDING_QUIET`]). A loss whose end
/// no loop told (its loop ended, its tab moved between them) is over by then,
/// and the next notice is a loss of its own: its own row, never one that
/// swallows the last.
pub(crate) const NOTICE_STORY_SPAN: Duration = aterm_messages::need::STANDING_QUIET;

impl NoticeStory {
    /// Whether the story goes on at wall `unix_ms`: its row still on the
    /// glass (`live`), or its last word inside [`NOTICE_STORY_SPAN`].
    fn goes_on(&self, live: bool, unix_ms: u64) -> bool {
        live || unix_ms < self.last.saturating_add(span_ms(NOTICE_STORY_SPAN))
    }

    /// Whether `key` is a notice the owner read in this story, still quiet at
    /// wall `unix_ms` ([`aterm_messages::need::STANDING_QUIET`] from its read).
    fn keeps_quiet(&self, key: &str, unix_ms: u64) -> bool {
        self.read.get(key).is_some_and(|&read| {
            unix_ms < read.saturating_add(span_ms(aterm_messages::need::STANDING_QUIET))
        })
    }
}

/// `span` in whole milliseconds, saturating.
fn span_ms(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

impl SupervisorFail {
    /// The instant a run started after the latest failure has outlived it
    /// without failing again.
    fn steady_at(&self) -> Instant {
        self.last + self.pause + SUPERVISOR_STEADY
    }
}

impl App {
    /// A condition that needs the owner's look, not now, holds. Idempotent: the
    /// same words are a duplicate, new words restate the entry in place, and a
    /// fact the owner filed inside its span is quiet.
    pub(crate) fn except(&mut self, entry: &Entry) {
        if entry.family().mode() != Mode::Event {
            aterm_log::error!(
                "exceptions: {} is a LEVEL family; it speaks with except_replace, not by event",
                entry.key()
            );
            return;
        }
        let Some(msg) = self.checked_entry(entry) else {
            return;
        };
        let posted = self.messages.post(msg, wall_stamp_now(), Instant::now());
        match posted.outcome {
            PostOutcome::New => {
                aterm_log::info!("exceptions: {:?} {}", posted.outcome, entry.key());
                if let Some(line) = entry.log_line_text() {
                    aterm_log::info!("{line}");
                }
                self.digest_moved();
            }
            // Words moving in place: not news for `aterm.log` (as a pass's).
            PostOutcome::Restated => {
                aterm_log::debug!("exceptions: {:?} {}", posted.outcome, entry.key());
                self.digest_moved();
            }
            PostOutcome::Duplicate | PostOutcome::Suppressed | PostOutcome::Superseded(_) => {}
        }
    }

    /// A fact that heals itself, said as an entry only once it has outlasted
    /// `after` (the engine's named horizons: 24 h of failing updates, an hour of
    /// a supervisor stopping): until then the reporter has written its record,
    /// and nothing here is shown. The FIRST claim fixes the instant; the same key
    /// again only re-words the entry it will raise, so a fact that is re-asserted
    /// every look never postpones itself. [`App::except_clear`] withdraws the
    /// claim (or ends the entry, once raised). An event family's call, like
    /// [`App::except`].
    pub(crate) fn except_after(&mut self, entry: &Entry, after: Duration) {
        if after.is_zero() {
            self.except(entry);
            return;
        }
        if entry.family().mode() != Mode::Event {
            aterm_log::error!(
                "exceptions: {} is a LEVEL family; it speaks with except_replace, not by event",
                entry.key()
            );
            return;
        }
        let key = entry.key();
        match self.ripening.iter_mut().find(|r| r.key == key) {
            Some(claim) => claim.entry = entry.clone(),
            None => self.ripening.push(Ripening {
                key,
                due: Instant::now() + after,
                entry: entry.clone(),
            }),
        }
    }

    /// Raise the entries whose horizon has come (`settle_messages` calls it on
    /// every sweep, and `messages_deadline` names the instant, so a quiet
    /// window still wakes for it).
    pub(crate) fn ripen_exceptions(&mut self, now: Instant) {
        self.settle_supervisors(now);
        self.end_relaunch_lines_of_closed_tabs();
        let since = *LOOKING_SINCE.get_or_init(|| now);
        if now >= since + SUPERVISOR_STEADY && !ADOPTED_SWEPT.swap(true, Ordering::Relaxed) {
            self.sweep_adopted_supervisor_lines();
        }
        // The resume family is a level family the loops speak for at every
        // change: lines an earlier process left open, which no loop of this one
        // tells of, end with the first whole answer after they have had time to.
        if !self.resume_swept {
            // The API outage is a level family too, stated only as tabs
            // enter and leave the wait: an entry an earlier process left open
            // would stand for the 14-day expiry where no tab here ever waits.
            // Its whole answer is given at the first look, and again at the
            // settle (a seamless successor adopts its predecessor's entries at
            // Commit, which can come after the first look).
            if self.resume_looked.is_none() {
                self.hand_api_outage();
            }
            let first = *self.resume_looked.get_or_insert(now);
            if now >= first + RESUME_SETTLE {
                self.resume_swept = true;
                self.hand_resume_exceptions();
                self.sweep_resume_asks();
                self.hand_api_outage();
                self.restate_long_holds();
            }
        }
        // The person's long holds: a level answer, stated at the first look
        // and the settle like the outage's, else only where it moved
        // (`App::hand_long_holds`).
        self.hand_long_holds();
        if self.ripening.iter().all(|claim| claim.due > now) {
            return;
        }
        let (ripe, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.ripening)
            .into_iter()
            .partition(|claim| claim.due <= now);
        self.ripening = waiting;
        for claim in ripe {
            self.except(&claim.entry);
        }
    }

    /// The instant the next claim ripens, or the next supervisor turns out
    /// steady or has failed for the hour, if one is held.
    pub(crate) fn ripening_deadline(&self) -> Option<Instant> {
        let supervisors = self.supervisor_failing.values().flat_map(|fail| {
            let horizon =
                (!fail.raised).then(|| fail.first + Horizon::SUPERVISOR_RESTARTS.duration());
            horizon.into_iter().chain([fail.steady_at()])
        });
        self.ripening
            .iter()
            .map(|claim| claim.due)
            .chain(supervisors)
            .chain(self.long_hold_deadline())
            .min()
    }

    /// A supervisor that failed past its budget is over the moment a run
    /// started after its latest failure has outlived it ([`SUPERVISOR_STEADY`]
    /// past the pause): its line is withdrawn, or never shown. One that is still
    /// failing when an hour of it has gone is one line of the digest
    /// (`harness.supervisor.<tab>.failing`).
    fn settle_supervisors(&mut self, now: Instant) {
        if self.supervisor_failing.is_empty() {
            return;
        }
        let mut steady: Vec<String> = Vec::new();
        let mut due: Vec<String> = Vec::new();
        for (sid, fail) in &self.supervisor_failing {
            if now >= fail.steady_at() {
                steady.push(sid.clone());
            } else if !fail.raised && now >= fail.first + Horizon::SUPERVISOR_RESTARTS.duration() {
                due.push(sid.clone());
            }
        }
        for sid in steady {
            self.note_harness_host(HostNote::SupervisorSteady { sid });
        }
        for sid in due {
            let spot = self.upgrade_spot(&sid);
            let Some(fail) = self.supervisor_failing.get_mut(&sid) else {
                continue;
            };
            fail.raised = true;
            let entry = message_reporters::supervisor_failing_entry(&sid, &spot, &fail.why);
            self.except(&entry);
        }
    }

    /// Whether a line under exactly `key` is open, or a claim to raise one is
    /// pending (test-only).
    #[cfg(test)]
    pub(crate) fn owes_exception(&self, key: &str) -> bool {
        self.ripening.iter().any(|claim| claim.key == key)
            || self
                .messages
                .digest_keys_under(key)
                .iter()
                .any(|k| k == key)
    }

    /// The line open under exactly `key` ends now, and a claim not yet ripe is
    /// withdrawn; `true` when a line ended. NOTHING THE OWNER FILED IS
    /// FORGOTTEN: this is for a reporter that can no longer say its fact, or
    /// says it anew of another subject, which is not the cause ending
    /// ([`Self::except_clear`] is that).
    pub(crate) fn except_end(&mut self, key: &str) -> bool {
        self.ripening.retain(|claim| claim.key != key);
        self.entries_refused.remove(key);
        let ended = self.messages.clear_digest(key, Instant::now());
        if ended {
            aterm_log::info!("exceptions: cleared {key}");
            self.digest_moved();
        }
        ended
    }

    /// The condition ended and its reporter says so (a verified grant, a switch
    /// turned off): the entry under exactly `key` ends now, and a claim not yet
    /// ripe is withdrawn.
    pub(crate) fn except_clear(&mut self, key: &str) {
        if self.except_end(key) {
            return;
        }
        if self
            .messages
            .forget_filing(key, wall_stamp_now(), Instant::now())
        {
            // Nothing was open, but the owner had put the fact away: its cause
            // has now ended, so the next time it returns it is news.
            aterm_log::info!("exceptions: {key} filed earlier; its cause has ended");
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_UI_WAIT);
        }
    }

    /// A config load's word on the settings removed features left behind
    /// ([`crate::message_reporters::REMOVED_FEATURES`]): each feature `text`
    /// still configures is an entry naming what replaced it and what the
    /// feature left on this machine, read off its table, its navigation
    /// opening the file where the feature's table begins, and one it no longer
    /// configures ends. The launch and every reload call it; a `text` that does
    /// not parse changes nothing (its parse error is told on its own).
    pub(crate) fn note_removed_settings(&mut self, text: &str) {
        use crate::message_reporters::{
            REMOVED_FEATURES, removed_setting_entry, removed_setting_key,
        };
        let Ok(document) = text.parse::<aterm_toml::edit::DocumentMut>() else {
            return;
        };
        let values = aterm_toml::from_str::<aterm_toml::Value>(text).ok();
        for feature in REMOVED_FEATURES {
            if document.get(feature.table).is_some() {
                let line = table_line(text, feature.table);
                let left = feature
                    .left
                    .zip(values.as_ref().and_then(|v| v.get(feature.table)))
                    .map(|(left, table)| left(table))
                    .unwrap_or_default();
                self.except(&removed_setting_entry(feature, line, &left));
            } else {
                self.except_clear(&removed_setting_key(feature));
            }
        }
    }

    /// A reporter's whole current answer for `family`: what still holds is
    /// posted or kept, and what the pass no longer names ends once it has gone
    /// unnamed for ten minutes (the digest's own clock, armed by
    /// `messages_deadline`, ends it if no later look comes). AN ENTRY WHOSE
    /// WORDS THE DOOR REFUSES IS STILL NAMED ([`Self::checked_entry`]): the
    /// fact holds, so an open entry under its key stands in the words it was
    /// last shown in, and its clock does not start — a wording fault never
    /// ends a fact (2026-10-08: the upgrade's own-work line, refused at every
    /// look, would have been filed `Resolved` while its cause stood).
    pub(crate) fn except_replace(&mut self, family: Family, entries: &[Entry]) {
        if family.mode() != Mode::Level || entries.iter().any(|e| e.family() != family) {
            aterm_log::error!(
                "exceptions: {family:?} is an EVENT family (or an entry names another); a pass \
                 would sweep entries it does not own"
            );
            return;
        }
        let prefix = family.prefix();
        let named: Vec<String> = entries.iter().map(Entry::key).collect();
        // A refusal this pass no longer names is over: said again if it returns.
        self.entries_refused
            .retain(|key, _| !key.starts_with(&prefix) || named.contains(key));
        let mut msgs: Vec<Message> = Vec::with_capacity(entries.len());
        let mut refused: Vec<String> = Vec::new();
        for (entry, key) in entries.iter().zip(named) {
            match self.checked_entry(entry) {
                Some(msg) => msgs.push(msg),
                None => refused.push(key),
            }
        }
        let sync = self.messages.sync_digest_keeping(
            &prefix,
            msgs,
            &refused,
            wall_stamp_now(),
            Instant::now(),
        );
        if sync.posted + sync.restated + sync.resolved > 0 {
            // A restatement alone is words moving in place (a count, an age),
            // at most once per `RESTATE_QUIET`: not news for `aterm.log`.
            if sync.posted + sync.resolved > 0 {
                aterm_log::info!("exceptions: {family:?} {sync:?}");
            } else {
                aterm_log::debug!("exceptions: {family:?} {sync:?}");
            }
            for entry in entries
                .iter()
                .filter(|e| sync.posted_keys.contains(&e.key()))
            {
                if let Some(line) = entry.log_line_text() {
                    aterm_log::info!("{line}");
                }
            }
            self.digest_moved();
        }
    }

    /// WHAT A HOST THREAD TELLS THE WINDOW ABOUT ITSELF (`Wake::HostNote`,
    /// [`HostNote`]): a fact that begins is a RECORD (the log the owner reads
    /// later) and a claim on the digest that only ripens if it goes on — the
    /// supervisor that keeps stopping and the survey the loop cannot close, an
    /// hour; or, when the reporter's own horizon has already passed, an entry at
    /// once (the volume aterm could not help through two passes, a `[disk]`
    /// setting aterm cannot read that turned its cleanup off and the volume
    /// short through two looks while it holds, the upgrade
    /// whose state folder cannot hold its lock, the Claude accounts that are
    /// signed out or whose roster is refused). A fact that ends withdraws the
    /// claim, or ends the entry. Never a band row, a tab mark or a notification.
    /// Every `Ended` is idempotent and UNCONDITIONAL: it ends the line whether or not
    /// this process raised it, because the log keeps an event family's open entries
    /// across a restart and only the note that its cause is over can end them.
    pub(crate) fn note_harness_host(&mut self, note: HostNote) {
        match note {
            HostNote::SupervisorFailing { sid, why, pause } => {
                let now = Instant::now();
                if let Some(fail) = self.supervisor_failing.get_mut(&sid) {
                    fail.last = now;
                    fail.pause = pause;
                    fail.why = why;
                } else {
                    // The episode begins: a record, once. The digest waits for
                    // the hour ([`Self::settle_supervisors`]).
                    let spot = self.upgrade_spot(&sid);
                    aterm_log::warn!("harness @{sid}: the supervisor keeps failing: {why}");
                    self.record_message(message_reporters::supervisor_failing_record(
                        &sid,
                        &spot.place,
                        &why,
                    ));
                    self.supervisor_failing.insert(
                        sid,
                        SupervisorFail {
                            first: now,
                            last: now,
                            pause,
                            why,
                            raised: false,
                        },
                    );
                }
            }
            HostNote::SupervisorSteady { sid } => {
                if self.supervisor_failing.remove(&sid).is_some() {
                    aterm_log::info!("harness @{sid}: the supervisor runs steadily again");
                }
                // Unconditional: the line may be one an earlier process raised
                // and this window adopted, which only this note can end.
                self.except_clear(&message_reporters::supervisor_failing_key(&sid));
            }
            HostNote::SurveyStuck { sid, kind, why } => {
                let spot = self.upgrade_spot(&sid);
                if self.host_noted.insert(format!("survey:{sid}:{kind:?}")) {
                    aterm_log::warn!("harness @{sid}: {why}");
                    self.record_message(message_reporters::survey_record(
                        kind,
                        &sid,
                        &spot.place,
                        &why,
                    ));
                }
                // Only a card the harness will not press ripens into a line; a
                // survey the loop is still trying to close stays a record.
                if kind == StuckSurvey::Withheld {
                    self.except_after(
                        &message_reporters::survey_entry(&sid, &spot, &why),
                        SURVEY_ENTRY_AFTER,
                    );
                }
            }
            HostNote::SurveyEnded { sid } => {
                let prefix = format!("survey:{sid}:");
                self.host_noted.retain(|key| !key.starts_with(&prefix));
                self.except_clear(&message_reporters::survey_key(&sid));
            }
            // A short volume is one low-disk line, in the form of the run that
            // opened it: a line of the other form gives way to it. A pass ran,
            // so the cleanup-off line's cause (cleanup off) has ended and is
            // cleared; the passes' line giving way to the cleanup-off form is
            // only ended, since the volume is still short and what the owner
            // filed of it holds until space is back.
            HostNote::DiskLow {
                free_gib,
                floor_gib,
                hogs,
            } => {
                if self.host_noted.insert(DISK_NOTED.to_string()) {
                    self.record_message(message_reporters::disk_low_record(
                        free_gib, floor_gib, &hogs,
                    ));
                }
                self.except_clear(message_reporters::DISK_LOW_CLEANUP_OFF_KEY);
                self.except(&message_reporters::disk_low_entry(
                    free_gib, floor_gib, &hogs,
                ));
            }
            HostNote::DiskLowCleanupOff {
                free_gib,
                floor_gib,
            } => {
                if self.host_noted.insert(DISK_CLEANUP_OFF_NOTED.to_string()) {
                    self.record_message(message_reporters::disk_low_cleanup_off_record(
                        free_gib, floor_gib,
                    ));
                }
                self.except_end(message_reporters::DISK_LOW_KEY);
                self.except(&message_reporters::disk_low_cleanup_off_entry(
                    free_gib, floor_gib,
                ));
            }
            HostNote::DiskPlenty => {
                // Both, whichever this process held: `|` is not `||`.
                if self.host_noted.remove(DISK_NOTED)
                    | self.host_noted.remove(DISK_CLEANUP_OFF_NOTED)
                {
                    aterm_log::info!("harness disk watch: space is back above the floor");
                }
                self.except_clear(message_reporters::DISK_LOW_KEY);
                self.except_clear(message_reporters::DISK_LOW_CLEANUP_OFF_KEY);
            }
            // A setting only the owner can say the meaning of: a line at once,
            // restated in place as the reading's notes change, and ended by
            // the first look that reads the settings without that fault —
            // with the low-disk line that fault opened, whose premise (aterm
            // frees nothing) ends with it. A volume still short is the passes'
            // to tell from here.
            HostNote::DiskSettingsOff { notes } => {
                self.except(&message_reporters::disk_settings_entry(&notes));
            }
            HostNote::DiskFreed { bytes, went } => {
                self.except(&message_reporters::disk_freed_entry(bytes, &went));
            }
            HostNote::DiskFull {
                free,
                freed,
                hogs,
                cause,
                thresholds,
            } => {
                self.post_message(message_reporters::disk_full(
                    free,
                    freed,
                    &hogs,
                    cause,
                    &thresholds,
                ));
            }
            HostNote::DiskFullOver => {
                self.messages.resolve_key_prefix(
                    message_reporters::DISK_FULL_KEY,
                    aterm_messages::Outcome::Ok,
                    Instant::now(),
                );
                aterm_log::info!("harness disk watch: the disk is out of its emergency");
            }
            // A beat, not a fact: no record, no claim. With no window showing
            // the stats bars, the jobs engine reads on it.
            HostNote::DiskLooked => self.vitals_disk_looked(Instant::now()),
            HostNote::DiskSettingsRead => {
                self.host_noted.remove(DISK_CLEANUP_OFF_NOTED);
                self.except_clear(message_reporters::DISK_SETTINGS_KEY);
                self.except_clear(message_reporters::DISK_LOW_CLEANUP_OFF_KEY);
            }
            HostNote::UpgradeCannotRun { sid, step, dir } => {
                self.host_noted.insert(format!("upgrade:{sid}"));
                self.except(&message_reporters::upgrade_cannot_run_entry(
                    &step,
                    dir.as_deref(),
                ));
            }
            HostNote::ResumeException {
                sid,
                agent,
                standing,
            } => {
                // A `Now` that no escalation of the loop's carries (Claude
                // Code's lost login carried to the owner's step: the owner's
                // four-part notice) is the owner's ask on the glass while it
                // stands; one the loop escalated is that escalation's, so
                // nothing is said twice.
                match standing
                    .as_ref()
                    .filter(|exception| exception.when == Urgency::Now && !exception.escalated)
                {
                    Some(exception) => self.raise_resume_ask(&sid, exception),
                    None => self.end_resume_ask(&sid),
                }
                match standing {
                    // What can wait for the owner's next look is a line. A
                    // notice on the glass is its ask alone: the digest says
                    // nothing of it beside the ask (Settings > Messages
                    // lists the ask once).
                    Some(exception) if exception.when == Urgency::Later => {
                        aterm_log::info!(
                            "harness @{sid}: needs a person ({}): {} => {}",
                            exception.kind.word(),
                            exception.why,
                            exception.action
                        );
                        self.resume_standing.insert(sid, (exception, agent));
                    }
                    Some(_) | None => {
                        self.resume_standing.remove(&sid);
                    }
                }
                self.hand_resume_exceptions();
            }
            HostNote::LoginBack { sid } => self.close_notice_story(&sid),
            HostNote::ResumeUnread { sid, agent, stall } => match stall {
                Some(stall) => {
                    // The second rung of the stall canary (90 minutes of
                    // background work, 30 of a dialog). A record, once per
                    // episode, and one log line, in the witness's words; the
                    // digest waits for the third (3 h; an hour).
                    if self.host_noted.insert(format!("stalled:{sid}")) {
                        let spot = self.upgrade_spot(&sid);
                        let record = match stall {
                            // aterm's own step is stuck on a screen it does
                            // not read, Claude's own title the witness.
                            StallSeen::Unread { minutes } => {
                                aterm_log::error!(
                                    "harness @{sid}: Claude's own title has said idle for \
                                     {minutes} min over a screen this build does not recognise"
                                );
                                message_reporters::stall_unread_record(&sid, &spot.place, minutes)
                            }
                            // The agent's own work, which its reader names,
                            // has not ended: no fault of aterm's reading.
                            StallSeen::OwnWork { minutes } => {
                                aterm_log::warn!(
                                    "harness @{sid}: {}'s own background work has run {minutes} \
                                     min under words that stood still",
                                    agent.product()
                                );
                                message_reporters::stall_on_work_record(
                                    &sid,
                                    &spot.place,
                                    agent,
                                    minutes,
                                )
                            }
                            // The jobs reading names the call the agent waits
                            // on, which has made no progress: no fault of
                            // aterm's reading.
                            StallSeen::NoProgress { minutes, pid } => {
                                aterm_log::warn!(
                                    "harness @{sid}: a background call of {}'s (pid {pid}) has \
                                     made no progress for {minutes} min",
                                    agent.product()
                                );
                                message_reporters::stall_no_progress_record(
                                    &sid,
                                    &spot.place,
                                    agent,
                                    pid,
                                    minutes,
                                )
                            }
                        };
                        self.record_message(record);
                    }
                }
                None => {
                    self.host_noted.remove(&format!("stalled:{sid}"));
                }
            },
            HostNote::Relaunch { sid, said } => self.note_relaunch(&sid, said),
            HostNote::Accounts(said) => {
                // A roster that lists rows speaks for Claude's login: the AI
                // CLIs' lines then say nothing of Claude (`ai_screen`).
                self.note_ai_accounts_listed(matches!(
                    said,
                    AccountsSaid::Listed { .. } | AccountsSaid::LoginsUnread
                ));
                // One fact at a time, the one whose action can be done: a
                // roster that cannot be read — refused, or this machine's
                // logins unread (r3h-7) — names no account, so its line
                // stands alone, each naming its own file. Each end is said
                // at every note, as every end here is.
                let refused = said == AccountsSaid::Refused;
                let logins_unread = said == AccountsSaid::LoginsUnread;
                let unread = refused || logins_unread;
                let (labels, credits, full, due, none_soon, wrong) = match said {
                    AccountsSaid::Nothing | AccountsSaid::Refused | AccountsSaid::LoginsUnread => {
                        Default::default()
                    }
                    AccountsSaid::Listed {
                        signed_out,
                        credits,
                        window_full,
                        due,
                        none_soon,
                        wrong,
                    } => (signed_out, credits, window_full, due, none_soon, wrong),
                };
                // The login lifecycle's lines and the account's usage
                // (accounts v2 §5.4: credits in use, the day's line, and the
                // week nearly used with no credits), each standing while its
                // fact does; a roster that cannot be read says nothing of
                // them, and what the owner filed of one holds.
                let lines = [
                    (
                        message_reporters::ACCOUNTS_DUE_KEY,
                        message_reporters::accounts_due_entry(&due),
                    ),
                    (
                        message_reporters::ACCOUNTS_NONE_SOON_KEY,
                        none_soon.then(message_reporters::accounts_none_soon_entry),
                    ),
                    (
                        message_reporters::ACCOUNTS_WRONG_KEY,
                        message_reporters::accounts_wrong_entry(&wrong),
                    ),
                    (
                        message_reporters::ACCOUNTS_CREDITS_KEY,
                        message_reporters::accounts_credits_entry(&credits),
                    ),
                    (
                        message_reporters::ACCOUNTS_WINDOW_FULL_KEY,
                        message_reporters::accounts_window_full_entry(
                            &full,
                            self.harness_policy.get().policy.usage_credits,
                        ),
                    ),
                ];
                for (key, entry) in lines {
                    match entry {
                        Some(entry) => self.except(&entry),
                        None if unread => {
                            self.except_end(key);
                        }
                        None => self.except_clear(key),
                    }
                }
                if refused {
                    self.except(&message_reporters::accounts_refused_entry());
                } else {
                    self.except_clear(message_reporters::ACCOUNTS_REFUSED_KEY);
                }
                if logins_unread {
                    self.except(&message_reporters::accounts_logins_unread_entry());
                } else {
                    self.except_clear(message_reporters::ACCOUNTS_LOGINS_UNREAD_KEY);
                }
                let key = message_reporters::ACCOUNTS_SIGNED_OUT_KEY;
                match message_reporters::accounts_signed_out_entry(&labels) {
                    Some(entry) => {
                        // THE LINE'S ASK IS ONE ACCOUNT'S COMMAND. Restated in
                        // place, the next account's would stay unpublished
                        // inside the digest's restate quiet, and the log —
                        // what `aterm exceptions` prints — keeps a line's
                        // first words for as long as it is open. So an open
                        // line whose action is for another account ends, and
                        // this one is raised with its own words. One the
                        // owner filed is not open: it stays quiet.
                        let first = labels.first().map(String::as_str);
                        let digest = self.messages.digest();
                        let other = digest.entries.iter().any(|open| {
                            open.key == key
                                && message_reporters::accounts_signed_out_first(&open.detail)
                                    != first
                        });
                        if other {
                            self.except_end(key);
                        }
                        self.except(&entry);
                    }
                    // A roster that cannot be read says nothing of who is
                    // signed out: the line gives way, and what the owner
                    // filed of it holds.
                    None if unread => {
                        self.except_end(key);
                    }
                    None => self.except_clear(key),
                }
            }
            HostNote::Inform { sid, text } => {
                let place = self.upgrade_place(&sid);
                self.record_message(message_reporters::supervisor_note(&sid, &place, &text));
            }
            // ONE line for the machine while any tab's usage tap stands
            // silent, raised at most once a calendar day (UTC): a fact worth
            // a look later. Its end is said when the last tab is heard again
            // or its worker ends.
            HostNote::UsageTap { sid, silent } => {
                let tab = format!("{USAGE_TAP_NOTED}{sid}");
                if silent {
                    self.host_noted.insert(tab);
                    let day = aterm_time::unix_secs().map_or(0, |s| s / 86_400);
                    let today = format!("{USAGE_TAP_DAY}{day}");
                    if !self.host_noted.contains(&today) {
                        self.host_noted
                            .retain(|key| !key.starts_with(USAGE_TAP_DAY));
                        self.host_noted.insert(today);
                        self.except(&message_reporters::usage_tap_silent_entry());
                    }
                } else {
                    self.host_noted.remove(&tab);
                    if !self
                        .host_noted
                        .iter()
                        .any(|key| key.starts_with(USAGE_TAP_NOTED))
                    {
                        self.except_clear(message_reporters::USAGE_TAP_SILENT_KEY);
                    }
                }
            }
            HostNote::Warn { sid, text } => {
                let place = self.upgrade_place(&sid);
                self.post_message(message_reporters::supervisor_warning(&sid, &place, &text));
            }
            // What a layout carries of the tab's agent moved (an in-app
            // `/clear` followed on a quiet desk, LC-1): the crash journal
            // captures again, at once, whatever its write interval says.
            HostNote::AgentPublished { .. } => self.crash_journal.owe_agent_capture(),
            // The records' folder the owner's view cannot list: the upgrades
            // shown stay at its last whole look. Its end is unconditional, so
            // a line an earlier process left ends at the first listed look.
            HostNote::UpgradeRecords { unlisted } => match unlisted {
                Some(dir) => self.except(&message_reporters::upgrade_records_unlisted_entry(&dir)),
                None => self.except_clear(message_reporters::UPGRADE_RECORDS_UNLISTED_KEY),
            },
            // The records the owner's view cannot read that keep a tab as it
            // stands: what the window shows of those tabs stays as it is. Its
            // end is unconditional too.
            HostNote::UpgradeRecordsUnreadable { files } => {
                if files.is_empty() {
                    self.except_clear(message_reporters::UPGRADE_RECORDS_UNREADABLE_KEY);
                } else {
                    self.except(&message_reporters::upgrade_records_unreadable_entry(&files));
                }
            }
            HostNote::UpgradeRuns { sid } => {
                self.host_noted.remove(&format!("upgrade:{sid}"));
                // Another tab's upgrade that cannot run keeps the one line.
                if !self
                    .host_noted
                    .iter()
                    .any(|key| key.starts_with("upgrade:"))
                {
                    self.except_clear(message_reporters::UPGRADE_CANNOT_RUN_KEY);
                }
            }
        }
    }

    /// A tab's relaunch says something about itself ([`RelaunchSay`]): a record
    /// for a relaunch that keeps failing — and, where no command is named for
    /// the tab's mark, one line of the digest at once (LC-5 of
    /// ATERM-KI-1015-1), save for the harness's own restart, whose record's
    /// own row speaks (`harness_host::failing_names_command`); else, for a
    /// cause only a person clears, a line once it has gone on for an hour —
    /// and for the owner's own limit; for an agent that cannot be brought
    /// back, a record and, where no command can be named for the tab's mark,
    /// one line of the digest. Its end (an agent runs in the tab again) ends
    /// every line but two: a `lost` one, which its conversation held again
    /// ends ([`RelaunchSay::Back`]), a newer word of the tab, or the tab
    /// closing; and the line of what a restart did not bring back
    /// ([`RelaunchSay::NotRestored`], a key of its own), which nothing the
    /// agent or its conversation does ends — what it lost stays lost — only
    /// the tab closing. Never a mark here: the host sets it.
    fn note_relaunch(&mut self, sid: &str, said: RelaunchSay) {
        let [blocked, gone, lost] = message_reporters::relaunch_keys(sid);
        let spot = self.upgrade_spot(sid);
        match said {
            RelaunchSay::Ended => {
                self.host_noted.remove(&relaunch_told_key(sid));
                self.except_clear(&blocked);
                self.except_clear(&gone);
            }
            // What the agent's restart did not bring back stays lost though
            // it runs again: its own key, which no `Ended`, `Back` or newer
            // word of the tab clears.
            RelaunchSay::NotRestored => {
                self.except(&message_reporters::relaunch_not_restored_entry(sid, &spot));
            }
            RelaunchSay::Failing {
                blocked_by,
                restarted,
                command,
            } => {
                let text = if restarted {
                    "aterm ended the agent and has not brought it back yet; it is \
                     still tried"
                } else {
                    "the agent exited and its relaunch keeps failing; it is still tried"
                };
                self.record_message(message_reporters::relaunch_record(sid, &spot.place, text));
                // AT ATTENTION (LC-5 of ATERM-KI-1015-1): with a command the
                // tab's mark says it; with none, the digest's line now — save
                // the harness's own restart's, which its row speaks for.
                if command.is_none() && !restarted {
                    self.host_noted.insert(relaunch_told_key(sid));
                    self.except(&message_reporters::relaunch_failing_entry(
                        sid, &spot, blocked_by, restarted,
                    ));
                } else if let Some(cause) = blocked_by {
                    self.host_noted.insert(relaunch_told_key(sid));
                    self.except_after(
                        &message_reporters::relaunch_blocked_entry(sid, &spot, cause, restarted),
                        Horizon::SUPERVISOR_RESTARTS.duration(),
                    );
                }
            }
            RelaunchSay::Limited { why } => {
                self.except_clear(&blocked);
                self.record_message(message_reporters::relaunch_record(
                    sid,
                    &spot.place,
                    &format!("the agent exited and is not relaunched: {why}"),
                ));
            }
            RelaunchSay::Cannot {
                reason,
                restarted,
                command,
                marked,
                lost: known,
                new_tab,
                then,
            } => {
                self.except_clear(&blocked);
                self.record_message(message_reporters::relaunch_record(
                    sid,
                    &spot.place,
                    &format!("aterm cannot bring the agent back: {reason}"),
                ));
                // With the tab's mark the tab carries the ask itself, and it
                // is the tab's one word: an earlier word's line goes. Without
                // one the owner's line is the digest's — keyed by its
                // conversation where one is known, which that conversation
                // held again ends (`Back`, LC-4), or its tab closing.
                if marked {
                    self.except_clear(&lost);
                } else {
                    self.host_noted.insert(relaunch_told_key(sid));
                    if known {
                        self.except(&message_reporters::relaunch_lost_entry(
                            sid,
                            &spot,
                            &reason,
                            restarted,
                            command.as_deref(),
                            then,
                        ));
                    } else {
                        self.except(&message_reporters::relaunch_gone_entry(
                            sid, &spot, &reason, restarted, new_tab, then,
                        ));
                    }
                }
            }
            RelaunchSay::SteppedDown {
                reason,
                restarted,
                command,
            } => {
                self.host_noted.insert(relaunch_told_key(sid));
                self.except(&message_reporters::relaunch_lost_entry(
                    sid,
                    &spot,
                    &reason,
                    restarted,
                    command.as_deref(),
                    None,
                ));
            }
            RelaunchSay::Back => self.except_clear(&lost),
        }
    }

    /// The resume loops' exceptions as they stand, handed to the digest as the
    /// family's WHOLE answer ([`Family::Resume`] is a level family): one entry per
    /// kind naming its tabs, a stalled or a context tab its own. A kind no tab has any more
    /// ends ten minutes after (the digest's clock).
    pub(crate) fn hand_resume_exceptions(&mut self) {
        let standing: Vec<_> = self
            .resume_standing
            .iter()
            .map(|(sid, (exception, agent))| {
                (
                    sid.clone(),
                    exception.clone(),
                    *agent,
                    self.upgrade_spot(sid),
                )
            })
            .collect();
        self.except_replace(
            Family::Resume,
            &message_reporters::resume_exceptions(&standing),
        );
    }

    /// A tab's resume loop needs the owner NOW and nothing of the loop's says
    /// so ([`message_reporters::resume_ask`]): its standing ask is posted —
    /// the same words are a duplicate, new ones restate it in place — and
    /// this process marks the tab as one it holds an ask for.
    ///
    /// A NOTICE IS SAID ONCE: its key is the tab's and the notice's
    /// ([`message_reporters::notice_ask_key`]), so the same notice at every
    /// point the loop tells it is the one row, and one the owner read stays
    /// quiet while it stands; another notice of the tab's is raised as news.
    ///
    /// ONE LOSS, ONE ROW IN SETTINGS ▸ MESSAGES (the owner, 2026-10-19: the
    /// same lost login listed three times): a tab's notices are one story
    /// ([`NoticeStory`]). The row a new notice is posted in replaces the
    /// story's row or record until now — on the glass, withdrawn as its point
    /// moved on, or read — so the page lists the story once, in its latest
    /// words ([`aterm_messages::MessageCenter::supersede_record`]); the login
    /// back closes it fixed ([`Self::close_notice_story`]). A notice the owner
    /// read stays quiet while its story stands, its row replaced or not
    /// ([`NoticeStory::keeps_quiet`]). A story whose row left the glass and
    /// that had no word for [`NOTICE_STORY_SPAN`] is over: the next notice is
    /// a loss of its own, its own row.
    fn raise_resume_ask(
        &mut self,
        sid: &str,
        exception: &aterm_agent::supervise::policy::exception::Exception,
    ) {
        let notice = exception.notice.as_ref();
        if self.host_noted.insert(resume_asked_key(sid)) {
            aterm_log::warn!(
                "harness @{sid}: needs a person now ({}{}): {} => {}",
                exception.kind.word(),
                notice.map_or_else(String::new, |n| format!(", notice {}", n.id)),
                exception.why,
                exception.action
            );
        }
        let key = message_reporters::notice_ask_key(sid, notice.map(|n| n.code));
        let wall = wall_stamp_now().unix_ms;
        // A story whose row left the glass and that has had no word for its
        // span is over (a loss whose end no loop told): this notice is a loss
        // of its own, and the last keeps its own row.
        if self
            .resume_stories
            .get(sid)
            .is_some_and(|story| !story.goes_on(self.messages.live(story.head).is_some(), wall))
        {
            self.resume_stories.remove(sid);
        }
        // A notice the owner read in this story, its row replaced since, is
        // not raised again while the story stands (the center's own quiet for
        // it ended with the replacement): nothing posted, and the tab's other
        // ask withdrawn, as the center's quiet return does.
        if let Some(story) = self.resume_stories.get_mut(sid)
            && story.keeps_quiet(&key, wall)
            && self.messages.live_by_key(&key).is_none()
        {
            story.last = wall;
            self.withdraw_resume_asks(sid, Some(&key));
            return;
        }
        let spot = self.upgrade_spot(sid);
        let msg = message_reporters::resume_ask(sid, &spot, exception);
        let back = msg.finished.clone();
        let head = self.notice_story_head(sid);
        // The head's read, taken before a replacement retires it again.
        let head_read = head.and_then(|head| {
            let rec = self.messages.log().get(head)?;
            matches!(
                rec.retired(),
                Some(aterm_messages::Retired::Folded | aterm_messages::Retired::Dismissed)
            )
            .then_some((rec.key.clone()?, rec.retired_unix_ms?))
        });
        let id = self.post_message(msg);
        // A later row tells the story on (a duplicate is the head itself, and
        // a notice the owner read in this story, kept quiet, comes back as
        // its earlier record: neither replaces anything).
        if head.is_none_or(|head| head < id) {
            let mut read = self
                .resume_stories
                .remove(sid)
                .map(|story| story.read)
                .unwrap_or_default();
            if let Some(head) = head
                && self.supersede_record(head, id)
                && let Some((key, at)) = head_read
            {
                read.insert(key, at);
            }
            self.resume_stories.insert(
                sid.to_string(),
                NoticeStory {
                    head: id,
                    back,
                    last: wall,
                    read,
                },
            );
        } else if let Some(story) = self.resume_stories.get_mut(sid) {
            story.last = wall;
        } else if head == Some(id) {
            // A row an earlier process carried here, told again in its own
            // words: the post is a duplicate of it, and the story this
            // process holds none of is told in it, so the login back closes
            // it fixed.
            self.resume_stories.insert(
                sid.to_string(),
                NoticeStory {
                    head: id,
                    back,
                    last: wall,
                    read: std::collections::BTreeMap::new(),
                },
            );
        }
        self.withdraw_resume_asks(sid, Some(&key));
    }

    /// The row or record tab `sid`'s notices are told in now: the story this
    /// process holds, else the newest ask of the tab an earlier process
    /// carried here.
    fn notice_story_head(&self, sid: &str) -> Option<aterm_messages::MessageId> {
        if let Some(story) = self.resume_stories.get(sid) {
            return Some(story.head);
        }
        let base = message_reporters::resume_ask_key(sid);
        self.messages
            .live_rows()
            .filter(|row| {
                row.msg
                    .key
                    .as_deref()
                    .is_some_and(|key| is_tab_ask_key(key, &base))
            })
            .map(|row| row.id)
            .max()
    }

    /// TAB `sid`'S LOST LOGIN IS BACK ([`HostNote::LoginBack`]): the story its
    /// notices told ends FIXED, as one row — a row still on the glass resolves
    /// `Ok` in its declared fix words, a record whose row already left
    /// (withdrawn as the agent went back to work, or read) is closed fixed in
    /// them ([`aterm_messages::MessageCenter::fix_record`]) — and the next
    /// loss is a story of its own, news even where the owner read this one.
    /// A tab with no story moves nothing.
    fn close_notice_story(&mut self, sid: &str) {
        let Some(story) = self.resume_stories.remove(sid) else {
            return;
        };
        self.host_noted.remove(&resume_asked_key(sid));
        if self.messages.live(story.head).is_some() {
            let _ = self.resolve_message(story.head, aterm_messages::Outcome::Ok);
        } else if let Some(back) = story.back
            && self
                .messages
                .fix_record(story.head, &back, wall_stamp_now(), Instant::now())
        {
            self.sync_messages();
        }
        aterm_log::info!("harness @{sid}: the login is back; its notice reads fixed");
    }

    /// Withdraws tab `sid`'s live asks ([`message_reporters::notice_ask_key`]
    /// under its [`message_reporters::resume_ask_key`]) but the one keyed
    /// `keep`, claiming no outcome.
    fn withdraw_resume_asks(&mut self, sid: &str, keep: Option<&str>) {
        let base = message_reporters::resume_ask_key(sid);
        let left: Vec<aterm_messages::MessageId> = self
            .messages
            .live_rows()
            .filter(|row| {
                row.msg
                    .key
                    .as_deref()
                    .is_some_and(|key| is_tab_ask_key(key, &base) && Some(key) != keep)
            })
            .map(|row| row.id)
            .collect();
        for id in left {
            let _ = self.withdraw_message(id);
        }
    }

    /// The tab's ask is over (the loop's point no longer holds it, or the loop
    /// ended): its row is withdrawn, claiming no outcome — the loop says the
    /// point moved on, not that the owner's step was taken. Its story stands
    /// ([`NOTICE_STORY_SPAN`] from this last word): the loss's next notice
    /// tells it on, and the login back closes it fixed. Only an ask that
    /// stood is a word of the story: the tab's other tells routed here (a
    /// stall, a spend, a goal cleared, a worker's end, each with no ask of
    /// this process's standing) say nothing of the loss, so they never keep
    /// a story that had no word open for the tab's next loss to swallow.
    fn end_resume_ask(&mut self, sid: &str) {
        let ended = self.host_noted.remove(&resume_asked_key(sid));
        self.withdraw_resume_asks(sid, None);
        if ended && let Some(story) = self.resume_stories.get_mut(sid) {
            story.last = wall_stamp_now().unix_ms;
        }
    }

    /// The asks an earlier process carried here that no loop of this one
    /// tells of: withdrawn at the resume family's settle, once every loop has
    /// had its first point ([`RESUME_SETTLE`]), as the digest's lines end then.
    fn sweep_resume_asks(&mut self) {
        let held: Vec<String> = self
            .host_noted
            .iter()
            .filter_map(|mark| mark.strip_prefix(RESUME_ASKED))
            .map(message_reporters::resume_ask_key)
            .collect();
        let prefix = format!("{}.", message_reporters::RESUME_ASK_KEY);
        let left: Vec<aterm_messages::MessageId> = self
            .messages
            .live_rows()
            .filter(|row| {
                row.msg.key.as_deref().is_some_and(|key| {
                    key.starts_with(&prefix) && !held.iter().any(|base| is_tab_ask_key(key, base))
                })
            })
            .map(|row| row.id)
            .collect();
        for id in left {
            let _ = self.withdraw_message(id);
        }
        // The machine's renew ask an earlier build left: no build raises it
        // any more (a lost login is its tab's notice).
        if let Some(id) = self
            .messages
            .live_by_key(message_reporters::RENEW_ASK_KEY)
            .map(|row| row.id)
        {
            let _ = self.withdraw_message(id);
        }
    }

    /// A session refused a program's palette set, which happens only under the
    /// owner's explicit `allow_palette_reconfigure = false` (palette sets are
    /// allowed by default; the engine tells only a session's first): one digest
    /// line for every tab, never an on-screen alert. Posting it again is a
    /// duplicate, a line the owner dismissed stays filed, and a config that
    /// allows palette sets again ends it.
    pub(crate) fn note_palette_refused(&mut self) {
        self.except(&message_reporters::palette_refused_entry());
    }

    /// The repaint nudge gave up on `session`'s render epoch (`render.<tab>.
    /// unrepaired`, an EVENT of [`Family::Render`]): the tab may be drawn one row
    /// off, and dragging the window edge one row and back is the owner's. One
    /// line, dismissable, never a mark on the tab; the entry ends when the epoch
    /// heals ([`Self::check_render_repairs`]).
    ///
    /// An ADOPTION nudge (a blank agent tab, [`ADOPTION_EPOCH`]) names no epoch of
    /// the ledger, so nothing could ever say it healed: it is a log line, not a
    /// line of the digest. Nor is a give-up whose epoch the ledger no longer
    /// holds at risk by the time it gets here (a resize opened a new one): the
    /// line's own end is already true, so it would be raised and ended one
    /// loop turn apart. ONE rule decides both ends, the engine's debt: the
    /// machine gives up on a debt only while the engine owes rows, read fresh
    /// at that step, so a debt the program's repaint paid never reaches here,
    /// however late the ledger looks.
    pub(crate) fn note_render_unrepaired(&mut self, session: u64, epoch: u64, why: GiveUp) {
        if epoch == ADOPTION_EPOCH {
            aterm_log::info!(
                "exceptions: the adoption repaint of session {session} gave up ({}); no epoch \
                 to watch, so no line",
                why.word()
            );
            return;
        }
        if !self.render_epoch_still_off(session, epoch) {
            aterm_log::info!(
                "exceptions: the repaint of session {session} gave up ({}) on epoch {epoch}, \
                 which is no longer at risk; no line",
                why.word()
            );
            return;
        }
        let Some(sid) = self.session_sid(session) else {
            return;
        };
        let spot = self.upgrade_spot(&sid);
        self.render_unrepaired.insert(session, (sid.clone(), epoch));
        self.except(&message_reporters::render_unrepaired_entry(
            &sid, &spot, why,
        ));
    }

    /// A Claude Code whose registry row reports a build its Store or Native
    /// drawer image does not name (card engine §3.10) is said as one entry
    /// per image and reported build
    /// ([`crate::session_timeline::SessionTimeline::take_registry_drift`]),
    /// on the footer's own wake. It gates nothing: the card reads neither.
    pub(crate) fn note_claude_build_drift(&mut self, session: u64) {
        let drift = {
            let Some(found) = self.pool.get(session) else {
                return;
            };
            found
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take_registry_drift()
        };
        let Some(drift) = drift else {
            return;
        };
        let Some(sid) = self.session_sid(session) else {
            return;
        };
        let spot = self.upgrade_spot(&sid);
        self.except(&message_reporters::claude_build_drift_entry(
            &sid, &spot, &drift,
        ));
    }

    /// End the `render.<tab>.unrepaired` lines whose epoch is no longer the
    /// tab's open, at-risk one (a real resize, a full clear or a screen switch
    /// healed it) or whose session is gone. A line this window does not hold an
    /// epoch for (one an earlier process raised and this one adopted: every grid
    /// is new since) ends at once. Cheap: nothing is open in the common case.
    pub(crate) fn check_render_repairs(&mut self) {
        // Every turn of the event loop asks: with nothing held and the earlier
        // process's lines swept once, it is one flag and one map length.
        if self.render_lines_swept && self.render_unrepaired.is_empty() {
            return;
        }
        let open = self.messages.digest_keys_under(&Family::Render.prefix());
        self.render_lines_swept = true;
        if open.is_empty() && self.render_unrepaired.is_empty() {
            return;
        }
        let healed: Vec<u64> = self
            .render_unrepaired
            .iter()
            .filter(|(session, (_, epoch))| !self.render_epoch_still_off(**session, *epoch))
            .map(|(session, _)| *session)
            .collect();
        for session in healed {
            if let Some((sid, _)) = self.render_unrepaired.remove(&session) {
                self.except_clear(&message_reporters::render_unrepaired_key(&sid));
            }
        }
        for key in open {
            let held = self
                .render_unrepaired
                .values()
                .any(|(sid, _)| message_reporters::render_unrepaired_key(sid) == key);
            if !held {
                self.except_clear(&key);
            }
        }
    }

    /// A tab's relaunch lines (`not-back`, `gone`, `lost`, `not-restored`) and
    /// the claim that would raise them are about the agent in a tab: with the
    /// tab closed the fact cannot be true and nobody can act on it — a `lost`
    /// line tells a command to run in that tab, a `not-restored` one asks a
    /// look at it, and both show it — so they end (the worker of a closed tab
    /// has no later word to end them, and its host forgets the tab's word
    /// with it).
    pub(crate) fn end_relaunch_lines_of_closed_tabs(&mut self) {
        // Every loop turn comes here (`ripen_exceptions`), and the live tabs'
        // keys cost a string per tab: they are computed only while there is a
        // relaunch line, claim or told mark to judge, which is almost never.
        let mut keys: Vec<String> = self
            .messages
            .digest_keys_under(&Family::Supervisor.prefix())
            .into_iter()
            .filter(|key| is_relaunch_key(key))
            .collect();
        keys.extend(
            self.ripening
                .iter()
                .filter(|claim| is_relaunch_key(&claim.key))
                .map(|claim| claim.key.clone()),
        );
        let told = self
            .host_noted
            .iter()
            .any(|noted| noted.starts_with("relaunch:"));
        if keys.is_empty() && !told {
            return;
        }
        // Whether a line's tab is still there is decided by computing the keys
        // of the tabs that ARE there, never by reading an id back out of a key:
        // a key shortens a subject that does not fit its cap (a real session id
        // is 22 characters), so it does not round-trip.
        let live = self.shown_session_sids();
        let live_keys: std::collections::HashSet<String> = live
            .iter()
            .flat_map(|sid| {
                let [blocked, gone, lost] = message_reporters::relaunch_keys(sid);
                [
                    blocked,
                    gone,
                    lost,
                    message_reporters::relaunch_not_restored_key(sid),
                ]
            })
            .collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            if !live_keys.contains(&key) {
                self.except_clear(&key);
            }
        }
        self.host_noted.retain(|noted| {
            noted
                .strip_prefix("relaunch:")
                .is_none_or(|sid| live.contains(sid))
        });
    }

    /// The lines of the process-local supervisor facts that an earlier process
    /// raised and this window adopted, ended once this process has run
    /// [`SUPERVISOR_STEADY`] without telling of them: a supervisor that failed
    /// again would have been noted within that time, and a survey that is still
    /// open is told again by the successor's first look. A line this process does
    /// hold ([`Self::supervisor_failing`], [`Self::host_noted`]) is left to its
    /// own end. A `lost` line an earlier process left ends so too, though its
    /// conversation may still be lost: the handoff carries no word of it to
    /// this process's host, so nothing here would ever end it (a residual,
    /// stated: the word lapses at a seamless update). A `not-restored` line
    /// is no process's word but a fact (what a restart did not bring back):
    /// it stands, to its tab's close or the owner's look.
    pub(crate) fn sweep_adopted_supervisor_lines(&mut self) {
        // The keys THIS process holds, computed from the ids it holds them
        // under: an id is never read back out of a key (a key shortens a
        // subject that does not fit its cap, so it does not round-trip).
        let mut held: std::collections::HashSet<String> = self
            .supervisor_failing
            .keys()
            .map(|sid| message_reporters::supervisor_failing_key(sid))
            .collect();
        for noted in &self.host_noted {
            if let Some(rest) = noted.strip_prefix("survey:") {
                if let Some((sid, _)) = rest.rsplit_once(':') {
                    held.insert(message_reporters::survey_key(sid));
                }
            } else if let Some(sid) = noted.strip_prefix("relaunch:") {
                // A relaunch line stands while THIS process's worker told of it
                // and has not seen the agent run again; an earlier run's is
                // ended here, a live relaunch tells it again.
                held.extend(message_reporters::relaunch_keys(sid));
            }
        }
        for key in self
            .messages
            .digest_keys_under(&Family::Supervisor.prefix())
        {
            let Some(cause) = key
                .strip_prefix(&Family::Supervisor.prefix())
                .and_then(|rest| rest.rsplit_once('.'))
                .map(|(_, cause)| cause)
            else {
                continue;
            };
            let process_held = matches!(cause, "failing" | "not-back" | "gone" | "lost")
                || cause.starts_with("survey-");
            if process_held && !held.contains(&key) {
                aterm_log::info!(
                    "exceptions: {key} was an earlier run's; nothing has told of it since"
                );
                self.except_clear(&key);
            }
        }
    }

    /// The session id (`s-3e79`) of the pool session `local`, if it is
    /// registered.
    pub(crate) fn session_sid(&self, local: u64) -> Option<String> {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        store
            .by_local(local)
            .map(|handle| handle.sid.as_str().to_string())
    }

    /// Whether `session`'s render epoch `epoch` is still open and at risk. A
    /// session that has gone, or whose timeline is busy, answers as the safer
    /// half: a gone tab has nothing off, and a busy timeline is asked again.
    fn render_epoch_still_off(&self, session: u64, epoch: u64) -> bool {
        let Some(found) = self.pool.get(session) else {
            return false;
        };
        // A timeline that is busy is asked again; one a panic poisoned is read
        // as the rest of the window reads it (`into_inner`), never as "still
        // off" for good.
        let timeline = match found.ctx.timeline.try_lock() {
            Ok(timeline) => timeline,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return true,
        };
        timeline.resize_epoch_at_risk(epoch)
    }

    /// The owner's `Dismiss all` (the menu-bar item, Settings, `notice dismiss
    /// review`): every open entry is filed `Dismissed` at one stamp. The log
    /// is flushed before this returns (bounded), so a dismissal cannot die
    /// with the process. Returns how many were filed.
    pub(crate) fn dismiss_exceptions(&mut self) -> usize {
        let filed = self.messages.file_digest(wall_stamp_now(), Instant::now());
        if filed > 0 {
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_UI_WAIT);
            self.digest_moved();
        }
        filed
    }

    /// The owner's dismissal of ONE entry (Settings ▸ Messages, `notice dismiss
    /// <id>`): filed `Dismissed`, flushed before it returns.
    // The Settings page's per-entry `Dismiss` (a view-time capsule,
    // `aterm_messages::page::DISMISS_ACTION`) calls it; `notice dismiss <id>` files
    // through the engine itself.
    pub(crate) fn dismiss_exception(&mut self, id: aterm_messages::MessageId) -> bool {
        let filed = self
            .messages
            .file_digest_one(id, wall_stamp_now(), Instant::now());
        if filed {
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_UI_WAIT);
            self.digest_moved();
        }
        filed
    }

    /// The entry as a checked message, or `None` (and ONE `aterm.log` error
    /// naming the key, said again only once its words have passed or its
    /// fault has changed: a level family restates at every look) when its
    /// words fail the rule: an owner is never shown words the rule refuses,
    /// and the roster test
    /// (`host_exceptions_tests::every_host_entry_passes_the_door_and_the_caps`)
    /// fails in a test run first.
    fn checked_entry(&mut self, entry: &Entry) -> Option<Message> {
        let key = entry.key();
        match entry.message() {
            Ok(msg) => {
                self.entries_refused.remove(&key);
                Some(msg)
            }
            Err(fault) => {
                if self.entries_refused.get(&key) != Some(&fault) {
                    aterm_log::error!("exceptions: {key} recorded, not shown: {fault:?}");
                    self.entries_refused.insert(key, fault);
                }
                None
            }
        }
    }

    /// The digest changed: Settings ▸ Messages and the menu-bar item follow,
    /// and NO window repaints (the band is untouched by construction). The
    /// new lines reach the log at the loop's next sweep (`settle_messages`).
    pub(crate) fn digest_moved(&mut self) {
        #[cfg(test)]
        DIGEST_MOVES.with(|c| c.set(c.get() + 1));
        self.publish_native_messages_state_if_due(Instant::now());
        self.mark_status_dirty();
        self.note_message_rule();
    }

    /// The rule's counter, said in `aterm.log` when it MOVES, from every path
    /// that posts (a plain post, a restatement, a family pass), not only from a
    /// digest change: a reporter whose message the door recorded (`demotions`:
    /// a defect, warn). The old spelling of a persistent hold is a compile
    /// error (the seal), so the builders the door once counted are gone.
    pub(crate) fn note_message_rule(&mut self) {
        let demoted = self.messages.demotions();
        if demoted != self.demotions_logged {
            self.demotions_logged = demoted;
            aterm_log::warn!(
                "message rule: {demoted} message(s) recorded instead of shown: nothing earned \
                 them a hold (last: {})",
                self.messages.last_demoted().unwrap_or("-")
            );
        }
    }

    /// AT COMMIT of a seamless update, in the successor: the parent's record was
    /// its own to the end, so a dismissal the owner made there after this
    /// process loaded the file is in the file and not in this ring. Read the
    /// tail again and fold it in BEFORE the first digest pass of this process.
    #[cfg(unix)]
    pub(crate) fn catch_up_messages_log(&mut self) {
        let Some(path) = crate::messages_store::path() else {
            return;
        };
        let (lines, _skipped) =
            crate::messages_store::tail_lines(&path, crate::messages_store::TAIL_BYTES);
        self.catch_up_messages_with(lines);
    }

    /// [`Self::catch_up_messages_log`] over the lines already read.
    #[cfg(unix)]
    fn catch_up_messages_with(&mut self, lines: Vec<aterm_messages::LogLine>) {
        let found = self
            .messages
            .catch_up(lines, wall_stamp_now(), Instant::now());
        if found.closed + found.adopted > 0 {
            aterm_log::info!("exceptions: caught up with the parent's log: {found:?}");
            self.digest_moved();
        }
    }

    /// A dev build named aterm.app writes the release's setup (`Wake::DevBuildWrites`):
    /// an entry of the digest, said once a launch. The Standing row an older
    /// build carried across an update under the old key is withdrawn first (one
    /// fact, one place).
    pub(crate) fn note_dev_build_writes(&mut self, writes: crate::DevSharedWrites) {
        let _ = self.messages.resolve_key_prefix(
            crate::update_words::KEY_DEV_BUILD_WRITES,
            aterm_messages::Outcome::Ok,
            Instant::now(),
        );
        self.except(&crate::update_words::dev_build_entry(writes));
    }
}

/// The 1-based line of `text` where `table` begins, for the editor the entry
/// opens: its first header (`[table]`, `[table.sub]`, `[[table.sub]]`), or,
/// above every header, its first key (`table.key = …`, `table = { … }`).
fn table_line(text: &str, table: &str) -> Option<u32> {
    let names = |rest: &str, ends: &[char]| {
        rest.strip_prefix(table)
            .is_some_and(|after| after.trim_start().starts_with(ends))
    };
    let mut above_headers = true;
    let at = text.lines().position(|line| {
        let line = line.trim_start();
        if let Some(header) = line.strip_prefix('[') {
            above_headers = false;
            return names(header.trim_start_matches('[').trim_start(), &[']', '.']);
        }
        above_headers && names(line, &['.', '='])
    })?;
    u32::try_from(at + 1).ok()
}

/// The `host_noted` key that says THIS process told of a tab's relaunch line.
fn relaunch_told_key(sid: &str) -> String {
    format!("relaunch:{sid}")
}

/// The stem of [`resume_asked_key`].
const RESUME_ASKED: &str = "resume-ask:";

/// The `host_noted` key that says THIS process holds a tab's resume ask
/// ([`App::raise_resume_ask`]).
fn resume_asked_key(sid: &str) -> String {
    format!("{RESUME_ASKED}{sid}")
}

/// Whether `key` is one of a tab's asks, whose own key is `base`
/// ([`message_reporters::resume_ask_key`]): that key, or a notice's under it
/// ([`message_reporters::notice_ask_key`]).
fn is_tab_ask_key(key: &str, base: &str) -> bool {
    key.strip_prefix(base)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Whether `key` is a `harness.supervisor.<subject>.not-back|gone|lost|
/// not-restored` key: a tab's relaunch line. Its subject may be a shortened
/// form of the session id, so it names no tab; compare it with
/// `message_reporters::relaunch_keys` and `relaunch_not_restored_key`.
fn is_relaunch_key(key: &str) -> bool {
    key.strip_prefix(&Family::Supervisor.prefix())
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(_, cause)| matches!(cause, "not-back" | "gone" | "lost" | "not-restored"))
}

#[cfg(test)]
mod tests {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};

    use crate::App;
    use crate::messages_host::message_regrids;

    fn tab_one() -> Entry {
        Entry::new(
            Family::Upgrade,
            "",
            "own-work",
            "Claude in tab 1 is still on 2.1.283",
            Why::OnlyYou(Reason::new(
                "aterm never ends running work, and a job it started 5 h ago (diskwatch.sh) still runs",
            )),
            Then::step("End that job in the tab"),
        )
    }

    fn full_disk_access() -> Entry {
        crate::message_reporters::file_access_entry(&crate::consent_card::NeedWords::sample())
    }

    /// LC-1 (ATERM-KI-1015-1): THE HOST'S WORD THAT WHAT A RESTORE CARRIES OF
    /// A TAB'S AGENT MOVED OWES THE CRASH JOURNAL A CAPTURE — its conversation
    /// followed on a desk that draws nothing more, the image a crash leaves
    /// names it — and says nothing. AT ONCE, inside the write interval
    /// (ATERM-KI-1016-1): held to it, a crash within 2 s of a `/clear` the
    /// host had followed left the image naming the conversation the agent had
    /// left, and the next launch resumed that one and typed its carry-on into
    /// it. NEGATIVE CONTROL: another note owes none, and plain activity waits
    /// for the interval.
    #[cfg(unix)]
    #[test]
    fn an_agent_publication_owes_the_crash_journal_a_capture() {
        let mut app = App::headless_for_test();
        let dir = crate::crash_journal::tests::scratch("cj-published");
        app.crash_journal = crate::crash_journal::Lane::new(
            Some(dir.path().to_path_buf()),
            crate::crash_journal::tests::reaped_pid(),
        );
        let t0 = std::time::Instant::now();
        assert!(app.crash_journal.offer(
            crate::crash_journal::fixtures::layout(&[("/a", "claude")]),
            Vec::new(),
            t0
        ));
        let inside = t0 + std::time::Duration::from_millis(300);
        app.note_harness_host(crate::harness_host::HostNote::SupervisorSteady {
            sid: "s-x".to_string(),
        });
        assert!(!app.crash_journal.take_dirty(), "another note owes none");
        app.crash_journal.note_activity();
        assert!(
            !app.crash_journal.capture_due(inside),
            "plain activity waits for the interval"
        );
        let said = app.messages.digest().len();
        app.note_harness_host(crate::harness_host::HostNote::AgentPublished {
            sid: "s-x".to_string(),
        });
        assert!(
            app.crash_journal.capture_due(inside),
            "a capture owed now, inside the interval"
        );
        assert_eq!(app.messages.digest().len(), said, "nothing said");
        app.crash_journal = crate::crash_journal::Lane::inert();
    }

    /// THE DIGEST COSTS THE TERMINAL NOTHING (the Row Law, clause 1): post,
    /// restate, family pass, clear, dismiss — the band's re-grid counter, the
    /// committed row count and the window's chrome rows do not move once, in a
    /// real `App` (presence owns no row to re-grid).
    #[test]
    fn a_digest_costs_no_regrid() {
        let mut app = App::headless_for_test();
        let message = message_regrids();
        let rows = app.message_band_rows;
        // The upgrade's rows are a LEVEL family (a pass, never an event); the
        // privacy card's is an EVENT family.
        // The loop's own sweep (`sync_messages`: settle, regrid to the band's
        // committed rows, publish) runs after each step: it is what would
        // re-grid a tab if a Later had been seated, so the counters below
        // judge something.
        app.sync_messages();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        app.sync_messages();
        app.except(&full_disk_access());
        app.sync_messages();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        app.sync_messages();
        assert_eq!(app.messages.digest().len(), 2);
        assert_eq!(app.messages.wanted_rows(), 0);
        app.except_clear("privacy.fda.needed");
        app.sync_messages();
        assert_eq!(app.dismiss_exceptions(), 1);
        app.sync_messages();
        assert!(app.messages.digest().is_empty());
        assert_eq!(message_regrids(), message, "no band re-grid");
        assert_eq!(
            app.message_band_rows, rows,
            "the pane's chrome rows never moved"
        );
    }

    /// A DISMISS CLICK NEVER WAITS ON A WRITER THAT DOES NOT ANSWER: the
    /// record's flush is bounded (law L5), so with the messages log's thread
    /// wedged the owner's `Dismiss all` and the per-entry `Dismiss` still
    /// file and return. Unbounded, either waited forever on the UI thread.
    #[test]
    fn a_dismissal_returns_though_the_log_writer_never_answers() {
        let mut app = App::headless_for_test();
        let dir = aterm_tempfile::tempdir().expect("a temp dir");
        let (writer, _thread) =
            crate::messages_store::Writer::stalled(&dir.path().join("messages.log"));
        app.messages_log = Some(writer);
        app.except_replace(Family::Upgrade, &[tab_one()]);
        app.sync_messages();
        let id = app.messages.digest().entries[0].id;
        assert!(app.dismiss_exception(id), "filed, the writer wedged");
        app.except(&full_disk_access());
        app.sync_messages();
        assert_eq!(app.dismiss_exceptions(), 1, "filed, the writer wedged");
        assert!(app.messages.digest().is_empty());
    }

    /// A SCRIPT'S DISMISSAL MOVES THE SURFACES (`aterm ctl notice dismiss review`
    /// and `dismiss <id>` file through the engine): the menu-bar item and
    /// Settings follow the same call, and the log is flushed before the reply.
    #[test]
    fn a_scripted_dismissal_moves_the_surfaces() {
        use super::DIGEST_MOVES;
        use aterm_messages::wire::NoticeRequest;
        let moves = || DIGEST_MOVES.with(std::cell::Cell::get);
        let mut app = App::headless_for_test();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        app.except(&full_disk_access());
        let id = app.messages.digest().entries[0].id;
        let before = moves();
        let one = app.take_notice(NoticeRequest::Dismiss(id));
        assert!(one.starts_with("OK"), "{one}");
        assert_eq!(app.messages.digest().len(), 1);
        assert_eq!(
            moves(),
            before + 1,
            "dismiss <id> moved the digest's readers"
        );
        let all = app.take_notice(NoticeRequest::DismissReview);
        assert!(all.contains("filed=1"), "{all}");
        assert!(app.messages.digest().is_empty());
        assert_eq!(moves(), before + 2, "dismiss review did too");
        let none = app.take_notice(NoticeRequest::DismissReview);
        assert!(none.contains("filed=0"), "{none}");
        assert_eq!(moves(), before + 2, "nothing to file, nothing to announce");
    }

    /// A dev build that writes the release's setup is ONE entry, said once.
    #[test]
    fn a_dev_build_that_writes_the_setup_is_one_entry() {
        let mut app = App::headless_for_test();
        let writes = || crate::DevSharedWrites {
            what: "the PATH links",
            repair: "`aterm pkg repair`",
        };
        app.note_dev_build_writes(writes());
        app.note_dev_build_writes(writes());
        assert_eq!(app.messages.digest().len(), 1);
        assert_eq!(app.messages.wanted_rows(), 0);
    }

    /// AT COMMIT the successor folds the parent's log tail in: a `Dismiss all`
    /// the parent made after this process loaded the file holds here, and the
    /// surfaces move.
    #[cfg(unix)]
    #[test]
    fn the_successor_folds_in_what_the_parent_filed() {
        use super::DIGEST_MOVES;
        use crate::messages_host::wall_stamp_now;
        use aterm_messages::{LogLine, MessageCenter, MessageLog};
        use std::time::Instant;
        let mut app = App::headless_for_test();
        app.except(&full_disk_access());
        assert_eq!(app.messages.digest().len(), 1);
        // The parent: the same open entry, then the owner's Dismiss all.
        let posted: Vec<LogLine> = app
            .messages
            .drain_shelved_for_persist()
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        let mut log = MessageLog::empty();
        log.replay_all(posted);
        let now = Instant::now();
        let mut parent = MessageCenter::new(log, now);
        assert_eq!(parent.adopt_digest(wall_stamp_now(), now), 1);
        assert_eq!(parent.file_digest(wall_stamp_now(), now), 1);
        let filed: Vec<LogLine> = parent
            .drain_shelved_for_persist()
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        let before = DIGEST_MOVES.with(std::cell::Cell::get);
        app.catch_up_messages_with(filed);
        assert!(
            app.messages.digest().is_empty(),
            "the parent's filing holds"
        );
        assert_eq!(DIGEST_MOVES.with(std::cell::Cell::get), before + 1);
    }

    /// The owner's dismissal is on disk before the call returns and holds after
    /// the same facts are asserted again (the next look, the next process).
    #[test]
    fn a_dismissal_holds_when_the_reporter_asserts_the_fact_again() {
        let mut app = App::headless_for_test();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        assert_eq!(app.dismiss_exceptions(), 1);
        app.except_replace(Family::Upgrade, &[tab_one()]);
        assert!(
            app.messages.digest().is_empty(),
            "a filed fact is quiet for its span"
        );
    }

    /// A reporter that words an entry against the rule shows the owner nothing:
    /// the entry is recorded in `aterm.log`, not posted (the roster test fails
    /// in a test run first).
    #[test]
    fn an_entry_that_breaks_the_rule_is_never_shown() {
        let mut app = App::headless_for_test();
        let bad = Entry::new(
            Family::Upgrade,
            "",
            "own-work",
            "Claude is behind",
            Why::OnlyYou(Reason::new("it is waiting for its work")),
            Then::step("Stop that work or skip the build"),
        );
        app.except(&bad);
        assert!(app.messages.digest().is_empty());
    }

    /// One producer, one mode: a LEVEL family speaks by pass and an EVENT family
    /// by event, and the host refuses the wrong call, so a pass over the
    /// upgrade's rows can never end the fact its HOST raised by event (review
    /// 2026-09-29, issue 10).
    #[test]
    fn a_level_family_is_never_raised_by_event_and_an_event_family_is_never_swept() {
        let mut app = App::headless_for_test();
        app.except(&tab_one());
        assert!(
            app.messages.digest().is_empty(),
            "the upgrade's rows speak by pass, not by event"
        );
        let cannot_run = Entry::new(
            Family::UpgradeHost,
            "",
            "cannot-run",
            "The upgrade cannot run",
            Why::OnlyYou(Reason::new("the upgrade cannot use its state folder")),
            Then::step("fix the folder's permissions"),
        );
        app.except(&cannot_run);
        assert_eq!(app.messages.digest().len(), 1);
        app.except_replace(Family::UpgradeHost, &[]);
        app.except_replace(Family::Upgrade, &[tab_one()]);
        app.except_replace(Family::Upgrade, &[]);
        assert_eq!(
            app.messages.digest().len(),
            2,
            "neither pass ended an entry it does not own"
        );
    }

    /// THE DIGEST'S OWN CLOCK (review 2026-09-29, issue 2): the upgrade's looks
    /// are sent only when its rows change. The row leaves in ONE change; the
    /// entry must still end — the loop is told when to come back
    /// (`messages_deadline`) and the sweep (`settle_messages`) ends it.
    #[test]
    fn an_entry_its_reporter_stopped_naming_ends_without_a_second_look() {
        use std::time::{Duration, Instant};
        let mut app = App::headless_for_test();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        assert_eq!(app.messages.digest().len(), 1);
        assert_eq!(app.messages_deadline(), None, "a named entry arms nothing");
        app.except_replace(Family::Upgrade, &[]);
        assert_eq!(app.messages.digest().len(), 1, "one look is a flicker");
        let due = app
            .messages_deadline()
            .expect("the loop is told when to come back");
        assert!(due > Instant::now(), "and not before the clock has run");
        let _ = app.settle_messages(due + Duration::from_secs(1));
        assert!(
            app.messages.digest().is_empty(),
            "no second look was needed"
        );
    }

    /// A WORDING FAULT NEVER ENDS A FACT (2026-10-08: the upgrade's own-work
    /// line restated in words the door refuses, twelve times in a day): the
    /// pass still names its key, so the open entry stands in the words it was
    /// shown in and no clock starts — the digest's own settle past
    /// `RESOLVE_AFTER` leaves it open. NEGATIVE CONTROL: a pass that stops
    /// naming it starts the clock as before.
    #[test]
    fn a_refused_restatement_leaves_its_entry_open() {
        use aterm_messages::need::RESOLVE_AFTER;
        use std::time::{Duration, Instant};
        let mut app = App::headless_for_test();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        let id = app.messages.digest().entries[0].id;
        let shown = app.messages.digest_entry(id).expect("open").detail;
        let refused = Entry::new(
            Family::Upgrade,
            "",
            "own-work",
            "Claude in tab 1 is still on 2.1.283",
            Why::OnlyYou(Reason::new(
                "aterm never ends running work, and it is waiting on background work of its own",
            )),
            Then::step("Stop that work in the tab when it suits you"),
        );
        assert_eq!(refused.key(), tab_one().key(), "the same fact");
        assert!(refused.message().is_err(), "words the door refuses");
        for _ in 0..3 {
            app.except_replace(Family::Upgrade, std::slice::from_ref(&refused));
        }
        assert_eq!(
            app.messages.digest_deadline(),
            None,
            "a fact still named starts no clock"
        );
        let _ = app.settle_messages(Instant::now() + RESOLVE_AFTER + Duration::from_secs(1));
        let open = app
            .messages
            .digest_entry(id)
            .expect("still open past the clock");
        assert_eq!(open.detail, shown, "in the words it was shown in");
        // NEGATIVE CONTROL: unnamed, the clock runs.
        app.except_replace(Family::Upgrade, &[]);
        assert!(app.messages.digest_deadline().is_some());
    }

    /// The page and the wire read ONE digest: an entry restated after its
    /// posting (a holder count moved) reads the same words in Settings, in
    /// `aterm ctl messages need=later live` and in the menu bar (review
    /// 2026-09-29, issue 12) — and plain `live` does not list it.
    #[test]
    fn the_surfaces_read_one_digest() {
        let mut app = App::headless_for_test();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        let id = app.messages.digest().entries[0].id;
        let state = app.messages_state();
        let page = state
            .entries
            .iter()
            .find(|e| e.id == id.raw())
            .expect("the entry stands on the page");
        let wire = app.messages.digest_entry(id).expect("open");
        assert_eq!(page.title, wire.title);
        assert_eq!(page.detail, wire.detail);
        let open =
            app.read_messages(&aterm_messages::wire::parse_read_args("need=later live").unwrap());
        assert_eq!(open.len(), 1);
        let live = app.read_messages(&aterm_messages::wire::parse_read_args("live").unwrap());
        assert!(
            live.is_empty(),
            "an open exception is never a live row: {live:?}"
        );
        assert_eq!(
            app.status_item_chrome_line(),
            "statusitem title=\u{276f}\u{2022} needs=0 stuck=0 exceptions=1 done=0 fleet=off",
            "the menu-bar cue an agent can read"
        );
    }

    /// THE PAGE'S OWN DISMISS BUTTONS (owner rule, 2026-09-28): every open entry
    /// offers `Dismiss` (files that one) and, with two or more open, the first
    /// offers `Dismiss all`; each press is flushed to the log; a settled entry
    /// offers neither; a stale press is `Gone`.
    #[test]
    fn the_page_dismisses_one_entry_or_all_of_them() {
        use crate::native_app::MessageActOutcome;
        use aterm_messages::page::{DISMISS_ACTION, DISMISS_ALL_ACTION};
        let mut app = App::headless_for_test();
        app.except_replace(Family::Upgrade, &[tab_one()]);
        app.except(&full_disk_access());
        assert_eq!(app.messages.digest().len(), 2);
        let state = app.messages_state();
        let open: Vec<_> = state
            .entries
            .iter()
            .filter(|e| e.state == "review")
            .collect();
        assert_eq!(open.len(), 2);
        for e in &open {
            assert!(
                e.actions
                    .iter()
                    .any(|a| a.index == DISMISS_ACTION && a.still_actionable),
                "every open entry offers Dismiss: {:?}",
                e.actions
            );
        }
        let all_on: Vec<_> = open
            .iter()
            .filter(|e| e.actions.iter().any(|a| a.index == DISMISS_ALL_ACTION))
            .collect();
        assert_eq!(all_on.len(), 1, "only the first offers Dismiss all");
        assert!(std::ptr::eq(*all_on[0], open[0]));

        // One entry: filed, the other stays.
        let (first, second) = (open[0].id, open[1].id);
        let outcome = app.perform_message_act(crate::WindowId(0), first, DISMISS_ACTION);
        assert!(
            matches!(&outcome, MessageActOutcome::Performed { .. }),
            "{outcome:?}"
        );
        assert_eq!(app.messages.digest().len(), 1);
        // A stale press on the same entry is gone; the settled record offers neither button.
        assert!(matches!(
            app.perform_message_act(crate::WindowId(0), first, DISMISS_ACTION),
            MessageActOutcome::Gone
        ));
        let state = app.messages_state();
        let filed = state
            .entries
            .iter()
            .find(|e| e.id == first)
            .expect("filed record");
        assert!(filed.actions.iter().all(|a| a.index != DISMISS_ACTION));
        let last = state
            .entries
            .iter()
            .find(|e| e.id == second)
            .expect("still open");
        assert!(
            last.actions.iter().all(|a| a.index != DISMISS_ALL_ACTION),
            "one open entry has nothing to dismiss all of"
        );
        // The last one goes with Dismiss all when it is asked from any open entry.
        let outcome = app.perform_message_act(crate::WindowId(0), second, DISMISS_ALL_ACTION);
        assert!(
            matches!(&outcome, MessageActOutcome::Performed { .. }),
            "{outcome:?}"
        );
        assert!(app.messages.digest().is_empty());
    }

    /// The rule's counter is said in `aterm.log` from EVERY posting path, once
    /// per change (review 2026-09-29, issue 11): a plain post that the door
    /// records.
    #[test]
    fn the_rules_counter_is_said_once_per_change() {
        use aterm_messages::need::{Ask, Reason, StandingWhy, Then};
        use aterm_messages::{Intent, Message, Severity, UpgradeWord, tags};
        let mut app = App::headless_for_test();
        let decline = |word| Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2.1.284".into(),
            word,
        };
        let classified = || {
            Message::new(
                tags::HARNESS,
                Severity::Warn,
                "Couldn't upgrade Claude in tab 1 yet",
            )
            .key("harness.upgrade.s-1")
            .action(decline(UpgradeWord::NotToday))
            .action(decline(UpgradeWord::Skip))
            .now(Ask::standing(
                StandingWhy::OnlyYou(Reason::new("its own work holds the move")),
                Then::step("Stop that work in the tab"),
            ))
        };
        app.post_message(classified());
        assert_eq!(
            app.demotions_logged, 1,
            "row 46's classified twin is recorded and said"
        );
        app.post_message(classified());
        assert_eq!(app.demotions_logged, 2);
        // A hold longer than two minutes on a message with no class is cut
        // and counted the same way.
        app.post_message(
            Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(
                aterm_messages::Hold::For(std::time::Duration::from_secs(600)),
            ),
        );
        assert_eq!(app.demotions_logged, 3, "an over-long hold is cut and said");
    }
}
