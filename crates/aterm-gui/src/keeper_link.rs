// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window-side PTY keeper client (`KeeperLink` worker).

#[cfg(unix)]
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, Weak};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use aterm_core::terminal::Terminal;
#[cfg(unix)]
use aterm_keeper::client::{KeeperClient, KeeperLink, LinkOp, Offered};
#[cfg(unix)]
use aterm_keeper::wire::{Birth, Frame, MarkerRef, MasterHeader};
#[cfg(unix)]
use aterm_update::recovery_ledger::KeeperAtLaunch;

static ON: AtomicBool = AtomicBool::new(false);

/// The update's parent (`0`: none), whose sessions a candidate's adopted sessions
/// were: set once at boot ([`set_predecessor`]), read by every registration
/// ([`registration_tag`]).
#[cfg_attr(not(unix), allow(dead_code))]
static PREDECESSOR: AtomicU32 = AtomicU32::new(0);

/// This launch is an update's candidate whose parent is `pid`.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn set_predecessor(pid: u32) {
    PREDECESSOR.store(pid, Ordering::Relaxed);
}

/// How often the meta pump looks at the registered terminals.
#[cfg(unix)]
pub(crate) const META_POLL: Duration = Duration::from_millis(250);

/// Development seam: milliseconds the keeper link's worker waits idle (for
/// [`aterm_keeper::client::LINK_POLL`]) and the META pump waits between its passes (for
/// [`META_POLL`]). An hour leaves only a queued op or the keeper's own frame to wake the worker:
/// the live lane's check that a recovered window reads its keeper's SYNC answer as it arrives.
#[cfg(unix)]
const QUIET_SEAM: &str = "ATERM_DEBUG_KEEPER_QUIET_MS";

/// [`QUIET_SEAM`]'s wait, when it is set to a number of milliseconds.
#[cfg(unix)]
fn quiet() -> Option<Duration> {
    aterm_types::dev_seam!(QUIET_SEAM)?
        .to_str()?
        .parse()
        .ok()
        .map(Duration::from_millis)
}

/// How long the update worker waits for its PENDING to be written before it
/// grants anyway (the holder scan is the keeper's independent gate, §5.4).
#[cfg(unix)]
const PENDING_WAIT: Duration = Duration::from_secs(1);

/// How long the final exit waits for its BYE to be written.
#[cfg(unix)]
const BYE_WAIT: Duration = Duration::from_millis(500);

/// How long the final exit waits for its RELEASEs to be written ([`release_at_quit`]), well
/// inside the reaper's kill tick for a shell that runs a hang-up handler.
#[cfg(unix)]
const QUIT_RELEASE_WAIT: Duration = Duration::from_millis(250);

/// Whether the keeper link is on in this process.
pub(crate) fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Local registration state, not evidence of a keeper's acknowledged custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "portable Settings projections include Unix registration states"
    )
)]
pub(crate) enum RegistrationPhase {
    Off,
    Armed,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) phase: RegistrationPhase,
    pub(crate) tracked: usize,
}

/// Reads existing local memory.
pub(crate) fn status() -> Status {
    #[cfg(unix)]
    {
        let state = keeper_state();
        Status {
            phase: match state.phase {
                Phase::Off => RegistrationPhase::Off,
                Phase::Armed => RegistrationPhase::Armed,
                Phase::Live(_) => RegistrationPhase::Live,
            },
            tracked: state.tracked.len(),
        }
    }
    #[cfg(not(unix))]
    {
        Status {
            phase: RegistrationPhase::Off,
            tracked: 0,
        }
    }
}

/// The fingerprint whose change sends a meta.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    modes: aterm_core::terminal::TerminalModes,
    kitty: aterm_core::terminal::KittyKeyboardFlags,
    xterm: aterm_core::terminal::XtermKeyboardState,
}

#[cfg(unix)]
impl Fingerprint {
    pub(crate) const fn of(term: &Terminal) -> Self {
        let mut modes = *term.modes();
        modes.synchronized_output = false;
        modes.cursor_visible = true;
        Self {
            modes,
            kitty: term.kitty_keyboard_flags(),
            xterm: *term.xterm_keyboard(),
        }
    }
}

#[cfg(unix)]
enum Phase {
    /// Not taking part ([`part`] said no).
    Off,
    /// An update's candidate before its Commit.
    Armed,
    /// Talking to the keeper.
    Live(Arc<KeeperLink>),
}

#[cfg(unix)]
#[derive(Clone)]
struct Tracked {
    rdev: u64,
    /// Boot offers and handoff adoptions can already belong to a keeper.
    prior_claim: bool,
    term: Weak<Mutex<Terminal>>,
    fabric: Weak<crate::fabric::SessionFabric>,
    /// What the last meta sent said.
    last: Option<(Fingerprint, crate::fabric::KeeperCarry)>,
}

#[cfg(unix)]
struct State {
    phase: Phase,
    /// A candidate's registrations, sent at its Commit.
    buffered: Vec<(u64, LinkOp)>,
    /// The owed ops, in order.
    owed: VecDeque<(Option<u64>, LinkOp)>,
    /// Every registered session, by this process's session id.
    tracked: BTreeMap<u64, Tracked>,
    pump: bool,
}

#[cfg(unix)]
static KEEPER_STATE: Mutex<State> = Mutex::new(State {
    phase: Phase::Off,
    buffered: Vec::new(),
    owed: VecDeque::new(),
    tracked: BTreeMap::new(),
    pump: false,
});

/// Send `op` (session `session_id`'s, for a candidate's buffer) over `st`: behind every
/// op still owed, else straight into the live link's queue, or OWED when the queue is
/// full ([`State::owed`]).
#[cfg(unix)]
fn send_in(st: &mut State, session_id: Option<u64>, op: LinkOp) {
    match &st.phase {
        Phase::Live(link) => {
            let link = Arc::clone(link);
            if !flush_owed(&link, &mut st.owed) {
                st.owed.push_back((session_id, op));
                return;
            }
            match link.offer(op) {
                Ok(()) => {}
                Err(aterm_keeper::client::Refused::Full(op)) => {
                    aterm_log::info!(
                        "keeper: the link's queue is full (a keeper slow to read); what it \
                         could not take is owed and offered again"
                    );
                    st.owed.push_back((session_id, op));
                }
                Err(aterm_keeper::client::Refused::Gone(_)) => {}
            }
        }
        Phase::Armed => {
            if let Some(id) = session_id {
                st.buffered.push((id, op));
            }
        }
        Phase::Off => {}
    }
}

/// Offer `link` every owed op again, oldest first, until its queue refuses one: `true`
/// when none is owed any more (a link whose worker has gone writes nothing, so what it
/// owed goes with it).
#[cfg(unix)]
fn flush_owed(link: &KeeperLink, owed: &mut VecDeque<(Option<u64>, LinkOp)>) -> bool {
    while let Some((session_id, op)) = owed.pop_front() {
        match link.offer(op) {
            Ok(()) => {
                if owed.is_empty() {
                    aterm_log::info!("keeper: every op the link's full queue owed is queued");
                }
            }
            Err(aterm_keeper::client::Refused::Full(op)) => {
                owed.push_front((session_id, op));
                return false;
            }
            Err(aterm_keeper::client::Refused::Gone(_)) => owed.clear(),
        }
    }
    true
}

#[cfg(unix)]
fn keeper_state() -> std::sync::MutexGuard<'static, State> {
    KEEPER_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// PENDING uses the waited room only after earlier lifecycle ops have entered
/// the channel.
#[cfg(unix)]
fn live_link(state: &Mutex<State>) -> Result<Option<Arc<KeeperLink>>, ()> {
    let mut st = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    live_link_in(&mut st)
}

#[cfg(unix)]
fn live_link_in(st: &mut State) -> Result<Option<Arc<KeeperLink>>, ()> {
    let link = match &st.phase {
        Phase::Live(link) => Arc::clone(link),
        Phase::Armed | Phase::Off => return Ok(None),
    };
    if !flush_owed(&link, &mut st.owed) {
        return Err(());
    }
    Ok(Some(link))
}

/// Final exit supersedes external lifecycle work. Already admitted channel
/// entries retain their FIFO order before BYE; no later lifecycle is admitted.
#[cfg(unix)]
fn quit_link_in(st: &mut State) -> Option<Arc<KeeperLink>> {
    let phase = std::mem::replace(&mut st.phase, Phase::Off);
    st.owed.clear();
    st.buffered.clear();
    st.tracked.clear();
    match phase {
        Phase::Live(link) => Some(link),
        Phase::Armed | Phase::Off => None,
    }
}

