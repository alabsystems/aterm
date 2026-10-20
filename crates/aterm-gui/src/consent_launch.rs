// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FILE ACCESS ON A NEW MAC, BY DEFAULT (owner ask 2026-10-16: "on starting aterm … it
//! needs to enable [file system access everywhere] by default", on macOS 26 Tahoe and 27).
//!
//! macOS lets no app grant itself file access (design §0): only the owner switches on Full
//! Disk Access, and only the owner answers a folder dialog. What aterm does by default is
//! put both in front of the owner when it starts, instead of waiting for an agent to hit a
//! dialog first:
//!
//! 1. **Full Disk Access.** A launch whose probe reads the grant off, on a Mac where aterm
//!    never verified it, brings the owner to System Settings ▸ Privacy & Security ▸ Full
//!    Disk Access at the first quiet moment, with aterm's panel and its icon to drag into
//!    the list (`App::hand_fda_to_owner`, `devtools_helper::admit_fda`): once per version,
//!    never while the owner's Not now holds, never on a build whose grant would not survive
//!    the next one. [`FdaLaunch`] is where that stands for this process.
//! 2. **The folders, while Full Disk Access stays off.** Once that hand-over ended without
//!    the owner acting on it (Not now, or the panel lapsed) or was not offered at all, the
//!    warm-up walks the configured folders (Documents, Desktop and Downloads) at the next
//!    quiet moment, so macOS asks there and not inside an agent's blocked syscall
//!    ([`decide_warmup`]; `[privacy] warmup = "automatic"`, the default). Once a pass has
//!    every folder answered, the bundle id is noted (`<state>/privacy-warmup-answered`)
//!    and never walked again: macOS keeps each answer for a grant-stable build.
//!
//! The order on a new Mac is the Full Disk Access page, the Developer Tools page, aterm's
//! once-per-Mac debugger-access question (`debugger_access`, owner ruling O29), then the
//! folders: the warm-up waits while that question is still to come and while a
//! debugger-access change is under way, so no folder dialog stacks on macOS's
//! administrator dialog (one left unanswered holds the folders for that process).
//!
//! Nothing here answers a dialog, flips a switch or writes TCC.db, and no control verb,
//! environment variable or session reaches it: the triggers are the window's own launch
//! check and the owner's own presses. Held by construction: `App::tick_launch_warmup`,
//! `App::hand_fda_to_owner`, `App::fda_page_opened_by_owner` and `Lane::queue_fda` are
//! clippy.toml `disallowed-methods` rows, and only those callers carry the `#[expect]`. A headless instance and every unit test hold the
//! inert arm ([`LaunchConsent::new`]), which reads and writes nothing.

use std::time::Instant;

use aterm_containment::consent::{FdaState, Folder};

use crate::consent_warmup::WarmupRow;
use crate::devtools_helper::{FdaEnded, HandOver, RECHECK_EVERY};

/// Where this process's own Full Disk Access hand-over stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FdaLaunch {
    /// The launch check has not decided yet.
    #[default]
    Undecided,
    /// Held for its quiet moment, or its panel is up.
    Waiting,
    /// The panel closed: whether the owner acted on it ([`FdaEnded::owner_acted`]).
    Ended {
        /// The owner said it is on, dropped aterm's icon, or went off with System Settings.
        owner_acted: bool,
    },
    /// Not offered this process: the grant reads held or unknown, the owner declined, it
    /// was shown already for this version, the build's grant would not survive the next
    /// one, or the Full Disk Access question is switched off.
    NotAsked,
}

/// WHETHER THE LAUNCH CHECK HANDS FULL DISK ACCESS OVER: the probe read the grant off on a
/// Mac where aterm never verified it — the access card's launch verdict `NoNeedYet`, or
/// `ReadsOffAfterVerified` beside a record verified on another Mac or for another bundle
/// (`verified_here` is `Some(false)`: the record travels with a synced or migrated config
/// directory; a record with no stamp keeps its word). Whose reading it was is not judged
/// here: the hand-over brings nothing up from a window another app is responsible for
/// (`devtools_helper::admit_fda`, at its quiet moment), and an update's successor on the
/// fork lane names its outgoing process responsible for the seconds the launch check runs
/// in. Pure.
#[must_use]
pub(crate) fn hands_over(
    fda: FdaState,
    verdict: &crate::consent_card::LaunchVerdict,
    verified_here: Option<bool>,
) -> bool {
    use crate::consent_card::{LaunchVerdict, NotDue};
    fda == FdaState::Denied
        && match verdict {
            LaunchVerdict::Quiet(NotDue::NoNeedYet) => true,
            LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified) => verified_here == Some(false),
            _ => false,
        }
}

