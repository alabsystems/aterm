// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Messages page's model: the projection a host builds once
//! from the center ([`MessagesState::of`]) (every record newest first, one a
//! later row tells on as part of that row, the
//! live rows in their current words, the band row each is on, whether each
//! authored intent can still be pressed, the tags in the chips' order) and the
//! rules and words the page reads it through: the plain words of a state, a
//! severity, a tag and a span ([`MessageView::state_words`], [`tag_words`]),
//! Copy's text, the filters ([`MessagesFilter`], [`same_chip`]), the chips and
//! the severity counts ([`MessagesState::chips`],
//! [`MessagesState::severity_counts`]), an entry's sentence and technical
//! lines ([`MessageView::sentence_and_technical`]), the count line
//! ([`status_words`]), the reader's calendar ([`MessagesClock`]), an entry's
//! spoken description ([`description`]) and the feedback line for a press the
//! host performed ([`act_feedback`]).
//!
//! It also holds the page's chrome words ([`HEADING`], [`SUBTITLE`],
//! [`SUBTITLE_OPEN_FIRST`], [`severity_segment_words`], [`ALL_TAGS`], [`Chip::label`],
//! [`tag_menu_label`], [`tag_menu_name`], [`COPY_ALL`], [`copy_all_text`],
//! [`EMPTY`], [`NO_MATCH`], [`SHOW_ALL`], [`MessageView::meta_words`],
//! [`TECHNICAL`], [`button_label`], [`COPY`]). A host's own affordances keep their words
//! there (the macOS page's `Open Log Folder`).
//!
//! The types keep the names they have in the macOS host, so a derived
//! `Debug` dump reads the same on either side.
//!
//! The projection is built from the same [`LogRecord`]s the `messages` verb
//! prints, with the wire's own state words ([`wire::state_word`]), so the
//! page and the verb can never disagree. The page never reaches the center: a
//! host hands the built [`MessagesState`] to its view whole, lays it out and
//! paints it.
//!
//! # The seam
//!
//! What only a host can know comes in through [`Host`]: the machine it runs
//! on (its wall clock, its log folder, whether the log is saved, its UTC
//! offset), the performers behind the intents (the staged build an `Install
//! now` installs, whether an `Open log`'s file is there, whether a tab's agent
//! upgrade still takes a word, whether it performs a navigation at all) and
//! one piece of reporter knowledge (how many leading detail lines of a record
//! are its sentence). The rule (which intent depends on which of those facts,
//! and which are pressable only while their row is live) is the engine's
//! ([`MessageActionView::still_actionable`]). The engine reads no clock and
//! touches no file.
//!
//! # The page as lines
//!
//! A host with no view of its own to hand the projection to (a web page,
//! which reads the module through strings) takes it as [`wire_lines`]: the
//! page's model under a filter with its words already said, in the log
//! codec's line style, with the chrome words. What a page that renders the
//! lines still words itself, [`wire_lines`]' doc lists.

use std::borrow::Cow;

use crate::center::{Live, MessageCenter};
use crate::digest::DigestEntry;
use crate::log::{LogRecord, Retired, escape_into, join_us};
use crate::model::{Glyph, Intent, MessageId, Severity, UpgradeWord, tags};
use crate::{wire, words};

/// The not-saved note: what a page over an in-memory ring says, in words a
/// person can act on.
pub const NOT_SAVED: &str = "Log file could not be opened; messages are kept until aterm quits.";

// The page's chrome words, so every host says the same page.

/// The page's heading: its name, which its list is read by too.
pub const HEADING: &str = "Messages";

/// The page's subtitle: most messages are records that were never on the band, so the
/// log is what aterm reported, not only what it told you.
pub const SUBTITLE: &str = "What aterm reported, newest first.";

/// [`SUBTITLE`] while an open exception leads the list: one is placed first
/// whatever its age ([`MessagesState::of`]), so the list is not newest first
/// and the page says what it is instead ([`MessagesState::subtitle`]). One
/// line at every width.
pub const SUBTITLE_OPEN_FIRST: &str = "What aterm reported, open items first.";

/// The page's one line where the log is empty, in place of its filters and
/// its list.
pub const EMPTY: &str = "Nothing yet.";

/// The page's line where the log has messages but the filters admit none, in
/// place of the list.
pub const NO_MATCH: &str = "No messages match these filters.";

/// The button beside [`NO_MATCH`]: it lifts both filters.
pub const SHOW_ALL: &str = "Show All";

/// The first tag chip, which lets every tag through.
pub const ALL_TAGS: &str = "All tags";

/// Every expanded entry's last footer button: it puts
/// [`MessageView::copy_text`] on the clipboard.
pub const COPY: &str = "Copy";

/// The count line's report button: it puts [`copy_all_text`] on the
/// clipboard.
pub const COPY_ALL: &str = "Copy All";

/// [`COPY_ALL`]'s spoken name: what it copies.
pub const COPY_ALL_NAME: &str = "Copy All shown messages";

/// The caption over an expanded entry's technical lines
/// ([`MessageView::sentence_and_technical`]).
pub const TECHNICAL: &str = "Technical details";