/// A recovered master's release obligation.
#[derive(Debug)]
pub(crate) struct Claim {
    rdev: u64,
    owner: Option<u32>,
    /// The update's parent's (window, session id) this shell was, when its
    /// registrant adopted it in an update ([`Tag::handed`]).
    handed: Option<(u32, u64)>,
    /// The pair the crash journal image its registrant took over keys it by,
    /// when that image was older than its parent's ([`Tag::keyed`]).
    keyed: Option<(u32, u64)>,
    /// What the crash restore brought back for this master (its rung and losses).
    restore: Option<crate::crash_restore::RestoreReport>,
    /// The last window's quit kept this master past it (`Offered::kept`): no crash left it.
    kept: bool,
    armed: bool,
    #[cfg(unix)]
    recovery_count: Option<std::sync::Arc<aterm_keeper::client::ReadObserver>>,
}

impl Claim {
    /// The obligation for the offered master `rdev`, registered by `owner`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) const fn new(rdev: u64, owner: Option<u32>) -> Self {
        Self {
            rdev,
            owner,
            handed: None,
            keyed: None,
            restore: None,
            kept: false,
            armed: true,
            #[cfg(unix)]
            recovery_count: None,
        }
    }

    /// The obligation for the offered master `rdev`, whose registration said `tag`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn of_tag(rdev: u64, tag: &Tag) -> Self {
        let mut claim = Self::new(rdev, tag.owner);
        claim.handed = tag.handed;
        claim.keyed = tag.keyed;
        claim
    }

    /// The window that registered the master (the tag's `owner`).
    pub(crate) const fn owner(&self) -> Option<u32> {
        self.owner
    }

    /// The update's parent's (window, session id) the shell was ([`Tag::handed`]).
    pub(crate) const fn handed(&self) -> Option<(u32, u64)> {
        self.handed
    }

    /// The pair the crash journal image its registrant took over keys the shell by.
    pub(crate) const fn keyed(&self) -> Option<(u32, u64)> {
        self.keyed
    }

    /// Whether the last window's quit kept this master past it (its BYE named it: the Mac Setup
    /// tab's installer, `Offered::kept`), rather than a crash leaving it.
    pub(crate) const fn kept(&self) -> bool {
        self.kept
    }

    /// This claim as one whose master the last window's quit kept (a test's).
    #[cfg(test)]
    pub(crate) fn keeping(mut self) -> Self {
        self.kept = true;
        self
    }

    /// The offered master's device (a test names a claim by it).
    #[cfg(test)]
    pub(crate) fn rdev(&self) -> u64 {
        self.rdev
    }

    /// What the crash restore brought back for this master, taken.
    pub(crate) const fn take_restore(&mut self) -> Option<crate::crash_restore::RestoreReport> {
        self.restore.take()
    }

    /// This old authority stays in the boot cohort even if the claim is dropped.
    #[cfg(unix)]
    pub(crate) fn recovery_count(
        &self,
    ) -> Option<std::sync::Arc<aterm_keeper::client::ReadObserver>> {
        self.recovery_count.as_ref().map(std::sync::Arc::clone)
    }

    fn discharge(mut self) {
        self.armed = false;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.armed {
            #[cfg(unix)]
            release_unclaimed(self.rdev, self.recovery_count.take());
            #[cfg(not(unix))]
            release_unclaimed(self.rdev);
        }
    }
}

/// The RELEASEs undischarged [`Claim`]s sent, in order (a test's view of the
/// obligation, whether or not a link carries them).
#[cfg(test)]
static UNCLAIMED_RELEASES: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// Whether an undischarged [`Claim`] released `rdev`.
#[cfg(test)]
pub(crate) fn released_unclaimed(rdev: u64) -> bool {
    UNCLAIMED_RELEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&rdev)
}

#[cfg(all(test, unix))]
thread_local! {
    /// A test's link state, standing in for the process's on this thread in what an
    /// undischarged [`Claim`]'s drop releases ([`claims_release_to`]).
    static CLAIMS_RELEASE_TO: std::cell::RefCell<Option<Arc<Mutex<State>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Send what an undischarged [`Claim`] dropped on this thread releases over `st` (a test's link
/// state, over a keeper the test steps) in place of the process's, until it is set to `None`: the
/// drop's own path, from the claim's fields to the op the link writes.
#[cfg(all(test, target_vendor = "apple"))]
fn claims_release_to(st: Option<Arc<Mutex<State>>>) {
    CLAIMS_RELEASE_TO.with(|to| *to.borrow_mut() = st);
}

/// Release `rdev`, an offered master that became no session; when its offer carried a journal of
/// the boot cohort, that journal's `observer` hears once the keeper confirms the RELEASE
/// ([`LinkOp::Retire`]).
#[cfg(unix)]
fn release_unclaimed(rdev: u64, observer: Option<Arc<aterm_keeper::client::ReadObserver>>) {
    #[cfg(test)]
    {
        UNCLAIMED_RELEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(rdev);
        if let Some(st) = CLAIMS_RELEASE_TO.with(|to| to.borrow().clone()) {
            let mut st = st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            release_unclaimed_in(&mut st, rdev, observer);
            return;
        }
    }
    if !on() {
        return;
    }
    release_unclaimed_in(&mut keeper_state(), rdev, observer);
}

#[cfg(not(unix))]
fn release_unclaimed(rdev: u64) {
    #[cfg(test)]
    UNCLAIMED_RELEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(rdev);
    #[cfg(not(test))]
    let _ = rdev;
}

/// [`release_unclaimed`] over `st`. Only a live link carries it: no other could confirm it.
#[cfg(unix)]
fn release_unclaimed_in(
    st: &mut State,
    rdev: u64,
    observer: Option<Arc<aterm_keeper::client::ReadObserver>>,
) {
    if matches!(st.phase, Phase::Live(_)) {
        aterm_log::info!("keeper: an offered master became no session; released");
        let op = match observer {
            Some(observer) => LinkOp::Retire { rdev, observer },
            None => LinkOp::Release { rdev },
        };
        send_in(st, None, op);
    }
}

/// Match recovered shells to the panes they filled by putting every id in one namespace keyed
/// by (writer, id).
pub(crate) fn place_by_owner(
    shells: &mut [crate::spawn::Adopted],
    layout: Option<&mut crate::restore::RestoreManifest>,
    writers: &[Option<u32>],
    handed: &[(u32, crate::crash_journal::Handed)],
) {
    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
    let mut ids = std::collections::BTreeMap::<(Option<u32>, u64), u64>::new();
    let mut next = 0u64;
    let mut key = |writer: Option<u32>, id: u64| -> u64 {
        *ids.entry((writer, id)).or_insert_with(|| {
            next += 1;
            next - 1
        })
    };
    fn tree(node: &mut RestoredSplitTree, map: &mut dyn FnMut(u64) -> u64) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(terminal),
            } => terminal.local_id = terminal.local_id.map(&mut *map),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                tree(first, map);
                tree(second, map);
            }
        }
    }
    fn legacy(node: &mut PaneLayout, map: &mut dyn FnMut(u64) -> u64) {
        match node {
            PaneLayout::Leaf { local_id, .. } => *local_id = local_id.map(&mut *map),
            PaneLayout::Split { first, second, .. } => {
                legacy(first, map);
                legacy(second, map);
            }
        }
    }
    if let Some(layout) = layout {
        for (i, window) in layout.windows.iter_mut().enumerate() {
            let writer = writers.get(i).copied().flatten();
            let mut map = |id| key(writer, id);
            for tab in &mut window.restored_tabs {
                tree(&mut tab.root, &mut map);
            }
            for tab in &mut window.tabs {
                legacy(tab, &mut map);
            }
        }
    }
    let mut unnamed = u64::try_from(ids.len()).unwrap_or(u64::MAX);
    for (writer, pair) in handed {
        if let Some(&leaf) = ids.get(&(Some(*writer), pair.id)) {
            ids.entry((Some(pair.pid), pair.was)).or_insert(leaf);
        }
    }
    for shell in shells {
        let claim = shell.keeper.as_ref();
        let own = claim
            .and_then(Claim::owner)
            .and_then(|owner| ids.get(&(Some(owner), shell.local_id)));
        let was = claim
            .and_then(Claim::handed)
            .and_then(|(owner, id)| ids.get(&(Some(owner), id)));
        let keyed = claim
            .and_then(Claim::keyed)
            .and_then(|(owner, id)| ids.get(&(Some(owner), id)));
        shell.local_id = match own.or(was).or(keyed) {
            Some(&id) => id,
            None => {
                unnamed += 1;
                unnamed - 1
            }
        };
    }
}

