// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` integration for the unified message center, band geometry sync, log persistence, and update/toolchain rows.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use aterm_messages::page::{self, act_feedback};
use aterm_messages::{
    ActionIndex, Carry, Decision, EchoKind, HOLD_ROUTE, Hold, Intent, Message, MessageId, Outcome,
    PROGRESS_GRACE, Restatement, Severity, WallStamp, drive, wire,
};

use crate::message_reporters::{PANE_DEVELOPER_TOOLS, PANE_FULL_DISK_ACCESS, log_did_not_open};
use crate::native_app::MessageActOutcome;
use crate::native_settings::SettingsRoute;
use crate::toolchain_words::{self, HookDialect, SnapshotWords};
use crate::update_words;
use crate::{App, WindowId};

/// Every center in this process shares one CSPRNG-seeded confirmation sequence.
pub(crate) fn new_center(
    log: aterm_messages::MessageLog,
    now: Instant,
) -> aterm_messages::MessageCenter {
    static SOURCE: OnceLock<Arc<aterm_messages::ConfirmationSource>> = OnceLock::new();
    let source = SOURCE.get_or_init(|| {
        Arc::new(aterm_messages::ConfirmationSource::from_entropy(|bytes| {
            aterm_uds::rand::fill(bytes).is_ok()
        }))
    });
    aterm_messages::MessageCenter::with_confirmations(log, now, Arc::clone(source))
}

pub(crate) const MESSAGES_FLUSH_UI_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

pub(crate) const MESSAGES_FLUSH_EXIT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(250);

#[cfg(test)]
pub(crate) use aterm_messages::page::MessageActionView;
pub(crate) use aterm_messages::page::{MessageView, MessagesState, tag_words};

struct PageDesk<'a> {
    app: &'a App,
    staged: Option<u64>,
    upgrade_now_s: u64,
}

impl<'a> PageDesk<'a> {
    fn of(app: &'a App) -> Self {
        let staged = app
            .staged_update_ready()
            .then(|| {
                app.native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .map(|s| s.build)
            })
            .flatten();
        Self {
            app,
            staged,
            upgrade_now_s: crate::upgrade_host::now_s(),
        }
    }
}

impl page::Host for PageDesk<'_> {
    fn now_unix_ms(&self) -> u64 {
        wall_stamp_now().unix_ms
    }

    fn log_folder(&self) -> Option<String> {
        self.app
            .messages_log
            .as_ref()
            .and_then(crate::messages_store::Writer::folder)
            .map(|dir| dir.display().to_string())
    }

    fn saved(&self) -> bool {
        self.app.messages_log.is_some()
    }

    fn utc_offset_s(&self) -> i64 {
        crate::presence::local_offset_s().unwrap_or(0)
    }

    fn staged_build(&self) -> Option<u64> {
        self.staged
    }

    fn is_regular_file(&self, path: &str) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
    }

    fn upgrade_takes(&self, tab: &str, to: &str, word: aterm_messages::UpgradeWord) -> bool {
        self.app
            .upgrade_view
            .offers(tab, to, word, self.upgrade_now_s)
    }

    fn upgrade_stands(&self, tab: &str) -> bool {
        self.app.upgrade_view.row_for(tab).is_some()
    }

    fn performs_navigation(&self) -> bool {
        true
    }

    fn sentence_lines(&self, key: Option<&str>, detail: &[String]) -> usize {
        crate::message_reporters::sentence_lines(key, detail)
    }
}

/// Witness token authorizing [`crate::notify::deliver_update_health_banner`].
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) struct HealthBannerVoice(());

#[cfg(test)]
thread_local! {
    static PTY_RESIZES_FOR_MESSAGES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static UPDATE_HEALTH_BANNERS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static UPDATER_ADOPTED_HEALTH: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn adopt_updater_announced_health(class: &'static str) {
    #[cfg(test)]
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().push(class));
    #[cfg(not(test))]
    aterm_update::adopt_announced_health(class);
}

/// Returns the count of window re-grids triggered by the message band on this thread (test-only).
#[cfg(test)]
pub(crate) fn message_regrids() -> u64 {
    PTY_RESIZES_FOR_MESSAGES.with(|c| c.get())
}

/// Returns the current wall-clock timestamp in Unix epoch milliseconds.
pub(crate) fn wall_stamp_now() -> WallStamp {
    let unix_ms = aterm_time::unix_millis().unwrap_or(0);
    WallStamp { unix_ms }
}

/// Builds a [`Restatement`] from `msg`'s fields for updating a live row in place.
pub(crate) fn restatement_of(msg: &Message) -> Restatement {
    Restatement {
        title: Some(msg.title.clone()),
        detail: Some(msg.detail.clone()),
        meter: Some(msg.meter.clone()),
        actions: Some(msg.actions.clone()),
        hold: Some(msg.hold),
        severity: Some(msg.severity),
        glyph: Some(msg.glyph),
        excerpt: Some(msg.excerpt),
        finished: Some(msg.finished.clone()),
        short_title: Some(msg.short_title.clone()),
    }
}

fn route_words(opened: bool, route: &str) -> Restatement {
    let caption = crate::consent_card::opened_settings_caption(opened, route);
    let (title, tail) = caption
        .split_once(" \u{2014} ")
        .unwrap_or((caption.as_str(), ""));
    let mut lines: Vec<String> = tail
        .split("; ")
        .filter(|clause| !clause.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(first) = lines.first_mut()
        && let Some(rest) = first.strip_prefix("System Settings \u{25b8} ")
    {
        *first = rest.to_string();
    }
    let title = if opened {
        let pane = route.rsplit(" \u{25b8} ").next().unwrap_or(route).trim();
        if pane.is_empty() {
            title.to_string()
        } else {
            format!("Enable aterm in {pane}")
        }
    } else {
        let did_not = "Settings did not open".to_string();
        match lines.first_mut() {
            Some(route) => format!("Open {}", std::mem::replace(route, did_not)),
            None => {
                lines.push(did_not);
                "Open System Settings".to_string()
            }
        }
    };
    let (severity, glyph) = if opened {
        (None, None)
    } else {
        let warn = aterm_messages::Severity::Warn;
        (Some(warn), Some(warn.default_glyph()))
    };
    Restatement {
        title: Some(title),
        detail: Some(lines),
        actions: Some(Vec::new()),
        hold: Some(Hold::For(HOLD_ROUTE)),
        excerpt: Some(opened),
        severity,
        glyph,
        ..Restatement::default()
    }
}

fn privacy_pane(pane: &str) -> Option<crate::menu::PrivacyPane> {
    match pane {
        PANE_FULL_DISK_ACCESS => Some(crate::menu::PrivacyPane::FullDiskAccess),
        PANE_DEVELOPER_TOOLS => Some(crate::menu::PrivacyPane::DeveloperTools),
        _ => None,
    }
}

fn admissible_log_path(path: &str, log_dir: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let p = Path::new(path);
    if !p.is_absolute() {
        return None;
    }
    let plain = p.components().all(|c| {
        matches!(
            c,
            Component::Prefix(_) | Component::RootDir | Component::Normal(_)
        )
    });
    if !plain || !p.starts_with(log_dir) {
        return None;
    }
    std::fs::symlink_metadata(p)
        .is_ok_and(|m| m.file_type().is_file())
        .then(|| p.to_path_buf())
}

fn openable_link(url: &str) -> bool {
    aterm_messages::link_host(url).is_some() && crate::is_safe_url(url)
}

fn open_path_external(path: &str) -> bool {
    let Some(log_dir) = crate::logging::log_dir() else {
        return false;
    };
    let Some(target) = admissible_log_path(path, &log_dir) else {
        return false;
    };
    crate::qos::spawn_lane_thread("aterm-open-path", crate::qos::Role::Background, move || {
        #[cfg(target_os = "macos")]
        let status = aterm_uds::spawnfd::command("/usr/bin/open")
            .arg("-t")
            .arg(&target)
            .status();
        #[cfg(target_os = "linux")]
        let status = crate::startup_token::for_a_press(aterm_uds::spawnfd::command("xdg-open"))
            .arg(&target)
            .status();
        #[cfg(windows)]
        let status = aterm_uds::spawnfd::command("notepad.exe")
            .arg(&target)
            .status();
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        let status: std::io::Result<std::process::ExitStatus> =
            Err(std::io::Error::other("no opener on this platform"));
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => aterm_log::warn!("open {}: exited {s}", target.display()),
            Err(error) => aterm_log::warn!("open {}: {error}", target.display()),
        }
    })
    .is_ok()
}

impl App {
    /// Posts `msg` into the message center at the current timestamp, logs it, and syncs the band.
    pub(crate) fn post_message(&mut self, msg: Message) -> MessageId {
        self.post_message_at(msg, wall_stamp_now())
    }

    /// Posts `msg` into the message center with an explicit ingress `stamp` and syncs the band.
    pub(crate) fn post_message_at(&mut self, msg: Message, stamp: WallStamp) -> MessageId {
        let id = self.post_unsynced(msg, stamp);
        self.sync_messages();
        id
    }

    /// Posts `msg` as a `Hold::LogOnly` record without showing a row on the glass.
    pub(crate) fn record_message(&mut self, msg: Message) -> MessageId {
        self.post_unsynced(msg.hold(Hold::LogOnly), wall_stamp_now())
    }

    /// Replaces config warning rows after a full reload and reconciles the font-warning row.
    pub(crate) fn replace_config_messages_after_font_work(
        &mut self,
        msgs: impl IntoIterator<Item = Message>,
        family_admitted: bool,
        fixed_as: &std::collections::BTreeMap<String, String>,
    ) {
        let reloads = |key: &String| {
            key.starts_with("config.")
                && (family_admitted || key != crate::message_reporters::KEY_FONT_FAMILY)
        };
        let scope: Vec<String> = self
            .messages
            .live_rows()
            .filter_map(|l| l.msg.key.clone())
            .filter(reloads)
            .collect();
        let mut rederived: std::collections::BTreeSet<String> = self
            .messages
            .log()
            .records()
            .filter(|r| r.retired_at.is_some())
            .filter_map(|r| r.key.clone())
            .filter(reloads)
            .collect();
        rederived.extend(scope.iter().cloned());
        self.replace_config_set(&scope, &rederived, msgs.into_iter().collect(), fixed_as);
    }

    /// Replaces config warning rows when no font family was admitted.
    #[cfg(test)]
    pub(crate) fn replace_config_messages(&mut self, msgs: impl IntoIterator<Item = Message>) {
        self.replace_config_messages_after_font_work(
            msgs,
            false,
            &std::collections::BTreeMap::new(),
        );
    }

    /// Replaces only the config warning rows whose keys are in `keys`.
    pub(crate) fn replace_config_messages_keyed(
        &mut self,
        keys: &[&str],
        msgs: impl IntoIterator<Item = Message>,
    ) {
        let scope: Vec<String> = keys.iter().map(|key| (*key).to_string()).collect();
        let rederived = scope.iter().cloned().collect();
        self.replace_config_set(
            &scope,
            &rederived,
            msgs.into_iter().collect(),
            &std::collections::BTreeMap::new(),
        );
    }

