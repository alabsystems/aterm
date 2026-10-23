// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host side of the exceptions digest (`aterm_messages::exceptions`).

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

/// How long a rating survey the loop cannot close stands before it earns a digest line: an hour,
const SURVEY_ENTRY_AFTER: Duration = Horizon::SUPERVISOR_RESTARTS.duration();

#[cfg(test)]
thread_local! {
    /// How often the digest announced a change (the menu-bar item and Settings follow): a test
    static DIGEST_MOVES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Deferred exception entry waiting until its ripening horizon (`due`) to be raised.
#[derive(Debug)]
pub(crate) struct Ripening {
    key: String,
    due: Instant,
    entry: Entry,
}

/// How long a restarted supervisor must run without failing again before the failure is taken
const SUPERVISOR_STEADY: Duration = Duration::from_secs(30 * 60);

/// Settle window (`10m`) before the resume family hands its full state to the digest.
pub(crate) const RESUME_SETTLE: Duration = Duration::from_secs(10 * 60);

/// The `host_noted` marks that this process recorded a short volume, one per form of its line:
const DISK_NOTED: &str = "disk";
const DISK_CLEANUP_OFF_NOTED: &str = "disk:cleanup-off";

/// The `host_noted` marks of the usage tap's line ([`HostNote::UsageTap`]): one per tab whose tap
const USAGE_TAP_NOTED: &str = "usage-tap:";
const USAGE_TAP_DAY: &str = "usage-tap-day:";

/// When this process first looked at its exceptions, and whether it has since ended the
static LOOKING_SINCE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static ADOPTED_SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Per-tab supervisor failure state after exceeding its restart budget ([`HostNote::SupervisorStopped`]).
#[derive(Debug)]
pub(crate) struct SupervisorFail {
    /// Instant of the first failure in this episode.
    pub(crate) first: Instant,
    /// Instant of the latest failure.
    pub(crate) last: Instant,
    pub(crate) pause: Duration,
    pub(crate) why: String,
    /// Whether the digest entry has been raised.
    pub(crate) raised: bool,
}

/// Per-tab login notice story tracking the standing row and read notice keys.
#[derive(Debug)]
pub(crate) struct NoticeStory {
    head: aterm_messages::MessageId,
    back: Option<String>,
    /// Wall-clock timestamp (unix ms) of the story's latest notice or ask end.
    pub(crate) last: u64,
    /// The notices of this story the owner read (folded or dismissed) whose
    read: std::collections::BTreeMap<String, u64>,
}

/// Duration a login story remains active after its row leaves the glass.
pub(crate) const NOTICE_STORY_SPAN: Duration = aterm_messages::need::STANDING_QUIET;

impl NoticeStory {
    /// Whether the story goes on at wall `unix_ms`: its row still on the
    fn goes_on(&self, live: bool, unix_ms: u64) -> bool {
        live || unix_ms < self.last.saturating_add(span_ms(NOTICE_STORY_SPAN))
    }

    /// Whether `key` is a notice the owner read in this story, still quiet at
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
    /// The instant a run started after the latest failure has outlived it without failing again.
    fn steady_at(&self) -> Instant {
        self.last + self.pause + SUPERVISOR_STEADY
    }
}

impl App {
    /// Posts or updates an exception `entry` in the digest.
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
            PostOutcome::Restated => {
                aterm_log::debug!("exceptions: {:?} {}", posted.outcome, entry.key());
                self.digest_moved();
            }
            PostOutcome::Duplicate | PostOutcome::Suppressed | PostOutcome::Superseded(_) => {}
        }
    }

    /// Schedules `entry` to be raised only after it persists for `after`.
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

    /// Raises any deferred exceptions or supervisor failures whose horizon has arrived by `now`.
    pub(crate) fn ripen_exceptions(&mut self, now: Instant) {
        self.settle_supervisors(now);
        self.end_relaunch_lines_of_closed_tabs();
        let since = *LOOKING_SINCE.get_or_init(|| now);
        if now >= since + SUPERVISOR_STEADY && !ADOPTED_SWEPT.swap(true, Ordering::Relaxed) {
            self.sweep_adopted_supervisor_lines();
        }
        if !self.resume_swept {
            // The API outage is a level family too, stated only as tabs
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

    /// Earliest deadline when a ripening exception or held supervisor state next changes.
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

    /// A supervisor that failed past its budget is over once a run started after its latest
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

    /// Whether an exception under `key` is currently open or pending ripening.
    #[cfg(test)]
    pub(crate) fn owes_exception(&self, key: &str) -> bool {
        self.ripening.iter().any(|claim| claim.key == key)
            || self
                .messages
                .digest_keys_under(key)
                .iter()
                .any(|k| k == key)
    }

    /// Ends an open exception under `key` and cancels any pending ripening claim.
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

    /// Clears the exception under `key` as resolved (`Resolution::Cleared`).
    pub(crate) fn except_clear(&mut self, key: &str) {
        if self.except_end(key) {
            return;
        }
        if self
            .messages
            .forget_filing(key, wall_stamp_now(), Instant::now())
        {
            aterm_log::info!("exceptions: {key} filed earlier; its cause has ended");
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_UI_WAIT);
        }
    }

    /// Posts the removed-settings exception entry from config load diagnostics.
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

    /// Replaces the open set for a level-mode `family` with `entries`.
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

    /// Processes a [`HostNote`] from a harness host thread (`Wake::HostNote`).
    pub(crate) fn note_harness_host(&mut self, note: HostNote) {
        match note {
            HostNote::SupervisorFailing { sid, why, pause } => {
                let now = Instant::now();
                if let Some(fail) = self.supervisor_failing.get_mut(&sid) {
                    fail.last = now;
                    fail.pause = pause;
                    fail.why = why;
                } else {
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
            HostNote::ProgramCheckStuck { workers, limit } => {
                self.post_message(message_reporters::program_check_stuck(workers, limit));
            }
            HostNote::ProgramCheckUnstuck => {
                self.messages.resolve_key_prefix(
                    message_reporters::PROGRAM_CHECK_STUCK_KEY,
                    aterm_messages::Outcome::Ok,
                    Instant::now(),
                );
                let _ = self.messages.end_standing_quiet(
                    message_reporters::PROGRAM_CHECK_STUCK_KEY,
                    wall_stamp_now(),
                    Instant::now(),
                );
            }
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
                match standing
                    .as_ref()
                    .filter(|exception| exception.when == Urgency::Now && !exception.escalated)
                {
                    Some(exception) => self.raise_resume_ask(&sid, exception),
                    None => self.end_resume_ask(&sid),
                }
                match standing {
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
            HostNote::LimitOver { sid } => self.end_notice_story(&sid),
            HostNote::ResumeUnread { sid, agent, stall } => match stall {
                Some(stall) => {
                    // The second rung of the stall canary (90 minutes of
                    if self.host_noted.insert(format!("stalled:{sid}")) {
                        let spot = self.upgrade_spot(&sid);
                        let record = match stall {
                            StallSeen::Unread { minutes } => {
                                aterm_log::error!(
                                    "harness @{sid}: Claude's own title has said idle for \
                                     {minutes} min over a screen this build does not recognise"
                                );
                                message_reporters::stall_unread_record(&sid, &spot.place, minutes)
                            }
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
                self.note_ai_accounts_listed(matches!(
                    said,
                    AccountsSaid::Listed { .. } | AccountsSaid::LoginsUnread
                ));
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
            HostNote::AgentPublished { .. } => self.crash_journal.owe_agent_capture(),
            HostNote::UpgradeRecords { unlisted } => match unlisted {
                Some(dir) => self.except(&message_reporters::upgrade_records_unlisted_entry(&dir)),
                None => self.except_clear(message_reporters::UPGRADE_RECORDS_UNLISTED_KEY),
            },
            HostNote::UpgradeRecordsUnreadable { files } => {
                if files.is_empty() {
                    self.except_clear(message_reporters::UPGRADE_RECORDS_UNREADABLE_KEY);
                } else {
                    self.except(&message_reporters::upgrade_records_unreadable_entry(&files));
                }
            }
            HostNote::UpgradeRuns { sid } => {
                self.host_noted.remove(&format!("upgrade:{sid}"));
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
    fn note_relaunch(&mut self, sid: &str, said: RelaunchSay) {
        let [blocked, gone, lost] = message_reporters::relaunch_keys(sid);
        let spot = self.upgrade_spot(sid);
        match said {
            RelaunchSay::Ended => {
                self.host_noted.remove(&relaunch_told_key(sid));
                self.except_clear(&blocked);
                self.except_clear(&gone);
            }
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

    /// Publishes the current resume loop exceptions to the digest ([`Family::HarnessResume`]).
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

    /// A tab's resume loop needs the owner now and nothing of the loop's says
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
        if self
            .resume_stories
            .get(sid)
            .is_some_and(|story| !story.goes_on(self.messages.live(story.head).is_some(), wall))
        {
            self.resume_stories.remove(sid);
        }
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
        let head_read = head.and_then(|head| {
            let rec = self.messages.log().get(head)?;
            matches!(
                rec.retired(),
                Some(aterm_messages::Retired::Folded | aterm_messages::Retired::Dismissed)
            )
            .then_some((rec.key.clone()?, rec.retired_unix_ms?))
        });
        let id = self.post_message(msg);
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

    /// Tab `sid`'s lost login is back ([`HostNote::LoginBack`]): the story its
    fn close_notice_story(&mut self, sid: &str) {
        let Some(story) = self.resume_stories.remove(sid) else {
            return;
        };
        self.host_noted.remove(&resume_asked_key(sid));
        let live = self.messages.live(story.head).is_some();
        if !story.goes_on(live, wall_stamp_now().unix_ms) {
            aterm_log::info!("harness @{sid}: a login back after its story's span closes nothing");
            return;
        }
        if live {
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

    /// Tab `sid`'s usage or model limit ended with no sign-in that took
    fn end_notice_story(&mut self, sid: &str) {
        if self.resume_stories.remove(sid).is_some() {
            aterm_log::info!("harness @{sid}: the limit ended by itself; its notice reads unfixed");
        }
    }

    /// Withdraws tab `sid`'s live asks ([`message_reporters::notice_ask_key`]
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
    fn end_resume_ask(&mut self, sid: &str) {
        let ended = self.host_noted.remove(&resume_asked_key(sid));
        self.withdraw_resume_asks(sid, None);
        if ended && let Some(story) = self.resume_stories.get_mut(sid) {
            story.last = wall_stamp_now().unix_ms;
        }
    }

    /// Withdraw the asks an earlier process carried here that no loop of this one tells of, at the
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
        if let Some(id) = self
            .messages
            .live_by_key(message_reporters::RENEW_ASK_KEY)
            .map(|row| row.id)
        {
            let _ = self.withdraw_message(id);
        }
    }

    /// Posts the `palette.refused` exception when `allow_palette_set = false` blocks a palette change.
    pub(crate) fn note_palette_refused(&mut self) {
        self.except(&message_reporters::palette_refused_entry());
    }

    /// Posts the `render.<tab>.unrepaired` exception when a repaint nudge fails to heal `session`.
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

    /// Posts the Claude Code build-drift exception for `session` when its drawer image mismatches.
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

    /// Clears `render.<tab>.unrepaired` exceptions whose render epoch has healed.
    pub(crate) fn check_render_repairs(&mut self) {
        // Every turn of the event loop asks: with nothing held and the earlier
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

    /// Ends relaunch exceptions and pending claims for tabs that have closed.
    pub(crate) fn end_relaunch_lines_of_closed_tabs(&mut self) {
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

    /// Ends adopted supervisor exceptions once the current process has settled.
    pub(crate) fn sweep_adopted_supervisor_lines(&mut self) {
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
                // A relaunch line stands while this process's worker told of it and has not
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

    /// Global session ID (`s-…`) of pool session `local`, if registered.
    pub(crate) fn session_sid(&self, local: u64) -> Option<String> {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        store
            .by_local(local)
            .map(|handle| handle.sid.as_str().to_string())
    }

    /// Whether `session`'s render epoch `epoch` is still open and at risk. A gone session answers
    fn render_epoch_still_off(&self, session: u64, epoch: u64) -> bool {
        let Some(found) = self.pool.get(session) else {
            return false;
        };
        let timeline = match found.ctx.timeline.try_lock() {
            Ok(timeline) => timeline,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return true,
        };
        timeline.resize_epoch_at_risk(epoch)
    }

    /// Dismisses all open exceptions (`Dismiss all`), returning the number dismissed.
    pub(crate) fn dismiss_exceptions(&mut self) -> usize {
        let filed = self.messages.file_digest(wall_stamp_now(), Instant::now());
        if filed > 0 {
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_UI_WAIT);
            self.digest_moved();
        }
        filed
    }

    /// Dismisses the open exception with raw message ID `id` (`Dismiss`).
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

    /// Notifies Settings ▸ Messages and the menu-bar item after the digest changes.
    pub(crate) fn digest_moved(&mut self) {
        #[cfg(test)]
        DIGEST_MOVES.with(|c| c.set(c.get() + 1));
        self.publish_native_messages_state_if_due(Instant::now());
        self.mark_status_dirty();
        self.note_message_rule();
    }

    /// Logs the message center's rule-demotion counter when it advances.
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

    /// Folds in log lines written by the parent process before seamless-update commit.
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

    /// Posts the single digest entry when a dev build named `aterm.app` writes release setup.
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

/// The `host_noted` key that says this process told of a tab's relaunch line.
fn relaunch_told_key(sid: &str) -> String {
    format!("relaunch:{sid}")
}

/// The stem of [`resume_asked_key`].
const RESUME_ASKED: &str = "resume-ask:";

/// The `host_noted` key that says this process holds a tab's resume ask
fn resume_asked_key(sid: &str) -> String {
    format!("{RESUME_ASKED}{sid}")
}

/// Whether `key` is one of a tab's asks, whose own key is `base`
fn is_tab_ask_key(key: &str, base: &str) -> bool {
    key.strip_prefix(base)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Whether `key` is a `harness.supervisor.<subject>.not-back|gone|lost|
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

    /// The host's word that what a restore carries of a tab's agent moved owes
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

    /// The digest costs the terminal nothing (the Row Law, clause 1): post,
    #[test]
    fn a_digest_costs_no_regrid() {
        let mut app = App::headless_for_test();
        let message = message_regrids();
        let rows = app.message_band_rows;
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

    /// A dismiss click never waits on a writer that does not answer: the
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

    /// A script's dismissal moves the surfaces (`aterm ctl notice dismiss review`
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

    /// A dev build that writes the release's setup is one entry, said once.
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

    /// At Commit the successor folds the parent's log tail in: a `Dismiss all` the parent made
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

    /// One producer, one mode: the host refuses the wrong call, so a pass over the upgrade's rows
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

    /// The digest's own clock: the upgrade's looks are sent only when its rows change, so an entry
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

    /// A wording fault never ends a fact: a pass that still names the key keeps the open entry in
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
        app.except_replace(Family::Upgrade, &[]);
        assert!(app.messages.digest_deadline().is_some());
    }

    /// The page and the wire read one digest: an open entry reads the same words in Settings and
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

    /// The page's own dismiss buttons: every open entry offers `Dismiss` (files
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

        let (first, second) = (open[0].id, open[1].id);
        let outcome = app.perform_message_act(crate::WindowId(0), first, DISMISS_ACTION);
        assert!(
            matches!(&outcome, MessageActOutcome::Performed { .. }),
            "{outcome:?}"
        );
        assert_eq!(app.messages.digest().len(), 1);
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
        let outcome = app.perform_message_act(crate::WindowId(0), second, DISMISS_ALL_ACTION);
        assert!(
            matches!(&outcome, MessageActOutcome::Performed { .. }),
            "{outcome:?}"
        );
        assert!(app.messages.digest().is_empty());
    }

    /// The rule's counter is said in `aterm.log` from every posting path, once per change: here a
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
        app.post_message(
            Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(
                aterm_messages::Hold::For(std::time::Duration::from_secs(600)),
            ),
        );
        assert_eq!(app.demotions_logged, 3, "an over-long hold is cut and said");
    }
}
