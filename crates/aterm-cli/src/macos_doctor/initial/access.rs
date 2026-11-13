// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Access rows of System State (`docs/DESIGN-system-state-2026-10-21.md` §3.3 and §6): what
//! each grant reads, read without a prompt, and when the Permissions stage of one revision is
//! complete. Nothing here opens System Settings or raises a dialog: the window's access lanes hand
//! each step to macOS and record it in [`access_record`]. The stage reads that record for what was
//! attempted and each grant afresh for whether it is on: a record never makes a row `ok`.
//!
//! A grant the person opted out of (§4: a `[privacy]` switch, a debugger access turned off, or a
//! Not now in this approval) is honored as the row's wanted state and never asks for a person.

use super::{OFFERED, Row, UNREAD, WAITING};
use aterm_types::macos_setup::opt_out::OptOut;
use aterm_types::macos_setup::{
    ACCESS_OFFERED, Access, AccessOutcome, DEBUGGER_NOT_ASKED, Observation, Selection, Setting,
    Standing, Subject, access_record, opt_out,
};
use std::sync::Mutex;

/// Wakes a concurrent Secrets lane waiting for the root LAN broker ([`wait_for_lan_broker`]).
static LANE_WAKE: (Mutex<u64>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

/// Wake a Secrets lane blocked in [`wait_for_lan_broker`], so it reads its stop flags now.
pub(super) fn wake_lane_waiters() {
    let (lock, cvar) = &LANE_WAKE;
    if let Ok(mut seq) = lock.lock() {
        *seq = seq.wrapping_add(1);
    }
    cvar.notify_all();
}

/// Why aterm's own LAN traffic cannot go out now, in the root LAN broker's own words (its
/// hello's error, [`aterm_uds::lan::LanError::Unavailable`] where none serves): on macOS every
/// LAN use of aterm's goes through the broker, or while none is installed the allowed direct
/// path (`aterm_uds::lan::direct`). `None` once a hello is answered, and always off macOS.
pub(super) fn lan_unavailable() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        aterm_uds::lan::broker::hello()
            .err()
            .map(|error| error.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// How often [`wait_for_lan_broker`] asks the broker again while nothing wakes it.
const BROKER_LOOK: std::time::Duration = std::time::Duration::from_secs(1);

/// Wait until `broker` says the root LAN broker answers (`None`, [`lan_unavailable`]) or `stop`
/// returns `true`, asking again every [`BROKER_LOOK`] and reading `stop` at once on a wake
/// ([`wake_lane_waiters`]): whether the broker answers. A wait for the broker's socket, never
/// for a person.
pub(super) fn wait_for_lan_broker(
    mut broker: impl FnMut() -> Option<String>,
    mut stop: impl FnMut() -> bool,
) -> bool {
    let (lock, cvar) = &LANE_WAKE;
    loop {
        let seen = lock.lock().ok().map(|guard| *guard);
        if broker().is_none() {
            return true;
        }
        if stop() {
            return false;
        }
        let Ok(guard) = lock.lock() else {
            std::thread::sleep(BROKER_LOOK);
            continue;
        };
        if seen.is_some_and(|before| *guard != before) {
            continue;
        }
        let _ = cvar.wait_timeout(guard, BROKER_LOOK);
    }
}

/// Full Disk Access as the prompt-free probe reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fda {
    /// The probe opened the protected database.
    On,
    /// The probe was refused: not proof the switch is off (a denied probe can
    /// be another identity's), so the row says `Not confirmed`.
    NotConfirmed,
    /// Not measured (outside the app bundle, or the probe is switched off).
    Unknown,
}

/// aterm's switch in the Developer Tools list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DevTools {
    On,
    Off,
    NotListed,
    /// The list is readable only with Full Disk Access.
    NeedsFullDiskAccess,
    Unknown,
}

/// macOS developer mode, and whether this build needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Debugger {
    On,
    /// Off, and setup's one Mac password, which turns it on, is still coming by itself.
    Off,
    /// Off, and that password does not turn it on by itself: a person declined its last ask, the
    /// account's record of it cannot be read, or setup's elevation finished once, so it was turned
    /// off since ([`turned_on_by_itself`], [`off`]).
    OffNotAsked,
    /// A build that is not hardened lets a debugger attach without it.
    NotNeeded(&'static str),
    /// The account is neither an admin nor in `_developer`, so macOS lets it turn neither this nor
    /// aterm's Developer Tools switch on: an administrator does ([`needs_administrator`]).
    NeedsAdmin,
    Unknown,
}