/// Fixed boot cohort of old journal authorities the keeper may still offer: it is ready once the
/// keeper confirmed, for every one, that it can no longer offer that journal (each a
/// [`ReadObserver`]): it read the new journal of the session that adopted the master, or that
/// session's COMPANION_RELEASE when it has no journal (`journal_op`) or its journal stopped, or
/// the master's RELEASE when no session kept it (a refused offer, `release_refused_offer`; an
/// adoption never placed, `release_unclaimed`) or the session closed before the keeper's read of
/// its journal frame was confirmed (the link hands the observer on, aterm-keeper
/// link-worker-delivery). No write confirms anything but a RELEASE's to a keeper without SYNC;
/// a COMPANION or COMPANION_RELEASE written to one leaves its observer, and the fence, waiting.
///
/// [`ReadObserver`]: aterm_keeper::client::ReadObserver
#[derive(Debug, Default)]
pub(crate) struct RecoveryCountAdmission {
    #[cfg(unix)]
    retained: Vec<std::sync::Arc<aterm_keeper::client::ReadObserver>>,
    ready: AtomicBool,
}

impl RecoveryCountAdmission {
    #[cfg(unix)]
    pub(crate) const fn from_retained(
        retained: Vec<std::sync::Arc<aterm_keeper::client::ReadObserver>>,
    ) -> Self {
        Self {
            retained,
            ready: AtomicBool::new(false),
        }
    }

    pub(crate) fn ready(&self) -> bool {
        if self.ready.load(Ordering::Acquire) {
            return true;
        }
        #[cfg(unix)]
        let all = self.retained.iter().all(|accepted| accepted.confirmed());
        #[cfg(not(unix))]
        let all = true;
        if all {
            self.ready.store(true, Ordering::Release);
        }
        all
    }
}

/// What the boot hello recovered.
#[derive(Default)]
pub(crate) struct Recovery {
    pub(crate) count_admission: std::sync::Arc<RecoveryCountAdmission>,
    /// The admitted offers, each an adoption for `seamless_adopt`.
    pub(crate) adopted: Vec<crate::spawn::Adopted>,
    /// Offers refused by the admission (each released back to the keeper, which closes its copy).
    pub(crate) refused: Vec<(u64, &'static str)>,
    /// The dead windows the offers came from (their pids, from the tags).
    #[cfg(unix)]
    pub(crate) owners: Vec<u32>,
    /// The shell pids of the adopted sessions a crash left running (what the launch's notice
    /// counts and `End sessions` ends): none of those `carried` counts.
    pub(crate) shells: Vec<i32>,
    /// How many adopted sessions the last window's quit carried past it (its BYE kept them: the
    /// Mac Setup tab's installer, `App::carry_developer_tools_install_at_exit`), which no crash
    /// left and no notice tells of.
    #[cfg(unix)]
    pub(crate) carried: usize,
    /// How many adopted sessions the crash restore brought back exactly.
    pub(crate) restores: crate::crash_restore::RestoreTally,
    /// The recovery census's `keeper=` for this cold launch, as [`boot`] left the link.
    #[cfg(unix)]
    pub(crate) keeper: KeeperAtLaunch,
}

impl Recovery {
    /// The recovery census's `restored=`, `restore_lost=`, `restore_why=`.
    #[cfg(unix)]
    pub(crate) fn ledger(&self) -> Option<aterm_update::recovery_ledger::Restores> {
        if self.shells.is_empty() && self.refused.is_empty() {
            return None;
        }
        let mut restores = self.restores.ledger();
        restores.refused = u32::try_from(self.refused.len()).unwrap_or(u32::MAX);
        Some(restores)
    }
}

/// The recovery census's `keeper=` for an update's successor at its Commit.
#[cfg(unix)]
pub(crate) fn at_commit() -> KeeperAtLaunch {
    if on() {
        KeeperAtLaunch::Armed
    } else {
        KeeperAtLaunch::Off
    }
}

/// The build string a HELLO carries.
#[cfg(unix)]
fn build() -> String {
    format!(
        "{}+{}",
        crate::build_info::version(),
        crate::build_info::build_number()
    )
}

/// Whether this window's HELLOs state `CAP_JOURNAL`: it registers a crash-restore region beside
/// each master and reads the regions it is offered (the keeper's companions, `crash_restore`).
#[cfg(unix)]
const JOURNAL: bool = true;

/// Whether the keeper this window booted with takes companions: set from the boot
/// WELCOME's caps.
#[cfg(unix)]
static KEEPER_JOURNALS: AtomicBool = AtomicBool::new(true);

const NO_JOURNAL_SEAM: &str = "ATERM_DEBUG_NO_CRASH_JOURNAL";

/// Why a session spawned now gets no crash-restore journal (`crash_restore`).
pub(crate) fn journal_refusal() -> Option<&'static str> {
    if !on() {
        return Some("keeper-off");
    }
    if aterm_types::dev_seam!(NO_JOURNAL_SEAM).is_some_and(|v| !v.is_empty() && v != "0") {
        return Some("seam");
    }
    #[cfg(unix)]
    {
        (!KEEPER_JOURNALS.load(Ordering::Relaxed)).then_some("keeper-takes-none")
    }
    #[cfg(not(unix))]
    {
        Some("platform")
    }
}

/// This window's crash marker, as the keeper is told it at HELLO (§5.4): the
/// keeper keeps it only if it finds the marker there with its lock held, and
/// after this window's death reads its absence (the exit path unlinked it)
/// together with `exit(0)` as a quit whose BYE was lost.
#[cfg(unix)]
fn marker() -> Option<MarkerRef> {
    use std::os::unix::ffi::OsStrExt as _;
    let (dir, nanos) = crate::crash_signal::own_marker_place()?;
    Some(MarkerRef {
        nanos: u64::try_from(nanos).ok()?,
        dir: dir.as_os_str().as_bytes().to_vec(),
    })
}

/// Whether a launch is an update's candidate for its keeper.
pub(crate) const fn is_update_candidate(overlap_admitted: bool, adopted_a_handoff: bool) -> bool {
    overlap_admitted || adopted_a_handoff
}

/// Whether a launch may take part in its keeper: every windowed launch, and a
/// headless one only of a build whose keeper is not the installed app's
/// ([`part`]'s App-class rule).
#[cfg_attr(not(unix), allow(dead_code))]
const fn takes_part(headless: bool, installed_keeper: bool) -> bool {
    !(headless && installed_keeper)
}

/// `[keeper] enabled` when the file says nothing: on where a keeper exists.
pub(crate) const DEFAULT_ON: bool = cfg!(target_vendor = "apple");

/// Development seam: the launchd label a headless launch starts its keeper under, as a bundled
/// window starts its own. A live test proves a default config's keeper on it without a window,
/// under a test label nothing else uses.
#[cfg(unix)]
const JOB_LABEL_SEAM: &str = "ATERM_DEBUG_KEEPER_JOB_LABEL";

/// What a launch does about its keeper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
enum Part {
    /// No link: its sessions have no journal and a crash hangs them up.
    None,
    /// A link that registers with whichever keeper answers, and starts none.
    Link,
    /// The link, and this launch starts the keeper if none runs.
    LinkAndStart,
}

/// What a launch's own path and mode say about its keeper ([`part`]'s inputs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
struct Launch {
    /// A launch with no window: a check or a lab instance.
    headless: bool,
    /// Its keeper is the installed app's (`aterm_keeper::job::place_for`).
    installed: bool,
    /// It runs from an app bundle that lasts: an installed copy, never one on a mounted image or
    /// an App Translocation copy, which go away (`aterm_update::first_launch::setup_runs_from_posture`).
    lasting_bundle: bool,
}

impl Launch {
    /// The facts of a launch of executable `exe`.
    #[cfg(unix)]
    fn of(exe: &std::path::Path, headless: bool) -> Self {
        let bundled = aterm_keeper::job::own_bundle(exe).is_some();
        #[cfg(target_os = "macos")]
        let lasting = aterm_update::first_launch::setup_runs_from_posture(
            aterm_update::which_copy::posture_from(exe),
        );
        #[cfg(not(target_os = "macos"))]
        let lasting = true;
        Self {
            headless,
            installed: aterm_keeper::job::place_for(exe).installed,
            lasting_bundle: bundled && lasting,
        }
    }

    /// A headless launch standing in for a lasting bundle's window: the live test's seam, so it
    /// proves [`part`]'s default on a test label without a window.
    #[cfg(unix)]
    const fn as_window(self) -> Self {
        Self {
            headless: false,
            installed: self.installed,
            lasting_bundle: true,
        }
    }
}

