// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SETTINGS ▸ MESSAGES PAGE'S MODEL (design §4.2, ruling 382): the
//! projection a host builds ONCE from the center ([`MessagesState::of`]) —
//! every record newest first, the live rows in their current words, the band
//! row each is on, whether each authored intent can still be pressed, the tags
//! in the chips' order — and the rules and words the page reads it through:
//! the plain words of a state, a severity, a tag and a span
//! ([`MessageView::state_words`], [`tag_words`]), Copy's text, the filters
//! ([`MessagesFilter`], [`same_chip`]), the chips and the severity counts
//! ([`MessagesState::chips`], [`MessagesState::severity_counts`]), an
//! entry's sentence and technical lines
//! ([`MessageView::sentence_and_technical`]), the count line
//! ([`status_words`]), the reader's calendar ([`MessagesClock`]), an entry's
//! spoken description ([`description`]) and the feedback line for a press
//! the host performed ([`act_feedback`]).
//!
//! And the page's CHROME words (ruling 395): its [`HEADING`] and
//! [`SUBTITLE`], the severity segments' `All · N` and `Problems · N`
//! ([`severity_segment_words`]), the `All tags` chip ([`ALL_TAGS`]) and each
//! chip's `ALab tools · 2` ([`Chip::label`]), the tag pop-up's `Tag: All`
//! ([`tag_menu_label`], [`tag_menu_name`]), `Copy All` ([`COPY_ALL`]) and
//! what it copies ([`copy_all_text`]), `Nothing yet.` ([`EMPTY`]), an
//! expanded entry's meta line ([`MessageView::meta_words`]), its `Technical
//! details` caption ([`TECHNICAL`]), its footer's `Open Manual` for the
//! intent's `Manual` ([`button_label`]) and its `Copy` ([`COPY`]). A host's
//! own affordances keep their words there: the macOS page's `Open Log
//! Folder` (the host opens its folder itself) and its `Explain heavy load`
//! switch (a native reporter's, in `aterm.toml`).
//!
//! The types keep the names they had in the macOS host (`MessagesState`,
//! `MessageView`, `MessageActionView`, `MessagesFilter`, `MessagesClock`): a
//! derived `Debug` prints a type's name, and the page's dumps read the same
//! byte for byte on either side of the move (ruling 385).
//!
//! The projection is built from the same [`LogRecord`]s the `messages` verb
//! prints (the Packages page's "screen == introspection" rule), with the
//! wire's own state words ([`wire::state_word`]), so the page and the verb
//! can never disagree. The page never reaches the center: a host hands the
//! built [`MessagesState`] to its view whole, lays it out and paints it.
//!
//! # The seam
//!
//! What only a host can know comes in through [`Host`]: the machine it runs
//! on (its wall clock, its log folder, whether the log is saved, its UTC
//! offset), the performers behind the intents (the staged build an
//! `Install now` installs, whether an `Open log`'s file is there, whether a
//! tab's agent upgrade still takes a word, whether it performs a navigation
//! at all) and one piece of reporter knowledge (how many leading detail
//! lines of a record are its sentence).
//! The RULE — which intent depends on which of those facts, and which are
//! pressable only while their row is live — is the engine's
//! ([`MessageActionView::still_actionable`]). The engine reads no clock and
//! touches no file.
//!
//! # The page as lines
//!
//! A host with no view of its own to hand the projection to — a web page,
//! which reads the module through strings — takes it as [`wire_lines`]: the
//! page's model under a filter with its words already said (the chips and
//! their counts, the count line, the day headers, each entry's lead, meta,
//! state, severity, chip and spoken words, its sentence and technical
//! lines, its Copy text, its offered capsules under their intents' own
//! labels and their footer buttons' words), in the log codec's line style
//! (design ruling 387), and the chrome words above with it (ruling 396).
//! Both web modules call it and word none of it; what a page that renders
//! the lines still words itself, [`wire_lines`]' doc lists.

use std::borrow::Cow;

use crate::center::{Live, MessageCenter};
use crate::log::{LogRecord, Retired, escape_into, join_us};
use crate::model::{Glyph, Intent, MessageId, Severity, UpgradeWord};
use crate::{wire, words};

/// The tag chips' fixed order on Settings ▸ Messages (design §4.3): the
/// families a person acts on first, then the ambient ones. Every word of the
/// reporter vocabulary is here (`page_tests::tags_chips_cover_the_vocabulary`
/// pins it); a tag this build does not know — a line another build wrote
/// into the same log — follows them in first-seen order rather than
/// vanishing.
pub const TAG_ORDER: [&str; 14] = [
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
    "script",
];

/// The not-saved note (design ruling 65): what a page over an in-memory ring
/// says, in words a person can act on.
pub const NOT_SAVED: &str =
    "These messages are kept only until aterm quits: the log file could not be opened.";

// THE PAGE'S CHROME WORDS (ruling 395): the words the page paints or names
// a control by that are not one entry's and that a page on any host shows —
// the heading and the subtitle, the filters' words, the report and Copy
// buttons, the empty log's line — so every host says the same page. A host's
// own affordances keep their words (the macOS page's `Open Log Folder` and
// its `Explain heavy load` switch), and so do its layout (its groups' names,
// a cut card's `… (N more lines)`) and its performers (`Copied`);
// `wire_lines`' doc lists them.

