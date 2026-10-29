// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission walk (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.4a): one helper panel
//! that takes the person through every macOS grant aterm still lacks, in [`Subject::ORDER`] —
//! Full Disk Access, Developer Tools, Accessibility (only for the Messages and FaceTime
//! sign-outs), then debugger access — each page opening its own System Settings deep link and
//! turning to the next a moment after its switch reads on ([`crate::devtools_helper::DONE_FOR`]),
//! or once the question macOS asks after it is gone.
//! Debugger access has no page: the walk has System State's once-per-Mac debugger step look at
//! once, and that step starts the change with the window's own admission at its own moment
//! (`docs/DESIGN-system-state-2026-10-21.md` §6). The walk is no person's press on the row's
//! `Turn On…`, so it never takes that door.
//!
//! * It starts by itself once, on a cold first launch ([`starts`]): no page aterm brought up
//!   before, System State in front of an active app, the person present ([`START_PRESENT_WITHIN`],
//!   read as the steps read it: [`App::person_present_within`]) and not typing
//!   ([`START_KEYS_QUIET`]), System Settings asking nothing (`App::system_settings_may_ask`), and
//!   System State's Local Network step run and its alert's hold over
//!   ([`App::local_network_step_to_come`]), so System Settings does not come up beside that
//!   alert; or that step still to come once it alone has held a ready walk for
//!   [`LOCAL_NETWORK_WAIT`] of unbroken looks and its own moment has not come
//!   ([`App::waited_out_local_network`]), after which the walk goes first and the step, and every
//!   step after it, the debugger's among them, asks once it is over: the one exception to the
//!   Local Network step going first. While it may still start and only the person's absence
//!   holds it, System State's later steps wait with it ([`App::walk_waits_for_the_person`]), so
//!   none takes its first page; the Local Network step does not. An update's successor never
//!   starts one. `aterm ctl permissions walk now` (at most once a minute, [`VERB_AGAIN_AFTER`])
//!   and the Permissions row's press start one; the verb sent while one is under way joins it
//!   ([`WalkLane::join`]), its pages before the debugger's, and takes back a page the walk passed
//!   only as often as it may start one. A verb's ask its control lane gave up before the main
//!   thread took it (the client answered `ERR late`, or gone) starts none
//!   ([`App::take_permission_walk_ask`]).
//! * A walk under way when System Settings' Quit & Reopen ends this process (the question macOS
//!   asks once Full Disk Access is switched on) leaves its pages, from the one it is at on, in
//!   `<state>` for the aterm that opens again ([`App::leave_walk_at_quit`]); that aterm's first
//!   look takes them, and they start a walk under the start's other rules though its pages came
//!   up before ([`App::take_walk_reopen`]).
//! * It is the owner's press (`devtools_helper::Origin::Walk`): no quiet moment, no ten-minute
//!   gap between its pages. A Not now holds the page it was said on, as on any panel. On a page
//!   asked for by name (the verb's subjects, the row pressed) the walk goes on only to another
//!   named page, and ends after the last, since the pages asked for were declined; on any other
//!   it goes on to the next. A verb that joins a walk names its pages only on a walk that names
//!   one, so the first launch's walk goes on from every Not now. The close box ends it. The
//!   control verb's walk opens its first page only while none of the window's other gestures
//!   and no macOS dialog of aterm's is up, and leaves out a page the owner's Not now holds back
//!   by its turn; a person's press on the row of the page it is at takes it over
//!   ([`WalkLane::pressed`]).
//! * A grant already held is skipped without opening its page ([`Walk::advance`]), and the
//!   panel's "Step 1 of N" counts only the pages that come up ([`Walk::position`]); while a page
//!   is up the window reads its grant every [`GRANT_POLL`]. Accessibility reading on asks macOS's
//!   Automation question too, and a walk that asked it opens no page while it is out
//!   ([`WalkReach::automation_out`]), so its next page never comes up beside that question.
//!   Full Disk Access reading on brings macOS's Quit & Reopen sheet over System Settings: the
//!   page's done words wait, naming the next page, until that sheet is gone and no quit is asked
//!   of this process ([`App::turn_after_fda`]), so the next page's link is never lost under it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{Access, AccessOutcome};

use crate::App;
use crate::devtools_helper::{Closed, HandOver, Moment, Origin, Page};

/// How recently the person must have touched the keyboard or pointer for the walk to start by
/// itself: present at the Mac, not away from it.
pub(crate) const START_PRESENT_WITHIN: Duration = Duration::from_secs(60);

/// How long the keyboard must have been still for the walk to start by itself: a word being
/// typed (a vault password, a first command) is not cut off by System Settings.
pub(crate) const START_KEYS_QUIET: Duration = Duration::from_secs(2);

/// How long the walk that starts by itself waits for System State's Local Network step
/// ([`App::local_network_step_to_come`]), counted over an unbroken run of looks at which the walk
/// was ready to start and only that step held it ([`App::waited_out_local_network`]): a look the
/// walk is not ready at (System State out of the front, a key typed, a gesture or walk up), and
/// System Settings asking something at the look the count runs out (where it is read, once per
/// wait), start the count again, as does focus back after none of aterm's
/// windows had it for `FOCUS_AWAY_FOR` or more, while a shorter flick of focus does not (the
/// steps' key rest reads the same away rule, `App::note_keyboard_focus`); a look at which the
/// step's own moment has come lets the step ask first; and any walk starting spends the count,
/// so the next walk by itself waits afresh. Past it the walk starts, and the step, which waits
/// out every gesture of the window's, the walk among them, asks once the walk is over. While the
/// walk is ready the step's moment differs from its start in its key rest
/// (`devtools_helper::QUIET_FOR`, not [`START_KEYS_QUIET`]) and focus-return rest, and the
/// launch's settle, which pass within seconds, and in parts that may last: an update hand-off
/// pending or in flight (one that stays parked), and a condition no reading here names. This ends
/// only a wait on such a part, which would otherwise hold the walk for good; presence and System
/// Settings asking the two read alike (`App::person_present_within`,
/// `App::system_settings_asks`), so neither ends it: the walk's presence, held through an
/// unbroken wait (`StartFacts::present`), spans at most two minutes from the last touch, inside
/// the steps' five (`devtools_helper::PRESENT_WITHIN`).
pub(crate) const LOCAL_NETWORK_WAIT: Duration = Duration::from_secs(60);

/// How often the window reads the grant the walk's page asks for while the panel is up, until
/// it shows its done words: the Full Disk Access probe, `AXIsProcessTrusted`, the Developer Tools
/// list.
pub(crate) const GRANT_POLL: Duration = Duration::from_millis(250);

/// How soon after the control verb started a walk, or took back a page the walk under way
/// passed, it may do either again: any program in a session can send the verb (the instance
/// token is readable by every program of this user), so it brings System Settings forward at
/// most once a minute, as the Developer Tools verb's `now` did before it.
pub(crate) const VERB_AGAIN_AFTER: Duration = Duration::from_secs(60);

/// How long a due page waits for the first reading of its grant (a launch's Full Disk Access
/// probe, the debugger rule) before it opens, or is tried, without one.
pub(crate) const UNREAD_WAIT: Duration = Duration::from_secs(3);

/// The marker in `<state>` a walk under way leaves at System Settings' Quit & Reopen for the
/// aterm that opens again ([`App::leave_walk_at_quit`]).
pub(crate) const REOPEN_MARKER: &str = "permission-walk-reopen";

/// The most a reopen marker holds: a line of seconds and words, a bundle's path and a Mac's
/// hardware UUID.
const MAX_REOPEN_BYTES: usize = 4096;

/// How long after the quit that wrote it a reopen marker starts a walk: the Quit & Reopen helper
/// opens aterm within [`crate::quit_reopen::REOPEN_CEILING`] of that quit (three minutes and
/// five seconds: its waits for the quitting aterm to be gone, for LaunchServices to drop it, and
/// for `open`), and the reopened aterm's first look comes seconds after its launch, a minute
/// allowed. One older is from a quit whose reopen never came, and an aterm the person opens
/// later starts no walk from it; one within it that another bundle or Mac wrote is left for the
/// aterm it names.
pub(crate) const REOPEN_WITHIN: Duration =
    Duration::from_secs(crate::quit_reopen::REOPEN_CEILING.as_secs() + 60);

/// A grant the walk asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Subject {
    /// Privacy & Security ▸ Full Disk Access.
    FullDiskAccess,
    /// Privacy & Security ▸ Developer Tools.
    DeveloperTools,
    /// Privacy & Security ▸ Accessibility: only the sign-outs need it.
    Accessibility,
    /// `DevToolsSecurity -enable`, with macOS's administrator dialog.
    Debugger,
}

impl Subject {
    /// The walk's order: Full Disk Access first, since without it the Developer Tools list
    /// cannot be read back; the debugger last, since its dialog asks for a password.
    pub(crate) const ORDER: [Self; 4] = [
        Self::FullDiskAccess,
        Self::DeveloperTools,
        Self::Accessibility,
        Self::Debugger,
    ];

    /// The subject's word on the control verb and in aterm.log.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "fda",
            Self::DeveloperTools => "devtools",
            Self::Accessibility => "accessibility",
            Self::Debugger => "debugger",
        }
    }

    /// The subject a control verb's word names: its [`Self::word`] or the row's name.
    pub(crate) fn parse(word: &str) -> Option<Self> {
        match word {
            "fda" | "full-disk-access" => Some(Self::FullDiskAccess),
            "devtools" | "developer-tools" => Some(Self::DeveloperTools),
            "accessibility" => Some(Self::Accessibility),
            "debugger" => Some(Self::Debugger),
            _ => None,
        }
    }

    /// The helper panel's page for the subject; `None` for the debugger, which System State's
    /// once-per-Mac step takes at its own moment, with macOS's administrator dialog as its one
    /// gesture and nothing of aterm's raised (`docs/DESIGN-system-state-2026-10-21.md` §6).
    pub(crate) const fn page(self) -> Option<Page> {
        match self {
            Self::FullDiskAccess => Some(Page::FullDiskAccess),
            Self::DeveloperTools => Some(Page::DeveloperTools),
            Self::Accessibility => Some(Page::Accessibility),
            Self::Debugger => None,
        }
    }

    /// The System State row whose grant the subject is: its [`Access::name`] names it on the
    /// panel.
    pub(crate) const fn access(self) -> Access {
        match self {
            Self::FullDiskAccess => Access::FullDiskAccess,
            Self::DeveloperTools => Access::DeveloperTools,
            Self::Accessibility => Access::Accessibility,
            Self::Debugger => Access::Debugger,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::FullDiskAccess => 0,
            Self::DeveloperTools => 1,
            Self::Accessibility => 2,
            Self::Debugger => 3,
        }
    }
}

/// What [`starts`] judges, read at the moment it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the start's independent readings, each a yes or no"
)]
pub(crate) struct StartFacts {
    /// The owner started this process (not an update's successor). Whether aterm brought up the
    /// Full Disk Access or Developer Tools page for this bundle on this Mac before (either page's
    /// opening of this TCC identity) is read only once these facts say start (`App::fresh_mac`).
    pub(crate) cold: bool,
    /// System State is the visible view of a focused window of the active app.
    pub(crate) presented: bool,
    /// A person is known to have been at the Mac within [`START_PRESENT_WITHIN`]
    /// (`App::person_present_within`: the keyboard or pointer touched then, by macOS's idle time,
    /// or, where macOS cannot give one, a key, click or scroll in an aterm window then), or the
    /// walk's wait for the Local Network step counts an unbroken run of looks the person was
    /// present at the start of (`App::local_network_wait_counting`). One touch so lets that wait
    /// run out where the idle time cannot be read and pointer motion is not seen, and the walk
    /// starts at most [`START_PRESENT_WITHIN`] plus [`LOCAL_NETWORK_WAIT`] after the last touch,
    /// within the steps' own presence (`devtools_helper::PRESENT_WITHIN`).
    pub(crate) present: bool,
    /// A key was typed within [`START_KEYS_QUIET`].
    pub(crate) typing: bool,
}

/// Whether the walk starts by itself now. Pure.
#[must_use]
pub(crate) const fn starts(facts: StartFacts) -> bool {
    facts.cold && facts.presented && facts.present && !facts.typing
}

/// Whether `word`, an opening `atpkg::devtools::PageMarkers::opened_as` found for `key`'s TCC
/// identity, stays one whatever bundle id and Mac `key` comes to carry: it names none that
/// `key`, written before they were read, lacks. Pure.
#[must_use]
fn found_for_good(word: &str, key: &str) -> bool {
    word.split('@').count() <= key.split('@').count()
}

