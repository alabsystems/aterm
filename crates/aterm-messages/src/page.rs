// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Model and projection for the Settings ▸ Messages page ([`MessagesState`], [`MessageView`], [`MessagesFilter`]).

use std::borrow::Cow;

use crate::center::{Live, MessageCenter};
use crate::digest::DigestEntry;
use crate::log::{LogRecord, Retired, escape_into, join_us};
use crate::model::{Glyph, Intent, MessageId, Severity, UpgradeWord, tags};
use crate::{wire, words};

/// Note shown when the log file could not be opened and messages live only in memory.
pub const NOT_SAVED: &str = "Log file could not be opened; messages are kept until aterm quits.";

/// Heading of the Messages page.
pub const HEADING: &str = "Messages";

/// Default page subtitle when no open exception leads the list.
pub const SUBTITLE: &str = "What aterm reported, newest first.";

/// Page subtitle when an open exception leads the list ([`MessagesState::of`]).
pub const SUBTITLE_OPEN_FIRST: &str = "What aterm reported, open items first.";

/// Empty-state message when the log has no entries.
pub const EMPTY: &str = "Nothing yet.";

/// Empty-state message when no entries match the active filters.
pub const NO_MATCH: &str = "No messages match these filters.";

/// Button label beside [`NO_MATCH`] that clears both filters.
pub const SHOW_ALL: &str = "Show All";

/// Label of the first tag chip that admits every tag.
pub const ALL_TAGS: &str = "All tags";

/// Footer button label that copies [`MessageView::copy_text`] to the clipboard.
pub const COPY: &str = "Copy";

/// Count-line button label that copies [`copy_all_text`] to the clipboard.
pub const COPY_ALL: &str = "Copy All";

/// Screen-reader name for [`COPY_ALL`].
pub const COPY_ALL_NAME: &str = "Copy All shown messages";

/// Caption above an expanded entry's technical lines ([`MessageView::sentence_and_technical`]).
pub const TECHNICAL: &str = "Technical details";

/// Formats the `(All, Problems)` severity segment labels with their counts.
#[must_use]
pub fn severity_segment_words(all: usize, problems: usize) -> (String, String) {
    (
        format!("All \u{00b7} {all}"),
        format!("Problems \u{00b7} {problems}"),
    )
}

/// Painted label for the tag filter pop-up menu.
#[must_use]
pub fn tag_menu_label(tag: Option<&str>) -> String {
    format!("Tag: {}", tag_menu_current(tag))
}

/// Screen-reader name for the tag filter pop-up menu ([`tag_menu_label`]).
#[must_use]
pub fn tag_menu_name(tag: Option<&str>) -> String {
    format!("Filter by tag: {}", tag_menu_current(tag))
}

/// The tag the pop-up says is down: `All`, or the chip's words.
fn tag_menu_current(tag: Option<&str>) -> Cow<'_, str> {
    tag.map_or(Cow::Borrowed("All"), tag_words)
}

/// Formats an action button label for the page footer.
#[must_use]
pub fn button_label(label: &str) -> String {
    match label {
        "Packages" | "Software Update" | "Manual" | "Appearance" | "Messages" | "Security"
        | "Settings" => format!("Open {label}"),
        other => other.to_string(),
    }
}

/// Index of the primary action in `actions` (first actionable non-decline intent), if any.
#[must_use]
pub fn primary_index(actions: &[MessageActionView]) -> Option<usize> {
    match actions.first() {
        Some(first) if first.still_actionable => Some(0),
        _ => actions
            .iter()
            .position(|action| action.still_actionable && !action.declines),
    }
}

/// Formats build information and all `shown` entries for Copy All.
#[must_use]
pub fn copy_all_text<'a>(build: &str, shown: impl IntoIterator<Item = &'a MessageView>) -> String {
    let mut text = build.to_string();
    for entry in shown {
        text.push_str("\n\n");
        text.push_str(&entry.copy_text());
    }
    text
}

/// Host callbacks and environment state needed to build a [`MessagesState`] projection.
pub trait Host {
    /// Current wall-clock time in milliseconds since the Unix epoch.
    fn now_unix_ms(&self) -> u64;
    /// Display path of the log folder when no file manager is available.
    fn log_folder(&self) -> Option<String>;
    /// Whether messages are persisted to a log file.
    fn saved(&self) -> bool;
    /// Reader's local clock offset from UTC in seconds (0 if unknown).
    fn utc_offset_s(&self) -> i64;
    /// Build number currently staged by the updater, if any.
    fn staged_build(&self) -> Option<u64>;
    /// Whether `path` exists as a regular file.
    fn is_regular_file(&self, path: &str) -> bool;
    /// Whether `tab`'s agent upgrade still targets `to` and accepts `word`.
    fn upgrade_takes(&self, tab: &str, to: &str, word: UpgradeWord) -> bool;
    /// Whether this host still tracks `tab`'s agent upgrade.
    fn upgrade_stands(&self, tab: &str) -> bool;
    /// Whether navigation intents from the page are supported on this host.
    fn performs_navigation(&self) -> bool;
    /// Number of leading `detail` lines that form the plain sentence for a record keyed `key`.
    fn sentence_lines(&self, key: Option<&str>, detail: &[String]) -> usize;
}