/// The severity segments' words, `(all, problems)`: `All · 42` and `Problems
/// · 23`, over the counts a press on each would show
/// ([`MessagesState::severity_counts`]). "Problems" is the warnings and the
/// errors together, painted and spoken alike.
#[must_use]
pub fn severity_segment_words(all: usize, problems: usize) -> (String, String) {
    (
        format!("All \u{00b7} {all}"),
        format!("Problems \u{00b7} {problems}"),
    )
}

/// The tag pop-up's painted words (the tag filter's form where the chips do not fit):
/// `Tag: All` with no tag down, else `Tag: ` and the chip's words ([`tag_words`]).
#[must_use]
pub fn tag_menu_label(tag: Option<&str>) -> String {
    format!("Tag: {}", tag_menu_current(tag))
}

/// The tag pop-up's spoken name: `Filter by tag: All`, `Filter by tag: ALab
/// tools` ([`tag_menu_label`]'s words, said whole).
#[must_use]
pub fn tag_menu_name(tag: Option<&str>) -> String {
    format!("Filter by tag: {}", tag_menu_current(tag))
}

/// The tag the pop-up says is down: `All`, or the chip's words.
fn tag_menu_current(tag: Option<&str>) -> Cow<'_, str> {
    tag.map_or(Cow::Borrowed("All"), tag_words)
}

/// The label an entry's footer button carries on the page: a destination
/// reads as where it goes (`Open Packages` for the intent's `Packages`, `Open
/// Manual` for `Manual`); a verb stays itself (`Open log`, `Upgrade now`). The
/// band's capsule keeps the intent's own label.
#[must_use]
pub fn button_label(label: &str) -> String {
    match label {
        "Packages" | "Software Update" | "Manual" | "Appearance" | "Messages" | "Security"
        | "Settings" => format!("Open {label}"),
        other => other.to_string(),
    }
}

/// Which offered intent is the page's Primary (the accent button a bare Return presses),
/// as a position in `actions` ([`MessageView::actions`]), not an `ActionIndex`. The lines
/// carry it as `primary=`:
///
/// - the first offered intent while a press can still perform it, a decline
///   included;
/// - past a first a press cannot perform, the first later intent a press can
///   perform that is not a decline (a decline drawn as the thing to do would
///   be pressed by a bare Return);
/// - none such (only declines left pressable, or nothing pressable): `None`,
///   as for no intents at all.
#[must_use]
pub fn primary_index(actions: &[MessageActionView]) -> Option<usize> {
    match actions.first() {
        Some(first) if first.still_actionable => Some(0),
        _ => actions
            .iter()
            .position(|action| action.still_actionable && !action.declines),
    }
}

/// What Copy All puts on the clipboard: the host's build information verbatim, then each
/// entry `shown` (the ones the filters admit, newest first) as its own Copy puts it
/// ([`MessageView::copy_text`]), each after `\n\n`.
#[must_use]
pub fn copy_all_text<'a>(build: &str, shown: impl IntoIterator<Item = &'a MessageView>) -> String {
    let mut text = build.to_string();
    for entry in shown {
        text.push_str("\n\n");
        text.push_str(&entry.copy_text());
    }
    text
}

/// What only the host knows about one projection: the machine, the performers behind the
/// intents, and one piece of reporter knowledge. Implement it on a value holding what the
/// host read once for the build, so [`MessagesState::of`] may ask as often as it needs.
pub trait Host {
    /// The wall clock at the build, in milliseconds since the Unix epoch:
    /// what the page's relative times read from (the engine reads no clock).
    fn now_unix_ms(&self) -> u64;
    /// The folder the log files live in, as the page names it (where there is
    /// no file manager to show it); `None` with no log dir. The host opens the
    /// folder itself, never a path from the page.
    fn log_folder(&self) -> Option<String>;
    /// Whether the messages are saved (the log file has a writer).
    fn saved(&self) -> bool;
    /// The reader's local clock's offset from UTC, in seconds; 0 where the
    /// host cannot say.
    fn utc_offset_s(&self) -> i64;
    /// The build the updater holds staged and ready right now, if any: an
    /// `Install now` for that build can still be pressed.
    fn staged_build(&self) -> Option<u64>;
    /// Whether `path` is there as a regular file: the `Open log` performer's
    /// own test (a symlink planted in the log dir is refused at press time, so
    /// it is not offered either).
    fn is_regular_file(&self, path: &str) -> bool;
    /// Whether tab `tab`'s agent upgrade still moves to `to` and takes `word`,
    /// from a retired row or the waiting record too (the person may change
    /// their mind; the press re-checks).
    fn upgrade_takes(&self, tab: &str, to: &str, word: UpgradeWord) -> bool;
    /// Whether this host still has tab `tab`'s agent upgrade at all: none
    /// means the tab's session ended here (a tab id never comes back) or its
    /// move is over.
    fn upgrade_stands(&self, tab: &str) -> bool;
    /// Whether a navigation pressed from the page (`Details ›`, a Settings
    /// page, `Open aterm.toml`, a System Settings pane, `New window`) is
    /// performed here, from any record, live or not. A host that performs no
    /// authored intent, or has no press route to a record that is not live,
    /// answers `false`, and no navigation is offered pressable.
    fn performs_navigation(&self) -> bool;
    /// How many leading `detail` lines of a record keyed `key` are the plain
    /// sentence for a person (the cause and the next step) above the technical
    /// lines; at least one. Reporter knowledge: the upgrade rows' key grammar
    /// and place words are the host's.
    fn sentence_lines(&self, key: Option<&str>, detail: &[String]) -> usize;
}

