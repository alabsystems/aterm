// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The message center's host: the `App` glue of the unified message system
//! (docs/DESIGN-unified-messages-2026-09-21.md §3.2, §3.6). It holds the center, the committed
//! band rows and the log writer; gives reporters the post/restate/resolve seam; runs the
//! settle-commit-persist sync after every input and wake; and performs each plain-data
//! [`Intent`] the engine returns.
//!
//! # Clocks
//!
//! The engine never reads a clock: the host mints every [`WallStamp`] at ingress
//! ([`wall_stamp_now`]) and passes `Instant::now()` into every call. A message queued before
//! `App` exists carries the instant it was queued ([`crate::message_inbox`]).
//!
//! # The handoff freeze
//!
//! While a seamless self-update is pending (this process parked for its successor, or the
//! successor awaiting Commit), [`App::message_holds_frozen`] suspends the holds (a fold would
//! paint the committed row as a hole) and refuses the re-grid (the successor's layout must match
//! the parent's capture). The staleness caps keep running, so a successor whose Commit never
//! comes does not hold a carried row forever; the loop's deadline reads the same predicate.
//!
//! The LOG is not frozen with the holds. The parent's record is its own to
//! the end: it keeps appending through the park and flushes it right before
//! it execs the successor (`exec` runs no destructor, so nothing the writer
//! still held would land) — or, on the lanes that never exec, right before
//! Commit `_exit`s it ([`App::flush_messages_log_within`], bounded like every
//! wait on the writer). Only
//! the SUCCESSOR's drain waits ([`App::message_persist_frozen`]), until
//! Commit — a refused successor then wrote nothing about rows that were the
//! parent's to record. A successor that booted BEFORE its parent's last write
//! (the late park) reads the file again after Commit and merges what the
//! parent wrote since ([`App::reload_messages_log_at_commit`]); rows cross only
//! in the carry, and a `notice` while parked is told to retry
//! ([`App::take_notice`]). The first cut froze the parent's drain too and
//! shipped the pending lines in the carry; the carry was captured BEFORE
//! the freeze (the manifest needs it), so it shipped nothing, and every
//! line posted under the freeze died with the parent (review, 2026-09-22).
//!
//! # Reporters
//!
//! The wake arms in `lib.rs` call a words builder (`toolchain_words`, `update_words`) and post,
//! record or restate; the state a builder cannot hold lives here on `App`. The toolchain lane is
//! silent by default: one first-run row, every other word a [`Hold::LogOnly`] record.
//!
//! # The Settings ▸ Messages page
//!
//! The page's MODEL is the engine's (`aterm_messages::page`, design ruling
//! 382): the projection, its words, the tag order, the filter, the day clock
//! and the feedback line for a press. This host builds the projection with
//! one call ([`App::messages_state`]) and answers the page's seam with
//! [`PageDesk`] — its wall clock, its log folder and writer, its UTC offset,
//! the staged build, a log file's `symlink_metadata`, the agent upgrades'
//! view and the reporters' sentence count
//! ([`crate::message_reporters::sentence_lines`]). It re-exports the
//! engine's `MessagesState`, `MessageView` and `tag_words` under the names
//! they had here (ruling 385), and for its tests `MessageActionView`;
//! publishing the projection to Settings and
//! performing a page's press stay here, and the page's layout is
//! `native_settings`'.

use std::path::{Path, PathBuf};
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

/// How long a UI-thread caller waits for the messages log to land a record
/// it wants on disk before it answers — a dismissal, an update's start, a
/// filing whose cause ended ([`App::flush_messages_log_within`]): a writer
/// that is merely behind lands well inside it, raised to the waiter's urgency
/// while it does, and a starved one costs a hitch, never a freeze. A miss is
/// said in `aterm.log`; the lines land when the writer catches up.
pub(crate) const MESSAGES_FLUSH_UI_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// The same wait at the process's last write — the graceful quit, the
/// Windows replacement's exit — which runs no destructor after it: the
/// update lanes' bound (`app_update_handoff::MESSAGES_FLUSH_COMMIT_WAIT`).
pub(crate) const MESSAGES_FLUSH_EXIT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(250);

// The page's model is the engine's, re-exported for the Settings page and its tests.
#[cfg(test)]
pub(crate) use aterm_messages::page::MessageActionView;
pub(crate) use aterm_messages::page::{MessageView, MessagesState, tag_words};

/// The page's host seam ([`page::Host`]): what only the App knows about one projection. The
/// staged build and the upgrade clock are read once, when the desk is made, so every
/// `Install now` and upgrade word of one projection is judged against the same facts.
struct PageDesk<'a> {
    app: &'a App,
    /// The build the updater holds staged and ready, if any.
    staged: Option<u64>,
    /// The upgrade clock (`upgrade_host::now_s`) at the build.
    upgrade_now_s: u64,
}

impl<'a> PageDesk<'a> {
    /// The desk for one projection of `app`'s center.
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

    /// `symlink_metadata`, as [`admissible_log_path`] reads it at press time: a planted symlink
    /// is refused there, so it is not offered here (the press re-validates the whole rule).
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

    /// Every navigation, from any record: `App::perform_message_act`
    /// resolves a press by the entry's id in the ring, live or retired.
    fn performs_navigation(&self) -> bool {
        true
    }

    fn sentence_lines(&self, key: Option<&str>, detail: &[String]) -> usize {
        crate::message_reporters::sentence_lines(key, detail)
    }
}

/// The update-health banner's voice: [`crate::notify::deliver_update_health_banner`]
/// takes one, and its field is private to this module, so
/// `App::post_update_health_banner` is that delivery's one caller.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) struct HealthBannerVoice(());

#[cfg(test)]
thread_local! {
    static PTY_RESIZES_FOR_MESSAGES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// The update-health desktop banners `App::post_update_health_banner` would have posted on
    /// this thread, as `(title, banner)`.
    pub(crate) static UPDATE_HEALTH_BANNERS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// The updater classes `adopt_announced_health` would have been handed on this thread: a
    /// test never touches the process's updater.
    pub(crate) static UPDATER_ADOPTED_HEALTH: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Hand `class` to this process's updater as announced by its parent
/// ([`aterm_update::adopt_announced_health`]; recorded instead under
/// `cfg(test)`, [`UPDATER_ADOPTED_HEALTH`]).
fn adopt_updater_announced_health(class: &'static str) {
    #[cfg(test)]
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().push(class));
    #[cfg(not(test))]
    aterm_update::adopt_announced_health(class);
}

/// How many window re-grids the message band has paid for ON THIS THREAD
/// (test-only: the launch-pass test's "at most two re-grids"; a headless App
/// is driven on its test's own thread).
#[cfg(test)]
pub(crate) fn message_regrids() -> u64 {
    PTY_RESIZES_FOR_MESSAGES.with(|c| c.get())
}

/// The wall clock now, as a message's ingress stamp: milliseconds since the
/// Unix epoch, 0 on a clock before it (the engine reads no clock of its own).
pub(crate) fn wall_stamp_now() -> WallStamp {
    let unix_ms = aterm_time::unix_millis().unwrap_or(0);
    WallStamp { unix_ms }
}

/// A live row re-worded to `msg`'s words: title and its short form, detail,
/// meter, actions, hold, severity, glyph, the excerpt flag and the finished
/// words — every field a builder authored,
/// applied in place
/// (no log line; the center bumps the row's revision). The reporters that
/// speak through ONE row (a tailed meter, the handoff bracket, the standing
/// health warning) build the new words with the same builder they posted
/// with and restate through this.
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
        // The short form goes with the title: left unset, a new title would
        // drop it, and the band would cut the whole title in its place.
        short_title: Some(msg.short_title.clone()),
    }
}

/// The words a decision row takes after *Open Settings*: the actions cleared (the row stays,
/// answered by the grant or its hold), the route's short hold, and the clauses of
/// `consent_card::opened_settings_caption`, split at its dash (title, tail) and its `; `
/// joints (lines behind Details), the route line without its `System Settings ▸` lead.
///
/// * Opened: the title is the instruction (`Enable aterm in Full Disk Access`) and the route
///   is the painted excerpt on every outcome: `openURL:` reports only that Settings took the
///   URL, not that it scrolled to the row.
/// * Refused: the title is the route, verb first (`Open Privacy & Security ▸ Full Disk
///   Access`), then `Settings did not open`, in the warning's tone and glyph; that line alone
///   reads as an FYI and drops the one part a person can act on.
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
        // The pane the route ends at: `… ▸ Full Disk Access`.
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

/// The `OpenSystemPane` pane names the intent codec carries.
fn privacy_pane(pane: &str) -> Option<crate::menu::PrivacyPane> {
    match pane {
        PANE_FULL_DISK_ACCESS => Some(crate::menu::PrivacyPane::FullDiskAccess),
        PANE_DEVELOPER_TOOLS => Some(crate::menu::PrivacyPane::DeveloperTools),
        _ => None,
    }
}

/// The path `Open log` may open, or `None`: absolute; every component a plain name
/// (`Path::starts_with` is lexical, so a `..` would escape `log_dir`); a regular file by
/// `symlink_metadata` (a planted symlink would open whatever it points at); under `log_dir`.
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

/// Whether `Open in browser` may open `url`, checked at the press: the engine's rule for
/// the address an entry carries ([`aterm_messages::link_host`]: https, no user, no port)
/// and the gate every link a click opens passes ([`crate::is_safe_url`]), both.
fn openable_link(url: &str) -> bool {
    aterm_messages::link_host(url).is_some() && crate::is_safe_url(url)
}