/// Shared projection behind Settings ▸ Messages ([`MessagesState::of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesState {
    /// Revision of the message center at which this projection was built.
    pub revision: u64,
    /// Wall-clock timestamp (unix ms) when this projection was built.
    pub now_unix_ms: u64,
    /// Projected entries, newest first, with open exceptions leading.
    pub entries: Vec<MessageView>,
    /// Pairs `(told_on, standing)` mapping superseded record IDs to the entry that stands for them.
    pub told_on: Vec<(u64, u64)>,
    /// `(tag, count)` pairs in [`tags::ALL`] then first-seen order.
    pub tags: Vec<(String, usize)>,
    /// Log folder display path ([`Host::log_folder`]).
    pub log_folder: Option<String>,
    /// Whether messages are persisted to disk.
    pub saved: bool,
    /// Reader's UTC offset in seconds ([`Host::utc_offset_s`]).
    pub utc_offset_s: i64,
}

impl MessagesState {
    /// Empty projection at revision 0.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            revision: 0,
            now_unix_ms: 0,
            entries: Vec::new(),
            told_on: Vec::new(),
            tags: Vec::new(),
            log_folder: None,
            saved: true,
            utc_offset_s: 0,
        }
    }

    /// Looks up the entry with raw ID `id`.
    #[must_use]
    pub fn entry(&self, id: u64) -> Option<&MessageView> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Page subtitle for this projection ([`SUBTITLE_OPEN_FIRST`] or [`SUBTITLE`]).
    #[must_use]
    pub fn subtitle(&self) -> &'static str {
        if self
            .entries
            .first()
            .is_some_and(|entry| entry.state == "review")
        {
            SUBTITLE_OPEN_FIRST
        } else {
            SUBTITLE
        }
    }

    /// Returns the ID of the entry currently standing for record `id` ([`Self::told_on`]).
    #[must_use]
    pub fn standing_for(&self, id: u64) -> Option<u64> {
        self.told_on
            .iter()
            .find(|(told, _)| *told == id)
            .map(|(_, standing)| *standing)
    }

    /// Next wall-clock timestamp (unix ms) when any relative time word in this projection changes.
    #[must_use]
    pub fn words_change_at(&self, offset_at: impl Fn(u64) -> Option<i64>) -> Option<u64> {
        let at = self
            .entries
            .iter()
            .flat_map(|entry| [entry.at_unix_ms, entry.began_unix_ms])
            .map(|at| words::words_change_after(self.now_unix_ms, at, self.utc_offset_s))
            .min()?
            .min(self.now_unix_ms.saturating_add(86_400_000));
        let same_offset =
            |instant: u64| offset_at(instant).is_none_or(|offset| offset == self.utc_offset_s);
        if same_offset(at) {
            return Some(at);
        }
        let (mut same, mut moved) = (self.now_unix_ms, at);
        while moved.saturating_sub(same) > 1_000 {
            let mid = same + (moved - same) / 2;
            if same_offset(mid) {
                same = mid;
            } else {
                moved = mid;
            }
        }
        Some(moved)
    }

    /// Builds the page projection from `center` and `host`.
    #[must_use]
    pub fn of(center: &MessageCenter, host: &impl Host) -> Self {
        let now_unix_ms = host.now_unix_ms();
        let glass: Vec<(MessageId, u8)> = center
            .on_glass()
            .filter_map(|l| {
                let row = center.glass_position(l.id)?;
                u8::try_from(row).ok().map(|row| (l.id, row))
            })
            .collect();
        let log = center.log();
        let mut entries = Vec::with_capacity(log.len());
        let mut to_look_at = Vec::new();
        let mut counts: Vec<(String, usize)> = Vec::new();
        let mut told_on: Vec<(u64, u64)> = Vec::new();
        let mut began: Vec<(u64, u64)> = Vec::new();
        for rec in log.records().rev() {
            // ONE FACT, ONE ROW: a record a later row tells the story of on
            // (a keyed post in new words, a notice moved to its next state, an
            // exception taking over a row) is that row's past, not a row of its
            // own; the `messages` verb still prints it, `superseded`. Newest
            // first, its successor was met already, so a chain resolves to the
            // entry at its head.
            if let Some(by) = told_on_by(log, rec) {
                let standing = told_on
                    .iter()
                    .find(|(id, _)| *id == by.raw())
                    .map_or(by.raw(), |(_, standing)| *standing);
                told_on.push((rec.id.raw(), standing));
                match began.iter_mut().find(|(id, _)| *id == standing) {
                    Some((_, at)) => *at = (*at).min(rec.stamp.unix_ms),
                    None => began.push((standing, rec.stamp.unix_ms)),
                }
                continue;
            }
            let live = center.live(rec.id);
            let on_glass = glass
                .iter()
                .find(|(id, _)| *id == rec.id)
                .map(|(_, row)| *row);
            let exception = (live.is_none() && rec.later && rec.is_live())
                .then(|| center.digest_entry(rec.id))
                .flatten();
            let view = MessageView::of(rec, live, exception.as_ref(), on_glass, host);
            match counts.iter_mut().find(|(tag, _)| *tag == view.tag) {
                Some((_, n)) => *n += 1,
                None => counts.push((view.tag.clone(), 1)),
            }
            if view.state == "review" {
                to_look_at.push(view);
            } else {
                entries.push(view);
            }
        }
        if to_look_at.len() >= 2
            && let Some(first) = to_look_at.first_mut()
        {
            first.actions.push(MessageActionView {
                index: DISMISS_ALL_ACTION,
                label: DISMISS_ALL_LABEL,
                still_actionable: true,
                declines: true,
            });
        }
        entries.splice(0..0, to_look_at);
        for (id, at) in began {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.began_unix_ms = entry.began_unix_ms.min(at);
            }
        }
        let mut chips: Vec<(String, usize)> = tags::ALL
            .iter()
            .map(|tag| tag.as_str())
            .filter_map(|tag| {
                counts
                    .iter()
                    .find(|(seen, _)| seen == tag)
                    .map(|(_, n)| (tag.to_string(), *n))
            })
            .collect();
        chips.extend(
            counts
                .into_iter()
                .filter(|(tag, _)| !tags::ALL.iter().any(|own| own.as_str() == tag)),
        );
        Self {
            revision: center.projection_revision(),
            now_unix_ms,
            entries,
            told_on,
            tags: chips,
            log_folder: host.log_folder(),
            saved: host.saved(),
            utc_offset_s: host.utc_offset_s(),
        }
    }
}

