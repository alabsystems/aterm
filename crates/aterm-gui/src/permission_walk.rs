// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission walk (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.4a): one helper panel
//! that takes the person through every macOS grant aterm still lacks, in [`Subject::ORDER`] —
//! Full Disk Access, Developer Tools, then Accessibility (only for the Messages and FaceTime
//! sign-outs) — each page opening its own System Settings deep link and turning to the next a
//! moment after its switch reads on ([`crate::devtools_helper::DONE_FOR`]), or once the question
//! macOS asks after it is gone. Debugger access is no page of the walk's: setup's one early
//! elevation turns developer mode on (`aterm_machines::root_steps`).
//!
//! * It starts by itself only in a process the owner started ([`starts`]; owner ruling
//!   2026-11-09: a walk that starts itself after an update is not the person's, so an update's
//!   successor starts none and what the Mac lacks stays on System State's Permissions rows and
//!   in the walk record for the next launch of theirs), with no typing ([`START_KEYS_QUIET`]), an
//!   aterm window holding keyboard focus ([`StartFacts::in_front`]: System Settings comes up over
//!   what the person is looking at, so only while that is aterm), the Mac Setup tab not
//!   the next macOS ask (`App::mac_setup_tab_comes_next`), no install of Apple's tools running
//!   that may still ask or that a quit would cut off (this launch's tab,
//!   `App::access_gesture_up`; an adopted one or an installer,
//!   `App::developer_tools_install_elsewhere`; neither once this launch's tabs are carried past
//!   their password, `App::developer_tools_install_carried`), and System Settings asking nothing
//!   (`App::system_settings_may_ask`), over the pages an earlier walk had still to show whose
//!   time has come ([`WALK_RECORD`], [`KeptPage::due`]); else, on a Mac where aterm never
//!   brought a page up, over every grant
//!   missing (the first walk); else over Accessibility alone, once per approved revision, when
//!   the setup run's reading says the sign-outs need it (`App::look_for_walk_start`). Automatic
//!   acquisition requires no visit to System State and no recent keyboard activity.
//!   `aterm ctl permissions walk now` (at most once a minute, [`VERB_AGAIN_AFTER`])
//!   and the Permissions row's press start one; the verb sent while one is under way joins it
//!   ([`WalkLane::join`]) and takes back a page the walk passed
//!   only as often as it may start one. A verb's ask its control lane gave up before the main
//!   thread took it (the client answered `ERR late`, or gone) starts none
//!   ([`App::take_permission_walk_ask`]).
//! * A walk keeps the pages it has still to show, from the one it is at on, in `<state>`'s walk
//!   record ([`WALK_RECORD`]) at its start and at each turn, for this `.app` on this Mac under
//!   the approved revision ([`WalkLane::keep_ahead`]), so however this process ends (aterm's quit,
//!   System Settings' Quit & Reopen once Full Disk Access is switched on, an update's handoff, a
//!   logout, a crash) the next one walks on from there under the start's other rules, though its
//!   pages came up before. The record goes once the walk ends by the person's answer (the close
//!   box, a Not now that ends it, System Settings quit under its page) or runs out
//!   ([`WalkLane::end`]), and a page the person left for its ten minutes leaves it alone
//!   ([`WalkLane::lapse`]): what the person left is theirs, offered on System State's Permissions
//!   row, never brought up again by itself: offered, never nagged. A walk that stops because aterm
//!   could not show a page (its panel gone or replaced, a page that could not open) keeps its
//!   pages, each counting one more walk in a row that left it so ([`WalkLane::stop`]): such a page
//!   comes back by itself on [`WALK_AGAIN`]'s schedule, whose tries and last time the record keeps,
//!   so in this process or the next, never at every launch and never left to a press. A walk that
//!   gives way to a press elsewhere counts none ([`WalkLane::give_way`]).
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
//!   is up and the person is at the Mac the window reads its grant every [`GRANT_POLL`]. A
//!   page's ten minutes count only the person's time: behind a lock, while nobody touches the
//!   Mac, or while a quit is asked of this process, they stop, and nothing is read
//!   (`App::count_walk_page_time`), so the page is there when they come back, and a going
//!   process never lapses its page. A page that lapses after ten minutes of theirs ends the walk
//!   giving way: the pages after it start again at the next quiet moment, and the page that
//!   lapsed leaves the walk record, the person's to give from System State
//!   ([`WalkLane::lapse`]), so no page comes back again and again. Accessibility
//!   reading on asks macOS's Automation question too, and a walk that asked it opens no page while
//!   it is out ([`WalkReach::automation_out`]), so its next page never comes up beside that
//!   question. Where Accessibility already reads on, so no walk comes to its page, the window
//!   asks the question by itself once per approved revision while the setup run's rows say a
//!   sign-out waits for its answer (`App::look_for_automation_ask`).
//!   Full Disk Access reading on brings macOS's Quit & Reopen sheet over System Settings: the
//!   page's done words wait, naming the next page, until that sheet is gone and no quit is asked
//!   of this process ([`App::turn_after_fda`]), so the next page's link is never lost under it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{Access, AccessOutcome};
use aterm_types::retry_schedule::Backoff;

use crate::App;
use crate::devtools_helper::{Closed, HandOver, Moment, Origin, Page};

/// Presence window for interactive-page timers. Starting automatic acquisition does not depend
/// on this window.
pub(crate) const START_PRESENT_WITHIN: Duration = Duration::from_secs(60);

/// How long the keyboard must have been still for the walk to start by itself: a word being
/// typed (a vault password, a first command) is not cut off by System Settings.
pub(crate) const START_KEYS_QUIET: Duration = Duration::from_secs(2);

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
/// probe) before it opens, or is tried, without one.
pub(crate) const UNREAD_WAIT: Duration = Duration::from_secs(3);

/// The walk record in `<state>`: for each `.app` and Mac, the pages its walk has still to show
/// under an approved revision, each with the walks in a row that left it unanswered
/// ([`keep_pages_in`], [`left_pages_in`]). `<state>` is shared by a network home's Macs and by a
/// dev bundle beside the release, so each keeps a line of its own.
pub(crate) const WALK_RECORD: &str = "permission-walk";

/// The Quit & Reopen marker an aterm before the walk record wrote in `<state>`: the quit's unix
/// seconds and the walk's pages' words on its first line, the `.app`'s path on the second, the
/// Mac's hardware UUID on the third. A Quit & Reopen under such an aterm can open into this one
/// (a staged update), so the first read of the walk record takes this `.app`'s marker on this
/// Mac into it and removes it ([`left_pages_in`]).
const OLD_REOPEN_MARKER: &str = "permission-walk-reopen";

/// The most the walk record holds, and the most of [`OLD_REOPEN_MARKER`] read.
const MAX_RECORD_BYTES: usize = 4096;

/// The most lines the walk record keeps, the newest: a line for each `.app` and Mac.
const MAX_RECORD_LINES: usize = 16;

/// When a page a walk left because aterm could not show it (its panel went or never came,
/// another panel replaced it, it could not open) comes back by itself: a day after the first
/// walk that left it so, twice the last pause after each later one in a row, never more than a
/// Not now's own hold (`atpkg::devtools::DECLINE_HOLD_S`), so an unanswered page neither comes
/// back at every launch nor waits on the person's press ([`KeptPage::due`]). The tries and the
/// last one's time live in the walk record, so a relaunch, an update or a reboot reads the same
/// due time, in this process or the next.
pub(crate) const WALK_AGAIN: Backoff = Backoff::new(
    Duration::from_secs(86_400),
    Duration::from_secs(atpkg::devtools::DECLINE_HOLD_S),
);

/// A grant the walk asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Subject {
    /// Privacy & Security ▸ Full Disk Access.
    FullDiskAccess,
    /// Privacy & Security ▸ Developer Tools.
    DeveloperTools,
    /// Privacy & Security ▸ Accessibility: only the sign-outs need it.
    Accessibility,
}

impl Subject {
    /// The walk's order: Full Disk Access first, since without it the Developer Tools list
    /// cannot be read back.
    pub(crate) const ORDER: [Self; 3] = [
        Self::FullDiskAccess,
        Self::DeveloperTools,
        Self::Accessibility,
    ];

    /// The subject's word on the control verb and in aterm.log.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "fda",
            Self::DeveloperTools => "devtools",
            Self::Accessibility => "accessibility",
        }
    }

    /// The subject a control verb's word names: its [`Self::word`] or the row's name.
    pub(crate) fn parse(word: &str) -> Option<Self> {
        match word {
            "fda" | "full-disk-access" => Some(Self::FullDiskAccess),
            "devtools" | "developer-tools" => Some(Self::DeveloperTools),
            "accessibility" => Some(Self::Accessibility),
            _ => None,
        }
    }

    /// The helper panel's page for the subject.
    pub(crate) const fn page(self) -> Page {
        match self {
            Self::FullDiskAccess => Page::FullDiskAccess,
            Self::DeveloperTools => Page::DeveloperTools,
            Self::Accessibility => Page::Accessibility,
        }
    }

    /// The System State row whose grant the subject is: its [`Access::name`] names it on the
    /// panel.
    pub(crate) const fn access(self) -> Access {
        match self {
            Self::FullDiskAccess => Access::FullDiskAccess,
            Self::DeveloperTools => Access::DeveloperTools,
            Self::Accessibility => Access::Accessibility,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::FullDiskAccess => 0,
            Self::DeveloperTools => 1,
            Self::Accessibility => 2,
        }
    }
}