/// What this launch does about its keeper. A lasting app bundle's window is the default's home, so
/// a new Mac's first window starts one. A bare build's window and a headless launch are checks and
/// lab instances: they keep to what their config says, so a scratch world's instance leaves no job
/// in the login's launchd domain and no region per session. A window of a copy that goes away (on
/// a mounted disk image, or translocated) starts no keeper either: its job would outlive the path
/// it runs from. A headless launch of the installed app never takes part, whatever the file says,
/// because the installed keeper is the person's.
#[cfg_attr(not(unix), allow(dead_code))]
const fn part(setting: Option<bool>, launch: Launch) -> Part {
    if !takes_part(launch.headless, launch.installed) {
        return Part::None;
    }
    match (setting, launch.headless, launch.lasting_bundle) {
        (Some(false), _, _) => Part::None,
        (None | Some(true), false, true) if DEFAULT_ON => Part::LinkAndStart,
        (Some(true), _, _) => Part::Link,
        (None, _, _) => Part::None,
    }
}

/// `setting` is `[keeper] enabled`, if the file says. `cfg` is the config this window builds a live session's engine with: a recovered session's crash restore replays into an engine built the same way.
pub(crate) fn boot(
    setting: Option<bool>,
    candidate: bool,
    headless: bool,
    cfg: Option<&aterm_core::config::TerminalConfig>,
) -> Recovery {
    #[cfg(unix)]
    {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|path| path.canonicalize().ok())
            .unwrap_or_default();
        let seam_label: Option<String> = aterm_types::dev_seam!(JOB_LABEL_SEAM)
            .and_then(|label| label.to_str().map(str::to_owned));
        let launch = Launch::of(&exe, headless);
        let launch = if seam_label.is_some() {
            launch.as_window()
        } else {
            launch
        };
        let part = part(setting, launch);
        if part == Part::None {
            if setting == Some(true) && headless {
                aterm_log::info!("keeper: a headless launch of the installed app takes no part");
            }
            return Recovery::default();
        }
        let recovery = boot_unix(candidate, cfg);
        if part == Part::LinkAndStart && on() {
            start_keeper(seam_label);
        }
        recovery
    }
    #[cfg(not(unix))]
    {
        let _ = (candidate, headless, cfg);
        if setting == Some(true) {
            aterm_log::info!("keeper: not available on this platform");
        }
        Recovery::default()
    }
}

/// Start this build's keeper off the launch's own path (one or two `launchctl` runs); the link
/// registers every terminal once it answers, and a launch whose keeper cannot run works as one
/// with none, with this log line and `aterm keeper status` to say why.
#[cfg(unix)]
fn start_keeper(label: Option<String>) {
    let started = std::thread::Builder::new()
        .name("keeper-start".to_string())
        .spawn(move || {
            match aterm_keeper::cli::ensure_for_window(label, crate::build_info::version()) {
                Ok(line) => aterm_log::info!("keeper: {line}"),
                Err(why) => aterm_log::warn!("keeper: not started: {why}"),
            }
        });
    if let Err(e) = started {
        aterm_log::warn!("keeper: the thread that starts it did not start: {e}");
    }
}

#[cfg(unix)]
fn place() -> Option<(std::path::PathBuf, aterm_keeper::identity::Identity)> {
    let socket = aterm_keeper::job::socket_path(&aterm_keeper::job::own_place())?;
    match aterm_keeper::identity::Identity::for_self(
        aterm_keeper::identity::IdentityPolicy::Designated,
    ) {
        Ok(identity) => Some((socket, identity)),
        Err(why) => {
            aterm_log::warn!("keeper: this build has no code identity to check a keeper by: {why}");
            None
        }
    }
}

#[cfg(unix)]
fn boot_unix(candidate: bool, cfg: Option<&aterm_core::config::TerminalConfig>) -> Recovery {
    let Some((socket, identity)) = place() else {
        return Recovery::default();
    };
    ON.store(true, Ordering::Relaxed);
    if candidate {
        keeper_state().phase = Phase::Armed;
        aterm_log::info!("keeper: armed; this update candidate registers at its Commit");
        return Recovery {
            keeper: KeeperAtLaunch::Armed,
            ..Recovery::default()
        };
    }
    crate::descriptor_limit::raise_descriptor_limit_for_recovery();
    let mut recovery = Recovery::default();
    let mut retained_counts = Vec::new();
    let booted = match KeeperClient::connect(&socket, &identity, Duration::from_secs(2)) {
        Ok(client) => match client.hello_app_marked(&build(), marker(), JOURNAL) {
            Ok(offers) => {
                crate::metrics::mark_keeper_hello();
                let seam_at_hello = crate::metrics::keeper_seam();
                KEEPER_JOURNALS.store(client.keeper_journals(), Ordering::Relaxed);
                let offered = offers.len();
                let (cohort, slowest) = admit_offers(&client, offers, &mut recovery, cfg);
                crate::metrics::record_keeper_admitted(
                    offered,
                    slowest,
                    crate::metrics::keeper_seam().since(seam_at_hello),
                );
                crate::metrics::record_keeper_cohort(cohort.clone());
                retained_counts = cohort;
                Some(client)
            }
            Err(e) => {
                aterm_log::warn!("keeper: the boot HELLO failed: {e}");
                None
            }
        },
        Err(e) if aterm_keeper::client::refused_identity(&e) => {
            aterm_log::warn!(
                "keeper: the keeper at {} is not this build's; nothing is registered with it",
                socket.display()
            );
            None
        }
        Err(e) => {
            aterm_log::info!(
                "keeper=absent at {} ({e}); terminals are registered once one answers",
                socket.display()
            );
            None
        }
    };
    recovery.count_admission = Arc::new(RecoveryCountAdmission::from_retained(retained_counts));
    recovery.keeper = if booted.is_some() {
        KeeperAtLaunch::On
    } else {
        KeeperAtLaunch::Absent
    };
    crate::crash_restore::continue_carried_turns(&mut recovery.adopted);
    if recovery.carried > 0 {
        aterm_log::info!(
            "keeper: {} session(s) the last quit carried past it are reattached (a Mac Setup \
             install); no crash left them",
            recovery.carried
        );
    }
    if recovery.adopted.len() > recovery.carried {
        aterm_log::info!(
            "keeper: {} session(s) the last window's end left running are reattached (from pid(s) \
             {:?}); screens restored exactly {}, with a loss counted {}, blank {}",
            recovery.adopted.len() - recovery.carried,
            recovery.owners,
            recovery.restores.counts.exact,
            recovery.restores.counts.screen,
            recovery.restores.counts.meta
        );
    }
    let idle = quiet().unwrap_or(aterm_keeper::client::LINK_POLL);
    match KeeperLink::start_idling(socket, identity, build(), marker(), booted, JOURNAL, idle) {
        Ok(link) => {
            keeper_state().phase = Phase::Live(Arc::new(link));
            start_meta_pump();
        }
        Err(e) => {
            aterm_log::warn!("keeper: the link worker did not start: {e}");
            ON.store(false, Ordering::Relaxed);
            recovery.keeper = KeeperAtLaunch::Off;
        }
    }
    recovery
}

/// Admit the boot HELLO's `offers`, made on `client`, into `recovery`, releasing each refused one
/// on `client` ([`release_refused_offer`]): the fence's boot cohort (one read observer per offer
/// that carried a journal), and the slowest admission, its restore seam's own work off the clock.
#[cfg(unix)]
fn admit_offers(
    client: &KeeperClient,
    offers: Vec<Offered>,
    recovery: &mut Recovery,
    cfg: Option<&aterm_core::config::TerminalConfig>,
) -> (Vec<Arc<aterm_keeper::client::ReadObserver>>, Duration) {
    let mut cohort = Vec::new();
    let mut used_cells = 0u64;
    let mut slowest = Duration::ZERO;
    for offer in offers {
        let recovery_count = offer.companion.as_ref().map(|_| {
            let accepted = Arc::new(aterm_keeper::client::ReadObserver::default());
            cohort.push(Arc::clone(&accepted));
            accepted
        });
        let rdev = offer.header.rdev;
        let local_id = offer.header.local_id;
        let kept = offer.kept;
        if let Some(owner) = Tag::decode(&offer.tag).and_then(|t| t.owner)
            && !recovery.owners.contains(&owner)
        {
            recovery.owners.push(owner);
        }
        // The restore lane's seam's own work is off this clock: a really damaged region never
        // pays it (`metrics::keeper_seam`).
        let seam_before = crate::metrics::keeper_seam();
        let started = std::time::Instant::now();
        let admitted = admit_recovered_master(offer, &mut used_cells, cfg);
        let seam = crate::metrics::keeper_seam().since(seam_before);
        slowest = slowest.max(started.elapsed().saturating_sub(seam.spent()));
        match admitted {
            Ok(mut adopted) => {
                if let Some(claim) = adopted.keeper.as_mut() {
                    claim.recovery_count = recovery_count;
                    claim.kept = kept;
                }
                if kept {
                    // The last window's quit carried it: no crash's recovery, so the launch tells
                    // of none and its End recovered sessions never reaches it.
                    recovery.carried += 1;
                } else {
                    if let Some(report) = adopted.keeper.as_ref().and_then(|c| c.restore.as_ref()) {
                        recovery.restores.note(report);
                    }
                    recovery.shells.push(adopted.pid);
                }
                recovery.adopted.push(adopted);
            }
            Err(why) => {
                aterm_log::warn!(
                    "keeper: refused the offer of session {local_id}: {why}; released"
                );
                release_refused_offer(client, rdev, recovery_count);
                recovery.refused.push((local_id, why));
            }
        }
    }
    (cohort, slowest)
}

