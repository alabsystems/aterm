// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission walk (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.4a): one helper panel
//! that takes the person through every macOS grant aterm still lacks, in [`Subject::ORDER`] —
//! Full Disk Access, Developer Tools, Accessibility (only for the Messages and FaceTime
//! sign-outs), then debugger access — each page opening its own System Settings deep link and
//! turning to the next a moment after its switch reads on ([`crate::devtools_helper::DONE_FOR`]).
//! Debugger access has no page: the walk has System State's once-per-Mac debugger step look at
//! once, and that step starts the change with the window's own admission at its own moment
//! (`docs/DESIGN-system-state-2026-10-21.md` §6). The walk is no person's press on the row's
//! `Turn On…`, so it never takes that door.
//!
//! * It starts by itself once, on a cold first launch ([`starts`]): no page aterm brought up
//!   before, System State in front of an active app, the person present ([`START_PRESENT_WITHIN`])
//!   and not typing ([`START_KEYS_QUIET`]). An update's successor never starts one. `aterm ctl
//!   permissions walk now` and the Permissions row's press start one at any time.
//! * It is the owner's press (`devtools_helper::Origin::Walk`): no quiet moment, no ten-minute
//!   gap between its pages. A Not now holds the page it was said on, as on any panel. On a page
//!   the start named (the verb's subjects, the row pressed) the walk goes on only to another
//!   named page, and ends after the last, since the pages asked for were declined; on any other
//!   it goes on to the next. The close box ends it.
//! * A grant already held is skipped without opening its page ([`Walk::advance`]); while a page
//!   is up the window reads its grant every [`GRANT_POLL`].

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

/// How often the window reads the grant the walk's page asks for while the panel is up: the
/// Full Disk Access probe, `AXIsProcessTrusted`, the Developer Tools list.
pub(crate) const GRANT_POLL: Duration = Duration::from_millis(250);

/// How soon after a walk the control verb started it may start another: any program in a
/// session can send the verb (the instance token is readable by every program of this user), so
/// it brings System Settings forward at most once a minute, as the Developer Tools verb's `now`
/// did before it.
pub(crate) const VERB_AGAIN_AFTER: Duration = Duration::from_secs(60);

/// How long a due page waits for the first reading of its grant (a launch's Full Disk Access
/// probe, the debugger rule) before it opens, or is tried, without one.
pub(crate) const UNREAD_WAIT: Duration = Duration::from_secs(3);

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
    /// Full Disk Access or Developer Tools page on this Mac before (either page's opened marker)
    /// is read only once these facts say start (`App::fresh_mac`).
    pub(crate) cold: bool,
    /// System State is the visible view of a focused window of the active app.
    pub(crate) presented: bool,
    /// The keyboard or pointer was touched within [`START_PRESENT_WITHIN`].
    pub(crate) present: bool,
    /// A key was typed within [`START_KEYS_QUIET`].
    pub(crate) typing: bool,
}

/// Whether the walk starts by itself now. Pure.
#[must_use]
pub(crate) const fn starts(facts: StartFacts) -> bool {
    facts.cold && facts.presented && facts.present && !facts.typing
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
    /// How many of `subjects`, from the first, were asked for by name (a row's press, the
    /// control verb): their pages open without waiting for a first reading.
    named: usize,
    at: usize,
    step: Step,
    /// When the current page became due.
    due_at: Instant,
}

impl Walk {
    /// A walk over `subjects` from `now`; `None` when there are none.
    pub(crate) fn new(subjects: Vec<Subject>, now: Instant) -> Option<Self> {
        Self::named(subjects, &[], now)
    }

    /// A walk over `subjects`, those in `named` asked for by name.
    pub(crate) fn named(subjects: Vec<Subject>, named: &[Subject], now: Instant) -> Option<Self> {
        let named = subjects
            .iter()
            .take_while(|subject| named.contains(subject))
            .count();
        (!subjects.is_empty()).then_some(Self {
            subjects,
            named,
            at: 0,
            step: Step::Due,
            due_at: now,
        })
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
    pub(crate) const fn at_named(&self) -> bool {
        self.at < self.named
    }

    /// Where its current page stands.
    pub(crate) const fn step(&self) -> Step {
        self.step
    }

    /// The current page's place, 1-based, and the walk's length: the panel's "1 of N".
    pub(crate) fn position(&self) -> (usize, usize) {
        (self.at + 1, self.subjects.len())
    }

    /// The current page came up (or is on its way).
    pub(crate) const fn opened(&mut self) {
        self.step = Step::Up;
    }

    /// On to the next subject at `now` (its grant read on, a Not now, or held already): the
    /// next subject, `None` once the walk is over.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<Subject> {
        self.at = self.at.saturating_add(1).min(self.subjects.len());
        self.step = Step::Due;
        self.due_at = now;
        self.current()
    }

