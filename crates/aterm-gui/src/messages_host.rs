// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MESSAGE CENTER'S HOST — the `App` glue of the unified message system
//! (docs/DESIGN-unified-messages-2026-09-21.md §3.2, §3.6): the fields that
//! hold the center, the committed band rows and the log writer; the
//! post/restate/resolve seam every reporter calls; the settle-commit-persist
//! sync that runs after every input and on every wake; the deadline the loop
//! folds; the inbox drain at the park; and the INTENT PERFORMER — the one
//! place a plain-data [`Intent`] the engine returned becomes a host action
//! (`ATERM_DESIGN` §2.2: the engine requests, the frontend performs).
//!
//! # Clocks (D4)
//!
//! The engine never reads a clock: the host mints every [`WallStamp`] at
//! ingress ([`wall_stamp_now`]) and passes `Instant::now()` into every call.
//! A message queued before `App` exists carries the instant it was queued
//! ([`crate::message_inbox`]), not the instant it was drained.
//!
//! # The handoff freeze
//!
//! While a seamless self-update is pending — this process parking its readers
//! for the successor, or this process the successor waiting for Commit — ONE
//! predicate ([`App::message_holds_frozen`]) suspends the holds (a row
//! folding on its hold would paint the committed row as a hole for the rest
//! of the freeze) and refuses the re-grid (the successor's layout must match
//! the parent's capture). The staleness caps keep running: a successor whose
//! Commit never arrives must not hold a carried row for the life of the
//! process. The deadline the loop arms is read through the same predicate,
//! so it never arms a wake the settle would decline to act on.
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
//! # The reporters' glue
//!
//! The wake arms in `lib.rs` are thin producers (design §3.10): each calls a
//! words builder (`toolchain_words`, `update_words`) and posts, records or
//! restates. What a builder cannot hold — whether a first-run toolchain child
//! is still live, the live meter's high-water mark, the words an "Installing"
//! row replaced so a refusal can put them back — lives here, on `App`, next
//! to the seam that reads it. The toolchain lane is SILENT BY DEFAULT since
//! the origin/main merge (upstream dbf97ecff + ac7942234, the design's
//! rulings of 2026-09-23): one first-run row, every other toolchain word a
//! [`Hold::LogOnly`] record ([`App::record_message`]).
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

// THE PAGE'S MODEL IS THE ENGINE'S (design ruling 382): the projection, its
// words, the tag order, the filter, the day clock and the feedback line live
// in `aterm_messages::page`, under the names they had here (ruling 385), so
// the Settings page and its tests read on; the host answers the page's seam
// ([`PageDesk`]).
#[cfg(test)]
pub(crate) use aterm_messages::page::MessageActionView;
pub(crate) use aterm_messages::page::{MessageView, MessagesState, tag_words};

/// THE PAGE'S HOST SEAM on this host ([`page::Host`], design ruling 382):
/// what only the App knows about one projection. The staged build and the
/// upgrade clock are read ONCE, as the desk is made for a build, so every
/// `Install now` and every upgrade word of one projection is judged against
/// the same facts; the rest is read when the engine asks.
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

    /// `symlink_metadata`, as [`admissible_log_path`] does at press time: a
    /// symlink planted in the log dir is refused there, so it is not offered
    /// here (the rest of that test needs the log dir, which is a directory
    /// probe per view; the press re-validates the whole rule).
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
    /// The update-health desktop banners `App::post_update_health_banner`
    /// would have posted ON THIS THREAD, as `(title, banner)` (test-only).
    pub(crate) static UPDATE_HEALTH_BANNERS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// The updater classes `adopt_announced_health` would have been handed ON
    /// THIS THREAD (test-only): a test never touches the process's updater.
    pub(crate) static UPDATER_ADOPTED_HEALTH: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Hand `class` to this process's updater as ANNOUNCED by its parent
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

/// A live row re-worded to `msg`'s words: title, detail, meter, actions, hold,
/// severity, glyph, the excerpt flag and the finished words — every field a
/// builder authored,
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
    }
}

/// R16 — the words a decision row takes after *Open Settings*: a TERSE
/// restate (the owner's attention rule, 2026-09-23 — design ruling 46),
/// each further clause of the consent card's caption a line behind Details,
/// the actions cleared (the row stays for the watcher, answered only by the
/// grant or its hold), and the route's SHORT hold. The caption is
/// `consent_card::opened_settings_caption`'s, split at its dash into the
/// title and the tail and at its `; ` joints into lines; the route line drops
/// its leading `System Settings ▸` (the title already names Settings).
///
/// * Opened: the INSTRUCTION is the title — `Enable aterm in Full Disk
///   Access` — and the route in words is the painted excerpt: `openURL:`
///   reports only that Settings TOOK the URL, never that it scrolled to the
///   row, so the route goes out on every outcome. `Opened System Settings`
///   was a confirmation of what the window already shows (review
///   2026-09-24).
/// * Refused: the route IS the title, verb first — `Open Privacy & Security
///   ▸ Full Disk Access` — and `Settings did not open` is the first line
///   behind it: at 60 columns `Settings did not open` alone read as an FYI
///   and lost the one part a person can act on (review round 2, 2026-09-23).
///   It is a failure, so it wears the warning's tone and glyph.
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

/// The path `Open log` may open, or `None`: absolute; every component a
/// plain name — `Path::starts_with` is lexical, so a `..` under the log dir
/// would pass it and escape; a REGULAR file by `symlink_metadata` (a symlink
/// planted in the log dir opens whatever it points at, and the editor would
/// follow it); under `log_dir`. Pure, so the refusals are pinned against a
/// scratch dir standing in for `logging::log_dir()`.
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