/// The shared projection behind Settings ▸ Messages, built once ([`MessagesState::of`])
/// and handed to the page whole. The page never reaches the center.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesState {
    /// The center's revision the projection was built at.
    pub revision: u64,
    /// The wall clock at the build, for the relative times.
    pub now_unix_ms: u64,
    /// Every record in the ring, newest first, but one a later row tells
    /// the story of on (it stands for it, [`Self::told_on`]); at most
    /// `LOG_CAP`.
    pub entries: Vec<MessageView>,
    /// `(told on, standing)`: every record of the ring a later row tells the
    /// story of on, by raw id, and the entry that stands for it now, a chain
    /// followed to its head. A host re-points what it holds by a record's id
    /// (the open entry, the focused row, a deep link) through it
    /// ([`Self::standing_for`]), so an entry that moved on stays open as the
    /// row that tells it now.
    pub told_on: Vec<(u64, u64)>,
    /// `(tag, count)` for every tag present, in [`tags::ALL`] then first-seen
    /// order: the page's filter chips.
    pub tags: Vec<(String, usize)>,
    /// The log folder as the page names it ([`Host::log_folder`]).
    pub log_folder: Option<String>,
    /// Whether these messages are saved (`messages.log` has a writer). A page over an
    /// in-memory ring only (a read-only log dir, a full disk) says so ([`NOT_SAVED`]).
    pub saved: bool,
    /// The reader's offset from UTC ([`Host::utc_offset_s`]), which an expanded entry's meta
    /// line reads its time on; Copy keeps UTC.
    pub utc_offset_s: i64,
}