/// The page's heading: its name, which its list is read by too.
pub const HEADING: &str = "Messages";

/// The page's subtitle: what the log is (design §10.11). Most messages are
/// records that were never on the band, so the log is what aterm REPORTED,
/// not only what it told you. One line at every width (it fits the macOS
/// host's 286.5 pt phone page).
pub const SUBTITLE: &str = "What aterm reported, newest first.";

/// The page's one line where the log is empty, in place of its filters and
/// its list.
pub const EMPTY: &str = "Nothing yet.";

/// The first tag chip, which lets every tag through (ruling 262).
pub const ALL_TAGS: &str = "All tags";

/// Every expanded entry's last footer button: it puts
/// [`MessageView::copy_text`] on the clipboard.
pub const COPY: &str = "Copy";

/// The count line's report button: it puts [`copy_all_text`] on the
/// clipboard.
pub const COPY_ALL: &str = "Copy All";

/// [`COPY_ALL`]'s spoken name: what it copies.
pub const COPY_ALL_NAME: &str = "Copy the shown messages";

/// The caption over an expanded entry's technical lines
/// ([`MessageView::sentence_and_technical`]).
pub const TECHNICAL: &str = "Technical details";

/// THE SEVERITY SEGMENTS' WORDS (ruling 262), `(all, problems)`: `All · 42`
/// and `Problems · 23`, over the counts a press on each would show
/// ([`MessagesState::severity_counts`]). "Problems" is the warnings and the
/// errors together, and it is that one word everywhere — painted and spoken
/// alike (ruling 264).
#[must_use]
pub fn severity_segment_words(all: usize, problems: usize) -> (String, String) {
    (
        format!("All \u{00b7} {all}"),
        format!("Problems \u{00b7} {problems}"),
    )
}

/// The TAG POP-UP's painted words (ruling 262), the form the tag filter takes
/// on a compact page, and on a wider one where the severity segments and the
/// chips would not fit in two lines (ruling 267): `Tag: All` with no tag
/// down, else `Tag: ` and the chip's words ([`tag_words`]) —
/// `Tag: ALab tools`.
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

/// The label an entry's footer button carries on the page (ruling 262): a
/// destination reads as where it goes (`Open Packages` for the intent's
/// `Packages`, `Open Manual` for `Manual`); a verb stays itself (`Open log`,
/// `Upgrade now`). The band's capsule keeps the intent's own label.
#[must_use]
pub fn button_label(label: &str) -> String {
    match label {
        "Packages" | "Software Update" | "Manual" | "Appearance" | "Messages" | "Settings" => {
            format!("Open {label}")
        }
        other => other.to_string(),
    }
}

/// WHICH OFFERED INTENT IS THE PAGE'S PRIMARY — the button drawn in the accent, the
/// one a bare Return presses — as a position in `actions` (the offered capsules in
/// order, [`MessageView::actions`]), not an `ActionIndex`. ONE rule, the engine's, so
/// no page re-derives it (ruling 407; the lines carry it as `primary=`):
///
/// - the FIRST offered intent while a press can still perform it (ruling 265) — a
///   decline included: a first `Not now` that is pressable keeps it;
/// - past a first a press cannot perform (a dead Primary drawn in the accent read
///   as the thing to do: day ten, D2; ruling 401), the first LATER intent a press
///   can perform that is not a decline (ruling 403: `Skip version` past a dead
///   `Upgrade now` would be a decision drawn as the thing to do, and pressed by a
///   bare Return);
/// - none such — only declines left pressable, or nothing pressable — and none
///   leads (`None`, as for no intents at all).
#[must_use]
pub fn primary_index(actions: &[MessageActionView]) -> Option<usize> {
    match actions.first() {
        Some(first) if first.still_actionable => Some(0),
        _ => actions
            .iter()
            .position(|action| action.still_actionable && !action.declines),
    }
}

/// WHAT COPY ALL PUTS ON THE CLIPBOARD (design ruling 65): the host's build
/// information, then each entry `shown` — the ones the filters admit, newest
/// first — as its own Copy puts it ([`MessageView::copy_text`]), each after
/// two line breaks (`\n\n`): what a report needs, in one press. The build
/// information is the host's, taken verbatim: the macOS page's
/// (`about::provenance_text`) ends in a line break of its own, so its first
/// entry follows two blank lines and every later one a single blank line.
/// An empty `build` leads with the two line breaks all the same.
#[must_use]
pub fn copy_all_text<'a>(build: &str, shown: impl IntoIterator<Item = &'a MessageView>) -> String {
    let mut text = build.to_string();
    for entry in shown {
        text.push_str("\n\n");
        text.push_str(&entry.copy_text());
    }
    text
}