/// What a Debugger or Developer Tools row says on an account that is neither an admin nor a
/// developer ([`Debugger::NeedsAdmin`]): the one command an administrator runs for it.
pub(super) fn needs_administrator() -> String {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let user = aterm_uds::account::user_name(uid).unwrap_or_else(|| uid.to_string());
    format!(
        "Needs an administrator: macOS lets only an admin or a developer turn it on. An admin runs \
         `sudo dseditgroup -o edit -a {user} -t user _developer`, and setup asks again."
    )
}

/// Every grant, read once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Grants {
    pub fda: Fda,
    /// This build's identity survives the next build, so a grant to it lasts.
    pub stable: bool,
    pub devtools: DevTools,
    pub debugger: Debugger,
    /// AXIsProcessTrusted, read only while a sign-out is managed.
    pub accessibility: Option<bool>,
    /// The sign-outs' plan read and found no Messages or FaceTime account to sign out, so nothing
    /// here waits on Accessibility.
    pub no_sign_out_owed: bool,
    /// Why the person opted each grant out, in [`Access::ALL`] order ([`opted_out`]).
    pub opted_out: [Option<&'static str>; Access::ALL.len()],
    /// A person asked for macOS's grants ([`super::note_person_asked_for_access`]). Otherwise a
    /// grant left to a person is on offer: it reads calm and holds nothing back.
    pub acquiring: bool,
}

/// A row's description after a Not now in this approval: a new approval
/// (Permissions turned off and on again) asks again (§6).
pub(super) const NOT_NOW: &str = "You chose Not now. Setting Permissions up again asks again.";

/// The Debugger row's note while developer mode reads off on an account that may turn it on.
pub(super) use aterm_types::macos_setup::DEBUGGER_BY_ELEVATION;

/// Why the person opted each grant out, in [`Access::ALL`] order, as its row says it: what the
/// window published ([`opt_out::get`]; an [`OptOut::WhileOff`] only where `reads_off`), else a
/// Not now in this approval's `records`. Nothing asks for debugger access on its own (setup's
/// one early elevation turns it on, [`DEBUGGER_BY_ELEVATION`]), so a declined Debugger record an
/// earlier aterm's step wrote is no opt-out.
pub(super) fn opted_out(
    published: [Option<OptOut>; Access::ALL.len()],
    records: &[(Access, AccessOutcome)],
    reads_off: [bool; Access::ALL.len()],
) -> [Option<&'static str>; Access::ALL.len()] {
    let mut opted = [None; Access::ALL.len()];
    for (slot, access) in Access::ALL.into_iter().enumerate() {
        let declined = access != Access::Debugger
            && records
                .iter()
                .rev()
                .find_map(|(row, outcome)| (*row == access).then_some(*outcome))
                == Some(AccessOutcome::Declined);
        opted[slot] = published[slot]
            .and_then(|why| why.holds(reads_off[slot]))
            .or(declined.then_some(NOT_NOW));
    }
    opted
}