impl MessagesState {
    /// The projection before the host has published one: nothing, revision 0
    /// (every real revision is higher, so the first publish always lands).
    #[must_use]
    pub fn empty() -> Self {
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

    /// The entry with this id, if the ring still holds it.
    #[must_use]
    pub fn entry(&self, id: u64) -> Option<&MessageView> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The page's subtitle over this projection: [`SUBTITLE_OPEN_FIRST`]
    /// while an open exception leads the list (it stands first whatever its
    /// age, so newer records follow it), else [`SUBTITLE`].
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

    /// The entry that stands for record `id` now, where a later row tells
    /// its story on ([`Self::told_on`]); `None` for a record that is its own
    /// entry, or one the ring no longer holds.
    #[must_use]
    pub fn standing_for(&self, id: u64) -> Option<u64> {
        self.told_on
            .iter()
            .find(|(told, _)| *told == id)
            .map(|(_, standing)| *standing)
    }

    /// When to reread this projection's time words: their next change on its
    /// own wall clock ([`words::words_change_after`] over every entry, the
    /// ones a filter hides too), or within a day to recheck the zone. `None`
    /// with no entry: an empty page says no time, so nothing is owed for the
    /// clock.
    ///
    /// `offset_at` is the reader's zone: its offset from UTC, in seconds, at
    /// an instant (unix ms). The words are read on [`Self::utc_offset_s`], the
    /// offset at the build; a zone that moves its offset re-reads every local
    /// clock word (`9:02 AM` becomes `10:02 AM`) and turns its day words at
    /// another midnight. With at most one offset change in the capped span,
    /// halving it finds the change within one second; the publish reads the
    /// zone again. This is not a search for arbitrary multiple transitions.
    /// A stamp far ahead of the clock can keep its words for months: the daily
    /// recheck keeps such a span from hiding a pair of seasonal changes whose
    /// ending offset is the build's again.
    #[must_use]
    pub fn words_change_at(&self, offset_at: impl Fn(u64) -> Option<i64>) -> Option<u64> {
        // The lead column reads each entry's own stamp, the meta line its
        // story's start.
        let at = self
            .entries
            .iter()
            .flat_map(|entry| [entry.at_unix_ms, entry.began_unix_ms])
            .map(|at| words::words_change_after(self.now_unix_ms, at, self.utc_offset_s))
            .min()?
            .min(self.now_unix_ms.saturating_add(86_400_000));
        // A zone that cannot be read at an instant keeps the build's offset.
        let same_offset =
            |instant: u64| offset_at(instant).is_none_or(|offset| offset == self.utc_offset_s);
        if same_offset(at) {
            return Some(at);
        }
        // The offset reads otherwise by `at`: the first second it does,
        // within one, and never before the build.
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

    /// The projection, built once from the ring: every record newest first
    /// (one a later row replaced is that row's past, not a row of its own,
    /// but a failure only a failure tells on: [`Self::told_on`]; the row
    /// standing for it starts when it did, [`MessageView::began_unix_ms`]),
    /// the live rows in their current words, the band row each is on (echoes
    /// counted), and whether each authored intent can still be pressed
    /// ([`MessageView::of`]). Tag counts in the chips' order: [`tags::ALL`]
    /// first, then every other tag present in first-seen (newest-first) order.
    #[must_use]
    pub fn of(center: &MessageCenter, host: &impl Host) -> Self {
        let now_unix_ms = host.now_unix_ms();
        // The band row each live row is on, echoes counted.
        let glass: Vec<(MessageId, u8)> = center
            .on_glass()
            .filter_map(|l| {
                let row = center.glass_position(l.id)?;
                u8::try_from(row).ok().map(|row| (l.id, row))
            })
            .collect();
        let log = center.log();
        let mut entries = Vec::with_capacity(log.len());
        // Open exceptions stand first, whatever their age, so they are never buried
        // under newer records.
        let mut to_look_at = Vec::new();
        let mut counts: Vec<(String, usize)> = Vec::new();
        // (told on, the entry standing for it), and each standing entry's
        // story start: the earliest stamp of the records it stands for.
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
        // Two or more open: the first carries `Dismiss all`, the menu-bar item's twin.
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

/// One record as the page shows it: the live row's current words while it is live (a
/// restatement is never logged), the record's final words once retired, as the wire's
/// `messages` rows choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageView {
    /// The raw message id.
    pub id: u64,
    /// The wall clock at ingress.
    pub at_unix_ms: u64,
    /// When what the entry tells began: the earliest stamp of the records it
    /// stands for ([`MessagesState::told_on`]: a lost login's sign-in before
    /// the cancel that replaced it), its own [`Self::at_unix_ms`] where it
    /// stands for none. The meta line's clock and an outcome's span (`took`,
    /// `lasted`, `ended with a problem after`) read from it; the lead column,
    /// the day sections, Copy and the row's own time on the band (`shown
    /// for`) keep the entry's stamp.
    pub began_unix_ms: u64,
    /// The reporter family.
    pub tag: String,
    /// The wire word: `success` / `info` / `warn` / `error`.
    pub severity: &'static str,
    /// The glyph the band paints at column 1.
    pub glyph: char,
    /// The title.
    pub title: String,
    /// The detail lines, pre-split, unwrapped: the page wraps them to its own
    /// measure.
    pub detail: Vec<String>,
    /// The `state=` word the `messages` verb prints ([`wire::state_word`]), save `aged` for
    /// an exception the wire calls `unseen`.
    pub state: &'static str,
    /// The band row (0 = topmost) while it is on the glass.
    pub on_glass: Option<u8>,
    /// Duplicate posts folded into this record (1 = posted once).
    pub repeats: u32,
    /// The wall clock it retired at, when it has.
    pub retired_unix_ms: Option<u64>,
    /// The id of the post that superseded it, when that is how it left.
    pub superseded_by: Option<u64>,
    /// The capsule a person answered a decision row with.
    pub answer: Option<String>,
    /// The authored intents, re-offered from the page.
    pub actions: Vec<MessageActionView>,
    /// How many leading `detail` lines are the plain sentence for a person
    /// (the cause and the next step) above the technical lines; at least one
    /// ([`Host::sentence_lines`]).
    pub sentences: usize,
}

/// The index of the page's own `Dismiss` button on an open exception. Authored capsules
/// number at most [`crate::MAX_ACTIONS`], so this index names no intent: a view-time
/// capsule (never in the record or the wire's `act` grammar) the host answers by filing
/// the entry.
pub const DISMISS_ACTION: u8 = 250;
/// The index of the page's `Dismiss all` button, offered on the first open exception
/// when two or more are open: files every entry, as the menu-bar item does.
pub const DISMISS_ALL_ACTION: u8 = 251;
/// The label of [`DISMISS_ACTION`].
pub const DISMISS_LABEL: &str = "Dismiss";
/// The label of [`DISMISS_ALL_ACTION`].
pub const DISMISS_ALL_LABEL: &str = "Dismiss all";

/// One authored intent as the page offers it: a button by index, enabled only
/// while pressing it would still do the thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageActionView {
    /// The `ActionIndex` a press names.
    pub index: u8,
    /// The full capsule label (`Intent::label`).
    pub label: &'static str,
    /// Whether a press can still be performed: `Not now`, `Stop paste`, `Show tab N` and
    /// `End sessions` only while the row is live; `Install now` only while that build is
    /// still staged; `Open log` only while the file is a regular file (the performer's own
    /// test); an agent upgrade's word while its tab's upgrade still takes it
    /// ([`Host::upgrade_takes`]); every navigation wherever the host performs one
    /// ([`Host::performs_navigation`]).
    pub still_actionable: bool,
    /// A decline ([`Intent::is_decline`]): no page promotes one to its Primary past a
    /// first intent a press cannot perform ([`primary_index`]). The page lines do not
    /// carry it; `primary=` carries the decision it takes part in.
    pub declines: bool,
}

impl MessageView {
    /// One record's view, in the live row's words while it is live, the record's once
    /// retired. `on_glass` is the band row it is on; `host` answers what only a host
    /// knows. `exception` is the digest entry of an open exception, whose current words
    /// the record lacks (a restatement is never logged).
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
        // The wire keeps `unseen`; on the page an exception that stood its 14 days is
        // aged out, not "too many at once" (the band queue's meaning of the word).
        let state = match wire::state_word(rec, live) {
            "unseen" if rec.later => "aged",
            other => other,
        };
        // An open exception is not in the live set (the digest is a store
        // beside it), but it is as open as a live row: its buttons are offered.
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
                // AN INTENT WHOSE MOMENT HAS PASSED IS NOT OFFERED (design
                // ruling 265): `Stop paste` on a paste that finished, `Not
                // now` on an answered ask, `Show tab 2` on a row
                // that folded could never be pressed again, and a dead
                // Primary drawn in the accent read as the thing to do. An
                // intent that can come back (`Install now`, `Open log`)
                // stays, disabled while it cannot.
                .filter(|(_, intent)| open || rec.still_offers(intent))
                // Nor is an upgrade word for a session that is gone: a session never
                // comes back under its id. One whose upgrade still stands but moved to
                // another build stays, disabled while it cannot.
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
        // An open exception can be put away from the page: a view-time `Dismiss` after
        // its own capsules. The menu-bar item files the whole digest; this files one.
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

    /// Warn or Error: the "Problems" chip's set.
    #[must_use]
    pub fn alarm(&self) -> bool {
        matches!(self.severity, "warn" | "error")
    }

    /// The state in plain words for the expanded row's meta line (no internal
    /// jargon): what the row is doing and, once it has left, how long it
    /// stood: `showing now`, `waiting to show`, `shown for 2 min`, `recorded`
    /// (never on the band), `replaced by a newer message after 40 s`,
    /// `answered: Not now`, `took 40 s` (delivered or fixed), `lasted 40 s`
    /// (ended with no outcome), `ended with a problem after 3 h`. An
    /// outcome's span (`took`, `lasted`, `ended with a problem after`) runs
    /// from when what the entry tells began ([`Self::began_unix_ms`]: a lost
    /// login fixed took from its first notice); the row's own time (`shown
    /// for`, `stopped reporting after`) from its own stamp. The wire keeps its
    /// own words ([`MessageView::state`], `copy_text`).
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
            // A record (`Hold::LogOnly`) retires at the instant it is posted; a row
            // that folded stood on the band for its hold, never zero.
            "folded" if self.retired_unix_ms == Some(self.at_unix_ms) => self.record_words(),
            "folded" => match &span {
                Some(span) => format!("shown for {span}"),
                None => "shown".to_string(),
            },
            // A row this process retired for going silent blames its reporter; a record
            // replayed open from a process that is gone (no retirement time) does not.
            "stale" if self.retired_unix_ms.is_none() => {
                "still open when aterm stopped".to_string()
            }
            "stale" => after("stopped reporting"),
            // Cut off by a graceful quit: aterm ended it, not the reporter.
            "quit" => after("still open when aterm quit"),
            "unseen" => "not shown \u{2014} too many at once".to_string(),
            "aged" => after("aged out"),
            "superseded" => "replaced".to_string(),
            // The outcome the echo showed, never "cleared" over a failure
            // (`resolve(Warn)` is failed or refused). Work that was delivered
            // or a problem that was fixed (its record now reads ✓ Success)
            // took its span; anything else withdrawn lasted it, never the
            // wire's `withdrawn`, nor `done` under a fixed warning.
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
            // A record (`Retired::Recorded`) was never on the band.
            "recorded" => self.record_words(),
            "evicted" => "dropped \u{2014} too many at once".to_string(),
            "carried" => "carried over to the updated aterm".to_string(),
            // An exception is in the page and the menu bar until it is dismissed.
            "review" => "here and in the menu bar".to_string(),
            other => after(other),
        }
    }

