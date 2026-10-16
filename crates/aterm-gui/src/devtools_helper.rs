// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE DEVELOPER TOOLS HAND-OVER: the one lane that brings the owner to System Settings ▸
//! Privacy & Security ▸ Developer Tools, and aterm's own panel beside it (owner ask
//! 2026-10-15: "I need some kind of banner explaining why I need this for aterm, and I
//! need for aterm to be autopopulated when this menu appears. make this even lower
//! friction").
//!
//! No app can put itself on that list (`atpkg::devtools`; debugger design §9.1, O19):
//! only the owner adds it, and macOS asks for their password. The most aterm can do is be
//! there when the page opens, saying why, with its own icon ready to drag into the list.
//! Every place that brings the owner there — the window's own once-per-version hand-over
//! (`devtools_watch`), the digest line's press, the Settings ▸ Security card and the
//! control verb — goes through ONE function, `App::hand_devtools_to_owner`, so none of
//! them can drift. This module holds the parts that decide, each testable without a
//! window, a process or System Settings:
//!
//! * [`admit`] — who may bring the page up, and when ([`Origin`] × the reading × the
//!   owner's decline × the build): an owner's press always opens it; the window's own
//!   hand-over and a control verb never on a state aterm did not read, never while the
//!   owner's Not now holds, never on a build whose grant would not survive the next one,
//!   never twice in ten minutes, and only at a quiet moment ([`quiet`]).
//! * [`Panel`] — the panel's states (why and how, with aterm's icon to drag; the variant
//!   for a list aterm cannot read; a drop that did not take, or did not land; done) and
//!   what each of the owner's presses asks of the window. One panel at a time.
//! * [`layout`] and [`place`] — the panel's frames from its words, and where it goes
//!   beside System Settings' window: never over the right pane, which holds the list,
//!   its switches and its + button.
//! * [`Lane`] — the `App`-owned bundle: the hand-over waiting for its quiet moment, the
//!   `spctl` worker for an empty list, the shown panel and its one-shot list reads. The
//!   park reads it (`App::tick_devtools_hand_over`, `DeadlineOwner::DevToolsHandOver`);
//!   workers publish into a shared slot, and their wake (`Wake::DevToolsHandOver`) is a
//!   nudge only, so a wake an uncommitted incoming handoff drops loses nothing.
//! * [`Reach`] and [`DevToolsHelperArm`] — everything the lane does to the Mac and its
//!   markers, and the panel itself, injected: a headless instance and every unit test get
//!   the inert arms, which open, run, read, write and show nothing.
//! * `live` (macOS) — the panel on AppKit, the windowed arm: a non-activating panel
//!   shown only while System Settings is in front, placed beside its window, with
//!   aterm's icon as a Copy-only drag source. `devtools_helper_drive` (the
//!   `objc_devtools_helper_drive` example) builds its views in no window and captures
//!   them; no unit test touches it.
//!
//! What it never does: press, flip, drop or type anything in System Settings, write
//! TCC.db, call tccutil, or ask for administrator rights. The drag is the owner's own
//! gesture; aterm only offers the icon.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::DevToolGrant;
use aterm_containment::consent::{
    DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, ProbeGate, devtool_grant,
};
use aterm_types::MutexExt as _;

// ---------------------------------------------------------------------------
// THE QUIET MOMENT
// ---------------------------------------------------------------------------

/// How long the owner's hands must have been off the keyboard and mouse before aterm
/// puts System Settings in front of them: a pause, not a word being typed (the
/// automatic update's 2.5 s gap is tuned for an apply landing mid-word, not for taking
/// focus away).
pub(crate) const QUIET_FOR: Duration = Duration::from_secs(10);

/// How recently the owner must have touched the keyboard or mouse to count as present:
/// a page opened for nobody is a page found hours later with no idea why.
pub(crate) const PRESENT_WITHIN: Duration = Duration::from_secs(5 * 60);

/// How often a waiting hand-over looks for its quiet moment while an aterm window is
/// focused. Unfocused it arms nothing: the focus gain wakes the loop.
pub(crate) const RECHECK_EVERY: Duration = Duration::from_secs(5);

/// How long a hand-over a control verb asked for waits for its quiet moment. The window's
/// own (once per version) waits as long as it takes.
pub(crate) const ASKED_LAPSE: Duration = Duration::from_secs(30 * 60);

/// Whether now is a moment to put System Settings in front of the owner unasked: an
/// aterm window is focused (they are looking at aterm, not at another app's work), the
/// owner is present ([`PRESENT_WITHIN`]) but not typing or pointing ([`QUIET_FOR`]),
/// this process has settled (`consent_card::LAUNCH_SETTLE`: a successor an update just
/// brought in is not "quiet" the second it starts), and no update hand-off is pending or
/// in flight. The input clock is the machine-wide HID idle time, which carries across an
/// update hand-off where the window's own keystroke stamp starts empty. Pure.
#[must_use]
pub(crate) fn quiet(
    focused: bool,
    input_within_quiet: bool,
    input_within_presence: bool,
    age: Duration,
    handoff_busy: bool,
) -> bool {
    focused
        && !input_within_quiet
        && input_within_presence
        && age >= crate::consent_card::LAUNCH_SETTLE
        && !handoff_busy
}

// ---------------------------------------------------------------------------
// WHO MAY BRING THE PAGE UP
// ---------------------------------------------------------------------------

/// Where a hand-over came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The window's own, once per aterm version (`devtools_watch`): at a quiet moment.
    Automatic,
    /// The Settings ▸ Security card's button: the owner's press.
    Settings,
    /// The digest line's mover: the owner's press.
    Digest,
    /// `aterm ctl devtools hand-over`: every in-session client can send it, so it is held
    /// to the window's own rules. `now` (a person at `aterm doctor`'s terminal) skips
    /// the quiet moment and the build's grant stability (the person asked, for this build),
    /// is held to a minute since the last opening rather than ten (and since the window's
    /// own last answer to a `now`, so an asker that never opens the page cannot raise the
    /// panel again and again), and shows only the panel while doctor opens the page itself.
    Ctl {
        /// Show the panel now instead of waiting for a quiet moment.
        now: bool,
    },
}

impl Origin {
    /// The owner's own press: it opens the page whatever aterm read, declined or built.
    pub(crate) const fn owner_press(self) -> bool {
        matches!(self, Self::Settings | Self::Digest)
    }

    /// How recently aterm may have opened the page with this origin still opening it
    /// again (`atpkg::devtools`' shared marker): never for an owner's press.
    pub(crate) const fn reopen_after_s(self) -> Option<u64> {
        match self {
            Self::Settings | Self::Digest => None,
            Self::Ctl { now: true } => Some(atpkg::devtools::REOPEN_NOW_AFTER_S),
            Self::Automatic | Self::Ctl { now: false } => Some(atpkg::devtools::REOPEN_AFTER_S),
        }
    }

    /// How long this origin's hand-over waits for a quiet moment before it lapses.
    const fn lapses_after(self) -> Option<Duration> {
        match self {
            Self::Ctl { now: false } => Some(ASKED_LAPSE),
            _ => None,
        }
    }

    /// The origin in aterm.log's words.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Settings => "settings",
            Self::Digest => "digest",
            Self::Ctl { now: false } => "ctl",
            Self::Ctl { now: true } => "ctl now",
        }
    }
}

/// When [`admit`] is asked: as the request arrives, or at the quiet moment a waiting
/// one was held for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Moment {
    /// The request itself.
    Asked,
    /// The lane found the quiet moment a queued request waited for.
    Quiet,
}

/// What [`admit`] judges on, read at the moment it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    /// The latest reading's joined verdict (`consent::devtool_grant`).
    pub(crate) grant: DevToolGrant,
    /// The switch's row in the system TCC.db, as last read.
    pub(crate) list: DevToolList,
    /// The owner's Not now still holds (`atpkg::devtools::declined_until`).
    pub(crate) declined: bool,
    /// A grant keyed to this build's designated requirement survives the next build
    /// (`DrClass::grant_stable`): a Developer ID build, not an ad hoc or dev one.
    pub(crate) grant_stable: bool,
    /// aterm opened the page within the origin's [`Origin::reopen_after_s`] — for
    /// `now`, or the window answered a `now` within that minute
    /// ([`Lane::now_answered_within_its_minute`]).
    pub(crate) recent: bool,
}

/// Why a hand-over did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// aterm is on the list and switched on.
    Granted,
    /// aterm could not read the list or tccd's answers: fail-closed, it may be listed.
    Unknown,
    /// The owner said Not now, and it still holds.
    Declined,
    /// A build whose grant the next build would lose (ad hoc, cdhash, dev).
    Unstable,
    /// aterm opened the page moments ago.
    Recent,
    /// aterm's row reads on while macOS still checks its new programs: the page holds
    /// nothing to add or switch on (`atpkg::devtools::Step::ListedOn`).
    Listed,
}

impl Refusal {
    /// The refusal in one word, for aterm.log and the control verb's answer.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Unknown => "unknown",
            Self::Declined => "declined",
            Self::Unstable => "unstable",
            Self::Recent => "recent",
            Self::Listed => "listed",
        }
    }
}

/// What [`admit`] allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Nothing, and why.
    Refuse(Refusal),
    /// Hold it for a quiet moment ([`quiet`]).
    Queue,
    /// Show the panel only: the asker opens the page itself (`aterm doctor`).
    PanelOnly,
    /// Open the page only: aterm is already on and switched on (an owner's press).
    PaneOnly,
    /// Open the page and show the panel; on a list read whole and EMPTY, run
    /// `spctl developer-mode enable-terminal` first so the page exists.
    Hand {
        /// Run `spctl developer-mode enable-terminal` first.
        list_terminal: bool,
    },
}

/// WHO MAY BRING THE PAGE UP, AND WHEN. An owner's press opens it whatever aterm read,
/// with the panel unless aterm reads on; for a list it could not read the panel says so
/// (the owner sees the list; aterm cannot). The window's own hand-over and the control
/// verb act only on a list read not on (`unknown` does nothing: aterm may already be
/// listed; a row read on while macOS still checks has nothing to add, so only `now`'s
/// panel says so), never while the owner's Not now holds, never on a build whose grant would
/// not survive the next build (the `now` form excepted: a person is at the terminal),
/// never within the origin's [`Origin::reopen_after_s`], and — but for `now`, which shows
/// only the panel — only at a quiet moment. Pure.
#[must_use]
pub(crate) fn admit(origin: Origin, moment: Moment, facts: &Facts) -> Admit {
    let empty = facts.list.label == DevToolListLabel::Read && facts.list.rows == Some(0);
    if origin.owner_press() {
        return match facts.grant {
            DevToolGrant::Granted => Admit::PaneOnly,
            DevToolGrant::NotGranted => Admit::Hand {
                list_terminal: empty,
            },
            DevToolGrant::Unknown => Admit::Hand {
                list_terminal: false,
            },
        };
    }
    match facts.grant {
        DevToolGrant::Granted => return Admit::Refuse(Refusal::Granted),
        DevToolGrant::Unknown => return Admit::Refuse(Refusal::Unknown),
        DevToolGrant::NotGranted => {}
    }
    let now = origin == Origin::Ctl { now: true };
    if !now && atpkg::devtools::Step::of(&facts.list) == atpkg::devtools::Step::ListedOn {
        return Admit::Refuse(Refusal::Listed);
    }
    if facts.declined {
        return Admit::Refuse(Refusal::Declined);
    }
    if !facts.grant_stable && !now {
        return Admit::Refuse(Refusal::Unstable);
    }
    if facts.recent {
        return Admit::Refuse(Refusal::Recent);
    }
    match (now, moment) {
        (true, _) => Admit::PanelOnly,
        (false, Moment::Asked) => Admit::Queue,
        (false, Moment::Quiet) => Admit::Hand {
            list_terminal: empty,
        },
    }
}

/// What one hand-over did: aterm.log's word, and the control verb's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandOver {
    /// A headless instance (or not macOS): nothing, ever.
    Headless,
    /// [`admit`] refused.
    Refused(Refusal),
    /// Held for a quiet moment.
    Queued,
    /// The panel is up; the asker opens the page.
    Shown,
    /// The panel could not be shown (the asker opens the page without it).
    Unshown,
    /// System Settings is open at the page (the panel beside it, when not granted).
    Opened,
    /// `spctl developer-mode enable-terminal` is running; the page opens when it ends.
    Listing,
    /// System Settings did not open.
    OpenFailed,
}

impl HandOver {
    /// Whether this brought, or is bringing, the owner to the page.
    pub(crate) const fn brings_owner(self) -> bool {
        matches!(self, Self::Opened | Self::Listing)
    }

    /// The outcome in one word, for aterm.log and the control verb's answer.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Refused(why) => why.as_str(),
            Self::Queued => "queued",
            Self::Shown => "shown",
            Self::Unshown => "unshown",
            Self::Opened => "opened",
            Self::Listing => "listing",
            Self::OpenFailed => "failed",
        }
    }
}

/// The latest completed Developer Tools reading the lane judges by: the window's own
/// reading (`ConsentState::devtools`), or the panel's one-shot list read joined with the
/// tccd answers the window last read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The joined verdict (`consent::devtool_grant` of `list` and `log`).
    pub(crate) grant: DevToolGrant,
    /// The switch's row in the system TCC.db.
    pub(crate) list: DevToolList,
    /// tccd's answers in its window: the newest one, when it came, and how many were
    /// not `Allowed` (new programs macOS checked before letting them start). A list
    /// read alone is joined with it ([`Reading::with_list`]), so a row the list still
    /// holds switched on while tccd answered after it last changed stays `not_granted`.
    pub(crate) log: DevToolProbe,
    /// The bundle id the reading was about.
    pub(crate) subject: Option<&'static str>,
}

impl Reading {
    /// Nothing read yet: `unknown`.
    pub(crate) const UNREAD: Self = Self {
        grant: DevToolGrant::Unknown,
        list: DevToolList::pending(),
        log: DevToolProbe::pending(),
        subject: None,
    };

    /// This reading with a new list read: the verdict joined again with the same tccd
    /// answers, by the window's own rule (`consent::devtool_grant`). A row read on
    /// that has not changed since tccd's newer answer is no grant; one that changed
    /// after it (the owner added aterm, or switched it) is. Pure.
    #[must_use]
    pub(crate) fn with_list(self, list: DevToolList) -> Self {
        Self {
            grant: devtool_grant(&list, &self.log),
            list,
            ..self
        }
    }
}

// ---------------------------------------------------------------------------
// THE PANEL'S WORDS
// ---------------------------------------------------------------------------

/// The tile's caption under aterm's icon.
pub(crate) const TILE_CAPTION: &str = "aterm";

/// The variant for a list aterm cannot read without Full Disk Access: the owner
/// can see it, aterm cannot.
pub(crate) const UNREAD_WORDS: &str = "aterm can't see this list without Full Disk Access. When \
                                       aterm's switch is on, press \"I've switched it on\".";

/// The UNREAD variant's line for the reason aterm cannot read the list: Full
/// Disk Access only when that is the reason ([`UNREAD_WORDS`]); otherwise the
/// reason in atpkg's words, so nobody is sent to grant Full Disk Access for a
/// probe they switched off.
fn unread_words(label: DevToolListLabel) -> std::borrow::Cow<'static, str> {
    if label == DevToolListLabel::NoFullDiskAccess {
        return UNREAD_WORDS.into();
    }
    format!(
        "aterm can't read this list ({}). When aterm's switch is on, press \"I've switched it \
         on\".",
        atpkg::devtools::list_reason(label)
    )
    .into()
}

/// Whether a list label says aterm cannot read the list for a reason the next
/// read will not change (no Full Disk Access, a probe switched off, an answer
/// in a shape it does not know): the UNREAD variant. A read that was not read
/// YET, or that failed on its way (a timeout, a tool that could not start),
/// says nothing lasting: the panel reads again at its next turn, up to
/// [`READ_FAILURES_LASTING`] in a row.
const fn lasting_unread(label: DevToolListLabel) -> bool {
    !matches!(
        label,
        DevToolListLabel::Read
            | DevToolListLabel::Pending
            | DevToolListLabel::OpenFailed
            | DevToolListLabel::ToolFailed
            | DevToolListLabel::TimedOut
            | DevToolListLabel::SpawnFailed
            | DevToolListLabel::OutputUnreadable
    )
}

/// How many reads in a row that failed on their way the panel takes before it
/// treats the failure as lasting: the UNREAD variant, and no more reads. A
/// `sqlite3` that exits non-zero may be meeting a lock that passes or a schema
/// or damaged file that does not (`ToolFailed` is either), and the panel does
/// not start it every [`PANEL_TICK`] for ten minutes to find out.
pub(crate) const READ_FAILURES_LASTING: u32 = 3;

/// The drag ended with nothing taken (the list's drop is unmeasured: spike D15).
pub(crate) const DROP_FAILED_WORDS: &str =
    "Didn't take? Press + below the Developer Tools list and choose aterm.";

/// An accepted drop, and a whole read of the list [`DROP_CONFIRM_WITHIN`] or
/// more later that still does not hold aterm: the panel cannot see which list
/// System Settings shows, so it may have landed on another one — or on Finder,
/// which takes a Copy of the bundle wherever it is let go.
pub(crate) const DROP_UNCONFIRMED_WORDS: &str = "aterm did not reach Developer Tools. If it \
                                                 landed on another list in System Settings, \
                                                 switch it off there; if you let go over \
                                                 Finder, it made a copy of aterm there, which \
                                                 you can move to the Trash.";

/// A drop something took, let go outside System Settings' window (a Finder window,
/// the Desktop, another app): not the list, so the icon stays offered.
pub(crate) const DROP_OUTSIDE_WORDS: &str = "That landed outside System Settings. If Finder \
                                             made a copy of aterm there, you can move that \
                                             copy to the Trash.";

/// How, once the tile is spent (one accepted drop per panel): the + route for a
/// list that does not hold aterm (the drop may have landed elsewhere), then
/// aterm's own switch, then the switches that are not it. Never the drag: there
/// is no tile left to drag.
fn dropped_how() -> String {
    use atpkg::devtools::{CHECK_SWITCH, PLUS_PRESS, WRONG_SWITCHES};
    format!(
        "If aterm is not on the Developer Tools list, {PLUS_PRESS} (macOS asks for your \
         password), {CHECK_SWITCH}. {WRONG_SWITCHES}"
    )
}

/// The footer link to aterm's own status screen, in atpkg's one spelling (`aterm doctor`'s
/// banner prints it too).
pub(crate) const STATUS_LINK: &str = atpkg::devtools::STATUS_LINK;

/// What `spctl developer-mode enable-terminal` failing leaves the owner: the + route.
fn show_list_failed_words(why: &str) -> String {
    format!(
        "macOS did not show the list ({why}). In {}, press + below the list and choose aterm.",
        atpkg::devtools::PANE
    )
}

/// tccd's measured cost, when its log was read whole and it checked any: the Settings
/// row's words.
#[must_use]
pub(crate) fn checked_words(not_allowed: Option<u32>) -> Option<String> {
    let window = aterm_containment::consent::DEVTOOL_WINDOW_S / 60;
    match not_allowed? {
        0 => None,
        1 => Some(format!(
            "In the last {window} minutes macOS checked 1 new program run in aterm before \
             letting it start."
        )),
        n => Some(format!(
            "In the last {window} minutes macOS checked {n} new programs run in aterm before \
             letting each start."
        )),
    }
}

/// Whole minutes from the opened marker's stamp `at` to `now` (unix seconds), for
/// the Settings card. A stamp more than [`atpkg::devtools::STAMP_SKEW_S`] ahead of
/// the clock was written under a clock since corrected and says nothing (atpkg
/// ignores it the same way); one a little ahead is "just now". Pure.
#[must_use]
pub(crate) const fn minutes_since(at: u64, now: u64) -> Option<u64> {
    if at > now.saturating_add(atpkg::devtools::STAMP_SKEW_S) {
        return None;
    }
    Some(now.saturating_sub(at) / 60)
}

/// How a line of the panel is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// The heading: bold.
    Title,
    /// The body.
    Body,
    /// Something the owner's last move changed: emphasized.
    Notice,
    /// The trade and the measured cost: the secondary label colour.
    Secondary,
    /// The done words, beside a check mark.
    Done,
    /// The bundle's path under the tile, and the status link: small.
    Small,
}

/// One of the panel's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    /// Close the panel; the window leaves the owner alone for a while.
    NotNow,
    /// The UNREAD variant's: the owner says aterm's switch is on.
    SwitchedOn,
    /// Run `spctl developer-mode enable-terminal` so an empty list shows, then bring
    /// the page up again: the UNREAD variant's (aterm cannot see whether the list is
    /// empty), and a readable list's while it reads whole and empty with nothing
    /// listed by this panel yet (the hand-over's own run failed, or none ran).
    ShowList,
}

impl Button {
    /// The button's title.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NotNow => "Not now",
            Self::SwitchedOn => "I've switched it on",
            Self::ShowList => "No Developer Tools list? Show it",
        }
    }

    /// Every button, in tag order: the live panel's buttons carry their index
    /// here as their `tag`, so one action reads which was pressed.
    pub(crate) const ALL: [Self; 3] = [Self::NotNow, Self::SwitchedOn, Self::ShowList];

    /// The button's `tag` on the live panel.
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        expect(dead_code, reason = "the AppKit panel is macOS-only")
    )]
    pub(crate) fn tag(self) -> isize {
        Self::ALL
            .iter()
            .position(|button| *button == self)
            .and_then(|index| isize::try_from(index).ok())
            .unwrap_or(-1)
    }

    /// The button a live panel's `tag` names, if any.
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        expect(dead_code, reason = "the AppKit panel is macOS-only")
    )]
    pub(crate) fn from_tag(tag: isize) -> Option<Self> {
        usize::try_from(tag)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
    }

    /// What pressing it asks.
    #[cfg_attr(
        not(any(test, target_os = "macos")),
        expect(dead_code, reason = "the AppKit panel is macOS-only")
    )]
    pub(crate) const fn event(self) -> PanelEvent {
        match self {
            Self::NotNow => PanelEvent::NotNow,
            Self::SwitchedOn => PanelEvent::SwitchedOn,
            Self::ShowList => PanelEvent::ShowList,
        }
    }
}

/// aterm's icon to drag, with its caption, the hint and the bundle's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tile {
    /// The bundle the drag carries ([`drag_bundle`]), shown in small type under the
    /// hint so the + route works for an aterm installed outside /Applications.
    pub(crate) bundle: PathBuf,
    /// Whether it can still be dragged: one accepted drop per panel.
    pub(crate) live: bool,
}

/// The hint beside a spent tile: something took its one drop.
pub(crate) const TILE_SPENT_HINT: &str = "Dropped.";

impl Tile {
    /// The hint beside the icon: the drag while it can be dragged
    /// ([`atpkg::devtools::HELPER_STEP`]), then what happened.
    pub(crate) const fn hint(&self) -> &'static str {
        if self.live {
            atpkg::devtools::HELPER_STEP
        } else {
            TILE_SPENT_HINT
        }
    }
}

/// One block of the panel, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// Wrapped text.
    Text {
        /// The words.
        text: String,
        /// How they are set.
        tone: Tone,
    },
    /// aterm's icon to drag.
    Tile(Tile),
    /// A row of buttons, right-aligned, wrapping when they do not fit.
    Buttons(Vec<Button>),
    /// The status link ([`STATUS_LINK`]).
    Link(&'static str),
}

impl Block {
    fn text(text: impl Into<String>, tone: Tone) -> Self {
        Self::Text {
            text: text.into(),
            tone,
        }
    }
}

/// What the panel shows: its blocks, top to bottom.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Content {
    /// The blocks, top to bottom.
    pub(crate) blocks: Vec<Block>,
}

impl Content {
    /// Every text block's words, in order.
    #[cfg(test)]
    pub(crate) fn texts(&self) -> impl Iterator<Item = (&str, Tone)> {
        self.blocks.iter().filter_map(|block| match block {
            Block::Text { text, tone } => Some((text.as_str(), *tone)),
            _ => None,
        })
    }

    /// The buttons, in order.
    #[cfg(test)]
    pub(crate) fn buttons(&self) -> &[Button] {
        self.blocks
            .iter()
            .find_map(|block| match block {
                Block::Buttons(buttons) => Some(buttons.as_slice()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The tile, when there is one.
    #[cfg(test)]
    pub(crate) fn tile(&self) -> Option<&Tile> {
        self.blocks.iter().find_map(|block| match block {
            Block::Tile(tile) => Some(tile),
            _ => None,
        })
    }
}

// ---------------------------------------------------------------------------
// THE PANEL'S STATES
// ---------------------------------------------------------------------------

/// How long the panel stays up at most: ten minutes.
pub(crate) const PANEL_FOR: Duration = Duration::from_secs(10 * 60);

/// The panel lane's cadence while the panel is up: place it, check System Settings is
/// still running, and (on a readable list) read it once.
pub(crate) const PANEL_TICK: Duration = Duration::from_secs(2);

/// How long the done words stay before the panel closes itself.
pub(crate) const DONE_FOR: Duration = Duration::from_secs(4);

/// How long after an accepted drop a readable list must read aterm on before the
/// panel says it may have landed elsewhere.
pub(crate) const DROP_CONFIRM_WITHIN: Duration = Duration::from_secs(15);

/// How long after a show System Settings may take to start before the lane takes its
/// absence for the owner having quit it (`aterm doctor` opens the page after the
/// window shows the panel).
pub(crate) const SETTINGS_GRACE: Duration = Duration::from_secs(5);

/// What the panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Why and how, with the tile.
    Explain,
    /// The owner's drag ended with nothing taken.
    DropFailed,
    /// Something outside System Settings' window took the owner's drop: the icon is
    /// still offered.
    DroppedOutside,
    /// An accepted drop, and a readable list that did not read aterm on within
    /// [`DROP_CONFIRM_WITHIN`].
    DroppedUnconfirmed,
    /// aterm's switch reads on: the done words, then the panel closes itself.
    Done,
}

/// What the owner can do on the panel (the live arm posts these; nothing else can).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) enum PanelEvent {
    /// Not now, or the close box.
    NotNow,
    /// The UNREAD variant's "I've switched it on".
    SwitchedOn,
    /// "No Developer Tools list? Show it" ([`Button::ShowList`]).
    ShowList,
    /// The status link.
    Status,
    /// The owner's drag of the tile ended: `accepted` when the destination took it.
    DragEnded {
        /// The drag ended with an operation (the destination took it).
        accepted: bool,
    },
    /// The owner's drag ended taken by something outside System Settings' window
    /// ([`drop_landed`]): not the list, so the one drop is not spent.
    DroppedOutside,
}

/// Why the panel closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Closed {
    /// Not now, or the close box, with aterm not switched on: the owner's decline.
    NotNow,
    /// The owner says aterm's switch is on (the UNREAD variant).
    SwitchedOn,
    /// aterm reads on, and the done words have stood.
    Done,
    /// [`PANEL_FOR`] passed.
    Lapsed,
    /// System Settings is no longer running.
    SettingsGone,
    /// Another show replaced it.
    Replaced,
}

impl Closed {
    /// Whether this close is the owner's Not now (`atpkg::devtools::note_declined`).
    pub(crate) const fn declines(self) -> bool {
        matches!(self, Self::NotNow)
    }
}

/// What one event, reading or turn asks of the lane and the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Effect {
    /// The words changed: redraw.
    pub(crate) redraw: bool,
    /// Close the panel, and why.
    pub(crate) close: Option<Closed>,
    /// A reading saw aterm on: end the digest line, refresh Settings.
    pub(crate) granted: bool,
    /// The owner says it is on: drop the cached reading, read on their return.
    pub(crate) recheck: bool,
    /// Run `spctl developer-mode enable-terminal`, then bring the page up again.
    pub(crate) list_terminal: bool,
    /// Open aterm's Settings ▸ Security.
    pub(crate) open_status: bool,
}

impl Effect {
    /// Both effects: the first close wins.
    fn and(self, other: Self) -> Self {
        Self {
            redraw: self.redraw || other.redraw,
            close: self.close.or(other.close),
            granted: self.granted || other.granted,
            recheck: self.recheck || other.recheck,
            list_terminal: self.list_terminal || other.list_terminal,
            open_status: self.open_status || other.open_status,
        }
    }
}

/// What a show starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Show {
    /// Who asked.
    pub(crate) origin: Origin,
    /// The reading the show judged by.
    pub(crate) reading: Reading,
    /// What `spctl developer-mode enable-terminal` answered, when this hand-over ran it
    /// first: Ok, the Terminal row the owner sees is aterm's doing; an error, macOS may
    /// show no list at all, and the panel says why and offers Show it again.
    pub(crate) listed: Option<Result<(), String>>,
    /// The bundle to drag ([`drag_bundle`]); `None` shows the words only.
    pub(crate) bundle: Option<PathBuf>,
    /// `SpikeEvidence::devtool_skips_first_run_check` ("should" until D14).
    pub(crate) skips: bool,
}