impl FdaLaunch {
    /// What one hand-over answer leaves. The page opened with no panel beside it (the arm
    /// showed none) counts as the owner at the page: nothing will say how it ended.
    pub(crate) const fn after(answer: HandOver, panel_up: bool) -> Self {
        match answer {
            HandOver::Queued | HandOver::Listing => Self::Waiting,
            HandOver::Opened if panel_up => Self::Waiting,
            HandOver::Opened | HandOver::Shown | HandOver::Unshown => {
                Self::Ended { owner_acted: true }
            }
            HandOver::Headless | HandOver::Refused(_) | HandOver::OpenFailed => Self::NotAsked,
        }
    }

    /// How a Full Disk Access panel's end leaves it.
    pub(crate) const fn ended(ended: FdaEnded) -> Self {
        Self::Ended {
            owner_acted: ended.owner_acted(),
        }
    }
}

/// What [`decide_warmup`] judges on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WarmupFacts {
    /// `[privacy] enabled` and `warmup = "automatic"`.
    pub(crate) automatic: bool,
    /// Where this process's Full Disk Access hand-over stands.
    pub(crate) fda_launch: FdaLaunch,
    /// The window's own Full Disk Access probe.
    pub(crate) fda: FdaState,
    /// The probe has not completed since it was last invalidated.
    pub(crate) probe_pending: bool,
    /// A folder answer keyed to this build survives the next build.
    pub(crate) grant_stable: bool,
    /// Every folder was answered for this bundle id by an earlier pass.
    /// `None`: the marker is unreadable or malformed, so no automatic pass is admitted.
    pub(crate) answered: Option<bool>,
    /// A warm-up pass walks already (the owner's own press on the Security page).
    pub(crate) walking: bool,
}

/// Why the launch warm-up does not walk this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    /// `[privacy] enabled = false`, or `warmup` is not `"automatic"`.
    Off,
    /// The owner acted on the Full Disk Access page: a grant made now may not reach this
    /// process (design §7 S1), and folder dialogs on top of it would ask for what they
    /// just gave.
    OwnerActed,
    /// The grant reads held, or the probe cannot tell.
    NotDenied,
    /// An ad hoc or unsigned build: macOS forgets its answers at the next build.
    Unstable,
    /// Every folder was answered for this bundle id before.
    Answered,
    /// The answered marker cannot be read as a bounded regular record.
    MarkerUnknown,
    /// A pass walks already.
    Walking,
    /// The worker could not start (no thread, no folder resolved).
    NotStarted,
    /// Every configured folder is `app-data`, which the launch pass never asks.
    OnlyAppData,
    /// Another app is responsible for this window (started in-process by another
    /// terminal): macOS would ask for that app, and keep its answers.
    NotOwn,
}

impl Skip {
    /// The reason in aterm.log's words.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "the automatic warm-up is off",
            Self::OwnerActed => "the owner acted on the Full Disk Access page",
            Self::NotDenied => "Full Disk Access does not read off",
            Self::Unstable => "this build's answers would not survive the next build",
            Self::Answered => "every folder was answered before",
            Self::MarkerUnknown => "the answered marker could not be read",
            Self::Walking => "a warm-up is walking already",
            Self::NotStarted => "the warm-up could not start",
            Self::OnlyAppData => "no configured folder is asked at launch (app-data never is)",
            Self::NotOwn => "another app is responsible for this window; macOS would ask for it",
        }
    }
}

/// What the launch warm-up does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WarmupStep {
    /// Something it judges on is still to come (the hand-over, the probe).
    Wait,
    /// Not this process, and why.
    Skip(Skip),
    /// Walk the folders at the next quiet moment.
    Start,
}