    fn replace_config_set(
        &mut self,
        scope: &[String],
        rederived: &std::collections::BTreeSet<String>,
        msgs: Vec<Message>,
        fixed_as: &std::collections::BTreeMap<String, String>,
    ) {
        let now = Instant::now();
        let mut resolved = 0usize;
        for key in rederived {
            if !msgs.iter().any(|m| m.key.as_deref() == Some(key.as_str())) {
                self.config_cleared.insert(key.clone());
                resolved +=
                    self.messages
                        .fix_closed_as(key, fixed_as.get(key).map(String::as_str), now);
            }
        }
        for key in scope {
            let Some(id) = self.messages.live_by_key(key).map(|l| l.id) else {
                continue;
            };
            let Some(next) = msgs.iter().find(|m| m.key.as_deref() == Some(key.as_str())) else {
                // GONE from the reload's set: the problem is fixed, and the
                // record takes the words this reload saw it end in, else the
                // ones its family declared for any fix.
                if let Some(words) = fixed_as.get(key) {
                    self.messages.restate(
                        id,
                        Restatement {
                            finished: Some(Some(words.clone())),
                            ..Restatement::default()
                        },
                        now,
                    );
                }
                resolved += usize::from(self.messages.resolve(id, Outcome::Ok, now));
                continue;
            };
            if next.hold == Hold::LogOnly {
                self.messages.restate(
                    id,
                    Restatement {
                        finished: Some(None),
                        ..Restatement::default()
                    },
                    now,
                );
                resolved += usize::from(self.messages.resolve(id, Outcome::Ok, now));
            }
        }
        self.post_config_set(resolved, msgs);
    }

    fn post_config_set(&mut self, resolved: usize, msgs: Vec<Message>) {
        let stamp = wall_stamp_now();
        let mut posted = 0usize;
        for msg in msgs {
            if let Some(key) = msg.key.as_deref()
                && self
                    .messages
                    .live_by_key(key)
                    .is_some_and(|l| l.msg.title == msg.title && l.msg.detail == msg.detail)
            {
                continue;
            }
            let msg = if msg.hold != Hold::LogOnly && self.config_repeat(&msg) {
                msg.record()
            } else {
                msg
            };
            let row = msg.hold != Hold::LogOnly;
            if row && let Some(key) = msg.key.as_deref() {
                self.config_cleared.remove(key);
            }
            self.post_unsynced(msg, stamp);
            posted += usize::from(row);
        }
        if resolved > 0 || posted > 0 {
            self.sync_messages();
        }
    }