/// Single message record projected for display on the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageView {
    /// Raw message ID.
    pub id: u64,
    /// Wall-clock timestamp (unix ms) at ingress.
    pub at_unix_ms: u64,
    /// Earliest wall-clock timestamp (unix ms) among the records this entry stands for.
    pub began_unix_ms: u64,
    /// Reporter family tag.
    pub tag: String,
    /// Wire severity word (`success`, `info`, `warn`, or `error`).
    pub severity: &'static str,
    /// Glyph painted at column 1 on the band.
    pub glyph: char,
    /// Entry title.
    pub title: String,
    /// Unwrapped detail lines.
    pub detail: Vec<String>,
    /// Wire state word ([`wire::state_word`]), or `aged` for an expired exception.
    pub state: &'static str,
    /// Band row index (`0` = topmost) while on glass.
    pub on_glass: Option<u8>,
    /// Count of duplicate posts folded into this record.
    pub repeats: u32,
    /// Wall-clock timestamp (unix ms) when the entry retired, if retired.
    pub retired_unix_ms: Option<u64>,
    /// ID of the entry that superseded this record, if superseded.
    pub superseded_by: Option<u64>,
    /// Capsule label chosen when answering a decision row, if any.
    pub answer: Option<String>,
    /// Authored intents offered on the page.
    pub actions: Vec<MessageActionView>,
    /// Number of leading `detail` lines that form the plain sentence above technical details.
    pub sentences: usize,
}

/// Action index for the page's per-entry `Dismiss` button.
pub const DISMISS_ACTION: u8 = 250;
/// Action index for the page's `Dismiss all` button.
pub const DISMISS_ALL_ACTION: u8 = 251;
/// Label for [`DISMISS_ACTION`].
pub const DISMISS_LABEL: &str = "Dismiss";
/// Label for [`DISMISS_ALL_ACTION`].
pub const DISMISS_ALL_LABEL: &str = "Dismiss all";

/// Authored intent projected as a page action button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageActionView {
    /// Action index reported when pressed.
    pub index: u8,
    /// Full capsule label (`Intent::label`).
    pub label: &'static str,
    /// Whether pressing this action can still be performed.
    pub still_actionable: bool,
    /// Whether this intent is a decline ([`Intent::is_decline`]).
    pub declines: bool,
}