/// Read every grant without a prompt. Accessibility is read only when `selection`
/// manages a sign-out, the one thing that needs it. `records` are this approval's
/// access steps, for a Not now.
pub(super) fn read(selection: Selection, records: &[(Access, AccessOutcome)]) -> Grants {
    use crate::{DebuggerMember, DebuggerMode, DrClass, FdaState, SelfHardening};
    let privacy = crate::privacy_facts();
    let fda = match privacy.fda {
        _ if privacy.fda_label.refused() => Fda::Unknown,
        FdaState::Granted => Fda::On,
        FdaState::Denied => Fda::NotConfirmed,
        FdaState::Unknown => Fda::Unknown,
    };
    let debugger = crate::debugger_facts();
    let debugger = match (
        debugger.hardening,
        debugger.probe.member,
        debugger.probe.mode,
    ) {
        (SelfHardening::NotHardened, ..) => {
            Debugger::NotNeeded("This aterm is not hardened, so a debugger attaches without it.")
        }
        (_, DebuggerMember::NotMember, _) => Debugger::NeedsAdmin,
        (_, _, DebuggerMode::Enabled) => Debugger::On,
        (_, _, DebuggerMode::Disabled) => off(aterm_types::clt_mark::path().is_some_and(|path| {
            turned_on_by_itself(&aterm_types::clt_mark::record(
                &aterm_types::clt_mark::Live,
                &path,
                super::ledger::now_secs(),
            ))
        })),
        _ => Debugger::Unknown,
    };
    let accessibility = Access::Accessibility
        .needed_for(selection)
        .then(|| {
            aterm_machines::init_accounts::accessibility_ready(&aterm_machines::port::Live::new())
                .ok()
        })
        .flatten();
    // Asked only where the answer could take a need down; a plan that cannot read leaves the row
    // as it reads.
    let no_sign_out_owed = accessibility == Some(false)
        && aterm_machines::init_accounts::accessibility_needed_selected(
            &aterm_machines::port::Live::new(),
            selection,
        ) == Ok(false);
    let mut grants = Grants {
        fda,
        stable: !matches!(privacy.dr, DrClass::Cdhash | DrClass::Unsigned),
        devtools: devtools_listing(),
        debugger,
        accessibility,
        no_sign_out_owed,
        opted_out: [None; Access::ALL.len()],
        acquiring: super::state_root().is_ok_and(|root| {
            super::permission_steps_at(&root) == Ok(super::PermissionSteps::Allowed)
        }),
    };
    grants.opted_out = opted_out(
        Access::ALL.map(opt_out::get),
        records,
        Access::ALL.map(|access| grants.reads_off(access)),
    );
    grants
}

/// Whether setup's one Mac password turns developer mode on by itself, as the account's `record`
/// says: an automatic ask still comes, now or at its next try, and setup's elevation never
/// finished on this account. Once it finished, developer mode off was turned off since, and the
/// automatic asks leave it as it reads (`aterm_machines::init_clt`); while a decline holds, or where
/// the record cannot be read, no ask comes by itself soon.
pub(super) fn turned_on_by_itself(record: &aterm_types::clt_mark::Record) -> bool {
    record.asked.coming() && !record.finished
}

/// Developer mode off, as setup's one Mac password reads (`coming`: it turns it on by itself,
/// [`turned_on_by_itself`]): a calm wait on it while it comes, else the person's.
pub(super) const fn off(coming: bool) -> Debugger {
    if coming {
        Debugger::Off
    } else {
        Debugger::OffNotAsked
    }
}

impl Grants {
    /// Grants a test reads from: nothing granted, developer mode off with its ask coming, nobody
    /// acquiring grants.
    #[cfg(test)]
    pub(super) const fn fixture() -> Self {
        Self {
            fda: Fda::NotConfirmed,
            stable: true,
            devtools: DevTools::NeedsFullDiskAccess,
            debugger: Debugger::Off,
            accessibility: Some(false),
            no_sign_out_owed: false,
            opted_out: [None; Access::ALL.len()],
            acquiring: false,
        }
    }

    /// Whether `access` reads a definite off. Full Disk Access never does (a refused probe reads
    /// `Not confirmed`).
    pub(super) fn reads_off(&self, access: Access) -> bool {
        match access {
            Access::DeveloperTools => self.devtools == DevTools::Off,
            Access::Debugger => matches!(self.debugger, Debugger::Off | Debugger::OffNotAsked),
            Access::Accessibility => self.accessibility == Some(false),
            Access::FullDiskAccess => false,
        }
    }
}

/// The Developer Tools list alone, from the system TCC database: tccd's log,
/// which the doctor also reads, takes seconds and adds nothing a row shows.
fn devtools_listing() -> DevTools {
    use aterm_containment::consent::{self, DevToolListLabel, DevToolListing, ProbeGate};
    let exe = consent::fence_exe();
    let protected = consent::protected_roots(&[]);
    let subject = exe
        .as_deref()
        .and_then(consent::app_bundle_root)
        .and_then(|root| consent::bundle_id_of(&root, &protected))
        .unwrap_or_default();
    let list = consent::probe_devtool_list(ProbeGate::on(), &subject);
    match (list.label, list.listing) {
        (DevToolListLabel::NoFullDiskAccess, _) => DevTools::NeedsFullDiskAccess,
        (_, DevToolListing::On) => DevTools::On,
        (_, DevToolListing::Off) => DevTools::Off,
        (_, DevToolListing::Absent) => DevTools::NotListed,
        (_, DevToolListing::Unknown) => DevTools::Unknown,
    }
}