/// WHETHER THE LAUNCH WARM-UP WALKS. Only once the Full Disk Access hand-over is decided
/// and did not end with the owner acting on it; only on a build whose answers outlive it;
/// only for a bundle id whose folders were not all answered before; never on top of a pass
/// already walking; and only while the probe reads the grant off (`unknown` walks nothing:
/// it may be held). The probe's two rules come last, so the window can judge the rest
/// before it reads one ([`probe_free`]). Pure.
#[must_use]
pub(crate) const fn decide_warmup(facts: &WarmupFacts) -> WarmupStep {
    if !facts.automatic {
        return WarmupStep::Skip(Skip::Off);
    }
    match facts.fda_launch {
        FdaLaunch::Undecided | FdaLaunch::Waiting => return WarmupStep::Wait,
        FdaLaunch::Ended { owner_acted: true } => return WarmupStep::Skip(Skip::OwnerActed),
        FdaLaunch::Ended { owner_acted: false } | FdaLaunch::NotAsked => {}
    }
    if !facts.grant_stable {
        return WarmupStep::Skip(Skip::Unstable);
    }
    match facts.answered {
        Some(true) => return WarmupStep::Skip(Skip::Answered),
        None => return WarmupStep::Skip(Skip::MarkerUnknown),
        Some(false) => {}
    }
    if facts.walking {
        return WarmupStep::Skip(Skip::Walking);
    }
    if facts.probe_pending {
        return WarmupStep::Wait;
    }
    if !matches!(facts.fda, FdaState::Denied) {
        return WarmupStep::Skip(Skip::NotDenied);
    }
    WarmupStep::Start
}

/// `facts` with the probe's two rules passed: [`decide_warmup`] of it answers everything
/// that needs no probe, so a window waiting for its quiet moment reads none. Pure.
#[must_use]
pub(crate) const fn probe_free(facts: WarmupFacts) -> WarmupFacts {
    WarmupFacts {
        fda: FdaState::Denied,
        probe_pending: false,
        ..facts
    }
}

/// The folders the launch warm-up walks: the configured ones, never `app-data`, whose
/// Allow macOS forgets when this process ends (design §1.6a), so asking it at every launch
/// would ask forever.
#[must_use]
pub(crate) fn launch_folders<P>(folders: Vec<(Folder, P)>) -> Vec<(Folder, P)> {
    folders
        .into_iter()
        .filter(|(folder, _)| *folder != Folder::AppData)
        .collect()
}

/// Whether a finished pass had every folder answered: allowed, denied, or an error that is
/// no question (a folder that is not there). One still `unknown` or `asking` was never
/// answered (the dialog was left up, or the worker ended first), so the next launch asks
/// again.
#[must_use]
pub(crate) fn every_folder_answered(rows: &[(Folder, WarmupRow)]) -> bool {
    !rows.is_empty()
        && rows
            .iter()
            .all(|(_, row)| !matches!(row, WarmupRow::Unknown | WarmupRow::Asking))
}

/// The answered marker, injected: [`Self::live`] for a window, [`Self::inert`] for a
/// headless instance and every unit test, which reads nothing ("not answered") and writes
/// nothing.
#[derive(Clone, Copy)]
pub(crate) struct LaunchReach {
    /// The running bundle's id, once the window has read its identity off the event
    /// loop (`control_privacy::running_bundle_id_if_warm`).
    pub(crate) bundle_id: fn() -> Option<&'static str>,
    /// Every folder was answered for this bundle id before.
    pub(crate) answered: fn(&str) -> Option<bool>,
    /// Note that every folder was answered for this bundle id.
    pub(crate) note_answered: fn(&str),
    pub(crate) live: bool,
}

impl LaunchReach {
    /// The windowed instance's arm: `<state>/privacy-warmup-answered`, one bundle id per
    /// line.
    pub(crate) const fn live() -> Self {
        Self {
            bundle_id: crate::control_privacy::running_bundle_id_if_warm,
            answered: live_answered,
            note_answered: live_note_answered,
            live: true,
        }
    }

    /// The headless / unit-test arm.
    pub(crate) const fn inert() -> Self {
        Self {
            bundle_id: inert_bundle_id,
            answered: inert_answered,
            note_answered: inert_note_answered,
            live: false,
        }
    }
}

impl std::fmt::Debug for LaunchReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchReach")
            .field("live", &self.live)
            .finish()
    }
}

/// `<state>/privacy-warmup-answered`: per machine, never beside `aterm.toml` (a config
/// directory synced to another Mac must not say that Mac's folders were answered).
fn answered_marker() -> Option<std::path::PathBuf> {
    aterm_types::dirs::state_dir().map(|dir| dir.join("privacy-warmup-answered"))
}

/// A small set of channel bundle ids, stored under the machine's state directory.
/// Refuse an oversized record rather than dropping another bundle's answer.
const MAX_ANSWERED_BYTES: usize = 4 * 1024;