    /// The subjects, in the walk's order.
    pub(crate) fn subjects(&self) -> &[Subject] {
        &self.subjects
    }

    /// Whether the walk has a page after the current one: the panel is kept up between them.
    pub(crate) fn continues(&self) -> bool {
        self.at + 1 < self.subjects.len()
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
}

impl std::fmt::Debug for WalkReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkReach").finish_non_exhaustive()
    }
}

impl WalkReach {
    /// Nothing reached: never trusted, nothing asked.
    pub(crate) const INERT: Self = Self {
        ax_trusted: || false,
        ask_automation: || {},
    };

    /// The live arms of a windowed macOS instance.
    pub(crate) const fn live() -> Self {
        Self {
            ax_trusted: live::ax_trusted,
            ask_automation: live::ask_automation,
        }
    }
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
    /// When the control verb last started a walk ([`VERB_AGAIN_AFTER`]).
    verb_started_at: Option<Instant>,
    /// The walk under way yields to a helper panel it did not bring up: the control verb's,
    /// which any program in a session can send, never takes the owner's panel away.
    yields: bool,
    /// A look found the Full Disk Access or Developer Tools page's opened marker: this Mac had
    /// a page brought up, so no walk starts by itself for the first time, and the markers are
    /// never read again in this process ([`App::fresh_mac`]).
    page_seen: bool,
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
            page_seen: false,
        }
    }

    pub(crate) const fn reach(&self) -> WalkReach {
        self.reach
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

    /// Whether a walk in this process already took `subject`.
    pub(crate) const fn walked(&self, subject: Subject) -> bool {
        self.walked[subject.index()]
    }

    /// Whether any walk ran in this process.
    pub(crate) fn any_walked(&self) -> bool {
        self.walked.iter().any(|walked| *walked)
    }

    /// Start `walk`, started `by` (aterm.log's words); it replaces any under way and takes the
    /// pages it opens as the owner's press would.
    pub(crate) fn start(&mut self, walk: Walk, by: &'static str) {
        self.yields = false;
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

    /// The walk is over (every page answered, or the close box).
    pub(crate) fn end(&mut self, why: &str) {
        if self.walk.take().is_some() {
            aterm_log::info!("permission walk ({}): over ({why})", self.by);
        }
    }
}

/// What the window's walk answered a start (`aterm ctl permissions walk now`, a row's press).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Started {
    /// A headless instance (or not macOS): nothing, ever.
    Headless,
    /// Every grant asked for is held: no walk.
    Granted,
    /// The control verb started a walk within [`VERB_AGAIN_AFTER`]: none now.
    Recent,
    /// Every grant missing is held back from the control verb by the owner's Not now.
    Declined,
    /// The walk is under way over these subjects (one already under way keeps going).
    Walking(Vec<Subject>),
}