/// Open `path` in the platform's text editor, off the UI thread, if [`admissible_log_path`]
/// admits it under `logging::log_dir()`; the path is re-validated at press time because the
/// intent came back from a log or a carry. macOS uses `open -t` (the `.log.seen` extension has
/// no registered handler), Linux `xdg-open`, Windows `notepad.exe`. `false` when refused or
/// nothing could be spawned.
fn open_path_external(path: &str) -> bool {
    let Some(log_dir) = crate::logging::log_dir() else {
        return false;
    };
    let Some(target) = admissible_log_path(path, &log_dir) else {
        return false;
    };
    crate::qos::spawn_lane_thread("aterm-open-path", crate::qos::Role::Background, move || {
        // Nobody is blocked on the editor coming up: below the keystroke
        // path, like every other one-shot spawn (`crate::qos`).
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
    /// Post `msg` into the center, stamped now; log it; sync. Returns the
    /// row's id (the existing one for a duplicate).
    pub(crate) fn post_message(&mut self, msg: Message) -> MessageId {
        self.post_message_at(msg, wall_stamp_now())
    }

    /// [`Self::post_message`] with the caller's ingress stamp — the inbox's
    /// path, so a message queued before `App` existed keeps its instant.
    pub(crate) fn post_message_at(&mut self, msg: Message, stamp: WallStamp) -> MessageId {
        let id = self.post_unsynced(msg, stamp);
        self.sync_messages();
        id
    }

    /// A record: posted as [`Hold::LogOnly`] whatever hold it was built with, so it reaches the
    /// log, `appstatus` and Settings ▸ Messages but never the glass. No settle and no repaint:
    /// the park's sweep persists it within the loop's next turn.
    pub(crate) fn record_message(&mut self, msg: Message) -> MessageId {
        self.post_unsynced(msg.hold(Hold::LogOnly), wall_stamp_now())
    }

    /// A full config reload's warnings replace the band's config rows as a set (design §3.10),
    /// with one settle: every live `config.`-keyed row is in scope ([`Self::replace_config_set`]),
    /// so an empty set clears every fixed family's row.
    ///
    /// The backend build's `font_family` verdict
    /// ([`crate::message_reporters::KEY_FONT_FAMILY`]) is in scope only when
    /// `family_admitted`: the reload's font work ran and ADMITTED the
    /// configured family (a refusal is the catalog worker's
    /// [`crate::native_font_catalog::FAMILY_REFUSED`] warning, said under
    /// `config.fonts`). Its row then closes fixed, and a folded record of it
    /// reads fixed (ruling 367). Any other reload did not answer it: in scope
    /// always, a Presence toggle closed a still-rejected family `Font applied`
    /// ✓ (round 44); out of scope always, the owner who fixed or cleared the
    /// family kept `Couldn't apply the font` up to its fold and in Problems
    /// for good (round 44's review, ruling 418 (c)).
    ///
    /// `fixed_as` holds, by key, the words each family's fix is logged in as this
    /// reload SAW it end ([`crate::message_reporters::config_fix_words`]: a
    /// wallpaper taken out reads "Wallpaper removed", one that now decodes
    /// "Wallpaper loaded") — decided now, not when the warning was posted.
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
        // Every family was re-derived: every `config.` key this process has spoken under is in
        // the verdict (the font family only as `reloads` admits it).
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

    /// The full reload's replace when its font work admitted no family
    /// ([`Self::replace_config_messages_after_font_work`]).
    #[cfg(test)]
    pub(crate) fn replace_config_messages(&mut self, msgs: impl IntoIterator<Item = Message>) {
        self.replace_config_messages_after_font_work(
            msgs,
            false,
            &std::collections::BTreeMap::new(),
        );
    }

    /// [`Self::replace_config_messages_after_font_work`] for a reload that re-derived only the
    /// rows under `keys`: the content-equal reload (`app_config.rs`'s dedupe arm), which re-reads
    /// only the ignored-key and unaccepted-value families. A prefix resolve there would take a
    /// still-true keybinding or font row down on a comment-only save.
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

    /// The replace itself, over the live rows under `scope`. A reload runs on
    /// EVERY Settings patch, Presence toggle and Serious Mode write (the
    /// parsed config changed), so a family's words are compared, not re-posted
    /// (review 2026-09-24 — "repeats go to the log"):
    ///
    /// * a live row whose family comes back in the same words stands — no
    ///   resolve, no re-post, no re-announcement;
    /// * a row whose family is GONE is resolved: the problem was fixed, and
    ///   its record reads in the words the family declared for the fix
    ///   (`Misspelled setting fixed`, ✓);
    /// * a row whose family's words CHANGED is superseded in place by the new
    ///   row: the problem became another, so its record keeps its title and
    ///   its ⚠, never the fix's words (design ruling 266). Settings ▸ Messages
    ///   lists it as part of the new row, which keeps the problem in Problems
    ///   (one fact, one row; `aterm ctl messages` and messages.log keep the
    ///   record itself). One that is now a record only comes down with the
    ///   fix's words cleared first;
    /// * a family whose row already LEFT the glass — its hold ran out, it was
    ///   read, or it was recorded as a repeat before — and that comes back in
    ///   the words THIS process last recorded under its key is a RECORD
    ///   ([`Self::config_repeat`]): Settings ▸ Messages and messages.log keep
    ///   it, the glass does not. A new or changed problem still raises its row;
    /// * a family in `rederived` that the reload found FIXED is remembered as
    ///   fixed (`App::config_cleared`), so the same words coming back after
    ///   the fix — a problem broken again — raise their row again, whether or
    ///   not its row was still on the glass when the fix landed; and every
    ///   record of it that left the glass unfixed in this process — folded by
    ///   the press that sent the person to fix it, read, run out — now reads
    ///   fixed (`MessageCenter::fix_closed`, ruling 367).
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
                // FIXED, whether or not its row was still up (ruling 367):
                // a warning folded by the `Open aterm.toml` or `Details`
                // press that sent the person to fix it closes as fixed too,
                // in the words this reload saw it end in.
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
                // Still a problem, now a record only: the row comes down without the fix's
                // words, so its record keeps its title and its ⚠ and stays in Problems.
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
            // Same words: the row stands. CHANGED words: the family's new row
            // SUPERSEDES it in place when posted below — the problem became
            // another, it was not fixed, so the record keeps its title and
            // its mark (ruling 266: `Misspelled setting fixed` was logged for
            // a typo still there beside a second one), and the page lists it
            // as part of the new row.
        }
        self.post_config_set(resolved, msgs);
    }

    /// The posting half of a config replace: `msgs` under one stamp, then one settle when
    /// anything was resolved or reached the glass. A family whose live row says the same words
    /// is skipped; a repeat of words already off the glass ([`Self::config_repeat`]) and a
    /// `Hold::LogOnly` family (`ConfigFamily::RetiredKeys`) are records.
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
            // A record, not a bare `LogOnly` hold: the door re-derives a
            // classified row's hold from its class, so a repeat must change
            // the class for it to stay off the glass.
            let msg = if msg.hold != Hold::LogOnly && self.config_repeat(&msg) {
                msg.record()
            } else {
                msg
            };
            let row = msg.hold != Hold::LogOnly;
            if row && let Some(key) = msg.key.as_deref() {
                // Said on the glass again: its next repeat is a record.
                self.config_cleared.remove(key);
            }
            self.post_unsynced(msg, stamp);
            posted += usize::from(row);
        }
        if resolved > 0 || posted > 0 {
            self.sync_messages();
        }
    }

    /// Whether `msg` repeats words that already left the glass: the latest record under its key
    /// from this process (a record replayed from disk was a previous launch's, and a launch says
    /// its config problems once) has the same title and detail and retired by running out (hold
    /// ended, read or dismissed, unseen, evicted, stale, or recorded as a repeat). A row resolved
    /// by a reload, or a family a reload found fixed after its row left the glass
    /// (`App::config_cleared`), means the same words coming back later are news again.
    fn config_repeat(&self, msg: &Message) -> bool {
        use aterm_messages::Retired;
        let Some(key) = msg.key.as_deref() else {
            return false;
        };
        if self.config_cleared.contains(key) {
            return false;
        }
        // A record keeps the sentences without the band's painted excerpt
        // (`config_record`): the same words in either form are a repeat.
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

    /// Whether a live row's title or detail contains `needle`.
    #[cfg(test)]
    pub(crate) fn has_live_message(&self, needle: &str) -> bool {
        self.messages.live_rows().any(|l| {
            l.msg.title.contains(needle) || l.msg.detail.iter().any(|d| d.contains(needle))
        })
    }

    /// The post itself, logged, with no settle: the callers above sync once
    /// for what they posted.
    pub(crate) fn post_unsynced(&mut self, msg: Message, stamp: WallStamp) -> MessageId {
        self.post_unsynced_at(msg, stamp, Instant::now())
    }

    /// [`Self::post_unsynced`] at the caller's instant: a post the settle makes
    /// on its own clock ([`Self::keep_owed_rows`]).
    fn post_unsynced_at(&mut self, msg: Message, stamp: WallStamp, now: Instant) -> MessageId {
        let posted = self.messages.post(msg, stamp, now);
        if let Some(live) = self.messages.live(posted.id) {
            aterm_log::info!("message {} {}: {}", posted.id, live.msg.tag, live.msg.title);
        }
        self.note_message_rule();
        posted.id
    }

    /// Re-word a live row in place; `false` when `id` is not live. A restate costs no log line
    /// (the final words land on the row's Retired line). One that changed nothing the row says
    /// (a heartbeat re-feeding the same reading) moves no revision, so it syncs and repaints
    /// nothing; its staleness cap is re-armed all the same.
    pub(crate) fn restate_message(&mut self, id: MessageId, r: Restatement) -> bool {
        let revision = self.messages.revision();
        let live = self.messages.restate(id, r, Instant::now());
        self.note_message_rule();
        if live && self.messages.revision() != revision {
            self.sync_messages();
        }
        live
    }

    /// A record ([`Self::record_message`]) re-worded in place to `msg`'s words: no new entry, one
    /// more log line ([`aterm_messages::MessageCenter::reword_record`]); `false` when `id` is no
    /// record or already says them. No settle and no repaint, as for a record's post.
    pub(crate) fn reword_record(&mut self, id: MessageId, msg: &Message) -> bool {
        self.messages.reword_record(id, msg)
    }

    /// A ROW OR RECORD REPLACED by the later row `by` that tells its story on
    /// ([`aterm_messages::MessageCenter::supersede_record`]), so Settings ▸
    /// Messages lists the story once; `false` when the center refuses it. A
    /// live row leaves the glass, which settles; a record's re-retirement, as
    /// its re-wording, needs no settle and no repaint: the park's sweep
    /// persists and publishes it.
    pub(crate) fn supersede_record(&mut self, id: MessageId, by: MessageId) -> bool {
        let live = self.messages.live(id).is_some();
        let replaced = self.messages.supersede_record(id, by, Instant::now());
        if replaced && live {
            self.sync_messages();
        }
        replaced
    }

    /// Whether a Settings view is the frontmost window's active tab: what is on
    /// screen when a config problem Settings says on its own row decides between
    /// a band row and a record.
    pub(crate) fn settings_in_front(&self) -> bool {
        self.frontmost_window
            .and_then(|wid| self.active_native_view(wid))
            .is_some_and(|(instance, _)| {
                self.native_runtime
                    .app(instance)
                    .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings)
            })
    }

    /// Note which config families' current problems Settings says on the row of
    /// the setting they are about (`shown`), for the keys a reload re-derived
    /// (`scope`): what [`Self::record_config_rows_settings_shows`] reads.
    pub(crate) fn note_config_settings_shows(&mut self, scope: &[&str], shown: &[&'static str]) {
        for key in scope {
            self.config_settings_shows.remove(*key);
        }
        self.config_settings_shows
            .extend(shown.iter().map(|key| (*key).to_string()));
    }

    /// The band-or-record decision for a config problem Settings says on its own
    /// row, made again now that Settings is what is on screen: such a row raised
    /// while a terminal was in front becomes its record in place (kept in Settings
    /// ▸ Messages), so the band never repeats the row above it or pushes the page
    /// down. A record, its next repeat stays one ([`Self::config_repeat`]); a new
    /// or changed problem still raises its row. Called wherever the front window
    /// or tab changes (`App::sync_active_session`).
    pub(crate) fn record_config_rows_settings_shows(&mut self) {
        if self.keep_config_rows_settings_shows_as_records() > 0 {
            self.sync_messages();
        }
    }

    /// [`Self::record_config_rows_settings_shows`] without the sync, for a caller
    /// that syncs once for everything it posted: how many rows became records.
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

    /// A live row that becomes its own record in place, in `msg`'s words
    /// ([`aterm_messages::MessageCenter::keep_as_record`]); `false` when `id`
    /// is not live. The row left the glass: synced as a withdraw is.
    pub(crate) fn keep_as_record(&mut self, id: MessageId, msg: &Message) -> bool {
        let kept = self.messages.keep_as_record(id, msg, Instant::now());
        if kept {
            self.sync_messages();
        }
        kept
    }

    /// The reporter is done with a row (the ledger healed, a new window
    /// painted): retire it now, recorded with `outcome`.
    pub(crate) fn resolve_message(&mut self, id: MessageId, outcome: Outcome) -> bool {
        let resolved = self.messages.resolve(id, outcome, Instant::now());
        if resolved {
            self.sync_messages();
        }
        resolved
    }

    /// The reporter withdrew a live row with no outcome to claim (the tailer's file vanished
    /// under a live meter before any marker said how the pass ended). The log keeps the row
    /// (`Retired::Withdrawn`); `appstatus` lists no finished activity for it; the marker, when
    /// it comes, posts the outcome under the same key.
    pub(crate) fn withdraw_message(&mut self, id: MessageId) -> bool {
        let withdrawn = self.messages.withdraw(id, Instant::now());
        if withdrawn {
            self.sync_messages();
        }
        withdrawn
    }

    /// The person's word on a live row was taken: it retires `Answered { label }` (the record
    /// says `answered: <label>`), for a capsule whose word is written off this thread and so
    /// could not close the row at the press. `false` when `id` is not live.
    pub(crate) fn answer_message(&mut self, id: MessageId, label: &str) -> bool {
        let answered = self.messages.answer(id, label, Instant::now());
        if answered {
            self.sync_messages();
        }
        answered
    }

    /// After any message input and when a handoff finishes: retire what has expired (holds
    /// suspended under a freeze), commit the row count, append what the log drained (held only in
    /// a successor before Commit), and repaint every window. The band is chrome in each; the
    /// RepaintKey's band fingerprint keeps a byte-identical tick from re-presenting.
    pub(crate) fn sync_messages(&mut self) {
        let now = Instant::now();
        // A host verb (a post, a retire, a restate) moved the glass before this settle, whose
        // `glass_changed` compares across the settle alone, so the lit chip is resolved again
        // whatever it reports.
        if !self.settle_messages(now) {
            self.refresh_band_hover();
        }
        // Settings ▸ Messages, when a view holds it, follows within the 2 Hz
        // gate (design §4.2); what the gate holds back the park publishes.
        self.publish_native_messages_state_if_due(now);
        self.request_redraw_all_windows();
    }

    /// [`Self::sync_messages`] without the repaint request, for the loop's own sweeps (the timer
    /// wake and the park), which repaint themselves; `true` when the glass changed. The step is
    /// the engine's (`aterm_messages::drive`); the host supplies the instant, the freeze, what its
    /// windows afford and the rows its geometry reserves, and re-grids between the two halves.
    pub(crate) fn settle_messages(&mut self, now: Instant) -> bool {
        // The tab strip's fall a Settings edit made inside the regrow floor
        // commits here once the floor has run, like the band's below.
        let strip = self.settle_strip_fall(now);
        // The digest's own clock (an entry a reporter stopped naming ends, an
        // aged one files itself): read here, on the loop's own sweep, so an
        // entry cannot outlive its cause because no second look came.
        if self.messages.settle_digest(wall_stamp_now(), now) {
            self.digest_moved();
        }
        // ... and the facts that mend themselves, once one has outlasted its
        // horizon (a supervisor stopping for an hour, updates failing for a day).
        self.ripen_exceptions(now);
        // A row said while no band was on screen: kept until one shows it, and
        // shown now if one does — before the commit, which reveals it.
        self.keep_owed_rows(now);
        let commit = self.band_commit(now);
        let step = drive::settle_and_commit(&mut self.messages, commit);
        let committed = self.regrid_message_band(step.regrid, now);
        // A `notice progress` restate the wire's pacing held back falls due here as one paced
        // paint, however many lines arrived.
        let persist = !self.message_persist_frozen();
        let (lines, wire) =
            drive::drain_and_pace(&mut self.messages, &mut self.wire_gate, now, persist);
        self.append_message_lines(&lines);
        let changed = step.glass_changed || committed || wire || strip;
        if changed {
            // The chips moved under a still pointer.
            self.refresh_band_hover();
        }
        changed
    }

    /// Drain the center's pending log lines to the writer.
    fn persist_messages(&mut self) {
        let lines = self.messages.drain_shelved_for_persist();
        self.append_message_lines(&lines);
    }

    /// Hand drained log lines to the writer — none, too, so lines its full
    /// channel held back go on at the next drain (one load when nothing
    /// waits). With no writer (a test App, or a log dir that could not be
    /// opened) the lines are dropped here, so the pending queue never fills
    /// and reports a `Dropped` count for a file that does not exist.
    fn append_message_lines(&self, lines: &[(aterm_messages::LogLine, aterm_messages::Shelf)]) {
        if let Some(writer) = &self.messages_log {
            writer.append(lines);
        }
    }

    /// [`Self::flush_messages_log_within`] with a minute's hang detector: the
    /// tests' way to read the file back.
    #[cfg(test)]
    pub(crate) fn flush_messages_log(&mut self) {
        assert!(
            self.flush_messages_log_within(std::time::Duration::from_secs(60)),
            "the messages writer landed nothing within a minute"
        );
    }

    /// What the center holds goes to the writer, and the writer lands
    /// everything it holds before this returns — or `within` passes. Every
    /// wait on the writer is bounded (law L5): the person sits through it, at
    /// a frozen Commit or exec as at a Dismiss click. The parent of a seamless
    /// handoff calls it right before it execs the successor, and the seamless
    /// parent before Commit `_exit`s it (round-seven update audit, finding
    /// 53): neither runs a destructor, so a line the writer thread still held
    /// — a record a script was answered `OK recorded` for during the park —
    /// died with the process, and the successor, which reads the file again
    /// after Commit ([`Self::reload_messages_log_at_commit`]), never saw it.
    /// The writer stays open: a failed exec carries on recording. `false`,
    /// said in `aterm.log`, when the writer was still behind at the deadline;
    /// its lines stay queued and land when it catches up, if the process
    /// lives that long.
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

    /// THE SEAMLESS PARENT'S ROWS THAT DID NOT CROSS (ruling 441 (d)): at
    /// Commit, before its last [`Self::flush_messages_log_within`], every live
    /// row the park's carry did not take (an unrevealed row, a row posted
    /// since the park) retires as ended with this process
    /// ([`aterm_messages::MessageCenter::close_uncarried`]). Left open, the
    /// successor replayed each from the file as cut off by a crash (`still
    /// open when aterm stopped`, ruling 267), beside its reporter's re-post
    /// there. The count closed.
    #[cfg(unix)]
    pub(crate) fn close_uncarried_messages(&mut self) -> usize {
        self.messages
            .close_uncarried(&self.handoff_carried_messages, Instant::now())
    }

    /// The graceful exit's last word: every row still open (live work, a held warning, one still
    /// queued) is recorded as cut off by aterm quitting (`Retired::Quit`, with the percent a
    /// determinate row reached), and the log is flushed, because the exit seam `forget`s the App
    /// and its writer's drop-time join never runs. A successor still frozen before Commit writes
    /// nothing: those rows are its parent's.
    pub(crate) fn close_messages_at_exit(&mut self) -> usize {
        if self.message_persist_frozen() {
            return 0;
        }
        // A held tab lifted on the last turn, its wake unread, is said before
        // the log closes (`App::let_go_every_lifted_held`).
        self.let_go_every_lifted_held();
        let closed = self.messages.quit(Instant::now());
        self.flush_messages_log_within(MESSAGES_FLUSH_EXIT_WAIT);
        closed
    }

    /// Whether the persist drain waits: only in a seamless successor before Commit, whose lines
    /// are about rows the parent still owns, so a refused successor writes nothing. The parent's
    /// drain never waits (module doc).
    pub(crate) fn message_persist_frozen(&self) -> bool {
        self.incoming_handoff_pending
    }

    /// Commit the band's row count to the window geometry; `true` when the count moved (and every
    /// window was re-gridded). The committed count (`message_band_rows`) is what the chrome-row
    /// law reads, so a row is reserved and released in the same step as the PTY resize.
    ///
    /// Never mid-handoff: the successor must get the parent's exact layout, and a re-grid here
    /// moves `ws.rows` outside `window_event`, which Commit refuses as a topology change; the band
    /// keeps its count until `Wake::UpdateHandoffFinished` re-syncs. Not deferred before a handoff
    /// either: the "update staged" row rises seconds before the park, and its resize is ordinary
    /// state the checkpoint carries (bounded by `seamless::checkpoint_meta_bound_violation`).
    ///
    /// Never more rows than the glass can spare: `grid_dims_for` floors the terminal at one row,
    /// so chrome that outgrew the window would be clipped; the band yields instead. Each window
    /// affords its terminal rows plus the committed band rows, so the answer does not depend on
    /// the re-grid it triggers, and the smallest window decides. The center's hysteresis
    /// (`commit_rows`) grows at once and shrinks after its quiet, and anchors a row's hold only
    /// within the committed count, so a clamped row never burns its hold unpainted.
    ///
    /// The geometry's count and the center's (`committed_rows`) differ only in a seamless
    /// successor: it reserves the rows its parent committed while its re-seeded center counts the
    /// rows carried, one more when the parent's frozen count had queued a row (`seed_carried`).
    /// Once the freeze lifts the geometry takes the center's count, so a carried row never paints
    /// into a row the window does not own.
    pub(crate) fn sync_message_band_rows(&mut self, now: Instant) -> bool {
        let commit = self.band_commit(now);
        let regrid = drive::commit(&mut self.messages, commit);
        self.regrid_message_band(regrid, now)
    }

    /// What this host's step commits against: the instant, the handoff freeze, the rows every
    /// window with glass affords (`App::chrome_commit_grid_rows`; a headless window has none to
    /// spare, so it never clamps), the rows its geometry reserves, and the regrow floor of the
    /// windows the band's re-grid resizes (`App::band_regrow_floor`).
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

    /// Take the row count the engine committed into the window geometry and re-grid every
    /// window; `true` when there was one to take. A rise (`aterm_messages::chrome::is_rise`) is
    /// noted on every window's regrow floor at `now`, before the re-grid, so no other chrome
    /// source's fall re-grids a window back inside the settle.
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

    /// Whether a handoff is freezing the holds and the re-grid right now. One predicate, read by
    /// the settle and by the deadline that arms it, so the loop never arms a wake its own settle
    /// will decline to act on.
    pub(crate) fn message_holds_frozen(&self) -> bool {
        self.update_handoff_parked() || self.incoming_handoff_pending
    }

    /// The next instant the band needs the loop back (what [`Self::sync_messages`] would act on,
    /// freeze included), or the instant the next Settings ▸ Messages publish falls due
    /// (`messages_publish_due_at`: a publish the 2 Hz gap held back, or the minute tick while a
    /// view is on the route). `None` with nothing armed: an idle band never wakes the loop.
    pub(crate) fn messages_deadline(&self) -> Option<Instant> {
        // Holds, echoes, patience and the wire's one paced paint.
        let state =
            drive::state_deadline(&self.messages, &self.wire_gate, self.message_holds_frozen());
        // The digest ends an entry no reporter named for ten minutes.
        [
            state,
            self.messages_publish_due_at(),
            self.messages.digest_deadline(),
            self.ripening_deadline(),
            // The tab strip's fall waiting the regrow floor, which
            // `settle_messages` commits.
            self.strip_fall_due(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Post what the pre-App inbox holds at each park, each with the stamp it
    /// was queued under, raise or end the exceptions queued there
    /// ([`crate::message_inbox::queue_exception`],
    /// [`crate::message_inbox::queue_exception_clear`]), and settle verified
    /// workstation setup and the folders fresh shells could not start in
    /// ([`Self::fold_folder_faults`]).
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
        // The launch's config problems meet what is on screen now: with Settings
        // restored in front, one it says on its own row is its record.
        let _ = self.keep_config_rows_settings_shows_as_records();
        if !folders.is_empty() {
            self.fold_folder_faults(&folders);
        }
        self.sync_messages();
    }

    /// Shells that started in the home folder because their folder was not
    /// there, could not be entered, or did not answer (audit #7 finding 48;
    /// [`crate::spawn_folder::Fault`]): ONE row per fault
    /// naming every such folder while it is up, so a restore that meets three
    /// gone folders is one row naming three, and a New Tab from a gone folder
    /// a minute later joins it. Once the row has gone, the next folder starts
    /// a fresh list. A row carried across an update arrives with no list
    /// behind it, so its folders are read back from the row first.
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

    /// [`Self::update_bar_press_applies`]'s law for row `id`'s `Install now`: the press
    /// applies in place where the row still offers this build, the stage is
    /// really ready and the seamless handoff can run with something to carry (a
    /// process that keeps its image and has no terminal open is refused,
    /// `App::kept_image_waits_for_terminal`); otherwise it opens the details page.
    /// ONE reading, for the press and for its feedback words.
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

    /// Perform one plain-data intent the engine returned for row `id` in window `wid` (a capsule,
    /// a body press, a screen reader's activate, a Settings ▸ Messages button). The row's
    /// lifecycle is the engine's (`act` already retired it if the intent closes it). `false` when
    /// refused (no page for the route, not a crash log, the system pane did not open).
    pub(crate) fn perform_intent(&mut self, wid: WindowId, id: MessageId, intent: Intent) -> bool {
        let now = Instant::now();
        // A navigation press (a Settings page, `aterm.toml`, the crash log) that took the person
        // there read the row (design §10.5 H6): a held or standing row folds; live and ask rows
        // stay, and a record re-offered from the page is a no-op.
        let navigation = matches!(
            intent,
            Intent::OpenSettings { .. }
                | Intent::OpenConfigEditor { .. }
                | Intent::OpenPath { .. }
                | Intent::OpenLink { .. }
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
            // Details (design §2.2, §3.6): mark the row seen (a held row folds now), then open
            // Settings ▸ Messages at this entry in the pressed window. Seen first, so the open's
            // publish already shows it folded (a second publish inside the 2 Hz gate would wait).
            Intent::Details => {
                if self.messages.mark_seen(id, now) {
                    self.sync_messages();
                }
                // The file-access question was read: never re-offered in this
                // process, and its fold is not the unanswered path (the
                // watch for the grant continues).
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
            // The config reporters carry no line and `ConfigEditorTarget` is a key or a search,
            // so the editor opens at the top.
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
            // macOS performs (the Security page's own gesture arm — a headless
            // instance's arm reaches no `NSWorkspace`); every other platform's
            // arm reports `Refused`, and the row says so honestly. The row
            // stays for the watcher either way (R16). The rest is the pane's
            // own: Full Disk Access records the access card's `opened` marker
            // so a later process can acknowledge the flip. Developer Tools is
            // the one hand-over every origin goes through
            // (`App::hand_devtools_to_owner`): the page, the helper panel
            // beside it, the shared opened marker and the owner's return read
            // — and never the file-access marker (a press there is no answer
            // to the file-access question, as on the Security page).
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
                        // aterm's panel, with its icon to drag, joins the page.
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
            // `Open in browser` on an entry a tool filed (`notice later … url=`): the
            // page it named, in the browser, only past `openable_link`, so a record
            // or a carry that reached here by another road opens nothing else. A
            // headless instance opens nothing and says so.
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
            // The row is already retired (`Intent::closes_row`); what is left
            // is the record the answer leaves for later processes.
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
            // The GPU-lost remedy: a real in-process window. No `ActiveEventLoop`
            // here, so post the wake the keyboard's Cmd-N posts; the
            // `user_event` arm (which has `el`) runs the creation.
            Intent::NewWindow => match self.proxy.as_ref() {
                Some(proxy) => proxy.send_event(crate::Wake::CreateWindow).is_ok(),
                None => false,
            },
            // `Stop paste`: the rest of the paste is dropped, the input after it goes through,
            // and the row fades.
            Intent::StopPaste { session } => self.stop_paste(session),
            // `End sessions` on the PTY keeper's recovery row: every shell
            // this launch reattached is hung up (`keeper_link`).
            Intent::EndRecovered => self.end_recovered_sessions() > 0,
            // Ordinal-only carried intents are readable, never rebound to
            // whichever tab occupies that position now.
            Intent::ShowTab { .. } => false,
            Intent::ShowSession {
                window, session, ..
            } => {
                // Drop the store read before focus takes any pane/terminal locks.
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
            // THE OWNER'S WORD on a tab's agent upgrade (gap #21): written
            // where `aterm harness upgrade <tab> --now|--defer|--skip` writes
            // it, for the build the row named, off this thread; the row says
            // what it did (`upgrade_host`). Only while the tab's upgrade
            // still takes that word for that build: Settings ▸ Messages
            // presses from the model it last drew, and the harness re-checks
            // only the build — an `Upgrade now` drawn before the owner's
            // `--now` landed would arm a second round for a hurried move.
            Intent::AgentUpgrade { tab, to, word } => {
                self.upgrade_view
                    .offers(&tab, &to, word, crate::upgrade_host::now_s())
                    && self.send_upgrade_word(crate::upgrade_host::WordAsk::new(tab, to, word))
            }
        }
    }

    /// Settings ▸ Messages at an entry, in this window (design §4.5): install or reuse `wid`'s
    /// Settings view on the Messages route, then, with an id, select the entry through
    /// `settings/messages/select` (the reducer expands it, clears a chip that would hide it and
    /// pages to it). `None` opens the page at the top. `false` when the window has no Settings.
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
        // The page measures its rows on a render (the section a compact
        // page seats an entry in depends on what fits above it); a select
        // that landed before its first render would page by a guess, so the
        // route's first frame is compiled here, before the select.
        let _ = self.compiled_native_ui(wid);
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(crate::native_settings::MESSAGES_SELECT),
            value: Some(crate::native_app::SemanticInput::Text(id.raw().to_string())),
        });
        self.dispatch_native_view_event(wid, view, action).is_ok()
    }

    /// [`Self::open_messages_entry`] from outside any window (a "To look at"
    /// line in the menu-bar item): the frontmost window, else the first —
    /// the window it opened in, which the caller raises. `None` when there
    /// is none to open it in, or the page did not open.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn open_messages_entry_anywhere(&mut self, id: u64) -> Option<WindowId> {
        let wid = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())?;
        self.open_messages_entry(wid, MessageId::from_raw(id))
            .then_some(wid)
    }

    /// The page's own press on an entry's button (design §4.4, `AppEffect::MessageAct`), named
    /// by id and index. A live row goes through the engine's `act` as a band capsule does; an
    /// open exception through `act_digest`; a retired record re-offers the intent the log kept,
    /// unless it is a decision (only a live row answers one) or the record no longer offers it
    /// ([`aterm_messages::LogRecord::still_offers`]). The intent is performed in `wid` and the
    /// outcome worded for the page's feedback.
    pub(crate) fn perform_message_act(
        &mut self,
        wid: WindowId,
        id: u64,
        action: u8,
    ) -> MessageActOutcome {
        let Some(id) = MessageId::from_raw(id) else {
            return MessageActOutcome::Gone;
        };
        // The page's own capsules on an open exception (view-time, no intent stored): `Dismiss`
        // files this entry, `Dismiss all` the whole digest, each flushed to the log before the
        // press returns.
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
            // The press is on record (`Acted`) whatever the intent does next.
            self.sync_messages();
            intent
        } else if self.messages.digest_entry(id).is_some() {
            // An open exception: the press is on record (`Acted`), the
            // entry's own action files it answered (the owner did what it
            // asked), a navigation leaves it (`MessageCenter::act_digest`).
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
        let feedback = |performed| act_feedback(&intent, performed, applies);
        if self.perform_intent(wid, id, intent.clone()) {
            MessageActOutcome::Performed {
                feedback: feedback(true),
            }
        } else {
            MessageActOutcome::Refused {
                feedback: feedback(false),
            }
        }
    }

    /// `messages …` on the main thread: the engine's rows over the center
    /// (design §5.1), every free field through the socket's `pct_encode`.
    pub(crate) fn read_messages(&self, q: &wire::ReadQuery) -> Vec<String> {
        wire::message_rows(
            &self.messages,
            q,
            wall_stamp_now().unix_ms,
            &crate::control::pct_encode,
        )
    }

    /// `notice …` on the main thread (design §5.2): the engine decides (verdict, caps, mint
    /// budget, pacing, reply words) and the host paints. `act` is the one form the host performs:
    /// the engine names the capsule and spends its press budget (`wire::press_target`), then
    /// [`Self::perform_message_act`] runs in the front window (window 0 when headless). Returns
    /// the reply line without its `\n`.
    ///
    /// Refused while parked for an update ([`wire::switching_reply`]): the band crossed in the
    /// carry and this process `_exit`s at Commit, so the script's retry reaches whichever version
    /// stands.
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
        // A log-only change also needs the writer before the reply: ending the cause
        // of an already-filed exception changes its record but no open digest entry.
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

    /// The page's projection, built by the engine ([`MessagesState::of`]) over this App's facts
    /// ([`PageDesk`]).
    pub(crate) fn messages_state(&self) -> MessagesState {
        MessagesState::of(&self.messages, &PageDesk::of(self))
    }
}