/// RELEASE `rdev`, an offer the window refused, on `client`, the boot connection it came on. When
/// the offer carried a journal of the boot cohort, `observer` hears once the keeper confirms the
/// RELEASE ([`KeeperClient::retire`]): the link started on `client` sees that through, and tells
/// it to the next connection if this one ends first.
#[cfg(unix)]
fn release_refused_offer(
    client: &KeeperClient,
    rdev: u64,
    observer: Option<Arc<aterm_keeper::client::ReadObserver>>,
) {
    let retiring = observer.is_some();
    let released = match observer {
        Some(observer) => client.retire(rdev, observer),
        None => client.send(&Frame::Release { rdev }, None),
    };
    if let Err(e) = released {
        if retiring {
            aterm_log::warn!(
                "keeper: the RELEASE of a refused offer was not written ({e}); the link tells it \
                 to its next connection"
            );
        } else {
            aterm_log::warn!("keeper: the RELEASE of a refused offer was not written ({e})");
        }
    }
}

/// Commit (an update's successor, beside `crash_signal::adopt_app_identity`).
#[cfg(unix)]
pub(crate) fn activate() {
    if !on() {
        return;
    }
    let mut st = keeper_state();
    if !matches!(st.phase, Phase::Armed) {
        return;
    }
    let Some((socket, identity)) = place() else {
        return;
    };
    let idle = quiet().unwrap_or(aterm_keeper::client::LINK_POLL);
    match KeeperLink::start_idling(socket, identity, build(), marker(), None, JOURNAL, idle) {
        Ok(link) => {
            st.phase = Phase::Live(Arc::new(link));
            for (id, op) in std::mem::take(&mut st.buffered) {
                send_in(&mut st, Some(id), op);
            }
            drop(st);
            start_meta_pump();
            aterm_log::info!("keeper: this successor's sessions are registered at its Commit");
        }
        Err(e) => aterm_log::warn!("keeper: the link worker did not start: {e}"),
    }
}

/// The opaque tag a registration carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tag {
    pub(crate) sid: Option<String>,
    /// The window that registered it.
    pub(crate) owner: Option<u32>,
    pub(crate) rekey: bool,
    pub(crate) loader: bool,
    pub(crate) frozen: bool,
    /// The parent's name for it (additive `handed=<pid>:<id>`, ignored by older builds).
    pub(crate) handed: Option<(u32, u64)>,
    /// The image's name for it (additive `keyed=<pid>:<id>`, ignored by older builds).
    pub(crate) keyed: Option<(u32, u64)>,
    /// The session log's epoch.
    pub(crate) slog_ep: Option<u32>,
}

#[cfg(any(unix, test))]
impl Tag {
    /// `key=value` lines, version first.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = String::from("v=1\n");
        let mut put = |k: &str, v: &str| {
            if !v.contains('\n') {
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
        };
        if let Some(sid) = &self.sid {
            put("sid", sid);
        }
        if let Some(owner) = self.owner {
            put("owner", &owner.to_string());
        }
        if let Some((owner, id)) = self.handed {
            put("handed", &format!("{owner}:{id}"));
        }
        if let Some((owner, id)) = self.keyed {
            put("keyed", &format!("{owner}:{id}"));
        }
        if let Some(ep) = self.slog_ep {
            put("slog_ep", &ep.to_string());
        }
        for (k, on) in [
            ("rekey", self.rekey),
            ("loader", self.loader),
            ("frozen", self.frozen),
        ] {
            if on {
                put(k, "1");
            }
        }
        out.into_bytes()
    }

    /// The inverse of [`Self::encode`].
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        if lines.next()? != "v=1" {
            return None;
        }
        let mut tag = Tag::default();
        for line in lines {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k {
                "sid" if valid_sid(v) => tag.sid = Some(v.to_string()),
                "owner" if v.bytes().all(|b| b.is_ascii_digit()) => tag.owner = v.parse().ok(),
                "rekey" => tag.rekey = v == "1",
                "loader" => tag.loader = v == "1",
                "frozen" => tag.frozen = v == "1",
                "handed" => {
                    tag.handed = v
                        .split_once(':')
                        .filter(|(o, id)| {
                            o.bytes().all(|b| b.is_ascii_digit())
                                && id.bytes().all(|b| b.is_ascii_digit())
                        })
                        .and_then(|(owner, id)| Some((owner.parse().ok()?, id.parse().ok()?)));
                }
                "keyed" => tag.keyed = crate::session_store::parse_journal_key(v),
                "slog_ep" if v.bytes().all(|b| b.is_ascii_digit()) => {
                    tag.slog_ep = v.parse().ok().filter(|ep| *ep >= 1);
                }
                _ => {}
            }
        }
        Some(tag)
    }
}

/// `s-<hex>`, at most 64 characters.
pub(crate) fn valid_sid(s: &str) -> bool {
    s.len() <= 64
        && s.strip_prefix("s-")
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A process's start time in the keeper wire's shape, from the pty crate's
/// microseconds.
#[cfg(unix)]
fn birth_of(micros: u64) -> Birth {
    Birth {
        seconds: micros / 1_000_000,
        micros: u32::try_from(micros % 1_000_000).unwrap_or(0),
    }
}

/// The one admission of a keeper offer ( item 5, step 7).
#[cfg(unix)]
pub(crate) fn admit_recovered_master(
    offer: Offered,
    used_cells: &mut u64,
    cfg: Option<&aterm_core::config::TerminalConfig>,
) -> Result<crate::spawn::Adopted, &'static str> {
    use std::os::fd::IntoRawFd as _;
    let Offered {
        master,
        header,
        tag,
        meta,
        companion,
        attempts,
        kept: _,
    } = offer;
    let master = crate::spawn::HandedMaster::new(master.into_raw_fd());
    admit_recovered_journaled(
        master,
        header,
        &tag,
        &meta,
        Journaled {
            companion,
            attempts,
            cfg,
        },
        used_cells,
    )
}

/// The crash-restore half of an offer: the region the keeper offered with the master,
/// its poison brake, and the config a restore replays under.
#[cfg(unix)]
struct Journaled<'a> {
    companion: Option<aterm_keeper::client::OfferedCompanion>,
    attempts: u8,
    cfg: Option<&'a aterm_core::config::TerminalConfig>,
}

/// [`admit_recovered_journaled`] with no region: the META rung, as before the journal (the
/// admission's tests).
#[cfg(all(unix, test))]
fn admit_recovered(
    master: crate::spawn::HandedMaster,
    header: MasterHeader,
    tag: &[u8],
    meta: &[u8],
    used_cells: &mut u64,
) -> Result<crate::spawn::Adopted, &'static str> {
    admit_recovered_journaled(
        master,
        header,
        tag,
        meta,
        Journaled {
            companion: None,
            attempts: 0,
            cfg: None,
        },
        used_cells,
    )
}