impl MessageView {
    /// Projects a single record from `rec`, `live`, `on_glass`, and `host`.
    #[must_use]
    pub fn of(
        rec: &LogRecord,
        live: Option<&Live>,
        exception: Option<&DigestEntry>,
        on_glass: Option<u8>,
        host: &impl Host,
    ) -> Self {
        let (severity, glyph, title, detail, actions, repeats) = match (live, exception) {
            (Some(l), _) => (
                l.msg.severity,
                l.msg.glyph,
                &l.msg.title,
                &l.msg.detail,
                &l.msg.actions,
                l.repeats,
            ),
            (None, Some(e)) => (
                e.severity, e.glyph, &e.title, &e.detail, &e.actions, e.repeats,
            ),
            (None, None) => (
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
        let state = match wire::state_word(rec, live) {
            "unseen" if rec.later => "aged",
            other => other,
        };
        let open = live.is_some() || (rec.later && rec.is_live());
        let mut view = Self {
            id: rec.id.raw(),
            at_unix_ms: rec.stamp.unix_ms,
            began_unix_ms: rec.stamp.unix_ms,
            tag: rec.tag.as_str().to_string(),
            severity: severity.as_str(),
            glyph: cut_off_mark(glyph, severity, state).ch(),
            title: title.clone(),
            detail: detail.clone(),
            state,
            on_glass,
            repeats,
            retired_unix_ms: rec.retired_unix_ms,
            superseded_by,
            answer,
            sentences: host.sentence_lines(rec.key.as_deref(), detail),
            actions: actions
                .iter()
                .enumerate()
                .filter(|(_, intent)| open || rec.still_offers(intent))
                .filter(|(_, intent)| open || !session_gone(intent, host))
                .filter_map(|(k, intent)| {
                    Some(MessageActionView {
                        index: u8::try_from(k).ok()?,
                        label: intent.label(),
                        still_actionable: still_actionable(intent, open, host),
                        declines: intent.is_decline(),
                    })
                })
                .collect(),
        };
        if exception.is_some() && live.is_none() && rec.later && rec.is_live() {
            view.actions.push(MessageActionView {
                index: DISMISS_ACTION,
                label: DISMISS_LABEL,
                still_actionable: true,
                declines: true,
            });
        }
        view
    }

    /// Whether this entry's severity is `warn` or `error`.
    #[must_use]
    pub fn alarm(&self) -> bool {
        matches!(self.severity, "warn" | "error")
    }

    /// Plain-language state description for an expanded row's meta line.
    #[must_use]
    pub fn state_words(&self) -> String {
        let span_from = |from: u64| {
            self.retired_unix_ms
                .map(|at| span_words(at.saturating_sub(from)))
        };
        let span = span_from(self.at_unix_ms);
        let story = span_from(self.began_unix_ms);
        let after = |words: &str| match &span {
            Some(span) => format!("{words} after {span}"),
            None => words.to_string(),
        };
        match self.state {
            "live" | "held" => match self.on_glass {
                Some(_) => "showing now".to_string(),
                None => "waiting to show".to_string(),
            },
            "folded" if self.retired_unix_ms == Some(self.at_unix_ms) => self.record_words(),
            "folded" => match &span {
                Some(span) => format!("shown for {span}"),
                None => "shown".to_string(),
            },
            "stale" if self.retired_unix_ms.is_none() => {
                "still open when aterm stopped".to_string()
            }
            "stale" => after("stopped reporting"),
            "quit" => after("still open when aterm quit"),
            "unseen" => "not shown \u{2014} too many at once".to_string(),
            "aged" => after("aged out"),
            "superseded" => "replaced".to_string(),
            "resolved-ok" | "withdrawn" => match (&story, self.severity) {
                (Some(span), "success") => format!("took {span}"),
                (Some(span), _) => format!("lasted {span}"),
                (None, "success") => "finished".to_string(),
                (None, _) => "ended".to_string(),
            },
            "resolved-warn" => match &story {
                Some(span) => format!("ended with a problem after {span}"),
                None => "ended with a problem".to_string(),
            },
            "dismissed" => "dismissed".to_string(),
            "answered" => match &self.answer {
                Some(label) => format!("answered: {label}"),
                None => "answered".to_string(),
            },
            "recorded" => self.record_words(),
            "evicted" => "dropped \u{2014} too many at once".to_string(),
            "carried" => "carried over to the updated aterm".to_string(),
            "review" => "here and in the menu bar".to_string(),
            other => after(other),
        }
    }

    /// Plain-language state description for a retired record.
    #[must_use]
    pub fn record_words(&self) -> String {
        self.shown_line()
            .map_or_else(|| "recorded".to_string(), str::to_string)
    }

    /// Trailing `shown for …` detail line carried by an older log record, if any.
    #[must_use]
    pub fn shown_line(&self) -> Option<&str> {
        self.detail
            .iter()
            .map(String::as_str)
            .find(|line| line.starts_with("shown for "))
    }

    /// Splits `detail` into leading plain-sentence lines and remaining technical lines.
    #[must_use]
    pub fn sentence_and_technical(&self) -> (Vec<&str>, Vec<&str>) {
        let shown = self.shown_line();
        let mut lines = self
            .detail
            .iter()
            .map(String::as_str)
            .filter(|l| Some(*l) != shown);
        let sentence = if self.is_diagram() {
            Vec::new()
        } else {
            lines.by_ref().take(self.sentences.max(1)).collect()
        };
        (sentence, lines.collect())
    }

    /// Whether any detail line starts with whitespace (diagram formatting).
    #[must_use]
    pub fn is_diagram(&self) -> bool {
        self.detail
            .iter()
            .any(|l| l.starts_with(char::is_whitespace))
    }

    /// Formats the expanded entry's meta line (`<time> · <tag> · <severity> · <state>`).
    #[must_use]
    pub fn meta_words(&self, now_unix_ms: u64, utc_offset_s: i64) -> String {
        let meta = format!(
            "{} \u{00b7} {}",
            words::local_words(now_unix_ms, self.began_unix_ms, utc_offset_s),
            self.state_words()
        );
        if self.repeats > 1 {
            format!("{meta} \u{00b7} \u{00d7}{}", self.repeats)
        } else {
            meta
        }
    }

    /// Plain-language severity word (`Error`, `Warning`, `Info`, or `Done`).
    #[must_use]
    pub fn severity_words(&self) -> &'static str {
        match self.severity {
            "error" => "Error",
            "warn" => "Warning",
            "success" => "Done",
            _ => "Info",
        }
    }

    /// Formats this entry for clipboard copy.
    #[must_use]
    pub fn copy_text(&self) -> String {
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

/// Plain-language display name for `tag`.
#[must_use]
pub fn tag_words(tag: &str) -> Cow<'_, str> {
    Cow::Borrowed(match tag {
        "crash" => "Crashes",
        "config" => "Config",
        "update" => "Updates",
        "toolchain" | "packages" => "ALab tools",
        "privacy" => "Privacy",
        "session" => "Sessions",
        "window" => "Windows",
        "render" => "Display",
        "a11y" => "Accessibility",
        "fabric" | "harness" => "Agents",
        "system" => "System",
        "script" => "Scripts",
        other => return script_tag_words(other),
    })
}

/// Formats a script's own tag as a chip's display words.
fn script_tag_words(tag: &str) -> Cow<'_, str> {
    let mut chars = tag.chars();
    let words = match chars.next() {
        _ if tag.chars().count() <= 2 => Cow::Owned(tag.to_uppercase()),
        Some(first) if first.is_lowercase() => {
            Cow::Owned(first.to_uppercase().chain(chars).collect())
        }
        _ => Cow::Borrowed(tag),
    };
    if tags::ALL.iter().any(|own| tag_words(own.as_str()) == words) {
        Cow::Borrowed(tag)
    } else {
        words
    }
}

/// Formats duration `ms` as a compact span (`40 s`, `2 min`, `3 h`, `2 d`).
pub(crate) fn span_words(ms: u64) -> String {
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

/// Whether pressing `intent` from the page is still actionable ([`MessageActionView::still_actionable`]).
fn still_actionable(intent: &Intent, live: bool, host: &impl Host) -> bool {
    match intent {
        Intent::AgentUpgrade { tab, to, word } => host.upgrade_takes(tab, to, *word),
        Intent::NotNow { .. }
        | Intent::StopPaste { .. }
        | Intent::ShowSession { .. }
        | Intent::EndRecovered => live,
        Intent::ShowTab { .. } => false,
        Intent::ApplyUpdate { build } => host.staged_build() == Some(*build),
        Intent::OpenPath { path } => host.is_regular_file(path),
        Intent::Details
        | Intent::OpenSettings { .. }
        | Intent::OpenConfigEditor { .. }
        | Intent::OpenSystemPane { .. }
        | Intent::OpenLink { .. }
        | Intent::NewWindow
        | Intent::ReviewVaultSync => host.performs_navigation(),
    }
}

/// Returns the glyph and severity for an entry, demoting cut-off work (`stale` or `quit`).
pub(crate) fn cut_off_mark(glyph: Glyph, severity: Severity, state: &str) -> Glyph {
    const IN_FLIGHT: [char; 5] = ['\u{21bb}', '\u{21e3}', '\u{2191}', '\u{2296}', '\u{2026}'];
    if matches!(state, "stale" | "quit") && IN_FLIGHT.contains(&glyph.ch()) {
        severity.default_glyph()
    } else {
        glyph
    }
}

/// The later record that superseded `rec` ([`Retired::Superseded`]), unless `rec` is an error and
/// that record was not posted as one.
fn told_on_by(log: &crate::log::MessageLog, rec: &LogRecord) -> Option<MessageId> {
    let Some(Retired::Superseded { by }) = rec.retired() else {
        return None;
    };
    let next = log.get(*by).filter(|_| *by > rec.id)?;
    (rec.severity != Severity::Error || next.posted_severity == Severity::Error).then_some(*by)
}

/// Whether `intent` targets a tab whose upgrade this host no longer tracks ([`Host::upgrade_stands`]).
fn session_gone(intent: &Intent, host: &impl Host) -> bool {
    match intent {
        Intent::AgentUpgrade { tab, .. } => !host.upgrade_stands(tab),
        _ => false,
    }
}

/// Formats user-facing feedback after `intent` was performed or refused; `applies` is an
/// Install now press that installs here, or a System State press whose review shows the vault
/// sync's rows.
#[must_use]
#[allow(
    clippy::match_same_arms,
    reason = "each intent's refusal is its own sentence: two that read the same today stay two arms, in the order the guards need"
)]
pub fn act_feedback(intent: &Intent, performed: bool, applies: bool) -> String {
    match (intent, performed) {
        (Intent::Details, true) => "Opened".to_string(),
        (Intent::Details, false) => "Could not open the entry".to_string(),
        (Intent::OpenSettings { .. }, true) => {
            format!("Opened Settings \u{25b8} {}", intent.label())
        }
        (Intent::OpenSettings { .. }, false) => "Could not open Settings".to_string(),
        (Intent::OpenConfigEditor { .. }, true) => "Opened aterm.toml".to_string(),
        (Intent::OpenConfigEditor { .. }, false) => "Could not open aterm.toml".to_string(),
        (Intent::OpenPath { .. }, true) => "Opened the log".to_string(),
        (Intent::OpenPath { .. }, false) => "Could not open the log".to_string(),
        (Intent::OpenSystemPane { .. }, true) => "Opened System Settings".to_string(),
        (Intent::OpenSystemPane { .. }, false) => "System Settings did not open".to_string(),
        (Intent::OpenLink { .. }, true) => "Opened the page in your browser".to_string(),
        (Intent::OpenLink { .. }, false) => "Could not open the page".to_string(),
        (Intent::ApplyUpdate { .. }, true) if applies => "Installing the update".to_string(),
        (Intent::ApplyUpdate { .. }, true) => {
            "Opened Settings \u{25b8} Software Update".to_string()
        }
        (Intent::ApplyUpdate { .. }, false) if applies => "Could not start the install".to_string(),
        (Intent::ApplyUpdate { .. }, false) => "Could not open Settings".to_string(),
        (Intent::NotNow { .. }, _) => "Not now recorded".to_string(),
        (Intent::NewWindow, true) => "Opened a new window".to_string(),
        (Intent::NewWindow, false) => "Could not open a window".to_string(),
        (Intent::EndRecovered, true) => "Ended the reattached sessions".to_string(),
        (Intent::EndRecovered, false) => "The reattached sessions had already ended".to_string(),
        (Intent::StopPaste { .. }, true) => "Stopped the paste".to_string(),
        (Intent::StopPaste { .. }, false) => "The paste had already finished".to_string(),
        (Intent::ReviewVaultSync, true) if applies => {
            "Opened System State with the vault row on and the Mac settings rows off".to_string()
        }
        (Intent::ReviewVaultSync, true) => {
            "Opened System State; check its vault and Mac settings rows before Apply".to_string()
        }
        (Intent::ReviewVaultSync, false) => "Could not open System State".to_string(),
        (Intent::ShowTab { tab, .. }, true) => format!("Showed tab {tab}"),
        (Intent::ShowTab { .. }, false) => "That tab is gone".to_string(),
        (Intent::ShowSession { .. }, true) => "Showed session".to_string(),
        (Intent::ShowSession { .. }, false) => "That session is gone".to_string(),
        (Intent::AgentUpgrade { .. }, true) => {
            format!("Sent {}; what it did shows in this log", intent.label())
        }
        (Intent::AgentUpgrade { .. }, false) => "The upgrade no longer takes that word".to_string(),
    }
}