    fn config_repeat(&self, msg: &Message) -> bool {
        use aterm_messages::Retired;
        let Some(key) = msg.key.as_deref() else {
            return false;
        };
        if self.config_cleared.contains(key) {
            return false;
        }
        let as_record = crate::message_reporters::config_record(msg);
        self.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key))
            .is_some_and(|r| {
                r.retired_at.is_some()
                    && r.title == msg.title
                    && (r.detail == msg.detail || r.detail == as_record.detail)
                    && matches!(
                        r.retired(),
                        Some(
                            Retired::Folded
                                | Retired::Dismissed
                                | Retired::Unseen
                                | Retired::Evicted
                                | Retired::Stale
                                | Retired::Recorded
                        )
                    )
            })
    }

    /// Returns whether any live row's title or detail contains `needle`.
    #[cfg(test)]
    pub(crate) fn has_live_message(&self, needle: &str) -> bool {
        self.messages.live_rows().any(|l| {
            l.msg.title.contains(needle) || l.msg.detail.iter().any(|d| d.contains(needle))
        })
    }

    /// Posts and logs `msg` at `stamp` without running a band settle pass.
    pub(crate) fn post_unsynced(&mut self, msg: Message, stamp: WallStamp) -> MessageId {
        self.post_unsynced_at(msg, stamp, Instant::now())
    }

    fn post_unsynced_at(&mut self, msg: Message, stamp: WallStamp, now: Instant) -> MessageId {
        let posted = self.messages.post(msg, stamp, now);
        if let Some(live) = self.messages.live(posted.id) {
            aterm_log::info!("message {} {}: {}", posted.id, live.msg.tag, live.msg.title);
        }
        self.note_message_rule();
        posted.id
    }

    /// Updates a live row in place with `r` without emitting a new log line.
    pub(crate) fn restate_message(&mut self, id: MessageId, r: Restatement) -> bool {
        let revision = self.messages.revision();
        let live = self.messages.restate(id, r, Instant::now());
        self.note_message_rule();
        if live && self.messages.revision() != revision {
            self.sync_messages();
        }
        live
    }

    /// Rewords a log-only record in place and appends a `reworded` log line.
    pub(crate) fn reword_record(&mut self, id: MessageId, msg: &Message) -> bool {
        self.messages.reword_record(id, msg)
    }

    /// Marks row or record `id` as superseded by `by`.
    pub(crate) fn supersede_record(&mut self, id: MessageId, by: MessageId) -> bool {
        let live = self.messages.live(id).is_some();
        let replaced = self.messages.supersede_record(id, by, Instant::now());
        if replaced && live {
            self.sync_messages();
        }
        replaced
    }

    /// Returns whether a Settings tab is active in the frontmost window.
    pub(crate) fn settings_in_front(&self) -> bool {
        self.frontmost_window
            .and_then(|wid| self.active_native_view(wid))
            .is_some_and(|(instance, _)| {
                self.native_runtime
                    .app(instance)
                    .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings)
            })
    }

    /// Records which config warning families in `scope` are currently shown inline in Settings.
    pub(crate) fn note_config_settings_shows(&mut self, scope: &[&str], shown: &[&'static str]) {
        for key in scope {
            self.config_settings_shows.remove(*key);
        }
        self.config_settings_shows
            .extend(shown.iter().map(|key| (*key).to_string()));
    }

    /// Downgrades live config warning rows to records when Settings opens and shows them inline.
    pub(crate) fn record_config_rows_settings_shows(&mut self) {
        if self.keep_config_rows_settings_shows_as_records() > 0 {
            self.sync_messages();
        }
    }

    fn keep_config_rows_settings_shows_as_records(&mut self) -> usize {
        if self.config_settings_shows.is_empty() || !self.settings_in_front() {
            return 0;
        }
        let rows: Vec<(MessageId, Message)> = self
            .messages
            .live_rows()
            .filter(|row| {
                row.msg
                    .key
                    .as_deref()
                    .is_some_and(|key| self.config_settings_shows.contains(key))
            })
            .map(|row| (row.id, row.msg.clone()))
            .collect();
        let now = Instant::now();
        let mut kept = 0usize;
        for (id, msg) in rows {
            kept += usize::from(self.messages.keep_as_record(
                id,
                &crate::message_reporters::config_record(&msg),
                now,
            ));
        }
        kept
    }

    /// Converts live row `id` into a log-only record in place with `msg`'s words.
    pub(crate) fn keep_as_record(&mut self, id: MessageId, msg: &Message) -> bool {
        let kept = self.messages.keep_as_record(id, msg, Instant::now());
        if kept {
            self.sync_messages();
        }
        kept
    }

    /// Resolves live row `id` with `outcome` and syncs the band.
    pub(crate) fn resolve_message(&mut self, id: MessageId, outcome: Outcome) -> bool {
        let resolved = self.messages.resolve(id, outcome, Instant::now());
        if resolved {
            self.sync_messages();
        }
        resolved
    }

    /// Withdraws live row `id` without an outcome and syncs the band.
    pub(crate) fn withdraw_message(&mut self, id: MessageId) -> bool {
        let withdrawn = self.messages.withdraw(id, Instant::now());
        if withdrawn {
            self.sync_messages();
        }
        withdrawn
    }

    /// Resolves live row `id` as `Answered` with `label` and syncs the band.
    pub(crate) fn answer_message(&mut self, id: MessageId, label: &str) -> bool {
        let answered = self.messages.answer(id, label, Instant::now());
        if answered {
            self.sync_messages();
        }
        answered
    }

    /// Settles expired rows, updates band geometry, persists log lines, and schedules repaints.
    pub(crate) fn sync_messages(&mut self) {
        let now = Instant::now();
        if !self.settle_messages(now) {
            self.refresh_band_hover();
        }
        self.publish_native_messages_state_if_due(now);
        self.request_redraw_all_windows();
    }

    /// Settles messages at `now` without requesting a repaint, returning whether band visuals changed.
    pub(crate) fn settle_messages(&mut self, now: Instant) -> bool {
        let strip = self.settle_strip_fall(now);
        if self.messages.settle_digest(wall_stamp_now(), now) {
            self.digest_moved();
        }
        self.ripen_exceptions(now);
        self.keep_owed_rows(now);
        let commit = self.band_commit(now);
        let step = drive::settle_and_commit(&mut self.messages, commit);
        let committed = self.regrid_message_band(step.regrid, now);
        let persist = !self.message_persist_frozen();
        let (lines, wire) =
            drive::drain_and_pace(&mut self.messages, &mut self.wire_gate, now, persist);
        self.append_message_lines(&lines);
        let changed = step.glass_changed || committed || wire || strip;
        if changed {
            self.refresh_band_hover();
        }
        changed
    }

    fn persist_messages(&mut self) {
        let lines = self.messages.drain_shelved_for_persist();
        self.append_message_lines(&lines);
    }

    fn append_message_lines(&self, lines: &[(aterm_messages::LogLine, aterm_messages::Shelf)]) {
        if let Some(writer) = &self.messages_log {
            writer.append(lines);
        }
    }

    /// Flushes pending message log lines with a 60-second test timeout.
    #[cfg(test)]
    pub(crate) fn flush_messages_log(&mut self) {
        assert!(
            self.flush_messages_log_within(std::time::Duration::from_secs(60)),
            "the messages writer landed nothing within a minute"
        );
    }

    /// Drains pending message log lines to the writer and waits up to `within` for disk flush.
    pub(crate) fn flush_messages_log_within(&mut self, within: std::time::Duration) -> bool {
        self.persist_messages();
        let Some(writer) = &self.messages_log else {
            return true;
        };
        let landed = writer.flush_within(within);
        if !landed {
            aterm_log::warn!(
                "messages log: the writer was still behind after {} ms; its queued lines \
                 land when it catches up, and not if this process ends first",
                within.as_millis()
            );
        }
        landed
    }

    /// Closes uncarried parent rows as `Superseded` when committing a seamless handoff.
    #[cfg(unix)]
    pub(crate) fn close_uncarried_messages(&mut self) -> usize {
        self.messages
            .close_uncarried(&self.handoff_carried_messages, Instant::now())
    }

    /// Closes all remaining open rows as `Interrupted` during a clean process exit.
    pub(crate) fn close_messages_at_exit(&mut self) -> usize {
        if self.message_persist_frozen() {
            return 0;
        }
        self.let_go_every_lifted_held();
        let closed = self.messages.quit(Instant::now());
        self.flush_messages_log_within(MESSAGES_FLUSH_EXIT_WAIT);
        closed
    }

    /// Returns whether message log persistence is frozen awaiting seamless-handoff Commit.
    pub(crate) const fn message_persist_frozen(&self) -> bool {
        self.incoming_handoff_pending
    }

    /// Commits the current message band row count to window geometry, re-gridding if it changed.
    pub(crate) fn sync_message_band_rows(&mut self, now: Instant) -> bool {
        let commit = self.band_commit(now);
        let regrid = drive::commit(&mut self.messages, commit);
        self.regrid_message_band(regrid, now)
    }

    fn band_commit(&self, now: Instant) -> drive::Commit {
        drive::Commit {
            now,
            frozen: self.message_holds_frozen(),
            afford: drive::afford(
                self.windows
                    .keys()
                    .filter_map(|&wid| self.chrome_commit_grid_rows(wid))
                    .filter(|&(_, windowed)| windowed)
                    .map(|(rows, _)| rows),
                self.message_band_rows,
            ),
            reserved: self.message_band_rows,
            floor: self.band_regrow_floor(),
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "BandRise",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "BandFall",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "BandTaken",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    fn regrid_message_band(&mut self, regrid: Option<u16>, now: Instant) -> bool {
        let Some(rows) = regrid else {
            return false;
        };
        if aterm_messages::chrome::is_rise(self.message_band_rows, rows) {
            self.note_chrome_rise_everywhere(now);
        }
        self.message_band_rows = rows;
        #[cfg(test)]
        PTY_RESIZES_FOR_MESSAGES.with(|c| c.set(c.get() + 1));
        self.regrid_for_chrome_rows();
        true
    }

    /// Returns whether a seamless handoff is currently freezing message holds and band re-gridding.
    pub(crate) fn message_holds_frozen(&self) -> bool {
        self.update_handoff_parked() || self.incoming_handoff_pending
    }

    /// Returns the earliest instant when the message band next requires an event-loop wake.
    pub(crate) fn messages_deadline(&self) -> Option<Instant> {
        let state =
            drive::state_deadline(&self.messages, &self.wire_gate, self.message_holds_frozen());
        [
            state,
            self.messages_publish_due_at(),
            self.messages.digest_deadline(),
            self.ripening_deadline(),
            self.strip_fall_due(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Drains messages queued in the pre-`App` inbox and reconciles the exceptions digest row.
    pub(crate) fn drain_message_inbox(&mut self) {
        let workstation_finished = crate::message_inbox::take_workstation_setup_finished();
        let queued = crate::message_inbox::take_queued();
        let exceptions = crate::message_inbox::take_queued_exceptions();
        let folders = crate::spawn_folder::take_noted();
        if queued.is_empty() && exceptions.is_empty() && folders.is_empty() && !workstation_finished
        {
            return;
        }
        for word in &exceptions {
            match word {
                crate::message_inbox::ExceptionWord::Raise(entry) => self.except(entry),
                crate::message_inbox::ExceptionWord::Clear(key) => self.except_clear(key),
            }
        }
        for m in queued {
            match m.restored {
                Some(row) => self.post_restored_row(row, m.stamp),
                None => {
                    self.post_unsynced(m.message, m.stamp);
                }
            }
        }
        if workstation_finished {
            self.except_clear(&crate::message_reporters::workstation_accounts_entry().key());
            self.except_clear(&crate::message_reporters::workstation_setup_failure_entry().key());
        }
        let _ = self.keep_config_rows_settings_shows_as_records();
        if !folders.is_empty() {
            self.fold_folder_faults(&folders);
        }
        self.sync_messages();
    }

    fn fold_folder_faults(&mut self, fresh: &[(String, crate::spawn_folder::Fault)]) {
        for fault in crate::spawn_folder::Fault::ALL {
            let mut dirs = fresh
                .iter()
                .filter(|(_, why)| *why == fault)
                .map(|(dir, _)| dir.clone())
                .peekable();
            if dirs.peek().is_none() {
                continue;
            }
            let key = crate::message_reporters::folder_row_key(fault);
            let named = self.folder_rows.of(fault);
            match self.messages.live_by_key(key) {
                None => *named = crate::spawn_folder::Named::default(),
                Some(live) if named.dirs.is_empty() => {
                    *named =
                        crate::message_reporters::folder_row_named(&live.msg).unwrap_or_default();
                }
                Some(_) => {}
            }
            dirs.for_each(|dir| named.add(dir));
            let row = crate::message_reporters::folder_row(fault, named);
            self.post_unsynced(row, wall_stamp_now());
        }
    }

    fn apply_press_applies(&self, id: MessageId, build: u64) -> bool {
        let offered = self
            .messages
            .live(id)
            .is_some_and(|l| l.msg.actions.contains(&Intent::ApplyUpdate { build }));
        let staged_ready = self.staged_update_ready();
        let handoff_available =
            self.seamless_handoff_unavailable().is_none() && !self.kept_image_waits_for_terminal();
        Self::update_bar_press_applies(offered, staged_ready, handoff_available)
    }

    /// Executes a plain-data [`Intent`] triggered from row `id` in window `wid`.
    pub(crate) fn perform_intent(&mut self, wid: WindowId, id: MessageId, intent: Intent) -> bool {
        let now = Instant::now();
        let navigation = matches!(
            intent,
            Intent::OpenSettings { .. }
                | Intent::OpenConfigEditor { .. }
                | Intent::OpenPath { .. }
                | Intent::OpenLink { .. }
                | Intent::ReviewVaultSync
        );
        let performed = self.perform_intent_inner(wid, id, intent, now);
        if navigation && performed && self.messages.mark_seen(id, now) {
            self.sync_messages();
        }
        performed
    }

    fn perform_intent_inner(
        &mut self,
        wid: WindowId,
        id: MessageId,
        intent: Intent,
        now: Instant,
    ) -> bool {
        match intent {
            Intent::Details => {
                if self.messages.mark_seen(id, now) {
                    self.sync_messages();
                }
                if self.consent_card_message == Some(id) {
                    self.consent_card.on_owner_acted();
                }
                self.open_messages_entry(wid, Some(id))
            }
            Intent::OpenSettings { route } => match SettingsRoute::from_path(&route) {
                Some(route) => self.open_settings_tab_in_window(wid, route),
                None => {
                    aterm_log::warn!("message {id}: no Settings route at {route:?}");
                    false
                }
            },
            Intent::OpenConfigEditor { line: _ } => {
                match self.ensure_and_open_config_editor_at_in_window(wid, None) {
                    Ok(_) => true,
                    Err(message) => {
                        aterm_log::warn!("message {id}: aterm.toml not opened: {message}");
                        false
                    }
                }
            }
            Intent::OpenPath { path } => {
                let opened = open_path_external(&path);
                if !opened {
                    aterm_log::warn!("message {id}: refused to open {path:?}");
                    self.post_message(log_did_not_open(&path));
                }
                opened
            }
            Intent::OpenSystemPane { pane } => {
                let Some(pane) = privacy_pane(&pane) else {
                    aterm_log::warn!("message {id}: no system pane {pane:?}");
                    return false;
                };
                let opened = match pane {
                    crate::menu::PrivacyPane::DeveloperTools => self
                        .hand_devtools_to_owner(crate::devtools_helper::Origin::Digest)
                        .brings_owner(),
                    crate::menu::PrivacyPane::FullDiskAccess
                    | crate::menu::PrivacyPane::FilesAndFolders => {
                        crate::native_settings::ConsentGestures::for_instance(self.headless)
                            .open_settings(pane)
                            .opened()
                    }
                };
                let route = crate::menu::privacy_settings_path_words(pane);
                self.restate_message(id, route_words(opened, route));
                if pane == crate::menu::PrivacyPane::FullDiskAccess {
                    if opened {
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "the owner's own press of the digest entry"
                        )]
                        self.fda_page_opened_by_owner(crate::devtools_helper::Origin::Digest);
                    }
                    self.note_macos_access_settings_opened(now);
                }
                opened
            }
            Intent::OpenLink { url } => {
                if !openable_link(&url) {
                    aterm_log::warn!("message {id}: refused to open a link");
                    return false;
                }
                if self.headless {
                    return false;
                }
                crate::app_mouse::open_url_external(&url);
                true
            }
            Intent::ApplyUpdate { build } => {
                if self.apply_press_applies(id, build) {
                    self.apply_update_or_details();
                    true
                } else {
                    self.open_settings_tab_in_window(wid, SettingsRoute::SoftwareUpdate)
                }
            }
            Intent::NotNow { decision } => {
                match decision {
                    Decision::FileAccess => {
                        self.record_macos_access_marker(crate::consent_card::Marker::NotNow);
                        self.consent_card.settle();
                        self.consent_card_message = None;
                    }
                }
                true
            }
            Intent::NewWindow => match self.proxy.as_ref() {
                Some(proxy) => proxy.send_event(crate::Wake::CreateWindow).is_ok(),
                None => false,
            },
            Intent::StopPaste { session } => self.stop_paste(session),
            Intent::EndRecovered => self.end_recovered_sessions() > 0,
            Intent::ReviewVaultSync => self.review_vault_sync_in_window(wid),
            Intent::ShowTab { .. } => false,
            Intent::ShowSession {
                window, session, ..
            } => {
                let local = {
                    let store = self.store.read().unwrap_or_else(|p| p.into_inner());
                    store
                        .by_sid(&aterm_session::SessionId::new(session))
                        .map(|h| h.local_id)
                };
                local.is_some_and(|local| {
                    self.focus_session_preferring(Some(WindowId(window)), local)
                })
            }
            Intent::AgentUpgrade { tab, to, word } => {
                self.upgrade_view
                    .offers(&tab, &to, word, crate::upgrade_host::now_s())
                    && self.send_upgrade_word(crate::upgrade_host::WordAsk::new(tab, to, word))
            }
        }
    }

    /// Opens or focuses Settings ▸ Messages in `wid`, optionally scrolling to entry `id`.
    pub(crate) fn open_messages_entry(&mut self, wid: WindowId, id: Option<MessageId>) -> bool {
        if !self.open_settings_tab_in_window(wid, SettingsRoute::Messages) {
            return false;
        }
        let Some(id) = id else {
            return true;
        };
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        let _ = self.compiled_native_ui(wid);
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(crate::native_settings::MESSAGES_SELECT),
            value: Some(crate::native_app::SemanticInput::Text(id.raw().to_string())),
        });
        self.dispatch_native_view_event(wid, view, action).is_ok()
    }

    /// Opens Settings ▸ Messages at entry `id` in the frontmost (or newly created) window.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn open_messages_entry_anywhere(&mut self, id: u64) -> Option<WindowId> {
        let wid = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())?;
        self.open_messages_entry(wid, MessageId::from_raw(id))
            .then_some(wid)
    }

    /// Invokes action `act_index` on message entry `id` from Settings ▸ Messages.
    pub(crate) fn perform_message_act(
        &mut self,
        wid: WindowId,
        id: u64,
        action: u8,
    ) -> MessageActOutcome {
        let Some(id) = MessageId::from_raw(id) else {
            return MessageActOutcome::Gone;
        };
        if action == page::DISMISS_ACTION && self.messages.digest_entry(id).is_some() {
            return if self.dismiss_exception(id) {
                MessageActOutcome::Performed {
                    feedback: "Filed in the log".to_string(),
                }
            } else {
                MessageActOutcome::Gone
            };
        }
        if action == page::DISMISS_ALL_ACTION && self.messages.digest_entry(id).is_some() {
            let filed = self.dismiss_exceptions();
            return if filed > 0 {
                MessageActOutcome::Performed {
                    feedback: format!("Filed {filed} in the log"),
                }
            } else {
                MessageActOutcome::Gone
            };
        }
        let intent = if self.messages.live(id).is_some() {
            let intent = self.messages.act(id, ActionIndex(action), Instant::now());
            self.sync_messages();
            intent
        } else if self.messages.digest_entry(id).is_some() {
            let intent =
                self.messages
                    .act_digest(id, ActionIndex(action), wall_stamp_now(), Instant::now());
            self.digest_moved();
            intent
        } else {
            self.messages.log().get(id).and_then(|rec| {
                rec.actions
                    .get(usize::from(action))
                    .filter(|intent| !intent.is_ask() && rec.still_offers(intent))
                    .cloned()
            })
        };
        let Some(intent) = intent else {
            return MessageActOutcome::Gone;
        };
        let applies = match &intent {
            Intent::ApplyUpdate { build } => self.apply_press_applies(id, *build),
            _ => false,
        };
        let performed = self.perform_intent(wid, id, intent.clone());
        let applies = applies
            || (performed
                && intent == Intent::ReviewVaultSync
                && self.native_runtime.system_state_shows_vault_sync());
        let feedback = act_feedback(&intent, performed, applies);
        if performed {
            MessageActOutcome::Performed { feedback }
        } else {
            MessageActOutcome::Refused { feedback }
        }
    }

    /// Evaluates a control-socket `messages` read query against the message center.
    pub(crate) fn read_messages(&self, q: &wire::ReadQuery) -> Vec<String> {
        wire::message_rows(
            &self.messages,
            q,
            wall_stamp_now().unix_ms,
            &crate::control::pct_encode,
        )
    }

    /// Handles a control-socket `notice` request on the main thread.
    pub(crate) fn take_notice(&mut self, req: wire::NoticeRequest) -> String {
        self.take_notice_within(req, MESSAGES_FLUSH_UI_WAIT)
    }

    fn take_notice_within(
        &mut self,
        req: wire::NoticeRequest,
        flush_within: std::time::Duration,
    ) -> String {
        if self.update_handoff_parked() {
            aterm_log::info!("notice: refused while parked for an update");
            return wire::switching_reply();
        }
        if let wire::NoticeRequest::Act { id, press } = &req {
            let enc = &crate::control::pct_encode;
            let target = wire::press_target(
                &self.messages,
                &mut self.wire_gate,
                *id,
                press,
                enc,
                Instant::now(),
            );
            let (index, label) = match target {
                Ok(target) => target,
                Err(line) => return line,
            };
            let wid = self.frontmost_window.unwrap_or(WindowId(0));
            let performed = match self.perform_message_act(wid, id.raw(), index.0) {
                MessageActOutcome::Performed { .. } => true,
                MessageActOutcome::Refused { .. } => false,
                MessageActOutcome::Gone => return wire::no_live_reply(*id),
            };
            if performed {
                aterm_log::info!("notice act {id} {label}");
            }
            return wire::acted_reply(label, performed, enc);
        }
        let digest_before = self.messages.digest_revision();
        let applied = wire::apply(
            &mut self.messages,
            &mut self.wire_gate,
            req,
            wall_stamp_now(),
            Instant::now(),
        );
        let digest_changed = self.messages.digest_revision() != digest_before;
        let log_dirty = self.messages.log().pending_len() != 0;
        if digest_changed || log_dirty {
            self.flush_messages_log_within(flush_within);
        }
        if digest_changed {
            self.digest_moved();
        } else if log_dirty {
            self.publish_native_messages_state_if_due(Instant::now());
        }
        if applied.paint == wire::Paint::Now {
            self.sync_messages();
        }
        if applied.note {
            aterm_log::info!("notice: {}", applied.reply);
        }
        applied.reply
    }

    /// Projects the current message center state into a [`MessagesState`] for Settings ▸ Messages.
    pub(crate) fn messages_state(&self) -> MessagesState {
        MessagesState::of(&self.messages, &PageDesk::of(self))
    }
}