/// What [`starts`] judges, read at the moment it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartFacts {
    /// The process may start a walk by itself (`App::walk_may_start_by_itself`): the owner
    /// started it; an update's successor starts none (owner ruling 2026-11-09). Whether
    /// aterm brought up the Full Disk Access or Developer Tools page for this bundle on this Mac
    /// before (either page's opening of this TCC identity) is read only once these facts say
    /// start (`App::fresh_mac`).
    pub(crate) cold: bool,
    /// A key was typed within [`START_KEYS_QUIET`].
    pub(crate) typing: bool,
    /// An aterm window holds keyboard focus (`App::any_os_window_focused`): System Settings, and
    /// macOS's Automation question, come up over what the person is looking at, so only while
    /// that is aterm.
    pub(crate) in_front: bool,
}

/// Whether the walk starts by itself now. Pure.
#[must_use]
pub(crate) const fn starts(facts: StartFacts) -> bool {
    facts.cold && !facts.typing && facts.in_front
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
    let mut seen = [false; Subject::ORDER.len()];
    named
        .iter()
        .chain(Subject::ORDER.iter())
        .copied()
        .filter(|subject| !std::mem::replace(&mut seen[subject.index()], true))
        .filter(|subject| missing(*subject))
        .collect()
}

/// A page the walk record keeps: its subject, how many walks in a row left it unanswered
/// (`tries`) and when the last of them did (`at`, unix seconds). It comes back by itself once
/// [`WALK_AGAIN`] says ([`Self::due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeptPage {
    pub(crate) subject: Subject,
    /// The walks in a row that left the page because aterm could not show it ([`WalkLane::stop`]).
    pub(crate) tries: u32,
    /// The unix second the last of them did; nothing while `tries` is none.
    pub(crate) at: u64,
}

impl KeptPage {
    /// A page no walk left unanswered: due at once.
    pub(crate) const fn new(subject: Subject) -> Self {
        Self {
            subject,
            tries: 0,
            at: 0,
        }
    }

    /// The unix second the page comes back by itself; none for one no walk left unanswered.
    pub(crate) fn due_at(self) -> Option<u64> {
        (self.tries > 0).then(|| WALK_AGAIN.due(self.at, self.tries))
    }

    /// Whether a walk that starts by itself may take the page at `now` (unix seconds, `None`
    /// unread): no walk left it unanswered, or [`WALK_AGAIN`]'s pause after the last that did has
    /// passed. A page that waits waits on an unread clock too.
    pub(crate) fn due(self, now: Option<u64>) -> bool {
        self.due_at()
            .is_none_or(|due| now.is_some_and(|now| now >= due))
    }

    /// The page as one more walk in a row left it unanswered at `now` (unix seconds).
    const fn left_unanswered(self, now: u64) -> Self {
        Self {
            tries: self.tries.saturating_add(1),
            at: now,
            ..self
        }
    }

    /// The page's word in a record line: the subject's [`Subject::word`], with `:<tries>:<at>`
    /// after it once a walk left it unanswered. Pure.
    fn token(self) -> String {
        if self.tries == 0 {
            self.subject.word().to_owned()
        } else {
            format!("{}:{}:{}", self.subject.word(), self.tries, self.at)
        }
    }

    /// [`Self::token`] read back; `None` for a word this aterm does not know, or numbers that do
    /// not read whole. Pure.
    fn read_token(token: &str) -> Option<Self> {
        let digits = |field: &str| !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit());
        let mut fields = token.split(':');
        let subject = Subject::parse(fields.next()?)?;
        match (fields.next(), fields.next(), fields.next()) {
            (None, None, None) => Some(Self::new(subject)),
            (Some(tries), Some(at), None) if digits(tries) && digits(at) => Some(Self {
                subject,
                tries: tries.parse().ok()?,
                at: at.parse().ok()?,
            }),
            _ => None,
        }
    }
}

/// When the window put macOS's Automation question for System Events under a walk record line's
/// revision, by itself or at the walk's Accessibility page: the `tries`-th time in a row, the
/// last at `at` (unix seconds). The question is asked again only on [`WALK_AGAIN`]'s pause after
/// it, and only while a sign-out still reads it unanswered
/// ([`aterm_types::macos_setup::StageRow::AUTOMATION_UNANSWERED`]); a new revision asks again by
/// itself, its line starting over. Kept in the walk record, so a relaunch, a Quit & Reopen or an
/// update's successor asks no sooner than the process that asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutomationAsked {
    pub(crate) tries: u32,
    pub(crate) at: u64,
}

impl AutomationAsked {
    /// The record word that names it, before `:<tries>:<at>`: no subject's word.
    const WORD: &'static str = "automation";

    /// The question asked once more at `now` (unix seconds), after `last` (`None`: the first time
    /// under the revision).
    pub(crate) const fn after(last: Option<Self>, now: u64) -> Self {
        Self {
            tries: match last {
                Some(last) => last.tries.saturating_add(1),
                None => 1,
            },
            at: now,
        }
    }

    /// The unix second it may be asked again at `now` ([`WALK_AGAIN`]'s pause, read by
    /// [`Backoff::due_by`], so a clock set back asks at once rather than never).
    pub(crate) fn due_by(self, now: u64) -> u64 {
        WALK_AGAIN.due_by(self.at, self.tries, now)
    }

    /// Its word in a record line: `automation:<tries>:<at>`. Pure.
    fn token(self) -> String {
        format!("{}:{}:{}", Self::WORD, self.tries, self.at)
    }

    /// [`Self::token`] read back; `None` for any other word. Pure.
    fn read_token(token: &str) -> Option<Self> {
        let digits = |field: &str| !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit());
        let mut fields = token.split(':');
        if fields.next()? != Self::WORD {
            return None;
        }
        match (fields.next(), fields.next(), fields.next()) {
            (Some(tries), Some(at), None) if digits(tries) && digits(at) => Some(Self {
                tries: tries.parse().ok()?,
                at: at.parse().ok()?,
            }),
            _ => None,
        }
    }
}

/// One line of the walk record ([`record_line`]): the pages the walk of `bundle` on `mac` had
/// still to show under `revision`, and when the window asked macOS's Automation question under it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    /// The Mac, by its hardware UUID: `<state>` is shared by a network home's Macs.
    mac: String,
    /// The approved setup revision the pages were kept under: a newer one asks again by itself.
    revision: u64,
    /// The pages, in the walk's order, the one it was at first, then any that wait for their
    /// time outside it.
    pages: Vec<KeptPage>,
    /// When macOS's Automation question was put under `revision` ([`AutomationAsked`]).
    automation: Option<AutomationAsked>,
    /// The `.app` whose walk kept them.
    bundle: PathBuf,
}

/// A walk record line: the Mac's hardware UUID, the revision, the pages' words
/// ([`KeptPage::token`], spaces between, then [`AutomationAsked::token`] where one was asked)
/// and the bundle's path (spaces and all), with a tab between each; `None` for a bundle path
/// that is not UTF-8. Pure.
fn record_line(line: &Line) -> Option<String> {
    let words: Vec<String> = line
        .pages
        .iter()
        .map(|page| page.token())
        .chain(line.automation.map(AutomationAsked::token))
        .collect();
    Some(format!(
        "{}\t{}\t{}\t{}",
        line.mac,
        line.revision,
        words.join(" "),
        line.bundle.to_str()?
    ))
}

/// Whether `mac` can stand as a record's Mac: one word. Pure.
fn one_word(mac: &str) -> bool {
    !mac.is_empty() && !mac.contains(char::is_whitespace)
}

/// The pages `words` name ([`KeptPage::read_token`]), each once, in their order, a word that does
/// not read left out. Pure.
fn read_pages<'a>(words: impl Iterator<Item = &'a str>) -> Vec<KeptPage> {
    let mut seen = [false; Subject::ORDER.len()];
    words
        .filter_map(KeptPage::read_token)
        .filter(|page| !std::mem::replace(&mut seen[page.subject.index()], true))
        .collect()
}

/// [`record_line`] read back: each page once, in its order, a word that does not read left out,
/// and the Automation question's ask (the first that reads). `None` for anything else: a Mac
/// that is not one word, no revision, neither a page nor an ask, a bundle path that is not
/// absolute, or a field more or fewer. Pure.
fn read_line(line: &str) -> Option<Line> {
    let mut fields = line.split('\t');
    let (mac, revision, words, bundle) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() || !one_word(mac) {
        return None;
    }
    if revision.is_empty() || !revision.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let revision = revision.parse::<u64>().ok()?;
    let pages = read_pages(words.split(' '));
    let automation = words.split(' ').find_map(AutomationAsked::read_token);
    let bundle = PathBuf::from(bundle);
    ((!pages.is_empty() || automation.is_some()) && bundle.is_absolute()).then(|| Line {
        mac: mac.to_owned(),
        revision,
        pages,
        automation,
        bundle,
    })
}