/// Whether a whole marker names this usable bundle id. Empty, truncated or
/// malformed records are unknown, never a reason to start an automatic pass.
fn names(text: &str, bundle_id: &str) -> Option<bool> {
    if text.is_empty()
        || !text.ends_with('\n')
        || bundle_id.trim() != bundle_id
        || !aterm_containment::consent::bundle_id_is_usable(bundle_id)
    {
        return None;
    }
    let mut found = false;
    for line in text.lines() {
        let line = line.trim();
        if !aterm_containment::consent::bundle_id_is_usable(line) {
            return None;
        }
        found |= line == bundle_id;
    }
    Some(found)
}

fn live_answered(bundle_id: &str) -> Option<bool> {
    answered_marker().and_then(|path| answered_at(&path, bundle_id))
}

/// A genuine absence is known. Every other read must pass the existing
/// bounded regular, no-final-symlink marker reader.
fn read_answered_at(path: &std::path::Path) -> std::io::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => crate::config_marker::read_marker(path, MAX_ANSWERED_BYTES)
            .map(Some)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the launch marker is not a bounded regular UTF-8 record",
                )
            }),
    }
}

/// The live marker read at an explicit path; tests own that path.
pub(crate) fn answered_at(path: &std::path::Path, bundle_id: &str) -> Option<bool> {
    if bundle_id.trim() != bundle_id || !aterm_containment::consent::bundle_id_is_usable(bundle_id)
    {
        return None;
    }
    match read_answered_at(path).ok()? {
        None => Some(false),
        Some(text) => names(&text, bundle_id),
    }
}

/// Best effort: a successful pass is answered in this process. A later
/// launch judges only a known marker; unreadable state admits no new pass.
fn live_note_answered(bundle_id: &str) {
    let Some(path) = answered_marker() else {
        return;
    };
    note_answered_at(&path, bundle_id);
}

/// The live marker write at an explicit path; tests own that path. Never
/// replace an unreadable or malformed occupant, and never follow its link.
fn note_answered_at(path: &std::path::Path, bundle_id: &str) {
    if bundle_id.trim() != bundle_id || !aterm_containment::consent::bundle_id_is_usable(bundle_id)
    {
        return;
    }
    let text = match read_answered_at(path) {
        Ok(None) => String::new(),
        Ok(Some(text)) => match names(&text, bundle_id) {
            Some(true) => return,
            Some(false) => text,
            None => return,
        },
        Err(_) => return,
    };
    // Ordered: an absent marker reads as not answered, so the pass is asked again.
    let _ = crate::config_marker::write_marker(
        path,
        &format!("{text}{bundle_id}\n"),
        MAX_ANSWERED_BYTES,
        crate::durable::Flush::Ordered,
    );
}

fn inert_bundle_id() -> Option<&'static str> {
    None
}

fn inert_answered(_bundle_id: &str) -> Option<bool> {
    Some(false)
}

fn inert_note_answered(_bundle_id: &str) {}

/// Where the launch warm-up stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Warmup {
    /// Not decided yet.
    #[default]
    Pending,
    /// This process's pass walks.
    Walking,
    /// Done for this process: walked, or skipped.
    Settled,
}

/// THE APP-OWNED STATE of the first-launch file access: where the Full Disk Access
/// hand-over stands and whether the launch warm-up walked.
#[derive(Debug)]
pub(crate) struct LaunchConsent {
    reach: LaunchReach,
    fda: FdaLaunch,
    warmup: Warmup,
    /// The bundle id the walking pass asks for.
    walking_for: Option<String>,
    /// When the warm-up looks for its quiet moment again; `None` while it waits on
    /// something that wakes the window itself (the launch check, the probe, the panel).
    next_look: Option<Instant>,
    /// The answered marker as read for this bundle id, once per process.
    answered: Option<(String, Option<bool>)>,
    /// The signing identity was asked to be read off the event loop.
    identity_asked: bool,
    /// The warm-up pass this state started (`WarmupState::generation`): while the
    /// stored rows are that pass's, they are the owner's answers to aterm's own
    /// question, never a need.
    launch_generation: Option<u64>,
    /// The completed probe reading the launch check decided on: a launch that read the
    /// grant held (or `unknown`) settles the warm-up at its first look, with no wait for
    /// a quiet moment.
    launch_read: Option<FdaState>,
}

impl LaunchConsent {
    /// The state for an instance: the live marker for a window, the inert one otherwise.
    pub(crate) fn new(headless: bool) -> Self {
        Self::with_reach(if headless {
            LaunchReach::inert()
        } else {
            LaunchReach::live()
        })
    }