impl App {
    /// Handles a click on the body of band row `id` in window `wid` by opening its details.
    pub(crate) fn press_message_body(&mut self, wid: WindowId, id: MessageId) {
        if self.messages.live(id).is_none() {
            return;
        }
        let _ = self.perform_intent(wid, id, Intent::Details);
    }
}

impl App {
    /// Returns the [`HookDialect`] for the user's configured shell.
    pub(crate) fn hook_dialect(&self) -> HookDialect {
        HookDialect::from(crate::spawn::detect_spawn_shell(
            self.session_factory.shell_override.as_deref(),
        ))
    }

    /// Posts the `managed-current:` toolchain record with the count of tabs running older builds.
    pub(crate) fn post_managed_current(&mut self, text: &str) -> bool {
        self.upgrade_view.note_managed_text(text);
        let tabs = self.tabs_behind();
        let unhooked = self.unhooked_clause();
        let hook = self.hook_dialect();
        let Some(msg) = toolchain_words::managed_current_for(text, tabs, unhooked.as_deref(), hook)
        else {
            return false;
        };
        let versions = toolchain_words::managed_items(text);
        if let Some((said, id)) = &self.managed_current_recorded
            && *said == versions
            && self
                .messages
                .log()
                .get(*id)
                .is_some_and(|rec| rec.retired() == Some(&aterm_messages::Retired::Recorded))
        {
            let id = *id;
            return self.reword_record(id, &msg);
        }
        let id = self.record_message(msg);
        self.managed_current_recorded = Some((versions, id));
        true
    }

    /// Posts a `harness`-tagged log record from `aterm ctl appnotice harness`.
    pub(crate) fn record_harness_note(&mut self, text: &str) -> bool {
        let msg = crate::message_reporters::harness_note(text);
        if self.harness_note_recorded.as_deref() == Some(msg.title.as_str()) {
            return false;
        }
        self.harness_note_recorded = Some(msg.title.clone());
        self.record_message(msg);
        true
    }

    /// Posts log records for each item in a `machine-settings:` marker.
    pub(crate) fn post_machine_settings(&mut self, text: &str) {
        for msg in toolchain_words::machine_settings(text) {
            self.record_message(msg);
        }
    }

    /// Posts or clears the foreign-tools exceptions digest entry from a `foreign:` marker.
    pub(crate) fn note_foreign_tools(&mut self, body: &str) {
        let Some((scope, items)) = toolchain_words::foreign_report(body) else {
            aterm_log::info!("atpkg foreign: a report this build cannot read: {body}");
            return;
        };
        let entry = toolchain_words::foreign_tools_entry(scope, &items);
        if items.is_empty() {
            self.except_clear(&entry.key());
        } else {
            self.except(&entry);
        }
    }

    /// Posts or clears the C-toolchain exceptions digest entry from a `c-toolchain:` marker.
    pub(crate) fn note_c_toolchain(&mut self, body: &str) {
        use toolchain_words::{CToolchain, c_toolchain_entry};
        let (missing, broken) = (
            c_toolchain_entry(CToolchain::Missing),
            c_toolchain_entry(CToolchain::Broken),
        );
        match body.trim() {
            atpkg::cli::C_TOOLCHAIN_MISSING => {
                self.except_clear(&broken.key());
                self.except(&missing);
            }
            atpkg::cli::C_TOOLCHAIN_BROKEN => {
                self.except_clear(&missing.key());
                self.except(&broken);
            }
            atpkg::cli::C_TOOLCHAIN_READY => {
                self.except_clear(&missing.key());
                self.except_clear(&broken.key());
            }
            other => {
                aterm_log::info!("atpkg c-toolchain: a verdict this build cannot read: {other}");
            }
        }
    }

    /// Opens the toolchain progress row when a first-run `net-starting:` marker arrives.
    pub(crate) fn announce_toolchain_pass(&mut self, detail: &str, first_run: bool) {
        if !first_run {
            // A person's verb: its busy row takes the size the announcement names until the plan
            // lands. While it queues, an announcement is the holder's, not the size of the
            // person's wait.
            if let Some(verb) = self.toolchain_person
                && !self.toolchain_person_waiting
            {
                let sized = toolchain_words::announced(detail);
                let mut row = crate::message_reporters::packages_verb_row(verb);
                row.detail.clone_from(&sized.detail);
                if let (Some(m), Some(said)) = (row.meter.as_mut(), sized.meter.as_ref()) {
                    m.stats.clone_from(&said.stats);
                    m.load = said.load;
                }
                self.show_person_row(row, true);
            }
            return;
        }
        self.toolchain_row_latched = true;
        self.toolchain_first_run = true;
        let announced = toolchain_words::announced(detail);
        match self.toolchain_ahead_row.take() {
            Some(id) if self.restate_message(id, restatement_of(&announced)) => {}
            _ => {
                self.post_message(announced);
            }
        }
    }

    /// Opens the first-run toolchain progress row ahead of child spawn (`Wake::PkgFirstRunAhead`).
    pub(crate) fn open_first_run_ahead(&mut self) -> bool {
        self.toolchain_ahead_carry = true;
        if self.toolchain_row_latched {
            return false;
        }
        self.toolchain_row_latched = true;
        self.toolchain_first_run = true;
        self.toolchain_ahead_row = Some(self.post_message(toolchain_words::announced("")));
        true
    }

    /// Withdraws the pre-opened first-run toolchain row if no network pass runs.
    pub(crate) fn withdraw_first_run_ahead(&mut self) {
        self.toolchain_ahead_carry = false;
        if self.toolchain_ahead_row.take().is_none() {
            return;
        }
        self.toolchain_row_latched = false;
        self.toolchain_first_run = false;
        self.toolchain_first_run_short = None;
        self.retire_live_toolchain(None);
        if let Some(verb) = self.toolchain_person {
            let row = if self.toolchain_person_waiting {
                crate::message_reporters::packages_waiting_row(verb)
            } else {
                crate::message_reporters::packages_verb_row(verb)
            };
            self.show_person_row(row, false);
        }
    }