    /// A record's state words: a record that stands for something that was on
    /// the band (an episode's, whose own row stood on the glass) says
    /// how long it was shown (`shown for 41 s`, the line it carries; no
    /// reporter writes one now, but a record from an older log still does);
    /// every other record was written down and never shown: `recorded`.
    #[must_use]
    pub fn record_words(&self) -> String {
        self.shown_line()
            .map_or_else(|| "recorded".to_string(), str::to_string)
    }

    /// The `shown for …` line a record carries, when it has one (a record
    /// from an older log: the retired strain row's episodes wrote it).
    #[must_use]
    pub fn shown_line(&self) -> Option<&str> {
        self.detail
            .iter()
            .map(String::as_str)
            .find(|line| line.starts_with("shown for "))
    }

    /// The detail lines as an expanded entry sets them: the leading
    /// [`MessageView::sentences`] lines (at least one) are the sentence and the rest the
    /// technical lines. A diagram ([`MessageView::is_diagram`]) is technical whole, and a
    /// record's `shown for …` line belongs to its meta line, so it is in neither.
    /// Unwrapped: a host wraps each to its own measure.
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

    /// Whether the detail is a diagram (any line opens with whitespace: a caret under a
    /// column): its lines are all technical, and a host cuts them to its measure rather
    /// than wrapping them, so a column stays a column.
    #[must_use]
    pub fn is_diagram(&self) -> bool {
        self.detail
            .iter()
            .any(|l| l.starts_with(char::is_whitespace))
    }

    /// An expanded entry's meta line: the local time what it tells began
    /// ([`Self::began_unix_ms`], [`words::local_words`] on a clock
    /// `utc_offset_s` from UTC, read at `now_unix_ms`), its state in plain
    /// words ([`MessageView::state_words`]) and, for a post folded in more
    /// than once, `×N`: `Today 12:53:49 PM · showing now · ×3`. Unwrapped: a
    /// host wraps it to its own measure.
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

