// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! System Settings hand-overs (`Privacy & Security` ▸ `Developer Tools` or `Full Disk Access`)
//! and the helper panel beside them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::DevToolGrant;
use aterm_containment::consent::{
    DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, FdaState, ProbeGate, devtool_grant,
};
use aterm_types::MutexExt as _;
use aterm_types::macos_setup::Access;

/// How long the owner's hands must have been off the keyboard and mouse before aterm puts System
/// Settings in front of them: a pause, not a word being typed.
pub(crate) const QUIET_FOR: Duration = Duration::from_secs(10);

/// How recently the owner must have touched the keyboard or mouse to count as present: a page
/// opened while nobody is there is found hours later with no idea why.
pub(crate) const PRESENT_WITHIN: Duration = Duration::from_secs(5 * 60);

/// How often a waiting hand-over looks for its quiet moment while an aterm window is focused.
pub(crate) const RECHECK_EVERY: Duration = Duration::from_secs(5);

/// When a page aterm brings up now is past its ten minutes ([`atpkg::devtools::REOPEN_AFTER_S`]),
/// read after its marker was written: the marker counts them in whole seconds from its own, so
/// a look from then on never finds the page recent.
pub(crate) fn past_its_ten_minutes() -> Instant {
    Instant::now() + Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S)
}

/// How long a hand-over a control verb asked for waits for its quiet moment.
pub(crate) const ASKED_LAPSE: Duration = Duration::from_secs(30 * 60);

/// How long a process the owner started waits before its first quiet moment, so on a new Mac
/// System Settings comes up at the owner's first pause; a successor an update brought in waits
/// `consent_card::LAUNCH_SETTLE` instead.
pub(crate) const COLD_START_SETTLE: Duration = Duration::from_secs(5);

/// Whether now is a moment to put System Settings in front of the owner unasked: an aterm window
/// is focused, the owner is present ([`PRESENT_WITHIN`]) but not typing or pointing
/// ([`QUIET_FOR`]), this process has settled ([`Lane::settled`]), and no update hand-off is
/// pending or in flight.
#[must_use]
pub(crate) const fn quiet(
    focused: bool,
    input_within_quiet: bool,
    input_within_presence: bool,
    settled: bool,
    handoff_busy: bool,
) -> bool {
    focused && !input_within_quiet && input_within_presence && settled && !handoff_busy
}

/// Where a hand-over came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The window's own, once per approved setup revision (`devtools_watch` for Developer Tools,
    /// the launch check, `App::note_launch_fda`, for Full Disk Access): a step of System State's
    /// run.
    Automatic,
    /// A control client's act on System State's Full Disk Access or Developer Tools row
    /// (`App::open_access_from_system_state`). It never mints the yes, so it is a step of the
    /// approved revision like the window's own: judged at the step moment, never opened at once,
    /// held by a Not now under its revision, but not limited to once per revision (the ten-minute
    /// gap bounds it). It lapses after [`ASKED_LAPSE`].
    SystemStateAct,
    /// The Settings ▸ Security card's button: the owner's press.
    Settings,
    /// The digest line's mover: the owner's press.
    Digest,
    /// `aterm ctl devtools hand-over`: every in-session client can send it, so it is held to the
    /// window's own rules.
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

    /// How long after aterm last opened the page (`atpkg::devtools`' shared marker) this origin
    /// may open it again; `None` for an owner's press.
    pub(crate) const fn reopen_after_s(self) -> Option<u64> {
        match self {
            Self::Settings | Self::Digest => None,
            Self::Ctl { now: true } => Some(atpkg::devtools::REOPEN_NOW_AFTER_S),
            Self::Automatic | Self::SystemStateAct | Self::Ctl { now: false } => {
                Some(atpkg::devtools::REOPEN_AFTER_S)
            }
        }
    }

    const fn lapses_after(self) -> Option<Duration> {
        match self {
            Self::Ctl { now: false } | Self::SystemStateAct => Some(ASKED_LAPSE),
            _ => None,
        }
    }

    /// The origin in aterm.log's words.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::SystemStateAct => "system state act",
            Self::Settings => "settings",
            Self::Digest => "digest",
            Self::Ctl { now: false } => "ctl",
            Self::Ctl { now: true } => "ctl now",
        }
    }

    /// A step of System State's run, judged at its step moment
    /// (`App::access_step_moment`): the window's own and a control client's act on a
    /// System State row. A control verb's keeps the quiet moment alone.
    pub(crate) const fn system_state_step(self) -> bool {
        matches!(self, Self::Automatic | Self::SystemStateAct)
    }
}

/// Which System Settings page a hand-over brings up, aterm's panel beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Page {
    /// Privacy & Security ▸ Developer Tools.
    #[default]
    DeveloperTools,
    /// Privacy & Security ▸ Full Disk Access: the window's own hand-over at the first quiet moment
    /// of a launch that reads it off ([`admit_fda`]), and the panel beside the page the Security
    /// card's and the digest's *Open Settings* open.
    FullDiskAccess,
}

impl Page {
    /// The page's System Settings anchor.
    pub(crate) const fn pane(self) -> crate::menu::PrivacyPane {
        match self {
            Self::DeveloperTools => crate::menu::PrivacyPane::DeveloperTools,
            Self::FullDiskAccess => crate::menu::PrivacyPane::FullDiskAccess,
        }
    }

    /// The page in aterm.log's words.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DeveloperTools => "devtools",
            Self::FullDiskAccess => "full disk access",
        }
    }

    /// The System State row the page grants.
    pub(crate) const fn access(self) -> Access {
        match self {
            Self::DeveloperTools => Access::DeveloperTools,
            Self::FullDiskAccess => Access::FullDiskAccess,
        }
    }
}

/// Whether the owner's Not now holds a hand-over from `origin` back, for the page whose current
/// key is `key`: `last` is the newest decline (`PageMarkers::last_declined`) and `held` whether
/// one is inside its span. A System State step ([`Origin::system_state_step`]) is held only by a
/// Not now under its own revision (`atpkg::devtools::same_step`): a new revision is a new
/// Permissions approval (`docs/DESIGN-system-state-2026-10-21.md` §6), which asks again. The
/// control verb is also held by any Not now inside its span; an owner's press never. Pure.
#[must_use]
pub(crate) fn declined_for(
    origin: Origin,
    key: &str,
    last: Option<&(String, u64)>,
    held: bool,
) -> bool {
    let this_revision = last.is_some_and(|(word, _)| atpkg::devtools::same_step(word, key));
    match origin {
        Origin::Settings | Origin::Digest => false,
        Origin::Automatic | Origin::SystemStateAct => this_revision,
        Origin::Ctl { .. } => this_revision || held,
    }
}

/// When [`admit`] is asked: as the request arrives, or at the quiet moment a waiting one was held
/// for.
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
    pub(crate) grant: DevToolGrant,
    pub(crate) list: DevToolList,
    /// The owner's Not now still holds ([`declined_for`]).
    pub(crate) declined: bool,
    pub(crate) grant_stable: bool,
    pub(crate) recent: bool,
    /// The window's own Full Disk Access probe reads the grant on
    /// ([`Refusal::NoFullDiskAccess`]).
    pub(crate) fda_on: bool,
    /// aterm already brought this page up under the access revision
    /// (`App::devtools_shown_this_revision`), judged as the page would open: a hand-over asked
    /// while no revision stood waits queued for one.
    pub(crate) shown_this_revision: bool,
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
    /// aterm's row reads on while macOS still checks its new programs: the page holds nothing to
    /// add or switch on (`atpkg::devtools::Step::ListedOn`).
    Listed,
    /// aterm already brought the page up for this approved revision, unasked
    /// ([`FdaFacts::shown_this_revision`], [`Facts::shown_this_revision`]).
    Shown,
    /// The window's own Developer Tools hand-over waits for Full Disk Access to read on:
    /// without it the list cannot be read back, so a switch flipped there could not be
    /// confirmed.
    NoFullDiskAccess,
    /// Another app is responsible for this window ([`FdaFacts::own`]): the probe read that app's
    /// grant, which aterm's page cannot change.
    NotOwn,
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
            Self::Shown => "shown",
            Self::NoFullDiskAccess => "waits on full disk access",
            Self::NotOwn => "not own",
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
    /// Open the page and show the panel; on a list read whole and empty, run `spctl developer-mode
    /// enable-terminal` first so the page exists.
    Hand {
        /// Run `spctl developer-mode enable-terminal` first.
        list_terminal: bool,
    },
}