    /// Applies a `progress.json` snapshot from the `atpkg` progress tailer.
    pub(crate) fn apply_toolchain_snapshot(
        &mut self,
        snap: Option<&crate::PkgProgressSnapshot>,
        first_run: bool,
    ) {
        let live = self
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| {
                let failure = l.msg.tag == aterm_messages::tags::PACKAGES;
                (l.id, l.msg.hold.is_held() && !failure, failure)
            });
        if let Some((_, true, _)) = live {
            return;
        }
        let live_id = live.filter(|(_, _, failure)| !failure).map(|(id, ..)| id);
        let heavy = !first_run
            && snap.is_some_and(|s| {
                s.running && s.file.overall.bytes_total >= toolchain_words::HEAVY_PASS_BYTES
            });
        let person = self.toolchain_person;
        let waiter = aterm_messages::Waiter::of(first_run || person.is_some());
        let paints = waiter.takes_row(heavy) || self.toolchain_row_latched;
        let peak = live_id.and(self.toolchain_pass_peak.clone());
        let routine = !first_run && person != Some(crate::message_reporters::PackagesVerb::Install);
        match toolchain_words::snapshot_words(snap, peak.as_ref(), routine) {
            SnapshotWords::Vanished | SnapshotWords::Over => match person {
                _ if self.toolchain_row_latched => {}
                None => self.retire_live_toolchain(None),
                Some(verb) if self.toolchain_person_waiting => {
                    self.restore_person_row(crate::message_reporters::packages_waiting_row(verb))
                }
                Some(_) => {}
            },
            SnapshotWords::Quiet => {}
            SnapshotWords::Live { message, pass_id } => {
                if !paints || self.holder_read_not_the_persons(snap, first_run) {
                    return;
                }
                if first_run || person.is_none() {
                    self.toolchain_row_latched = true;
                    self.toolchain_first_run |= first_run;
                }
                let message = match person.filter(|_| !self.toolchain_row_latched) {
                    Some(verb) => Box::new((*message).finished_as(verb.finished_work())),
                    None => message,
                };
                self.toolchain_pass_peak = message
                    .meter
                    .as_ref()
                    .and_then(|m| m.fill_permille)
                    .map(|fill| (pass_id, fill));
                match live_id {
                    Some(id) => {
                        self.restate_message(id, restatement_of(&message));
                    }
                    None if !first_run => {
                        self.post_message(message.reveal_after(PROGRESS_GRACE));
                    }
                    None => {
                        self.post_message(*message);
                    }
                }
            }
        }
    }

    fn retire_live_toolchain(&mut self, echo: Option<EchoKind>) {
        self.toolchain_pass_peak = None;
        let Some(id) = self
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .filter(|l| !l.msg.hold.is_held())
            .map(|l| l.id)
        else {
            return;
        };
        match echo {
            Some(kind) => {
                if self.messages.withdraw_with(id, kind, Instant::now()) {
                    self.sync_messages();
                }
            }
            None => {
                self.withdraw_message(id);
            }
        }
    }

    /// Records the final outcome message of a toolchain pass and resolves its live row.
    pub(crate) fn record_toolchain_outcome(&mut self, msg: Message) {
        if !self.toolchain_row_latched && self.toolchain_person.is_none() {
            self.retire_live_toolchain(None);
        }
        self.record_message(msg);
    }

    /// Latches a short-ended first-run warning (`seed-failed:`, `net-failed:`, `seed-partial:`).
    pub(crate) fn note_toolchain_first_run_short(
        &mut self,
        how: toolchain_words::FirstRunShort,
        cause: &str,
    ) {
        if self.toolchain_row_latched && self.toolchain_first_run {
            self.toolchain_first_run_short = Some(toolchain_words::first_run_short(how, cause));
        }
    }

    /// Folds any remaining live toolchain progress row when the `atpkg` child process exits.
    pub(crate) fn toolchain_pass_ended(&mut self, clean: bool) {
        // The LANE'S row outlives its launch seed (`Self::open_first_run_ahead`): the
        // update that follows takes it over, and that child's exit is the one fold.
        if std::mem::take(&mut self.toolchain_ahead_carry) {
            return;
        }
        let unannounced = self.toolchain_ahead_row.take().is_some();
        let echo = if !clean {
            Some(EchoKind::Fault)
        } else if unannounced {
            None
        } else {
            Some(EchoKind::Complete)
        };
        let latched = std::mem::take(&mut self.toolchain_row_latched);
        let first_run = std::mem::take(&mut self.toolchain_first_run);
        let short = self.toolchain_first_run_short.take();
        if let Some(verb) = self.toolchain_person {
            // A person's verb is waiting (queued behind this child, or running beside its exit):
            // the row is theirs. A latched lane row ends as the lane's work did, and the person's
            // row comes back after the grace if their verb is still running.
            if latched {
                self.retire_live_toolchain(echo);
                match short {
                    Some(msg) if !clean && first_run => {
                        self.post_message(msg);
                    }
                    _ if self.toolchain_person_waiting => self.show_person_row(
                        crate::message_reporters::packages_waiting_row(verb),
                        false,
                    ),
                    _ => self
                        .show_person_row(crate::message_reporters::packages_verb_row(verb), false),
                }
            }
            return;
        }
        self.retire_live_toolchain(echo);
        if !clean
            && first_run
            && let Some(msg) = short
        {
            self.post_message(msg);
        }
    }

    /// Handles progress and completion events for a user-initiated Settings ▸ Packages action.
    pub(crate) fn person_pass(&mut self, event: PersonPass) {
        use crate::message_reporters::{packages_verb_row, packages_waiting_row};
        match event {
            PersonPass::Began(verb) => {
                self.toolchain_person = Some(verb);
                self.toolchain_person_waiting = false;
                self.toolchain_holder_pid = None;
                self.show_person_row(packages_verb_row(verb), false);
            }
            PersonPass::Waiting => {
                if let Some(verb) = self.toolchain_person {
                    self.toolchain_person_waiting = true;
                    self.show_person_row(packages_waiting_row(verb), true);
                }
            }
            PersonPass::Running => {
                let waited = std::mem::take(&mut self.toolchain_person_waiting);
                if let Some(verb) = self.toolchain_person
                    && waited
                    && !self.toolchain_row_latched
                {
                    self.restore_person_row(packages_verb_row(verb));
                }
            }
            PersonPass::Ended(echo) => {
                self.toolchain_person_waiting = false;
                self.toolchain_holder_pid = None;
                if self.toolchain_person.take().is_none() || self.toolchain_row_latched {
                    return;
                }
                self.retire_live_toolchain(Some(echo));
            }
        }
    }

    fn show_person_row(&mut self, row: Message, restate: bool) {
        let live = self
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| {
                let failure = l.msg.tag == aterm_messages::tags::PACKAGES;
                let filled = l
                    .msg
                    .meter
                    .as_ref()
                    .is_some_and(|m| m.fill_permille.is_some());
                (l.id, l.msg.hold.is_held() && !failure, failure, filled)
            });
        match live {
            None | Some((_, _, true, _)) => {
                self.toolchain_pass_peak = None;
                self.post_message(row);
            }
            Some((id, false, false, false)) if restate => {
                self.restate_message(id, restatement_of(&row));
            }
            Some(_) => {}
        }
    }

    fn restore_person_row(&mut self, row: Message) {
        self.toolchain_pass_peak = None;
        let live = self
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .filter(|l| !l.msg.hold.is_held() && l.msg.tag != aterm_messages::tags::PACKAGES)
            .map(|l| l.id);
        match live {
            Some(id) => {
                self.restate_message(id, restatement_of(&row));
            }
            None => self.show_person_row(row, false),
        }
    }

    fn holder_read_not_the_persons(
        &mut self,
        snap: Option<&crate::PkgProgressSnapshot>,
        first_run: bool,
    ) -> bool {
        if self.toolchain_person.is_none() {
            return false;
        }
        let pid = snap.and_then(|s| s.file.pid);
        if self.toolchain_person_waiting {
            if pid.is_some() {
                self.toolchain_holder_pid = pid;
            }
            return self.toolchain_row_latched && self.toolchain_first_run && !first_run;
        }
        pid.is_some() && pid == self.toolchain_holder_pid
    }
}

/// Event reported by the packages worker for a user-initiated Settings ▸ Packages action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonPass {
    Began(crate::message_reporters::PackagesVerb),
    Waiting,
    Running,
    Ended(EchoKind),
}

/// State of the live `update.progress` band row (`App::update_flow`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateFlow {
    pub(crate) id: MessageId,
    pub(crate) version: String,
    pub(crate) phase: FlowPhase,
}

/// Lifecycle phase of the live `update.progress` band row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlowPhase {
    Downloading,
    Checking,
    Staged { build: u64 },
    Installing,
    Finishing,
}

impl App {
    /// Returns `App::update_flow` only while its message row is still live on the band.
    pub(crate) fn live_update_flow(&self) -> Option<&UpdateFlow> {
        self.update_flow
            .as_ref()
            .filter(|flow| self.messages.live(flow.id).is_some())
    }

    /// Posts `msg` as the live `update.progress` row in `phase`.
    pub(crate) fn post_update_row(
        &mut self,
        msg: Message,
        version: &str,
        phase: FlowPhase,
    ) -> MessageId {
        self.resolve_held_row_under(update_words::KEY_PROGRESS);
        let id = self.post_message(msg);
        self.update_flow = Some(UpdateFlow {
            id,
            version: version.to_string(),
            phase,
        });
        id
    }

    /// Resolves any held row under `key` before posting a replacement row.
    pub(crate) fn resolve_held_row_under(&mut self, key: &str) {
        let held = self
            .messages
            .live_by_key(key)
            .filter(|l| l.msg.hold.is_held())
            .map(|l| (l.id, l.msg.severity));
        if let Some((id, severity)) = held {
            let outcome = if severity >= Severity::Warn {
                Outcome::Warn
            } else {
                Outcome::Ok
            };
            self.messages.resolve(id, outcome, Instant::now());
        }
    }