// ---------------------------------------------------------------------------
// The body press.
// ---------------------------------------------------------------------------

impl App {
    /// The body press: a press on any part of a row that is not a capsule is the row's
    /// `Details ›` (Settings ▸ Messages opens at that entry in the pressed window and the row is
    /// marked seen). A decision row is answered only by its capsules. The mouse (`press_band`)
    /// and a screen reader's activate share this entry, so they never disagree.
    pub(crate) fn press_message_body(&mut self, wid: WindowId, id: MessageId) {
        if self.messages.live(id).is_none() {
            return;
        }
        let _ = self.perform_intent(wid, id, Intent::Details);
    }
}

// ---------------------------------------------------------------------------
// The toolchain reporters' glue.
// ---------------------------------------------------------------------------

impl App {
    /// The dialect of the atpkg hook the frozen-tab note tells a person to source: the shell this
    /// window spawns, read from the spawn configuration (`--shell` / config `shell` / `$SHELL`)
    /// as the spawn seam reads it, at post time so it follows a reloaded config.
    pub(crate) fn hook_dialect(&self) -> HookDialect {
        HookDialect::from(crate::spawn::detect_spawn_shell(
            self.session_factory.shell_override.as_deref(),
        ))
    }

    /// R32 — `managed-current:`, from the pass's marker or `appnotice`: the
    /// record for `text` with the count of frozen tabs and this window's hook
    /// dialect — never a row (the silent lane, upstream dbf97ecff), and
    /// RECORDED ONLY WHEN ITS SET OF VERSIONS CHANGES (2026-09-23, design
    /// ruling 58; audit 2026-10-09): atpkg prints the marker at the end of
    /// every pass, four and more times a day, and the log ring (`LOG_CAP`) and
    /// Settings ▸ Messages are for news. A new version, or a program the pass
    /// could not check (each program's standing is part of the set), is a new
    /// record; the same set with the tabs counted differently — a frozen tab
    /// adopted, the sessions the upgrade moves — RE-WORDS the record already
    /// written, in place ([`Self::reword_record`]): measured that day, one
    /// install was recorded three times in five seconds (12:48:46/49/51) as its
    /// rows retargeted, 28 records for 11 updates. That memory
    /// (`managed_current_recorded`) is a log-noise rule, not the retired glass
    /// rule of ruling 34. `true` when a record was written or re-worded; the
    /// caller logs INFO then, DEBUG otherwise.
    ///
    /// THE TABS ARE COUNTED BY WHAT THEY RUN (gap audit 2026-09-24,
    /// [`Self::tabs_behind`]): a frozen tab whose foreground is an agent is
    /// never handed the hook to type, the agent sessions behind their own
    /// program's managed build (a Codex against the managed codex) turn the
    /// title to "installed", and a change in those counts
    /// re-derives the words from the text remembered here.
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
        // Re-word only a record still in the ring as `Recorded`; otherwise the counts are a new
        // record's.
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