/// Open `path` in the platform's text editor, OFF the UI thread, if and only
/// if [`admissible_log_path`] admits it under `logging::log_dir()` — the
/// host-minted paths the crash reporter carries, re-validated at press time
/// because the intent came back from a log or a carry. macOS `open -t` (the
/// default text editor — the artifact's `.log.seen` extension has no
/// registered handler, and `-t` opens it as text), Linux `xdg-open`, Windows
/// `notepad.exe`. No main-thread gate, so `menu::open_in_workspace` is not
/// widened and `is_safe_url` is not touched. `false` when refused or nothing
/// could be spawned.
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
        let status = aterm_uds::spawnfd::command("xdg-open")
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

    /// A RECORD — a message that never touches the glass: posted as
    /// [`Hold::LogOnly`] whatever hold it was built with, so it is in the log,
    /// on `appstatus` (a finished activity) and on Settings ▸ Messages, and
    /// never a row. No settle and no repaint: nothing on the glass moved, and
    /// the park's sweep (`settle_messages`, then the page's publish) persists
    /// it within the loop's next turn — the bars' ledger rows, which the
    /// silent toolchain lane (upstream 2026-09-22) records instead of rows,
    /// never repainted a window either.
    pub(crate) fn record_message(&mut self, msg: Message) -> MessageId {
        self.post_unsynced(msg.hold(Hold::LogOnly), wall_stamp_now())
    }

    /// A config reload's warnings REPLACE the band's config rows as a set
    /// (design §3.10, R4), one settle for the whole of it. This is the FULL
    /// reload's replace, the one that re-derived every family, so every live
    /// `config.`-keyed row is in scope: a fixed config takes its row down (an
    /// empty set is how a fixed typo clears its row), and a family whose words
    /// are unchanged leaves its row alone — see [`Self::replace_config_set`]
    /// for why a repeat never comes back to the glass. A reload that
    /// re-derived only some families scopes to those
    /// ([`Self::replace_config_messages_keyed`]).
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
        // Every family was re-derived, so every `config.` key this process
        // has said something under is in the reload's verdict — but the
        // backend thread's family verdict, unless the font work admitted it.
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

    /// [`Self::replace_config_messages_after_font_work`] for a reload that re-derived only
    /// the rows under `keys` — the content-equal reload, which reads the raw
    /// text for the ignored-key and unaccepted-value families and recomputes
    /// nothing from the parse (`app_config.rs`, the dedupe arm; the contract
    /// its test pins). The prefix resolve there took a still-true keybinding
    /// or font row down on a comment-only save and logged it `resolved` for a
    /// config that was still broken (Phase 3 review, ruling 28).
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
                // Still a problem, now a record only: the row comes down
                // WITHOUT the fix's words, so its record keeps its title and
                // its ⚠ and stays in Problems (design ruling 266).
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

    /// The posting half of a config replace: `msgs` under one stamp, then
    /// ONE settle when anything was resolved or posted to the glass. A family
    /// whose live row still says the same words is not posted at all; a
    /// repeat of words already off the glass is posted as a record
    /// ([`Self::config_repeat`]); a `Hold::LogOnly` family
    /// (`ConfigFamily::RetiredKeys`) is a record, and a record alone moves
    /// nothing on the glass ([`Self::record_message`]).
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
            // A RECORD, not a bare `LogOnly` hold: the door re-derives a
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

    /// Whether `msg` repeats words that already LEFT the glass: the latest
    /// record under its key, from THIS process (a record replayed from disk
    /// was a previous launch's, and a launch says its config problems once),
    /// has the same title and detail and retired by running out — its hold
    /// ended, it was read or dismissed, it went unseen or was evicted, or it
    /// was recorded as a repeat already. A row RESOLVED by a reload means the
    /// problem was fixed or changed, and a family a reload found fixed after
    /// its row had left the glass is remembered as fixed
    /// (`App::config_cleared`): either way the same words coming back later
    /// are news again.
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

    /// Whether a live row's title or detail contains `needle` — the tests'
    /// question for a reporter that used to write a banner line.
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

    /// Re-word a live row in place; `false` when `id` is not live. A restate
    /// costs no log line (the final words land on the row's Retired line).
    /// One that changed nothing the row says — a heartbeat re-feeding the same
    /// reading — moves no revision, so it syncs and repaints nothing (review
    /// 2026-09-24); its staleness cap is re-armed all the same.
    pub(crate) fn restate_message(&mut self, id: MessageId, r: Restatement) -> bool {
        let revision = self.messages.revision();
        let live = self.messages.restate(id, r, Instant::now());
        self.note_message_rule();
        if live && self.messages.revision() != revision {
            self.sync_messages();
        }
        live
    }

    /// A RECORD ([`Self::record_message`]) re-worded in place to `msg`'s
    /// words — no new entry, one more log line
    /// ([`aterm_messages::MessageCenter::reword_record`]); `false` when `id`
    /// is no record or already says them. As a record's post, no settle and
    /// no repaint: nothing on the glass moved, and the park's sweep persists
    /// and publishes it.
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

    /// A live row that becomes its own RECORD in place, in `msg`'s words
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

    /// The reporter withdrew a live row with NO outcome to claim — the
    /// tailer's file vanished under a live meter before any marker said how
    /// the pass ended. The log keeps the row (`Retired::Withdrawn`);
    /// `appstatus` lists no finished activity for it, as the bars listed
    /// none (status_bars.rs:1176-1181 dropped the bar silently); the marker,
    /// when it comes, posts the outcome under the same key.
    pub(crate) fn withdraw_message(&mut self, id: MessageId) -> bool {
        let withdrawn = self.messages.withdraw(id, Instant::now());
        if withdrawn {
            self.sync_messages();
        }
        withdrawn
    }

    /// The person's word on a live row was TAKEN (ruling 270): it retires
    /// `Answered { label }` — the record says `answered: <label>`, never
    /// `took` — for a capsule whose word is written off this thread and so
    /// could not close the row at the press. `false` when `id` is not live.
    pub(crate) fn answer_message(&mut self, id: MessageId, label: &str) -> bool {
        let answered = self.messages.answer(id, label, Instant::now());
        if answered {
            self.sync_messages();
        }
        answered
    }

    /// After any message input, on the timer-wake sweep, at the park and
    /// when a handoff finishes: retire what has expired (holds suspended
    /// under a freeze), commit the row count, append what the log drained
    /// (never under a freeze), and repaint every window — the band is chrome
    /// in each of them; the RepaintKey's band fingerprint keeps a
    /// byte-identical tick from re-presenting.
    pub(crate) fn sync_messages(&mut self) {
        let now = Instant::now();
        // A host verb (a post, a retire, a restate) moved the glass BEFORE
        // this settle, whose `glass_changed` compares the glass across the
        // settle alone: the lit chip is resolved again whatever it reports
        // (round 44's review, ruling 418 (a)).
        if !self.settle_messages(now) {
            self.refresh_band_hover();
        }
        // Settings ▸ Messages, when a view holds it, follows within the 2 Hz
        // gate (design §4.2); what the gate holds back the park publishes.
        self.publish_native_messages_state_if_due(now);
        self.request_redraw_all_windows();
    }

    /// [`Self::sync_messages`] without the repaint request, for the loop's
    /// own sweeps — the timer wake and the park — which collect what to
    /// repaint themselves: retire what has expired (holds suspended under a
    /// freeze), commit the row count, drain the log. `true` when the GLASS
    /// changed — a row folded, arrived or re-worded, or the count moved — so
    /// the caller repaints exactly then (the bars' `settle || sync` answer).
    ///
    /// The step is the engine's (`aterm_messages::drive`, design ruling 336):
    /// this host gives it the instant, the handoff freeze, what its windows
    /// afford and the rows its geometry reserves, re-grids between the two
    /// halves, and hands the drained lines to its writer.
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
        // A `notice progress` restate the wire's pacing held back (ruling 170):
        // its one paced paint falls due here, however many lines arrived.
        let persist = !self.message_persist_frozen();
        let (lines, wire) =
            drive::drain_and_pace(&mut self.messages, &mut self.wire_gate, now, persist);
        self.append_message_lines(&lines);
        let changed = step.glass_changed || committed || wire || strip;
        if changed {
            // The chips moved under a still pointer (round 44).
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

    /// THE GRACEFUL EXIT'S LAST WORD (design ruling 267): every row still
    /// open — live work, a held warning, one still queued — is recorded as
    /// cut off by aterm quitting (`Retired::Quit`, with the percent a
    /// determinate row had reached), and the log is flushed, because the
    /// exit seam `forget`s the App and its writer's drop-time join never
    /// runs. A successor still frozen before its handoff commit writes
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

    /// Whether the persist drain waits right now: only in the SUCCESSOR of a
    /// seamless handoff before Commit, whose lines are about rows the parent
    /// still owns — a refused successor must have written nothing. The
    /// parent's drain never waits (module doc).
    pub(crate) fn message_persist_frozen(&self) -> bool {
        self.incoming_handoff_pending
    }

    /// Commit the band's row count to the window geometry. `true` when the
    /// count moved (and every window was re-gridded). The committed count
    /// (`message_band_rows`) is what the chrome-row law reads, so a row is
    /// reserved and released in the SAME step as the PTY resize — the compose
    /// can never prepend a row the grid still owns, or vice versa.
    ///
    /// NEVER MID-HANDOFF. A seamless self-update parks the readers and hands
    /// the successor an exact window layout; a re-grid here moves `ws.rows`
    /// outside `window_event`, so at Commit the layout no longer matches and
    /// the handoff is refused as a topology change — after the user sat
    /// through the frozen echo. The band keeps its row count until the
    /// handoff finishes (`Wake::UpdateHandoffFinished` re-syncs); the rows
    /// themselves keep updating, painted within the committed count. BOTH
    /// SIDES OF THAT SEAM: the outgoing process holds `pending_update_handoff`;
    /// the INCOMING one holds `incoming_handoff_pending` instead, and its
    /// layout is the one the proof compares against what the parent captured.
    ///
    /// AND NOT BEFORE IT EITHER — by design, and recorded here so nobody
    /// "fixes" it by deferring the row. The "update staged" row is raised
    /// when a build is ARMED, before any attempt exists, so its one row
    /// lands on every session seconds before the park: on 2026-09-22 it
    /// shrank 56-row sessions to 55, and a Claude Code tab whose DECSC slot
    /// sat on row 55 refused the visible-checkpoint set on every release
    /// from v0.87.0 to v0.90.0. The fix was the wire's bound on that slot
    /// (`seamless::checkpoint_meta_bound_violation`), not this shrink: a
    /// resize a second before a handoff is ordinary desk state the
    /// checkpoint must carry, and a 55-row refusal in the log under a
    /// 56-row window is this shrink, not a mystery.
    ///
    /// …AND NEVER MORE ROWS THAN THE GLASS CAN SPARE. `grid_dims_for` floors
    /// the terminal at one row, so chrome that outgrows the window does not
    /// shrink the grid — it makes the COMPOSED frame taller than the window,
    /// and the surplus is simply clipped. The band yields instead: it is
    /// chrome about background work, and background work never outranks the
    /// one row of terminal the user is actually looking at. Measured against
    /// what each window CURRENTLY affords — its terminal rows plus whatever
    /// band rows are already committed — so the answer does not depend on the
    /// re-grid it is about to trigger; the smallest window decides, because
    /// the count is global. The clamp is passed INTO the center
    /// (`commit_rows`), whose D1 hysteresis grows at once and shrinks after
    /// its quiet — measured against `now`, the instant the caller's settle
    /// ran at, so the two agree on the clock — and which anchors a row's hold
    /// only when the row is within the committed count — a clamped third row
    /// never burns its hold unpainted.
    ///
    /// THE TWO COUNTS CONVERGE HERE. The geometry's count (`message_band_rows`)
    /// and the center's (`committed_rows`) agree on every step this method
    /// takes, and disagree in exactly one state: the successor of a seamless
    /// handoff, which reserved the rows its parent had COMMITTED (the layout
    /// the proof compares) while the center it re-seeded counts the rows the
    /// parent CARRIED — a row the parent's frozen count had queued makes the
    /// center's count one higher (`seed_carried`). Once the freeze lifts the
    /// geometry takes the center's count whether or not `commit_rows` moved
    /// it this step, so a carried row is never presented into a row the
    /// window does not own.
    ///
    /// The commit and the convergence are the engine's (`drive::commit`); the
    /// window set it measures and the re-grid are this host's.
    pub(crate) fn sync_message_band_rows(&mut self, now: Instant) -> bool {
        let commit = self.band_commit(now);
        let regrid = drive::commit(&mut self.messages, commit);
        self.regrid_message_band(regrid, now)
    }

    /// What this host's step commits against: the instant, the handoff
    /// freeze, the rows every window WITH GLASS affords (a headless window
    /// has none to spare, so it never clamps; "glass" is
    /// `app_presence::chrome_regrids_grid`, which is `os_window.is_some()`
    /// outside the tests; inside a chrome re-grid batch, the rows its pending
    /// re-grid gives — `App::chrome_commit_grid_rows`), the rows its geometry
    /// reserves, and the windows' chrome regrow floor — the latest of every
    /// window whose grid the band's re-grid resizes (`App::band_regrow_floor`,
    /// ruling 414).
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

    /// Take the row count the engine committed into the window geometry and
    /// re-grid every window. `true` when there was one to take. A RISE
    /// (`aterm_messages::chrome::is_rise`) is noted on every window's regrow
    /// floor at the step's `now`, before the re-grid, so no other chrome
    /// source's fall re-grids a window back inside the settle (ruling 414,
    /// proposed).
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

    /// Whether a handoff is freezing the holds, the re-grid and the persist
    /// drain right now. ONE predicate, read by the settle and by the deadline
    /// that arms it, so the loop can never arm a wake its own settle will
    /// decline to act on.
    pub(crate) fn message_holds_frozen(&self) -> bool {
        self.update_handoff_parked() || self.incoming_handoff_pending
    }

    /// The next instant the band needs the loop back — what
    /// [`Self::sync_messages`] would act on, freeze included — or the
    /// instant the next Settings ▸ Messages publish falls due
    /// (`messages_publish_due_at`: a publish the 2 Hz gap held back, or the
    /// minute tick while a view is on the route; performed by
    /// `publish_native_messages_state_if_due` at the park that wake ends
    /// in). `None` with nothing armed: an idle band never wakes the loop
    /// (FL-1).
    pub(crate) fn messages_deadline(&self) -> Option<Instant> {
        // The engine's state deadline (holds, echoes, patience, the wire's
        // one paced paint, ruling 170) and this host's own publish; `None`
        // when idle (FL-1).
        let state =
            drive::state_deadline(&self.messages, &self.wire_gate, self.message_holds_frozen());
        // ... and the instant the digest next has something to end on its own
        // (an entry no reporter named for ten minutes); `None` when idle.
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

    /// Post what the pre-App inbox holds (every park; one atomic load when
    /// empty), each with the stamp it was queued under — and the folders
    /// fresh shells could not start in ([`Self::fold_folder_faults`]).
    pub(crate) fn drain_message_inbox(&mut self) {
        let queued = crate::message_inbox::take_queued();
        let folders = crate::spawn_folder::take_noted();
        if queued.is_empty() && folders.is_empty() {
            return;
        }
        for m in queued {
            match m.restored {
                Some(row) => self.post_restored_row(row, m.stamp),
                None => {
                    self.post_unsynced(m.message, m.stamp);
                }
            }
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

    /// Perform one plain-data intent the engine returned for row `id` in
    /// window `wid` — a capsule press, a body press, a screen reader's
    /// activate, a button on Settings ▸ Messages. The row's own lifecycle is
    /// the engine's (`act` retired it already if the intent closes it); this
    /// only does the thing. `true` when it was done, `false` when it was
    /// refused (a route this build has no page for, a path that is not a
    /// crash log, a system pane that did not open) — the page words its
    /// feedback from it; the band's press paths have nothing to say back.
    pub(crate) fn perform_intent(&mut self, wid: WindowId, id: MessageId, intent: Intent) -> bool {
        let now = Instant::now();
        // A NAVIGATION press (a Settings page, `aterm.toml`, the crash log) that
        // took the person there READ the row (design §10.5 H6): a held or
        // standing row folds once they have gone to fix it; live and ask rows
        // stay, and a record re-offered from the page is a no-op.
        let navigation = matches!(
            intent,
            Intent::OpenSettings { .. } | Intent::OpenConfigEditor { .. } | Intent::OpenPath { .. }
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
            // THE DETAILS PRESS (design §2.2, §3.6): the row is read — the
            // engine marks it seen, which folds a held row now — and Settings
            // ▸ Messages opens at this entry in the pressed window. Seen
            // FIRST: the open publishes the page's projection, and the entry
            // it selects should already read as folded, not as a row about
            // to leave (a second publish inside the 2 Hz gate would wait).
            Intent::Details => {
                if self.messages.mark_seen(id, now) {
                    self.sync_messages();
                }
                // The file-access question READ: never re-offered in this
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
            // The config reporters (`message_reporters`, R1–R4, R6, R12, R13)
            // carry no line — the editor host has no line target today
            // (`ConfigEditorTarget` is a key or a search) — so the editor opens
            // at the top, through the existing `AppEffect::OpenConfigEditor`
            // body.
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
            // `Stop paste` (ruling 231): the rest of the paste is dropped, the
            // input after it goes through, and the row fades.
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

    /// SETTINGS ▸ MESSAGES, AT AN ENTRY, IN THIS WINDOW (design §4.5, D5):
    /// install or reuse `wid`'s own Settings view on the Messages route —
    /// `open_settings_tab_in_window`, so a press in a background window opens
    /// the page there and raises it — then, with an id, select that entry
    /// through the page's own `settings/messages/select` action (the
    /// `settings/route` host path `open_settings_tab` itself uses): the
    /// reducer expands it, clears a chip that would hide it and pages to
    /// it. The overflow row and its a11y link pass `None`: the page, top of
    /// the log, nothing expanded. `false` when the window has no Settings
    /// to show — a window with no tabs to install into.
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

    /// THE PAGE'S OWN PRESS on an entry's button (design §4.4,
    /// `AppEffect::MessageAct`): the reducer named the entry and the button
    /// by IDENTITY — an id and an index, never a path or a route — and the
    /// host resolves both here. A LIVE row goes through the engine's `act`
    /// (the press is logged, a `Not now` retires its row) exactly as a band
    /// capsule does; a RETIRED record re-offers the intent the log kept
    /// (design §1.3: every intent is codec-able for exactly this), except a
    /// decision, which only a live row can answer, and anything else the
    /// record no longer offers ([`aterm_messages::LogRecord::still_offers`],
    /// the page's and the wire's one rule: an intent that acted on the live
    /// row, or the word it was answered with — a stale page press performed
    /// `Show tab` or `Stop paste` for a row already gone, round 44). Then the
    /// intent is performed in `wid` and the outcome worded for the page's
    /// feedback.
    pub(crate) fn perform_message_act(
        &mut self,
        wid: WindowId,
        id: u64,
        action: u8,
    ) -> MessageActOutcome {
        let Some(id) = MessageId::from_raw(id) else {
            return MessageActOutcome::Gone;
        };
        // THE PAGE'S OWN CAPSULES on an open exception (view-time: no intent, nothing
        // stored): `Dismiss` files this entry, `Dismiss all` the whole digest, each
        // flushed to the log before the press returns.
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
            // entry's own action files it ANSWERED (the owner did what it
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

    /// `notice …` on the main thread (design §5.2, rulings 163-198): the
    /// engine decides — verdict, caps, mint budget, pacing, reply words — and
    /// the host paints. `act` is the one form the host performs: the engine
    /// names the capsule and spends its press budget (`wire::press_target`),
    /// then the page's own press ([`Self::perform_message_act`]) runs in the
    /// front window (window 0 on a headless instance), exactly what a click on
    /// the capsule does. Returns the whole reply line without its `\n`.
    ///
    /// NOT WHILE PARKED (round-seven update audit, finding 32): the band
    /// crossed to the successor in the carry the park captured, and this
    /// process `_exit`s at Commit — a row posted now would never be shown (and
    /// a script is told its id for the row it asked for), and a dismiss, a
    /// re-word or a press would be undone there. The script is told to retry
    /// ([`wire::switching_reply`]), and its retry reaches the new version (or
    /// this one, if the switch stood down).
    pub(crate) fn take_notice(&mut self, req: wire::NoticeRequest) -> String {
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
        // A script's `notice dismiss review|<id>` filed an exception through
        // the engine alone: flush the log before the reply (a dismissal must
        // not die with the process) and move the menu-bar item and Settings.
        if self.messages.digest_revision() != digest_before {
            self.flush_messages_log_within(MESSAGES_FLUSH_UI_WAIT);
            self.digest_moved();
        }
        if applied.paint == wire::Paint::Now {
            self.sync_messages();
        }
        if applied.note {
            aterm_log::info!("notice: {}", applied.reply);
        }
        applied.reply
    }

    /// THE PROJECTION, built once from the ring (design §4.2) by the engine
    /// ([`MessagesState::of`], design ruling 382) over this App's facts
    /// ([`PageDesk`]): every record newest first, the live rows in their
    /// current words, the band row each is on, and whether each authored
    /// intent can still be pressed — the staged build read once for every
    /// `Install now`, the crash log's file stat'd per `Open log` (a few
    /// records at most; the publish runs at 2 Hz at the most). Tag counts in
    /// the chips' order.
    pub(crate) fn messages_state(&self) -> MessagesState {
        MessagesState::of(&self.messages, &PageDesk::of(self))
    }
}

// ---------------------------------------------------------------------------
// The body press.
// ---------------------------------------------------------------------------

impl App {
    /// THE BODY PRESS — one law everywhere (design §2.2, interim answer 2):
    /// a press on any part of a row that is not a capsule is the row's
    /// `Details ›` — Settings ▸ Messages opens at that entry in the pressed
    /// window and the row is marked seen (a held row folds; the config
    /// band's "click at a notice you have read", now recorded and with a
    /// destination). A decision row is answered only by its capsules. The
    /// mouse (`press_band`) and a screen reader's activate on the row share
    /// this one entry, so the two can never disagree about what a press on
    /// the body does. Phase 1's law — the body performed the first authored
    /// intent while the page a Details press opens did not exist — is gone
    /// with the page's arrival.
    pub(crate) fn press_message_body(&mut self, wid: WindowId, id: MessageId) {
        if self.messages.live(id).is_none() {
            return;
        }
        let _ = self.perform_intent(wid, id, Intent::Details);
    }
}

// ---------------------------------------------------------------------------
// The toolchain reporters' glue (R25–R34).
// ---------------------------------------------------------------------------

impl App {
    /// The dialect of the atpkg hook the frozen-tab note tells a person to
    /// source ([`HookDialect`]): the shell this window spawns, read from the
    /// spawn configuration (`--shell` / config `shell` / `$SHELL`) the way
    /// the spawn seam reads it, so a config `shell = "fish"` gets fish's
    /// `source`. The bars pinned it once at construction; read at post time
    /// it can never go stale against a reloaded config.
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
        // The record re-worded must still be one: past the ring's cap, or
        // retired otherwise, the counts it would carry are a new record's —
        // never silence until the versions move (review of 2026-10-09).
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

    /// A HARNESS NOTE (`aterm ctl appnotice harness <text>`, an agent
    /// harness's acts): a `harness`-tagged record, never a row
    /// ([`crate::message_reporters::harness_note`], design ruling 61) — on
    /// `appstatus` as `kind=harness`, in `messages.log` and on Settings ▸
    /// Messages. A note repeated word for word is not written again. `true`
    /// when a record was written.
    pub(crate) fn record_harness_note(&mut self, text: &str) -> bool {
        let msg = crate::message_reporters::harness_note(text);
        if self.harness_note_recorded.as_deref() == Some(msg.title.as_str()) {
            return false;
        }
        self.harness_note_recorded = Some(msg.title.clone());
        self.record_message(msg);
        true
    }

    /// R33 — `machine-settings:`: one record per item, in wire order
    /// ([`toolchain_words::machine_settings`]), never a row.
    pub(crate) fn post_machine_settings(&mut self, text: &str) {
        for msg in toolchain_words::machine_settings(text) {
            self.record_message(msg);
        }
    }

    /// `foreign:` — what one of a pass's looks found that aterm LEAVES ALONE, with
    /// atpkg's fix ([`toolchain_words::foreign_report`]; audit 2026-10-09: a foreign
    /// rustup `trust` link and a managed tool shadowed on PATH were prose 30-odd passes
    /// printed, logged and shown to no one). Each look — the rustup seam, the PATH
    /// shadow reconcile — reports every time it runs, so each has ONE exceptions entry
    /// of its own ([`toolchain_words::foreign_tools_entry`]): raised, or re-worded in
    /// place, while its report names something, and ended by its first report that
    /// names nothing. A body this build cannot read changes nothing.
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

    /// R25 — a pass opens (`net-starting:`). Only a FIRST
    /// RUN opens the row (`first_run`, the lane's verdict for the child that
    /// printed it: `crate::pass_is_first_run`); a routine pass's announcement
    /// is the arm's log line alone (the silent lane, upstream dbf97ecff). The
    /// first-run child LATCHES the row from here until its exit
    /// ([`Self::toolchain_pass_ended`]).
    pub(crate) fn announce_toolchain_pass(&mut self, detail: &str, first_run: bool) {
        if !first_run {
            // A person's verb (ruling 224): its busy row takes the size the
            // announcement names — how big the wait is — until the plan lands.
            // While it queues, an announcement is the holder's (the lane's
            // own child), not the size of the person's wait (ruling 229).
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

    /// R26 — one classified `progress.json` read from the child-scoped
    /// tailer (`Wake::PkgProgress`; `None` = the file vanished at child
    /// exit), applied against the row that carries the pass
    /// ([`toolchain_words::KEY_PASS`]). `first_run` is the lane's verdict for
    /// the child the tailer is scoped to:
    ///
    /// * a FIRST RUN paints, and so does a VERY HEAVY routine pass — one that
    ///   planned at least [`toolchain_words::HEAVY_PASS_BYTES`] (design §10.5
    ///   H8, ruling 84): `Updating ALab tools`, posted after the
    ///   progress grace so a pass that ends quickly never touches the glass.
    ///   Either LATCHES the row: one open, one fold, at the child's exit. A
    ///   read restates the live row in place (a restate costs no log line, so
    ///   the 10 Hz tailer is free) or posts it when none is up, and records the
    ///   meter's high-water mark for the next read's clamp. A light routine
    ///   pass's reads paint nothing;
    /// * a read of a pass that is OVER, and a vanished file, claim NO outcome
    ///   (the markers' records do): an UNLATCHED live row is withdrawn at once
    ///   (its fade); a latched one is the child's exit's to fold;
    /// * a plan of nothing leaves the glass alone (an announced row keeps its
    ///   announcement until the plan lands);
    /// * a HELD row on the key — a carried one, from a build before the
    ///   silent lane — is left alone. A first run's FAILURE row
    ///   ([`toolchain_words::first_run_short`], held too) is an outcome, not
    ///   work: a read of a pass that paints replaces it by key — a retry from
    ///   Settings ▸ Packages shows its progress at once, never behind the
    ///   failure it is answering — and an over or vanished read leaves it.
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
        // WHO WAITS DECIDES (rulings 220, 224; the rule is the engine's): a
        // first run and a person's Settings verb are waited on and paint
        // whatever they weigh; a routine pass paints only while very heavy.
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
                // The HOLDER the person queued behind is done: its last frame
                // leaves, and the row says again that they wait (ruling 229).
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

    /// Fold the LIVE toolchain row — the meter or its announcement — with no
    /// words of its own: the pass it tracked is over, and what it came to is
    /// a marker's record. WITHDRAWN, so `appstatus` lists no finished activity
    /// for it (the bars dropped the bar without a ledger row, ruling 13); the
    /// glass ends its indicator with `echo` (the child's exit names Complete
    /// or Fault; a vanished read, a fade). A held row (a carried one) is left
    /// standing.
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

    /// A marker ANSWERED a pass: the outcome is `msg`'s RECORD
    /// ([`toolchain_words::installed`] / `ended` / `failed` — each
    /// [`Hold::LogOnly`]), and the live row folds — unless it is LATCHED (a
    /// first run, or a heavy routine pass), whose answer may be one
    /// sub-pass's (the vendor lane's, before the ALab set's; upstream
    /// ac7942234): its child's exit folds it.
    pub(crate) fn record_toolchain_outcome(&mut self, msg: Message) {
        if !self.toolchain_row_latched && self.toolchain_person.is_none() {
            self.retire_live_toolchain(None);
        }
        self.record_message(msg);
    }

    /// A marker said the latched FIRST RUN is ending short (`seed-failed:`,
    /// `net-failed:`, `seed-partial:`, `seed-nothing:`, a `seed-unusable:`
    /// that is not a fact about the machine): its failure row is held for the
    /// child's exit ([`Self::toolchain_pass_ended`]). Not a first run — a
    /// heavy routine pass, or no latched row at all — keeps only the record:
    /// the versions already installed keep working.
    pub(crate) fn note_toolchain_first_run_short(
        &mut self,
        how: toolchain_words::FirstRunShort,
        cause: &str,
    ) {
        if self.toolchain_row_latched && self.toolchain_first_run {
            self.toolchain_first_run_short = Some(toolchain_words::first_run_short(how, cause));
        }
    }

    /// The lane's child EXITED (`Wake::PkgPassEnded`): whatever it held on the
    /// glass folds now — ONE open and ONE fold per latched child, however
    /// many sub-passes it ran — and its indicator ends by how the child
    /// exited: Complete when `clean`, Fault when not. The record stays
    /// `Withdrawn` (ruling 13: `appstatus` claims no outcome for it). A FIRST
    /// RUN that exits unclean after a marker said it ended short leaves its
    /// failure row behind the Fault echo — the deliverable the row told the
    /// person to wait for did not arrive, and they have something to do
    /// (review 2026-09-24).
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
            // A person's verb is waiting (queued behind this child, or
            // running beside its exit): the row is theirs. A latched lane row
            // ends as the lane's work did, and the person's own row comes back
            // after the grace if their verb is still running (ruling 224).
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

    /// A PERSON'S SETTINGS ▸ PACKAGES VERB (`Wake::PkgPersonPass`, design
    /// ruling 224). They pressed it and wait for it, so its pass takes the
    /// band's row whatever it weighs — the 250 MB bar is for work nobody
    /// asked for. `Began` puts up the verb's busy row, revealed after the
    /// progress grace (a 1–4 s no-op Check never reaches the glass);
    /// `Waiting` says it is queued behind another pass, `Running` that it took
    /// the lock; the tailer's reads restate it with the fill and the ETA once
    /// the plan lands (`apply_toolchain_snapshot`); `Ended` folds it with the
    /// echo the worker names — Complete with the verb's finished words, Fault,
    /// or a Vanish for a wait that ran out. The outcome stays the markers'
    /// records (`ALab tools checked`, …) and the page's headline.
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
                // Back to the verb's own busy row, whatever the holder's
                // reads left on it — its fill, its ETA, its finished words
                // (ruling 229). A latched lane row is the lane's until its
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

    /// Put `row` (a person's busy verb or waiting row) on the lane's key: a
    /// row that is not up yet is posted (its grace from now; it supersedes a
    /// first run's failure row, as a retry's reads do); a BUSY row already up
    /// is restated in place — its reveal kept — when `restate`; a row with a
    /// fill (a plan's meter) or a held one is left alone.
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

    /// Put the person's `row` back over whatever the HOLDER'S reads left on
    /// the lane's key — its fill included, which [`Self::show_person_row`]
    /// keeps (ruling 229). The holder's high-water mark goes with it. A held
    /// row is left alone; a failure row, or none, is posted over.
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

    /// Whether a painting read is the HOLDER'S and must not paint the
    /// person's row (ruling 229):
    ///
    /// * a read tailed while the person queues is the holder's — noted by the
    ///   pid that wrote it, and painted, as what they wait behind;
    /// * once the lock is taken, a read of that pid is late (another thread's
    ///   tailer can land it after `lock-acquired:`) and paints nothing;
    /// * a LATCHED FIRST RUN is painted by the lane's own tailer: the
    ///   person's tailer follows the same file (`first_run` false), and its
    ///   reads would restate the first run's row in the person's words
    ///   (`Updating`, not `Installing`), turn and turn about with the lane's.
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
/// (`Wake::PkgPersonPass`, design ruling 224; `App::person_pass`).
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
// The update reporters' glue (R35–R38).
// ---------------------------------------------------------------------------

/// THE UPDATE'S FLOW ROW, as state (`App::update_flow`): the live
/// `update.progress` row that is the lane's, the version it names and the
/// phase it is in. What a row IS is state, never its glyph or title (ruling
/// 57). A reader re-validates `id` against the center: a row that folded,
/// went stale or was superseded by a wire post is not the flow.
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
    /// The lane's flow state while its row is still live — never a stale
    /// memory of one that folded or was superseded.
    pub(crate) fn live_update_flow(&self) -> Option<&UpdateFlow> {
        self.update_flow
            .as_ref()
            .filter(|flow| self.messages.live(flow.id).is_some())
    }

    /// Post `msg` as the lane's flow row (it carries [`update_words::KEY_PROGRESS`])
    /// in `phase`, and remember it as the flow. A HELD row the lane replaces — a
    /// ready row — is resolved first, so
    /// `appstatus` keeps it as a finished activity (a live row replaced is only
    /// superseded, and records nothing); the design's own §5.3 edge ("an
    /// announcement over a HELD terminal row first resolves that row"). One sync.
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

    /// A live row under `key` that is HELD (a `Default` / `For` hold): resolved
    /// now by its tone, no sync — the post that replaces it syncs once.
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
    /// THE LATCH IS HERE, AT THE ONE DOOR (the review of the update audit's
    /// merge onto this surface). Three producers now announce "Couldn't
    /// install updates": the updater's persistent streak, its overdue notice
    /// (a newer build waiting over an hour) and the automatic lane's
    /// convergence (`App::announce_automatic_apply_stranded`), each with its
    /// own latch — so one stuck build could raise the ⚠ row and the OS banner
    /// two or three times in one launch. A failing title this process has
    /// already said, and that no proof has answered since
    /// (`update_health_latched`), is a log line and `false`: no row, no
    /// banner. A healing that answers the title re-opens it, so a new episode
    /// speaks again.
    ///
    /// KEYED BY KIND, NOT BY WORDS (design ruling 311): the failing title is
    /// read back to the half it names once, here
    /// ([`update_words::HealthKind::of_updater_title`]), and the latch, the row's
    /// key, the heal proof and the record of the healing all carry that kind —
    /// so the words can take the family grammar and a warning an older build
    /// raised in its own words still heals.
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
        // AN ATTEMPT STILL IN FLIGHT KEEPS ITS ROW AND ITS RIM (round six,
        // finding 44): a warning that lands while the successor boots or holds
        // for its park is not that attempt's refusal — the install goes on to
        // Commit, and the rim is the only explanation of the frozen frame the
        // park is about to show. The attempt's own completion retires them if
        // it does not take over (`reduce_returned_handoff_completion`).
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

    /// A health warning of `kind` ALREADY SAID this launch whose remedy no
    /// longer holds (the automatic lane's stranded warning, its retry
    /// withdrawn): the live row takes the new words in place — no new row, no
    /// banner, its hold as it was — and one already folded off the glass is
    /// followed by a record in them, so Messages never keeps a promise the
    /// lane withdrew. The healing is recorded against the new words.
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

    /// THE ONE DOOR OF THE OS BANNER an update-health ANNOUNCEMENT owes (the
    /// caller has just had [`Self::note_update_health`] or
    /// [`Self::note_update_health_as`] return `true`), and it opens only when no
    /// lane can install until the owner acts
    /// ([`crate::update_apply_trouble::ApplyRetry::NeedsPerson`]): the installed
    /// copy is not the signed release, or its folder cannot be written. The
    /// banner names that remedy. Every other
    /// failure is a record now and, after a day, a line of the digest
    /// ([`update_words::update_health_entry`]): a banner that says "updates
    /// failed" when the owner can do nothing about it that minute is the
    /// non-actionable alert the owner ruled out (2026-09-28), and the banner of a
    /// failure the next check may end is the noise this door used to make.
    ///
    /// Off the UI thread — `notify::deliver` blocks on a notifier subprocess —
    /// and in the banner's words ([`update_words::health_notification_body`]). No
    /// focus suppression: updater health belongs to no tab. Both producers of an
    /// install warning (`Wake::UpdateHealth`, the automatic lane's convergence)
    /// call it, so ONE check holds `desktop_alerts`: off (the default) posts
    /// nothing, and the record, Settings ▸ Software Update and `messages.log`
    /// still say it. `true` when a banner was posted. Under `cfg(test)` the
    /// banner is recorded ([`UPDATE_HEALTH_BANNERS`]) instead, so no test ever
    /// posts to the desktop of whoever runs the suite. It is the one caller of
    /// `notify::deliver_update_health_banner` (only this module mints its
    /// `HealthBannerVoice`); `notify::deliver` is private.
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
        // No desktop banner for update health here (it never had one off macOS).
        #[cfg(all(not(target_os = "macos"), not(test)))]
        {
            let _ = (title, body, remedy);
            false
        }
    }

    /// THE LEDGER HEALED as far as `proof` can tell: the health warning
    /// ([`update_words::KEY_HEALTH`]), if it is still up, is over — resolved
    /// with the outcome it stood for (a warning) — and, once per announced
    /// episode (after the warning's 45 s fold too), the healing is ON RECORD
    /// against what the warning said ([`update_words::health_recovered`]).
    ///
    /// ONLY WHERE THE PROOF ANSWERS THE WARNING ([`HealthProof`]). The kind
    /// names the broken half, so a download cannot heal "Couldn't install
    /// updates": the record would read "aterm updates work again" while the
    /// install streak the updater's ledger still holds says otherwise — the
    /// ledger itself clears every streak on a whole-pipeline success but an
    /// install streak, which only an install clears. A warning the proof does
    /// not answer stays up (and remembered), for the proof that does.
    pub(crate) fn heal_update_health(&mut self, proof: HealthProof) {
        self.heal_update_health_where(|kind| proof.answers(kind));
    }

    /// [`Self::heal_update_health`] over the kinds `answered` picks: their
    /// latch, their rows, their day's entry and the record of their healing
    /// go, the rest stay. The ONE heal of an update-health warning: the
    /// ledger's recovery, this process's own proof and the checker watchdog's
    /// stall and lock-held heals all come through here.
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

    /// What a health warning of this kind said, kept for its record of healing
    /// — ONE ENTRY PER KIND (design ruling 318): the same kind said again
    /// replaces its own entry, and another kind's stays, so each healing is
    /// recorded against the warning it answers.
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

    /// The healing of every remembered warning whose kind `healed` picks goes
    /// ON RECORD — one record per warning, each against its own words, oldest
    /// first — and leaves the memory; the others stay for the proof that
    /// answers them (design ruling 318).
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

    /// R36 — THE NEW BUILD TOOK OVER (or a cold-lane boot found it already
    /// running): the lane's live flow row — the carried Finishing row, or an
    /// Installing row — is resolved `Ok` (the work was delivered: its Complete
    /// echo is the landing's moment, ruling 141), and the good
    /// news is a RECORD in main's words ([`update_words::landed`]: "Updated to
    /// aterm vX") — and, when this process (or the predecessor that handed
    /// over) downloaded THIS build, how long the update took, a line of that
    /// same record. Never for a build met already on disk, nor for another
    /// build's download.
    ///
    /// `repainted` is how many tabs the seamless successor adopted onto a
    /// blank screen (the 2026-09-22/23 update audit, plan P1-5 —
    /// [`Self::seamless_repainted_tabs`]; 0 on the cold lane, which carried no
    /// screen): the landing record names them and says why they came over
    /// blank. A record only, like the landing itself — one fact, one entry.
    pub(crate) fn post_update_landed(&mut self, version: &str, build: u64, repainted: usize) {
        self.heal_update_health(HealthProof::Landed);
        let took = self
            .update_verified
            .take()
            .filter(|(verified, _)| *verified == build)
            .map(|(_, at)| Instant::now().saturating_duration_since(at));
        // The flow row by STATE (ruling 57), or — a carry from a build before
        // the flow state — the live row on the lane's key.
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
        // THE APPLY LANE'S LAST OUTCOME is answered by the landing (round-seven
        // update audit, finding 30): "Update waits for you" (transient, or the
        // standing row of round eight's U38) or "Couldn't install aterm vX ·
        // Install now" — raised before the attempt that landed, carried across
        // it on glass — would otherwise take a fresh hold beside "Updated to
        // aterm vX", its press opening Settings for a build already running.
        // Withdrawn, not resolved: no ✓ stands on a failure's words, and the
        // record keeps what it said.
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

    /// How many tabs this seamless successor adopted onto a blank screen (the
    /// 2026-09-22/23 update audit, plan P1-5): `App::handoff_repainted_tabs`,
    /// the count the boot took from the adopted set before the restore drained
    /// it — what [`Self::post_update_landed`] names on the seamless lane.
    pub(crate) fn seamless_repainted_tabs(&self) -> usize {
        self.handoff_repainted_tabs
    }

    /// THE STAGED DECISION, raised once (design §10.5 H2, ruling 119): when
    /// `build` is the staged build, its posture is one where a press installs
    /// it, and no staged row is up, the `aterm vX is ready` decision row is
    /// posted as the lane's flow row — the lane that stopped hands the person
    /// the one thing they can do. ONCE per build and posture: every reconcile
    /// re-states the posture, and after the person let the question lapse (its
    /// hold ran out, or they read it) the same question on the next check was a
    /// nag (review 2026-09-24) — the Version menu and Software Update still
    /// offer it. A posture that has just become a decision (a lane that
    /// stopped) raises it again.
    ///
    /// `posture` is the caller's `apply_posture_for(build)` — or a test's,
    /// since a headless App's own posture is always the handoff-off record.
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

    /// The id of the lane's flow row while it is the STAGED one — the state in
    /// which the build is ready (and, where a press installs, a press may).
    pub(crate) fn staged_update_row(&self) -> Option<MessageId> {
        self.live_update_flow()
            .filter(|flow| matches!(flow.phase, FlowPhase::Staged { .. }))
            .map(|flow| flow.id)
    }

    /// The staged row is obsolete — its bytes are installed and activating
    /// (`delivered`: it resolves `Ok`, which is true), or the artifact it offered is gone
    /// (withdrawn with no outcome: nothing was installed, so no ✓ may stand
    /// beside `Couldn't finish the update`) — so it leaves now rather than standing
    /// beside the outcome that says so.
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

    /// THE SWITCH TO A NEW VERSION BEGINS, on record ([`update_words::switch_started`])
    /// — and flushed to `messages.log` NOW: a line queued for after the process
    /// execs, or inside the park's few milliseconds, is a line never written (the
    /// seamless parent flushes again, bounded, right before Commit `_exit`s it:
    /// [`Self::flush_messages_log_within`]). `build` is the
    /// RUNNING build; a same-image switch (`same_image`: a reload of this very
    /// build) installs nothing, whatever is staged. A switch that then stops says
    /// so ([`Self::record_update_switch_stopped`]).
    ///
    /// NOISE BOUND: every attempt a person makes is on record (a press held for
    /// the candidate's read and run again is one attempt); the automatic
    /// lane's are, only its FIRST for a build in this process — its ladder
    /// retries every few seconds while someone types, and the 512-record ring
    /// (`LOG_CAP`) is every reporter's. `aterm.log` keeps each attempt.
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

    /// [`Self::record_update_switch_started`] without its flush, for a caller
    /// that flushes once, bounded, right after — the cold lane's
    /// `prepare_cold_exec`, where every wait on the writer thread is time the
    /// person sits through a frozen window. `false` when nothing was put on
    /// record (the noise bound).
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
        // The automatic lane's later attempts at a build, and a person's press
        // held for the candidate's read and run again (round eight, U35's
        // review), whose start is still on record with no stop since: one fact,
        // one entry.
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
        // ONE FACT, ONE ENTRY: an Installing row this attempt POSTED (the
        // explicit lane with nothing up) is already that line in the log; a
        // row re-worded in place, or none (the automatic lane), is not
        // (audit 2026-09-24).
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

    /// THE SWITCH THAT DID NOT HAPPEN, on record ([`update_words::switch_stopped`]):
    /// an attempt [`Self::record_update_switch_started`] wrote down ended with
    /// this process still running — refused, failed, or stood down to try again
    /// by itself (`routine`, in `why`'s words) — so the record never leaves an
    /// "Installing" that was not. Nothing for an attempt that never reached the
    /// record.
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
/// occluded (minimized, fully covered). A headless window has no OS window,
/// so its band never moves. `WindowEvent::Occluded` is not reported on
/// Windows or Wayland (crates/aterm-window, the platform notes above `Occluded`),
/// and X11 reports it only from `VisibilityNotify` — a window fully covered,
/// not one iconified (unmapped) — so outside macOS a MINIMIZED window is
/// asked directly (review 2026-09-24: it kept its 1 Hz text ticks, and its
/// frames while focused or recorded). Wayland answers `None` (it cannot
/// say); macOS reports minimizing as occlusion.
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
    // macOS reports minimizing as occlusion, and asking AppKit on every loop
    // turn would buy nothing there.
    let minimized = |w: &aterm_window::window::Window| {
        cfg!(not(target_os = "macos")) && w.is_minimized() == Some(true)
    };
    shown && ws.os_window.as_deref().is_some_and(|w| !minimized(w))
}

impl App {
    /// How window `wid`'s band draws motion (design ruling 140, the gating
    /// UNION of the two sides): MOVING while its effect policy animates
    /// `BandProgress` (focused or recorded, no Reduce Motion, no
    /// `motion = "reduced"`, no load-shed latch), Serious Mode allows it, the
    /// band is on screen, and no handoff freezes the band
    /// ([`Self::message_holds_frozen`]: the screen is parked, and the rows are
    /// the committed ones — main's busy-frame rule); STILL otherwise. GRADED
    /// unless an OS High Contrast palette owns the chrome. The rule is the
    /// engine's (`drive::look`); the readings are this host's.
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

    /// The next instant any ON-SCREEN window's band needs a motion frame (or a
    /// time word's tick) — the loop's `DeadlineOwner::MessageBandMotion`, the
    /// band's ONLY clock (ruling 140; main's 125 ms busy frame retired into
    /// it). `None` with no committed row, or none on screen, or nothing live
    /// or echoing on the glass: an idle band arms nothing. `None` too while a
    /// handoff freezes the band: the frame on glass STAYS where it is — the
    /// screen is parked, and not even a move to the still form repaints it
    /// (main's freeze rule) — so the loop never arms a wake it would not act
    /// on. The fold is the engine's (`drive::motion_deadline_over`: each
    /// view's memoised next change from its prepared frame, never in the
    /// past); which windows are on screen, and each one's look, are this
    /// host's.
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

    /// The on-screen windows whose band is due a new motion frame at `now`:
    /// the deadline read at the frame each last presented has passed (or none
    /// was prepared yet while something moves), or the frame on glass was
    /// drawn in ANOTHER look — focus, Reduce Motion, Serious Mode, the
    /// load-shed latch or a recording changed it — so the move to (or from)
    /// the still form never depends on each of those triggers asking for its
    /// own redraw. Nothing while a handoff freezes the band (the frame stays,
    /// [`Self::band_motion_deadline`]). The rule is the engine's
    /// (`drive::due_views`).
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

    /// What this process's update-health warnings said that no proof has
    /// answered, in the shape a successor takes up
    /// ([`Self::take_up_carried_health_said`]) — the seamless lanes' window carry
    /// and the cold lane's `exec` (round eight, finding 79) alike.
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

    /// THE FILE READ AGAIN AT COMMIT (round-seven update audit, findings 41
    /// and 18): a late-park successor loaded `messages.log` at its boot, and
    /// its parent kept recording — posting, retiring, pressing, taking
    /// `appnotice` records — until Commit, flushing its last line first
    /// ([`Self::flush_messages_log_within`]). Merged here
    /// ([`aterm_messages::MessageCenter::merge_reloaded_log`]), so the history
    /// holds the parent's records since this process booted and each row it
    /// closed with its real ending, and every id minted from now on is above
    /// the file's.
    ///
    /// SYNCHRONOUS, AND FIRST: the Commit wake calls this before anything else
    /// it does can mint ("Updated to aterm vX" among them) and before a
    /// `messages since=` reader can reach this process's ring, so no record of
    /// the parent's appears later BELOW an id a reader has already been handed
    /// (the review of the first cut, which read it on a worker). The read is
    /// the launch load's own, bounded by size ([`crate::messages_store::load_tail`]:
    /// at most [`crate::messages_store::TAIL_BYTES`] of the log and
    /// [`crate::messages_store::WIRE_TAIL_BYTES`] of the wire's, regular files,
    /// no-follow) — a message log is tens of kilobytes. Nothing with no writer:
    /// no file was opened, so there is none to read.
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
        // …and a carried row the PARKED PARENT ended after the capture (a press,
        // a resolve, a heal) stays ended (round eight of the update audit, lead
        // U62). That ends the ROW, and nothing beside it.
        let ended = self
            .messages
            .end_carried_rows_the_parent_ended(&disk, Instant::now());
        if !ended.is_empty() {
            aterm_log::info!(
                "messages: {} carried row(s) the parked parent ended stay ended",
                ended.len()
            );
        }
        // A health warning the parked parent HEALED is no longer this process's
        // to heal: its healing is on record already, and a second would say it
        // twice. Decided by THAT RECORD, never by how the row ended (the second
        // review of U62): the parent records a healing from what the warning
        // said, whether its row was still up, dismissed by the person, or folded
        // off the glass 45 s after it was said (the usual state at a park), and a
        // dismissal, an answer or a withdrawal there records none.
        if let Some(parent) = self.parked_parent.take() {
            self.forget_health_the_parked_parent_healed(&disk, parent);
        }
    }

    /// Each update-health warning this process remembers
    /// (`App::update_health_said`) whose healing the PARKED PARENT put on
    /// record after the capture is healed here as the parent healed it, with no
    /// second record: what it said, its latch, its day's entry and any live row
    /// of its kind go ([`Self::heal_update_health_where`] less the record).
    ///
    /// The parent's record is read from `disk`, the file read again at Commit:
    /// a record in [`update_words::health_recovered`]'s words for what the
    /// warning said that the PARENT wrote AFTER THE CAPTURE ([`ParkedParent`]):
    /// written under an id of the parent's band — from the carry's next id, the
    /// [`PARKED_PARENT_ID_RESERVE`] it keeps below — as the file numbered it
    /// (a record a collision moved to a fresh id is judged by the number it was
    /// written under, [`aterm_messages::MessageLog::file_id_of`]), and stamped
    /// at or after the capture. The id alone was no such proof (the U5 group's
    /// review): every aterm of the user writes this file from its own counter,
    /// so another window's healing of the same words — the same day's — sat at
    /// an id above the parent's, and a record of any writer that collided
    /// loaded above the file's top, and either one dropped a warning still
    /// failing here, its day's entry with it (L5). A warning the parent did not
    /// heal stays, whatever its row did there, for the proof here that answers
    /// it.
    ///
    /// WHAT IT STILL CANNOT TELL APART (the second review of #909): the band
    /// and the stamp single out the parent's records only from writers outside
    /// them both. Every aterm counts its ids from the file's top at its launch
    /// and then from its own writes alone, so a window launched the same day
    /// mints, as a rule, within [`PARKED_PARENT_ID_RESERVE`] of the parent's
    /// next id, and the band seldom shuts it out; the stamp does, except for
    /// what it records during the park itself. So another window that records
    /// the healing of the same kind, in the same day's words, while the parent
    /// is parked after the capture (seconds; the automatic lane's hold is
    /// capped at two minutes) is read as the parent's, and a warning still
    /// failing here is dropped with its latch and its day's entry, as #909
    /// described. Nothing in the file says which process wrote a record:
    /// telling the two apart needs a writer's mark on every record, a change
    /// to the file every aterm of the user shares, not made for a race this
    /// narrow.
    ///
    /// ITS ROW ENDS TOO (the U5 group's review): the parent heals a warning
    /// whose row it no longer shows (one that folded off its glass after the
    /// capture, say) with no row to resolve, and the carried row of it here,
    /// which the parent's ending of the row cannot end, would come back on
    /// glass with a fresh hold at Commit. It is resolved as a warning, as the
    /// heal resolves it, with no repaint (the Commit wake republishes).
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

    /// THE SUCCESSOR'S FIRST FRAMES, before Commit: re-seed the rows the
    /// outgoing process carried into the center (each keeps its id, words and
    /// hold under the handoff's staleness cap until Commit), queue the lines
    /// the parent never wrote, and — when this process is the handed-off
    /// successor with a carried progress row — say its own phase in that row
    /// ("almost done — what you type is kept", [`update_words::finishing`])
    /// rather than the parent's "installing…", and adopt it as this process's
    /// flow row. A carried warning that installing fails leaves with it: the
    /// build that landed is the proof ([`HealthProof::Landed`]). A carried
    /// check, download, stall or lock-held warning stays up: a landing proves
    /// nothing about those halves, and the producer here whose own proof heals
    /// each takes it up ([`Self::take_up_carried_update_health`]). No sync
    /// here: the row count is the carried one until Commit, and nothing paints
    /// yet. `finishing` is the running build's version.
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
        // AN INTENT ONLY THE OLD PROCESS COULD PERFORM DOES NOT CROSS (ruling
        // 265 / 417 (j), 441 (w)): `End sessions` hangs up the shells THIS
        // launch reattached from the keeper (`keeper_recovered`), and an update
        // successor reattached none, so the press would end nothing and the
        // page would say the running sessions "had already ended". The
        // recovery row keeps its words: they are the crashed launch's history.
        if self.keeper_recovered.is_empty() {
            let end = Intent::EndRecovered.encode();
            for message in &mut carried.live {
                message.actions.retain(|action| *action != end);
            }
        }
        // THE PARENT'S NEXT ID crosses even when no row does (round-seven
        // update audit, finding 18): the automatic lane parks with nothing on
        // the band, and the parent recorded past what this process loaded at
        // its boot — `seed_carried` raises the ids first, rows or none.
        let nothing_carried = carried.is_empty() && carried.next_id == 0;
        // THE PARENT RECORDS ON UNTIL COMMIT (round-seven update audit,
        // finding 18, as reviewed): the carry's next id was taken at the park,
        // and the parent keeps minting from it — a harness note, a toolchain
        // or `appnotice` record, an update record — while this process mints
        // too (before Commit, and "Updated to aterm vX" at it). The parent
        // keeps the band below `next_id + PARKED_PARENT_ID_RESERVE`; this
        // process mints above it, so no id is handed out twice and none of
        // this process's records sits below one a `messages since=` reader
        // took from the parent. The records themselves cross in the file
        // ([`Self::reload_messages_log_at_commit`]). BEFORE the seed: a record
        // of another writer the seed moves off a carried number takes a fresh
        // id, and it must be above the band too, never the parent's next one
        // (design ruling 415 (d)).
        if self.incoming_handoff_pending && !nothing_carried {
            self.messages.raise_next_id(
                carried
                    .next_id
                    .max(1)
                    .saturating_add(PARKED_PARENT_ID_RESERVE),
            );
            // Where and when the parent's records after the capture begin: a
            // healing it records while parked is read back at Commit by those
            // bounds ([`Self::forget_health_the_parked_parent_healed`]). A parent
            // older than the band's carry hands no next id, and one older than
            // the capture's stamp hands no time: nothing then marks its records
            // after the capture, and none is read as a healing.
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
        // WHAT THE PARENT'S WARNINGS SAID, whether or not their rows still
        // stand (round six, finding 54; round 44 for a row that folded or never
        // crossed): the landing at Commit is the proof, and it is this
        // process's to record — the parent `_exit`s at Commit. A plain restart
        // (`finishing` None) carries nothing it could heal. After the raise:
        // taking a warning up claims its digest line, which may mint an id.
        if finishing.is_some() {
            self.take_up_carried_health_said(&carry.update_health_said);
        }
        if nothing_carried {
            return;
        }
        self.messages.seed_carried(&carried, wall_stamp_now(), now);
        // A row about the OLD PROCESS leaves with it: a session's wait (a
        // paste on its way, a rewrap) is the process's that watches it
        // (rulings 231–234), and this one watches nothing it did not start;
        // the dead accessibility publisher's "restart aterm to retry" is
        // answered by this very process, whose publisher is its own (finding
        // 42) — carried, either would stand for good.
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
            // The landing at Commit is the proof, and it is this process's to
            // record against what the parent's warning said
            // (`post_update_landed` → `heal_update_health`): the parent `_exit`s
            // at Commit and never learns its warning healed. It answers ONLY an
            // install warning (round-seven update audit, finding 54): a build
            // staged before the check or download streak began lands without
            // proving that checking or downloading works, and the updater's
            // ledger keeps those streaks (`record_apply_success` clears the
            // apply streak only). Such a warning stays up, TAKEN UP by the
            // producer here whose own proof heals it
            // ([`Self::take_up_carried_update_health`]) — never latched, which
            // stood for the launch (round eight, findings 15 and 301).
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
                // Remembered and taken up above with what the parent said,
                // unless an older parent carried the row alone. One entry per
                // carried row: two kinds the parent raised are two records at
                // their healings (ruling 318), never only the first.
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

    /// A HEALTH WARNING THE PARENT RAISED AND THE LANDING DOES NOT ANSWER (round
    /// eight, findings 15 and 301; round seven, finding 206), handed to the
    /// producer in THIS process whose own proof heals it — and NEVER LATCHED
    /// here. A latch only a proof lifts stood for the launch: this process's
    /// updater had announced nothing, so its first up-to-date check cleared the
    /// streak in the ledger and sent no recovery; the watchdog's stall heal
    /// needed a stall THIS watchdog had announced; the lock-held heal needed
    /// this process's own streak. The next episode of the kind — a new one,
    /// hours later — then reached only the log, with no row (L5), and its
    /// healing was recorded against the parent's old words.
    ///
    /// * The ledger's check and download warnings: this process's updater takes
    ///   the class up as announced ([`aterm_update::adopt_announced_health`]),
    ///   so a streak still failing is restated quietly — the person is not told
    ///   twice — and the ledger's healing, which a successor's first up-to-date
    ///   check brings, sends the recovery this process heals by
    ///   ([`Self::note_update_health`]). ONLY WHEN `title`, the warning's own
    ///   words, names a ledger class (`aterm_update::health_failing_class`): the
    ///   window reads a title it does not know as the check kind, the strand
    ///   notice's among them ([`aterm_update::STRANDED_TITLE`]), which is no
    ///   ledger failure — adopted as `manifest`, the successor's first cycle
    ///   over a ledger that never failed "healed" it while the machine stayed
    ///   stranded (round seven, group RL). Such a warning is only remembered;
    ///   this process says its own strand once, as every process does.
    /// * The watchdog's stall and lock-held warnings: this process's checker
    ///   stands to the parent's as a replacement generation does, and heals
    ///   them by a replacement's proof
    ///   ([`crate::update_checker_watch::CheckerWatchState`]'s
    ///   `adopt_carried_stall`, `adopt_carried_lock_held`). A stall of THIS
    ///   process's own checker is a new episode, said with its row; the
    ///   sibling still holding the lock is the same streak, followed rather
    ///   than said again.
    ///
    /// Its digest line is claimed again here, in `words`, the parent's
    /// sentence: the parent's pending claim died with it, and a warning taken
    /// up is not said again, so a failure that spans an update never reached
    /// "To look at". A parent older than the sentence's carry hands its excerpt
    /// alone (`since Sep 14`), which still dates the line.
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

/// What an update-health warning said ([`App::remember_update_health_said`]):
/// its kind, the title and first line its record of healing is written
/// against, and the sentence it was raised with, which its "To look at" line
/// quotes when a successor takes it up
/// ([`App::take_up_carried_update_health`]).
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

/// The ids a seamless successor leaves its parent, which records on from the
/// carry's next id until Commit (round-seven update audit, finding 18;
/// [`App::seed_carried_messages`]). The park lasts seconds (the automatic
/// lane's hold is capped at two minutes), `notice` is refused while parked
/// ([`App::take_notice`]), and what the parent still records then is the odd
/// harness note or toolchain and update record — a thousand is far past it.
/// Beyond it the two processes' ids could meet again; the merge at Commit
/// still keeps both records, the parent's under a fresh id
/// ([`aterm_messages::MessageCenter::merge_reloaded_log`]).
pub(crate) const PARKED_PARENT_ID_RESERVE: u64 = 1000;

/// What marks the PARKED PARENT's records after the capture, in the file every
/// aterm of the user shares ([`App::forget_health_the_parked_parent_healed`]):
/// the carry's next id, from which the parent mints below
/// [`PARKED_PARENT_ID_RESERVE`] more, and the capture's wall-clock stamp
/// (`WindowCarry::messages_captured_unix_ms`), at or after which it stamps them.
/// Neither names the writer: another window's record made during the park can
/// pass for the parent's (the limit that function's docs state).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkedParent {
    pub(crate) next_id: u64,
    pub(crate) captured_unix_ms: u64,
}

/// Whether a carried row keyed `key` states a fact about the PROCESS that
/// posted it — one a successor, a new process, has made untrue by existing
/// ([`App::seed_carried_messages`]): a session wait's row (the waits belong to
/// the process that watches them), or the dead accessibility publisher's (a
/// process gets one publisher; the successor starts its own).
fn describes_the_old_process(key: &str) -> bool {
    aterm_messages::waits::is_wait_key(key) || key == crate::message_reporters::KEY_A11Y_PUBLISHER
}

/// Tier-1 for `NativeUpdateCarriedHealth` (findings 206/207).
#[cfg(all(test, unix))]
#[path = "carried_health_conformance.rs"]
pub(crate) mod carried_health_conformance;

/// The band's motion on the host side — the idle law, hidden windows, the
/// frame cadence, the per-frame cost, the still looks, the faces that must
/// not move (review 2026-09-24).
#[cfg(test)]
#[path = "band_motion_tests.rs"]
mod band_motion_tests;

/// What a heal of the update-health warning rests on ([`App::heal_update_health`]).
/// Each proves one more half of updating than the last, so each heals only the
/// warnings it answers — the warning's kind names the broken half
/// ([`update_words::HealthKind`]), and the updater's own ledger reads the
/// same way: a whole-pipeline success clears every streak but an install streak.
/// [`Self::Landed`] stands apart from that ladder (its order is never read):
/// it answers the install warning only ([`Self::answers`]).
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
    /// A build LANDED (`post_update_landed`): installing works — and nothing
    /// more is proved (round-seven update audit, finding 54). Off the ladder:
    /// the build may have been staged before a check or download streak
    /// began, and the updater's ledger keeps those streaks through the
    /// install (`record_apply_success` clears the apply streak only), so a
    /// landing answers an install warning alone.
    Landed,
}

impl HealthProof {
    /// What the updater's `HEALTH_RECOVERED_TITLE` proves, from the class it
    /// names as the body (`aterm_update::HealthNotify`): the healing of that
    /// class answers exactly the warning that class raised. A heal of the
    /// download or check streak is no proof that installing works, and an
    /// install warning another producer raised stays up for the proof that
    /// answers it. An empty body — the heal before it named its class — keeps
    /// main's reading: the whole ledger healed.
    fn healed_by_ledger(class: &str) -> Self {
        if class.is_empty() {
            Self::Installed
        } else {
            Self::needed_for(update_words::HealthKind::of_class(class)).unwrap_or(Self::Installed)
        }
    }

    /// The proof a warning of `kind` needs before it may leave and its healing
    /// be recorded. The check half and the watchdog's stall need the least:
    /// the lane's old rule, "a check that downloads is a check that works" —
    /// for the stall, a check THIS process ran, never the shared ledger's
    /// healing ([`Self::answers_from_the_ledger`]). `None` for a warning no
    /// proof of this process's own answers: another
    /// aterm holding the update check's lock (round six, finding 53) is healed
    /// only by its streak ending, by the watchdog that said it.
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

    /// Whether this proof, as THE LEDGER'S HEALING ([`Self::healed_by_ledger`]),
    /// answers a warning of `kind`: [`Self::answers`], but never the window
    /// watchdog's stall (round-seven update audit, group RL).
    ///
    /// The ledger is the MACHINE's — a sibling aterm's good check heals it — and
    /// "Update check stopped" is about THIS process's check thread. Every
    /// generation ticks with what the process announced
    /// (`aterm_update::adopt_announced_health`), so a replacement the watchdog
    /// started behind a generation still holding the check lane, which can
    /// never check itself, sends the recovery of the class the process had
    /// announced once a sibling heals the ledger. As `Checked` it answered the
    /// stall: the row withdrawn, its latch lifted and "aterm updates work
    /// again" recorded against "Update check stopped" while no check could run
    /// here — the notice gone while updates were stopped (L5). The stall's own
    /// proof is this process's completed check (the watchdog's
    /// `heal_update_checker_stall`) or its progress ([`Self::answers`]).
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