    /// Restates the live `update.progress` row in place and optionally advances its [`FlowPhase`].
    pub(crate) fn restate_update_flow(&mut self, r: Restatement, phase: Option<FlowPhase>) -> bool {
        let Some(id) = self.live_update_flow().map(|flow| flow.id) else {
            return false;
        };
        if let (Some(phase), Some(flow)) = (phase, self.update_flow.as_mut()) {
            flow.phase = phase;
        }
        self.restate_message(id, r)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_carried_health",
            action = "Recover",
            project = "aterm_gui::messages_host::carried_health_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_carried_health",
            action = "Announce",
            project = "aterm_gui::messages_host::carried_health_conformance::project"
        )
    )]
    /// Routes an legacy `(title, body)` update-health warning to [`Self::note_update_health_as`].
    pub(crate) fn note_update_health(&mut self, title: &str, body: &str) -> bool {
        // The Linux lane's wait for a start is no health warning of any kind.
        if title == aterm_update::INSTALL_AWAITS_START_TITLE {
            self.note_install_awaits_start(body);
            return false;
        }
        if title == aterm_update::INSTALL_WAIT_OVER_TITLE {
            self.end_install_awaits_start(body);
            return false;
        }
        if title == aterm_update::HEALTH_RECOVERED_TITLE {
            aterm_log::info!("update-health: {title}: {body}");
            let proof = HealthProof::healed_by_ledger(body);
            self.heal_update_health_where(|kind| proof.answers_from_the_ledger(kind));
            return false;
        }
        if title == aterm_update::HEALTH_RESTATED_TITLE {
            aterm_log::info!("update-health: {title}: {body}");
            return false;
        }
        if title == aterm_update::INSTALL_CRASH_LOOP_TITLE {
            let kind = update_words::HealthKind::Install;
            self.update_health_latched
                .retain(|latched| *latched != kind);
            return self.note_update_health_as(
                kind,
                aterm_update::health_failing_title("apply"),
                body,
            );
        }
        self.note_update_health_as(
            update_words::HealthKind::of_updater_title(title),
            title,
            body,
        )
    }

    /// Posts or records an update-health warning of `kind` once per launch until healed.
    pub(crate) fn note_update_health_as(
        &mut self,
        kind: update_words::HealthKind,
        title: &str,
        body: &str,
    ) -> bool {
        if self.update_health_latched.contains(&kind) {
            aterm_log::info!("update-health: already said this launch: {title}: {body}");
            return false;
        }
        self.update_health_latched.push(kind);
        aterm_log::warn!("update-health: {title}: {body}");
        if !self.update_handoff_in_flight() {
            self.retire_update_installing();
        }
        let msg = update_words::health_warning(kind, title, body);
        let now = Instant::now();
        let older: Vec<MessageId> = self
            .messages
            .live_rows()
            .filter(|l| {
                l.msg.key.as_deref() == Some(update_words::KEY_HEALTH)
                    && update_words::HealthKind::of_row(&l.msg) == Some(kind)
            })
            .map(|l| l.id)
            .collect();
        for id in older {
            self.messages.withdraw(id, now);
        }
        self.remember_update_health_said(HealthSaid::of(kind, &msg, body));
        self.post_message(msg.clone());
        self.owe_unless_seen(msg);
        self.claim_update_health_entry(kind, body);
        true
    }

    fn note_install_awaits_start(&mut self, body: &str) {
        aterm_log::info!(
            "update: {}: {body}",
            aterm_update::INSTALL_AWAITS_START_TITLE
        );
        self.say_owed_unless_seen(update_words::install_awaits_start(body));
    }

    fn end_install_awaits_start(&mut self, version: &str) {
        aterm_log::info!(
            "update: {}: {version}",
            aterm_update::INSTALL_WAIT_OVER_TITLE
        );
        self.withdraw_owed_row(update_words::KEY_AWAITS_START);
    }

    /// Posts `msg` and queues it as owed if no window currently has a visible band.
    pub(crate) fn say_owed_unless_seen(&mut self, msg: Message) {
        self.post_message(msg.clone());
        self.owe_unless_seen(msg);
    }

    /// Withdraws any live or owed row under `key`.
    pub(crate) fn withdraw_owed_row(&mut self, key: &str) {
        self.forget_owed(key);
        if let Some(id) = self.messages.live_by_key(key).map(|l| l.id) {
            self.withdraw_message(id);
        }
    }

    fn owe_unless_seen(&mut self, msg: Message) {
        let Some(key) = msg.key.clone() else {
            return;
        };
        if let Some(owed) = self.rows_owed.iter_mut().find(|owed| owed.key == key) {
            owed.msg = msg;
        } else if !self.any_band_on_screen() {
            self.rows_owed.push(OwedRow {
                key,
                msg,
                seen: false,
            });
        }
    }

    fn reword_owed(&mut self, key: &str, msg: &Message) {
        if let Some(owed) = self.rows_owed.iter_mut().find(|owed| owed.key == key) {
            owed.msg = msg.clone();
        }
    }

    fn forget_owed(&mut self, key: &str) {
        self.rows_owed.retain(|owed| owed.key != key);
    }

    fn any_band_on_screen(&self) -> bool {
        self.windows.values().any(|ws| band_on_screen(ws))
    }

    fn keep_owed_rows(&mut self, now: Instant) {
        if self.rows_owed.is_empty() || self.message_holds_frozen() {
            return;
        }
        let on_screen = self.any_band_on_screen();
        let mut owed_rows = std::mem::take(&mut self.rows_owed);
        owed_rows.retain_mut(|owed| {
            let live: Vec<MessageId> = self
                .messages
                .live_rows()
                .filter(|l| l.msg.key.as_deref() == Some(owed.key.as_str()))
                .map(|l| l.id)
                .collect();
            if live.is_empty() {
                // Shown on screen and gone: its hold ran out where it was seen,
                // or the person put it away.
                if owed.seen {
                    return false;
                }
                if on_screen && self.messages.has_live_capacity() {
                    aterm_log::info!(
                        "message: left unseen, posted again for a visible band: {}",
                        owed.msg.title
                    );
                    let id = self.post_unsynced_at(owed.msg.clone(), wall_stamp_now(), now);
                    owed.seen = self.messages.live(id).is_some_and(|live| !live.is_queued());
                }
                return true;
            }
            let showing = on_screen
                && live
                    .iter()
                    .any(|id| self.messages.live(*id).is_some_and(|row| !row.is_queued()));
            if showing && owed.seen {
                return true;
            }
            let hold = Restatement {
                hold: Some(owed.msg.hold),
                ..Restatement::default()
            };
            for id in live {
                self.messages.restate(id, hold.clone(), now);
                self.messages.restart_patience(id, now);
            }
            if showing {
                aterm_log::info!(
                    "message: said while no window was on screen, shown now: {}",
                    owed.msg.title
                );
                self.note_message_rule();
            }
            owed.seen = showing;
            true
        });
        self.rows_owed = owed_rows;
    }

    fn claim_update_health_entry(&mut self, kind: update_words::HealthKind, body: &str) {
        let now_unix = i64::try_from(wall_stamp_now().unix_ms / 1000).unwrap_or(i64::MAX);
        self.except_after(
            &update_words::update_health_entry(kind, body),
            update_words::health_entry_delay(body, now_unix),
        );
    }

    /// Restates an active update-health warning when its remedy changes.
    pub(crate) fn restate_update_health(
        &mut self,
        kind: update_words::HealthKind,
        title: &str,
        body: &str,
    ) {
        aterm_log::info!("update-health: restated: {title}: {body}");
        let msg = update_words::health_warning(kind, title, body);
        let words = Restatement {
            title: Some(msg.title.clone()),
            detail: Some(msg.detail.clone()),
            ..Restatement::default()
        };
        let mut restated = false;
        for id in self.live_update_health(|live| live == kind) {
            restated |= self.restate_message(id, words.clone());
        }
        self.reword_owed(kind.key(), &msg);
        self.remember_update_health_said(HealthSaid::of(kind, &msg, body));
        if !restated {
            self.record_message(msg);
        }
    }

    /// Delivers the OS notification banner for an update-health warning at most once per 24 hours.
    pub(crate) fn post_update_health_banner(
        &self,
        kind: update_words::HealthKind,
        title: &str,
        body: &str,
    ) -> bool {
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.build);
        let remedy = match self.apply_retry_for(staged) {
            crate::update_apply_trouble::ApplyRetry::NeedsPerson(remedy)
                if kind == update_words::HealthKind::Install =>
            {
                remedy
            }
            _ => {
                aterm_log::info!(
                    "update-health: no desktop banner: nothing here needs a person now"
                );
                return false;
            }
        };
        if !self.config.desktop_alerts_or_default() {
            aterm_log::info!("update-health: desktop banner not posted: desktop_alerts is off");
            return false;
        }
        #[cfg(test)]
        {
            let banner = update_words::health_notification_body(body, remedy);
            let title = update_words::health_notification_title(title);
            UPDATE_HEALTH_BANNERS.with(|posted| posted.borrow_mut().push((title, banner)));
            true
        }
        #[cfg(all(target_os = "macos", not(test)))]
        {
            let banner = update_words::health_notification_body(body, remedy);
            let title = update_words::health_notification_title(title);
            let center = self.notice_center;
            let own_alerts = std::sync::Arc::clone(&self.notify_own_alerts);
            std::thread::spawn(move || {
                crate::notify::deliver_update_health_banner(
                    HealthBannerVoice(()),
                    center,
                    &own_alerts,
                    &title,
                    &banner,
                );
            });
            true
        }
        #[cfg(all(not(target_os = "macos"), not(test)))]
        {
            let _ = (title, body, remedy);
            false
        }
    }

    /// Resolves update-health warnings answered by `proof`.
    pub(crate) fn heal_update_health(&mut self, proof: HealthProof) {
        self.heal_update_health_where(|kind| proof.answers(kind));
    }

    /// Resolves update-health warnings matching `answered` and clears their digest entries.
    pub(crate) fn heal_update_health_where(
        &mut self,
        answered: impl Fn(update_words::HealthKind) -> bool,
    ) {
        self.update_health_latched.retain(|kind| !answered(*kind));
        self.rows_owed
            .retain(|owed| !update_words::HealthKind::of_key(&owed.key).is_some_and(&answered));
        for id in self.live_update_health(&answered) {
            self.resolve_message(id, Outcome::Warn);
        }
        for kind in update_words::HealthKind::ALL {
            if answered(kind) {
                self.except_clear(&update_words::update_health_entry(kind, "").key());
            }
        }
        self.record_update_health_healed(answered);
    }

    fn remember_update_health_said(&mut self, said: HealthSaid) {
        match self
            .update_health_said
            .iter_mut()
            .find(|entry| entry.kind == said.kind)
        {
            Some(entry) => *entry = said,
            None => self.update_health_said.push(said),
        }
    }

    /// Logs a healed record for each remembered update-health warning matching `healed`.
    pub(crate) fn record_update_health_healed(
        &mut self,
        healed: impl Fn(update_words::HealthKind) -> bool,
    ) {
        let (answered, standing): (Vec<_>, Vec<_>) = std::mem::take(&mut self.update_health_said)
            .into_iter()
            .partition(|said| healed(said.kind));
        self.update_health_said = standing;
        for said in answered {
            self.record_message(update_words::health_recovered(&said.title, &said.line));
        }
    }

    /// Returns live update-health warning `(id, kind)` pairs matching `which`.
    pub(crate) fn live_update_health(
        &self,
        which: impl Fn(update_words::HealthKind) -> bool,
    ) -> Vec<MessageId> {
        self.messages
            .live_rows()
            .filter(|l| update_words::HealthKind::of_row(&l.msg).is_some_and(&which))
            .map(|l| l.id)
            .collect()
    }

    /// Resolves the update flow row and logs `Updated to aterm <version>` once the new build runs.
    pub(crate) fn post_update_landed(&mut self, version: &str, build: u64, repainted: usize) {
        self.heal_update_health(HealthProof::Landed);
        let took = self
            .update_verified
            .take()
            .filter(|(verified, _)| *verified == build)
            .map(|(_, at)| Instant::now().saturating_duration_since(at));
        let live = self.live_update_flow().map(|flow| flow.id).or_else(|| {
            self.messages
                .live_by_key(update_words::KEY_PROGRESS)
                .filter(|l| matches!(l.msg.hold, Hold::Live { .. }))
                .map(|l| l.id)
        });
        self.update_flow = None;
        if let Some(id) = live {
            self.resolve_message(id, Outcome::Ok);
        }
        for key in [update_words::KEY_OUTCOME, update_words::KEY_WAITS_FOR_YOU] {
            if let Some(id) = self.messages.live_by_key(key).map(|l| l.id) {
                self.messages.withdraw(id, Instant::now());
            }
        }
        self.update_wait_said = false;
        self.record_message(update_words::landed(version, build, repainted, took));
        if repainted > 0 {
            aterm_log::info!("update landed: {repainted} tab(s) adopted onto a blank screen");
        }
    }

    /// Returns the count of tabs repainted onto a blank screen during seamless handoff adoption.
    pub(crate) const fn seamless_repainted_tabs(&self) -> usize {
        self.handoff_repainted_tabs
    }

    /// Posts the staged-update decision row once for a staged build when manual confirmation is required.
    pub(crate) fn ensure_staged_decision_in(
        &mut self,
        build: u64,
        posture: crate::update_words::ApplyPosture,
    ) {
        let Some(version) = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .filter(|stage| stage.build == build)
            .map(|stage| stage.version.clone())
        else {
            return;
        };
        if !update_words::staged_is_decision(Some(posture))
            || self.staged_update_row().is_some()
            || self.staged_said == Some((build, posture))
        {
            return;
        }
        self.staged_said = Some((build, posture));
        self.post_update_row(
            update_words::staged(&version, build, Some(posture)),
            &version,
            FlowPhase::Staged { build },
        );
    }

    /// Returns the [`MessageId`] of the live flow row while it is in [`FlowPhase::Staged`].
    pub(crate) fn staged_update_row(&self) -> Option<MessageId> {
        self.live_update_flow()
            .filter(|flow| matches!(flow.phase, FlowPhase::Staged { .. }))
            .map(|flow| flow.id)
    }

    /// Retires the live staged-update row as `Ok` (when delivered) or withdrawn.
    pub(crate) fn retire_staged_update_row(&mut self, delivered: bool) {
        if let Some(id) = self.staged_update_row() {
            if delivered {
                self.resolve_message(id, Outcome::Ok);
            } else {
                self.update_flow = None;
                self.withdraw_message(id);
            }
        }
    }

    /// Logs the `switch_started` record and flushes `messages.log` before activating an update.
    #[cfg(unix)]
    pub(crate) fn record_update_switch_started(
        &mut self,
        build: u64,
        same_image: bool,
        automatic: bool,
    ) {
        if self.note_update_switch_started(build, same_image, automatic) {
            self.flush_messages_log_within(MESSAGES_FLUSH_UI_WAIT);
        }
    }

    /// Logs the `switch_started` record without flushing `messages.log` (used by cold exec).
    #[cfg(unix)]
    pub(crate) fn note_update_switch_started(
        &mut self,
        build: u64,
        same_image: bool,
        automatic: bool,
    ) -> bool {
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.version.clone()));
        let target = if same_image {
            None
        } else {
            staged.as_ref().map(|(_, version)| version.clone())
        };
        let target_build = if same_image {
            build
        } else {
            staged.as_ref().map_or(build, |(b, _)| *b)
        };
        if self.update_switch_recorded_for == Some(target_build)
            && (automatic || self.update_switch_on_record.is_some())
        {
            return false;
        }
        self.update_switch_recorded_for = Some(target_build);
        self.update_switch_on_record = Some(target.clone());
        aterm_log::info!(
            "update switch started: target {:?} (build {target_build}) from build {build}",
            target
        );
        let posted_row = self.update_row_before_install.is_none()
            && self
                .live_update_flow()
                .is_some_and(|flow| flow.phase == FlowPhase::Installing);
        if !posted_row {
            self.record_message(update_words::switch_started(
                target.as_deref(),
                crate::build_info::version_display(),
            ));
        }
        true
    }

    /// Logs the `switch_stopped` record when an update activation attempt aborts.
    #[cfg(unix)]
    pub(crate) fn record_update_switch_stopped(&mut self, routine: bool, why: &str) {
        let Some(target) = self.update_switch_on_record.take() else {
            return;
        };
        self.record_message(update_words::switch_stopped(
            target.as_deref(),
            crate::build_info::version_display(),
            routine,
            why,
        ));
    }
}