/// What a look at the page markers found of this bundle's openings on this Mac ([`found`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// Neither page came up for it: the walk may start.
    Nothing,
    /// A page came up that might be another bundle's or Mac's until the key names this one: it
    /// holds the start at each look under the same key.
    ThisKey,
    /// A page came up that stays this bundle's on this Mac whatever the key comes to carry
    /// ([`found_for_good`]): no later look starts the walk.
    ForGood,
}

/// What the page markers say of `key`'s TCC identity, from `fda` and `devtools`, each page's
/// opening `atpkg::devtools::PageMarkers::opened_as` found for `key`. A Developer Tools opening
/// that names no Mac (the plain word a released aterm or `aterm doctor` writes alone, which
/// names every Mac) is told apart by the Full Disk Access markers, whose lines a window writes
/// with its bundle id and Mac. While they hold no opening of any identity, it might be this
/// Mac's and counts as one. While they hold this identity's (`fda`), `fda` decides alone. While
/// they hold only other identities' openings, it is theirs: the bundle's or Mac's whose
/// `<state>` this is (Migration Assistant's, a network home's). `fda_marked` reads whether the
/// Full Disk Access marker holds an opening of any identity, and is called only when the answer
/// counts. Pure but for `fda_marked`.
fn found(
    key: &str,
    fda: Option<&str>,
    fda_marked: impl FnOnce() -> bool,
    devtools: Option<&str>,
) -> Found {
    let names_a_mac = |word: &str| word.split('@').nth(2).is_some();
    let devtools = devtools.filter(|word| names_a_mac(word) || (fda.is_none() && !fda_marked()));
    let found = [fda, devtools];
    if found.iter().all(Option::is_none) {
        Found::Nothing
    } else if found.iter().flatten().any(|word| found_for_good(word, key)) {
        Found::ForGood
    } else {
        Found::ThisKey
    }
}

/// The subjects a walk takes: those `named`, in the order named, then the others in
/// [`Subject::ORDER`]; each once, and only while `missing` reads it missing. Pure.
pub(crate) fn subjects(named: &[Subject], missing: impl Fn(Subject) -> bool) -> Vec<Subject> {
    let mut seen = [false; 4];
    named
        .iter()
        .chain(Subject::ORDER.iter())
        .copied()
        .filter(|subject| !std::mem::replace(&mut seen[subject.index()], true))
        .filter(|subject| missing(*subject))
        .collect()
}

/// What a walk under way left at System Settings' Quit & Reopen ([`reopen_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reopen {
    /// The quit's unix seconds.
    at: u64,
    /// The walk's pages from the one it was at on, in its order.
    subjects: Vec<Subject>,
    /// The `.app` the Quit & Reopen opens again.
    bundle: PathBuf,
    /// The Mac it runs on, by its hardware UUID: `<state>` is shared by a network home's Macs.
    mac: String,
}

/// A reopen marker's text: the quit's unix seconds and the pages' words on the first line, the
/// bundle's path (spaces and all) on the second, the Mac's hardware UUID on the third. Pure.
fn reopen_text(at: u64, subjects: &[Subject], bundle: &str, mac: &str) -> String {
    let words: Vec<&str> = subjects.iter().map(|subject| subject.word()).collect();
    format!("{at} {}\n{bundle}\n{mac}\n", words.join(" "))
}

/// Whether `mac` can stand as a reopen marker's Mac: one word on its own line. Pure.
fn one_word(mac: &str) -> bool {
    !mac.is_empty() && !mac.contains(char::is_whitespace)
}

/// [`reopen_text`] read back: each page once, in its order, a word this aterm does not know
/// left out. `None` for anything else: no seconds, no page, a bundle path that is not absolute,
/// a Mac that is not one word, or a line more or fewer. Pure.
fn read_reopen(text: &str) -> Option<Reopen> {
    let mut lines = text.lines();
    let (first, bundle, mac) = (lines.next()?, lines.next()?, lines.next()?);
    if lines.next().is_some() || !one_word(mac) {
        return None;
    }
    let mut words = first.split(' ');
    let at = words.next()?.parse::<u64>().ok()?;
    let mut seen = [false; 4];
    let subjects: Vec<Subject> = words
        .filter_map(Subject::parse)
        .filter(|subject| !std::mem::replace(&mut seen[subject.index()], true))
        .collect();
    let bundle = PathBuf::from(bundle);
    (!subjects.is_empty() && bundle.is_absolute()).then_some(Reopen {
        at,
        subjects,
        bundle,
        mac: mac.to_owned(),
    })
}

/// Leave `subjects`, a walk's pages, in `state`'s reopen marker for the aterm `bundle` opens
/// again on the Mac `mac` (its hardware UUID) after a quit at `at` (unix seconds).
///
/// # Errors
/// `bundle`'s path is not one line of UTF-8, `mac` is not one word, or the marker could not be
/// written (`config_marker::write_marker`).
pub(crate) fn leave_reopen_in(
    state: &Path,
    at: u64,
    subjects: &[Subject],
    bundle: &Path,
    mac: &str,
) -> std::io::Result<()> {
    let bundle = bundle
        .to_str()
        .filter(|path| !path.contains(['\n', '\r']))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the bundle's path is not one line of UTF-8",
            )
        })?;
    if !one_word(mac) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the Mac's hardware UUID is not one word",
        ));
    }
    crate::config_marker::write_marker(
        &state.join(REOPEN_MARKER),
        &reopen_text(at, subjects, bundle, mac),
        MAX_REOPEN_BYTES,
        crate::durable::Flush::Ordered,
    )
}

/// Take `state`'s reopen marker when it is this aterm's: its pages when the quit that wrote it
/// was `bundle`'s (the `.app` this aterm runs from) on `mac` (this Mac's hardware UUID) within
/// [`REOPEN_WITHIN`] of `now` (unix seconds, either side, for a clock set back between the two
/// processes), the marker then removed so it starts no second walk. `None` for no marker, junk
/// or one outside that bound, each removed, and for another bundle's or Mac's within it, or any
/// while this aterm cannot name its bundle or Mac, left in place for the aterm it names: `<state>`
/// is shared by a network home's Macs and by a dev bundle beside the release.
pub(crate) fn take_reopen_in(
    state: &Path,
    now: u64,
    bundle: Option<&Path>,
    mac: Option<&str>,
) -> Option<Vec<Subject>> {
    let path = state.join(REOPEN_MARKER);
    let text = crate::config_marker::read_marker(&path, MAX_REOPEN_BYTES);
    let remove = || {
        if let Err(error) = crate::config_marker::remove_marker(&path) {
            aterm_log::warn!("permission walk: the Quit & Reopen marker was not removed: {error}");
        }
    };
    let Some(reopen) = text.as_deref().and_then(read_reopen) else {
        remove();
        if text.is_some() {
            aterm_log::info!("permission walk: a Quit & Reopen marker held no walk; it is removed");
        }
        return None;
    };
    if now.abs_diff(reopen.at) > REOPEN_WITHIN.as_secs() {
        remove();
        aterm_log::info!(
            "permission walk: the Quit & Reopen marker was written at {}, {} s from now; no \
             walk, and it is removed",
            reopen.at,
            now.abs_diff(reopen.at)
        );
        return None;
    }
    if bundle != Some(reopen.bundle.as_path()) || mac != Some(reopen.mac.as_str()) {
        aterm_log::info!(
            "permission walk: the Quit & Reopen marker is {}'s on the Mac {}, not this aterm's; \
             no walk, and it is left for that aterm",
            reopen.bundle.display(),
            reopen.mac
        );
        return None;
    }
    remove();
    Some(reopen.subjects)
}

/// Where the walk's current page stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Its page is still to open (or to be skipped, its grant held).
    Due,
    /// Its page is up, or on its way (a `spctl` run).
    Up,
}

/// One walk: its subjects and the one it is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Walk {
    subjects: Vec<Subject>,
    /// The subjects asked for by name (a row's press, the control verb), by [`Subject::ORDER`]:
    /// their pages open without waiting for a first reading.
    named: [bool; 4],
    at: usize,
    step: Step,
    /// When the current page became due.
    due_at: Instant,
    /// The pages before the current one that came up (or were on their way): the panel's count
    /// leaves out a page skipped, its grant held, declined or handed.
    shown: usize,
}

impl Walk {
    /// A walk over `subjects` from `now`; `None` when there are none.
    pub(crate) fn new(subjects: Vec<Subject>, now: Instant) -> Option<Self> {
        Self::named(subjects, &[], now)
    }

    /// A walk over `subjects`, those in `named` asked for by name.
    pub(crate) fn named(subjects: Vec<Subject>, named: &[Subject], now: Instant) -> Option<Self> {
        let named = Subject::ORDER.map(|subject| named.contains(&subject));
        (!subjects.is_empty()).then_some(Self {
            subjects,
            named,
            at: 0,
            step: Step::Due,
            due_at: now,
            shown: 0,
        })
    }

    /// Take `take` too, as a walk under way takes the control verb's ask: each subject the walk
    /// has not still to show goes on after its last page, before a debugger still to come (the
    /// current page when its step has not run: the debugger's never comes up, so a page taken
    /// while the walk waits there goes before it), so the debugger's step stays last. On a walk
    /// that asks for a page by name, each subject of `named` (each of `take` when `named` names
    /// none) counts as asked for by name; a walk that asks for none (the first launch's) goes on
    /// from every Not now, and its new pages are its own. Returns the subjects that went on.
    pub(crate) fn take_also(&mut self, take: &[Subject], named: &[Subject]) -> Vec<Subject> {
        let names = self.named.contains(&true);
        let named = if named.is_empty() { take } else { named };
        let from = if self.step == Step::Due {
            self.at
        } else {
            self.at + 1
        };
        let mut before_debugger = self
            .subjects
            .iter()
            .skip(from)
            .position(|subject| *subject == Subject::Debugger)
            .map(|at| from + at);
        let mut added = Vec::new();
        for subject in take {
            if !self.ahead().contains(subject) {
                match before_debugger.as_mut() {
                    Some(at) => {
                        self.subjects.insert(*at, *subject);
                        *at += 1;
                    }
                    None => self.subjects.push(*subject),
                }
                added.push(*subject);
            }
            if names && named.contains(subject) {
                self.named[subject.index()] = true;
            }
        }
        added
    }

    /// Whether the walk passed `subject`: it is one of its subjects that it has not still to show.
    pub(crate) fn passed(&self, subject: Subject) -> bool {
        self.subjects.contains(&subject) && !self.ahead().contains(&subject)
    }

    /// Whether taking `take` brings back a page the walk passed ([`Self::passed`]).
    pub(crate) fn takes_back(&self, take: &[Subject]) -> bool {
        take.iter().any(|subject| self.passed(*subject))
    }

    /// Whether the current page need wait no longer for its grant's first reading: it was asked
    /// for by name (the press opens it as it would a held one), or it has waited
    /// [`UNREAD_WAIT`].
    pub(crate) fn waited_for_reading(&self, now: Instant) -> bool {
        self.at_named() || now.saturating_duration_since(self.due_at) >= UNREAD_WAIT
    }

    /// How long the current page has been due.
    pub(crate) fn due_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.due_at)
    }

    /// The subject whose page the walk is at.
    pub(crate) fn current(&self) -> Option<Subject> {
        self.subjects.get(self.at).copied()
    }

    /// Whether the current page was asked for by name.
    pub(crate) fn at_named(&self) -> bool {
        self.current()
            .is_some_and(|subject| self.named[subject.index()])
    }

    /// Where its current page stands.
    pub(crate) const fn step(&self) -> Step {
        self.step
    }

    /// The current page's place, 1-based, and the walk's length, counting only the pages the
    /// person sees: the panel's "1 of N". Its place counts the pages before it that came up; the
    /// length adds each later subject whose grant does not read `held` now, as the walk would
    /// skip it at its turn ([`App::walk_reads_held`]), the debugger's step among them.
    pub(crate) fn position(&self, held: impl Fn(Subject) -> bool) -> (usize, usize) {
        let at = self.shown + 1;
        let later = self
            .ahead()
            .iter()
            .skip(1)
            .filter(|subject| !held(**subject))
            .count();
        (at, at + later)
    }

    /// The current page came up (or is on its way).
    pub(crate) const fn opened(&mut self) {
        self.step = Step::Up;
    }

    /// On to the next subject at `now` (its grant read on, a Not now, or held already): the
    /// next subject, `None` once the walk is over.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<Subject> {
        if self.step == Step::Up && self.at < self.subjects.len() {
            self.shown += 1;
        }
        self.at = self.at.saturating_add(1).min(self.subjects.len());
        self.step = Step::Due;
        self.due_at = now;
        self.current()
    }

    /// On past the current subject to the next one asked for by name, at `now`: the walk's way
    /// on from a Not now on a page asked for by name. `None` when no later one was.
    pub(crate) fn advance_to_named(&mut self, now: Instant) -> Option<Subject> {
        while self.advance(now).is_some() {
            if self.at_named() {
                return self.current();
            }
        }
        None
    }

    /// The subjects, in the walk's order.
    pub(crate) fn subjects(&self) -> &[Subject] {
        &self.subjects
    }

    /// The subjects from the current one on: the pages the walk has still to show.
    pub(crate) fn ahead(&self) -> &[Subject] {
        self.subjects.get(self.at..).unwrap_or_default()
    }

    /// The subject after the current one, when the walk has one: the panel is kept up between
    /// them.
    pub(crate) fn next(&self) -> Option<Subject> {
        self.subjects.get(self.at + 1).copied()
    }
}