/// The lines of `state`'s walk record that read whole ([`read_line`]); none for no record.
fn read_record(state: &Path) -> Vec<Line> {
    crate::config_marker::read_marker(&state.join(WALK_RECORD), MAX_RECORD_BYTES)
        .map(|text| text.lines().filter_map(read_line).collect())
        .unwrap_or_default()
}

/// Keep `pages`, the pages the walk of `bundle` on the Mac `mac` (its hardware UUID) has still
/// to show and those outside it the record still holds, in `state`'s walk record under `revision`, in
/// place of the pages that `.app` on that Mac had: no page removes its line, unless it holds no
/// Automation ask under `revision` either ([`keep_automation_ask_in`], which these pages keep),
/// and the record goes with its last line. Other `.app`s' and Macs' lines stay, the newest that
/// fit in [`MAX_RECORD_LINES`] and [`MAX_RECORD_BYTES`]; a line that does not read whole goes.
/// The record is written whole in place of the last (`config_marker::write_marker`, ordered
/// before the rename), so a process that ends at any moment leaves the old record or the new one.
///
/// # Errors
/// `bundle`'s path is not one tab-free line of UTF-8, `mac` is not one word, the line is larger
/// than the record may hold, or the record could not be written or removed
/// (`config_marker::write_marker`, `config_marker::remove_marker`).
pub(crate) fn keep_pages_in(
    state: &Path,
    bundle: &Path,
    mac: &str,
    revision: u64,
    pages: &[KeptPage],
) -> std::io::Result<()> {
    write_line_in(state, bundle, mac, revision, |line| {
        (pages.to_vec(), line.and_then(|line| line.automation))
    })
}

/// Keep `asked`, the window's last ask of macOS's Automation question under `revision`, in
/// `state`'s walk record line for `bundle` on `mac`, beside the pages it holds under that
/// revision ([`keep_pages_in`], which keeps the ask in turn).
///
/// # Errors
/// As [`keep_pages_in`].
pub(crate) fn keep_automation_ask_in(
    state: &Path,
    bundle: &Path,
    mac: &str,
    revision: u64,
    asked: AutomationAsked,
) -> std::io::Result<()> {
    write_line_in(state, bundle, mac, revision, |line| {
        (
            line.map(|line| line.pages.clone()).unwrap_or_default(),
            Some(asked),
        )
    })
}

/// The window's last ask of macOS's Automation question that `state`'s walk record keeps for
/// `bundle` on `mac` under `revision` ([`keep_automation_ask_in`]); none under another revision.
pub(crate) fn automation_asked_in(
    state: &Path,
    bundle: &Path,
    mac: &str,
    revision: u64,
) -> Option<AutomationAsked> {
    read_record(state)
        .into_iter()
        .find(|line| line.mac == mac && line.bundle == bundle && line.revision == revision)?
        .automation
}

/// Write `state`'s walk record line for `bundle` on `mac` under `revision` as `line` makes it
/// from the line it had under that revision (`None` for none, or one under another revision): its
/// pages and Automation ask, the line going where it holds neither. As [`keep_pages_in`] says.
fn write_line_in(
    state: &Path,
    bundle: &Path,
    mac: &str,
    revision: u64,
    line: impl FnOnce(Option<&Line>) -> (Vec<KeptPage>, Option<AutomationAsked>),
) -> std::io::Result<()> {
    bundle
        .to_str()
        .filter(|path| !path.contains(['\n', '\r', '\t']))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the bundle's path is not one tab-free line of UTF-8",
            )
        })?;
    if !one_word(mac) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the Mac's hardware UUID is not one word",
        ));
    }
    let (mut lines, this): (Vec<Line>, Vec<Line>) = read_record(state)
        .into_iter()
        .partition(|line| line.mac != mac || line.bundle != bundle);
    let (pages, automation) = line(this.iter().find(|line| line.revision == revision));
    if !pages.is_empty() || automation.is_some() {
        lines.push(Line {
            mac: mac.to_owned(),
            revision,
            pages,
            automation,
            bundle: bundle.to_path_buf(),
        });
    }
    let record = state.join(WALK_RECORD);
    if lines.is_empty() {
        return crate::config_marker::remove_marker(&record);
    }
    let mut newest: Vec<String> = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().rev().filter_map(record_line) {
        bytes += line.len() + 1;
        if newest.len() == MAX_RECORD_LINES || bytes > MAX_RECORD_BYTES {
            break;
        }
        newest.push(line);
    }
    if newest.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the line is larger than the walk record may hold",
        ));
    }
    let text: String = newest
        .iter()
        .rev()
        .map(|line| format!("{line}\n"))
        .collect();
    crate::config_marker::write_marker(
        &record,
        &text,
        MAX_RECORD_BYTES,
        crate::durable::Flush::Ordered,
    )
}