/// Who may bring the Developer Tools page up, and when.
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
    if origin == Origin::Automatic && !facts.fda_on {
        return Admit::Refuse(Refusal::NoFullDiskAccess);
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
    if origin == Origin::Automatic && facts.shown_this_revision {
        return Admit::Refuse(Refusal::Shown);
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

/// What [`admit_fda`] judges on, read at the moment it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FdaFacts {
    pub(crate) fda: FdaState,
    pub(crate) declined: bool,
    pub(crate) grant_stable: bool,
    pub(crate) recent: bool,
    pub(crate) devtools_recent: bool,
    /// aterm already brought this page up under the approved setup revision (`fda-pane-opened`'s
    /// key, `atpkg::devtools::same_step`; [`fda_opened_word`] keeps it across a press while no
    /// revision stands).
    pub(crate) shown_this_revision: bool,
    pub(crate) own: bool,
}

/// The word `fda-pane-opened` takes for the Full Disk Access page brought up now under `key`
/// (`App::access_marks`): `key` while an access revision stands (`revision`). With none (after
/// Stop, during a decision hold) `key` names the running version, and a word `opened` that names
/// a revision's step for this TCC identity is kept instead: Set up keeps the access revision, so
/// the press must not cost the window's once-per-revision check ([`FdaFacts::shown_this_revision`])
/// the page that revision already brought up. A word under a version, or one another bundle id
/// or Mac wrote (wherever both words carry one, as `atpkg::devtools::same_step` reads a word
/// written cold), gives way to `key`.
pub(crate) fn fda_opened_word(revision: bool, opened: Option<&str>, key: &str) -> String {
    let kept = opened.filter(|word| {
        let step = word.split('@').next().unwrap_or_default();
        let names_revision = step
            .strip_prefix('r')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        let same_identity = word
            .split('@')
            .skip(1)
            .zip(key.split('@').skip(1))
            .all(|(left, right)| left == right);
        !revision && names_revision && same_identity
    });
    kept.unwrap_or(key).to_owned()
}

/// Who may bring the Full Disk Access page up, and when. A control client's act on System State's
/// row ([`Origin::SystemStateAct`]) is held as the window's own hand-over except for the
/// once-per-revision limit, and is queued, never opened at once.
#[must_use]
pub(crate) fn admit_fda(origin: Origin, moment: Moment, facts: &FdaFacts) -> Admit {
    if origin.owner_press() {
        return if facts.fda == FdaState::Granted {
            Admit::PaneOnly
        } else {
            Admit::Hand {
                list_terminal: false,
            }
        };
    }
    match facts.fda {
        FdaState::Granted => return Admit::Refuse(Refusal::Granted),
        FdaState::Unknown => return Admit::Refuse(Refusal::Unknown),
        FdaState::Denied => {}
    }
    if facts.declined {
        return Admit::Refuse(Refusal::Declined);
    }
    if !facts.grant_stable {
        return Admit::Refuse(Refusal::Unstable);
    }
    if origin == Origin::Automatic && facts.shown_this_revision {
        return Admit::Refuse(Refusal::Shown);
    }
    if facts.recent {
        return Admit::Refuse(Refusal::Recent);
    }
    match moment {
        Moment::Quiet if !facts.own => Admit::Refuse(Refusal::NotOwn),
        Moment::Quiet if !facts.devtools_recent => Admit::Hand {
            list_terminal: false,
        },
        Moment::Asked | Moment::Quiet => Admit::Queue,
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
/// (`ConsentState::devtools`), or the panel's one-shot list read joined with the tccd answers the
/// window last read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    pub(crate) grant: DevToolGrant,
    pub(crate) list: DevToolList,
    pub(crate) log: DevToolProbe,
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

    /// This reading with a new list read, its verdict joined again with the same tccd answers
    /// (`consent::devtool_grant`).
    #[must_use]
    pub(crate) fn with_list(self, list: DevToolList) -> Self {
        Self {
            grant: devtool_grant(&list, &self.log),
            list,
            ..self
        }
    }
}

/// The tile's caption under aterm's icon.
pub(crate) const TILE_CAPTION: &str = "aterm";

/// The variant for a list aterm cannot read without Full Disk Access: the owner can see it, aterm
/// cannot.
pub(crate) const UNREAD_WORDS: &str = "aterm can't see this list without Full Disk Access. When \
                                       aterm's switch is on, press \"I've switched it on\".";

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

/// How many reads in a row that failed on their way the panel takes before it treats the failure
/// as lasting: the UNREAD variant, and no more reads.
pub(crate) const READ_FAILURES_LASTING: u32 = 3;

/// The drag ended with nothing taken.
pub(crate) const DROP_FAILED_WORDS: &str =
    "Didn't take? Press + below the Developer Tools list and choose aterm.";

/// An accepted drop, and a whole read of the list [`DROP_CONFIRM_WITHIN`] or more later that
/// still does not hold aterm.
pub(crate) const DROP_UNCONFIRMED_WORDS: &str = "aterm did not reach Developer Tools. If it \
                                                 landed on another list in System Settings, \
                                                 switch it off there; if you let go over \
                                                 Finder, it made a copy of aterm there, which \
                                                 you can move to the Trash.";

/// A drop something took outside System Settings' window: not the list, so the icon stays offered.
pub(crate) const DROP_OUTSIDE_WORDS: &str = "That landed outside System Settings. If Finder \
                                             made a copy of aterm there, you can move that \
                                             copy to the Trash.";

fn dropped_how() -> String {
    use atpkg::devtools::{CHECK_SWITCH, PLUS_PRESS, WRONG_SWITCHES};
    format!(
        "If aterm is not on the Developer Tools list, {PLUS_PRESS} (macOS asks for your \
         password), {CHECK_SWITCH}. {WRONG_SWITCHES}"
    )
}

/// The footer link to aterm's own status screen, in atpkg's one spelling (`aterm doctor` prints
/// it too).
pub(crate) const STATUS_LINK: &str = atpkg::devtools::STATUS_LINK;

fn show_list_failed_words(why: &str) -> String {
    format!(
        "macOS did not show the list ({why}). In {}, press + below the list and choose aterm.",
        atpkg::devtools::PANE
    )
}

/// The Full Disk Access panel's heading.
pub(crate) const FDA_PANEL_TITLE: &str = "Turn on Full Disk Access for aterm";

/// Why: what the switch grants, the access card's `consent_card::FDA_GRANTS` as a sentence.
pub(crate) const FDA_PANEL_WHY: &str = "Full Disk Access lets programs you run in aterm, agents \
                                        included, read protected data such as Mail and \
                                        Messages.";

/// The tile's hint while it can be dragged.
pub(crate) const FDA_PANEL_STEP: &str = "Drag aterm into the Full Disk Access list";

/// How, with aterm's icon to drag.
pub(crate) const FDA_PANEL_HOW: &str = "Drag aterm into the list (or press + below it and choose \
                                        aterm), enter your password if macOS asks, and switch \
                                        aterm on.";

/// How, with no icon to drag (the running copy is not one aterm offers): + leads.
pub(crate) const FDA_PANEL_HOW_PLUS: &str = "Press + below the list, choose aterm, enter your \
                                             password if macOS asks, and switch aterm on.";

/// How, once the tile is spent (aterm cannot read this list, so it never says where the drop
/// went).
pub(crate) const FDA_PANEL_DROPPED: &str = "Switch aterm on in the list. If aterm is not there, \
                                            press + below the list and choose aterm.";

/// The drag ended with nothing taken.
pub(crate) const FDA_DROP_FAILED_WORDS: &str =
    "Didn't take? Press + below the Full Disk Access list and choose aterm.";

/// The trade, as the Security page states it.
pub(crate) const FDA_PANEL_TRADE: &str = "Full Disk Access on a terminal is broad: everything you \
                                          run in aterm can then read everything this account \
                                          can read.";

/// The done words: aterm's own probe reads the grant.
pub(crate) const FDA_PANEL_DONE: &str = "aterm has Full Disk Access.";

/// tccd's measured cost, when its log was read whole and it checked any: the Settings row's words.
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

/// Whole minutes from the opened marker's stamp `at` to `now` (unix seconds), for the Settings
/// card.
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
    /// Run `spctl developer-mode enable-terminal` so an empty list shows, then bring the page up
    /// again: offered on the UNREAD variant (aterm cannot see whether the list is empty), and on a
    /// readable list while it reads whole and empty with nothing listed by this panel yet.
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

    /// Every button, in tag order: a live panel button's `tag` is its index here.
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
    pub(crate) bundle: PathBuf,
    pub(crate) live: bool,
    pub(crate) step: &'static str,
}

/// The hint beside a spent tile: something took its one drop.
pub(crate) const TILE_SPENT_HINT: &str = "Dropped.";

impl Tile {
    /// The hint beside the icon: [`Self::step`] while it can be dragged, then what happened.
    pub(crate) const fn hint(&self) -> &'static str {
        if self.live {
            self.step
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

/// How long the panel stays up at most: ten minutes.
pub(crate) const PANEL_FOR: Duration = Duration::from_secs(10 * 60);

/// The panel lane's cadence while the panel is up: place it, check System Settings is still
/// running, and (on a readable list) read it once.
pub(crate) const PANEL_TICK: Duration = Duration::from_secs(2);

/// How long the done words stay before the panel closes itself.
pub(crate) const DONE_FOR: Duration = Duration::from_secs(4);

/// How long after an accepted drop a readable list must read aterm on before the panel says it
/// may have landed elsewhere.
pub(crate) const DROP_CONFIRM_WITHIN: Duration = Duration::from_secs(15);

/// How long after a show System Settings may take to start before the lane takes its absence for
/// the owner having quit it (`aterm doctor` opens the page after the window shows the panel).
pub(crate) const SETTINGS_GRACE: Duration = Duration::from_secs(5);

/// What the panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Why and how, with the tile.
    Explain,
    /// The owner's drag ended with nothing taken.
    DropFailed,
    /// Something outside System Settings' window took the owner's drop: the icon is still offered.
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
    pub(crate) redraw: bool,
    pub(crate) close: Option<Closed>,
    pub(crate) granted: bool,
    pub(crate) recheck: bool,
    pub(crate) list_terminal: bool,
    pub(crate) open_status: bool,
}

impl Effect {
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
    pub(crate) page: Page,
    pub(crate) origin: Origin,
    pub(crate) reading: Reading,
    pub(crate) listed: Option<Result<(), String>>,
    pub(crate) bundle: Option<PathBuf>,
    pub(crate) skips: bool,
}

/// The panel's state machine: what it shows, and what each of the owner's presses, each reading
/// and each turn of the lane asks.
#[derive(Clone, Debug)]
pub(crate) struct Panel {
    page: Page,
    origin: Origin,
    shown_at: Instant,
    phase: Phase,
    unread: Option<DevToolListLabel>,
    failed_reads: u32,
    listed_off: bool,
    listed_on: bool,
    listed_terminal: bool,
    empty: bool,
    log: DevToolProbe,
    subject: Option<&'static str>,
    bundle: Option<PathBuf>,
    skips: bool,
    dropped_at: Option<Instant>,
    show_list_failed: Option<String>,
    done_at: Option<Instant>,
    read_out: bool,
    last_read: Option<Instant>,
}

fn listed_off(list: DevToolList) -> bool {
    list.label == DevToolListLabel::Read && list.listing == DevToolListing::Off
}

fn read_empty(list: DevToolList) -> bool {
    list.label == DevToolListLabel::Read && list.rows == Some(0)
}

fn listed_on(list: DevToolList, grant: DevToolGrant) -> bool {
    grant != DevToolGrant::Granted
        && atpkg::devtools::Step::of(&list) == atpkg::devtools::Step::ListedOn
}

impl Panel {
    /// A panel shown at `now`.
    pub(crate) fn new(show: Show, now: Instant) -> Self {
        Self {
            page: show.page,
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

    /// The page it stands beside.
    pub(crate) const fn page(&self) -> Page {
        self.page
    }

    /// Whether the lane may read the list (otherwise the UNREAD variant).
    pub(crate) fn readable(&self) -> bool {
        self.page == Page::DeveloperTools && self.unread.is_none()
    }

    /// The words, top to bottom.
    #[must_use]
    pub(crate) fn content(&self) -> Content {
        use atpkg::devtools::{
            HELPER_HOW, HELPER_HOW_PLUS, HELPER_LISTED_ON, HELPER_SWITCH_ON, HELPER_TITLE, done,
            only_aterm, trade, why,
        };
        if self.page == Page::FullDiskAccess {
            return self.fda_content();
        }
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
        let listed = self.listed_off || self.listed_on;
        if let Some(bundle) = self.bundle.as_ref().filter(|_| !listed) {
            blocks.push(Block::Tile(Tile {
                bundle: bundle.clone(),
                live: self.dropped_at.is_none(),
                step: atpkg::devtools::HELPER_STEP,
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

    fn fda_content(&self) -> Content {
        let mut blocks = vec![Block::text(FDA_PANEL_TITLE, Tone::Title)];
        if self.phase == Phase::Done {
            blocks.push(Block::text(FDA_PANEL_DONE, Tone::Done));
            blocks.push(Block::Link(STATUS_LINK));
            return Content { blocks };
        }
        blocks.push(Block::text(FDA_PANEL_WHY, Tone::Body));
        if let Some(bundle) = &self.bundle {
            blocks.push(Block::Tile(Tile {
                bundle: bundle.clone(),
                live: self.dropped_at.is_none(),
                step: FDA_PANEL_STEP,
            }));
        }
        match self.phase {
            Phase::DropFailed => blocks.push(Block::text(FDA_DROP_FAILED_WORDS, Tone::Notice)),
            Phase::DroppedOutside => blocks.push(Block::text(DROP_OUTSIDE_WORDS, Tone::Notice)),
            Phase::Explain | Phase::DroppedUnconfirmed | Phase::Done => {}
        }
        blocks.push(Block::text(
            if self.dropped_at.is_some() {
                FDA_PANEL_DROPPED
            } else if self.bundle.is_none() {
                FDA_PANEL_HOW_PLUS
            } else {
                FDA_PANEL_HOW
            },
            Tone::Body,
        ));
        blocks.push(Block::text(FDA_PANEL_TRADE, Tone::Secondary));
        blocks.push(Block::Buttons(vec![Button::SwitchedOn, Button::NotNow]));
        blocks.push(Block::Link(STATUS_LINK));
        Content { blocks }
    }

    /// Whether the owner dropped aterm's icon somewhere this panel's life.
    pub(crate) const fn dropped(&self) -> bool {
        self.dropped_at.is_some()
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

    /// A completed read of the list (the lane's one-shot).
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

    /// The window's own reading completed while the panel is up: a grant is done; otherwise the
    /// panel's next list read joins with its tccd answers.
    pub(crate) fn note_reading(&mut self, reading: &Reading, now: Instant) -> Effect {
        if reading.grant == DevToolGrant::Granted {
            return self.granted(now);
        }
        self.log = reading.log;
        Effect::default()
    }

    /// Whether the panel was shown before any reading named the bundle id it reads the list for
    /// (an owner's press while the first reading was still out).
    pub(crate) const fn wants_subject(&self) -> bool {
        self.subject.is_none()
    }

    /// A later reading named the bundle id: the panel's list reads start, and the icon to drag
    /// appears when the running copy is one aterm offers (`bundle`, from [`drag_bundle`]).
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

    /// A turn of the lane at `now`: the done words' time and the ten-minute bound.
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

    const fn read_asked(&self) -> bool {
        self.last_read.is_none()
    }

    /// Whether the lane should read the list now: only a readable list, one read at a time, at
    /// most one per [`PANEL_TICK`] (at once after a drop), never once done.
    pub(crate) fn wants_read(&self, now: Instant) -> bool {
        self.page == Page::DeveloperTools
            && self.unread.is_none()
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

/// The panel's width, in points.
pub(crate) const PANEL_W: f64 = 360.0;

const INSET: f64 = 16.0;

const SPACING: f64 = 8.0;

/// The tile's icon, square.
pub(crate) const ICON: f64 = 64.0;

const BUTTON_H: f64 = 28.0;

/// A rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
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

/// How the live panel measures its words: AppKit's text system there, a fixed-width estimate in
/// the tests.
pub(crate) trait Measure {
    fn text_height(&self, text: &str, tone: Tone, width: f64) -> f64;
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
    pub(crate) w: f64,
    pub(crate) h: f64,
    pub(crate) parts: Vec<(Part, Rect)>,
}

/// The panel's frames, from its words: blocks stacked top to bottom at [`PANEL_W`], text wrapped,
/// the tile's icon at the left with its caption, hint and path beside it, buttons right-aligned
/// and wrapping onto a new row when they do not fit.
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

/// Whether the panel may come in front now: only while System Settings is the frontmost app, and
/// not when it is in front already or its tile's drag runs.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) const fn may_reveal(settings_front: bool, visible: bool, dragging: bool) -> bool {
    settings_front && !visible && !dragging
}

/// What the lane's placement turn ([`live`]'s `place_beside_settings`) does with a panel the owner
/// has not moved and whose drag is not running, from whether it is in front, whether System
/// Settings is the active app, whether the panel has stood beside its window this show (`beside`),
/// and that window as read now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PlaceTurn {
    /// Leave it.
    Nothing,
    /// Order it out: System Settings is still the active app but the window it stood beside left
    /// the screen (minimized); a panel left up would jump to a screen corner over other apps' work.
    Hide,
    /// Place it beside the window (`None`: the window was never found this show, so the screen's
    /// corner).
    Place(Option<Rect>),
    /// Bring it back beside the window, which is on screen again (un-minimizing posts no
    /// activation, so only this turn sees it).
    Return(Rect),
}

/// [`PlaceTurn`] for one turn.
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

/// Whether a drop something took, let go at (`x`, `y`) in Cocoa screen space, landed on System
/// Settings' window `settings` as the placement reads it (true with no window read), so on the
/// list as far as aterm can tell.
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

const PLACE_GAP: f64 = 12.0;

/// System Settings' sidebar width.
pub(crate) const SETTINGS_SIDEBAR_W: f64 = 220.0;

/// Convert a Quartz window-list rectangle (origin top-left of the primary screen, y down) to
/// Cocoa's screen space (origin bottom-left, y up).
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

/// Where the panel goes, in Cocoa screen space.
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
    /// It moved on the same arrangement of screens: the owner moved it, and it stays where they
    /// put it.
    Owner,
    /// It moved and the screens changed with it: macOS moved it off a display that went away or
    /// was rearranged, so aterm places it again.
    Screens,
}

/// Whether the owner moved the panel: `placed` is where aterm left it, on screens whose frames
/// were `placed_on`; `frame` and `screens` are now.
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

/// The smallest window counted as System Settings' main window, wide by [`SETTINGS_MIN_H`] high:
/// its sheets, popovers and password window are smaller (the password window is 260 × 349 pt on
/// 27.0).
pub(crate) const SETTINGS_MIN_W: f64 = 320.0;
/// See [`SETTINGS_MIN_W`].
pub(crate) const SETTINGS_MIN_H: f64 = 240.0;

/// One of System Settings' on-screen windows as the placement reads it: its layer, opacity and
/// frame in Cocoa screen space, never its name, title or number, and kept no longer than the turn
/// that read it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SettingsWindow {
    pub(crate) layer: isize,
    pub(crate) alpha: f64,
    pub(crate) bounds: Rect,
}

/// Which of `windows` is System Settings' main window, the one the panel stands beside: the
/// largest on layer 0 with any opacity and at least [`SETTINGS_MIN_W`] × [`SETTINGS_MIN_H`], the
/// first of equals.
#[must_use]
pub(crate) fn settings_main(windows: &[SettingsWindow]) -> Option<usize> {
    let area = |r: Rect| r.w * r.h;
    let mut best: Option<usize> = None;
    for (i, window) in windows.iter().enumerate() {
        if window.layer != 0
            || window.alpha <= 0.0
            || window.bounds.w < SETTINGS_MIN_W
            || window.bounds.h < SETTINGS_MIN_H
        {
            continue;
        }
        if best.is_none_or(|b| area(window.bounds) > area(windows[b].bounds)) {
            best = Some(i);
        }
    }
    best
}

fn secondary(windows: &[SettingsWindow], level: isize) -> impl Iterator<Item = &SettingsWindow> {
    let main = settings_main(windows);
    windows
        .iter()
        .enumerate()
        .filter(move |&(i, window)| {
            main.is_some_and(|m| {
                i != m
                    && window.layer <= level
                    && window.alpha > 0.0
                    && !window.bounds.holds(windows[m].bounds)
            })
        })
        .map(|(_, window)| window)
}

/// Whether one of System Settings' secondary windows (its password window) lies under the panel
/// at `panel`, floating at `level`: the panel orders out until it goes ([`cover_turn`]).
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn under_panel(windows: &[SettingsWindow], panel: Rect, level: isize) -> bool {
    secondary(windows, level).any(|window| window.bounds.overlap(panel) > 0.0)
}

/// Whether System Settings shows one of its secondary windows over its main window, wherever the
/// panel stands: the owner may be typing their password, so the panel's list read waits and a
/// drop is judged after it (judged mid-password, it would read as not landed).
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) fn settings_busy(windows: &[SettingsWindow], level: isize) -> bool {
    let Some(main) = settings_main(windows).map(|i| windows[i].bounds) else {
        return false;
    };
    secondary(windows, level).any(|window| window.bounds.overlap(main) > 0.0)
}

/// What the lane's turn does about System Settings' secondary windows ([`under_panel`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cover {
    /// Nothing to do.
    Nothing,
    /// Order the panel out: one lies under it.
    Out,
    /// Bring back the panel this ordered out: the window went, and System Settings is still the
    /// active app.
    Back,
}

/// [`Cover`] for one turn, from whether the panel is in front, whether a secondary window ordered
/// it out (`yielded`), whether System Settings is the active app, and whether one lies under it
/// now.
#[must_use]
#[cfg_attr(
    not(any(test, target_os = "macos")),
    expect(dead_code, reason = "the AppKit panel is macOS-only")
)]
pub(crate) const fn cover_turn(
    visible: bool,
    yielded: bool,
    settings_front: bool,
    under: bool,
) -> Cover {
    match (visible, yielded) {
        (true, _) if under => Cover::Out,
        (false, true) if settings_front && !under => Cover::Back,
        _ => Cover::Nothing,
    }
}

/// The bundle the tile offers to drag: the running aterm's own bundle, when it is the installed
/// copy (not a translocated or mounted image), it exists, and its bundle id is the one the list is
/// read for.
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

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn accepted_bundle(
    root: PathBuf,
    installed: bool,
    root_id: Option<&str>,
    subject: Option<&str>,
) -> Option<PathBuf> {
    (installed && root_id.is_some() && root_id == subject).then_some(root)
}

/// What the hand-over does to the Mac and to its markers, injected like the consent probes
/// (`consent_state::ConsentProbes`): the window's instance gets [`Self::live`]; a headless
/// instance and every unit test get [`Self::inert`], which opens, runs, reads and writes nothing.
#[derive(Clone, Copy)]
pub(crate) struct Reach {
    pub(crate) open_pane: fn() -> bool,
    pub(crate) list_terminal: fn() -> Result<(), String>,
    pub(crate) read_list: fn(ProbeGate, &str) -> DevToolList,
    pub(crate) last_opened: fn() -> Option<(String, u64)>,
    pub(crate) opened_within: fn(u64) -> Option<u64>,
    pub(crate) note_opened: fn(&str),
    pub(crate) declined: fn() -> Option<(u64, u64)>,
    pub(crate) last_declined: fn() -> Option<(String, u64)>,
    pub(crate) note_declined: fn(&str),
    pub(crate) grant_stable: fn() -> bool,
    pub(crate) drag_bundle: fn(Option<&str>) -> Option<PathBuf>,
    pub(crate) refresh_verdict: fn() -> Result<(), String>,
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
            last_declined: live_last_declined,
            note_declined: atpkg::devtools::note_declined,
            grant_stable: live_grant_stable,
            drag_bundle,
            refresh_verdict: atpkg::devtools::refresh_verdict,
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
            last_declined: inert_last_opened,
            note_declined: inert_note,
            grant_stable: inert_no,
            drag_bundle: inert_drag_bundle,
            refresh_verdict: inert_list_terminal,
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

fn live_last_declined() -> Option<(String, u64)> {
    atpkg::devtools::DEVELOPER_TOOLS.last_declined()
}

fn live_grant_stable() -> bool {
    crate::signing_identity::running_dr_class_if_warm().grant_stable()
}

const fn inert_open_pane() -> bool {
    false
}

fn inert_list_terminal() -> Result<(), String> {
    Err("not run on this instance".to_string())
}

const fn inert_read_list(_gate: ProbeGate, _subject: &str) -> DevToolList {
    DevToolList::refused(DevToolListLabel::RefusedDisabled)
}

const fn inert_last_opened() -> Option<(String, u64)> {
    None
}

const fn inert_opened_within(_window_s: u64) -> Option<u64> {
    None
}

const fn inert_note(_version: &str) {}

const fn inert_not_declined() -> Option<(u64, u64)> {
    None
}

const fn inert_no() -> bool {
    false
}

/// What the Full Disk Access hand-over does to the Mac and to its markers
/// (`atpkg::devtools::FULL_DISK_ACCESS`), injected beside [`Reach`]: the window's instance gets
/// [`Self::live`]; a headless instance and every unit test get [`Self::inert`].
#[derive(Clone, Copy)]
pub(crate) struct FdaReach {
    pub(crate) open_pane: fn() -> bool,
    pub(crate) last_opened: fn() -> Option<(String, u64)>,
    pub(crate) opened_within: fn(u64) -> Option<u64>,
    pub(crate) note_opened: fn(&str),
    pub(crate) declined: fn() -> Option<(u64, u64)>,
    pub(crate) last_declined: fn() -> Option<(String, u64)>,
    pub(crate) note_declined: fn(&str),
    live: bool,
}

impl FdaReach {
    /// The windowed instance's arms.
    pub(crate) const fn live() -> Self {
        Self {
            open_pane: live_open_fda_pane,
            last_opened: live_fda_last_opened,
            opened_within: live_fda_opened_within,
            note_opened: live_fda_note_opened,
            declined: live_fda_declined,
            last_declined: live_fda_last_declined,
            note_declined: live_fda_note_declined,
            live: true,
        }
    }

    /// The headless / unit-test arms: nothing opens, is read or written.
    pub(crate) const fn inert() -> Self {
        Self {
            open_pane: inert_open_pane,
            last_opened: inert_last_opened,
            opened_within: inert_opened_within,
            note_opened: inert_note,
            declined: inert_not_declined,
            last_declined: inert_last_opened,
            note_declined: inert_note,
            live: false,
        }
    }
}

impl std::fmt::Debug for FdaReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FdaReach")
            .field("live", &self.live)
            .finish()
    }
}

fn live_open_fda_pane() -> bool {
    crate::menu::open_privacy_settings(Page::FullDiskAccess.pane()).opened()
}

fn live_fda_last_opened() -> Option<(String, u64)> {
    atpkg::devtools::FULL_DISK_ACCESS.last_opened()
}

fn live_fda_opened_within(window_s: u64) -> Option<u64> {
    atpkg::devtools::FULL_DISK_ACCESS.opened_within(window_s)
}

fn live_fda_note_opened(version: &str) {
    atpkg::devtools::FULL_DISK_ACCESS.note_opened(version);
}

fn live_fda_declined() -> Option<(u64, u64)> {
    atpkg::devtools::FULL_DISK_ACCESS
        .declined_until()
        .map(|declined| (declined.at, declined.until))
}

fn live_fda_last_declined() -> Option<(String, u64)> {
    atpkg::devtools::FULL_DISK_ACCESS.last_declined()
}

fn live_fda_note_declined(version: &str) {
    atpkg::devtools::FULL_DISK_ACCESS.note_declined(version);
}

const fn inert_drag_bundle(_subject: Option<&str>) -> Option<PathBuf> {
    None
}

/// The panel itself, injected like `ConsentGestures`: main thread only.
#[derive(Clone, Copy)]
pub(crate) struct DevToolsHelperArm {
    pub(crate) show: fn(&Content, PanelEvents) -> bool,
    pub(crate) update: fn(&Content),
    pub(crate) place: fn(),
    pub(crate) close: fn(),
    pub(crate) settings_running: fn() -> bool,
    pub(crate) settings_frontmost: fn() -> bool,
    pub(crate) settings_busy: fn() -> bool,
    live: bool,
}

impl DevToolsHelperArm {
    /// The windowed instance's arm: AppKit's panel (`live`) on macOS.
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
                settings_busy: live::settings_busy_now,
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
            settings_busy: inert_no,
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

const fn inert_update(_content: &Content) {}

const fn inert_unit() {}

#[derive(Debug, Default)]
struct Shared {
    listed: Option<Result<(), String>>,
    generation: u64,
    list: Option<(u64, DevToolList)>,
    events: Vec<(u64, PanelEvent)>,
}

/// The nudge a worker or a press sends (`Wake::DevToolsHandOver`).
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// Where the live panel posts the owner's presses: the shared slot the park reads, and the
/// nudge.
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
    /// A handle bound to no lane: what it posts lands in a slot nobody reads.
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

/// What a `spctl developer-mode enable-terminal` run is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listing {
    /// A hand-over on a list read whole and empty: open the page and show the panel when it
    /// ends.
    HandOver(Origin),
    /// The panel's Show it: bring the page up again when it ends.
    ShowList,
}

impl Listing {
    const fn rank(self) -> u8 {
        match self {
            Self::ShowList => 0,
            Self::HandOver(origin) if origin.owner_press() => 2,
            Self::HandOver(_) => 1,
        }
    }
}

/// Who waits on a `spctl` run besides its [`Listing`]: the owner's presses answered
/// [`HandOver::Listing`], which must hear how it came out, whichever ask's run they rode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub(crate) digest: bool,
    pub(crate) settings: Vec<(crate::WindowId, crate::tab_model::ViewId)>,
}

#[derive(Clone, Debug)]
struct Queued {
    origin: Origin,
    at: Instant,
    next_check: Instant,
    listed: Option<Result<(), String>>,
    /// Another origin's ask the window's own step holds in its place: one hand-over waits per
    /// page and the step never lapses, so an ask that comes while it waits, or that it takes the
    /// place of, rides with it ([`Lane::take_rider`]).
    rider: Option<Rider>,
    /// The window's own step waits out a page brought up while no access revision stood
    /// ([`came_up`]), until a revision stands ([`Lane::revision_stands`]).
    kept: bool,
    /// When the ten minutes of a page the step waits out are up ([`came_up`],
    /// [`Lane::wait_out_page`]): it looks no sooner, whatever [`Self::next_check`] says, and
    /// a later page only lengthens the wait.
    page_wait: Option<Instant>,
}

/// An ask the window's own step carries ([`Queued::rider`]): who asked and when, and what the
/// `spctl` run it made answered, if one did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rider {
    pub(crate) origin: Origin,
    pub(crate) at: Instant,
    pub(crate) listed: Option<Result<(), String>>,
}

impl Queued {
    /// `origin`'s hand-over, asked at `at`, in the place of `waiting`: the window's own step
    /// carries the ask it takes the place of, or the one the step it replaces carried.
    fn in_place_of(
        waiting: Option<&Self>,
        origin: Origin,
        at: Instant,
        next_check: Instant,
        listed: Option<Result<(), String>>,
    ) -> Self {
        let rider = waiting
            .filter(|_| origin == Origin::Automatic)
            .and_then(|waiting| {
                if waiting.origin == Origin::Automatic {
                    waiting.rider.clone()
                } else {
                    Some(Rider {
                        origin: waiting.origin,
                        at: waiting.at,
                        listed: waiting.listed.clone(),
                    })
                }
            });
        // A step of its own that another replaces keeps the page it waits out, and kept, keeps.
        let own = waiting
            .filter(|waiting| origin == Origin::Automatic && waiting.origin == Origin::Automatic);
        Self {
            origin,
            at,
            next_check,
            listed,
            rider,
            kept: own.is_some_and(|waiting| waiting.kept),
            page_wait: own.and_then(|waiting| waiting.page_wait),
        }
    }

    /// When the hand-over looks next: its [`Self::next_check`], or once the page it waits out
    /// is ten minutes old ([`Self::page_wait`]), whichever is later.
    fn look_at(&self) -> Instant {
        self.page_wait
            .map_or(self.next_check, |end| end.max(self.next_check))
    }

    /// Wait out a page's ten minutes, up at `end`, unless one it waits out ends later.
    fn wait_out_page(&mut self, end: Instant) {
        self.page_wait = Some(self.page_wait.map_or(end, |held| held.max(end)));
    }

    /// Whether the window's own step waits here, which keeps its place: an ask from another
    /// origin (`rider`) then rides with it.
    fn carry(&mut self, rider: Rider) -> bool {
        if self.origin != Origin::Automatic {
            return false;
        }
        if rider.origin != Origin::Automatic {
            self.rider = Some(rider);
        }
        true
    }
}

/// A page came up (`Lane::page_came_up`): what waits in `slot` for it is answered, except the
/// window's own step while no access revision stands (`revision`). A page brought up then is
/// not that revision's (the Developer Tools record has no revision to take it, and
/// [`fda_opened_word`] keeps the revision's word), so the step waits on for the revision, with
/// no rider (the page was the ask's), and looks again no sooner than `look_after`
/// ([`Queued::page_wait`]). A page's caller passes [`past_its_ten_minutes`], taken once the page
/// came up: after its marker was written (the window writes the Full Disk Access page's), or,
/// for a control verb's `now`, after the window's answer, whose asker brings the Developer Tools
/// page up and writes its marker later. A Developer Tools marker still within its ten minutes at
/// that look holds the step on (`App::devtools_page_holds_the_step`), so neither page refuses
/// the step as [`Refusal::Recent`], which would leave the revision's step unasked in this
/// process.
fn came_up(slot: &mut Option<Queued>, revision: bool, look_after: Instant) {
    match slot.as_mut() {
        Some(queued) if !revision && queued.origin == Origin::Automatic => {
            queued.rider = None;
            queued.kept = true;
            queued.wait_out_page(look_after);
        }
        _ => *slot = None,
    }
}

/// `rider` rides with the window's own step waiting in `slot` again, unless the step carries an
/// ask asked after it (one waits, the newest, as [`Lane::queue`] keeps).
fn ride_on(slot: &mut Option<Queued>, rider: Rider) {
    if let Some(queued) = slot
        .as_mut()
        .filter(|queued| queued.origin == Origin::Automatic)
        && queued.rider.as_ref().is_none_or(|held| held.at <= rider.at)
    {
        queued.rider = Some(rider);
    }
}

/// `rider` unless it lapsed by `now` ([`Origin::lapses_after`], from when it was asked), as
/// `page`'s hand-over says in aterm.log when it does.
fn unlapsed(rider: Option<Rider>, now: Instant, page: &str) -> Option<Rider> {
    let rider = rider?;
    if rider
        .origin
        .lapses_after()
        .is_some_and(|after| now.saturating_duration_since(rider.at) >= after)
    {
        aterm_log::info!(
            "{page} hand-over ({}): asked {} minutes ago or more, it lapsed while it rode the \
             window's own step",
            rider.origin.as_str(),
            ASKED_LAPSE.as_secs() / 60
        );
        return None;
    }
    Some(rider)
}

/// A hand-over [`Lane::take_queued`] gives back at its quiet moment: who asked, and what the
/// `spctl` run it already made answered, if one did.
pub(crate) type TakenHandOver = (Origin, Option<Result<(), String>>);

/// How a Full Disk Access panel ended: why it closed, and whether the owner dropped aterm's icon
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FdaEnded {
    pub(crate) why: Closed,
    pub(crate) dropped: bool,
}

impl FdaEnded {
    /// Whether the owner acted on the page: anything but leaving the panel alone (Not now, or
    /// [`PANEL_FOR`] with nothing dropped).
    pub(crate) const fn owner_acted(self) -> bool {
        self.dropped || !matches!(self.why, Closed::NotNow | Closed::Lapsed)
    }
}

/// What a panel turn asks of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Asks {
    pub(crate) page: Page,
    pub(crate) granted: bool,
    pub(crate) recheck: bool,
    pub(crate) list_terminal: bool,
    pub(crate) open_status: bool,
}