/// What only the HOST knows about one projection (design ruling 382): the
/// machine, the performers behind the intents, and one piece of reporter
/// knowledge. A host implements it on a value that holds what it read ONCE
/// for the build (the macOS host's `PageDesk` reads the staged build and the
/// upgrade clock once), so [`MessagesState::of`] may ask as often as it needs.
pub trait Host {
    /// The wall clock at the build, in milliseconds since the Unix epoch:
    /// what the page's relative times read from (the engine reads no clock).
    fn now_unix_ms(&self) -> u64;
    /// The folder the log files live in, as the page NAMES it (where there
    /// is no file manager to show it); `None` with no log dir. The host opens
    /// the folder itself, never a path from the page.
    fn log_folder(&self) -> Option<String>;
    /// Whether the messages are SAVED — the log file has a writer.
    fn saved(&self) -> bool;
    /// The reader's local clock's offset from UTC, in seconds; 0 where the
    /// host cannot say.
    fn utc_offset_s(&self) -> i64;
    /// The build the updater holds staged and ready right now, if any: an
    /// `Install now` for that build can still be pressed.
    fn staged_build(&self) -> Option<u64>;
    /// Whether `path` is there as a REGULAR file — the `Open log`
    /// performer's own test (a symlink planted in the log dir is refused at
    /// press time, so it is not offered either).
    fn is_regular_file(&self, path: &str) -> bool;
    /// Whether tab `tab`'s agent upgrade still moves to `to` and takes
    /// `word` — from a retired row or the waiting record too (the owner may
    /// change their mind; the press re-checks).
    fn upgrade_takes(&self, tab: &str, to: &str, word: UpgradeWord) -> bool;
    /// Whether this host still has tab `tab`'s agent upgrade at all (ruling
    /// 315): none means the tab's session ended here — a tab id never comes
    /// back — or its move is over.
    fn upgrade_stands(&self, tab: &str) -> bool;
    /// Whether a NAVIGATION pressed from the page — `Details ›`, a Settings
    /// page, `Open aterm.toml`, a System Settings pane, `New window` — is
    /// performed here, from any record, live or not (ruling 389). A host
    /// that performs no authored intent, or has no press route to a record
    /// that is no longer live, answers `false`, and no navigation is offered
    /// pressable.
    fn performs_navigation(&self) -> bool;
    /// How many leading `detail` lines of a record keyed `key` are the plain
    /// sentence for a person — the cause and the next step — above the
    /// technical lines; at least one (ruling 314). Reporter knowledge: the
    /// upgrade rows' key grammar and place words are the host's.
    fn sentence_lines(&self, key: Option<&str>, detail: &[String]) -> usize;
}

/// THE SHARED PROJECTION behind Settings ▸ Messages (design §4.2): built
/// ONCE ([`MessagesState::of`]) from the same [`LogRecord`]s the `messages` verb
/// prints and handed to the page whole. The page never reaches the center.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesState {
    /// The center's revision the projection was built at.
    pub revision: u64,
    /// The wall clock at the build, for the relative times.
    pub now_unix_ms: u64,
    /// Every record in the ring, NEWEST FIRST — at most `LOG_CAP`.
    pub entries: Vec<MessageView>,
    /// `(tag, count)` for every tag PRESENT, in [`TAG_ORDER`] then first-seen
    /// order — the page's filter chips.
    pub tags: Vec<(String, usize)>,
    /// The folder the log files live in (`aterm.log`, `messages.log`,
    /// `packages.log`, the crash reports) — what the page NAMES where there
    /// is no file manager to show it; the host opens it itself, never a path
    /// from the view. `None` with no log dir.
    pub log_folder: Option<String>,
    /// Whether these messages are SAVED — `messages.log` has a writer. A page
    /// over an in-memory ring only (a read-only log dir, a full disk) says so
    /// ([`NOT_SAVED`]): the person must not believe the record survives a
    /// quit.
    pub saved: bool,
    /// The reader's local clock's offset from UTC, in seconds — what an
    /// expanded entry's meta line reads its time on (`Today 12:53:49 PM`,
    /// design ruling 262); 0 where the host cannot say. Copy keeps UTC.
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

    /// THE PROJECTION, built once from the ring (design §4.2): every record
    /// newest first, the live rows in their current words, the band row each
    /// is on (echoes counted, design §10.5 M2), and whether each authored
    /// intent can still be pressed ([`MessageView::of`]). Tag counts in the
    /// chips' order: [`TAG_ORDER`] first, then every other tag present in
    /// first-seen (newest-first) order.
    #[must_use]
    pub fn of(center: &MessageCenter, host: &impl Host) -> Self {
        let now_unix_ms = host.now_unix_ms();
        // The band row each live row is on, echoes counted (design §10.5 M2).
        let glass: Vec<(MessageId, u8)> = center
            .on_glass()
            .filter_map(|l| {
                let row = center.glass_position(l.id)?;
                u8::try_from(row).ok().map(|row| (l.id, row))
            })
            .collect();
        let log = center.log();
        let mut entries = Vec::with_capacity(log.len());
        let mut counts: Vec<(String, usize)> = Vec::new();
        for rec in log.records().rev() {
            let live = center.live(rec.id);
            let on_glass = glass
                .iter()
                .find(|(id, _)| *id == rec.id)
                .map(|(_, row)| *row);
            let view = MessageView::of(rec, live, on_glass, host);
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
        Self {
            revision: center.revision(),
            now_unix_ms,
            entries,
            tags,
            log_folder: host.log_folder(),
            saved: host.saved(),
            utc_offset_s: host.utc_offset_s(),
        }
    }
}