    /// A harness note (`aterm ctl appnotice harness <text>`): a `harness`-tagged record, never a
    /// row ([`crate::message_reporters::harness_note`]); a note repeated word for word is not
    /// written again. `true` when a record was written.
    pub(crate) fn record_harness_note(&mut self, text: &str) -> bool {
        let msg = crate::message_reporters::harness_note(text);
        if self.harness_note_recorded.as_deref() == Some(msg.title.as_str()) {
            return false;
        }
        self.harness_note_recorded = Some(msg.title.clone());
        self.record_message(msg);
        true
    }

    /// `machine-settings:`: one record per item, in wire order
    /// ([`toolchain_words::machine_settings`]), never a row.
    pub(crate) fn post_machine_settings(&mut self, text: &str) {
        for msg in toolchain_words::machine_settings(text) {
            self.record_message(msg);
        }
    }

    /// `foreign:`: what one of a pass's looks found that aterm leaves alone, with atpkg's fix
    /// ([`toolchain_words::foreign_report`]). Each look (the rustup seam, the PATH shadow
    /// reconcile) reports on every run and owns one exceptions entry
    /// ([`toolchain_words::foreign_tools_entry`]): raised or re-worded while its report names
    /// something, ended by one that names nothing.
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

    /// `c-toolchain:` — the C toolchain's verdict ([`atpkg::cli::C_TOOLCHAIN_MARKER`]).
    /// `missing` or `broken` files the one entry naming atpkg's fix
    /// ([`toolchain_words::c_toolchain_entry`]; on macOS `aterm init --only base.clt`),
    /// a fact for later and never on screen (docs/DESIGN-init-system-2026-10-20.md
    /// §4.2), and ends the other cause's; the same verdict again is a duplicate. `ready`
    /// ends both. The window runs no probe of its own: atpkg's pass did. A body this
    /// build cannot read changes nothing.
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