/// What the walk reaches outside the App, injected so a test grants without macOS.
#[derive(Clone, Copy)]
pub(crate) struct WalkReach {
    /// `AXIsProcessTrusted`: aterm's own Accessibility grant, read in-process.
    pub(crate) ax_trusted: fn() -> bool,
    /// Send one harmless Apple event to System Events on a worker, so macOS asks its
    /// Automation question while the person is at the walk, not in the middle of a sign-out.
    pub(crate) ask_automation: fn(),
    /// Whether an ask's Apple event has not returned yet: macOS's Automation question may be on
    /// screen until the person answers it (or the ask's two-minute deadline passes).
    pub(crate) automation_out: fn() -> bool,
    /// The owner's newest Not now on the Accessibility page whatever its age, `(key, unix
    /// seconds)` (`accessibility-declined`, `atpkg::devtools::PageMarkers::last_declined`).
    pub(crate) ax_last_declined: fn() -> Option<(String, u64)>,
    /// Whether a Not now on the Accessibility page is inside its span
    /// (`atpkg::devtools::PageMarkers::declined_until`).
    pub(crate) ax_declined: fn() -> bool,
    /// Note the owner's Not now on the Accessibility page now, under a key.
    pub(crate) note_ax_declined: fn(&str),
    /// Whether the system or System Settings' Quit & Reopen asked this process to quit, by a
    /// quit nobody cancelled (`crate::system_quit::asked`): it is going, so the walk opens no
    /// page.
    pub(crate) quit_asked: fn() -> bool,
    /// Leave a walk's pages in `<state>`'s reopen marker for the aterm System Settings' Quit &
    /// Reopen opens again, with the quit's time, the `.app` this aterm runs from and this Mac.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only macOS's quit leaves a Quit & Reopen marker")
    )]
    pub(crate) leave_reopen: fn(&[Subject]),
    /// Take `<state>`'s reopen marker when it is this aterm's ([`take_reopen_in`]): the pages a
    /// walk left at a Quit & Reopen of this `.app` on this Mac within [`REOPEN_WITHIN`], the
    /// marker removed; another bundle's or Mac's is left in place for the aterm it names.
    pub(crate) take_reopen: fn() -> Option<Vec<Subject>>,
}

impl std::fmt::Debug for WalkReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkReach").finish_non_exhaustive()
    }
}

impl WalkReach {
    /// Nothing reached: never trusted, nothing asked, nothing out, no Not now read or noted, no
    /// quit asked, no reopen marker left or taken.
    pub(crate) const INERT: Self = Self {
        ax_trusted: || false,
        ask_automation: || {},
        automation_out: || false,
        ax_last_declined: || None,
        ax_declined: || false,
        note_ax_declined: |_| {},
        quit_asked: || false,
        leave_reopen: |_| {},
        take_reopen: || None,
    };

    /// The live arms of a windowed macOS instance.
    pub(crate) const fn live() -> Self {
        Self {
            ax_trusted: live::ax_trusted,
            ask_automation: live::ask_automation,
            automation_out: live::automation_out,
            ax_last_declined: live_ax_last_declined,
            ax_declined: live_ax_declined,
            note_ax_declined: live_note_ax_declined,
            quit_asked: crate::system_quit::asked,
            leave_reopen: live_leave_reopen,
            take_reopen: live_take_reopen,
        }
    }
}

/// The `.app` this aterm runs from, as the Quit & Reopen helper finds the one it opens again
/// (`crate::quit_reopen::leave_reopen_at_quit`).
fn this_bundle() -> Option<PathBuf> {
    aterm_containment::consent::fence_exe()
        .and_then(|exe| aterm_containment::consent::app_bundle_root(&exe))
}

fn live_leave_reopen(subjects: &[Subject]) {
    let (Some(state), Some(at), Some(bundle), Some(mac)) = (
        aterm_types::dirs::state_dir(),
        aterm_time::unix_secs(),
        this_bundle(),
        aterm_containment::consent::host_uuid(),
    ) else {
        aterm_log::warn!(
            "permission walk: no Quit & Reopen marker (no <state>, clock, .app or Mac to name)"
        );
        return;
    };
    let words: Vec<&str> = subjects.iter().map(|subject| subject.word()).collect();
    match leave_reopen_in(&state, at, subjects, &bundle, &mac) {
        Ok(()) => aterm_log::info!(
            "permission walk: Quit & Reopen; the aterm {} opens walks on from {}",
            bundle.display(),
            words.join(", ")
        ),
        Err(error) => {
            aterm_log::warn!("permission walk: the Quit & Reopen marker was not written: {error}");
        }
    }
}

fn live_take_reopen() -> Option<Vec<Subject>> {
    take_reopen_in(
        &aterm_types::dirs::state_dir()?,
        aterm_time::unix_secs()?,
        this_bundle().as_deref(),
        aterm_containment::consent::host_uuid().as_deref(),
    )
}

fn live_ax_last_declined() -> Option<(String, u64)> {
    atpkg::devtools::ACCESSIBILITY.last_declined()
}

fn live_ax_declined() -> bool {
    atpkg::devtools::ACCESSIBILITY.declined_until().is_some()
}

fn live_note_ax_declined(key: &str) {
    atpkg::devtools::ACCESSIBILITY.note_declined(key);
}

/// The App-owned walk lane: the walk under way, the subjects walks took this process, and the
/// reach.
#[derive(Debug)]
pub(crate) struct WalkLane {
    reach: WalkReach,
    walk: Option<Walk>,
    /// Subjects a walk took in this process, by [`Subject::ORDER`]: a later walk that starts by
    /// itself takes only a subject none took (Accessibility, once the sign-outs say they need it).
    walked: [bool; 4],
    /// Who started the walk under way, for aterm.log.
    by: &'static str,
    /// When the control verb last started a walk, or took back a page the walk under way passed
    /// ([`VERB_AGAIN_AFTER`]).
    verb_started_at: Option<Instant>,
    /// The walk under way yields to a helper panel it did not bring up: the control verb's,
    /// which any program in a session can send, never takes the owner's panel away, opens its
    /// first page only once it wins the dialog slot ([`App::reserve_walk_dialog`]), and leaves
    /// out a page the owner's Not now holds back by its turn.
    yields: bool,
    /// The yielding walk under way won the dialog slot and holds it: its next page takes the
    /// slot as won.
    slot_won: bool,
    /// A look found an opening of the Full Disk Access or Developer Tools page that is this
    /// bundle's on this Mac whatever Mac the key comes to name: a page was brought up here, so
    /// no walk starts by itself for the first time, and the markers are never read again in
    /// this process ([`App::fresh_mac`]).
    page_seen: bool,
    /// A walk gave way to a person's press elsewhere ([`Self::give_way`]): the subjects it had
    /// not gone past are free for the next walk that starts by itself, which no page marker then
    /// holds back.
    gave_way: bool,
    /// The key of the last look that found an opening that might be another bundle's or Mac's
    /// ([`Found::ThisKey`]): a look under the same key finds the same, so it reads no marker.
    held_under: Option<String>,
    /// The walk under way asked macOS's Automation question (its Accessibility page read on): it
    /// opens no page while that question is out ([`WalkReach::automation_out`]), unless a
    /// person's press takes it over ([`Self::pressed`]).
    asked_automation: bool,
    /// Whether `<state>` holds the Accessibility page back from the control verb
    /// ([`App::accessibility_declined_for_the_verb`]), and the key it was read under: read once
    /// per key, since the walk's start looks at every park, and held from the moment this
    /// process notes a Not now ([`Self::note_ax_declined`]).
    ax_declined: Option<(String, bool)>,
    /// What a walk left at a Quit & Reopen for this process ([`App::take_walk_reopen`]).
    reopened: Reopened,
    /// The walk's wait for System State's Local Network step ([`LOCAL_NETWORK_WAIT`]), from the
    /// first of an unbroken run of looks at which only that step held a start; spent by any walk
    /// that starts ([`Self::start`]).
    local_network_wait: Option<LocalNetworkWait>,
}

/// The walk's wait for System State's Local Network step ([`App::waited_out_local_network`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalNetworkWait {
    /// The approved revision whose step it waits for: another revision's step starts it again.
    revision: u64,
    /// The first of the unbroken run of looks at which the walk was ready to start and only the
    /// step still to come held it.
    since: Instant,
    /// The run lasted [`LOCAL_NETWORK_WAIT`]: the walk waits for that revision's step no more.
    over: bool,
}

/// The pages a walk under way left for the aterm System Settings' Quit & Reopen opened, as this
/// process holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reopened {
    /// The reopen marker is still to be taken: the first look takes it.
    Unread,
    /// Its pages wait for the start's other rules.
    Waits(Vec<Subject>),
    /// Taken: they started a walk, there were none, or another walk started first.
    Spent,
}

impl WalkLane {
    /// The lane for an instance: the live reach for a window on macOS, inert otherwise.
    pub(crate) fn new(headless: bool) -> Self {
        let reach = if !headless && cfg!(target_os = "macos") {
            WalkReach::live()
        } else {
            WalkReach::INERT
        };
        Self::with_reach(reach)
    }

    /// A lane with an injected reach.
    pub(crate) const fn with_reach(reach: WalkReach) -> Self {
        Self {
            reach,
            walk: None,
            walked: [false; 4],
            by: "",
            verb_started_at: None,
            yields: false,
            slot_won: false,
            page_seen: false,
            gave_way: false,
            held_under: None,
            asked_automation: false,
            ax_declined: None,
            reopened: Reopened::Unread,
            local_network_wait: None,
        }
    }

    pub(crate) const fn reach(&self) -> WalkReach {
        self.reach
    }

    /// The owner said Not now to the Accessibility page (or closed it unanswered): note it in
    /// `accessibility-declined` under `key`, so it holds the page back from the control verb in
    /// later processes too, and from now on in this one.
    pub(crate) fn note_ax_declined(&mut self, key: String) {
        (self.reach.note_ax_declined)(&key);
        self.ax_declined = Some((key, true));
    }

    /// The walk under way.
    pub(crate) const fn walk(&self) -> Option<&Walk> {
        self.walk.as_ref()
    }

    pub(crate) const fn walk_mut(&mut self) -> Option<&mut Walk> {
        self.walk.as_mut()
    }

    /// Whether a walk is under way.
    pub(crate) const fn active(&self) -> bool {
        self.walk.is_some()
    }

    /// Whether the walk under way is the control verb's (one that yields) and holds no dialog
    /// slot: its next page waits to win one ([`App::reserve_walk_dialog`]).
    pub(crate) const fn waits_for_slot(&self) -> bool {
        self.walk.is_some() && self.yields && !self.slot_won
    }

    /// Whether a walk in this process already took `subject`.
    pub(crate) const fn walked(&self, subject: Subject) -> bool {
        self.walked[subject.index()]
    }

    /// Whether any walk ran in this process.
    pub(crate) fn any_walked(&self) -> bool {
        self.walked.iter().any(|walked| *walked)
    }