    /// The severity in plain words for the meta line: `Error`, `Warning`,
    /// `Info`, `Done` (the wire's `error` / `warn` / `info` / `success`).
    #[must_use]
    pub fn severity_words(&self) -> &'static str {
        match self.severity {
            "error" => "Error",
            "warn" => "Warning",
            "success" => "Done",
            _ => "Info",
        }
    }

    /// What Copy puts on the clipboard: the title, then `<stamp> · <tag> · <sev>`, then
    /// every detail line; byte for byte [`words::copy_text`] of the record, built here
    /// because the page holds views, not records.
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

/// A tag's chip and meta name in plain words: `Updates`, `ALab tools`,
/// `Display`; the wire and the chips' action keys keep the tag itself. A tag
/// this build does not know reads as itself.
#[must_use]
pub fn tag_words(tag: &str) -> Cow<'_, str> {
    Cow::Borrowed(match tag {
        "crash" => "Crashes",
        "config" => "Config",
        "update" => "Updates",
        // One chip for the ALab lane's two tags, one for the agents' two.
        "toolchain" | "packages" => "ALab tools",
        "privacy" => "Privacy",
        "session" => "Sessions",
        "window" => "Windows",
        "render" => "Display",
        "a11y" => "Accessibility",
        "fabric" | "harness" => "Agents",
        "system" => "System",
        // A script's row that named no tag.
        "script" => "Scripts",
        other => return script_tag_words(other),
    })
}

/// A script's own tag as a chip's words: capitalized like every chip beside it (`search`
/// → `Search`), a two-letter tag an initialism (`ci` → `CI`). Never one of aterm's own
/// chips: chips merge by their words ([`same_chip`]), so a tag whose capitalized words
/// are an aterm chip's (`updates`) reads as the script wrote it.
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

/// A span in words: `40 s`, `2 min`, `3 h`, `2 d`.
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

/// Whether pressing `intent` from the page would still do the thing
/// ([`MessageActionView::still_actionable`]): the facts are the host's, the rule is this
/// match, and the press re-validates it.
fn still_actionable(intent: &Intent, live: bool, host: &impl Host) -> bool {
    match intent {
        // An upgrade's word is for the build its row named, from a retired row too; the
        // press re-checks under the harness's lock.
        Intent::AgentUpgrade { tab, to, word } => host.upgrade_takes(tab, to, *word),
        // A stop is for the paste still on its way: its row is live exactly
        // as long as the paste is watched; a row's tab is the one it named
        // while it was up.
        // The keeper's recovered sessions are ended from the live row only.
        Intent::NotNow { .. }
        | Intent::StopPaste { .. }
        | Intent::ShowSession { .. }
        | Intent::EndRecovered => live,
        // An old ordinal has no destination identity to revalidate.
        Intent::ShowTab { .. } => false,
        Intent::ApplyUpdate { build } => host.staged_build() == Some(*build),
        Intent::OpenPath { path } => host.is_regular_file(path),
        // A navigation's moment never passes; whether it is pressable is
        // whether the host performs it at all.
        Intent::Details
        | Intent::OpenSettings { .. }
        | Intent::OpenConfigEditor { .. }
        | Intent::OpenSystemPane { .. }
        | Intent::OpenLink { .. }
        | Intent::NewWindow => host.performs_navigation(),
    }
}

/// The mark of work cut off: a row still moving when aterm stopped (`stale`) or quit
/// (`quit`) is not in flight any more, so its record does not wear the working mark it
/// was posted with; its severity's own mark stands in.
pub(crate) fn cut_off_mark(glyph: Glyph, severity: Severity, state: &str) -> Glyph {
    const IN_FLIGHT: [char; 5] = ['\u{21bb}', '\u{21e3}', '\u{2191}', '\u{2296}', '\u{2026}'];
    if matches!(state, "stale" | "quit") && IN_FLIGHT.contains(&glyph.ch()) {
        severity.default_glyph()
    } else {
        glyph
    }
}

/// The LATER record that tells `rec`'s story on, when there is one: the
/// record it was replaced by ([`Retired::Superseded`]), which stands for it on
/// the page. A replacement the ring no longer holds, or one that would point
/// back (no chain can loop), leaves the record its own row, `replaced`.
///
/// A FAILURE IS TOLD ON ONLY BY A FAILURE: an `✕` record stands for itself
/// unless the record that replaced it was posted an `✕` too
/// ([`LogRecord::posted_severity`]). A script's error replaced by a retry
/// under its key (`notice progress`, then `done ok`) keeps its own `replaced`
/// row in Problems, as an error whose row folded before the retry came does,
/// so whether a failure stays listed never hangs on when the next word came.
/// One failure told again in new words (a file that still does not load, a
/// disk still full) is one row, the latest, and stays one once that is fixed.
fn told_on_by(log: &crate::log::MessageLog, rec: &LogRecord) -> Option<MessageId> {
    let Some(Retired::Superseded { by }) = rec.retired() else {
        return None;
    };
    let next = log.get(*by).filter(|_| *by > rec.id)?;
    (rec.severity != Severity::Error || next.posted_severity == Severity::Error).then_some(*by)
}