/// The pages `state`'s walk record holds for `bundle` (the `.app` this aterm runs from) on `mac`
/// (this Mac's hardware UUID) under `revision`: what this `.app`'s walk on this Mac had still to
/// show when the process that kept them ended, however it ended, each with the walks in a row
/// that left it unanswered. Its line under another revision is removed, starting nothing: a new
/// revision asks again by itself. Another `.app`'s or Mac's line is left for the aterm it names.
/// None for no line.
///
/// An [`OLD_REOPEN_MARKER`] this `.app` wrote on this Mac is taken in first: its pages, due at
/// once (its Quit & Reopen left them), lead this line under `revision`, and the marker goes. One
/// that does not read goes too; another `.app`'s or Mac's stays for the aterm it names.
pub(crate) fn left_pages_in(
    state: &Path,
    bundle: &Path,
    mac: &str,
    revision: u64,
) -> Vec<KeptPage> {
    let old = take_old_reopen_in(state, bundle, mac);
    let line = read_record(state)
        .into_iter()
        .find(|line| line.mac == mac && line.bundle == bundle);
    let kept = match line {
        Some(line) if line.revision == revision => line.pages,
        Some(line) => {
            aterm_log::info!(
                "permission walk: the pages kept under revision {} are no walk under revision \
                 {revision}; their line goes",
                line.revision
            );
            if old.is_none()
                && let Err(error) =
                    write_line_in(state, bundle, mac, line.revision, |_| (Vec::new(), None))
            {
                aterm_log::warn!(
                    "permission walk: the walk record's old line was not removed: {error}"
                );
            }
            Vec::new()
        }
        None => Vec::new(),
    };
    let Some(old) = old else {
        return kept;
    };
    let pages: Vec<KeptPage> = old
        .iter()
        .copied()
        .map(KeptPage::new)
        .chain(kept.into_iter().filter(|page| !old.contains(&page.subject)))
        .collect();
    aterm_log::info!(
        "permission walk: an older aterm's Quit & Reopen left {}; the walk record keeps them",
        old.iter()
            .map(|subject| subject.word())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Err(error) = keep_pages_in(state, bundle, mac, revision, &pages) {
        aterm_log::warn!("permission walk: the walk record was not written: {error}");
    }
    pages
}

/// [`OLD_REOPEN_MARKER`]'s pages, when `state` holds one `bundle` wrote on `mac`, the marker then
/// removed; one that does not read is removed, and another `.app`'s or Mac's is left in place.
fn take_old_reopen_in(state: &Path, bundle: &Path, mac: &str) -> Option<Vec<Subject>> {
    let path = state.join(OLD_REOPEN_MARKER);
    let text = crate::config_marker::read_marker(&path, MAX_RECORD_BYTES)?;
    let read = read_old_reopen(&text);
    if read
        .as_ref()
        .is_some_and(|(of, on, _)| of != bundle || on != mac)
    {
        return None;
    }
    if let Err(error) = crate::config_marker::remove_marker(&path) {
        aterm_log::warn!("permission walk: the old Quit & Reopen marker was not removed: {error}");
    }
    read.map(|(_, _, pages)| pages)
}

/// An [`OLD_REOPEN_MARKER`]'s text read back: the bundle, the Mac and the pages (each once, in
/// their order, a word this aterm does not know left out). `None` for anything else: no seconds,
/// no page, a bundle path that is not absolute, a Mac that is not one word, or a line more or
/// fewer. Pure.
fn read_old_reopen(text: &str) -> Option<(PathBuf, String, Vec<Subject>)> {
    let mut lines = text.lines();
    let (first, bundle, mac) = (lines.next()?, lines.next()?, lines.next()?);
    if lines.next().is_some() || !one_word(mac) {
        return None;
    }
    let mut words = first.split(' ');
    let seconds = words.next()?;
    if seconds.is_empty() || !seconds.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pages: Vec<Subject> = read_pages(words.filter(|word| !word.contains(':')))
        .into_iter()
        .map(|page| page.subject)
        .collect();
    let bundle = PathBuf::from(bundle);
    (!pages.is_empty() && bundle.is_absolute()).then(|| (bundle, mac.to_owned(), pages))
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
    named: [bool; Subject::ORDER.len()],
    at: usize,
    step: Step,
    /// When the current page became due.
    due_at: Instant,
    /// How long the current page has been held off with the person at the Mac
    /// ([`Self::count_held_off`]), and the last look that counted it, with whether it read the
    /// person present.
    held_off: (Duration, Option<(Instant, bool)>),
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
            held_off: (Duration::ZERO, None),
            shown: 0,
        })
    }

    /// Take `take` too, as a walk under way takes the control verb's ask: each subject the walk
    /// has not still to show goes on after its last page. On a walk
    /// that asks for a page by name, each subject of `named` (each of `take` when `named` names
    /// none) counts as asked for by name; a walk that asks for none (the first launch's) goes on
    /// from every Not now, and its new pages are its own. Returns the subjects that went on.
    pub(crate) fn take_also(&mut self, take: &[Subject], named: &[Subject]) -> Vec<Subject> {
        let names = self.named.contains(&true);
        let named = if named.is_empty() { take } else { named };
        let mut added = Vec::new();
        for subject in take {
            if !self.ahead().contains(subject) {
                self.subjects.push(*subject);
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

    /// Count the time the current page has been held off (it could not open) at a look at
    /// `now`, `present` whether the look reads the person at the Mac: the time since the last
    /// look counts only with the person present at both, as the walk's page counts its own ten
    /// minutes (`devtools_helper::Lane::count_walk_page_time`), and the Mac asleep counts nothing
    /// on this clock. The total.
    pub(crate) fn count_held_off(&mut self, now: Instant, present: bool) -> Duration {
        let (counted, last) = &mut self.held_off;
        if let Some((at, was)) = *last
            && was
            && present
        {
            *counted = counted.saturating_add(now.saturating_duration_since(at));
        }
        *last = Some((now, present));
        *counted
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
    /// skip it at its turn ([`App::walk_reads_held`]).
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
        self.held_off = (Duration::ZERO, None);
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
    /// page and its page's time stands still.
    pub(crate) quit_asked: fn() -> bool,
    /// Keep, in `<state>`'s walk record, the pages a walk has still to show and those that wait
    /// for their time under an approved revision, for the `.app` this aterm runs from on this Mac
    /// ([`keep_pages_in`]); no page removes that line.
    pub(crate) keep_pages: fn(u64, &[KeptPage]),
    /// The pages `<state>`'s walk record holds for the `.app` this aterm runs from on this Mac
    /// under an approved revision ([`left_pages_in`]): what its walk had still to show when the
    /// process that kept them ended, each with the walks in a row that left it unanswered.
    pub(crate) left_pages: fn(u64) -> Vec<KeptPage>,
    /// Keep, in the same line of `<state>`'s walk record, the window's last ask of macOS's
    /// Automation question under an approved revision ([`keep_automation_ask_in`]).
    pub(crate) keep_automation_ask: fn(u64, AutomationAsked),
    /// The window's last ask of macOS's Automation question that line keeps under an approved
    /// revision ([`automation_asked_in`]).
    pub(crate) automation_asked: fn(u64) -> Option<AutomationAsked>,
    /// The clock the walk record's times are read and written by, in unix seconds
    /// (`aterm_time::unix_secs`); `None` unread.
    pub(crate) unix_now: fn() -> Option<u64>,
}

impl std::fmt::Debug for WalkReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkReach").finish_non_exhaustive()
    }
}

impl WalkReach {
    /// Nothing reached: never trusted, nothing asked, nothing out, no Not now read or noted, no
    /// quit asked, no walk record kept or read, no clock read.
    pub(crate) const INERT: Self = Self {
        ax_trusted: || false,
        ask_automation: || {},
        automation_out: || false,
        ax_last_declined: || None,
        ax_declined: || false,
        note_ax_declined: |_| {},
        quit_asked: || false,
        keep_pages: |_, _| {},
        left_pages: |_| Vec::new(),
        keep_automation_ask: |_, _| {},
        automation_asked: |_| None,
        unix_now: || None,
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
            keep_pages: live_keep_pages,
            left_pages: live_left_pages,
            keep_automation_ask: live_keep_automation_ask,
            automation_asked: live_automation_asked,
            unix_now: aterm_time::unix_secs,
        }
    }
}

/// The `.app` this aterm runs from, as the Quit & Reopen helper finds the one it opens again
/// (`crate::quit_reopen::leave_reopen_at_quit`).
fn this_bundle() -> Option<PathBuf> {
    aterm_containment::consent::fence_exe()
        .and_then(|exe| aterm_containment::consent::app_bundle_root(&exe))
}

/// Where the walk record's line for this aterm is: `<state>`, the `.app` this aterm runs from
/// and this Mac's hardware UUID.
fn record_place() -> Option<(PathBuf, PathBuf, String)> {
    Some((
        aterm_types::dirs::state_dir()?,
        this_bundle()?,
        aterm_containment::consent::host_uuid()?,
    ))
}

fn live_keep_pages(revision: u64, pages: &[KeptPage]) {
    let Some((state, bundle, mac)) = record_place() else {
        aterm_log::warn!("permission walk: no walk record (no <state>, .app or Mac to name)");
        return;
    };
    if let Err(error) = keep_pages_in(&state, &bundle, &mac, revision, pages) {
        aterm_log::warn!("permission walk: the walk record was not written: {error}");
    }
}

fn live_left_pages(revision: u64) -> Vec<KeptPage> {
    record_place().map_or_else(Vec::new, |(state, bundle, mac)| {
        left_pages_in(&state, &bundle, &mac, revision)
    })
}

fn live_keep_automation_ask(revision: u64, asked: AutomationAsked) {
    let Some((state, bundle, mac)) = record_place() else {
        aterm_log::warn!("permission walk: no walk record (no <state>, .app or Mac to name)");
        return;
    };
    if let Err(error) = keep_automation_ask_in(&state, &bundle, &mac, revision, asked) {
        aterm_log::warn!("permission walk: the walk record was not written: {error}");
    }
}