    /// Start `walk`, started `by` (aterm.log's words); it replaces any under way and takes the
    /// pages it opens as the owner's press would. Pages a Quit & Reopen left that wait for the
    /// start's rules wait no more: this walk (a person's press, the control verb) came first.
    pub(crate) fn start(&mut self, walk: Walk, by: &'static str) {
        // A walk under way holds the Local Network step, and no look counts the walk's readiness
        // while it lasts, so a wait run before it (over or not) says nothing of the looks after
        // it: the next walk by itself waits [`LOCAL_NETWORK_WAIT`] afresh.
        self.local_network_wait = None;
        self.yields = false;
        self.slot_won = false;
        self.asked_automation = false;
        if self.reopened != Reopened::Unread {
            self.reopened = Reopened::Spent;
        }
        for subject in walk.subjects() {
            self.walked[subject.index()] = true;
        }
        aterm_log::info!(
            "permission walk ({by}): {}",
            walk.subjects()
                .iter()
                .map(|subject| subject.word())
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.by = by;
        self.walk = Some(walk);
    }

    /// The walk under way takes `take` too, asked `by` (aterm.log's words), counting those in
    /// `named` asked for by name as [`Walk::take_also`] does; nothing without a walk under way.
    /// Returns whether a page went on.
    pub(crate) fn join(&mut self, take: &[Subject], named: &[Subject], by: &'static str) -> bool {
        let Some(walk) = self.walk.as_mut() else {
            return false;
        };
        let added = walk.take_also(take, named);
        if added.is_empty() {
            return false;
        }
        for subject in &added {
            self.walked[subject.index()] = true;
        }
        aterm_log::info!(
            "permission walk ({}): {by} adds {}",
            self.by,
            added
                .iter()
                .map(|subject| subject.word())
                .collect::<Vec<_>>()
                .join(", ")
        );
        true
    }

    /// A person pressed the row of the page the walk is at: the walk under way is theirs from
    /// here on, so that page opens now, as their own walk's would, past whatever the control
    /// verb's walk waits out, past a Not now, and past the Automation question the walk asked,
    /// as a press opens its page beside any other question.
    pub(crate) fn pressed(&mut self) {
        self.asked_automation = false;
        if std::mem::replace(&mut self.yields, false) {
            aterm_log::info!(
                "permission walk ({}): a person's press takes it over",
                self.by
            );
        }
    }

    /// The walk is over (every page answered, or the close box).
    pub(crate) fn end(&mut self, why: &str) {
        if self.walk.take().is_some() {
            aterm_log::info!("permission walk ({}): over ({why})", self.by);
        }
    }

    /// The walk steps aside for a person's press elsewhere (`App::press_meets_unseen_panel`):
    /// it ends, and the subjects it had not gone past, its current page's included, are free
    /// again, so the walk that starts by itself at the next quiet moment (a cold first launch's,
    /// `App::look_for_walk_start`) takes them up. A press is no answer to a page it never saw.
    pub(crate) fn give_way(&mut self, why: &str) {
        let Some(walk) = self.walk.as_ref() else {
            return;
        };
        for subject in walk.ahead() {
            self.walked[subject.index()] = false;
        }
        self.gave_way = true;
        self.end(why);
    }
}

/// What the window's walk answered a start (`aterm ctl permissions walk now`, a row's press).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Started {
    /// A headless instance (or not macOS): nothing, ever.
    Headless,
    /// Every grant asked for is held: no walk.
    Granted,
    /// The control verb started a walk, or took back a page the walk under way passed, within
    /// [`VERB_AGAIN_AFTER`]: no walk starts, or a page asked for (any, when none is named) that
    /// the walk under way passed does not come back. No passed page comes back then; a page the
    /// walk never had joins it all the same.
    Recent,
    /// Every grant asked for (every grant, when none is named) that is missing is held back
    /// from the control verb by the owner's Not now.
    Declined,
    /// The walk is under way and has `pages` still to show: a new walk's subjects, or the pages
    /// of one already under way, which keeps going and takes the pages asked for. `not_now`
    /// are the subjects asked for (every subject, when none is named) whose grant is missing
    /// and which the owner's Not now holds back from the control verb, none of them in `pages`.
    Walking {
        pages: Vec<Subject>,
        not_now: Vec<Subject>,
    },
}