/// THE PANEL'S STATE MACHINE: what it shows, and what each of the owner's presses, each
/// reading and each turn of the lane asks. Pure: the lane owns the arm that draws it.
#[derive(Clone, Debug)]
pub(crate) struct Panel {
    origin: Origin,
    shown_at: Instant,
    phase: Phase,
    /// Why aterm cannot read the list, when the reason lasts ([`lasting_unread`],
    /// or [`READ_FAILURES_LASTING`] failed reads in a row): the UNREAD variant.
    /// `None`: the lane reads it, and a read decides a drop.
    unread: Option<DevToolListLabel>,
    /// Reads in a row that failed on their way.
    failed_reads: u32,
    /// The list holds aterm, switched off: the switch, not the drag.
    listed_off: bool,
    /// The list holds aterm switched ON while tccd answered after the switch last
    /// changed (`atpkg::devtools::Step::ListedOn`): nothing to drag in, nothing to
    /// switch on, and a read of the same row is no grant.
    listed_on: bool,
    listed_terminal: bool,
    /// The list was read whole and EMPTY: macOS shows no Developer Tools page until
    /// something is on it, so the panel offers Show it while nothing it ran listed
    /// Terminal.
    empty: bool,
    /// tccd's answers the show judged by (the window's reading, kept current by
    /// [`Self::note_reading`]): a list read is joined with them.
    log: DevToolProbe,
    subject: Option<&'static str>,
    bundle: Option<PathBuf>,
    skips: bool,
    /// The one accepted drop, when it came.
    dropped_at: Option<Instant>,
    /// `spctl developer-mode enable-terminal` failed, in its words.
    show_list_failed: Option<String>,
    done_at: Option<Instant>,
    read_out: bool,
    last_read: Option<Instant>,
}

/// The list was read whole and holds aterm, switched off.
fn listed_off(list: DevToolList) -> bool {
    list.label == DevToolListLabel::Read && list.listing == DevToolListing::Off
}

/// The list was read whole and holds nothing at all.
fn read_empty(list: DevToolList) -> bool {
    list.label == DevToolListLabel::Read && list.rows == Some(0)
}

/// The list was read whole and holds aterm switched on, while the joined verdict is not
/// granted: tccd answered after the switch last changed.
fn listed_on(list: DevToolList, grant: DevToolGrant) -> bool {
    grant != DevToolGrant::Granted
        && atpkg::devtools::Step::of(&list) == atpkg::devtools::Step::ListedOn
}

impl Panel {
    /// A panel shown at `now`.
    pub(crate) fn new(show: Show, now: Instant) -> Self {
        Self {
            origin: show.origin,
            shown_at: now,
            phase: Phase::Explain,
            unread: lasting_unread(show.reading.list.label).then_some(show.reading.list.label),
            failed_reads: 0,
            listed_off: listed_off(show.reading.list),
            listed_on: listed_on(show.reading.list, show.reading.grant),
            listed_terminal: matches!(show.listed, Some(Ok(()))),
            empty: read_empty(show.reading.list),
            log: show.reading.log,
            subject: show.reading.subject,
            bundle: show.bundle,
            skips: show.skips,
            dropped_at: None,
            show_list_failed: show.listed.and_then(Result::err),
            done_at: None,
            read_out: false,
            last_read: Some(now),
        }
    }

    /// What it is showing.
    pub(crate) const fn phase(&self) -> Phase {
        self.phase
    }

    /// Who asked for it.
    pub(crate) const fn origin(&self) -> Origin {
        self.origin
    }

    /// Whether the lane may read the list (otherwise the UNREAD variant).
    pub(crate) const fn readable(&self) -> bool {
        self.unread.is_none()
    }

    /// The words, top to bottom.
    #[must_use]
    pub(crate) fn content(&self) -> Content {
        use atpkg::devtools::{
            HELPER_HOW, HELPER_HOW_PLUS, HELPER_LISTED_ON, HELPER_SWITCH_ON, HELPER_TITLE, done,
            only_aterm, trade, why,
        };
        let mut blocks = vec![Block::text(HELPER_TITLE, Tone::Title)];
        if self.phase == Phase::Done {
            blocks.push(Block::text(done(self.skips), Tone::Done));
            blocks.push(Block::Link(STATUS_LINK));
            return Content { blocks };
        }
        blocks.push(Block::text(why(self.skips), Tone::Body));
        if let Some(words) = checked_words(self.log.not_allowed) {
            blocks.push(Block::text(words, Tone::Secondary));
        }
        // aterm already on the list (off, or on while macOS still checks): nothing to drag.
        let listed = self.listed_off || self.listed_on;
        if let Some(bundle) = self.bundle.as_ref().filter(|_| !listed) {
            blocks.push(Block::Tile(Tile {
                bundle: bundle.clone(),
                live: self.dropped_at.is_none(),
            }));
        }
        match self.phase {
            Phase::DropFailed => blocks.push(Block::text(DROP_FAILED_WORDS, Tone::Notice)),
            Phase::DroppedOutside => blocks.push(Block::text(DROP_OUTSIDE_WORDS, Tone::Notice)),
            Phase::DroppedUnconfirmed => {
                blocks.push(Block::text(DROP_UNCONFIRMED_WORDS, Tone::Notice));
            }
            Phase::Explain | Phase::Done => {}
        }
        if let Some(why) = &self.show_list_failed {
            blocks.push(Block::text(show_list_failed_words(why), Tone::Notice));
        }
        blocks.push(Block::text(
            if self.listed_on {
                HELPER_LISTED_ON.to_owned()
            } else if self.listed_off {
                HELPER_SWITCH_ON.to_owned()
            } else if self.dropped_at.is_some() {
                dropped_how()
            } else if self.bundle.is_none() {
                // No icon to drag: + leads.
                HELPER_HOW_PLUS.to_owned()
            } else {
                HELPER_HOW.to_owned()
            },
            Tone::Body,
        ));
        blocks.push(Block::text(only_aterm(self.listed_terminal), Tone::Body));
        if let Some(label) = self.unread {
            blocks.push(Block::text(unread_words(label), Tone::Notice));
        }
        blocks.push(Block::text(trade(self.skips), Tone::Secondary));
        blocks.push(Block::Buttons(if self.unread.is_some() {
            vec![Button::ShowList, Button::SwitchedOn, Button::NotNow]
        } else if self.empty && !self.listed_terminal {
            vec![Button::ShowList, Button::NotNow]
        } else {
            vec![Button::NotNow]
        }));
        blocks.push(Block::Link(STATUS_LINK));
        Content { blocks }
    }

    /// One of the owner's presses, or the end of their drag.
    pub(crate) fn on(&mut self, event: PanelEvent, now: Instant) -> Effect {
        match event {
            PanelEvent::NotNow => Effect {
                close: Some(if self.phase == Phase::Done {
                    Closed::Done
                } else {
                    Closed::NotNow
                }),
                ..Effect::default()
            },
            PanelEvent::SwitchedOn => Effect {
                close: Some(Closed::SwitchedOn),
                recheck: true,
                ..Effect::default()
            },
            PanelEvent::ShowList => {
                self.show_list_failed = None;
                Effect {
                    list_terminal: true,
                    redraw: true,
                    ..Effect::default()
                }
            }
            PanelEvent::Status => Effect {
                open_status: true,
                ..Effect::default()
            },
            PanelEvent::DragEnded { accepted } => {
                if self.phase == Phase::Done || self.dropped_at.is_some() {
                    return Effect::default();
                }
                if accepted {
                    self.dropped_at = Some(now);
                    self.phase = Phase::Explain;
                    // Read now, not at the next turn: the drop is when it may have moved.
                    self.last_read = None;
                } else {
                    self.phase = Phase::DropFailed;
                }
                Effect {
                    redraw: true,
                    ..Effect::default()
                }
            }
            PanelEvent::DroppedOutside => {
                if self.phase == Phase::Done || self.dropped_at.is_some() {
                    return Effect::default();
                }
                self.phase = Phase::DroppedOutside;
                Effect {
                    redraw: true,
                    ..Effect::default()
                }
            }
        }
    }

    /// A completed read of the list (the lane's one-shot). Read whole and joined with
    /// tccd's answers by the window's own rule (`consent::devtool_grant`): a grant is
    /// done; a row on that has not changed since tccd's newer answer is the ListedOn
    /// words, never done (reading the row the panel opened on again observes no
    /// grant); a row switched off asks for the
    /// switch (and says a drop, or a press of +, landed here); aterm still absent
    /// [`DROP_CONFIRM_WITHIN`] or more after an accepted drop says it may have
    /// landed on another list. A lasting reason it could not be read turns the
    /// panel UNREAD; a read that failed on its way changes nothing, and the next
    /// turn reads again — until [`READ_FAILURES_LASTING`] in a row, which turn
    /// it UNREAD too.
    pub(crate) fn on_read(&mut self, list: DevToolList, now: Instant) -> Effect {
        self.read_out = false;
        self.last_read = Some(now);
        if self.phase == Phase::Done {
            return Effect::default();
        }
        if list.label == DevToolListLabel::Read {
            self.failed_reads = 0;
        } else if !lasting_unread(list.label) {
            self.failed_reads = self.failed_reads.saturating_add(1);
        }
        let changed = match list.label {
            DevToolListLabel::Read
                if list.listing == DevToolListing::On
                    && devtool_grant(&list, &self.log) == DevToolGrant::Granted =>
            {
                return self.granted(now);
            }
            DevToolListLabel::Read => {
                let off = listed_off(list);
                let on = list.listing == DevToolListing::On;
                let empty = read_empty(list);
                let mut changed = self.unread.take().is_some()
                    || off != self.listed_off
                    || on != self.listed_on
                    || empty != self.empty;
                self.listed_off = off;
                self.listed_on = on;
                self.empty = empty;
                let phase = if off || on {
                    // aterm is on this list: whatever the drag did, it is here.
                    Phase::Explain
                } else if self
                    .dropped_at
                    .is_some_and(|at| now >= at + DROP_CONFIRM_WITHIN)
                {
                    Phase::DroppedUnconfirmed
                } else {
                    self.phase
                };
                changed |= phase != self.phase;
                self.phase = phase;
                changed
            }
            label if lasting_unread(label) || self.failed_reads >= READ_FAILURES_LASTING => {
                let changed = self.unread != Some(label);
                self.unread = Some(label);
                changed
            }
            _ => false,
        };
        Effect {
            redraw: changed,
            ..Effect::default()
        }
    }

    /// The window's own reading completed while the panel is up: a grant is done;
    /// otherwise its tccd answers are the ones the panel's next list read is joined
    /// with (a newer answer can turn a row on into ListedOn, or a toggled row into a
    /// grant).
    pub(crate) fn note_reading(&mut self, reading: &Reading, now: Instant) -> Effect {
        if reading.grant == DevToolGrant::Granted {
            return self.granted(now);
        }
        self.log = reading.log;
        Effect::default()
    }

    /// Whether the panel was shown before any reading named the bundle id it reads
    /// the list for (an owner's press while the first reading was still out).
    pub(crate) const fn wants_subject(&self) -> bool {
        self.subject.is_none()
    }

    /// A later reading named the bundle id: the panel's list reads start, and the
    /// icon to drag appears when the running copy is one aterm offers (`bundle`,
    /// [`drag_bundle`] for it).
    pub(crate) fn learn_subject(
        &mut self,
        subject: &'static str,
        bundle: Option<PathBuf>,
    ) -> Effect {
        if self.subject.is_some() {
            return Effect::default();
        }
        self.subject = Some(subject);
        if self.bundle.is_none() {
            self.bundle = bundle;
        }
        Effect {
            redraw: true,
            ..Effect::default()
        }
    }

    /// aterm reads on (the joined verdict said so): the done words.
    pub(crate) fn granted(&mut self, now: Instant) -> Effect {
        if self.phase == Phase::Done {
            return Effect::default();
        }
        self.phase = Phase::Done;
        self.done_at = Some(now);
        Effect {
            redraw: true,
            granted: true,
            ..Effect::default()
        }
    }

    /// `spctl developer-mode enable-terminal` ended (the panel's Show it).
    pub(crate) fn on_listed(&mut self, listed: &Result<(), String>) -> Effect {
        match listed {
            Ok(()) => self.listed_terminal = true,
            Err(why) => self.show_list_failed = Some(why.clone()),
        }
        Effect {
            redraw: true,
            ..Effect::default()
        }
    }

    /// A turn of the lane at `now`: the done words' time and the ten-minute
    /// bound. (A drop is judged by the read that follows it, [`Self::on_read`]:
    /// while System Settings is not in front nothing is read, and nothing is
    /// judged — the owner may be typing their password into macOS's prompt.)
    pub(crate) fn turn(&mut self, now: Instant) -> Effect {
        if self.done_at.is_some_and(|at| now >= at + DONE_FOR) {
            return Effect {
                close: Some(Closed::Done),
                ..Effect::default()
            };
        }
        if now >= self.shown_at + PANEL_FOR {
            return Effect {
                close: Some(Closed::Lapsed),
                ..Effect::default()
            };
        }
        Effect::default()
    }

    /// Whether a read is asked for ahead of the lane's cadence: the drop just
    /// ended, which is when the list may have moved.
    const fn read_asked(&self) -> bool {
        self.last_read.is_none()
    }

    /// Whether the lane should read the list now: only a readable list, one read at a
    /// time, at most one per [`PANEL_TICK`] (sooner after a drop), never once done.
    pub(crate) fn wants_read(&self, now: Instant) -> bool {
        self.unread.is_none()
            && !self.read_out
            && self.phase != Phase::Done
            && self
                .last_read
                .is_none_or(|at| now.saturating_duration_since(at) >= PANEL_TICK)
    }

    /// The soonest instant [`Self::turn`] changes something.
    pub(crate) fn next_turn(&self) -> Instant {
        let mut soonest = self.shown_at + PANEL_FOR;
        if let Some(at) = self.done_at {
            soonest = soonest.min(at + DONE_FOR);
        }
        soonest
    }
}

// ---------------------------------------------------------------------------
// LAYOUT AND PLACEMENT
// ---------------------------------------------------------------------------
//
// Off macOS, outside the tests, the AppKit panel that calls the pure functions
// below is not compiled, so each of them expects `dead_code` there. What only
// they use expects nothing: rustc counts what an expected-dead item uses as
// used, and rustc up to 1.96 (the cross cells' stable) calls an expectation on
// it unfulfilled (`verify cells`, linux and win); no rustc warns without one.

/// The panel's width, in points.
pub(crate) const PANEL_W: f64 = 360.0;

/// The panel's margin.
const INSET: f64 = 16.0;

/// The space between blocks.
const SPACING: f64 = 8.0;

/// The tile's icon, square.
pub(crate) const ICON: f64 = 64.0;

/// A push button's height.
const BUTTON_H: f64 = 28.0;

/// A rectangle. In [`layout`] the origin is the panel's top-left, y down (the live
/// panel's content view is flipped); in [`place`] it is Cocoa's screen space, origin at
/// the primary screen's bottom-left, y up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    /// Left edge.
    pub(crate) x: f64,
    /// Bottom edge in screen space; top edge in [`layout`].
    pub(crate) y: f64,
    /// Width.
    pub(crate) w: f64,
    /// Height.
    pub(crate) h: f64,
}

impl Rect {
    /// The right edge.
    pub(crate) fn max_x(self) -> f64 {
        self.x + self.w
    }

    /// The far edge along y (the top in screen space, the bottom in [`layout`]).
    pub(crate) fn max_y(self) -> f64 {
        self.y + self.h
    }

    /// Whether `inner` lies wholly inside `self`.
    pub(crate) fn holds(self, inner: Self) -> bool {
        inner.x >= self.x
            && inner.max_x() <= self.max_x()
            && inner.y >= self.y
            && inner.max_y() <= self.max_y()
    }

    /// The area `self` and `other` share.
    pub(crate) fn overlap(self, other: Self) -> f64 {
        let w = (self.max_x().min(other.max_x()) - self.x.max(other.x)).max(0.0);
        let h = (self.max_y().min(other.max_y()) - self.y.max(other.y)).max(0.0);
        w * h
    }
}

/// How the live panel measures its words: AppKit's text system there, a fixed-width
/// estimate in the tests.
pub(crate) trait Measure {
    /// The height `text` takes wrapped at `width`, set in `tone`.
    fn text_height(&self, text: &str, tone: Tone, width: f64) -> f64;
    /// The width of a push button titled `title`.
    fn button_width(&self, title: &str) -> f64;
}

/// One placed part of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// The text block at this index of [`Content::blocks`].
    Text(usize),
    /// The tile's icon.
    Icon,
    /// The tile's caption.
    Caption,
    /// The tile's hint ([`Tile::hint`]).
    Hint,
    /// The bundle's path under the hint.
    Path,
    /// A button.
    Button(Button),
    /// The status link.
    Link,
}

/// The panel's content size and every part's frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    /// The content view's size.
    pub(crate) w: f64,
    /// The content view's height.
    pub(crate) h: f64,
    /// Each part's frame, top to bottom.
    pub(crate) parts: Vec<(Part, Rect)>,
}

/// THE PANEL'S FRAMES, from its words: blocks stacked top to bottom at [`PANEL_W`],
/// text wrapped, the tile's icon at the left with its caption, hint and path beside it,
/// buttons right-aligned and wrapping onto a new row when they do not fit. Flipped
/// coordinates (y down). Pure: `measure` is the text system.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn layout(content: &Content, measure: &dyn Measure) -> Layout {
    let inner = PANEL_W - 2.0 * INSET;
    let mut parts = Vec::new();
    let mut y = INSET;
    for (index, block) in content.blocks.iter().enumerate() {
        if index > 0 {
            y += SPACING;
        }
        match block {
            Block::Text { text, tone } => {
                let h = measure.text_height(text, *tone, inner);
                parts.push((
                    Part::Text(index),
                    Rect {
                        x: INSET,
                        y,
                        w: inner,
                        h,
                    },
                ));
                y += h;
            }
            Block::Tile(tile) => {
                let x = INSET + ICON + SPACING;
                let w = inner - ICON - SPACING;
                parts.push((
                    Part::Icon,
                    Rect {
                        x: INSET,
                        y,
                        w: ICON,
                        h: ICON,
                    },
                ));
                let mut text_y = y;
                let path = tile.bundle.display().to_string();
                for (part, text, tone) in [
                    (Part::Caption, TILE_CAPTION, Tone::Title),
                    (Part::Hint, tile.hint(), Tone::Body),
                    (Part::Path, path.as_str(), Tone::Small),
                ] {
                    let h = measure.text_height(text, tone, w);
                    parts.push((part, Rect { x, y: text_y, w, h }));
                    text_y += h;
                }
                y = (y + ICON).max(text_y);
            }
            Block::Buttons(buttons) => {
                // Right to left from the panel's right margin, a new row when the next
                // one would cross the left margin.
                let mut right = INSET + inner;
                let mut row_y = y;
                for button in buttons.iter().rev() {
                    let w = measure.button_width(button.label()).min(inner);
                    if right - w < INSET {
                        right = INSET + inner;
                        row_y += BUTTON_H + SPACING;
                    }
                    parts.push((
                        Part::Button(*button),
                        Rect {
                            x: right - w,
                            y: row_y,
                            w,
                            h: BUTTON_H,
                        },
                    ));
                    right -= w + SPACING;
                }
                y = row_y + BUTTON_H;
            }
            Block::Link(text) => {
                let h = measure.text_height(text, Tone::Small, inner);
                parts.push((
                    Part::Link,
                    Rect {
                        x: INSET,
                        y,
                        w: inner,
                        h,
                    },
                ));
                y += h;
            }
        }
    }
    Layout {
        w: PANEL_W,
        h: y + INSET,
        parts,
    }
}

/// Whether the panel may come in front now: only while System Settings is the frontmost
/// app, and not when it is in front already or its tile's drag runs. Every reveal asks
/// at the moment it would order the panel in, not when it was scheduled: the delayed
/// retry that waits for Settings' window can fire after another app came forward. Pure.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) const fn may_reveal(settings_front: bool, visible: bool, dragging: bool) -> bool {
    settings_front && !visible && !dragging
}

/// What the lane's placement turn does with a panel the owner has not moved and whose
/// drag is not running ([`live`]'s `place_beside_settings`), from whether it is in
/// front, whether System Settings is the active app, whether the panel has stood beside
/// its window this show (`beside`), and that window as read now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PlaceTurn {
    /// Leave it.
    Nothing,
    /// Order it out: System Settings is still the active app but the window it stood
    /// beside left the screen (minimized), and a panel left up would jump to a screen
    /// corner and float over other apps' work.
    Hide,
    /// Place it beside the window (`None`: the window was never found this show, the
    /// screen's corner, O27).
    Place(Option<Rect>),
    /// Bring it back beside the window, which is on screen again (un-minimizing posts
    /// no activation, so only this turn sees it).
    Return(Rect),
}

/// [`PlaceTurn`] for one turn. Pure.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) const fn place_turn(
    visible: bool,
    settings_front: bool,
    beside: bool,
    window: Option<Rect>,
) -> PlaceTurn {
    match (visible, window) {
        (false, Some(window)) if settings_front && beside => PlaceTurn::Return(window),
        (false, _) => PlaceTurn::Nothing,
        (true, None) if beside => PlaceTurn::Hide,
        (true, window) => PlaceTurn::Place(window),
    }
}

/// Whether a drop something took, let go at (`x`, `y`) in Cocoa screen space, landed on
/// System Settings' window `settings` as the placement reads it — so on the list, as far
/// as aterm can tell. `true` when the window was not found: a failed read never takes a
/// drop away from the list. The point AppKit reports may be the drag image's lower-left
/// corner rather than the cursor (unmeasured, D15), so a point up to one image
/// (`image_w` by `image_h`) left of or below the window still counts as on it. Pure.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn drop_landed(
    settings: Option<Rect>,
    x: f64,
    y: f64,
    image_w: f64,
    image_h: f64,
) -> bool {
    settings.is_none_or(|r| {
        x >= r.x - image_w && x <= r.max_x() && y >= r.y - image_h && y <= r.max_y()
    })
}

/// The space between System Settings' window and the panel.
const PLACE_GAP: f64 = 12.0;

/// System Settings' sidebar width. NOT READ (aterm reads only the window's bounds, never
/// its contents): System Settings' own default. The panel overlaps at most this strip.
pub(crate) const SETTINGS_SIDEBAR_W: f64 = 220.0;

/// Convert a Quartz window-list rectangle (origin top-left of the primary screen, y
/// down) to Cocoa's screen space (origin bottom-left, y up). `primary_h`: the primary
/// screen's height. Pure.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn cg_to_cocoa(cg: Rect, primary_h: f64) -> Rect {
    Rect {
        y: primary_h - cg.y - cg.h,
        ..cg
    }
}

/// WHERE THE PANEL GOES, in Cocoa screen space. `screens` are the screens' visible
/// frames, the main (menu-bar) screen first; `settings` is System Settings' window, when
/// it was found. Beside it on its screen: to the right, else to the left (tops aligned),
/// else above, else below (left edges aligned). When nothing fits, over the sidebar strip
/// only, its bottom at Settings' bottom: never over the right pane, which holds the list,
/// its switches and its + button (as far as the screen allows: a Settings window flush
/// against a screen's left edge leaves no room left of it). No window: the main screen's
/// upper-right corner. Pure.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn place(panel_w: f64, panel_h: f64, settings: Option<Rect>, screens: &[Rect]) -> Rect {
    let Some(&main) = screens.first() else {
        return Rect {
            x: 0.0,
            y: 0.0,
            w: panel_w,
            h: panel_h,
        };
    };
    let corner = Rect {
        x: main.max_x() - PLACE_GAP - panel_w,
        y: main.max_y() - PLACE_GAP - panel_h,
        w: panel_w,
        h: panel_h,
    };
    let Some(settings) = settings else {
        return corner;
    };
    let screen = screens
        .iter()
        .copied()
        .filter(|screen| screen.overlap(settings) > 0.0)
        .max_by(|a, b| a.overlap(settings).total_cmp(&b.overlap(settings)))
        .unwrap_or(main);
    let clamp_x = |x: f64| x.min(screen.max_x() - panel_w).max(screen.x);
    let clamp_y = |y: f64| y.min(screen.max_y() - panel_h).max(screen.y);
    let at = |x: f64, y: f64| Rect {
        x,
        y,
        w: panel_w,
        h: panel_h,
    };
    let top = clamp_y(settings.max_y() - panel_h);
    for candidate in [
        at(settings.max_x() + PLACE_GAP, top),
        at(settings.x - PLACE_GAP - panel_w, top),
        at(clamp_x(settings.x), settings.max_y() + PLACE_GAP),
        at(clamp_x(settings.x), settings.y - PLACE_GAP - panel_h),
    ] {
        if screen.holds(candidate) {
            return candidate;
        }
    }
    at(
        clamp_x(settings.x + SETTINGS_SIDEBAR_W - panel_w),
        clamp_y(settings.y + PLACE_GAP),
    )
}

/// What it means that the panel's frame is not where aterm left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drift {
    /// It is where aterm left it (to within half a point).
    Still,
    /// It moved on the same arrangement of screens: the owner moved it, and it
    /// stays where they put it.
    Owner,
    /// It moved and the screens changed with it: macOS moved it off a display
    /// that went away or was rearranged, so aterm places it again.
    Screens,
}

/// WHETHER THE OWNER MOVED THE PANEL: `placed` is where aterm left it, on
/// screens whose frames were `placed_on`; `frame` and `screens` are now. Pure.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn drift(placed: Rect, placed_on: &[Rect], frame: Rect, screens: &[Rect]) -> Drift {
    let near = |a: f64, b: f64| (a - b).abs() < 0.5;
    if near(placed.x, frame.x)
        && near(placed.y, frame.y)
        && near(placed.w, frame.w)
        && near(placed.h, frame.h)
    {
        Drift::Still
    } else if placed_on == screens {
        Drift::Owner
    } else {
        Drift::Screens
    }
}

// ---------------------------------------------------------------------------
// WHAT TO DRAG
// ---------------------------------------------------------------------------

/// The bundle the tile offers to drag: the running aterm's own bundle, when it is the
/// INSTALLED copy (not a translocated or mounted image), it exists, and its bundle id is
/// the one the list is read for. Anything else is `None`: the panel shows no tile, only
/// words. Reads the bundle's `Info.plist`, never under a protected root.
#[must_use]
pub(crate) fn drag_bundle(subject: Option<&str>) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use aterm_containment::consent;
        let exe = consent::fence_exe()?;
        let root = consent::app_bundle_root(&exe)?;
        let installed = aterm_update::which_copy::posture_from(&exe)
            == aterm_update::which_copy::InstallPosture::Installed;
        let id = (installed && root.is_dir())
            .then(|| consent::bundle_id_of(&root, &consent::protected_roots(&[])))
            .flatten();
        accepted_bundle(root, installed, id.as_deref(), subject)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = subject;
        None
    }
}

/// The pure half of [`drag_bundle`]: `root` only when it is installed and its bundle id
/// is the subject's.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn accepted_bundle(
    root: PathBuf,
    installed: bool,
    root_id: Option<&str>,
    subject: Option<&str>,
) -> Option<PathBuf> {
    (installed && root_id.is_some() && root_id == subject).then_some(root)
}

// ---------------------------------------------------------------------------
// THE ARMS
// ---------------------------------------------------------------------------

/// What the hand-over does to the Mac and to its markers, injected like the consent
/// probes (`control_privacy::ConsentProbes`) and atpkg's own `Reach`: the window's
/// instance gets [`Self::live`]; a headless instance and every unit test [`Self::inert`],
/// which opens, runs, reads and writes nothing. None of it flips the switch.
#[derive(Clone, Copy)]
pub(crate) struct Reach {
    /// Open System Settings at the page (`menu::open_privacy_settings`: main thread,
    /// NSWorkspace, the extension's own id first). `true` when it opened.
    pub(crate) open_pane: fn() -> bool,
    /// `spctl developer-mode enable-terminal` (`atpkg::devtools::list_terminal`), on a
    /// worker.
    pub(crate) list_terminal: fn() -> Result<(), String>,
    /// The switch's row in the system TCC.db (`consent::probe_devtool_list`), on a
    /// worker.
    pub(crate) read_list: fn(ProbeGate, &str) -> DevToolList,
    /// The shared opened marker (`atpkg::devtools::last_opened`).
    pub(crate) last_opened: fn() -> Option<(String, u64)>,
    /// How many seconds ago aterm opened the page, when under the window
    /// (`atpkg::devtools::opened_within`).
    pub(crate) opened_within: fn(u64) -> Option<u64>,
    /// Note the page opened for this version (`atpkg::devtools::note_opened`).
    pub(crate) note_opened: fn(&str),
    /// The owner's Not now, while it holds: when they said it and until when, in unix
    /// seconds (`atpkg::devtools::declined_until`).
    pub(crate) declined: fn() -> Option<(u64, u64)>,
    /// Note the owner's Not now for this version (`atpkg::devtools::note_declined`).
    pub(crate) note_declined: fn(&str),
    /// This build's grant survives the next build (`DrClass::grant_stable`, from the
    /// warm signing identity; a cold one is unstable).
    pub(crate) grant_stable: fn() -> bool,
    /// The bundle to drag ([`drag_bundle`]).
    pub(crate) drag_bundle: fn(Option<&str>) -> Option<PathBuf>,
    live: bool,
}