    /// The state with an injected marker arm: the tests' stand-in for a window.
    pub(crate) fn with_reach(reach: LaunchReach) -> Self {
        Self {
            reach,
            fda: FdaLaunch::Undecided,
            warmup: Warmup::Pending,
            walking_for: None,
            next_look: None,
            answered: None,
            identity_asked: false,
            launch_generation: None,
            launch_read: None,
        }
    }

    /// The launch check decided on the completed reading `fda`.
    pub(crate) fn note_launch_read(&mut self, fda: FdaState) {
        self.launch_read = Some(fda);
    }

    /// The completed reading the launch check decided on, when it has.
    pub(crate) const fn launch_read(&self) -> Option<FdaState> {
        self.launch_read
    }

    /// Ask, once, for the signing identity to be read off the event loop: `true` the
    /// first time (the caller starts the read), `false` after.
    pub(crate) fn ask_identity(&mut self) -> bool {
        !std::mem::replace(&mut self.identity_asked, true)
    }

    /// Whether this instance may act at all.
    pub(crate) const fn live(&self) -> bool {
        self.reach.live
    }

    /// The running bundle's id, when the window has read it.
    pub(crate) fn bundle_id(&self) -> Option<&'static str> {
        (self.reach.bundle_id)()
    }

    /// Where the Full Disk Access hand-over stands.
    pub(crate) const fn fda(&self) -> FdaLaunch {
        self.fda
    }

    /// The Full Disk Access hand-over moved.
    pub(crate) fn set_fda(&mut self, fda: FdaLaunch) {
        self.fda = fda;
    }

    /// Whether the launch warm-up is done for this process.
    pub(crate) fn settled(&self) -> bool {
        self.warmup == Warmup::Settled
    }

    /// Whether this process's pass walks.
    pub(crate) fn walking(&self) -> bool {
        self.warmup == Warmup::Walking
    }

    /// Whether a look for the quiet moment is due at `now` (or nothing is scheduled).
    pub(crate) fn look_due(&self, now: Instant) -> bool {
        self.next_look.is_none_or(|at| now >= at)
    }

    /// Wait on what wakes the window itself: no timer.
    pub(crate) fn wait(&mut self) {
        self.next_look = None;
    }

    /// Not a quiet moment: look again [`RECHECK_EVERY`] later.
    pub(crate) fn look_later(&mut self, now: Instant) {
        self.next_look = Some(now + RECHECK_EVERY);
    }

    /// The park's deadline: the next look, while one is scheduled.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        if self.warmup == Warmup::Pending {
            self.next_look
        } else {
            None
        }
    }

    /// Whether every folder was answered for `bundle_id` before: the marker, read once.
    pub(crate) fn answered(&mut self, bundle_id: &str) -> Option<bool> {
        match &self.answered {
            Some((id, answered)) if id == bundle_id => *answered,
            _ => {
                let answered = (self.reach.answered)(bundle_id);
                self.answered = Some((bundle_id.to_owned(), answered));
                answered
            }
        }
    }

    /// Not this process.
    pub(crate) fn skip(&mut self, why: Skip) {
        aterm_log::info!(
            "file access at launch: no folder warm-up ({})",
            why.as_str()
        );
        self.warmup = Warmup::Settled;
        self.next_look = None;
    }

    /// The pass for `bundle_id` started, as the warm-up's pass `generation`.
    pub(crate) fn started(&mut self, bundle_id: &str, generation: u64) {
        aterm_log::info!("file access at launch: asking macOS for each folder");
        self.warmup = Warmup::Walking;
        self.walking_for = Some(bundle_id.to_owned());
        self.launch_generation = Some(generation);
        self.next_look = None;
    }

    /// Whether the warm-up's rows of pass `generation` are this launch pass's: a
    /// Don't Allow among them is the owner's answer to aterm's own question, however
    /// many wakes report the pass after it ended. An owner's pass is a new generation.
    pub(crate) fn owns_rows(&self, generation: u64) -> bool {
        self.launch_generation == Some(generation)
    }

    /// The pass ended with `rows`: every folder answered notes the bundle id, so no later
    /// launch walks again.
    pub(crate) fn pass_ended(&mut self, rows: &[(Folder, WarmupRow)]) {
        if self.warmup != Warmup::Walking {
            return;
        }
        self.warmup = Warmup::Settled;
        let Some(bundle_id) = self.walking_for.take() else {
            return;
        };
        if every_folder_answered(rows) {
            (self.reach.note_answered)(&bundle_id);
            self.answered = Some((bundle_id, Some(true)));
            aterm_log::info!("file access at launch: every folder answered");
        } else {
            aterm_log::info!(
                "file access at launch: a folder went unanswered; asked again next launch"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtools_helper::{Closed, Refusal};

    fn facts() -> WarmupFacts {
        WarmupFacts {
            automatic: true,
            fda_launch: FdaLaunch::NotAsked,
            fda: FdaState::Denied,
            probe_pending: false,
            grant_stable: true,
            answered: Some(false),
            walking: false,
        }
    }

    /// The warm-up walks only after the Full Disk Access hand-over is decided and left
    /// without the owner acting, only while the grant reads off on a build that keeps its
    /// answers, once per bundle id; each refusal holds on its own.
    #[test]
    fn the_launch_warm_up_is_a_table_of_named_refusals_then_a_walk() {
        assert_eq!(decide_warmup(&facts()), WarmupStep::Start);
        let ended = |owner_acted| FdaLaunch::Ended { owner_acted };
        let cases = [
            (
                WarmupFacts {
                    automatic: false,
                    ..facts()
                },
                WarmupStep::Skip(Skip::Off),
            ),
            (
                WarmupFacts {
                    fda_launch: FdaLaunch::Undecided,
                    ..facts()
                },
                WarmupStep::Wait,
            ),
            (
                WarmupFacts {
                    fda_launch: FdaLaunch::Waiting,
                    ..facts()
                },
                WarmupStep::Wait,
            ),
            (
                WarmupFacts {
                    fda_launch: ended(true),
                    ..facts()
                },
                WarmupStep::Skip(Skip::OwnerActed),
            ),
            (
                WarmupFacts {
                    fda_launch: ended(false),
                    ..facts()
                },
                WarmupStep::Start,
            ),
            (
                WarmupFacts {
                    probe_pending: true,
                    ..facts()
                },
                WarmupStep::Wait,
            ),
            (
                WarmupFacts {
                    fda: FdaState::Granted,
                    ..facts()
                },
                WarmupStep::Skip(Skip::NotDenied),
            ),
            (
                WarmupFacts {
                    fda: FdaState::Unknown,
                    ..facts()
                },
                WarmupStep::Skip(Skip::NotDenied),
            ),
            (
                WarmupFacts {
                    grant_stable: false,
                    ..facts()
                },
                WarmupStep::Skip(Skip::Unstable),
            ),
            (
                WarmupFacts {
                    answered: Some(true),
                    ..facts()
                },
                WarmupStep::Skip(Skip::Answered),
            ),
            (
                WarmupFacts {
                    walking: true,
                    ..facts()
                },
                WarmupStep::Skip(Skip::Walking),
            ),
        ];
        for (facts, step) in cases {
            assert_eq!(decide_warmup(&facts), step, "{facts:?}");
        }
    }

    /// Everything but the probe's two rules is answered before any probe is read: a window
    /// waiting for its quiet moment reads none, and a probe-free refusal holds whatever
    /// the probe would say.
    #[test]
    fn the_probe_free_rules_are_judged_before_a_probe() {
        let unprobed = WarmupFacts {
            fda: FdaState::Unknown,
            probe_pending: true,
            ..facts()
        };
        assert_eq!(decide_warmup(&unprobed), WarmupStep::Wait);
        assert_eq!(decide_warmup(&probe_free(unprobed)), WarmupStep::Start);
        for (facts, why) in [
            (
                WarmupFacts {
                    grant_stable: false,
                    ..unprobed
                },
                Skip::Unstable,
            ),
            (
                WarmupFacts {
                    answered: Some(true),
                    ..unprobed
                },
                Skip::Answered,
            ),
            (
                WarmupFacts {
                    walking: true,
                    ..unprobed
                },
                Skip::Walking,
            ),
        ] {
            assert_eq!(decide_warmup(&facts), WarmupStep::Skip(why));
            assert_eq!(decide_warmup(&probe_free(facts)), WarmupStep::Skip(why));
        }
    }

    /// A panel the owner said Not now to, or that lapsed with nothing dropped, is followed
    /// by the folders; one they acted on (switched on, done, dropped, went off with System
    /// Settings) is not.
    #[test]
    fn only_a_panel_left_alone_is_followed_by_the_folders() {
        let acted = |why, dropped| {
            FdaLaunch::ended(FdaEnded { why, dropped }) == FdaLaunch::Ended { owner_acted: true }
        };
        assert!(!acted(Closed::NotNow, false));
        assert!(!acted(Closed::Lapsed, false));
        assert!(acted(Closed::NotNow, true));
        assert!(acted(Closed::Lapsed, true));
        for why in [
            Closed::SwitchedOn,
            Closed::Done,
            Closed::SettingsGone,
            Closed::Replaced,
        ] {
            assert!(acted(why, false), "{why:?}");
        }
    }

    /// What each hand-over answer leaves: queued or opened with its panel waits; a page
    /// with no panel is the owner's; a refusal or a failed open is no hand-over.
    #[test]
    fn each_hand_over_answer_moves_the_launch_state() {
        assert_eq!(
            FdaLaunch::after(HandOver::Queued, false),
            FdaLaunch::Waiting
        );
        assert_eq!(FdaLaunch::after(HandOver::Opened, true), FdaLaunch::Waiting);
        assert_eq!(
            FdaLaunch::after(HandOver::Opened, false),
            FdaLaunch::Ended { owner_acted: true }
        );
        for answer in [
            HandOver::Headless,
            HandOver::OpenFailed,
            HandOver::Refused(Refusal::Declined),
            HandOver::Refused(Refusal::Shown),
            HandOver::Refused(Refusal::Granted),
        ] {
            assert_eq!(
                FdaLaunch::after(answer, false),
                FdaLaunch::NotAsked,
                "{answer:?}"
            );
        }
    }

    /// The launch pass never walks `app-data`, whatever `warmup_folders` names.
    #[test]
    fn the_launch_pass_never_walks_app_data() {
        let folders = vec![
            (Folder::Documents, 1),
            (Folder::AppData, 2),
            (Folder::Downloads, 3),
        ];
        assert_eq!(
            launch_folders(folders),
            vec![(Folder::Documents, 1), (Folder::Downloads, 3)]
        );
    }

    /// A pass with a folder still unknown or asking was not answered whole; allowed,
    /// denied and an error (a folder that is not there) are answers.
    #[test]
    fn a_pass_is_answered_only_when_every_folder_is() {
        use WarmupRow::{Allowed, Asking, Denied, Error, Unknown};
        assert!(!every_folder_answered(&[]));
        assert!(every_folder_answered(&[
            (Folder::Documents, Allowed),
            (Folder::Desktop, Denied),
            (Folder::Downloads, Error),
        ]));
        for open in [Unknown, Asking] {
            assert!(!every_folder_answered(&[
                (Folder::Documents, Allowed),
                (Folder::Desktop, open),
            ]));
        }
    }

    /// The marker names a bundle id on a line of its own; a dev bundle's answers are not
    /// the release's.
    #[test]
    fn the_marker_names_whole_bundle_ids() {
        let text = "com.aterm.aterm.dev\n";
        assert_eq!(names(text, "com.aterm.aterm.dev"), Some(true));
        assert_eq!(names(text, "com.aterm.aterm"), Some(false));
        assert_eq!(names("", "com.aterm.aterm"), None);
    }

    thread_local! {
        static NOTED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn noting(bundle_id: &str) {
        NOTED.with(|noted| noted.borrow_mut().push(bundle_id.to_owned()));
    }

    /// A pass that answered every folder notes its bundle id once and settles; one that
    /// left a folder unanswered settles and notes nothing, so the next launch walks again.
    #[test]
    fn a_whole_pass_notes_its_bundle_id_and_a_partial_one_does_not() {
        let reach = LaunchReach {
            note_answered: noting,
            live: true,
            ..LaunchReach::inert()
        };
        let mut state = LaunchConsent::with_reach(reach);
        state.started("com.aterm.aterm", 7);
        assert!(state.walking());
        state.pass_ended(&[(Folder::Documents, WarmupRow::Denied)]);
        assert!(state.settled());
        assert!(state.owns_rows(7), "its rows stay its own after it ended");
        assert!(!state.owns_rows(8), "an owner's later pass is not");
        assert_eq!(state.answered("com.aterm.aterm"), Some(true));
        NOTED.with(|noted| assert_eq!(*noted.borrow(), ["com.aterm.aterm"]));

        let mut state = LaunchConsent::with_reach(reach);
        state.started("com.aterm.aterm", 1);
        state.pass_ended(&[(Folder::Documents, WarmupRow::Asking)]);
        assert!(state.settled());
        assert_eq!(state.answered("com.aterm.aterm"), Some(false));
        NOTED.with(|noted| assert_eq!(noted.borrow().len(), 1));
    }

    /// Actual marker readers and writers use only these private paths. An
    /// ordinary multi-bundle record round-trips first; neither a planted
    /// link nor an oversized/malformed record authorizes a read or rewrite.
    #[test]
    #[cfg(unix)]
    fn the_launch_marker_is_bounded_and_never_follows_a_link() {
        const RELEASE: &str = "com.aterm.aterm";
        const DEV: &str = "com.aterm.aterm.dev";
        let root = aterm_tempfile::TempDir::new().expect("a private launch marker");
        let ordinary = root.path().join("ordinary");
        assert_eq!(
            answered_at(&ordinary, RELEASE),
            Some(false),
            "a genuine absence"
        );
        let recording = aterm_durable::seam::Recording::start();
        note_answered_at(&ordinary, RELEASE);
        assert_eq!(
            recording.take(),
            [aterm_durable::Flush::Ordered],
            "an answered pass is only ordered: an absent marker asks it again"
        );
        drop(recording);
        assert_eq!(
            answered_at(&ordinary, RELEASE),
            Some(true),
            "ordinary write/read"
        );
        assert_eq!(answered_at(&ordinary, DEV), Some(false), "another bundle");
        note_answered_at(&ordinary, DEV);
        let whole = std::fs::read(&ordinary).expect("both bundles");
        assert_eq!(
            (answered_at(&ordinary, RELEASE), answered_at(&ordinary, DEV)),
            (Some(true), Some(true))
        );
        note_answered_at(&ordinary, RELEASE);
        assert_eq!(std::fs::read(&ordinary).unwrap(), whole, "no duplicate");

        let target = root.path().join("target");
        let standing = format!("{DEV}\n");
        std::fs::write(&target, &standing).expect("private target");
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(&target, &link).expect("private link");
        let link_admitted = answered_at(&link, DEV);
        note_answered_at(&link, RELEASE);
        let target_after = std::fs::read(&target).expect("target after writer");

        let oversized = root.path().join("oversized");
        let large = format!("{RELEASE}\n{}\n", "x".repeat(1024 * 1024));
        std::fs::write(&oversized, &large).expect("oversized private record");
        let large_admitted = answered_at(&oversized, RELEASE);
        note_answered_at(&oversized, DEV);
        let large_preserved = std::fs::read(&oversized).unwrap() == large.as_bytes();

        let malformed = root.path().join("malformed");
        let invalid = format!("{RELEASE}\nnot a bundle id\n");
        std::fs::write(&malformed, &invalid).expect("malformed private record");
        let malformed_admitted = answered_at(&malformed, RELEASE);
        note_answered_at(&malformed, DEV);
        let malformed_preserved = std::fs::read(&malformed).unwrap() == invalid.as_bytes();

        assert_eq!(
            (
                link_admitted,
                target_after,
                large_admitted,
                large_preserved,
                malformed_admitted,
                malformed_preserved
            ),
            (None, standing.into_bytes(), None, true, None, true),
            "all standing occupants are observed before the aggregate refusal"
        );
    }

    /// Only a known marker may admit the automatic pass. These are actual
    /// private filesystem observations, not global environment or permission changes.
    #[test]
    fn an_unknown_launch_marker_never_admits_an_automatic_pass() {
        const ID: &str = "com.aterm.aterm";
        let root = aterm_tempfile::TempDir::new().expect("private marker state");
        let path = root.path().join("answered");
        let judge = || WarmupFacts {
            answered: answered_at(&path, ID),
            ..facts()
        };
        assert_eq!(judge().answered, Some(false), "known absence");
        assert_eq!(decide_warmup(&judge()), WarmupStep::Start);
        for bytes in [
            &b""[..],
            &b"com.aterm.aterm"[..],
            &b"bad record\n"[..],
            &b"\xff\n"[..],
        ] {
            std::fs::write(&path, bytes).expect("unknown private record");
            assert_eq!(judge().answered, None);
            assert_eq!(
                decide_warmup(&judge()),
                WarmupStep::Skip(Skip::MarkerUnknown)
            );
            note_answered_at(&path, ID);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "unknown is never overwritten"
            );
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(judge().answered, None, "a nonregular occupant");
        assert_eq!(
            decide_warmup(&judge()),
            WarmupStep::Skip(Skip::MarkerUnknown)
        );
        note_answered_at(&path, ID);
        assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
    }
}