#[cfg(unix)]
fn admit_recovered_journaled(
    master: crate::spawn::HandedMaster,
    header: MasterHeader,
    tag: &[u8],
    meta: &[u8],
    journaled: Journaled<'_>,
    used_cells: &mut u64,
) -> Result<crate::spawn::Adopted, &'static str> {
    let fd = master.raw();
    crate::seamless::admit_master_descriptor(fd)?;
    let pid = i32::try_from(header.shell_pid).map_err(|_| "names no shell")?;
    let shell = aterm_pty::record_adopted_shell(pid, fd);
    match shell.birth.map(birth_of) {
        None => return Err("its shell is gone or no longer leads its terminal"),
        Some(birth) if birth != header.shell_birth => {
            return Err("its shell's pid now names another process");
        }
        Some(_) => {}
    }
    let tag = Tag::decode(tag).ok_or("carries a tag this build does not read")?;
    let sid = tag.sid.clone().ok_or("carries no session id")?;
    let fabric = crate::seamless::recovered_fabric(meta);
    if fabric.topics_lost > 0 || fabric.queued_lost > 0 {
        aterm_log::warn!(
            "keeper: session {sid}'s last META had no room to name {} broadcast opt-in(s) and {} \
             queued post(s); the opt-ins are not restored (`topic add` them again), and whether \
             those posts were sent is not known",
            fabric.topics_lost,
            fabric.queued_lost
        );
    }
    let Journaled {
        companion,
        attempts,
        cfg,
    } = journaled;
    // SAFETY: `companion` came with the boot HELLO's OFFER, which a keeper makes only beside an orphaned master, after every window that claimed it (the region's only writers) has died; the keeper holds it unmapped and read-only, so nothing modifies or resizes the object while the restore maps it.
    let restored =
        unsafe { crate::crash_restore::restore_offered(companion, attempts, cfg, used_cells) };
    if let Some(carry) = &restored.control {
        aterm_log::info!(
            "keeper: session {sid}'s turn ledger: {} record(s) carried",
            carry.turns.len()
        );
    }
    let control = crate::crash_restore::counted_carry(restored.control, restored.turn_ids);
    let (checkpoint, repaint, report, history) = match restored.screen {
        Ok(restore) => (
            Some(restore.checkpoint),
            restore.repaint,
            restore.report,
            restore.history,
        ),
        Err(report) => {
            let history = crate::crash_restore::meta_history(&report);
            (
                crate::seamless::recovered_checkpoint(meta, used_cells),
                true,
                report,
                history,
            )
        }
    };
    if report.rung != crate::crash_restore::Rung::Meta || report.why != "no-region" {
        aterm_log::info!("keeper: session {sid}'s screen: {}", report.status_words());
    }
    let mut claim = Claim::of_tag(header.rdev, &tag);
    claim.restore = Some(report);
    Ok(crate::spawn::Adopted {
        keeper: Some(claim),
        local_id: header.local_id,
        master,
        pid,
        sid: aterm_session::SessionId::new(sid),
        nonce: aterm_session::LaunchNonce::generate(),
        checkpoint,
        control,
        repaint,
        frozen_path: tag.frozen,
        topics: fabric.topics,
        fg_holder: 0,
        rekey: tag.rekey,
        loader: tag.loader,
        history,
        holds: fabric.holds,
        title: String::new(),
        supervisor: None,
        attention_owners: Vec::new(),
        claim_grace: false,
        outgoing_build: None,
        render_debt: None,
        fabric: fabric.carry,
        timeline_id: 0,
        log: crate::session_log::LogAdopt {
            keeper_ep: tag.slog_ep,
            mirror: None,
            ..Default::default()
        },
        journal_key: None,
        task: None,
        autopilot_end: None,
        wakes: None,
        agent_act: None,
    })
}

/// Register (the end of `spawn_session`, for a fresh or an adopted master).
pub(crate) fn register(session: &crate::Session, claim: Option<Claim>) {
    if !on() {
        // No link: nothing was offered, and nothing can be released.
        if let Some(claim) = claim {
            claim.discharge();
        }
        return;
    }
    #[cfg(unix)]
    let tracked = register_unix(
        session,
        claim.is_some() || session.handoff_local_id.is_some(),
        claim.as_ref().and_then(Claim::recovery_count),
    );
    #[cfg(not(unix))]
    let tracked = {
        let _ = session;
        false
    };
    if tracked && let Some(claim) = claim {
        claim.discharge();
    }
}

/// Whether the session is now tracked (and so released at its drop). `retires` is the read
/// observer of the old journal the adopted master's offer carried, when it is in the boot cohort.
#[cfg(unix)]
fn register_unix(
    session: &crate::Session,
    prior_claim: bool,
    retires: Option<Arc<aterm_keeper::client::ReadObserver>>,
) -> bool {
    use std::os::fd::{BorrowedFd, OwnedFd};
    use std::os::unix::fs::MetadataExt as _;
    if session.master < 3 || session.pid <= 1 {
        return false;
    }
    let Some(shell_birth) = session.shell_identity.birth else {
        return false;
    };
    // SAFETY: `session.master` is this live session's open master (its sink owns it for as long as the session exists, and the session is borrowed here); the borrow ends with this statement, which only duplicates it (`F_DUPFD_CLOEXEC`).
    let dup = unsafe { BorrowedFd::borrow_raw(session.master) }.try_clone_to_owned();
    let Ok(dup) = dup else {
        return false;
    };
    let file = std::fs::File::from(dup);
    let Ok(rdev) = file.metadata().map(|m| m.rdev()) else {
        return false;
    };
    let master = OwnedFd::from(file);
    let Ok(shell_pid) = u32::try_from(session.pid) else {
        return false;
    };
    let local_id = session.id;
    let predecessor = Some(PREDECESSOR.load(Ordering::Relaxed)).filter(|&pid| pid != 0);
    let tag = registration_tag(session, std::process::id(), predecessor);
    let op = LinkOp::Register {
        master,
        header: MasterHeader {
            rdev,
            shell_pid,
            shell_birth: birth_of(shell_birth),
            local_id,
        },
        tag: tag.encode(),
    };
    let mut st = keeper_state();
    let custody = session.ctx.crash_journal.get().and_then(|j| {
        j.note_custody_registered();
        j.take_custody_with_receipt()
    });
    let journal = journal_op(rdev, custody, retires);
    register_in(
        &mut st,
        session.id,
        Tracked {
            rdev,
            prior_claim,
            term: Arc::downgrade(&session.term),
            fabric: Arc::downgrade(&session.ctx.fabric),
            last: None,
        },
        op,
        journal,
    );
    true
}

/// The op that settles a registration's journal claim on `rdev`: its region's `custody`, with the
/// receipt the journal made (which reports to the boot cohort's observer when the journal was
/// bound to one, `SessionJournal::bind_recovery_count`), or, with none (no journal: its creation
/// failed or was refused, or it is broken), COMPANION_RELEASE. That one tells the keeper to drop
/// the region an adopted master's offer carried, so it reports the keeper's read to that old
/// journal's observer, `retires`: folding the count locally would leave the keeper free to offer
/// the old journal again.
#[cfg(unix)]
fn journal_op(
    rdev: u64,
    custody: Option<crate::crash_restore::JournalCustody>,
    retires: Option<Arc<aterm_keeper::client::ReadObserver>>,
) -> LinkOp {
    match custody {
        Some(held) => LinkOp::Companion {
            rdev,
            region: held.region,
            len: held.len,
            receipt: held.receipt,
        },
        None => LinkOp::CompanionRelease {
            rdev,
            receipt: retires.map(|observer| {
                Arc::new(aterm_keeper::client::CompanionReceipt::reporting_confirmation(observer))
            }),
        },
    }
}

/// [`register`] over `st` (the process's link state, or a test's): track session
/// `session_id`, then send its REGISTER and, right behind it, the op that settles its claim
/// (its first COMPANION, or COMPANION_RELEASE).
#[cfg(unix)]
fn register_in(
    st: &mut State,
    session_id: u64,
    tracked: Tracked,
    register: LinkOp,
    journal: LinkOp,
) {
    st.tracked.insert(session_id, tracked);
    send_in(st, Some(session_id), register);
    send_in(st, Some(session_id), journal);
}

/// COMPANION_RELEASE (`crash_restore::SessionJournal::stop`).
#[cfg(unix)]
pub(crate) fn release_companion(session_id: u64) {
    if !on() {
        return;
    }
    release_companion_in(&mut keeper_state(), session_id);
}

/// [`release_companion`] over `st`: owed on a full queue, never dropped before a BYE
/// ([`send_in`]),
/// or the keeper would keep the region the journal stopped writing and restore it after
/// a crash as if it were the session's last state.
#[cfg(unix)]
fn release_companion_in(st: &mut State, session_id: u64) {
    let Some(rdev) = st.tracked.get(&session_id).map(|t| t.rdev) else {
        return;
    };
    send_in(
        st,
        Some(session_id),
        LinkOp::CompanionRelease {
            rdev,
            receipt: None,
        },
    );
}

/// What became of a fresh region's custody offered to the keeper link ([`replace_companion`]).
#[cfg(unix)]
#[derive(Debug)]
pub(crate) enum CompanionOffer {
    /// Queued for the keeper (or, for an update's candidate, buffered until its Commit).
    Handed,
    /// The link's queue is full.
    Busy(std::os::fd::OwnedFd),
    /// No link carries the session.
    Unlinked(std::os::fd::OwnedFd),
}

/// Companion again (`crash_restore::SessionJournal::replace_region`, the memory shed).
#[cfg(unix)]
pub(crate) fn replace_companion(
    session_id: u64,
    region: std::os::fd::OwnedFd,
    len: u64,
) -> CompanionOffer {
    if !on() {
        return CompanionOffer::Unlinked(region);
    }
    replace_companion_in(&mut keeper_state(), session_id, region, len, None)
}

/// Replacement with explicit keeper-read evidence.
#[cfg(unix)]
pub(crate) fn replace_companion_checked(
    session_id: u64,
    region: std::os::fd::OwnedFd,
    len: u64,
    receipt: Arc<aterm_keeper::client::CompanionReceipt>,
) -> CompanionOffer {
    if !on() {
        return CompanionOffer::Unlinked(region);
    }
    replace_companion_in(&mut keeper_state(), session_id, region, len, Some(receipt))
}