impl Reach {
    /// The windowed instance's arms.
    pub(crate) const fn live() -> Self {
        Self {
            open_pane: live_open_pane,
            list_terminal: atpkg::devtools::list_terminal,
            read_list: aterm_containment::consent::probe_devtool_list,
            last_opened: atpkg::devtools::last_opened,
            opened_within: atpkg::devtools::opened_within,
            note_opened: atpkg::devtools::note_opened,
            declined: live_declined,
            note_declined: atpkg::devtools::note_declined,
            grant_stable: live_grant_stable,
            drag_bundle,
            live: true,
        }
    }

    /// The headless / unit-test arms: nothing opens, runs, is read or written.
    pub(crate) const fn inert() -> Self {
        Self {
            open_pane: inert_open_pane,
            list_terminal: inert_list_terminal,
            read_list: inert_read_list,
            last_opened: inert_last_opened,
            opened_within: inert_opened_within,
            note_opened: inert_note,
            declined: inert_not_declined,
            note_declined: inert_note,
            grant_stable: inert_no,
            drag_bundle: inert_drag_bundle,
            live: false,
        }
    }
}

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reach").field("live", &self.live).finish()
    }
}

fn live_open_pane() -> bool {
    crate::menu::open_privacy_settings(crate::menu::PrivacyPane::DeveloperTools).opened()
}

fn live_declined() -> Option<(u64, u64)> {
    atpkg::devtools::declined_until().map(|declined| (declined.at, declined.until))
}

fn live_grant_stable() -> bool {
    crate::control_privacy::running_dr_class_if_warm().grant_stable()
}

fn inert_open_pane() -> bool {
    false
}

fn inert_list_terminal() -> Result<(), String> {
    Err("not run on this instance".to_string())
}

fn inert_read_list(_gate: ProbeGate, _subject: &str) -> DevToolList {
    DevToolList::refused(DevToolListLabel::RefusedDisabled)
}

fn inert_last_opened() -> Option<(String, u64)> {
    None
}

fn inert_opened_within(_window_s: u64) -> Option<u64> {
    None
}

fn inert_note(_version: &str) {}

fn inert_not_declined() -> Option<(u64, u64)> {
    None
}

fn inert_no() -> bool {
    false
}

fn inert_drag_bundle(_subject: Option<&str>) -> Option<PathBuf> {
    None
}

/// THE PANEL ITSELF, injected (`ConsentGestures`-style): main thread only. The live arm
/// is AppKit's; the inert arm (a headless instance, every unit test) shows nothing.
#[derive(Clone, Copy)]
pub(crate) struct DevToolsHelperArm {
    /// Show the panel with `content`, its presses posted to the events handle. `false`
    /// when nothing was shown.
    pub(crate) show: fn(&Content, PanelEvents) -> bool,
    /// Redraw it with new words.
    pub(crate) update: fn(&Content),
    /// Put it beside System Settings' window ([`place`]), unless the owner moved it.
    pub(crate) place: fn(),
    /// Close it.
    pub(crate) close: fn(),
    /// Whether System Settings is running.
    pub(crate) settings_running: fn() -> bool,
    /// Whether System Settings is the frontmost app (the lane reads the list only then).
    pub(crate) settings_frontmost: fn() -> bool,
    live: bool,
}

impl DevToolsHelperArm {
    /// The windowed instance's arm: AppKit's panel (`live`) on macOS. Off macOS
    /// there is no System Settings to stand beside, and it shows nothing.
    pub(crate) const fn live() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                show: live::show,
                update: live::update,
                place: live::place_beside_settings,
                close: live::close,
                settings_running: live::settings_running,
                settings_frontmost: live::settings_frontmost,
                live: true,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                live: true,
                ..Self::inert()
            }
        }
    }

    /// The headless / unit-test arm: nothing is shown.
    pub(crate) const fn inert() -> Self {
        Self {
            show: inert_show,
            update: inert_update,
            place: inert_unit,
            close: inert_unit,
            settings_running: inert_no,
            settings_frontmost: inert_no,
            live: false,
        }
    }
}

impl std::fmt::Debug for DevToolsHelperArm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevToolsHelperArm")
            .field("live", &self.live)
            .finish()
    }
}

fn inert_show(_content: &Content, _events: PanelEvents) -> bool {
    false
}

fn inert_update(_content: &Content) {}

fn inert_unit() {}

/// What the panel's workers and presses publish, read by the park.
#[derive(Debug, Default)]
struct Shared {
    /// `spctl developer-mode enable-terminal` ended.
    listed: Option<Result<(), String>>,
    /// The panel whose list worker may publish into the slot. Admission and
    /// publication share this mutex so a replaced worker cannot erase its answer.
    generation: u64,
    /// A one-shot list read ended, for the panel of this generation.
    list: Option<(u64, DevToolList)>,
    /// The owner's presses, for the panel of each generation.
    events: Vec<(u64, PanelEvent)>,
}

/// The nudge a worker or a press sends (`Wake::DevToolsHandOver`).
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// Where the live panel posts the owner's presses: the shared slot the park reads, and
/// the nudge. Bound to one panel: a press on a panel another show replaced is dropped.
#[derive(Clone)]
pub(crate) struct PanelEvents {
    panel: u64,
    shared: Arc<Mutex<Shared>>,
    wake: Wake,
}

#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
impl PanelEvents {
    /// A handle bound to no lane: what it posts lands in a slot nobody reads. For
    /// the panel's driver (`devtools_helper_drive`), which builds the panel's views
    /// in no window and presses nothing.
    #[cfg(target_os = "macos")]
    pub(crate) fn unbound() -> Self {
        Self {
            panel: 0,
            shared: Arc::new(Mutex::new(Shared::default())),
            wake: Arc::new(|| {}),
        }
    }

    /// Post one of the owner's presses.
    pub(crate) fn post(&self, event: PanelEvent) {
        self.shared
            .lock_or_recover()
            .events
            .push((self.panel, event));
        (self.wake)();
    }
}

impl std::fmt::Debug for PanelEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelEvents")
            .field("panel", &self.panel)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// THE LANE
// ---------------------------------------------------------------------------

/// What a `spctl developer-mode enable-terminal` run is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listing {
    /// A hand-over on a list read whole and empty: open the page and show the panel
    /// when it ends.
    HandOver(Origin),
    /// The panel's Show it: bring the page up again when it ends.
    ShowList,
}

impl Listing {
    /// Whose end a run's end is taken as, when a later ask rides a run already out: the
    /// UNREAD panel's Show it least (a hand-over's end brings the page up and shows its
    /// own panel anyway), the window's own or a control verb's hand-over next, and an
    /// owner's press most (the panel it shows is the one the owner asked for). Pure.
    const fn rank(self) -> u8 {
        match self {
            Self::ShowList => 0,
            Self::HandOver(origin) if origin.owner_press() => 2,
            Self::HandOver(_) => 1,
        }
    }
}

/// Who waits on a `spctl` run besides its [`Listing`]: the owner's presses that were
/// answered [`HandOver::Listing`] — told the page is on its way — and must hear how it
/// came out, whichever ask's run they rode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Waiting {
    /// The digest line's press: the message center filed the line answered for it (its
    /// one action) before the page was known to open, so a page that then does not
    /// open brings the line back (`App::settle_devtools_waiting`).
    pub(crate) digest: bool,
    /// The Settings card's press, from these views: each bar says the page is on its
    /// way until the run's end says what happened.
    pub(crate) settings: Vec<(crate::WindowId, crate::tab_model::ViewId)>,
}

/// A hand-over held for its quiet moment.
#[derive(Clone, Debug)]
struct Queued {
    origin: Origin,
    at: Instant,
    next_check: Instant,
    /// What the `spctl developer-mode enable-terminal` run this hand-over already
    /// made answered, when its quiet moment passed while the run was out
    /// ([`Lane::queue_after_listing`]): an Ok run is not made again, and the panel
    /// still says Terminal is aterm's doing.
    listed: Option<Result<(), String>>,
}

/// A hand-over [`Lane::take_queued`] gives back at its quiet moment: who asked, and
/// what the `spctl` run it already made answered, if one did.
pub(crate) type TakenHandOver = (Origin, Option<Result<(), String>>);

/// What a panel turn asks of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Asks {
    /// A reading saw aterm on: end the digest line, refresh Settings.
    pub(crate) granted: bool,
    /// The owner says it is on: drop the cached reading, read on their return.
    pub(crate) recheck: bool,
    /// Run `spctl developer-mode enable-terminal` for the panel's Show it.
    pub(crate) list_terminal: bool,
    /// Open aterm's Settings ▸ Security.
    pub(crate) open_status: bool,
}

/// THE `App`-OWNED HAND-OVER LANE: the hand-over waiting for its quiet moment, the
/// `spctl` worker, the panel and its reads, and the latest reading. Inert on a headless
/// instance (`windowed` false): nothing is queued, run, read or shown.
#[derive(Debug)]
pub(crate) struct Lane {
    windowed: bool,
    born: Instant,
    reach: Reach,
    arm: DevToolsHelperArm,
    reading: Option<Reading>,
    queued: Option<Queued>,
    listing: Option<Listing>,
    /// The owner's presses waiting on the `spctl` run that is out.
    waiting: Waiting,
    panel: Option<Panel>,
    /// The panel shown last, for the slot's generations.
    generation: u64,
    next_tick: Option<Instant>,
    shared: Arc<Mutex<Shared>>,
    /// The shared opened marker's stamp (unix seconds) as last read, for the
    /// Settings card: `None` until read, then kept — updated when this lane
    /// writes the marker and read again after a focus gain
    /// ([`Self::forget_opened`]), so the per-park projection reads no file.
    opened: Option<Option<u64>>,
    /// The owner's Not now as last read (when said, until when; unix seconds), for
    /// the Settings card, cached as [`Self::opened`] is: read again after a focus
    /// gain and after this lane writes a decline.
    declined: Option<Option<(u64, u64)>>,
    /// When the window last answered a control verb's `now` (its panel shown or
    /// not). The asker opens the page, and only an opening moves the shared marker, so the
    /// window holds `now` to its minute itself: a program that asks and never
    /// opens cannot raise the panel again and again.
    now_answered: Option<Instant>,
}

impl Lane {
    /// The lane for an instance: live arms for a window on macOS, inert otherwise.
    pub(crate) fn new(headless: bool) -> Self {
        let windowed = !headless && cfg!(target_os = "macos");
        if windowed {
            Self::with_arms(true, Reach::live(), DevToolsHelperArm::live())
        } else {
            Self::with_arms(false, Reach::inert(), DevToolsHelperArm::inert())
        }
    }

    fn with_arms(windowed: bool, reach: Reach, arm: DevToolsHelperArm) -> Self {
        Self {
            windowed,
            born: Instant::now(),
            reach,
            arm,
            reading: None,
            queued: None,
            listing: None,
            waiting: Waiting::default(),
            panel: None,
            generation: 0,
            next_tick: None,
            shared: Arc::new(Mutex::new(Shared::default())),
            opened: None,
            declined: None,
            now_answered: None,
        }
    }

    /// A windowed lane with injected arms: the tests' stand-in for a window.
    #[cfg(test)]
    pub(crate) fn windowed_for_test(reach: Reach, arm: DevToolsHelperArm) -> Self {
        Self::with_arms(true, reach, arm)
    }

    /// Whether this lane may act at all (a window on macOS).
    pub(crate) const fn windowed(&self) -> bool {
        self.windowed
    }

    /// The hand-over's reach.
    pub(crate) const fn reach(&self) -> Reach {
        self.reach
    }

    /// How long ago this instance's lane was made (the App's construction).
    pub(crate) fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.born)
    }

    /// The panel, when one is up.
    pub(crate) const fn panel(&self) -> Option<&Panel> {
        self.panel.as_ref()
    }

    /// The origin of the hand-over waiting for its quiet moment.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> Option<Origin> {
        self.queued.as_ref().map(|q| q.origin)
    }

    /// The latest reading.
    pub(crate) const fn reading(&self) -> Option<Reading> {
        self.reading
    }

    /// When aterm last opened the page for the owner (unix seconds, the shared
    /// marker any aterm writes, `aterm doctor` included), read at most once
    /// between focus gains: the Settings card's "last opened" line.
    pub(crate) fn last_opened_at(&mut self) -> Option<u64> {
        let reach = self.reach;
        *self
            .opened
            .get_or_insert_with(|| (reach.last_opened)().map(|(_, at)| at))
    }

    /// This lane just wrote the marker at `at` (unix seconds; `None` when the
    /// clock could not be read, which reads the marker again).
    pub(crate) fn noted_opened(&mut self, at: Option<u64>) {
        self.opened = at.map(Some);
    }

    /// A focus gain: another process (`aterm doctor`) may have written the
    /// markers since, so the next [`Self::last_opened_at`] and
    /// [`Self::declined_marker`] read them again.
    pub(crate) fn forget_opened(&mut self) {
        self.opened = None;
        self.declined = None;
    }

    /// The owner's Not now while it holds (when said, until when; unix seconds),
    /// read at most once between focus gains and this lane's own declines: the
    /// Settings card's line saying why aterm stays quiet.
    pub(crate) fn declined_marker(&mut self) -> Option<(u64, u64)> {
        let reach = self.reach;
        *self.declined.get_or_insert_with(|| (reach.declined)())
    }

    /// A completed reading. aterm reading on ends a waiting hand-over and turns the
    /// panel to its done words; any other reading gives the panel its tccd answers,
    /// and — to a panel shown before any reading named the bundle id (an owner's
    /// press while the first reading was out) — the subject its list reads need and
    /// the icon to drag.
    pub(crate) fn note_reading(&mut self, reading: Reading, now: Instant) {
        self.reading = Some(reading);
        if reading.grant == DevToolGrant::Granted {
            self.queued = None;
        }
        let drag_bundle = self.reach.drag_bundle;
        if let Some(panel) = self.panel.as_mut() {
            let mut effect = panel.note_reading(&reading, now);
            if panel.wants_subject()
                && let Some(subject) = reading.subject
            {
                effect = effect.and(panel.learn_subject(subject, drag_bundle(Some(subject))));
            }
            self.redraw(effect);
        }
    }

    /// The panel's own list read is the lane's reading too, joined with the tccd
    /// answers the lane's reading holds by the window's own rule
    /// ([`Reading::with_list`]): a row on that has not changed since tccd's newer
    /// answer stays `not_granted` (so nothing ends the digest line on it), a row that
    /// changed after it is granted, and aterm granted ends a waiting hand-over. A list
    /// it could not read changes nothing.
    fn note_list(&mut self, list: DevToolList) {
        if list.label != DevToolListLabel::Read {
            return;
        }
        let reading = self.reading.unwrap_or(Reading::UNREAD).with_list(list);
        self.reading = Some(reading);
        if reading.grant == DevToolGrant::Granted {
            self.queued = None;
        }
    }

    /// Hold a hand-over for its quiet moment. The window's own outranks a control verb's
    /// (it never lapses); a second ask of the same kind starts its wait again.
    pub(crate) fn queue(&mut self, origin: Origin, now: Instant) {
        if self
            .queued
            .as_ref()
            .is_some_and(|q| q.origin == Origin::Automatic)
        {
            return;
        }
        self.queued = Some(Queued {
            origin,
            at: now,
            next_check: now,
            listed: None,
        });
    }

    /// The `spctl` run a hand-over the owner did not press made at its quiet moment
    /// ended at `now`, and the moment has passed (the owner is typing, or another app
    /// is in front): hold it for the next one, with what the run answered, so an Ok
    /// run is not made again and the panel still says Terminal is aterm's doing. The
    /// next look is [`RECHECK_EVERY`] away: the moment was judged just now. The
    /// window's own outranks a control verb's, as in [`Self::queue`], and keeps the
    /// run's answer.
    pub(crate) fn queue_after_listing(
        &mut self,
        origin: Origin,
        now: Instant,
        listed: Result<(), String>,
    ) {
        match self.queued.as_mut() {
            Some(queued) if queued.origin == Origin::Automatic && origin != Origin::Automatic => {
                queued.listed.get_or_insert(listed);
            }
            _ => {
                self.queued = Some(Queued {
                    origin,
                    at: now,
                    next_check: now + RECHECK_EVERY,
                    listed: Some(listed),
                });
            }
        }
    }

    /// The window answered a control verb's `now` at `now`, with its panel or
    /// without it.
    pub(crate) fn noted_now_answered(&mut self, now: Instant) {
        self.now_answered = Some(now);
    }

    /// Whether the window answered a `now` within
    /// [`atpkg::devtools::REOPEN_NOW_AFTER_S`] before `now`: the next one is
    /// `recent` whether or not its asker opened the page.
    pub(crate) fn now_answered_within_its_minute(&self, now: Instant) -> bool {
        self.now_answered.is_some_and(|at| {
            now.saturating_duration_since(at)
                < Duration::from_secs(atpkg::devtools::REOPEN_NOW_AFTER_S)
        })
    }

    /// The owner reached the page another way: nothing waits any longer.
    pub(crate) fn drop_queued(&mut self) {
        self.queued = None;
    }

    /// Whether the waiting hand-over's next look at the moment is due.
    pub(crate) fn hand_over_due(&self, now: Instant) -> bool {
        self.queued.as_ref().is_some_and(|q| now >= q.next_check)
    }

    /// The waiting hand-over, when `quiet` (the caller judged [`quiet`] at `now`),
    /// with what its earlier `spctl` run answered; otherwise it looks again
    /// [`RECHECK_EVERY`] later, or lapses.
    pub(crate) fn take_queued(&mut self, now: Instant, quiet: bool) -> Option<TakenHandOver> {
        let queued = self.queued.as_mut()?;
        if queued
            .origin
            .lapses_after()
            .is_some_and(|after| now.saturating_duration_since(queued.at) >= after)
        {
            aterm_log::info!(
                "devtools hand-over ({}): no quiet moment came in {} minutes; it lapsed",
                queued.origin.as_str(),
                ASKED_LAPSE.as_secs() / 60
            );
            self.queued = None;
            return None;
        }
        if quiet {
            return self.queued.take().map(|q| (q.origin, q.listed));
        }
        queued.next_check = now + RECHECK_EVERY;
        None
    }

    /// Start `spctl developer-mode enable-terminal` on a worker for `listing`. One run at
    /// a time: a later ask while one is out rides it (`true`), and when it outranks the
    /// run's ask ([`Listing::rank`]) the run's end is taken as ITS — an owner's press
    /// riding the window's own hand-over, or any hand-over riding the UNREAD panel's
    /// Show it, gets its page and its panel when the run ends. A digest press is noted
    /// as waiting either way ([`Waiting`]). `false` when the worker could not start.
    pub(crate) fn start_listing(&mut self, listing: Listing, wake: Wake) -> bool {
        if let Some(out) = self.listing {
            if listing.rank() > out.rank() {
                self.listing = Some(listing);
            }
            self.note_waiting(listing);
            return true;
        }
        let list_terminal = self.reach.list_terminal;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("aterm-devtools-terminal".to_owned())
            .spawn(move || {
                // Responsive: the worker publishes under the slot mutex the UI
                // thread's park reads, and nearly all of its life is a wait on
                // one short child.
                crate::qos::set_self(crate::qos::Role::Responsive);
                let listed = list_terminal();
                shared.lock_or_recover().listed = Some(listed);
                wake();
            });
        match spawned {
            Ok(_) => {
                self.listing = Some(listing);
                self.waiting = Waiting::default();
                self.note_waiting(listing);
                true
            }
            Err(e) => {
                aterm_log::warn!("devtools: the `spctl` worker could not start: {e}");
                false
            }
        }
    }

    /// A digest press waits on the run it started or rode.
    fn note_waiting(&mut self, listing: Listing) {
        if listing == Listing::HandOver(Origin::Digest) {
            self.waiting.digest = true;
        }
    }

    /// The Settings card's press from `views` was answered [`HandOver::Listing`]: their
    /// bars hear the run's end. Nothing when no run is out.
    pub(crate) fn wait_in_settings(
        &mut self,
        views: impl IntoIterator<Item = (crate::WindowId, crate::tab_model::ViewId)>,
    ) {
        if self.listing.is_some() {
            self.waiting.settings.extend(views);
        }
    }

    /// Whether a `spctl` run is out.
    #[cfg(test)]
    pub(crate) const fn listing(&self) -> Option<Listing> {
        self.listing
    }

    /// A `spctl` run that ended: whose end it is, what it answered, and the owner's
    /// presses that waited on it.
    pub(crate) fn take_listed(&mut self) -> Option<(Listing, Result<(), String>, Waiting)> {
        self.listing?;
        let listed = self.shared.lock_or_recover().listed.take()?;
        let waiting = std::mem::take(&mut self.waiting);
        self.listing
            .take()
            .map(|listing| (listing, listed, waiting))
    }

    /// The panel's Show it ended.
    pub(crate) fn on_listed(&mut self, listed: &Result<(), String>) {
        if let Some(panel) = self.panel.as_mut() {
            let effect = panel.on_listed(listed);
            self.redraw(effect);
        }
    }

    /// Show the panel (one at a time: an earlier one closes). `false` when the arm
    /// showed nothing.
    pub(crate) fn show(&mut self, show: Show, now: Instant, wake: Wake) -> bool {
        self.close(Closed::Replaced);
        self.generation = self.generation.wrapping_add(1);
        {
            let mut shared = self.shared.lock_or_recover();
            shared.generation = self.generation;
            shared.list = None;
        }
        let panel = Panel::new(show, now);
        let events = PanelEvents {
            panel: self.generation,
            shared: Arc::clone(&self.shared),
            wake,
        };
        if !(self.arm.show)(&panel.content(), events) {
            return false;
        }
        aterm_log::info!(
            "devtools: the helper panel is up ({}{})",
            panel.origin().as_str(),
            if panel.readable() { "" } else { ", unread" }
        );
        self.panel = Some(panel);
        self.next_tick = Some(now);
        true
    }

    /// Close the panel, if one is up, for `why`. The owner's Not now is noted
    /// (`atpkg::devtools::note_declined`): the window's own hand-over and the control
    /// verb leave them alone while it holds.
    fn close(&mut self, why: Closed) {
        let Some(panel) = self.panel.take() else {
            return;
        };
        (self.arm.close)();
        self.next_tick = None;
        if why.declines() {
            (self.reach.note_declined)(aterm_types::version::app_version());
            // The Settings card reads the decline again, once.
            self.declined = None;
        }
        aterm_log::info!(
            "devtools: the helper panel closed ({why:?}, {:?})",
            panel.phase()
        );
    }

    fn redraw(&mut self, effect: Effect) {
        if let Some(why) = effect.close {
            self.close(why);
        } else if effect.redraw
            && let Some(panel) = &self.panel
        {
            (self.arm.update)(&panel.content());
        }
    }

    /// THE PANEL'S TURN at the park: the owner's presses, a list read that ended, and —
    /// every [`PANEL_TICK`] — placement and System Settings still running; then the done
    /// words' time and the ten-minute bound; then, at that same cadence (or at once after
    /// a drop), one list read on a worker when the panel wants one, System Settings is
    /// frontmost and `read` names the gate (`None`: `[privacy] observer = false`, which
    /// reads nothing). Returns what the window must do.
    pub(crate) fn tick_panel(
        &mut self,
        now: Instant,
        read: Option<ProbeGate>,
        wake: &Wake,
    ) -> Asks {
        if self.panel.is_none() {
            return Asks::default();
        }
        let generation = self.generation;
        let (events, list) = {
            let mut shared = self.shared.lock_or_recover();
            let events = std::mem::take(&mut shared.events);
            let list = shared.list.take();
            (events, list)
        };
        let list = list.filter(|(g, _)| *g == generation).map(|(_, list)| list);
        if let Some(list) = list {
            self.note_list(list);
        }
        let mut effect = Effect::default();
        if let Some(panel) = self.panel.as_mut() {
            for (_, event) in events.into_iter().filter(|(g, _)| *g == generation) {
                effect = effect.and(panel.on(event, now));
            }
            if let Some(list) = list {
                effect = effect.and(panel.on_read(list, now));
            }
        }
        // The lane's own cadence: placement, System Settings still running and
        // in front, and a read at most once per PANEL_TICK — never per loop turn,
        // which is every keystroke and every redraw while the panel is up. A
        // drop that just ended asks for its read at once.
        let due = self.next_tick.is_some_and(|at| now >= at);
        let read_asked = self.panel.as_ref().is_some_and(Panel::read_asked);
        if effect.close.is_none() && due {
            self.next_tick = Some(now + PANEL_TICK);
            (self.arm.place)();
            let shown_for = self.panel.as_ref().map_or(Duration::ZERO, |p| {
                now.saturating_duration_since(p.shown_at)
            });
            if shown_for >= SETTINGS_GRACE && !(self.arm.settings_running)() {
                effect = effect.and(Effect {
                    close: Some(Closed::SettingsGone),
                    ..Effect::default()
                });
            }
        }
        if effect.close.is_none()
            && let Some(panel) = self.panel.as_mut()
        {
            effect = effect.and(panel.turn(now));
        }
        if effect.close.is_none()
            && (due || read_asked)
            && let Some(gate) = read
        {
            self.start_read(now, gate, wake);
        }
        let asks = Asks {
            granted: effect.granted,
            recheck: effect.recheck,
            list_terminal: effect.list_terminal,
            open_status: effect.open_status,
        };
        self.redraw(effect);
        asks
    }

    /// One list read on a worker, when the panel wants one and System Settings is in
    /// front: TCC.db's row only (`consent::probe_devtool_list`), never tccd's log.
    fn start_read(&mut self, now: Instant, gate: ProbeGate, wake: &Wake) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let Some(subject) = panel.subject else {
            return;
        };
        if !panel.wants_read(now) {
            return;
        }
        if !(self.arm.settings_frontmost)() {
            // Not in front (macOS's password prompt may be): a drop's read waits
            // for the cadence like any other, rather than asking every loop turn.
            panel.last_read.get_or_insert(now);
            return;
        }
        let read_list = self.reach.read_list;
        let shared = Arc::clone(&self.shared);
        let generation = self.generation;
        let wake = Arc::clone(wake);
        let spawned = std::thread::Builder::new()
            .name("aterm-devtools-list".to_owned())
            .spawn(move || {
                // Responsive, as the `spctl` worker: it publishes under the slot
                // mutex the park reads.
                crate::qos::set_self(crate::qos::Role::Responsive);
                let list = read_list(gate, subject);
                {
                    let mut shared = shared.lock_or_recover();
                    if shared.generation == generation {
                        shared.list = Some((generation, list));
                    }
                }
                wake();
            });
        match spawned {
            Ok(_) => panel.read_out = true,
            Err(e) => aterm_log::warn!("devtools: the list read could not start: {e}"),
        }
    }

    /// The park's deadline for this lane: the waiting hand-over's next look, only while
    /// `focused` says an aterm window is (a focus gain wakes the loop); the panel's next
    /// turn; and, while a worker is out, a re-read of its slot whatever its wake did.
    pub(crate) fn next_deadline(
        &self,
        now: Instant,
        focused: impl FnOnce() -> bool,
    ) -> Option<Instant> {
        if !self.windowed {
            return None;
        }
        let mut soonest: Option<Instant> = None;
        let mut fold = |at: Instant| soonest = Some(soonest.map_or(at, |s| s.min(at)));
        if let Some(panel) = &self.panel {
            fold(panel.next_turn());
            if let Some(at) = self.next_tick {
                fold(at);
            }
            if panel.read_out {
                fold(now + PANEL_TICK);
            }
        }
        if self.listing.is_some() {
            fold(now + PANEL_TICK);
        }
        if let Some(queued) = &self.queued
            && focused()
        {
            fold(queued.next_check);
        }
        soonest
    }
}

// ---------------------------------------------------------------------------
// THE LIVE PANEL (macOS)
// ---------------------------------------------------------------------------

