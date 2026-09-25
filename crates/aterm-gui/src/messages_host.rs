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
//! the end: it keeps appending through the park and
//! [`App::flush_messages_log`]es right before it execs the successor (`exec`
//! runs no destructor, so nothing the writer still held would land). Only
//! the SUCCESSOR's drain waits ([`App::message_persist_frozen`]), until
//! Commit — a refused successor then wrote nothing about rows that were the
//! parent's to record. The first cut froze the parent's drain too and
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

use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_messages::{
    ActionIndex, Carry, Decision, EchoKind, HOLD_ROUTE, Hold, Intent, Live, LogRecord, MAX_ROWS,
    Message, MessageId, Outcome, PROGRESS_GRACE, Restatement, Retired, Severity, WallStamp, wire,
    words,
};

use crate::message_reporters::{PANE_FULL_DISK_ACCESS, log_did_not_open};
use crate::native_app::MessageActOutcome;
use crate::native_settings::SettingsRoute;
use crate::toolchain_words::{self, HookDialect, SnapshotWords};
use crate::update_words;
use crate::{App, WindowId};

/// The tag chips' fixed order on Settings ▸ Messages (design §4.3): the
/// families a person acts on first, then the ambient ones. Every word of the
/// reporter vocabulary is here (`tags_chips_cover_the_vocabulary` pins it); a
/// tag this build does not know — a line another build wrote into the same
/// log — follows them in first-seen order rather than vanishing.
pub(crate) const TAG_ORDER: [&str; 13] = [
    "crash",
    "config",
    "update",
    "toolchain",
    "packages",
    "privacy",
    "session",
    "window",
    "render",
    "a11y",
    "fabric",
    "harness",
    "system",
];

/// THE SHARED PROJECTION behind Settings ▸ Messages (design §4.2): built
/// ONCE by the host ([`App::messages_state`]) from the same [`LogRecord`]s
/// the `messages` verb prints — the Packages page's "screen == introspection"
/// rule — and handed to the Settings controller whole
/// (`SettingsApp::replace_messages`). The page never reaches the center.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessagesState {
    /// The center's revision the projection was built at.
    pub(crate) revision: u64,
    /// The wall clock at the build, for the relative times.
    pub(crate) now_unix_ms: u64,
    /// Every record in the ring, NEWEST FIRST — at most `LOG_CAP`.
    pub(crate) entries: Vec<MessageView>,
    /// `(tag, count)` for every tag PRESENT, in [`TAG_ORDER`] then first-seen
    /// order — the page's filter chips.
    pub(crate) tags: Vec<(String, usize)>,
    /// The folder the log files live in (`aterm.log`, `messages.log`,
    /// `packages.log`, the crash reports) — what the page NAMES off macOS,
    /// where there is no NSWorkspace to show it; the host opens it itself
    /// (`AppEffect::OpenLogFolder`, never a path from the view). `None` with
    /// no log dir.
    pub(crate) log_folder: Option<String>,
    /// Whether these messages are SAVED — `messages.log` has a writer. A page
    /// over an in-memory ring only (a read-only log dir, a full disk) says so:
    /// the person must not believe the record survives a quit.
    pub(crate) saved: bool,
}

impl MessagesState {
    /// The projection before the host has published one: nothing, revision 0
    /// (every real revision is higher, so the first publish always lands).
    pub(crate) fn empty() -> Self {
        Self {
            revision: 0,
            now_unix_ms: 0,
            entries: Vec::new(),
            tags: Vec::new(),
            log_folder: None,
            saved: true,
        }
    }

    /// The entry with this id, if the ring still holds it.
    pub(crate) fn entry(&self, id: u64) -> Option<&MessageView> {
        self.entries.iter().find(|e| e.id == id)
    }
}

/// One record as the page shows it: the live row's CURRENT words while it is
/// live (a restatement is never logged, so the record's own words would be
/// its first ones), the record's final words once retired — the same choice
/// `wire::message_row` makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessageView {
    /// The raw message id.
    pub(crate) id: u64,
    /// The wall clock at ingress.
    pub(crate) at_unix_ms: u64,
    /// The reporter family.
    pub(crate) tag: String,
    /// The wire word: `success` / `info` / `warn` / `error`.
    pub(crate) severity: &'static str,
    /// The glyph the band paints at column 1.
    pub(crate) glyph: char,
    /// The title.
    pub(crate) title: String,
    /// The detail lines, pre-split, UNWRAPPED — the page wraps them to its
    /// own measure.
    pub(crate) detail: Vec<String>,
    /// The `state=` word the `messages` verb prints ([`wire::state_word`]):
    /// `live` / `held` / `folded` / `stale` / `unseen` / `superseded` /
    /// `resolved-ok` / `resolved-warn` / `dismissed` / `answered` /
    /// `evicted` / `carried` / `withdrawn` / `recorded` (a record, never on
    /// the band).
    pub(crate) state: &'static str,
    /// The band row (0 = topmost) while it is on the glass.
    pub(crate) on_glass: Option<u8>,
    /// Duplicate posts folded into this record (1 = posted once).
    pub(crate) repeats: u32,
    /// The wall clock it retired at, when it has.
    pub(crate) retired_unix_ms: Option<u64>,
    /// The id of the post that superseded it, when that is how it left.
    pub(crate) superseded_by: Option<u64>,
    /// The capsule a person answered a decision row with.
    pub(crate) answer: Option<String>,
    /// The authored intents, re-offered from the page.
    pub(crate) actions: Vec<MessageActionView>,
}

/// One authored intent as the page offers it: a button by index, enabled only
/// while pressing it would still do the thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessageActionView {
    /// The `ActionIndex` a press names.
    pub(crate) index: u8,
    /// The full capsule label (`Intent::label`).
    pub(crate) label: &'static str,
    /// Whether a press can still be performed (design §4.2): a decision
    /// (`Not now`, `Install`) only while the row is live; `Install now` only
    /// while that build is still the staged one; `Open log` only while the
    /// file is there as a regular file — the performer's own test, so a
    /// capsule is not offered where the press would be refused; every
    /// navigation always.
    pub(crate) still_actionable: bool,
}

impl MessageView {
    /// Warn or Error: the "Warnings & errors" chip's set.
    pub(crate) fn alarm(&self) -> bool {
        matches!(self.severity, "warn" | "error")
    }

    /// The state in PLAIN words for the expanded row's meta line (the owner's
    /// rule of 2026-09-23 — no internal jargon; design ruling 66): what the
    /// row is doing, and — once it has left — how long it stood: `showing
    /// now`, `waiting to show`, `shown for 2 min`, `recorded` (never on the
    /// band), `replaced by a newer
    /// message after 40 s`, `answered: Not now`, `cleared with a warning after
    /// 3 h`. The wire keeps its own words ([`MessageView::state`], `copy_text`).
    pub(crate) fn state_words(&self) -> String {
        let span = self
            .retired_unix_ms
            .map(|at| span_words(at.saturating_sub(self.at_unix_ms)));
        let after = |words: &str| match &span {
            Some(span) => format!("{words} after {span}"),
            None => words.to_string(),
        };
        match self.state {
            "live" | "held" => match self.on_glass {
                Some(_) => "showing now".to_string(),
                None => "waiting to show".to_string(),
            },
            // A record (`Hold::LogOnly`) retires at the instant it is posted:
            // it was written down, never shown. A row that folded stood on the
            // band for its hold, never zero.
            "folded" if self.retired_unix_ms == Some(self.at_unix_ms) => "recorded".to_string(),
            "folded" => match &span {
                Some(span) => format!("shown for {span}"),
                None => "shown".to_string(),
            },
            "stale" => after("stopped reporting"),
            "unseen" => "not shown \u{2014} too many at once".to_string(),
            "superseded" => after("replaced by a newer message"),
            "resolved-ok" => after("cleared"),
            "resolved-warn" => after("cleared with a warning"),
            "dismissed" => "dismissed".to_string(),
            "answered" => match &self.answer {
                Some(label) => format!("answered: {label}"),
                None => "answered".to_string(),
            },
            // A record (`Retired::Recorded`, the log's `how=folded rec=1`) was
            // never on the band: "shown for 0 s" would say it had been (design
            // §10.11, ruling 149 — one word, main's).
            "recorded" => "recorded".to_string(),
            "evicted" => "dropped \u{2014} too many at once".to_string(),
            "carried" => "carried over to the updated aterm".to_string(),
            "withdrawn" => after("withdrawn"),
            other => after(other),
        }
    }

    /// The severity in plain words for the meta line: `Error`, `Warning`,
    /// `Info`, `Done` (the wire's `error` / `warn` / `info` / `success`).
    pub(crate) fn severity_words(&self) -> &'static str {
        match self.severity {
            "error" => "Error",
            "warn" => "Warning",
            "success" => "Done",
            _ => "Info",
        }
    }

    /// What Copy puts on the clipboard: the title, then `<stamp> · <tag> ·
    /// <sev>`, then every detail line — byte for byte `words::copy_text` of
    /// the record (`copy_text_of_a_view_is_the_engines`), built here because
    /// the page holds views, not records.
    pub(crate) fn copy_text(&self) -> String {
        let mut out = self.title.clone();
        out.push('\n');
        out.push_str(&words::stamp_words(self.at_unix_ms));
        out.push_str(" \u{00b7} ");
        out.push_str(&self.tag);
        out.push_str(" \u{00b7} ");
        out.push_str(self.severity);
        for line in &self.detail {
            out.push('\n');
            out.push_str(line);
        }
        out
    }
}

/// A tag's chip and meta name in plain words (design ruling 66): `Updates`,
/// `ALab tools`, `Display` — the wire and the chips' action keys keep the tag
/// itself. A tag this build does not know reads as itself.
pub(crate) fn tag_words(tag: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(match tag {
        "crash" => "Crashes",
        "config" => "Config",
        "update" => "Updates",
        "toolchain" => "ALab tools",
        "packages" => "Packages",
        "privacy" => "Privacy",
        "session" => "Sessions",
        "window" => "Windows",
        "render" => "Display",
        "a11y" => "Accessibility",
        "fabric" => "Fabric",
        "harness" => "Agents",
        "system" => "System",
        other => return std::borrow::Cow::Borrowed(other),
    })
}

/// A span in words: `40 s`, `2 min`, `3 h`, `2 d`.
fn span_words(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else if secs < 86_400 {
        format!("{} h", secs / 3600)
    } else {
        format!("{} d", secs / 86_400)
    }
}

/// Whether pressing `intent` from the page would still do the thing (the
/// rule on [`MessageActionView::still_actionable`]). `staged` is the build
/// the updater holds ready right now, if any. `Open log` asks
/// `symlink_metadata`, as [`admissible_log_path`] does at press time — a
/// symlink planted in the log dir is refused there, so it is not offered
/// here (the rest of that test needs the log dir, which is a directory
/// probe per view; the press re-validates the whole rule).
fn still_actionable(intent: &Intent, live: bool, staged: Option<u64>) -> bool {
    match intent {
        Intent::NotNow { .. } => live,
        Intent::ApplyUpdate { build } => staged == Some(*build),
        Intent::OpenPath { path } => {
            std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
        }
        Intent::Details
        | Intent::OpenSettings { .. }
        | Intent::OpenConfigEditor { .. }
        | Intent::OpenSystemPane { .. }
        | Intent::NewWindow => true,
    }
}

/// One record's view (the `wire::message_row` choice of words: the live row's
/// while it is live, the record's once retired).
fn message_view(
    rec: &LogRecord,
    live: Option<&Live>,
    on_glass: Option<u8>,
    staged: Option<u64>,
) -> MessageView {
    let (severity, glyph, title, detail, actions, repeats) = match live {
        Some(l) => (
            l.msg.severity,
            l.msg.glyph,
            &l.msg.title,
            &l.msg.detail,
            &l.msg.actions,
            l.repeats,
        ),
        None => (
            rec.severity,
            rec.glyph,
            &rec.title,
            &rec.detail,
            &rec.actions,
            rec.repeats,
        ),
    };
    let (superseded_by, answer) = match rec.retired() {
        Some(Retired::Superseded { by }) => (Some(by.raw()), None),
        Some(Retired::Answered { label }) => (None, Some(label.clone())),
        _ => (None, None),
    };
    MessageView {
        id: rec.id.raw(),
        at_unix_ms: rec.stamp.unix_ms,
        tag: rec.tag.as_str().to_string(),
        severity: severity.as_str(),
        glyph: glyph.ch(),
        title: title.clone(),
        detail: detail.clone(),
        state: wire::state_word(rec, live),
        on_glass,
        repeats,
        retired_unix_ms: rec.retired_unix_ms,
        superseded_by,
        answer,
        actions: actions
            .iter()
            .enumerate()
            .filter_map(|(k, intent)| {
                Some(MessageActionView {
                    index: u8::try_from(k).ok()?,
                    label: intent.label(),
                    still_actionable: still_actionable(intent, live.is_some(), staged),
                })
            })
            .collect(),
    }
}

/// The page's feedback line for an act the host performed (`performed`) or
/// refused: what happened, in the words a person would use.
fn act_feedback(intent: &Intent, performed: bool) -> String {
    match (intent, performed) {
        (Intent::Details, true) => "Opened".to_string(),
        (Intent::Details, false) => "Could not open the entry".to_string(),
        (Intent::OpenSettings { .. }, true) => {
            format!("Opened Settings \u{25b8} {}", intent.label())
        }
        (Intent::OpenSettings { route }, false) => format!("No Settings page at {route}"),
        (Intent::OpenConfigEditor { .. }, true) => "Opened aterm.toml".to_string(),
        (Intent::OpenConfigEditor { .. }, false) => "Could not open aterm.toml".to_string(),
        (Intent::OpenPath { .. }, true) => "Opened crash log".to_string(),
        (Intent::OpenPath { .. }, false) => "Could not open the crash log".to_string(),
        (Intent::OpenSystemPane { .. }, true) => "Opened System Settings".to_string(),
        (Intent::OpenSystemPane { .. }, false) => "System Settings did not open".to_string(),
        (Intent::ApplyUpdate { .. }, _) => "Installing the update".to_string(),
        (Intent::NotNow { .. }, _) => "Not now recorded".to_string(),
        (Intent::NewWindow, true) => "Opened a new window".to_string(),
        (Intent::NewWindow, false) => "Could not open a window".to_string(),
    }
}