/// [`replace_companion`] over `st` (the process's link state, or a test's).
#[cfg(unix)]
fn replace_companion_in(
    st: &mut State,
    session_id: u64,
    region: std::os::fd::OwnedFd,
    len: u64,
    receipt: Option<Arc<aterm_keeper::client::CompanionReceipt>>,
) -> CompanionOffer {
    let Some(rdev) = st.tracked.get(&session_id).map(|t| t.rdev) else {
        return CompanionOffer::Unlinked(region);
    };
    let op = LinkOp::Companion {
        rdev,
        region,
        len,
        receipt,
    };
    let (busy, op) = match &st.phase {
        Phase::Live(link) if !flush_owed(&Arc::clone(link), &mut st.owed) => (true, op),
        Phase::Live(link) => match link.offer(op) {
            Ok(()) => return CompanionOffer::Handed,
            Err(aterm_keeper::client::Refused::Full(op)) => (true, op),
            Err(aterm_keeper::client::Refused::Gone(op)) => (false, op),
        },
        Phase::Armed => {
            st.buffered.push((session_id, op));
            return CompanionOffer::Handed;
        }
        Phase::Off => (false, op),
    };
    let LinkOp::Companion { region, .. } = op else {
        unreachable!("the link hands back the COMPANION it was offered")
    };
    if busy {
        CompanionOffer::Busy(region)
    } else {
        CompanionOffer::Unlinked(region)
    }
}

/// A keeper link whose keeper does not read (a test's).
#[cfg(all(test, unix))]
pub(crate) struct TestLink {
    state: Mutex<State>,
    far: aterm_keeper::client::Stalled,
}

/// The rdev of the ops [`TestLink::fill`] queues.
#[cfg(all(test, unix))]
pub(crate) const FILLER_RDEV: u64 = u64::MAX;

#[cfg(all(test, unix))]
impl TestLink {
    pub(crate) fn stalled(session_id: u64, rdev: u64) -> Self {
        let link = Self::bare();
        link.state.lock().expect("test link").tracked.insert(
            session_id,
            Tracked {
                rdev,
                prior_claim: false,
                term: Weak::new(),
                fabric: Weak::new(),
                last: None,
            },
        );
        link
    }

    /// A stalled link that carries no session yet ([`Self::register`] registers one).
    pub(crate) fn bare() -> Self {
        let (link, far) = KeeperLink::stalled();
        Self {
            state: Mutex::new(State {
                phase: Phase::Live(Arc::new(link)),
                buffered: Vec::new(),
                owed: VecDeque::new(),
                tracked: BTreeMap::new(),
                pump: false,
            }),
            far,
        }
    }

    /// Queue ops until the queue takes no more.
    pub(crate) fn fill(&self) {
        let st = self.state.lock().expect("test link");
        let Phase::Live(link) = &st.phase else {
            unreachable!("a test link is live")
        };
        while link.enqueue(LinkOp::Release { rdev: FILLER_RDEV }) {}
    }

    /// Fill the queue but for `free` places (what the keeper read so far is taken).
    pub(crate) fn fill_but(&self, free: usize) {
        let _ = self.far.drain();
        let st = self.state.lock().expect("test link");
        let Phase::Live(link) = &st.phase else {
            unreachable!("a test link is live")
        };
        for _ in free..aterm_keeper::client::LINK_QUEUE {
            assert!(link.enqueue(LinkOp::Release { rdev: FILLER_RDEV }));
        }
    }

    /// [`register`] over this link.
    pub(crate) fn register(
        &self,
        session_id: u64,
        rdev: u64,
        master: std::os::fd::OwnedFd,
        journal: LinkOp,
    ) {
        let op = LinkOp::Register {
            master,
            header: MasterHeader {
                rdev,
                shell_pid: 1,
                shell_birth: Birth {
                    seconds: 1,
                    micros: 0,
                },
                local_id: session_id,
            },
            tag: Vec::new(),
        };
        register_in(
            &mut self.state.lock().expect("test link"),
            session_id,
            Tracked {
                rdev,
                prior_claim: false,
                term: Weak::new(),
                fabric: Weak::new(),
                last: None,
            },
            op,
            journal,
        );
    }

    /// [`release_companion`] over this link.
    pub(crate) fn release_companion(&self, session_id: u64) {
        release_companion_in(&mut self.state.lock().expect("test link"), session_id);
    }

    /// [`release`] over this link.
    pub(crate) fn release(&self, session_id: u64) {
        release_in(&mut self.state.lock().expect("test link"), session_id);
    }

    /// One pass of the meta pump over this link (no session here has a terminal to send a meta of).
    pub(crate) fn tick(&self) {
        let _ = meta_pass_over(&self.state, |_, _| None);
    }

    /// How many ops are owed.
    pub(crate) fn owed(&self) -> usize {
        self.state.lock().expect("test link").owed.len()
    }

    /// [`pending_before_grant`] over this link, waiting for nothing.
    pub(crate) fn pending(&self, successor: u32) -> bool {
        pending_over(&self.state, successor, Duration::ZERO)
    }

    /// [`bye`] over this link, carrying `carried`, waiting for nothing, as [`Self::pending`].
    pub(crate) fn bye(&self, carried: &[u64]) {
        bye_over(&self.state, carried, Duration::ZERO);
    }

    /// What the old code did with `op` (`let _ = link.enqueue(op)`).
    pub(crate) fn enqueue_dropping(&self, op: LinkOp) {
        let st = self.state.lock().expect("test link");
        let Phase::Live(link) = &st.phase else {
            unreachable!("a test link is live")
        };
        let _ = link.enqueue(op);
    }

    /// The keeper reads: every op queued so far, in order.
    pub(crate) fn drain(&self) -> Vec<LinkOp> {
        let ops = self.far.drain();
        for op in &ops {
            if let LinkOp::Companion {
                receipt: Some(receipt),
                ..
            } = op
            {
                receipt.confirm_captured();
            }
        }
        ops
    }

    /// [`replace_companion`] over this link.
    pub(crate) fn replace_companion(
        &self,
        session_id: u64,
        region: std::os::fd::OwnedFd,
        len: u64,
    ) -> CompanionOffer {
        replace_companion_in(
            &mut self.state.lock().expect("test link"),
            session_id,
            region,
            len,
            None,
        )
    }

    /// Take queued ops without claiming a keeper read or sync response.
    pub(crate) fn take_queued(&self) -> Vec<LinkOp> {
        self.far.drain()
    }

    pub(crate) fn replace_companion_checked(
        &self,
        session_id: u64,
        region: std::os::fd::OwnedFd,
        len: u64,
        receipt: Arc<aterm_keeper::client::CompanionReceipt>,
    ) -> CompanionOffer {
        replace_companion_in(
            &mut self.state.lock().expect("test link"),
            session_id,
            region,
            len,
            Some(receipt),
        )
    }
}

/// What `session`'s registration by window `own_pid` says of it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn registration_tag(
    session: &crate::Session,
    own_pid: u32,
    predecessor: Option<u32>,
) -> Tag {
    Tag {
        sid: Some(session.ctx.self_id.as_str().to_string()),
        owner: Some(own_pid),
        rekey: session.rekey_channel,
        loader: session.body_loader,
        frozen: session.frozen_path,
        handed: predecessor.zip(session.handoff_local_id),
        keyed: session.journal_key,
        slog_ep: Some(
            session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog()
                .log()
                .ep(),
        ),
    }
}

/// Release (`Session::drop`).
pub(crate) fn release(session_id: u64) {
    if !on() {
        return;
    }
    #[cfg(unix)]
    release_in(&mut keeper_state(), session_id);
    #[cfg(not(unix))]
    let _ = session_id;
}

/// [`release`] over `st`.
#[cfg(unix)]
fn release_in(st: &mut State, session_id: u64) {
    if let Some(op) = release_op(st, session_id)
        && matches!(st.phase, Phase::Live(_))
    {
        send_in(st, Some(session_id), op);
    }
}

/// What releasing `session_id` owes the keeper, its tracking dropped from `st`: `None` when the
/// keeper holds nothing of it (untracked, or its REGISTER still owed with no prior claim, which
/// is dropped here instead).
#[cfg(unix)]
fn release_op(st: &mut State, session_id: u64) -> Option<LinkOp> {
    let tracked = st.tracked.remove(&session_id)?;
    st.buffered.retain(|(id, _)| *id != session_id);
    let unadmitted = st.owed.iter().any(|(id, op)| {
        *id == Some(session_id)
            && matches!(op, LinkOp::Register { header, .. } if header.local_id == session_id)
    });
    if unadmitted && !tracked.prior_claim {
        st.owed.retain(|(id, _)| *id != Some(session_id));
        return None;
    }
    Some(LinkOp::Release { rdev: tracked.rdev })
}