/// Whether `selection` manages `access`'s row: the sign-outs' Accessibility while they are
/// selected, every other grant while Permissions is.
fn managed(access: Access, selection: Selection) -> bool {
    if access == Access::Accessibility {
        access.needed_for(selection)
    } else {
        selection.contains(Setting::Permissions)
    }
}

/// One Access row as `grants` read it. `selection` decides whether the row is
/// managed: an unmanaged row that differs reads Differs, never Needs you. A grant the person
/// opted out of reads as they chose ([`honor_opt_out`]).
pub(super) fn observation(access: Access, grants: &Grants, selection: Selection) -> Observation {
    let seen = reading(access, grants, selection);
    match opted(grants, access) {
        Some(why) if managed(access, selection) => honor_opt_out(seen, why),
        _ => seen,
    }
}

/// [`observation`] before an opt-out is honored: what the grant reads, offered where nobody
/// asked for it.
fn reading(access: Access, grants: &Grants, selection: Selection) -> Observation {
    let managed = managed(access, selection);
    let person = if managed {
        Standing::NeedsYou
    } else {
        Standing::Differs
    };
    let admin = (grants.debugger == Debugger::NeedsAdmin).then(needs_administrator);
    let (standing, now, note, detail): (Standing, Option<&str>, Option<&str>, Option<&str>) =
        match access {
            Access::FullDiskAccess => match grants.fda {
                Fda::On => (Standing::InPlace, Some("On"), None, None),
                Fda::NotConfirmed if !grants.stable => (
                    Standing::NotNeeded,
                    Some("Not confirmed"),
                    None,
                    Some(
                        "This build's identity changes with every build, so a grant would not \
                         last; a signed aterm asks for it.",
                    ),
                ),
                Fda::NotConfirmed => (
                    person,
                    Some("Not confirmed"),
                    None,
                    Some(
                        "Full Disk Access keeps macOS from interrupting programs you run here \
                         with privacy dialogs. Only you can switch it on, in System Settings.",
                    ),
                ),
                Fda::Unknown => (Standing::Unknown, None, None, None),
            },
            Access::DeveloperTools => match grants.devtools {
                DevTools::On => (Standing::InPlace, Some("On"), None, None),
                // The same as Full Disk Access (§6 step 3): a grant to this build
                // would not survive the next one.
                DevTools::Off | DevTools::NotListed if !grants.stable => (
                    Standing::NotNeeded,
                    Some(if grants.devtools == DevTools::Off {
                        "Off"
                    } else {
                        "Not listed"
                    }),
                    None,
                    Some(
                        "This build's identity changes with every build, so a grant would not \
                         last; a signed aterm asks for it.",
                    ),
                ),
                DevTools::Off => (person, Some("Off"), None, admin.as_deref()),
                DevTools::NotListed => (person, Some("Not listed"), None, admin.as_deref()),
                DevTools::NeedsFullDiskAccess => (
                    Standing::Waiting,
                    None,
                    Some("Readable once Full Disk Access is on"),
                    None,
                ),
                DevTools::Unknown => (Standing::Unknown, None, None, None),
            },
            Access::Debugger => match grants.debugger {
                Debugger::On => (Standing::InPlace, Some("On"), None, None),
                Debugger::Off if managed => (
                    Standing::Waiting,
                    Some("Off"),
                    Some(DEBUGGER_BY_ELEVATION),
                    Some("Turning it on lets debuggers attach without a password."),
                ),
                Debugger::Off => (
                    person,
                    Some("Off"),
                    None,
                    Some("Turning it on lets debuggers attach without a password."),
                ),
                Debugger::OffNotAsked => (
                    person,
                    Some("Off"),
                    managed.then_some(DEBUGGER_NOT_ASKED),
                    Some("Turning it on lets debuggers attach without a password."),
                ),
                Debugger::NotNeeded(why) => (Standing::NotNeeded, None, None, Some(why)),
                Debugger::NeedsAdmin => (person, Some("Off"), None, admin.as_deref()),
                Debugger::Unknown => (Standing::Unknown, None, None, None),
            },
            Access::Accessibility if !managed => (
                Standing::NotNeeded,
                None,
                None,
                Some("Only the Messages and FaceTime sign-outs need it."),
            ),
            Access::Accessibility if grants.no_sign_out_owed => (
                Standing::NotNeeded,
                None,
                None,
                Some("No Messages or FaceTime account here to sign out."),
            ),
            Access::Accessibility => match grants.accessibility {
                Some(true) => (Standing::InPlace, Some("On"), None, None),
                Some(false) => (
                    person,
                    Some("Off"),
                    None,
                    Some("The Messages and FaceTime sign-outs need it to press Sign Out."),
                ),
                None => (Standing::Unknown, None, None, None),
            },
        };
    // Nobody asked for macOS's grants: what only a person can give is offered, never waited on.
    // The sign-outs' Accessibility stays their own stage's, which waits for a person by design,
    // and a grant the person opted out of or refused reads as they chose ([`honor_opt_out`]),
    // never offered again. Developer mode is setup's own elevation's while that comes by itself;
    // once it does not, it is offered as theirs, its note naming their routes, never a Needs you.
    let offered = managed
        && !grants.acquiring
        && access != Access::Accessibility
        && note != Some(DEBUGGER_BY_ELEVATION)
        && opted(grants, access).is_none()
        && matches!(standing, Standing::NeedsYou | Standing::Waiting);
    let (standing, note) = match note {
        _ if !offered => (standing, note),
        Some(DEBUGGER_NOT_ASKED) => (Standing::NotNeeded, note),
        _ => (Standing::NotNeeded, Some(ACCESS_OFFERED)),
    };
    let subject = Subject::Access(access);
    Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: note.map(str::to_owned),
        detail: detail.map(str::to_owned),
        read: aterm_types::macos_setup::Read::Unstamped,
    }
}