impl Started {
    /// The control verb's answer line.
    pub(crate) fn reply(&self) -> String {
        match self {
            Self::Headless => {
                "ERR headless: a headless instance brings no System Settings page up\n".to_owned()
            }
            Self::Granted => "OK granted\n".to_owned(),
            Self::Recent => "OK recent\n".to_owned(),
            Self::Declined => "OK declined\n".to_owned(),
            Self::Walking(subjects) => format!(
                "OK walking {}\n",
                subjects
                    .iter()
                    .map(|subject| subject.word())
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
    /// Another macOS gesture owns the slot: look again next turn.
    Wait,
    /// It could not open; the walk goes on without it.
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
    /// process took and the owner's Not now does not hold back ([`Self::declined_for_the_verb`]:
    /// a re-ask after a decline waits for a quiet moment on the automatic hand-overs). A walk the
    /// setup run has not admitted (no approved revision, a `[privacy]` switch off) never starts
    /// by itself, nor one while another of the window's gestures or a macOS dialog of aterm's is
    /// up. This runs at every park, so the in-memory readings come first and the page markers
    /// last ([`Self::fresh_mac`]).
    fn look_for_walk_start(&mut self, now: Instant) {
        let cold = self.devtools_helper.cold_start();
        if !cold || self.access_revision().is_none() || !self.access_steps_allowed() {
            return;
        }
        let facts = StartFacts {
            cold,
            presented: self.system_state_in_front(),
            present: (self.user_input_recent)(START_PRESENT_WITHIN),
            typing: self
                .last_keystroke_at
                .is_some_and(|at| now.saturating_duration_since(at) < START_KEYS_QUIET),
        };
        if !starts(facts) || self.access_gesture_up(now) || self.access_dialog_up() {
            return;
        }
        if !self.permission_walk.any_walked() && !self.fresh_mac() {
            return;
        }
        let missing = Subject::ORDER.map(|subject| {
            !self.permission_walk.walked(subject)
                && !self.declined_for_the_verb(subject)
                && self.walk_missing(subject, false)
        });
        if let Some(walk) = Walk::new(subjects(&[], |subject| missing[subject.index()]), now) {
            self.permission_walk.start(walk, "first launch");
        }
    }

    /// Whether aterm never brought the Full Disk Access or Developer Tools page up on this Mac
    /// (neither page's opened marker): read at most until a marker is found, which no later
    /// look in this process reads again.
    fn fresh_mac(&mut self) -> bool {
        if self.permission_walk.page_seen {
            return false;
        }
        let fresh = (self.devtools_helper.reach().last_opened)().is_none()
            && (self.devtools_helper.fda_reach().last_opened)().is_none();
        self.permission_walk.page_seen = !fresh;
        fresh
    }

    /// Start a walk over `named` first, then every other subject missing, started `by`
    /// (aterm.log's words): a person's press on a Permissions row, or the control verb through
    /// [`Self::ask_permission_walk`]. One under way keeps going.
    pub(crate) fn start_permission_walk(
        &mut self,
        named: &[Subject],
        by: &'static str,
        now: Instant,
    ) -> Started {
        self.start_walk_without(named, [false; 4], false, by, now)
    }

    /// [`Self::start_permission_walk`] leaving out each subject `held_back` names (by
    /// [`Subject::ORDER`]): a decline that holds the control verb's walk back. A walk that
    /// `yields` opens no page while a helper panel it did not bring up is up.
    fn start_walk_without(
        &mut self,
        named: &[Subject],
        held_back: [bool; 4],
        yields: bool,
        by: &'static str,
        now: Instant,
    ) -> Started {
        if !cfg!(target_os = "macos") || !self.devtools_helper.windowed() {
            return Started::Headless;
        }
        if let Some(walk) = self.permission_walk.walk() {
            return Started::Walking(walk.subjects().to_vec());
        }
        let missing =
            Subject::ORDER.map(|subject| self.walk_missing(subject, named.contains(&subject)));
        let taken =
            Subject::ORDER.map(|subject| missing[subject.index()] && !held_back[subject.index()]);
        let Some(walk) = Walk::named(
            subjects(named, |subject| taken[subject.index()]),
            named,
            now,
        ) else {
            return if missing.contains(&true) {
                Started::Declined
            } else {
                Started::Granted
            };
        };
        let walking = walk.subjects().to_vec();
        self.permission_walk.start(walk, by);
        self.permission_walk.yields = yields;
        self.drive_permission_walk(now);
        Started::Walking(walking)
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
    /// the last the verb started, and without a page the owner's Not now holds back
    /// ([`Self::declined_for_the_verb`]): any program in a session can send the verb, and only
    /// the owner's own press goes past a Not now. A walk under way answers with its subjects.
    pub(crate) fn ask_permission_walk(&mut self, named: &[Subject], now: Instant) -> Started {
        if !self.permission_walk.active()
            && self
                .permission_walk
                .verb_started_at
                .is_some_and(|at| now.saturating_duration_since(at) < VERB_AGAIN_AFTER)
        {
            return Started::Recent;
        }
        let active = self.permission_walk.active();
        let held_back = Subject::ORDER.map(|subject| self.declined_for_the_verb(subject));
        let started = self.start_walk_without(named, held_back, true, "a control client", now);
        if !active && matches!(started, Started::Walking(_)) {
            self.permission_walk.verb_started_at = Some(now);
        }
        started
    }

    /// Whether the owner's Not now holds `subject` back from the control verb's walk, as it held
    /// the verb's hand-overs (`devtools_helper::declined_for` with [`Origin::Ctl`]): a Not now
    /// under this setup revision, or one inside its span; for Full Disk Access also the access
    /// card's.
    fn declined_for_the_verb(&self, subject: Subject) -> bool {
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
            Subject::Accessibility => self.access_declined_here(Access::Accessibility),
            Subject::Debugger => self.access_declined_here(Access::Debugger),
        }
    }

    /// Whether `subject`'s grant reads held now.
    fn walk_held(&mut self, subject: Subject) -> Held {
        use aterm_containment::consent::{DebuggerMode, FdaState, ProbeLabel};
        match subject {
            Subject::FullDiskAccess => {
                let read = self.consent_panel_facts();
                if read.probe == ProbeLabel::Pending {
                    Held::Unread
                } else if read.fda == FdaState::Granted && self.consent.window_reads_own_grant() {
                    Held::Yes
                } else {
                    Held::No
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
            .then(|| walk.position())
    }

    /// The walk's turn after the hand-overs: how its page closed, the grant of the page that is
    /// up, the next page when one is due; then whether the panel is held up for a next page.
    pub(crate) fn drive_permission_walk(&mut self, now: Instant) {
        if !self.permission_walk.active() {
            self.devtools_helper.hold_for_walk(false);
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
        self.open_due_walk_pages(now);
        match self.permission_walk.walk() {
            Some(walk) => {
                let continues = walk.continues();
                self.devtools_helper.hold_for_walk(continues);
            }
            None => {
                self.devtools_helper.hold_for_walk(false);
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
            self.devtools_helper.hold_for_walk(false);
            self.devtools_helper.release_held();
            self.release_unused_access_dialog(now);
        }
    }

    /// A walk page closed `why`: done or switched on goes on to the next page, and so does a
    /// Not now on a page the walk added; a Not now on a page asked for by name (a row's press,
    /// the control verb) goes on only to another page asked for by name, and otherwise ends the
    /// walk there, as do the close box, the page's ten minutes, System Settings quitting and
    /// another panel.
    fn walk_page_closed(&mut self, page: Page, why: Closed, now: Instant) {
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
                let _ = walk.advance(now);
                if !walk.at_named() {
                    self.permission_walk
                        .end("Not now on the last page asked for");
                }
            }
            Closed::CloseBox => self.permission_walk.end("the close box"),
            Closed::Lapsed => self.permission_walk.end("its page lapsed"),
            Closed::SettingsGone => self.permission_walk.end("System Settings quit"),
            Closed::Replaced => self.permission_walk.end("another panel replaced its page"),
        }
    }

    /// While a walk page is up, read its grant every [`GRANT_POLL`]: a fresh Full Disk Access
    /// probe (its answer turns the page in `App::tick_fda_hand_over`) and `AXIsProcessTrusted`.
    /// The Developer Tools page reads its own list at that pace.
    fn read_walk_grant(&mut self, now: Instant) {
        let Some(walk) = self.permission_walk.walk() else {
            return;
        };
        if walk.step() != Step::Up {
            return;
        }
        match walk.current() {
            Some(Subject::FullDiskAccess) => {
                let _ = self
                    .consent
                    .fda(self.config.privacy_probe_gate(), GRANT_POLL);
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
    /// for a quiet moment.
    fn open_walk_page(&mut self, subject: Subject, now: Instant) -> Opened {
        if self.permission_walk.yields
            && self
                .devtools_helper
                .panel()
                .is_some_and(|panel| panel.origin() != Origin::Walk)
        {
            return Opened::Wait;
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
                self.devtools_helper.hold_for_walk(false);
                self.devtools_helper.release_held();
                self.drop_access_dialog();
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

    /// One Apple event to System Events on a worker thread: macOS asks its Automation question
    /// for aterm now, or answers at once with an earlier answer.
    pub(super) fn ask_automation() {
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-automation-ask".to_owned(),
            crate::qos::Role::Background,
            || {
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
                    Err(error) => aterm_log::info!(
                        "permission walk: the Automation ask did not finish: {error}"
                    ),
                }
            },
        );
        if let Err(error) = spawned {
            aterm_log::warn!("permission walk: the Automation ask could not start: {error}");
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod live {
    pub(super) const fn ax_trusted() -> bool {
        false
    }

    pub(super) const fn ask_automation() {}
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

    #[test]
    fn a_walk_counts_its_pages_and_ends_after_the_last() {
        let now = Instant::now();
        let mut walk =
            Walk::new(vec![Subject::FullDiskAccess, Subject::Debugger], now).expect("two pages");
        assert_eq!(walk.position(), (1, 2));
        assert!(walk.continues());
        walk.opened();
        assert_eq!(walk.step(), Step::Up);
        assert_eq!(walk.advance(now), Some(Subject::Debugger));
        assert_eq!((walk.position(), walk.step()), ((2, 2), Step::Due));
        assert!(!walk.continues());
        assert_eq!(walk.advance(now), None);
        assert_eq!(walk.advance(now), None, "stays over");
        assert!(Walk::new(Vec::new(), now).is_none());
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
}