fn live_automation_asked(revision: u64) -> Option<AutomationAsked> {
    let (state, bundle, mac) = record_place()?;
    automation_asked_in(&state, &bundle, &mac, revision)
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

/// The App-owned walk lane: the walk under way, the subjects walks took this process, the walk
/// record as this process knows it, and the reach.
#[derive(Debug)]
pub(crate) struct WalkLane {
    reach: WalkReach,
    walk: Option<Walk>,
    /// Subjects a walk took in this process, by [`Subject::ORDER`]: no walk that starts by
    /// itself takes one again in this process, unless the walk that took it left it in the walk
    /// record unanswered ([`Self::stop`], [`Self::give_way`], [`Self::lapse`]), whose time for it
    /// then decides ([`KeptPage::due`]) in this process as in the next.
    walked: [bool; Subject::ORDER.len()],
    /// Who started the walk under way, for aterm.log.
    by: &'static str,
    /// The approved revision the walk under way started under ([`Self::start`]): the walk record
    /// keeps its pages under it, and nothing for a walk started while none stood.
    walk_revision: Option<u64>,
    /// The walk record's line for this `.app` on this Mac, as this process knows it.
    record: Record,
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
    /// The key of the last look that found an opening that might be another bundle's or Mac's
    /// ([`Found::ThisKey`]): a look under the same key finds the same, so it reads no marker.
    held_under: Option<String>,
    /// The walk under way asked macOS's Automation question (its Accessibility page read on): it
    /// opens no page while that question is out ([`WalkReach::automation_out`]), unless a
    /// person's press takes it over ([`Self::pressed`]).
    asked_automation: bool,
    /// The window's last ask of macOS's Automation question under an approved revision, as the
    /// walk record keeps it ([`AutomationAsked`], [`Self::automation_asked`]): read once per
    /// revision, and kept as this process asks.
    automation_ask: Option<(u64, Option<AutomationAsked>)>,
    /// Whether `<state>` holds the Accessibility page back from the control verb
    /// ([`App::accessibility_declined_for_the_verb`]), and the key it was read under: read once
    /// per key, since the walk's start looks at every park, and held from the moment this
    /// process notes a Not now ([`Self::note_ax_declined`]).
    ax_declined: Option<(String, bool)>,
    /// The approved revision under which a look found the Accessibility page handed in the
    /// access record ([`App::accessibility_handed`]): a line the record never loses, so it is
    /// not read again under that revision.
    ax_handed: Option<u64>,
}

/// The walk record's line for this `.app` on this Mac ([`WALK_RECORD`]), as this process knows
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Record {
    /// Not read yet: the first look that would start a walk, or the first walk that keeps its
    /// pages, reads it ([`WalkLane::take_left`]).
    Unread,
    /// The pages it holds under an approved revision, as read or as this process last kept them.
    Holds(u64, Vec<KeptPage>),
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
            walked: [false; Subject::ORDER.len()],
            by: "",
            walk_revision: None,
            record: Record::Unread,
            verb_started_at: None,
            yields: false,
            slot_won: false,
            page_seen: false,
            held_under: None,
            asked_automation: false,
            automation_ask: None,
            ax_declined: None,
            ax_handed: None,
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

    /// Keep `pages` in the walk record under `revision` ([`WalkReach::keep_pages`]) unless it
    /// holds them already as this process knows it.
    fn keep(&mut self, revision: u64, pages: Vec<KeptPage>) {
        if matches!(&self.record, Record::Holds(held, kept) if *held == revision && *kept == pages)
        {
            return;
        }
        (self.reach.keep_pages)(revision, &pages);
        self.record = Record::Holds(revision, pages);
    }

    /// Keep the pages the walk under way has still to show ([`Walk::ahead`]) under the revision
    /// it started under, each with the walks in a row the record has leaving it unanswered, and
    /// beside them the record's pages outside the walk ([`walk_pages`]); the record is read
    /// first if this process has not read it. Nothing under no revision.
    fn keep_ahead(&mut self) {
        let Some(revision) = self.walk_revision.filter(|_| self.walk.is_some()) else {
            return;
        };
        self.take_left(revision);
        let Some(walk) = self.walk.as_ref() else {
            return;
        };
        let pages = walk_pages(walk, self.kept(revision));
        self.keep(revision, pages);
    }

    /// Read the walk record's line under `revision` ([`WalkReach::left_pages`]) unless this
    /// process knows what it holds under that revision already: the pages an earlier process's
    /// walk had still to show, however that process ended, each with the walks in a row that
    /// left it unanswered. A page whose last such walk reads later than the clock's now (a clock
    /// set back, a walk that ended while the clock was unread) is kept as left unanswered now,
    /// so its pause counts from a time the clock has seen.
    pub(crate) fn take_left(&mut self, revision: u64) {
        if matches!(&self.record, Record::Holds(held, _) if *held == revision) {
            return;
        }
        let mut left = (self.reach.left_pages)(revision);
        if !left.is_empty() {
            aterm_log::info!(
                "permission walk: the walk record keeps {} from an earlier process",
                words(&left)
            );
        }
        if let Some(now) = (self.reach.unix_now)()
            && left.iter().any(|page| page.tries > 0 && page.at > now)
        {
            for page in &mut left {
                if page.tries > 0 && page.at > now {
                    page.at = now;
                }
            }
            (self.reach.keep_pages)(revision, &left);
        }
        self.record = Record::Holds(revision, left);
    }

    /// The pages the walk record holds under `revision`, as this process knows it: none before
    /// [`Self::take_left`] read it.
    pub(crate) fn kept(&self, revision: u64) -> &[KeptPage] {
        match &self.record {
            Record::Holds(held, pages) if *held == revision => pages,
            Record::Holds(..) | Record::Unread => &[],
        }
    }

    /// When the soonest page the walk record keeps that waits for its time comes back by itself
    /// ([`KeptPage::due_at`]), as an instant from `now`; none with no such page or an unread
    /// clock. The park wakes for it, so a page comes back in a process that outlives its pause.
    pub(crate) fn next_due(&self, now: Instant) -> Option<Instant> {
        let Record::Holds(_, pages) = &self.record else {
            return None;
        };
        let unix = (self.reach.unix_now)()?;
        pages
            .iter()
            .filter_map(|page| page.due_at())
            .filter(|due| *due > unix)
            .min()
            .map(|due| now + Duration::from_secs(due - unix))
    }

    /// Start `walk`, started `by` (aterm.log's words) under the approved `revision`; it replaces
    /// any under way and takes the pages it opens as the owner's press would. The walk record
    /// keeps its pages under that revision from now on ([`Self::keep_ahead`]), in place of the
    /// line an earlier walk left: a page that line had keeps its walks in a row left unanswered,
    /// and one outside this walk stays.
    pub(crate) fn start(&mut self, walk: Walk, by: &'static str, revision: Option<u64>) {
        self.yields = false;
        self.slot_won = false;
        // macOS's Automation question the window put just before ([`App::look_for_automation_ask`],
        // asked ahead of the walk's pages) holds its first page as the walk's own ask would: the
        // pages follow the answer, never beside the alert.
        self.asked_automation = (self.reach.automation_out)();
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
        self.walk_revision = revision;
        self.walk = Some(walk);
        self.keep_ahead();
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
        self.keep_ahead();
        true
    }

    /// The walk under way on to its next subject at `now` ([`Walk::advance`]), the walk record
    /// keeping the pages it has still to show: the next subject, `None` once it is over or with
    /// no walk under way.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<Subject> {
        let next = self.walk.as_mut()?.advance(now);
        self.keep_ahead();
        next
    }

    /// [`Self::advance`] on to the next subject asked for by name ([`Walk::advance_to_named`]).
    pub(crate) fn advance_to_named(&mut self, now: Instant) -> Option<Subject> {
        let next = self.walk.as_mut()?.advance_to_named(now);
        self.keep_ahead();
        next
    }

    /// The window's last ask of macOS's Automation question under `revision`
    /// ([`AutomationAsked`]): the walk record's, read once per revision.
    fn automation_asked(&mut self, revision: u64) -> Option<AutomationAsked> {
        match self.automation_ask {
            Some((under, asked)) if under == revision => asked,
            _ => {
                let asked = (self.reach.automation_asked)(revision);
                self.automation_ask = Some((revision, asked));
                asked
            }
        }
    }

    /// The window asked macOS's Automation question under `revision` just now: kept in the walk
    /// record ([`WalkReach::keep_automation_ask`]) once the clock reads, and in this process
    /// either way, so neither asks again before [`AutomationAsked::due_by`].
    fn note_automation_ask(&mut self, revision: u64) {
        let last = self.automation_asked(revision);
        let clock = (self.reach.unix_now)();
        let asked = AutomationAsked::after(last, clock.unwrap_or_default());
        if clock.is_some() {
            (self.reach.keep_automation_ask)(revision, asked);
        }
        self.automation_ask = Some((revision, Some(asked)));
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

    /// The walk is over by the person's answer, or ran out (every page answered, the close box,
    /// a Not now that ends it): the walk record keeps none of its pages, only those outside it
    /// (a person's press walked one page while the record held others for the walk that starts
    /// by itself, or held pages that wait for their time).
    pub(crate) fn end(&mut self, why: &str) {
        let Some(walk) = self.walk.take() else {
            return;
        };
        if let Some(revision) = self.walk_revision {
            let rest: Vec<KeptPage> = self
                .kept(revision)
                .iter()
                .filter(|page| !walk.subjects().contains(&page.subject))
                .copied()
                .collect();
            self.keep(revision, rest);
        }
        aterm_log::info!("permission walk ({}): over ({why})", self.by);
    }

    /// The walk stops because aterm could not show the pages it has still to show (its panel
    /// went or never came, another panel replaced it, its page could not open): each counts one
    /// more walk in a row that left it so, and the walk record keeps them, free in this process
    /// too, so they come back by themselves once [`WALK_AGAIN`] says, in this process or the
    /// next, and not at every launch ([`Self::leave`]). System Settings quit under a page is the
    /// person's answer, which ends the walk ([`Self::end`]).
    pub(crate) fn stop(&mut self, why: &str) {
        self.leave(|_| true, None, why);
    }

    /// The walk steps aside for a person's press elsewhere (`App::press_meets_unseen_panel`, a
    /// press on another row): the subjects it had not gone past, its current page's included,
    /// stay in the walk record with no walk counted against them, and are free again in this
    /// process too, so the walk that starts by itself at the next quiet moment walks on from them
    /// (`App::look_for_walk_start`). A press is no answer to a page it never saw.
    pub(crate) fn give_way(&mut self, why: &str) {
        let Some(walk) = self.walk.as_ref() else {
            return;
        };
        for subject in walk.ahead() {
            self.walked[subject.index()] = false;
        }
        self.leave(|_| false, None, why);
    }

    /// The walk's page lapsed: its ten minutes of the person's time at the Mac
    /// (`devtools_helper::Panel::count_time`) passed with no answer. The walk gives way as to a
    /// press ([`Self::give_way`]) for the pages after it, which the walk that starts by itself
    /// takes up at the next quiet moment; the page that lapsed leaves the walk record: the
    /// person had it in front of them and left it, so it is theirs to give from System State's
    /// Permissions row, and no walk brings it up by itself again: offered, never nagged. It
    /// stays walked in this process.
    pub(crate) fn lapse(&mut self) {
        let Some(walk) = self.walk.as_ref() else {
            return;
        };
        let lapsed = walk.current();
        for subject in walk.ahead() {
            if Some(*subject) != lapsed {
                self.walked[subject.index()] = false;
            }
        }
        self.leave(|_| false, lapsed, "its page lapsed");
    }

    /// The walk under way ends `why` with the pages it has still to show unanswered: each that
    /// `unanswered` names counts one more walk in a row that left it so, at the clock's now (or,
    /// unread, at a time the next read takes as now, [`Self::take_left`]); every one but
    /// `dropped` stays in the walk record and is free in this process, so whether a walk that
    /// starts by itself takes it is the record's to say ([`KeptPage::due`]). `dropped` leaves the
    /// record and stays walked here. Under no revision nothing is kept, and they stay walked
    /// here.
    fn leave(&mut self, unanswered: impl Fn(Subject) -> bool, dropped: Option<Subject>, why: &str) {
        self.keep_ahead();
        let Some(walk) = self.walk.take() else {
            return;
        };
        let Some(revision) = self.walk_revision else {
            aterm_log::info!("permission walk ({}): stops ({why})", self.by);
            return;
        };
        let at = (self.reach.unix_now)().unwrap_or(u64::MAX);
        let ahead = walk.ahead();
        let pages: Vec<KeptPage> = self
            .kept(revision)
            .iter()
            .filter(|page| Some(page.subject) != dropped)
            .map(|page| {
                if ahead.contains(&page.subject) && unanswered(page.subject) {
                    page.left_unanswered(at)
                } else {
                    *page
                }
            })
            .collect();
        for subject in ahead.iter().filter(|subject| Some(**subject) != dropped) {
            self.walked[subject.index()] = false;
        }
        aterm_log::info!(
            "permission walk ({}): stops ({why}); the walk record keeps {}",
            self.by,
            words(&pages)
        );
        self.keep(revision, pages);
    }
}

/// The walk record's pages for `walk` over `kept`, what the record holds: the pages `walk` has
/// still to show, each with the walks in a row `kept` has leaving it unanswered, then those of
/// `kept` that are none of the walk's subjects, due or not ([`KeptPage::due`]): a page leaves the
/// record only by its walk's answer or by a look that reads it no longer wanted
/// (`App::look_for_walk_start`). Pure.
fn walk_pages(walk: &Walk, kept: &[KeptPage]) -> Vec<KeptPage> {
    let of = |subject: Subject| {
        kept.iter()
            .find(|page| page.subject == subject)
            .copied()
            .unwrap_or(KeptPage::new(subject))
    };
    walk.ahead()
        .iter()
        .map(|subject| of(*subject))
        .chain(
            kept.iter()
                .filter(|page| !walk.subjects().contains(&page.subject))
                .copied(),
        )
        .collect()
}

/// The record words of `pages` ([`KeptPage::token`]), for aterm.log.
fn words(pages: &[KeptPage]) -> String {
    pages
        .iter()
        .map(|page| page.token())
        .collect::<Vec<_>>()
        .join(", ")
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
}

impl App {
    /// The walk's turn before the hand-overs: with none under way, start one by itself when its
    /// rules say so ([`Self::look_for_walk_start`]); then [`Self::drive_permission_walk`].
    pub(crate) fn tick_permission_walk(&mut self, now: Instant) {
        if !cfg!(target_os = "macos") || !self.devtools_helper.windowed() {
            return;
        }
        self.count_walk_page_time(now);
        // macOS's Automation question goes first, at the same look, so the walk that starts
        // beside it opens its pages after the answer (`WalkLane::start`): one sitting.
        if !self.permission_walk.active() {
            self.look_for_automation_ask(now);
        }
        if !self.permission_walk.active() {
            self.look_for_walk_start(now);
        }
        self.drive_permission_walk(now);
    }

    /// Ask macOS's Automation question for System Events by itself, early and batched with the
    /// walk (H27): at a look where the plan in force signs out of Messages or FaceTime, whose
    /// clicks go through System Events, and Accessibility already reads on, so no walk comes to its
    /// page, whose grant asks it ([`Self::read_walk_grant`]). The first look of a process that
    /// may start a walk by itself asks it, ahead of any walk's pages (`WalkLane::start` holds them
    /// for the answer), with no sign-out row needed: the sign-outs come last in the setup run,
    /// and asking then would be late and alone. Under the walk's own start rules
    /// ([`Self::look_for_walk_start`]): an admitted revision, a process that may start a walk by
    /// itself, no typing, an aterm window holding keyboard focus ([`StartFacts::in_front`]),
    /// none of the window's gestures or aterm's macOS dialogs up, the Mac
    /// Setup tab not next, no install of the tools running outside this launch's tab
    /// (`App::developer_tools_install_elsewhere`), and System Settings asking nothing; and only
    /// with a person at the Mac, its screen in use (`App::screen_away`) and touched within
    /// [`START_PRESENT_WITHIN`], as a walk page's time counts ([`Self::walk_page_time_stands`]),
    /// so the question never goes up behind a lock. The ask's Apple event is a macOS dialog up
    /// until it returns ([`WalkReach::automation_out`]), and the sign-out's run reads the answer
    /// itself at its next look. The walk record keeps when it was asked under the revision
    /// ([`AutomationAsked`]), as does the walk's own ask: it is asked again, on [`WALK_AGAIN`]'s
    /// pause after the last ask, only while a sign-out still reads macOS's answer missing
    /// ([`aterm_types::macos_setup::StageRow::AUTOMATION_UNANSWERED`]), and a new revision asks
    /// it again by itself. Nothing nags.
    fn look_for_automation_ask(&mut self, now: Instant) {
        let cold = self.walk_may_start_by_itself();
        let Some(revision) = self
            .access_revision()
            .filter(|_| cold && self.access_steps_allowed() && self.permission_steps_admitted())
        else {
            return;
        };
        if (self.permission_walk.reach().automation_out)() {
            return;
        }
        let status = self.system_state_host.latest_status();
        let signs_out = status
            .plan
            .as_ref()
            .is_some_and(|plan| Access::Accessibility.needed_for(plan.selection));
        let unanswered = status.rows.iter().any(|row| {
            row.detail
                .contains(aterm_types::macos_setup::StageRow::AUTOMATION_UNANSWERED)
        });
        let facts = StartFacts {
            cold,
            typing: self
                .last_keystroke_at
                .is_some_and(|at| now.saturating_duration_since(at) < START_KEYS_QUIET),
            in_front: self.any_os_window_focused(),
        };
        if !signs_out
            || !starts(facts)
            || (self.screen_away)()
            || !self.person_present_within(START_PRESENT_WITHIN, now)
            || !(self.permission_walk.reach().ax_trusted)()
            || self.access_gesture_up(now)
            || self.access_dialog_up()
            || self.mac_setup_tab_comes_next()
        {
            return;
        }
        if let Some(last) = self.permission_walk.automation_asked(revision) {
            let clock = (self.permission_walk.reach().unix_now)();
            if !unanswered || !clock.is_some_and(|clock| clock >= last.due_by(clock)) {
                return;
            }
        }
        // Read last, as the walk's own start reads them (`Self::look_for_walk_start`): System
        // Settings asking, and an install of the tools outside this launch's tab, whose prompt or
        // installer window macOS's alert in front would take the keyboard from.
        if self.system_settings_may_ask() || self.developer_tools_install_elsewhere() {
            return;
        }
        aterm_log::info!(
            "permission walk: Accessibility reads on and the plan signs out of an account, whose \
             clicks go through System Events; asking macOS's Automation question"
        );
        (self.permission_walk.reach().ask_automation)();
        self.permission_walk.note_automation_ask(revision);
    }

    /// Start a walk by itself when [`starts`] says so, over the first of these that holds a page:
    ///
    /// 1. The pages the walk record keeps under the approved revision ([`WalkLane::take_left`]:
    ///    an earlier process's walk, however that process ended, or a walk of this one that
    ///    stopped, gave way or lapsed) whose time has come ([`KeptPage::due`]: none for a page no
    ///    walk left unanswered, [`WALK_AGAIN`]'s pause for one a walk did) and that no walk of
    ///    this process took since, still missing, not held back by the owner's Not now and, for
    ///    Accessibility, still offered by the setup run's reading ([`Self::walk_missing`]; until
    ///    the run's status is in, the look waits, [`Self::setup_run_read`]), in the order kept.
    ///    Their pages came up before, so neither the page markers nor this process's walks hold
    ///    them back. A due one this look reads no longer wanted leaves the record, whether or not
    ///    the others start a walk; no other look drops a page.
    /// 2. The subjects missing that no walk of this process took, the walk record does not keep
    ///    (a page that waits for its time comes back by that time alone) and the owner's Not now
    ///    does not hold back ([`Self::declined_for_the_verb`]: a re-ask after a decline waits for
    ///    a quiet moment on the automatic hand-overs): Full Disk Access and Developer Tools on a
    ///    Mac where aterm never brought either page up ([`Self::fresh_mac`]), or once a walk ran
    ///    in this process; Accessibility whenever the setup run's reading says the sign-outs
    ///    need it ([`Self::walk_missing`]), once per approved revision: not once the access
    ///    record holds its page handed under it ([`Self::accessibility_handed`]), whichever
    ///    process handed it.
    ///
    /// A walk the setup run has not admitted (no approved revision, a `[privacy]` switch off)
    /// never starts by itself, nor one while no aterm window holds keyboard focus
    /// ([`StartFacts::in_front`]: its pages wait for the person to be looking at aterm), nor one
    /// while another of the window's gestures or a macOS dialog
    /// of aterm's is up, nor one while the Mac Setup tab comes next
    /// ([`Self::mac_setup_tab_comes_next`]) or an install of Apple's tools runs that may still ask
    /// or that a quit would cut off (this launch's tab is a gesture up while it may ask; an
    /// adopted tab or an installer, [`Self::developer_tools_install_elsewhere`]: Full Disk Access
    /// switched on brings macOS's Quit & Reopen, whose quit would hang up mid-install a tab no
    /// keeper carries; neither once this launch's tabs are carried past their password,
    /// [`Self::developer_tools_install_carried`]), or System Settings asks
    /// something
    /// ([`Self::system_settings_may_ask`], a question seen asking that lapsed off screen among
    /// them: the walk opens its pages). Readiness does not require a focused System State page or
    /// a recent-input signal. This runs at every park, so the in-memory readings come first, then
    /// the walk record, the page markers, the declines, the subjects missing and the access
    /// record (files and grants read), and System Settings asking and an install of the tools
    /// elsewhere last (one may read the window list, the other this Mac's processes), read once
    /// per look and only at a look that would start a walk.
    fn look_for_walk_start(&mut self, now: Instant) {
        let cold = self.walk_may_start_by_itself();
        let Some(revision) = self
            .access_revision()
            .filter(|_| cold && self.access_steps_allowed() && self.permission_steps_admitted())
        else {
            return;
        };
        let facts = StartFacts {
            cold,
            typing: self
                .last_keystroke_at
                .is_some_and(|at| now.saturating_duration_since(at) < START_KEYS_QUIET),
            in_front: self.any_os_window_focused(),
        };
        if !starts(facts)
            || self.access_gesture_up(now)
            || self.access_dialog_up()
            || self.mac_setup_tab_comes_next()
        {
            return;
        }
        self.permission_walk.take_left(revision);
        let clock = (self.permission_walk.reach().unix_now)();
        let kept = self.permission_walk.kept(revision).to_vec();
        let left: Vec<Subject> = kept
            .iter()
            .filter(|page| page.due(clock) && !self.permission_walk.walked(page.subject))
            .map(|page| page.subject)
            .collect();
        let (walk, by) = if left.is_empty() {
            let first = self.permission_walk.any_walked() || self.fresh_mac();
            let missing = Subject::ORDER.map(|subject| {
                !self.permission_walk.walked(subject)
                    && !kept.iter().any(|page| page.subject == subject)
                    && (first || subject == Subject::Accessibility)
                    && !self.declined_for_the_verb(subject)
                    && self.walk_missing(subject, false)
                    && (subject != Subject::Accessibility || !self.accessibility_handed(revision))
            });
            let walk = Walk::new(subjects(&[], |subject| missing[subject.index()]), now);
            let by = if walk
                .as_ref()
                .is_some_and(|walk| walk.subjects() == [Subject::Accessibility])
            {
                "the sign-outs"
            } else {
                "first launch"
            };
            (walk, by)
        } else {
            if left.contains(&Subject::Accessibility) && !self.setup_run_read() {
                // Whether the sign-outs still need it is the run's to say; unread is no answer.
                return;
            }
            let missing = Subject::ORDER.map(|subject| {
                left.contains(&subject)
                    && !self.declined_for_the_verb(subject)
                    && self.walk_missing(subject, false)
            });
            let wanted: Vec<KeptPage> = kept
                .iter()
                .filter(|page| !left.contains(&page.subject) || missing[page.subject.index()])
                .copied()
                .collect();
            if wanted.len() < kept.len() {
                aterm_log::info!(
                    "permission walk: of the pages the walk record keeps that are due, {} read \
                     held, declined or no longer wanted; they leave it",
                    left.iter()
                        .filter(|subject| !missing[subject.index()])
                        .map(|subject| subject.word())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                self.permission_walk.keep(revision, wanted);
            }
            let walk = Walk::new(subjects(&left, |subject| missing[subject.index()]), now);
            if walk.is_none() {
                return;
            }
            (walk, "walks on")
        };
        let Some(walk) = walk else {
            return;
        };
        // Read last, once per look, and only at a look that would start the walk: one that opens
        // System Settings' pages, so a question seen asking that lapsed off screen holds it too;
        // and an install of the tools outside this launch's tab, whose probe reads this Mac's
        // processes, which no park should pay for.
        if self.system_settings_may_ask() || self.developer_tools_install_elsewhere() {
            return;
        }
        self.permission_walk.start(walk, by, Some(revision));
    }

    /// Whether the access record holds the Accessibility page handed under `revision`
    /// ([`Self::handed_this_revision`]): a walk of this process or an earlier one brought it up
    /// then (`Self::open_walk_page` records it as it opens), so no walk takes it by itself again
    /// under that revision, and a new revision offers it again. Read once found
    /// (`WalkLane::ax_handed`): the record keeps that line.
    fn accessibility_handed(&mut self, revision: u64) -> bool {
        if self.permission_walk.ax_handed == Some(revision) {
            return true;
        }
        let handed = self.access_revision() == Some(revision)
            && self.handed_this_revision(Access::Accessibility);
        if handed {
            self.permission_walk.ax_handed = Some(revision);
        }
        handed
    }

    /// Whether the setup run's status is in: its plan published (`system_state_host`), so
    /// [`Self::system_state_offers`] answers from a reading. Before it, a row it does not offer
    /// is unread, not unwanted.
    fn setup_run_read(&self) -> bool {
        self.system_state_host.latest_status().plan.is_some()
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

    /// Whether this process may start a walk by itself ([`StartFacts::cold`]): the owner started
    /// it (`devtools_helper::Lane::cold_start`). An update's successor (a launch into a staged
    /// update, a seamless hand-over, a relaunch of the running aterm) starts none by itself
    /// (owner ruling 2026-11-09): the pages the walk record keeps wait for the next process the
    /// owner starts, or for a press on a Permissions row.
    fn walk_may_start_by_itself(&self) -> bool {
        self.devtools_helper.cold_start()
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
        self.start_walk_without(named, [false; Subject::ORDER.len()], false, by, now)
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
        held_back: [bool; Subject::ORDER.len()],
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
        let revision = self.access_revision();
        self.permission_walk.start(walk, by, revision);
        self.permission_walk.yields = verb;
        if verb {
            self.permission_walk.verb_started_at = Some(now);
        }
        self.drive_permission_walk(now);
        Started::Walking { pages, not_now }
    }

    /// Whether the walk takes `subject`: its grant does not read held, and, for one it was not
    /// asked for by name, the setup run wants it (Accessibility: System State's row offers it,
    /// so the sign-outs need it).
    pub(crate) fn walk_missing(&mut self, subject: Subject, named: bool) -> bool {
        let wanted = match subject {
            Subject::FullDiskAccess | Subject::DeveloperTools => true,
            Subject::Accessibility => named || self.system_state_offers(Access::Accessibility),
        };
        wanted && self.walk_held(subject) != Held::Yes
    }

    /// `aterm ctl permissions walk now [<subject>…]` (and `devtools hand-over now`): a walk as
    /// [`Self::start_permission_walk`] starts one, at most once per [`VERB_AGAIN_AFTER`] from
    /// the last walk the verb started or page it took back, and without a page the owner's Not
    /// now holds back ([`Self::declined_for_the_verb`]): any program in a session can send the
    /// verb, and only the owner's own press goes past a Not now. The walk reads that hold again
    /// before each page, and opens its first only under the one-gesture rule
    /// ([`Self::open_walk_page`]). A walk under way takes the verb's subjects and answers with
    /// the pages it has still to show. A walk it starts or joins brings System State in front,
    /// whose card says each page.
    pub(crate) fn ask_permission_walk(&mut self, named: &[Subject], now: Instant) -> Started {
        let held_back = Subject::ORDER.map(|subject| self.declined_for_the_verb(subject));
        let started = self.start_walk_without(named, held_back, true, "a control client", now);
        // The walk's pages are the setup screen's Needs-you card's
        // (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.8): System State comes in front
        // to say each one, where the person sees it beside System Settings.
        if matches!(started, Started::Walking { .. }) {
            self.open_system_state_tab();
        }
        started
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
        use aterm_containment::consent::{FdaState, ProbeLabel};
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
        }
    }

    /// The walk's page place for a panel `origin` shows `page`: the walk's own page only.
    pub(crate) fn walk_step(&self, page: Page, origin: Origin) -> Option<(usize, usize)> {
        let walk = self.permission_walk.walk()?;
        (origin == Origin::Walk && walk.current().map(Subject::page) == Some(page))
            .then(|| walk.position(|subject| self.walk_reads_held(subject)))
    }

    /// The walk's page now and its place in the walk, for the setup screen's Needs-you card
    /// (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.8): one page of one says no step.
    pub(crate) fn system_state_walk_page(&self) -> Option<crate::system_state::WalkPage> {
        let walk = self.permission_walk.walk()?;
        let subject = walk.current()?;
        let (at, of) = walk.position(|subject| self.walk_reads_held(subject));
        (of > 1).then_some(crate::system_state::WalkPage {
            access: subject.access(),
            at,
            of,
        })
    }

    /// Whether `subject`'s grant reads held by the readings already in, as [`Self::walk_held`]
    /// judges it but starting no reading: the walk's count of the pages it will show
    /// ([`Walk::position`]), read at every turn while a page is up.
    pub(crate) fn walk_reads_held(&self, subject: Subject) -> bool {
        use aterm_containment::consent::{FdaState, ProbeLabel};
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
            && let Some(page) = walk.current().map(Subject::page)
            && !self.devtools_helper.walk_page_alive(page)
        {
            // Its panel never came up (or went without a close the walk heard): the page in
            // System Settings stays, as after a press with no panel; nothing in this process
            // walks on from it, and the walk record keeps its pages for the next.
            self.permission_walk.stop("its panel is gone");
        }
        self.read_walk_grant(now);
        self.turn_after_fda(now);
        self.open_due_walk_pages(now);
        match self.permission_walk.walk() {
            Some(walk) => {
                let next = walk.next().map(|subject| subject.access().name());
                let up = walk
                    .current()
                    .map(Subject::page)
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
    /// walk there, as does the close box (on done words that wait for macOS's question too):
    /// those are the person's answers, so the walk record keeps none of the pages the walk
    /// passed or ended on (`WalkLane::end`), and so is System Settings quit under its page: the
    /// person left it, and what they left is theirs, offered on System State, never brought up
    /// again by itself. Another panel replacing it answers no page: the walk stops, its pages
    /// kept, each counting one more walk that left it unanswered, to come back by itself once
    /// `WALK_AGAIN` says (`WalkLane::stop`). A give-way to a press elsewhere (whose walk already
    /// stepped aside) counts none (`WalkLane::give_way`). The page's ten minutes of the person's
    /// time (`devtools_helper::Panel::count_time`) end it giving way, the pages after it free for
    /// the walk that starts by itself, the page that lapsed leaving the record
    /// (`WalkLane::lapse`). A Not now or the
    /// close box on the Accessibility page, the walk's alone, is noted as its decline
    /// (`WalkLane::note_ax_declined`), as the lane notes the other pages'.
    fn walk_page_closed(&mut self, page: Page, why: Closed, now: Instant) {
        if page == Page::Accessibility && why.declines() {
            let key = self.access_marks().fda;
            self.permission_walk.note_ax_declined(key);
        }
        let Some(walk) = self.permission_walk.walk() else {
            return;
        };
        if walk.current().map(Subject::page) != Some(page) || walk.step() != Step::Up {
            return;
        }
        let named = walk.at_named();
        let lane = &mut self.permission_walk;
        match why {
            Closed::Done | Closed::SwitchedOn => {
                let _ = lane.advance(now);
            }
            Closed::NotNow if !named => {
                let _ = lane.advance(now);
            }
            Closed::NotNow => {
                if lane.advance_to_named(now).is_none() {
                    lane.end("Not now on the last page asked for");
                }
            }
            Closed::CloseBox | Closed::Waiting => lane.end("the close box"),
            Closed::Lapsed => lane.lapse(),
            Closed::SettingsGone => lane.end("System Settings quit under its page"),
            Closed::Replaced => lane.stop("another panel replaced its page"),
            Closed::GaveWay => lane.give_way("its panel gave way to a person's press elsewhere"),
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

    /// The walk's look at the time of its page that is up: the page's ten minutes count only
    /// while the person is at the Mac with its screen in use, and its grant and list are read only
    /// then (`devtools_helper::Lane::count_walk_page_time`), so a page left up behind a lock, or
    /// while nobody touches the Mac, is there when they come back and spends nothing meanwhile.
    /// The person is away once the screen is locked, asleep or another user's
    /// (`App::screen_away`), or nobody has touched the Mac for [`START_PRESENT_WITHIN`]
    /// ([`Self::person_present_within`]). While a quit is asked of this process
    /// ([`WalkReach::quit_asked`]) the time stands too: the process is going, and a page that
    /// lapsed while it went would leave the walk record without it. The page behind another app
    /// does not stop its time: a person who moved on to other work leaves it, and it lapses ten
    /// minutes into theirs.
    fn count_walk_page_time(&mut self, now: Instant) {
        let present = !self.permission_walk.active() || !self.walk_page_time_stands(now);
        self.devtools_helper.count_walk_page_time(now, present);
    }

    /// Whether the walk's page time stands still ([`Self::count_walk_page_time`]): the person is
    /// away from it, or this process is going.
    fn walk_page_time_stands(&self, now: Instant) -> bool {
        (self.screen_away)()
            || !self.person_present_within(START_PRESENT_WITHIN, now)
            || (self.permission_walk.reach().quit_asked)()
    }

    /// While a walk page is up, read its grant every [`GRANT_POLL`] until the page shows its done
    /// words: a fresh Full Disk Access probe (its answer turns the page in
    /// `App::tick_fda_hand_over`) and `AXIsProcessTrusted`. Full Disk Access's done words may
    /// wait minutes for macOS's sheet ([`Self::turn_after_fda`]), and the grant they show is in,
    /// so no probe (a child aterm each) runs under them. The Developer Tools page reads its own
    /// list at that pace. Nothing is read while the person is away
    /// ([`Self::count_walk_page_time`]): no switch flips then.
    fn read_walk_grant(&mut self, now: Instant) {
        let Some(walk) = self.permission_walk.walk() else {
            return;
        };
        if walk.step() != Step::Up || self.devtools_helper.walk_page_rests() {
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
                    if let Some(revision) = self.permission_walk.walk_revision {
                        self.permission_walk.note_automation_ask(revision);
                    }
                }
            }
            Some(Subject::DeveloperTools) | None => {}
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
                    let _ = self.permission_walk.advance(now);
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
                    // A page held off as long as a panel stays up stops the walk, so a dialog
                    // slot that never frees keeps no walk of this process looking for it; the
                    // walk record keeps its pages for the next. Only the person's time at the
                    // Mac counts, as the page's own does: a walk held off while they are away,
                    // or while this process is going, is there when they come back.
                    let present = !self.walk_page_time_stands(now);
                    if self.permission_walk.walk_mut().is_some_and(|walk| {
                        walk.count_held_off(now, present) >= crate::devtools_helper::PANEL_FOR
                    }) {
                        self.permission_walk
                            .stop("its page could not open for ten minutes");
                    }
                    return;
                }
                Opened::Skip(why) => {
                    aterm_log::info!("permission walk: {} skipped ({why})", subject.word());
                    let _ = self.permission_walk.advance(now);
                }
            }
        }
    }

    /// Bring `subject`'s page up as the owner's press would ([`Origin::Walk`]): the window's
    /// dialog slot first, so a page that cannot open now is tried again next turn, never queued
    /// for a quiet moment. The control verb's walk (one that yields) waits out a helper panel it
    /// did not bring up, wins the slot only while no other gesture of the window and no macOS
    /// dialog of this process is up ([`Self::reserve_walk_dialog`]) and then holds it for its
    /// later pages, and skips a page the owner's Not now holds
    /// back now ([`Self::declined_for_the_verb`]), as the verb sent now would: one said since the
    /// verb asked (on the panel the walk waited behind, say) holds it too. A walk that asked
    /// macOS's Automation question opens no page while that
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
            if !self.permission_walk.slot_won {
                if !self.reserve_walk_dialog(now) {
                    return Opened::Wait;
                }
                self.permission_walk.slot_won = true;
            }
            if self.declined_for_the_verb(subject) {
                return Opened::Skip("the owner's Not now holds it back from the control verb");
            }
        }
        if !self.reserve_hand_over_dialog(Origin::Walk) {
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
    /// While the person is away from a page that is up ([`Self::count_walk_page_time`]) the walk
    /// looks every [`crate::devtools_helper::PANEL_TICK`] instead, for their return. With none
    /// under way, the park wakes when the soonest page the walk record keeps that waits for its
    /// time comes back ([`WalkLane::next_due`]), so it does in a process that outlives its pause.
    pub(crate) fn next_permission_walk_deadline(&self, now: Instant) -> Option<Instant> {
        let every = if self.devtools_helper.walk_page_rests() {
            crate::devtools_helper::PANEL_TICK
        } else {
            GRANT_POLL
        };
        match self.permission_walk.walk() {
            Some(_) => Some(now + every),
            None => self.permission_walk.next_due(now),
        }
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
#[path = "permission_walk_tests.rs"]
mod tests;