/// Release each of `sessions` at the final exit, as closing its tab does, and wait at most
/// [`QUIT_RELEASE_WAIT`] until the link has written the last of those RELEASEs. The link drops
/// its duplicate of a master as it takes the op and the keeper its own as it reads it, so each
/// master's last close can come while its shell's reap runs rather than at the BYE's disconnect,
/// after this process has exited. `true` too when the keeper holds none of them (off, or not yet
/// live). `false` when they were not all written in time (a full queue, a slow keeper): a RELEASE
/// already queued is still written in its turn, one still owed goes with the BYE, and the caller
/// keeps reading its masters, which the keeper may still hold.
#[cfg(unix)]
pub(crate) fn release_at_quit(sessions: &[u64]) -> bool {
    !on() || release_at_quit_over(&KEEPER_STATE, sessions, QUIT_RELEASE_WAIT)
}

/// [`release_at_quit`] over `state`, waiting at most `wait`.
#[cfg(unix)]
fn release_at_quit_over(state: &Mutex<State>, sessions: &[u64], wait: Duration) -> bool {
    let (link, last) = {
        let mut st = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let live = matches!(st.phase, Phase::Live(_));
        let mut last = None;
        for &id in sessions {
            if let Some(op) = release_op(&mut st, id)
                && live
                && let Some((before, op)) = last.replace((id, op))
            {
                send_in(&mut st, Some(before), op);
            }
        }
        let Some((id, op)) = last else {
            return true;
        };
        match live_link_in(&mut st) {
            Ok(Some(link)) => (link, op),
            Ok(None) => return true,
            Err(()) => {
                st.owed.push_back((Some(id), op));
                return false;
            }
        }
    };
    link.send_lasting_and_wait(last, wait)
}

/// Pending (both seamless-update lanes, before the grant).
#[cfg(any(unix, test))]
pub(crate) fn pending_before_grant(successor: u32) -> bool {
    if !on() {
        return true;
    }
    #[cfg(unix)]
    {
        pending_over(&KEEPER_STATE, successor, PENDING_WAIT)
    }
    #[cfg(not(unix))]
    {
        let _ = successor;
        true
    }
}

/// [`pending_before_grant`] over `state`, waiting at most `wait`.
#[cfg(unix)]
fn pending_over(state: &Mutex<State>, successor: u32, wait: Duration) -> bool {
    let written = match live_link(state) {
        Ok(None) => true,
        Ok(Some(link)) => link.send_and_wait(LinkOp::Pending { pid: successor }, wait),
        Err(()) => false,
    };
    if !written {
        aterm_log::warn!(
            "keeper: PENDING for successor {successor} was not written; the holder scan stands alone"
        );
    }
    written
}

/// Bye — only from the final-exit path, just before `exit(0)` ( step 4), naming the masters of
/// `carried`, the sessions the quit leaves running (`App::developer_tools_custody_at_exit`): the
/// keeper keeps those past the quit for the next launch's HELLO, and closes every other.
pub(crate) fn bye(carried: &[u64]) {
    #[cfg(unix)]
    if on() {
        bye_over(&KEEPER_STATE, carried, BYE_WAIT);
        ON.store(false, Ordering::Relaxed);
    }
    #[cfg(not(unix))]
    let _ = carried;
}

/// Whether aterm's quit would leave session `session_id`'s program running: the keeper this
/// process talks to keeps past the quit the masters its BYE names (its WELCOME said so) and
/// holds this session's, whose REGISTER the link's connection to it carried with no refusal read
/// since ([`KeeperLink::keeps`]). `false` with no live link, with a keeper older than the keep
/// set (which reads that BYE as a plain one and closes them all), and for a session whose
/// REGISTER is still owed or queued, which a BYE drops.
pub(crate) fn carries_at_quit(session_id: u64) -> bool {
    #[cfg(unix)]
    {
        if !on() {
            return false;
        }
        carries_in(&keeper_state(), session_id)
    }
    #[cfg(not(unix))]
    {
        let _ = session_id;
        false
    }
}

/// [`carries_at_quit`] over `st`.
#[cfg(unix)]
fn carries_in(st: &State, session_id: u64) -> bool {
    match &st.phase {
        Phase::Live(link) => st
            .tracked
            .get(&session_id)
            .is_some_and(|tracked| link.keeps(tracked.rdev)),
        Phase::Off | Phase::Armed => false,
    }
}

/// Final quit intent supersedes unsent lifecycle work; the waited-room bound
/// does not guarantee delivery. The BYE names the master of each of `carried` the link
/// registered; one it never registered has none to keep.
#[cfg(unix)]
fn bye_over(state: &Mutex<State>, carried: &[u64], wait: Duration) {
    let (link, keep) = {
        let mut st = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let keep: Vec<u64> = carried
            .iter()
            .filter_map(|session_id| st.tracked.get(session_id).map(|tracked| tracked.rdev))
            .collect();
        (quit_link_in(&mut st), keep)
    };
    if let Some(link) = link
        && !link.send_and_wait(LinkOp::Bye { keep }, wait)
    {
        aterm_log::warn!("keeper: the BYE was not written");
    }
}

/// The META pump: one thread, started with the live link, that looks at every
/// registered terminal every [`META_POLL`] and attempts its META when the
/// fingerprint or the session's fabric moved.
#[cfg(unix)]
fn start_meta_pump() {
    {
        let mut st = keeper_state();
        if st.pump {
            return;
        }
        st.pump = true;
    }
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-keeper-meta".to_string(),
        crate::qos::Role::Housekeeping,
        || {
            let period = quiet().unwrap_or(META_POLL);
            loop {
                std::thread::sleep(period);
                if !on() {
                    return;
                }
                meta_pass();
            }
        },
    );
    if let Err(e) = spawned {
        aterm_log::warn!("keeper: the META pump did not start: {e}");
        keeper_state().pump = false;
    }
}

/// One pass of the pump (and, for tests, one step of it).
#[cfg(unix)]
pub(crate) fn meta_pass() -> usize {
    meta_pass_over(&KEEPER_STATE, |meta, fabric| {
        crate::seamless::keeper_meta_of(meta, Some(fabric))
    })
}

/// [`meta_pass`] over `state`, building each META with `build` from the
/// terminal's meta carry and the session's fabric.
#[cfg(unix)]
fn meta_pass_over<B>(state: &Mutex<State>, build: B) -> usize
where
    B: Fn(aterm_core::terminal::CheckpointMeta, &crate::fabric::KeeperCarry) -> Option<Vec<u8>>,
{
    let targets: Vec<(u64, Tracked)> = {
        let mut st = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Phase::Live(link) = &st.phase {
            let link = Arc::clone(link);
            let _ = flush_owed(&link, &mut st.owed);
        }
        st.tracked.iter().map(|(id, t)| (*id, t.clone())).collect()
    };
    let mut sent = 0;
    for (
        id,
        Tracked {
            rdev,
            prior_claim: _,
            term,
            fabric,
            last,
        },
    ) in targets
    {
        let Some(term) = term.upgrade() else {
            continue;
        };
        let carry = fabric
            .upgrade()
            .map(|fabric| fabric.keeper_carry())
            .unwrap_or_default();
        let (fingerprint, meta) = {
            let Ok(engine) = term.try_lock() else {
                continue;
            };
            let fingerprint = Fingerprint::of(&engine);
            if last
                .as_ref()
                .is_some_and(|(sent, sent_carry)| *sent == fingerprint && *sent_carry == carry)
            {
                continue;
            }
            (fingerprint, engine.carry_meta_abandoning_partial())
        };
        let meta = build(meta, &carry);
        let mut st = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State {
            phase,
            tracked,
            owed,
            ..
        } = &mut *st;
        let Some(tracked) = tracked.get_mut(&id) else {
            continue;
        };
        if let (Some(meta), Phase::Live(link)) = (meta, &*phase)
            && owed.is_empty()
            && link.enqueue(LinkOp::Meta { rdev, meta })
        {
            tracked.last = Some((fingerprint, carry));
            sent += 1;
        }
    }
    sent
}

impl crate::App {
    /// `End sessions` (the recovery row).
    pub(crate) fn end_recovered_sessions(&mut self) -> usize {
        let shells = std::mem::take(&mut self.keeper_recovered);
        let mut ended = 0;
        for pooled in self.pool.sessions.values() {
            let session = &pooled.session;
            if shells.contains(&session.pid)
                && session.handoff_local_id.is_some()
                && !session
                    .child_reaped
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = aterm_pty::hangup_shell(&session.shell_identity, session.master);
                ended += 1;
            }
        }
        ended
    }
}

#[cfg(test)]
#[path = "keeper_link_tests.rs"]
mod tests;