    /// A pass opens (`net-starting:`). Only a first run opens the row (`first_run`, the lane's
    /// verdict for the child that printed it: `crate::pass_is_first_run`); a routine pass's
    /// announcement is the arm's log line alone. The first-run child latches the row until its
    /// exit ([`Self::toolchain_pass_ended`]).
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
        // The row the lane opened ahead of this announcement takes its words IN PLACE:
        // one row, opened once and folded once, whatever the size it now names.
        let announced = toolchain_words::announced(detail);
        match self.toolchain_ahead_row.take() {
            Some(id) if self.restate_message(id, restatement_of(&announced)) => {}
            _ => {
                self.post_message(announced);
            }
        }
    }

    /// TC-5 — a FIRST RUN the lane knows of before any child says so
    /// (`Wake::PkgFirstRunAhead`, `crate::first_run_ahead`: no ALab program in the
    /// store, and a machine that consents to the adoption): the row opens NOW, as the
    /// announcement's with no size yet (`toolchain_words::announced("")`), and LATCHES
    /// like one; the update's announcement restates it in place
    /// ([`Self::announce_toolchain_pass`]). The next child exit — the launch seed's — is
    /// carried over instead of folding it ([`Self::toolchain_pass_ended`]). Posted
    /// again before a retried seed, or an update that stood aside at the lock: it then
    /// only carries that exit too. `true` when a row went up.
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

    /// The lane runs no update after the seed after all (it parks, or Automatic updates
    /// went off): the row it opened ahead, if no announcement took it over, is WITHDRAWN —
    /// no outcome claimed, as a vanished read retires a live row
    /// ([`Self::retire_live_toolchain`]) — and a person's verb waiting behind it gets its
    /// own row back.
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

    /// One classified `progress.json` read from the child-scoped tailer (`Wake::PkgProgress`;
    /// `None` = the file vanished at child exit), applied to the pass's row
    /// ([`toolchain_words::KEY_PASS`]); `first_run` is the lane's verdict for the tailed child.
    ///
    /// * A first run paints, and so does a routine pass that planned at least
    ///   [`toolchain_words::HEAVY_PASS_BYTES`] (design §10.5 H8), revealed after the progress
    ///   grace. Either latches the row: one open, one fold, at the child's exit. A read restates
    ///   the live row in place (no log line) or posts it, and records the meter's high-water mark
    ///   for the next read's clamp. A light routine pass paints nothing.
    /// * A read of a pass that is over, or a vanished file, claims no outcome (the markers'
    ///   records do): an unlatched row is withdrawn at once; a latched one is the exit's to fold.
    /// * A plan of nothing leaves the glass alone, and so does a held (carried) row. A first
    ///   run's failure row ([`toolchain_words::first_run_short`]) is replaced by a painting read,
    ///   so a retry shows its progress at once; an over or vanished read leaves it.
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
        // Who waits decides (the rule is the engine's): a first run and a person's Settings verb
        // are waited on and paint whatever they weigh; a routine pass paints only while very
        // heavy.
        let person = self.toolchain_person;
        let waiter = aterm_messages::Waiter::of(first_run || person.is_some());
        let paints = waiter.takes_row(heavy) || self.toolchain_row_latched;
        // A high-water mark belongs to the live meter it was read from: with
        // no live row up there is none to clamp against.
        let peak = live_id.and(self.toolchain_pass_peak.clone());
        // A person's Install lays the set down: its title is the install's.
        let routine = !first_run && person != Some(crate::message_reporters::PackagesVerb::Install);
        match toolchain_words::snapshot_words(snap, peak.as_ref(), routine) {
            SnapshotWords::Vanished | SnapshotWords::Over => match person {
                _ if self.toolchain_row_latched => {}
                None => self.retire_live_toolchain(None),
                // The holder the person queued behind is done: its last frame leaves, and the
                // row says again that they wait.
                Some(verb) if self.toolchain_person_waiting => {
                    self.restore_person_row(crate::message_reporters::packages_waiting_row(verb))
                }
                // A person's own row is the worker's to fold, at the child's exit.
                Some(_) => {}
            },
            SnapshotWords::Quiet => {}
            SnapshotWords::Live { message, pass_id } => {
                if !paints || self.holder_read_not_the_persons(snap, first_run) {
                    return;
                }
                // The lane's latch is the lane's: a person's pass holds its row
                // by `toolchain_person`, so a later light routine pass is not
                // left latched behind it.
                if first_run || person.is_none() {
                    self.toolchain_row_latched = true;
                    self.toolchain_first_run |= first_run;
                }
                // The person's finished words are for the person's row: a
                // latched lane row (a first run they queue behind) keeps the
                // lane's (`Installed ALab tools`).
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

    /// Withdraw the live toolchain row (meter or announcement): its pass is over and its outcome
    /// is a marker's record, so `appstatus` lists no finished activity for it. The indicator ends
    /// with `echo` (Complete or Fault at the child's exit; a fade for a vanished read). A held
    /// (carried) row stands.
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

    /// A marker answered a pass: the outcome is `msg`'s record ([`toolchain_words::installed`] /
    /// `ended` / `failed`, each [`Hold::LogOnly`]), and the live row folds, unless it is latched
    /// (a first run or a heavy routine pass, whose answer may be one sub-pass's) or a person's
    /// verb holds it: then its child's exit folds it.
    pub(crate) fn record_toolchain_outcome(&mut self, msg: Message) {
        if !self.toolchain_row_latched && self.toolchain_person.is_none() {
            self.retire_live_toolchain(None);
        }
        self.record_message(msg);
    }

    /// A marker said the latched first run is ending short (`seed-failed:`, `net-failed:`,
    /// `seed-partial:`, `seed-nothing:`, a `seed-unusable:` that is not a fact about the
    /// machine): its failure row is held for the child's exit ([`Self::toolchain_pass_ended`]).
    /// Otherwise (a heavy routine pass, or no latched row) only the record stays: the versions
    /// already installed keep working.
    pub(crate) fn note_toolchain_first_run_short(
        &mut self,
        how: toolchain_words::FirstRunShort,
        cause: &str,
    ) {
        if self.toolchain_row_latched && self.toolchain_first_run {
            self.toolchain_first_run_short = Some(toolchain_words::first_run_short(how, cause));
        }
    }

    /// The lane's child exited (`Wake::PkgPassEnded`): whatever it held on the glass folds now
    /// (one open and one fold per latched child, however many sub-passes it ran), its indicator
    /// ending Complete when `clean`, Fault when not. The record stays `Withdrawn` (`appstatus`
    /// claims no outcome for it). A first run that exits unclean after a marker said it ended
    /// short leaves its failure row behind the Fault echo: the person has something to do.
    pub(crate) fn toolchain_pass_ended(&mut self, clean: bool) {
        // The LANE'S row outlives its launch seed (`Self::open_first_run_ahead`): the
        // update that follows takes it over, and that child's exit is the one fold.
        if std::mem::take(&mut self.toolchain_ahead_carry) {
            return;
        }
        // A row the lane opened ahead that no announcement took over: the child installed
        // nothing over the wire (an architecture no build serves, a set it found complete),
        // so a CLEAN exit claims no Complete and the row is withdrawn; an unclean one is
        // still the Fault it was.
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
                    // A first run that ended short keeps its failure row.
                    Some(msg) if !clean && first_run => {
                        self.post_message(msg);
                    }
                    // Still queued (the lock is a moment away): it says so.
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

    /// A person's Settings ▸ Packages verb (`Wake::PkgPersonPass`). They wait for it, so its
    /// pass takes the band's row whatever it weighs. `Began` puts up the verb's busy row after
    /// the progress grace (a quick no-op Check never reaches the glass); `Waiting` and `Running`
    /// say it queued behind another pass or took the lock; the tailer's reads add the fill and
    /// ETA (`apply_toolchain_snapshot`); `Ended` folds it with the worker's echo. The outcome is
    /// the markers' records and the page's headline.
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
                // Back to the verb's own busy row, whatever the holder's reads left on it (its
                // fill, its ETA, its finished words). A latched lane row is the lane's until its
                // exit, which puts the person's row back itself.
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
                    // A latched lane row (a first run) is the lane's to fold.
                    return;
                }
                self.retire_live_toolchain(Some(echo));
            }
        }
    }

    /// Put `row` (a person's busy verb or waiting row) on the lane's key: a row not up yet is
    /// posted (its grace from now; it supersedes a first run's failure row, as a retry's reads
    /// do); a busy row already up is restated in place, its reveal kept, when `restate`; a row
    /// with a fill (a plan's meter) or a held one is left alone.
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

    /// Put the person's `row` back over whatever the holder's reads left on the lane's key, its
    /// fill included (which [`Self::show_person_row`] keeps), and drop the holder's high-water
    /// mark. A held row is left alone; a failure row, or none, is posted over.
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

    /// Whether a painting read is the holder's and must not paint the person's row:
    ///
    /// * a read tailed while the person queues is the holder's: its pid is noted, and it paints,
    ///   as what they wait behind;
    /// * once the lock is taken, a read of that pid is late (another thread's tailer can land it
    ///   after `lock-acquired:`) and paints nothing;
    /// * a latched first run is painted by the lane's own tailer: the person's tailer follows the
    ///   same file (`first_run` false), and its reads would restate the first run's row in the
    ///   person's words, turn about with the lane's.
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

/// What the packages worker reports about a person's Settings ▸ Packages verb
/// (`Wake::PkgPersonPass`; `App::person_pass`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonPass {
    /// The verb's child is about to run.
    Began(crate::message_reporters::PackagesVerb),
    /// It queued behind another pass at the store lock (`lock-waiting:`).
    Waiting,
    /// It took the lock (`lock-acquired:`).
    Running,
    /// Its child exited, and how its row ends.
    Ended(EchoKind),
}

// ---------------------------------------------------------------------------
// The update reporters' glue.
// ---------------------------------------------------------------------------

/// The update's flow row, as state (`App::update_flow`): the lane's live `update.progress` row,
/// the version it names and its phase. What a row is lives in state, never in its glyph or
/// title; a reader re-validates `id` against the center (a row that folded, went stale or was
/// superseded by a wire post is not the flow).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateFlow {
    /// The live row.
    pub(crate) id: MessageId,
    /// The version it names (empty for the QA seam's same-image reload).
    pub(crate) version: String,
    /// Where the update is.
    pub(crate) phase: FlowPhase,
}

/// Where an update's flow row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlowPhase {
    /// Bytes arriving.
    Downloading,
    /// The container's checks and the stage publish.
    Checking,
    /// Staged, and a press is how it installs: the ready row. (A build the
    /// lane installs by itself is a record and takes no row.)
    Staged {
        /// The staged build.
        build: u64,
    },
    /// The outgoing process is handing over.
    Installing,
    /// The successor, before Commit.
    Finishing,
}

impl App {
    /// The lane's flow state while its row is still live, never a stale memory of one that folded
    /// or was superseded.
    pub(crate) fn live_update_flow(&self) -> Option<&UpdateFlow> {
        self.update_flow
            .as_ref()
            .filter(|flow| self.messages.live(flow.id).is_some())
    }

    /// Post `msg` as the lane's flow row (it carries [`update_words::KEY_PROGRESS`]) in `phase`,
    /// and remember it as the flow. A held row the lane replaces (a ready row) is resolved first,
    /// so `appstatus` keeps it as a finished activity (a live row replaced is only superseded and
    /// records nothing; design §5.3). One sync.
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

    /// A live row under `key` that is held (a `Default` / `For` hold): resolved now by its tone,
    /// with no sync; the post that replaces it syncs once.
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

    /// Re-word the live flow row in place (`r`), and move its phase when the
    /// words say a new one (`phase`). `false` when there is no live flow row.
    pub(crate) fn restate_update_flow(&mut self, r: Restatement, phase: Option<FlowPhase>) -> bool {
        let Some(id) = self.live_update_flow().map(|flow| flow.id) else {
            return false;
        };
        if let (Some(phase), Some(flow)) = (phase, self.update_flow.as_mut()) {
            flow.phase = phase;
        }
        self.restate_message(id, r)
    }