/// The keys a panel's Not now is noted under, and the approved revision it is recorded
/// for: the window sets them before it shows a panel ([`Lane::set_marks`]). With no
/// approved revision the keys are the running version and nothing is recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Marks {
    /// `devtools-declined`'s key (`atpkg::devtools::revision_key`).
    pub(crate) devtools: String,
    /// `fda-declined`'s key: the revision's, with the bundle id and this Mac after it.
    pub(crate) fda: String,
    /// The approved setup revision a decline is recorded for in
    /// `<state>/system-state-access`.
    pub(crate) revision: Option<u64>,
}

impl Default for Marks {
    fn default() -> Self {
        let version = aterm_types::version::app_version().to_owned();
        Self {
            devtools: version.clone(),
            fda: version,
            revision: None,
        }
    }
}

/// The `App`-owned hand-over lane: the hand-over waiting for its quiet moment, the `spctl` worker,
/// the panel and its reads, and the latest reading.
#[derive(Debug)]
pub(crate) struct Lane {
    windowed: bool,
    born: Instant,
    settle: Duration,
    reach: Reach,
    fda: FdaReach,
    arm: DevToolsHelperArm,
    reading: Option<Reading>,
    queued: Option<Queued>,
    fda_queued: Option<Queued>,
    /// The ask each page's step carried when [`Self::take_queued`] (Developer Tools, first) or
    /// [`Self::take_queued_fda`] gave it back, until the window takes it.
    taken_riders: [Option<Rider>; 2],
    /// The ask the Developer Tools step carried when it went to its `spctl` run, until the run
    /// ends ([`Self::take_listing_rider`]).
    listing_rider: Option<Rider>,
    fda_ended: Option<FdaEnded>,
    listing: Option<Listing>,
    waiting: Waiting,
    panel: Option<Panel>,
    generation: u64,
    next_tick: Option<Instant>,
    shared: Arc<Mutex<Shared>>,
    opened: Option<Option<u64>>,
    declined: Option<Option<(u64, u64)>>,
    now_answered: Option<Instant>,
    refreshing: Arc<std::sync::atomic::AtomicBool>,
    refreshes: u32,
    /// The keys and revision a panel's Not now is noted under ([`Self::set_marks`]).
    marks: Marks,
    /// Not nows said under an approved revision, until the window records them
    /// ([`Self::take_declines`]).
    declines: Vec<(u64, Access)>,
}

impl Lane {
    /// The lane for an instance: live arms for a window on macOS, inert otherwise.
    pub(crate) fn new(headless: bool) -> Self {
        let windowed = !headless && cfg!(target_os = "macos");
        if windowed {
            Self::with_arms(
                true,
                Reach::live(),
                FdaReach::live(),
                DevToolsHelperArm::live(),
            )
        } else {
            Self::with_arms(
                false,
                Reach::inert(),
                FdaReach::inert(),
                DevToolsHelperArm::inert(),
            )
        }
    }

    fn with_arms(windowed: bool, reach: Reach, fda: FdaReach, arm: DevToolsHelperArm) -> Self {
        Self {
            windowed,
            born: Instant::now(),
            settle: crate::consent_card::LAUNCH_SETTLE,
            reach,
            fda,
            arm,
            reading: None,
            queued: None,
            fda_queued: None,
            taken_riders: [None, None],
            listing_rider: None,
            fda_ended: None,
            listing: None,
            waiting: Waiting::default(),
            panel: None,
            generation: 0,
            next_tick: None,
            shared: Arc::new(Mutex::new(Shared::default())),
            opened: None,
            declined: None,
            now_answered: None,
            refreshing: Arc::default(),
            refreshes: 0,
            marks: Marks::default(),
            declines: Vec::new(),
        }
    }

    /// A windowed lane with injected arms: the tests' stand-in for a window.
    #[cfg(test)]
    pub(crate) fn windowed_for_test(reach: Reach, arm: DevToolsHelperArm) -> Self {
        Self::with_arms(true, reach, FdaReach::inert(), arm)
    }

    /// A windowed lane with injected arms for both pages.
    #[cfg(test)]
    pub(crate) fn windowed_with_fda_for_test(
        reach: Reach,
        fda: FdaReach,
        arm: DevToolsHelperArm,
    ) -> Self {
        Self::with_arms(true, reach, fda, arm)
    }

    /// Whether this lane may act at all (a window on macOS).
    pub(crate) const fn windowed(&self) -> bool {
        self.windowed
    }

    /// The hand-over's reach.
    pub(crate) const fn reach(&self) -> Reach {
        self.reach
    }

    /// The Full Disk Access hand-over's reach.
    pub(crate) const fn fda_reach(&self) -> FdaReach {
        self.fda
    }