/// Whether `intent` is a word for a tab whose upgrade this host does not have at all
/// ([`Host::upgrade_stands`]): its session ended here or its move is over, so the word is
/// moot.
fn session_gone(intent: &Intent, host: &impl Host) -> bool {
    match intent {
        Intent::AgentUpgrade { tab, .. } => !host.upgrade_stands(tab),
        _ => false,
    }
}

/// The page's feedback line for an act the host performed (`performed`) or refused, in
/// the words a person would use. `applies`: an `Install now` press that really installs;
/// otherwise the press opened Software Update, and says so.
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
        (Intent::ShowTab { tab, .. }, true) => format!("Showed tab {tab}"),
        (Intent::ShowTab { .. }, false) => "That tab is gone".to_string(),
        (Intent::ShowSession { .. }, true) => "Showed session".to_string(),
        (Intent::ShowSession { .. }, false) => "That session is gone".to_string(),
        // Written off this thread and queued, not done: what it did lands as
        // the pressed entry's own words, or as a row if the harness refuses it.
        (Intent::AgentUpgrade { .. }, true) => {
            format!("Sent {}; what it did shows in this log", intent.label())
        }
        (Intent::AgentUpgrade { .. }, false) => "The upgrade no longer takes that word".to_string(),
    }
}

/// What the filters let through: a severity (All, or Problems: warnings and errors) and a
/// tag, or every tag; the two combine. A tag is matched by the chip it shows under
/// ([`same_chip`]), so a deep link to either tag of a pair selects that chip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessagesFilter {
    /// Only this tag's chip.
    pub tag: Option<String>,
    /// Only Warn and Error.
    pub warn_only: bool,
}

impl MessagesFilter {
    /// Whether `entry` passes both filters.
    #[must_use]
    pub fn admits(&self, entry: &MessageView) -> bool {
        (!self.warn_only || entry.alarm())
            && self.tag.as_ref().is_none_or(|t| same_chip(t, &entry.tag))
    }

    /// Neither filter is down.
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.tag.is_none() && !self.warn_only
    }
}

/// Whether two tags show under one chip (their plain words are the same).
#[must_use]
pub fn same_chip(a: &str, b: &str) -> bool {
    tag_words(a) == tag_words(b)
}

/// The count line's words: `42 messages`, or `12 of 42` under a filter: the whole of what
/// the filters admit, never a page.
#[must_use]
pub fn status_words(total: usize, shown: usize, all: bool) -> String {
    if all || total == 0 {
        format!("{total} message{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{shown} of {total}")
    }
}

/// The log's calendar: the reader's local day, and whether any admitted entry is from an
/// earlier day; only then does the list carry day headers. Under the headers a row older
/// than today says its local time (`9:41 PM`); today's rows, and every row of an
/// unheaded list, say relative words (`3 h ago`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessagesClock {
    /// The wall clock the page was built at.
    pub now_unix_ms: u64,
    /// The reader's offset from UTC, in seconds.
    pub offset_s: i64,
    /// Today, as a [`words::local_day`].
    pub today: i64,
    /// Whether the list carries day headers.
    pub headed: bool,
}

impl MessagesClock {
    /// The calendar of the entries `visible` (the ones the filters admit),
    /// read at `now_unix_ms` on a clock `offset_s` seconds from UTC.
    #[must_use]
    pub fn of(visible: &[&MessageView], now_unix_ms: u64, offset_s: i64) -> Self {
        let day = |e: &&MessageView| words::local_day(e.at_unix_ms, offset_s);
        let today = words::local_day(now_unix_ms, offset_s);
        // Earlier, never merely other: an entry stamped past midnight by a writer whose
        // clock runs ahead is today's.
        let headed = visible.iter().any(|e| day(e) < today);
        Self {
            now_unix_ms,
            offset_s,
            today,
            headed,
        }
    }

    /// The lead column's words for an entry stamped `at`.
    #[must_use]
    pub fn when(self, at_unix_ms: u64) -> String {
        if self.headed && words::local_day(at_unix_ms, self.offset_s) < self.today {
            words::clock_words(at_unix_ms, self.offset_s)
        } else {
            words::relative_words(self.now_unix_ms, at_unix_ms)
        }
    }

    /// Each entry's section day, newest first, when the list is headed: its local day,
    /// never newer than the section above it (a stamp from another writer's clock stays in
    /// the section it is listed in) and never newer than today. The open exceptions listed
    /// first are a run of their own: the log under them starts again from today.
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

/// An entry's name for a screen reader beyond its title: severity, chip name and when, in
/// words (`Warning, ALab tools, 6 hours ago`).
#[must_use]
pub fn description(entry: &MessageView, now_unix_ms: u64) -> String {
    format!(
        "{}, {}, {}",
        entry.severity_words(),
        tag_words(&entry.tag),
        words::spoken_relative_words(now_unix_ms, entry.at_unix_ms)
    )
}

/// One tag chip as the page offers it: the tag a press filters by, the chip's words and
/// how many entries a press would show under the severity filter that is down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    /// The first tag present that shows under the chip.
    pub tag: String,
    /// The chip's words.
    pub words: String,
    /// The entries of the chip the severity filter admits.
    pub count: usize,
}