/// THE PANEL ITSELF, on AppKit: the [`DevToolsHelperArm::live`] arm.
///
/// A small floating, NON-ACTIVATING utility panel titled "aterm" (style mask set
/// at init: a non-activating mask set later is not one AppKit honours), at the
/// floating level, on every Space and beside a full-screen System Settings, that
/// stays up while aterm is not the active app (`hidesOnDeactivate` NO) or hidden
/// (`canHide` NO: hiding aterm, or Hide Others in System Settings, leaves System
/// Settings its helper), and becomes key only for a view that needs it (none
/// does). Not movable by its background: the title bar moves it, so a press on
/// the tile starts a drag instead.
///
/// * **Visible only while System Settings is the frontmost app.** An NSWorkspace
///   activation observer orders it out whenever another app comes forward and
///   back in when System Settings returns — so it never floats over another app's
///   work, and it is out of the way while macOS's own password prompt is in front.
///   On a show it waits up to [`REVEAL_WAIT`] for System Settings' window before
///   placing itself, rather than appearing at a screen corner and jumping.
/// * **Placed beside System Settings' window ([`place`]), never over its right
///   pane.** The window's frame comes from the window list, reading ONLY each
///   entry's owner pid, layer, alpha and bounds — never a name or a title, never
///   logged or kept — for the largest layer-0 window of System Settings' process.
///   This is a placement read, not the scan TCC design :670 forbids (that one
///   infers a system dialog from what is on screen; this one infers nothing). It
///   re-places itself each lane turn until the owner moves it — a move read
///   before anything re-places it or takes its frame as the new baseline, so one
///   made just before a drag, a redraw or a switch of apps stays, while a frame
///   that changed with the screens (a display unplugged) is macOS's, and the
///   panel goes beside System Settings again.
/// * **The tile is aterm's icon, the owner's to drag.** The drag starts only past
///   [`DRAG_SLOP`] (a click is not a drag), offers Copy and nothing else in every
///   context, ignores the modifier keys (a held ⌘ must not turn it into a Move of
///   the running `/Applications/aterm.app`), and carries a Finder-like payload
///   built from `NSURL` — never a hand-formatted string (`aterm (dev).app` has a
///   space and parentheses). While it runs the panel dims, and lets the mouse
///   through, as a whole, only while it lies wholly on System Settings' window
///   (the owner moved it over the list): otherwise a release over it puts the
///   icon back, never dropping it on whatever window is beneath (an aterm
///   terminal would type its path; aterm's own fallback over the sidebar strip
///   hangs the tile outside System Settings' window). Its
///   end says whether the destination took it. One accepted drop per panel. An
///   open hand shows over the tile while it can be dragged (a tracking area
///   active always: cursor rects work only in the key window, which this panel
///   never is). aterm never drops anything itself.
/// * **Its presses are posted, not acted on**: Not now, the close box, "I've
///   switched it on", Show it and the status link go to the lane through
///   [`PanelEvents`]; the lane decides.
///
/// Main thread only; every entry asks [`MainThread::new`] and does nothing off
/// it. A headless process never gets here (its lane holds the inert arm), and
/// the one send that puts the panel in front goes through
/// `menu::order_front_regardless`, which asks `os_ui_refused` first. The panel
/// has no semantic node: `key`, `pointer` and `act` cannot press it.
#[cfg(target_os = "macos")]
pub(crate) mod live {
    use std::borrow::Cow;
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use aterm_objc::{Bool, CGPoint, CGRect, CGSize, Id, Obj, Retained, Sel, class, sel};

    use super::{
        Block, Button, Content, Drift, ICON, Measure, PANEL_W, PanelEvent, PanelEvents, Part,
        PlaceTurn, Rect, TILE_CAPTION, Tone, cg_to_cocoa, drift, drop_landed, layout, may_reveal,
        place, place_turn,
    };
    use crate::appkit::{self, MainThread, consts};

    /// System Settings' bundle id.
    const SETTINGS_ID: &str = "com.apple.systempreferences";

    /// What `menu::order_front_regardless` and `os_ui_refused` say they were
    /// asked to show.
    const WHAT: &str = "the Developer Tools helper panel";

    /// The panel's title.
    const TITLE: &str = "aterm";

    /// How long a reveal waits for System Settings' window (it may still be
    /// launching) before the panel takes the screen's corner.
    pub(crate) const REVEAL_WAIT: Duration = Duration::from_secs(3);

    /// How often a waiting reveal looks again, in seconds.
    const REVEAL_RETRY_SECS: f64 = 0.25;

    /// How far, in points, the pointer moves from its press before a drag starts.
    pub(crate) const DRAG_SLOP: f64 = 4.0;

    /// The panel's opacity while a drag lets the mouse through it.
    const DRAG_ALPHA: f64 = 0.72;

    /// The tile's opacity once its one drop is spent.
    const SPENT_ALPHA: f64 = 0.4;

    /// The smallest window counted as System Settings' own (its sheets and
    /// popovers are smaller).
    const SETTINGS_MIN: CGSize = CGSize {
        width: 320.0,
        height: 240.0,
    };

    /// The width a push button adds to its title's cell.
    const BUTTON_PAD: f64 = 8.0;

    /// The drag's pasteboard types, the Finder-like set System Settings accepts
    /// most readily: the file URL as both URL types, and the plain path.
    pub(crate) const TYPE_FILE_URL: &str = "public.file-url";
    /// See [`TYPE_FILE_URL`].
    pub(crate) const TYPE_URL: &str = "public.url";
    /// See [`TYPE_FILE_URL`].
    pub(crate) const TYPE_TEXT: &str = "public.utf8-plain-text";
    /// The old filenames list a Finder drag also answers. NOT WRITTEN: AppKit
    /// refuses it as a type a pasteboard item or writer offers (`is not a valid
    /// UTI string`) and derives it from [`TYPE_FILE_URL`] for whoever asks
    /// (both measured by the driver, which reads it back).
    pub(crate) const TYPE_FILENAMES: &str = "NSFilenamesPboardType";

    /// The window list's keys this panel reads, and no other: owner pid, layer,
    /// alpha, bounds. Spelled as their values (CoreGraphics' `CFSTR`s are their
    /// own names); the driver holds each to the SDK's exported symbol.
    pub(crate) const WINDOW_KEYS: [&str; 4] = [
        "kCGWindowOwnerPID",
        "kCGWindowLayer",
        "kCGWindowAlpha",
        "kCGWindowBounds",
    ];