#[cfg(test)]
thread_local! {
    static PTY_RESIZES_FOR_MESSAGES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// The repaints `notice` asked for at once (`Paint::Now`) ON THIS THREAD —
    /// the flood test's count (design ruling 170).
    static WIRE_SYNCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many window re-grids the message band has paid for ON THIS THREAD
/// (test-only: the launch-pass test's "at most two re-grids" — the
/// `PTY_RESIZES_FOR_PRESENCE` shape; a headless App is driven on its test's
/// own thread).
#[cfg(test)]
pub(crate) fn message_regrids() -> u64 {
    PTY_RESIZES_FOR_MESSAGES.with(|c| c.get())
}

/// The wall clock now, as a message's ingress stamp: milliseconds since the
/// Unix epoch, 0 on a clock before it (the engine reads no clock of its own).
pub(crate) fn wall_stamp_now() -> WallStamp {
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
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
    std::thread::Builder::new()
        .name("aterm-open-path".into())
        .spawn(move || {
            // Nobody is blocked on the editor coming up: below the keystroke
            // path, like every other one-shot spawn (`crate::qos`).
            crate::qos::set_self(crate::qos::Role::Background);
            #[cfg(target_os = "macos")]
            let status = std::process::Command::new("/usr/bin/open")
                .arg("-t")
                .arg(&target)
                .status();
            #[cfg(target_os = "linux")]
            let status = std::process::Command::new("xdg-open").arg(&target).status();
            #[cfg(windows)]
            let status = std::process::Command::new("notepad.exe")
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
    pub(crate) fn replace_config_messages(&mut self, msgs: impl IntoIterator<Item = Message>) {
        let scope: Vec<String> = self
            .messages
            .live_rows()
            .filter_map(|l| l.msg.key.clone())
            .filter(|key| key.starts_with("config."))
            .collect();
        // Every family was re-derived, so every `config.` key this process
        // has said something under is in the reload's verdict.
        let mut rederived: std::collections::BTreeSet<String> = self
            .messages
            .log()
            .records()
            .filter(|r| r.retired_at.is_some())
            .filter_map(|r| r.key.clone())
            .filter(|key| key.starts_with("config."))
            .collect();
        rederived.extend(scope.iter().cloned());
        self.replace_config_set(&scope, &rederived, msgs.into_iter().collect());
    }

    /// [`Self::replace_config_messages`] for a reload that re-derived only
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
        self.replace_config_set(&scope, &rederived, msgs.into_iter().collect());
    }

    /// The replace itself, over the live rows under `scope`. A reload runs on
    /// EVERY Settings patch, Presence toggle and Serious Mode write (the
    /// parsed config changed), so a family's words are compared, not re-posted
    /// (review 2026-09-24 — "repeats go to the log"):
    ///
    /// * a live row whose family comes back in the same words stands — no
    ///   resolve, no re-post, no re-announcement;
    /// * a row whose family is gone, or whose words changed, is resolved (the
    ///   log says the problem was fixed or became another), and a changed
    ///   family posts its new row;
    /// * a family whose row already LEFT the glass — its hold ran out, it was
    ///   read, or it was recorded as a repeat before — and that comes back in
    ///   the words THIS process last recorded under its key is a RECORD
    ///   ([`Self::config_repeat`]): Settings ▸ Messages and messages.log keep
    ///   it, the glass does not. A new or changed problem still raises its row;
    /// * a family in `rederived` that the reload found FIXED is remembered as
    ///   fixed (`App::config_cleared`), so the same words coming back after
    ///   the fix — a problem broken again — raise their row again, whether or
    ///   not its row was still on the glass when the fix landed.
    fn replace_config_set(
        &mut self,
        scope: &[String],
        rederived: &std::collections::BTreeSet<String>,
        msgs: Vec<Message>,
    ) {
        for key in rederived {
            if !msgs.iter().any(|m| m.key.as_deref() == Some(key.as_str())) {
                self.config_cleared.insert(key.clone());
            }
        }
        let now = Instant::now();
        let mut resolved = 0usize;
        for key in scope {
            let Some((id, title, detail)) = self
                .messages
                .live_by_key(key)
                .map(|l| (l.id, l.msg.title.clone(), l.msg.detail.clone()))
            else {
                continue;
            };
            let stands = msgs.iter().any(|m| {
                m.key.as_deref() == Some(key.as_str())
                    && m.hold != Hold::LogOnly
                    && m.title == title
                    && m.detail == detail
            });
            if !stands {
                resolved += usize::from(self.messages.resolve(id, Outcome::Ok, now));
            }
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
            let msg = if msg.hold != Hold::LogOnly && self.config_repeat(&msg) {
                msg.hold(Hold::LogOnly)
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
        self.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key))
            .is_some_and(|r| {
                r.retired_at.is_some()
                    && r.title == msg.title
                    && r.detail == msg.detail
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
    fn post_unsynced(&mut self, msg: Message, stamp: WallStamp) -> MessageId {
        let posted = self.messages.post(msg, stamp, Instant::now());
        if let Some(live) = self.messages.live(posted.id) {
            aterm_log::info!("message {} {}: {}", posted.id, live.msg.tag, live.msg.title);
        }
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
        if live && self.messages.revision() != revision {
            self.sync_messages();
        }
        live
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

    /// After any message input, on the timer-wake sweep, at the park and
    /// when a handoff finishes: retire what has expired (holds suspended
    /// under a freeze), commit the row count, append what the log drained
    /// (never under a freeze), and repaint every window — the band is chrome
    /// in each of them; the RepaintKey's band fingerprint keeps a
    /// byte-identical tick from re-presenting.
    pub(crate) fn sync_messages(&mut self) {
        let now = Instant::now();
        let _ = self.settle_messages(now);
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
    pub(crate) fn settle_messages(&mut self, now: Instant) -> bool {
        let frozen = self.message_holds_frozen();
        let settled = self.messages.settle(now, !frozen);
        let committed = self.sync_message_band_rows(now);
        if !self.message_persist_frozen() {
            self.persist_messages();
        }
        // A `notice progress` restate the wire's pacing held back (ruling 170):
        // its one paced paint falls due here, however many lines arrived.
        let wire = self.wire_gate.take_due(now);
        settled.glass_changed || committed || wire
    }

    /// Drain the center's pending log lines to the writer. With no writer (a
    /// test App, or a log dir that could not be opened) the lines are dropped
    /// here, so the pending queue never fills and reports a `Dropped` count
    /// for a file that does not exist.
    fn persist_messages(&mut self) {
        let lines = self.messages.drain_shelved_for_persist();
        if lines.is_empty() {
            return;
        }
        if let Some(writer) = &self.messages_log {
            writer.append(&lines);
        }
    }

    /// THE PARENT'S LAST WRITE before it execs the successor of a seamless
    /// handoff: what the center holds goes to the writer, and the writer
    /// lands everything it holds before this returns — `exec` runs no
    /// destructor, so the drop-time join would never happen and the lines
    /// the thread still held would die with the process. The writer stays
    /// open: a failed exec leaves a process that carries on recording.
    pub(crate) fn flush_messages_log(&mut self) {
        self.persist_messages();
        if let Some(writer) = &self.messages_log {
            writer.flush();
        }
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
    pub(crate) fn sync_message_band_rows(&mut self, now: Instant) -> bool {
        if self.message_holds_frozen() {
            return false;
        }
        let afford = self
            .windows
            .values()
            .filter(|ws| ws.os_window.is_some())
            .map(|ws| {
                ws.rows
                    .saturating_add(self.message_band_rows)
                    .saturating_sub(1)
            })
            .min()
            .unwrap_or(MAX_ROWS);
        let moved = self.messages.commit_rows(now, afford);
        let rows = self.messages.committed_rows();
        if moved.is_none() && rows == self.message_band_rows {
            return false;
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
        self.pending_update_handoff.is_some() || self.incoming_handoff_pending
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
        let settle = self.messages.deadline(!self.message_holds_frozen());
        let publish = self.messages_publish_due_at();
        // The wire's one paced paint (ruling 170); `None` when idle (FL-1).
        let wire = self.wire_gate.due();
        [settle, publish, wire].into_iter().flatten().min()
    }

    /// Post what the pre-App inbox holds (every park; one atomic load when
    /// empty), each with the stamp it was queued under.
    pub(crate) fn drain_message_inbox(&mut self) {
        let queued = crate::message_inbox::take_queued();
        if queued.is_empty() {
            return;
        }
        for m in queued {
            self.post_unsynced(m.message, m.stamp);
        }
        self.sync_messages();
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
            // stays for the watcher either way (R16); the `opened` marker is
            // recorded so a later process can acknowledge the flip.
            Intent::OpenSystemPane { pane } => {
                let Some(pane) = privacy_pane(&pane) else {
                    aterm_log::warn!("message {id}: no system pane {pane:?}");
                    return false;
                };
                let opened = matches!(
                    crate::native_settings::ConsentGestures::for_instance(self.headless)
                        .open_settings(pane),
                    crate::menu::SettingsOpen::Anchored | crate::menu::SettingsOpen::PaneRoot
                );
                let route = crate::menu::privacy_settings_path_words(pane);
                self.restate_message(id, route_words(opened, route));
                self.note_macos_access_settings_opened(now);
                opened
            }
            // `press_update_bar`'s law: a press on a row still offering this
            // build applies it in place — where the stage is really ready and
            // the seamless handoff can run — and opens the details otherwise.
            Intent::ApplyUpdate { build } => {
                let offered = self
                    .messages
                    .live(id)
                    .is_some_and(|l| l.msg.actions.contains(&Intent::ApplyUpdate { build }));
                let staged_ready = self.staged_update_ready();
                let handoff_available = self.seamless_handoff_unavailable().is_none();
                if Self::update_bar_press_applies(offered, staged_ready, handoff_available) {
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

    /// THE PAGE'S OWN PRESS on an entry's button (design §4.4,
    /// `AppEffect::MessageAct`): the reducer named the entry and the button
    /// by IDENTITY — an id and an index, never a path or a route — and the
    /// host resolves both here. A LIVE row goes through the engine's `act`
    /// (the press is logged, a `Not now` retires its row) exactly as a band
    /// capsule does; a RETIRED record re-offers the intent the log kept
    /// (design §1.3: every intent is codec-able for exactly this), except a
    /// decision, which only a live row can answer. Then the intent is
    /// performed in `wid` and the outcome worded for the page's feedback.
    pub(crate) fn perform_message_act(
        &mut self,
        wid: WindowId,
        id: u64,
        action: u8,
    ) -> MessageActOutcome {
        let Some(id) = MessageId::from_raw(id) else {
            return MessageActOutcome::Gone;
        };
        let intent = if self.messages.live(id).is_some() {
            let intent = self.messages.act(id, ActionIndex(action), Instant::now());
            // The press is on record (`Acted`) whatever the intent does next.
            self.sync_messages();
            intent
        } else {
            self.messages
                .log()
                .get(id)
                .and_then(|rec| rec.actions.get(usize::from(action)).cloned())
                .filter(|intent| !intent.is_ask())
        };
        let Some(intent) = intent else {
            return MessageActOutcome::Gone;
        };
        let feedback = |performed| act_feedback(&intent, performed);
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
    pub(crate) fn take_notice(&mut self, req: wire::NoticeRequest) -> String {
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
        let applied = wire::apply(
            &mut self.messages,
            &mut self.wire_gate,
            req,
            wall_stamp_now(),
            Instant::now(),
        );
        if applied.paint == wire::Paint::Now {
            #[cfg(test)]
            WIRE_SYNCS.with(|c| c.set(c.get() + 1));
            self.sync_messages();
        }
        if applied.note {
            aterm_log::info!("notice: {}", applied.reply);
        }
        applied.reply
    }

    /// THE PROJECTION, built once from the ring (design §4.2): every record
    /// newest first, the live rows in their current words, the band row each
    /// is on, and whether each authored intent can still be pressed — the
    /// staged build read once for every `Install now`, the crash log's file
    /// stat'd per `Open log` (a few records at most; the publish runs at
    /// 2 Hz at the most). Tag counts in the chips' order.
    pub(crate) fn messages_state(&self) -> MessagesState {
        let now_unix_ms = wall_stamp_now().unix_ms;
        // The band row each live row is on, echoes counted (design §10.5 M2).
        let glass: Vec<(MessageId, u8)> = self
            .messages
            .on_glass()
            .filter_map(|l| {
                let row = self.messages.glass_position(l.id)?;
                u8::try_from(row).ok().map(|row| (l.id, row))
            })
            .collect();
        let staged = self
            .staged_update_ready()
            .then(|| {
                self.native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .map(|s| s.build)
            })
            .flatten();
        let log = self.messages.log();
        let mut entries = Vec::with_capacity(log.len());
        let mut counts: Vec<(String, usize)> = Vec::new();
        for rec in log.records().rev() {
            let live = self.messages.live(rec.id);
            let on_glass = glass
                .iter()
                .find(|(id, _)| *id == rec.id)
                .map(|(_, row)| *row);
            let view = message_view(rec, live, on_glass, staged);
            match counts.iter_mut().find(|(tag, _)| *tag == view.tag) {
                Some((_, n)) => *n += 1,
                None => counts.push((view.tag.clone(), 1)),
            }
            entries.push(view);
        }
        let mut tags: Vec<(String, usize)> = TAG_ORDER
            .iter()
            .filter_map(|tag| {
                counts
                    .iter()
                    .find(|(seen, _)| seen == tag)
                    .map(|(_, n)| ((*tag).to_string(), *n))
            })
            .collect();
        tags.extend(
            counts
                .into_iter()
                .filter(|(tag, _)| !TAG_ORDER.contains(&tag.as_str())),
        );
        MessagesState {
            revision: self.messages.revision(),
            now_unix_ms,
            entries,
            tags,
            log_folder: self
                .messages_log
                .as_ref()
                .and_then(crate::messages_store::Writer::folder)
                .map(|dir| dir.display().to_string()),
            saved: self.messages_log.is_some(),
        }
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
    /// RECORDED ONLY WHEN ITS WORDS CHANGE (2026-09-23; design ruling 58):
    /// atpkg prints the marker at the end of every pass, four and more times
    /// a day, and the log ring (`LOG_CAP`) and Settings ▸ Messages are for
    /// news. That memory (`managed_current_recorded`) is a log-noise rule,
    /// not the retired glass rule of ruling 34 — a new version, or a frozen
    /// tab counted or adopted, is new words and is recorded. `true` when a
    /// record was written; the caller logs INFO then, DEBUG otherwise.
    pub(crate) fn post_managed_current(&mut self, text: &str) -> bool {
        let frozen_tabs = self.frozen_path_tabs();
        let hooked = self.this_tab_hooked();
        let hook = self.hook_dialect();
        let Some(msg) = toolchain_words::managed_current(text, frozen_tabs, hooked, hook) else {
            return false;
        };
        let said = (msg.title.clone(), msg.detail.clone());
        if self.managed_current_recorded.as_ref() == Some(&said) {
            return false;
        }
        self.managed_current_recorded = Some(said);
        self.record_message(msg);
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

    /// R25 — a pass opens (`net-starting:`). Only a FIRST
    /// RUN opens the row (`first_run`, the lane's verdict for the child that
    /// printed it: `crate::pass_is_first_run`); a routine pass's announcement
    /// is the arm's log line alone (the silent lane, upstream dbf97ecff). The
    /// first-run child LATCHES the row from here until its exit
    /// ([`Self::toolchain_pass_ended`]).
    pub(crate) fn announce_toolchain_pass(&mut self, detail: &str, first_run: bool) {
        if !first_run {
            return;
        }
        self.toolchain_row_latched = true;
        self.toolchain_first_run = true;
        self.post_message(toolchain_words::announced(detail));
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
        let paints = first_run || heavy || self.toolchain_row_latched;
        // A high-water mark belongs to the live meter it was read from: with
        // no live row up there is none to clamp against.
        let peak = live_id.and(self.toolchain_pass_peak.clone());
        match toolchain_words::snapshot_words(snap, peak.as_ref(), !first_run) {
            SnapshotWords::Vanished | SnapshotWords::Over => {
                if !self.toolchain_row_latched {
                    self.retire_live_toolchain(None);
                }
            }
            SnapshotWords::Quiet => {}
            SnapshotWords::Live { message, pass_id } => {
                if !paints {
                    return;
                }
                self.toolchain_row_latched = true;
                self.toolchain_first_run |= first_run;
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
        if !self.toolchain_row_latched {
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
        self.toolchain_row_latched = false;
        let first_run = std::mem::take(&mut self.toolchain_first_run);
        let short = self.toolchain_first_run_short.take();
        self.retire_live_toolchain(Some(if clean {
            EchoKind::Complete
        } else {
            EchoKind::Fault
        }));
        if !clean
            && first_run
            && let Some(msg) = short
        {
            self.post_message(msg);
        }
    }
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
    /// owed the OS banner (the caller's). The three shapes:
    ///
    /// * THE LEDGER HEALED (`aterm_update::HEALTH_RECOVERED_TITLE`): the warning
    ///   leaves if it is still up, and the healing is on record against what it
    ///   said ([`Self::heal_update_health`]).
    /// * THE COUNT MOVED on a class this process already announced
    ///   (`HEALTH_RESTATED_TITLE`): a log line, nothing else — the row says which
    ///   half is broken and since when, neither of which a count moves, and the
    ///   person was told when it was announced (design ruling 59).
    /// * A CLASS IS FAILING: say what is wrong, where the user is looking — the
    ///   title names the broken half ("aterm can't install updates"), the excerpt
    ///   since when; the whole sentence — count, cause, command — is the row's
    ///   detail behind `Details ›`, and Settings ▸ Software Update, re-read by
    ///   the caller, headlines the same verdict. The row holds the warning's 45 s,
    ///   once per class per launch (the updater latches the announcement): the
    ///   2026-09-10 standing row was a ⚠ with nothing to press for the life of the
    ///   process, its cause cut off at 110 columns. What it said is kept, so the
    ///   healing is recorded against it.
    ///
    /// THE LATCH IS HERE, AT THE ONE DOOR (the review of the update audit's
    /// merge onto this surface). Three producers now announce "aterm can't
    /// install updates": the updater's persistent streak, its overdue notice
    /// (a newer build waiting over an hour) and the automatic lane's
    /// convergence (`App::announce_automatic_apply_stranded`), each with its
    /// own latch — so one stuck build could raise the ⚠ row and the OS banner
    /// two or three times in one launch. A failing title this process has
    /// already said, and that no proof has answered since
    /// (`update_health_latched`), is a log line and `false`: no row, no
    /// banner. A healing that answers the title re-opens it, so a new episode
    /// speaks again.
    pub(crate) fn note_update_health(&mut self, title: &str, body: &str) -> bool {
        if title == aterm_update::HEALTH_RECOVERED_TITLE {
            aterm_log::info!("update-health: {title}: {body}");
            self.heal_update_health(HealthProof::healed_by_ledger(body));
            return false;
        }
        if title == aterm_update::HEALTH_RESTATED_TITLE {
            aterm_log::info!("update-health: {title}: {body}");
            return false;
        }
        let msg = update_words::health_warning(title, body);
        if self.update_health_latched.contains(&msg.title) {
            aterm_log::info!("update-health: already said this launch: {title}: {body}");
            return false;
        }
        self.update_health_latched.push(msg.title.clone());
        aterm_log::warn!("update-health: {title}: {body}");
        self.retire_update_installing();
        self.update_health_said = Some((
            msg.title.clone(),
            msg.detail.first().cloned().unwrap_or_default(),
        ));
        self.post_message(msg);
        true
    }

    /// THE LEDGER HEALED as far as `proof` can tell: the health warning
    /// ([`update_words::KEY_HEALTH`]), if it is still up, is over — resolved
    /// with the outcome it stood for (a warning) — and, once per announced
    /// episode (after the warning's 45 s fold too), the healing is ON RECORD
    /// against what the warning said ([`update_words::health_recovered`]).
    ///
    /// ONLY WHERE THE PROOF ANSWERS THE WARNING ([`HealthProof`]). The title
    /// names the broken half, so a download cannot heal "aterm can't install
    /// updates": the record would read "aterm updates work again" while the
    /// install streak the updater's ledger still holds says otherwise — the
    /// ledger itself clears every streak on a whole-pipeline success but an
    /// install streak, which only an install clears. A warning the proof does
    /// not answer stays up (and remembered), for the proof that does.
    pub(crate) fn heal_update_health(&mut self, proof: HealthProof) {
        self.update_health_latched
            .retain(|title| proof < HealthProof::needed_for(title));
        if let Some(id) = self
            .messages
            .live_by_key(update_words::KEY_HEALTH)
            .filter(|l| proof >= HealthProof::needed_for(&l.msg.title))
            .map(|l| l.id)
        {
            self.resolve_message(id, Outcome::Warn);
        }
        if self
            .update_health_said
            .as_ref()
            .is_some_and(|(title, _)| proof >= HealthProof::needed_for(title))
            && let Some((title, line0)) = self.update_health_said.take()
        {
            self.record_message(update_words::health_recovered(&title, &line0));
        }
    }

    /// R36 — THE NEW BUILD TOOK OVER (or a cold-lane boot found it already
    /// running): the lane's live flow row — the carried Finishing row, or an
    /// Installing row — is resolved `Ok` (the work was delivered: its Complete
    /// echo is the landing's moment, ruling 141), and the good
    /// news is a RECORD in main's words ([`update_words::landed`]: "Updated to
    /// aterm vX") — and, when this process (or the predecessor that handed
    /// over) downloaded THIS build, how long the update took, on record at the
    /// landing instant ([`update_words::installed_after`]). Never for a build
    /// met already on disk, nor for another build's download.
    ///
    /// `repainted` is how many tabs the seamless successor adopted onto a
    /// blank screen (the 2026-09-22/23 update audit, plan P1-5 —
    /// [`Self::seamless_repainted_tabs`]; 0 on the cold lane, which carried no
    /// screen): the landing record names them and says why they came over
    /// blank. A record only, like the landing itself — one fact, one entry.
    pub(crate) fn post_update_landed(&mut self, version: &str, build: u64, repainted: usize) {
        self.heal_update_health(HealthProof::Installed);
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
        self.record_message(update_words::landed(version, build, repainted));
        if let Some(took) = took {
            self.record_message(update_words::installed_after(version, took));
        }
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
    pub(crate) fn ensure_staged_decision(&mut self, build: u64) {
        let posture = self.apply_posture_for(build);
        self.ensure_staged_decision_in(build, posture);
    }

    /// [`Self::ensure_staged_decision`] under an explicit `posture` — the
    /// seam its tests drive, since a headless App's own posture is always
    /// the handoff-off record.
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
    /// beside `Update didn't finish`) — so it leaves now rather than standing
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
    /// execs, or inside the park's few milliseconds, is a line never written, and
    /// the seamless parent `_exit`s at Commit without a flush. `build` is the
    /// RUNNING build; a same-image switch (`same_image`: a reload of this very
    /// build) installs nothing, whatever is staged. A switch that then stops says
    /// so ([`Self::record_update_switch_stopped`]).
    ///
    /// NOISE BOUND: every attempt a person makes is on record; the automatic
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
        if automatic && self.update_switch_recorded_for == Some(target_build) {
            return;
        }
        self.update_switch_recorded_for = Some(target_build);
        self.update_switch_on_record = Some(target.clone());
        self.record_message(update_words::switch_started(
            target.as_deref(),
            crate::build_info::version_display(),
            build,
        ));
        self.flush_messages_log();
    }

    /// THE SWITCH THAT DID NOT HAPPEN, on record ([`update_words::switch_stopped`]):
    /// an attempt [`Self::record_update_switch_started`] wrote down ended with
    /// this process still running — refused, failed, or stood down because the
    /// terminal was in use (`routine`) — so the record never leaves an
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
/// Windows or Wayland (vendor/winit, the platform notes above `Occluded`),
/// and X11 reports it only from `VisibilityNotify` — a window fully covered,
/// not one iconified (unmapped) — so outside macOS a MINIMIZED window is
/// asked directly (review 2026-09-24: it kept its 1 Hz text ticks, and its
/// frames while focused or recorded). Wayland answers `None` (it cannot
/// say); macOS reports minimizing as occlusion.
pub(crate) fn band_on_screen(ws: &crate::WindowState) -> bool {
    #[cfg(test)]
    if ws.band_on_screen_for_test {
        return !ws.occluded;
    }
    // macOS reports minimizing as occlusion, and asking AppKit on every loop
    // turn would buy nothing there.
    let minimized = |w: &winit::window::Window| {
        cfg!(not(target_os = "macos")) && w.is_minimized() == Some(true)
    };
    ws.os_window.as_deref().is_some_and(|w| !minimized(w)) && !ws.occluded
}

/// A deadline belongs to the last prepared frame, its time-free layout and
/// the message inputs behind its estimator. The GUI reads this on both the
/// event sweep and the park; neither read should rescan future cell surfaces
/// while the frame and its sources have not changed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BandMotionDeadlineMemo {
    layout_fp: u64,
    motion_input_epoch: u64,
    cols: usize,
    look: aterm_messages::Look,
    frame_at: Instant,
    next: Option<Instant>,
}

impl App {
    /// How window `wid`'s band draws motion (design ruling 140, the gating
    /// UNION of the two sides): MOVING while its effect policy animates
    /// `BandProgress` (focused or recorded, no Reduce Motion, no
    /// `motion = "reduced"`, no load-shed latch), Serious Mode allows it, the
    /// band is on screen, and no handoff freezes the band
    /// ([`Self::message_holds_frozen`]: the screen is parked, and the rows are
    /// the committed ones — main's busy-frame rule); STILL otherwise. GRADED
    /// unless an OS High Contrast palette owns the chrome.
    pub(crate) fn band_look(&self, wid: WindowId) -> aterm_messages::Look {
        use crate::motion::{MotionEffect, SeriousEffect};
        let (focused, on_screen) = self
            .windows
            .get(&wid)
            .map_or((false, false), |ws| (ws.focused, band_on_screen(ws)));
        let effect = MotionEffect::BandProgress;
        let moving = on_screen
            && !self.message_holds_frozen()
            && self
                .effect_policy(effect, self.motion_focus(wid, focused))
                .animate(effect)
            && self
                .serious_mode_policy()
                .allows(SeriousEffect::BandProgress);
        aterm_messages::Look {
            pace: if moving {
                aterm_messages::Pace::Moving
            } else {
                aterm_messages::Pace::Still
            },
            graded: crate::chrome_band::forced_chrome().is_none(),
        }
    }

    /// Window `wid`'s band layout at its width, from its cache when the
    /// center has not moved since; `None` with no committed row.
    fn band_layout_now(
        &self,
        wid: WindowId,
    ) -> Option<std::borrow::Cow<'_, aterm_messages::Presentation>> {
        let ws = self.windows.get(&wid)?;
        let cols = usize::from(ws.cols);
        let fp = self.messages.fingerprint(cols);
        if fp == 0 {
            return None;
        }
        match &ws.band_layout {
            Some((f, c, p)) if *f == fp && *c == cols => Some(std::borrow::Cow::Borrowed(p)),
            _ => Some(std::borrow::Cow::Owned(self.band_presentation(cols))),
        }
    }

    fn compute_band_motion_deadline(
        &self,
        wid: WindowId,
        p: &aterm_messages::Presentation,
        from: Instant,
        look: aterm_messages::Look,
    ) -> Option<Instant> {
        #[cfg(not(test))]
        let _ = wid;
        #[cfg(test)]
        if let Some(ws) = self.windows.get(&wid) {
            ws.band_motion_deadline_computations
                .set(ws.band_motion_deadline_computations.get() + 1);
            ws.band_motion_deadline_last_from.set(Some(from));
        }
        self.messages.motion_deadline(p, from, look)
    }

    /// Find the next visible change from the frame on glass. Its costly bar
    /// scan is memoized only when that frame and its look were prepared; a
    /// newly posted row without a frame still takes the ordinary wake path.
    fn band_motion_next_for(
        &self,
        wid: WindowId,
        now: Instant,
        look: aterm_messages::Look,
    ) -> Option<Instant> {
        let ws = self.windows.get(&wid)?;
        let cols = usize::from(ws.cols);
        let layout_fp = self.messages.fingerprint(cols);
        if layout_fp == 0 {
            return None;
        }
        let motion_input_epoch = self.messages.motion_input_epoch();
        let prepared = ws.band_motion.is_some()
            && ws.band_motion_look == Some(look)
            && matches!(&ws.band_layout, Some((f, c, _)) if *f == layout_fp && *c == cols);
        // A frame in another look or at another layout cannot be the time
        // origin of this view's next change. Start the unprepared query at
        // the current event/park instant, as the uncached host did.
        let frame_at = if prepared {
            ws.band_motion.as_ref().map_or(now, |m| m.at)
        } else {
            now
        };
        if prepared
            && let Some(memo) = ws.band_motion_next.get()
            && memo.layout_fp == layout_fp
            && memo.motion_input_epoch == motion_input_epoch
            && memo.cols == cols
            && memo.look == look
            && memo.frame_at == frame_at
        {
            return memo.next;
        }
        let p = self.band_layout_now(wid)?;
        let next = self.compute_band_motion_deadline(wid, &p, frame_at, look);
        if prepared {
            ws.band_motion_next.set(Some(BandMotionDeadlineMemo {
                layout_fp,
                motion_input_epoch,
                cols,
                look,
                frame_at,
                next,
            }));
        }
        next
    }

    /// The next instant any ON-SCREEN window's band needs a motion frame (or a
    /// time word's tick) — the loop's `DeadlineOwner::MessageBandMotion`, the
    /// band's ONLY clock (ruling 140; main's 125 ms busy frame retired into
    /// it). `None` with no committed row, or none on screen, or nothing live
    /// or echoing on the glass: an idle band arms nothing. `None` too while a
    /// handoff freezes the band: the frame on glass STAYS where it is — the
    /// screen is parked, and not even a move to the still form repaints it
    /// (main's freeze rule) — so the loop never arms a wake it would not act
    /// on.
    pub(crate) fn band_motion_deadline(&self, now: Instant) -> Option<Instant> {
        if self.message_band_rows == 0 || self.message_holds_frozen() {
            return None;
        }
        self.windows
            .iter()
            .filter(|(_, ws)| band_on_screen(ws))
            .filter_map(|(wid, _)| {
                let look = self.band_look(*wid);
                let next = self.band_motion_next_for(*wid, now, look)?;
                if next > now {
                    return Some(next);
                }
                // A due frame may still be queued in winit. Never re-arm its
                // already-past instant; the next grid instant is the park's
                // safe deadline until the queued redraw prepares a new frame.
                let p = self.band_layout_now(*wid)?;
                self.compute_band_motion_deadline(*wid, &p, now, look)
            })
            .min()
    }

    /// The on-screen windows whose band is due a new motion frame at `now`:
    /// the deadline read at the frame each last presented has passed (or none
    /// was prepared yet while something moves), or the frame on glass was
    /// drawn in ANOTHER look — focus, Reduce Motion, Serious Mode, the
    /// load-shed latch or a recording changed it — so the move to (or from)
    /// the still form never depends on each of those triggers asking for its
    /// own redraw. Nothing while a handoff freezes the band (the frame stays,
    /// [`Self::band_motion_deadline`]).
    pub(crate) fn band_motion_due(&self, now: Instant) -> Vec<WindowId> {
        if self.message_band_rows == 0 || self.message_holds_frozen() {
            return Vec::new();
        }
        self.windows
            .iter()
            .filter(|(_, ws)| band_on_screen(ws))
            .filter_map(|(wid, ws)| {
                let look = self.band_look(*wid);
                if ws.band_motion.is_some() && ws.band_motion_look != Some(look) {
                    return (self.messages.fingerprint(usize::from(ws.cols)) != 0).then_some(*wid);
                }
                let due = self
                    .band_motion_next_for(*wid, now, look)
                    .is_some_and(|d| d <= now || ws.band_motion.is_none());
                due.then_some(*wid)
            })
            .collect()
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
    /// pure function of the center's state and that instant.
    pub(crate) fn prepare_band_motion_with(
        &mut self,
        wid: WindowId,
        now: Instant,
        look: aterm_messages::Look,
    ) -> u64 {
        let Some(cols) = self.windows.get(&wid).map(|ws| usize::from(ws.cols)) else {
            return 0;
        };
        let fp = self.messages.fingerprint(cols);
        if fp == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.band_motion = None;
                ws.band_motion_fp = 0;
                ws.band_motion_look = None;
                ws.band_motion_next.set(None);
            }
            return 0;
        }
        let stale = self
            .windows
            .get(&wid)
            .is_none_or(|ws| !matches!(&ws.band_layout, Some((f, c, _)) if *f == fp && *c == cols));
        let fresh = stale.then(|| self.band_presentation(cols));
        let messages = &self.messages;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        if let Some(p) = fresh {
            ws.band_layout = Some((fp, cols, p));
        }
        let Some((_, _, p)) = &ws.band_layout else {
            return 0;
        };
        let motion = messages.motion(p, now, look);
        let mfp = motion.fingerprint();
        ws.band_motion = Some(motion);
        ws.band_motion_fp = mfp;
        ws.band_motion_look = Some(look);
        // A content-only redraw can prepare the SAME 33 ms band frame again.
        // The memo's frame/source/look key keeps that answer valid, so leave
        // it in place; a genuinely new frame misses that key on the next
        // event/park query and computes its own deadline.
        mfp
    }
}

// ---------------------------------------------------------------------------
// The handoff carry (design §3.8).
// ---------------------------------------------------------------------------

impl App {
    /// The live rows and the undrained log lines for the handoff manifest:
    /// the center's [`Carry`] in the manifest's own plain-data shape.
    pub(crate) fn carried_messages(&self) -> crate::session_store::MessagesCarry {
        crate::session_store::MessagesCarry::from(&self.messages.carried())
    }

    /// THE SUCCESSOR'S FIRST FRAMES, before Commit: re-seed the rows the
    /// outgoing process carried into the center (each keeps its id, words and
    /// hold under the handoff's staleness cap until Commit), queue the lines
    /// the parent never wrote, and — when this process is the handed-off
    /// successor with a carried progress row — say its own phase in that row
    /// ("almost done — what you type is kept", [`update_words::finishing`])
    /// rather than the parent's "installing…", and adopt it as this process's
    /// flow row. A carried health warning leaves with
    /// it: the build that landed is the proof. No sync here: the row count
    /// is the carried one until Commit, and nothing paints yet. `finishing`
    /// is the running build's version.
    pub(crate) fn seed_carried_messages(
        &mut self,
        carry: &crate::session_store::WindowCarry,
        finishing: Option<&str>,
    ) {
        let now = Instant::now();
        let carried: Carry = carry.message_carry();
        if carried.is_empty() {
            return;
        }
        self.messages.seed_carried(&carried, wall_stamp_now(), now);
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
            // at Commit and never learns its warning healed.
            if let Some((health, said)) =
                self.messages
                    .live_by_key(update_words::KEY_HEALTH)
                    .map(|l| {
                        (
                            l.id,
                            (
                                l.msg.title.clone(),
                                l.msg.detail.first().cloned().unwrap_or_default(),
                            ),
                        )
                    })
            {
                self.messages.resolve(health, Outcome::Warn, now);
                self.update_health_said.get_or_insert(said);
            }
        }
    }
}

/// The band's motion on the host side — the idle law, hidden windows, the
/// frame cadence, the per-frame cost, the still looks, the faces that must
/// not move (review 2026-09-24).
#[cfg(test)]
#[path = "band_motion_tests.rs"]
mod band_motion_tests;

/// What a heal of the update-health warning rests on ([`App::heal_update_health`]).
/// Each proves one more half of updating than the last, so each heals only the
/// warnings it answers — the warning's title names the broken half
/// (`aterm_update::health_failing_title`), and the updater's own ledger reads the
/// same way: a whole-pipeline success clears every streak but an install streak.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HealthProof {
    /// The check reached a release and its bytes are arriving (`Downloading`,
    /// `Verifying`): checking for updates works.
    Checked,
    /// A build downloaded, passed its checks and was staged (`Staged`):
    /// downloading works too.
    Downloaded,
    /// The ledger healed (`HEALTH_RECOVERED_TITLE`, sent only once no class is
    /// failing), or a build landed: installing works too.
    Installed,
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
            Self::needed_for(aterm_update::health_failing_title(class))
        }
    }

    /// The proof a warning titled `title` needs before it may leave and its
    /// healing be recorded. A title this build does not know needs the least:
    /// the lane's old rule, "a check that downloads is a check that works".
    fn needed_for(title: &str) -> Self {
        if title == aterm_update::health_failing_title("apply") {
            Self::Installed
        } else if title == aterm_update::health_failing_title("pipeline") {
            Self::Downloaded
        } else {
            Self::Checked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{HOLD_GESTURE, HOLD_SUCCESS, LogState, Meter, Severity, tags};

    fn warn(title: &str) -> Message {
        Message::new(tags::CONFIG, Severity::Warn, title).line("why")
    }

    /// The Settings view fronting window `wid`, if Settings fronts it.
    fn settings_view(
        app: &App,
        wid: WindowId,
    ) -> Option<&crate::native_settings::SettingsViewState> {
        let (_, view) = app.active_native_view(wid)?;
        match app.native_runtime.view_state(view)? {
            crate::native_app::AppViewState::Settings(state) => Some(state),
            _ => None,
        }
    }

    /// The idle center arms nothing (FL-1); a posted row arms its fold; a
    /// handoff freeze suspends the holds — and the deadline agrees with the
    /// settle it arms, both reading the one predicate.
    #[test]
    fn the_deadline_is_none_when_idle_and_the_holds_freeze_under_a_handoff() {
        let mut app = App::headless_for_test();
        assert_eq!(app.messages_deadline(), None, "an idle band never wakes");
        let id = app.post_message(warn("a warning"));
        assert!(app.messages.live(id).is_some());
        assert_eq!(
            app.message_band_rows, 1,
            "a headless App has no window to clamp against: the row commits"
        );
        assert!(app.messages_deadline().is_some(), "the hold is armed");
        app.incoming_handoff_pending = true;
        assert!(app.message_holds_frozen());
        assert_eq!(
            app.messages_deadline(),
            None,
            "a held row's fold is not a wake while the holds are frozen"
        );
        // A LIVE row's staleness cap still wakes the loop mid-freeze.
        let live = app.post_message(
            Message::new(tags::UPDATE, Severity::Info, "downloading").hold(Hold::Live {
                stale_after: std::time::Duration::from_secs(30),
            }),
        );
        assert!(app.messages.live(live).is_some());
        assert!(app.messages_deadline().is_some(), "staleness keeps running");
        app.incoming_handoff_pending = false;
        assert!(!app.message_holds_frozen());
    }

    /// Mid-handoff the row count never moves — the successor's layout must
    /// match the parent's capture — and `sync_messages` still runs the rest.
    #[test]
    fn a_handoff_freeze_settles_only_staleness_and_never_regrids() {
        let mut app = App::headless_for_test();
        app.incoming_handoff_pending = true;
        let id = app.post_message(warn("frozen"));
        assert!(app.messages.live(id).is_some(), "the row is live");
        assert_eq!(app.message_band_rows, 0, "…but no row was committed");
        assert!(
            !app.sync_message_band_rows(Instant::now()),
            "and none will be, mid-handoff"
        );
        app.incoming_handoff_pending = false;
        assert!(
            app.sync_message_band_rows(Instant::now()),
            "the freeze lifted: the row commits"
        );
        assert_eq!(app.message_band_rows, 1);
    }

    /// THE SEAMLESS LANDING NAMES THE REPAINTED TABS (the 2026-09-22/23
    /// update audit, plan P1-5), on main's messages surfaces: the count the
    /// boot took from the adopted set reaches the landing RECORD the Commit
    /// arm writes (ruling 141) — and only the record: one fact is one log
    /// entry, and a no-action row on the glass after the landing is what
    /// ruling 141 made record-only. A launch that adopted nothing counts
    /// nothing.
    #[test]
    fn the_seamless_landing_names_the_tabs_the_successor_repainted() {
        let mut app = App::headless_for_test();
        assert_eq!(
            app.seamless_repainted_tabs(),
            0,
            "a launch that adopted nothing counts nothing"
        );
        app.post_update_landed("0.92.0", 7, app.seamless_repainted_tabs());
        assert!(
            app.messages.live_rows().next().is_none(),
            "an exact landing is a record only"
        );
        app.handoff_repainted_tabs = 2;
        let entries = app.messages.log().records().count();
        app.post_update_landed("0.92.0", 7, app.seamless_repainted_tabs());
        assert!(
            app.messages.live_rows().next().is_none(),
            "a repainted landing is a record only too"
        );
        assert!(
            app.messages
                .log()
                .records()
                .any(|r| r.title == "Updated to aterm v0.92.0"
                    && r.detail.first().map(String::as_str) == Some("2 tabs repainted")),
            "the landing record names the repaint"
        );
        assert_eq!(
            app.messages.log().records().count(),
            entries + 1,
            "one fact, one log entry"
        );
    }

    /// The inbox drains into the center with the stamp each message was
    /// queued under; the drained lines reach the log ring.
    #[test]
    fn the_pre_app_inbox_drains_into_the_center_with_its_own_stamp() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let mut app = App::headless_for_test();
        crate::message_inbox::queue_message(warn("queued before App"));
        let before = wall_stamp_now();
        app.drain_message_inbox();
        let live = app
            .messages
            .live_rows()
            .find(|l| l.msg.title == "queued before App")
            .expect("posted from the inbox");
        assert!(
            live.stamp.unix_ms <= before.unix_ms,
            "stamped at queue time, not drain time"
        );
        assert_eq!(app.messages.log().len(), 1);
        assert_eq!(
            app.messages.log().pending_len(),
            0,
            "drained to the (absent) writer, never left to fill the queue"
        );
        app.drain_message_inbox();
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "an empty inbox posts nothing"
        );
    }

    /// A reload's set REPLACES the config rows: the old family rows are
    /// resolved (the log says so), the new ones posted, in one settle; an
    /// EMPTY set — the fixed-typo reload — takes every `config.` row down and
    /// touches nothing keyed elsewhere.
    #[test]
    fn a_config_reload_resolves_the_old_config_rows_then_posts_the_new() {
        use crate::message_reporters::{ConfigFamily, ConfigWarnings};
        let mut app = App::headless_for_test();
        let mut launch = ConfigWarnings::default();
        launch.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        launch.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\"".into(),
        );
        app.replace_config_messages(launch.into_messages());
        let other = app.post_message(warn("a row keyed elsewhere").key("privacy.fda"));
        assert_eq!(app.messages.live_rows().count(), 3);
        let old_keys = app
            .messages
            .live_by_key("config.keybindings")
            .map(|l| l.id)
            .expect("the keybinding row");
        let old_typo = app
            .messages
            .live_by_key("config.ignored-keys")
            .map(|l| l.id)
            .expect("the typo's row");

        // The reload fixed the chord and left the typo: the chord's row is
        // resolved, and the typo's — the same words — STANDS, untouched: no
        // resolve, no second post, no second announcement (review
        // 2026-09-24).
        let mut reload = ConfigWarnings::default();
        reload.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\"".into(),
        );
        app.replace_config_messages(reload.into_messages());
        assert!(
            app.messages.live(old_keys).is_none(),
            "the old row is resolved"
        );
        assert!(app.messages.live_by_key("config.keybindings").is_none());
        let typo = app
            .messages
            .live_by_key("config.ignored-keys")
            .expect("the typo's row stands");
        assert_eq!(typo.id, old_typo, "the same row, not a new one");
        assert!(typo.msg.detail.iter().any(|l| l.contains("windw_padding")));
        assert!(
            app.messages.live(other).is_some(),
            "other keys are untouched"
        );
        assert_eq!(app.messages.live_rows().count(), 2);

        // The fixed reload: nothing to post, every config row down.
        app.replace_config_messages(Vec::new());
        assert!(app.messages.live_by_key("config.ignored-keys").is_none());
        assert!(app.messages.live(other).is_some());
        assert_eq!(app.messages.live_rows().count(), 1);
        assert_eq!(
            app.messages.wanted_rows(),
            1,
            "the band wants the survivor's row only (the committed count follows after the shrink quiet)"
        );
    }

    /// A REPEAT GOES TO THE LOG (review 2026-09-24). A reload runs on every
    /// Settings patch, Presence toggle and Serious Mode write, so once the
    /// `Unknown key` row has left the glass — its hold ran out, or it was
    /// read — the next unrelated toggle must not put the same words back and
    /// announce them again: they are a RECORD. A CHANGED problem (a new typo)
    /// raises its row; a FIXED one that comes back later is news again.
    #[test]
    fn a_repeated_config_warning_is_a_record_once_its_row_left_the_glass() {
        use crate::message_reporters::{ConfigFamily, ConfigWarnings};
        let typo = |line: u32, key: &str| {
            let mut warns = ConfigWarnings::default();
            warns.push(
                ConfigFamily::IgnoredKeys,
                format!("config line {line}: unknown key \"{key}\""),
            );
            warns.into_messages()
        };
        let key = ConfigFamily::IgnoredKeys.key();
        let mut app = App::headless_for_test();
        app.replace_config_messages(typo(3, "windw_padding"));
        let first = app.messages.live_by_key(key).map(|l| l.id).expect("a row");
        // The person reads it: it folds.
        assert!(app.messages.mark_seen(first, Instant::now()));
        app.sync_messages();
        assert!(app.messages.live_by_key(key).is_none(), "read, and folded");
        let records = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(key))
                .count()
        };
        let before = records(&app);
        // Three unrelated toggles: the same words each time — records only.
        for _ in 0..3 {
            app.replace_config_messages(typo(3, "windw_padding"));
            assert!(
                app.messages.live_by_key(key).is_none(),
                "a repeat never comes back to the glass"
            );
        }
        assert_eq!(records(&app), before + 3, "…and the log keeps each one");
        assert_eq!(
            app.messages.wanted_rows(),
            0,
            "the band wants no row for it"
        );
        // A CHANGED problem is news: its row is raised.
        app.replace_config_messages(typo(5, "colums"));
        let changed = app.messages.live_by_key(key).expect("a new typo's row");
        assert!(changed.msg.detail.iter().any(|d| d.contains("colums")));
        // Fixed, then broken again the same way: the fix resolved the row, so
        // the words coming back are news again.
        app.replace_config_messages(Vec::new());
        assert!(app.messages.live_by_key(key).is_none(), "fixed");
        app.replace_config_messages(typo(5, "colums"));
        let again = app
            .messages
            .live_by_key(key)
            .map(|l| l.id)
            .expect("a problem that came back after its fix is raised again");
        // …and when the row had ALREADY left the glass when the fix landed
        // (read, then fixed, then broken the same way): news again too.
        assert!(app.messages.mark_seen(again, Instant::now()));
        app.sync_messages();
        app.replace_config_messages(typo(5, "colums"));
        assert!(
            app.messages.live_by_key(key).is_none(),
            "a repeat: a record"
        );
        app.replace_config_messages(Vec::new());
        app.replace_config_messages(typo(5, "colums"));
        assert!(
            app.messages.live_by_key(key).is_some(),
            "fixed while off the glass, then broken again: raised"
        );
        // …and its next repeat is a record again.
        let raised = app.messages.live_by_key(key).map(|l| l.id).unwrap();
        assert!(app.messages.mark_seen(raised, Instant::now()));
        app.sync_messages();
        app.replace_config_messages(typo(5, "colums"));
        assert!(app.messages.live_by_key(key).is_none());
    }

    /// The CONTENT-EQUAL reload re-derives only the two text-read families,
    /// so its replace resolves only those (and the launch load failure a
    /// file that now parses has answered): a still-true keybinding row
    /// survives a comment-only save, where the prefix resolve took it down
    /// and logged it `resolved` for a config that was still broken
    /// (ruling 28).
    #[test]
    fn a_content_equal_reload_resolves_only_the_families_it_re_derived() {
        use crate::message_reporters::{
            ConfigFamily, ConfigWarnings, KEY_LAUNCH_LOAD, launch_load_failure,
        };
        let mut app = App::headless_for_test();
        let mut launch = ConfigWarnings::default();
        launch.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        launch.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\"".into(),
        );
        app.replace_config_messages(launch.into_messages());
        let load = app.post_message(launch_load_failure(
            "aterm.toml could not be read at launch (permission denied) \u{2014} every setting \
             is running at its default.",
        ));
        let chord = app
            .messages
            .live_by_key(ConfigFamily::Keybindings.key())
            .map(|l| l.id)
            .expect("the chord's row");
        let typo = app
            .messages
            .live_by_key(ConfigFamily::IgnoredKeys.key())
            .map(|l| l.id)
            .expect("the typo's row");
        let keys = [
            ConfigFamily::IgnoredKeys.key(),
            ConfigFamily::UnacceptedValues.key(),
            KEY_LAUNCH_LOAD,
        ];

        // The typo fixed by a save that parses equal: its row and the load
        // failure come down; the chord's row, not re-derived, stands.
        app.replace_config_messages_keyed(&keys, Vec::new());
        assert!(
            app.messages.live(typo).is_none(),
            "the typo's row is resolved"
        );
        assert!(
            app.messages.live(load).is_none(),
            "a file that loads answers the launch failure"
        );
        assert!(
            app.messages.live(chord).is_some(),
            "the chord's row was not re-derived, so it stands"
        );
        assert_eq!(app.messages.live_rows().count(), 1);
        let rec = app.messages.log().get(typo).expect("logged");
        assert_eq!(
            rec.retired(),
            Some(&aterm_messages::Retired::Resolved(Outcome::Ok)),
            "the log says the typo was fixed: {rec:?}"
        );

        // A new typo on the same path: its family's row is posted, the
        // chord's row still stands, and a key with no live row resolves
        // nothing.
        let mut reload = ConfigWarnings::default();
        reload.push(
            ConfigFamily::IgnoredKeys,
            "config line 5: unknown key \"colums\"".into(),
        );
        app.replace_config_messages_keyed(&keys, reload.into_messages());
        assert!(app.messages.live(chord).is_some());
        assert!(
            app.messages
                .live_by_key(ConfigFamily::IgnoredKeys.key())
                .is_some_and(|l| l.msg.detail.iter().any(|d| d.contains("colums")))
        );
        assert_eq!(app.messages.live_rows().count(), 2);
    }

    /// A RETIRED SPELLING IS A RECORD ON EVERY PATH (design ruling 42): the
    /// launch's and every reload's `collect_key_notices` puts
    /// `[packages] auto_update = false` in `ConfigFamily::RetiredKeys`, and the
    /// replace logs it without a row, so a comment-only save never puts it back
    /// on glass. The typo beside it keeps its row.
    #[test]
    fn a_retired_spelling_reaches_the_log_and_never_the_glass() {
        use crate::message_reporters::{ConfigFamily, ConfigWarnings};
        let mut app = App::headless_for_test();
        let text = "windw_padding = 20\n[packages]\nauto_update = false\n";
        for _ in 0..2 {
            let mut warns = ConfigWarnings::default();
            crate::app_config::collect_key_notices(&mut warns, text);
            app.replace_config_messages_keyed(
                &[
                    ConfigFamily::IgnoredKeys.key(),
                    ConfigFamily::RetiredKeys.key(),
                ],
                warns.into_messages(),
            );
        }
        assert!(
            app.messages
                .live_by_key(ConfigFamily::RetiredKeys.key())
                .is_none(),
            "a record is never a row"
        );
        assert!(
            app.messages
                .live_by_key(ConfigFamily::IgnoredKeys.key())
                .is_some_and(|l| l.msg.detail.iter().any(|d| d.contains("windw_padding"))),
            "the typo keeps its row"
        );
        assert_eq!(app.messages.live_rows().count(), 1);
        assert_eq!(app.messages.wanted_rows(), 1);
        let records = app
            .messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(ConfigFamily::RetiredKeys.key()))
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2, "one record per launch or reload");
        assert!(records.iter().all(|r| !r.is_live()));
        // Design §10.3: the title names the kind in a few words; the key's
        // own sentence is the detail.
        assert_eq!(records[0].title, "Retired config key");
        assert!(
            records[0]
                .detail
                .iter()
                .any(|d| d.contains("auto_update = false is retired")),
            "{:?}",
            records[0].detail
        );
    }

    /// Restate and resolve go through the center and sync; a restatement of a
    /// builder's words carries every field.
    /// THE FINAL READ LEAVES A HELD ROW ALONE (ruling 19). Since the silent
    /// lane the `installed` words are a record, so a HELD row on the pass key
    /// is one carried across a handoff from a build before it — stood in for
    /// here by the record's words at the default hold. The tailer's final
    /// snapshot of the ended pass changes nothing on it, first run or not: no
    /// meter, no words, no revision, no log line.
    #[test]
    fn the_final_snapshot_leaves_a_held_row_alone() {
        let mut app = App::headless_for_test();
        let id = app.post_message(toolchain_words::installed("claude, codex").hold(Hold::Default));
        {
            let live = app.messages.live(id).expect("held on glass");
            assert!(live.msg.hold.is_held());
            assert_eq!(live.msg.meter, None, "no meter to complete");
            assert_eq!(live.revision, 0);
        }
        let file = atpkg::progress::ProgressFile {
            v: atpkg::progress::PROGRESS_VERSION,
            pid: None,
            pass: "net".into(),
            started_unix: 1_700_000_000,
            heartbeat_unix: 1_700_000_100,
            overall: atpkg::progress::Overall {
                programs_done: 10,
                programs_total: 10,
                bytes_done: 1_200_000_000,
                bytes_total: 1_200_000_000,
            },
            queue: Vec::new(),
            programs: std::collections::BTreeMap::new(),
            ended_unix: Some(1_700_000_100),
        };
        for first_run in [false, true] {
            app.apply_toolchain_snapshot(
                Some(&crate::PkgProgressSnapshot {
                    file: file.clone(),
                    running: false,
                }),
                first_run,
            );
            app.apply_toolchain_snapshot(None, first_run);
        }
        let live = app.messages.live(id).expect("still on glass, still held");
        assert_eq!(live.revision, 0, "not restated");
        assert_eq!(live.msg.meter, None, "no meter, no 100%");
        assert_eq!(live.msg.title, "ALab tools installed");
        assert_eq!(live.msg.detail, vec!["claude, codex"]);
        assert_eq!(app.messages.log().len(), 1, "the read posted nothing");
    }

    #[test]
    fn restate_and_resolve_reach_the_center() {
        let mut app = App::headless_for_test();
        let id = app.post_message(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing").hold(Hold::Live {
                stale_after: std::time::Duration::from_secs(30),
            }),
        );
        let words = Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
            .line("trust · extracting")
            .meter(Meter {
                fill_permille: Some(420),
                stats: "1 of 2".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: std::time::Duration::from_secs(30),
            });
        let r = restatement_of(&words);
        assert_eq!(r.title.as_deref(), Some("Installing"));
        assert_eq!(r.detail, Some(vec!["trust · extracting".to_string()]));
        assert_eq!(r.meter, Some(words.meter.clone()));
        assert_eq!(r.actions, Some(Vec::new()));
        assert_eq!(r.severity, Some(Severity::Info));
        assert!(app.restate_message(id, r));
        assert_eq!(app.messages.live(id).unwrap().revision, 1);
        assert_eq!(app.messages.log().len(), 1, "a restate writes no log line");
        assert!(!app.restate_message(MessageId::from_raw(9_999).unwrap(), Restatement::default()));
        assert!(app.resolve_message(id, Outcome::Ok));
        assert!(app.messages.live(id).is_none());
        assert_eq!(
            app.messages.log().get(id).unwrap().state,
            LogState::Retired(Retired::Resolved(Outcome::Ok))
        );
        assert!(!app.resolve_message(id, Outcome::Ok), "already gone");
    }

    /// THE DETAILS PRESS (design §3.6): the row is seen — a held row folds,
    /// the config band's "click at a notice you have read" — and Settings ▸
    /// Messages opens in THE PRESSED WINDOW with that entry selected, the
    /// page already showing the record as folded. A standing row stays and
    /// is selected all the same; the overflow link opens the page unselected.
    #[test]
    fn a_details_press_marks_the_row_seen_and_folds_a_held_one() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let id = app.post_message(warn("read me"));
        assert!(app.perform_intent(wid, id, Intent::Details));
        assert!(
            app.messages.live(id).is_none(),
            "a held row folds once read"
        );
        assert_eq!(
            app.messages.log().get(id).unwrap().state,
            LogState::Retired(Retired::Folded)
        );
        let view = settings_view(&app, wid).expect("Settings fronts the pressed window");
        assert_eq!(view.route, SettingsRoute::Messages);
        assert_eq!(view.messages_selected, Some(id.raw()));
        assert!(
            app.messages_last_publish.is_some(),
            "the page was published on the way in"
        );
        assert_eq!(
            app.messages_published_revision,
            app.messages.revision(),
            "…at the center's revision AFTER the fold, so the entry reads as folded"
        );
        // A STANDING row folds once read too (design §10.4.3 E3, ruling 79).
        let standing = app.post_message(
            Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(Hold::Standing),
        );
        assert!(app.perform_intent(wid, standing, Intent::Details));
        assert!(
            app.messages.live(standing).is_none(),
            "a standing row folds once read"
        );
        assert_eq!(
            app.messages.log().get(standing).unwrap().state,
            LogState::Retired(Retired::Folded)
        );
        // A LIVE row stays — work in flight is not read away — and is selected.
        let live = app.post_message(
            Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
                .in_flight()
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                }),
        );
        assert!(app.perform_intent(wid, live, Intent::Details));
        assert!(app.messages.live(live).expect("a live row stays").seen);
        assert_eq!(
            settings_view(&app, wid).unwrap().messages_selected,
            Some(live.raw())
        );
        // The overflow row's link: the page, top of the log, nothing selected.
        assert!(app.open_messages_entry(wid, None));
        let view = settings_view(&app, wid).unwrap();
        assert_eq!(view.route, SettingsRoute::Messages);
        assert_eq!(
            view.messages_selected,
            Some(live.raw()),
            "a plain open leaves the page as it was"
        );
        // A window that does not exist opens nothing.
        assert!(!app.open_messages_entry(WindowId(77), Some(live)));
    }

    /// THE PROJECTION (design §4.2): every record newest first, in the
    /// wire's own state words, the live row's current words, the band row
    /// it is on, tag counts in the chips' order — and `still_actionable`
    /// following the intent and the row's life: a decision only while live,
    /// `Apply now` only while that build is staged, `Open log` only while
    /// the file exists, a navigation always.
    #[test]
    fn the_projection_is_newest_first_in_the_wires_words_with_live_actionability() {
        let mut app = App::headless_for_test();
        let first = app.post_message(
            Message::new(
                tags::CRASH,
                Severity::Error,
                "aterm closed unexpectedly last time",
            )
            .line("crash log at /definitely/not/there.log")
            .action(Intent::OpenPath {
                path: "/definitely/not/there.log".into(),
            }),
        );
        let ask = app.post_message(
            Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                .action(Intent::OpenSystemPane {
                    pane: "full-disk-access".into(),
                })
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::Ask {
                    for_: aterm_messages::HOLD_ASK,
                }),
        );
        let staged = app.post_message(update_words::staged(
            "0.91.0",
            1234,
            Some(update_words::ApplyPosture::ManualByConfig),
        ));
        let meter = app.post_message(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .line("trust \u{00b7} extracting")
                .action(Intent::OpenSettings {
                    route: "/packages".into(),
                })
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_TAILED,
                }),
        );
        assert!(app.messages.restate(
            meter,
            Restatement {
                title: Some("Installing ALab tools".into()),
                detail: Some(vec!["trust \u{00b7} linking".into()]),
                ..Restatement::default()
            },
            Instant::now()
        ));
        assert!(app.resolve_message(first, Outcome::Warn));

        let state = app.messages_state();
        assert_eq!(state.revision, app.messages.revision());
        let ids: Vec<u64> = state.entries.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            [meter.raw(), staged.raw(), ask.raw(), first.raw()],
            "newest first"
        );
        assert_eq!(
            state.tags,
            [
                ("crash".to_string(), 1),
                ("update".to_string(), 1),
                ("toolchain".to_string(), 1),
                ("privacy".to_string(), 1),
            ],
            "the chips' fixed order, present tags only"
        );
        let entry = |id: MessageId| state.entry(id.raw()).expect("in the projection");
        // The live meter: its CURRENT words (the restatement is never logged).
        let m = entry(meter);
        assert_eq!(m.state, "live");
        assert_eq!(m.detail, vec!["trust \u{00b7} linking"]);
        assert_eq!(m.tag, "toolchain");
        assert_eq!(m.severity, "info");
        assert!(m.actions[0].still_actionable, "a navigation is always live");
        assert_eq!(m.actions[0].label, "Packages");
        assert_eq!(m.state_words(), "showing now");
        assert_eq!(m.severity_words(), "Info");
        assert_eq!(tag_words(&m.tag), "ALab tools");
        // The ask: held (a question waits on its patience), both decisions
        // pressable while it is up.
        let a = entry(ask);
        assert_eq!(a.state, "held");
        assert!(a.actions.iter().all(|x| x.still_actionable));
        assert_eq!(
            a.actions.iter().map(|x| x.label).collect::<Vec<_>>(),
            ["Open Settings", "Not now"]
        );
        // The staged row: `Apply now` only while that build is really staged
        // (a test App stages nothing), the page link always.
        let s = entry(staged);
        assert_eq!(s.state, "held");
        let apply = s
            .actions
            .iter()
            .find(|a| a.label == "Install now")
            .expect("a manually applied staged row offers Install now");
        assert!(!apply.still_actionable, "nothing is staged in a test App");
        assert!(
            s.actions
                .iter()
                .filter(|a| a.label != "Install now")
                .all(|a| a.still_actionable),
            "every other capsule is a navigation: {:?}",
            s.actions
        );
        // The crash record, retired: the wire's word, the retired stamp, and
        // an `Open log` whose file is gone.
        let f = entry(first);
        assert_eq!(f.state, "resolved-warn");
        assert!(f.retired_unix_ms.is_some());
        assert!(
            f.state_words().starts_with("cleared with a warning after "),
            "{}",
            f.state_words()
        );
        assert!(!f.actions[0].still_actionable, "no file, no press");
        assert_eq!(f.glyph, '\u{2715}');
        assert_eq!(f.repeats, 1);
        // What the page prints is what the wire prints.
        let rec = app.messages.log().get(first).unwrap();
        assert_eq!(f.state, wire::state_word(rec, None));
        assert_eq!(f.copy_text(), words::copy_text(rec));
        // Every vocabulary word has a chip position.
        for tag in tags::ALL {
            assert!(TAG_ORDER.contains(&tag.as_str()), "{tag}");
        }
        assert_eq!(TAG_ORDER.len(), tags::ALL.len());
        // A decision row answered: its label in the meta words.
        let answered = app.post_message(
            Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::Ask {
                    for_: aterm_messages::HOLD_ASK,
                }),
        );
        assert_eq!(
            app.messages.act(answered, ActionIndex(0), Instant::now()),
            Some(Intent::NotNow {
                decision: Decision::FileAccess
            })
        );
        let state = app.messages_state();
        let ans = state.entry(answered.raw()).unwrap();
        assert_eq!(ans.state, "answered");
        assert_eq!(ans.state_words(), "answered: Not now");
        assert!(
            !ans.actions[0].still_actionable,
            "a decision is answered once"
        );
        assert_eq!(span_words(40_000), "40 s");
        assert_eq!(span_words(150_000), "2 min");
        assert_eq!(span_words(7_200_000), "2 h");
        assert_eq!(span_words(200_000_000), "2 d");
    }

    /// THE PAGE'S PRESS (`perform_message_act`): a live row's button goes
    /// through the engine's `act` — logged, a `Not now` retiring its row —
    /// then the intent is performed in the pressing window and worded; a
    /// retired record re-offers its navigation from the log; a decision
    /// cannot be answered once its row is gone; an unknown id or button is
    /// `Gone`.
    #[test]
    fn the_pages_press_acts_through_the_engine_and_words_the_outcome() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let ask = app.post_message(
            Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::Ask {
                    for_: aterm_messages::HOLD_ASK,
                }),
        );
        assert_eq!(
            app.perform_message_act(wid, ask.raw(), 0),
            MessageActOutcome::Performed {
                feedback: "Not now recorded".to_string()
            }
        );
        assert!(
            app.messages.live(ask).is_none(),
            "the answer retired the row"
        );
        assert_eq!(
            app.messages
                .log()
                .get(ask)
                .unwrap()
                .last_action
                .as_ref()
                .map(|(l, _)| l.as_str()),
            Some("Not now"),
            "the press is on record"
        );
        assert_eq!(
            app.perform_message_act(wid, ask.raw(), 0),
            MessageActOutcome::Gone,
            "a decision cannot be answered twice"
        );
        // A retired navigation, re-offered from the log.
        let nav = app.post_message(
            Message::new(tags::TOOLCHAIN, Severity::Success, "ALab tools installed").action(
                Intent::OpenSettings {
                    route: "/packages".into(),
                },
            ),
        );
        assert!(app.resolve_message(nav, Outcome::Ok));
        assert_eq!(
            app.perform_message_act(wid, nav.raw(), 0),
            MessageActOutcome::Performed {
                feedback: "Opened Settings \u{25b8} Packages".to_string()
            }
        );
        assert_eq!(
            settings_view(&app, wid).map(|v| v.route),
            Some(SettingsRoute::Packages)
        );
        assert_eq!(
            app.perform_message_act(wid, nav.raw(), 1),
            MessageActOutcome::Gone
        );
        assert_eq!(app.perform_message_act(wid, 0, 0), MessageActOutcome::Gone);
        assert_eq!(
            app.perform_message_act(wid, 9_999, 0),
            MessageActOutcome::Gone
        );
        // A refusal is worded as one.
        let crash = app.post_message(
            Message::new(
                tags::CRASH,
                Severity::Error,
                "aterm closed unexpectedly last time",
            )
            .action(Intent::OpenPath {
                path: "/definitely/not/there.log".into(),
            }),
        );
        assert_eq!(
            app.perform_message_act(wid, crash.raw(), 0),
            MessageActOutcome::Refused {
                feedback: "Could not open the crash log".to_string()
            }
        );
        assert_eq!(
            act_feedback(&Intent::NewWindow, true),
            "Opened a new window"
        );
        assert_eq!(
            act_feedback(&Intent::ApplyUpdate { build: 1 }, false),
            "Installing the update"
        );
        assert_eq!(
            act_feedback(
                &Intent::OpenSettings {
                    route: "/nowhere".into()
                },
                false
            ),
            "No Settings page at /nowhere"
        );
    }

    /// THE PUBLISH GATE (design §4.2): nothing is published without a
    /// Settings instance; with one, the first sync publishes, the next
    /// within the 2 Hz gap waits (and arms the deadline the park serves),
    /// a quiet center republishes only on the 60 s tick while a view is on
    /// the route, and opening Settings publishes unconditionally.
    #[test]
    fn the_projection_is_published_at_two_hertz_and_on_the_minute_tick() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.post_message(warn("before Settings"));
        assert_eq!(app.messages_last_publish, None, "no Settings, no publish");
        assert_eq!(app.messages_publish_due_at(), None);
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
        let opened = app
            .messages_last_publish
            .expect("opening Settings publishes");
        assert_eq!(app.messages_published_revision, app.messages.revision());
        assert_eq!(app.messages_publish_due_at(), None, "nothing pending");
        // A post inside the gap: the center moved, the publish waits, and
        // the deadline says when.
        app.post_message(warn("inside the gap"));
        assert_eq!(
            app.messages_last_publish,
            Some(opened),
            "within 500 ms of the last publish nothing is republished"
        );
        assert_ne!(app.messages_published_revision, app.messages.revision());
        assert_eq!(
            app.messages_publish_due_at(),
            Some(opened + crate::app_native::MESSAGES_PUBLISH_MIN_GAP)
        );
        assert!(
            app.messages_deadline()
                .is_some_and(|d| d <= opened + crate::app_native::MESSAGES_PUBLISH_MIN_GAP),
            "the loop is asked back for the pending publish"
        );
        // The park, past the gap: published.
        app.publish_native_messages_state_if_due(
            opened + crate::app_native::MESSAGES_PUBLISH_MIN_GAP,
        );
        assert_eq!(app.messages_published_revision, app.messages.revision());
        let second = app.messages_last_publish.unwrap();
        assert_ne!(second, opened);
        // A quiet center off the route: the minute tick publishes nothing.
        app.publish_native_messages_state_if_due(second + crate::app_native::MESSAGES_PUBLISH_TICK);
        assert_eq!(app.messages_last_publish, Some(second));
        // On the route: it does, so the relative times move.
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
        let on_route = app.messages_last_publish.unwrap();
        app.publish_native_messages_state_if_due(
            on_route + crate::app_native::MESSAGES_PUBLISH_TICK - std::time::Duration::from_secs(1),
        );
        assert_eq!(app.messages_last_publish, Some(on_route), "not yet");
        app.publish_native_messages_state_if_due(
            on_route + crate::app_native::MESSAGES_PUBLISH_TICK,
        );
        assert_ne!(app.messages_last_publish, Some(on_route), "the tick");
    }

    /// THE PUBLISH IS RATE-LIMITED, AND ONLY WHILE SETTINGS EXISTS (design
    /// §4.2): with no Settings controller a publish — asked for outright or
    /// on the sweep — is a no-op and nothing is pending; opening Settings
    /// publishes at once; a BURST inside the 2 Hz gap (posts, a resolve) is
    /// held back whole — the page keeps showing the last publish — and ONE
    /// publish at the gap carries all of it, after which nothing of the
    /// center's is pending (only the minute tick, the view being on the
    /// route); closing the last Settings tab drops the controller, after
    /// which the center moving is nobody's business again; reopening
    /// publishes at once, gap or no gap.
    #[test]
    fn publish_is_rate_limited_and_only_while_settings_exists() {
        use crate::app_native::MESSAGES_PUBLISH_MIN_GAP as GAP;
        use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let settings_exists = |app: &App| {
            app.native_runtime
                .instance_by_kind(crate::native_app::AppKind::Settings)
                .is_some()
        };
        // The page's own count of the log — the status line's words — as
        // the last publish left it.
        let status = |app: &App| {
            app.compiled_native_ui(wid)
                .expect("Settings fronts the window")
                .semantic(&crate::native_ui::UiKey::new("settings/messages/status"))
                .map(|node| node.label.clone())
                .expect("the Messages page is up")
        };
        app.post_message(warn("before Settings"));
        // No controller: nothing to publish into, and nothing pending.
        app.publish_native_messages_state();
        app.publish_native_messages_state_if_due(std::time::Instant::now() + GAP);
        assert!(!settings_exists(&app));
        assert_eq!(app.messages_last_publish, None, "no Settings, no publish");
        assert_eq!(app.messages_published_revision, 0);
        assert_eq!(app.messages_publish_due_at(), None);

        // Settings opens, at the Messages route: published at once.
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
        assert!(settings_exists(&app));
        let opened = app
            .messages_last_publish
            .expect("opening Settings publishes");
        let published = app.messages_published_revision;
        assert_eq!(published, app.messages.revision());
        assert!(status(&app).starts_with("1 message"), "{}", status(&app));

        // A burst inside the gap — four posts and a resolve — is held back
        // whole: the page keeps the last publish, the pending one is due at
        // the gap, and the loop is asked back for it.
        let ids: Vec<MessageId> = (0..4)
            .map(|i| app.post_message(warn(&format!("burst {i}"))))
            .collect();
        assert!(app.resolve_message(ids[0], Outcome::Ok));
        assert_eq!(
            app.messages_last_publish,
            Some(opened),
            "five moves inside 500 ms: not one republish"
        );
        assert_eq!(app.messages_published_revision, published);
        assert!(app.messages.revision() > published);
        assert!(
            status(&app).starts_with("1 message"),
            "the page shows the last publish: {}",
            status(&app)
        );
        assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
        assert!(
            app.messages_deadline().is_some_and(|d| d <= opened + GAP),
            "the loop is asked back for the pending publish"
        );
        app.publish_native_messages_state_if_due(
            opened + GAP - std::time::Duration::from_millis(1),
        );
        assert_eq!(
            app.messages_last_publish,
            Some(opened),
            "one millisecond short of the gap: still held"
        );
        app.publish_native_messages_state_if_due(opened + GAP);
        let second = app.messages_last_publish.expect("published at the gap");
        assert_ne!(second, opened);
        assert_eq!(
            app.messages_published_revision,
            app.messages.revision(),
            "one publish carries the whole burst"
        );
        assert_eq!(
            app.messages_publish_due_at(),
            Some(second + TICK),
            "nothing of the center's pending: only the minute tick, the view on the route"
        );
        assert!(
            status(&app).starts_with("5 messages"),
            "…and the page shows all of it: {}",
            status(&app)
        );

        // Closing the last Settings tab drops the controller: the center
        // moving is nobody's business — no publish, nothing pending.
        assert!(app.close_settings_tabs());
        assert!(!settings_exists(&app), "no view, no controller");
        app.post_message(warn("after close"));
        assert_ne!(app.messages.revision(), app.messages_published_revision);
        assert_eq!(
            app.messages_publish_due_at(),
            None,
            "nothing is pending without a page to publish into"
        );
        app.publish_native_messages_state();
        app.publish_native_messages_state_if_due(second + GAP + GAP);
        assert_eq!(app.messages_last_publish, Some(second), "no publish");
        assert_ne!(app.messages.revision(), app.messages_published_revision);

        // Reopened: published at once, whatever the gap would say.
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
        assert_ne!(app.messages_last_publish, Some(second));
        assert_eq!(app.messages_published_revision, app.messages.revision());
        assert!(status(&app).starts_with("6 messages"), "{}", status(&app));
    }

    /// REVIEW PIN (Phase 2, lens B, 2026-09-22): A 10 Hz RESTATE STORM
    /// PUBLISHES AT 2 Hz AT MOST (design §4.2) — the path a downloading
    /// update takes: `restate_message` every 100 ms for three seconds,
    /// each followed by the park's own gate
    /// (`publish_native_messages_state_if_due`), the park's clock stepped
    /// in lock-step with the real instant each publish stamps, a view on
    /// the route. Thirty restatements of the title: six publishes at most
    /// (2 Hz × 3 s), at least five (a moving center is never starved), the
    /// page two restatements behind inside a gap, and the last publish
    /// carrying the last words.
    #[test]
    fn a_ten_hertz_restate_storm_publishes_at_two_hertz_at_most() {
        use crate::app_native::MESSAGES_PUBLISH_MIN_GAP as GAP;
        let mut app = App::headless_for_test();
        // A writer, as a real App has one: without it the page heads its list
        // with the not-saved note, and the row would start the second section.
        let dir = std::env::temp_dir().join(format!("aterm-storm-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        app.messages_log = crate::messages_store::Writer::spawn(&dir.join("messages.log"));
        let wid = WindowId(0);
        let meter = app.post_message(
            Message::new(tags::UPDATE, Severity::Info, "aterm update v0.48.0")
                .line("0%")
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                }),
        );
        // The row's title as the page shows it — on the first compact
        // section whatever the window's width.
        let title = |app: &App| {
            app.compiled_native_ui(wid)
                .expect("Settings fronts the window")
                .semantic(&crate::native_ui::UiKey::new(format!(
                    "settings/messages/row/{}/title",
                    meter.raw()
                )))
                .map(|node| node.label.clone())
                .expect("the entry is on the page")
        };
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
        let opened = app
            .messages_last_publish
            .expect("opening Settings publishes");
        assert_eq!(title(&app), "aterm update v0.48.0");
        // The park's clock: virtual time from the opening publish, re-based
        // on the real instant each publish stamps, so the gate sees the
        // spacing the loop would.
        let step = std::time::Duration::from_millis(100);
        let mut base_real = opened;
        let mut base_virtual = std::time::Duration::ZERO;
        let mut publishes = 0usize;
        for k in 1..=30u32 {
            let before = app.messages_last_publish;
            let revision = app.messages.revision();
            assert!(app.restate_message(
                meter,
                Restatement {
                    title: Some(format!("aterm update v0.48.0 \u{00b7} {k}%")),
                    ..Restatement::default()
                },
            ));
            assert!(
                app.messages.revision() > revision,
                "a restatement moves the center"
            );
            let elapsed = step * k;
            let now = base_real + (elapsed - base_virtual);
            app.publish_native_messages_state_if_due(now);
            if app.messages_last_publish != before {
                publishes += 1;
                base_real = app.messages_last_publish.expect("published");
                base_virtual = elapsed;
                assert_eq!(
                    app.messages_published_revision,
                    app.messages.revision(),
                    "tick {k}: a publish carries everything so far"
                );
            }
            if k == 7 {
                assert_eq!(
                    title(&app),
                    "aterm update v0.48.0 \u{00b7} 5%",
                    "inside the gap the page keeps the last publish"
                );
                assert_eq!(app.messages_publish_due_at(), Some(base_real + GAP));
            }
        }
        assert!(
            (5..=6).contains(&publishes),
            "thirty restatements at 10 Hz: {publishes} publishes (2 Hz at most)"
        );
        assert_eq!(app.messages_published_revision, app.messages.revision());
        assert_eq!(
            title(&app),
            "aterm update v0.48.0 \u{00b7} 30%",
            "the last publish carries the last words"
        );
    }

    /// REVIEW PIN (Phase 2, lens B, 2026-09-22): THE MINUTE TICK IS A WAKE
    /// THE LOOP MUST BE ASKED BACK FOR. Design §4.2: the projection
    /// republishes once per 60 s while any Settings view is on the Messages
    /// route so the relative times tick. `publish_native_messages_state_if_due`
    /// honours the tick when the loop happens to run, but `messages_deadline`
    /// folds only the 2 Hz hold-back (`messages_publish_due_at` is `None`
    /// while the center is quiet), so an idle window on the page — no key,
    /// no byte, no pointer — is never woken for it and `3 min ago` stays
    /// `3 min ago`. With a standing row (the center arms nothing of its
    /// own) and a view on the route, the deadline must be the tick.
    #[test]
    fn the_minute_tick_arms_a_wake_while_a_view_is_on_the_route() {
        use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.post_message(
            Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(Hold::Standing),
        );
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
        let published = app
            .messages_last_publish
            .expect("opening Settings publishes");
        assert_eq!(app.messages_published_revision, app.messages.revision());
        assert_eq!(
            settings_view(&app, wid).map(|v| v.route),
            Some(SettingsRoute::Messages)
        );
        assert_eq!(
            app.messages.deadline(true),
            None,
            "a standing row arms no fold: the center is quiet"
        );
        // The park at the tick publishes — this half holds.
        let mut probe = App::headless_for_test();
        probe.post_message(
            Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(Hold::Standing),
        );
        assert!(probe.open_settings_tab_in_window(wid, SettingsRoute::Messages));
        let probe_published = probe.messages_last_publish.unwrap();
        probe.publish_native_messages_state_if_due(probe_published + TICK);
        assert_ne!(
            probe.messages_last_publish,
            Some(probe_published),
            "the park ticks"
        );
        // …but nothing asks the loop to park there.
        assert_eq!(
            app.messages_deadline(),
            Some(published + TICK),
            "the tick must be armed as a wake while a view is on the route"
        );
    }

    /// The route words after *Open Settings* are the consent card's caption,
    /// TERSE (ruling 46): the title before its dash, the route as
    /// `detail[0]`, each further clause a line behind Details, the actions
    /// withdrawn, the route's short hold.
    #[test]
    fn the_route_words_are_the_consent_cards_caption() {
        let route = "Privacy & Security \u{25b8} Full Disk Access";
        let opened = route_words(true, route);
        assert_eq!(
            opened.title.as_deref(),
            Some("Enable aterm in Full Disk Access"),
            "the instruction is the title, not the confirmation"
        );
        assert_eq!(opened.severity, None, "the question's tone");
        assert_eq!(
            opened.detail,
            Some(vec![
                route.to_string(),
                "find aterm there (+ adds a missing app)".to_string(),
                "if already enabled, leave it on".to_string(),
                "aterm keeps checking access".to_string(),
            ])
        );
        assert_eq!(opened.actions, Some(Vec::new()));
        assert_eq!(opened.hold, Some(Hold::For(HOLD_ROUTE)));
        assert!(
            HOLD_ROUTE <= std::time::Duration::from_secs(15),
            "a terse restate holds briefly: {HOLD_ROUTE:?}"
        );
        assert_eq!(opened.excerpt, Some(true), "the route is painted");
        let refused = route_words(false, route);
        assert_eq!(
            refused.title.as_deref(),
            Some("Open Privacy & Security \u{25b8} Full Disk Access"),
            "the route is the title, verb first"
        );
        assert_eq!(
            refused.detail,
            Some(vec![
                "Settings did not open".to_string(),
                "find aterm there (+ adds a missing app)".to_string(),
                "if already enabled, leave it on".to_string(),
            ]),
            "the outcome first behind it"
        );
        assert_eq!(refused.excerpt, Some(false), "the title alone on the glass");
        assert_eq!(
            refused.severity,
            Some(aterm_messages::Severity::Warn),
            "Settings not opening is a failure"
        );
        assert_eq!(
            refused.glyph,
            Some(aterm_messages::Severity::Warn.default_glyph())
        );
        assert!(
            refused.title.as_ref().unwrap().chars().count() <= 48,
            "fits at 60 columns beside the short Details form"
        );
        let full = route_words(
            false,
            "System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access",
        );
        assert_eq!(
            full.title.as_deref(),
            Some("Open Privacy & Security \u{25b8} Full Disk Access"),
            "the route does not repeat System Settings"
        );
        assert_eq!(
            privacy_pane("full-disk-access"),
            Some(crate::menu::PrivacyPane::FullDiskAccess)
        );
        assert_eq!(privacy_pane("camera"), None);
    }

    /// `Open log` opens only an absolute, regular file under the log dir —
    /// not a relative path, not a missing file, not a regular file OUTSIDE
    /// the dir, not a `..` that lexically starts under the dir and escapes
    /// it, not the dir itself, and not a SYMLINK planted in the dir (whatever
    /// it points at, inside or out) — and a refusal is a Warn row naming the
    /// path. The validator is pinned against a scratch dir standing in for
    /// the log dir; the real one is asked only for what it must refuse.
    #[test]
    fn open_path_external_refuses_paths_outside_the_log_dir_and_symlinks() {
        assert!(!open_path_external("relative/crash.log"));
        assert!(!open_path_external("/definitely/not/a/file"));
        let root = std::env::temp_dir().join(format!("aterm-open-path-{}", std::process::id()));
        let logs = root.join("logs");
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let artifact = logs.join("crash-signal-1-1.log.seen");
        std::fs::write(&artifact, "fatal signal 11").unwrap();
        let outside = elsewhere.join("crash-signal-1-1.log.seen");
        std::fs::write(&outside, "x").unwrap();
        let admits = |p: &Path| admissible_log_path(&p.to_string_lossy(), &logs);
        assert_eq!(
            admits(&artifact),
            Some(artifact.clone()),
            "the artifact the crash reporter minted"
        );
        assert_eq!(
            admits(&outside),
            None,
            "a regular file OUTSIDE the log dir is refused"
        );
        let escape = logs
            .join("..")
            .join("elsewhere")
            .join("crash-signal-1-1.log.seen");
        assert!(
            escape.starts_with(&logs) && std::fs::metadata(&escape).is_ok_and(|m| m.is_file()),
            "the escape starts under the dir lexically and resolves to a regular file\u{2026}"
        );
        assert_eq!(admits(&escape), None, "\u{2026}and is refused for its `..`");
        assert_eq!(admits(&logs), None, "the dir itself");
        assert_eq!(admits(&logs.join("missing.log")), None, "a missing file");
        assert_eq!(
            admissible_log_path("crash-signal-1-1.log.seen", &logs),
            None,
            "a relative path"
        );
        #[cfg(unix)]
        {
            let link = logs.join("crash-signal-2-2.log.seen");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(
                std::fs::metadata(&link).is_ok_and(|m| m.is_file()),
                "the link resolves to a regular file\u{2026}"
            );
            assert_eq!(
                admits(&link),
                None,
                "\u{2026}and is refused for being a link"
            );
            let inner = logs.join("crash-signal-3-3.log.seen");
            std::os::unix::fs::symlink(&artifact, &inner).unwrap();
            assert_eq!(
                admits(&inner),
                None,
                "a link to a file INSIDE the dir is still a link"
            );
        }
        assert!(
            !open_path_external(&outside.to_string_lossy()),
            "through the real log dir: a regular file outside it is refused"
        );
        let _ = std::fs::remove_dir_all(&root);
        let refusal = log_did_not_open("/x/y.log");
        assert_eq!(refusal.title, "Log did not open");
        assert_eq!(refusal.detail, vec!["/x/y.log"]);
        assert_eq!(refusal.tag, tags::SYSTEM);
        assert_eq!(
            refusal.hold,
            Hold::For(HOLD_GESTURE),
            "the person's own press"
        );
        let mut app = App::headless_for_test();
        app.perform_intent(
            WindowId(0),
            MessageId::from_raw(1).unwrap(),
            Intent::OpenPath {
                path: "/definitely/not/a/file".into(),
            },
        );
        assert!(
            app.messages
                .live_rows()
                .any(|l| l.msg.title == "Log did not open"),
            "the refusal is a row"
        );
    }

    /// A classified `progress.json` read of a "net" pass started at `started`:
    /// running mid-extract, or over (ended a minute later) — the tailer's reads
    /// the silent-lane tests drive.
    fn net_read(running: bool, started: u64) -> crate::PkgProgressSnapshot {
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "trust".to_string(),
            atpkg::progress::ProgramProgress {
                phase: if running {
                    atpkg::progress::Phase::Extract
                } else {
                    atpkg::progress::Phase::Done
                },
                bytes_done: 120_000_000,
                bytes_total: 900_000_000,
                build: Some(5520),
                bumped: false,
                error: None,
            },
        );
        crate::PkgProgressSnapshot {
            file: atpkg::progress::ProgressFile {
                v: atpkg::progress::PROGRESS_VERSION,
                pid: running.then_some(7),
                pass: "net".to_string(),
                started_unix: started,
                heartbeat_unix: started,
                overall: atpkg::progress::Overall {
                    programs_done: 3,
                    programs_total: 10,
                    bytes_done: 512_000_000,
                    bytes_total: 1_200_000_000,
                },
                queue: vec!["ty".to_string()],
                programs,
                ended_unix: (!running).then_some(started + 60),
            },
            running,
        }
    }

    /// `appstatus` as the socket renders it.
    fn appstatus(app: &App) -> Vec<String> {
        aterm_messages::wire::activity_rows_compat(
            &app.messages,
            Instant::now(),
            &crate::control::pct_encode,
        )
    }

    /// THE `appstatus` GRAMMAR IS THE BARS', THE WORDS ARE THE ROW'S (ruling 116;
    /// review 2026-09-24). The compat face promised byte-identical GRAMMAR:
    /// field order, keys, percent-encoding and the phase/outcome words, the
    /// bars' `activity_rows` format strings (status_bars.rs before 31b679459,
    /// :982/:991) — while a `phase=live` line says what the row says NOW, so
    /// its words follow the owner's rule. Pinned here in full, deliberately:
    /// the first run's live line (`Installing ALab tools`, was `Installing
    /// the ALab toolchain`; stats `3 of 10 programs`, the byte rollup behind
    /// Details) and the download's; every line parses back into the bars'
    /// seven (live) or eight (done) keys in order.
    #[test]
    fn appstatus_keeps_the_bars_grammar_in_the_rows_current_words() {
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "9.9.9".into(),
            bytes_done: 10_000_000,
            bytes_total: 74_000_000,
        });
        let rows = appstatus(&app);
        assert_eq!(
            rows,
            [
                "activity kind=toolchain phase=live progress=36/100 \
                 title=Installing%20ALab%20tools detail=trust%20%C2%B7%20extracting \
                 stats=3%20of%2010%20programs outcome=-",
                "activity kind=update phase=live progress=14/100 \
                 title=Downloading%20aterm%20v9.9.9 detail=downloading \
                 stats=10%20MB%20/%2074%20MB \
                 outcome=-",
            ]
        );
        // A finished activity: the bars' `phase=done` grammar.
        app.record_message(toolchain_words::failed("install failed"));
        let rows = appstatus(&app);
        let done = rows.last().expect("the record's done line");
        assert!(done.starts_with("activity kind=toolchain phase=done progress=- title="));
        assert!(done.contains(" stats= outcome=warn since_ms="), "{done}");
        // Every line, parsed back: the bars' keys, in the bars' order, each
        // value one token (percent-encoded, so no space or newline forges a
        // field), the phase and outcome words the bars' own.
        for row in &rows {
            let mut words = row.split(' ');
            assert_eq!(words.next(), Some("activity"), "{row}");
            let fields: Vec<(&str, &str)> = words
                .map(|w| w.split_once('=').expect("key=value"))
                .collect();
            let keys: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
            let value = |key: &str| fields.iter().find(|(k, _)| *k == key).unwrap().1;
            match value("phase") {
                "live" => {
                    assert_eq!(
                        keys,
                        [
                            "kind", "phase", "progress", "title", "detail", "stats", "outcome"
                        ]
                    );
                    assert_eq!(value("outcome"), "-");
                }
                "done" => {
                    assert_eq!(
                        keys,
                        [
                            "kind", "phase", "progress", "title", "detail", "stats", "outcome",
                            "since_ms"
                        ]
                    );
                    assert_eq!(value("progress"), "-");
                    assert!(["ok", "warn"].contains(&value("outcome")), "{row}");
                }
                other => panic!("phase={other}: {row}"),
            }
            assert!(["toolchain", "update"].contains(&value("kind")), "{row}");
            let progress = value("progress");
            assert!(
                progress == "-"
                    || progress
                        .strip_suffix("/100")
                        .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 100)),
                "{row}"
            );
        }
    }

    /// SILENT BY DEFAULT: A ROUTINE PASS RAISES NO ROW (upstream dbf97ecff's
    /// `a_routine_pass_raises_no_row_and_records_every_outcome`, ported onto
    /// the center; the rulings on the origin/main merge, 2026-09-23). Every row
    /// the toolchain lane used to raise for a pass that updates what is already
    /// installed — its announcement, its live meter, "installed", the
    /// managed-current row, the machine-settings rows, a deferred pass —
    /// re-gridded every window and resized every running TUI. Pinned for the
    /// whole marker set of a routine pass: no row, no re-grid, no deadline —
    /// and every outcome on `appstatus` in the bars' ledger grammar, titled by
    /// what happened (design ruling 67), in the order it arrived. The plan is
    /// LIGHT: a routine pass of at least `HEAVY_PASS_BYTES` is very heavy
    /// system use and raises its row (design §10.6, pinned by
    /// `a_heavy_routine_pass_raises_one_row_after_the_grace_and_a_light_one_none`).
    #[test]
    fn a_routine_pass_raises_no_row_and_records_every_outcome() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        app.announce_toolchain_pass(
            "installing 1 program(s) over the network: ty (about 40 MB)",
            false,
        );
        app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
        app.apply_toolchain_snapshot(Some(&light_read(false, 1_700_000_000)), false);
        app.record_toolchain_outcome(toolchain_words::installed(
            "\u{2713} ALab toolchain installed: ty",
        ));
        app.record_toolchain_outcome(toolchain_words::ended(
            "the pass finished; 12 ALab program(s) are installed",
        ));
        app.post_managed_current(
            "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)",
        );
        app.post_machine_settings(
            "spotlight-noindex 73 dir(s) migrated; universal-control disabled",
        );
        app.record_message(toolchain_words::deferred(
            "an earlier toolchain pass is still running",
        ));
        app.toolchain_pass_ended(true);
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(message_regrids(), regrids, "no re-grid for routine work");
        assert_eq!(app.messages_deadline(), None, "nothing to wake for");
        let enc = crate::control::pct_encode;
        let ledger = [
            ("ALab tools installed", "ty", "ok"),
            (
                "Package update finished",
                "the pass finished; 12 ALab program(s) are installed",
                "ok",
            ),
            (
                "Claude Code 2.1.280 and Codex 0.156.0 are up to date",
                "used in every tab",
                "ok",
            ),
            ("Spotlight: 73 build dirs moved to .noindex", "", "ok"),
            (
                "Universal Control disabled",
                "undo: `aterm pkg machine` prints the revert \u{00b7} \
                 defaults -currentHost delete com.apple.universalcontrol Disable; \
                 defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges",
                "ok",
            ),
            (
                "Package update postponed",
                "an earlier toolchain pass is still running",
                "ok",
            ),
        ];
        let rows = appstatus(&app);
        assert_eq!(rows.len(), ledger.len(), "{rows:#?}");
        for (row, (title, detail, outcome)) in rows.iter().zip(ledger) {
            let want = format!(
                "activity kind=toolchain phase=done progress=- title={} detail={} stats= \
                 outcome={outcome} since_ms=",
                enc(title),
                enc(detail)
            );
            assert!(row.starts_with(&want), "{row}\n  want {want}");
        }
    }

    /// THE ONE ROW: THE FIRST-RUN ROW, THEN NO SUCCESS ROW (upstream
    /// dbf97ecff/ac7942234's `a_first_run_shows_the_bar_then_folds_with_no_success_row`).
    /// The pass that lays the default set onto a machine that had none of it
    /// (multi-GB) keeps its live row — silence there reads as a hang — from the
    /// announcement through the meter; the marker and the tailer's final read,
    /// in either order, fold nothing while the child runs, and its EXIT folds
    /// the row with no "installed" row after it; the roster is the record's.
    /// Two re-grids: one open, one fold.
    #[test]
    fn a_first_run_shows_the_row_then_folds_with_no_success_row() {
        // Marker first, then the final read.
        let mut a = App::headless_for_test();
        let regrids = message_regrids();
        a.announce_toolchain_pass(
            "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)",
            true,
        );
        assert_eq!(
            a.messages.live_rows().count(),
            1,
            "the announcement opens the row"
        );
        a.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        let live = a.messages.live_rows().next().expect("the live row");
        assert_eq!(live.msg.title, "Installing ALab tools");
        assert!(
            live.msg
                .meter
                .as_ref()
                .and_then(|m| m.fill_permille)
                .is_some_and(|f| f > 0 && f < 1000),
            "a live meter"
        );
        a.record_toolchain_outcome(toolchain_words::installed(
            "\u{2713} ALab toolchain installed: ay, trust \u{2014} ready in every aterm tab, this one too",
        ));
        a.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
        assert_eq!(
            a.messages.live_rows().count(),
            1,
            "the child still runs: nothing folds yet"
        );
        a.toolchain_pass_ended(true);
        a.settle_messages(
            Instant::now() + aterm_messages::SHRINK_QUIET + std::time::Duration::from_millis(1),
        );
        assert_eq!(a.messages.live_rows().count(), 0, "its exit folds the row");
        assert_eq!(a.message_band_rows, 0);
        assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
        let rows = appstatus(&a);
        assert_eq!(
            rows.len(),
            1,
            "the roster is on record, and only it: {rows:?}"
        );
        let enc = crate::control::pct_encode;
        let want = format!(
            "activity kind=toolchain phase=done progress=- title={} detail={} stats= \
             outcome=ok since_ms=",
            enc("ALab tools installed"),
            enc("ay, trust \u{2014} ready in every aterm tab, this one too")
        );
        assert!(rows[0].starts_with(&want), "{}\n  want {want}", rows[0]);
        // The final read first, then the marker: the same glass.
        let mut b = App::headless_for_test();
        b.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        assert_eq!(b.messages.live_rows().count(), 1);
        b.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
        b.apply_toolchain_snapshot(None, true);
        assert!(
            appstatus(&b)[0].contains("phase=live"),
            "the read claims no outcome"
        );
        b.record_toolchain_outcome(toolchain_words::ended("the toolchain pass finished"));
        assert_eq!(b.messages.live_rows().count(), 1);
        b.toolchain_pass_ended(true);
        assert_eq!(b.messages.live_rows().count(), 0);
        assert_eq!(appstatus(&b).len(), 1, "the one record");
    }

    /// ONE FIRST-RUN CHILD, ONE OPEN AND ONE FOLD (upstream ac7942234's
    /// `a_first_run_bar_holds_across_the_vendor_sub_pass_and_folds_at_exit`).
    /// A first-run `atpkg update` runs the vendor lane before the ALab set: its
    /// own `net-starting:`, meter, pass end and `net-installed: claude, codex`
    /// come first, then the set's announcement and meter. Folding on the vendor
    /// sub-pass's ending read or its marker reopened the row for the set — two
    /// more re-grids of every window in the one pass meant to show exactly one
    /// row. The row holds across it (a failed vendor sub-pass too) and folds
    /// only at the child's exit.
    #[test]
    fn a_first_run_row_holds_across_the_vendor_sub_pass_and_folds_at_exit() {
        for vendor_failed in [false, true] {
            let mut app = App::headless_for_test();
            let mut shown = Vec::new();
            let note = |app: &App, shown: &mut Vec<usize>| {
                shown.push(app.messages.live_rows().count());
            };
            app.announce_toolchain_pass(
                "installing 2 program(s) over the network: claude, codex (about 300 MB)",
                true,
            );
            note(&app, &mut shown);
            app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
            note(&app, &mut shown);
            app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
            note(&app, &mut shown);
            app.record_toolchain_outcome(if vendor_failed {
                toolchain_words::failed("network provisioning installed nothing")
            } else {
                toolchain_words::installed("claude, codex")
            });
            note(&app, &mut shown);
            app.announce_toolchain_pass(
                "installing 10 ALab program(s) over the network (about 3 GB on disk)",
                true,
            );
            note(&app, &mut shown);
            app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), true);
            note(&app, &mut shown);
            app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_100)), true);
            note(&app, &mut shown);
            app.record_toolchain_outcome(toolchain_words::installed("ay, trust"));
            note(&app, &mut shown);
            app.toolchain_pass_ended(true);
            note(&app, &mut shown);
            let changes = shown.windows(2).filter(|w| w[0] != w[1]).count();
            assert_eq!(
                (shown.first(), shown.last(), changes),
                (Some(&1), Some(&0), 1),
                "vendor failed {vendor_failed}: one open, one fold: {shown:?}"
            );
            assert_eq!(appstatus(&app).len(), 2, "both answers are on record");
        }
    }

    /// A FAILURE TAKES NO ROW EITHER (upstream dbf97ecff's
    /// `a_failure_takes_no_row_and_folds_the_first_run_bar`). Now the
    /// first-run row folds at its child's exit, the sentence is a Warn record,
    /// and the failure's surface is the Settings ▸ Packages badge — so no meter
    /// can be fused with a verdict at all, in either order.
    #[test]
    fn a_failure_takes_no_row_and_folds_the_first_run_row() {
        let mut failed_read = net_read(false, 1_700_000_000);
        if let Some(trust) = failed_read.file.programs.get_mut("trust") {
            trust.phase = atpkg::progress::Phase::Failed;
            trust.error = Some("no space left on device".into());
        }
        let mut a = App::headless_for_test();
        a.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        a.record_toolchain_outcome(toolchain_words::failed(
            "partly installed \u{2014} see Settings \u{25b8} Packages",
        ));
        a.apply_toolchain_snapshot(Some(&failed_read), true);
        let live = a.messages.live_rows().next().expect("the child still runs");
        assert!(
            matches!(live.msg.hold, Hold::Live { .. }) && live.msg.severity == Severity::Info,
            "the live row, never a verdict on glass: {:?}",
            live.msg
        );
        a.toolchain_pass_ended(true);
        assert_eq!(a.messages.live_rows().count(), 0);
        let rows = appstatus(&a);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].contains("outcome=warn") && rows[0].contains("partly%20installed"),
            "{}",
            rows[0]
        );
        let mut b = App::headless_for_test();
        b.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        b.apply_toolchain_snapshot(Some(&failed_read), true);
        b.record_toolchain_outcome(toolchain_words::failed(
            "partly installed \u{2014} see Settings \u{25b8} Packages",
        ));
        b.toolchain_pass_ended(true);
        assert_eq!(
            b.messages.live_rows().count(),
            0,
            "the failed read raises nothing"
        );
    }

    /// `appstatus` against the bars' ledger on the withdrawals (review
    /// 2026-09-22, re-cut for the silent lane and the latch of design §10.5
    /// H8). A LATCHED row — a first run's — is folded by its child's exit
    /// and by nothing else: neither its own vanished read nor the lane's
    /// untagged clear at the waited child's exit. The fold WITHDRAWS it — the
    /// log keeps the row, the face lists no finished activity and claims no
    /// `ok` for a pass whose marker never came (ruling 13) — while the glass
    /// ends its indicator by how the child exited. An announcement over a
    /// live row supersedes it — one live row, nothing done.
    #[test]
    fn appstatus_lists_no_outcome_for_a_withdrawn_row() {
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
        let row = app
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| l.id)
            .expect("the first-run row");
        app.apply_toolchain_snapshot(None, true);
        assert!(
            app.messages.live(row).is_some(),
            "its exit folds it, not the read"
        );
        // The lane's own clear at a waited child's exit is untagged: a
        // latched row is left for the exit.
        app.apply_toolchain_snapshot(None, false);
        assert!(
            app.messages.live(row).is_some(),
            "latched: the exit's to fold"
        );
        app.toolchain_pass_ended(true);
        assert!(app.messages.live(row).is_none(), "withdrawn");
        assert_eq!(
            app.messages.log().get(row).unwrap().state,
            LogState::Retired(Retired::Withdrawn)
        );
        assert!(
            appstatus(&app).is_empty(),
            "no live row, and no finished activity claimed: {:?}",
            appstatus(&app)
        );
        // A LIVE row under the next first run's announcement is superseded,
        // not finished.
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass("installing 2 ALab program(s)", true);
        app.announce_toolchain_pass("installing 3 ALab program(s)", true);
        let rows = appstatus(&app);
        assert_eq!(rows.len(), 1, "one live row, nothing done: {rows:?}");
        assert!(
            rows[0].starts_with(
                "activity kind=toolchain phase=live progress=- title=Installing%20ALab%20tools"
            ),
            "{}",
            rows[0]
        );
    }

    /// A "net" read with no plan at all — atpkg begins its pass before the
    /// signed index resolves, and the common outcome is a plan of nothing.
    /// Running, or over (its writer gone, an end stamp written).
    fn empty_read(running: bool) -> crate::PkgProgressSnapshot {
        let mut read = net_read(running, 1_700_000_000);
        read.file.overall = atpkg::progress::Overall::default();
        read.file.queue.clear();
        read.file.programs.clear();
        if !running {
            read.file.ended_unix = Some(1);
        }
        read
    }

    /// THE FIRST-RUN ANNOUNCEMENT OPENS THE ROW BEFORE ANY SNAPSHOT (upstream
    /// dbf97ecff's `a_first_run_announcement_opens_the_toolchain_bar_before_any_snapshot`,
    /// ported onto the App). The row goes up at the marker — before
    /// `progress.json` exists — with atpkg's size, no meter, and live under
    /// the announcement's cap rather than a fold. A pass that plans nothing
    /// never opens a row of its own, and an announced one that ends empty
    /// folds at its child's exit, claiming nothing; a ROUTINE pass's
    /// announcement and reads — the every-6-hours no-op — stay invisible.
    #[test]
    fn a_first_run_announcement_opens_the_row_before_any_snapshot() {
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass(
            "installing 10 ALab program(s) from the bundled registry (about 3 GB on disk when finished)",
            true,
        );
        assert_eq!(app.messages.live_rows().count(), 1);
        assert_eq!(app.message_band_rows, 1, "on glass at once");
        {
            let live = app
                .messages
                .live_by_key(toolchain_words::KEY_PASS)
                .expect("the announcement opens the row");
            assert_eq!(live.msg.tag, tags::TOOLCHAIN);
            assert_eq!(live.msg.title, "Installing ALab tools");
            assert!(
                live.msg.detail[0].contains("about 3 GB on disk when finished"),
                "{:?}",
                live.msg.detail
            );
            let meter = live
                .msg
                .meter
                .as_ref()
                .expect("busy before the file exists");
            assert!(
                meter.busy && meter.fill_permille.is_none(),
                "no fill before the file exists — the row moves instead: {meter:?}"
            );
            assert_eq!(meter.stats, "10 programs \u{00b7} ~3 GB", "how big");
            assert_eq!(
                live.msg.hold,
                Hold::Live {
                    stale_after: aterm_messages::STALE_ANNOUNCE
                },
                "live: no fold…"
            );
            assert_eq!(live.fold_at, None);
            assert!(live.stale_at.is_some(), "…but a cap");
        }
        assert!(app.messages_deadline().is_some(), "the cap arms a wake");
        // A pass that plans nothing never opens a row of its own…
        let mut quiet = App::headless_for_test();
        quiet.apply_toolchain_snapshot(Some(&empty_read(true)), true);
        assert_eq!(
            quiet.messages.live_rows().count(),
            0,
            "unplanned running pass: no row"
        );
        assert_eq!(quiet.message_band_rows, 0);
        // …and an announced one that ends empty folds at its child's exit.
        app.apply_toolchain_snapshot(Some(&empty_read(true)), true);
        assert_eq!(
            app.messages
                .live_by_key(toolchain_words::KEY_PASS)
                .map(|l| l.msg.title.as_str()),
            Some("Installing ALab tools"),
            "an announced row keeps its announcement until the plan lands"
        );
        app.apply_toolchain_snapshot(Some(&empty_read(false)), true);
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "the first run's child still runs"
        );
        app.toolchain_pass_ended(true);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "announced, then ended empty: folded at exit"
        );
        assert!(
            appstatus(&app).is_empty(),
            "and it claims no outcome: {:?}",
            appstatus(&app)
        );
        // The every-6-hours no-op stays invisible: a routine pass's
        // announcement and reads raise nothing and move no grid.
        let mut routine = App::headless_for_test();
        let regrids = message_regrids();
        routine.announce_toolchain_pass("installing 1 program(s) over the network: ty", false);
        routine.apply_toolchain_snapshot(Some(&empty_read(true)), false);
        routine.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
        routine.apply_toolchain_snapshot(Some(&empty_read(false)), false);
        routine.toolchain_pass_ended(true);
        assert_eq!(routine.messages.live_rows().count(), 0);
        assert_eq!(message_regrids(), regrids, "no re-grid");
        assert!(appstatus(&routine).is_empty(), "{:?}", appstatus(&routine));
        assert_eq!(routine.messages_deadline(), None);
    }

    /// A PASS THAT IS OVER FOLDS ITS ROW AND CLAIMS NO OUTCOME (upstream
    /// dbf97ecff/ac7942234's `a_pass_that_is_over_folds_its_bar_and_claims_no_outcome`,
    /// its whole first-run × {clean, failed, dead writer} loop on the App). A
    /// read of a pass that ended — cleanly, with a failure, or with its writer
    /// gone — used to raise a held "all 10 installed" / "1 failed" /
    /// "stopped" row; now it raises nothing, and the live row folds — at once
    /// for a routine or a sibling's read, at the child's exit for a first
    /// run's (whose ending read may be one sub-pass's). The outcome is the
    /// markers' record and a failure's the Packages badge's.
    #[test]
    fn a_pass_that_is_over_folds_its_row_and_claims_no_outcome() {
        let over = |what: &str| {
            let mut read = net_read(false, 1_700_000_000);
            if what != "clean" {
                read.file.programs.get_mut("trust").unwrap().phase = atpkg::progress::Phase::Failed;
            }
            if what == "dead writer" {
                read.file.ended_unix = None;
            }
            read
        };
        for what in ["clean", "failed", "dead writer"] {
            for first_run in [true, false] {
                let mut app = App::headless_for_test();
                app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
                assert_eq!(app.messages.live_rows().count(), 1);
                // Whoever reads the pass as over, the row is LATCHED: its
                // child's exit folds it (design §10.2 #6).
                app.apply_toolchain_snapshot(Some(&over(what)), first_run);
                assert_eq!(
                    app.messages.live_rows().count(),
                    1,
                    "{what}, first_run {first_run}: the child still runs"
                );
                app.toolchain_pass_ended(what == "clean");
                assert_eq!(
                    app.messages.live_rows().count(),
                    0,
                    "{what}, first_run {first_run}: folded"
                );
                assert!(
                    appstatus(&app).is_empty(),
                    "{what}, first_run {first_run}: no outcome claimed: {:?}",
                    appstatus(&app)
                );
                app.settle_messages(
                    Instant::now()
                        + aterm_messages::SHRINK_QUIET
                        + std::time::Duration::from_millis(1),
                );
                assert_eq!(app.message_band_rows, 0, "{what}, first_run {first_run}");
                assert_eq!(
                    app.messages_deadline(),
                    None,
                    "{what}, first_run {first_run}: nothing to wake for"
                );
            }
        }
    }

    /// A HELD ROW OUTLIVES THE FINAL SNAPSHOT, AND NONE NEVER ERASES IT
    /// (upstream dbf97ecff's
    /// `a_held_notice_outlives_the_final_snapshot_and_none_never_erases_it`,
    /// re-cut: `appnotice` text is a record since design §10.5 H7, so the held
    /// row here is a keyless toolchain row of the old notice's shape). No
    /// tailed read — running or over, first run or not — replaces or re-words
    /// it, and neither the vanished-file clear nor the child's exit folds it;
    /// the first run's own row stands BESIDE it and folds at the exit.
    #[test]
    fn a_held_row_outlives_the_final_snapshot_and_none_never_erases_it() {
        let mut app = App::headless_for_test();
        let held = app.post_message(
            Message::new(
                tags::TOOLCHAIN,
                Severity::Warn,
                "aterm pkg install claude: 2.1.267 installed",
            )
            .hold(Hold::Default),
        );
        let mut over = net_read(false, 1_700_000_000);
        over.file.ended_unix = Some(1);
        app.apply_toolchain_snapshot(Some(&over), true);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        {
            let live = app.messages.live(held).expect("never replaced by a read");
            assert_eq!(live.msg.meter, None);
            assert_eq!(live.revision, 0, "never re-worded");
            assert!(live.msg.hold.is_held());
        }
        assert_eq!(
            app.messages.live_rows().count(),
            2,
            "the first run's row stands beside it"
        );
        app.apply_toolchain_snapshot(None, true);
        app.apply_toolchain_snapshot(None, false);
        app.toolchain_pass_ended(true);
        assert!(
            app.messages.live(held).is_some(),
            "the lane's clear and the child's exit keep it"
        );
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "only the live row folded"
        );
    }

    /// `appnotice` TEXT IS RECORDED (design §10.5 H7; this replaces
    /// `an_announcement_beside_a_held_notice_displaces_nothing`, whose held
    /// notice row no longer exists): the text is on record and on
    /// `appstatus` as a finished activity, never a row; a first run's
    /// announcement beside it is the one row on the glass, and a routine one
    /// raises nothing.
    #[test]
    fn appnotice_text_is_recorded() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        assert!(
            app.take_app_notice("toolchain", "aterm pkg install claude: 2.1.267 installed")
                .is_ok()
        );
        assert!(app.take_app_notice("update", "an operator's note").is_ok());
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(message_regrids(), regrids, "no re-grid");
        let rows = appstatus(&app);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter().all(|r| r.contains(" phase=done ")),
            "records are finished activities: {rows:?}"
        );
        app.announce_toolchain_pass("installing 1", false);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "a routine announcement raises nothing"
        );
        app.announce_toolchain_pass("installing 1 program(s) over the network: ty", true);
        assert_eq!(
            app.messages
                .live_by_key(toolchain_words::KEY_PASS)
                .map(|l| l.msg.title.as_str()),
            Some("Installing ALab tools"),
            "the new pass is on glass at once"
        );
        assert_eq!(app.message_band_rows, 1);
    }

    /// A FIRST RUN'S ROW WHOSE FEED WENT SILENT FOLDS AT ITS CAP (upstream
    /// `a_live_bar_whose_feed_went_silent_folds_at_its_cap`, whose reads are
    /// a first run's since dbf97ecff): no read for the tailed cap and the
    /// meter is gone — and, as the bars' settle recorded every bar that left
    /// the glass, the words it showed are on record. The center's own test
    /// pins that a fresh read re-arms the cap (`center.rs`, "each restate
    /// re-arms the cap").
    #[test]
    fn a_first_run_row_whose_feed_went_silent_folds_at_its_cap() {
        let mut app = App::headless_for_test();
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        let now = Instant::now();
        let (stale_at, load_shown) = app
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| (l.stale_at, l.load_shown))
            .expect("the live row");
        let stale_at = stale_at.expect("the tailed cap");
        assert!(stale_at <= now + aterm_messages::STALE_TAILED);
        // The extraction's load words are up from the first frame (design
        // §10.6, review round 2): the cap is the one deadline.
        assert!(load_shown, "an extraction declares the disk, shown at post");
        assert_eq!(app.messages_deadline(), Some(stale_at));
        assert!(
            !app.settle_messages(now + aterm_messages::STALE_TAILED / 2),
            "not yet"
        );
        assert_eq!(app.messages.live_rows().count(), 1);
        assert!(
            app.settle_messages(now + aterm_messages::STALE_TAILED),
            "no report for the cap: gone"
        );
        assert_eq!(app.messages.live_rows().count(), 0);
        let rows = appstatus(&app);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].starts_with(
                "activity kind=toolchain phase=done progress=- title=Installing%20ALab%20tools "
            ),
            "the capped row leaves its words on record, as the bars' settle did: {}",
            rows[0]
        );
    }

    /// THE ROW OUTLIVES THE GLASS (upstream `a_folded_bar_leaves_its_sentence_in_the_ledger`
    /// and `settle_folds_expired_bars_and_reports_the_row_change`, re-cut for
    /// the owner's attention rule, design §10): a RECORD is on record at once
    /// and never on the glass — an `appnotice` text since H7; a ROW's fold is
    /// on record with the severity as the outcome — the ready row after its
    /// hold (ok, with its honest sentence, never "restart"), then a stopped
    /// lane's warning (warn). Each fold is reported as a glass change, the
    /// record is never a row, and a full record is still an idle zero.
    #[test]
    fn a_folded_row_leaves_its_sentence_on_record() {
        let mut app = App::headless_for_test();
        assert!(
            app.take_app_notice("toolchain", "an operator's note")
                .is_ok()
        );
        app.post_message(update_words::staged(
            "0.48.0",
            99,
            Some(update_words::ApplyPosture::ManualByConfig),
        ));
        let now = Instant::now();
        let done = |app: &App| -> Vec<String> {
            appstatus(app)
                .into_iter()
                .filter(|r| r.contains(" phase=done "))
                .collect()
        };
        assert_eq!(app.message_band_rows, 1, "the decision alone is a row");
        let rows = done(&app);
        assert_eq!(rows.len(), 1, "the record is on record at once: {rows:?}");
        let enc = crate::control::pct_encode;
        assert!(
            rows[0].starts_with(&format!(
                "activity kind=toolchain phase=done progress=- title={} ",
                enc("an operator's note")
            )) && rows[0].contains(" outcome=ok "),
            "{}",
            rows[0]
        );
        assert!(!app.settle_messages(now), "nothing due yet");
        assert!(
            app.settle_messages(now + aterm_messages::HOLD_STAGED_MANUAL),
            "the ready row folds after its hold, and the glass changed"
        );
        let rows = done(&app);
        assert_eq!(rows.len(), 2);
        assert!(
            rows[1].starts_with("activity kind=update ") && rows[1].contains(" outcome=ok "),
            "oldest first — the order they left the glass: {rows:?}"
        );
        let staged = app
            .messages
            .log()
            .records()
            .rfind(|r| r.tag == tags::UPDATE)
            .map(|r| r.detail.join(" "))
            .expect("the staged row is on record");
        assert!(
            staged.contains("automatic install is off") && !staged.contains("restart"),
            "the record carries the honest sentence: {staged:?}"
        );
        // A warn row retires as a warn outcome, not an ok one.
        let later = now + aterm_messages::HOLD_STAGED_MANUAL;
        app.note_update_outcome(update_words::needs_install(
            "Couldn't install aterm v0.48.0",
            update_words::INSTALL_FROM_MENU,
            Severity::Warn,
            99,
        ));
        assert!(
            app.settle_messages(later + aterm_messages::HOLD_WARN),
            "then the stopped lane's warning"
        );
        let rows = done(&app);
        let last = rows.last().expect("recorded");
        assert!(
            last.starts_with("activity kind=update ") && last.contains(" outcome=warn "),
            "{last}"
        );
        app.settle_messages(
            later
                + aterm_messages::HOLD_WARN
                + aterm_messages::SHRINK_QUIET
                + std::time::Duration::from_millis(1),
        );
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.message_band_rows, 0,
            "the record holds no rows on the glass"
        );
        assert_eq!(
            app.messages_deadline(),
            None,
            "FL-1: a full record is still an idle zero"
        );
    }

    /// The shape `appstatus` promises (upstream
    /// `activity_rows_put_the_live_bars_above_the_finished_ones`, whose
    /// failure goes straight to the ledger since dbf97ecff): live rows
    /// first, then the finished ones, each carrying how it ended and how long
    /// ago — a toolchain failure is a record from the start, never a row.
    #[test]
    fn appstatus_puts_the_live_rows_above_the_finished_ones() {
        let mut app = App::headless_for_test();
        app.record_toolchain_outcome(toolchain_words::failed("install failed"));
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "a failure goes straight to the record"
        );
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "0.62.0".into(),
            bytes_done: 45,
            bytes_total: 90,
        });
        let rows = appstatus(&app);
        assert_eq!(rows.len(), 2, "one live, one finished: {rows:?}");
        assert!(
            rows[0].starts_with("activity kind=update phase=live progress=50/100 "),
            "the live download leads, with its meter: {}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("activity kind=toolchain phase=done progress=- ")
                && rows[1].contains(" outcome=warn ")
                && rows[1].contains(" since_ms="),
            "the finished install follows, with how it ended: {}",
            rows[1]
        );
    }

    /// `seed-done:` is an ANSWER like the others (upstream `toolchain_ended`
    /// → `retire_answered_toolchain`; the `Wake::PkgSeedDone` arm routes it
    /// through [`App::record_toolchain_outcome`], as every other answer
    /// arm does): its record goes on `appstatus`, and the live row folds —
    /// unless a first-run child still runs, whose exit folds it.
    #[test]
    fn a_seed_done_answer_folds_the_live_row_unless_a_first_run_child_runs() {
        let answer =
            || toolchain_words::ended("the pass finished; 12 ALab program(s) are installed");
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
        app.record_toolchain_outcome(answer());
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "the first run's child still runs"
        );
        app.toolchain_pass_ended(true);
        assert_eq!(app.messages.live_rows().count(), 0);
        let rows = appstatus(&app);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].contains(" outcome=ok ")
                && rows[0].contains("title=Package%20update%20finished "),
            "{}",
            rows[0]
        );
        // A live row with no first-run child behind it (one carried across a
        // handoff) folds at the answer.
        let mut app = App::headless_for_test();
        app.post_message(toolchain_words::announced("installing 2 program(s)"));
        app.record_toolchain_outcome(answer());
        assert_eq!(app.messages.live_rows().count(), 0, "answered: folded");
        assert_eq!(appstatus(&app).len(), 1);
    }

    /// A parked outgoing handoff — the PARENT's half of the freeze predicate
    /// — with nothing live behind it (the `app_update_handoff` tests' own
    /// fixture, `parked_record`).
    fn parked() -> crate::PendingUpdateHandoff {
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        crate::PendingUpdateHandoff {
            park_at: Instant::now(),
            proof_ready_at: None,
            #[cfg(target_os = "macos")]
            activate_at_commit: false,
            attempt_id: 1,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode: crate::native_updater_service::ApplyMode::Automatic,
            apply_attempt: None,
            same_image: Some(crate::app_update_handoff::SameImageHandoff::DebugSeam),
            target_build: 0,
            target_commit: String::new(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: 0,
            cancel,
            #[cfg(unix)]
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: false,
        }
    }

    /// The manifest's window carry around what `carried_messages` handed over,
    /// as the two construction sites in `app_update_handoff` write it.
    fn window_carry(
        rows: u16,
        carried: crate::session_store::MessagesCarry,
    ) -> crate::session_store::WindowCarry {
        crate::session_store::WindowCarry {
            rows: 24,
            cols: 80,
            outer_x: None,
            outer_y: None,
            status_bar_rows: rows,
            messages: carried.messages,
            next_message_id: carried.next_message_id,
            update_verified_unix_ms: None,
        }
    }

    /// THE PARENT'S RECORD IS ITS OWN TO THE END (design §3.7, revised on
    /// review 2026-09-22), in PRODUCTION ORDER: the manifest captures the
    /// carry, THEN the attempt parks (`app_update_handoff`, both sites). A
    /// row posted under the park drains to the writer at once while its
    /// hold waits; the flush the parent runs before it execs lands it on
    /// disk; the carry ships rows, never lines. The SUCCESSOR's drain is the
    /// one that waits — until Commit — so a refused successor wrote nothing.
    #[test]
    fn the_parents_record_is_its_own_to_the_end_and_the_successors_waits_for_commit() {
        let dir =
            std::env::temp_dir().join(format!("aterm-messages-freeze-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(crate::messages_store::FILE_NAME);
        let mut app = App::headless_for_test();
        app.messages_log = crate::messages_store::Writer::spawn(&path);
        assert!(app.messages_log.is_some(), "the writer opened");
        let before = app.post_message(warn("before the park"));
        assert_eq!(
            app.messages.log().pending_len(),
            0,
            "an unfrozen post drains to the writer at once"
        );
        // The capture, then the park: the order the manifest imposes.
        let carried = app.carried_messages();
        assert_eq!(carried.messages.len(), 1);
        app.pending_update_handoff = Some(parked());
        assert!(
            app.message_holds_frozen(),
            "the parent's half of the holds predicate"
        );
        assert!(
            !app.message_persist_frozen(),
            "…and the parent's drain never waits"
        );
        let during = app.post_message(warn("during the park"));
        assert_eq!(
            app.messages.log().pending_len(),
            0,
            "the line reached the writer under the park"
        );
        let past_hold =
            Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1);
        assert!(
            !app.settle_messages(past_hold),
            "a hold that fell due under the park folds nothing"
        );
        assert!(app.messages.live(before).is_some() && app.messages.live(during).is_some());
        assert_eq!(
            app.carried_messages().next_message_id,
            during.raw() + 1,
            "the successor's ids continue past the parent's"
        );
        // The exec: the flush lands everything before the process is
        // replaced, and leaves the writer open for a failed exec.
        app.flush_messages_log();
        assert!(app.messages_log.is_some());
        let text = std::fs::read_to_string(&path).expect("the log was written");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("before the park"), "{}", lines[0]);
        assert!(lines[1].contains("during the park"), "{}", lines[1]);
        drop(app.messages_log.take());

        // THE SUCCESSOR before Commit: its lines wait; Commit drains them.
        let successor_path = dir.join("successor.log");
        let mut successor = App::headless_for_test();
        successor.messages_log = crate::messages_store::Writer::spawn(&successor_path);
        successor.incoming_handoff_pending = true;
        assert!(successor.message_persist_frozen());
        successor.post_message(warn("before Commit"));
        assert_eq!(
            successor.messages.log().pending_len(),
            1,
            "a successor that may yet be refused writes nothing"
        );
        successor.incoming_handoff_pending = false;
        successor.sync_messages();
        assert_eq!(successor.messages.log().pending_len(), 0, "Commit drains");
        drop(successor.messages_log.take());
        let text = std::fs::read_to_string(&successor_path).expect("written after Commit");
        assert_eq!(text.lines().count(), 1, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE HANDOFF CARRY (design §3.8; the port of status_bars.rs:4775 and
    /// app_restore.rs:4946-4961): the live rows cross the swap as plain words
    /// in glass order and re-seed under the handoff's staleness cap with
    /// their ids, words and holds; the update row moves on to "finishing"
    /// only when this process is the handed-off successor AND the carry had
    /// one, and a carried standing health warning leaves with it; no log
    /// line crosses (the parent's record is its own to the end); an older
    /// parent's `bars` map to rows and junk is dropped.
    #[test]
    fn a_handoff_carries_live_rows() {
        let mut parent = App::headless_for_test();
        let toolchain = parent.post_message(toolchain_words::announced("installing ay, trust"));
        parent.post_message(update_words::staged("0.76.0", 7, None));
        let installing = parent.post_message(update_words::installing("0.76.0"));
        let health = parent.post_message(update_words::health_warning(
            "Update checks failing",
            "3 consecutive check failures",
        ));
        assert_eq!(parent.message_band_rows, 3);
        assert_eq!(
            parent.messages.log().pending_len(),
            0,
            "everything before the park was drained"
        );
        // The park: a record posted under it drains as any other (a
        // ledger-only row — two lines, nothing on the glass, no writer here).
        parent.pending_update_handoff = Some(parked());
        let logged = parent.post_message(
            Message::new(tags::SYSTEM, Severity::Info, "ledger only").hold(Hold::LogOnly),
        );
        assert_eq!(
            parent.messages.log().pending_len(),
            0,
            "the parent's drain never waits"
        );
        let carried = parent.carried_messages();
        let titles: Vec<&str> = carried.messages.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Update checks failing",
                "Installing ALab tools",
                "Installing aterm v0.76.0",
            ],
            "the glass rows, in glass order (the warning outranks the lane rows)"
        );
        assert!(carried.messages.iter().all(|m| m.on_glass));
        assert_eq!(carried.messages[2].id, installing.raw());
        assert_eq!(
            carried.messages[2].key.as_deref(),
            Some(update_words::KEY_PROGRESS)
        );
        assert_eq!(carried.messages[0].hold, "default");
        assert!(
            carried.messages[1].busy && carried.messages[2].busy,
            "the first-run row and the installing row moved; the carry says so"
        );
        assert_eq!(carried.next_message_id, logged.raw() + 1);
        let carry = window_carry(parent.message_band_rows, carried.clone());

        // THE SUCCESSOR, before Commit.
        let mut successor = App::headless_for_test();
        successor.incoming_handoff_pending = true;
        successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
        successor.seed_carried_messages(&carry, Some("0.76.0"));
        let row = successor.messages.live(toolchain).expect("the id is kept");
        assert_eq!(row.msg.title, "Installing ALab tools");
        assert!(
            row.msg.detail.is_empty(),
            "no size in the sentence: no line"
        );
        assert!(
            row.msg.meter.as_ref().is_some_and(|m| m.busy),
            "the busy flag rides the carry"
        );
        assert!(!row.is_queued(), "a glass row lands on the glass");
        assert_eq!(row.fold_at, None, "no hold before Commit…");
        assert!(row.stale_at.is_some(), "…only the handoff's cap");
        assert_eq!(row.msg.origin, aterm_messages::Origin::Carried);
        let update = successor
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .expect("the progress row crossed");
        assert_eq!(update.id, installing);
        assert_eq!(update.msg.title, "Finishing aterm v0.76.0");
        assert_eq!(update.msg.detail, vec![update_words::FINISHING]);
        assert_eq!(
            successor.live_update_flow().map(|f| f.phase),
            Some(FlowPhase::Finishing),
            "the successor adopts the flow row as its own"
        );
        assert!(
            successor.messages.live(health).is_none(),
            "the build that landed is the proof: the standing warning leaves"
        );
        assert_eq!(
            successor.messages.log().get(health).unwrap().state,
            aterm_messages::LogState::Retired(aterm_messages::Retired::Resolved(Outcome::Warn))
        );
        assert_eq!(
            successor.messages.log().next_id().raw(),
            carried.next_message_id,
            "ids continue past the carry"
        );
        assert_eq!(
            successor.messages.log().pending_len(),
            1,
            "the line the warning's resolution wrote here waits for Commit; nothing crossed"
        );
        assert!(
            !successor.sync_message_band_rows(Instant::now()),
            "the geometry is the parent's until Commit"
        );
        assert_eq!(successor.message_band_rows, 3);

        // Not the successor of a handoff: the words come back as they were.
        let mut plain = App::headless_for_test();
        plain.seed_carried_messages(&carry, None);
        assert_eq!(
            plain
                .messages
                .live_by_key(update_words::KEY_PROGRESS)
                .unwrap()
                .msg
                .title,
            "Installing aterm v0.76.0"
        );
        assert_eq!(
            plain
                .messages
                .live_by_key(update_words::KEY_PROGRESS)
                .unwrap()
                .msg
                .detail,
            vec![update_words::INSTALLING],
            "its words as they were"
        );
        assert!(
            plain.messages.live(health).is_some(),
            "with no landing to prove it, the warning stands"
        );

        // No progress row carried: nothing is invented, and the health row
        // stands even for the successor.
        let mut healthy = App::headless_for_test();
        healthy.post_message(update_words::health_warning("Update checks failing", "x"));
        let only_health = window_carry(1, healthy.carried_messages());
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&only_health, Some("0.76.0"));
        assert!(
            successor
                .messages
                .live_by_key(update_words::KEY_PROGRESS)
                .is_none()
        );
        assert!(
            successor
                .messages
                .live_by_key(update_words::KEY_HEALTH)
                .is_some()
        );

        // An OLDER parent's `bars` (the status bars' carry, read until Phase 6):
        // the manifest still decodes — serde skips the key — and seeds nothing,
        // so the successor's own reporters say those lanes at Commit.
        let old: crate::session_store::WindowCarry = aterm_toml::from_str(
            "rows = 24\ncols = 80\nstatus_bar_rows = 1\n\n[[bars]]\nlane = \"toolchain\"\n\
             glyph = \"!\"\ntitle = \"ALab toolchain\"\ndetail = \"\"\ntone = \"warn\"\n\
             fill_permille = 430\n",
        )
        .expect("an older parent's window carry still decodes");
        assert_eq!(
            old.status_bar_rows, 1,
            "the rows it had committed still count"
        );
        let mut adopting = App::headless_for_test();
        adopting.seed_carried_messages(&old, Some("0.76.0"));
        assert_eq!(
            adopting.messages.live_rows().count(),
            0,
            "no bar is carried"
        );
        // An empty carry seeds nothing and raises nothing.
        let mut empty = App::headless_for_test();
        empty.seed_carried_messages(&window_carry(0, Default::default()), Some("0.76.0"));
        assert_eq!(empty.messages.live_rows().count(), 0);
        assert_eq!(empty.messages.log().next_id(), MessageId::FIRST);
    }

    /// AFTER COMMIT the carried rows take their own lifetimes back (design
    /// §3.8): a held row on glass anchors its hold at the commit instant and
    /// folds on it — into the ledger, as Folded — a standing row stands, and
    /// the band shrinks after the D1 quiet. Before Commit nothing folds on a
    /// hold and the row count never moves. And the one state in which the
    /// two counts disagree — a row the parent's frozen count had queued —
    /// converges on the center's count at the first unfrozen sync.
    #[test]
    fn carried_messages_reseed_live_and_fold_after_commit() {
        let mut parent = App::headless_for_test();
        let tools = vec!["ay".to_string()];
        let pill = toolchain_words::seed_pill_text(&tools, None, 0, HookDialect::Zsh);
        // A held Success row (the record's words at the default hold, as a
        // build before the silent lane carried them).
        let installed = parent.post_message(toolchain_words::installed(&pill).hold(Hold::Default));
        // A STANDING row (the health warning holds its 45 s since the merge's
        // ruling 59; a standing row is the GPU-lost and a11y lanes' shape).
        let health = parent.post_message(
            update_words::health_warning("Update checks failing", "3 consecutive check failures")
                .hold(Hold::Standing),
        );
        assert_eq!(parent.message_band_rows, 2);
        parent.pending_update_handoff = Some(parked());
        let carry = window_carry(parent.message_band_rows, parent.carried_messages());
        assert_eq!(carry.status_bar_rows, 2);

        let mut successor = App::headless_for_test();
        successor.incoming_handoff_pending = true;
        successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
        successor.seed_carried_messages(&carry, None);
        assert_eq!(successor.messages.live_rows().count(), 2);
        assert_eq!(successor.messages.committed_rows(), 2);
        // A hold that would be due: nothing folds before Commit — the rows
        // have no hold yet, and the freeze would decline one anyway.
        let now = Instant::now();
        let past_success = now + HOLD_SUCCESS + std::time::Duration::from_secs(1);
        assert!(!successor.settle_messages(past_success));
        assert!(successor.messages.live(installed).is_some());
        assert_eq!(successor.message_band_rows, 2, "frozen");

        // COMMIT — what the `ActivateCommittedHandoff` arm does.
        successor.incoming_handoff_pending = false;
        successor.messages.after_handoff_commit(now);
        successor.sync_messages();
        let row = successor.messages.live(installed).unwrap();
        assert_eq!(
            row.fold_at,
            Some(now + HOLD_SUCCESS),
            "the success's own hold, from Commit"
        );
        assert_eq!(row.stale_at, None, "the handoff cap is gone");
        let standing = successor.messages.live(health).unwrap();
        assert_eq!(standing.fold_at, None);
        assert_eq!(standing.stale_at, None, "a standing row stands");
        assert_eq!(successor.message_band_rows, 2);
        // The hold falls due: the row folds into the ledger; the band keeps
        // its height through the D1 quiet and gives the row back after it.
        assert!(successor.settle_messages(past_success));
        assert!(successor.messages.live(installed).is_none());
        assert_eq!(
            successor.messages.log().get(installed).unwrap().state,
            aterm_messages::LogState::Retired(aterm_messages::Retired::Folded)
        );
        assert!(successor.messages.live(health).is_some());
        assert_eq!(successor.message_band_rows, 2, "shrink after the quiet…");
        assert!(successor.settle_messages(past_success + aterm_messages::SHRINK_QUIET));
        assert_eq!(successor.message_band_rows, 1, "…not before");
        assert_eq!(
            successor.messages.log().pending_len(),
            0,
            "the successor's own lines drained on the first sync after Commit (no writer here: dropped at the seam)"
        );

        // THE QUEUED CARRY. A one-row band, a second row of the same rank
        // posted under the parent's freeze: it crosses queued; the successor's
        // center commits two rows for it while the geometry keeps the
        // parent's one — until Commit, when the geometry takes the center's
        // count even though `commit_rows` itself reports no move.
        let mut parent = App::headless_for_test();
        let first = parent
            .post_message(Message::new(tags::SESSION, Severity::Success, "first").line("one"));
        assert_eq!(parent.message_band_rows, 1);
        parent.pending_update_handoff = Some(parked());
        let second = parent
            .post_message(Message::new(tags::SESSION, Severity::Success, "second").line("two"));
        assert!(
            parent.messages.live(second).unwrap().is_queued(),
            "the frozen count has one slot and the earlier row holds it"
        );
        let carried = parent.carried_messages();
        assert_eq!(carried.messages.len(), 2);
        assert!(carried.messages[0].on_glass && !carried.messages[1].on_glass);
        let carry = window_carry(parent.message_band_rows, carried);
        let mut successor = App::headless_for_test();
        successor.incoming_handoff_pending = true;
        successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
        successor.seed_carried_messages(&carry, None);
        assert_eq!(successor.messages.committed_rows(), 2);
        assert_eq!(successor.message_band_rows, 1);
        assert!(!successor.sync_message_band_rows(now), "frozen");
        successor.incoming_handoff_pending = false;
        successor.messages.after_handoff_commit(now);
        assert_eq!(
            successor.messages.commit_rows(now, MAX_ROWS),
            None,
            "the center's count already stands at two…"
        );
        assert!(
            successor.sync_message_band_rows(now),
            "…so the geometry converges on it here"
        );
        assert_eq!(successor.message_band_rows, 2);
        assert!(
            successor.messages.live(first).is_some() && successor.messages.live(second).is_some()
        );
        assert!(!successor.messages.live(second).unwrap().is_queued());
    }

    /// NO TOOLCHAIN ANSWER ARM POSTS A ROW (origin/main merge review,
    /// 2026-09-23). The merge's first cut left `Wake::PkgSeedDone` on
    /// `post_message(toolchain_words::ended(..))`: the record still landed,
    /// but the live row never folded at the answer and every window repainted
    /// for a record (ruling 34). The arms need an event loop, so the seam is
    /// pinned in the source: in `lib.rs` a `toolchain_words` builder reaches
    /// `post_message` only for an `appnotice` text (R34); every pass answer
    /// goes through [`App::record_toolchain_outcome`] or
    /// [`App::record_message`]. Whitespace is dropped first, so a rustfmt
    /// line break between the call and its builder cannot hide one.
    #[test]
    fn no_toolchain_answer_arm_posts_a_row() {
        let source: String = include_str!("lib.rs").split_whitespace().collect();
        let mut offenders = Vec::new();
        for prefix in [
            concat!("post_message(", "toolchain_words::"),
            concat!("post_message(", "crate::toolchain_words::"),
        ] {
            for (at, _) in source.match_indices(prefix) {
                let builder = source[at + prefix.len()..]
                    .split('(')
                    .next()
                    .unwrap_or_default();
                if builder != "appnotice" {
                    offenders.push(builder.to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a toolchain answer raises a row instead of recording through \
             `record_toolchain_outcome` / `record_message`: {offenders:?}"
        );
    }

    // ---- Attention and motion (design §10.5, §10.8, §10.12) --------------

    /// A LIGHT routine read: the same pass as [`net_read`] with a plan far
    /// under [`toolchain_words::HEAVY_PASS_BYTES`] — the pass that paints
    /// nothing.
    fn light_read(running: bool, started: u64) -> crate::PkgProgressSnapshot {
        let mut snap = net_read(running, started);
        snap.file.overall.bytes_done = 12_000_000;
        snap.file.overall.bytes_total = 40_000_000;
        snap
    }

    /// The newest record under `key` that retired as a record.
    fn recorded(app: &App, key: &str) -> Option<LogRecord> {
        app.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(key))
            .filter(|r| r.retired() == Some(&Retired::Recorded))
            .cloned()
    }

    /// An automatic update's staged build is a RECORD (design §10.5 H2): no
    /// row, no re-grid, the words in the log — and a fast download that ends
    /// inside its grace costs nothing on the glass.
    #[test]
    fn an_automatic_staged_build_is_a_record_and_leaves_no_row() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "9.9.9".into(),
            bytes_done: 10_000_000,
            bytes_total: 74_000_000,
        });
        assert_eq!(app.messages.live_rows().count(), 1, "the download is live");
        assert_eq!(app.message_band_rows, 0, "…inside its grace, off the glass");
        // The App's own posture — a headless App's is the handoff-off
        // record; the automatic lane's is a record too.
        app.note_update_progress(&aterm_update::Progress::Staged {
            version: "9.9.9".into(),
            build: 41,
        });
        app.settle_messages(Instant::now() + PROGRESS_GRACE * 2);
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        assert!(
            app.messages.echoes().is_empty(),
            "an unrevealed row leaves no echo"
        );
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(
            message_regrids(),
            regrids,
            "a fast automatic update costs no re-grid"
        );
        let rec = recorded(&app, update_words::KEY_PROGRESS).expect("the staged record");
        assert_eq!(rec.title, "aterm v9.9.9 is ready");
    }

    /// AN AUTOMATIC UPDATE COSTS AT MOST TWO RE-GRIDS: a download that
    /// outlives its grace opens its row once, its end (the staged record)
    /// resolves it — a Complete echo holds the slot — and the row is given
    /// back once, after the echo and the D1 quiet.
    #[test]
    fn an_automatic_update_costs_at_most_two_regrids_and_a_fast_one_none() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        let t0 = Instant::now();
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "9.9.9".into(),
            bytes_done: 10_000_000,
            bytes_total: 74_000_000,
        });
        let t = t0 + PROGRESS_GRACE + std::time::Duration::from_millis(10);
        app.settle_messages(t);
        assert_eq!(
            app.message_band_rows, 1,
            "the slow download is on the glass"
        );
        assert_eq!(message_regrids(), regrids + 1);
        // The App's own posture — a headless App's is the handoff-off
        // record; the automatic lane's is a record too.
        app.note_update_progress(&aterm_update::Progress::Staged {
            version: "9.9.9".into(),
            build: 41,
        });
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(EchoKind::Complete),
            "the download ends in its Complete echo"
        );
        assert_eq!(app.message_band_rows, 1, "the echo holds the slot");
        let mut t = t;
        for _ in 0..40 {
            t += std::time::Duration::from_millis(100);
            app.settle_messages(t);
        }
        assert_eq!(app.message_band_rows, 0, "the row is given back");
        assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
    }

    /// A RATE-LIMITED DOWNLOAD DID NOT FAIL (review 2026-09-24): a GitHub
    /// rate limit (`Progress::Deferred`) heals by itself and retries on the
    /// next check, so the revealed download row leaves with no outcome to
    /// claim — a fade, never `⚠ failed` — and the deferral is a record. A
    /// real failure still ends in its Fault echo.
    #[test]
    fn a_deferred_download_fades_and_a_failed_one_faults() {
        for (progress, kind) in [
            (
                aterm_update::Progress::Deferred {
                    detail: "rate limited".into(),
                },
                EchoKind::Vanish,
            ),
            (
                aterm_update::Progress::Failed {
                    detail: "could not verify the download".into(),
                },
                EchoKind::Fault,
            ),
        ] {
            let mut app = App::headless_for_test();
            app.note_update_progress(&aterm_update::Progress::Downloading {
                version: "9.9.9".into(),
                bytes_done: 10_000_000,
                bytes_total: 74_000_000,
            });
            app.settle_messages(
                Instant::now() + PROGRESS_GRACE + std::time::Duration::from_millis(10),
            );
            assert_eq!(app.message_band_rows, 1, "revealed");
            app.note_update_progress(&progress);
            assert_eq!(app.messages.live_rows().count(), 0, "{progress:?}");
            assert_eq!(
                app.messages.echoes().first().map(|e| e.kind),
                Some(kind),
                "{progress:?}"
            );
        }
    }

    /// THE LANDING (design §10.5 H4): the live Finishing row is resolved
    /// `Ok` — its Complete echo plays — and the good news is a record.
    #[test]
    fn the_landing_resolves_the_finishing_row_and_records_the_words() {
        let mut app = App::headless_for_test();
        let finishing = app.post_message(update_words::finishing("0.91.0"));
        app.settle_messages(Instant::now() + PROGRESS_GRACE * 2);
        assert!(
            app.messages.glass_position(finishing).is_some(),
            "on the glass"
        );
        app.post_update_landed("0.91.0", 41, 0);
        assert!(app.messages.live(finishing).is_none());
        assert!(matches!(
            app.messages
                .log()
                .get(finishing)
                .and_then(LogRecord::retired),
            Some(Retired::Resolved(Outcome::Ok))
        ));
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(EchoKind::Complete)
        );
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "the landing takes no row"
        );
        let landed = app
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.tag == aterm_messages::tags::UPDATE)
            .expect("the landing's record");
        assert_eq!(landed.retired(), Some(&Retired::Recorded));
    }

    /// A NAVIGATION press that took the person to fix the thing READ the row
    /// (design §10.5 H6): a held row folds; a live row stays.
    #[test]
    fn a_navigation_press_folds_a_held_row() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let packages = Intent::OpenSettings {
            route: "/packages".into(),
        };
        let held = app.post_message(warn("Packages need attention").action(packages.clone()));
        assert!(app.perform_intent(wid, held, packages.clone()));
        assert!(app.messages.live(held).is_none(), "the held row folded");
        assert!(!app.messages.log().get(held).is_some_and(LogRecord::is_live));
        let live = app.post_message(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab toolchain")
                .action(packages.clone())
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_ANNOUNCE,
                }),
        );
        assert!(app.perform_intent(wid, live, packages));
        assert!(app.messages.live(live).is_some(), "work in flight stays");
    }

    /// A REFUSED INSTALL (design §10.2 #7): the Installing row an explicit
    /// apply added resolves `Warn` — a refusal is not a delivery — and its
    /// indicator ends in the Fault echo.
    #[test]
    fn a_refused_install_resolves_warn_and_echoes_fault() {
        let mut app = App::headless_for_test();
        app.begin_update_installing(41, true);
        let id = app
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .expect("the explicit apply's row")
            .id;
        assert!(
            app.messages.glass_position(id).is_some(),
            "committed at once"
        );
        app.retire_update_installing();
        assert!(app.messages.live(id).is_none());
        assert!(matches!(
            app.messages.log().get(id).and_then(LogRecord::retired),
            Some(Retired::Resolved(Outcome::Warn))
        ));
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(EchoKind::Fault)
        );
    }

    /// VERY HEAVY SYSTEM USE (design §10.6, H8): a routine pass that planned
    /// at least `HEAVY_PASS_BYTES` raises its one row — after the grace, so
    /// the glass moves only if the work lasts — and a light one raises none.
    #[test]
    fn a_heavy_routine_pass_raises_one_row_after_the_grace_and_a_light_one_none() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "a light pass paints nothing"
        );
        app.settle_messages(Instant::now() + PROGRESS_GRACE * 2);
        assert_eq!(message_regrids(), regrids);
        let t0 = Instant::now();
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
        let live = app
            .messages
            .live_rows()
            .next()
            .expect("the heavy pass's row");
        assert_eq!(live.msg.title, "Updating ALab tools");
        assert_eq!(app.message_band_rows, 0, "inside its grace, off the glass");
        assert_eq!(message_regrids(), regrids);
        app.settle_messages(t0 + PROGRESS_GRACE + std::time::Duration::from_millis(10));
        assert_eq!(app.message_band_rows, 1, "revealed once the work lasted");
        assert_eq!(message_regrids(), regrids + 1);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "a read restates in place"
        );
        assert_eq!(message_regrids(), regrids + 1);
    }

    /// ONE OPEN, ONE FOLD across the child's sub-passes (design §10.2 #6): a
    /// heavy routine row is LATCHED — a sub-pass's answer and its ending read
    /// fold nothing — and the child's exit folds it with its Complete echo.
    #[test]
    fn a_heavy_routine_row_folds_once_at_the_childs_exit_across_sub_passes() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        let t0 = Instant::now();
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
        let mut t = t0 + PROGRESS_GRACE + std::time::Duration::from_millis(10);
        app.settle_messages(t);
        assert_eq!(app.message_band_rows, 1);
        // The vendor sub-pass answers and ends; the ALab set's reads follow.
        app.record_toolchain_outcome(toolchain_words::ended("the vendor pass finished"));
        app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), false);
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "latched: nothing folds"
        );
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), false);
        assert_eq!(app.messages.live_rows().count(), 1);
        app.toolchain_pass_ended(true);
        assert_eq!(app.messages.live_rows().count(), 0, "the exit folds it");
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(EchoKind::Complete)
        );
        for _ in 0..40 {
            t += std::time::Duration::from_millis(100);
            app.settle_messages(t);
        }
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
    }

    /// The first run's end names how the child exited: Complete when clean,
    /// Fault when not.
    #[test]
    fn the_first_run_end_echoes_complete_when_clean_and_fault_when_not() {
        for (clean, kind) in [(true, EchoKind::Complete), (false, EchoKind::Fault)] {
            let mut app = App::headless_for_test();
            app.announce_toolchain_pass("installing 10 ALab program(s)", true);
            app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
            app.toolchain_pass_ended(clean);
            assert_eq!(app.messages.live_rows().count(), 0);
            assert_eq!(
                app.messages.echoes().first().map(|e| e.kind),
                Some(kind),
                "clean={clean}"
            );
        }
    }

    /// A FIRST RUN THAT ENDS SHORT LEAVES A FAILURE ROW (review 2026-09-24):
    /// the row told the person to wait for a multi-gigabyte deliverable, so
    /// when a failure marker names the end and the child exits unclean, the
    /// Fault echo is followed by a row they can act on — `Packages`, the
    /// default hold, the cause behind Details. A clean exit, a heavy ROUTINE
    /// pass (the installed versions keep working) and a failure with no
    /// first run latched keep only the record.
    #[test]
    fn a_first_run_that_ends_short_leaves_a_failure_row() {
        use crate::toolchain_words::FirstRunShort;
        let failure_rows = |app: &App| {
            app.messages
                .live_rows()
                .filter(|l| l.msg.key.as_deref() == Some(toolchain_words::KEY_PASS))
                .map(|l| (l.msg.title.clone(), l.msg.hold, l.msg.severity))
                .collect::<Vec<_>>()
        };
        // The first run, ended by `seed-failed:` and an unclean exit.
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        app.note_toolchain_first_run_short(FirstRunShort::Failed, "could not reach the index");
        assert_eq!(
            failure_rows(&app).len(),
            1,
            "the live row until the child exits"
        );
        app.toolchain_pass_ended(false);
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(EchoKind::Fault),
            "the indicator ends as a fault"
        );
        assert_eq!(
            failure_rows(&app),
            vec![(
                toolchain_words::PACKAGES_FAILED.to_string(),
                Hold::Default,
                Severity::Error
            )],
            "…and the failure row follows it"
        );
        // An over read after the exit leaves the failure standing.
        app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
        assert_eq!(failure_rows(&app).len(), 1, "an over read leaves it");
        assert_eq!(failure_rows(&app)[0].1, Hold::Default);
        // A retry's first read replaces it at once — never behind the failure
        // it answers — and so does the next first run's announcement.
        let mut retry = App::headless_for_test();
        retry.announce_toolchain_pass("installing 10 ALab program(s)", true);
        retry.note_toolchain_first_run_short(FirstRunShort::Failed, "offline");
        retry.toolchain_pass_ended(false);
        assert_eq!(failure_rows(&retry)[0].1, Hold::Default, "the failure row");
        retry.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), true);
        let rows = failure_rows(&retry);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            matches!(rows[0].1, Hold::Live { .. }),
            "the retry's progress took the key: {rows:?}"
        );
        app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
        assert!(
            failure_rows(&app)
                .iter()
                .all(|(_, hold, _)| matches!(hold, Hold::Live { .. })),
            "a new pass takes the key back"
        );

        // A clean exit after a marker: the record only.
        let mut clean = App::headless_for_test();
        clean.announce_toolchain_pass("installing 10 ALab program(s)", true);
        clean.note_toolchain_first_run_short(FirstRunShort::Failed, "trust failed");
        clean.toolchain_pass_ended(true);
        assert!(
            failure_rows(&clean).is_empty(),
            "a clean exit raises nothing"
        );

        // A heavy ROUTINE pass that fails: the record only.
        let mut heavy = App::headless_for_test();
        let mut read = net_read(true, 1_700_000_000);
        read.file.overall.bytes_total = toolchain_words::HEAVY_PASS_BYTES * 2;
        heavy.apply_toolchain_snapshot(Some(&read), false);
        heavy.note_toolchain_first_run_short(FirstRunShort::Failed, "could not reach the index");
        heavy.toolchain_pass_ended(false);
        assert!(
            failure_rows(&heavy).is_empty(),
            "a routine pass's failure keeps the installed versions: a record"
        );

        // No latched first run at all: nothing.
        let mut idle = App::headless_for_test();
        idle.note_toolchain_first_run_short(FirstRunShort::Nothing, "nothing installed");
        idle.toolchain_pass_ended(false);
        assert!(failure_rows(&idle).is_empty());
    }

    /// A headless App with its first window put ON SCREEN (the test seam for
    /// a real OS window) and focused — the one place the band moves.
    fn on_screen_app() -> App {
        let mut app = App::headless_for_test();
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        ws.band_on_screen_for_test = true;
        ws.focused = true;
        app.system_reduce_motion = false;
        app.serious_mode = false;
        app
    }

    /// THE IDLE LAW (design §10.8): no row, nothing armed; a live row on a
    /// window that is not on screen (headless, occluded), nothing armed; a
    /// held row only, nothing armed; a live row on screen, a frame on the
    /// grid; after the last row folds and its echo ends, nothing armed.
    #[test]
    fn the_band_motion_deadline_is_none_when_idle_or_hidden() {
        let mut app = App::headless_for_test();
        let now = Instant::now();
        assert_eq!(
            app.band_motion_deadline(now),
            None,
            "an idle band arms nothing"
        );
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        assert_eq!(app.message_band_rows, 1);
        assert_eq!(
            app.band_motion_deadline(now),
            None,
            "a headless window has no motion"
        );
        let mut app = on_screen_app();
        app.post_message(warn("a held warning"));
        assert_eq!(
            app.band_motion_deadline(now),
            None,
            "a held row never moves"
        );
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        let now = Instant::now();
        let at = app
            .band_motion_deadline(now)
            .expect("a live row on screen moves");
        assert!(
            at > now && at <= now + aterm_messages::ANIM_FRAME * 2,
            "the next frame is on the grid"
        );
        app.windows.get_mut(&WindowId(0)).unwrap().occluded = true;
        assert_eq!(
            app.band_motion_deadline(now),
            None,
            "occluded: nothing armed"
        );
        app.windows.get_mut(&WindowId(0)).unwrap().occluded = false;
        app.toolchain_pass_ended(true);
        app.clear_messages_for_test();
        assert_eq!(
            app.band_motion_deadline(Instant::now()),
            None,
            "folded: nothing armed"
        );
    }

    /// Unfocused, the band is STILL: the meter is its still form and the
    /// only wake left is a time word's tick — never a frame-rate wake.
    #[test]
    fn an_unfocused_window_paints_the_still_form_and_wakes_only_for_text() {
        let mut app = on_screen_app();
        app.windows.get_mut(&WindowId(0)).unwrap().focused = false;
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        let look = app.band_look(WindowId(0));
        assert_eq!(look.pace, aterm_messages::Pace::Still);
        let now = Instant::now();
        let first = app
            .band_motion_deadline(now)
            .expect("the elapsed words tick");
        assert!(
            first >= now + std::time::Duration::from_secs(5),
            "no frame-rate wake while unfocused: {:?}",
            first.saturating_duration_since(now)
        );
        let fp0 = app.prepare_band_motion(WindowId(0), now);
        let fp1 = app.prepare_band_motion(WindowId(0), now + std::time::Duration::from_secs(3));
        assert_eq!(fp0, fp1, "the still form does not move with time");
        let motion = app.windows[&WindowId(0)]
            .band_motion
            .clone()
            .expect("prepared");
        assert_eq!(
            motion.rows[0].anim,
            aterm_messages::Anim::Track,
            "the still busy form: the unlit track"
        );
        assert!(
            motion.rows[0]
                .surface
                .cells(80)
                .iter()
                .all(|t| *t == aterm_messages::Tone::TRACK),
            "…lit nowhere"
        );
    }

    /// A CHANGE OF LOOK IS A DUE FRAME by itself (review 2026-09-24): a
    /// frame on glass drawn in the moving look is due the moment the window's
    /// look turns still — focus lost, Serious Mode, Reduce Motion — whether or
    /// not the trigger asked for its own redraw, and a still frame is due the
    /// moment motion is allowed again. Once re-drawn in the window's look it is
    /// due only on its deadline.
    #[test]
    fn a_frame_drawn_in_another_look_is_due_at_once() {
        let wid = WindowId(0);
        let mut app = on_screen_app();
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        let now = Instant::now();
        app.prepare_band_motion(wid, now);
        assert!(
            app.band_motion_due(now).is_empty(),
            "just drawn in its own look: not due yet"
        );
        app.windows.get_mut(&wid).unwrap().focused = false;
        assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Still);
        assert_eq!(
            app.band_motion_due(now),
            vec![wid],
            "the moving frame on glass is stale in the still look"
        );
        app.prepare_band_motion(wid, now);
        assert!(app.band_motion_due(now).is_empty(), "re-drawn: settled");
        app.windows.get_mut(&wid).unwrap().focused = true;
        assert_eq!(app.band_motion_due(now), vec![wid], "and back again");
    }

    /// SERIOUS MODE AND REDUCED MOTION retire motion only: the band keeps
    /// its row and its words, in the still form.
    #[test]
    fn serious_mode_and_reduced_motion_paint_the_still_forms() {
        let wid = WindowId(0);
        let mut app = on_screen_app();
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Moving);
        app.serious_mode = true;
        assert_eq!(
            app.band_look(wid).pace,
            aterm_messages::Pace::Still,
            "Serious Mode"
        );
        assert_eq!(app.message_band_rows, 1, "…keeps the row");
        app.serious_mode = false;
        app.system_reduce_motion = true;
        assert_eq!(
            app.band_look(wid).pace,
            aterm_messages::Pace::Still,
            "Reduce Motion"
        );
        app.system_reduce_motion = false;
        app.config.motion = Some("reduced".into());
        assert_eq!(
            app.band_look(wid).pace,
            aterm_messages::Pace::Still,
            "motion = reduced"
        );
        app.config.motion = None;
        assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Moving);
    }

    /// AN ANIMATION NEVER RE-GRIDS: 200 motion frames and a Complete echo
    /// cost exactly the open and the fold the same run costs with no motion.
    #[test]
    fn an_animation_never_regrids() {
        let wid = WindowId(0);
        let mut app = on_screen_app();
        let regrids = message_regrids();
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        assert_eq!(message_regrids(), regrids + 1, "the open");
        let mut t = Instant::now();
        let mut moved = std::collections::BTreeSet::new();
        for _ in 0..200 {
            t += aterm_messages::ANIM_FRAME;
            for due in app.band_motion_due(t) {
                moved.insert(app.prepare_band_motion(due, t));
            }
            assert_eq!(app.message_band_rows, 1);
        }
        assert!(
            moved.len() > 10,
            "the frames drew something new: {}",
            moved.len()
        );
        assert_eq!(message_regrids(), regrids + 1, "no frame re-grids");
        app.toolchain_pass_ended(true);
        for _ in 0..30 {
            t += aterm_messages::ANIM_FRAME;
            app.prepare_band_motion(wid, t);
        }
        assert_eq!(message_regrids(), regrids + 1, "the echo re-grids nothing");
        for _ in 0..40 {
            t += std::time::Duration::from_millis(100);
            app.settle_messages(t);
        }
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
    }

    // -----------------------------------------------------------------------
    // The update lane on the band (the ux/status-reporting merge, 2026-09-23).
    // -----------------------------------------------------------------------

    fn download(version: &str) -> aterm_update::Progress {
        aterm_update::Progress::Downloading {
            version: version.into(),
            bytes_done: 45_000_000,
            bytes_total: 74_000_000,
        }
    }

    /// The ready row a press installs from (automatic install off).
    fn post_ready_staged(app: &mut App, version: &str, build: u64) -> MessageId {
        app.post_update_row(
            update_words::staged(
                version,
                build,
                Some(update_words::ApplyPosture::ManualByConfig),
            ),
            version,
            FlowPhase::Staged { build },
        )
    }

    /// The switch's row: busy, on the glass at once.
    fn post_installing(app: &mut App, version: &str) -> MessageId {
        app.post_update_row(
            update_words::installing(version),
            version,
            FlowPhase::Installing,
        )
    }

    fn update_row(app: &App) -> Option<String> {
        app.messages
            .live_by_key(update_words::KEY_PROGRESS)
            .map(|l| format!("{} \u{2014} {}", l.msg.title, l.msg.detail.join("; ")))
    }

    /// A SPINNER MUST NEVER DELAY THE UPDATE IT ANIMATES (2026-09-23; design
    /// ruling 56, re-pinned on the engine's motion by ruling 140). The flow
    /// row is busy while the lane works and the automatic lane waits for quiet
    /// moments, so an animation frame that counted as terminal activity would
    /// push the very install it is showing. Sixteen spinner steps through the host's
    /// own frame path (`prepare_band_motion`, one per [`SPIN_FRAMES`] of the
    /// 33 ms grid) move the painted frame and touch no activity clock — not
    /// the quiet-epoch stamp, not the handoff epoch, not the keystroke clock
    /// — and the lane's quiet verdict at a later instant is the same with or
    /// without them. The negative control: a tolerated event (a pointer move)
    /// DOES move the stamp, so the reading is not vacuous. And with no window
    /// on screen (a headless App) there is no frame and no wake.
    ///
    /// [`SPIN_FRAMES`]: aterm_messages::SPIN_FRAMES
    #[test]
    fn the_busy_animation_never_touches_the_update_activity_clocks() {
        let wid = WindowId(0);
        let mut app = on_screen_app();
        let now = Instant::now();
        // The switch's row: busy, on the glass at once (no grace).
        post_installing(&mut app, "9.9.9");
        assert!(
            app.messages.busy_on_glass(),
            "PRECONDITION: a busy row is up"
        );
        assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Moving);
        let clocks = |app: &App| {
            (
                app.last_update_activity_at,
                app.update_handoff_activity_epoch,
                app.last_keystroke_at,
            )
        };
        let before = clocks(&app);
        let later = now + std::time::Duration::from_secs(5);
        let quiet_before = app.automatic_update_activity_quiet_with_pending_input(later, false);
        let step = aterm_messages::ANIM_FRAME * aterm_messages::SPIN_FRAMES;
        let mut drawn = std::collections::BTreeSet::new();
        for k in 0..16_u32 {
            drawn.insert(app.prepare_band_motion(wid, now + step * k));
        }
        assert!(
            drawn.len() >= 15,
            "PRECONDITION: the frames moved ({})",
            drawn.len()
        );
        assert_eq!(clocks(&app), before, "an animation frame is not activity");
        assert_eq!(
            app.automatic_update_activity_quiet_with_pending_input(later, false),
            quiet_before,
            "the automatic lane reads the terminal exactly as it would without the spinner"
        );
        // Off screen (a headless window): no frame, and nothing wakes.
        let mut headless = App::headless_for_test();
        headless.note_update_progress(&aterm_update::Progress::Verifying {
            version: "9.9.9".into(),
        });
        assert_eq!(headless.band_motion_deadline(later), None);
        assert!(headless.band_motion_due(later).is_empty());
        assert_eq!(clocks(&app), before);
        // NEGATIVE CONTROL: real (tolerated) activity does move the stamp.
        std::thread::sleep(std::time::Duration::from_millis(2));
        app.note_update_handoff_tolerated_activity();
        assert_ne!(
            clocks(&app).0,
            before.0,
            "the clock this test reads is live"
        );
    }

    /// THE BAND ANIMATES ONLY WHILE A BUSY ROW IS COMMITTED, THE WINDOW IS
    /// FOCUSED AND ON SCREEN, MOTION IS ALLOWED AND NOTHING IS FROZEN (the
    /// gating union, ruling 140): serious mode and Reduce Motion draw the row
    /// still; a busy row queued behind the committed rows does not animate;
    /// and under a handoff freeze the band is still and arms and asks for
    /// nothing — the frame on glass stays WHERE IT IS, not even a switch to
    /// still repaints the parked screen.
    #[test]
    fn the_band_animates_only_while_committed_focused_moving_and_unfrozen() {
        use aterm_messages::Pace;
        let wid = WindowId(0);
        let mut app = on_screen_app();
        let now = Instant::now();
        assert_eq!(app.band_motion_deadline(now), None, "no row");
        // A busy row with no grace to wait out: the switch's.
        app.post_message(update_words::installing("9.9.9"));
        assert_eq!(app.band_look(wid).pace, Pace::Moving);
        assert!(app.band_motion_deadline(now).is_some());
        app.windows.get_mut(&wid).unwrap().focused = false;
        assert_eq!(app.band_look(wid).pace, Pace::Still, "no focused window");
        app.windows.get_mut(&wid).unwrap().focused = true;
        app.serious_mode = true;
        assert_eq!(app.band_look(wid).pace, Pace::Still, "serious mode: still");
        app.serious_mode = false;
        app.system_reduce_motion = true;
        assert_eq!(app.band_look(wid).pace, Pace::Still, "Reduce Motion: still");
        app.system_reduce_motion = false;
        // Frozen: still, nothing armed, nothing due — the frame stays.
        let at = now + aterm_messages::ANIM_FRAME * 12;
        let fp = app.prepare_band_motion(wid, at);
        assert_ne!(fp, 0, "PRECONDITION: a moving frame is on glass");
        app.incoming_handoff_pending = true;
        assert_eq!(app.band_look(wid).pace, Pace::Still, "frozen: still");
        assert_eq!(app.band_motion_deadline(at), None, "frozen: nothing armed");
        assert!(
            app.band_motion_due(at + aterm_messages::ANIM_FRAME * 30)
                .is_empty(),
            "frozen: nothing due, even in another look"
        );
        assert_eq!(app.windows[&wid].band_motion_fp, fp, "frozen where it is");
        app.incoming_handoff_pending = false;
        // Queued behind three Warn rows: a row the band does not show does
        // not move.
        let mut queued = on_screen_app();
        for n in 0..3 {
            queued.post_message(Message::new(tags::CONFIG, Severity::Warn, format!("w{n}")));
        }
        queued.note_update_progress(&aterm_update::Progress::Verifying {
            version: "9.9.9".into(),
        });
        let cols = usize::from(queued.windows[&wid].cols);
        let p = queued.band_presentation(cols);
        let busy_shown = p.rows.iter().any(|r| match r.kind {
            aterm_messages::RowKind::Message(id) => {
                queued.messages.live(id).is_some_and(|l| l.is_busy())
            }
            _ => false,
        });
        assert!(
            !busy_shown,
            "PRECONDITION: the busy row is queued behind the warnings"
        );
        assert_eq!(
            queued.band_motion_deadline(now),
            None,
            "a busy row the band does not show does not move"
        );
    }

    /// A FRAME NEVER BUMPS THE CENTER'S REVISION: a frame is paint, so
    /// Settings ▸ Messages — which republishes on the revision — is never
    /// republished at the frame rate.
    #[test]
    fn a_frame_tick_never_bumps_the_center_revision() {
        let mut app = on_screen_app();
        app.note_update_progress(&aterm_update::Progress::Verifying {
            version: "9.9.9".into(),
        });
        let revision = app.messages.revision();
        let now = Instant::now();
        for k in 0..40_u32 {
            app.prepare_band_motion(WindowId(0), now + aterm_messages::ANIM_FRAME * k);
        }
        assert_eq!(app.messages.revision(), revision);
    }

    /// ONE UPDATE COSTS ONE GROW AND ONE SHRINK (the regrid budget, measured):
    /// download → check → the ready row → the press's installing are
    /// restatements or supersedes of ONE row, and forty animation frames are
    /// paint — one grow for all of it; the landing's fold after the D1 quiet is
    /// the one shrink.
    #[test]
    fn an_update_flow_costs_one_grow_and_one_shrink() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        app.note_update_progress(&download("9.9.9"));
        app.note_update_progress(&aterm_update::Progress::Verifying {
            version: "9.9.9".into(),
        });
        post_ready_staged(&mut app, "9.9.9", 7);
        app.begin_update_installing(7, true);
        let frames_from = Instant::now();
        for k in 0..40_u32 {
            app.prepare_band_motion(WindowId(0), frames_from + aterm_messages::ANIM_FRAME * k);
        }
        assert_eq!(app.message_band_rows, 1);
        assert_eq!(message_regrids(), regrids + 1, "one grow");
        app.post_update_landed("9.9.9", 7, 0);
        let now = Instant::now();
        let _ = app.settle_messages(now + aterm_messages::HOLD_SUCCESS);
        let _ = app.settle_messages(
            now + aterm_messages::HOLD_SUCCESS
                + aterm_messages::SHRINK_QUIET
                + std::time::Duration::from_millis(1),
        );
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(message_regrids(), regrids + 2, "and one shrink");
    }

    /// UNSAVED EDITOR WORK HOLDING THE INSTALL IS A ROW OF ITS OWN: the one
    /// blocker only the person can clear, painted (ruling 77) — the automatic
    /// lane raises no row of its own to carry it (the owner's silent path,
    /// 2026-09-24).
    #[test]
    fn an_editor_holding_the_install_is_a_row_of_its_own() {
        let mut app = App::headless_for_test();
        app.note_update_blockers(&[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()], true);
        let outcome = app
            .messages
            .live_by_key(update_words::KEY_OUTCOME)
            .expect("the row");
        assert_eq!(
            outcome.msg.title,
            crate::app_update_screen::UPDATE_WAITS_FOR_YOU
        );
        assert_eq!(
            outcome.msg.detail[0],
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            "the blocker is what the person does: painted"
        );
        assert!(outcome.msg.excerpt);
        assert_eq!(outcome.msg.severity, Severity::Warn);
        assert_eq!(app.messages.live_rows().count(), 1, "one row");
        // THE WORDS FIT WHERE A TERMINAL IS (2026-09-24): no capsule takes the
        // columns the instruction needs — at 80 it reads whole up to its last
        // word, and at 60 it still says what to do.
        let _ = app.settle_messages(Instant::now());
        for (cols, painted) in [
            (80, "Save or close the open editor to finish"),
            (60, "Save or close"),
        ] {
            let row = app.band_presentation(cols).rows[0].clone();
            let detail = row.detail.map(|(_, words)| words).unwrap_or_default();
            assert!(detail.starts_with(painted), "{cols} columns: {detail:?}");
            assert!(
                row.capsules.iter().all(|c| c.action.is_details()),
                "{cols} columns: only `Details ›`"
            );
        }
        // NEGATIVE CONTROL: a blocker that clears by itself raises nothing.
        let mut quiet = App::headless_for_test();
        quiet.note_update_blockers(&[App::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string()], true);
        assert_eq!(quiet.messages.live_rows().count(), 0);
    }

    /// A HELD UPDATE ROW REPLACED BY A NEWER ONE IS RESOLVED ON RECORD (main's
    /// rule, kept by ruling 143): one outcome row replaced by another leaves a
    /// finished activity on `appstatus` — a superseded row would record
    /// nothing. A failed download is a record of its own (a self-retry), and
    /// the live download row it ends is resolved `Warn`, on record too.
    #[test]
    fn a_held_update_row_replaced_by_a_newer_one_is_resolved_on_record() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        app.note_update_progress(&download("9.9.9"));
        app.note_update_progress(&aterm_update::Progress::Failed {
            detail: "connection reset".into(),
        });
        assert_eq!(update_row(&app), None, "a self-retry is a record");
        app.note_update_progress(&download("9.9.9"));
        app.note_update_outcome(update_words::needs_install("first", "", Severity::Warn, 7));
        app.note_update_outcome(update_words::needs_install("second", "", Severity::Warn, 7));
        let done: Vec<String> = appstatus(&app)
            .into_iter()
            .filter(|r| r.contains(" phase=done "))
            .collect();
        assert!(
            done.iter().any(
                |r| r.contains("title=Couldn%27t%20download%20aterm%20v9.9.9")
                    || r.contains("title=Couldn't%20download%20aterm%20v9.9.9")
            ),
            "{done:#?}"
        );
        assert!(done.iter().any(|r| r.contains("title=first ")), "{done:#?}");
        assert!(
            done.iter()
                .any(|r| r.contains("title=Downloading%20aterm%20v9.9.9")),
            "the download the failure ended is on record: {done:#?}"
        );
    }

    /// THE LANDING IS ON RECORD WITH HOW LONG THE UPDATE TOOK — from THIS
    /// build's finished download, carried across the handoff; never for a build
    /// met already on disk, nor for another build's download.
    #[test]
    fn the_landing_is_on_record_with_how_long_the_update_took() {
        let took = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.title.contains("after it downloaded"))
                .map(|r| r.title.clone())
                .collect::<Vec<_>>()
        };
        // The carry: the parent's instant on the wall clock, the successor's
        // on its own monotonic clock.
        let now = Instant::now();
        let wall = std::time::SystemTime::now();
        let ms = update_words::verified_unix_ms(
            Some((7, now - std::time::Duration::from_secs(42))),
            7,
            now,
            wall,
        );
        let mut app = App::headless_for_test();
        app.update_verified = update_words::carried_verification(ms, 7, now, wall);
        app.post_update_landed("9.9.9", 7, 0);
        assert_eq!(
            update_row(&app),
            None,
            "the landing takes no row (ruling 141)"
        );
        assert!(
            app.messages
                .log()
                .records()
                .any(|r| r.title == "Updated to aterm v9.9.9"),
            "the landing's words are on record"
        );
        let said = took(&app);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0] == "Installed aterm v9.9.9 42 s after it downloaded"
                || said[0] == "Installed aterm v9.9.9 43 s after it downloaded",
            "{said:?}"
        );
        // Negative controls: met on disk (no instant), another build's instant,
        // no carried instant.
        let mut disk = App::headless_for_test();
        disk.post_update_landed("9.9.9", 7, 0);
        assert!(took(&disk).is_empty());
        let mut other = App::headless_for_test();
        other.update_verified = Some((6, Instant::now()));
        other.post_update_landed("9.9.9", 7, 0);
        assert!(took(&other).is_empty());
        let mut none = App::headless_for_test();
        none.update_verified = update_words::carried_verification(None, 7, now, wall);
        none.post_update_landed("9.9.9", 7, 0);
        assert!(took(&none).is_empty());
    }

    /// A HEALED WARNING IS RECORDED AS WORKING AGAIN — ONCE per announced
    /// episode, after its 45 s fold too; a second heal records nothing.
    #[test]
    fn a_healed_update_warning_is_recorded_as_working_again_once() {
        let body = "3 failed checks in a row since 2026-09-14T22:04:36Z: manifests \
                    cannot be downloaded. Run `aterm ctl update status` for details.";
        let mut app = App::headless_for_test();
        assert!(app.note_update_health(aterm_update::health_failing_title("pipeline"), body));
        let now = Instant::now();
        let _ = app
            .settle_messages(now + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1));
        assert!(
            app.messages.live_by_key(update_words::KEY_HEALTH).is_none(),
            "folded"
        );
        let healed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.title == "aterm updates work again")
                .count()
        };
        assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
        assert_eq!(healed(&app), 1);
        let rec = app
            .messages
            .log()
            .records()
            .rfind(|r| r.title == "aterm updates work again")
            .unwrap();
        assert_eq!(
            rec.detail[0],
            "it had said: aterm can't download updates \u{2014} since Sep 14"
        );
        assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
        assert_eq!(healed(&app), 1, "once per episode");
    }

    /// A PERSISTENT FAILURE IS READ ONCE PER CLASS: its title names the broken
    /// half, the excerpt since when, the whole sentence behind Details, the
    /// warning's hold (never Standing); a count restatement posts nothing.
    #[test]
    fn a_persistent_failure_is_read_once_per_class() {
        let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release manifests \
                    exist but cannot be downloaded. Run `aterm ctl update status` for details.";
        for class in ["apply", "pipeline", "stage", "manifest"] {
            let mut app = App::headless_for_test();
            let title = aterm_update::health_failing_title(class);
            assert!(app.note_update_health(title, body));
            let row = app
                .messages
                .live_by_key(update_words::KEY_HEALTH)
                .expect("the warning");
            assert_eq!(row.msg.title, title);
            assert_eq!(row.msg.detail[0], "since Sep 14");
            assert!(
                row.msg.detail[1..]
                    .join(" ")
                    .contains("aterm ctl update status")
            );
            assert_eq!(row.msg.hold, Hold::Default);
            let revision = app.messages.revision();
            assert!(!app.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, "21 failed"));
            assert_eq!(app.messages.revision(), revision, "RESTATED posts nothing");
        }
    }

    /// ONE ANNOUNCEMENT PER TITLE PER LAUNCH, WHOEVER RAISES IT (the review of
    /// the update audit's merge onto main's health door): the updater's
    /// streak, its overdue notice and the automatic lane's convergence all say
    /// "aterm can't install updates". The first is a row and owed the OS
    /// banner (`true`); the same title again, unhealed, posts nothing and owes
    /// nothing — also after the row folded. A different class still speaks, and
    /// a healing re-opens the title for a new episode.
    ///
    /// RED before the latch moved to this door: the second call returned
    /// `true` and re-posted the row (and, in the caller, a second banner).
    #[test]
    fn a_failing_title_is_announced_once_whoever_raises_it() {
        let apply = aterm_update::health_failing_title("apply");
        let streak = "3 failed attempts to install in a row since 2026-09-24T01:00:00Z: why. \
                      Run `aterm ctl update status` for details.";
        let overdue = "build 9 has been waiting on this machine since 2026-09-24T00:00:00Z and \
                       build 8 is still running: why. Run `aterm ctl update status` for details.";
        let mut app = App::headless_for_test();
        assert!(app.note_update_health(apply, "automatic apply of build 9 stopped"));
        let revision = app.messages.revision();
        assert!(
            !app.note_update_health(apply, overdue),
            "the overdue notice, same title"
        );
        assert_eq!(app.messages.revision(), revision, "no second row");
        let _ = app.settle_messages(
            Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1),
        );
        assert!(
            app.messages.live_by_key(update_words::KEY_HEALTH).is_none(),
            "folded"
        );
        assert!(
            !app.note_update_health(apply, streak),
            "folded is still said"
        );
        assert!(app.messages.live_by_key(update_words::KEY_HEALTH).is_none());
        assert!(
            app.note_update_health(aterm_update::health_failing_title("manifest"), streak),
            "another class is other news"
        );
        assert!(
            !app.note_update_health(apply, overdue),
            "and does not re-open this one"
        );
        assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest"));
        assert!(
            !app.note_update_health(apply, overdue),
            "a healed download streak does not answer an install warning"
        );
        assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "apply"));
        assert!(
            app.note_update_health(apply, streak),
            "a healed title speaks again"
        );
    }

    /// THE LEDGER'S HEAL PROVES THE CLASS IT NAMES, NOT EVERY WARNING: an
    /// install warning the convergence or overdue notice raised is not
    /// answered by the updater healing a download streak it had announced —
    /// that read put "aterm updates work again" on record over a build still
    /// stranded. The heal of the apply class, or a heal that names no class
    /// (the updater before it named one), still answers it.
    #[test]
    fn the_ledgers_heal_answers_only_the_class_it_counted() {
        let apply = aterm_update::health_failing_title("apply");
        let body = "automatic apply of build 9 stopped after 2 failed handoffs";
        let up = |app: &App| app.messages.live_by_key(update_words::KEY_HEALTH).is_some();
        let healed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.title == "aterm updates work again")
                .count()
        };
        for (class, answers) in [
            ("manifest", false),
            ("stage", false),
            ("apply", true),
            ("", true),
        ] {
            let mut app = App::headless_for_test();
            assert!(app.note_update_health(apply, body));
            assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, class));
            assert_eq!(!up(&app), answers, "{class:?}: the row");
            assert_eq!(healed(&app), usize::from(answers), "{class:?}: the record");
        }
    }

    /// A WARNING HEALS ONLY ON THE PROOF THAT ANSWERS IT: the title names the
    /// broken half, so a download never records "aterm updates work again"
    /// over "aterm can't install updates" — the updater's ledger keeps an
    /// install streak through a whole-pipeline success. Checking is answered by
    /// a download starting, downloading by a staged build, installing only by a
    /// landing (or the ledger's own recovery); an unanswered warning stays up
    /// and remembered for the proof that does answer it.
    #[test]
    fn a_warning_heals_only_on_the_proof_that_answers_it() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
        let healed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.title == "aterm updates work again")
                .count()
        };
        let staged = aterm_update::Progress::Staged {
            version: "9.9.9".into(),
            build: 7,
        };
        let warning_up = |app: &App| app.messages.live_by_key(update_words::KEY_HEALTH).is_some();
        // Install: neither a download nor a staged build answers it.
        let mut install = App::headless_for_test();
        assert!(install.note_update_health(aterm_update::health_failing_title("apply"), body));
        install.note_update_progress(&download("9.9.9"));
        install.note_update_progress(&staged);
        assert!(
            warning_up(&install),
            "a download says nothing about installing"
        );
        assert_eq!(healed(&install), 0, "NEGATIVE: no false recovery on record");
        install.post_update_landed("9.9.9", 7, 0);
        assert!(!warning_up(&install));
        assert_eq!(healed(&install), 1, "the landing is the proof");
        // Download: a download starting does not answer it; a staged build does.
        let mut download_class = App::headless_for_test();
        assert!(
            download_class.note_update_health(aterm_update::health_failing_title("stage"), body)
        );
        download_class.note_update_progress(&download("9.9.9"));
        assert!(warning_up(&download_class));
        assert_eq!(healed(&download_class), 0);
        download_class.note_update_progress(&staged);
        assert!(!warning_up(&download_class));
        assert_eq!(healed(&download_class), 1);
        // Check: a download starting answers it.
        let mut check = App::headless_for_test();
        assert!(check.note_update_health(aterm_update::health_failing_title("manifest"), body));
        check.note_update_progress(&download("9.9.9"));
        assert!(!warning_up(&check));
        assert_eq!(healed(&check), 1);
        // The ledger's own recovery answers every class.
        let mut ledger = App::headless_for_test();
        assert!(ledger.note_update_health(aterm_update::health_failing_title("apply"), body));
        assert!(!ledger.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
        assert_eq!(healed(&ledger), 1);
    }

    /// A CARRIED WARNING HEALED BY THE LANDING IS ON THE SUCCESSOR'S RECORD: the
    /// parent `_exit`s at Commit and never learns its warning healed, so the
    /// successor that resolved the carried row records the healing at its
    /// landing — once — against what the parent's warning said.
    #[test]
    fn a_carried_warning_healed_by_the_landing_is_on_the_successors_record() {
        let mut parent = App::headless_for_test();
        let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
        parent.post_message(update_words::installing("0.76.0"));
        assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
        let carry = window_carry(parent.message_band_rows, parent.carried_messages());
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&carry, Some("0.76.0"));
        assert!(
            successor
                .messages
                .live_by_key(update_words::KEY_HEALTH)
                .is_none(),
            "resolved with the handoff"
        );
        successor.post_update_landed("0.76.0", 7, 0);
        let said: Vec<String> = successor
            .messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .map(|r| r.detail.join(" "))
            .collect();
        assert_eq!(
            said,
            vec!["it had said: aterm can't install updates \u{2014} since Sep 14".to_string()]
        );
        // A plain restart carries nothing to heal.
        let mut plain = App::headless_for_test();
        plain.seed_carried_messages(&carry, None);
        plain.post_update_landed("0.76.0", 7, 0);
        assert!(
            plain
                .messages
                .log()
                .records()
                .all(|r| r.title != "aterm updates work again"),
            "no finishing successor: nothing this process said, so nothing to record"
        );
    }

    /// EVERY UPDATE MOMENT IS A LINE IN THE LOG (the log area's P3): downloaded,
    /// the switch begun, a stopped automatic attempt, the landing — each a record
    /// or a supersede the log keeps, in order, in its words; a restatement alone
    /// writes no line, which is why the lane POSTS each phase.
    #[test]
    fn every_update_moment_is_a_line_in_the_log() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        app.note_update_progress(&download("9.9.9"));
        app.note_update_progress(&aterm_update::Progress::Verifying {
            version: "9.9.9".into(),
        });
        app.record_message(update_words::downloaded("9.9.9", 7));
        post_ready_staged(&mut app, "9.9.9", 7);
        #[cfg(unix)]
        {
            app.record_update_switch_started(6, false, true);
            app.record_update_switch_stopped(true, "you were typing");
            // The automatic lane's second attempt at the same build: aterm.log's.
            let before = app.messages.log().len();
            app.record_update_switch_started(6, false, true);
            assert_eq!(app.messages.log().len(), before, "one record per build");
        }
        app.post_update_landed("9.9.9", 7, 0);
        let titles: Vec<String> = app
            .messages
            .log()
            .records()
            .map(|r| r.title.clone())
            .collect();
        let pos = |needle: &str| {
            titles
                .iter()
                .position(|t| t.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is not on record: {titles:#?}"))
        };
        assert!(pos("Downloaded aterm 9.9.9") < pos("Updated to aterm v9.9.9"));
        for phase in [
            "Downloading aterm v9.9.9",
            "Checking aterm v9.9.9",
            "aterm v9.9.9 is ready",
        ] {
            assert!(
                titles.iter().any(|t| t == phase),
                "downloading, checking and staged each a line (a title per phase, \
                 ruling 143): {phase:?} in {titles:#?}"
            );
        }
        // NEGATIVE CONTROL: a restatement writes no line.
        let before = app.messages.log().len();
        let id = post_installing(&mut app, "9.9.8");
        let after_post = app.messages.log().len();
        assert!(after_post > before);
        app.restate_message(id, restatement_of(&update_words::finishing("9.9.8")));
        assert_eq!(
            app.messages.log().len(),
            after_post,
            "a restate is not a line"
        );
    }

    /// A HARNESS NOTE IS RECORDED AND NEVER TAKES A ROW (design ruling 61): on
    /// `appstatus` as `kind=harness`, no row, no re-grid, no wake; the same
    /// note again is not written again; beside a first-run row it leaves that
    /// one row alone.
    #[test]
    fn a_harness_note_is_recorded_and_never_takes_a_row() {
        let mut app = App::headless_for_test();
        let regrids = message_regrids();
        assert_eq!(
            app.take_app_notice("harness", "aterm harness acting \u{00b7} approving rm"),
            Ok(crate::AppNoticeTaken::Recorded)
        );
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(app.messages_deadline(), None);
        assert_eq!(message_regrids(), regrids);
        let last = appstatus(&app).pop().expect("on record");
        assert!(
            last.starts_with(
                "activity kind=harness phase=done progress=- \
                 title=aterm%20harness%20acting%20%C2%B7%20approving%20rm "
            ),
            "{last}"
        );
        let len = app.messages.log().len();
        assert!(!app.record_harness_note("aterm harness acting \u{00b7} approving rm"));
        assert_eq!(
            app.messages.log().len(),
            len,
            "a repeat is not written again"
        );
        assert!(app.record_harness_note("aterm harness acting \u{00b7} approving ls"));
        assert_eq!(app.messages.log().len(), len + 1);
        app.announce_toolchain_pass("installing 2 ALab program(s) (about 1 GB)", true);
        app.record_harness_note("aterm harness idle");
        assert_eq!(app.messages.live_rows().count(), 1);
        assert_eq!(
            app.messages.live_rows().next().unwrap().msg.title,
            toolchain_words::INSTALLING_ALAB
        );
    }

    /// AN UNCHANGED MANAGED-CURRENT MARKER IS RECORDED ONCE; NEWS IS RECORDED
    /// AGAIN (design ruling 58): atpkg prints it every pass, and the shared ring
    /// is for news — a moved version, or a frozen tab counted, is new words.
    #[test]
    fn an_unchanged_managed_current_is_recorded_once_and_news_again() {
        let mut app = App::headless_for_test();
        let managed = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.key.as_deref() == Some(toolchain_words::KEY_MANAGED))
                .count()
        };
        let wire = "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)";
        assert!(app.post_managed_current(wire));
        assert!(!app.post_managed_current(wire), "the same words: not again");
        assert_eq!(managed(&app), 1);
        assert!(app.post_managed_current(
            "claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)"
        ));
        assert_eq!(managed(&app), 2, "a moved version is news");
        app.seamless_adopt = vec![crate::spawn::Adopted {
            local_id: 3,
            master: -1,
            pid: -1,
            sid: aterm_session::SessionId::generate(),
            nonce: aterm_session::LaunchNonce::generate(),
            checkpoint: None,
            control: None,
            frozen_path: true,
            identity: None,
            topics: Vec::new(),
            repaint: false,
        }];
        assert!(
            app.post_managed_current(
                "claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)"
            ),
            "a frozen tab counted is new words"
        );
        assert!(
            !app.post_managed_current(""),
            "an empty body records nothing"
        );
    }

    // ---- the wire's verbs: `notice` and `messages` (design rulings 163-179) ----

    /// One `notice` line through the control thread's parse and the main
    /// thread's apply, as `cmd_notice` runs them.
    fn notice(app: &mut App, line: &str) -> String {
        let req = wire::NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        app.take_notice(req)
    }

    /// The id an `OK …=<id>` reply names.
    fn replied_id(reply: &str, field: &str) -> MessageId {
        let raw = reply
            .split_whitespace()
            .find_map(|w| w.strip_prefix(field))
            .unwrap_or_else(|| panic!("{reply:?} has no {field}"));
        MessageId::from_raw(raw.parse().expect("a number")).expect("a nonzero id")
    }

    /// A warn post round-trips: the reply names the row, the band commits it,
    /// and `messages` is the engine's own rows byte for byte (the host adds
    /// nothing but the clock and the socket's encoder).
    #[test]
    fn a_notice_round_trips_through_the_band() {
        let mut app = App::headless_for_test();
        let reply = notice(
            &mut app,
            "post system sev=warn key=deploy Deploy failed -- connection refused",
        );
        assert!(reply.starts_with("OK message="), "{reply}");
        let id = replied_id(&reply, "message=");
        let live = app.messages.live(id).expect("a warn post is a row");
        assert_eq!(live.msg.origin, aterm_messages::Origin::Wire);
        assert_eq!(app.message_band_rows, 1, "the row is on the band");
        let q = wire::ReadQuery::default();
        // `ago_ms=` reads the clock: retry across a millisecond boundary.
        let same = (0..5).any(|_| {
            let host = app.read_messages(&q);
            let engine = wire::message_rows(
                &app.messages,
                &q,
                wall_stamp_now().unix_ms,
                &crate::control::pct_encode,
            );
            host == engine
        });
        assert!(same, "{:?}", app.read_messages(&q));
        let rows = app.read_messages(&q);
        let row = rows.last().expect("one row");
        assert!(
            row.contains(" origin=wire state=held ")
                && row.contains(" key=wire.deploy ")
                && row.contains(" detail=connection%20refused "),
            "{row}"
        );
        // An info post is a record: no row, nothing on the band.
        let reply = notice(&mut app, "post system hello from a script");
        assert!(reply.starts_with("OK recorded message="), "{reply}");
        let rec = replied_id(&reply, "message=");
        assert!(app.messages.live(rec).is_none());
        assert_eq!(app.message_band_rows, 1);
    }

    /// A progress flood repaints at most once a motion frame: the engine paces,
    /// the host counts only the paints it asked for at once, and the one paced
    /// paint left over is in the loop's deadline and taken by its settle.
    #[test]
    fn a_wire_progress_flood_repaints_at_most_once_a_frame() {
        let mut app = App::headless_for_test();
        let before = WIRE_SYNCS.with(std::cell::Cell::get);
        let start = Instant::now();
        let first = notice(&mut app, "progress build pct=0 Building aterm");
        let id = replied_id(&first, "message=");
        for i in 1..500_u32 {
            let pct = f64::from(i) / 5.0;
            let reply = notice(
                &mut app,
                &format!("progress build pct={pct:.1} Building aterm"),
            );
            assert_eq!(replied_id(&reply, "message="), id, "one row per key");
        }
        let elapsed_ms = start.elapsed().as_millis();
        let syncs = u128::from(WIRE_SYNCS.with(std::cell::Cell::get) - before);
        let frame_ms = aterm_messages::WIRE_PAINT_GAP.as_millis();
        assert!(
            syncs <= 2 + elapsed_ms / frame_ms,
            "{syncs} repaints in {elapsed_ms} ms"
        );
        // Arm a paced paint (a line inside a frame of the last paint).
        let mut n = 0_u32;
        while app.wire_gate.due().is_none() && n < 64 {
            n += 1;
            let _ = notice(
                &mut app,
                &format!("progress build done={n}/100 Building aterm"),
            );
        }
        let due = app.wire_gate.due().expect("a paced paint is armed");
        let deadline = app.messages_deadline().expect("the loop is due back");
        assert!(deadline <= due, "the deadline covers the paced paint");
        assert!(app.settle_messages(due), "the paced paint repaints");
        assert_eq!(app.wire_gate.due(), None, "taken once");
    }

    /// `notice act` is the page's own press: an index answers an ask (`Not
    /// now` retires it, the press is on record), a FULL label presses a
    /// navigation, `details` marks a row seen; a press the row lacks and an
    /// id with no live row are refused with the engine's words.
    #[test]
    fn notice_act_is_the_pages_press() {
        let mut app = App::headless_for_test();
        let ask = app.post_message(
            Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::Ask {
                    for_: aterm_messages::HOLD_ASK,
                }),
        );
        let act = |id: MessageId, press: wire::Press| wire::NoticeRequest::Act { id, press };
        assert_eq!(
            app.take_notice(act(ask, wire::Press::Index(0))),
            "OK acted=Not%20now performed=1"
        );
        assert!(app.messages.live(ask).is_none(), "the answer retired it");
        let rec = app.messages.log().get(ask).expect("on record");
        assert_eq!(wire::state_word(rec, None), "answered");
        assert_eq!(
            rec.last_action.as_ref().map(|(l, _)| l.as_str()),
            Some("Not now")
        );
        assert_eq!(
            app.take_notice(act(ask, wire::Press::Index(0))),
            format!("ERR notice: no live message {ask}")
        );
        // A navigation by its full label.
        let open = Intent::OpenSettings {
            route: "/packages".into(),
        };
        let label = open.label();
        let nav = app.post_message(
            Message::new(tags::CONFIG, Severity::Warn, "Tools need attention").action(open),
        );
        assert_eq!(
            app.take_notice(act(nav, wire::Press::Label(label.to_lowercase()))),
            format!(
                "ERR notice: no action {} on message {nav}",
                crate::control::pct_encode(&label.to_lowercase())
            ),
            "labels are exact"
        );
        assert_eq!(
            app.take_notice(act(nav, wire::Press::Label(label.to_string()))),
            format!("OK acted={} performed=1", crate::control::pct_encode(label))
        );
        assert_eq!(
            settings_view(&app, WindowId(0)).map(|v| v.route),
            Some(SettingsRoute::Packages)
        );
        // `details` on a live progress row marks it seen.
        let p = replied_id(
            &notice(&mut app, "progress fetch Fetching sources"),
            "message=",
        );
        assert!(!app.messages.live(p).expect("live").seen);
        let reply = app.take_notice(act(p, wire::Press::Details));
        assert!(
            reply.starts_with(&format!(
                "OK acted={} performed=",
                crate::control::pct_encode(Intent::Details.label())
            )),
            "{reply}"
        );
        assert!(app.messages.live(p).expect("still live").seen);
        assert_eq!(
            app.take_notice(act(p, wire::Press::Index(0))),
            format!("ERR notice: no action 0 on message {p}")
        );
    }

    /// The host never spells the wire's key namespace: only `notice` can
    /// name a `wire.` key, so no host row can ever be restated, ended or
    /// superseded from the socket (design ruling 167).
    #[test]
    fn no_host_key_is_in_the_wire_namespace() {
        let needle = format!("\"{}", aterm_messages::WIRE_KEY_PREFIX);
        for (file, text) in [
            ("message_reporters.rs", include_str!("message_reporters.rs")),
            ("messages_host.rs", include_str!("messages_host.rs")),
            ("toolchain_words.rs", include_str!("toolchain_words.rs")),
            ("update_words.rs", include_str!("update_words.rs")),
            ("app_update_screen.rs", include_str!("app_update_screen.rs")),
            ("lib.rs", include_str!("lib.rs")),
        ] {
            assert!(!text.contains(&needle), "{file} spells a wire key");
        }
    }
}