/// Active severity and tag filters on the Messages page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessagesFilter {
    /// Selected tag chip, if filtering by tag.
    pub tag: Option<String>,
    /// Whether to show only warnings and errors.
    pub warn_only: bool,
}

impl MessagesFilter {
    /// Whether `entry` passes both active filters.
    #[must_use]
    pub fn admits(&self, entry: &MessageView) -> bool {
        (!self.warn_only || entry.alarm())
            && self.tag.as_ref().is_none_or(|t| same_chip(t, &entry.tag))
    }

    /// Whether neither filter is active.
    #[must_use]
    pub const fn is_all(&self) -> bool {
        self.tag.is_none() && !self.warn_only
    }
}

/// Whether tags `a` and `b` map to the same display chip ([`tag_words`]).
#[must_use]
pub fn same_chip(a: &str, b: &str) -> bool {
    tag_words(a) == tag_words(b)
}

/// Formats the entry count line (`42 messages` or `12 of 42`).
#[must_use]
pub fn status_words(total: usize, shown: usize, all: bool) -> String {
    if all || total == 0 {
        format!("{total} message{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{shown} of {total}")
    }
}

/// Reader-local calendar context for formatting entry timestamps and day headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessagesClock {
    /// Wall-clock timestamp (unix ms) when the page was built.
    pub now_unix_ms: u64,
    /// Reader's UTC offset in seconds.
    pub offset_s: i64,
    /// Reader's current local day number ([`words::local_day`]).
    pub today: i64,
    /// Whether the entry list includes day section headers.
    pub headed: bool,
}