/// One record as the page shows it: the live row's CURRENT words while it is
/// live (a restatement is never logged, so the record's own words would be
/// its first ones), the record's final words once retired — the same choice
/// `wire::message_row` makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageView {
    /// The raw message id.
    pub id: u64,
    /// The wall clock at ingress.
    pub at_unix_ms: u64,
    /// The reporter family.
    pub tag: String,
    /// The wire word: `success` / `info` / `warn` / `error`.
    pub severity: &'static str,
    /// The glyph the band paints at column 1.
    pub glyph: char,
    /// The title.
    pub title: String,
    /// The detail lines, pre-split, UNWRAPPED — the page wraps them to its
    /// own measure.
    pub detail: Vec<String>,
    /// The `state=` word the `messages` verb prints ([`wire::state_word`]):
    /// `live` / `held` / `folded` / `stale` / `unseen` / `superseded` /
    /// `resolved-ok` / `resolved-warn` / `dismissed` / `answered` /
    /// `evicted` / `carried` / `withdrawn` / `recorded` (a record, never on
    /// the band).
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
    /// — the cause and the next step — above the technical lines (at least
    /// one; ruling 314: [`Host::sentence_lines`]).
    pub sentences: usize,
}

/// One authored intent as the page offers it: a button by index, enabled only
/// while pressing it would still do the thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageActionView {
    /// The `ActionIndex` a press names.
    pub index: u8,
    /// The full capsule label (`Intent::label`).
    pub label: &'static str,
    /// Whether a press can still be performed (design §4.2): `Not now`,
    /// `Stop paste`, `Show tab N` and `End sessions` only while the row is
    /// live; `Install now` only while that build is still the staged one;
    /// `Open log` only while the file is there as a regular file — the
    /// performer's own test, so a capsule is not offered where the press
    /// would be refused; an agent upgrade's word while its tab's upgrade
    /// still takes it ([`Host::upgrade_takes`]); every navigation wherever
    /// the host performs one ([`Host::performs_navigation`]).
    pub still_actionable: bool,
    /// A decline — `Not now`, `Not today`, `Skip version`
    /// ([`Intent::is_decline`]): no page promotes one to its Primary past a
    /// first intent a press cannot perform (design ruling 403; the rule is
    /// [`primary_index`]). The page lines do not carry it; `label=` names it,
    /// and `primary=` carries the decision it takes part in.
    pub declines: bool,
}

impl MessageView {
    /// One record's view (the `wire::message_row` choice of words: the live
    /// row's while it is live, the record's once retired). `on_glass` is the
    /// band row it is on; `host` answers what only a host knows.
    #[must_use]
    pub fn of(
        rec: &LogRecord,
        live: Option<&Live>,
        on_glass: Option<u8>,
        host: &impl Host,
    ) -> Self {
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
        let state = wire::state_word(rec, live);
        Self {
            id: rec.id.raw(),
            at_unix_ms: rec.stamp.unix_ms,
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
                // now` on an answered ask, `Show tab 2` on a strain episode
                // that folded could never be pressed again, and a dead
                // Primary drawn in the accent read as the thing to do. An
                // intent that can come back (`Install now`, `Open log`)
                // stays, disabled while it cannot.
                .filter(|(_, intent)| live.is_some() || rec.still_offers(intent))
                // A WORD FOR A SESSION THAT IS GONE is not offered either
                // (ruling 315, day eight E3): `Upgrade now` / `Not today` on
                // a record whose tab's session no longer runs here stood
                // drawn, disabled, three relaunches later. A session never
                // comes back under its id, so the word never can; one whose
                // upgrade still stands but moved to another build stays,
                // disabled while it cannot.
                .filter(|(_, intent)| live.is_some() || !session_gone(intent, host))
                .filter_map(|(k, intent)| {
                    Some(MessageActionView {
                        index: u8::try_from(k).ok()?,
                        label: intent.label(),
                        still_actionable: still_actionable(intent, live.is_some(), host),
                        declines: intent.is_decline(),
                    })
                })
                .collect(),
        }
    }

    /// Warn or Error: the "Problems" chip's set (ruling 264).
    #[must_use]
    pub fn alarm(&self) -> bool {
        matches!(self.severity, "warn" | "error")
    }

