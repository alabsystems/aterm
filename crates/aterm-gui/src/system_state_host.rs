// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Boundary between the System State tab and the binary's setup workers.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{Access, Plan, Selection, Setting};

use crate::native_app::{AppKind, NativeApp};
use crate::system_state::{
    Admission, PasswordAcceptance, SetupOperation, SetupStatus, SetupStatusRow, SystemStateAction,
};
use crate::workstation_setup::{OfferKind, Request, WorkstationSetupDecision};
use crate::{App, Wake};

/// Provisioning callbacks installed by the shipped binary.
#[derive(Clone, Copy)]
pub struct InitialSetupHandlers {
    /// Durably admit `plan` (with its vault-typed flag and optional join
    /// password) as the next run; the receipt says whose choice it is and
    /// whether it shapes the plan. A choice takes effect at once (design §3.3):
    /// a running pass stops at its next stage boundary and `plan` runs after
    /// it, and a later call overtakes one still waiting, whose dropped receipt
    /// keeps the page's input.
    pub apply: fn(Plan, bool, Option<SecretBytes>, InitialSetupApplyReceipt) -> bool,
    /// Start Check again for `plan`: its forced reading of every row, and the
    /// check itself, which a setup pass holding the worker refuses (design
    /// §3.2). Check again started when either did; the window labels it a
    /// check only when the check did.
    pub check: fn(Plan) -> CheckStarted,
    pub restore: fn() -> bool,
    pub status: fn() -> SetupStatus,
    /// Start one read-only, prompt-free observation of every row for `plan`
    /// (managed rows decide which network reads run). Its result arrives in
    /// a later `status()`. Never holds the setup slot.
    /// Returns whether the worker accepted the observation.
    pub observe: fn(Plan) -> bool,
    /// While the GitHub repositories need a person, look on a worker whether a token renewed
    /// in the vault reached this Mac: the vault's materialization mark, a local reading that
    /// asks nothing of GitHub. Only when it moved since that row was last read is the GitHub
    /// repositories row read again for `plan`, and that row alone (design §3.6). Returns
    /// whether the look started.
    pub look_for_token: fn(Plan) -> bool,
    /// Stop was pressed: the pass going now stops at its next stage boundary,
    /// and so does the run of every plan sent before the press that is still
    /// being admitted, once it registers (design §3.3). Asked once: the worker
    /// keeps it. It touches no file on the UI thread: a worker saves the Stop,
    /// and [`Self::settle`] waits for that save.
    pub stop: fn(),
    /// The app is quitting, or an update's handoff is about to end it: wait,
    /// briefly, for what a Stop still owes the person's records (a plan it
    /// stops still being admitted, the Stop's save), so a quit right after a
    /// Stop never resumes the run (design §3.3). At once when nothing is owed.
    pub settle: fn(),
    /// `true`: the app is quitting, or an update's Commit is about to be written, so the
    /// setup stages running now are cut off by no crash, and the next launch resumes them
    /// without counting toward a park. `false`: that Commit was not written, or returned, so
    /// the handoff did not go and the notes are taken back: aterm stopping in one of those
    /// stages later counts again (design §6.5). A handoff notes only right before its write,
    /// never at its start, which a refusal may still follow.
    pub cut_off: fn(bool),
    /// Turn On Sync on the Secrets vault row: a worker reinstalls this Mac's
    /// subscriber, removes the record of the person's opt-out and reads the
    /// row again (design §3.3). Returns whether that worker started.
    pub sync_on: fn() -> bool,
    /// The Command Line Tools tab said Apple's Command Line Tools are installed (design §3.3):
    /// read the GitHub repositories row again at once and wake the run waiting on them, so the
    /// page moves on without waiting for its next look.
    pub tools_installed: fn(),
    /// The window verified its own Full Disk Access grant without durable
    /// access history. Refresh observations and finish any
    /// previously owed preference work on a worker, without admitting prompts.
    pub fda_granted: fn(),
    /// The Secrets vault row's Join under its phrase field: on a worker, ALab lets this Mac in
    /// with the phrase a member shows once its person pressed Allow, as `aterm members join
    /// <phrase>` does (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.6 step 5); the receipt says
    /// whether it did, and the Secrets row is read again. Returns whether the worker started.
    pub join_phrase: fn(SecretBytes, PhraseJoinReceipt) -> bool,
}

#[cfg(test)]
impl InitialSetupHandlers {
    pub(crate) fn inert_for_test() -> Self {
        Self {
            apply: |_, _, _, _| false,
            check: |_| CheckStarted::default(),
            restore: || false,
            status: SetupStatus::default,
            observe: |_| false,
            look_for_token: |_| false,
            stop: || {},
            settle: || {},
            cut_off: |_| {},
            sync_on: || false,
            tools_installed: || {},
            fda_granted: || {},
            join_phrase: |_, _| false,
        }
    }
}

/// What a phrase typed on the Secrets vault row came to ([`PhraseJoinReceipt`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhraseOutcome {
    /// It let this Mac in.
    Joined,
    /// It reached a member and let nobody in, or it is not three of the list's words: spent, so
    /// the member's person presses Allow again for a new one.
    Refused,
    /// It reached no member: no ALab Mac answered on this network, so nothing was asked of it
    /// and the same words go again.
    NotReached,
    /// The try stopped before it said what the phrase came to (its worker ended): it may not
    /// have sent the phrase, so the words stay and Join tries again.
    Stopped,
}

/// What the worker of a phrase typed on the Secrets vault row says of it
/// ([`InitialSetupHandlers::join_phrase`]): what it came to ([`PhraseOutcome`]). Dropped unsaid,
/// the try stopped ([`PhraseOutcome::Stopped`]).
#[must_use = "say what the phrase came to; dropping says the try stopped"]
#[derive(Debug)]
pub struct PhraseJoinReceipt {
    answer: Option<Sender<PhraseOutcome>>,
}

impl PhraseJoinReceipt {
    fn new() -> (Self, Receiver<PhraseOutcome>) {
        let (answer, receiver) = mpsc::channel();
        (
            Self {
                answer: Some(answer),
            },
            receiver,
        )
    }

    /// The join with the phrase ended with `outcome`.
    pub fn finished(mut self, outcome: PhraseOutcome) {
        self.finish(outcome);
    }

    fn finish(&mut self, outcome: PhraseOutcome) {
        if let Some(answer) = self.answer.take() {
            let _ = answer.send(outcome);
            crate::workstation_setup::send_setup_wake(Wake::InitialSetupPhraseJoined);
        }
    }
}

impl Drop for PhraseJoinReceipt {
    fn drop(&mut self) {
        self.finish(PhraseOutcome::Stopped);
    }
}

/// What a press of Check again started ([`InitialSetupHandlers::check`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckStarted {
    /// The forced reading of every row, GitHub repositories included.
    pub reading: bool,
    /// The check on the setup worker: refused while a pass holds it.
    pub check: bool,
}

/// Receipt owned by a queued Apply until its plan is durably authorized.
#[must_use = "accept only after the plan is durable; dropping preserves the draft"]
#[derive(Debug)]
pub struct InitialSetupApplyReceipt {
    answer: Option<Sender<bool>>,
    admission: Admission,
    chosen: bool,
    changed: Option<Selection>,
    password: Option<PasswordAcceptance>,
}

impl InitialSetupApplyReceipt {
    fn new(
        admission: Admission,
        chosen: bool,
        changed: Option<Selection>,
        password: Option<PasswordAcceptance>,
    ) -> (Self, Receiver<bool>) {
        let (answer, receiver) = mpsc::channel();
        (
            Self {
                answer: Some(answer),
                admission,
                chosen,
                changed,
                password,
            },
            receiver,
        )
    }

    /// A person's receipt for the setup worker's own tests to hand its entry
    /// points, and the answer it sends ([`Self::accept`], or `false` dropped).
    #[cfg(any(test, debug_assertions))]
    pub fn detached(chosen: bool) -> (Self, Receiver<bool>) {
        Self::detached_as(Admission::Person, chosen)
    }

    /// [`Self::detached`] for an Apply whose word on whose it is is `admission`: the page's own
    /// answer to an offer ([`Admission::Automatic`]), say.
    #[cfg(any(test, debug_assertions))]
    pub fn detached_as(admission: Admission, chosen: bool) -> (Self, Receiver<bool>) {
        Self::new(admission, chosen, None, None)
    }

    /// The settings the page changed since it last took the plan in force, which the worker
    /// applies onto that plan as it admits this one ([`crate::SetupAnswer::changed`]).
    pub fn changed(&self) -> Option<Selection> {
        self.changed
    }

    /// Whether the plan is a group choice (or a Join) made on the page, which
    /// makes it the chooser's for good, rather than the plan as it stood (Set
    /// up now, or the first-run offer), which leaves a provisional plan
    /// provisional (design §3.5; [`crate::SetupAnswer::chosen`]).
    pub fn chosen(&self) -> bool {
        self.chosen
    }

    /// Whose choice the plan is (design §6): the page's own answer to the
    /// first-run offer, a person's press, or a control client's `act` or
    /// `key`. Only the first two are the window's own admission; a client's
    /// never stands as the yes the access steps need, and its plan is not
    /// the provisional one aterm chose by itself.
    pub fn admission(&self) -> Admission {
        self.admission
    }

    /// Confirm that the worker holds the claim and durably saved the reviewed plan.
    pub fn accept(mut self) {
        self.finish(true);
    }

    fn finish(&mut self, accepted: bool) {
        if let Some(answer) = self.answer.take() {
            if accepted && let Some(password) = &self.password {
                password.accept();
            }
            let _ = answer.send(accepted);
            crate::workstation_setup::send_setup_wake(Wake::InitialSetupApplyResolved);
        }
    }
}

impl Drop for InitialSetupApplyReceipt {
    fn drop(&mut self) {
        self.finish(false);
    }
}

struct PendingApply {
    instance: crate::native_app::AppInstanceId,
    admission: Admission,
    plan: Plan,
    answer: Receiver<bool>,
    /// Stop may keep this admission's answer while withdrawing its right to
    /// release automatic permission guidance in this process.
    release_permissions: bool,
}