impl MessagesClock {
    /// Builds calendar context for `visible` entries at `now_unix_ms` and `offset_s`.
    #[must_use]
    pub fn of(visible: &[&MessageView], now_unix_ms: u64, offset_s: i64) -> Self {
        let day = |e: &&MessageView| words::local_day(e.at_unix_ms, offset_s);
        let today = words::local_day(now_unix_ms, offset_s);
        let headed = visible.iter().any(|e| day(e) < today);
        Self {
            now_unix_ms,
            offset_s,
            today,
            headed,
        }
    }

    /// Formats the lead-column timestamp for an entry at `at_unix_ms`.
    #[must_use]
    pub fn when(self, at_unix_ms: u64) -> String {
        if self.headed && words::local_day(at_unix_ms, self.offset_s) < self.today {
            words::clock_words(at_unix_ms, self.offset_s)
        } else {
            words::relative_words(self.now_unix_ms, at_unix_ms)
        }
    }

    /// Local day numbers for each entry in `visible` when [`Self::headed`] is true.
    #[must_use]
    pub fn sections(self, visible: &[&MessageView]) -> Option<Vec<i64>> {
        if !self.headed {
            return None;
        }
        let mut floor = self.today;
        let mut in_open_run = true;
        Some(
            visible
                .iter()
                .map(|e| {
                    if in_open_run && e.state != "review" {
                        in_open_run = false;
                        floor = self.today;
                    }
                    floor = floor.min(words::local_day(e.at_unix_ms, self.offset_s));
                    floor
                })
                .collect(),
        )
    }
}