    /// The activation notification's name and its user-info key for the app,
    /// spelled as their values (the driver holds each to AppKit's symbol).
    pub(crate) const WORKSPACE_NAMES: [&str; 2] = [
        "NSWorkspaceDidActivateApplicationNotification",
        "NSWorkspaceApplicationKey",
    ];

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        /// `CGWindow.h:170` — the on-screen windows' descriptions, a +1
        /// `CFArrayRef` of `CFDictionaryRef`s (toll-free `NSArray`/`NSDictionary`).
        fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> Id;
        /// `CGGeometry.h:276` — a bounds dictionary back into a `CGRect`.
        fn CGRectMakeWithDictionaryRepresentation(dict: Id, rect: *mut CGRect) -> bool;
    }

    thread_local! {
        /// The panel up, if any. The main thread's: every entry is main-thread
        /// only, and the objects it holds are AppKit's.
        static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
        /// A tile whose panel closed while its drag ran: kept until the drag ends,
        /// because the drag still sends its source the end.
        static LINGER: RefCell<Option<Retained<TileView>>> = const { RefCell::new(None) };
    }

    /// The panel up, and what it knows.
    struct Live {
        panel: Obj,
        target: Retained<HelperTarget>,
        content: Retained<ContentView>,
        tile: Option<Retained<TileView>>,
        events: PanelEvents,
        /// System Settings is the frontmost app (the activation observer's word).
        settings_front: bool,
        /// Ordered in.
        visible: bool,
        /// When the reveal that waits for System Settings' window began.
        reveal_since: Option<Instant>,
        /// The frame aterm last put the panel at, read back.
        placed: Option<CGRect>,
        /// The screens' arrangement (each screen's frame) when it did: a frame
        /// that changed with them is macOS moving the panel off a display, not
        /// the owner.
        placed_on: Vec<Rect>,
        /// The owner moved it: it stays where they put it.
        moved: bool,
        /// It has stood beside System Settings' window this show: a window that then
        /// leaves the screen (minimized) hides it rather than sending it to a corner.
        beside: bool,
        /// The tile's drag is running — from the moment its session is created, not
        /// only from `willBeginAtPoint:`, which AppKit sends later in the event loop.
        dragging: bool,
        /// Words that came while the drag ran, drawn when it ends.
        pending: Option<Content>,
    }

    /// `f` on the panel up, if there is one and nothing is using it already.
    fn with_live<R>(f: impl FnOnce(&mut Live) -> R) -> Option<R> {
        LIVE.with(|slot| {
            let mut slot = slot.try_borrow_mut().ok()?;
            slot.as_mut().map(f)
        })
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    fn to_cg(r: Rect) -> CGRect {
        rect(r.x, r.y, r.w, r.h)
    }

    fn from_cg(r: CGRect) -> Rect {
        Rect {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.width,
            h: r.size.height,
        }
    }

    /// Whether two frames are the same to within half a point.
    fn same_frame(a: CGRect, b: CGRect) -> bool {
        (a.origin.x - b.origin.x).abs() < 0.5
            && (a.origin.y - b.origin.y).abs() < 0.5
            && (a.size.width - b.size.width).abs() < 0.5
            && (a.size.height - b.size.height).abs() < 0.5
    }

    // ---- the declared classes ----

    /// The target's ivars.
    pub(crate) struct TargetIvars {
        events: PanelEvents,
    }

    aterm_objc::declare_class! {
        /// The panel's one target and delegate: the buttons' `press:` (each
        /// button's `tag` names it, [`Button::from_tag`]), the status link, the
        /// close box (`windowShouldClose:` — the owner's Not now; the lane closes
        /// the panel) and the workspace activation observer. A relay: what each
        /// press means is the lane's to decide.
        pub(crate) struct HelperTarget: NSObject {
            const NAME: &str = "ATermDevToolsHelperTarget";
            type Ivars = TargetIvars;
            protocols: [NSObject, NSWindowDelegate];

            /// A button: post the event its tag names.
            @sel(press:)
            fn press(&self, sender: Id) {
                // SAFETY: `sender` is the live NSButton AppKit passes its action;
                // `-tag` is `-(NSInteger)`.
                let tag = if sender.is_null() {
                    -1
                } else {
                    unsafe { appkit::send_isize(sender, sel!(tag)) }
                };
                if let Some(button) = Button::from_tag(tag) {
                    self.ivars().events.post(button.event());
                }
            }

            /// The status link: bring aterm forward (the owner asked to see it),
            /// then post it; the lane opens Settings ▸ Security.
            @sel(openStatus:)
            fn openStatus(&self, _sender: Id) {
                // SAFETY: main thread, inside AppKit's action dispatch.
                unsafe { activate_aterm() };
                self.ivars().events.post(PanelEvent::Status);
            }

            /// The close box is Not now: post it, and leave the closing to the
            /// lane (which notes the decline, then closes through the arm).
            @sel(windowShouldClose:)
            fn windowShouldClose(&self, _window: Id) -> Bool {
                self.ivars().events.post(PanelEvent::NotNow);
                Bool::NO
            }

            /// Another app came forward: show the panel only for System Settings.
            @sel(workspaceDidActivate:)
            fn workspaceDidActivate(&self, note: Id) {
                // SAFETY: `note` is the live NSNotification the workspace centre
                // delivers, on the main thread.
                let front = unsafe { activated_settings(note) };
                let _ = with_live(|live| {
                    live.settings_front = front;
                    if front {
                        reveal(live);
                    } else {
                        hide(live);
                    }
                });
            }

            /// A reveal waiting for System Settings' window looks again.
            @sel(revealSoon:)
            fn revealSoon(&self, _unused: Id) {
                let _ = with_live(reveal);
            }
        }
    }

    aterm_objc::declare_class! {
        /// The panel's content view: flipped (the layout is y-down), painted in
        /// the window background colour so a capture of it looks like the panel.
        pub(crate) struct ContentView: NSView {
            const NAME: &str = "ATermDevToolsHelperContent";
            type Ivars = ();
            protocols: [NSObject];

            /// Flipped. Not contained: the body is send-free.
            @sel(isFlipped) @abort_on_exception
            fn isFlipped(&self) -> Bool {
                Bool::YES
            }

            /// The panel's background.
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: inside AppKit's draw cycle on the main thread;
                // `+windowBackgroundColor` is +0, `-set` is `-(void)`, `-bounds`
                // `-(NSRect)` and `+fillRect:` `+(void)(NSRect)`.
                unsafe {
                    let bounds = appkit::send_rect(self.as_id(), sel!(bounds));
                    let color =
                        appkit::send_id(class(c"NSColor").as_id(), sel!(windowBackgroundColor));
                    if color.is_null() {
                        return;
                    }
                    appkit::send_v(color, sel!(set));
                    appkit::send_v_rect(class(c"NSBezierPath").as_id(), sel!(fillRect:), bounds);
                }
            }

            /// A click works while System Settings is the active app. Send-free.
            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> Bool {
                Bool::YES
            }
        }
    }

    aterm_objc::declare_class! {
        /// A push button (or the borderless status link) whose first click works
        /// while another app is active: a plain `NSButton` asks for a click to
        /// bring its window forward first, and this panel's window never comes
        /// forward.
        pub(crate) struct HelperButton: NSButton {
            const NAME: &str = "ATermDevToolsHelperButton";
            type Ivars = ();
            protocols: [NSObject];

            /// Send-free.
            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> Bool {
                Bool::YES
            }
        }
    }

    /// The tile's ivars.
    pub(crate) struct TileIvars {
        bundle: PathBuf,
        icon: Obj,
        /// Not yet spent: one accepted drop per panel.
        live: Cell<bool>,
        /// Where the press began, in the view's coordinates.
        down: Cell<Option<CGPoint>>,
        /// This press's drag has begun.
        began: Cell<bool>,
        events: PanelEvents,
    }

    aterm_objc::declare_class! {
        /// aterm's icon, the owner's to drag into the Developer Tools list. A
        /// drag source with a Finder-like payload: Copy only, modifier keys
        /// ignored, one accepted drop.
        pub(crate) struct TileView: NSView {
            const NAME: &str = "ATermDevToolsHelperTile";
            type Ivars = TileIvars;
            protocols: [NSObject, NSDraggingSource];

            /// The icon, filling the tile.
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: inside AppKit's draw cycle; the icon is the live NSImage
                // the ivars own; `-drawInRect:` is `-(void)(NSRect)`.
                unsafe {
                    let bounds = appkit::send_rect(self.as_id(), sel!(bounds));
                    appkit::send_v_rect(self.ivars().icon.id(), sel!(drawInRect:), bounds);
                }
            }

            /// The first click starts a drag while System Settings is active.
            /// Send-free.
            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> Bool {
                Bool::YES
            }

            /// A press: remember where. Nothing starts until the pointer moves.
            @sel(mouseDown:)
            fn mouseDown(&self, event: Id) {
                // SAFETY: `event` is the live NSEvent AppKit passes.
                let at = unsafe { point_in(self.as_id(), event) };
                self.ivars().down.set(Some(at));
                self.ivars().began.set(false);
            }

            /// Past [`DRAG_SLOP`] from the press, the drag begins — once per
            /// press, and never once the tile is spent.
            @sel(mouseDragged:)
            fn mouseDragged(&self, event: Id) {
                let ivars = self.ivars();
                let Some(down) = ivars.down.get() else {
                    return;
                };
                if ivars.began.get() || !ivars.live.get() {
                    return;
                }
                // SAFETY: as in `mouseDown:`.
                let at = unsafe { point_in(self.as_id(), event) };
                if (at.x - down.x).hypot(at.y - down.y) <= DRAG_SLOP {
                    return;
                }
                ivars.began.set(true);
                // SAFETY: inside AppKit's `mouseDragged:` dispatch on the main
                // thread, with the event that dispatch passed.
                if unsafe { begin_drag(self, event) } {
                    // The session exists from here, though AppKit sends its
                    // `willBeginAtPoint:` later in the event loop: the panel counts
                    // the drag as running now, so a close or new words in between
                    // keep this tile — the session's source — alive and in place.
                    let tile = self.as_id();
                    let _ = with_live(|live| {
                        if holds_tile(live, tile) {
                            note_owner_move(live);
                            live.dragging = true;
                        }
                    });
                } else {
                    ivars.began.set(false);
                }
            }

            /// The press ended.
            @sel(mouseUp:)
            fn mouseUp(&self, _event: Id) {
                self.ivars().down.set(None);
                self.ivars().began.set(false);
            }

            /// The pointer came over the tile: an open hand while it can still be
            /// dragged. From the tracking area `build` installs, active always:
            /// cursor rects work only in the key window, which this panel, beside
            /// the active System Settings, never is. Best effort: the cursor is
            /// the active app's to set again.
            @sel(mouseEntered:)
            fn mouseEntered(&self, _event: Id) {
                if self.ivars().live.get() {
                    set_cursor(true);
                }
            }

            /// The pointer left the tile: the arrow again.
            @sel(mouseExited:)
            fn mouseExited(&self, _event: Id) {
                set_cursor(false);
            }

            /// Copy, and nothing else, in every context: a Move or a Generic
            /// lets Finder take the running aterm out of its install path.
            @sel(draggingSession:sourceOperationMaskForDraggingContext:)
            fn draggingSessionSourceOperationMask(&self, _session: Id, _context: isize) -> usize {
                consts::NS_DRAG_OPERATION_COPY
            }

            /// A held modifier key does not change the operation.
            @sel(ignoreModifierKeysForDraggingSession:) @abort_on_exception
            fn ignoreModifierKeysForDraggingSession(&self, _session: Id) -> Bool {
                Bool::YES
            }

            /// The drag began: the panel dims, and steps aside for the list when
            /// it lies wholly on System Settings' window ([`drag_began`]).
            @sel(draggingSession:willBeginAtPoint:)
            fn draggingSessionWillBegin(&self, _session: Id, _at: CGPoint) {
                drag_began(self.as_id());
            }

            /// The drag ended, taken or not: the panel comes back, the tile is
            /// spent if the destination took it, and the lane hears which.
            @sel(draggingSession:endedAtPoint:operation:)
            fn draggingSessionEnded(&self, _session: Id, at: CGPoint, operation: usize) {
                // Held for the whole method: `drag_ended` draws the words that
                // waited, which can take this tile off the panel and drop the
                // panel's hold on it inside its own pool, while this method is
                // still running on it (a message send retains no receiver, and
                // the drag session need not hold its source). Autoreleased at
                // the end, into AppKit's pool, never released here.
                // SAFETY: `self` is the live tile AppKit sent this to.
                let keep = unsafe { Obj::retain(self.as_id()) };
                let accepted = operation != consts::NS_DRAG_OPERATION_NONE;
                // Taken outside System Settings' window (Finder copies the bundle
                // wherever it is let go): not the list, so the drop is not spent.
                // SAFETY: main thread; read-only window and screen queries.
                let outside = accepted && !unsafe { dropped_in_settings(self.as_id(), at) };
                let taken = accepted && !outside;
                let ivars = self.ivars();
                ivars.down.set(None);
                ivars.began.set(false);
                if taken {
                    ivars.live.set(false);
                    // SAFETY: main thread; `-setAlphaValue:` is `-(void)(CGFloat)` on
                    // the live tile.
                    unsafe {
                        appkit::send_v_f64(self.as_id(), sel!(setAlphaValue:), SPENT_ALPHA);
                    }
                }
                aterm_log::info!(
                    "devtools: the owner's drag of aterm ended (operation {operation}: {})",
                    if outside {
                        "taken outside System Settings"
                    } else if taken {
                        "taken"
                    } else {
                        "not taken"
                    }
                );
                drag_ended(self.as_id());
                ivars.events.post(if outside {
                    PanelEvent::DroppedOutside
                } else {
                    PanelEvent::DragEnded { accepted }
                });
                // A tile whose panel closed mid-drag goes with the drag's end:
                // autoreleased, so not while this method still runs on it.
                LINGER.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut()
                        && let Some(tile) = slot.take()
                    {
                        let _ = tile.autorelease();
                    }
                });
                if let Some(keep) = keep {
                    let _ = keep.autorelease();
                }
            }
        }
    }

    // ---- the arm ----

    /// Show the panel for `content`, its presses posted to `events`. One panel:
    /// an earlier one closes. It comes in front when System Settings is the
    /// frontmost app (now, or when it comes forward). `false` when nothing was
    /// built.
    pub(crate) fn show(content: &Content, events: PanelEvents) -> bool {
        if crate::menu::os_ui_refused(WHAT) {
            return false;
        }
        let Some(mtm) = MainThread::new() else {
            return false;
        };
        close();
        aterm_objc::autoreleasepool(|_| {
            let Some(built) = build(mtm, content, events.clone()) else {
                return false;
            };
            // SAFETY: main thread; the target is the live object the observer
            // registration names, kept by `Live` until `close` removes it.
            if !unsafe { observe_activation(built.target.as_id()) } {
                aterm_log::warn!(
                    "devtools: the helper panel cannot watch which app is in front; not shown"
                );
                return false;
            }
            // SAFETY: main thread; read-only workspace query.
            let settings_front = unsafe { frontmost_is_settings() };
            let Built {
                panel,
                target,
                content: view,
                tile,
            } = built;
            let live = Live {
                panel,
                target,
                content: view,
                tile,
                events,
                settings_front,
                visible: false,
                reveal_since: None,
                placed: None,
                placed_on: Vec::new(),
                moved: false,
                beside: false,
                dragging: false,
                pending: None,
            };
            let refused = LIVE.with(|slot| match slot.try_borrow_mut() {
                Ok(mut slot) => {
                    *slot = Some(live);
                    None
                }
                Err(_) => Some(live),
            });
            if let Some(live) = refused {
                // SAFETY: main thread; `live` still holds the target, released
                // only after the centre lets it go.
                unsafe { stop_observing(live.target.as_id()) };
                return false;
            }
            if settings_front {
                let _ = with_live(reveal);
            }
            true
        })
    }

    /// Draw new words: the panel resizes about its top edge. Words that come
    /// while the tile's drag runs wait for its end (the tile is the drag's
    /// source, and rebuilding the views under it would pull it out).
    pub(crate) fn update(content: &Content) {
        let Some(mtm) = MainThread::new() else {
            return;
        };
        let _ = with_live(|live| {
            if live.dragging {
                live.pending = Some(content.clone());
                return;
            }
            refill(mtm, live, content);
        });
    }

    /// The lane's turn: beside System Settings again (it may have moved),
    /// unless a drag runs or the owner moved the panel ([`place_turn`]). System
    /// Settings still the active app with the window the panel stood beside off
    /// the screen (minimized: no activation fires) orders the panel out rather
    /// than sending it to a screen corner over other apps' work, and the turn
    /// that finds the window back brings it back beside it. A panel out because
    /// another app is in front is left to the activation observer.
    pub(crate) fn place_beside_settings() {
        if MainThread::new().is_none() {
            return;
        }
        aterm_objc::autoreleasepool(|_| {
            let _ = with_live(|live| {
                if live.dragging || live.moved {
                    return;
                }
                if !live.visible && !(live.settings_front && live.beside) {
                    return;
                }
                note_owner_move(live);
                if live.moved {
                    return;
                }
                // SAFETY: main thread; the window and screen reads are read-only.
                let screens = unsafe { screens() };
                // SAFETY: as above.
                let window = unsafe { settings_window(&screens) };
                match place_turn(live.visible, live.settings_front, live.beside, window) {
                    PlaceTurn::Nothing => {}
                    PlaceTurn::Hide => hide(live),
                    // SAFETY: main thread.
                    PlaceTurn::Place(window) => unsafe { place_now(live, &screens, window) },
                    PlaceTurn::Return(window) => {
                        live.reveal_since = None;
                        // SAFETY: main thread; the target is the live one `Live`
                        // holds.
                        unsafe {
                            cancel_reveal_soon(live.target.as_id());
                            place_now(live, &screens, Some(window));
                        }
                        order_in(live);
                    }
                }
            });
        });
    }

    /// Close the panel (and stop watching which app is in front).
    pub(crate) fn close() {
        if MainThread::new().is_none() {
            return;
        }
        let Some(live) = LIVE.with(|slot| slot.try_borrow_mut().ok().and_then(|mut s| s.take()))
        else {
            return;
        };
        aterm_objc::autoreleasepool(|_| {
            let target = live.target.as_id();
            // SAFETY: main thread; the target is live until `live` drops below.
            // `-setDelegate:` and `-orderOut:` are `-(void)(id)` on the live panel,
            // nil meaning "none" and "no sender".
            unsafe {
                cancel_reveal_soon(target);
                stop_observing(target);
                appkit::send_v_id(live.panel.id(), sel!(setDelegate:), Id::NIL);
                appkit::send_v_id(live.panel.id(), sel!(orderOut:), Id::NIL);
            }
            if live.dragging
                && let Some(tile) = &live.tile
            {
                let keep = tile.clone_retained();
                LINGER.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut() {
                        *slot = Some(keep);
                    }
                });
            }
            drop(live);
        });
    }

    /// Whether System Settings is running (one of its processes that may show a
    /// window).
    pub(crate) fn settings_running() -> bool {
        // SAFETY: read-only NSRunningApplication queries.
        aterm_objc::autoreleasepool(|_| unsafe { settings_pid() }.is_some())
    }

    /// Whether System Settings is the frontmost app: the activation observer's
    /// word, kept on the panel — no AppKit query, however often it is asked.
    pub(crate) fn settings_frontmost() -> bool {
        LIVE.with(|slot| {
            slot.try_borrow()
                .ok()
                .and_then(|slot| slot.as_ref().map(|live| live.settings_front))
        })
        .unwrap_or(false)
    }

    // ---- visibility and placement ----

    /// Bring the panel in, beside System Settings' window — waiting up to
    /// [`REVEAL_WAIT`] for that window to be on screen first — only while System
    /// Settings is still the frontmost app ([`may_reveal`]).
    fn reveal(live: &mut Live) {
        if !may_reveal(live.settings_front, live.visible, live.dragging) {
            return;
        }
        let now = Instant::now();
        let since = *live.reveal_since.get_or_insert(now);
        // SAFETY: main thread; read-only window and screen queries.
        let screens = unsafe { screens() };
        // SAFETY: as above.
        let settings = unsafe { settings_window(&screens) };
        if settings.is_none() && live.beside {
            // The window it stood beside is off the screen (minimized) while System
            // Settings came forward: no corner — the lane's turn brings the panel
            // back beside the window when it returns.
            live.reveal_since = None;
            return;
        }
        if settings.is_none() && now.saturating_duration_since(since) < REVEAL_WAIT {
            // SAFETY: main thread; the target lives as long as the panel, and
            // `close` cancels what this schedules.
            unsafe { reveal_soon(live.target.as_id()) };
            return;
        }
        live.reveal_since = None;
        // SAFETY: as above.
        unsafe { place_now(live, &screens, settings) };
        order_in(live);
    }

    /// Put the panel in front, and remember the frame it took there: an
    /// adjustment AppKit makes on the way in is not the owner moving it. A move
    /// the owner made before it is read first ([`note_owner_move`]).
    fn order_in(live: &mut Live) {
        note_owner_move(live);
        let panel = live.panel.id();
        live.visible = crate::menu::order_front_regardless(panel, WHAT);
        if live.visible && live.placed.is_some() && !live.moved {
            // SAFETY: main thread (`Live` lives on it); read-only screen queries.
            unsafe { note_placed(live, &screens()) };
        }
    }

    /// Remember where aterm left the panel, and on which arrangement of screens.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn note_placed(live: &mut Live, screens: &Screens) {
        // SAFETY: main thread; `-frame` is `-(NSRect)` on the live panel.
        live.placed = Some(unsafe { appkit::send_rect(live.panel.id(), sel!(frame)) });
        live.placed_on.clone_from(&screens.frames);
    }

    /// Note in `moved` that the owner moved the panel since aterm last placed it:
    /// up, and its frame no longer where aterm left it on the same arrangement of
    /// screens. Read before anything re-places the panel or takes its frame as the
    /// new baseline — a drag starting, words redrawn, the panel ordered in or out —
    /// and not only on the lane's turn, which a move made just before one of those
    /// would never reach: the panel snapped back beside System Settings. A frame
    /// that changed with the screens is macOS moving it off a display: not the
    /// owner, so the baseline is dropped and the lane's turn places it again.
    fn note_owner_move(live: &mut Live) {
        if live.moved || !live.visible {
            return;
        }
        let Some(placed) = live.placed else {
            return;
        };
        // SAFETY: main thread (`Live` lives on it); `-frame` is `-(NSRect)` on the
        // live panel, and the screen queries are read-only.
        let (frame, screens) =
            unsafe { (appkit::send_rect(live.panel.id(), sel!(frame)), screens()) };
        match drift(
            from_cg(placed),
            &live.placed_on,
            from_cg(frame),
            &screens.frames,
        ) {
            Drift::Still => {}
            Drift::Screens => {
                live.placed = None;
                aterm_log::info!(
                    "devtools: the screens changed under the helper panel; placing it again"
                );
            }
            Drift::Owner => {
                live.moved = true;
                aterm_log::info!(
                    "devtools: the owner moved the helper panel; it stays where they put it"
                );
            }
        }
    }

    /// Order the panel out (another app is in front), and drop a reveal still
    /// waiting for System Settings' window.
    fn hide(live: &mut Live) {
        live.reveal_since = None;
        // SAFETY: main thread; the target is the live one `Live` holds.
        unsafe { cancel_reveal_soon(live.target.as_id()) };
        if !live.visible || live.dragging {
            return;
        }
        note_owner_move(live);
        // SAFETY: main thread; `-orderOut:` is `-(void)(id)`, nil the sender.
        unsafe { appkit::send_v_id(live.panel.id(), sel!(orderOut:), Id::NIL) };
        live.visible = false;
    }

    /// Put the panel where [`place`] says for `screens` and System Settings'
    /// window `settings`, unless the owner moved it, and remember where that
    /// left it.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn place_now(live: &mut Live, screens: &Screens, settings: Option<Rect>) {
        if live.moved {
            return;
        }
        let panel = live.panel.id();
        // SAFETY: main thread; `-frame` is `-(NSRect)` and `-setFrame:display:`
        // `-(void)(NSRect, BOOL)` on the live panel.
        unsafe {
            let frame = appkit::send_rect(panel, sel!(frame));
            let to = place(
                frame.size.width,
                frame.size.height,
                settings,
                &screens.visible,
            );
            if !same_frame(frame, to_cg(to)) {
                appkit::send_v_rect_bool(panel, sel!(setFrame:display:), to_cg(to), true);
            }
            note_placed(live, screens);
        }
        if settings.is_some() {
            live.beside = true;
        }
    }

    /// The screens' visible frames, the main (menu-bar) screen first, and the
    /// primary screen's height (Quartz's y axis turns about it).
    struct Screens {
        visible: Vec<Rect>,
        /// Each screen's whole frame: the arrangement, which the Dock and the
        /// menu bar do not change.
        frames: Vec<Rect>,
        primary_h: f64,
    }

    /// The screens now.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn screens() -> Screens {
        let mut visible = Vec::new();
        let mut frames = Vec::new();
        let mut primary_h = 0.0;
        // SAFETY: `+screens` is `+(NSArray *)`, +0; `-count`/`-objectAtIndex:`
        // read it; `-visibleFrame` and `-frame` are `-(NSRect)` on each screen.
        unsafe {
            let all = appkit::send_id(class(c"NSScreen").as_id(), sel!(screens));
            if !all.is_null() {
                for i in 0..appkit::send_usize(all, sel!(count)) {
                    let screen = appkit::send_id_usize(all, sel!(objectAtIndex:), i);
                    if screen.is_null() {
                        continue;
                    }
                    let frame = appkit::send_rect(screen, sel!(frame));
                    if i == 0 {
                        primary_h = frame.size.height;
                    }
                    frames.push(from_cg(frame));
                    visible.push(from_cg(appkit::send_rect(screen, sel!(visibleFrame))));
                }
            }
        }
        Screens {
            visible,
            frames,
            primary_h,
        }
    }

    /// The pid of the System Settings process that may show a window (it runs
    /// helpers under the same bundle id with the Prohibited policy).
    ///
    /// # Safety
    /// Any thread; read-only NSRunningApplication queries.
    unsafe fn settings_pid() -> Option<i32> {
        let id = appkit::nsstring(SETTINGS_ID)?;
        // SAFETY: `+runningApplicationsWithBundleIdentifier:` is `+(NSArray *)
        // (NSString *)`, +0; `-activationPolicy` is `-(NSInteger)` and
        // `-processIdentifier` `-(pid_t)` on each element.
        unsafe {
            let apps = appkit::send_id_id(
                class(c"NSRunningApplication").as_id(),
                sel!(runningApplicationsWithBundleIdentifier:),
                id.id(),
            );
            if apps.is_null() {
                return None;
            }
            (0..appkit::send_usize(apps, sel!(count)))
                .map(|i| appkit::send_id_usize(apps, sel!(objectAtIndex:), i))
                .find(|app| {
                    !app.is_null()
                        && appkit::send_isize(*app, sel!(activationPolicy))
                            != consts::NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED
                })
                .map(|app| appkit::send_i32(app, sel!(processIdentifier)))
                .filter(|pid| *pid > 0)
        }
    }

    /// System Settings' window in Cocoa screen space: the largest on-screen
    /// layer-0 window of its process with any opacity. Reads each entry's owner
    /// pid, layer, alpha and bounds, nothing else; keeps and logs none of it.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn settings_window(screens: &Screens) -> Option<Rect> {
        // SAFETY: read-only, as `settings_pid` says.
        let pid = unsafe { settings_pid() }?;
        let [pid_key, layer_key, alpha_key, bounds_key] = WINDOW_KEYS.map(appkit::nsstring);
        let (pid_key, layer_key, alpha_key, bounds_key) =
            (pid_key?, layer_key?, alpha_key?, bounds_key?);
        // SAFETY: `CGWindowListCopyWindowInfo` returns a +1 CFArray (the Copy
        // rule), adopted here and released once; toll-free bridging makes it an
        // NSArray of NSDictionaries, read with `-count`, `-objectAtIndex:` and
        // `-objectForKey:` (+0 each), the numbers with `-integerValue`/
        // `-doubleValue`, and the bounds dictionary turned back into a CGRect by
        // its own CoreGraphics reader into a local.
        unsafe {
            let list = Obj::from_owned(CGWindowListCopyWindowInfo(
                consts::CG_WINDOW_LIST_ON_SCREEN_NON_DESKTOP,
                consts::CG_NULL_WINDOW_ID,
            ))?;
            let mut best: Option<CGRect> = None;
            for i in 0..appkit::send_usize(list.id(), sel!(count)) {
                let window = appkit::send_id_usize(list.id(), sel!(objectAtIndex:), i);
                if window.is_null() {
                    continue;
                }
                let value = |key: &Obj| {
                    let v = appkit::send_id_id(window, sel!(objectForKey:), key.id());
                    (!v.is_null()).then_some(v)
                };
                let owner = value(&pid_key).map(|v| appkit::send_isize(v, sel!(integerValue)));
                if owner != Some(isize::try_from(pid).unwrap_or(-1)) {
                    continue;
                }
                if value(&layer_key).map_or(0, |v| appkit::send_isize(v, sel!(integerValue))) != 0 {
                    continue;
                }
                if value(&alpha_key).map_or(1.0, |v| appkit::send_f64(v, sel!(doubleValue))) <= 0.0
                {
                    continue;
                }
                let Some(bounds) = value(&bounds_key) else {
                    continue;
                };
                let mut frame = CGRect::default();
                if !CGRectMakeWithDictionaryRepresentation(bounds, &raw mut frame)
                    || frame.size.width < SETTINGS_MIN.width
                    || frame.size.height < SETTINGS_MIN.height
                {
                    continue;
                }
                let area = |r: CGRect| r.size.width * r.size.height;
                if best.is_none_or(|b| area(frame) > area(b)) {
                    best = Some(frame);
                }
            }
            best.map(|frame| cg_to_cocoa(from_cg(frame), screens.primary_h))
        }
    }

    /// Whether System Settings is the frontmost app now.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn frontmost_is_settings() -> bool {
        // SAFETY: `+sharedWorkspace` and `-frontmostApplication` are `-(id)`
        // accessors, +0.
        unsafe {
            let workspace = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            !workspace.is_null()
                && is_settings(appkit::send_id(workspace, sel!(frontmostApplication)))
        }
    }

    /// Whether `app` (an `NSRunningApplication`, or nil) is System Settings.
    ///
    /// # Safety
    /// `app` is nil or a live `NSRunningApplication`.
    unsafe fn is_settings(app: Id) -> bool {
        if app.is_null() {
            return false;
        }
        // SAFETY: `-bundleIdentifier` is `-(NSString *)`, +0 or nil.
        unsafe {
            let id = appkit::send_id(app, sel!(bundleIdentifier));
            !id.is_null() && appkit::nsstring_to_rust(id) == SETTINGS_ID
        }
    }

    /// Whether an activation notification is System Settings coming forward.
    ///
    /// # Safety
    /// `note` is the live NSNotification the workspace centre delivered.
    unsafe fn activated_settings(note: Id) -> bool {
        let Some(key) = appkit::nsstring(WORKSPACE_NAMES[1]) else {
            return false;
        };
        if note.is_null() {
            return false;
        }
        // SAFETY: `-userInfo` is `-(NSDictionary *)`, +0 or nil; `-objectForKey:`
        // `-(id)(id)`, +0 or nil — an NSRunningApplication under this key.
        unsafe {
            let info = appkit::send_id(note, sel!(userInfo));
            !info.is_null() && is_settings(appkit::send_id_id(info, sel!(objectForKey:), key.id()))
        }
    }

    /// `[[NSWorkspace sharedWorkspace] notificationCenter]`, or nil.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn workspace_center() -> Id {
        // SAFETY: both `-(id)` accessors, +0.
        unsafe {
            let workspace = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if workspace.is_null() {
                return Id::NIL;
            }
            appkit::send_id(workspace, sel!(notificationCenter))
        }
    }

    /// Watch which app comes forward. The centre holds `target` weakly; `close`
    /// removes it before `Live` lets it go.
    ///
    /// # Safety
    /// Main thread; `target` is a live [`HelperTarget`].
    unsafe fn observe_activation(target: Id) -> bool {
        let Some(name) = appkit::nsstring(WORKSPACE_NAMES[0]) else {
            return false;
        };
        // SAFETY: `-addObserver:selector:name:object:` is `-(void)(id, SEL, id,
        // id)`; the selector is one the target declares; nil object means "from
        // any sender".
        unsafe {
            let center = workspace_center();
            if center.is_null() {
                return false;
            }
            appkit::send_v_id_sel_id_id(
                center,
                sel!(addObserver:selector:name:object:),
                target,
                sel!(workspaceDidActivate:),
                name.id(),
                Id::NIL,
            );
        }
        true
    }

    /// Stop watching.
    ///
    /// # Safety
    /// Main thread; `target` is live.
    unsafe fn stop_observing(target: Id) {
        // SAFETY: `-removeObserver:` is `-(void)(id)`.
        unsafe {
            let center = workspace_center();
            if !center.is_null() {
                appkit::send_v_id(center, sel!(removeObserver:), target);
            }
        }
    }

    /// Look again for System Settings' window in a moment: a delayed perform on
    /// the main run loop, never a sleep. One at a time: an earlier one not yet
    /// fired is dropped.
    ///
    /// # Safety
    /// Main thread; `target` is a live [`HelperTarget`].
    unsafe fn reveal_soon(target: Id) {
        // SAFETY: `-performSelector:withObject:afterDelay:` is `-(void)(SEL, id,
        // NSTimeInterval)`; the run loop retains the target until it fires, and
        // `hide` and `close` cancel it.
        unsafe {
            cancel_reveal_soon(target);
            appkit::send_v_sel_id_f64(
                target,
                sel!(performSelector:withObject:afterDelay:),
                sel!(revealSoon:),
                Id::NIL,
                REVEAL_RETRY_SECS,
            );
        }
    }

    /// Drop a [`reveal_soon`] not yet fired (the target's only delayed perform).
    ///
    /// # Safety
    /// Main thread; `target` is a live [`HelperTarget`].
    unsafe fn cancel_reveal_soon(target: Id) {
        // SAFETY: `+cancelPreviousPerformRequestsWithTarget:` is `+(void)(id)`;
        // a target with nothing scheduled is a no-op.
        unsafe {
            appkit::send_v_id(
                class(c"NSObject").as_id(),
                sel!(cancelPreviousPerformRequestsWithTarget:),
                target,
            );
        }
    }

    /// Bring aterm forward (the status link: the owner asked to see it).
    ///
    /// # Safety
    /// Main thread.
    unsafe fn activate_aterm() {
        // SAFETY: `+currentApplication` is `+(NSRunningApplication *)`, +0;
        // `-activateWithOptions:` is `-(BOOL)(NSApplicationActivationOptions)`.
        unsafe {
            let me = appkit::send_id(
                class(c"NSRunningApplication").as_id(),
                sel!(currentApplication),
            );
            if !me.is_null() {
                let _ = appkit::send_bool_usize(
                    me,
                    sel!(activateWithOptions:),
                    consts::NS_APPLICATION_ACTIVATE_ALL_WINDOWS_IGNORING_OTHER_APPS,
                );
            }
        }
    }

    // ---- the drag ----

    /// The open hand (`true`) or the arrow, set now.
    fn set_cursor(open_hand: bool) {
        // SAFETY: main thread (AppKit's mouse-tracking dispatch); `+openHandCursor`
        // and `+arrowCursor` are `+(NSCursor *)`, +0, and `-set` is `-(void)`.
        unsafe {
            let cursors = class(c"NSCursor").as_id();
            let cursor = if open_hand {
                appkit::send_id(cursors, sel!(openHandCursor))
            } else {
                appkit::send_id(cursors, sel!(arrowCursor))
            };
            if !cursor.is_null() {
                appkit::send_v(cursor, sel!(set));
            }
        }
    }

    /// Whether `tile` is the tile on the panel up: a drag belongs to the panel whose
    /// tile is its source, and to no other.
    fn holds_tile(live: &Live, tile: Id) -> bool {
        live.tile.as_ref().is_some_and(|held| held.as_id() == tile)
    }

    /// The drag of `tile` began: its panel dims, and when it lies wholly on System
    /// Settings' window (the owner moved it over the list) lets the mouse through,
    /// as a whole, and steps back, so the list beneath it takes the drop.
    /// Otherwise — beside the window, straddling its edge, or in aterm's own
    /// fallback over the sidebar strip, which hangs the tile outside the window —
    /// it keeps the mouse: a release over it puts the icon back (no destination
    /// takes it) rather than dropping it on whatever window is beneath — an aterm
    /// terminal would type the bundle's path into its shell, Finder would copy it.
    fn drag_began(tile: Id) {
        let _ = with_live(|live| {
            if !holds_tile(live, tile) {
                return;
            }
            note_owner_move(live);
            live.dragging = true;
            let panel = live.panel.id();
            // SAFETY: main thread; `-frame` is `-(NSRect)`, the window and screen
            // queries are read-only; `-setIgnoresMouseEvents:` `-(void)(BOOL)`,
            // `-setAlphaValue:` `-(void)(CGFloat)`, `-orderBack:` `-(void)(id)`.
            unsafe {
                let frame = from_cg(appkit::send_rect(panel, sel!(frame)));
                let on_settings =
                    settings_window(&screens()).is_some_and(|window| window.holds(frame));
                appkit::send_v_f64(panel, sel!(setAlphaValue:), DRAG_ALPHA);
                if on_settings {
                    appkit::send_v_bool(panel, sel!(setIgnoresMouseEvents:), true);
                    appkit::send_v_id(panel, sel!(orderBack:), Id::NIL);
                }
            }
        });
    }

    /// The drag of `tile` ended: its panel takes the mouse again, draws any
    /// words that waited, and is in front again if System Settings still is.
    /// Only the panel whose tile `tile` is — the drag's source — and only while
    /// its drag runs: a panel shown after that one closed mid-drag (its tile
    /// lingering) holds another tile, and is not touched.
    fn drag_ended(tile: Id) {
        let Some(mtm) = MainThread::new() else {
            return;
        };
        let _ = with_live(|live| {
            if !live.dragging || !holds_tile(live, tile) {
                return;
            }
            live.dragging = false;
            let panel = live.panel.id();
            // SAFETY: as in `drag_began`.
            unsafe {
                appkit::send_v_bool(panel, sel!(setIgnoresMouseEvents:), false);
                appkit::send_v_f64(panel, sel!(setAlphaValue:), 1.0);
            }
            if let Some(content) = live.pending.take() {
                refill(mtm, live, &content);
            }
            if live.settings_front {
                order_in(live);
            } else {
                hide(live);
            }
        });
    }

    /// Whether a drag of `tile` that ended at `at` (screen space, Cocoa's) ended
    /// over System Settings' window as the placement reads it ([`drop_landed`];
    /// the drag image is the tile's bounds, as `begin_drag` set it).
    ///
    /// # Safety
    /// Main thread; `tile` is the live tile view.
    unsafe fn dropped_in_settings(tile: Id, at: CGPoint) -> bool {
        // SAFETY: main thread; read-only window and screen queries, and
        // `-bounds` is `-(NSRect)` on the live tile.
        let (settings, image) = unsafe {
            let screens = screens();
            (
                settings_window(&screens),
                appkit::send_rect(tile, sel!(bounds)),
            )
        };
        drop_landed(settings, at.x, at.y, image.size.width, image.size.height)
    }

    /// The point `event` happened at, in `view`'s coordinates.
    ///
    /// # Safety
    /// `view` is a live NSView and `event` nil or a live NSEvent, main thread.
    unsafe fn point_in(view: Id, event: Id) -> CGPoint {
        if event.is_null() {
            return CGPoint::default();
        }
        // SAFETY: `-locationInWindow` is `-(NSPoint)`; `-convertPoint:fromView:`
        // `-(NSPoint)(NSPoint, NSView *)`, nil meaning window coordinates.
        unsafe {
            let at = appkit::send_point(event, sel!(locationInWindow));
            appkit::send_point_point_id(view, sel!(convertPoint:fromView:), at, Id::NIL)
        }
    }

    /// The drag's payload for `bundle`, built from `NSURL` (percent-encoded, a
    /// trailing slash for the bundle directory) and never by hand: the file URL
    /// as both URL types and the path as plain text ([`TYPE_FILENAMES`] says why
    /// the old filenames list is not among them). +1.
    ///
    /// # Safety
    /// Main thread.
    pub(crate) unsafe fn pasteboard_item(bundle: &Path) -> Option<Obj> {
        let path = appkit::nsstring(bundle.to_str()?)?;
        let [file_url, url, text] = [TYPE_FILE_URL, TYPE_URL, TYPE_TEXT].map(appkit::nsstring);
        let (file_url, url, text) = (file_url?, url?, text?);
        // SAFETY: `+fileURLWithPath:isDirectory:` is `+(NSURL *)(NSString *,
        // BOOL)`, +0; `-absoluteString`/`-path` `-(NSString *)`, +0;
        // `+[NSPasteboardItem new]` +1, adopted; `-setString:forType:` is
        // `-(BOOL)(NSString *, NSPasteboardType)`.
        unsafe {
            let ns_url = appkit::send_id_id_bool(
                class(c"NSURL").as_id(),
                sel!(fileURLWithPath:isDirectory:),
                path.id(),
                true,
            );
            if ns_url.is_null() {
                return None;
            }
            let absolute = appkit::send_id(ns_url, sel!(absoluteString));
            let plain = appkit::send_id(ns_url, sel!(path));
            if absolute.is_null() || plain.is_null() {
                return None;
            }
            let item = Obj::from_owned(appkit::send_id(
                class(c"NSPasteboardItem").as_id(),
                sel!(new),
            ))?;
            let set = |value: Id, ty: &Obj| {
                appkit::send_bool_id_id(item.id(), sel!(setString:forType:), value, ty.id())
            };
            let wrote = set(absolute, &file_url) && set(absolute, &url) && set(plain, &text);
            wrote.then_some(item)
        }
    }

    /// Begin the owner's drag of `tile` with the `mouseDragged:` event that
    /// crossed the slop. `false` when it could not begin.
    ///
    /// # Safety
    /// Main thread, inside AppKit's `mouseDragged:` dispatch with its event.
    unsafe fn begin_drag(tile: &TileView, event: Id) -> bool {
        let ivars = tile.ivars();
        // SAFETY: the payload as `pasteboard_item` says; `+alloc` and
        // `-initWithPasteboardWriter:` (`-(id)(id<NSPasteboardWriting>)`) give a
        // +1 NSDraggingItem, adopted; `-setDraggingFrame:contents:` is
        // `-(void)(NSRect, id)`; `+arrayWithObject:` +0; and
        // `-beginDraggingSessionWithItems:event:source:` is `-(NSDraggingSession
        // *)(NSArray *, NSEvent *, id<NSDraggingSource>)`, +0 — the session runs
        // in the event loop and returns at once (no nested tracking loop).
        unsafe {
            let Some(writer) = pasteboard_item(&ivars.bundle) else {
                aterm_log::warn!("devtools: the tile's drag has no payload for its bundle");
                return false;
            };
            let Some(item) = Obj::from_owned(appkit::send_id_id(
                appkit::alloc(class(c"NSDraggingItem")),
                sel!(initWithPasteboardWriter:),
                writer.id(),
            )) else {
                return false;
            };
            let bounds = appkit::send_rect(tile.as_id(), sel!(bounds));
            appkit::send_v_rect_id(
                item.id(),
                sel!(setDraggingFrame:contents:),
                bounds,
                ivars.icon.id(),
            );
            let items =
                appkit::send_id_id(class(c"NSArray").as_id(), sel!(arrayWithObject:), item.id());
            if items.is_null() {
                return false;
            }
            !appkit::send_id_id_id_id(
                tile.as_id(),
                sel!(beginDraggingSessionWithItems:event:source:),
                items,
                event,
                tile.as_id(),
            )
            .is_null()
        }
    }

    // ---- building the views ----

    /// A panel and its views, built and not shown.
    struct Built {
        panel: Obj,
        target: Retained<HelperTarget>,
        content: Retained<ContentView>,
        tile: Option<Retained<TileView>>,
    }

    /// Build the panel for `content`: its target, content view and window.
    fn build(mtm: MainThread, content: &Content, events: PanelEvents) -> Option<Built> {
        let target = HelperTarget::alloc_init(
            mtm,
            TargetIvars {
                events: events.clone(),
            },
        )?;
        let view = ContentView::build(mtm)?;
        // SAFETY: main thread; every object here is fresh and live.
        unsafe {
            let (tile, size) = fill(mtm, view.as_id(), content, target.as_id(), None, &events);
            let panel = new_panel(target.as_id())?;
            appkit::send_v_id(panel.id(), sel!(setContentView:), view.as_id());
            size_to_content(panel.id(), size);
            Some(Built {
                panel,
                target,
                content: view,
                tile,
            })
        }
    }

    /// Draw `content` again on the panel up.
    fn refill(mtm: MainThread, live: &mut Live, content: &Content) {
        note_owner_move(live);
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: main thread; the panel, its views and target are live.
            unsafe {
                let (tile, size) = fill(
                    mtm,
                    live.content.as_id(),
                    content,
                    live.target.as_id(),
                    live.tile.take(),
                    &live.events,
                );
                live.tile = tile;
                size_to_content(live.panel.id(), size);
                // aterm's own resize: not the owner moving it.
                if live.placed.is_some() && !live.moved {
                    note_placed(live, &screens());
                }
                // A panel the owner moved is never placed again, so it has no
                // corner to avoid, and it stays up as the lane leaves it: hidden
                // here, it would not come back when the window does (the lane's
                // turn returns early on `moved`).
                if live.visible && !live.moved {
                    let screens = screens();
                    let settings = settings_window(&screens);
                    // The lane's own rule: beside a window it no longer finds (System
                    // Settings minimized), the panel hides rather than jump to a corner.
                    match place_turn(true, true, live.beside, settings) {
                        PlaceTurn::Hide => hide(live),
                        _ => place_now(live, &screens, settings),
                    }
                }
            }
        });
    }

    /// The panel window: a non-activating utility panel, floating, on every
    /// Space and beside full-screen apps, kept while aterm is inactive, key
    /// only when a view needs it, kept after a close (aterm releases it), not
    /// movable by its background, titled "aterm", with `delegate` for its close
    /// box. +1. Not shown.
    ///
    /// # Safety
    /// Main thread; `delegate` is a live [`HelperTarget`] that outlives the panel's
    /// use of it (`close` clears it first).
    unsafe fn new_panel(delegate: Id) -> Option<Obj> {
        let title = appkit::nsstring(TITLE)?;
        // SAFETY: `+alloc` then the designated initializer
        // `-initWithContentRect:styleMask:backing:defer:` (`-(id)(NSRect,
        // NSUInteger, NSUInteger, BOOL)`), which consumes the +1 and returns +1;
        // every setter after it is `-(void)(BOOL)`, `-(void)(NSInteger)`,
        // `-(void)(NSUInteger)` or `-(void)(id)` on the fresh panel.
        unsafe {
            let raw = appkit::alloc(class(c"NSPanel"));
            if raw.is_null() {
                return None;
            }
            let panel = Obj::from_owned(appkit::send_id_rect_usize_usize_bool(
                raw,
                sel!(initWithContentRect:styleMask:backing:defer:),
                rect(0.0, 0.0, PANEL_W, PANEL_W),
                consts::NS_WINDOW_STYLE_MASK_UTILITY_PANEL,
                consts::NS_BACKING_STORE_BUFFERED,
                true,
            ))?;
            let p = panel.id();
            appkit::send_v_bool(p, sel!(setFloatingPanel:), true);
            appkit::send_v_isize(p, sel!(setLevel:), consts::NS_FLOATING_WINDOW_LEVEL);
            appkit::send_v_bool(p, sel!(setHidesOnDeactivate:), false);
            appkit::send_v_bool(p, sel!(setCanHide:), false);
            appkit::send_v_bool(p, sel!(setBecomesKeyOnlyIfNeeded:), true);
            appkit::send_v_usize(
                p,
                sel!(setCollectionBehavior:),
                consts::NS_WINDOW_COLLECTION_BEHAVIOR_ALL_SPACES_AUXILIARY,
            );
            appkit::send_v_bool(p, sel!(setReleasedWhenClosed:), false);
            appkit::send_v_bool(p, sel!(setMovableByWindowBackground:), false);
            appkit::send_v_id(p, sel!(setTitle:), title.id());
            appkit::send_v_id(p, sel!(setDelegate:), delegate);
            Some(panel)
        }
    }

    /// Size the panel to its laid-out content, `(width, height)` from [`fill`],
    /// keeping its top edge. Never the content view's bounds: `-setContentView:`
    /// fits the view to the window's content rect, so on a new panel they read
    /// back its first square, and a panel sized from them cut off the buttons
    /// under taller words (measured live 2026-10-16).
    ///
    /// # Safety
    /// Main thread; `panel` live.
    unsafe fn size_to_content(panel: Id, (width, height): (f64, f64)) {
        // SAFETY: `-frame` is `-(NSRect)`; `-frameRectForContentRect:`
        // `-(NSRect)(NSRect)`; `-setFrame:display:` `-(void)(NSRect, BOOL)`.
        unsafe {
            let wanted = appkit::send_rect_rect(
                panel,
                sel!(frameRectForContentRect:),
                rect(0.0, 0.0, width, height),
            );
            let frame = appkit::send_rect(panel, sel!(frame));
            let top = frame.origin.y + frame.size.height;
            let to = rect(
                frame.origin.x,
                top - wanted.size.height,
                wanted.size.width,
                wanted.size.height,
            );
            if !same_frame(frame, to) {
                appkit::send_v_rect_bool(panel, sel!(setFrame:display:), to, true);
            }
        }
    }

    /// Lay `content` out ([`layout`], measured by AppKit) and put its views on
    /// `view`, replacing what was there. The tile is reused when it carries the
    /// same bundle. Returns the tile on the view, if any, and the size the
    /// content lays out to (`(width, height)`, for [`size_to_content`]).
    ///
    /// # Safety
    /// Main thread; `view` is a live [`ContentView`] and `target` a live
    /// [`HelperTarget`].
    unsafe fn fill(
        mtm: MainThread,
        view: Id,
        content: &Content,
        target: Id,
        reuse: Option<Retained<TileView>>,
        events: &PanelEvents,
    ) -> (Option<Retained<TileView>>, (f64, f64)) {
        let laid = layout(content, &AppKitMeasure { mtm });
        let tile_block = content.blocks.iter().find_map(|block| match block {
            Block::Tile(tile) => Some(tile),
            _ => None,
        });
        let link = content.blocks.iter().find_map(|block| match block {
            Block::Link(text) => Some(*text),
            _ => None,
        });
        let mut tile_out = None;
        // SAFETY: main thread; `+array` +0 and `-setSubviews:` `-(void)(NSArray
        // *)` clear the view (each old subview released by it); every subview
        // below is fresh or the reused tile, framed with `-setFrame:`
        // (`-(void)(NSRect)`) and added with `-addSubview:` (`-(void)(id)`),
        // which retains it.
        unsafe {
            let empty = appkit::send_id(class(c"NSArray").as_id(), sel!(array));
            if !empty.is_null() {
                appkit::send_v_id(view, sel!(setSubviews:), empty);
            }
            for (part, at) in &laid.parts {
                let frame = to_cg(*at);
                let sub: Option<Obj> = match part {
                    Part::Text(index) => match content.blocks.get(*index) {
                        Some(Block::Text { text, tone }) => label(text, *tone),
                        _ => None,
                    },
                    Part::Icon => tile_block.and_then(|tile| {
                        let reused = reuse
                            .as_ref()
                            .filter(|old| old.ivars().bundle == tile.bundle)
                            .map(Retained::clone_retained);
                        let built = reused
                            .or_else(|| TileView::build(mtm, &tile.bundle, events.clone()))?;
                        built.set_live(tile.live);
                        let id = Obj::retain(built.as_id());
                        tile_out = Some(built);
                        id
                    }),
                    Part::Caption => label(TILE_CAPTION, Tone::Title),
                    Part::Hint => tile_block.and_then(|tile| label(tile.hint(), Tone::Body)),
                    Part::Path => tile_block
                        .and_then(|tile| label(&tile.bundle.display().to_string(), Tone::Small)),
                    Part::Button(button) => HelperButton::titled(mtm, button.label(), false)
                        .inspect(|b| b.wire(target, sel!(press:), button.tag()))
                        .and_then(|b| Obj::retain(b.as_id())),
                    Part::Link => link
                        .and_then(|text| HelperButton::titled(mtm, text, true))
                        .inspect(|b| b.wire(target, sel!(openStatus:), -1))
                        .and_then(|b| Obj::retain(b.as_id())),
                };
                if let Some(sub) = sub {
                    appkit::send_v_rect(sub.id(), sel!(setFrame:), frame);
                    appkit::send_v_id(view, sel!(addSubview:), sub.id());
                }
            }
            appkit::send_v_rect(view, sel!(setFrame:), rect(0.0, 0.0, laid.w, laid.h));
        }
        (tile_out, (laid.w, laid.h))
    }

    /// The words a block shows: the done words beside a check mark.
    fn shown(text: &str, tone: Tone) -> Cow<'_, str> {
        if tone == Tone::Done {
            Cow::Owned(format!("\u{2713} {text}"))
        } else {
            Cow::Borrowed(text)
        }
    }

    /// The font a tone is set in, +0.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn font(tone: Tone) -> Id {
        let (size, bold) = match tone {
            Tone::Title => (13.0, true),
            Tone::Body => (12.0, false),
            Tone::Notice | Tone::Done => (12.0, true),
            Tone::Secondary => (11.0, false),
            Tone::Small => (10.0, false),
        };
        let font = class(c"NSFont").as_id();
        // Each selector at its own send, so the census pairs it with its helper.
        // SAFETY: both are `+(NSFont *)(CGFloat)`, +0, non-raising.
        unsafe {
            if bold {
                appkit::send_id_f64(font, sel!(boldSystemFontOfSize:), size)
            } else {
                appkit::send_id_f64(font, sel!(systemFontOfSize:), size)
            }
        }
    }

    /// The colour a tone is set in, +0.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn color(tone: Tone) -> Id {
        let color = class(c"NSColor").as_id();
        // SAFETY: a `+(NSColor *)` system colour, +0.
        unsafe {
            match tone {
                Tone::Secondary | Tone::Small => appkit::send_id(color, sel!(secondaryLabelColor)),
                Tone::Title | Tone::Body | Tone::Notice | Tone::Done => {
                    appkit::send_id(color, sel!(labelColor))
                }
            }
        }
    }

    /// A wrapping, unselectable label of `text` set in `tone`. +1.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn label(text: &str, tone: Tone) -> Option<Obj> {
        let words = appkit::nsstring(&shown(text, tone))?;
        // SAFETY: `+wrappingLabelWithString:` is the documented non-raising
        // factory, +0, retained here; `-setFont:`/`-setTextColor:` are
        // `-(void)(id)` and `-setSelectable:` `-(void)(BOOL)`.
        unsafe {
            let field = Obj::retain(appkit::send_id_id(
                class(c"NSTextField").as_id(),
                sel!(wrappingLabelWithString:),
                words.id(),
            ))?;
            appkit::send_v_id(field.id(), sel!(setFont:), font(tone));
            appkit::send_v_id(field.id(), sel!(setTextColor:), color(tone));
            appkit::send_v_bool(field.id(), sel!(setSelectable:), false);
            Some(field)
        }
    }

    /// How AppKit sets the panel's words: each measured by the same label (or
    /// button) the panel shows.
    struct AppKitMeasure {
        mtm: MainThread,
    }

    impl Measure for AppKitMeasure {
        fn text_height(&self, text: &str, tone: Tone, width: f64) -> f64 {
            // SAFETY: main thread (the witness); `-cell` is `-(id)`, +0, and
            // `-cellSizeForBounds:` `-(NSSize)(NSRect)` — a wrapping cell answers
            // the height its words take at that width.
            unsafe {
                let Some(field) = label(text, tone) else {
                    return 0.0;
                };
                let cell = appkit::send_id(field.id(), sel!(cell));
                if cell.is_null() {
                    return 0.0;
                }
                appkit::send_size_rect(
                    cell,
                    sel!(cellSizeForBounds:),
                    rect(0.0, 0.0, width, f64::from(u16::MAX)),
                )
                .height
                .ceil()
            }
        }

        fn button_width(&self, title: &str) -> f64 {
            // SAFETY: main thread (the witness); `-cellSize` is `-(NSSize)` on the
            // button's cell.
            unsafe {
                HelperButton::titled(self.mtm, title, false).map_or(0.0, |button| {
                    let cell = appkit::send_id(button.as_id(), sel!(cell));
                    if cell.is_null() {
                        0.0
                    } else {
                        appkit::send_size(cell, sel!(cellSize)).width.ceil() + BUTTON_PAD
                    }
                })
            }
        }
    }

    impl ContentView {
        /// A fresh content view.
        fn build(mtm: MainThread) -> Option<Retained<Self>> {
            // SAFETY: `alloc_ivars` returns a +1 instance owing one designated
            // initializer, the next send: `-initWithFrame:` (`-(id)(NSRect)`),
            // which consumes it and returns +1.
            unsafe {
                let raw = Self::alloc_ivars(mtm, ());
                if raw.is_null() {
                    return None;
                }
                Retained::from_owned(appkit::send_id_rect(
                    raw,
                    sel!(initWithFrame:),
                    rect(0.0, 0.0, PANEL_W, PANEL_W),
                ))
            }
        }
    }

    impl HelperButton {
        /// A push button titled `title`, or (`link`) the borderless status link
        /// in the link colour, left-aligned.
        ///
        /// # Safety
        /// Main thread.
        unsafe fn titled(mtm: MainThread, title: &str, link: bool) -> Option<Retained<Self>> {
            let words = appkit::nsstring(title)?;
            // SAFETY: `alloc_ivars` then `-initWithFrame:`, as for the content
            // view; then `-setTitle:` `-(void)(NSString *)`, `-setBezelStyle:`
            // `-(void)(NSUInteger)`, `-setBordered:` `-(void)(BOOL)`, `-setFont:`/
            // `-setContentTintColor:` `-(void)(id)` and `-setAlignment:`
            // `-(void)(NSInteger)` on the fresh button.
            unsafe {
                let raw = Self::alloc_ivars(mtm, ());
                if raw.is_null() {
                    return None;
                }
                let this = Retained::<Self>::from_owned(appkit::send_id_rect(
                    raw,
                    sel!(initWithFrame:),
                    rect(0.0, 0.0, 0.0, 0.0),
                ))?;
                let b = this.as_id();
                appkit::send_v_id(b, sel!(setTitle:), words.id());
                if link {
                    appkit::send_v_bool(b, sel!(setBordered:), false);
                    appkit::send_v_id(b, sel!(setFont:), font(Tone::Small));
                    appkit::send_v_id(
                        b,
                        sel!(setContentTintColor:),
                        appkit::send_id(class(c"NSColor").as_id(), sel!(linkColor)),
                    );
                    appkit::send_v_isize(b, sel!(setAlignment:), consts::NS_TEXT_ALIGNMENT_LEFT);
                } else {
                    appkit::send_v_usize(b, sel!(setBezelStyle:), consts::NS_BEZEL_STYLE_PUSH);
                }
                Some(this)
            }
        }

        /// Point the button at `target`'s `action`, carrying `tag`.
        ///
        /// # Safety
        /// Main thread; `target` outlives the button's use of it (AppKit holds a
        /// control's target weakly; `Live` keeps it).
        unsafe fn wire(&self, target: Id, action: Sel, tag: isize) {
            // SAFETY: `-setTarget:` `-(void)(id)`, `-setAction:` `-(void)(SEL)`,
            // `-setTag:` `-(void)(NSInteger)`.
            unsafe {
                appkit::send_v_id(self.as_id(), sel!(setTarget:), target);
                appkit::send_v_sel(self.as_id(), sel!(setAction:), action);
                appkit::send_v_isize(self.as_id(), sel!(setTag:), tag);
            }
        }
    }

    impl TileView {
        /// A tile for `bundle`: its Finder icon at [`ICON`] points, an
        /// accessibility image named "aterm". `None` when there is no icon.
        fn build(mtm: MainThread, bundle: &Path, events: PanelEvents) -> Option<Retained<Self>> {
            let path = appkit::nsstring(bundle.to_str()?)?;
            let name = appkit::nsstring(TILE_CAPTION)?;
            let role = appkit::nsstring("AXImage")?;
            // SAFETY: `+sharedWorkspace` +0; `-iconForFile:` `-(NSImage *)(NSString
            // *)`, +0, retained here; `-setSize:` `-(void)(NSSize)`; then
            // `alloc_ivars` + `-initWithFrame:` as for the content view, and the
            // accessibility setters `-(void)(BOOL)` / `-(void)(id)`.
            unsafe {
                let workspace =
                    appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
                if workspace.is_null() {
                    return None;
                }
                let icon =
                    Obj::retain(appkit::send_id_id(workspace, sel!(iconForFile:), path.id()))?;
                appkit::send_v_size(
                    icon.id(),
                    sel!(setSize:),
                    CGSize {
                        width: ICON,
                        height: ICON,
                    },
                );
                let raw = Self::alloc_ivars(
                    mtm,
                    TileIvars {
                        bundle: bundle.to_path_buf(),
                        icon,
                        live: Cell::new(true),
                        down: Cell::new(None),
                        began: Cell::new(false),
                        events,
                    },
                );
                if raw.is_null() {
                    return None;
                }
                let this = Retained::<Self>::from_owned(appkit::send_id_rect(
                    raw,
                    sel!(initWithFrame:),
                    rect(0.0, 0.0, ICON, ICON),
                ))?;
                appkit::send_v_bool(this.as_id(), sel!(setAccessibilityElement:), true);
                appkit::send_v_id(this.as_id(), sel!(setAccessibilityRole:), role.id());
                appkit::send_v_id(this.as_id(), sel!(setAccessibilityLabel:), name.id());
                // The hover that sets the open hand (`mouseEntered:`): entered and
                // exited, active always, following the visible rect. The view keeps
                // its tracking areas; `-initWithRect:options:owner:userInfo:`
                // consumes `+alloc`'s +1, and the owner is held weakly.
                if let Some(area) = Obj::from_owned(appkit::send_id_rect_usize_id_id(
                    appkit::alloc(class(c"NSTrackingArea")),
                    sel!(initWithRect:options:owner:userInfo:),
                    rect(0.0, 0.0, ICON, ICON),
                    consts::NS_TRACKING_HOVER_IN_VISIBLE_RECT,
                    this.as_id(),
                    Id::NIL,
                )) {
                    appkit::send_v_id(this.as_id(), sel!(addTrackingArea:), area.id());
                }
                Some(this)
            }
        }

        /// Whether the tile can still be dragged (dimmed once spent).
        fn set_live(&self, live: bool) {
            self.ivars().live.set(live);
            // SAFETY: main thread; `-setAlphaValue:` is `-(void)(CGFloat)`.
            unsafe {
                appkit::send_v_f64(
                    self.as_id(),
                    sel!(setAlphaValue:),
                    if live { 1.0 } else { SPENT_ALPHA },
                );
            }
        }
    }

    // ---- for the driver ----

    /// The panel's views for `content`, built exactly as the live panel builds
    /// them, in no window: what `devtools_helper_drive` renders and measures.
    pub(crate) struct Preview {
        /// The content view, laid out.
        pub(crate) view: Retained<ContentView>,
        /// The panel window, never shown.
        pub(crate) panel: Option<Obj>,
        /// Its buttons' target (kept: they hold it weakly).
        pub(crate) target: Retained<HelperTarget>,
        /// The tile, when the words carry one.
        pub(crate) tile: Option<Retained<TileView>>,
    }

    /// Build a [`Preview`] of `content`; `panel` also builds the (never shown)
    /// window around it.
    pub(crate) fn preview(mtm: MainThread, content: &Content, panel: bool) -> Option<Preview> {
        let events = PanelEvents::unbound();
        if panel {
            let built = build(mtm, content, events)?;
            return Some(Preview {
                view: built.content,
                panel: Some(built.panel),
                target: built.target,
                tile: built.tile,
            });
        }
        let target = HelperTarget::alloc_init(
            mtm,
            TargetIvars {
                events: events.clone(),
            },
        )?;
        let view = ContentView::build(mtm)?;
        // SAFETY: main thread; fresh live objects.
        let (tile, _) = unsafe { fill(mtm, view.as_id(), content, target.as_id(), None, &events) };
        Some(Preview {
            view,
            panel: None,
            target,
            tile,
        })
    }
}