static HANDLERS: Mutex<Option<InitialSetupHandlers>> = Mutex::new(None);

pub fn set_initial_setup_handlers(handlers: InitialSetupHandlers) {
    if let Ok(mut current) = HANDLERS.lock() {
        *current = Some(handlers);
    }
}

pub fn publish_initial_setup_status(status: SetupStatus) {
    crate::workstation_setup::send_setup_wake(Wake::InitialSetupStatus {
        generation: None,
        status,
    });
}

/// The dev seam that shows System State a fixed scenario in place of the
/// setup worker (`crate::system_state::fixture_status`): its actions are inert.
const FIXTURE_SEAM: &str = "ATERM_DEV_SYSTEM_STATE_FIXTURE";

/// A read that met a write of the saved setup ([`SetupStatus::changing`]) is read again this
/// soon: the write is a rename, over in a moment.
const CHANGING_REREAD: Duration = Duration::from_millis(100);

/// Reads in a row that may meet a write before the page shows the next as it is: records that
/// keep changing show as its pending row. The window's own admission keeps the last read that
/// named a plan all the same, since a read that met a write says nothing of one.
const CHANGING_READS: u8 = 10;

/// How long a shown page goes between two looks for a renewed GitHub token while the GitHub
/// repositories need a person ([`SetupStatus::github_needs_person`]): their remedy is the
/// vault's (a token renewed there and synced here), which tells the page nothing. Each look
/// checks the vault's local mark alone, and reads the GitHub repositories row again only when
/// that mark moved ([`InitialSetupHandlers::look_for_token`]); the page reads every row again
/// whenever its window regains focus (§3.6).
pub(crate) const TOKEN_LOOK: Duration = Duration::from_secs(60);

/// How recently a keyboard, mouse or trackpad must have been used for a person stage to start
/// ([`App::person_at_this_screen`]): a person who just opened aterm has.
pub(crate) const PERSON_PRESENT_WITHIN: Duration = Duration::from_secs(60);

/// How often a page in front whose sign-outs are parked for want of a person reads presence
/// again with nothing else to wake it ([`Host::presence_look`]), so a person who comes back,
/// with input anywhere, wakes the parked sign-outs within this.
pub(crate) const PRESENCE_LOOK: Duration = Duration::from_secs(1);

/// How the window reads whether a person is at this Mac: macOS's idle time where it gives one,
/// else the last key, click or scroll in an aterm window ([`App::person_present_within`]).
#[derive(Clone, Copy)]
pub(crate) struct Presence {
    pub(crate) input_recent: fn(Duration) -> bool,
    pub(crate) input_readable: fn() -> bool,
    pub(crate) last_input: Option<Instant>,
}

impl Presence {
    /// Whether a person touched this Mac within `within` of `now`.
    pub(crate) fn within(self, within: Duration, now: Instant) -> bool {
        if (self.input_readable)() {
            (self.input_recent)(within)
        } else {
            self.last_input
                .is_some_and(|at| now.saturating_duration_since(at) < within)
        }
    }
}

/// The window's [`Presence`], as it last published System State's frontness
/// ([`App::publish_system_state_front`]), which the setup worker reads presence through when it
/// uses its `on_screen` port ([`system_state_on_screen`]).
static PRESENCE: Mutex<Option<Presence>> = Mutex::new(None);

/// The window's [`Presence`] as it last published it ([`App::publish_system_state_front`]), in a
/// process with a window: the reader [`crate::person_at_this_screen`] uses there.
pub(crate) fn published_presence() -> Option<Presence> {
    *PRESENCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether a person touched this Mac within [`PERSON_PRESENT_WITHIN`] of `now` (pure but for its
/// readers): by `published`, the window's reader with its fallback to aterm's own last input,
/// where this process has a window; else by `os`, macOS's idle time alone.
pub(crate) fn person_touched(
    published: Option<Presence>,
    os: impl FnOnce() -> bool,
    now: Instant,
) -> bool {
    published.map_or_else(os, |presence| presence.within(PERSON_PRESENT_WITHIN, now))
}

/// The setup worker's `on_screen` port, which starts the Messages and FaceTime sign-outs, a
/// person stage (init-on-by-default design §6.4): System State in front with a person at this
/// Mac as the window last published it ([`aterm_types::macos_setup::system_state_front`], on
/// its events and page ticks, the screen's lock included), and a person here now, read as the
/// worker asks: a page with nothing running owes the window no tick, so a person who walked
/// away from it since its last publication never opens the sign-outs.
#[must_use]
pub fn system_state_on_screen() -> bool {
    aterm_types::macos_setup::system_state_front()
        && PRESENCE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|presence| presence.within(PERSON_PRESENT_WITHIN, Instant::now()))
}

/// How a request to read the shown page's rows went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observe {
    /// The reading started (or there is nothing to read: a scenario, no worker).
    Started,
    /// The worker did not start it: it is tried again a second later.
    Refused,
    /// The page waits for the plan in force (§3.5): read at the first tick that knows it.
    NoPlan,
}

#[derive(Default)]
pub(crate) struct Host {
    pending: Option<Request>,
    pending_apply: Option<PendingApply>,
    /// The try of the phrase typed on the Secrets vault row whose join runs, and its answer
    /// ([`PhraseJoinReceipt`]), while it runs.
    phrase_join: Option<(crate::system_state::PhraseTry, Receiver<PhraseOutcome>)>,
    /// An accepted plan's run has not shown up in a fresh status yet: the
    /// page stays busy meanwhile.
    awaiting_run: bool,
    /// A test's own handlers, in place of the binary's ([`App::system_state_handlers`]).
    #[cfg(test)]
    handlers: Option<InitialSetupHandlers>,
    /// An accepted nonrun operation, reserved across controller closure until
    /// a fresh status says it ended or a setup pass takes over.
    operation: Option<SetupOperation>,
    next_status: Option<Instant>,
    status_in_flight: bool,
    /// The window has shown a status (the worker's, or a scenario's): until then a page that is
    /// open asks for one read, which nothing else may bring (a launch whose worker says
    /// nothing, a page opened by hand), so it never stays at `Checking this Mac…`.
    status_shown: bool,
    /// The one thread that reads the worker's status, one read for each poll sent it
    /// ([`Host::send_status_poll`]): started at the first poll, and let go once setup has
    /// nothing left to say ([`App::publish_system_state`]).
    status_reader: Option<Sender<StatusPoll>>,
    /// Reads in a row that met a write of the saved setup ([`SetupStatus::changing`]).
    changing_reads: u8,
    next_tick: Option<Instant>,
    /// When a page in front whose sign-outs are parked for want of a person reads presence again
    /// ([`PRESENCE_LOOK`]): its return wakes them ([`App::publish_system_state_front`]).
    presence_look: Option<Instant>,
    generation: u64,
    /// An offer was shown in this process: a later one (an offer no window could show the first
    /// time, or one whose page closed before it answered) opens its tab behind the person's,
    /// where the page answers it, never taking the tab they are in (design §6).
    review_shown: bool,
    /// A System State view was visible at the last tick: showing it again
    /// asks for a fresh observation (§3.5).
    shown: bool,
    /// A refused observation start is tried again on the page's tick,
    /// no sooner than this deadline.
    observe_retry: Option<Instant>,
    /// When the shown page's rows were last read, or last looked at for a renewed GitHub
    /// token ([`TOKEN_LOOK`]).
    token_looked_at: Option<Instant>,
    /// The plan in force when the shown page's rows were last read: a status
    /// that names another (a Restore in a terminal) has them read again.
    observed_plan: Option<Plan>,
    /// The access approval whose Not now answers the last reading interpreted.
    /// A new approval can keep the same plan and still needs fresh Access rows.
    observed_access_revision: Option<u64>,
    action_refusal: Option<SetupStatusRow>,
    latest_status: SetupStatus,
    /// The access step last published with the status.
    access_step: Option<Access>,
    /// The permission walk's page last published with the status (the card's `Step 1 of 3`).
    walk_page: Option<crate::system_state::WalkPage>,
    /// Since when the vault's join has asked the Macs in ALab to let this Mac in, while it asks:
    /// the page says it in the card only once it has asked for
    /// [`crate::system_state::JOIN_QUIET`] (lead decision 6 of the setup screen).
    asking_since: Option<Instant>,
    /// What the status last published said of that asking ([`SetupStatus::asking_long`]).
    asking_long: bool,
    /// Where the moment the join began asking is kept across launches (wall-clock
    /// milliseconds), so a relaunch never starts the join's quiet again: `None` keeps it in
    /// this process alone (tests, a headless instance).
    asking_record: Option<std::path::PathBuf>,
    /// When the page next does something by itself (a run, or a Check, it starts after its
    /// wait: `SystemStateApp::next_by_itself`), which wakes the loop then.
    page_due: Option<Instant>,
    /// When the setup run going now started, while one goes.
    run_started: Option<Instant>,
    /// How long the last setup run took to finish ([`SetupStatus::ready_in`]).
    ready_in: Option<Duration>,
    /// The dev scenario shown in place of the worker, read once.
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fixture: Option<SetupStatus>,
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fixture_read: bool,
    /// The line the dev scenario's views open (`<scenario>@<line>`).
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fixture_line: Option<crate::system_state::lines::Line>,
}

/// The file, in this login's state folder, that keeps when the vault's join began asking the
/// Macs in ALab ([`Host::asking_record`]).
const JOIN_ASKING_RECORD: &str = "system-state-join-asking";

/// One read the status reader is asked for: the status handler in force, and the generation its
/// answer carries ([`Host::send_status_poll`]).
type StatusPoll = (fn() -> SetupStatus, u64);

impl Host {
    /// The window's own host: the join's asking start kept in this login's state folder.
    pub(crate) fn for_launch(headless: bool) -> Self {
        Self {
            asking_record: if headless || cfg!(test) {
                None
            } else {
                aterm_types::dirs::state_dir().map(|dir| dir.join(JOIN_ASKING_RECORD))
            },
            ..Self::default()
        }
    }