/// Screen-reader description for `entry` at `now_unix_ms` (severity, tag, and relative time).
#[must_use]
pub fn description(entry: &MessageView, now_unix_ms: u64) -> String {
    format!(
        "{}, {}, {}",
        entry.severity_words(),
        tag_words(&entry.tag),
        words::spoken_relative_words(now_unix_ms, entry.at_unix_ms)
    )
}

/// Tag filter chip with its canonical tag, display label, and matching entry count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    /// First tag present that maps to this chip.
    pub tag: String,
    /// Display words for the chip.
    pub words: String,
    /// Count of entries matching this chip under the current severity filter.
    pub count: usize,
}

impl Chip {
    /// Formats the chip's painted and spoken label (`<words> · <count>`).
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} \u{00b7} {}", self.words, self.count)
    }
}

impl MessagesState {
    /// Tag chips in [`MessagesState::tags`] order with counts under `warn_only`.
    #[must_use]
    pub fn chips(&self, warn_only: bool) -> Vec<Chip> {
        let mut chips: Vec<Chip> = Vec::new();
        for (tag, _) in &self.tags {
            let words = tag_words(tag);
            if chips.iter().any(|chip| chip.words == words) {
                continue;
            }
            let count = self
                .entries
                .iter()
                .filter(|e| same_chip(tag, &e.tag) && (!warn_only || e.alarm()))
                .count();
            chips.push(Chip {
                tag: tag.clone(),
                words: words.into_owned(),
                count,
            });
        }
        chips
    }

    /// `(all, problems)` counts under `tag`, independent of the severity filter.
    #[must_use]
    pub fn severity_counts(&self, tag: Option<&str>) -> (usize, usize) {
        self.entries
            .iter()
            .filter(|e| tag.is_none_or(|t| same_chip(t, &e.tag)))
            .fold((0, 0), |(all, problems), e| {
                (all + 1, problems + usize::from(e.alarm()))
            })
    }
}