/// Returns whether window `ws` is visible on screen and capable of showing the message band.
pub(crate) fn band_on_screen(ws: &crate::WindowState) -> bool {
    let shown = ws.pending_reveal.is_none() && !ws.occluded;
    #[cfg(test)]
    {
        crate::work_counts::on_screen_asked();
        if ws.band_on_screen_for_test {
            return shown;
        }
    }
    let minimized =
        |w: &aterm_window::window::Window| cfg!(windows) && w.is_minimized() == Some(true);
    shown && ws.os_window.as_deref().is_some_and(|w| !minimized(w))
}

impl App {
    /// Returns the animation [`aterm_messages::Look`] for window `wid`'s message band.
    pub(crate) fn band_look(&self, wid: WindowId) -> aterm_messages::Look {
        use crate::motion::{MotionEffect, SeriousEffect};
        let (focused, on_screen) = self
            .windows
            .get(&wid)
            .map_or((false, false), |ws| (ws.focused, band_on_screen(ws)));
        let effect = MotionEffect::BandProgress;
        let motion_allowed = self
            .effect_policy(effect, self.motion_focus(wid, focused))
            .animate(effect)
            && self
                .serious_mode_policy()
                .allows(SeriousEffect::BandProgress);
        drive::look(drive::LookIn {
            motion_allowed,
            on_screen,
            frozen: self.message_holds_frozen(),
            forced: crate::chrome_band::forced_chrome().is_some(),
        })
    }

    /// Returns the earliest instant when any visible window's message band needs an animation frame.
    pub(crate) fn band_motion_deadline(&self, now: Instant) -> Option<Instant> {
        drive::motion_deadline_over(
            &self.messages,
            self.message_band_rows,
            self.message_holds_frozen(),
            now,
            self.windows
                .iter()
                .filter(|(_, ws)| band_on_screen(ws))
                .map(|(wid, ws)| {
                    (
                        &ws.band,
                        (self.band_lay)(usize::from(ws.cols)),
                        self.band_look(*wid),
                    )
                }),
        )
    }

    /// Returns the visible windows whose message band is due for an animation frame at `now`.
    pub(crate) fn band_motion_due(&self, now: Instant) -> Vec<WindowId> {
        drive::due_views(
            &self.messages,
            self.message_band_rows,
            self.message_holds_frozen(),
            now,
            self.windows
                .iter()
                .filter(|(_, ws)| band_on_screen(ws))
                .map(|(wid, ws)| {
                    (
                        *wid,
                        &ws.band,
                        (self.band_lay)(usize::from(ws.cols)),
                        self.band_look(*wid),
                    )
                }),
        )
    }

    /// Prepares window `wid`'s message band layout and animation frame at `now`.
    pub(crate) fn prepare_band_motion(&mut self, wid: WindowId, now: Instant) -> u64 {
        let look = self.band_look(wid);
        self.prepare_band_motion_with(wid, now, look)
    }

    /// Prepares window `wid`'s message band frame at `now` with an explicit [`aterm_messages::Look`].
    pub(crate) fn prepare_band_motion_with(
        &mut self,
        wid: WindowId,
        now: Instant,
        look: aterm_messages::Look,
    ) -> u64 {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        let lay = (self.band_lay)(usize::from(ws.cols));
        ws.band.prepare(&self.messages, &lay, now, look)
    }
}

impl App {
    /// Captures live rows and undrained log lines for the seamless-handoff manifest.
    #[cfg(any(unix, test))]
    pub(crate) fn carried_messages(&self) -> crate::session_store::MessagesCarry {
        let mut carry = crate::session_store::MessagesCarry::from(&self.messages.carried());
        carry.update_health_said = self.carried_health_said();
        carry.captured_unix_ms = Some(wall_stamp_now().unix_ms);
        carry
    }

    /// Captures unanswered update-health warnings for the seamless-handoff manifest.
    #[cfg(any(unix, test))]
    pub(crate) fn carried_health_said(&self) -> Vec<crate::session_store::CarriedHealthSaid> {
        self.update_health_said
            .iter()
            .map(|said| crate::session_store::CarriedHealthSaid {
                kind: said.kind.key().to_string(),
                title: said.title.clone(),
                line: said.line.clone(),
                body: said.body.clone(),
                owed: self
                    .rows_owed
                    .iter()
                    .any(|owed| owed.key == said.kind.key()),
            })
            .collect()
    }

    /// Reloads `messages.log` at successor Commit to incorporate records written by a late-parking parent.
    #[cfg(unix)]
    pub(crate) fn reload_messages_log_at_commit(&mut self) {
        let Some(path) = self
            .messages_log
            .as_ref()
            .and_then(crate::messages_store::Writer::folder)
            .map(|dir| dir.join(crate::messages_store::FILE_NAME))
        else {
            return;
        };
        let disk = crate::messages_store::load_tail(&path, crate::messages_store::TAIL_BYTES).log;
        self.messages.merge_reloaded_log(&disk);
        let ended = self
            .messages
            .end_carried_rows_the_parent_ended(&disk, Instant::now());
        if !ended.is_empty() {
            aterm_log::info!(
                "messages: {} carried row(s) the parked parent ended stay ended",
                ended.len()
            );
        }
        if let Some(parent) = self.parked_parent.take() {
            self.forget_health_the_parked_parent_healed(&disk, parent);
        }
    }