    /// How long ago this instance's lane was made (the App's construction).
    pub(crate) fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.born)
    }

    /// This process was started by the owner, not brought in by an update: its first quiet moment
    /// may come [`COLD_START_SETTLE`] after it was made.
    pub(crate) const fn note_cold_start(&mut self) {
        self.settle = COLD_START_SETTLE;
    }

    /// Whether this process has settled enough for a quiet moment ([`quiet`]).
    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.age(now) >= self.settle
    }

    /// The panel, when one is up.
    pub(crate) const fn panel(&self) -> Option<&Panel> {
        self.panel.as_ref()
    }

    /// The origin of the hand-over waiting for its quiet moment.
    pub(crate) fn queued(&self) -> Option<Origin> {
        self.queued.as_ref().map(|q| q.origin)
    }

    /// The latest reading.
    pub(crate) const fn reading(&self) -> Option<Reading> {
        self.reading
    }

    /// Forget a reading that only said the probe was switched off (`[privacy] enabled` or
    /// `check`): it read nothing, so the next completed reading takes its place
    /// (`App::resume_held_launch_steps`). `true` when one was forgotten.
    pub(crate) fn forget_switched_off_reading(&mut self) -> bool {
        let switched_off = self.reading.is_some_and(|reading| {
            reading.log.label == aterm_containment::consent::DevToolProbeLabel::RefusedDisabled
        });
        if switched_off {
            self.reading = None;
        }
        switched_off
    }

    /// When aterm last opened the page for the owner (unix seconds, the shared marker any aterm
    /// writes, `aterm doctor` included), read at most once between focus gains: the Settings
    /// card's "last opened" line.
    pub(crate) fn last_opened_at(&mut self) -> Option<u64> {
        let reach = self.reach;
        *self
            .opened
            .get_or_insert_with(|| (reach.last_opened)().map(|(_, at)| at))
    }

    /// This lane just wrote the marker at `at` (unix seconds; `None` when the clock could not be
    /// read, which reads the marker again).
    pub(crate) fn noted_opened(&mut self, at: Option<u64>) {
        self.opened = at.map(Some);
    }

    /// A focus gain: another process (`aterm doctor`) may have written the markers since, so the
    /// next [`Self::last_opened_at`] and [`Self::declined_marker`] read them again.
    pub(crate) const fn forget_opened(&mut self) {
        self.opened = None;
        self.declined = None;
    }

    /// The owner's Not now while it holds (when said, until when; unix seconds), read at most once
    /// between focus gains and this lane's own declines: the Settings card's line saying why aterm
    /// stays quiet.
    pub(crate) fn declined_marker(&mut self) -> Option<(u64, u64)> {
        let reach = self.reach;
        *self.declined.get_or_insert_with(|| (reach.declined)())
    }

    /// A completed reading. aterm reading on ends a waiting hand-over and turns the panel to its
    /// done words; any other reading gives the panel its tccd answers and, to a panel shown before
    /// any reading named the bundle id, the subject its list reads need and the icon to drag.
    pub(crate) fn note_reading(&mut self, reading: Reading, now: Instant) {
        self.reading = Some(reading);
        if reading.grant == DevToolGrant::Granted {
            self.queued = None;
        }
        let drag_bundle = self.reach.drag_bundle;
        if let Some(panel) = self
            .panel
            .as_mut()
            .filter(|panel| panel.page == Page::DeveloperTools)
        {
            let mut effect = panel.note_reading(&reading, now);
            if panel.wants_subject()
                && let Some(subject) = reading.subject
            {
                effect = effect.and(panel.learn_subject(subject, drag_bundle(Some(subject))));
            }
            self.redraw(effect);
        }
    }

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

    /// Hold a hand-over for its quiet moment. One waits: the window's own step keeps its place
    /// and carries another origin's ask ([`Self::take_rider`]); otherwise the newest ask waits.
    pub(crate) fn queue(&mut self, origin: Origin, now: Instant) {
        let rider = Rider {
            origin,
            at: now,
            listed: None,
        };
        if self
            .queued
            .as_mut()
            .is_some_and(|queued| queued.carry(rider))
        {
            return;
        }
        self.queued = Some(Queued::in_place_of(
            self.queued.as_ref(),
            origin,
            now,
            now,
            None,
        ));
    }

    /// The `spctl` run a hand-over the owner did not press made at its quiet moment ended at `now`,
    /// and the moment has passed: hold it for the next one with what the run answered, so an Ok
    /// run is not made again and the panel still says Terminal is aterm's doing.
    pub(crate) fn queue_after_listing(
        &mut self,
        origin: Origin,
        now: Instant,
        listed: Result<(), String>,
    ) {
        match self.queued.as_mut() {
            Some(queued) if queued.origin == Origin::Automatic && origin != Origin::Automatic => {
                queued.listed.get_or_insert_with(|| listed.clone());
                queued.carry(Rider {
                    origin,
                    at: now,
                    listed: Some(listed),
                });
            }
            _ => {
                self.queued = Some(Queued::in_place_of(
                    self.queued.as_ref(),
                    origin,
                    now,
                    now + RECHECK_EVERY,
                    Some(listed),
                ));
            }
        }
    }

    /// The window answered a control verb's `now` at `now`, with its panel or without it.
    pub(crate) const fn noted_now_answered(&mut self, now: Instant) {
        self.now_answered = Some(now);
    }

    /// Whether the window answered a `now` within [`atpkg::devtools::REOPEN_NOW_AFTER_S`] before
    /// `now`: the next one is `recent` whether or not its asker opened the page.
    pub(crate) fn now_answered_within_its_minute(&self, now: Instant) -> bool {
        self.now_answered.is_some_and(|at| {
            now.saturating_duration_since(at)
                < Duration::from_secs(atpkg::devtools::REOPEN_NOW_AFTER_S)
        })
    }

    /// The Developer Tools page, or its panel, came up: what waits for it is answered, except
    /// the window's own step while no access revision stands (`revision`), which waits on for
    /// the revision with no rider and looks again no sooner than `look_after` ([`came_up`]).
    pub(crate) fn page_came_up(&mut self, revision: bool, look_after: Instant) {
        came_up(&mut self.queued, revision, look_after);
    }

    /// Which pages' own steps wait out a page brought up while no access revision stood
    /// ([`came_up`]): Developer Tools first, then Full Disk Access.
    pub(crate) fn kept(&self) -> [bool; 2] {
        [&self.queued, &self.fda_queued].map(|slot| slot.as_ref().is_some_and(|q| q.kept))
    }

    /// An access revision stands at `now`, and `shown` says, for each page's kept step
    /// ([`Self::kept`]), whether it brought that page up already. Such a step looks at once,
    /// where it is refused as shown, rather than holding the steps behind it for the rest of
    /// the page's ten minutes; any other waits on as it was. Each is asked once.
    pub(crate) fn revision_stands(&mut self, now: Instant, shown: [bool; 2]) {
        for (slot, shown) in [&mut self.queued, &mut self.fda_queued]
            .into_iter()
            .zip(shown)
        {
            if let Some(queued) = slot.as_mut().filter(|queued| queued.kept) {
                queued.kept = false;
                if shown {
                    queued.page_wait = None;
                    queued.next_check = now;
                }
            }
        }
    }

    /// Whether a Full Disk Access hand-over or panel goes first: a Developer Tools one waits behind
    /// it (that list cannot be read without Full Disk Access, and one System Settings window shows
    /// one page).
    pub(crate) fn fda_first(&self) -> bool {
        self.fda_queued.is_some()
            || self
                .panel
                .as_ref()
                .is_some_and(|panel| panel.page == Page::FullDiskAccess)
    }

    /// Whether the waiting hand-over's next look at the moment is due.
    pub(crate) fn hand_over_due(&self, now: Instant) -> bool {
        !self.fda_first() && self.queued.as_ref().is_some_and(|q| now >= q.look_at())
    }

    /// Hold the Full Disk Access hand-over for its quiet moment. One waits: the window's own
    /// step keeps its place and carries another origin's ask ([`Self::take_fda_rider`]);
    /// otherwise the first ask waits.
    pub(crate) fn queue_fda(&mut self, origin: Origin, now: Instant) {
        if self.fda_queued.as_mut().is_some_and(|queued| {
            queued.carry(Rider {
                origin,
                at: now,
                listed: None,
            }) || origin != Origin::Automatic
        }) {
            return;
        }
        self.fda_queued = Some(Queued::in_place_of(
            self.fda_queued.as_ref(),
            origin,
            now,
            now,
            None,
        ));
    }

    /// The origin of the Full Disk Access hand-over waiting for its quiet moment.
    #[cfg(test)]
    pub(crate) fn fda_queued(&self) -> Option<Origin> {
        self.fda_queued.as_ref().map(|q| q.origin)
    }

    /// The owner's press on the panel that is up, as the live arm posts it.
    #[cfg(test)]
    pub(crate) fn press_for_test(&self, event: PanelEvent) {
        self.shared
            .lock_or_recover()
            .events
            .push((self.generation, event));
    }

    /// Whether a Full Disk Access hand-over waits for its quiet moment.
    pub(crate) const fn fda_waiting(&self) -> bool {
        self.fda_queued.is_some()
    }

    /// The grant reads held: nothing waits for the Full Disk Access page any longer.
    pub(crate) fn drop_queued_fda(&mut self) {
        self.fda_queued = None;
    }

    /// [`Self::page_came_up`] for the Full Disk Access page.
    pub(crate) fn fda_page_came_up(&mut self, revision: bool, look_after: Instant) {
        came_up(&mut self.fda_queued, revision, look_after);
    }

    /// Whether the waiting Full Disk Access hand-over's next look at the moment is due.
    pub(crate) fn fda_hand_over_due(&self, now: Instant) -> bool {
        self.fda_queued.as_ref().is_some_and(|q| now >= q.look_at())
    }

    /// The probe the waiting Full Disk Access hand-over judges by is being read again: look at
    /// `at`, not now (a look on a pending read would refuse it as unknown), and no sooner than a
    /// page it waits out allows.
    pub(crate) const fn defer_fda(&mut self, at: Instant) {
        if let Some(queued) = self.fda_queued.as_mut() {
            queued.next_check = at;
        }
    }

    /// The waiting Full Disk Access hand-over waits out a page up until `end` (the Developer
    /// Tools page's ten minutes, or a new access revision's wait behind its own page,
    /// `App::rearm_fda_step`): it looks no sooner, and a nearer `end` changes nothing.
    pub(crate) fn fda_wait_out_page(&mut self, end: Instant) {
        if let Some(queued) = self.fda_queued.as_mut() {
            queued.wait_out_page(end);
        }
    }

    /// The waiting Developer Tools hand-over looks at `at`, not before, and no sooner than a
    /// page it waits out allows: the window's own Full Disk Access reading is still out
    /// (`App::hold_devtools_for_fda_reading`).
    pub(crate) fn defer(&mut self, at: Instant) {
        if let Some(queued) = self.queued.as_mut() {
            queued.next_check = at;
        }
    }

    /// The waiting Developer Tools hand-over waits out a page up until `end`: aterm brought the
    /// page up within [`atpkg::devtools::REOPEN_AFTER_S`] and a new access revision's page
    /// waits until those ten minutes are up (`App::rearm_devtools_step`), or a page the step
    /// already waits out is younger than it allowed for (`App::devtools_page_holds_the_step`).
    /// It looks no sooner, and a nearer `end` changes nothing.
    pub(crate) fn wait_out_page(&mut self, end: Instant) {
        if let Some(queued) = self.queued.as_mut() {
            queued.wait_out_page(end);
        }
    }

    /// Whether the window's own Developer Tools step waits out a page ([`Queued::page_wait`]).
    pub(crate) fn waits_out_a_page(&self) -> bool {
        self.queued
            .as_ref()
            .is_some_and(|q| q.origin == Origin::Automatic && q.page_wait.is_some())
    }

    /// Whether a Developer Tools hand-over waits for its quiet moment, or its `spctl` run is out.
    pub(crate) const fn devtools_waiting(&self) -> bool {
        self.queued.is_some() || self.listing.is_some()
    }

    /// The waiting Full Disk Access hand-over, when `quiet` and no panel is up (an owner's
    /// Developer Tools panel is not taken away from them); otherwise it looks again
    /// [`RECHECK_EVERY`] later (no sooner than a page it waits out allows), or (a control
    /// client's act, after [`ASKED_LAPSE`]) lapses.
    pub(crate) fn take_queued_fda(&mut self, now: Instant, quiet: bool) -> Option<Origin> {
        let panel_up = self.panel.is_some();
        let queued = self.fda_queued.as_mut()?;
        if queued
            .origin
            .lapses_after()
            .is_some_and(|after| now.saturating_duration_since(queued.at) >= after)
        {
            aterm_log::info!(
                "full disk access hand-over ({}): no step moment came in {} minutes; it lapsed",
                queued.origin.as_str(),
                ASKED_LAPSE.as_secs() / 60
            );
            self.fda_queued = None;
            return None;
        }
        if quiet && !panel_up {
            let taken = self.fda_queued.take()?;
            self.taken_riders[1] = taken.rider;
            return Some(taken.origin);
        }
        queued.next_check = now + RECHECK_EVERY;
        None
    }

    /// The window's own Full Disk Access probe completed with `fda`: a grant ends a waiting
    /// hand-over and turns a Full Disk Access panel to its done words.
    pub(crate) fn note_fda(&mut self, fda: FdaState, now: Instant) {
        if fda != FdaState::Granted {
            return;
        }
        self.fda_queued = None;
        if let Some(panel) = self
            .panel
            .as_mut()
            .filter(|panel| panel.page == Page::FullDiskAccess)
        {
            let effect = panel.granted(now);
            self.redraw(effect);
        }
    }

    /// How the last Full Disk Access panel ended, once: the window's launch warm-up takes it.
    pub(crate) const fn take_fda_ended(&mut self) -> Option<FdaEnded> {
        self.fda_ended.take()
    }

    /// The waiting hand-over, when `quiet` (the caller judged [`quiet`] at `now`), with what its
    /// earlier `spctl` run answered; otherwise it looks again [`RECHECK_EVERY`] later (no sooner
    /// than a page it waits out allows), or lapses.
    pub(crate) fn take_queued(&mut self, now: Instant, quiet: bool) -> Option<TakenHandOver> {
        let fda_first = self.fda_first();
        let queued = self.queued.as_mut()?;
        if fda_first {
            queued.next_check = now + RECHECK_EVERY;
            return None;
        }
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
            let taken = self.queued.take()?;
            self.taken_riders[0] = taken.rider;
            return Some((taken.origin, taken.listed));
        }
        queued.next_check = now + RECHECK_EVERY;
        None
    }

    /// The ask the Developer Tools step [`Self::take_queued`] last gave back carried, unless it
    /// lapsed by `now`: the window judges it by its own rules once the step is refused, since a
    /// refusal of the step (a page this revision already brought up) need not be the ask's.
    pub(crate) fn take_rider(&mut self, now: Instant) -> Option<Rider> {
        unlapsed(self.taken_riders[0].take(), now, "devtools")
    }

    /// The control verb's ask the waiting Developer Tools step carries, taken at that ask's own
    /// quiet moment while the step waits on for its step moment: the verb keeps the quiet moment
    /// alone ([`Origin::system_state_step`]), so riding the step never makes it wait for an
    /// approved revision or System State in front. A System State step's own rider waits on.
    pub(crate) fn take_quiet_rider(&mut self, now: Instant) -> Option<Rider> {
        let queued = self.queued.as_mut()?;
        if queued
            .rider
            .as_ref()
            .is_none_or(|rider| rider.origin.system_state_step())
        {
            return None;
        }
        unlapsed(queued.rider.take(), now, "devtools")
    }

    /// [`Self::take_rider`] for the Full Disk Access step [`Self::take_queued_fda`] gave back.
    pub(crate) fn take_fda_rider(&mut self, now: Instant) -> Option<Rider> {
        unlapsed(self.taken_riders[1].take(), now, "full disk access")
    }

    /// The step a [`Self::take_rider`] came from waits again: `rider` rides with it, unless it
    /// carries a newer ask already (one waits, the newest).
    pub(crate) fn ride(&mut self, rider: Rider) {
        ride_on(&mut self.queued, rider);
    }

    /// [`Self::ride`] for the Full Disk Access step.
    pub(crate) fn ride_fda(&mut self, rider: Rider) {
        ride_on(&mut self.fda_queued, rider);
    }

    /// The step a [`Self::take_rider`] came from went to its `spctl` run: `rider` waits for the
    /// run's end ([`Self::take_listing_rider`]), then for what the step makes of it.
    pub(crate) fn ride_listing(&mut self, rider: Rider) {
        self.listing_rider = Some(rider);
    }

    /// The ask that waited for the `spctl` run [`Self::take_listed`] gave back the end of, unless
    /// it lapsed by `now`.
    pub(crate) fn take_listing_rider(&mut self, now: Instant) -> Option<Rider> {
        unlapsed(self.listing_rider.take(), now, "devtools")
    }

    /// Start `spctl developer-mode enable-terminal` on a worker for `listing`.
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
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-devtools-terminal".to_owned(),
            crate::qos::Role::Responsive,
            move || {
                let listed = list_terminal();
                shared.lock_or_recover().listed = Some(listed);
                wake();
            },
        );
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

    /// Make macOS ask about aterm again once the owner switched it on ([`Reach::refresh_verdict`],
    /// `atpkg::devtools::refresh_verdict`): syspolicyd keeps the verdict it reached for this
    /// process before the switch went on until an aterm process it has not checked starts a new
    /// program (measured).
    pub(crate) fn refresh_verdict(&mut self) -> bool {
        use std::sync::atomic::Ordering;
        if !self.windowed || self.refreshing.swap(true, Ordering::AcqRel) {
            return false;
        }
        let refresh = self.reach.refresh_verdict;
        let refreshing = Arc::clone(&self.refreshing);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-devtools-refresh",
            crate::qos::Role::Background,
            move || {
                match refresh() {
                    Ok(()) => aterm_log::info!(
                        "devtools: a fresh aterm process ran a new program, so macOS asked \
                         about aterm again"
                    ),
                    Err(e) => aterm_log::warn!("devtools: asking macOS about aterm again: {e}"),
                }
                refreshing.store(false, Ordering::Release);
            },
        );
        if let Err(e) = spawned {
            self.refreshing.store(false, Ordering::Release);
            aterm_log::warn!("devtools: the refresh worker could not start: {e}");
            return false;
        }
        self.refreshes += 1;
        true
    }

    /// How many verdict refreshes this lane started ([`Self::refresh_verdict`]).
    #[cfg(test)]
    pub(crate) const fn refreshes_started(&self) -> u32 {
        self.refreshes
    }

    fn note_waiting(&mut self, listing: Listing) {
        if listing == Listing::HandOver(Origin::Digest) {
            self.waiting.digest = true;
        }
    }

    /// The Settings card's press from `views` was answered [`HandOver::Listing`]: their bars hear
    /// the run's end.
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

    /// A `spctl` run that ended: whose end it is, what it answered, and the owner's presses that
    /// waited on it.
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
        if let Some(panel) = self
            .panel
            .as_mut()
            .filter(|panel| panel.page == Page::DeveloperTools)
        {
            let effect = panel.on_listed(listed);
            self.redraw(effect);
        }
    }

    /// Show the panel (one at a time: an earlier one closes).
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
            "{}: the helper panel is up ({}{})",
            panel.page.as_str(),
            panel.origin().as_str(),
            if panel.page == Page::DeveloperTools && !panel.readable() {
                ", unread"
            } else {
                ""
            }
        );
        self.panel = Some(panel);
        self.next_tick = Some(now);
        true
    }

    /// Set the [`Marks`] a panel's Not now is noted under from now on.
    pub(crate) fn set_marks(&mut self, marks: Marks) {
        self.marks = marks;
    }

    /// The Not nows said under an approved revision since the last take, for the window to
    /// record as each row's `declined` (`<state>/system-state-access`).
    pub(crate) fn take_declines(&mut self) -> Vec<(u64, Access)> {
        std::mem::take(&mut self.declines)
    }

    fn close(&mut self, why: Closed) {
        let Some(panel) = self.panel.take() else {
            return;
        };
        (self.arm.close)();
        self.next_tick = None;
        if why.declines()
            && let Some(revision) = self.marks.revision
        {
            self.declines.push((revision, panel.page.access()));
        }
        match panel.page {
            Page::DeveloperTools => {
                if why.declines() {
                    (self.reach.note_declined)(&self.marks.devtools);
                    self.declined = None;
                }
            }
            Page::FullDiskAccess => {
                if why.declines() {
                    (self.fda.note_declined)(&self.marks.fda);
                }
                self.fda_ended = Some(FdaEnded {
                    why,
                    dropped: panel.dropped(),
                });
            }
        }
        aterm_log::info!(
            "{}: the helper panel closed ({why:?}, {:?})",
            panel.page.as_str(),
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

    /// The panel's turn at the park: the owner's presses, a list read that ended, and, every
    /// [`PANEL_TICK`], placement and System Settings still running; then the done words' time and
    /// the ten-minute bound; then, at that cadence (or at once after a drop), one list read on a
    /// worker when the panel wants one, System Settings is frontmost and `read` names the gate
    /// (`None`: `[privacy] observer = false`, which reads nothing).
    pub(crate) fn tick_panel(
        &mut self,
        now: Instant,
        read: Option<ProbeGate>,
        wake: &Wake,
    ) -> Asks {
        let Some(page) = self.panel.as_ref().map(Panel::page) else {
            return Asks::default();
        };
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
            page,
            granted: effect.granted,
            recheck: effect.recheck,
            list_terminal: effect.list_terminal,
            open_status: effect.open_status,
        };
        self.redraw(effect);
        asks
    }

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
        if !(self.arm.settings_frontmost)() || (self.arm.settings_busy)() {
            panel.last_read.get_or_insert(now);
            return;
        }
        let read_list = self.reach.read_list;
        let shared = Arc::clone(&self.shared);
        let generation = self.generation;
        let wake = Arc::clone(wake);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-devtools-list".to_owned(),
            crate::qos::Role::Responsive,
            move || {
                let list = read_list(gate, subject);
                {
                    let mut shared = shared.lock_or_recover();
                    if shared.generation == generation {
                        shared.list = Some((generation, list));
                    }
                }
                wake();
            },
        );
        match spawned {
            Ok(_) => panel.read_out = true,
            Err(e) => aterm_log::warn!("devtools: the list read could not start: {e}"),
        }
    }

    /// The park's deadline for this lane: the waiting hand-over's next look, only while `focused`
    /// says an aterm window is (a focus gain wakes the loop); the panel's next turn; and, while a
    /// worker is out, a re-read of its slot whatever its wake did.
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
        let devtools = self.queued.as_ref().filter(|_| !self.fda_first());
        let waiting = [devtools, self.fda_queued.as_ref()];
        if waiting.iter().any(Option::is_some) && focused() {
            for queued in waiting.into_iter().flatten() {
                fold(queued.look_at());
            }
        }
        soonest
    }
}

/// The panel itself, on AppKit: the [`DevToolsHelperArm::live`] arm.
#[cfg(target_os = "macos")]
pub(crate) mod live {
    use std::borrow::Cow;
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use aterm_objc::{Bool, CGPoint, CGRect, CGSize, Id, Obj, Retained, Sel, class, sel};

    use super::{
        Block, Button, Content, Cover, Drift, ICON, Measure, PANEL_W, PanelEvent, PanelEvents,
        Part, PlaceTurn, Rect, SettingsWindow, TILE_CAPTION, Tone, cg_to_cocoa, cover_turn, drift,
        drop_landed, layout, may_reveal, place, place_turn, settings_busy, settings_main,
        under_panel,
    };
    use crate::appkit::{self, MainThread, consts};

    const SETTINGS_ID: &str = "com.apple.systempreferences";

    const WHAT: &str = "the Developer Tools helper panel";

    const TITLE: &str = "aterm";

    pub(crate) const REVEAL_WAIT: Duration = Duration::from_secs(3);

    const REVEAL_RETRY_SECS: f64 = 0.25;

    pub(crate) const DRAG_SLOP: f64 = 4.0;

    const DRAG_ALPHA: f64 = 0.72;

    const SPENT_ALPHA: f64 = 0.4;

    const HOVER_RADIUS: f64 = 12.0;

    const HOVER_WASH_ALPHA: f64 = 0.18;

    const BUTTON_PAD: f64 = 8.0;

    pub(crate) const TYPE_FILE_URL: &str = "public.file-url";
    pub(crate) const TYPE_URL: &str = "public.url";
    pub(crate) const TYPE_TEXT: &str = "public.utf8-plain-text";
    pub(crate) const TYPE_FILENAMES: &str = "NSFilenamesPboardType";

    pub(crate) const WINDOW_KEYS: [&str; 4] = [
        "kCGWindowOwnerPID",
        "kCGWindowLayer",
        "kCGWindowAlpha",
        "kCGWindowBounds",
    ];