    /// The status generation a read must carry to land (tests outside this module).
    #[cfg(test)]
    pub(crate) const fn status_generation_for_test(&self) -> u64 {
        self.generation
    }

    /// Whether the run's Accounts stage, a person stage, is parked for want of a person: the
    /// plan in force manages a Messages or FaceTime sign-out whose row does not read settled, its
    /// run has not finished, and the stage's last pass said it waits for a person at this Mac
    /// ([`aterm_types::macos_setup::StageRow::WAITS_FOR_A_PERSON`]). Only then does a page in
    /// front read presence again by itself ([`Self::presence_look`]): a sign-out waiting for
    /// Check again, one that failed, or one waiting for Accessibility changes nothing on a
    /// person's return, so it owes the page no look and no timer wake (§5.6's idle budget).
    fn sign_outs_wait_for_a_person(&self) -> bool {
        let status = &self.latest_status;
        !status.finished
            && status.rows.iter().any(|row| {
                row.detail
                    .ends_with(aterm_types::macos_setup::StageRow::WAITS_FOR_A_PERSON)
            })
            && status.plan.as_ref().is_some_and(|plan| {
                aterm_types::macos_setup::Group::Accounts
                    .settings()
                    .iter()
                    .any(|setting| {
                        plan.selection.contains(*setting)
                            && !status.observed.iter().any(|observation| {
                                observation.subject
                                    == aterm_types::macos_setup::Subject::Setting(*setting)
                                    && observation.standing.settled()
                            })
                    })
            })
    }

    /// The next observation retry or shown-page token look: a real reading timer, which the
    /// page's refresh clock must not postpone once it is due.
    fn read_timer(&self) -> Option<Instant> {
        let token_look = self
            .token_looked_at
            .filter(|_| self.shown && self.latest_status.github_needs_person())
            .map(|at| at + TOKEN_LOOK);
        self.observe_retry.into_iter().chain(token_look).min()
    }

    /// Ask the status reader for one read of `status` (the handlers in force when the poll is
    /// sent), answered with `generation`, starting the reader if none runs (or the last one
    /// ended). `false` when no reader could take it.
    fn send_status_poll(&mut self, status: fn() -> SetupStatus, generation: u64) -> bool {
        if let Some(reader) = &self.status_reader
            && reader.send((status, generation)).is_ok()
        {
            return true;
        }
        let (polls, asked) = mpsc::channel::<StatusPoll>();
        let started = std::thread::Builder::new()
            .name("aterm-setup-status".into())
            .spawn(move || {
                // Ends once the window lets the reader go and no poll is left to answer.
                while let Ok((status, generation)) = asked.recv() {
                    crate::workstation_setup::send_setup_wake(Wake::InitialSetupStatus {
                        generation: Some(generation),
                        status: status(),
                    });
                }
            });
        if started.is_err() || polls.send((status, generation)).is_err() {
            self.status_reader = None;
            return false;
        }
        self.status_reader = Some(polls);
        true
    }

    /// The status last published to System State (`system_state_access`'s
    /// menu line reads it).
    pub(crate) const fn latest_status(&self) -> &SetupStatus {
        &self.latest_status
    }

    /// The wake [`App::tick_system_state`] serves: the next status poll (unless one
    /// is in flight — its reply wakes the loop itself), and, only while a run goes on a
    /// shown page, whose lockup moves with it
    /// ([`crate::system_state::page_owes_animation_wake`]), the next animation tick.
    /// A settled, finished or hidden page owes no sub-second tick wake (§5.6): its
    /// re-reads and offer answers ride the worker's publications (each requests a
    /// redraw, which wakes the loop) and a focus, file or network event, and the status
    /// poll runs only while a page shows something moving ([`App::system_state_polls`]).
    /// The `next_tick` the page keeps is still its walk clock; folding it here only while
    /// the walk runs is what holds the idle wake at zero. A true timer wakes once, at its
    /// due time: a refused reading's retry, a shown page's look for a renewed GitHub
    /// token ([`TOKEN_LOOK`]), and, on a shown page, the moment a join's own asking turns
    /// into the card's Allow ([`crate::system_state::JOIN_QUIET`]). `hands_off_keys` (the caller's app-wide typing hold) also
    /// withholds the tick while a key was typed within the 2.5 s gap, so the walk never
    /// wakes the loop over the input path.
    pub(crate) fn deadline(&self, now: Instant, hands_off_keys: bool) -> Option<Instant> {
        let walks = crate::system_state::page_owes_animation_wake(
            self.latest_status.running(),
            self.shown,
            hands_off_keys,
        );
        // The join's own asking turns into the card's Allow once it has asked long enough: one
        // wake, at that moment, shown page or not, so the menu line and the tab say it then too.
        let asking_long = self
            .asking_since
            .filter(|_| !self.asking_long)
            .map(|since| since + crate::system_state::JOIN_QUIET);
        self.next_status
            .filter(|_| !self.status_in_flight)
            .into_iter()
            .chain(self.next_tick.filter(|_| walks))
            .chain(self.presence_look)
            .chain(self.read_timer())
            .chain(asking_long)
            .chain(self.page_due)
            .min()
            .map(|next| next.max(now))
    }

    /// Follow the worker's status for what the window says of it ([`SetupStatus::asking_long`],
    /// [`SetupStatus::ready_in`]): since when the vault's join asks the Macs in ALab, and when
    /// the run going now started and how long the last one took.
    fn track(&mut self, status: &SetupStatus, now: Instant) {
        let asking = status.observed.iter().any(|row| {
            row.subject == aterm_types::macos_setup::Subject::Setting(Setting::Secrets)
                && row.now.as_deref() == Some(aterm_types::macos_setup::VAULT_ASKING)
        });
        let since = asking.then(|| {
            self.asking_since
                .unwrap_or_else(|| self.asked_before(now).unwrap_or(now))
        });
        if since.is_some() != self.asking_since.is_some() {
            self.keep_asking_since(since, now);
        }
        self.asking_since = since;
        let run = status.busy && self.operation.is_none() && !status.queued;
        match self.run_started {
            None if run => {
                self.run_started = Some(now);
                self.ready_in = None;
            }
            Some(started) if !status.busy => {
                self.run_started = None;
                self.ready_in = status
                    .finished
                    .then(|| now.saturating_duration_since(started));
            }
            _ => {}
        }
    }

    /// When an earlier launch saw the join begin asking, from [`Self::asking_record`], as an
    /// instant of this process: `None` with no record, or one an hour old or more (that join is
    /// long over, so this asking is a new one).
    fn asked_before(&self, now: Instant) -> Option<Instant> {
        let path = self.asking_record.as_ref()?;
        let then: u64 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
        let ago = Duration::from_millis(aterm_time::unix_millis()?.checked_sub(then)?);
        (ago < Duration::from_secs(60 * 60)).then(|| now.checked_sub(ago).unwrap_or(now))
    }