impl Chip {
    /// The chip's painted and spoken words: `ALab tools · 2`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} \u{00b7} {}", self.words, self.count)
    }
}

impl MessagesState {
    /// The tag chips, one per chip name present, in [`MessagesState::tags`]' order, each
    /// counting what a press would show under the severity that is down (`warn_only`:
    /// Problems).
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

    /// The severity segments' counts, `(all, problems)`, under the chip of `tag` (every
    /// tag with none), independent of the severity that is down.
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

/// The page as lines: the page under `filter` with every word said, for a host whose view
/// reads the page through a string (a web page). One line per item, none carrying a `\n`:
/// a kind word, then TAB-separated `key=value` fields in the `messages.log` codec's line
/// style and escaping (`\` → `\\`, TAB → `\t`, LF → `\n`, CR → `\r`, US → `\u`), a list of
/// lines joined by US first (the codec's `detail=`). Keys come in a fixed order, a key added
/// later after every older key of its kind; a reader ignores a key or a kind it does not
/// know. A bare key carries the wire's word (`tag=`, `sev=`, `state=`), a `…_words` key the
/// person's.
///
/// - `page rev= now= total= shown= all= problems= status= [note=] [folder=]
///   heading= subtitle= all_words= problems_words= all_tags= all_tags_on=
///   tag_menu= tag_menu_name= copy_all_button= copy_all_name= empty=
///   technical_caption= copy_button=`, first and once: the projection's
///   revision and wall clock; the ring's records and how many the filter
///   admits; the severity segments' counts under the tag filter
///   ([`MessagesState::severity_counts`]); the count line ([`status_words`],
///   empty for an empty log, which shows no count); the not-saved note
///   ([`NOT_SAVED`]) only while unsaved; the log folder only where there is
///   one. Then the chrome: the [`HEADING`] and the subtitle
///   ([`MessagesState::subtitle`]); the severity segments' words over those
///   counts ([`severity_segment_words`]); the first tag chip's words
///   ([`ALL_TAGS`]) and whether it is down (no tag filter);
///   the tag pop-up's painted words and spoken name ([`tag_menu_label`],
///   [`tag_menu_name`]); the count line's report button and its spoken name
///   ([`COPY_ALL`], [`COPY_ALL_NAME`]), pressable while `shown` is above 0 (a
///   filter that admits nothing has nothing to copy); the empty log's line
///   ([`EMPTY`], shown while `total=0`); the caption over an entry's
///   technical lines ([`TECHNICAL`]) and every entry's Copy button
///   ([`COPY`]).
/// - `chip tag= words= count= on= label=`, one per chip in the chips' order
///   ([`MessagesState::chips`] under the severity filter), `on=1` on the one
///   the tag filter selects; `label=` the chip's painted words
///   ([`Chip::label`]: `ALab tools · 2`).
/// - `day day= words= short=`, a day header, only in a headed list
///   ([`MessagesClock`]) and before its section's first entry: the local day
///   ([`words::local_day`]) and its heading ([`words::day_heading`]) whole
///   and short.
/// - `entry id= at= when= local= tag= tag_words= sev= sev_words= mark= title=
///   state= state_words= rep= description= sentence= technical= meta= copy=`,
///   one per admitted entry, newest first: the stamp; the lead column's words
///   ([`MessagesClock::when`]); the meta line's local time
///   ([`words::local_words`]); the tag and its chip's words; the severity and
///   its words; the mark painted at column 1; the title; the state and its
///   plain words ([`MessageView::state_words`]); the posts folded in; the
///   screen reader's [`description`]; the sentence and the technical lines
///   ([`MessageView::sentence_and_technical`]); the expanded entry's meta
///   line whole ([`MessageView::meta_words`]); and the text Copy puts on the
///   clipboard ([`MessageView::copy_text`]), one text, not a list: its line
///   breaks are the escaping's `\n`, so it reads back byte for byte.
/// - `action id= index= label= actionable= button= primary=`, after its
///   entry, one per offered capsule in order: its entry, the `ActionIndex` a
///   press names, the full capsule label, whether a press can still be
///   performed ([`MessageActionView::still_actionable`]), the words its
///   footer button paints ([`button_label`]), and `primary=1` on the entry's
///   Primary alone ([`primary_index`]).
///
/// Not a field (what a page that renders the lines words or decides itself):
/// - Copy All's text: the host's build information, then, for every `entry`
///   in list order, `\n\n` and its `copy=` ([`copy_all_text`]); the lines
///   carry every part but the build's.
/// - A host's own affordances: the macOS page's `Open Log Folder`, and the
///   line the page shows where messages exist but `shown=0` ([`NO_MATCH`],
///   with its [`SHOW_ALL`] button).
/// - The layout's words and choices: its groups' names (`Severity`, `Tags`,
///   `Message actions`), a bounded card's `… (N more lines)`, and whether the
///   tag filter takes the chips' form or the pop-up's.
/// - The performers' words, for a copy the host performs itself: the macOS
///   page's `Copying N messages…`, `Copied` and `Couldn’t copy: …`.
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