    /// One `Wake::UpdateHealth` from the check thread (R38), on the band and on
    /// record; `true` when it ANNOUNCED a failing class, which is the one case
    /// owed the OS banner (the caller's). The shapes:
    ///
    /// * THE LEDGER HEALED (`aterm_update::HEALTH_RECOVERED_TITLE`): the warning
    ///   leaves if it is still up, and the healing is on record against what it
    ///   said ([`Self::heal_update_health`]) — each warning the healed class
    ///   answers, but never the watchdog's stall, which the machine's ledger
    ///   says nothing about ([`HealthProof::answers_from_the_ledger`]).
    /// * THE COUNT MOVED on a class this process already announced
    ///   (`HEALTH_RESTATED_TITLE`): a log line, nothing else — the row says which
    ///   half is broken and since when, neither of which a count moves, and the
    ///   person was told when it was announced (design ruling 59).
    /// * A LINUX CRASH LOOP ENDED (`aterm_update::INSTALL_CRASH_LOOP_TITLE`): the
    ///   install half's warning in the words that name the build that failed, said
    ///   EVERY TIME — each crash loop is news of its own, so it re-opens the install
    ///   latch first; a second before the first was answered was a log line and no
    ///   row (ATERM-KI-1015-2, U21).
    /// * A CLASS IS FAILING: say what is wrong, where the user is looking — the
    ///   title names the broken half ("Couldn't install updates"), the excerpt
    ///   since when; the whole sentence — count, cause, command — is the row's
    ///   detail behind `Details ›`, and Settings ▸ Software Update, re-read by
    ///   the caller, headlines the same verdict. The row holds the warning's 45 s,
    ///   once per class per launch (the updater latches the announcement): the
    ///   2026-09-10 standing row was a ⚠ with nothing to press for the life of the
    ///   process, its cause cut off at 110 columns. What it said is kept, so the
    ///   healing is recorded against it.
    ///
    /// The latch is here, at the one door: several producers announce "Couldn't install updates"
    /// (the updater's streak, its overdue notice, `App::announce_automatic_apply_stranded`), so a
    /// kind this process already said that no proof has answered (`update_health_latched`) is a
    /// log line and `false`; a healing that answers it re-opens the latch for the next episode.
    /// Keyed by kind, not words ([`update_words::HealthKind::of_updater_title`]), so a warning an
    /// older build raised in its own words still heals.
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
        // A Linux crash loop's end is news of its own each time (ATERM-KI-1015-2, U21): a
        // second before the first was answered is a row too, never the latch's log line.
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

    /// A failing health warning of `kind` through the one door: once per kind
    /// per launch until a proof answers it ([`Self::note_update_health`]).
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
        // An attempt still in flight keeps its row and its rim: a warning that lands while the
        // successor boots or holds for its park is not that attempt's refusal (the install goes
        // on to Commit, and the rim explains the frozen frame the park shows). The attempt's own
        // completion retires them if it does not take over (`reduce_returned_handoff_completion`).
        if !self.update_handoff_in_flight() {
            self.retire_update_installing();
        }
        let msg = update_words::health_warning(kind, title, body);
        // The same warning an older build raised under the family key (a
        // carried row) is this one said again: it leaves for its successor.
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
        // The warning holds its 45 s and folds into the record
        // (`update.health.<kind>`); a failure of the same kind that has gone on
        // for a day is one dismissable line of the digest, under its own key
        // (`update.review.<kind>`).
        self.post_message(msg.clone());
        self.owe_unless_seen(msg);
        self.claim_update_health_entry(kind, body);
        true
    }

    /// AN INSTALLED LINUX EXECUTABLE WAITS FOR THE NEXT ATERM A PERSON STARTS
    /// (`aterm_update::INSTALL_AWAITS_START_TITLE`; the review of
    /// ATERM-KI-1015-2, U21) — a notice, never the install half's warning. The
    /// install worked, and on Linux no lane starts an installed executable
    /// inside a running aterm, so this wait is how every automatic install there
    /// ends: "Couldn't install updates", and a day later "Updates are not
    /// installing", were false. It latches no kind, claims no digest line and
    /// owes no banner (the caller's `false`); it holds its 45 s and folds into
    /// the record, and is shown once a band can show it
    /// ([`Self::owe_unless_seen`]).
    fn note_install_awaits_start(&mut self, body: &str) {
        aterm_log::info!(
            "update: {}: {body}",
            aterm_update::INSTALL_AWAITS_START_TITLE
        );
        self.say_owed_unless_seen(update_words::install_awaits_start(body));
    }

    /// The wait is over (`aterm_update::INSTALL_WAIT_OVER_TITLE`, `version` the
    /// installed one): its notice leaves the band, withdrawn — what ended it, a
    /// start or a rollback, is said by its own producer — and is owed no more.
    fn end_install_awaits_start(&mut self, version: &str) {
        aterm_log::info!(
            "update: {}: {version}",
            aterm_update::INSTALL_WAIT_OVER_TITLE
        );
        self.withdraw_owed_row(update_words::KEY_AWAITS_START);
    }

    /// Post `msg`, owed to a band that can show it when none can now
    /// ([`Self::owe_unless_seen`]).
    pub(crate) fn say_owed_unless_seen(&mut self, msg: Message) {
        self.post_message(msg.clone());
        self.owe_unless_seen(msg);
    }

    /// The row under `key` is answered: it leaves the band, withdrawn, and is
    /// owed no more.
    pub(crate) fn withdraw_owed_row(&mut self, key: &str) {
        self.forget_owed(key);
        if let Some(id) = self.messages.live_by_key(key).map(|l| l.id) {
            self.withdraw_message(id);
        }
    }

    /// A ROW NOBODY COULD SEE IS OWED TO THE FIRST WINDOW THAT CAN SHOW IT
    /// (ATERM-KI-1015-2, U37). Said while no window's band is on screen
    /// ([`band_on_screen`]: every window minimized, covered or not yet shown,
    /// or none open), the row held its 45 s and folded unseen — and an
    /// update-health warning's latch kept its kind from being said again for
    /// the rest of the launch: a stuck update was told to nobody. So the row is
    /// owed, by its key, in the words it was said in, and the settle keeps it
    /// until a band has shown it for its whole hold ([`Self::keep_owed_rows`]).
    /// A row owed already takes the new words. A row with no key has nothing to
    /// be owed by.
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

    /// The row owed under `key` ([`Self::owe_unless_seen`]), if any, takes
    /// `msg`'s words: a re-wording owes nothing new.
    fn reword_owed(&mut self, key: &str, msg: &Message) {
        if let Some(owed) = self.rows_owed.iter_mut().find(|owed| owed.key == key) {
            owed.msg = msg.clone();
        }
    }

    /// Nothing is owed under `key` any more: what the row said was answered.
    fn forget_owed(&mut self, key: &str) {
        self.rows_owed.retain(|owed| owed.key != key);
    }

    /// Whether any window's band is on screen ([`band_on_screen`]).
    fn any_band_on_screen(&self) -> bool {
        self.windows.values().any(|ws| band_on_screen(ws))
    }

    /// The owed rows ([`Self::owe_unless_seen`]) at `now`: each waits on the
    /// band until a band on screen has shown it for its whole hold, then is paid.
    ///
    /// * WHILE NO BAND IS ON SCREEN its hold starts again at every settle, and
    ///   so does its queue patience when other rows hold the band's slots, so it
    ///   never folds unseen however long nobody looks — and it stays the one
    ///   record it was posted as. Posting it again once seen put a second entry
    ///   in Settings ▸ Messages beside the fold nobody saw (U37's reviews).
    /// * WHEN THE ROW REACHES AN ON-SCREEN BAND it takes its whole hold from
    ///   there. A visible band with this row still queued pays nothing: queue
    ///   patience keeps restarting, including after a partly seen row was
    ///   requeued. Seen for part of it and then hidden — the window minimized or
    ///   covered, or a handoff's boot window not yet shown — it waits again, and
    ///   is not used up unseen.
    /// * ONE THAT LEFT UNSEEN ANYWAY (a staleness cap, or a flood that took the
    ///   queued rows) is posted again in the words it is owed in once a band is
    ///   on screen.
    ///
    /// What answered a row since — an update-health warning healed, the Linux
    /// wait over — forgot it ([`Self::forget_owed`]). Nothing moves under a
    /// handoff's freeze, which suspends the holds itself. Asks nothing of the
    /// windows while nothing is owed, so an idle settle stays free.
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
                // Wait for a real vacancy before re-posting. At the cap a
                // protected queue may evict the newcomer immediately; retrying
                // that post every settle would flood the log without a row.
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

    /// The digest line a failing `kind` owes once it has gone on for a day
    /// ([`update_words::update_health_entry`]), its horizon counted from the
    /// streak's start where `body` dates it ([`update_words::health_entry_delay`]).
    fn claim_update_health_entry(&mut self, kind: update_words::HealthKind, body: &str) {
        let now_unix = i64::try_from(wall_stamp_now().unix_ms / 1000).unwrap_or(i64::MAX);
        self.except_after(
            &update_words::update_health_entry(kind, body),
            update_words::health_entry_delay(body, now_unix),
        );
    }

    /// A health warning of `kind` already said this launch whose remedy no longer holds (the
    /// automatic lane's stranded warning, its retry withdrawn): the live row takes the new words
    /// in place (no new row, no banner, its hold as it was), and one already folded off the glass
    /// is followed by a record in them, so Messages never keeps a promise the lane withdrew. The
    /// healing is recorded against the new words.
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

    /// The one door of the OS banner an update-health announcement owes (after
    /// [`Self::note_update_health`] or [`Self::note_update_health_as`] returned `true`). It opens
    /// only when no lane can install until the owner acts
    /// ([`crate::update_apply_trouble::ApplyRetry::NeedsPerson`]: the installed copy is not the
    /// signed release, or its folder cannot be written), and the banner names that remedy; any
    /// other failure is a record and, after a day, a digest line. `desktop_alerts` off (the
    /// default) posts nothing. Delivered off the UI thread (`notify::deliver` blocks on a
    /// notifier subprocess), with no focus suppression (updater health belongs to no tab);
    /// recorded instead under `cfg(test)` ([`UPDATE_HEALTH_BANNERS`]). `true` when posted.
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
        // No desktop banner for update health off macOS.
        #[cfg(all(not(target_os = "macos"), not(test)))]
        {
            let _ = (title, body, remedy);
            false
        }
    }

    /// The ledger healed as far as `proof` can tell: each health warning
    /// ([`update_words::KEY_HEALTH`]) the proof answers ([`HealthProof`]) resolves if still up,
    /// and its healing is recorded once against what it said ([`update_words::health_recovered`]).
    /// The kind names the broken half, so a download cannot heal "Couldn't install updates" (the
    /// ledger clears every streak on a whole-pipeline success except an install streak). A
    /// warning the proof does not answer stays, remembered, for the proof that does.
    pub(crate) fn heal_update_health(&mut self, proof: HealthProof) {
        self.heal_update_health_where(|kind| proof.answers(kind));
    }

    /// [`Self::heal_update_health`] over the kinds `answered` picks: their latch, their rows,
    /// their day's entry and the record of their healing go, the rest stay. The one heal of an
    /// update-health warning: the ledger's recovery, this process's own proof and the checker
    /// watchdog's stall and lock-held heals all come through here.
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
        // The day's entry, raised or not yet, ends with the failure it stood for.
        for kind in update_words::HealthKind::ALL {
            if answered(kind) {
                self.except_clear(&update_words::update_health_entry(kind, "").key());
            }
        }
        self.record_update_health_healed(answered);
    }

    /// What a health warning of this kind said, kept for its record of healing, one entry per
    /// kind: the same kind said again replaces its own entry and another kind's stays, so each
    /// healing is recorded against the warning it answers.
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

    /// The healing of every remembered warning whose kind `healed` picks goes on record (one
    /// record per warning, each against its own words, oldest first) and leaves the memory; the
    /// others stay for the proof that answers them.
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

    /// The live health warnings whose kind `which` picks — each read by its key,
    /// or, carried from an older build under the family key, by its words
    /// ([`update_words::HealthKind::of_row`]).
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

    /// The new build took over (or a cold-lane boot found it already running): the lane's live
    /// flow row resolves `Ok` and the news is a record ([`update_words::landed`]: "Updated to
    /// aterm vX"), with how long the update took when this process or its predecessor downloaded
    /// this build. `repainted` is how many tabs the seamless successor adopted onto a blank
    /// screen ([`Self::seamless_repainted_tabs`]; 0 on the cold lane): the record names them.
    pub(crate) fn post_update_landed(&mut self, version: &str, build: u64, repainted: usize) {
        self.heal_update_health(HealthProof::Landed);
        let took = self
            .update_verified
            .take()
            .filter(|(verified, _)| *verified == build)
            .map(|(_, at)| Instant::now().saturating_duration_since(at));
        // The flow row by state or, for a carry from a build before the flow state, the live
        // row on the lane's key.
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
        // The landing answers the apply lane's last outcome ("Update waits for you", "Couldn't
        // install aterm vX · Install now"), which would otherwise take a fresh hold beside
        // "Updated to aterm vX". Withdrawn, not resolved: no ✓ stands on a failure's words.
        for key in [update_words::KEY_OUTCOME, update_words::KEY_WAITS_FOR_YOU] {
            if let Some(id) = self.messages.live_by_key(key).map(|l| l.id) {
                self.messages.withdraw(id, Instant::now());
            }
        }
        // …and the wait it said is over: a later build's is news.
        self.update_wait_said = false;
        self.record_message(update_words::landed(version, build, repainted, took));
        if repainted > 0 {
            aterm_log::info!("update landed: {repainted} tab(s) adopted onto a blank screen");
        }
    }

    /// How many tabs this seamless successor adopted onto a blank screen
    /// (`App::handoff_repainted_tabs`, counted at boot before the restore drained the adopted
    /// set): what [`Self::post_update_landed`] names on the seamless lane.
    pub(crate) fn seamless_repainted_tabs(&self) -> usize {
        self.handoff_repainted_tabs
    }

    /// The staged decision, raised once (design §10.5 H2): when `build` is the staged build, its
    /// posture is one where a press installs it, and no staged row is up, post the `aterm vX is
    /// ready` decision as the lane's flow row. Once per build and posture: every reconcile
    /// re-states the posture, and the same question after the person let it lapse would be a
    /// nag; a posture that has just become a decision (a lane that stopped) raises it again.
    /// `posture` is the caller's `apply_posture_for(build)` (a headless App's own is always the
    /// handoff-off record).
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

    /// The id of the lane's flow row while it is the staged one: the build is ready (and, where a
    /// press installs, a press may).
    pub(crate) fn staged_update_row(&self) -> Option<MessageId> {
        self.live_update_flow()
            .filter(|flow| matches!(flow.phase, FlowPhase::Staged { .. }))
            .map(|flow| flow.id)
    }

    /// The staged row is obsolete (its bytes are installed and activating, `delivered`: it
    /// resolves `Ok`; or the artifact it offered is gone: withdrawn with no outcome, since nothing
    /// was installed and no ✓ may stand beside `Couldn't finish the update`), so it leaves now
    /// rather than standing beside the outcome that says so.
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

    /// The switch to a new version begins, on record ([`update_words::switch_started`]), flushed
    /// to `messages.log` now: a line queued for after the exec, or inside the park's few
    /// milliseconds, is never written. `build` is the running build; a same-image switch
    /// (`same_image`) installs nothing, whatever is staged. A switch that then stops says so
    /// ([`Self::record_update_switch_stopped`]).
    ///
    /// Noise bound: every attempt a person makes is on record (a press held for the candidate's
    /// read and run again is one); of the automatic lane's, only its first for a build in this
    /// process, since its ladder retries every few seconds and the ring (`LOG_CAP`) is every
    /// reporter's. `aterm.log` keeps each attempt.
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

    /// [`Self::record_update_switch_started`] without its flush, for the cold lane's
    /// `prepare_cold_exec`, which flushes once, bounded, right after (the person sits through
    /// every wait frozen). `false` when nothing was put on record (the noise bound).
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
        // The automatic lane's later attempts at a build, and a person's press held for the
        // candidate's read and run again, whose start is still on record with no stop since: one
        // fact, one entry.
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
        // One fact, one entry: an Installing row this attempt posted (the explicit lane with
        // nothing up) is already that line in the log; a row re-worded in place, or none (the
        // automatic lane), is not.
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

    /// The switch that did not happen, on record ([`update_words::switch_stopped`]): an attempt
    /// [`Self::record_update_switch_started`] wrote down ended with this process still running
    /// (refused, failed, or stood down to try again by itself: `routine`, in `why`'s words), so
    /// the record never leaves an "Installing" that was not. Nothing for an attempt that never
    /// reached the record.
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