    /// Keep (or, once the join stops asking, forget) when the join began asking.
    fn keep_asking_since(&self, since: Option<Instant>, now: Instant) {
        let Some(path) = &self.asking_record else {
            return;
        };
        match since {
            Some(since) => {
                let ago = now.saturating_duration_since(since);
                let Some(then) = aterm_time::unix_millis().map(|ms| {
                    ms.saturating_sub(u64::try_from(ago.as_millis()).unwrap_or(u64::MAX))
                }) else {
                    return;
                };
                // A record lost to a crash only starts the quiet again: no flush is owed.
                if let Err(error) = std::fs::write(path, then.to_string()) {
                    aterm_log::info!("system state: the join's asking start was not kept: {error}");
                }
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Whether the join has asked the Macs in ALab long enough at `now` for the card's Allow.
    fn asking_long_at(&self, now: Instant) -> bool {
        self.asking_since.is_some_and(|since| {
            now.saturating_duration_since(since) >= crate::system_state::JOIN_QUIET
        })
    }

    /// The dev scenario, when the seam names one (debug and dev-seam builds).
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fn fixture(&mut self) -> Option<&SetupStatus> {
        if !self.fixture_read {
            self.fixture_read = true;
            self.fixture = aterm_types::dev_seam!(FIXTURE_SEAM).and_then(|name| {
                let name = name.to_str()?.to_owned();
                let status = crate::system_state::fixture_status(&name);
                if status.is_none() {
                    aterm_log::warn!("{FIXTURE_SEAM}={name} names no System State scenario");
                }
                self.fixture_line = crate::system_state::fixture_line(&name);
                status
            });
        }
        self.fixture.as_ref()
    }

    /// A release build shows no dev scenario.
    #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
    fn fixture(&mut self) -> Option<&SetupStatus> {
        None
    }

    /// The line a new view of the dev scenario opens (`<scenario>@<line>`), so a lab capture
    /// sees its rows; `None` for every other page.
    pub(crate) fn fixture_line(&mut self) -> Option<crate::system_state::lines::Line> {
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        {
            self.fixture()?;
            self.fixture_line
        }
        #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
        {
            None
        }
    }

    /// Whether a dev scenario stands in for the worker (its actions inert).
    fn showing_fixture(&self) -> bool {
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        {
            self.fixture.is_some()
        }
        #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
        {
            false
        }
    }
}

impl App {
    /// The binary's handlers ([`set_initial_setup_handlers`]), or a test's own.
    pub(crate) fn system_state_handlers(&self) -> Option<InitialSetupHandlers> {
        #[cfg(test)]
        if let Some(handlers) = self.system_state_host.handlers {
            return Some(handlers);
        }
        HANDLERS.lock().ok().and_then(|handlers| *handlers)
    }

    /// Hand this App's System State `handlers` in place of the binary's.
    #[cfg(test)]
    pub(crate) fn set_system_state_handlers_for_test(&mut self, handlers: InitialSetupHandlers) {
        self.system_state_host.handlers = Some(handlers);
    }

    /// A verified grant without durable access history still refreshes the rows
    /// and lets the worker finish already-owed preferences. It admits no access step.
    pub(crate) fn notify_initial_setup_fda_granted(&self) {
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.fda_granted)();
        }
    }

    /// The event loop has ended: an offer nobody answered is let go, and setup
    /// resumes at the next launch, unless a Stop was pressed: the quit waits
    /// for what it owes the records ([`InitialSetupHandlers::settle`]).
    pub(crate) fn finish_system_state_shutdown(&mut self) {
        let handlers = self.system_state_handlers();
        self.finish_system_state_shutdown_with(handlers);
    }

    /// The launch's offer still waits for the page's answer at its first tick: its headline is
    /// the page's one signal, so the menu line and tab's diamond stay quiet (§3.3, §3.6).
    pub(crate) fn system_state_launch_offer_waits(&self) -> bool {
        self.system_state_host
            .pending
            .as_ref()
            .is_some_and(|request| matches!(request.kind(), OfferKind::Launch) && request.pending())
    }

    /// Queued setup and Restore still belong to this worker; a live installer also
    /// keeps its prompt guard here until exit or final detachment.
    pub(crate) fn system_state_handoff_pending(&self) -> bool {
        let host = &self.system_state_host;
        host.pending_apply.is_some()
            || host.operation == Some(SetupOperation::Restore)
            || host.latest_status.queued
            || self.developer_tools_install_active()
    }

    /// An update's handoff is about to end this process: what a Stop still
    /// owes the records gets the same bounded wait a quit gives it
    /// ([`InitialSetupHandlers::settle`]); with nothing owed it returns at
    /// once. The handoff asks as it starts, at the Commit's claim and right
    /// before its write, since the window handles input in between.
    #[cfg(target_os = "macos")]
    pub(crate) fn settle_system_state_before_handoff(&self) {
        aterm_types::macos_setup::set_system_state_front(false);
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.settle)();
        }
    }

    /// An update's Commit is about to be written: the setup stages running now are cut off by
    /// no crash ([`InitialSetupHandlers::cut_off`]).
    #[cfg(target_os = "macos")]
    pub(crate) fn note_system_state_cut_off_at_commit(&self) {
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.cut_off)(true);
        }
    }

    /// The Commit [`Self::note_system_state_cut_off_at_commit`] noted was not written, or
    /// returned: the handoff did not go, so the notes are taken back.
    #[cfg(target_os = "macos")]
    pub(crate) fn take_back_system_state_cut_off(&self) {
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.cut_off)(false);
        }
    }

    fn finish_system_state_shutdown_with(&mut self, handlers: Option<InitialSetupHandlers>) {
        // Cmd-Q can keep the models and windows until after settlement, but
        // their old frontness no longer authorizes an on-screen worker step.
        aterm_types::macos_setup::set_system_state_front(false);
        if let Some(request) = self.system_state_host.pending.take() {
            request.finish(WorkstationSetupDecision::Unavailable);
        }
        if let Some(handlers) = handlers {
            (handlers.settle)();
            (handlers.cut_off)(true);
        }
    }

    pub(crate) fn receive_system_state_status(
        &mut self,
        generation: Option<u64>,
        status: SetupStatus,
    ) {
        let Some(generation) = generation else {
            self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
            self.system_state_host.status_in_flight = false;
            self.system_state_host.next_status = Some(Instant::now());
            self.system_state_host.next_tick = None;
            self.request_redraw_all_windows();
            return;
        };
        if generation != self.system_state_host.generation {
            return;
        }
        // An admission that resolved meanwhile makes this read stale; its
        // resolution asks for a fresh one.
        self.resolve_system_state_apply();
        if generation != self.system_state_host.generation {
            return;
        }
        // A read that met a write under way says nothing of the saved setup: the window's own
        // admission keeps the last read that did, and so does the page until records that keep
        // changing show as such. The next read comes once the write is over.
        let changing = status.changing;
        if changing && self.system_state_host.changing_reads < CHANGING_READS {
            // Keep the displayed plan and access approvals, but an in-flight
            // external mutation supplies no current run for a deferred command.
            self.system_state_host.latest_status.run = None;
            self.system_state_host.changing_reads += 1;
            self.system_state_host.status_in_flight = false;
            self.system_state_host.next_status = Some(Instant::now() + CHANGING_REREAD);
            return;
        }
        self.system_state_host.changing_reads = 0;
        // This read was asked for after any accepted plan's admission, so it
        // shows that run (or its end).
        self.system_state_host.awaiting_run = false;
        if !status.busy {
            self.system_state_host.operation = None;
        }
        if !changing {
            self.observe_initial_setup_permissions(status.permissions);
        }
        self.publish_system_state(status);
    }

    pub(crate) fn offer_system_state(&mut self, request: Request) {
        if self.resume_offer_moot(&request) {
            return;
        }
        let quiet =
            self.system_state_host.review_shown || matches!(request.kind(), OfferKind::Resume);
        if !request.pending()
            || self.headless
            || !self.can_offer_system_state(request.kind())
            || !(if quiet {
                self.open_system_state_tab_quietly()
            } else {
                self.open_system_state_tab()
            })
        {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        self.attach_system_state_offer(request);
    }

    fn can_offer_system_state(&self, kind: &OfferKind) -> bool {
        // The host keeps accepted work and admissions after their final view
        // closes. A queued startup offer must yield before opening a new
        // review, just as it does while a busy controller survives (§3.3).
        // The offer of a plan an earlier process admitted yields to the same
        // work, but not to that plan's own run going (§3.3, §6), and to a
        // Stop or a choice the window holds setup for, which no answer of it
        // may take the place of.
        let resume = matches!(kind, OfferKind::Resume);
        self.system_state_host.operation.is_none()
            && (!resume || self.workstation_setup.setup_unheld())
            && !self.system_state_host.awaiting_run
            && (resume || !self.system_state_host.latest_status.busy)
            && self.system_state_host.pending.is_none()
            && self.system_state_host.pending_apply.is_none()
            && self
                .native_runtime
                .instance_by_kind(AppKind::SystemState)
                .is_none_or(|instance| {
                    matches!(self.native_runtime.app(instance), Some(NativeApp::SystemState(state))
                        if if resume { state.can_offer_resume() } else { state.can_offer() })
                })
    }

    /// The offer of a plan an earlier process admitted is moot while this window holds its own
    /// admission of the plan in force (a press's, or an answer to an earlier offer): it is let go
    /// with no receipt ([`Request::let_go`]), and nothing answers it for that admission (§6).
    /// Whether `request` was that offer, now finished.
    fn resume_offer_moot(&self, request: &Request) -> bool {
        let moot = matches!(request.kind(), OfferKind::Resume) && self.own_admission_stands();
        if moot {
            request.let_go();
        }
        moot
    }

    fn attach_system_state_offer(&mut self, request: Request) {
        if self.resume_offer_moot(&request) {
            return;
        }
        if !request.pending() || !self.can_offer_system_state(request.kind()) {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        if !self.native_runtime.offer_system_state(request.clone()) {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        // The review of the groups the ALab signal adds holds none of aterm's own permissions:
        // they are in the plan in force, whose run carries them (design §3.5). Nor does the offer
        // of a plan an earlier process admitted: its run goes on, its access steps with it, and
        // the page reads as it did; nor does its answer (§3.3, §6).
        let launch = matches!(request.kind(), OfferKind::Launch);
        let resume = matches!(request.kind(), OfferKind::Resume);
        self.system_state_host.pending = Some(request);
        if resume {
            self.system_state_host.action_refusal = None;
            self.system_state_host.next_tick = None;
            self.system_state_host.next_status = Some(Instant::now());
            self.request_redraw_all_windows();
            return;
        }
        self.system_state_host.review_shown = true;
        if launch {
            self.hold_initial_setup_permissions();
        }
        self.refresh_system_state_tab_indicators();
        self.system_state_host.action_refusal = None;
        // The page answers the offer on its first tick: tick now.
        self.system_state_host.next_tick = None;
        self.system_state_host.latest_status = SetupStatus::default();
        let views: Vec<_> = self
            .view_store
            .iter()
            .filter_map(|(view, state)| match state {
                crate::tab_model::View::Native(native)
                    if self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == AppKind::SystemState) =>
                {
                    Some(view)
                }
                _ => None,
            })
            .collect();
        for view in views {
            if let Some(state) = self.native_runtime.view_state_mut(view) {
                state.common_mut().last_focus = None;
                state.common_mut().focus_visible = false;
            }
        }
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_all_windows();
    }

    /// Project the worker's status for a controller without changing its
    /// cached reading or poll state. The active step is returned separately
    /// because a fixture can keep its own displayed step.
    pub(crate) fn project_system_state_status(
        &self,
        mut status: SetupStatus,
    ) -> (SetupStatus, Option<Access>) {
        if self.system_state_host.operation.is_some()
            || self.system_state_host.awaiting_run
            || self.system_state_host.pending_apply.is_some()
        {
            status.busy = true;
        }
        // A dev scenario keeps the operation it names (a Restore waiting for another setup), as
        // its actions are inert and the window asks for none.
        let named = if self.system_state_host.showing_fixture() {
            status.operation
        } else {
            None
        };
        status.operation = if status.busy && status.running.is_empty() {
            self.system_state_host.operation.or(named)
        } else {
            None
        };
        if let Some(refusal) = &self.system_state_host.action_refusal {
            status.rows.insert(0, refusal.clone());
        }
        let step = self.system_state_access_step();
        if step.is_some() || !self.system_state_host.showing_fixture() {
            status.access_step = step;
        }
        // The walk's page, the join's asking and the last run's time are the window's to say; a
        // scenario keeps its own.
        let walk = self.system_state_walk_page();
        if walk.is_some() || !self.system_state_host.showing_fixture() {
            status.walk = walk;
        }
        if !self.system_state_host.showing_fixture() {
            status.asking_long = self.system_state_host.asking_long_at(Instant::now());
            status.ready_in = self.system_state_host.ready_in;
        }
        (status, step)
    }

    /// Whether what the window says of the status moved since it was last published: macOS's
    /// dialog up for a grant, the walk's page, or the join's asking running long.
    pub(crate) fn system_state_window_moved(&self, now: Instant) -> bool {
        let host = &self.system_state_host;
        host.access_step != self.system_state_access_step()
            || host.walk_page != self.system_state_walk_page()
            || host.asking_long != host.asking_long_at(now)
    }

    pub(crate) fn publish_system_state(&mut self, mut status: SetupStatus) {
        self.system_state_host.status_shown = true;
        if !self.system_state_host.showing_fixture() {
            self.system_state_host.track(&status, Instant::now());
        }
        // The page and the menu line read where the Command Line Tools install stands, which
        // the window alone knows (its tab, Apple's installer); a scenario keeps its own.
        if !self.system_state_host.showing_fixture() {
            if status.repositories_tools_wait().is_some() {
                status.developer_tools = self.developer_tools_install_step(&status);
            } else {
                // A reading with the tools there ends the calm kept since the tab installed them.
                self.workstation_setup
                    .developer_tools_after
                    .settle(status.repositories_reading(), Instant::now());
            }
        }
        self.system_state_host.latest_status = status.clone();
        let (status, step) = self.project_system_state_status(status);
        // A stage running is a setup pass, whatever the window asked for last:
        // Check again and Restore never run one. A pass that took the worker
        // as a Check ended (or woke by it) is a run, with its Stop and meter.
        if !status.running.is_empty() {
            self.system_state_host.operation = None;
        }
        self.system_state_host.access_step = step;
        self.system_state_host.walk_page = status.walk;
        self.system_state_host.asking_long = status.asking_long;
        self.refresh_system_state_menu_line();
        // Setup that finished, or that a person cancelled, says nothing more by itself while no
        // page shows it: the reader is let go. A worker's own publication, a page opening or a
        // person's press asks for a read again, which starts a reader.
        let quiet = (status.finished || status.cancelled)
            && !status.busy
            && self.system_state_host.pending.is_none()
            && self
                .native_runtime
                .instance_by_kind(AppKind::SystemState)
                .is_none();
        let polls = self.system_state_polls(&status);
        let changed = self.native_runtime.publish_system_state(status);
        self.system_state_host.status_in_flight = false;
        if quiet {
            self.system_state_host.status_reader = None;
        }
        // Only something that moves between the worker's publications is read again a second
        // from now; anything else waits for an event, and owes no wake.
        self.system_state_host.next_status = polls.then(|| Instant::now() + Duration::from_secs(1));
        // A Command Line Tools install a pass asked for opens its tab once it is due.
        self.tick_developer_tools_install();
        // A tab indicator that changed redraws its own window (its tab chrome).
        self.refresh_system_state_tab_indicators();
        if changed {
            self.request_redraw_system_state_windows();
        }
    }

    /// Whether the worker's status is read again by itself, a second after each read
    /// (init-on-by-default design §5.6): only while something moves between the worker's
    /// publications: a run, a Check or a Restore going, an accepted plan whose run has not
    /// shown up yet, or an offer waiting for its answer, which the page, the menu line and the
    /// tab's diamond follow. Anything else reaches them as an event (a worker's publication, a
    /// write this process made, a focus, file or network signal), so neither a settled page nor
    /// an unfinished setup that waits wakes the loop.
    fn system_state_polls(&self, status: &SetupStatus) -> bool {
        let host = &self.system_state_host;
        status.busy
            || host.awaiting_run
            || host.pending_apply.is_some()
            || host.operation.is_some()
            || host.pending.is_some()
    }

    /// The page follows the window's access step at once (design §3.2, §6): when
    /// [`Self::system_state_access_step`] moved since the last publish, publish
    /// the status again so the next frame names macOS's dialog, or stops naming
    /// it. The status poll is left as it was.
    pub(crate) fn republish_system_state_access_step(&mut self) {
        if !self.system_state_window_moved(Instant::now()) {
            return;
        }
        let (in_flight, next) = (
            self.system_state_host.status_in_flight,
            self.system_state_host.next_status,
        );
        self.publish_system_state(self.system_state_host.latest_status.clone());
        self.system_state_host.status_in_flight = in_flight;
        self.system_state_host.next_status = next;
    }

    /// Republish System State when [`Self::developer_tools_install_step`] moved since the last
    /// publish (for example, `Install…` was queued behind a macOS permission gesture or started,
    /// its tab ended, or a press opened nothing), leaving the status poll as it was.
    pub(crate) fn republish_system_state_developer_tools(&mut self) {
        if self.system_state_host.showing_fixture() {
            return;
        }
        let latest = self.system_state_host.latest_status.clone();
        if latest.repositories_tools_wait().is_none()
            || latest.developer_tools == self.developer_tools_install_step(&latest)
        {
            return;
        }
        let (in_flight, next) = (
            self.system_state_host.status_in_flight,
            self.system_state_host.next_status,
        );
        self.publish_system_state(self.system_state_host.latest_status.clone());
        self.system_state_host.status_in_flight = in_flight;
        self.system_state_host.next_status = next;
    }

    /// A Messages press that names System State (`Intent::ReviewVaultSync`, on a vault line an
    /// earlier build filed, or the note that setup waits for its review): open or focus the
    /// page in `wid`, whose rows are the remedy.
    pub(crate) fn review_vault_sync_in_window(&mut self, wid: crate::WindowId) -> bool {
        self.open_system_state_tab_in_window(wid)
    }

    /// The access step the page was last given ([`SetupStatus::access_step`]).
    #[cfg(test)]
    pub(crate) fn published_system_state_access_step(&self) -> Option<Access> {
        self.system_state_host.access_step
    }

    /// Redraw only the windows that show a System State page: its status, its
    /// tick (an offer let go) and a kitty pose baked change only what it draws.
    pub(crate) fn request_redraw_system_state_windows(&self) {
        for wid in self.system_state_showing_windows() {
            if let Some(os_window) = self.windows.get(&wid).and_then(|w| w.os_window.as_ref()) {
                os_window.request_redraw();
            }
        }
    }

    /// The on-screen windows whose active tab's visible plan has a System State
    /// leaf, focused or not. A window where the page is a background tab is not
    /// among them: switching to that tab draws the page as it stands then.
    fn system_state_showing_windows(&self) -> Vec<crate::WindowId> {
        self.system_state_on_screen()
            .filter(|wid| {
                self.active_visible_leaf_plan(*wid).is_some_and(|plan| {
                    plan.leaves.iter().any(|leaf| {
                        matches!(self.view_store.get(leaf.view), Some(crate::tab_model::View::Native(native))
                            if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                    })
                })
            })
            .collect()
    }

    fn refresh_system_state_tab_indicators(&mut self) -> bool {
        // The menu line says what the diamond says, the review's end or start included.
        self.refresh_system_state_menu_line();
        let mut tabs = Vec::new();
        for (wid, window) in &self.windows {
            for tab in window.tab_set.tabs() {
                if tab.root.any_leaf(&mut |view| {
                    matches!(self.view_store.get(*view), Some(crate::tab_model::View::Native(native))
                        if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                }) {
                    tabs.push((*wid, tab.id));
                }
            }
        }
        let mut windows = Vec::new();
        for (wid, tab) in tabs {
            if self.refresh_native_tab_indicators(wid, tab) && !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        let changed = !windows.is_empty();
        self.refresh_tab_chrome_windows(windows);
        changed
    }

    /// A click, key, text or scroll reached the page: note whether it is a
    /// person's or a control client's, before the page's reducer acts on it,
    /// so what it submits or opens carries who asked (design §6).
    pub(crate) fn interact_system_state(&mut self, wid: crate::WindowId) {
        if let Some((_, view)) = self.active_native_view(wid) {
            self.interact_system_state_view(wid, view);
        }
    }

    /// Pointer input can reach a visible split without moving keyboard focus.
    /// Record only a System State leaf that this window currently shows.
    pub(crate) fn interact_system_state_view(
        &mut self,
        wid: crate::WindowId,
        view: crate::tab_model::ViewId,
    ) {
        let ours = self
            .active_visible_leaf_plan(wid)
            .is_some_and(|plan| plan.leaf(view).is_some())
            && matches!(
                self.view_store.get(view),
                Some(crate::tab_model::View::Native(native))
                    if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_)))
            );
        if ours {
            let by_person = !self.person_gesture_refused();
            self.native_runtime.system_state_gesture(by_person);
            // The permission gate's own evidence of whose gesture this is.
            self.note_system_state_touch();
        }
    }

    pub(crate) fn tick_system_state(&mut self, now: Instant) {
        // The setup worker runs its on-screen steps (the sign-outs) only while
        // System State is in front (design §6); its arrival wakes a parked run.
        self.publish_system_state_front();
        // What the window says of the status (the walk's page, the join's asking running long)
        // reaches the page at once, whatever the page's own tick.
        self.republish_system_state_access_step();
        if self.publish_access_opt_outs() {
            // The worker's rows were read with other opt-outs (a `[privacy]`
            // switch, or a debugger turned off that the record read at launch
            // names): a shown page reads them again, so its rows and the menu
            // line agree now.
            self.system_state_host.shown = false;
        }
        self.resolve_withdrawn_system_state_offer(now);
        self.resolve_system_state_apply();
        if self
            .system_state_host
            .next_tick
            .is_some_and(|next| next > now)
            && self
                .system_state_host
                .read_timer()
                .is_none_or(|timer| timer > now)
            && self.system_state_host.page_due.is_none_or(|due| due > now)
        {
            // A real reading timer keeps its own clock, independent of this refresh throttle.
            // A due status alone can be served without refreshing the page.
            self.poll_system_state_status(now);
            return;
        }
        let instance = self
            .native_runtime
            .instance_by_kind(AppKind::SystemState)
            .filter(|instance| self.system_state_instance_has_view(*instance));
        let Some(instance) = instance else {
            // The offer's last view went before the page answered it: it is let go, and the
            // launch's is made again where a page can answer it.
            if let Some(request) = self.system_state_host.pending.take() {
                request.let_go();
            }
            self.system_state_host.next_tick = None;
            self.system_state_host.page_due = None;
            self.system_state_host.shown = false;
            self.system_state_host.observe_retry = None;
            self.poll_system_state_status(now);
            return;
        };
        self.system_state_host.next_tick = Some(now + Duration::from_millis(250));
        // A page opened by hand learns the plan in force from the last status
        // the window read, so it never shows or submits a guess (§3.5).
        if self.system_state_host.latest_status.plan.is_some()
            && matches!(self.native_runtime.app(instance),
                Some(NativeApp::SystemState(state)) if !state.plan_known())
        {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        let shown = self.system_state_shown_views(instance);
        // A plan or access approval changed outside the page reaches it in the status.
        // The same plan can have a new approval whose Not now answers differ, so both
        // changes read the rows again, as when the page is shown.
        let host = &mut self.system_state_host;
        if host.shown
            && ((host.latest_status.plan.is_some()
                && host.observed_plan.is_some()
                && host.observed_plan != host.latest_status.plan)
                || (!host.latest_status.changing
                    && host.latest_status.permissions.is_some_and(|policy| {
                        Some(policy.revision) != host.observed_access_revision
                    })))
        {
            host.shown = false;
        }
        // While the GitHub repositories need a person, a page someone sees looks once a minute
        // whether a renewed token reached this Mac; only that look's worker reads, and only
        // that row, and only once the vault's mark moved (§3.6).
        if host.shown
            && !shown.is_empty()
            && host
                .token_looked_at
                .is_some_and(|at| now.saturating_duration_since(at) >= TOKEN_LOOK)
            && host.latest_status.github_needs_person()
        {
            host.token_looked_at = Some(now);
            self.look_for_github_token(instance);
        }
        if shown.is_empty() {
            self.system_state_host.shown = false;
            self.system_state_host.observe_retry = None;
        } else if !self.system_state_host.shown
            && self
                .system_state_host
                .observe_retry
                .is_none_or(|retry| retry <= now)
        {
            // A page waiting for the plan reads at the first tick that knows it; only a
            // refused start waits a second before it is tried again.
            let observe = self.observe_system_state(instance);
            let host = &mut self.system_state_host;
            host.shown = observe == Observe::Started;
            host.observe_retry =
                (observe == Observe::Refused).then_some(now + Duration::from_secs(1));
            if observe == Observe::Started {
                host.token_looked_at = Some(now);
                host.observed_plan = host.latest_status.plan.clone();
                if !host.latest_status.changing
                    && let Some(policy) = host.latest_status.permissions
                {
                    host.observed_access_revision = Some(policy.revision);
                }
            }
        }
        // A dev scenario is a still picture of one moment: its page starts nothing by itself
        // (its actions are inert, so a run it started would never report).
        let changed =
            !self.system_state_host.showing_fixture() && self.native_runtime.tick_system_state();
        self.system_state_host.page_due = match self.native_runtime.app(instance) {
            Some(NativeApp::SystemState(state)) => state.next_by_itself(),
            _ => None,
        };
        self.drain_system_state_actions();
        // Something moving is read again by itself; an open page that never showed a status
        // asks for its first read.
        if !self.system_state_host.status_in_flight
            && (self.system_state_polls(&self.system_state_host.latest_status)
                || !self.system_state_host.status_shown)
        {
            self.system_state_host.next_status.get_or_insert(now);
        }
        self.poll_system_state_status(now);
        self.refresh_system_state_tab_indicators();
        if changed {
            self.request_redraw_system_state_windows();
        }
    }

    /// Where setup may not start (a headless instance), the page still runs
    /// while the dev seam shows it a scenario, so a lab capture sees it.
    pub(crate) fn tick_system_state_fixture(&mut self, now: Instant) {
        if self.system_state_host.fixture().is_some() {
            self.tick_system_state(now);
        }
    }

    fn resolve_withdrawn_system_state_offer(&mut self, now: Instant) {
        if !self
            .system_state_host
            .pending
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            return;
        }
        // The offer of a plan an earlier process admitted held nothing, so its going releases
        // nothing either.
        if !self
            .system_state_host
            .pending
            .take()
            .is_some_and(|request| matches!(request.kind(), OfferKind::Resume))
        {
            self.recheck_initial_setup_permissions();
        }
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        self.system_state_host.next_status = Some(now);
        self.system_state_host.next_tick = None;
    }

    /// Publish visibility before another event or worker step can use it: half of the setup
    /// worker's `on_screen` port ([`system_state_on_screen`]), which starts the Messages and
    /// FaceTime sign-outs, a person stage, only with a person at the page
    /// ([`Self::system_state_front_with_a_person`]), and how the worker reads the other half,
    /// whether a person is here as it asks. Closing a passive copy keeps the visible page's
    /// authority; losing the last front page withdraws it even while a hidden controller
    /// survives. Every event and every page tick publishes it again; a page with nothing running
    /// ticks only while it is in front with its sign-outs parked for want of a person, each
    /// [`PRESENCE_LOOK`], so the published bit goes false within a second of the minute of no
    /// input running out and true again within a second of the person's return, input anywhere,
    /// which wakes the parked run (`aterm_types::macos_setup::on_system_state_front`). The port
    /// never waits on a tick to go false: the worker reads presence itself, for the stage and
    /// again right before each sign-out's clicks.
    pub(crate) fn publish_system_state_front(&mut self) {
        let frontmost = self.system_state_frontmost();
        let front = frontmost && self.person_at_this_screen();
        *PRESENCE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(self.presence());
        aterm_types::macos_setup::set_system_state_front(front);
        self.system_state_host.presence_look = (frontmost
            && self.system_state_host.sign_outs_wait_for_a_person())
        .then(|| Instant::now() + PRESENCE_LOOK);
    }

    /// Whether a person stage may put its gesture on screen now (init-on-by-default design §6.4):
    /// System State is in front ([`Self::system_state_frontmost`]) and a person is at this Mac's
    /// screen ([`Self::person_at_this_screen`], SAFE-1's fence, moved from the admission onto the
    /// person stages: the Messages and FaceTime sign-outs, the debugger step's macOS dialog and
    /// the Command Line Tools' authorization). The background stages never ask it. The input
    /// and screen readings (an IOKit and a window server read on this thread) are made only
    /// once the page is in front.
    pub(crate) fn system_state_front_with_a_person(&self) -> bool {
        self.system_state_frontmost() && self.person_at_this_screen()
    }

    /// Whether a System State view is the visible, active view of a focused,
    /// on-screen window with no overlay: the only time a setup run's access
    /// steps, the sign-outs and the debugger step may put a macOS gesture on
    /// screen (design §6). A focused window macOS reports occluded (the screen
    /// locked or asleep, a screen saver up) stays key but shows nothing.
    pub(crate) fn system_state_frontmost(&self) -> bool {
        self.windows
            .keys()
            .any(|wid| self.system_state_frontmost_in(*wid))
    }

    /// Revalidate the particular window a deferred setup gesture was booked in.
    pub(crate) fn system_state_frontmost_in(&self, wid: crate::WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|window| {
            crate::os_window_focused(window)
                && !window.occluded
                && window.overlay.is_none()
                && self
                    .active_native_view(wid)
                    .is_some_and(|(instance, view)| {
                        matches!(
                            self.native_runtime.app(instance),
                            Some(crate::native_app::NativeApp::SystemState(_))
                        ) && self
                            .active_visible_leaf_plan(wid)
                            .is_some_and(|plan| plan.leaf(view).is_some())
                    })
        })
    }

    /// Which part of [`Self::system_state_frontmost`] fails, in aterm.log's words: `None` while a
    /// window has System State frontmost. Read in the aterm window that last had keyboard focus:
    /// while System State is not in view there (closed, behind another tab, or only in another
    /// window), or in view but not its active view (a terminal pane of a split active beside it),
    /// that alone is said, whatever the focus, so keys typed and windows or apps switched to there
    /// change nothing said; once it is that window's active view, the part that fails next
    /// (keyboard focus, occlusion, an overlay, the visible layout) is said.
    pub(crate) fn system_state_front_gap(&self) -> Option<&'static str> {
        if self.system_state_frontmost() {
            return None;
        }
        let Some(wid) = self
            .frontmost_window
            .filter(|wid| self.system_state_laid_out_in(*wid))
        else {
            return Some("System State is not in view in the aterm window last focused");
        };
        let Some((_, view)) = self.active_native_view(wid).filter(|(instance, _)| {
            matches!(
                self.native_runtime.app(*instance),
                Some(crate::native_app::NativeApp::SystemState(_))
            )
        }) else {
            return Some("System State is not the active view in the aterm window last focused");
        };
        let Some(window) = self
            .windows
            .get(&wid)
            .filter(|window| crate::os_window_focused(window))
        else {
            return Some("no aterm window has keyboard focus");
        };
        if window.occluded {
            return Some(
                "macOS reads the focused window occluded (covered, minimized, or the screen \
                 locked or asleep)",
            );
        }
        if window.overlay.is_some() {
            return Some("the focused window has the palette or the Overview up");
        }
        if !self
            .active_visible_leaf_plan(wid)
            .is_some_and(|plan| plan.leaf(view).is_some())
        {
            return Some("System State's view is not in the focused window's visible layout");
        }
        Some("System State is not the visible view of a focused window")
    }

    /// Whether a System State view is a leaf of window `wid`'s active tab's visible layout,
    /// active or not, on screen or not.
    fn system_state_laid_out_in(&self, wid: crate::WindowId) -> bool {
        let Some(instance) = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
        else {
            return false;
        };
        self.active_visible_leaf_plan(wid).is_some_and(|plan| {
            plan.leaves.iter().any(|leaf| {
                matches!(self.view_store.get(leaf.view),
                    Some(crate::tab_model::View::Native(native)) if native.instance == instance)
            })
        })
    }

    /// The windows someone can see: a minimized, hidden or fully covered
    /// window (occluded) shows nothing, as Settings' `settings_view_on_screen`
    /// reads it.
    fn system_state_on_screen(&self) -> impl Iterator<Item = crate::WindowId> + '_ {
        self.windows
            .iter()
            .filter(|(_, window)| !window.occluded)
            .map(|(wid, _)| *wid)
    }

    /// The views of `instance` that are the visible, active view of some
    /// on-screen window, focused or not.
    fn system_state_shown_views(
        &self,
        instance: crate::native_app::AppInstanceId,
    ) -> Vec<crate::tab_model::ViewId> {
        self.system_state_on_screen()
            .filter_map(|wid| {
                self.active_native_view(wid).and_then(|(current, view)| {
                    (current == instance
                        && self
                            .active_visible_leaf_plan(wid)
                            .is_some_and(|plan| plan.leaf(view).is_some()))
                    .then_some(view)
                })
            })
            .collect()
    }

    /// A person returning from System Settings may have changed this Mac
    /// while the page stayed visible. Its accepted reading is stale then;
    /// the next tick reads again, keeping a refused start's retry deadline.
    pub(crate) fn system_state_focus_returned(&mut self, wid: crate::WindowId) {
        let visible = self
            .windows
            .get(&wid)
            .is_some_and(|window| !window.occluded && window.overlay.is_none())
            && self
                .active_native_view(wid)
                .is_some_and(|(instance, view)| {
                    matches!(
                        self.native_runtime.app(instance),
                        Some(NativeApp::SystemState(_))
                    ) && self
                        .active_visible_leaf_plan(wid)
                        .is_some_and(|plan| plan.leaf(view).is_some())
                });
        if visible {
            self.system_state_host.shown = false;
            self.system_state_host.next_tick = Some(Instant::now());
        }
    }

    /// Whether a person is at this Mac's screen: a keyboard, mouse or trackpad was used within
    /// [`PERSON_PRESENT_WITHIN`] ([`Self::person_present_within`], read as the permission walk
    /// and System State's access steps read it), and this login's screen is neither locked nor
    /// switched away for another login. A person stage starts only then, so a window that opened
    /// with nobody there (a launch from another Mac, a login with nobody at the screen, an update
    /// relaunched overnight) runs its background stages and puts nothing on screen until a
    /// person comes (SAFE-1's fence, on the person stages: init-on-by-default design §6.4).
    pub(crate) fn person_at_this_screen(&self) -> bool {
        self.person_present_within(PERSON_PRESENT_WITHIN, Instant::now()) && !(self.screen_away)()
    }

    /// Ask the worker for a fresh, prompt-free reading of every row for the
    /// plan in force (§3.5) when the page is shown: its managed rows decide
    /// which network reads run, so a page that does not know the plan yet
    /// waits for it. `false` while it waits.
    fn observe_system_state(&mut self, instance: crate::native_app::AppInstanceId) -> Observe {
        let handlers = self.system_state_handlers();
        self.observe_system_state_with(instance, handlers)
    }

    fn observe_system_state_with(
        &mut self,
        instance: crate::native_app::AppInstanceId,
        handlers: Option<InitialSetupHandlers>,
    ) -> Observe {
        if self.system_state_host.fixture().is_some() {
            return Observe::Started;
        }
        let Some(handlers) = handlers else {
            return Observe::Started;
        };
        let plan = match self.native_runtime.app(instance) {
            Some(NativeApp::SystemState(_)) => match self.system_state_observation_plan(instance) {
                Some(plan) => plan,
                // A newer aterm's setup names no plan this aterm reads, now or later; reading
                // writes nothing, so its rows are read as they stand with nothing set up: no
                // account and no network read but on Check again (§3.2, §3.5).
                None if self.system_state_host.latest_status.newer() => {
                    Plan::from(Selection::none())
                }
                None => return Observe::NoPlan,
            },
            _ => return Observe::Started,
        };
        if (handlers.observe)(plan) {
            Observe::Started
        } else {
            aterm_log::info!("system state: the observation did not start");
            Observe::Refused
        }
    }

    /// The plan the shown page's rows are read for: the page's own, once it knows the plan in
    /// force, else that plan from the last status. `None` while neither knows it.
    fn system_state_observation_plan(
        &self,
        instance: crate::native_app::AppInstanceId,
    ) -> Option<Plan> {
        match self.native_runtime.app(instance) {
            Some(NativeApp::SystemState(state)) if state.plan_known() => {
                Some(state.observation_plan())
            }
            Some(NativeApp::SystemState(_)) => self.system_state_host.latest_status.plan.clone(),
            _ => None,
        }
    }

    /// One look for a renewed GitHub token while the GitHub repositories need a person
    /// ([`TOKEN_LOOK`]): the worker reads that row again, alone, only if the vault's mark moved.
    fn look_for_github_token(&mut self, instance: crate::native_app::AppInstanceId) {
        if self.system_state_host.fixture().is_some() {
            return;
        }
        let Some(handlers) = self.system_state_handlers() else {
            return;
        };
        if let Some(plan) = self.system_state_observation_plan(instance)
            && !(handlers.look_for_token)(plan)
        {
            aterm_log::info!("system state: the look for a renewed GitHub token did not start");
        }
    }

    fn poll_system_state_status(&mut self, now: Instant) {
        if !self.system_state_host.status_in_flight
            && self
                .system_state_host
                .next_status
                .is_some_and(|next| next <= now)
        {
            if let Some(status) = self.system_state_host.fixture().cloned() {
                self.system_state_host.next_status = None;
                self.publish_system_state(status);
                return;
            }
            let Some(handlers) = self.system_state_handlers() else {
                self.system_state_host.next_status = None;
                return;
            };
            self.system_state_host.status_in_flight = true;
            self.system_state_host.next_status = None;
            let generation = self.system_state_host.generation;
            if !self
                .system_state_host
                .send_status_poll(handlers.status, generation)
            {
                self.system_state_host.status_in_flight = false;
                self.system_state_host.next_status = Some(now + Duration::from_secs(1));
            }
        }
    }

    /// Hand off a page's intents before another input can dispose of it. The
    /// same path serves automatic admission from the periodic tick.
    pub(crate) fn drain_system_state_actions(&mut self) {
        while let Some(action) = self.native_runtime.take_system_state_action() {
            self.run_system_state_action(action);
        }
    }

    fn run_system_state_action(&mut self, action: SystemStateAction) {
        if self.system_state_host.fixture().is_some() {
            aterm_log::info!("system state: {action:?} is inert under {FIXTURE_SEAM}");
            return;
        }
        let handlers = self.system_state_handlers();
        self.run_system_state_action_with_handlers(action, handlers);
    }

    /// The final view has gone: an offer it still held is let go; closing never cancels setup.
    pub(crate) fn finish_system_state_close(
        &mut self,
        instance: crate::native_app::AppInstanceId,
        scope: crate::native_app::CloseScope,
    ) {
        if matches!(
            self.native_runtime.app(instance),
            Some(NativeApp::SystemState(_))
        ) {
            self.system_state_host.shown = false;
            self.system_state_host.observe_retry = None;
            self.native_runtime
                .finish_system_state_close(instance, scope);
            // Retire the ended offer before a reopened page can submit a new
            // intent; that intent belongs to its manual handler, not this request.
            self.resolve_withdrawn_system_state_offer(Instant::now());
        }
    }

    fn run_system_state_action_with_handlers(
        &mut self,
        action: SystemStateAction,
        handlers: Option<InitialSetupHandlers>,
    ) {
        match action {
            // Stop changes no plan or status generation. It withdraws future
            // automatic gestures locally even when the durable run already
            // finished, and the worker keeps its ordinary durable Stop.
            SystemStateAction::Stop => {
                let _ = self.take_system_state_press();
                self.stop_initial_setup_permissions();
                if let Some(pending) = &mut self.system_state_host.pending_apply {
                    pending.release_permissions = false;
                }
                if let Some(handlers) = handlers {
                    (handlers.stop)();
                }
                // A stopped run has nothing to go on with: the offer of its plan goes, once the
                // worker's Stop is raised, so the offer's thread wakes to a run already stopped,
                // and the window, stopped, takes no offer of it again.
                if self
                    .system_state_host
                    .pending
                    .as_ref()
                    .is_some_and(|request| matches!(request.kind(), OfferKind::Resume))
                    && let Some(request) = self.system_state_host.pending.take()
                {
                    request.finish(WorkstationSetupDecision::Unavailable);
                }
            }
            SystemStateAction::OpenAccess { access, by_person } => {
                let press = self.take_system_state_press();
                let by_person = by_person && press == crate::workstation_setup::PagePress::Person;
                if !self.open_access_from_system_state(access, by_person) {
                    aterm_log::info!(
                        "system state: {} did not open (by a person: {by_person})",
                        access.name()
                    );
                }
            }
            SystemStateAction::TurnSyncOn { by_person } => {
                let press = self.take_system_state_press();
                if !by_person || press != crate::workstation_setup::PagePress::Person {
                    aterm_log::info!("system state: Turn On Sync refused: not a person's press");
                    return;
                }
                if !handlers.is_some_and(|handlers| (handlers.sync_on)()) {
                    aterm_log::info!("system state: Turn On Sync did not start");
                }
            }
            SystemStateAction::InstallDeveloperTools { by_person } => {
                let press = self.take_system_state_press();
                if !by_person || press != crate::workstation_setup::PagePress::Person {
                    aterm_log::info!("system state: Install… refused: not a person's press");
                    return;
                }
                if !self.press_developer_tools_install() {
                    aterm_log::info!("system state: Install… opened no Command Line Tools tab");
                }
            }
            SystemStateAction::GoToToolsTab => {
                let _ = self.take_system_state_press();
                if !self.go_to_developer_tools_tab() {
                    aterm_log::info!("system state: Go to tab found no Command Line Tools tab");
                }
            }
            SystemStateAction::CopyCommand { command, by_person } => {
                // A control client's gesture has no clipboard authority, as for ⌘C.
                let press = self.take_system_state_press();
                if !by_person
                    || press != crate::workstation_setup::PagePress::Person
                    || self.person_gesture_refused()
                {
                    crate::app_take_over::note_clipboard_refused("copy");
                    return;
                }
                if !crate::control::pbcopy(&command) {
                    aterm_log::info!("system state: Copy Command reached no clipboard");
                }
            }
            SystemStateAction::JoinPhrase(phrase, sent) => {
                // The phrase is its own authority: a member's person pressed Allow for it, so a
                // control client's Join goes as a person's does.
                let _ = self.take_system_state_press();
                if self.system_state_host.phrase_join.is_some() {
                    // An earlier page's try still runs (this page was opened again since): this
                    // one never starts, so its words stay for a Join once that one answered.
                    self.native_runtime.system_state_phrase_finished(sent, None);
                    return;
                }
                let (receipt, answer) = PhraseJoinReceipt::new();
                if handlers.is_some_and(|handlers| (handlers.join_phrase)(phrase, receipt)) {
                    self.system_state_host.phrase_join = Some((sent, answer));
                } else {
                    self.native_runtime.system_state_phrase_finished(sent, None);
                    self.finish_system_state_action(false, "Join");
                }
            }
            SystemStateAction::Apply(mut answer, admission) => {
                let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
                    return;
                };
                let mut plan = plan.clone();
                if admission == Admission::Automatic
                    && plan.selection.contains(Setting::Permissions)
                {
                    plan.selection.set(Setting::Permissions, false);
                    answer.decision = WorkstationSetupDecision::Begin(plan.clone());
                    aterm_log::info!(
                        "system state: automatic setup deferred permission acquisition; continuing with available work"
                    );
                }
                let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState)
                else {
                    return;
                };
                // A fresh explicit delta belongs to the manual page once the worker withdrew
                // its offer. A queued whole-offer answer still belongs to that ended Request.
                if admission != Admission::Automatic && answer.changed.is_some() {
                    self.resolve_withdrawn_system_state_offer(Instant::now());
                }
                // The page's own answer to the offer of a plan an earlier process admitted holds
                // nothing (§3.3, §6): it admits nothing new, so it neither withdraws the window's
                // own admission nor turns a Stop a person's row press may go past into one it may
                // not. It is moot while the window holds its own admission already.
                let resume_answer = admission == Admission::Automatic
                    && self
                        .system_state_host
                        .pending
                        .as_ref()
                        .is_some_and(|request| matches!(request.kind(), OfferKind::Resume));
                if resume_answer
                    && self.own_admission_stands()
                    && let Some(request) = self.system_state_host.pending.take()
                {
                    request.let_go();
                    return;
                }
                // A press answers for itself, never for the offer of a plan an earlier process
                // admitted (§6): that offer goes, and the press is the manual choice it is.
                if admission != Admission::Automatic
                    && self
                        .system_state_host
                        .pending
                        .as_ref()
                        .is_some_and(|request| matches!(request.kind(), OfferKind::Resume))
                    && let Some(request) = self.system_state_host.pending.take()
                {
                    request.finish(WorkstationSetupDecision::Unavailable);
                }
                // A run going now, or a plan still being admitted, stops for
                // this one at its next step (§3.3): the worker does that as it
                // takes the plan, so the window never sends a Stop that could
                // reach this plan's own run.
                self.begin_system_state_action(true, !resume_answer);
                self.note_system_state_apply(admission);
                self.system_state_host.operation = None;
                aterm_log::info!("system state: submitting a plan ({admission:?})");
                let password = answer.password.as_ref().and_then(|_| {
                    match self.native_runtime.app(instance) {
                        Some(NativeApp::SystemState(state)) => {
                            state.pending_password_acceptance(&plan)
                        }
                        _ => None,
                    }
                });
                let (receipt, receiver) = InitialSetupApplyReceipt::new(
                    admission,
                    answer.chosen,
                    answer.changed,
                    password,
                );
                self.system_state_host.pending_apply = Some(PendingApply {
                    instance,
                    admission,
                    plan: plan.clone(),
                    answer: receiver,
                    release_permissions: true,
                });
                let wants_repositories = plan
                    .selection
                    .contains(aterm_types::macos_setup::Setting::Repositories);
                let started = if let Some(request) = self.system_state_host.pending.take() {
                    answer.receipt = Some(receipt);
                    request.finish_answer(answer)
                } else if let Some(handlers) = handlers {
                    (handlers.apply)(plan, answer.vault_typed, answer.password, receipt)
                } else {
                    false
                };
                if started {
                    // A plan that leaves the GitHub repositories out (the Developer group turned
                    // off) wants no Command Line Tools: their shim goes now, as no pass of it
                    // will say.
                    if !wants_repositories {
                        self.withdraw_developer_tools_want();
                    }
                    // Sending a startup answer and spawning a manual worker
                    // both await the same durable admission before consuming
                    // the review.
                    self.publish_system_state(self.system_state_host.latest_status.clone());
                } else {
                    let pending = self.system_state_host.pending_apply.take().unwrap();
                    self.refuse_system_state_apply(pending);
                }
                self.finish_system_state_action(started, "Setup");
            }
            SystemStateAction::Check(plan) => {
                let _ = self.take_system_state_press();
                // A read-only Check cannot revoke its durable permission decision.
                self.begin_system_state_action(false, false);
                let started = handlers.map_or_else(CheckStarted::default, |handlers| {
                    let started = (handlers.check)(plan.clone());
                    // Check again is a check and a forced observation (§3.4).
                    if !(handlers.observe)(plan) {
                        aterm_log::info!("system state: the observation did not start");
                    }
                    started
                });
                // Only its own check is labelled one: a pass that held the
                // worker is a run, with its Stop and meter (§3.2). Its forced
                // reading ran all the same, so Check again did start. A Restore
                // waiting for another setup holds the worker, and stays the
                // Restore it is (§3.4).
                self.system_state_host.operation = if started.check {
                    Some(SetupOperation::Check)
                } else {
                    self.system_state_host
                        .operation
                        .filter(|operation| *operation == SetupOperation::Restore)
                };
                self.finish_system_state_action(started.reading || started.check, "Check again");
            }
            SystemStateAction::Restore => {
                let _ = self.take_system_state_press();
                // A Restore puts back Mac preferences and never withdraws aterm's own
                // permissions (§3.4): the access steps go on as they were.
                self.begin_system_state_action(true, false);
                if let Some(request) = self.system_state_host.pending.take() {
                    request.finish(WorkstationSetupDecision::Unavailable);
                    // The offer's hold goes with it: a fresh read of the records decides.
                    self.recheck_initial_setup_permissions();
                    self.native_runtime.tick_system_state();
                    self.refresh_system_state_tab_indicators();
                }
                let started = handlers.is_some_and(|handlers| (handlers.restore)());
                self.system_state_host.operation = started.then_some(SetupOperation::Restore);
                self.finish_system_state_action(started, "Restore previous settings");
            }
        }
    }

    /// What every Apply, Check and Restore does first: a `mutating` one retires the review and
    /// forgets any pending Apply, and one that `holds_access` (an Apply, whose plan may change
    /// the Permissions group) holds the permission gate until its plan's fresh status; each
    /// clears a refused action's note and starts a new status generation.
    fn begin_system_state_action(&mut self, mutating: bool, holds_access: bool) {
        if mutating {
            if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState) {
                self.retire_native_review(instance);
            }
            self.system_state_host.pending_apply = None;
        }
        if holds_access {
            self.hold_initial_setup_permissions();
        }
        self.system_state_host.action_refusal = None;
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
    }

    fn finish_system_state_action(&mut self, started: bool, action_name: &str) {
        if !started {
            self.system_state_host.action_refusal = Some(SetupStatusRow {
                name: action_name.into(),
                status: "not started".into(),
                detail: "Try again.".into(),
            });
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_all_windows();
    }

    /// A failed automatic offer releases its temporary hold after a fresh
    /// status. A local choice or Stop keeps its hold.
    fn refuse_system_state_apply(&mut self, pending: PendingApply) {
        if pending.admission == Admission::Automatic && pending.release_permissions {
            self.recheck_initial_setup_permissions();
        }
        self.native_runtime.refuse_system_state_apply();
    }

    /// The join with the phrase typed on the Secrets vault row answered
    /// ([`PhraseJoinReceipt`]): the page says what it came to, and the rows are read again, so
    /// the row moves on to Allowed or a member's vault.
    pub(crate) fn resolve_system_state_phrase_join(&mut self) {
        let Some((sent, answer)) = self.system_state_host.phrase_join.as_ref() else {
            return;
        };
        let sent = *sent;
        let outcome = match answer.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => PhraseOutcome::Stopped,
        };
        self.system_state_host.phrase_join = None;
        self.native_runtime
            .system_state_phrase_finished(sent, Some(outcome));
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_system_state_windows();
    }

    pub(crate) fn resolve_system_state_apply(&mut self) {
        // A superseded Join can still be accepted. Consume only its exact
        // password, never its old plan or permission decision.
        if self.native_runtime.forget_accepted_system_state_password() {
            self.request_redraw_system_state_windows();
        }
        let Some(pending) = self.system_state_host.pending_apply.as_ref() else {
            return;
        };
        let accepted = match pending.answer.try_recv() {
            Ok(accepted) => accepted,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => false,
        };
        let pending = self.system_state_host.pending_apply.take().unwrap();
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        if accepted {
            self.system_state_host.awaiting_run = true;
            // The worker holds this plan now. The last read was taken before
            // it and names the old one, which must not undo the choice when
            // it is shown again below.
            self.system_state_host.latest_status.plan = Some(pending.plan.clone());
            if pending.release_permissions {
                self.accepted_initial_setup_apply();
            }
            if self.system_state_instance_has_view(pending.instance) {
                self.native_runtime.accept_system_state_apply(pending.plan);
                self.refresh_system_state_tab_indicators();
            }
        } else {
            self.refuse_system_state_apply(pending);
        }
        // A plan the worker took and then let go says why in the worker's own rows (a step
        // that failed, in the page's words), or says nothing, overtaken by a later choice: the
        // host adds no row of its own beside them, so one press is said once.
        self.finish_system_state_action(true, "Setup");
        if accepted {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
    }

    fn system_state_instance_has_view(&self, instance: crate::native_app::AppInstanceId) -> bool {
        self.native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == AppKind::SystemState)
            && self.view_store.iter().any(|(_, view)| {
                matches!(view, crate::tab_model::View::Native(native) if native.instance == instance)
            })
    }
}

#[cfg(test)]
mod tests;