    /// The state in PLAIN words for the expanded row's meta line (the owner's
    /// rule of 2026-09-23 — no internal jargon; design ruling 66): what the
    /// row is doing, and — once it has left — how long it stood: `showing
    /// now`, `waiting to show`, `shown for 2 min`, `recorded` (never on the
    /// band), `replaced by a newer
    /// message after 40 s`, `answered: Not now`, `took 40 s` (delivered
    /// or fixed), `lasted 40 s` (ended with no outcome), `ended with a problem
    /// after 3 h`. The wire keeps its own words ([`MessageView::state`], `copy_text`).
    #[must_use]
    pub fn state_words(&self) -> String {
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
            "folded" if self.retired_unix_ms == Some(self.at_unix_ms) => self.record_words(),
            "folded" => match &span {
                Some(span) => format!("shown for {span}"),
                None => "shown".to_string(),
            },
            // A row THIS process retired for going silent blames its
            // reporter; a record replayed open from a process that is gone
            // (killed or crashed — it wrote no ending, and a replayed record
            // carries no retirement time) does not (ruling 267).
            "stale" if self.retired_unix_ms.is_none() => {
                "still open when aterm stopped".to_string()
            }
            "stale" => after("stopped reporting"),
            // Cut off by a graceful quit (ruling 267): the reporter did not
            // end it, aterm did.
            "quit" => after("still open when aterm quit"),
            "unseen" => "not shown \u{2014} too many at once".to_string(),
            "superseded" => "replaced".to_string(),
            // The outcome the echo showed, never "cleared" over a failure
            // (ruling 93: `resolve(Warn)` is failed or refused; audit
            // 2026-09-24). Work that was delivered or a problem that was
            // fixed (its record now reads ✓ Success, ruling 265) TOOK its
            // span; anything else withdrawn LASTED it — never the wire's
            // `withdrawn`, nor `done` under a fixed warning.
            "resolved-ok" | "withdrawn" => match (&span, self.severity) {
                (Some(span), "success") => format!("took {span}"),
                (Some(span), _) => format!("lasted {span}"),
                (None, "success") => "finished".to_string(),
                (None, _) => "ended".to_string(),
            },
            "resolved-warn" => after("ended with a problem"),
            "dismissed" => "dismissed".to_string(),
            "answered" => match &self.answer {
                Some(label) => format!("answered: {label}"),
                None => "answered".to_string(),
            },
            // A record (`Retired::Recorded`, the log's `how=folded rec=1`) was
            // never on the band: "shown for 0 s" would say it had been (design
            // §10.11, ruling 149 — one word, main's).
            "recorded" => self.record_words(),
            "evicted" => "dropped \u{2014} too many at once".to_string(),
            "carried" => "carried over to the updated aterm".to_string(),
            other => after(other),
        }
    }

    /// A RECORD's state words (design ruling 262, keeping ruling 149): a record
    /// that stands for something that WAS on the band — the strain episode's,
    /// whose own row stood on the glass — says how long it was shown (`shown for
    /// 41 s`, the line its reporter wrote); every other record was written
    /// down and never shown: `recorded`.
    #[must_use]
    pub fn record_words(&self) -> String {
        self.shown_line()
            .map_or_else(|| "recorded".to_string(), str::to_string)
    }

    /// The `shown for …` line a record carries, when it has one.
    #[must_use]
    pub fn shown_line(&self) -> Option<&str> {
        self.detail
            .iter()
            .map(String::as_str)
            .find(|line| line.starts_with("shown for "))
    }

    /// The detail lines as an expanded entry sets them (ruling 314): the
    /// leading [`MessageView::sentences`] lines (at least one) are the
    /// SENTENCE — the cause and the next step, a person's words — and the
    /// rest the TECHNICAL lines. A diagram (any line that opens with
    /// whitespace: a caret under a column) is technical whole, and the
    /// `shown for …` line a record carries is its meta line's
    /// ([`MessageView::record_words`]), so it is in neither. Unwrapped: a
    /// host wraps each to its own measure.
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

    /// Whether the detail is a DIAGRAM — any line that opens with
    /// whitespace, a caret under a column: its lines are all technical
    /// ([`MessageView::sentence_and_technical`]), and a host CUTS them to
    /// its measure rather than wrapping them, so a column stays a column.
    #[must_use]
    pub fn is_diagram(&self) -> bool {
        self.detail
            .iter()
            .any(|l| l.starts_with(char::is_whitespace))
    }

    /// An expanded entry's META LINE (ruling 262): the local time it was
    /// stamped ([`words::local_words`] on a clock `utc_offset_s` from UTC,
    /// read at `now_unix_ms`), its state in plain words
    /// ([`MessageView::state_words`]) and, for a post folded in more than
    /// once, `×N` — `Today 12:53:49 PM · showing now · ×3`. Unwrapped: a host
    /// wraps it to its own measure.
    #[must_use]
    pub fn meta_words(&self, now_unix_ms: u64, utc_offset_s: i64) -> String {
        let meta = format!(
            "{} \u{00b7} {}",
            words::local_words(now_unix_ms, self.at_unix_ms, utc_offset_s),
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

    /// What Copy puts on the clipboard: the title, then `<stamp> · <tag> ·
    /// <sev>`, then every detail line — byte for byte [`words::copy_text`] of
    /// the record (`copy_text_of_a_view_is_the_engines`), built here because
    /// the page holds views, not records.
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

/// A tag's chip and meta name in plain words (design ruling 66): `Updates`,
/// `ALab tools`, `Display` — the wire and the chips' action keys keep the tag
/// itself. A tag this build does not know reads as itself.
#[must_use]
pub fn tag_words(tag: &str) -> Cow<'_, str> {
    Cow::Borrowed(match tag {
        "crash" => "Crashes",
        "config" => "Config",
        "update" => "Updates",
        // One chip for the ALab lane's two tags, one for the agents' two
        // (design ruling 262): the wire keeps each tag.
        "toolchain" | "packages" => "ALab tools",
        "privacy" => "Privacy",
        "session" => "Sessions",
        "window" => "Windows",
        "render" => "Display",
        "a11y" => "Accessibility",
        "fabric" | "harness" => "Agents",
        "system" => "System",
        // A script's row that named no tag (ruling 265).
        "script" => "Scripts",
        other => return script_tag_words(other),
    })
}

/// A script's own tag as a chip's words (design ruling 265): capitalized like
/// every chip beside it — `search` → `Search`, `deploy` → `Deploy` — and a
/// two-letter tag is an initialism, `ci` → `CI`. The wire and Copy keep the
/// tag as the script wrote it.
///
/// NEVER ONE OF ATERM'S OWN CHIPS (ruling 390): chips merge by their words
/// ([`same_chip`]), so a script's `updates` capitalized to `Updates` would
/// count, filter and be spoken as aterm's own update lane — the tag the wire
/// refuses (ruling 171). A tag whose capitalized words are an aterm chip's
/// reads as the script wrote it (`updates`), a chip of its own.
fn script_tag_words(tag: &str) -> Cow<'_, str> {
    let mut chars = tag.chars();
    let words = match chars.next() {
        _ if tag.chars().count() <= 2 => Cow::Owned(tag.to_uppercase()),
        Some(first) if first.is_lowercase() => {
            Cow::Owned(first.to_uppercase().chain(chars).collect())
        }
        _ => Cow::Borrowed(tag),
    };
    if TAG_ORDER.iter().any(|own| tag_words(own) == words) {
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

/// Whether pressing `intent` from the page would still do the thing (the
/// rule on [`MessageActionView::still_actionable`]). The facts are the host's
/// ([`Host::staged_build`], [`Host::is_regular_file`],
/// [`Host::upgrade_takes`], [`Host::performs_navigation`]); the rule is this
/// match. `Open log` asks the
/// performer's own regular-file test — a symlink planted in the log dir is
/// refused there, so it is not offered here (the rest of that test needs the
/// log dir, which is a directory probe per view; the press re-validates the
/// whole rule).
fn still_actionable(intent: &Intent, live: bool, host: &impl Host) -> bool {
    match intent {
        // An upgrade's word is for the build its row named: offered while
        // the tab's upgrade still moves to it and takes that word — from a
        // retired row or the waiting record too (the owner may change their
        // mind); the press re-checks under the harness's lock.
        Intent::AgentUpgrade { tab, to, word } => host.upgrade_takes(tab, to, *word),
        // An agent's one row's word: while any tab it named still takes it.
        Intent::AgentUpgradeTabs { word, moves } => moves
            .iter()
            .any(|(tab, to)| host.upgrade_takes(tab, to, *word)),
        // A stop is for the paste still on its way: its row is live exactly
        // as long as the paste is watched; a strain row's tab is the one it
        // named while it was up.
        // The keeper's recovered sessions are ended from the live row only.
        Intent::NotNow { .. }
        | Intent::StopPaste { .. }
        | Intent::ShowTab { .. }
        | Intent::EndRecovered => live,
        Intent::ApplyUpdate { build } => host.staged_build() == Some(*build),
        Intent::OpenPath { path } => host.is_regular_file(path),
        // A navigation's moment never passes; whether it is pressable is
        // whether the host performs it at all (ruling 389).
        Intent::Details
        | Intent::OpenSettings { .. }
        | Intent::OpenConfigEditor { .. }
        | Intent::OpenSystemPane { .. }
        | Intent::NewWindow => host.performs_navigation(),
    }
}

/// THE MARK OF WORK CUT OFF (ruling 317, day eight E5): a row still moving
/// when aterm stopped (`stale`, replayed open from a process that is gone) or
/// quit (`quit`) is no longer in flight, so its record does not wear the
/// working mark it was posted with — `↻ Rendering the video` read, in the
/// log, as work still under way. Its severity's own mark stands in; every
/// other mark, and every other state, is kept.
pub(crate) fn cut_off_mark(glyph: Glyph, severity: Severity, state: &str) -> Glyph {
    const IN_FLIGHT: [char; 5] = ['\u{21bb}', '\u{21e3}', '\u{2191}', '\u{2296}', '\u{2026}'];
    if matches!(state, "stale" | "quit") && IN_FLIGHT.contains(&glyph.ch()) {
        severity.default_glyph()
    } else {
        glyph
    }
}

/// Whether `intent` is a word for a tab whose upgrade this host no longer
/// has at all (ruling 315, [`Host::upgrade_stands`]): the host hands over the
/// upgrade rows of its live tabs, so none for the intent's tab means its
/// session ended here — a tab id never comes back — or its move is over.
/// Either way the word is moot.
fn session_gone(intent: &Intent, host: &impl Host) -> bool {
    match intent {
        Intent::AgentUpgrade { tab, .. } => !host.upgrade_stands(tab),
        // An agent's one row's word: moot once every tab it named is gone.
        Intent::AgentUpgradeTabs { moves, .. } => {
            moves.iter().all(|(tab, _)| !host.upgrade_stands(tab))
        }
        _ => false,
    }
}

/// The page's feedback line for an act the host performed (`performed`) or
/// refused: what happened, in the words a person would use. `applies`: an
/// `Install now` press that really installs (the host's
/// `update_bar_press_applies`); otherwise the press opened Software Update,
/// and says so (audit 2026-09-24: "Installing the update" was said over a
/// page that opened, and over a press that failed).
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
        // Written off this thread: what it did lands on the page as a record.
        // Queued, not done (ruling 270): what it did lands as the pressed
        // entry's own words, or as a row if the harness refuses it.
        (Intent::AgentUpgrade { .. }, true) => {
            format!("Sent {}; what it did shows in this log", intent.label())
        }
        (Intent::AgentUpgrade { .. }, false) => "The upgrade no longer takes that word".to_string(),
        (Intent::AgentUpgradeTabs { .. }, true) => {
            format!(
                "Sent {} for each tab; what it did shows in this log",
                intent.label()
            )
        }
        (Intent::AgentUpgradeTabs { .. }, false) => {
            "No tab's upgrade takes that word any more".to_string()
        }
    }
}