impl Started {
    /// The control verb's answer line: `OK walking` names each page still to show by its word
    /// and each page the owner's Not now holds back as `not-now:<word>`, a word that names no
    /// page, so a client that reads only the pages takes it for none.
    pub(crate) fn reply(&self) -> String {
        match self {
            Self::Headless => {
                "ERR headless: a headless instance brings no System Settings page up\n".to_owned()
            }
            Self::Granted => "OK granted\n".to_owned(),
            Self::Recent => "OK recent\n".to_owned(),
            Self::Declined => "OK declined\n".to_owned(),
            Self::Walking { pages, not_now } => format!(
                "OK walking {}\n",
                pages
                    .iter()
                    .map(|subject| subject.word().to_owned())
                    .chain(
                        not_now
                            .iter()
                            .map(|subject| format!("not-now:{}", subject.word()))
                    )
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
    }
}

/// Whether a subject's grant reads held, as the walk judges it before its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Yes,
    No,
    /// Its first reading is still out.
    Unread,
}

/// What opening a walk's page came to.
enum Opened {
    /// The page is up, or on its way.
    Up,
    /// Another macOS gesture owns the slot, or the yielding walk waits one out: look again next
    /// turn.
    Wait,
    /// It could not open, or the owner's Not now holds it back from the yielding walk; the walk
    /// goes on without it.
    Skip(&'static str),
    /// Another step of aterm's takes it (the debugger: System State's step); the walk goes on.
    Handed(&'static str),
}

impl App {
    /// The walk's turn before the hand-overs: on a cold first launch with the person at System
    /// State, start one by itself ([`starts`]); then [`Self::drive_permission_walk`].
    pub(crate) fn tick_permission_walk(&mut self, now: Instant) {
        if !cfg!(target_os = "macos") || !self.devtools_helper.windowed() {
            return;
        }
        if !self.permission_walk.active() {
            self.look_for_walk_start(now);
        }
        self.drive_permission_walk(now);
    }

    /// Start a walk by itself when [`starts`] says so: the subjects missing that no walk of this
    /// process took (one that gave way to a press elsewhere leaves those it had not gone past,
    /// [`WalkLane::give_way`]) and the owner's Not now does not hold back
    /// ([`Self::declined_for_the_verb`]: a re-ask after a decline waits for a quiet moment on the
    /// automatic hand-overs). A walk the setup run has not admitted (no approved revision, a
    /// `[privacy]` switch off) never starts by itself, nor one while another of the window's
    /// gestures (the Local Network alert's hold among them) or a macOS dialog of aterm's is up, nor
    /// one while System Settings asks something ([`Self::system_settings_may_ask`], a question
    /// seen asking that lapsed off screen among them: the walk opens its pages), nor one before
    /// the approved revision's Local Network step has run or is found on record
    /// ([`Self::local_network_step_to_come`]): that step goes first in the run, at its own moment,
    /// and its alert is not left beside System Settings. That wait ends once only the step has held
    /// a ready walk for [`LOCAL_NETWORK_WAIT`] of unbroken looks, and never at a look where the
    /// step's own moment has come ([`Self::waited_out_local_network`]): the walk starts then, says
    /// so in aterm.log with what held the step, and the step, held by the walk as by any of the
    /// window's gestures, asks after it, never beside it. A look during a flick of focus shorter
    /// than [`crate::FOCUS_AWAY_FOR`] ([`Self::focus_away_briefly`]) neither starts the walk nor
    /// breaks that run, and the person present is read as the steps read it
    /// ([`Self::person_present_within`]), the person read present at the look that began an
    /// unbroken wait staying present through it ([`StartFacts::present`]). This runs at every park,
    /// so the in-memory readings come first, the access record once per revision, then the page
    /// markers ([`Self::fresh_mac`]), the declines and the subjects missing (files and grants
    /// read), and System Settings asking last (it may read the window list), read once per look
    /// and only at a look that would start a walk. A wait for the Local Network step reads the
    /// page markers, declines and grants at the look that begins it, which found a walk to start,
    /// and again, with System Settings, only at the look that finds it run out: the looks between
    /// read memory alone ([`Self::waited_out_local_network`]). Pages a walk left at System Settings' Quit & Reopen, which the first look
    /// takes ([`Self::take_walk_reopen`]), start a walk under these rules in place of the page
    /// markers: those still missing and not held back by the owner's Not now, in the walk's order.
    fn look_for_walk_start(&mut self, now: Instant) {
        self.take_walk_reopen();
        let cold = self.devtools_helper.cold_start();
        let Some(revision) = self
            .access_revision()
            .filter(|_| cold && self.access_steps_allowed())
        else {
            self.restart_local_network_wait();
            return;
        };
        let facts = StartFacts {
            cold,
            presented: self.system_state_in_front(),
            present: self.person_present_within(START_PRESENT_WITHIN, now)
                || self.local_network_wait_counting(revision),
            typing: self
                .last_keystroke_at
                .is_some_and(|at| now.saturating_duration_since(at) < START_KEYS_QUIET),
        };
        let ready_beside_front = starts(StartFacts {
            presented: true,
            ..facts
        }) && !self.access_gesture_up(now)
            && !self.access_dialog_up();
        if !ready_beside_front {
            self.restart_local_network_wait();
            return;
        }
        if !facts.presented {
            // A flick of focus is no break: the steps' key rest reads the same away rule.
            if !self.focus_away_briefly(now) {
                self.restart_local_network_wait();
            }
            return;
        }
        let ahead_of_local_network = self.local_network_step_to_come();
        // A wait under way found a walk to start at the look that began it: its later looks read
        // no markers, declines, grants or System Settings until it runs out.
        let ran_out = match ahead_of_local_network
            .then(|| self.waited_out_local_network(revision, now))
            .flatten()
        {
            Some(false) => return,
            Some(true) => true,
            None => false,
        };
        let reopened = matches!(self.permission_walk.reopened, Reopened::Waits(_));
        let walk = if let Reopened::Waits(left) = &self.permission_walk.reopened {
            let left = left.clone();
            let missing = Subject::ORDER.map(|subject| {
                left.contains(&subject)
                    && !self.declined_for_the_verb(subject)
                    && self.walk_missing(subject, false)
            });
            let walk = Walk::new(subjects(&left, |subject| missing[subject.index()]), now);
            if walk.is_none() {
                self.permission_walk.reopened = Reopened::Spent;
                aterm_log::info!(
                    "permission walk: every page Quit & Reopen left reads held or declined"
                );
                self.restart_local_network_wait();
                return;
            }
            walk
        } else {
            if !self.permission_walk.any_walked()
                && !self.permission_walk.gave_way
                && !self.fresh_mac()
            {
                self.restart_local_network_wait();
                return;
            }
            let missing = Subject::ORDER.map(|subject| {
                !self.permission_walk.walked(subject)
                    && !self.declined_for_the_verb(subject)
                    && self.walk_missing(subject, false)
            });
            Walk::new(subjects(&[], |subject| missing[subject.index()]), now)
        };
        let Some(walk) = walk else {
            self.restart_local_network_wait();
            return;
        };
        if ahead_of_local_network && !ran_out {
            self.begin_local_network_wait(revision, now);
            return;
        }
        // Read last, once per look, and only at a look that would start the walk: one that opens
        // System Settings' pages, so a question seen asking that lapsed off screen holds it too.
        if self.system_settings_may_ask() {
            self.restart_local_network_wait();
            return;
        }
        if ran_out && let Some(wait) = self.permission_walk.local_network_wait.as_mut() {
            wait.over = true;
        }
        if reopened {
            self.permission_walk.reopened = Reopened::Spent;
            self.start_walk_by_itself(walk, "Quit & Reopen", ahead_of_local_network, now);
        } else {
            self.start_walk_by_itself(walk, "first launch", ahead_of_local_network, now);
        }
    }

    /// Whether the walk's wait for `revision`'s Local Network step counts an unbroken run of ready
    /// looks now ([`Self::waited_out_local_network`]): the person read present at the look that
    /// began it stays present through it ([`StartFacts::present`]).
    fn local_network_wait_counting(&self, revision: u64) -> bool {
        self.permission_walk
            .local_network_wait
            .is_some_and(|wait| wait.revision == revision && !wait.over)
    }

    /// Start `walk` by itself, `by` in aterm.log's words; one that goes `ahead` of System State's
    /// Local Network step ([`Self::waited_out_local_network`]) says so with what held the step
    /// ([`Self::local_network_holds`]), read before the walk counts among them.
    fn start_walk_by_itself(&mut self, walk: Walk, by: &'static str, ahead: bool, now: Instant) {
        // System Settings read asking nothing at this look ([`Self::look_for_walk_start`]).
        let holds = ahead.then(|| self.local_network_holds_read(now, Some(false)));
        self.permission_walk.start(walk, by);
        if let Some(holds) = holds {
            aterm_log::info!(
                "permission walk: System State's Local Network step has not come in {} s of \
                 looks with the walk ready to start ({}); the walk goes first, and the step asks \
                 once it is over",
                LOCAL_NETWORK_WAIT.as_secs(),
                if holds.is_empty() {
                    "nothing it reads holds it now".to_owned()
                } else {
                    format!("it waits: {}", holds.join("; "))
                }
            );
        }
    }

    /// The walk's wait for System State's Local Network step starts again at a look the walk is
    /// not ready at, unless it is over: [`LOCAL_NETWORK_WAIT`] counts only unbroken readiness.
    fn restart_local_network_wait(&mut self) {
        if self
            .permission_walk
            .local_network_wait
            .is_some_and(|wait| !wait.over)
        {
            self.permission_walk.local_network_wait = None;
        }
    }

    /// Whether the walk that starts by itself, ready to start at this look, has waited
    /// [`LOCAL_NETWORK_WAIT`] for `revision`'s Local Network step, still to come, as far as the
    /// wait under way knows: `None` when no unbroken wait is under way for that revision (this
    /// look, finding a walk to start, begins one: [`Self::begin_local_network_wait`]);
    /// `Some(false)` while it counts, or once it ran out but the step's own moment has come
    /// ([`Self::local_network_moment_but_settings`]: the step asks later in this turn, unless
    /// System Settings asks something, which holds the walk too); `Some(true)` once it ran out (or
    /// ran out before) with the step's moment not come. It reads nothing but in memory, and the
    /// step's moment only once the wait ran out, so a look that finds it counting returns before
    /// the walk is worked out again. An aterm window taking keyboard focus back after none had it
    /// for [`crate::FOCUS_AWAY_FOR`] or more since the clock started breaks the run, though no
    /// look fell in that time away (`App::note_keyboard_focus`).
    fn waited_out_local_network(&self, revision: u64, now: Instant) -> Option<bool> {
        let back = self.keyboard_focus_back_at;
        let wait = self.permission_walk.local_network_wait.filter(|wait| {
            wait.revision == revision && (wait.over || back.is_none_or(|back| back <= wait.since))
        })?;
        let ran_out = wait.over || now.saturating_duration_since(wait.since) >= LOCAL_NETWORK_WAIT;
        Some(ran_out && !self.local_network_moment_but_settings(now))
    }

    /// Begin the walk's wait for `revision`'s Local Network step at this look, the first of an
    /// unbroken run (or of another revision) at which a walk would start
    /// ([`Self::waited_out_local_network`]), unless the step's own moment has come
    /// ([`Self::local_network_moment_but_settings`]): it asks later in this turn.
    fn begin_local_network_wait(&mut self, revision: u64, now: Instant) {
        if self.local_network_moment_but_settings(now) {
            return;
        }
        self.permission_walk.local_network_wait = Some(LocalNetworkWait {
            revision,
            since: now,
            over: false,
        });
    }

    /// Take, once per process, the pages a walk under way left when System Settings' Quit &
    /// Reopen ended the process before this one ([`Self::leave_walk_at_quit`]): the reopen
    /// marker is read and removed at the first look ([`WalkReach::take_reopen`]: this `.app`'s,
    /// within [`REOPEN_WITHIN`]), and its pages wait in memory, not on disk, until the start's
    /// other rules hold ([`Self::look_for_walk_start`]). Switching Full Disk Access on brings that
    /// question, and its Quit & Reopen ends the walk with its next pages still to show
    /// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 1); the
    /// aterm it opens is a cold start whose pages came up before ([`Self::fresh_mac`]), so
    /// without them it would start none. A walk that started in this process before the look (a
    /// person's press, the control verb) ends the wait.
    fn take_walk_reopen(&mut self) {
        if self.permission_walk.reopened != Reopened::Unread {
            return;
        }
        let left = (self.permission_walk.reach().take_reopen)();
        self.permission_walk.reopened = match left {
            Some(left) if !self.permission_walk.any_walked() => {
                aterm_log::info!(
                    "permission walk: Quit & Reopen left {}; they wait for the walk's start",
                    left.iter()
                        .map(|subject| subject.word())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                Reopened::Waits(left)
            }
            Some(_) => {
                aterm_log::info!(
                    "permission walk: a walk started before the pages Quit & Reopen left were \
                     taken; they start none"
                );
                Reopened::Spent
            }
            None => Reopened::Spent,
        };
    }

    /// At this process's quit, when System Settings' Quit & Reopen asked for it (`reopens`,
    /// `crate::system_quit::reopens`), leave the walk under way's pages, from the one it is at on,
    /// for the aterm that opens again ([`WalkReach::leave_reopen`]), which walks on from there
    /// ([`Self::take_walk_reopen`]). The page it is at is left too: the reopened aterm reads its
    /// grant and skips a page whose grant reads held. A person's quit, a logout's, or no walk
    /// under way leaves nothing. Called where the quit leaves the Quit & Reopen helper, before
    /// it, so the marker stands before that aterm can start.
    #[cfg(target_os = "macos")]
    pub(crate) fn leave_walk_at_quit(&self, reopens: bool) {
        let Some(walk) = self.permission_walk.walk() else {
            return;
        };
        if reopens && !walk.ahead().is_empty() {
            (self.permission_walk.reach().leave_reopen)(walk.ahead());
        }
    }

    /// Whether aterm never brought the Full Disk Access or Developer Tools page up for this
    /// bundle on this Mac: neither page's markers hold an opening of this TCC identity
    /// (`atpkg::devtools::PageMarkers::opened_as` under the access marks' Full Disk Access key),
    /// so another Mac's or bundle's, which `<state>` keeps on a network home or after Migration
    /// Assistant, leaves the walk to start. A Developer Tools opening that names no Mac (a
    /// released aterm's, `aterm doctor`'s) is this Mac's only while the Full Disk Access marker
    /// holds no opening of any ([`found`]). Read at most until an opening is found that stays
    /// this identity's whatever bundle id and Mac the key, read before them, comes to carry
    /// ([`found_for_good`]), which no later look in this process reads again; one that might be
    /// another Mac's holds the start, and the markers are read again only once the key changes
    /// (`WalkLane::held_under`).
    fn fresh_mac(&mut self) -> bool {
        if self.permission_walk.page_seen {
            return false;
        }
        let key = self.access_marks().fda;
        if self.permission_walk.held_under.as_deref() == Some(key.as_str()) {
            return false;
        }
        let fda = self.devtools_helper.fda_reach();
        let fda_word = (fda.opened_as)(&key).map(|(word, _)| word);
        let devtools_word = (self.devtools_helper.reach().opened_as)(&key).map(|(word, _)| word);
        match found(
            &key,
            fda_word.as_deref(),
            || (fda.last_opened)().is_some(),
            devtools_word.as_deref(),
        ) {
            Found::Nothing => true,
            Found::ThisKey => {
                self.permission_walk.held_under = Some(key);
                false
            }
            Found::ForGood => {
                self.permission_walk.page_seen = true;
                false
            }
        }
    }

    /// Whether a walk may still start by itself in this process and only the person's absence
    /// holds it: a windowed macOS instance the owner started ([`StartFacts::cold`]), no walk run
    /// in it, no look that found an opening holding the start ([`Self::fresh_mac`]: none that
    /// latched, none held under the key the access marks carry now), and no touch of the
    /// keyboard or pointer within [`START_PRESENT_WITHIN`], read as the walk's start reads it
    /// ([`Self::person_present_within`]). System State's own steps wait with it
    /// (`App::access_step_moment`): a step that brought the Full Disk Access or Developer Tools
    /// page up while nobody was at the Mac would write the marker that leaves this Mac without
    /// its first walk. Once the person is back the walk looks first in each turn, under rules the
    /// step moment needs too (System State in front, the Local Network step run, none of the
    /// window's gestures and no macOS dialog of aterm's up) and a key rest ([`START_KEYS_QUIET`])
    /// shorter than the steps' quiet (`devtools_helper::QUIET_FOR`), so it starts, or finds what
    /// holds it, before a step goes. The Local Network step's own moment does not wait with it,
    /// so the walk, which waits for that step, is never held by a step that waits for the walk.
    pub(crate) fn walk_waits_for_the_person(&self, now: Instant) -> bool {
        cfg!(target_os = "macos")
            && self.devtools_helper.windowed()
            && self.devtools_helper.cold_start()
            && !self.permission_walk.any_walked()
            && !self.permission_walk.page_seen
            && !self.person_present_within(START_PRESENT_WITHIN, now)
            && self.permission_walk.held_under.as_deref() != Some(self.access_marks().fda.as_str())
    }

    /// Start a walk over `named` first, then every other subject missing, started `by`
    /// (aterm.log's words): a person's press on a Permissions row, or the control verb through
    /// [`Self::ask_permission_walk`]. One under way keeps going and takes those subjects too
    /// ([`WalkLane::join`]).
    pub(crate) fn start_permission_walk(
        &mut self,
        named: &[Subject],
        by: &'static str,
        now: Instant,
    ) -> Started {
        self.start_walk_without(named, [false; 4], false, by, now)
    }

    /// [`Self::start_permission_walk`] leaving out each subject `held_back` names (by
    /// [`Subject::ORDER`]): a decline that holds the control verb's walk back. The control
    /// verb's ask (`verb`) starts a walk, or takes back a page the walk under way passed, at
    /// most once per [`VERB_AGAIN_AFTER`] from the last it did either. Inside that span it starts
    /// none and takes no passed page back, answering recent when a page it asked for (any, when
    /// it names none) stays passed, so a client opens nothing beside the walk for that page; a
    /// page the walk under way never had still joins it. Its walk yields: it opens no page while
    /// a helper panel it did not bring up is up, waits before its first page for the window's
    /// other gestures and macOS dialogs to end ([`Self::reserve_walk_dialog`]), and leaves out a
    /// page the owner's Not now holds back by its turn; the answer waits for neither. A walk under
    /// way takes the subjects a new one would (one it passed with no grant read on comes back,
    /// [`Walk::take_also`]) and answers with the pages it has still to show. The subjects asked
    /// for (every subject, when none is named) that are missing and held back are the answer's
    /// Not nows; with no walk to start, they decide between declined and granted.
    fn start_walk_without(
        &mut self,
        named: &[Subject],
        held_back: [bool; 4],
        verb: bool,
        by: &'static str,
        now: Instant,
    ) -> Started {
        if !cfg!(target_os = "macos") || !self.devtools_helper.windowed() {
            return Started::Headless;
        }
        let recent = verb
            && self
                .permission_walk
                .verb_started_at
                .is_some_and(|at| now.saturating_duration_since(at) < VERB_AGAIN_AFTER);
        if recent && !self.permission_walk.active() {
            return Started::Recent;
        }
        let missing =
            Subject::ORDER.map(|subject| self.walk_missing(subject, named.contains(&subject)));
        let taken =
            Subject::ORDER.map(|subject| missing[subject.index()] && !held_back[subject.index()]);
        let take = subjects(named, |subject| taken[subject.index()]);
        let not_now = subjects(named, |subject| {
            (named.is_empty() || named.contains(&subject))
                && missing[subject.index()]
                && held_back[subject.index()]
        });
        if self.permission_walk.active() {
            // Inside the minute a page the walk passed stays passed; the rest still join.
            let (kept_back, take): (Vec<Subject>, Vec<Subject>) =
                take.into_iter().partition(|subject| {
                    recent
                        && self
                            .permission_walk
                            .walk()
                            .is_some_and(|walk| walk.passed(*subject))
                });
            let takes_back = self
                .permission_walk
                .walk()
                .is_some_and(|walk| walk.takes_back(&take));
            if self.permission_walk.join(&take, named, by) {
                if takes_back && verb {
                    self.permission_walk.verb_started_at = Some(now);
                }
                // The panel is held up for the page that went on, as for any next page, and the
                // page up counts it.
                self.drive_permission_walk(now);
            }
            if kept_back
                .iter()
                .any(|subject| named.is_empty() || named.contains(subject))
            {
                return Started::Recent;
            }
            let pages = self
                .permission_walk
                .walk()
                .map_or_else(Vec::new, |walk| walk.ahead().to_vec());
            let not_now = not_now
                .into_iter()
                .filter(|subject| !pages.contains(subject))
                .collect();
            return Started::Walking { pages, not_now };
        }
        let Some(walk) = Walk::named(take, named, now) else {
            return if not_now.is_empty() {
                Started::Granted
            } else {
                Started::Declined
            };
        };
        let pages = walk.subjects().to_vec();
        self.permission_walk.start(walk, by);
        self.permission_walk.yields = verb;
        if verb {
            self.permission_walk.verb_started_at = Some(now);
        }
        self.drive_permission_walk(now);
        Started::Walking { pages, not_now }
    }

    /// Whether the walk takes `subject`: its grant does not read held, and, for one it was not
    /// asked for by name, the setup run wants it (Accessibility: System State's row offers it,
    /// so the sign-outs need it; the debugger: under an approved revision). The debugger is
    /// taken only while System State's once-per-Mac step is still to come, named or not: that
    /// step is the walk's one way to it ([`Opened::Handed`]), so a walk never names a page it
    /// would only skip. A debugger turned off by the person is never taken.
    pub(crate) fn walk_missing(&mut self, subject: Subject, named: bool) -> bool {
        let wanted = match subject {
            Subject::FullDiskAccess | Subject::DeveloperTools => true,
            Subject::Accessibility => named || self.system_state_offers(Access::Accessibility),
            Subject::Debugger => {
                self.debugger_launch.queued() && (named || self.access_revision().is_some())
            }
        };
        let chosen_off = subject == Subject::Debugger && self.debugger_change.off_is_chosen();
        wanted && !chosen_off && self.walk_held(subject) != Held::Yes
    }

    /// `aterm ctl permissions walk now [<subject>…]` (and `devtools hand-over now`): a walk as
    /// [`Self::start_permission_walk`] starts one, at most once per [`VERB_AGAIN_AFTER`] from
    /// the last walk the verb started or page it took back, and without a page the owner's Not
    /// now holds back ([`Self::declined_for_the_verb`]): any program in a session can send the
    /// verb, and only the owner's own press goes past a Not now. The walk reads that hold again
    /// before each page, and opens its first only under the one-gesture rule
    /// ([`Self::open_walk_page`]). A walk under way takes the verb's subjects and answers with
    /// the pages it has still to show.
    pub(crate) fn ask_permission_walk(&mut self, named: &[Subject], now: Instant) -> Started {
        let held_back = Subject::ORDER.map(|subject| self.declined_for_the_verb(subject));
        self.start_walk_without(named, held_back, true, "a control client", now)
    }

    /// A control lane's ask, as this turn takes it: [`Self::ask_permission_walk`], its answer
    /// sent on `reply`, unless the lane gave the ask up first (`claim`,
    /// [`crate::control::control_media::Claim::take`]). Its client then heard `ERR late`, or
    /// hung up, and goes on without the walk (`aterm doctor` opens the pages itself, a sign-out
    /// asks macOS), so a walk started now would bring a second page or question up beside the
    /// client's: none starts, and nothing is answered.
    pub(crate) fn take_permission_walk_ask(
        &mut self,
        named: &[Subject],
        claim: &crate::control::control_media::Claim,
        reply: &std::sync::mpsc::Sender<Started>,
        now: Instant,
    ) {
        if !claim.take() {
            aterm_log::info!(
                "permission walk: a control client's ask came after its lane gave it up; no \
                 walk starts"
            );
            return;
        }
        let _ = reply.send(self.ask_permission_walk(named, now));
    }

    /// Whether the owner's Not now holds `subject` back from the control verb's walk, as it held
    /// the verb's hand-overs (`devtools_helper::declined_for` with [`Origin::Ctl`]): a Not now
    /// under this setup revision, or one inside its span; for Full Disk Access also the access
    /// card's; for Accessibility, the walk's page alone, as
    /// [`Self::accessibility_declined_for_the_verb`] reads it.
    fn declined_for_the_verb(&mut self, subject: Subject) -> bool {
        let marks = self.access_marks();
        match subject {
            Subject::FullDiskAccess => {
                let fda = self.devtools_helper.fda_reach();
                self.access_declined_here(Access::FullDiskAccess)
                    || crate::devtools_helper::declined_for(
                        Origin::Ctl,
                        &marks.fda,
                        (fda.last_declined)().as_ref(),
                        (fda.declined)().is_some(),
                    )
                    || self.consent_card_host.marker == Some(crate::consent_card::Marker::NotNow)
            }
            Subject::DeveloperTools => {
                let reach = self.devtools_helper.reach();
                self.access_declined_here(Access::DeveloperTools)
                    || crate::devtools_helper::declined_for(
                        Origin::Ctl,
                        &marks.devtools,
                        (reach.last_declined)().as_ref(),
                        (reach.declined)().is_some(),
                    )
            }
            Subject::Accessibility => self.accessibility_declined_for_the_verb(),
            Subject::Debugger => self.access_declined_here(Access::Debugger),
        }
    }

    /// Whether the owner's Not now on the walk's Accessibility page holds it back from the
    /// control verb: one this process took under the approved revision
    /// ([`Self::access_declined_here`]), else what `<state>` keeps of one, read once per key
    /// (`WalkLane::ax_declined`, since the walk's start looks at every park): the page's
    /// `accessibility-declined` as `devtools_helper::declined_for` reads it for [`Origin::Ctl`]
    /// under the access marks' Full Disk Access key (the TCC identity's, whose grant Accessibility
    /// is too), so one said in an earlier process or while no revision stood holds it too, or the
    /// approved revision's `declined` in the access record (an aterm before that marker kept its
    /// Not now only there).
    fn accessibility_declined_for_the_verb(&mut self) -> bool {
        if self.access_declined_here(Access::Accessibility) {
            return true;
        }
        let key = self.access_marks().fda;
        if let Some((read, declined)) = &self.permission_walk.ax_declined
            && *read == key
        {
            return *declined;
        }
        let reach = self.permission_walk.reach();
        let declined = crate::devtools_helper::declined_for(
            Origin::Ctl,
            &key,
            (reach.ax_last_declined)().as_ref(),
            (reach.ax_declined)(),
        ) || self.access_declined_on_record(Access::Accessibility);
        self.permission_walk.ax_declined = Some((key, declined));
        declined
    }

    /// Whether `subject`'s grant reads held now.
    fn walk_held(&mut self, subject: Subject) -> Held {
        use aterm_containment::consent::{DebuggerMode, FdaState, ProbeLabel};
        match subject {
            Subject::FullDiskAccess => {
                let read = self.consent_panel_facts();
                if read.probe == ProbeLabel::Pending {
                    match self.consent_launch.launch_read() {
                        Some(FdaState::Granted) if self.consent.window_reads_own_grant() => {
                            Held::Yes
                        }
                        Some(_) => Held::No,
                        None => Held::Unread,
                    }
                } else {
                    self.consent_launch.note_launch_read(read.fda);
                    if read.fda == FdaState::Granted && self.consent.window_reads_own_grant() {
                        Held::Yes
                    } else {
                        Held::No
                    }
                }
            }
            Subject::DeveloperTools => {
                if self.devtools_reading().grant == aterm_containment::DevToolGrant::Granted {
                    Held::Yes
                } else {
                    Held::No
                }
            }
            Subject::Accessibility => {
                if (self.permission_walk.reach().ax_trusted)() {
                    Held::Yes
                } else {
                    Held::No
                }
            }
            Subject::Debugger => {
                let row = self.consent.debugger(self.config.privacy_probe_gate());
                if row.pending() {
                    Held::Unread
                } else if self.debugger_access_facts_from(row).probe.mode == DebuggerMode::Enabled {
                    Held::Yes
                } else {
                    Held::No
                }
            }
        }
    }

    /// The walk's page place for a panel `origin` shows `page`: the walk's own page only.
    pub(crate) fn walk_step(&self, page: Page, origin: Origin) -> Option<(usize, usize)> {
        let walk = self.permission_walk.walk()?;
        (origin == Origin::Walk && walk.current().and_then(Subject::page) == Some(page))
            .then(|| walk.position(|subject| self.walk_reads_held(subject)))
    }

    /// Whether `subject`'s grant reads held by the readings already in, as [`Self::walk_held`]
    /// judges it but starting no reading: the walk's count of the pages it will show
    /// ([`Walk::position`]), read at every turn while a page is up.
    pub(crate) fn walk_reads_held(&self, subject: Subject) -> bool {
        use aterm_containment::consent::{DebuggerMode, FdaState, ProbeLabel};
        match subject {
            Subject::FullDiskAccess => {
                let read = self.consent_panel_facts_cached();
                read.probe != ProbeLabel::Pending
                    && read.fda == FdaState::Granted
                    && self.consent.window_reads_own_grant()
            }
            Subject::DeveloperTools => self
                .devtools_helper
                .reading()
                .is_some_and(|reading| reading.grant == aterm_containment::DevToolGrant::Granted),
            Subject::Accessibility => (self.permission_walk.reach().ax_trusted)(),
            Subject::Debugger => self
                .consent
                .debugger_cached(self.config.privacy_probe_gate())
                .is_some_and(|row| !row.pending() && row.probe().mode == DebuggerMode::Enabled),
        }
    }

    /// Whether the walk's Developer Tools page waits for its hand-over to bring it up: the walk
    /// stands there, opened, with no panel of that page up yet. A `spctl` run the walk started
    /// brings its page up only then (`App::tick_devtools_hand_over`); after a press on another
    /// row moved the walk on, or once the page came up another way, its end is no page of the
    /// walk's.
    pub(crate) fn walk_awaits_devtools_page(&self) -> bool {
        self.permission_walk.walk().is_some_and(|walk| {
            walk.step() == Step::Up && walk.current() == Some(Subject::DeveloperTools)
        }) && self
            .devtools_helper
            .panel()
            .is_none_or(|panel| panel.page() != Page::DeveloperTools)
    }

    /// The walk's turn after the hand-overs: how its page closed, the grant of the page that is
    /// up, the Full Disk Access page's done words once macOS's question is gone
    /// ([`Self::turn_after_fda`]), the next page when one is due; then the next page the panel is
    /// held up for, by name, and the page up's place in the walk, which a page the walk took
    /// since moves.
    pub(crate) fn drive_permission_walk(&mut self, now: Instant) {
        if !self.permission_walk.active() {
            self.devtools_helper.hold_for_walk(None);
            self.devtools_helper.release_held();
            return;
        }
        self.take_walk_close(now);
        if let Some(walk) = self.permission_walk.walk()
            && walk.step() == Step::Up
            && let Some(page) = walk.current().and_then(Subject::page)
            && !self.devtools_helper.walk_page_alive(page)
        {
            // Its panel never came up (or went without a close the walk heard): the page in
            // System Settings stays, as after a press with no panel; nothing walks on from it.
            self.permission_walk.end("its panel is gone");
        }
        self.read_walk_grant(now);
        self.turn_after_fda(now);
        self.open_due_walk_pages(now);
        match self.permission_walk.walk() {
            Some(walk) => {
                let next = walk.next().map(|subject| subject.access().name());
                let up = walk
                    .current()
                    .and_then(Subject::page)
                    .filter(|_| walk.step() == Step::Up)
                    .map(|page| (page, walk.position(|subject| self.walk_reads_held(subject))));
                self.devtools_helper.hold_for_walk(next);
                if let Some((page, step)) = up {
                    self.devtools_helper.restep(page, step);
                }
            }
            None => {
                self.devtools_helper.hold_for_walk(None);
                self.devtools_helper.release_held();
                self.release_unused_access_dialog(now);
            }
        }
    }

    /// How the walk's page closed, when it did since the last look.
    pub(crate) fn take_walk_close(&mut self, now: Instant) {
        if let Some((page, why)) = self.devtools_helper.take_closed() {
            self.walk_page_closed(page, why, now);
        }
    }

    /// [`Self::take_walk_close`] as the panel's turn ends; a walk it left with no page to go on
    /// to is over, and the panel it held up goes.
    pub(crate) fn settle_walk_close(&mut self, now: Instant) {
        if !self.permission_walk.active() {
            return;
        }
        self.take_walk_close(now);
        if self
            .permission_walk
            .walk()
            .is_some_and(|walk| walk.current().is_none())
        {
            self.permission_walk.end("every page answered");
        }
        if !self.permission_walk.active() {
            self.devtools_helper.hold_for_walk(None);
            self.devtools_helper.release_held();
            self.release_unused_access_dialog(now);
        }
    }

    /// A walk page closed `why`: done or switched on goes on to the next page, and so does a
    /// Not now on a page the walk added; a Not now on a page asked for by name (a row's press,
    /// the control verb) goes on only to another page asked for by name, and otherwise ends the
    /// walk there, as do the close box (on done words that wait for macOS's question too), the
    /// page's ten minutes, System Settings quitting, another panel and a give-way to a press
    /// elsewhere (whose walk already stepped aside, `WalkLane::give_way`). A Not now or the close
    /// box on the Accessibility page, the walk's alone, is noted as its decline
    /// (`WalkLane::note_ax_declined`), as the lane notes the other pages'.
    fn walk_page_closed(&mut self, page: Page, why: Closed, now: Instant) {
        if page == Page::Accessibility && why.declines() {
            let key = self.access_marks().fda;
            self.permission_walk.note_ax_declined(key);
        }
        let Some(walk) = self.permission_walk.walk_mut() else {
            return;
        };
        if walk.current().and_then(Subject::page) != Some(page) || walk.step() != Step::Up {
            return;
        }
        match why {
            Closed::Done | Closed::SwitchedOn => {
                let _ = walk.advance(now);
            }
            Closed::NotNow if !walk.at_named() => {
                let _ = walk.advance(now);
            }
            Closed::NotNow => {
                if walk.advance_to_named(now).is_none() {
                    self.permission_walk
                        .end("Not now on the last page asked for");
                }
            }
            Closed::CloseBox | Closed::Waiting => self.permission_walk.end("the close box"),
            Closed::Lapsed => self.permission_walk.end("its page lapsed"),
            Closed::SettingsGone => self.permission_walk.end("System Settings quit"),
            Closed::Replaced => self.permission_walk.end("another panel replaced its page"),
            Closed::GaveWay => self
                .permission_walk
                .end("its panel gave way to a person's press elsewhere"),
        }
    }

    /// The walk's Full Disk Access page read on with a page after it, and its done words wait
    /// (`devtools_helper::Lane::note_fda`): switching the grant on for a running aterm has macOS
    /// ask, in a sheet over System Settings' page, whether to quit aterm ("will not have full
    /// disk access until it is quit", Quit & Reopen or Later), and the next page's link opened
    /// under that sheet never takes System Settings there, while its panel stood beside the sheet
    /// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 3). The words
    /// stand [`crate::devtools_helper::DONE_FOR`] as before, by when the sheet is up if it comes
    /// (within 0.4 s of the grant's reading, as measured), and go, the panel turning to the next
    /// page, at the first look after that which reads no window of System Settings' own over its
    /// main one (`devtools_helper::DevToolsHelperArm::settings_asks`: the sheet left at Later)
    /// and no quit asked of this process ([`WalkReach::quit_asked`]: Quit & Reopen asks for one
    /// before its sheet goes, and this process is then going). A grant read with no sheet to
    /// come (the page opened while the grant's first reading was still out) waits the second
    /// alone. Their close box ends the walk, and a sheet left standing ends it ten minutes after
    /// the grant (`devtools_helper::Panel::turn`), so none holds it for good.
    fn turn_after_fda(&mut self, now: Instant) {
        let Some(done_at) = self.devtools_helper.waiting_done(Page::FullDiskAccess) else {
            return;
        };
        if now.saturating_duration_since(done_at) < crate::devtools_helper::DONE_FOR
            || (self.permission_walk.reach().quit_asked)()
            || self.devtools_helper.settings_may_ask()
        {
            return;
        }
        aterm_log::info!(
            "permission walk: System Settings asks nothing over Full Disk Access; on to its next \
             page"
        );
        self.devtools_helper.let_done_go();
        self.take_walk_close(now);
    }

    /// While a walk page is up, read its grant every [`GRANT_POLL`] until the page shows its done
    /// words: a fresh Full Disk Access probe (its answer turns the page in
    /// `App::tick_fda_hand_over`) and `AXIsProcessTrusted`. Full Disk Access's done words may
    /// wait minutes for macOS's sheet ([`Self::turn_after_fda`]), and the grant they show is in,
    /// so no probe (a child aterm each) runs under them. The Developer Tools page reads its own
    /// list at that pace.
    fn read_walk_grant(&mut self, now: Instant) {
        let Some(walk) = self.permission_walk.walk() else {
            return;
        };
        if walk.step() != Step::Up {
            return;
        }
        let done = |page: Page| {
            self.devtools_helper.panel().is_some_and(|panel| {
                panel.page() == page && panel.phase() == crate::devtools_helper::Phase::Done
            })
        };
        match walk.current() {
            Some(Subject::FullDiskAccess) => {
                if !done(Page::FullDiskAccess) {
                    let _ = self
                        .consent
                        .fda(self.config.privacy_probe_gate(), GRANT_POLL);
                }
            }
            Some(Subject::Accessibility) => {
                // Read until the page shows its tick, once.
                let up = self.devtools_helper.panel().is_some_and(|panel| {
                    panel.page() == Page::Accessibility
                        && panel.phase() != crate::devtools_helper::Phase::Done
                });
                if up && (self.permission_walk.reach().ax_trusted)() {
                    aterm_log::info!("permission walk: accessibility reads on");
                    self.devtools_helper.note_granted(Page::Accessibility, now);
                    // The access record's line wakes the setup worker's run at once.
                    self.record_access_step(Access::Accessibility, AccessOutcome::Granted);
                    (self.permission_walk.reach().ask_automation)();
                    self.permission_walk.asked_automation = true;
                }
            }
            // The debugger is never up: System State's step takes it ([`Opened::Handed`]).
            Some(Subject::DeveloperTools | Subject::Debugger) | None => {}
        }
    }

    /// Open the walk's due page, skipping each subject whose grant reads held without opening
    /// its page; the walk ends after the last.
    fn open_due_walk_pages(&mut self, now: Instant) {
        loop {
            let Some(walk) = self.permission_walk.walk() else {
                return;
            };
            if walk.step() != Step::Due {
                return;
            }
            let waited = walk.waited_for_reading(now);
            let Some(subject) = walk.current() else {
                self.permission_walk.end("every page answered");
                return;
            };
            match self.walk_held(subject) {
                Held::Yes => {
                    aterm_log::info!("permission walk: {} reads held; skipped", subject.word());
                    self.advance_walk(now);
                    continue;
                }
                Held::Unread if !waited => return,
                Held::Unread | Held::No => {}
            }
            match self.open_walk_page(subject, now) {
                Opened::Up => {
                    if let Some(walk) = self.permission_walk.walk_mut() {
                        walk.opened();
                    }
                    return;
                }
                Opened::Wait => {
                    // A page held off as long as a panel stays up ends the walk, so a dialog
                    // slot that never frees keeps no walk looking for it.
                    if self
                        .permission_walk
                        .walk()
                        .is_some_and(|walk| walk.due_for(now) >= crate::devtools_helper::PANEL_FOR)
                    {
                        self.permission_walk
                            .end("its page could not open for ten minutes");
                    }
                    return;
                }
                Opened::Skip(why) => {
                    aterm_log::info!("permission walk: {} skipped ({why})", subject.word());
                    self.advance_walk(now);
                }
                Opened::Handed(to) => {
                    aterm_log::info!("permission walk: {} handed to {to}", subject.word());
                    self.advance_walk(now);
                }
            }
        }
    }

    fn advance_walk(&mut self, now: Instant) {
        if let Some(walk) = self.permission_walk.walk_mut() {
            let _ = walk.advance(now);
        }
    }

    /// Bring `subject`'s page up as the owner's press would ([`Origin::Walk`]): the window's
    /// dialog slot first, so a page that cannot open now is tried again next turn, never queued
    /// for a quiet moment. The control verb's walk (one that yields) waits out a helper panel it
    /// did not bring up, wins the slot only while no other gesture of the window and no macOS
    /// dialog of this process is up ([`Self::reserve_walk_dialog`]) and then holds it for its
    /// later pages (the debugger's hand lets it go), and skips a page the owner's Not now holds
    /// back now ([`Self::declined_for_the_verb`]), as the verb sent now would: one said since the
    /// verb asked (on the panel the walk waited behind, say) holds it too. A walk that asked
    /// macOS's Automation question opens no page, nor hands the debugger to its step, while that
    /// question is out ([`WalkReach::automation_out`]): its next page waits for the person's
    /// answer rather than come up beside the alert.
    fn open_walk_page(&mut self, subject: Subject, now: Instant) -> Opened {
        if self.permission_walk.asked_automation && (self.permission_walk.reach().automation_out)()
        {
            return Opened::Wait;
        }
        if self.permission_walk.yields {
            if self
                .devtools_helper
                .panel()
                .is_some_and(|panel| panel.origin() != Origin::Walk)
            {
                return Opened::Wait;
            }
            if subject != Subject::Debugger && !self.permission_walk.slot_won {
                if !self.reserve_walk_dialog(now) {
                    return Opened::Wait;
                }
                self.permission_walk.slot_won = true;
            }
            if self.declined_for_the_verb(subject) {
                return Opened::Skip("the owner's Not now holds it back from the control verb");
            }
        }
        if subject != Subject::Debugger && !self.reserve_hand_over_dialog(Origin::Walk) {
            return Opened::Wait;
        }
        match subject {
            Subject::FullDiskAccess => {
                let answer = self.fda_hand_over(
                    Origin::Walk,
                    Moment::Asked,
                    aterm_containment::consent::FdaState::Denied,
                );
                match answer {
                    HandOver::Opened => {
                        self.fda_page_up_for_launch();
                        // The access card watches for the grant as after a press's page.
                        self.note_macos_access_settings_opened(now);
                        Opened::Up
                    }
                    HandOver::Queued => Opened::Up,
                    other => Opened::Skip(other.as_str()),
                }
            }
            Subject::DeveloperTools => {
                match self.hand_devtools_to_owner(Origin::Walk) {
                    // A queued hand-over of the walk's own opens at once once its `spctl`
                    // run or Full Disk Access reading is in; the walk watches it there.
                    HandOver::Opened | HandOver::Listing | HandOver::Queued => Opened::Up,
                    other => Opened::Skip(other.as_str()),
                }
            }
            Subject::Accessibility => {
                if !self.open_access_pane(Access::Accessibility, Origin::Walk, now) {
                    return Opened::Skip("System Settings did not open");
                }
                self.record_access_step(Access::Accessibility, AccessOutcome::Handed);
                if self.show_walk_accessibility(now) {
                    Opened::Up
                } else {
                    Opened::Skip("its panel could not be shown")
                }
            }
            Subject::Debugger => {
                // System State's once-per-Mac step is the debugger's way in for a walk: it
                // spends the window's own admission as the yes and starts the change once its
                // moment comes (System State in front, no page or dialog of aterm's up), so the
                // panel and the walk's hold on the dialog slot go, and the step looks now
                // instead of at its next look. The walk is no person's press on the row's
                // `Turn On…` (`App::turn_on_debugger_from_system_state`), whose door a control
                // client must never reach.
                self.devtools_helper.hold_for_walk(None);
                self.devtools_helper.release_held();
                self.drop_access_dialog();
                self.permission_walk.slot_won = false;
                if self.debugger_launch.queued() {
                    self.debugger_launch.look_later(now);
                    Opened::Handed("System State's debugger step")
                } else {
                    Opened::Skip("System State's debugger step is not queued; its row stays")
                }
            }
        }
    }

    /// The walk's Accessibility page beside System Settings' list.
    fn show_walk_accessibility(&mut self, now: Instant) -> bool {
        use crate::devtools_helper::{Reading, Show};
        let reach = self.devtools_helper.reach();
        let marks = self.access_marks();
        self.devtools_helper.set_marks(marks);
        let show = Show {
            page: Page::Accessibility,
            origin: Origin::Walk,
            reading: Reading::UNREAD,
            listed: None,
            bundle: (reach.drag_bundle)(crate::signing_identity::running_bundle_id_if_warm()),
            skips: false,
            step: self.walk_step(Page::Accessibility, Origin::Walk),
        };
        let wake = self.devtools_hand_over_wake();
        let shown = self.devtools_helper.show(show, now, wake);
        if shown {
            self.access_helper_shown();
        }
        shown
    }

    /// The park's deadline for the walk: every [`GRANT_POLL`] while one is under way, so a page
    /// that could not open yet (the dialog slot taken, a first reading still out, a panel the
    /// walk yields to) is tried again and a page that is up reads its grant. A page that comes
    /// due as another ends opens in the same turn ([`Self::drive_permission_walk`]), so nothing
    /// waits on this deadline to open at once, and a page that cannot open never spins the park.
    pub(crate) fn next_permission_walk_deadline(&self, now: Instant) -> Option<Instant> {
        self.permission_walk.walk().map(|_| now + GRANT_POLL)
    }
}

#[cfg(target_os = "macos")]
mod live {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    /// aterm's own Accessibility grant, as macOS answers this process.
    pub(super) fn ax_trusted() -> bool {
        // SAFETY: `AXIsProcessTrusted` takes no arguments and returns a `Boolean`; it reads
        // this process's TCC record and touches no memory of ours.
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Automation asks whose Apple event has not returned: macOS's question may be on screen.
    static ASKS_OUT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// One ask out: counted in [`ASKS_OUT`], and as a macOS dialog of this process up, until it
    /// drops.
    struct AskOut {
        _dialog: aterm_types::macos_setup::MacosDialog,
    }

    impl AskOut {
        fn new() -> Self {
            ASKS_OUT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Self {
                _dialog: aterm_types::macos_setup::MacosDialog::up(),
            }
        }
    }

    impl Drop for AskOut {
        fn drop(&mut self) {
            ASKS_OUT.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }

    /// Whether an Automation ask's Apple event has not returned yet.
    pub(super) fn automation_out() -> bool {
        ASKS_OUT.load(std::sync::atomic::Ordering::Acquire) != 0
    }

    /// One Apple event to System Events on a worker thread: macOS asks its Automation question
    /// for aterm now, or answers at once with an earlier answer.
    pub(super) fn ask_automation() {
        if let Err(error) = ask_with(send_to_system_events) {
            aterm_log::warn!("permission walk: the Automation ask could not start: {error}");
        }
    }

    /// Run `send` on a worker thread, out ([`automation_out`]) and counted as a macOS dialog up
    /// (`aterm_types::macos_setup::macos_dialog_up`, which the steps' one-gesture rule and the
    /// setup worker wait on) until `send` returns.
    ///
    /// # Errors
    /// The thread could not start; the ask is then not out.
    pub(super) fn ask_with(
        send: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        // Taken before the spawn, so the UI thread's next look already counts it, and dropped
        // when the worker ends however it ends (a panic too), or with the closure if the thread
        // cannot start.
        let out = AskOut::new();
        crate::qos::spawn_lane_thread(
            "aterm-automation-ask".to_owned(),
            crate::qos::Role::Background,
            move || {
                let _out = out;
                send();
            },
        )
    }

    /// `tell application "System Events" to count processes`, for at most two minutes: it
    /// returns once the person answers macOS's question, or at once with an earlier answer.
    fn send_to_system_events() {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/osascript");
        command.args([
            "-e",
            "tell application \"System Events\" to count processes",
        ]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        match aterm_uds::deadline::output_until(&mut command, deadline) {
            Ok(out) if out.status.success() => {
                aterm_log::info!("permission walk: Automation for System Events is on");
            }
            Ok(out) => aterm_log::info!(
                "permission walk: Automation for System Events is off ({})",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(error) => {
                aterm_log::info!("permission walk: the Automation ask did not finish: {error}");
            }
        }
    }
}

/// The live Automation ask's worker ([`WalkReach::live`]) with `send` in place of its Apple
/// event to System Events.
///
/// # Errors
/// The thread could not start.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn ask_automation_with_for_test(
    send: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    live::ask_with(send)
}

#[cfg(not(target_os = "macos"))]
mod live {
    pub(super) const fn ax_trusted() -> bool {
        false
    }

    pub(super) const fn ask_automation() {}

    pub(super) const fn automation_out() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn facts() -> StartFacts {
        StartFacts {
            cold: true,
            presented: true,
            present: true,
            typing: false,
        }
    }

    /// A cold first launch with System State in front, the person present 30 s ago and not
    /// typing, starts the walk; each missing condition holds it.
    #[test]
    fn the_walk_starts_on_a_cold_first_launch_with_the_person_present() {
        assert!(starts(facts()));
        assert!(
            !starts(StartFacts {
                typing: true,
                ..facts()
            }),
            "a key typed 1 s ago waits for the keyboard to rest 2 s"
        );
        assert!(
            !starts(StartFacts {
                cold: false,
                ..facts()
            }),
            "an update's successor, or a Mac whose pages came up before, never"
        );
        assert!(!starts(StartFacts {
            presented: false,
            ..facts()
        }));
        assert!(!starts(StartFacts {
            present: false,
            ..facts()
        }));
    }

    /// An opening found for a key read before the bundle id or the Mac holds the walk for good
    /// only when it names neither beyond what the key carries: one that names a Mac the key
    /// lacks may be another Mac's.
    #[test]
    fn only_an_opening_that_names_no_more_than_the_key_holds_the_walk_for_good() {
        for (word, key, want) in [
            ("r1@com.aterm.aterm@A", "r1@com.aterm.aterm@A", true),
            ("r1", "r1@com.aterm.aterm@A", true),
            ("0.146.0", "r1@com.aterm.aterm", true),
            ("r1@com.aterm.aterm", "r1@com.aterm.aterm", true),
            ("r1@com.aterm.aterm@A", "r1@com.aterm.aterm", false),
            ("r1@com.aterm.aterm", "r1", false),
        ] {
            assert_eq!(found_for_good(word, key), want, "{word} for {key}");
        }
    }

    /// The named subjects lead in their order, the rest follow in the walk's own, each once and
    /// only while missing.
    #[test]
    fn named_subjects_lead_and_the_walk_continues_with_the_rest() {
        use Subject::{Accessibility, Debugger, DeveloperTools, FullDiskAccess};
        assert_eq!(
            subjects(&[], |_| true),
            vec![FullDiskAccess, DeveloperTools, Accessibility, Debugger]
        );
        assert_eq!(
            subjects(&[DeveloperTools], |subject| subject != Accessibility),
            vec![DeveloperTools, FullDiskAccess, Debugger],
            "doctor's Developer Tools hand-over: Full Disk Access right after it"
        );
        assert_eq!(
            subjects(&[Debugger, Debugger], |subject| subject == Debugger),
            vec![Debugger]
        );
        assert!(subjects(&[FullDiskAccess], |_| false).is_empty());
    }

    /// The walk counts the pages the person sees: a page that came up counts, and so does each
    /// later one whose grant does not read held; a page skipped without coming up does not.
    #[test]
    fn a_walk_counts_its_pages_and_ends_after_the_last() {
        use Subject::{Debugger, DeveloperTools, FullDiskAccess};
        let now = Instant::now();
        let none_held = |_| false;
        let mut walk = Walk::new(vec![FullDiskAccess, Debugger], now).expect("two pages");
        assert_eq!(walk.position(none_held), (1, 2));
        assert_eq!(walk.next(), Some(Debugger));
        walk.opened();
        assert_eq!(walk.step(), Step::Up);
        assert_eq!(walk.advance(now), Some(Debugger));
        assert_eq!((walk.position(none_held), walk.step()), ((2, 2), Step::Due));
        assert_eq!(walk.next(), None);
        assert_eq!(walk.advance(now), None);
        assert_eq!(walk.advance(now), None, "stays over");
        assert!(Walk::new(Vec::new(), now).is_none());

        let mut walk =
            Walk::new(vec![FullDiskAccess, DeveloperTools, Debugger], now).expect("three pages");
        assert_eq!(
            walk.position(|subject| subject == DeveloperTools),
            (1, 2),
            "a later page whose grant reads held is not counted"
        );
        assert_eq!(walk.advance(now), Some(DeveloperTools), "skipped, never up");
        assert_eq!(walk.position(none_held), (1, 2), "nor is a page skipped");
    }

    #[test]
    fn each_subject_has_its_word_and_the_rows_name() {
        for subject in Subject::ORDER {
            assert_eq!(Subject::parse(subject.word()), Some(subject));
        }
        assert_eq!(
            Subject::parse("full-disk-access"),
            Some(Subject::FullDiskAccess)
        );
        assert_eq!(
            Subject::parse("developer-tools"),
            Some(Subject::DeveloperTools)
        );
        assert_eq!(Subject::parse("camera"), None);
    }

    /// An absolute path to a `.app`, spaces and all, on the host the test runs on.
    fn host_bundle(name: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            r"C:\Applications"
        } else {
            "/Applications"
        })
        .join(name)
    }

    /// A hardware UUID, one word.
    const MAC: &str = "1D683F62-5C4B-4E3A-9F21-7B8C6D5E4A30";

    /// A reopen marker reads back the quit's seconds, the pages in their order, the bundle and the
    /// Mac, a page's word this aterm does not know left out and each page once; anything else is
    /// junk.
    #[test]
    fn a_reopen_marker_reads_back_what_the_quit_wrote_and_nothing_else() {
        use Subject::{Debugger, DeveloperTools, FullDiskAccess};
        let bundle = host_bundle("aterm (dev).app");
        let path = bundle.to_str().expect("a UTF-8 path");
        let text = reopen_text(1_793_161_729, &[DeveloperTools, Debugger], path, MAC);
        assert_eq!(
            text,
            format!("1793161729 devtools debugger\n{path}\n{MAC}\n")
        );
        assert_eq!(
            read_reopen(&text),
            Some(Reopen {
                at: 1_793_161_729,
                subjects: vec![DeveloperTools, Debugger],
                bundle: bundle.clone(),
                mac: MAC.to_owned(),
            })
        );
        assert_eq!(
            read_reopen(&format!("5 camera devtools devtools fda\n{path}\n{MAC}\n"))
                .map(|reopen| reopen.subjects),
            Some(vec![DeveloperTools, FullDiskAccess])
        );
        for junk in [
            String::new(),
            "\n".to_owned(),
            format!("devtools\n{path}\n{MAC}\n"),
            format!("-5 devtools\n{path}\n{MAC}\n"),
            format!("5\n{path}\n{MAC}\n"),
            format!("5 camera\n{path}\n{MAC}\n"),
            format!("5 devtools\n{MAC}\n"),
            format!("5 devtools\nApplications/aterm.app\n{MAC}\n"),
            format!("5 devtools\n{path}\n"),
            format!("5 devtools\n{path}\n\n"),
            format!("5 devtools\n{path}\nMAC A\n"),
            format!("5 devtools\n{path}\n{MAC}\nmore\n"),
        ] {
            assert_eq!(read_reopen(&junk), None, "{junk:?}");
        }
    }

    /// The marker is taken once, by the bundle that wrote it on the Mac it was written on, within
    /// [`REOPEN_WITHIN`] of the quit (a clock set back a little too), and removed then. Another
    /// bundle's or Mac's within the bound, or any while this aterm names no bundle or Mac, starts
    /// nothing and is left in place, so the aterm it names still takes it; an older one or junk
    /// starts nothing and is gone. A bundle path no line can hold, or a Mac that is not one word,
    /// writes none.
    #[test]
    fn a_reopen_marker_is_taken_once_and_only_by_its_bundle_on_its_mac_within_its_bound() {
        use Subject::{Debugger, DeveloperTools, FullDiskAccess};
        const OTHER_MAC: &str = "8F2E6A41-0B3C-4D5E-A697-C8D9E0F1A2B3";
        let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
        let state = dir.path();
        let marker = state.join(REOPEN_MARKER);
        let (bundle, other) = (host_bundle("aterm.app"), host_bundle("aterm (dev).app"));
        let pages = [FullDiskAccess, DeveloperTools, Debugger];
        let at = 1_793_161_750;
        let within = REOPEN_WITHIN.as_secs();
        let leave = || leave_reopen_in(state, at, &pages, &bundle, MAC).expect("the marker");
        let ours = Some(bundle.as_path());

        leave();
        assert_eq!(
            take_reopen_in(state, at + within, ours, Some(MAC)),
            Some(pages.to_vec())
        );
        assert!(!marker.exists(), "taken");
        assert_eq!(take_reopen_in(state, at + within, ours, Some(MAC)), None);

        leave();
        assert_eq!(
            take_reopen_in(state, at - 30, ours, Some(MAC)),
            Some(pages.to_vec()),
            "a clock set back"
        );
        leave();
        for (this, mac) in [
            (Some(other.as_path()), Some(MAC)),
            (ours, Some(OTHER_MAC)),
            (None, Some(MAC)),
            (ours, None),
        ] {
            assert_eq!(
                take_reopen_in(state, at + 1, this, mac),
                None,
                "{this:?} on {mac:?}"
            );
            assert!(
                marker.exists(),
                "left for its own aterm: {this:?} on {mac:?}"
            );
        }
        assert_eq!(
            take_reopen_in(state, at + 1, ours, Some(MAC)),
            Some(pages.to_vec()),
            "its own aterm takes it after them"
        );

        for this in [ours, Some(other.as_path())] {
            leave();
            assert_eq!(
                take_reopen_in(state, at + within + 1, this, Some(MAC)),
                None
            );
            assert!(!marker.exists(), "an older one is removed: {this:?}");
        }
        std::fs::write(&marker, "junk\n").expect("junk");
        assert_eq!(take_reopen_in(state, at, ours, Some(MAC)), None);
        assert!(!marker.exists(), "junk removed");

        let split = PathBuf::from(format!("{}\nx.app", bundle.display()));
        assert!(leave_reopen_in(state, at, &pages, &split, MAC).is_err());
        assert!(leave_reopen_in(state, at, &pages, &bundle, "MAC A").is_err());
        assert!(leave_reopen_in(state, at, &pages, &bundle, "").is_err());
        assert!(!marker.exists());
    }
}