/// Stand-in arms for the tests that drive the lane through an `App`: nothing they hold
/// reaches the OS, and what they were asked is counted per test thread.
#[cfg(test)]
pub(crate) mod test_arms {
    use std::cell::Cell;
    use std::time::Instant;

    use aterm_containment::DevToolGrant;
    use aterm_containment::consent::{
        DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel, ProbeGate,
    };

    use super::{DevToolsHelperArm, Lane, Reach, Reading};

    /// The bundle id the stand-ins read for.
    pub(crate) const SUBJECT: &str = "com.aterm.test";

    /// The phrases no Developer Tools surface says, on top of the owner's restart
    /// phrases (`native_settings`' `BANNED_PANEL_PHRASES`): what being listed skips is
    /// "should" until D14, and nothing claims a check or a prompt gone for good.
    const RULED_OUT_PHRASES: [&str; 2] = ["never prompt", "can stay off"];

    /// Stems no Developer Tools surface starts a word with ("covers", "eliminates"):
    /// matched at a word's start only, so "discover" and "recover" pass.
    const RULED_OUT_STEMS: [&str; 2] = ["cover", "eliminat"];

    /// THE ONE FENCE for every Developer Tools sweep in this crate — the panel's
    /// states and its own words, the Settings card, its bar: the first ruled-out
    /// phrase or stem `text` holds, or `None`. atpkg's own test sweeps atpkg's words
    /// with the same set (a test of one crate cannot read another's).
    pub(crate) fn ruled_out(text: &str) -> Option<&'static str> {
        let lower = text.to_lowercase();
        if let Some(phrase) = crate::native_settings::tests::BANNED_PANEL_PHRASES
            .iter()
            .chain(RULED_OUT_PHRASES.iter())
            .find(|phrase| lower.contains(*phrase))
        {
            return Some(phrase);
        }
        RULED_OUT_STEMS.iter().copied().find(|stem| {
            lower
                .split(|c: char| !c.is_alphabetic())
                .any(|word| word.starts_with(stem))
        })
    }

    thread_local! {
        static OPENS: Cell<u32> = const { Cell::new(0) };
        static OPENED_NOTES: Cell<u32> = const { Cell::new(0) };
        static DECLINE_NOTES: Cell<u32> = const { Cell::new(0) };
    }

    fn bump(counter: &'static std::thread::LocalKey<Cell<u32>>) {
        counter.with(|c| c.set(c.get() + 1));
    }

    /// How often this thread's stand-ins were asked to open the page.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn opens() -> u32 {
        OPENS.with(Cell::get)
    }

    /// How often this thread's stand-ins noted the page opened.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn opened_notes() -> u32 {
        OPENED_NOTES.with(Cell::get)
    }

    /// How often this thread's stand-ins noted the owner's Not now.
    pub(crate) fn decline_notes() -> u32 {
        DECLINE_NOTES.with(Cell::get)
    }

    /// A list row.
    pub(crate) fn list(
        listing: DevToolListing,
        label: DevToolListLabel,
        rows: Option<u32>,
    ) -> DevToolList {
        DevToolList {
            listing,
            label,
            modified_at_s: None,
            rows,
        }
    }

    /// A list read whole, `rows` long, without aterm.
    pub(crate) fn absent(rows: u32) -> DevToolList {
        list(DevToolListing::Absent, DevToolListLabel::Read, Some(rows))
    }

    /// A list aterm cannot read (no Full Disk Access).
    pub(crate) fn unread() -> DevToolList {
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
    }

    /// aterm listed and switched on.
    pub(crate) fn on() -> DevToolList {
        list(DevToolListing::On, DevToolListLabel::Read, Some(2))
    }

    /// tccd's answers in a stand-in reading: three new programs checked, the newest
    /// with no time on it (so a read row decides the joined verdict).
    pub(crate) const fn checked_three() -> DevToolProbe {
        DevToolProbe {
            requests: Some(3),
            not_allowed: Some(3),
            ..DevToolProbe::refused(DevToolProbeLabel::Observed)
        }
    }

    /// A reading of `list` with `grant`, about [`SUBJECT`].
    pub(crate) fn reading(grant: DevToolGrant, list: DevToolList) -> Reading {
        Reading {
            grant,
            list,
            log: checked_three(),
            subject: Some(SUBJECT),
        }
    }

    /// A reach whose page opens (counted), whose `spctl` answers Ok, whose list reads
    /// aterm on, whose markers are counted and never written, and whose build is
    /// grant-stable.
    pub(crate) fn opening() -> Reach {
        Reach {
            open_pane: || {
                bump(&OPENS);
                true
            },
            list_terminal: || Ok(()),
            read_list: |_, _| on(),
            note_opened: |_| bump(&OPENED_NOTES),
            note_declined: |_| bump(&DECLINE_NOTES),
            grant_stable: || true,
            ..Reach::inert()
        }
    }

    /// When the stand-in decline was said, and until when it holds (unix seconds).
    pub(crate) const DECLINED: (u64, u64) = (1_791_000_000, 1_791_000_000 + 30 * 86_400);

    /// [`opening`], with the owner's Not now on record ([`DECLINED`]).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn declining() -> Reach {
        Reach {
            declined: || Some(DECLINED),
            ..opening()
        }
    }

    /// [`opening`], whose shared opened marker reads `last_opened`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn marked(last_opened: fn() -> Option<(String, u64)>) -> Reach {
        Reach {
            last_opened,
            ..opening()
        }
    }

    /// [`opening`], whose `opened_within` answers as `opened_within` does.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn opened(opened_within: fn(u64) -> Option<u64>) -> Reach {
        Reach {
            opened_within,
            ..opening()
        }
    }

    /// [`opening`], on a build whose grant the next build would lose (ad hoc, dev).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn unstable() -> Reach {
        Reach {
            grant_stable: || false,
            ..opening()
        }
    }

    /// [`opening`], whose `spctl developer-mode enable-terminal` fails.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn spctl_failing() -> Reach {
        Reach {
            list_terminal: || Err("spctl exited 1".to_string()),
            ..opening()
        }
    }

    /// [`opening`], but System Settings does not open (counted).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn refusing() -> Reach {
        Reach {
            open_pane: || {
                bump(&OPENS);
                false
            },
            ..opening()
        }
    }

    /// A panel that shows, with System Settings running and in front.
    pub(crate) fn showing() -> DevToolsHelperArm {
        DevToolsHelperArm {
            show: |_, _| true,
            settings_running: || true,
            settings_frontmost: || true,
            ..DevToolsHelperArm::inert()
        }
    }

    /// Stand a window in on a headless `App`: its cached reading pending (so the lane's
    /// own judges), a windowed lane with `reach` and [`showing`], and a reading that
    /// finds aterm not on a list of two.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn stand_in(app: &mut crate::App, reach: Reach) {
        fn no_probe(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
        }
        fn subject() -> Option<&'static str> {
            Some(SUBJECT)
        }
        app.consent.set_devtools_arms_for_test(no_probe, subject);
        app.devtools_helper = Lane::windowed_for_test(reach, showing());
        app.devtools_helper
            .note_reading(reading(DevToolGrant::NotGranted, absent(2)), Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::test_arms::{absent, on, unread};
    use super::*;

    /// The quiet moment both ways: every input alone keeps it from coming.
    #[test]
    fn the_quiet_moment_needs_every_input_and_each_alone_holds_it() {
        let settled = crate::consent_card::LAUNCH_SETTLE;
        assert!(quiet(true, false, true, settled, false), "the quiet moment");
        assert!(
            !quiet(false, false, true, settled, false),
            "aterm not focused"
        );
        assert!(!quiet(true, true, true, settled, false), "typing");
        assert!(
            !quiet(true, false, false, settled, false),
            "nobody there for five minutes"
        );
        assert!(
            !quiet(true, false, true, settled - Duration::from_secs(1), false),
            "a launch (an update's successor) not yet settled"
        );
        assert!(
            !quiet(true, false, true, settled, true),
            "a hand-off under way"
        );
        assert!(QUIET_FOR < PRESENT_WITHIN);
    }

    /// THE ORIGIN × GRANT × DECLINE TABLE. An owner's press opens the page whatever was
    /// read, declined or built (the panel unless aterm reads on; `spctl` only on a list
    /// read whole and empty); the window's own hand-over and the control verb act only on
    /// `not_granted`, never while a decline holds, never on an unstable build (`now`
    /// excepted), never twice in their window, and queue for the quiet moment (`now`:
    /// the panel only).
    #[test]
    fn the_origin_grant_and_decline_table() {
        let facts = |grant, list, declined, grant_stable, recent| Facts {
            grant,
            list,
            declined,
            grant_stable,
            recent,
        };
        let presses = [Origin::Settings, Origin::Digest];
        let asked = [
            Origin::Automatic,
            Origin::Ctl { now: false },
            Origin::Ctl { now: true },
        ];
        for declined in [false, true] {
            for grant_stable in [false, true] {
                for recent in [false, true] {
                    for origin in presses {
                        let at = |grant, list| {
                            admit(
                                origin,
                                Moment::Asked,
                                &facts(grant, list, declined, grant_stable, recent),
                            )
                        };
                        assert_eq!(at(DevToolGrant::Granted, on()), Admit::PaneOnly);
                        assert_eq!(
                            at(DevToolGrant::NotGranted, absent(2)),
                            Admit::Hand {
                                list_terminal: false
                            }
                        );
                        assert_eq!(
                            at(DevToolGrant::NotGranted, absent(0)),
                            Admit::Hand {
                                list_terminal: true
                            },
                            "an empty list: Terminal first, so the page exists"
                        );
                        assert_eq!(
                            at(DevToolGrant::Unknown, unread()),
                            Admit::Hand {
                                list_terminal: false
                            },
                            "unread: the panel's UNREAD variant, no spctl"
                        );
                    }
                    for origin in asked {
                        for moment in [Moment::Asked, Moment::Quiet] {
                            let at = |grant, list| {
                                admit(
                                    origin,
                                    moment,
                                    &facts(grant, list, declined, grant_stable, recent),
                                )
                            };
                            assert_eq!(
                                at(DevToolGrant::Granted, on()),
                                Admit::Refuse(Refusal::Granted)
                            );
                            assert_eq!(
                                at(DevToolGrant::Unknown, unread()),
                                Admit::Refuse(Refusal::Unknown),
                                "fail-closed"
                            );
                            let now = origin == Origin::Ctl { now: true };
                            let want = if declined {
                                Admit::Refuse(Refusal::Declined)
                            } else if !grant_stable && !now {
                                Admit::Refuse(Refusal::Unstable)
                            } else if recent {
                                Admit::Refuse(Refusal::Recent)
                            } else if now {
                                Admit::PanelOnly
                            } else if moment == Moment::Asked {
                                Admit::Queue
                            } else {
                                Admit::Hand {
                                    list_terminal: false,
                                }
                            };
                            assert_eq!(
                                at(DevToolGrant::NotGranted, absent(2)),
                                want,
                                "{origin:?} {moment:?} declined={declined} \
                                 stable={grant_stable} recent={recent}"
                            );
                        }
                    }
                }
            }
        }
        // A row read on while macOS still checks: nothing on the page to add, so the
        // window and an agent's ask leave the owner alone; `now`'s panel says so.
        for origin in [Origin::Automatic, Origin::Ctl { now: false }] {
            for moment in [Moment::Asked, Moment::Quiet] {
                assert_eq!(
                    admit(
                        origin,
                        moment,
                        &facts(DevToolGrant::NotGranted, on(), false, true, false)
                    ),
                    Admit::Refuse(Refusal::Listed),
                    "{origin:?} {moment:?}"
                );
            }
        }
        assert_eq!(
            admit(
                Origin::Ctl { now: true },
                Moment::Asked,
                &facts(DevToolGrant::NotGranted, on(), false, true, false)
            ),
            Admit::PanelOnly
        );
        // At the quiet moment, an empty list runs `spctl` first.
        assert_eq!(
            admit(
                Origin::Automatic,
                Moment::Quiet,
                &facts(DevToolGrant::NotGranted, absent(0), false, true, false)
            ),
            Admit::Hand {
                list_terminal: true
            }
        );
        // The windows each origin is held to.
        assert_eq!(Origin::Digest.reopen_after_s(), None);
        assert_eq!(Origin::Settings.reopen_after_s(), None);
        assert_eq!(
            Origin::Automatic.reopen_after_s(),
            Some(atpkg::devtools::REOPEN_AFTER_S)
        );
        assert_eq!(
            Origin::Ctl { now: true }.reopen_after_s(),
            Some(atpkg::devtools::REOPEN_NOW_AFTER_S)
        );
        assert!(Origin::Digest.owner_press() && Origin::Settings.owner_press());
        assert!(!Origin::Automatic.owner_press() && !Origin::Ctl { now: true }.owner_press());
    }

    /// The Settings card's age of the opened marker: whole minutes, a stamp a
    /// little ahead of the clock is "just now", one more than an hour ahead says
    /// nothing (atpkg's rule for the same marker).
    #[test]
    fn the_opened_marker_ages_in_whole_minutes_and_a_far_future_stamp_says_nothing() {
        let now = 1_791_381_501;
        assert_eq!(minutes_since(now, now), Some(0));
        assert_eq!(minutes_since(now - 59, now), Some(0));
        assert_eq!(minutes_since(now - 60, now), Some(1));
        assert_eq!(minutes_since(now - 3 * 86_400, now), Some(4_320));
        assert_eq!(
            minutes_since(now + 600, now),
            Some(0),
            "a clock a little behind"
        );
        assert_eq!(
            minutes_since(now + atpkg::devtools::STAMP_SKEW_S, now),
            Some(0)
        );
        assert_eq!(
            minutes_since(now + atpkg::devtools::STAMP_SKEW_S + 1, now),
            None,
            "written under a clock since corrected"
        );
    }

    fn show(list: DevToolList, bundle: Option<PathBuf>) -> Show {
        Show {
            origin: Origin::Digest,
            reading: test_arms::reading(DevToolGrant::NotGranted, list),
            listed: None,
            bundle,
            skips: false,
        }
    }

    fn has(content: &Content, words: &str) -> bool {
        content.texts().any(|(text, _)| text == words)
    }

    /// The words in each state and variant: why and how with the tile; the measured
    /// cost; the UNREAD variant's line and its buttons; a drop that did not take or did
    /// not land; done. "should" while D14 is unmeasured; every hint names the list;
    /// Terminal is spoken of only when this hand-over added it.
    #[test]
    fn the_panels_words_in_each_state_and_variant() {
        use atpkg::devtools::{HELPER_HOW, HELPER_TITLE, done, only_aterm, trade, why};
        let t0 = Instant::now();
        let bundle = PathBuf::from("/Applications/aterm.app");
        let mut panel = Panel::new(show(absent(2), Some(bundle.clone())), t0);
        let explain = panel.content();
        let texts: Vec<&str> = explain.texts().map(|(t, _)| t).collect();
        assert_eq!(texts[0], HELPER_TITLE);
        assert_eq!(texts[1], why(false));
        assert!(texts[1].contains("should"), "D14 unmeasured");
        assert_eq!(texts[2], checked_words(Some(3)).unwrap());
        assert!(has(&explain, HELPER_HOW) && has(&explain, only_aterm(false)));
        assert!(has(&explain, trade(false)));
        assert!(!has(&explain, UNREAD_WORDS) && !has(&explain, DROP_FAILED_WORDS));
        assert_eq!(
            explain.tile(),
            Some(&Tile {
                bundle: bundle.clone(),
                live: true
            })
        );
        assert_eq!(
            explain.buttons(),
            [Button::NotNow],
            "no Done before a grant"
        );
        assert_eq!(explain.blocks.last(), Some(&Block::Link(STATUS_LINK)));
        assert!(atpkg::devtools::HELPER_STEP.contains("Developer Tools list"));
        assert!(DROP_FAILED_WORDS.contains("Developer Tools list"));
        // No bundle: words only, + first — nothing to drag, so no drag is spoken of.
        let words_only = Panel::new(show(absent(2), None), t0).content();
        assert_eq!(words_only.tile(), None);
        assert!(has(&words_only, atpkg::devtools::HELPER_HOW_PLUS));
        assert!(!has(&words_only, HELPER_HOW));
        assert!(
            words_only
                .texts()
                .all(|(text, _)| !text.to_lowercase().contains("drag")),
            "{words_only:?}"
        );
        // Listed, switched off: the switch, no tile to drag.
        let off = test_arms::list(DevToolListing::Off, DevToolListLabel::Read, Some(2));
        let mut listed_off = Panel::new(show(off, Some(bundle.clone())), t0);
        let words = listed_off.content();
        assert!(has(&words, atpkg::devtools::HELPER_SWITCH_ON) && !has(&words, HELPER_HOW));
        assert_eq!(words.tile(), None, "aterm is already listed");
        assert!(listed_off.on_read(absent(2), t0).redraw, "it left the list");
        assert!(has(&listed_off.content(), HELPER_HOW));
        assert!(listed_off.content().tile().is_some());
        // Terminal only when this hand-over added it.
        let mut listed = show(absent(0), None);
        listed.listed = Some(Ok(()));
        assert!(has(&Panel::new(listed, t0).content(), only_aterm(true)));
        // The UNREAD variant: its line, and the owner's two extra buttons.
        let unread_panel = Panel::new(show(unread(), None), t0);
        assert!(!unread_panel.readable());
        let words = unread_panel.content();
        assert!(has(&words, UNREAD_WORDS));
        // Full Disk Access only when that is why: a probe switched off says so.
        let probe_off = Panel::new(
            show(
                DevToolList::refused(DevToolListLabel::RefusedDisabled),
                None,
            ),
            t0,
        );
        assert!(!probe_off.readable());
        let off_words = probe_off.content();
        assert!(!has(&off_words, UNREAD_WORDS));
        assert!(off_words.texts().any(|(text, tone)| tone == Tone::Notice
            && text.contains(atpkg::devtools::list_reason(
                DevToolListLabel::RefusedDisabled
            ))
            && !text.contains("Full Disk Access")));
        assert_eq!(off_words.buttons(), words.buttons());
        // A list not read YET is not unreadable: the panel reads it.
        let pending = Panel::new(show(DevToolList::pending(), None), t0);
        assert!(pending.readable() && pending.content().buttons() == [Button::NotNow]);
        assert_eq!(
            words.buttons(),
            [Button::ShowList, Button::SwitchedOn, Button::NotNow]
        );
        assert_eq!(
            words
                .buttons()
                .iter()
                .map(|b| b.label())
                .collect::<Vec<_>>(),
            [
                "No Developer Tools list? Show it",
                "I've switched it on",
                "Not now"
            ]
        );
        // A drag that did not take: the + route; the tile stays live.
        assert!(
            panel
                .on(PanelEvent::DragEnded { accepted: false }, t0)
                .redraw
        );
        assert_eq!(panel.phase(), Phase::DropFailed);
        assert!(has(&panel.content(), DROP_FAILED_WORDS));
        assert!(panel.content().tile().is_some_and(|t| t.live));
        // An accepted drop: the tile is spent; a whole read 15 s on that still
        // lacks aterm says it may have landed elsewhere — a read sooner does not,
        // and neither does time alone (no read while System Settings is not in
        // front: the owner may be at macOS's password prompt).
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0);
        assert_eq!(panel.phase(), Phase::Explain);
        assert!(panel.content().tile().is_some_and(|t| !t.live), "one drop");
        // A spent tile is never offered to drag again: its hint says it was
        // dropped, and the how leads with the + route.
        let spent = panel.content();
        assert_eq!(spent.tile().map(Tile::hint), Some(TILE_SPENT_HINT));
        assert_eq!(
            explain.tile().map(Tile::hint),
            Some(atpkg::devtools::HELPER_STEP)
        );
        assert!(!has(&spent, HELPER_HOW) && has(&spent, &dropped_how()));
        assert!(!dropped_how().to_lowercase().contains("drag"));
        assert_eq!(
            panel.on(PanelEvent::DragEnded { accepted: true }, t0),
            Effect::default(),
            "a second drag changes nothing"
        );
        assert!(
            !panel
                .on_read(absent(2), t0 + DROP_CONFIRM_WITHIN / 2)
                .redraw
        );
        assert_eq!(panel.turn(t0 + DROP_CONFIRM_WITHIN * 2), Effect::default());
        assert_eq!(panel.phase(), Phase::Explain, "time alone judges nothing");
        assert!(panel.on_read(absent(2), t0 + DROP_CONFIRM_WITHIN).redraw);
        assert_eq!(panel.phase(), Phase::DroppedUnconfirmed);
        let unconfirmed = panel.content();
        assert!(has(&unconfirmed, DROP_UNCONFIRMED_WORDS));
        assert!(!has(&unconfirmed, HELPER_HOW) && has(&unconfirmed, &dropped_how()));
        // A drop that landed as a row switched off DID reach this list: the
        // switch, never "did not reach" beside it.
        let mut landed_off = Panel::new(show(absent(2), Some(bundle.clone())), t0);
        let _ = landed_off.on(PanelEvent::DragEnded { accepted: true }, t0);
        let _ = landed_off.on_read(off, t0 + DROP_CONFIRM_WITHIN * 2);
        let words = landed_off.content();
        assert_eq!(landed_off.phase(), Phase::Explain);
        assert!(has(&words, atpkg::devtools::HELPER_SWITCH_ON));
        assert!(!has(&words, DROP_UNCONFIRMED_WORDS));
        // …and an unconfirmed drop the list later shows switched off is here too.
        let mut late = panel.clone();
        let _ = late.on_read(off, t0 + DROP_CONFIRM_WITHIN * 3);
        assert_eq!(late.phase(), Phase::Explain);
        assert!(!has(&late.content(), DROP_UNCONFIRMED_WORDS));
        // Done: the done words, no buttons.
        let effect = panel.on_read(on(), t0 + DROP_CONFIRM_WITHIN);
        assert!(effect.granted && effect.redraw);
        assert_eq!(panel.phase(), Phase::Done);
        let done_words = panel.content();
        assert_eq!(
            done_words.texts().collect::<Vec<_>>(),
            [(HELPER_TITLE, Tone::Title), (done(false), Tone::Done)]
        );
        assert!(done_words.buttons().is_empty());
    }

    /// EVERY WORD THE PANEL CAN SAY passes the one fence (`test_arms::ruled_out`: the
    /// owner's restart phrases and the design's own bans): every state and variant's
    /// content — each list it can be shown on, with and without a tile, after no
    /// `spctl` run, an Ok one and a failed one, D14 either way, then a drop that did
    /// not take, one taken, one taken outside System Settings, each read on, and done —
    /// and the panel's own words directly: its button labels, the unread line for every
    /// reason, the drop and failure notices, the + route after a drop, the checked
    /// counts and the status link. NEGATIVE CONTROL: the fence catches a restart word,
    /// a phrase and a stem at a word's start, and lets "discover" through.
    #[test]
    fn every_word_the_panel_can_say_passes_the_fence() {
        use test_arms::ruled_out;
        let t0 = Instant::now();
        let off = test_arms::list(DevToolListing::Off, DevToolListLabel::Read, Some(2));
        let labels = [
            DevToolListLabel::Pending,
            DevToolListLabel::Read,
            DevToolListLabel::NoFullDiskAccess,
            DevToolListLabel::OpenFailed,
            DevToolListLabel::ToolFailed,
            DevToolListLabel::FormatUnrecognized,
            DevToolListLabel::TimedOut,
            DevToolListLabel::SpawnFailed,
            DevToolListLabel::OutputUnreadable,
            DevToolListLabel::RefusedDisabled,
            DevToolListLabel::RefusedObserverOff,
            DevToolListLabel::RefusedOutOfBundle,
            DevToolListLabel::RefusedNoExe,
            DevToolListLabel::RefusedNoSubject,
            DevToolListLabel::UnsupportedPlatform,
        ];
        let mut lists = vec![absent(2), absent(0), off, on()];
        lists.extend(labels.iter().map(|label| DevToolList::refused(*label)));
        let mut said: Vec<String> = Vec::new();
        let mut take = |content: Content| {
            said.extend(content.texts().map(|(text, _)| text.to_string()));
        };
        for skips in [false, true] {
            for list in &lists {
                for bundle in [Some(PathBuf::from("/Applications/aterm.app")), None] {
                    for listed in [None, Some(Ok(())), Some(Err("spctl exited 1".to_string()))] {
                        let mut shown = show(*list, bundle.clone());
                        shown.skips = skips;
                        shown.listed = listed;
                        let panel = Panel::new(shown, t0);
                        take(panel.content());
                        for event in [
                            PanelEvent::DragEnded { accepted: false },
                            PanelEvent::DragEnded { accepted: true },
                            PanelEvent::DroppedOutside,
                        ] {
                            let mut moved = panel.clone();
                            let _ = moved.on(event, t0);
                            take(moved.content());
                            let _ = moved.on_read(absent(2), t0 + DROP_CONFIRM_WITHIN);
                            take(moved.content());
                        }
                        let mut done = panel.clone();
                        let _ = done.granted(t0);
                        take(done.content());
                    }
                }
            }
        }
        said.extend(Button::ALL.iter().map(|button| button.label().to_string()));
        said.extend(labels.iter().map(|label| unread_words(*label).into_owned()));
        said.extend(
            [
                DROP_FAILED_WORDS,
                DROP_UNCONFIRMED_WORDS,
                DROP_OUTSIDE_WORDS,
                TILE_SPENT_HINT,
                TILE_CAPTION,
                STATUS_LINK,
            ]
            .map(str::to_string),
        );
        said.push(dropped_how());
        said.push(show_list_failed_words("spctl exited 1"));
        said.extend([Some(1), Some(3)].into_iter().filter_map(checked_words));
        for text in &said {
            assert_eq!(ruled_out(text), None, "{text:?}");
        }
        // NEGATIVE CONTROL: the fence itself.
        assert_eq!(ruled_out("Quit and Reopen aterm"), Some("reopen"));
        assert_eq!(ruled_out("Switch it off whenever you like."), None);
        assert_eq!(ruled_out("The switch can stay off."), Some("can stay off"));
        assert_eq!(ruled_out("This covers every tool."), Some("cover"));
        assert_eq!(
            ruled_out("aterm can't discover which copy; it recovers."),
            None
        );
    }

    /// A HAND-OVER WHOSE `spctl` RUN FAILED (a list read whole and empty, so macOS may
    /// show no Developer Tools page): the panel says macOS did not show the list and
    /// why, leads with the + route, and offers Show it to run `spctl` again — on a
    /// readable list too, not only the UNREAD variant. A Show it that then answers Ok
    /// takes its button away and says Terminal is aterm's doing. An empty list nobody
    /// ran `spctl` for offers Show it as well. NEGATIVE CONTROL: a run that answered Ok
    /// offers no Show it, and a list with rows offers none.
    #[test]
    fn a_failed_spctl_run_says_why_and_offers_show_it_again() {
        use atpkg::devtools::only_aterm;
        let t0 = Instant::now();
        let mut failed = show(absent(0), None);
        failed.listed = Some(Err("spctl exited 1".to_string()));
        let mut panel = Panel::new(failed, t0);
        assert!(
            panel.readable(),
            "the list was read: not the UNREAD variant"
        );
        let words = panel.content();
        assert!(
            has(&words, &show_list_failed_words("spctl exited 1")),
            "{words:?}"
        );
        assert_eq!(words.buttons(), [Button::ShowList, Button::NotNow]);
        assert!(has(&words, only_aterm(false)), "nothing was listed");
        let asked = panel.on(Button::ShowList.event(), t0);
        assert!(asked.list_terminal && asked.redraw);
        assert!(!has(
            &panel.content(),
            &show_list_failed_words("spctl exited 1")
        ));
        let _ = panel.on_listed(&Ok(()));
        let listed = panel.content();
        assert_eq!(listed.buttons(), [Button::NotNow]);
        assert!(has(&listed, only_aterm(true)));
        // An empty list nobody ran `spctl` for: Show it.
        let unrun = Panel::new(show(absent(0), None), t0).content();
        assert_eq!(unrun.buttons(), [Button::ShowList, Button::NotNow]);
        // NEGATIVE CONTROLS: a run that answered Ok, and a list with rows.
        let mut ran = show(absent(0), None);
        ran.listed = Some(Ok(()));
        assert_eq!(Panel::new(ran, t0).content().buttons(), [Button::NotNow]);
        assert_eq!(
            Panel::new(show(absent(2), None), t0).content().buttons(),
            [Button::NotNow]
        );
        // A read that finds rows (Terminal, or aterm, added) takes Show it away.
        let mut emptied = Panel::new(show(absent(0), None), t0);
        assert!(emptied.on_read(absent(1), t0 + PANEL_TICK).redraw);
        assert_eq!(emptied.content().buttons(), [Button::NotNow]);
    }

    /// THE PANEL'S STATE MACHINE: Not now declines (but not once done); "I've switched
    /// it on" closes and rechecks; Show it asks for `spctl` and its failure says the +
    /// route; a read that cannot read the list turns UNREAD; done closes itself after
    /// [`DONE_FOR`]; ten minutes is the bound. Reads: only a readable list, one at a
    /// time, once per [`PANEL_TICK`], sooner after a drop.
    #[test]
    fn the_panels_state_machine() {
        let t0 = Instant::now();
        let mut panel = Panel::new(show(absent(2), None), t0);
        let not_now = panel.clone().on(Button::NotNow.event(), t0);
        assert_eq!(not_now.close, Some(Closed::NotNow));
        assert!(Closed::NotNow.declines());
        for closed in [
            Closed::SwitchedOn,
            Closed::Done,
            Closed::Lapsed,
            Closed::SettingsGone,
            Closed::Replaced,
        ] {
            assert!(!closed.declines(), "{closed:?}");
        }
        // The live panel's buttons carry their tag; one action reads it back.
        for button in Button::ALL {
            assert_eq!(Button::from_tag(button.tag()), Some(button));
        }
        assert_eq!(Button::from_tag(-1), None, "the status link's tag");
        assert_eq!(Button::from_tag(Button::ALL.len() as isize), None);
        let switched = panel.clone().on(PanelEvent::SwitchedOn, t0);
        assert_eq!(switched.close, Some(Closed::SwitchedOn));
        assert!(switched.recheck);
        assert!(panel.clone().on(PanelEvent::Status, t0).open_status);
        // Reads.
        assert!(!panel.wants_read(t0), "the show's own reading is fresh");
        assert!(panel.wants_read(t0 + PANEL_TICK));
        panel.read_out = true;
        assert!(!panel.wants_read(t0 + PANEL_TICK), "one at a time");
        let _ = panel.on_read(absent(2), t0 + PANEL_TICK);
        assert!(!panel.wants_read(t0 + PANEL_TICK));
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0 + PANEL_TICK);
        assert!(panel.wants_read(t0 + PANEL_TICK), "a drop reads at once");
        // A read that failed on its way says nothing lasting: still readable,
        // and the next turn reads again.
        for transient in [
            DevToolListLabel::TimedOut,
            DevToolListLabel::ToolFailed,
            DevToolListLabel::SpawnFailed,
            DevToolListLabel::OutputUnreadable,
            DevToolListLabel::OpenFailed,
            DevToolListLabel::Pending,
        ] {
            let mut tried = panel.clone();
            tried.read_out = true;
            let effect = tried.on_read(DevToolList::refused(transient), t0 + PANEL_TICK * 2);
            assert!(!effect.redraw && tried.readable(), "{transient:?}");
            assert!(tried.wants_read(t0 + PANEL_TICK * 3), "{transient:?}");
        }
        // …until READ_FAILURES_LASTING in a row: then it is taken as lasting
        // (sqlite3 is not started every tick for ten minutes over a damaged
        // file), and a whole read between failures starts the count again.
        let failed = DevToolList::refused(DevToolListLabel::ToolFailed);
        let mut failing = panel.clone();
        for n in 1..READ_FAILURES_LASTING {
            let _ = failing.on_read(failed, t0 + PANEL_TICK * (1 + n));
        }
        let _ = failing.on_read(absent(2), t0 + PANEL_TICK * 9);
        for n in 1..READ_FAILURES_LASTING {
            let _ = failing.on_read(failed, t0 + PANEL_TICK * (9 + n));
            assert!(failing.readable(), "{n} failed read(s) in a row");
        }
        let effect = failing.on_read(failed, t0 + PANEL_TICK * 20);
        assert!(effect.redraw && !failing.readable());
        assert!(!failing.wants_read(t0 + PANEL_TICK * 30), "no more reads");
        assert!(
            failing
                .content()
                .texts()
                .any(|(text, tone)| tone == Tone::Notice
                    && text.contains(atpkg::devtools::list_reason(DevToolListLabel::ToolFailed)))
        );
        // A read that cannot read the list: UNREAD, no more reads.
        let effect = panel.on_read(unread(), t0 + PANEL_TICK * 2);
        assert!(effect.redraw && !panel.readable());
        assert!(!panel.wants_read(t0 + PANEL_TICK * 9));
        assert_eq!(
            panel.turn(t0 + DROP_CONFIRM_WITHIN * 2),
            Effect::default(),
            "an unread list confirms no drop"
        );
        // Show it.
        let asked = panel.on(PanelEvent::ShowList, t0);
        assert!(asked.list_terminal);
        let _ = panel.on_listed(&Err("spctl exited 1".to_string()));
        assert!(
            panel
                .content()
                .texts()
                .any(|(t, tone)| tone == Tone::Notice && t.contains("press + below the list"))
        );
        let _ = panel.on_listed(&Ok(()));
        assert!(has(&panel.content(), atpkg::devtools::only_aterm(true)));
        // Granted: done, closes itself after DONE_FOR; Not now then is no decline.
        let t1 = t0 + Duration::from_secs(30);
        assert!(panel.granted(t1).granted);
        assert_eq!(panel.granted(t1), Effect::default(), "once");
        assert_eq!(panel.next_turn(), t1 + DONE_FOR);
        assert_eq!(panel.turn(t1), Effect::default());
        assert_eq!(panel.turn(t1 + DONE_FOR).close, Some(Closed::Done));
        assert_eq!(
            panel.clone().on(PanelEvent::NotNow, t1).close,
            Some(Closed::Done),
            "closing the done words is no decline"
        );
        // The ten-minute bound.
        let mut idle = Panel::new(show(absent(2), None), t0);
        assert_eq!(idle.next_turn(), t0 + PANEL_FOR);
        assert_eq!(idle.turn(t0 + PANEL_FOR).close, Some(Closed::Lapsed));
    }

    /// When the stand-in row below last changed (unix seconds).
    const ROW_CHANGED_AT: i64 = 1_791_000_000;

    /// aterm's row read whole, switched on, last changed at `changed_at`.
    fn on_since(changed_at: i64) -> DevToolList {
        DevToolList {
            modified_at_s: Some(changed_at),
            ..on()
        }
    }

    /// tccd's answer that it did not allow a new program aterm started, a minute
    /// after the stand-in row last changed.
    fn denied_after_the_row() -> DevToolProbe {
        DevToolProbe {
            state: aterm_containment::DevToolState::Denied,
            requests: Some(1),
            not_allowed: Some(1),
            answered_at_ms: Some(ROW_CHANGED_AT * 1_000 + 60_000),
            ..DevToolProbe::refused(aterm_containment::DevToolProbeLabel::Observed)
        }
    }

    /// The window's reading of `list` joined with tccd's denial after the row
    /// changed, as `consent::devtool_grant` joins it.
    fn joined(list: DevToolList) -> Reading {
        let log = denied_after_the_row();
        Reading {
            grant: devtool_grant(&list, &log),
            list,
            log,
            subject: Some(test_arms::SUBJECT),
        }
    }

    /// LISTED AND ON WHILE macOS STILL CHECKS (`Step::ListedOn`: the row reads on,
    /// tccd answered after it last changed, the joined verdict is `not_granted`): the
    /// panel says there is nothing to add — no tile, no drag — and reading the row it
    /// opened on again is no grant: no done words, nothing for the window to end the
    /// digest line on. Only a read that is new evidence by the window's own rule (the
    /// row changed after tccd's answer: the owner switched it) is done. NEGATIVE
    /// CONTROL: a panel opened on aterm absent, whose read then finds the row the
    /// owner just added, is done.
    #[test]
    fn a_row_already_on_while_macos_still_checks_is_never_done_until_it_changes() {
        use atpkg::devtools::{HELPER_HOW, HELPER_LISTED_ON};
        let t0 = Instant::now();
        let reading = joined(on_since(ROW_CHANGED_AT));
        assert_eq!(reading.grant, DevToolGrant::NotGranted, "ListedOn");
        let bundle = Some(PathBuf::from("/Applications/aterm.app"));
        let mut panel = Panel::new(
            Show {
                origin: Origin::Digest,
                reading,
                listed: None,
                bundle: bundle.clone(),
                skips: false,
            },
            t0,
        );
        let words = panel.content();
        assert!(has(&words, HELPER_LISTED_ON) && !has(&words, HELPER_HOW));
        assert_eq!(words.tile(), None, "aterm is already listed");
        assert!(words.texts().all(|(text, _)| !text.contains("Drag")));
        // The row it opened on, read again: nothing new.
        let again = panel.on_read(on_since(ROW_CHANGED_AT), t0 + PANEL_TICK);
        assert!(!again.granted && !again.redraw);
        assert_eq!(panel.phase(), Phase::Explain);
        // The owner switched it off and on: changed after tccd's answer, done.
        let toggled = on_since(ROW_CHANGED_AT + 3_600);
        let effect = panel.on_read(toggled, t0 + PANEL_TICK * 2);
        assert!(effect.granted && panel.phase() == Phase::Done);
        // A newer tccd answer the window read while the panel is up holds a
        // toggled row to it too.
        let mut held = Panel::new(
            Show {
                origin: Origin::Digest,
                reading,
                listed: None,
                bundle: bundle.clone(),
                skips: false,
            },
            t0,
        );
        let newer = DevToolProbe {
            answered_at_ms: Some((ROW_CHANGED_AT + 7_200) * 1_000),
            ..denied_after_the_row()
        };
        let _ = held.note_reading(
            &Reading {
                log: newer,
                ..reading
            },
            t0,
        );
        assert!(!held.on_read(toggled, t0 + PANEL_TICK).granted);
        // The row left the list: the add words and the tile come back, and the
        // row the owner then adds is done.
        assert!(held.on_read(absent(2), t0 + PANEL_TICK * 2).redraw);
        assert!(has(&held.content(), HELPER_HOW) && held.content().tile().is_some());
        // NEGATIVE CONTROL: opened on aterm absent; the owner adds it.
        let mut adding = Panel::new(
            Show {
                origin: Origin::Digest,
                reading: joined(absent(2)),
                listed: None,
                bundle,
                skips: false,
            },
            t0,
        );
        assert!(has(&adding.content(), HELPER_HOW));
        let added = on_since(ROW_CHANGED_AT + 600);
        assert!(adding.on_read(added, t0 + PANEL_TICK).granted);
        assert_eq!(adding.phase(), Phase::Done);
    }

    /// THE LANE'S READING FOLLOWS THE WINDOW'S RULE: the panel's list read of a row
    /// that still reads on, unchanged since tccd's newer answer, leaves the lane's
    /// verdict `not_granted` — so the window ends no digest line on it and a waiting
    /// hand-over keeps waiting — while a row that changed after that answer is
    /// granted. NEGATIVE CONTROL: with no tccd answer to join, a row read on decides.
    #[test]
    fn the_panels_read_of_an_unchanged_row_on_grants_nothing() {
        let t0 = Instant::now();
        let reach = Reach {
            read_list: |_, _| on_since(ROW_CHANGED_AT),
            ..test_arms::opening()
        };
        let mut lane = Lane::windowed_for_test(reach, test_arm());
        let wake = nudge();
        let reading = joined(on_since(ROW_CHANGED_AT));
        lane.note_reading(reading, t0);
        lane.queue(Origin::Automatic, t0);
        assert!(lane.show(
            Show {
                origin: Origin::Digest,
                reading,
                listed: None,
                bundle: None,
                skips: false,
            },
            t0,
            Arc::clone(&wake),
        ));
        let t1 = t0 + PANEL_TICK;
        let _ = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
        assert!(lane.panel().is_some_and(|p| p.read_out), "one read out");
        let mut asks = Asks::default();
        until(|| {
            asks = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
            lane.panel().is_some_and(|p| !p.read_out)
        });
        assert!(!asks.granted, "nothing ends the digest line");
        assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Explain));
        assert_eq!(
            lane.reading().map(|r| r.grant),
            Some(DevToolGrant::NotGranted)
        );
        assert_eq!(lane.queued(), Some(Origin::Automatic), "still waiting");
        // The row changed after tccd's answer: granted.
        assert_eq!(
            reading.with_list(on_since(ROW_CHANGED_AT + 600)).grant,
            DevToolGrant::Granted
        );
        // NEGATIVE CONTROL: nothing to join, the row decides.
        assert_eq!(
            Reading::UNREAD.with_list(on_since(ROW_CHANGED_AT)).grant,
            DevToolGrant::Granted
        );
    }

    /// A PANEL SHOWN BEFORE ANY READING NAMED ITS SUBJECT (an owner's press while the
    /// window's first reading was still out: `Reading::UNREAD`) reads nothing and has
    /// no icon — until the window's reading lands, which gives it the subject: its
    /// list reads start, the icon to drag appears, and the how leads with the drag
    /// again. NEGATIVE CONTROL: a reading for a copy aterm does not offer (no bundle)
    /// starts the reads but leaves + leading.
    #[test]
    fn a_panel_shown_before_the_first_reading_learns_its_subject_from_it() {
        use atpkg::devtools::{HELPER_HOW, HELPER_HOW_PLUS};
        for (offered, how) in [(true, HELPER_HOW), (false, HELPER_HOW_PLUS)] {
            let reach = if offered {
                Reach {
                    drag_bundle: |subject| {
                        subject.map(|_| PathBuf::from("/Applications/aterm.app"))
                    },
                    ..test_arms::opening()
                }
            } else {
                test_arms::opening()
            };
            let mut lane = Lane::windowed_for_test(reach, test_arm());
            let wake = nudge();
            let t0 = Instant::now();
            assert!(lane.show(
                Show {
                    origin: Origin::Settings,
                    reading: Reading::UNREAD,
                    listed: None,
                    bundle: None,
                    skips: false,
                },
                t0,
                Arc::clone(&wake),
            ));
            let words = lane.panel().map(Panel::content).expect("shown");
            assert_eq!(words.tile(), None);
            assert!(has(&words, HELPER_HOW_PLUS), "no icon: + leads");
            let _ = lane.tick_panel(t0 + PANEL_TICK, Some(ProbeGate::on()), &wake);
            assert!(
                lane.panel().is_some_and(|p| !p.read_out),
                "no subject: no read"
            );
            lane.note_reading(
                test_arms::reading(DevToolGrant::NotGranted, absent(2)),
                t0 + PANEL_TICK,
            );
            let panel = lane.panel().expect("still up");
            assert!(!panel.wants_subject());
            let words = panel.content();
            assert_eq!(words.tile().is_some(), offered, "offered={offered}");
            assert!(has(&words, how), "offered={offered}: {words:?}");
            let _ = lane.tick_panel(t0 + PANEL_TICK * 2, Some(ProbeGate::on()), &wake);
            assert!(
                lane.panel().is_some_and(|p| p.read_out),
                "the subject is known: the list is read"
            );
        }
    }

    /// A measurer that sets every character 6 pt wide on 16 pt lines.
    struct Fixed;

    impl Measure for Fixed {
        fn text_height(&self, text: &str, _tone: Tone, width: f64) -> f64 {
            let per_line = (width / 6.0).floor().max(1.0);
            let lines = (text.chars().count() as f64 / per_line).ceil().max(1.0);
            lines * 16.0
        }

        fn button_width(&self, title: &str) -> f64 {
            title.chars().count() as f64 * 7.0 + 24.0
        }
    }

    /// THE LAYOUT: every part inside the panel's margins, top to bottom in block order,
    /// none overlapping; the icon beside its caption, hint and path; the buttons right-
    /// aligned in one row when they fit, wrapping when they do not; the height is the
    /// last part's bottom plus the margin.
    #[test]
    fn the_layout_stacks_the_words_and_wraps_the_buttons() {
        let t0 = Instant::now();
        let bundle = Some(PathBuf::from("/Applications/aterm.app"));
        for content in [
            Panel::new(show(absent(2), bundle.clone()), t0).content(),
            Panel::new(show(unread(), bundle), t0).content(),
        ] {
            let layout = layout(&content, &Fixed);
            assert_eq!(layout.w, PANEL_W);
            let frame = Rect {
                x: 0.0,
                y: 0.0,
                w: layout.w,
                h: layout.h,
            };
            for (i, (part, a)) in layout.parts.iter().enumerate() {
                assert!(a.w > 0.0 && a.h > 0.0, "{part:?}");
                assert!(
                    a.x >= INSET - 1e-9 && a.max_x() <= PANEL_W - INSET + 1e-9,
                    "{part:?} {a:?}"
                );
                assert!(frame.holds(*a), "{part:?}");
                for (other, b) in &layout.parts[i + 1..] {
                    assert_eq!(a.overlap(*b), 0.0, "{part:?} overlaps {other:?}");
                }
            }
            let bottom = layout
                .parts
                .iter()
                .map(|(_, r)| r.max_y())
                .fold(0.0, f64::max);
            assert_eq!(layout.h, bottom + INSET);
            let at = |want: Part| {
                layout
                    .parts
                    .iter()
                    .find(|(part, _)| *part == want)
                    .map(|(_, r)| *r)
                    .unwrap()
            };
            let icon = at(Part::Icon);
            assert_eq!((icon.w, icon.h), (ICON, ICON));
            for beside in [Part::Caption, Part::Hint, Part::Path] {
                assert!(at(beside).x > icon.max_x(), "{beside:?} right of the icon");
            }
            // Text blocks keep their order down the panel.
            let texts: Vec<Rect> = layout
                .parts
                .iter()
                .filter(|(part, _)| matches!(part, Part::Text(_)))
                .map(|(_, r)| *r)
                .collect();
            assert!(texts.windows(2).all(|w| w[0].max_y() <= w[1].y));
            let buttons: Vec<Rect> = layout
                .parts
                .iter()
                .filter(|(part, _)| matches!(part, Part::Button(_)))
                .map(|(_, r)| *r)
                .collect();
            assert_eq!(buttons.len(), content.buttons().len());
            assert!(
                buttons
                    .iter()
                    .any(|b| (b.max_x() - (PANEL_W - INSET)).abs() < 1e-9),
                "right-aligned"
            );
            let rows: std::collections::BTreeSet<u64> =
                buttons.iter().map(|b| b.y.to_bits()).collect();
            if content.buttons().len() == 1 {
                assert_eq!(rows.len(), 1);
            } else {
                assert!(rows.len() > 1, "three wide buttons wrap at 360 pt");
            }
        }
    }

    /// PLACEMENT: right of System Settings when it fits, else left, else above, else
    /// below; when nothing fits, over the sidebar strip only — never over the right
    /// pane; on the screen Settings is on; the main screen's corner when it was not
    /// found. And Quartz's y-down rectangles turned the right way up.
    #[test]
    fn the_panel_goes_beside_system_settings_and_never_over_its_list() {
        let (w, h) = (PANEL_W, 400.0);
        let main = Rect {
            x: 0.0,
            y: 0.0,
            w: 1728.0,
            h: 1080.0,
        };
        let inside = |r: Rect, screen: Rect| screen.holds(r);
        // Room on the right.
        let s = Rect {
            x: 200.0,
            y: 300.0,
            w: 800.0,
            h: 600.0,
        };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!(r.x, s.max_x() + PLACE_GAP);
        assert_eq!(r.max_y(), s.max_y(), "tops aligned");
        assert!(inside(r, main));
        // No room right: left.
        let s = Rect { x: 500.0, ..s };
        let s = Rect {
            w: main.max_x() - 500.0,
            ..s
        };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!(r.max_x(), s.x - PLACE_GAP);
        assert!(inside(r, main));
        // Wide and short: above, left edges aligned.
        let s = Rect {
            x: 0.0,
            y: 100.0,
            w: main.w,
            h: 400.0,
        };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!((r.x, r.y), (s.x, s.max_y() + PLACE_GAP));
        assert!(inside(r, main));
        // No room above: below.
        let s = Rect { y: 500.0, ..s };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!(r.max_y(), s.y - PLACE_GAP);
        // Nothing fits (less than a panel's width left of it): over the sidebar
        // strip, never the right pane.
        let s = Rect {
            x: 200.0,
            y: 0.0,
            w: main.w - 200.0,
            h: main.h,
        };
        let r = place(w, h, Some(s), &[main]);
        assert!(inside(r, main));
        assert!(r.max_x() <= s.x + SETTINGS_SIDEBAR_W, "{r:?}");
        assert_eq!(r.y, s.y + PLACE_GAP, "the sidebar's lower part");
        // On Settings' own screen, not the main one.
        let second = Rect {
            x: main.max_x(),
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let s = Rect {
            x: second.x + 100.0,
            y: 200.0,
            w: 800.0,
            h: 600.0,
        };
        let r = place(w, h, Some(s), &[main, second]);
        assert!(inside(r, second));
        // Not found: the main screen's upper-right corner.
        let r = place(w, h, None, &[main, second]);
        assert!(inside(r, main));
        assert_eq!(
            (r.max_x(), r.max_y()),
            (main.max_x() - PLACE_GAP, main.max_y() - PLACE_GAP)
        );
        // Quartz → Cocoa: a window 100 pt from the top of a 1080 pt screen.
        let cg = Rect {
            x: 10.0,
            y: 100.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(
            cg_to_cocoa(cg, 1080.0),
            Rect {
                x: 10.0,
                y: 380.0,
                w: 800.0,
                h: 600.0
            }
        );
    }

    /// VISIBILITY: the panel comes in front only while System Settings is the
    /// frontmost app — so the retry that waits for Settings' window, firing after
    /// the owner brought another app forward, orders nothing in — and never over
    /// itself or under a running drag.
    #[test]
    fn a_reveal_comes_in_front_only_while_system_settings_is() {
        assert!(
            may_reveal(true, false, false),
            "Settings in front, panel out"
        );
        assert!(
            !may_reveal(false, false, false),
            "a retry after another app came forward"
        );
        assert!(!may_reveal(true, true, false), "already in front");
        assert!(!may_reveal(true, false, true), "its tile's drag runs");
    }

    /// THE PLACEMENT TURN: a panel up beside System Settings stays beside it; System
    /// Settings still the active app with that window off the screen (minimized — no
    /// activation fires) orders it out instead of sending it to a screen corner over
    /// other apps' work, and the turn that finds the window back returns it. A window
    /// never found this show keeps the corner (O27), and a panel out because another
    /// app is in front is the activation observer's, not this turn's.
    #[test]
    fn a_minimized_system_settings_hides_the_panel_until_its_window_returns() {
        let window = Rect {
            x: 200.0,
            y: 300.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(
            place_turn(true, true, true, Some(window)),
            PlaceTurn::Place(Some(window))
        );
        assert_eq!(
            place_turn(true, true, true, None),
            PlaceTurn::Hide,
            "minimized"
        );
        assert_eq!(
            place_turn(false, true, true, Some(window)),
            PlaceTurn::Return(window),
            "un-minimized"
        );
        assert_eq!(place_turn(false, true, true, None), PlaceTurn::Nothing);
        // Never found this show: the corner, as before.
        assert_eq!(place_turn(true, true, false, None), PlaceTurn::Place(None));
        // Another app in front: left to the activation observer.
        assert_eq!(
            place_turn(false, false, true, Some(window)),
            PlaceTurn::Nothing
        );
    }

    /// A DROP SOMETHING TOOK OUTSIDE SYSTEM SETTINGS' WINDOW (a Finder window, the
    /// Desktop) is not the list's: the icon stays offered and the panel says where it
    /// landed and that a copy Finder made can go to the Trash; a drag after it still
    /// counts. Inside the window, or with no window read, a taken drop is the list's.
    /// The unconfirmed words name the Finder copy too.
    #[test]
    fn a_drop_taken_outside_system_settings_spends_nothing() {
        let window = Rect {
            x: 200.0,
            y: 300.0,
            w: 800.0,
            h: 600.0,
        };
        let landed = |x, y| drop_landed(Some(window), x, y, 64.0, 64.0);
        assert!(landed(500.0, 500.0), "on the list's window");
        assert!(landed(window.x, window.max_y()), "its edge");
        assert!(!landed(window.max_x() + 6.0, 500.0), "the gap");
        assert!(!landed(500.0, 100.0), "below it");
        // The image's lower-left corner, let go just inside the window's bottom edge.
        assert!(
            landed(500.0, window.y - 40.0),
            "the image's corner below the edge"
        );
        assert!(
            landed(window.x - 40.0, 500.0),
            "the image's corner left of it"
        );
        assert!(!landed(window.x - 100.0, 500.0), "more than an image away");
        assert!(
            drop_landed(None, 0.0, 0.0, 64.0, 64.0),
            "no window read: the list's"
        );
        let t0 = Instant::now();
        let bundle = PathBuf::from("/Applications/aterm.app");
        let mut panel = Panel::new(show(absent(2), Some(bundle)), t0);
        assert!(panel.on(PanelEvent::DroppedOutside, t0).redraw);
        assert_eq!(panel.phase(), Phase::DroppedOutside);
        let words = panel.content();
        assert!(has(&words, DROP_OUTSIDE_WORDS));
        assert!(words.tile().is_some_and(|tile| tile.live), "not spent");
        assert!(has(&words, atpkg::devtools::HELPER_HOW), "drag or +, again");
        // The next drag the list takes counts.
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0);
        assert!(panel.content().tile().is_some_and(|tile| !tile.live));
        assert_eq!(panel.on(PanelEvent::DroppedOutside, t0), Effect::default());
        assert!(
            DROP_UNCONFIRMED_WORDS.contains("Finder") && DROP_UNCONFIRMED_WORDS.contains("Trash")
        );
        assert!(DROP_OUTSIDE_WORDS.contains("Trash"));
    }

    /// The tile drags only the installed copy whose bundle id is the subject's.
    #[test]
    fn the_tile_drags_only_the_installed_aterm_it_reads_for() {
        let root = || PathBuf::from("/Applications/aterm.app");
        let id = Some("com.aterm.aterm");
        assert_eq!(accepted_bundle(root(), true, id, id), Some(root()));
        assert_eq!(
            accepted_bundle(root(), false, id, id),
            None,
            "not installed"
        );
        assert_eq!(accepted_bundle(root(), true, None, id), None, "no id read");
        assert_eq!(
            accepted_bundle(root(), true, Some("com.aterm.aterm.dev"), id),
            None,
            "another bundle"
        );
        assert_eq!(accepted_bundle(root(), true, id, None), None, "no subject");
    }

    fn test_arm() -> DevToolsHelperArm {
        test_arms::showing()
    }

    fn test_reach() -> Reach {
        test_arms::opening()
    }

    fn nudge() -> Wake {
        Arc::new(|| {})
    }

    /// Wait for a worker the lane started: a one-minute hang detector.
    fn until(mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// THE QUEUE: the window's own hand-over waits as long as it takes and outranks a
    /// control verb's, which lapses after [`ASKED_LAPSE`]; while it is not quiet it looks
    /// again [`RECHECK_EVERY`] later, and arms the park only while aterm is focused.
    /// NEGATIVE CONTROL: an inert (headless) lane arms nothing at all.
    #[test]
    fn a_waiting_hand_over_looks_every_few_seconds_only_while_focused() {
        let t0 = Instant::now();
        let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
        assert_eq!(lane.next_deadline(t0, || true), None, "nothing waits");
        lane.queue(Origin::Ctl { now: false }, t0);
        lane.queue(Origin::Automatic, t0);
        lane.queue(Origin::Ctl { now: false }, t0);
        assert_eq!(
            lane.queued(),
            Some(Origin::Automatic),
            "the window's own wins"
        );
        assert_eq!(lane.next_deadline(t0, || true), Some(t0));
        assert_eq!(lane.next_deadline(t0, || false), None, "unfocused: nothing");
        assert!(lane.hand_over_due(t0));
        assert_eq!(lane.take_queued(t0, false), None);
        assert!(!lane.hand_over_due(t0 + RECHECK_EVERY / 2));
        assert_eq!(lane.next_deadline(t0, || true), Some(t0 + RECHECK_EVERY));
        let late = t0 + ASKED_LAPSE * 4;
        assert_eq!(
            lane.take_queued(late, true),
            Some((Origin::Automatic, None)),
            "the window's own never lapses"
        );
        assert_eq!(lane.queued(), None);
        // A control verb's lapses.
        lane.queue(Origin::Ctl { now: false }, t0);
        assert_eq!(lane.take_queued(t0 + ASKED_LAPSE, true), None);
        assert_eq!(lane.queued(), None, "lapsed");
        // Reading aterm on ends the wait.
        lane.queue(Origin::Automatic, t0);
        lane.note_reading(
            Reading {
                grant: DevToolGrant::Granted,
                list: on(),
                ..Reading::UNREAD
            },
            t0,
        );
        assert_eq!(lane.queued(), None);
        // A hand-over whose `spctl` run ended after its quiet moment passed waits for
        // the next one with the run's answer, a RECHECK_EVERY on; the window's own
        // keeps its rank and takes the answer.
        lane.queue_after_listing(Origin::Ctl { now: false }, t0, Ok(()));
        assert!(!lane.hand_over_due(t0), "the moment was judged just now");
        assert!(lane.hand_over_due(t0 + RECHECK_EVERY));
        assert_eq!(
            lane.take_queued(t0 + RECHECK_EVERY, true),
            Some((Origin::Ctl { now: false }, Some(Ok(()))))
        );
        lane.queue(Origin::Automatic, t0);
        lane.queue_after_listing(Origin::Ctl { now: false }, t0, Ok(()));
        assert_eq!(
            lane.take_queued(t0, true),
            Some((Origin::Automatic, Some(Ok(())))),
            "the window's own keeps its rank, with the run's answer"
        );
        // The control: an inert lane.
        let mut inert = Lane::new(true);
        assert!(!inert.windowed());
        inert.queue(Origin::Automatic, t0);
        assert_eq!(inert.next_deadline(t0, || true), None);
    }

    /// THE PANEL'S LANE: shown, it arms the park every [`PANEL_TICK`]; its turn reads the
    /// list on a worker and the result lands in the slot whatever the wake did; a read
    /// that says on turns it done (the window ends the line), and done closes after
    /// [`DONE_FOR`]. The owner's presses arrive through the events handle, and a press
    /// on a panel another show replaced is dropped. System Settings gone closes it.
    #[test]
    fn the_panels_lane_reads_the_list_and_closes_when_done() {
        let t0 = Instant::now();
        let mut lane = Lane::windowed_for_test(test_reach(), test_arm());
        let wake = nudge();
        assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
        assert_eq!(lane.next_deadline(t0, || false), Some(t0), "the first turn");
        let asks = lane.tick_panel(t0, Some(ProbeGate::on()), &wake);
        assert_eq!(asks, Asks::default());
        assert_eq!(
            lane.next_deadline(t0, || false),
            Some(t0 + PANEL_TICK),
            "every PANEL_TICK while shown, focused or not"
        );
        let t1 = t0 + PANEL_TICK;
        let mut asks = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
        assert!(lane.panel().is_some_and(|p| p.read_out), "one read out");
        until(|| {
            asks = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
            asks.granted
        });
        assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Done));
        let _ = lane.tick_panel(t1 + DONE_FOR, Some(ProbeGate::on()), &wake);
        assert!(lane.panel().is_none(), "done closes itself");
        assert_eq!(lane.next_deadline(t1, || true), None);
        // Presses through the handle; a stale panel's are dropped.
        let shared = Arc::clone(&lane.shared);
        assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
        let stale = PanelEvents {
            panel: lane.generation - 1,
            shared: Arc::clone(&shared),
            wake: Arc::clone(&wake),
        };
        stale.post(PanelEvent::NotNow);
        let _ = lane.tick_panel(t0, None, &wake);
        assert!(
            lane.panel().is_some(),
            "a replaced panel's press is dropped"
        );
        let events = PanelEvents {
            panel: lane.generation,
            shared,
            wake: Arc::clone(&wake),
        };
        events.post(PanelEvent::Status);
        assert!(lane.tick_panel(t0, None, &wake).open_status);
        assert_eq!(
            test_arms::decline_notes(),
            0,
            "done and replaced: no decline"
        );
        events.post(PanelEvent::NotNow);
        let _ = lane.tick_panel(t0, None, &wake);
        assert!(lane.panel().is_none(), "Not now closes it");
        assert_eq!(test_arms::decline_notes(), 1, "and is the owner's decline");
        // System Settings gone (after the grace) closes it; before, it waits.
        let mut lane = Lane::windowed_for_test(
            test_reach(),
            DevToolsHelperArm {
                settings_running: || false,
                ..test_arm()
            },
        );
        assert!(lane.show(show(unread(), None), t0, Arc::clone(&wake)));
        let _ = lane.tick_panel(t0, None, &wake);
        assert!(
            lane.panel().is_some(),
            "System Settings may still be starting"
        );
        let _ = lane.tick_panel(t0 + SETTINGS_GRACE, None, &wake);
        assert!(lane.panel().is_none());
        // NEGATIVE CONTROL: the inert arm shows nothing, so nothing is armed.
        let mut inert = Lane::windowed_for_test(test_reach(), DevToolsHelperArm::inert());
        assert!(!inert.show(show(absent(2), None), t0, wake));
        assert!(inert.panel().is_none());
        assert_eq!(inert.next_deadline(t0, || true), None);
    }

    /// A replaced panel's read may finish after its successor's. Both are real
    /// lane workers, released in that order before the park takes either answer:
    /// the old answer must not erase the successor's grant or leave its read out.
    #[test]
    fn a_replaced_panels_read_cannot_overwrite_its_successors_completion() {
        use std::sync::mpsc::{self, Sender};
        type Read = (String, Sender<DevToolList>);
        static READS: Mutex<Option<Sender<Read>>> = Mutex::new(None);
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                *READS.lock_or_recover() = None;
            }
        }
        fn read(_gate: ProbeGate, subject: &str) -> DevToolList {
            let (answer, rx) = mpsc::channel();
            if let Some(reads) = READS.lock_or_recover().as_ref() {
                let _ = reads.send((subject.to_owned(), answer));
            }
            rx.recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| unread())
        }
        let (reads, requests) = mpsc::channel();
        *READS.lock_or_recover() = Some(reads);
        let _reset = Reset;
        let (completed, completions) = mpsc::channel();
        let wake: Wake = Arc::new(move || {
            let _ = completed.send(());
        });
        let reach = Reach {
            read_list: read,
            ..test_reach()
        };
        let mut lane = Lane::windowed_for_test(reach, test_arm());
        let t0 = Instant::now();
        let mut old = show(absent(2), None);
        old.reading.subject = Some("old-panel");
        assert!(lane.show(old, t0, Arc::clone(&wake)));
        let t1 = t0 + PANEL_TICK;
        let _ = lane.tick_panel(t1, Some(ProbeGate::on()), &wake);
        let (subject, old_answer) = requests.recv_timeout(Duration::from_secs(60)).unwrap();
        assert_eq!(subject, "old-panel");

        let mut new = show(absent(2), None);
        new.reading.subject = Some("new-panel");
        assert!(lane.show(new, t1, Arc::clone(&wake)));
        let t2 = t1 + PANEL_TICK;
        let _ = lane.tick_panel(t2, Some(ProbeGate::on()), &wake);
        let (subject, new_answer) = requests.recv_timeout(Duration::from_secs(60)).unwrap();
        assert_eq!(subject, "new-panel");
        assert!(lane.panel().is_some_and(|panel| panel.read_out));

        new_answer.send(on()).unwrap();
        completions.recv_timeout(Duration::from_secs(60)).unwrap();
        old_answer.send(absent(2)).unwrap();
        completions.recv_timeout(Duration::from_secs(60)).unwrap();
        let asks = lane.tick_panel(t2, Some(ProbeGate::on()), &wake);
        assert!(
            asks.granted,
            "the successor's completed grant must reach the park"
        );
        let panel = lane.panel().unwrap();
        assert_eq!(panel.phase(), Phase::Done);
        assert!(!panel.read_out, "the completed read must be retired");
    }

    /// A frame aterm did not set is the owner's move only on the same screens: one
    /// that changed with them is macOS moving the panel off a display (it is
    /// placed again), and half a point is no move at all.
    #[test]
    fn a_move_is_the_owners_only_on_the_same_screens() {
        let r = |x: f64, y: f64, w: f64, h: f64| Rect { x, y, w, h };
        let laptop = [r(0.0, 0.0, 1728.0, 1117.0)];
        let docked = [r(0.0, 0.0, 1728.0, 1117.0), r(1728.0, 0.0, 2560.0, 1440.0)];
        let placed = r(2300.0, 500.0, 360.0, 564.0);
        assert_eq!(drift(placed, &docked, placed, &docked), Drift::Still);
        assert_eq!(
            drift(placed, &docked, r(2300.4, 499.7, 360.0, 564.0), &docked),
            Drift::Still
        );
        assert_eq!(
            drift(placed, &docked, r(2100.0, 420.0, 360.0, 564.0), &docked),
            Drift::Owner
        );
        assert_eq!(
            drift(placed, &docked, r(1300.0, 500.0, 360.0, 564.0), &laptop),
            Drift::Screens
        );
        // The screens changed and the panel did not move: nothing to do.
        assert_eq!(drift(placed, &docked, placed, &laptop), Drift::Still);
    }

    /// The `spctl` worker: its result lands in the slot for the park; one run at a time.
    #[test]
    fn the_terminal_listing_runs_once_and_lands_in_the_slot() {
        let mut lane = Lane::windowed_for_test(test_reach(), test_arm());
        let t0 = Instant::now();
        assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
        assert!(
            lane.start_listing(Listing::ShowList, nudge()),
            "rides the first"
        );
        assert!(
            lane.next_deadline(t0, || false).is_some(),
            "re-read while out"
        );
        let mut taken = None;
        until(|| {
            taken = lane.take_listed();
            taken.is_some()
        });
        let digest_waits = Waiting {
            digest: true,
            ..Waiting::default()
        };
        assert_eq!(
            taken,
            Some((
                Listing::HandOver(Origin::Digest),
                Ok(()),
                digest_waits.clone()
            ))
        );
        assert_eq!(lane.listing(), None);
        assert_eq!(lane.next_deadline(t0, || false), None);
        // A hand-over riding the UNREAD panel's Show it takes the run over: its
        // end opens the page with that hand-over's panel. The reverse rides.
        assert!(lane.start_listing(Listing::ShowList, nudge()));
        assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
        assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
        assert!(lane.start_listing(Listing::ShowList, nudge()));
        assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
        let mut taken = None;
        until(|| {
            taken = lane.take_listed();
            taken.is_some()
        });
        assert_eq!(
            taken,
            Some((
                Listing::HandOver(Origin::Settings),
                Ok(()),
                Waiting::default()
            )),
            "one run, the hand-over's"
        );
    }

    /// A LATER, STRONGER ASK RIDING A RUN TAKES ITS END: an owner's press riding the
    /// window's own (or a control verb's) hand-over gets the page and the panel it asked
    /// for, and a digest press is noted as waiting whichever run it rode, so a page that
    /// then does not open can bring its line back; the Settings card's views wait on the
    /// run that is out, and only then. NEGATIVE CONTROL: a weaker ask riding a stronger
    /// one changes whose end it is not at all.
    #[test]
    fn a_stronger_ask_riding_a_listing_takes_its_end_and_presses_wait_on_it() {
        let view = crate::tab_model::ViewId::from_stored(7);
        let wid = crate::WindowId(3);
        let mut lane = Lane::windowed_for_test(test_reach(), test_arm());
        lane.wait_in_settings([(wid, view)]);
        assert_eq!(lane.waiting, Waiting::default(), "no run out: nobody waits");
        assert!(lane.start_listing(Listing::HandOver(Origin::Automatic), nudge()));
        assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
        assert_eq!(
            lane.listing(),
            Some(Listing::HandOver(Origin::Digest)),
            "the owner's press outranks the window's own"
        );
        assert!(lane.start_listing(Listing::HandOver(Origin::Ctl { now: false }), nudge()));
        assert!(lane.start_listing(Listing::ShowList, nudge()));
        assert_eq!(
            lane.listing(),
            Some(Listing::HandOver(Origin::Digest)),
            "weaker asks ride without taking it over"
        );
        assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
        assert_eq!(
            lane.listing(),
            Some(Listing::HandOver(Origin::Digest)),
            "an equal one rides too: the first press's panel"
        );
        lane.wait_in_settings([(wid, view)]);
        let mut taken = None;
        until(|| {
            taken = lane.take_listed();
            taken.is_some()
        });
        assert_eq!(
            taken,
            Some((
                Listing::HandOver(Origin::Digest),
                Ok(()),
                Waiting {
                    digest: true,
                    settings: vec![(wid, view)],
                }
            ))
        );
        assert_eq!(lane.waiting, Waiting::default(), "taken with the run");
        // A digest press that rode another's run still waits on it.
        assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
        assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
        assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
        let mut taken = None;
        until(|| {
            taken = lane.take_listed();
            taken.is_some()
        });
        assert!(taken.is_some_and(|(_, _, waiting)| waiting.digest));
        // Ranks: Show it < a hand-over the owner did not press < an owner's press.
        assert!(Listing::ShowList.rank() < Listing::HandOver(Origin::Automatic).rank());
        assert!(
            Listing::HandOver(Origin::Ctl { now: true }).rank()
                < Listing::HandOver(Origin::Settings).rank()
        );
    }

    /// The panel's turn asks whether System Settings is in front (the gate on its
    /// list read) at most once per [`PANEL_TICK`] however many loop turns come —
    /// every keystroke and every redraw is one while the panel is up — and once
    /// at once after a drop. NEGATIVE CONTROL: crossing a tick asks again.
    #[test]
    fn the_panel_asks_whether_system_settings_is_in_front_once_per_tick() {
        thread_local! {
            static ASKED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        fn asked() -> u32 {
            ASKED.with(std::cell::Cell::get)
        }
        let arm = DevToolsHelperArm {
            settings_frontmost: || {
                ASKED.with(|c| c.set(c.get() + 1));
                false
            },
            ..test_arm()
        };
        let mut lane = Lane::windowed_for_test(test_reach(), arm);
        let wake = nudge();
        let t0 = Instant::now();
        assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
        let gate = Some(ProbeGate::on());
        // The show's own reading is fresh: the first turn reads nothing.
        let _ = lane.tick_panel(t0, gate, &wake);
        assert_eq!(asked(), 0);
        let t1 = t0 + PANEL_TICK;
        for k in 0..200_u32 {
            let _ = lane.tick_panel(t1 + Duration::from_millis(u64::from(k)), gate, &wake);
        }
        assert_eq!(
            asked(),
            1,
            "two hundred loop turns inside one tick: one look"
        );
        let _ = lane.tick_panel(t1 + PANEL_TICK, gate, &wake);
        assert_eq!(asked(), 2, "the next tick looks again");
        // A drop asks at once, off the cadence.
        events_of(&lane).post(PanelEvent::DragEnded { accepted: true });
        let _ = lane.tick_panel(t1 + PANEL_TICK + Duration::from_millis(10), gate, &wake);
        assert_eq!(asked(), 3, "a drop's read is not held to the cadence");
        assert!(
            lane.panel().is_some_and(|p| !p.read_asked()),
            "not in front: the drop's read falls back to the cadence"
        );
        let _ = lane.tick_panel(t1 + PANEL_TICK + Duration::from_millis(20), gate, &wake);
        assert_eq!(asked(), 3, "and asks no more until the next tick");
    }

    /// The Settings card's decline line reads the owner's Not now from the marker at
    /// most once between focus gains — never per park — and again after this lane
    /// writes a decline (the owner's Not now on its panel).
    #[test]
    fn the_decline_marker_is_read_once_and_again_after_a_not_now() {
        thread_local! {
            static READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        fn reads() -> u32 {
            READS.with(std::cell::Cell::get)
        }
        let reach = Reach {
            declined: || {
                READS.with(|c| c.set(c.get() + 1));
                Some(test_arms::DECLINED)
            },
            ..test_arms::opening()
        };
        let mut lane = Lane::windowed_for_test(reach, test_arm());
        for _ in 0..3 {
            assert_eq!(lane.declined_marker(), Some(test_arms::DECLINED));
        }
        assert_eq!(reads(), 1, "once, not per park");
        lane.forget_opened();
        let _ = lane.declined_marker();
        assert_eq!(reads(), 2, "a focus gain reads it again");
        let wake = nudge();
        let t0 = Instant::now();
        assert!(lane.show(show(absent(2), None), t0, Arc::clone(&wake)));
        events_of(&lane).post(PanelEvent::NotNow);
        let _ = lane.tick_panel(t0, None, &wake);
        assert!(lane.panel().is_none(), "Not now closes it");
        let _ = lane.declined_marker();
        let _ = lane.declined_marker();
        assert_eq!(reads(), 3, "the lane's own decline: read again, once");
    }

    /// The events handle of the lane's current panel.
    fn events_of(lane: &Lane) -> PanelEvents {
        PanelEvents {
            panel: lane.generation,
            shared: Arc::clone(&lane.shared),
            wake: nudge(),
        }
    }
}