    pub(crate) const WORKSPACE_NAMES: [&str; 2] = [
        "NSWorkspaceDidActivateApplicationNotification",
        "NSWorkspaceApplicationKey",
    ];

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> Id;
        fn CGRectMakeWithDictionaryRepresentation(dict: Id, rect: *mut CGRect) -> bool;
    }

    thread_local! {
        static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
        static LINGER: RefCell<Option<Retained<TileView>>> = const { RefCell::new(None) };
    }

    struct Live {
        panel: Obj,
        target: Retained<HelperTarget>,
        content: Retained<ContentView>,
        tile: Option<Retained<TileView>>,
        events: PanelEvents,
        settings_front: bool,
        visible: bool,
        reveal_since: Option<Instant>,
        placed: Option<CGRect>,
        placed_on: Vec<Rect>,
        moved: bool,
        beside: bool,
        dragging: bool,
        pending: Option<Content>,
        yielded: bool,
        busy: bool,
    }

    fn with_live<R>(f: impl FnOnce(&mut Live) -> R) -> Option<R> {
        LIVE.with(|slot| {
            let mut slot = slot.try_borrow_mut().ok()?;
            slot.as_mut().map(f)
        })
    }

    const fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
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

    const fn from_cg(r: CGRect) -> Rect {
        Rect {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.width,
            h: r.size.height,
        }
    }

    fn same_frame(a: CGRect, b: CGRect) -> bool {
        (a.origin.x - b.origin.x).abs() < 0.5
            && (a.origin.y - b.origin.y).abs() < 0.5
            && (a.size.width - b.size.width).abs() < 0.5
            && (a.size.height - b.size.height).abs() < 0.5
    }

    pub(crate) struct TargetIvars {
        events: PanelEvents,
    }

    aterm_objc::declare_class! {
        pub(crate) struct HelperTarget: NSObject {
            const NAME: &str = "ATermDevToolsHelperTarget";
            type Ivars = TargetIvars;
            protocols: [NSObject, NSWindowDelegate];

            @sel(press:)
            fn press(&self, sender: Id) {
                // SAFETY: `sender` is the live NSButton AppKit passes its action; `-tag` is
                // `-(NSInteger)`.
                let tag = if sender.is_null() {
                    -1
                } else {
                    unsafe { appkit::send_isize(sender, sel!(tag)) }
                };
                if let Some(button) = Button::from_tag(tag) {
                    self.ivars().events.post(button.event());
                }
            }

            @sel(openStatus:)
            fn openStatus(&self, _sender: Id) {
                // SAFETY: main thread, inside AppKit's action dispatch.
                unsafe { activate_aterm() };
                self.ivars().events.post(PanelEvent::Status);
            }

            @sel(windowShouldClose:)
            fn windowShouldClose(&self, _window: Id) -> Bool {
                self.ivars().events.post(PanelEvent::NotNow);
                Bool::NO
            }

            @sel(workspaceDidActivate:)
            fn workspaceDidActivate(&self, note: Id) {
                // SAFETY: `note` is the live NSNotification the workspace centre delivers, on the
                // main thread.
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

            @sel(revealSoon:)
            fn revealSoon(&self, _unused: Id) {
                let _ = with_live(reveal);
            }
        }
    }

    aterm_objc::declare_class! {
        pub(crate) struct ContentView: NSView {
            const NAME: &str = "ATermDevToolsHelperContent";
            type Ivars = ();
            protocols: [NSObject];

            @sel(isFlipped) @abort_on_exception
            fn isFlipped(&self) -> Bool {
                Bool::YES
            }

            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: inside AppKit's draw cycle on the main thread; `+windowBackgroundColor`
                // is +0, `-set` is `-(void)`, `-bounds` `-(NSRect)` and `+fillRect:`
                // `+(void)(NSRect)`.
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

            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> Bool {
                Bool::YES
            }
        }
    }

    aterm_objc::declare_class! {
        pub(crate) struct HelperButton: NSButton {
            const NAME: &str = "ATermDevToolsHelperButton";
            type Ivars = ();
            protocols: [NSObject];

            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> Bool {
                Bool::YES
            }
        }
    }

    pub(crate) struct TileIvars {
        bundle: PathBuf,
        icon: Obj,
        live: Cell<bool>,
        down: Cell<Option<CGPoint>>,
        began: Cell<bool>,
        hover: Cell<bool>,
        events: PanelEvents,
    }

    aterm_objc::declare_class! {
        pub(crate) struct TileView: NSView {
            const NAME: &str = "ATermDevToolsHelperTile";
            type Ivars = TileIvars;
            protocols: [NSObject, NSDraggingSource];

            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                let ivars = self.ivars();
                // SAFETY: inside AppKit's draw cycle on the main thread; the icon is the live
                // NSImage the ivars own; `-drawInRect:` is `-(void)(NSRect)`; the colour and path
                // factories are +0 and non-raising, and `-set`, `-fill`, `-stroke` `-(void)`,
                // `-setLineWidth:` `-(void)(CGFloat)`.
                unsafe {
                    let bounds = appkit::send_rect(self.as_id(), sel!(bounds));
                    if ivars.hover.get() && ivars.live.get() {
                        let accent = appkit::send_id(
                            class(c"NSColor").as_id(),
                            sel!(controlAccentColor),
                        );
                        if !accent.is_null() {
                            let ring = CGRect {
                                origin: CGPoint {
                                    x: bounds.origin.x + 1.0,
                                    y: bounds.origin.y + 1.0,
                                },
                                size: CGSize {
                                    width: bounds.size.width - 2.0,
                                    height: bounds.size.height - 2.0,
                                },
                            };
                            let path = appkit::send_id_rect_f64_f64(
                                class(c"NSBezierPath").as_id(),
                                sel!(bezierPathWithRoundedRect:xRadius:yRadius:),
                                ring,
                                HOVER_RADIUS,
                                HOVER_RADIUS,
                            );
                            let wash = appkit::send_id_f64(
                                accent,
                                sel!(colorWithAlphaComponent:),
                                HOVER_WASH_ALPHA,
                            );
                            if !path.is_null() && !wash.is_null() {
                                appkit::send_v(wash, sel!(set));
                                appkit::send_v(path, sel!(fill));
                                appkit::send_v(accent, sel!(set));
                                appkit::send_v_f64(path, sel!(setLineWidth:), 2.0);
                                appkit::send_v(path, sel!(stroke));
                            }
                        }
                    }
                    appkit::send_v_rect(ivars.icon.id(), sel!(drawInRect:), bounds);
                }
            }

            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> Bool {
                Bool::YES
            }

            @sel(mouseDown:)
            fn mouseDown(&self, event: Id) {
                // SAFETY: `event` is the live NSEvent AppKit passes.
                let at = unsafe { point_in(self.as_id(), event) };
                self.ivars().down.set(Some(at));
                self.ivars().began.set(false);
            }

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
                // SAFETY: inside AppKit's `mouseDragged:` dispatch on the main thread, with the
                // event that dispatch passed.
                if unsafe { begin_drag(self, event) } {
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

            @sel(mouseUp:)
            fn mouseUp(&self, _event: Id) {
                self.ivars().down.set(None);
                self.ivars().began.set(false);
            }

            @sel(mouseEntered:)
            fn mouseEntered(&self, _event: Id) {
                self.set_hover(self.ivars().live.get());
            }

            @sel(mouseExited:)
            fn mouseExited(&self, _event: Id) {
                self.set_hover(false);
            }

            @sel(draggingSession:sourceOperationMaskForDraggingContext:)
            fn draggingSessionSourceOperationMask(&self, _session: Id, _context: isize) -> usize {
                consts::NS_DRAG_OPERATION_COPY
            }

            @sel(ignoreModifierKeysForDraggingSession:) @abort_on_exception
            fn ignoreModifierKeysForDraggingSession(&self, _session: Id) -> Bool {
                Bool::YES
            }

            @sel(draggingSession:willBeginAtPoint:)
            fn draggingSessionWillBegin(&self, _session: Id, _at: CGPoint) {
                drag_began(self.as_id());
            }

            @sel(draggingSession:endedAtPoint:operation:)
            fn draggingSessionEnded(&self, _session: Id, at: CGPoint, operation: usize) {
                // SAFETY: `self` is the live tile AppKit sent this to.
                let keep = unsafe { Obj::retain(self.as_id()) };
                let accepted = operation != consts::NS_DRAG_OPERATION_NONE;
                // SAFETY: main thread; read-only window and screen queries.
                let outside = accepted && !unsafe { dropped_in_settings(self.as_id(), at) };
                let taken = accepted && !outside;
                let ivars = self.ivars();
                ivars.down.set(None);
                ivars.began.set(false);
                self.set_hover(false);
                if taken {
                    self.set_live(false);
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
            // SAFETY: main thread; the target is the live object the observer registration names,
            // kept by `Live` until `close` removes it.
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
                yielded: false,
                busy: false,
            };
            let refused = LIVE.with(|slot| match slot.try_borrow_mut() {
                Ok(mut slot) => {
                    *slot = Some(live);
                    None
                }
                Err(_) => Some(live),
            });
            if let Some(live) = refused {
                // SAFETY: main thread; `live` still holds the target, released only after the
                // centre lets it go.
                unsafe { stop_observing(live.target.as_id()) };
                return false;
            }
            if settings_front {
                let _ = with_live(reveal);
            }
            true
        })
    }

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

    pub(crate) fn place_beside_settings() {
        if MainThread::new().is_none() {
            return;
        }
        aterm_objc::autoreleasepool(|_| {
            let _ = with_live(|live| {
                if live.dragging {
                    return;
                }
                if !live.visible && !live.yielded && !(live.settings_front && live.beside) {
                    return;
                }
                // SAFETY: main thread; the window and screen reads are read-only.
                let screens = unsafe { screens() };
                // SAFETY: as above.
                let windows = unsafe { settings_windows(&screens) };
                // SAFETY: main thread; reads the live panel's frame and level.
                let (frame, level) = unsafe { frame_and_level(live) };
                live.busy = settings_busy(&windows, level);
                let window = settings_main(&windows).map(|i| windows[i].bounds);
                let under = under_panel(&windows, frame, level);
                match cover_turn(live.visible, live.yielded, live.settings_front, under) {
                    Cover::Out => {
                        hide(live);
                        live.yielded = true;
                        aterm_log::info!(
                            "devtools: System Settings' password window is under the helper \
                             panel; it steps aside"
                        );
                        return;
                    }
                    Cover::Back => {
                        live.reveal_since = None;
                        // SAFETY: main thread; the target is the live one `Live` holds.
                        unsafe {
                            cancel_reveal_soon(live.target.as_id());
                            place_now(live, &screens, window);
                        }
                        order_in(live);
                        return;
                    }
                    Cover::Nothing if live.yielded => return,
                    Cover::Nothing => {}
                }
                if live.moved {
                    return;
                }
                note_owner_move(live);
                if live.moved {
                    return;
                }
                match place_turn(live.visible, live.settings_front, live.beside, window) {
                    PlaceTurn::Nothing => {}
                    PlaceTurn::Hide => hide(live),
                    // SAFETY: main thread.
                    PlaceTurn::Place(window) => unsafe { place_now(live, &screens, window) },
                    PlaceTurn::Return(window) => {
                        live.reveal_since = None;
                        // SAFETY: main thread; the target is the live one `Live` holds.
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
            // SAFETY: main thread; the target is live until `live` drops below. `-setDelegate:` and
            // `-orderOut:` are `-(void)(id)` on the live panel, nil meaning "none" and "no sender".
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

    pub(crate) fn settings_running() -> bool {
        // SAFETY: read-only NSRunningApplication queries.
        aterm_objc::autoreleasepool(|_| unsafe { settings_pid() }.is_some())
    }

    pub(crate) fn settings_frontmost() -> bool {
        LIVE.with(|slot| {
            slot.try_borrow()
                .ok()
                .and_then(|slot| slot.as_ref().map(|live| live.settings_front))
        })
        .unwrap_or(false)
    }

    pub(crate) fn settings_busy_now() -> bool {
        LIVE.with(|slot| {
            slot.try_borrow()
                .ok()
                .and_then(|slot| slot.as_ref().map(|live| live.busy))
        })
        .unwrap_or(false)
    }

    fn reveal(live: &mut Live) {
        if !may_reveal(live.settings_front, live.visible, live.dragging) {
            return;
        }
        let now = Instant::now();
        let since = *live.reveal_since.get_or_insert(now);
        // SAFETY: main thread; read-only window and screen queries.
        let screens = unsafe { screens() };
        // SAFETY: as above.
        let windows = unsafe { settings_windows(&screens) };
        let settings = settings_main(&windows).map(|i| windows[i].bounds);
        if settings.is_none() && live.beside {
            live.reveal_since = None;
            return;
        }
        if settings.is_none() && now.saturating_duration_since(since) < REVEAL_WAIT {
            // SAFETY: main thread; the target lives as long as the panel, and `close` cancels what
            // this schedules.
            unsafe { reveal_soon(live.target.as_id()) };
            return;
        }
        live.reveal_since = None;
        // SAFETY: as above.
        unsafe { place_now(live, &screens, settings) };
        // SAFETY: main thread; reads the live panel's frame and level.
        let (frame, level) = unsafe { frame_and_level(live) };
        live.busy = settings_busy(&windows, level);
        if under_panel(&windows, frame, level) {
            live.yielded = true;
            return;
        }
        order_in(live);
    }

    /// # Safety
    /// Main thread.
    unsafe fn frame_and_level(live: &Live) -> (Rect, isize) {
        let panel = live.panel.id();
        // SAFETY: main thread; `-frame` is `-(NSRect)` and `-level` `-(NSInteger)` on the live
        // panel.
        unsafe {
            (
                from_cg(appkit::send_rect(panel, sel!(frame))),
                appkit::send_isize(panel, sel!(level)),
            )
        }
    }

    fn order_in(live: &mut Live) {
        note_owner_move(live);
        let panel = live.panel.id();
        live.visible = crate::menu::order_front_regardless(panel, WHAT);
        if live.visible {
            live.yielded = false;
        }
        if live.visible && live.placed.is_some() && !live.moved {
            // SAFETY: main thread (`Live` lives on it); read-only screen queries.
            unsafe { note_placed(live, &screens()) };
        }
    }

    /// # Safety
    /// Main thread.
    unsafe fn note_placed(live: &mut Live, screens: &Screens) {
        // SAFETY: main thread; `-frame` is `-(NSRect)` on the live panel.
        live.placed = Some(unsafe { appkit::send_rect(live.panel.id(), sel!(frame)) });
        live.placed_on.clone_from(&screens.frames);
    }

    fn note_owner_move(live: &mut Live) {
        if live.moved || !live.visible {
            return;
        }
        let Some(placed) = live.placed else {
            return;
        };
        // SAFETY: main thread (`Live` lives on it); `-frame` is `-(NSRect)` on the live panel, and
        // the screen queries are read-only.
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
        if let Some(tile) = &live.tile {
            tile.set_hover(false);
        }
        live.visible = false;
    }

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

    struct Screens {
        visible: Vec<Rect>,
        frames: Vec<Rect>,
        primary_h: f64,
    }

    /// # Safety
    /// Main thread.
    unsafe fn screens() -> Screens {
        let mut visible = Vec::new();
        let mut frames = Vec::new();
        let mut primary_h = 0.0;
        // SAFETY: `+screens` is `+(NSArray *)`, +0; `-count`/`-objectAtIndex:` read it;
        // `-visibleFrame` and `-frame` are `-(NSRect)` on each screen.
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

    /// # Safety
    /// Any thread; read-only NSRunningApplication queries.
    unsafe fn settings_pid() -> Option<i32> {
        let id = appkit::nsstring(SETTINGS_ID)?;
        // SAFETY: `+runningApplicationsWithBundleIdentifier:` is `+(NSArray *)(NSString *)`, +0;
        // `-activationPolicy` is `-(NSInteger)` and `-processIdentifier` `-(pid_t)` on each
        // element.
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

    /// # Safety
    /// Main thread.
    unsafe fn settings_window(screens: &Screens) -> Option<Rect> {
        // SAFETY: the caller's main thread; read-only queries.
        let windows = unsafe { settings_windows(screens) };
        settings_main(&windows).map(|i| windows[i].bounds)
    }

    /// # Safety
    /// Main thread.
    unsafe fn settings_windows(screens: &Screens) -> Vec<SettingsWindow> {
        // SAFETY: read-only, as `settings_pid` says.
        let Some(pid) = (unsafe { settings_pid() }) else {
            return Vec::new();
        };
        let [pid_key, layer_key, alpha_key, bounds_key] = WINDOW_KEYS.map(appkit::nsstring);
        let (Some(pid_key), Some(layer_key), Some(alpha_key), Some(bounds_key)) =
            (pid_key, layer_key, alpha_key, bounds_key)
        else {
            return Vec::new();
        };
        // SAFETY: `CGWindowListCopyWindowInfo` returns a +1 CFArray (the Copy rule), adopted here
        // and released once; toll-free bridging makes it an NSArray of NSDictionaries, read with
        // `-count`, `-objectAtIndex:` and `-objectForKey:` (+0 each), the numbers with
        // `-integerValue`/`-doubleValue`, and the bounds dictionary turned back into a CGRect by
        // its own CoreGraphics reader into a local.
        unsafe {
            let Some(list) = Obj::from_owned(CGWindowListCopyWindowInfo(
                consts::CG_WINDOW_LIST_ON_SCREEN_NON_DESKTOP,
                consts::CG_NULL_WINDOW_ID,
            )) else {
                return Vec::new();
            };
            let mut windows = Vec::new();
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
                let layer =
                    value(&layer_key).map_or(0, |v| appkit::send_isize(v, sel!(integerValue)));
                let alpha =
                    value(&alpha_key).map_or(1.0, |v| appkit::send_f64(v, sel!(doubleValue)));
                let Some(bounds) = value(&bounds_key) else {
                    continue;
                };
                let mut frame = CGRect::default();
                if !CGRectMakeWithDictionaryRepresentation(bounds, &raw mut frame) {
                    continue;
                }
                windows.push(SettingsWindow {
                    layer,
                    alpha,
                    bounds: cg_to_cocoa(from_cg(frame), screens.primary_h),
                });
            }
            windows
        }
    }

    /// # Safety
    /// Main thread.
    unsafe fn frontmost_is_settings() -> bool {
        // SAFETY: `+sharedWorkspace` and `-frontmostApplication` are `-(id)` accessors, +0.
        unsafe {
            let workspace = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            !workspace.is_null()
                && is_settings(appkit::send_id(workspace, sel!(frontmostApplication)))
        }
    }

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

    /// # Safety
    /// `note` is nil or the live NSNotification the workspace centre delivered.
    unsafe fn activated_settings(note: Id) -> bool {
        let Some(key) = appkit::nsstring(WORKSPACE_NAMES[1]) else {
            return false;
        };
        if note.is_null() {
            return false;
        }
        // SAFETY: `-userInfo` is `-(NSDictionary *)`, +0 or nil; `-objectForKey:` `-(id)(id)`, +0
        // or nil — an NSRunningApplication under this key.
        unsafe {
            let info = appkit::send_id(note, sel!(userInfo));
            !info.is_null() && is_settings(appkit::send_id_id(info, sel!(objectForKey:), key.id()))
        }
    }

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

    /// # Safety
    /// Main thread; `target` is a live [`HelperTarget`].
    unsafe fn observe_activation(target: Id) -> bool {
        let Some(name) = appkit::nsstring(WORKSPACE_NAMES[0]) else {
            return false;
        };
        // SAFETY: `-addObserver:selector:name:object:` is `-(void)(id, SEL, id, id)`; the selector
        // is one the target declares; nil object means "from any sender".
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

    /// # Safety
    /// Main thread; `target` is a live [`HelperTarget`].
    unsafe fn reveal_soon(target: Id) {
        // SAFETY: `-performSelector:withObject:afterDelay:` is `-(void)(SEL, id, NSTimeInterval)`;
        // the run loop retains the target until it fires, and `hide` and `close` cancel it.
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

    /// # Safety
    /// Main thread; `target` is a live [`HelperTarget`].
    unsafe fn cancel_reveal_soon(target: Id) {
        // SAFETY: `+cancelPreviousPerformRequestsWithTarget:` is `+(void)(id)`; a target with
        // nothing scheduled is a no-op.
        unsafe {
            appkit::send_v_id(
                class(c"NSObject").as_id(),
                sel!(cancelPreviousPerformRequestsWithTarget:),
                target,
            );
        }
    }

    /// # Safety
    /// Main thread.
    unsafe fn activate_aterm() {
        // SAFETY: `+currentApplication` is `+(NSRunningApplication *)`, +0; `-activateWithOptions:`
        // is `-(BOOL)(NSApplicationActivationOptions)`.
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

    fn holds_tile(live: &Live, tile: Id) -> bool {
        live.tile.as_ref().is_some_and(|held| held.as_id() == tile)
    }

    fn drag_began(tile: Id) {
        let _ = with_live(|live| {
            if !holds_tile(live, tile) {
                return;
            }
            note_owner_move(live);
            live.dragging = true;
            let panel = live.panel.id();
            // SAFETY: main thread; `-frame` is `-(NSRect)`, the window and screen queries are
            // read-only; `-setIgnoresMouseEvents:` `-(void)(BOOL)`, `-setAlphaValue:`
            // `-(void)(CGFloat)`, `-orderBack:` `-(void)(id)`.
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

    /// # Safety
    /// Main thread; `tile` is the live tile view.
    unsafe fn dropped_in_settings(tile: Id, at: CGPoint) -> bool {
        // SAFETY: main thread; read-only window and screen queries, and `-bounds` is `-(NSRect)` on
        // the live tile.
        let (settings, image) = unsafe {
            let screens = screens();
            (
                settings_window(&screens),
                appkit::send_rect(tile, sel!(bounds)),
            )
        };
        drop_landed(settings, at.x, at.y, image.size.width, image.size.height)
    }

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

    /// # Safety
    /// Main thread.
    pub(crate) unsafe fn pasteboard_item(bundle: &Path) -> Option<Obj> {
        let path = appkit::nsstring(bundle.to_str()?)?;
        let [file_url, url, text] = [TYPE_FILE_URL, TYPE_URL, TYPE_TEXT].map(appkit::nsstring);
        let (file_url, url, text) = (file_url?, url?, text?);
        // SAFETY: `+fileURLWithPath:isDirectory:` is `+(NSURL *)(NSString *, BOOL)`, +0;
        // `-absoluteString`/`-path` `-(NSString *)`, +0; `+[NSPasteboardItem new]` +1, adopted;
        // `-setString:forType:` is `-(BOOL)(NSString *, NSPasteboardType)`.
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

    /// # Safety
    /// Main thread, inside AppKit's `mouseDragged:` dispatch with its event.
    unsafe fn begin_drag(tile: &TileView, event: Id) -> bool {
        let ivars = tile.ivars();
        // SAFETY: the payload as `pasteboard_item` says; `+alloc` and `-initWithPasteboardWriter:`
        // (`-(id)(id<NSPasteboardWriting>)`) give a +1 NSDraggingItem, adopted;
        // `-setDraggingFrame:contents:` is `-(void)(NSRect, id)`; `+arrayWithObject:` +0; and
        // `-beginDraggingSessionWithItems:event:source:` is
        // `-(NSDraggingSession *)(NSArray *, NSEvent *, id<NSDraggingSource>)`, +0 — the session
        // runs in the event loop and returns at once (no nested tracking loop).
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

    struct Built {
        panel: Obj,
        target: Retained<HelperTarget>,
        content: Retained<ContentView>,
        tile: Option<Retained<TileView>>,
    }

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
                if live.placed.is_some() && !live.moved {
                    note_placed(live, &screens());
                }
                if live.visible && !live.moved {
                    let screens = screens();
                    let settings = settings_window(&screens);
                    match place_turn(true, true, live.beside, settings) {
                        PlaceTurn::Hide => hide(live),
                        _ => place_now(live, &screens, settings),
                    }
                }
            }
        });
    }

    /// # Safety
    /// Main thread; `delegate` is a live [`HelperTarget`] that outlives the panel's
    /// use of it (`close` clears it first).
    unsafe fn new_panel(delegate: Id) -> Option<Obj> {
        let title = appkit::nsstring(TITLE)?;
        // SAFETY: `+alloc` then the designated initializer
        // `-initWithContentRect:styleMask:backing:defer:`
        // (`-(id)(NSRect, NSUInteger, NSUInteger, BOOL)`), which consumes the +1 and returns +1;
        // every setter after it is `-(void)(BOOL)`, `-(void)(NSInteger)`, `-(void)(NSUInteger)` or
        // `-(void)(id)` on the fresh panel.
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

    /// # Safety
    /// Main thread; `panel` live.
    unsafe fn size_to_content(panel: Id, (width, height): (f64, f64)) {
        // SAFETY: `-frame` is `-(NSRect)`; `-frameRectForContentRect:` `-(NSRect)(NSRect)`;
        // `-setFrame:display:` `-(void)(NSRect, BOOL)`.
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

    /// # Safety
    /// Main thread; `view` is a live [`ContentView`] and `target` a live [`HelperTarget`].
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
        // SAFETY: main thread; `+array` +0 and `-setSubviews:` `-(void)(NSArray *)` clear the view
        // (each old subview released by it); every subview below is fresh or the reused tile,
        // framed with `-setFrame:` (`-(void)(NSRect)`) and added with `-addSubview:`
        // (`-(void)(id)`), which retains it.
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

    fn shown(text: &str, tone: Tone) -> Cow<'_, str> {
        if tone == Tone::Done {
            Cow::Owned(format!("\u{2713} {text}"))
        } else {
            Cow::Borrowed(text)
        }
    }

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
        // SAFETY: both are `+(NSFont *)(CGFloat)`, +0, non-raising.
        unsafe {
            if bold {
                appkit::send_id_f64(font, sel!(boldSystemFontOfSize:), size)
            } else {
                appkit::send_id_f64(font, sel!(systemFontOfSize:), size)
            }
        }
    }

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

    /// # Safety
    /// Main thread.
    unsafe fn label(text: &str, tone: Tone) -> Option<Obj> {
        let words = appkit::nsstring(&shown(text, tone))?;
        // SAFETY: `+wrappingLabelWithString:` is the documented non-raising factory, +0, retained
        // here; `-setFont:`/`-setTextColor:` are `-(void)(id)` and `-setSelectable:`
        // `-(void)(BOOL)`.
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

    struct AppKitMeasure {
        mtm: MainThread,
    }

    impl Measure for AppKitMeasure {
        fn text_height(&self, text: &str, tone: Tone, width: f64) -> f64 {
            // SAFETY: main thread (the witness); `-cell` is `-(id)`, +0, and `-cellSizeForBounds:`
            // `-(NSSize)(NSRect)` — a wrapping cell answers the height its words take at that
            // width.
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
            // SAFETY: main thread (the witness); `-cellSize` is `-(NSSize)` on the button's cell.
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
        fn build(mtm: MainThread) -> Option<Retained<Self>> {
            // SAFETY: `alloc_ivars` returns a +1 instance owing one designated initializer, the
            // next send: `-initWithFrame:` (`-(id)(NSRect)`), which consumes it and returns +1.
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
        /// # Safety
        /// Main thread.
        unsafe fn titled(mtm: MainThread, title: &str, link: bool) -> Option<Retained<Self>> {
            let words = appkit::nsstring(title)?;
            // SAFETY: `alloc_ivars` then `-initWithFrame:`, as for the content view; then
            // `-setTitle:` `-(void)(NSString *)`, `-setBezelStyle:` `-(void)(NSUInteger)`,
            // `-setBordered:` `-(void)(BOOL)`, `-setFont:`/`-setContentTintColor:` `-(void)(id)`
            // and `-setAlignment:` `-(void)(NSInteger)` on the fresh button.
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

        /// # Safety
        /// Main thread; `target` outlives the button's use of it (AppKit holds a control's target
        /// weakly; `Live` or the `Preview` keeps it).
        unsafe fn wire(&self, target: Id, action: Sel, tag: isize) {
            // SAFETY: `-setTarget:` is `-(void)(id)`, `-setAction:` `-(void)(SEL)`, `-setTag:`
            // `-(void)(NSInteger)`.
            unsafe {
                appkit::send_v_id(self.as_id(), sel!(setTarget:), target);
                appkit::send_v_sel(self.as_id(), sel!(setAction:), action);
                appkit::send_v_isize(self.as_id(), sel!(setTag:), tag);
            }
        }
    }

    impl TileView {
        fn build(mtm: MainThread, bundle: &Path, events: PanelEvents) -> Option<Retained<Self>> {
            let path = appkit::nsstring(bundle.to_str()?)?;
            let name = appkit::nsstring(TILE_CAPTION)?;
            let role = appkit::nsstring("AXImage")?;
            // SAFETY: `+sharedWorkspace` +0; `-iconForFile:` `-(NSImage *)(NSString *)`, +0,
            // retained here; `-setSize:` `-(void)(NSSize)`; then `alloc_ivars` + `-initWithFrame:`
            // as for the content view, and the accessibility setters `-(void)(BOOL)` /
            // `-(void)(id)`.
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
                        hover: Cell::new(false),
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

        fn set_hover(&self, hover: bool) {
            if self.ivars().hover.replace(hover) != hover {
                // SAFETY: main thread (AppKit's mouse-tracking dispatch or the lane);
                // `-setNeedsDisplay:` is `-(void)(BOOL)`.
                unsafe { appkit::send_v_bool(self.as_id(), sel!(setNeedsDisplay:), true) };
            }
        }

        pub(crate) fn hovered(&self) -> bool {
            self.ivars().hover.get()
        }

        fn set_live(&self, live: bool) {
            self.ivars().live.set(live);
            if !live {
                self.set_hover(false);
            }
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

    pub(crate) struct Preview {
        pub(crate) view: Retained<ContentView>,
        pub(crate) panel: Option<Obj>,
        pub(crate) target: Retained<HelperTarget>,
        pub(crate) tile: Option<Retained<TileView>>,
    }

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

/// Stand-in arms for the tests that drive the lane through an `App`: nothing they hold reaches the
/// OS, and what they were asked is counted per test thread.
#[cfg(test)]
pub(crate) mod test_arms {
    use std::cell::Cell;
    use std::time::Instant;

    use aterm_containment::DevToolGrant;
    use aterm_containment::consent::{
        DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel, ProbeGate,
    };

    use super::{DevToolsHelperArm, FdaReach, Lane, Reach, Reading};

    pub(crate) const SUBJECT: &str = "com.aterm.test";

    thread_local! {
        static OPENS: Cell<u32> = const { Cell::new(0) };
        static OPENED_NOTES: Cell<u32> = const { Cell::new(0) };
        static DECLINE_NOTES: Cell<u32> = const { Cell::new(0) };
        static FDA_OPENS: Cell<u32> = const { Cell::new(0) };
        static FDA_OPENED_NOTES: Cell<u32> = const { Cell::new(0) };
        static FDA_DECLINE_NOTES: Cell<u32> = const { Cell::new(0) };
        static FDA_DECLINED_KEY: std::cell::RefCell<Option<String>> =
            const { std::cell::RefCell::new(None) };
        static FDA_OPENED_AGO: Cell<Option<u64>> = const { Cell::new(None) };
    }

    pub(crate) fn fda_counts() -> (u32, u32, u32) {
        (
            FDA_OPENS.with(Cell::get),
            FDA_OPENED_NOTES.with(Cell::get),
            FDA_DECLINE_NOTES.with(Cell::get),
        )
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn fda_opened_just_now() -> FdaReach {
        FdaReach {
            opened_within: |_| Some(30),
            ..fda_opening()
        }
    }

    pub(crate) fn fda_opening() -> FdaReach {
        FdaReach {
            open_pane: || {
                bump(&FDA_OPENS);
                true
            },
            note_opened: |_| bump(&FDA_OPENED_NOTES),
            note_declined: |_| bump(&FDA_DECLINE_NOTES),
            ..FdaReach::inert()
        }
    }

    /// [`fda_opening`], whose `fda-pane-opened` reads `last_opened` and is written by
    /// `note_opened`.
    #[cfg(target_os = "macos")]
    pub(crate) fn fda_marking(
        last_opened: fn() -> Option<(String, u64)>,
        note_opened: fn(&str),
    ) -> FdaReach {
        FdaReach {
            last_opened,
            note_opened,
            ..fda_opening()
        }
    }

    /// [`fda_opening`], with the owner's Not now on the panel holding, said under
    /// approved setup revision 1.
    #[cfg(target_os = "macos")]
    pub(crate) fn fda_declined() -> FdaReach {
        FdaReach {
            declined: || Some((1, u64::MAX)),
            last_declined: || Some((atpkg::devtools::revision_key(1), 1)),
            ..fda_opening()
        }
    }

    /// [`fda_opening`], whose panel Not now is kept on this thread as
    /// `fda-declined` keeps it: noted under its key, read back as the newest, and
    /// held for its span; and whose page was opened as long ago as
    /// [`set_fda_opened_ago`] says. [`forget_fda_declines`] starts both empty.
    #[cfg(target_os = "macos")]
    pub(crate) fn fda_keeping_declines() -> FdaReach {
        FdaReach {
            opened_within: |window| FDA_OPENED_AGO.with(Cell::get).filter(|ago| *ago < window),
            declined: || {
                FDA_DECLINED_KEY
                    .with(|key| key.borrow().is_some())
                    .then_some((1, u64::MAX))
            },
            last_declined: || FDA_DECLINED_KEY.with(|key| key.borrow().clone().map(|key| (key, 1))),
            note_declined: |noted| {
                bump(&FDA_DECLINE_NOTES);
                FDA_DECLINED_KEY.with(|key| *key.borrow_mut() = Some(noted.to_owned()));
            },
            ..fda_opening()
        }
    }

    /// No Not now kept for [`fda_keeping_declines`] on this thread, and no page
    /// opened.
    #[cfg(target_os = "macos")]
    pub(crate) fn forget_fda_declines() {
        FDA_DECLINED_KEY.with(|key| *key.borrow_mut() = None);
        set_fda_opened_ago(None);
    }

    /// [`fda_keeping_declines`]'s page was opened `ago` seconds ago (`None`: not
    /// within any window).
    #[cfg(target_os = "macos")]
    pub(crate) fn set_fda_opened_ago(ago: Option<u64>) {
        FDA_OPENED_AGO.with(|cell| cell.set(ago));
    }

    fn bump(counter: &'static std::thread::LocalKey<Cell<u32>>) {
        counter.with(|c| c.set(c.get() + 1));
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn opens() -> u32 {
        OPENS.with(Cell::get)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn opened_notes() -> u32 {
        OPENED_NOTES.with(Cell::get)
    }

    pub(crate) fn decline_notes() -> u32 {
        DECLINE_NOTES.with(Cell::get)
    }

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

    pub(crate) fn absent(rows: u32) -> DevToolList {
        list(DevToolListing::Absent, DevToolListLabel::Read, Some(rows))
    }

    pub(crate) fn unread() -> DevToolList {
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
    }

    pub(crate) fn on() -> DevToolList {
        list(DevToolListing::On, DevToolListLabel::Read, Some(2))
    }

    pub(crate) const fn checked_three() -> DevToolProbe {
        DevToolProbe {
            requests: Some(3),
            not_allowed: Some(3),
            ..DevToolProbe::refused(DevToolProbeLabel::Observed)
        }
    }

    pub(crate) fn reading(grant: DevToolGrant, list: DevToolList) -> Reading {
        Reading {
            grant,
            list,
            log: checked_three(),
            subject: Some(SUBJECT),
        }
    }

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
            refresh_verdict: || Ok(()),
            ..Reach::inert()
        }
    }

    pub(crate) const DECLINED: (u64, u64) = (1_791_000_000, 1_791_000_000 + 30 * 86_400);

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn declining() -> Reach {
        Reach {
            declined: || Some(DECLINED),
            last_declined: || Some((atpkg::devtools::revision_key(1), DECLINED.0)),
            ..opening()
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn marked(last_opened: fn() -> Option<(String, u64)>) -> Reach {
        Reach {
            last_opened,
            ..opening()
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn opened(opened_within: fn(u64) -> Option<u64>) -> Reach {
        Reach {
            opened_within,
            ..opening()
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn unstable() -> Reach {
        Reach {
            grant_stable: || false,
            ..opening()
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn spctl_failing() -> Reach {
        Reach {
            list_terminal: || Err("spctl exited 1".to_string()),
            ..opening()
        }
    }

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

    pub(crate) fn showing() -> DevToolsHelperArm {
        DevToolsHelperArm {
            show: |_, _| true,
            settings_running: || true,
            settings_frontmost: || true,
            ..DevToolsHelperArm::inert()
        }
    }

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

    #[test]
    fn the_quiet_moment_needs_every_input_and_each_alone_holds_it() {
        assert!(quiet(true, false, true, true, false), "the quiet moment");
        assert!(!quiet(false, false, true, true, false), "aterm not focused");
        assert!(!quiet(true, true, true, true, false), "typing");
        assert!(
            !quiet(true, false, false, true, false),
            "nobody there for five minutes"
        );
        assert!(
            !quiet(true, false, true, false, false),
            "a launch not yet settled"
        );
        assert!(
            !quiet(true, false, true, true, true),
            "a hand-off under way"
        );
        assert!(QUIET_FOR < PRESENT_WITHIN);
    }

    fn fda_facts() -> FdaFacts {
        FdaFacts {
            fda: FdaState::Denied,
            declined: false,
            grant_stable: true,
            recent: false,
            devtools_recent: false,
            shown_this_revision: false,
            own: true,
        }
    }

    #[test]
    fn the_full_disk_access_hand_over_is_a_table_of_named_refusals() {
        let hand = Admit::Hand {
            list_terminal: false,
        };
        for origin in [Origin::Settings, Origin::Digest] {
            assert_eq!(admit_fda(origin, Moment::Asked, &fda_facts()), hand);
            let refused_everything = FdaFacts {
                fda: FdaState::Unknown,
                declined: true,
                grant_stable: false,
                recent: true,
                devtools_recent: true,
                shown_this_revision: true,
                own: false,
            };
            assert_eq!(admit_fda(origin, Moment::Asked, &refused_everything), hand);
            let granted = FdaFacts {
                fda: FdaState::Granted,
                ..fda_facts()
            };
            assert_eq!(admit_fda(origin, Moment::Asked, &granted), Admit::PaneOnly);
        }
        let auto = Origin::Automatic;
        assert_eq!(admit_fda(auto, Moment::Asked, &fda_facts()), Admit::Queue);
        assert_eq!(admit_fda(auto, Moment::Quiet, &fda_facts()), hand);
        let cases = [
            (
                FdaFacts {
                    fda: FdaState::Granted,
                    ..fda_facts()
                },
                Refusal::Granted,
            ),
            (
                FdaFacts {
                    fda: FdaState::Unknown,
                    ..fda_facts()
                },
                Refusal::Unknown,
            ),
            (
                FdaFacts {
                    declined: true,
                    ..fda_facts()
                },
                Refusal::Declined,
            ),
            (
                FdaFacts {
                    grant_stable: false,
                    ..fda_facts()
                },
                Refusal::Unstable,
            ),
            (
                FdaFacts {
                    shown_this_revision: true,
                    ..fda_facts()
                },
                Refusal::Shown,
            ),
            (
                FdaFacts {
                    recent: true,
                    ..fda_facts()
                },
                Refusal::Recent,
            ),
        ];
        let act = Origin::Ctl { now: false };
        for (facts, why) in cases {
            for moment in [Moment::Asked, Moment::Quiet] {
                assert_eq!(
                    admit_fda(auto, moment, &facts),
                    Admit::Refuse(why),
                    "{facts:?} {moment:?}"
                );
                let for_act = if why == Refusal::Shown {
                    // An act asks: the page shown this revision does not hold it.
                    if moment == Moment::Quiet {
                        hand
                    } else {
                        Admit::Queue
                    }
                } else {
                    Admit::Refuse(why)
                };
                assert_eq!(
                    admit_fda(act, moment, &facts),
                    for_act,
                    "a client's act: {facts:?} {moment:?}"
                );
            }
        }
        assert_eq!(admit_fda(act, Moment::Asked, &fda_facts()), Admit::Queue);
        let devtools_recent = FdaFacts {
            devtools_recent: true,
            ..fda_facts()
        };
        for moment in [Moment::Asked, Moment::Quiet] {
            assert_eq!(
                admit_fda(auto, moment, &devtools_recent),
                Admit::Queue,
                "{moment:?}"
            );
        }
        let not_own = FdaFacts {
            own: false,
            ..fda_facts()
        };
        assert_eq!(admit_fda(auto, Moment::Asked, &not_own), Admit::Queue);
        assert_eq!(
            admit_fda(auto, Moment::Quiet, &not_own),
            Admit::Refuse(Refusal::NotOwn)
        );
    }

    /// A page brought up while no access revision stands keeps the marker's revision step for
    /// this bundle and Mac (Set up keeps the revision, so its page stays shown); under a revision,
    /// or over a word under a version or another bundle's or Mac's, the page's own key is written.
    #[test]
    fn a_full_disk_access_page_up_with_no_revision_keeps_this_identitys_revision_step() {
        const KEY: &str = "0.137.0@com.aterm.aterm@HOST-A";
        let word = |revision: bool, opened: Option<&str>| fda_opened_word(revision, opened, KEY);
        for kept in [
            "r4@com.aterm.aterm@HOST-A",
            "r4@com.aterm.aterm",
            "r4",
            "r12",
        ] {
            assert_eq!(word(false, Some(kept)), kept, "{kept}: kept");
            assert_eq!(
                word(true, Some(kept)),
                KEY,
                "{kept}: a revision writes its own key"
            );
        }
        for other in [
            "r4@com.aterm.aterm.dev@HOST-A",
            "r4@com.aterm.aterm@HOST-B",
            "0.136.0@com.aterm.aterm@HOST-A",
            "rc1@com.aterm.aterm@HOST-A",
            "r@com.aterm.aterm@HOST-A",
            "",
        ] {
            assert_eq!(word(false, Some(other)), KEY, "{other:?}: gives way");
        }
        assert_eq!(word(false, None), KEY);
        assert_eq!(
            fda_opened_word(
                true,
                Some("r3@com.aterm.aterm@HOST-A"),
                "r4@com.aterm.aterm@HOST-A"
            ),
            "r4@com.aterm.aterm@HOST-A"
        );
    }

    fn fda_show(bundle: Option<PathBuf>) -> Show {
        Show {
            page: Page::FullDiskAccess,
            origin: Origin::Automatic,
            reading: Reading::UNREAD,
            listed: None,
            bundle,
            skips: false,
        }
    }

    #[test]
    fn the_full_disk_access_panels_words_in_each_state() {
        let now = Instant::now();
        let bundle = PathBuf::from("/Applications/aterm.app");
        assert_eq!(
            FDA_PANEL_WHY,
            format!("{}.", crate::consent_card::FDA_GRANTS),
            "the panel's why is the card's one spelling"
        );
        let mut panel = Panel::new(fda_show(Some(bundle.clone())), now);
        assert_eq!(panel.page(), Page::FullDiskAccess);
        assert!(!panel.readable() && !panel.wants_read(now + PANEL_TICK));
        let mut seen = Vec::new();
        let explain = panel.content();
        assert!(has(&explain, FDA_PANEL_TITLE) && has(&explain, FDA_PANEL_WHY));
        assert!(has(&explain, FDA_PANEL_HOW) && has(&explain, FDA_PANEL_TRADE));
        assert_eq!(
            explain.tile(),
            Some(&Tile {
                bundle: bundle.clone(),
                live: true,
                step: FDA_PANEL_STEP,
            })
        );
        assert_eq!(explain.tile().map(Tile::hint), Some(FDA_PANEL_STEP));
        assert_eq!(explain.buttons(), [Button::SwitchedOn, Button::NotNow]);
        seen.push(explain);
        let plus = Panel::new(fda_show(None), now).content();
        assert!(plus.tile().is_none() && has(&plus, FDA_PANEL_HOW_PLUS));
        seen.push(plus);
        let _ = panel.on(PanelEvent::DragEnded { accepted: false }, now);
        let failed = panel.content();
        assert!(has(&failed, FDA_DROP_FAILED_WORDS));
        seen.push(failed);
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, now);
        let dropped = panel.content();
        assert!(panel.dropped() && has(&dropped, FDA_PANEL_DROPPED));
        assert_eq!(dropped.tile().map(Tile::hint), Some(TILE_SPENT_HINT));
        seen.push(dropped);
        let effect = panel.granted(now);
        assert!(effect.granted && effect.redraw);
        let done = panel.content();
        assert!(has(&done, FDA_PANEL_DONE) && done.buttons().is_empty());
        seen.push(done);
        for content in &seen {
            for (text, _) in content.texts() {
                assert!(!text.contains("Developer Tools"), "{text}");
            }
            assert!(!content.buttons().contains(&Button::ShowList));
        }
    }

    #[test]
    fn a_full_disk_access_hand_over_goes_first_and_declines_only_its_own_page() {
        let now = Instant::now();
        let mut lane = Lane::windowed_with_fda_for_test(
            test_arms::opening(),
            test_arms::fda_opening(),
            test_arms::showing(),
        );
        lane.queue(Origin::Automatic, now);
        #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
        lane.queue_fda(Origin::Automatic, now + Duration::from_secs(1));
        assert_eq!(lane.fda_queued(), Some(Origin::Automatic));
        let later = now + Duration::from_secs(30);
        assert!(
            !lane.hand_over_due(later),
            "Developer Tools waits behind it"
        );
        assert!(lane.fda_hand_over_due(later));
        assert_eq!(
            lane.next_deadline(later, || true),
            Some(now + Duration::from_secs(1)),
            "only the Full Disk Access look is armed"
        );
        assert_eq!(lane.take_queued(later, true), None);
        assert_eq!(lane.take_queued_fda(later, false), None, "not quiet");
        assert_eq!(lane.take_queued_fda(later, true), Some(Origin::Automatic));
        let wake: Wake = Arc::new(|| {});
        assert!(lane.show(fda_show(None), later, Arc::clone(&wake)));
        assert!(!lane.hand_over_due(later + RECHECK_EVERY * 10));
        let (_, _, declines_before) = test_arms::fda_counts();
        let devtools_declines = test_arms::decline_notes();
        lane.press_for_test(PanelEvent::NotNow);
        let asks = lane.tick_panel(later, None, &wake);
        assert_eq!(asks.page, Page::FullDiskAccess);
        assert!(lane.panel().is_none());
        assert_eq!(test_arms::fda_counts().2, declines_before + 1);
        assert_eq!(test_arms::decline_notes(), devtools_declines);
        assert_eq!(
            lane.take_fda_ended(),
            Some(FdaEnded {
                why: Closed::NotNow,
                dropped: false
            })
        );
        assert_eq!(lane.take_fda_ended(), None);
        assert!(
            lane.hand_over_due(later + RECHECK_EVERY),
            "Developer Tools' turn at its next look"
        );
    }

    /// A System State step is held only by a Not now under its own revision, written cold or
    /// warm; the control verb also by any Not now inside its span; an owner's press by neither.
    #[test]
    fn a_not_now_holds_a_system_state_step_for_its_own_revision_only() {
        let key = "r4@com.aterm.aterm@UUID";
        let this = (key.to_owned(), 10);
        let cold = (atpkg::devtools::revision_key(4), 10);
        let earlier = (atpkg::devtools::revision_key(3), 10);
        let version = ("0.105.0".to_owned(), 10);
        for (origin, last, held, want) in [
            (Origin::Automatic, None, false, false),
            (Origin::Automatic, Some(&this), false, true),
            (Origin::Automatic, Some(&this), true, true),
            (Origin::Automatic, Some(&cold), false, true),
            (Origin::Automatic, Some(&earlier), true, false),
            (Origin::Automatic, Some(&earlier), false, false),
            (Origin::Automatic, Some(&version), true, false),
            (Origin::Automatic, Some(&version), false, false),
            (Origin::SystemStateAct, Some(&this), false, true),
            (Origin::SystemStateAct, Some(&cold), true, true),
            (Origin::SystemStateAct, Some(&earlier), true, false),
            (Origin::SystemStateAct, Some(&version), true, false),
            (Origin::Ctl { now: false }, Some(&earlier), true, true),
            (Origin::Ctl { now: false }, Some(&earlier), false, false),
            (Origin::Ctl { now: false }, Some(&this), false, true),
            (Origin::Ctl { now: true }, Some(&version), true, true),
            (Origin::Settings, Some(&this), true, false),
            (Origin::Digest, Some(&this), true, false),
        ] {
            assert_eq!(
                declined_for(origin, key, last, held),
                want,
                "{origin:?} after {last:?} (held {held})"
            );
        }
    }

    /// A Not now under an approved revision is handed back once as its decline; with no
    /// revision, nothing is.
    #[test]
    fn a_not_now_is_handed_back_once_as_the_revisions_decline() {
        let now = Instant::now();
        let wake: Wake = Arc::new(|| {});
        for (revision, want) in [
            (Some(4), vec![(4, Access::FullDiskAccess)]),
            (None, Vec::new()),
        ] {
            let mut lane = Lane::windowed_with_fda_for_test(
                test_arms::opening(),
                test_arms::fda_opening(),
                test_arms::showing(),
            );
            lane.set_marks(Marks {
                devtools: "r4".to_owned(),
                fda: "r4@com.aterm.test@HERE".to_owned(),
                revision,
            });
            assert!(lane.show(fda_show(None), now, Arc::clone(&wake)));
            assert!(lane.take_declines().is_empty(), "nothing said yet");
            lane.press_for_test(PanelEvent::NotNow);
            let _ = lane.tick_panel(now, None, &wake);
            assert!(lane.panel().is_none());
            assert_eq!(lane.take_declines(), want, "revision {revision:?}");
            assert!(lane.take_declines().is_empty(), "handed back once");
        }
    }

    #[test]
    fn a_grant_ends_the_full_disk_access_wait_and_panel() {
        let now = Instant::now();
        let mut lane = Lane::windowed_with_fda_for_test(
            test_arms::opening(),
            test_arms::fda_opening(),
            test_arms::showing(),
        );
        #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
        lane.queue_fda(Origin::Automatic, now);
        lane.note_fda(FdaState::Denied, now);
        lane.note_fda(FdaState::Unknown, now);
        assert!(lane.fda_waiting());
        lane.note_fda(FdaState::Granted, now);
        assert!(!lane.fda_waiting());
        assert!(lane.show(fda_show(None), now, Arc::new(|| {})));
        lane.note_reading(
            test_arms::reading(DevToolGrant::Granted, test_arms::on()),
            now,
        );
        assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Explain));
        lane.note_fda(FdaState::Granted, now);
        assert_eq!(lane.panel().map(Panel::phase), Some(Phase::Done));
    }

    #[test]
    fn a_cold_start_settles_in_seconds_and_a_successor_waits_out_the_launch_settle() {
        let lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
        let born = lane.born;
        assert!(!lane.settled(born + COLD_START_SETTLE));
        assert!(lane.settled(born + crate::consent_card::LAUNCH_SETTLE));
        let mut cold = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
        cold.note_cold_start();
        let born = cold.born;
        assert!(!cold.settled(born));
        assert!(cold.settled(born + COLD_START_SETTLE));
        assert!(COLD_START_SETTLE < QUIET_FOR);
    }

    #[test]
    fn the_origin_grant_and_decline_table() {
        let facts = |grant, list, declined, grant_stable, recent| Facts {
            grant,
            list,
            declined,
            grant_stable,
            recent,
            fda_on: true,
            shown_this_revision: false,
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
        // The page already up in this revision holds only the window's own step, judged as it
        // would open.
        let shown = Facts {
            shown_this_revision: true,
            ..facts(DevToolGrant::NotGranted, absent(2), false, true, false)
        };
        for moment in [Moment::Asked, Moment::Quiet] {
            assert_eq!(
                admit(Origin::Automatic, moment, &shown),
                Admit::Refuse(Refusal::Shown)
            );
        }
        assert_eq!(
            admit(Origin::Ctl { now: false }, Moment::Asked, &shown),
            Admit::Queue
        );
        assert_eq!(
            admit(Origin::Settings, Moment::Asked, &shown),
            Admit::Hand {
                list_terminal: false
            }
        );
        // Without Full Disk Access the window's own waits (the list could not be read
        // back); a control verb's and an owner's press do not.
        let no_fda = Facts {
            fda_on: false,
            ..facts(DevToolGrant::NotGranted, absent(2), false, true, false)
        };
        for moment in [Moment::Asked, Moment::Quiet] {
            assert_eq!(
                admit(Origin::Automatic, moment, &no_fda),
                Admit::Refuse(Refusal::NoFullDiskAccess)
            );
        }
        assert_eq!(
            admit(Origin::Ctl { now: false }, Moment::Asked, &no_fda),
            Admit::Queue
        );
        assert_eq!(
            admit(Origin::Settings, Moment::Asked, &no_fda),
            Admit::Hand {
                list_terminal: false
            }
        );
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
            page: Page::DeveloperTools,
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
                live: true,
                step: atpkg::devtools::HELPER_STEP,
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
        let off = test_arms::list(DevToolListing::Off, DevToolListLabel::Read, Some(2));
        let mut listed_off = Panel::new(show(off, Some(bundle.clone())), t0);
        let words = listed_off.content();
        assert!(has(&words, atpkg::devtools::HELPER_SWITCH_ON) && !has(&words, HELPER_HOW));
        assert_eq!(words.tile(), None, "aterm is already listed");
        assert!(listed_off.on_read(absent(2), t0).redraw, "it left the list");
        assert!(has(&listed_off.content(), HELPER_HOW));
        assert!(listed_off.content().tile().is_some());
        let mut listed = show(absent(0), None);
        listed.listed = Some(Ok(()));
        assert!(has(&Panel::new(listed, t0).content(), only_aterm(true)));
        let unread_panel = Panel::new(show(unread(), None), t0);
        assert!(!unread_panel.readable());
        let words = unread_panel.content();
        assert!(has(&words, UNREAD_WORDS));
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
        assert!(
            panel
                .on(PanelEvent::DragEnded { accepted: false }, t0)
                .redraw
        );
        assert_eq!(panel.phase(), Phase::DropFailed);
        assert!(has(&panel.content(), DROP_FAILED_WORDS));
        assert!(panel.content().tile().is_some_and(|t| t.live));
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0);
        assert_eq!(panel.phase(), Phase::Explain);
        assert!(panel.content().tile().is_some_and(|t| !t.live), "one drop");
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
        let mut landed_off = Panel::new(show(absent(2), Some(bundle.clone())), t0);
        let _ = landed_off.on(PanelEvent::DragEnded { accepted: true }, t0);
        let _ = landed_off.on_read(off, t0 + DROP_CONFIRM_WITHIN * 2);
        let words = landed_off.content();
        assert_eq!(landed_off.phase(), Phase::Explain);
        assert!(has(&words, atpkg::devtools::HELPER_SWITCH_ON));
        assert!(!has(&words, DROP_UNCONFIRMED_WORDS));
        let mut late = panel.clone();
        let _ = late.on_read(off, t0 + DROP_CONFIRM_WITHIN * 3);
        assert_eq!(late.phase(), Phase::Explain);
        assert!(!has(&late.content(), DROP_UNCONFIRMED_WORDS));
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
        let unrun = Panel::new(show(absent(0), None), t0).content();
        assert_eq!(unrun.buttons(), [Button::ShowList, Button::NotNow]);
        let mut ran = show(absent(0), None);
        ran.listed = Some(Ok(()));
        assert_eq!(Panel::new(ran, t0).content().buttons(), [Button::NotNow]);
        assert_eq!(
            Panel::new(show(absent(2), None), t0).content().buttons(),
            [Button::NotNow]
        );
        let mut emptied = Panel::new(show(absent(0), None), t0);
        assert!(emptied.on_read(absent(1), t0 + PANEL_TICK).redraw);
        assert_eq!(emptied.content().buttons(), [Button::NotNow]);
    }

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
        for button in Button::ALL {
            assert_eq!(Button::from_tag(button.tag()), Some(button));
        }
        assert_eq!(Button::from_tag(-1), None, "the status link's tag");
        assert_eq!(Button::from_tag(Button::ALL.len() as isize), None);
        let switched = panel.clone().on(PanelEvent::SwitchedOn, t0);
        assert_eq!(switched.close, Some(Closed::SwitchedOn));
        assert!(switched.recheck);
        assert!(panel.clone().on(PanelEvent::Status, t0).open_status);
        assert!(!panel.wants_read(t0), "the show's own reading is fresh");
        assert!(panel.wants_read(t0 + PANEL_TICK));
        panel.read_out = true;
        assert!(!panel.wants_read(t0 + PANEL_TICK), "one at a time");
        let _ = panel.on_read(absent(2), t0 + PANEL_TICK);
        assert!(!panel.wants_read(t0 + PANEL_TICK));
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0 + PANEL_TICK);
        assert!(panel.wants_read(t0 + PANEL_TICK), "a drop reads at once");
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
        let effect = panel.on_read(unread(), t0 + PANEL_TICK * 2);
        assert!(effect.redraw && !panel.readable());
        assert!(!panel.wants_read(t0 + PANEL_TICK * 9));
        assert_eq!(
            panel.turn(t0 + DROP_CONFIRM_WITHIN * 2),
            Effect::default(),
            "an unread list confirms no drop"
        );
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
        let mut idle = Panel::new(show(absent(2), None), t0);
        assert_eq!(idle.next_turn(), t0 + PANEL_FOR);
        assert_eq!(idle.turn(t0 + PANEL_FOR).close, Some(Closed::Lapsed));
    }

    const ROW_CHANGED_AT: i64 = 1_791_000_000;

    fn on_since(changed_at: i64) -> DevToolList {
        DevToolList {
            modified_at_s: Some(changed_at),
            ..on()
        }
    }

    fn denied_after_the_row() -> DevToolProbe {
        DevToolProbe {
            state: aterm_containment::DevToolState::Denied,
            requests: Some(1),
            not_allowed: Some(1),
            answered_at_ms: Some(ROW_CHANGED_AT * 1_000 + 60_000),
            ..DevToolProbe::refused(aterm_containment::DevToolProbeLabel::Observed)
        }
    }

    fn joined(list: DevToolList) -> Reading {
        let log = denied_after_the_row();
        Reading {
            grant: devtool_grant(&list, &log),
            list,
            log,
            subject: Some(test_arms::SUBJECT),
        }
    }

    #[test]
    fn a_row_already_on_while_macos_still_checks_is_never_done_until_it_changes() {
        use atpkg::devtools::{HELPER_HOW, HELPER_LISTED_ON};
        let t0 = Instant::now();
        let reading = joined(on_since(ROW_CHANGED_AT));
        assert_eq!(reading.grant, DevToolGrant::NotGranted, "ListedOn");
        let bundle = Some(PathBuf::from("/Applications/aterm.app"));
        let mut panel = Panel::new(
            Show {
                page: Page::DeveloperTools,
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
        let again = panel.on_read(on_since(ROW_CHANGED_AT), t0 + PANEL_TICK);
        assert!(!again.granted && !again.redraw);
        assert_eq!(panel.phase(), Phase::Explain);
        let toggled = on_since(ROW_CHANGED_AT + 3_600);
        let effect = panel.on_read(toggled, t0 + PANEL_TICK * 2);
        assert!(effect.granted && panel.phase() == Phase::Done);
        let mut held = Panel::new(
            Show {
                page: Page::DeveloperTools,
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
        assert!(held.on_read(absent(2), t0 + PANEL_TICK * 2).redraw);
        assert!(has(&held.content(), HELPER_HOW) && held.content().tile().is_some());
        let mut adding = Panel::new(
            Show {
                page: Page::DeveloperTools,
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
                page: Page::DeveloperTools,
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
        assert_eq!(
            reading.with_list(on_since(ROW_CHANGED_AT + 600)).grant,
            DevToolGrant::Granted
        );
        assert_eq!(
            Reading::UNREAD.with_list(on_since(ROW_CHANGED_AT)).grant,
            DevToolGrant::Granted
        );
    }

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
                    page: Page::DeveloperTools,
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
        let s = Rect { x: 500.0, ..s };
        let s = Rect {
            w: main.max_x() - 500.0,
            ..s
        };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!(r.max_x(), s.x - PLACE_GAP);
        assert!(inside(r, main));
        let s = Rect {
            x: 0.0,
            y: 100.0,
            w: main.w,
            h: 400.0,
        };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!((r.x, r.y), (s.x, s.max_y() + PLACE_GAP));
        assert!(inside(r, main));
        let s = Rect { y: 500.0, ..s };
        let r = place(w, h, Some(s), &[main]);
        assert_eq!(r.max_y(), s.y - PLACE_GAP);
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
        let r = place(w, h, None, &[main, second]);
        assert!(inside(r, main));
        assert_eq!(
            (r.max_x(), r.max_y()),
            (main.max_x() - PLACE_GAP, main.max_y() - PLACE_GAP)
        );
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
        assert_eq!(place_turn(true, true, false, None), PlaceTurn::Place(None));
        assert_eq!(
            place_turn(false, false, true, Some(window)),
            PlaceTurn::Nothing
        );
    }

    #[test]
    fn system_settings_password_window_under_the_panel_orders_it_out_until_it_goes() {
        let main = Rect {
            x: 200.0,
            y: 300.0,
            w: 740.0,
            h: 625.0,
        };
        let dialog = Rect {
            x: main.x + 240.0,
            y: main.y + 138.0,
            w: 260.0,
            h: 349.0,
        };
        let window = |layer, alpha, bounds| SettingsWindow {
            layer,
            alpha,
            bounds,
        };
        let both = [window(0, 1.0, main), window(0, 1.0, dialog)];
        let level = 3;
        assert_eq!(settings_main(&both), Some(0));
        assert_eq!(
            settings_main(&[both[1], both[0]]),
            Some(1),
            "the 740 × 625 window, wherever it is listed"
        );
        assert_eq!(
            settings_main(&both[1..]),
            None,
            "the password window alone is no main window"
        );
        let over = Rect {
            x: main.x + 300.0,
            y: main.y + 50.0,
            w: PANEL_W,
            h: 540.0,
        };
        assert!(under_panel(&both, over, level), "moved over the window");
        let screen = Rect {
            x: 0.0,
            y: 0.0,
            w: 1728.0,
            h: 1117.0,
        };
        let beside = place(PANEL_W, 540.0, Some(main), &[screen]);
        assert!(!under_panel(&both, beside, level), "where `place` puts it");
        assert!(
            !under_panel(&both[..1], over, level),
            "the main window alone is no dialog: a panel moved over the list stays"
        );
        assert!(
            !under_panel(&[both[0], window(0, 0.0, dialog)], over, level),
            "transparent"
        );
        assert!(
            !under_panel(&[both[0], window(8, 1.0, dialog)], over, level),
            "the + button's file chooser floats above the panel"
        );
        assert!(
            under_panel(&[both[0], window(3, 1.0, dialog)], over, level),
            "at the panel's own level: out"
        );
        assert!(
            !under_panel(&[both[0], window(0, 1.0, main)], over, level),
            "a companion on the main window's own frame is no dialog"
        );
        assert!(settings_busy(&both, level), "wherever the panel stands");
        assert!(!settings_busy(&both[..1], level));
        assert!(
            !settings_busy(&both[1..], level),
            "no main window to be busy over"
        );
        assert!(!settings_busy(&[both[0], window(8, 1.0, dialog)], level));
        assert_eq!(cover_turn(true, false, true, true), Cover::Out);
        assert_eq!(cover_turn(true, false, true, false), Cover::Nothing);
        assert_eq!(
            cover_turn(false, true, true, true),
            Cover::Nothing,
            "still under: it stays out"
        );
        assert_eq!(
            cover_turn(false, true, true, false),
            Cover::Back,
            "gone: back"
        );
        assert_eq!(
            cover_turn(false, true, false, false),
            Cover::Nothing,
            "another app in front: the activation observer's"
        );
        assert_eq!(
            cover_turn(false, false, true, false),
            Cover::Nothing,
            "out for another reason: not this turn's to bring back"
        );
    }

    #[test]
    fn the_list_read_waits_while_system_settings_shows_its_password_window() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        static BUSY: AtomicBool = AtomicBool::new(true);
        static READS: AtomicU32 = AtomicU32::new(0);
        let reach = Reach {
            read_list: |_, _| {
                READS.fetch_add(1, Ordering::SeqCst);
                test_arms::absent(2)
            },
            ..test_reach()
        };
        let arm = DevToolsHelperArm {
            settings_busy: || BUSY.load(Ordering::SeqCst),
            ..test_arm()
        };
        let mut lane = Lane::windowed_for_test(reach, arm);
        let wake = nudge();
        let t0 = Instant::now();
        assert!(lane.show(show(test_arms::absent(2), None), t0, Arc::clone(&wake)));
        let gate = Some(ProbeGate::on());
        let _ = lane.tick_panel(t0, gate, &wake);
        events_of(&lane).post(PanelEvent::DragEnded { accepted: true });
        let mut at = t0;
        for _ in 0..12 {
            at += PANEL_TICK;
            let _ = lane.tick_panel(at, gate, &wake);
        }
        assert_eq!(
            READS.load(Ordering::SeqCst),
            0,
            "a drop and twelve ticks while the password window is up: no read"
        );
        BUSY.store(false, Ordering::SeqCst);
        let _ = lane.tick_panel(at + PANEL_TICK, gate, &wake);
        until(|| READS.load(Ordering::SeqCst) == 1);
    }

    #[test]
    fn the_verdict_refresh_runs_on_a_window_one_at_a_time_and_never_headless() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        static RUNS: AtomicU32 = AtomicU32::new(0);
        static HELD: AtomicBool = AtomicBool::new(true);
        let mut headless = Lane::new(true);
        assert!(!headless.refresh_verdict(), "headless: nothing runs");
        assert_eq!(headless.refreshes_started(), 0);
        let reach = Reach {
            refresh_verdict: || {
                RUNS.fetch_add(1, Ordering::SeqCst);
                until(|| !HELD.load(Ordering::SeqCst));
                Ok(())
            },
            ..test_reach()
        };
        let mut lane = Lane::windowed_for_test(reach, test_arm());
        assert!(lane.refresh_verdict());
        until(|| RUNS.load(Ordering::SeqCst) == 1);
        assert!(
            !lane.refresh_verdict(),
            "one at a time: a second waits for the first"
        );
        assert_eq!(lane.refreshes_started(), 1);
        HELD.store(false, Ordering::SeqCst);
        until(|| !lane.refreshing.load(Ordering::SeqCst));
        assert!(lane.refresh_verdict(), "the next switch-on runs again");
        until(|| RUNS.load(Ordering::SeqCst) == 2);
        assert_eq!(lane.refreshes_started(), 2);
    }

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
        let _ = panel.on(PanelEvent::DragEnded { accepted: true }, t0);
        assert!(panel.content().tile().is_some_and(|tile| !tile.live));
        assert_eq!(panel.on(PanelEvent::DroppedOutside, t0), Effect::default());
        assert!(
            DROP_UNCONFIRMED_WORDS.contains("Finder") && DROP_UNCONFIRMED_WORDS.contains("Trash")
        );
        assert!(DROP_OUTSIDE_WORDS.contains("Trash"));
    }

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

    fn until(mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The window's own step keeps its place in each page's queue, and another origin's ask it
    /// holds in its place rides with it, with what its own `spctl` run answered: one that comes
    /// while it waits, the one it replaces, or the one a step it replaces carried. The window
    /// takes the rider once the step is given back and judges it by its own rules
    /// ([`Lane::take_rider`]); a client's rider lapses as it would alone, and one that waits
    /// again with the step, or for the step's `spctl` run, keeps its time unless the step carries
    /// a newer ask. The control verb's rider is also taken at its own quiet moment while the
    /// step waits on ([`Lane::take_quiet_rider`]); a System State act stays with the step.
    #[test]
    fn the_window_s_own_step_carries_the_ask_it_holds_in_its_place() {
        fn who(rider: Option<Rider>) -> Option<(Origin, Instant)> {
            rider.map(|rider| (rider.origin, rider.at))
        }
        let t0 = Instant::now();
        let t1 = t0 + RECHECK_EVERY;
        let ctl = Origin::Ctl { now: false };
        let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
        lane.queue(Origin::Automatic, t0);
        assert_eq!(lane.take_queued(t0, true), Some((Origin::Automatic, None)));
        assert_eq!(lane.take_rider(t0), None, "nothing rode");
        lane.queue(Origin::Automatic, t0);
        lane.queue(ctl, t1);
        assert_eq!(lane.queued(), Some(Origin::Automatic));
        assert_eq!(lane.take_queued(t0, true), Some((Origin::Automatic, None)));
        assert_eq!(who(lane.take_rider(t0)), Some((ctl, t1)));
        assert_eq!(lane.take_rider(t0), None, "taken once");
        lane.queue(Origin::SystemStateAct, t0);
        lane.queue(Origin::Automatic, t1);
        lane.queue(Origin::Automatic, t0 + RECHECK_EVERY * 2);
        assert_eq!(
            lane.take_queued(t0, true),
            Some((Origin::Automatic, None)),
            "the step took the act's place"
        );
        assert_eq!(who(lane.take_rider(t0)), Some((Origin::SystemStateAct, t0)));
        lane.queue(Origin::Automatic, t0);
        lane.queue_after_listing(ctl, t0, Err("spctl exited 1".to_string()));
        let _ = lane.take_queued(t0, true);
        assert_eq!(
            lane.take_rider(t0 + ASKED_LAPSE),
            None,
            "a client's ask lapses as it would alone"
        );
        lane.queue(Origin::Automatic, t0);
        lane.queue_after_listing(ctl, t0, Ok(()));
        let _ = lane.take_queued(t0, true);
        assert_eq!(
            lane.take_rider(t0).map(|rider| rider.listed),
            Some(Some(Ok(()))),
            "the rider keeps its own run's answer"
        );
        lane.queue(ctl, t0);
        lane.queue_after_listing(Origin::Automatic, t1, Ok(()));
        assert_eq!(
            lane.take_queued(t1, true),
            Some((Origin::Automatic, Some(Ok(()))))
        );
        assert_eq!(
            who(lane.take_rider(t1)),
            Some((ctl, t0)),
            "the ask a listed step took the place of"
        );
        lane.queue(Origin::Automatic, t0);
        lane.queue(Origin::SystemStateAct, t0);
        assert_eq!(
            lane.take_quiet_rider(t0),
            None,
            "a client's act waits for the step moment with the step"
        );
        let _ = lane.take_queued(t0, true);
        assert_eq!(
            who(lane.take_rider(t0)),
            Some((Origin::SystemStateAct, t0)),
            "and still rides it"
        );
        lane.queue(Origin::Automatic, t0);
        lane.queue_after_listing(ctl, t0, Ok(()));
        let rider = lane.take_quiet_rider(t0).expect("the control verb's ask");
        assert_eq!(
            (rider.origin, rider.listed),
            (ctl, Some(Ok(()))),
            "the control verb's keeps its own quiet moment, and its run's answer"
        );
        assert_eq!(lane.queued(), Some(Origin::Automatic), "the step waits on");
        assert_eq!(lane.take_quiet_rider(t0), None, "taken once");
        lane.queue(ctl, t0);
        assert_eq!(lane.take_quiet_rider(t0 + ASKED_LAPSE), None, "lapsed");
        let _ = lane.take_queued(t0, true);
        assert_eq!(lane.take_rider(t0), None, "and gone");
        lane.queue(Origin::Automatic, t0);
        lane.queue(ctl, t0);
        let _ = lane.take_queued(t0, true);
        let rider = lane.take_rider(t0).expect("the client's ask");
        lane.queue(Origin::Automatic, t1);
        lane.ride(rider.clone());
        let _ = lane.take_queued(t1, true);
        assert_eq!(
            who(lane.take_rider(t0)),
            Some((ctl, t0)),
            "it waited again as asked"
        );
        lane.queue(Origin::Automatic, t1);
        lane.queue(Origin::SystemStateAct, t1);
        lane.ride(rider.clone());
        let _ = lane.take_queued(t1, true);
        assert_eq!(
            who(lane.take_rider(t1)),
            Some((Origin::SystemStateAct, t1)),
            "the step keeps the newer ask"
        );
        lane.ride_listing(rider.clone());
        assert_eq!(
            lane.take_listing_rider(t0),
            Some(rider.clone()),
            "it waited for the run"
        );
        assert_eq!(lane.take_listing_rider(t0), None, "taken once");
        lane.ride_listing(rider);
        assert_eq!(lane.take_listing_rider(t0 + ASKED_LAPSE), None, "lapsed");

        let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
            #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
            lane.queue_fda(origin, at);
        };
        queue_fda(&mut lane, Origin::Automatic, t0);
        queue_fda(&mut lane, Origin::SystemStateAct, t1);
        assert_eq!(lane.take_queued_fda(t0, true), Some(Origin::Automatic));
        assert_eq!(
            who(lane.take_fda_rider(t0)),
            Some((Origin::SystemStateAct, t1))
        );
        queue_fda(&mut lane, Origin::SystemStateAct, t0);
        queue_fda(&mut lane, Origin::Automatic, t0);
        assert_eq!(lane.take_queued_fda(t0, true), Some(Origin::Automatic));
        let rider = lane
            .take_fda_rider(t0)
            .expect("the act the step took the place of");
        assert_eq!(who(Some(rider.clone())), Some((Origin::SystemStateAct, t0)));
        queue_fda(&mut lane, Origin::Automatic, t0);
        lane.ride_fda(rider.clone());
        assert_eq!(lane.take_queued_fda(t0, true), Some(Origin::Automatic));
        assert_eq!(lane.take_fda_rider(t0), Some(rider), "it waited again");
        assert_eq!(lane.take_rider(t0), None, "each page keeps its own");
    }

    /// A page that comes up answers what waits for it, except the window's own step while no
    /// access revision stands: that page is not the revision's, so the step waits on for it,
    /// without the ask it carried (the page was that ask's), and looks again only once the
    /// page's ten minutes are past, so it is not refused as recent: a look that finds no moment
    /// meanwhile, another step of its own taking its place, a nearer look asked of it
    /// (`Lane::defer`, a reading still out) or a nearer page keeps that. Once a revision
    /// stands, a kept step whose page that revision brought up already looks at once. Under a
    /// revision, or for another origin's ask, the page answers it.
    #[test]
    fn a_page_up_with_no_revision_leaves_the_window_s_own_step_waiting() {
        let t0 = Instant::now();
        let ten = Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
        let ctl = Origin::Ctl { now: false };
        let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
        lane.queue(Origin::Automatic, t0);
        lane.queue(ctl, t0);
        lane.page_came_up(false, t0 + ten);
        assert_eq!(lane.queued(), Some(Origin::Automatic), "the step waits on");
        assert_eq!(lane.kept(), [true, false]);
        assert_eq!(lane.take_queued(t0, false), None, "no moment");
        assert!(
            !lane.hand_over_due(t0 + ten - RECHECK_EVERY),
            "within the page's ten minutes"
        );
        lane.queue_after_listing(Origin::Automatic, t0, Ok(()));
        assert!(
            !lane.hand_over_due(t0 + ten - RECHECK_EVERY),
            "its own step in its place keeps the wait"
        );
        lane.defer(t0 + RECHECK_EVERY);
        lane.wait_out_page(t0 + RECHECK_EVERY);
        assert!(
            !lane.hand_over_due(t0 + ten - RECHECK_EVERY),
            "a nearer look or page keeps the wait"
        );
        assert_eq!(
            lane.next_deadline(t0, || true),
            Some(t0 + ten),
            "and wakes for it"
        );
        assert!(lane.hand_over_due(t0 + ten), "then it looks");
        let _ = lane.take_queued(t0 + ten, true);
        assert_eq!(lane.take_rider(t0), None, "the page was the ask's");
        lane.queue(Origin::Automatic, t0);
        lane.page_came_up(false, t0 + ten);
        lane.revision_stands(t0, [false, false]);
        assert_eq!(lane.kept(), [false, false], "asked once");
        assert!(!lane.hand_over_due(t0), "not shown: it waits out the page");
        lane.page_came_up(false, t0 + ten);
        lane.revision_stands(t0, [true, false]);
        assert!(lane.hand_over_due(t0), "shown: it looks at once");
        lane.page_came_up(true, t0);
        assert_eq!(lane.queued(), None, "a revision's page answers its step");
        lane.queue(ctl, t0);
        lane.page_came_up(false, t0);
        assert_eq!(lane.queued(), None, "and any other ask");

        let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
            #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
            lane.queue_fda(origin, at);
        };
        queue_fda(&mut lane, Origin::Automatic, t0);
        queue_fda(&mut lane, Origin::SystemStateAct, t0);
        lane.fda_page_came_up(false, t0 + ten);
        assert_eq!(
            lane.fda_queued(),
            Some(Origin::Automatic),
            "the step waits on"
        );
        assert_eq!(lane.kept(), [false, true]);
        assert_eq!(lane.take_queued_fda(t0, false), None, "no moment");
        assert!(!lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY));
        lane.defer_fda(t0 + RECHECK_EVERY);
        lane.fda_wait_out_page(t0 + RECHECK_EVERY);
        assert!(
            !lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY),
            "a nearer look or page keeps the wait"
        );
        lane.defer_fda(t0 + ten + RECHECK_EVERY);
        assert!(!lane.fda_hand_over_due(t0 + ten), "a later one moves it");
        assert!(lane.fda_hand_over_due(t0 + ten + RECHECK_EVERY));
        assert_eq!(
            lane.take_queued_fda(t0 + ten + RECHECK_EVERY, true),
            Some(Origin::Automatic)
        );
        assert_eq!(lane.take_fda_rider(t0), None, "the page was the act's");
        queue_fda(&mut lane, Origin::Automatic, t0);
        lane.fda_page_came_up(true, t0);
        assert_eq!(lane.fda_queued(), None);
    }

    /// A page a step waits out (a new access revision's step behind a page under ten minutes
    /// old, `App::rearm_devtools_step`) holds it whatever else asks it to look: a reading still
    /// out (`Lane::defer`), a look that finds no moment, or another step of its own taking its
    /// place with its `spctl` run's answer. Only the reading's own look moves freely, and only
    /// after the page.
    #[test]
    fn a_page_the_step_waits_out_holds_every_nearer_look() {
        let t0 = Instant::now();
        let ten = Duration::from_secs(atpkg::devtools::REOPEN_AFTER_S);
        let mut lane = Lane::windowed_for_test(Reach::inert(), DevToolsHelperArm::inert());
        lane.queue(Origin::Automatic, t0);
        assert!(!lane.waits_out_a_page());
        lane.wait_out_page(t0 + ten);
        assert!(lane.waits_out_a_page());
        lane.queue(Origin::Automatic, t0);
        lane.defer(t0 + PANEL_TICK);
        assert!(!lane.hand_over_due(t0 + ten - RECHECK_EVERY), "a reading");
        assert_eq!(lane.take_queued(t0, false), None);
        assert!(!lane.hand_over_due(t0 + ten - RECHECK_EVERY), "no moment");
        lane.queue_after_listing(Origin::Automatic, t0, Ok(()));
        assert!(!lane.hand_over_due(t0 + ten - RECHECK_EVERY), "its own run");
        assert!(lane.waits_out_a_page());
        assert!(lane.hand_over_due(t0 + ten));
        lane.defer(t0 + ten + PANEL_TICK);
        assert!(!lane.hand_over_due(t0 + ten), "a reading after the page");
        assert!(lane.hand_over_due(t0 + ten + PANEL_TICK));
        lane.queue(Origin::Ctl { now: false }, t0);
        assert!(lane.waits_out_a_page(), "an ask rides the step");
        let _ = lane.take_queued(t0 + ten + PANEL_TICK, true);
        lane.queue(Origin::Ctl { now: false }, t0);
        lane.wait_out_page(t0 + ten);
        assert!(!lane.waits_out_a_page(), "only the window's own step");

        let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
            #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
            lane.queue_fda(origin, at);
        };
        queue_fda(&mut lane, Origin::Automatic, t0);
        lane.fda_wait_out_page(t0 + ten);
        lane.defer_fda(t0 + PANEL_TICK);
        assert!(
            !lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY),
            "a reading"
        );
        assert_eq!(lane.take_queued_fda(t0, false), None);
        assert!(
            !lane.fda_hand_over_due(t0 + ten - RECHECK_EVERY),
            "no moment"
        );
        assert!(lane.fda_hand_over_due(t0 + ten));
    }

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
        lane.queue(Origin::Ctl { now: false }, t0);
        assert_eq!(lane.take_queued(t0 + ASKED_LAPSE, true), None);
        assert_eq!(lane.queued(), None, "lapsed");
        // The Full Disk Access queue keeps the same order: the window's own outranks a
        // client's act, which lapses while the window's own waits on.
        let queue_fda = |lane: &mut Lane, origin: Origin, at: Instant| {
            #[expect(clippy::disallowed_methods, reason = "the lane's own test")]
            lane.queue_fda(origin, at);
        };
        queue_fda(&mut lane, Origin::Ctl { now: false }, t0);
        queue_fda(&mut lane, Origin::Ctl { now: false }, t0 + RECHECK_EVERY);
        assert_eq!(lane.fda_queued(), Some(Origin::Ctl { now: false }));
        assert_eq!(lane.take_queued_fda(t0 + ASKED_LAPSE, false), None);
        assert_eq!(lane.fda_queued(), None, "a client's act lapsed");
        queue_fda(&mut lane, Origin::Ctl { now: false }, t0);
        queue_fda(&mut lane, Origin::Automatic, t0);
        queue_fda(&mut lane, Origin::Ctl { now: false }, t0);
        assert_eq!(
            lane.fda_queued(),
            Some(Origin::Automatic),
            "the window's own wins"
        );
        assert_eq!(
            lane.take_queued_fda(late, true),
            Some(Origin::Automatic),
            "and never lapses"
        );
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
        let mut inert = Lane::new(true);
        assert!(!inert.windowed());
        inert.queue(Origin::Automatic, t0);
        assert_eq!(inert.next_deadline(t0, || true), None);
    }

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
        let mut inert = Lane::windowed_for_test(test_reach(), DevToolsHelperArm::inert());
        assert!(!inert.show(show(absent(2), None), t0, wake));
        assert!(inert.panel().is_none());
        assert_eq!(inert.next_deadline(t0, || true), None);
    }

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
        assert_eq!(drift(placed, &docked, placed, &laptop), Drift::Still);
    }

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
        assert!(lane.start_listing(Listing::HandOver(Origin::Settings), nudge()));
        assert!(lane.start_listing(Listing::HandOver(Origin::Digest), nudge()));
        assert_eq!(lane.listing(), Some(Listing::HandOver(Origin::Settings)));
        let mut taken = None;
        until(|| {
            taken = lane.take_listed();
            taken.is_some()
        });
        assert!(taken.is_some_and(|(_, _, waiting)| waiting.digest));
        assert!(Listing::ShowList.rank() < Listing::HandOver(Origin::Automatic).rank());
        assert!(
            Listing::HandOver(Origin::Ctl { now: true }).rank()
                < Listing::HandOver(Origin::Settings).rank()
        );
    }

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

    fn events_of(lane: &Lane) -> PanelEvents {
        PanelEvents {
            panel: lane.generation,
            shared: Arc::clone(&lane.shared),
            wake: nudge(),
        }
    }
}