// ---------------------------------------------------------------------------
// The band's motion (design §10.8).
// ---------------------------------------------------------------------------

/// Whether window `ws`'s band is on screen: a real OS window that is not
/// occluded (minimized, on another workspace, fully covered). A headless
/// window has no OS window, so its band never moves. `WindowEvent::Occluded`
/// is not reported on Windows or Wayland (crates/aterm-window, the platform
/// notes above `Occluded`), so on Windows a MINIMIZED window is asked directly
/// (review 2026-09-24: it kept its 1 Hz text ticks, and its frames while
/// focused or recorded); Wayland cannot say. macOS reports minimizing as
/// occlusion, and so does X11: a window the window manager unmapped
/// (iconified, or on another workspace) or marked `_NET_WM_STATE_HIDDEN`, as
/// well as one `VisibilityNotify` says is fully covered, which a compositing
/// window manager never lets it say.
pub(crate) fn band_on_screen(ws: &crate::WindowState) -> bool {
    // A handoff boot's window is hidden until its first real present
    // (`pending_reveal`), and not yet occluded: nothing on it is seen
    // (ATERM-KI-1015-2, U37).
    let shown = ws.pending_reveal.is_none() && !ws.occluded;
    #[cfg(test)]
    {
        crate::work_counts::on_screen_asked();
        if ws.band_on_screen_for_test {
            return shown;
        }
    }
    // macOS and X11 report minimizing as occlusion, and asking on every loop
    // turn would buy nothing there (on X11, a round trip to the server).
    let minimized =
        |w: &aterm_window::window::Window| cfg!(windows) && w.is_minimized() == Some(true);
    shown && ws.os_window.as_deref().is_some_and(|w| !minimized(w))
}

impl App {
    /// How window `wid`'s band draws motion: moving while its effect policy animates
    /// `BandProgress` (focused or recorded, no Reduce Motion, no `motion = "reduced"`, no
    /// load-shed latch), Serious Mode allows it, the band is on screen, and no handoff freezes
    /// the band ([`Self::message_holds_frozen`]: the screen is parked); still otherwise. Graded
    /// unless an OS High Contrast palette owns the chrome. The rule is the engine's
    /// (`drive::look`); the readings are this host's.
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

    /// The next instant any on-screen window's band needs a motion frame (or a time word's
    /// tick): the loop's `DeadlineOwner::MessageBandMotion`, the band's only clock. `None` when
    /// idle (no committed row, none on screen, nothing live or echoing) and while a handoff
    /// freezes the band (the parked frame stays as it is). The fold is the engine's
    /// (`drive::motion_deadline_over`).
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

    /// The on-screen windows whose band is due a new motion frame at `now`: the deadline read at
    /// the frame each last presented has passed (or none was prepared while something moves), or
    /// the frame on glass was drawn in another look, so the move to or from the still form never
    /// depends on each trigger asking for its own redraw. Nothing while a handoff freezes the
    /// band. The rule is the engine's (`drive::due_views`).
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

    /// Prepare window `wid`'s band motion frame for a present at `now` in the
    /// window's own look: the layout (cached by center fingerprint and
    /// width) and one frame of the center's motion. Returns the frame's
    /// fingerprint — the RepaintKey's motion term — 0 when nothing moves.
    pub(crate) fn prepare_band_motion(&mut self, wid: WindowId, now: Instant) -> u64 {
        let look = self.band_look(wid);
        self.prepare_band_motion_with(wid, now, look)
    }

    /// [`Self::prepare_band_motion`] in an explicit look — the capture
    /// harness's injected instant and look (design §10.12), so a capture is a
    /// pure function of the center's state and that instant. The frame is the
    /// engine's (`drive::View::prepare`).
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

// ---------------------------------------------------------------------------
// The handoff carry (design §3.8).
// ---------------------------------------------------------------------------

impl App {
    /// The live rows and the undrained log lines for the handoff manifest:
    /// the center's [`Carry`] in the manifest's own plain-data shape.
    #[cfg(any(unix, test))]
    pub(crate) fn carried_messages(&self) -> crate::session_store::MessagesCarry {
        let mut carry = crate::session_store::MessagesCarry::from(&self.messages.carried());
        carry.update_health_said = self.carried_health_said();
        carry.captured_unix_ms = Some(wall_stamp_now().unix_ms);
        carry
    }

    /// What this process's update-health warnings said that no proof has answered, in the shape
    /// a successor takes up ([`Self::take_up_carried_health_said`]), for the seamless lanes'
    /// window carry and the cold lane's `exec` alike.
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

    /// The file read again at Commit: a late-park successor loaded `messages.log` at its boot,
    /// and its parent recorded until Commit, flushing its last line first. Merged here
    /// ([`aterm_messages::MessageCenter::merge_reloaded_log`]), so the history holds the
    /// parent's records since this process booted and each row it closed with its real ending,
    /// and every id minted from now on is above the file's.
    ///
    /// Synchronous and first in the Commit wake: before anything it does can mint ("Updated to
    /// aterm vX" among them) and before a `messages since=` reader reaches this ring, so no
    /// parent record appears below an id a reader already holds. The read is the launch load's,
    /// bounded ([`crate::messages_store::load_tail`]). Nothing with no writer.
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
        // The Commit wake republishes (its own `sync_messages`) once the
        // carried windows are shown; nothing is painted from here.
        self.messages.merge_reloaded_log(&disk);
        // …and a carried row the parked parent ended after the capture (a press, a resolve, a
        // heal) stays ended. That ends the row, and nothing beside it.
        let ended = self
            .messages
            .end_carried_rows_the_parent_ended(&disk, Instant::now());
        if !ended.is_empty() {
            aterm_log::info!(
                "messages: {} carried row(s) the parked parent ended stay ended",
                ended.len()
            );
        }
        // A health warning the parked parent healed is no longer this process's to heal.
        // Decided by the parent's record of the healing, never by how the row ended: a row up,
        // dismissed or folded (the usual state at a park) heals alike, and a dismissal, an
        // answer or a withdrawal records none.
        if let Some(parent) = self.parked_parent.take() {
            self.forget_health_the_parked_parent_healed(&disk, parent);
        }
    }

    /// Each update-health warning this process remembers whose healing the parked parent
    /// recorded after the capture is healed here as the parent healed it, with no second record
    /// ([`Self::heal_update_health_where`] less the record). Its carried row resolves too, with
    /// no repaint (the Commit wake republishes): the parent may heal it with no row left to end
    /// there, and the carried row here would come back with a fresh hold.
    ///
    /// The parent's record is read from `disk`, the file re-read at Commit: one in
    /// [`update_words::health_recovered`]'s words, written under an id in the parent's band (from
    /// the carry's next id, [`PARKED_PARENT_ID_RESERVE`] wide) as the file numbered it
    /// ([`aterm_messages::MessageLog::file_id_of`]), and stamped at or after the capture
    /// ([`ParkedParent`]). The id alone is no proof: every aterm of the user writes this file
    /// from its own counter.
    ///
    /// Known limit: another window minting within the parent's band that records the same
    /// kind's healing in the same words during the park (seconds) is read as the parent's;
    /// telling them apart needs a writer's mark on every record, not worth it for so narrow a
    /// race.
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