/// Formats `state` under `filter` as wire lines for CLI and test inspection.
#[must_use]
pub fn wire_lines(state: &MessagesState, filter: &MessagesFilter) -> Vec<String> {
    let visible: Vec<&MessageView> = state.entries.iter().filter(|e| filter.admits(e)).collect();
    let chips = state.chips(filter.warn_only);
    let clock = MessagesClock::of(&visible, state.now_unix_ms, state.utc_offset_s);
    let sections = clock.sections(&visible);
    let mut lines = Vec::with_capacity(1 + chips.len() + 2 * visible.len());
    lines.push(page_line(state, filter, visible.len()));
    let tag = filter.tag.as_deref();
    for chip in chips {
        let on = tag.is_some_and(|t| same_chip(t, &chip.tag));
        lines.push(
            Line::new("chip")
                .field("tag", &chip.tag)
                .field("words", &chip.words)
                .num("count", chip.count)
                .flag("on", on)
                .field("label", &chip.label())
                .0,
        );
    }
    let mut day_above = None;
    for (k, entry) in visible.iter().enumerate() {
        if let Some(day) = sections.as_ref().and_then(|s| s.get(k)).copied()
            && day_above != Some(day)
        {
            day_above = Some(day);
            lines.push(
                Line::new("day")
                    .num("day", day)
                    .field("words", &words::day_heading(clock.today, day, false))
                    .field("short", &words::day_heading(clock.today, day, true))
                    .0,
            );
        }
        lines.push(entry_line(state, clock, entry));
        let primary = primary_index(&entry.actions);
        for (k, action) in entry.actions.iter().enumerate() {
            lines.push(
                Line::new("action")
                    .num("id", entry.id)
                    .num("index", action.index)
                    .field("label", action.label)
                    .flag("actionable", action.still_actionable)
                    .field("button", &button_label(action.label))
                    .flag("primary", primary == Some(k))
                    .0,
            );
        }
    }
    lines
}

/// [`wire_lines`]' `page` line: the head, over the `shown` entries `filter`
/// admits, then the chrome.
fn page_line(state: &MessagesState, filter: &MessagesFilter, shown: usize) -> String {
    let total = state.entries.len();
    let tag = filter.tag.as_deref();
    let (all, problems) = state.severity_counts(tag);
    let status = if total == 0 {
        String::new()
    } else {
        status_words(total, shown, filter.is_all())
    };
    let mut head = Line::new("page")
        .num("rev", state.revision)
        .num("now", state.now_unix_ms)
        .num("total", total)
        .num("shown", shown)
        .num("all", all)
        .num("problems", problems)
        .field("status", &status);
    if !state.saved {
        head = head.field("note", NOT_SAVED);
    }
    if let Some(folder) = &state.log_folder {
        head = head.field("folder", folder);
    }
    let (all_words, problems_words) = severity_segment_words(all, problems);
    head.field("heading", HEADING)
        .field("subtitle", state.subtitle())
        .field("all_words", &all_words)
        .field("problems_words", &problems_words)
        .field("all_tags", ALL_TAGS)
        .flag("all_tags_on", tag.is_none())
        .field("tag_menu", &tag_menu_label(tag))
        .field("tag_menu_name", &tag_menu_name(tag))
        .field("copy_all_button", COPY_ALL)
        .field("copy_all_name", COPY_ALL_NAME)
        .field("empty", EMPTY)
        .field("technical_caption", TECHNICAL)
        .field("copy_button", COPY)
        .0
}

/// [`wire_lines`]' `entry` line for one admitted entry, its lead column's
/// words on `clock`.
fn entry_line(state: &MessagesState, clock: MessagesClock, entry: &MessageView) -> String {
    let (sentence, technical) = entry.sentence_and_technical();
    Line::new("entry")
        .num("id", entry.id)
        .num("at", entry.at_unix_ms)
        .field("when", &clock.when(entry.at_unix_ms))
        .field(
            "local",
            &words::local_words(state.now_unix_ms, entry.began_unix_ms, state.utc_offset_s),
        )
        .field("tag", &entry.tag)
        .field("tag_words", &tag_words(&entry.tag))
        .field("sev", entry.severity)
        .field("sev_words", entry.severity_words())
        .num("mark", entry.glyph)
        .field("title", &entry.title)
        .field("state", entry.state)
        .field("state_words", &entry.state_words())
        .num("rep", entry.repeats)
        .field("description", &description(entry, state.now_unix_ms))
        .lines("sentence", &sentence)
        .lines("technical", &technical)
        .field(
            "meta",
            &entry.meta_words(state.now_unix_ms, state.utc_offset_s),
        )
        .field("copy", &entry.copy_text())
        .0
}

/// One line of [`wire_lines`]' grammar as it is built: the kind word, then
/// each field.
struct Line(String);

impl Line {
    /// A line of `kind`.
    fn new(kind: &str) -> Self {
        Self(kind.to_string())
    }

    /// `key=value`, the value through the codec's escaping.
    fn field(mut self, key: &str, value: &str) -> Self {
        self.0.push('\t');
        self.0.push_str(key);
        self.0.push('=');
        escape_into(&mut self.0, value);
        self
    }

    /// `key=` a number (or a mark) as it prints.
    fn num(self, key: &str, value: impl std::fmt::Display) -> Self {
        self.field(key, &value.to_string())
    }

    /// `key=1` or `key=0`.
    fn flag(self, key: &str, on: bool) -> Self {
        self.field(key, if on { "1" } else { "0" })
    }

    /// `key=` several lines, joined by US (the codec's `detail=`).
    fn lines(self, key: &str, lines: &[&str]) -> Self {
        let owned: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
        self.field(key, &join_us(&owned))
    }
}