/// Why the person opted `access` out, if they did.
fn opted(grants: &Grants, access: Access) -> Option<&'static str> {
    Access::ALL
        .iter()
        .position(|row| *row == access)
        .and_then(|slot| grants.opted_out[slot])
}

/// A grant the person opted out of, honored as the row's wanted state (§4): a
/// definite reading is what they want, so it reads in place; an unread
/// or unconfirmed one is not needed. Either way nothing asks for a person.
fn honor_opt_out(seen: Observation, why: &'static str) -> Observation {
    if matches!(seen.standing, Standing::InPlace | Standing::NotNeeded) {
        return seen;
    }
    let definite = seen
        .now
        .clone()
        .filter(|now| now != "Not confirmed" && seen.standing != Standing::Unknown);
    Observation {
        standing: if definite.is_some() {
            Standing::InPlace
        } else {
            Standing::NotNeeded
        },
        wanted: definite.unwrap_or(seen.wanted),
        note: None,
        detail: Some(why.to_owned()),
        ..seen
    }
}

/// The Permissions stage's rows for the revision `records` belong to (§6). A row is `ok` only on
/// this process's reading: a grant that reads in place (an opt-out's definite reading included)
/// or one this build does not need (an identity that changes with every build, a build that is
/// not hardened). A grant whose reading is not in place yet waits for nobody is [`OFFERED`], its
/// detail opening with that reading: one on offer, one only an administrator turns on, developer
/// mode behind setup's one Mac password while that password comes by itself, developer mode left
/// to the person once it does not ([`DEBUGGER_NOT_ASKED`]), one the person opted out of while it
/// reads `Not confirmed`, and one this revision handed to macOS, whose record says what was
/// attempted and never that the grant is on. One this process cannot read (or reads only with Full Disk
/// Access), opted out of or not, is [`UNREAD`]. The stage completes on all of these
/// ([`super::row_ready`]); any other row waits for the window's lane ([`WAITING`]): the pass
/// parks, never retries it.
pub(super) fn stage_rows(grants: &Grants, records: &[(Access, AccessOutcome)]) -> Vec<Row> {
    let selection = {
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        selection
    };
    [
        Access::FullDiskAccess,
        Access::DeveloperTools,
        Access::Debugger,
    ]
    .into_iter()
    .map(|access| {
        let read = reading(access, grants, selection);
        let seen = observation(access, grants, selection);
        let recorded = records
            .iter()
            .rev()
            .find_map(|(row, outcome)| (*row == access).then_some(*outcome));
        // What this revision's step did, as attempted: a record is no reading.
        let step = recorded.map(|outcome| match outcome {
            AccessOutcome::Handed => "handed to macOS in this revision",
            AccessOutcome::Declined => "declined in this revision",
            AccessOutcome::Granted => "answered in this revision",
        });
        let reading = seen.now.as_deref().unwrap_or("Not read");
        let offered = |why: &str| (OFFERED, format!("{reading}; {why}"));
        let (status, detail) = match seen.standing {
            // An opt-out read as the person's wanted value is in place as they chose.
            Standing::InPlace => match opted(grants, access) {
                Some(why) if seen.wanted != Subject::Access(access).wanted() => {
                    ("ok", format!("as you chose: {why}"))
                }
                _ => ("ok", "on".to_owned()),
            },
            Standing::NotNeeded if seen.note.as_deref() == Some(ACCESS_OFFERED) => {
                offered(step.unwrap_or(ACCESS_OFFERED))
            }
            // Developer mode left to the person, setup's one Mac password not turning it on by
            // itself (a decline, an unreadable record, or a finish it was turned off since): the
            // row says so and names their routes, offered while nobody acquires grants and asked
            // of them while a person does. Either way nothing here takes a step on it, so it is
            // offered, never ok, and the stage completes on it as on a row only an administrator
            // moves.
            Standing::NotNeeded | Standing::NeedsYou
                if seen.note.as_deref() == Some(DEBUGGER_NOT_ASKED) =>
            {
                offered(DEBUGGER_NOT_ASKED)
            }
            // An opt-out honored over a reading that is not definite ([`honor_opt_out`]): nothing
            // asks for it, and nothing read it in place either.
            Standing::NotNeeded if read.standing != Standing::NotNeeded => {
                let why = seen.detail.as_deref().unwrap_or(NOT_NOW);
                match read.now.as_deref() {
                    Some(now) => (OFFERED, format!("{now}; {why}")),
                    None => (
                        UNREAD,
                        format!("{}; {why}", read.note.as_deref().unwrap_or("Not read")),
                    ),
                }
            }
            Standing::NotNeeded => (
                "ok",
                format!(
                    "not needed: {}",
                    seen.detail.as_deref().unwrap_or("this Mac does not use it")
                ),
            ),
            // The list is readable only with Full Disk Access, which nothing here waits on.
            Standing::Waiting if access == Access::DeveloperTools => (
                UNREAD,
                format!(
                    "{}{}",
                    seen.note
                        .as_deref()
                        .unwrap_or("Readable once Full Disk Access is on"),
                    step.map(|step| format!("; {step}")).unwrap_or_default()
                ),
            ),
            // The elevation's, which nothing here waits on.
            Standing::Waiting if seen.note.as_deref() == Some(DEBUGGER_BY_ELEVATION) => {
                offered(DEBUGGER_BY_ELEVATION)
            }
            // The window takes no step on a grant this Mac does not let aterm read, so waiting
            // on one would park the run for good.
            Standing::Unknown => (
                UNREAD,
                format!(
                    "could not be read here, so aterm leaves it: {}{}",
                    seen.detail.as_deref().unwrap_or("macOS did not say"),
                    step.map(|step| format!("; {step}")).unwrap_or_default()
                ),
            ),
            // Only an administrator moves these on this account: the stage completes on the
            // grants the person can give.
            Standing::NeedsYou
                if grants.debugger == Debugger::NeedsAdmin
                    && matches!(access, Access::DeveloperTools | Access::Debugger) =>
            {
                offered(seen.detail.as_deref().unwrap_or("needs an administrator"))
            }
            _ => match step {
                Some(step) => offered(step),
                // Nobody asked for the steps, so none is coming.
                None if !grants.acquiring => offered(ACCESS_OFFERED),
                None => (
                    WAITING,
                    "waits for System State to hand its step to macOS while it is the visible \
                     view"
                        .to_owned(),
                ),
            },
        };
        Row {
            name: access.name().into(),
            status: status.into(),
            detail,
            elapsed_ms: None,
        }
    })
    .collect()
}

/// The access steps the window recorded for `revision` (an access revision).
pub(super) fn records(revision: u64) -> Vec<(Access, AccessOutcome)> {
    aterm_types::dirs::state_dir()
        .map(|dir| access_record::read(&dir, revision))
        .unwrap_or_default()
}

/// The Permissions stage of `revision` (the run's access revision), read live.
pub(super) fn stage(revision: u64) -> Vec<Row> {
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    let records = records(revision);
    stage_rows(&read(selection, &records), &records)
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;