/// What the filters let through (design ruling 262): a SEVERITY — All, or
/// Problems (warnings and errors together, ruling 264) — AND a TAG, or every
/// tag. The two combine: "updates that failed" is the Updates chip and
/// Problems together. A tag is
/// matched by the chip it shows under ([`tag_words`]),
/// so `fabric` and `harness` are one Agents chip and `toolchain` and
/// `packages` one `ALab tools` chip, and a deep link to either tag of a pair
/// selects that chip; the wire keeps each tag.
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

/// Whether two tags show under ONE chip (their plain words are the same).
#[must_use]
pub fn same_chip(a: &str, b: &str) -> bool {
    tag_words(a) == tag_words(b)
}

/// The count line's words: `42 messages`, or `12 of 42` under a filter. The
/// log is one scrolling list (ruling 264): the count is the whole of what
/// the filters admit, never a page.
#[must_use]
pub fn status_words(total: usize, shown: usize, all: bool) -> String {
    if all || total == 0 {
        format!("{total} message{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{shown} of {total}")
    }
}

/// THE LOG'S CALENDAR (design ruling 273): the reader's local day, and
/// whether any admitted entry is from an EARLIER day — only then does the
/// list carry day headers (a list all of today would spend a line saying what
/// every row's time already says). Under the headers a row older than today
/// says its local time (`9:41 PM`), since its header already names the day;
/// today's rows, and every row of an unheaded list, keep their relative
/// words (`3 h ago`). Round 18, day four (D12): headers came only when the
/// entries spanned two days, so a filter that left ONE past day showed no
/// heading and a raw `2026-09-24` in the time column.
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
        // EARLIER, never merely other: an entry stamped past midnight by a
        // writer whose clock runs ahead is today's (ruling 281) — it heads
        // no list of today alone, and its section is today's.
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

    /// Each entry's SECTION day, newest first, when the list is headed: its
    /// local day, never newer than the section above it — a record whose
    /// stamp runs ahead of its neighbours' (another writer's clock) stays in
    /// the section it is listed in, so every day has one header — and never
    /// newer than today (ruling 281: a stamp from tomorrow opened a section
    /// that read `Today` above today's own).
    #[must_use]
    pub fn sections(self, visible: &[&MessageView]) -> Option<Vec<i64>> {
        if !self.headed {
            return None;
        }
        let mut floor = self.today;
        Some(
            visible
                .iter()
                .map(|e| {
                    floor = floor.min(words::local_day(e.at_unix_ms, self.offset_s));
                    floor
                })
                .collect(),
        )
    }
}