    /// The successor's first frames, before Commit: re-seed the rows the outgoing process
    /// carried (each keeps its id, words and hold under the handoff's staleness cap), queue the
    /// lines the parent never wrote, and, as the handed-off successor with a carried progress
    /// row, say its own phase there ([`update_words::finishing`]) and adopt it as the flow row. A
    /// carried install warning leaves ([`HealthProof::Landed`]); any other stays for the producer
    /// here whose proof heals it ([`Self::take_up_carried_update_health`]). No sync: the row
    /// count is the carried one until Commit. `finishing` is the running build's version.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_carried_health",
            action = "Seed",
            project = "aterm_gui::messages_host::carried_health_conformance::project"
        )
    )]
    pub(crate) fn seed_carried_messages(
        &mut self,
        carry: &crate::session_store::WindowCarry,
        finishing: Option<&str>,
    ) {
        let now = Instant::now();
        let mut carried: Carry = carry.message_carry();
        // An intent only the old process could perform does not cross: `End sessions` hangs up
        // the shells this launch reattached from the keeper (`keeper_recovered`), and an update
        // successor reattached none, so the press would end nothing. The recovery row keeps its
        // words: they are the crashed launch's history.
        if self.keeper_recovered.is_empty() {
            let end = Intent::EndRecovered.encode();
            for message in &mut carried.live {
                message.actions.retain(|action| *action != end);
            }
        }
        // The parent's next id crosses even when no row does: the automatic lane parks with
        // nothing on the band, and the parent recorded past what this process loaded at its
        // boot; `seed_carried` raises the ids first, rows or none.
        let nothing_carried = carried.is_empty() && carried.next_id == 0;
        // The parent mints on from the carry's next id until Commit while this process mints
        // too: the parent keeps below `next_id + PARKED_PARENT_ID_RESERVE` and this process
        // mints above it, so no id is handed out twice and none of this process's records sits
        // below one a `messages since=` reader took from the parent. Before the seed: a record
        // the seed moves off a carried number takes a fresh id, which must be above the band too.
        if self.incoming_handoff_pending && !nothing_carried {
            self.messages.raise_next_id(
                carried
                    .next_id
                    .max(1)
                    .saturating_add(PARKED_PARENT_ID_RESERVE),
            );
            // Where and when the parent's records after the capture begin, read back at Commit
            // (`forget_health_the_parked_parent_healed`). A parent that hands no next id or no
            // capture stamp marks nothing, and none of its records is read as a healing.
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
        // What the parent's warnings said, whether or not their rows still stand: the landing at
        // Commit is the proof, and it is this process's to record (the parent `_exit`s at
        // Commit). A plain restart (`finishing` None) carries nothing it could heal. After the
        // raise: taking a warning up claims its digest line, which may mint an id.
        if finishing.is_some() {
            self.take_up_carried_health_said(&carry.update_health_said);
        }
        if nothing_carried {
            return;
        }
        self.messages.seed_carried(&carried, wall_stamp_now(), now);
        // A row about the old process leaves with it: a session's wait (a paste on its way, a
        // rewrap) belongs to the process that watches it, and this one watches nothing it did
        // not start; the dead accessibility publisher's "restart aterm to retry" is answered by
        // this very process, whose publisher is its own. Carried, either would stand for good.
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
            // A carried install warning leaves now; the landing at Commit records its healing
            // against what the parent said (the parent `_exit`s and never learns). Any other
            // stays up for its producer here.
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
                // Remembered and taken up above with what the parent said, unless an older
                // parent carried the row alone. One entry per carried row: two kinds the parent
                // raised are two records at their healings.
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

    /// WHAT AN UPDATE'S PARENT HAD SAID AND NO PROOF HAD ANSWERED, taken up by
    /// its successor — the seamless lanes' carry
    /// ([`Self::seed_carried_messages`]) and the cold lane's
    /// (`crate::app_update_handoff::take_cold_lane_health_said`, round eight,
    /// finding 79): each warning is remembered, so the proof that answers it
    /// here records its healing against the parent's words — the landing for
    /// an install warning ([`Self::post_update_landed`]) — and each the landing
    /// does not answer is taken up by its producer here
    /// ([`Self::take_up_carried_update_health`]) and, when the parent said it
    /// while no band was on screen (`CarriedHealthSaid::owed`), owed to this
    /// process's windows. An install warning is only remembered: the landing
    /// answers it, so it is neither taken up nor owed. A kind this build does
    /// not know is skipped, never guessed.
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
                // A kind the landing answers (an install warning) is neither taken up
                // nor owed: this process is an update's landing, and shown before it
                // — on the cold lane's first settles, before the first present lands
                // it; on a seamless lane under the freeze — the row would put a failure
                // on the band that the landing is about to answer, and leave a second
                // record of it beside the healing (the second review of U37).
                if !HealthProof::Landed.answers(kind) {
                    self.take_up_carried_update_health(kind, &said.title, words);
                    // Said while nobody could see it, and taken up here unsaid: still
                    // owed, now to this process's windows (the review of U37).
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

    /// A health warning the parent raised and the landing does not answer, handed to the
    /// producer here whose own proof heals it, never latched: a latch only a proof lifts would
    /// stand for the launch (this process's updater announced nothing, so its first good check
    /// clears the streak with no recovery), muting the kind's next episode.
    ///
    /// * Check and download: this process's updater takes the class up as announced
    ///   ([`aterm_update::adopt_announced_health`]), so a streak still failing is restated
    ///   quietly and the ledger's healing sends the recovery ([`Self::note_update_health`]).
    ///   Only when `title` names a ledger class (`aterm_update::health_failing_class`): an
    ///   unknown title reads as the check kind, the strand notice's among them
    ///   ([`aterm_update::STRANDED_TITLE`]), and adopting that would let a ledger that never
    ///   failed "heal" a machine still stranded.
    /// * Stall and lock-held: this process's checker heals them as a replacement generation
    ///   would ([`crate::update_checker_watch::CheckerWatchState`]'s `adopt_carried_stall`,
    ///   `adopt_carried_lock_held`).
    ///
    /// Its digest line is claimed again in `words`, the parent's sentence (the parent's pending
    /// claim died with it); an older parent hands its excerpt alone (`since Sep 14`), which still
    /// dates the line.
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
            // The landing answers it ([`HealthProof::Landed`]).
            update_words::HealthKind::Install => {}
        }
    }
}

/// A row said while no window's band was on screen ([`App::owe_unless_seen`]):
/// the key it is owed by, the words it is shown in once a band is, and whether
/// a band on screen is showing it now ([`App::keep_owed_rows`]).
#[derive(Clone, Debug)]
pub(crate) struct OwedRow {
    key: String,
    msg: Message,
    seen: bool,
}

/// What an update-health warning said ([`App::remember_update_health_said`]): its kind, the
/// title and first line its record of healing is written against, and the sentence it was
/// raised with, which a successor's "To look at" line quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HealthSaid {
    pub kind: update_words::HealthKind,
    pub title: String,
    pub line: String,
    pub body: String,
}

impl HealthSaid {
    /// What `msg`, the warning [`update_words::health_warning`] raised from
    /// `body`, says.
    fn of(kind: update_words::HealthKind, msg: &Message, body: &str) -> Self {
        Self {
            kind,
            title: msg.title.clone(),
            line: msg.detail.first().cloned().unwrap_or_default(),
            body: body.to_string(),
        }
    }
}

/// The ids a seamless successor leaves its parent, which records on from the carry's next id
/// until Commit ([`App::seed_carried_messages`]). The park lasts seconds (the automatic lane's
/// hold is capped at two minutes) and `notice` is refused while parked, so a thousand is far
/// past what the parent records then; beyond it the merge at Commit still keeps both records,
/// the parent's under a fresh id ([`aterm_messages::MessageCenter::merge_reloaded_log`]).
pub(crate) const PARKED_PARENT_ID_RESERVE: u64 = 1000;

/// What marks the parked parent's records after the capture in the file every aterm of the user
/// shares ([`App::forget_health_the_parked_parent_healed`]): the carry's next id, from which the
/// parent mints below [`PARKED_PARENT_ID_RESERVE`] more, and the capture's stamp
/// (`WindowCarry::messages_captured_unix_ms`). Neither names the writer.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkedParent {
    pub(crate) next_id: u64,
    pub(crate) captured_unix_ms: u64,
}

/// Whether a carried row keyed `key` states a fact about the process that posted it, made
/// untrue by a successor's existing ([`App::seed_carried_messages`]): a session wait's row (the
/// waits belong to the process that watches them), or the dead accessibility publisher's (a
/// process gets one publisher; the successor starts its own).
fn describes_the_old_process(key: &str) -> bool {
    aterm_messages::waits::is_wait_key(key) || key == crate::message_reporters::KEY_A11Y_PUBLISHER
}

/// Tier-1 conformance for `NativeUpdateCarriedHealth`.
#[cfg(all(test, unix))]
#[path = "carried_health_conformance.rs"]
pub(crate) mod carried_health_conformance;

/// The band's motion on the host side: the idle law, hidden windows, the frame cadence, the
/// per-frame cost, the still looks, the faces that must not move.
#[cfg(test)]
#[path = "band_motion_tests.rs"]
mod band_motion_tests;

/// What a heal of the update-health warning rests on ([`App::heal_update_health`]). Each proves
/// one more half of updating than the last, so each heals only the warnings it answers, as the
/// updater's ledger does: a whole-pipeline success clears every streak but an install streak.
/// [`Self::Landed`] stands off that ladder and answers the install warning only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HealthProof {
    /// The check reached a release and its bytes are arriving (`Downloading`,
    /// `Verifying`): checking for updates works.
    Checked,
    /// A build downloaded, passed its checks and was staged (`Staged`):
    /// downloading works too.
    Downloaded,
    /// The ledger healed (`HEALTH_RECOVERED_TITLE`, sent only once no class is
    /// failing): installing works too.
    Installed,
    /// A build landed (`post_update_landed`): installing works, and nothing more is proved. Off
    /// the ladder: the build may have been staged before a check or download streak began, and
    /// the updater's ledger keeps those streaks through the install (`record_apply_success`
    /// clears the apply streak only), so a landing answers an install warning alone.
    Landed,
}

impl HealthProof {
    /// What the updater's `HEALTH_RECOVERED_TITLE` proves, from the class it names as the body
    /// (`aterm_update::HealthNotify`): the healing of that class answers exactly the warning
    /// that class raised, so a heal of the download or check streak leaves an install warning
    /// another producer raised up for its own proof. An empty body (a heal that names no class)
    /// reads as the whole ledger healed.
    fn healed_by_ledger(class: &str) -> Self {
        if class.is_empty() {
            Self::Installed
        } else {
            Self::needed_for(update_words::HealthKind::of_class(class)).unwrap_or(Self::Installed)
        }
    }

    /// The proof a warning of `kind` needs before it may leave and its healing be recorded. The
    /// check half and the watchdog's stall need the least (a check that downloads is a check
    /// that works); for the stall, a check this process ran, never the shared ledger's healing
    /// ([`Self::answers_from_the_ledger`]). `None` for a warning no proof of this process's own
    /// answers: another aterm holding the update check's lock is healed only by its streak
    /// ending, by the watchdog that said it.
    fn needed_for(kind: update_words::HealthKind) -> Option<Self> {
        match kind {
            update_words::HealthKind::Install => Some(Self::Installed),
            update_words::HealthKind::Download => Some(Self::Downloaded),
            update_words::HealthKind::Check | update_words::HealthKind::Stalled => {
                Some(Self::Checked)
            }
            update_words::HealthKind::LockHeld => None,
        }
    }

    /// Whether this proof answers a warning of `kind` ([`Self::needed_for`]).
    fn answers(self, kind: update_words::HealthKind) -> bool {
        if self == Self::Landed {
            return kind == update_words::HealthKind::Install;
        }
        Self::needed_for(kind).is_some_and(|needed| self >= needed)
    }

    /// [`Self::answers`] for the ledger's healing ([`Self::healed_by_ledger`]), never the window
    /// watchdog's stall: the ledger is the machine's (a sibling's good check heals it), while
    /// "Update check stopped" is about this process's check thread, which a sibling's healing
    /// does not prove. The stall's own proof is this process's completed check (the watchdog's
    /// `heal_update_checker_stall`) or its progress.
    fn answers_from_the_ledger(self, kind: update_words::HealthKind) -> bool {
        kind != update_words::HealthKind::Stalled && self.answers(kind)
    }
}

#[cfg(test)]
#[path = "messages_host_tests.rs"]
pub(crate) mod tests;

/// `NativeUpdateCarriedHealth`'s actions no shipping function here performs, each waived
/// with where it is bound instead.
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
// The machine's other actions: the environment's steps, time, the frame and
// the `Buggy` members. `BandRise`/`BandFall`/`BandTaken` carry `#[refines]`
// anchors on `App::regrid_message_band`, `StripRise`/`StripUnwant`/`StripBack`
// on `App::set_tab_strip_rows`, `StripFall` on `App::settle_strip_fall`.
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