    #[cfg(unix)]
    fn forget_health_the_parked_parent_healed(
        &mut self,
        disk: &aterm_messages::MessageLog,
        parent: ParkedParent,
    ) {
        let band = parent.next_id..parent.next_id.saturating_add(PARKED_PARENT_ID_RESERVE);
        let healed: Vec<update_words::HealthKind> = self
            .update_health_said
            .iter()
            .filter(|said| {
                let record = update_words::health_recovered(&said.title, &said.line).normalized();
                disk.records().any(|rec| {
                    disk.file_id_of(rec.id)
                        .is_some_and(|file| band.contains(&file))
                        && rec.stamp.unix_ms >= parent.captured_unix_ms
                        && rec.tag == record.tag
                        && rec.title == record.title
                        && rec.detail == record.detail
                })
            })
            .map(|said| said.kind)
            .collect();
        let now = Instant::now();
        for kind in healed {
            aterm_log::info!(
                "update-health: the parked parent recorded the healing of {}; not recorded again",
                kind.key()
            );
            self.update_health_said.retain(|said| said.kind != kind);
            self.update_health_latched
                .retain(|latched| *latched != kind);
            self.forget_owed(kind.key());
            self.except_clear(&update_words::update_health_entry(kind, "").key());
            for id in self.live_update_health(|live| live == kind) {
                self.messages.resolve(id, Outcome::Warn, now);
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_carried_health",
            action = "Seed",
            project = "aterm_gui::messages_host::carried_health_conformance::project"
        )
    )]
    /// Seeds the message center from a predecessor's [`crate::session_store::MessagesCarry`] handoff.
    pub(crate) fn seed_carried_messages(
        &mut self,
        carry: &crate::session_store::WindowCarry,
        finishing: Option<&str>,
    ) {
        let now = Instant::now();
        let mut carried: Carry = carry.message_carry();
        if self.keeper_recovered.is_empty() {
            let end = Intent::EndRecovered.encode();
            for message in &mut carried.live {
                message.actions.retain(|action| *action != end);
            }
        }
        let nothing_carried = carried.is_empty() && carried.next_id == 0;
        if self.incoming_handoff_pending && !nothing_carried {
            self.messages.raise_next_id(
                carried
                    .next_id
                    .max(1)
                    .saturating_add(PARKED_PARENT_ID_RESERVE),
            );
            #[cfg(unix)]
            {
                self.parked_parent = carry
                    .messages_captured_unix_ms
                    .filter(|_| carried.next_id > 0)
                    .map(|captured_unix_ms| ParkedParent {
                        next_id: carried.next_id,
                        captured_unix_ms,
                    });
            }
        }
        if finishing.is_some() {
            self.take_up_carried_health_said(&carry.update_health_said);
        }
        if nothing_carried {
            return;
        }
        self.messages.seed_carried(&carried, wall_stamp_now(), now);
        let old_process: Vec<MessageId> = self
            .messages
            .live_rows()
            .filter(|l| l.msg.key.as_deref().is_some_and(describes_the_old_process))
            .map(|l| l.id)
            .collect();
        for id in old_process {
            self.messages.withdraw(id, now);
        }
        let Some(version) = finishing else {
            return;
        };
        if let Some(id) = self
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .map(|l| l.id)
        {
            self.messages
                .restate(id, restatement_of(&update_words::finishing(version)), now);
            self.update_flow = Some(UpdateFlow {
                id,
                version: version.to_string(),
                phase: FlowPhase::Finishing,
            });
            let carried: Vec<_> = self
                .messages
                .live_rows()
                .filter_map(|l| {
                    let kind = update_words::HealthKind::of_row(&l.msg)?;
                    Some((l.id, HealthSaid::of(kind, &l.msg, "")))
                })
                .collect();
            for (health, said) in carried {
                if HealthProof::Landed.answers(said.kind) {
                    self.messages.resolve(health, Outcome::Warn, now);
                }
                if carry
                    .update_health_said
                    .iter()
                    .any(|taken| update_words::HealthKind::of_key(&taken.kind) == Some(said.kind))
                {
                    continue;
                }
                if !HealthProof::Landed.answers(said.kind) {
                    self.take_up_carried_update_health(said.kind, &said.title, &said.line);
                }
                self.remember_update_health_said(said);
            }
        }
    }

    /// Adopts unanswered update-health warnings carried from a predecessor across an update.
    pub(crate) fn take_up_carried_health_said(
        &mut self,
        said: &[crate::session_store::CarriedHealthSaid],
    ) {
        for said in said {
            if let Some(kind) = update_words::HealthKind::of_key(&said.kind) {
                let words = if said.body.is_empty() {
                    &said.line
                } else {
                    &said.body
                };
                if !HealthProof::Landed.answers(kind) {
                    self.take_up_carried_update_health(kind, &said.title, words);
                    if said.owed {
                        self.forget_owed(kind.key());
                        self.rows_owed.push(OwedRow {
                            key: kind.key().to_string(),
                            msg: update_words::health_warning(kind, &said.title, words),
                            seen: false,
                        });
                    }
                }
                self.remember_update_health_said(HealthSaid {
                    kind,
                    title: said.title.clone(),
                    line: said.line.clone(),
                    body: said.body.clone(),
                });
            }
        }
    }

    fn take_up_carried_update_health(
        &mut self,
        kind: update_words::HealthKind,
        title: &str,
        words: &str,
    ) {
        self.claim_update_health_entry(kind, words);
        match kind {
            update_words::HealthKind::Check | update_words::HealthKind::Download => {
                if let Some(class) = aterm_update::health_failing_class(title)
                    .filter(|class| update_words::HealthKind::of_class(class) == kind)
                {
                    adopt_updater_announced_health(class);
                }
            }
            update_words::HealthKind::Stalled => self.update_checker_watch.adopt_carried_stall(),
            update_words::HealthKind::LockHeld => {
                self.update_checker_watch.adopt_carried_lock_held();
            }
            update_words::HealthKind::Install => {}
        }
    }
}

/// Band row posted while no window was visible, held to re-post when a window appears.
#[derive(Clone, Debug)]
pub(crate) struct OwedRow {
    key: String,
    msg: Message,
    seen: bool,
}

/// Remembered metadata for an update-health warning posted during this launch or carried from a parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HealthSaid {
    pub kind: update_words::HealthKind,
    pub title: String,
    pub line: String,
    pub body: String,
}

impl HealthSaid {
    fn of(kind: update_words::HealthKind, msg: &Message, body: &str) -> Self {
        Self {
            kind,
            title: msg.title.clone(),
            line: msg.detail.first().cloned().unwrap_or_default(),
            body: body.to_string(),
        }
    }
}

pub(crate) const PARKED_PARENT_ID_RESERVE: u64 = 1000;

/// Log watermarks used to fold records written by a parked parent after handoff capture.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkedParent {
    pub(crate) next_id: u64,
    pub(crate) captured_unix_ms: u64,
}

fn describes_the_old_process(key: &str) -> bool {
    aterm_messages::waits::is_wait_key(key) || key == crate::message_reporters::KEY_A11Y_PUBLISHER
}

#[cfg(all(test, unix))]
#[path = "carried_health_conformance.rs"]
pub(crate) mod carried_health_conformance;

#[cfg(test)]
#[path = "band_motion_tests.rs"]
mod band_motion_tests;

/// Proof level demonstrating that part or all of the update pipeline has healed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HealthProof {
    Checked,
    Downloaded,
    Installed,
    Landed,
}

impl HealthProof {
    fn healed_by_ledger(class: &str) -> Self {
        if class.is_empty() {
            Self::Installed
        } else {
            Self::needed_for(update_words::HealthKind::of_class(class)).unwrap_or(Self::Installed)
        }
    }

    const fn needed_for(kind: update_words::HealthKind) -> Option<Self> {
        match kind {
            update_words::HealthKind::Install => Some(Self::Installed),
            update_words::HealthKind::Download => Some(Self::Downloaded),
            update_words::HealthKind::Check | update_words::HealthKind::Stalled => {
                Some(Self::Checked)
            }
            update_words::HealthKind::LockHeld => None,
        }
    }

    fn answers(self, kind: update_words::HealthKind) -> bool {
        if self == Self::Landed {
            return kind == update_words::HealthKind::Install;
        }
        Self::needed_for(kind).is_some_and(|needed| self >= needed)
    }

    fn answers_from_the_ledger(self, kind: update_words::HealthKind) -> bool {
        kind != update_words::HealthKind::Stalled && self.answers(kind)
    }
}

#[cfg(test)]
#[path = "messages_host_tests.rs"]
pub(crate) mod tests;

#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "LedgerHeals",
    reason = "The environment: a check of this process's or a sibling's clears the shared \
              ledger's streak. The updater's half is aterm-update's persistent_notice_tests."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "LedgerFails",
    reason = "The environment: the shared ledger's check streak fails again."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "CheckCompletes",
    reason = "The environment: this process's checker thread completes a check, which the \
              watchdog reads off the heartbeat; the conformance moves the beat's count."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "SeedRemembersOnly",
    reason = "The Buggy=1 mutant: the seed before finding 206, a carried warning kept with \
              no healer; the negative control replays it on the real seed's remember-only \
              branch (a title naming no ledger class)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "SeedStallRemembersOnly",
    reason = "The Buggy=1 mutant: a carried stall kept with the watchdog unarmed; \
              replayed on the real successor by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "RecoverHealsStall",
    reason = "The Buggy=1 mutant: the ledger's healing read as `Checked`, which answers the \
              stall; replayed on the real successor by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "SeedForgetsEntry",
    reason = "The Buggy=1 mutant: the parent's pending digest claim lost at the handoff; \
              replayed on the real successor by the negative control."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_carried_health",
    action = "StallHealKeepsEntry",
    reason = "The Buggy=1 mutant: the stall's heal before this change, its digest claim \
              left ripening; replayed on the real successor by the negative control."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn carried_health_scope_waivers() {}

#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandUnwant",
    reason = "The band row's want drops (a dismissal, a hold's end): the center's state. \
              Tier-1 dismisses a real row and settles the real host step."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandBack",
    reason = "A band row wanted again while its shrink waits: no re-grid (D1). The engine's \
              `commit_rows` Equal arm; pinned by the center's own D1 tests."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandQuiet",
    reason = "Time: the band's own `SHRINK_QUIET` runs out. Tier-1 steps the clock past \
              `MessageCenter::quiet_shrink_at`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Tick",
    reason = "Time: the floor's `CHROME_REGROW_SETTLE` runs out. Tier-1 steps the clock past \
              the window's `RegrowFloor::floor`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Cramp",
    reason = "The window's frame loses its room (a drag, a zoom): the OS window's size, \
              booked by `on_resize`. Tier-1 takes the stand-in's terminal rows to none and \
              drives the take (`BandTaken`) inside the floor."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Uncramp",
    reason = "The window's frame gets its room back: the OS window's size."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "BandFallOwnQuiet",
    reason = "`Buggy = 1` mutant: the band's quiet shrink before the floor. The Tier-1 \
              negative control is admitted only as this."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "StripFallAtOnce",
    reason = "`Buggy = 1` mutant: the strip's fall at once, unfloored (the law before \
              2026-10-19). The Tier-1 negative control is admitted only as this."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ChromeRegrowFloor",
    action = "Restart",
    reason = "`Buggy = 1` mutant: a floor re-anchored on a step that is no rise. No shipping \
              code re-anchors: `RegrowFloor::rose` alone moves it, pinned by the engine's \
              `chrome` tests."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn chrome_regrow_waivers() {}