/// An entry's name for a screen reader beyond its title (ruling 262): its
/// severity, its chip name and when, in words — `Warning, ALab tools, 6 hours
/// ago` — never the wire's `warn · toolchain`.
#[must_use]
pub fn description(entry: &MessageView, now_unix_ms: u64) -> String {
    format!(
        "{}, {}, {}",
        entry.severity_words(),
        tag_words(&entry.tag),
        words::spoken_relative_words(now_unix_ms, entry.at_unix_ms)
    )
}

/// One TAG CHIP as the page offers it (design ruling 262): the first tag
/// present — in [`MessagesState::tags`]' order — that shows under it, which
/// is the filter a press sets; the chip's words ([`tag_words`]); and how many
/// entries a press would show under the severity filter that is down.
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
    /// The chip's painted and spoken words: its name and its count —
    /// `ALab tools · 2` (ruling 262).
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} \u{00b7} {}", self.words, self.count)
    }
}

impl MessagesState {
    /// The TAG CHIPS, one per chip NAME present (ruling 262) — `fabric` and
    /// `harness` one Agents chip, `toolchain` and `packages` one `ALab tools`
    /// chip, a script's own tag its own — in [`MessagesState::tags`]' order,
    /// each counting what a press would show under the severity that is down
    /// (`warn_only`: Problems, ruling 264).
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

    /// The SEVERITY segments' counts (ruling 262), `(all, problems)`: every
    /// entry, and the warnings and errors together (ruling 264), under the
    /// chip of `tag` — every tag with none. Independent of the severity that
    /// is down: each is what a press on its segment would show.
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

/// THE PAGE AS LINES (design ruling 387): the page under `filter` with
/// every word said, for a host whose view reads the page through a string
/// (a web page) — its model, and since ruling 396 its chrome: the words the
/// macOS page paints and names its controls by that a page on any host
/// shows. What the page that renders the lines still words or decides
/// itself is listed at the end. One line per item, none carrying a `\n`: a
/// KIND word, then TAB-separated `key=value` fields — the `messages.log`
/// codec's line style, through its escaping (`\` → `\\`, TAB → `\t`, LF →
/// `\n`, CR → `\r`, US → `\u`), a field that is a LIST of lines joined by
/// US first (the codec's `detail=`). Keys come in a fixed order, a key added
/// later after every older key of its kind; a reader ignores a key or a kind
/// it does not know (the codec's forward-compatible rule). A bare key carries
/// the wire's word (the `messages` verb's `tag=`, `sev=`, `state=`), a
/// `…_words` key the person's.
///
/// - `page rev= now= total= shown= all= problems= status= [note=] [folder=]
///   heading= subtitle= all_words= problems_words= all_tags= all_tags_on=
///   tag_menu= tag_menu_name= copy_all_button= copy_all_name= empty=
///   technical_caption= copy_button=`, first and once: the projection's
///   revision and wall clock; the ring's records and how many the filter
///   admits; the severity segments' counts under the tag filter
///   ([`MessagesState::severity_counts`]); the count line ([`status_words`]
///   — empty for an empty log, which shows no count); the not-saved note
///   ([`NOT_SAVED`]) only while unsaved; the log folder only where there is
///   one. Then the chrome (ruling 396): the [`HEADING`] and the
///   [`SUBTITLE`]; the severity segments' words over those counts
///   ([`severity_segment_words`]); the first tag chip's words ([`ALL_TAGS`])
///   and whether it is down (no tag filter); the tag pop-up's painted words
///   and spoken name, the form the chips take on a compact page and where
///   they would not fit in two lines ([`tag_menu_label`],
///   [`tag_menu_name`]); the count line's report button and its spoken name
///   ([`COPY_ALL`], [`COPY_ALL_NAME`]), pressable while `total` is above 0
///   (on an empty log the macOS count line shows no count and its Copy All
///   not pressable; a filter that admits nothing leaves it pressable, and it
///   copies the build information alone); the empty log's line ([`EMPTY`] —
///   shown while `total=0`, under that count line, where the filters and the
///   list would be); the caption over an entry's technical lines
///   ([`TECHNICAL`]) and every entry's Copy button ([`COPY`]).
/// - `chip tag= words= count= on= label=`, one per chip in the chips' order
///   ([`MessagesState::chips`] under the severity filter), `on=1` on the one
///   the tag filter selects; `label=` the chip's painted words
///   ([`Chip::label`]: `ALab tools · 2`).
/// - `day day= words= short=`, a day header, only in a headed list
///   ([`MessagesClock`], ruling 273) and before its section's first entry:
///   the local day ([`words::local_day`]) and its heading
///   ([`words::day_heading`]) whole and short.
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
///   clipboard ([`MessageView::copy_text`]) — ONE text, not a list: its line
///   breaks are the escaping's `\n`, so it reads back byte for byte (a US
///   join would drop a US inside a line).
/// - `action id= index= label= actionable= button= primary=`, after its
///   entry, one per offered capsule in order: its entry, the `ActionIndex` a
///   press names, the full capsule label, whether a press can still be
///   performed ([`MessageActionView::still_actionable`]), the words its
///   footer button paints ([`button_label`]: `Open Manual` for `Manual`),
///   and `primary=1` on the entry's Primary alone ([`primary_index`], ruling
///   407: the first while it has `actionable=1`, else the first later one
///   with `actionable=1` whose `label=` is no decline — `Not now`, `Not
///   today`, `Skip version` — else none: rulings 265, 401 and 403). Every
///   key is appended after the older ones, so a reader of the older keys
///   reads on.
///
/// Not a field — what a page that renders the lines words or decides
/// itself:
/// - Copy All's text (ruling 397): the host's build information, then, for
///   every `entry` in list order, `\n\n` and its `copy=` ([`copy_all_text`]);
///   the lines carry every part but the build's, which the engine cannot
///   word. Whether Copy All is pressable follows `total` (above).
/// - A host's own affordances: the macOS page's `Open Log Folder` and its
///   `Explain heavy load` switch.
/// - The layout's words and choices: the names the macOS layout gives its
///   groups (`Severity`, `Tags`, `Message actions`), a bounded card's
///   `… (N more lines)`, and whether the tag filter takes the chips' form
///   or the pop-up's.
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
/// admits, then the chrome (ruling 396).
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
        .field("subtitle", SUBTITLE)
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
            &words::local_words(state.now_unix_ms, entry.at_unix_ms, state.utc_offset_s),
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
