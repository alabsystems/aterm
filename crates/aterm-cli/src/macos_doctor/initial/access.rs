// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Access rows of System State (`docs/DESIGN-system-state-2026-10-21.md` §3.3 and §6): what
//! each grant reads, read without a prompt, and when the Permissions stage of one revision is
//! complete. Nothing here opens System Settings or raises a dialog: the window's access lanes hand
//! each step to macOS and record it in [`access_record`], and the stage only reads that record.
//!
//! A grant the person opted out of (§4: a `[privacy]` switch, a debugger access turned off, or a
//! Not now in this approval) is honored as the row's wanted state and never asks for a person.

use super::{Row, WAITING};
use aterm_types::macos_setup::opt_out::OptOut;
use aterm_types::macos_setup::{
    ACCESS_OFFERED, Access, AccessOutcome, Observation, Selection, Setting, Standing, Subject,
    access_record, opt_out,
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
/// LAN use of aterm's goes through the broker. `None` once it answers, and always off macOS.
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
    Off,
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
        (_, _, DebuggerMode::Disabled) => Debugger::Off,
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

impl Grants {
    /// Whether `access` reads a definite off. Full Disk Access never does (a refused probe reads
    /// `Not confirmed`).
    pub(super) fn reads_off(&self, access: Access) -> bool {
        match access {
            Access::DeveloperTools => self.devtools == DevTools::Off,
            Access::Debugger => self.debugger == Debugger::Off,
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

/// One Access row as `grants` read it. `selection` decides whether the row is
/// managed: an unmanaged row that differs reads Differs, never Needs you.
pub(super) fn observation(access: Access, grants: &Grants, selection: Selection) -> Observation {
    let managed = if access == Access::Accessibility {
        access.needed_for(selection)
    } else {
        selection.contains(Setting::Permissions)
    };
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
    // never offered again. Developer mode is setup's own elevation's, never a person's press.
    let (standing, note) = if managed
        && !grants.acquiring
        && access != Access::Accessibility
        && note != Some(DEBUGGER_BY_ELEVATION)
        && opted(grants, access).is_none()
        && matches!(standing, Standing::NeedsYou | Standing::Waiting)
    {
        (Standing::NotNeeded, Some(ACCESS_OFFERED))
    } else {
        (standing, note)
    };
    let subject = Subject::Access(access);
    let seen = Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: note.map(str::to_owned),
        detail: detail.map(str::to_owned),
    };
    match opted(grants, access) {
        Some(why) if managed => honor_opt_out(seen, why),
        _ => seen,
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

/// The Permissions stage's rows for the revision `records` belong to, complete per §6 once every
/// grant is in place, not needed, waiting on Full Disk Access, unreadable here, or has a step
/// recorded. Any other row waits for the
/// window's lane or a person ([`WAITING`]): the pass parks, never retries it.
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
        let seen = observation(access, grants, selection);
        let recorded = records
            .iter()
            .rev()
            .find_map(|(row, outcome)| (*row == access).then_some(*outcome));
        let (done, detail) = match (seen.standing, recorded) {
            // An opt-out read as the person's wanted value is in place as they chose.
            (Standing::InPlace, _) => match opted(grants, access) {
                Some(why) if seen.wanted != Subject::Access(access).wanted() => {
                    (true, format!("as you chose: {why}"))
                }
                _ => (true, "on".to_owned()),
            },
            (Standing::NotNeeded, _) if seen.note.as_deref() == Some(ACCESS_OFFERED) => {
                (true, ACCESS_OFFERED.to_owned())
            }
            (Standing::NotNeeded, _) => (
                true,
                format!(
                    "not needed: {}",
                    seen.detail.as_deref().unwrap_or("this Mac does not use it")
                ),
            ),
            (Standing::Waiting, _) if access == Access::DeveloperTools => {
                (true, "waits on Full Disk Access".to_owned())
            }
            // The elevation's, which nothing here waits on.
            (Standing::Waiting, _) if seen.note.as_deref() == Some(DEBUGGER_BY_ELEVATION) => {
                (true, DEBUGGER_BY_ELEVATION.to_owned())
            }
            // The window takes no step on a grant this Mac does not let aterm read, so waiting
            // on one would park the run for good.
            (Standing::Unknown, None) => (
                true,
                format!(
                    "could not be read here, so aterm leaves it: {}",
                    seen.detail.as_deref().unwrap_or("macOS did not say")
                ),
            ),
            // Only an administrator moves these on this account: answered, and the stage
            // completes on the grants the person can give.
            (Standing::NeedsYou, _)
                if grants.debugger == Debugger::NeedsAdmin
                    && matches!(access, Access::DeveloperTools | Access::Debugger) =>
            {
                (
                    true,
                    seen.detail
                        .as_deref()
                        .unwrap_or("needs an administrator")
                        .to_owned(),
                )
            }
            // Nobody asked for the steps, so none is coming.
            (_, None) if !grants.acquiring => (true, "macOS asks at the first use".to_owned()),
            (_, Some(outcome)) => (
                true,
                match outcome {
                    AccessOutcome::Handed => "handed to macOS in this revision",
                    AccessOutcome::Declined => "declined in this revision",
                    AccessOutcome::Granted => "granted in this revision",
                }
                .to_owned(),
            ),
            _ => (
                false,
                "waits for System State to hand its step to macOS while it is the visible view"
                    .to_owned(),
            ),
        };
        Row {
            name: access.name().into(),
            status: if done { "ok" } else { WAITING }.into(),
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
mod tests {
    use super::*;

    fn grants() -> Grants {
        Grants {
            fda: Fda::NotConfirmed,
            stable: true,
            devtools: DevTools::NeedsFullDiskAccess,
            debugger: Debugger::Off,
            accessibility: Some(false),
            no_sign_out_owed: false,
            opted_out: [None; Access::ALL.len()],
            acquiring: true,
        }
    }

    fn managed() -> Selection {
        Selection::all()
    }

    /// With nobody asking, what only a person can give is on offer and the stage completes: the
    /// run finishes by itself. Developer mode is never on offer: setup's one early Mac password
    /// turns it on, so its row says so and waits on nothing. Negative control: a person's ask
    /// parks on the same person's grants.
    #[test]
    fn a_grant_nobody_asked_for_is_offered_and_completes_the_stage() {
        let offered = Grants {
            acquiring: false,
            ..grants()
        };
        for access in [Access::FullDiskAccess, Access::DeveloperTools] {
            let seen = observation(access, &offered, managed());
            assert_eq!(seen.standing, Standing::NotNeeded, "{access:?}");
            assert_eq!(seen.note.as_deref(), Some(ACCESS_OFFERED), "{access:?}");
        }
        let debugger = observation(Access::Debugger, &offered, managed());
        assert_eq!(debugger.standing, Standing::Waiting);
        assert_eq!(debugger.note.as_deref(), Some(DEBUGGER_BY_ELEVATION));
        assert_eq!(
            observation(Access::Accessibility, &offered, managed()).standing,
            Standing::NeedsYou,
            "the sign-outs' own stage still waits for a person"
        );
        let rows = stage_rows(&offered, &[]);
        assert!(rows.iter().all(|row| row.status == "ok"), "{rows:?}");
        let asked = stage_rows(&grants(), &[]);
        assert!(
            asked.iter().any(|row| row.status == WAITING),
            "CONTROL: {asked:?}"
        );
    }

    /// A grant the person refused or opted out of reads as they chose with nobody asking, never
    /// as offered again. Negative control: the same grant with no opt-out is on offer.
    #[test]
    fn a_grant_the_person_refused_is_not_offered_again() {
        let mut refused = Grants {
            acquiring: false,
            devtools: DevTools::Off,
            ..grants()
        };
        let slot = Access::ALL
            .iter()
            .position(|row| *row == Access::DeveloperTools)
            .unwrap();
        refused.opted_out[slot] = Some(NOT_NOW);
        let seen = observation(Access::DeveloperTools, &refused, managed());
        assert_ne!(seen.note.as_deref(), Some(ACCESS_OFFERED), "{seen:?}");
        assert_eq!(seen.detail.as_deref(), Some(NOT_NOW));
        assert!(matches!(
            seen.standing,
            Standing::InPlace | Standing::NotNeeded
        ));
        refused.opted_out[slot] = None;
        assert_eq!(
            observation(Access::DeveloperTools, &refused, managed())
                .note
                .as_deref(),
            Some(ACCESS_OFFERED),
            "CONTROL: no opt-out, on offer"
        );
    }

    #[test]
    fn each_grant_reads_in_its_rows_words_and_never_guesses() {
        let g = grants();
        let fda = observation(Access::FullDiskAccess, &g, managed());
        assert_eq!(
            (fda.standing, fda.now.as_deref()),
            (Standing::NeedsYou, Some("Not confirmed"))
        );
        let tools = observation(Access::DeveloperTools, &g, managed());
        assert_eq!((tools.standing, tools.now), (Standing::Waiting, None));
        assert_eq!(
            tools.note.as_deref(),
            Some("Readable once Full Disk Access is on")
        );
        let debugger = observation(Access::Debugger, &g, managed());
        assert_eq!(
            (debugger.standing, debugger.now.as_deref()),
            (Standing::Waiting, Some("Off"))
        );
        assert_eq!(debugger.note.as_deref(), Some(DEBUGGER_BY_ELEVATION));
        let unmanaged = observation(Access::Debugger, &g, Selection::none());
        assert_eq!(
            unmanaged.standing,
            Standing::Differs,
            "CONTROL: {unmanaged:?}"
        );
        for access in Access::ALL {
            assert_eq!(observation(access, &g, managed()).wanted, "On");
        }
    }

    /// Accessibility is off and the sign-outs are selected, but their plan found no account: the
    /// page asks for nothing it would not use.
    #[test]
    fn accessibility_is_no_need_while_no_account_waits_to_be_signed_out() {
        let owed = grants();
        assert_eq!(owed.accessibility, Some(false));
        assert_ne!(
            observation(Access::Accessibility, &owed, Selection::all()).standing,
            Standing::NotNeeded,
            "CONTROL: an account to sign out keeps the need"
        );
        let none_owed = Grants {
            no_sign_out_owed: true,
            ..owed
        };
        let row = observation(Access::Accessibility, &none_owed, Selection::all());
        assert_eq!(row.standing, Standing::NotNeeded);
        assert_eq!(
            row.detail.as_deref(),
            Some("No Messages or FaceTime account here to sign out.")
        );
    }

    #[test]
    fn a_row_nobody_manages_differs_and_never_asks_for_a_person() {
        let g = grants();
        let mut none = Selection::none();
        none.set(Setting::Permissions, false);
        assert_eq!(
            observation(Access::FullDiskAccess, &g, none).standing,
            Standing::Differs
        );
        let accessibility = observation(Access::Accessibility, &g, none);
        assert_eq!(accessibility.standing, Standing::NotNeeded);
        let unstable = Grants { stable: false, ..g };
        assert_eq!(
            observation(Access::FullDiskAccess, &unstable, managed()).standing,
            Standing::NotNeeded
        );
        let unhardened = Grants {
            debugger: Debugger::NotNeeded("not hardened"),
            ..g
        };
        assert_eq!(
            observation(Access::Debugger, &unhardened, managed()).standing,
            Standing::NotNeeded
        );
    }

    #[test]
    fn an_unreadable_grant_never_parks_the_run() {
        let unread = Grants {
            fda: Fda::Unknown,
            debugger: Debugger::Unknown,
            ..grants()
        };
        let rows = stage_rows(&unread, &[]);
        let row = |name: &str| rows.iter().find(|row| row.name == name).unwrap();
        for name in ["Full Disk Access", "Debugger access"] {
            assert_eq!(row(name).status, "ok", "{name}: no step can be taken on it");
            assert!(
                row(name).detail.starts_with("could not be read here"),
                "{name}"
            );
        }
    }

    /// An account that is neither an admin nor a developer cannot turn on debugger access or
    /// aterm's Developer Tools switch: both rows need an administrator, name the one command
    /// one runs, and count as answered, so the stage completes on the grants the person gives.
    #[test]
    fn a_standard_account_completes_the_stage_without_the_admin_only_grants() {
        let standard = Grants {
            fda: Fda::On,
            devtools: DevTools::Off,
            debugger: Debugger::NeedsAdmin,
            ..grants()
        };
        let rows = stage_rows(&standard, &[]);
        assert!(rows.iter().all(|row| row.status == "ok"), "{rows:?}");
        for name in ["Developer Tools", "Debugger access"] {
            let row = rows.iter().find(|row| row.name == name).unwrap();
            assert!(
                row.detail.starts_with("Needs an administrator")
                    && row.detail.contains("dseditgroup -o edit -a ")
                    && row.detail.contains(" -t user _developer"),
                "{row:?}"
            );
        }
        for access in [Access::DeveloperTools, Access::Debugger] {
            let seen = observation(access, &standard, managed());
            assert_eq!(seen.standing, Standing::NeedsYou, "{access:?}");
            assert!(seen.detail.unwrap().contains("_developer"));
        }
        // An admin's account that has not turned them on still waits for the window's
        // Developer Tools step; developer mode is the early elevation's, waited on by nothing.
        let admin = Grants {
            debugger: Debugger::Off,
            ..standard
        };
        let rows = stage_rows(&admin, &[]);
        let row = |name: &str| rows.iter().find(|row| row.name == name).unwrap();
        assert_eq!(row("Developer Tools").status, WAITING);
        assert_eq!(row("Debugger access").status, "ok");
        assert_eq!(row("Debugger access").detail, DEBUGGER_BY_ELEVATION);
    }

    #[test]
    fn the_stage_completes_on_what_this_revision_handed_and_parks_otherwise() {
        let g = grants();
        let rows = stage_rows(&g, &[]);
        let status = |rows: &[Row], name: &str| {
            rows.iter()
                .find(|row| row.name == name)
                .map(|row| row.status.clone())
                .unwrap()
        };
        assert_eq!(status(&rows, "Full Disk Access"), WAITING);
        // Setup's one early Mac password turns developer mode on: nothing here waits on it.
        assert_eq!(status(&rows, "Debugger access"), "ok");
        // Developer Tools waits on Full Disk Access: complete for this stage.
        assert_eq!(status(&rows, "Developer Tools"), "ok");
        assert_eq!(
            super::super::SetupNext::of(&rows),
            super::super::SetupNext::Park
        );

        let handed = [
            (Access::FullDiskAccess, AccessOutcome::Handed),
            (Access::Debugger, AccessOutcome::Declined),
        ];
        let rows = stage_rows(&g, &handed);
        assert!(rows.iter().all(|row| row.status == "ok"), "{rows:?}");
        assert_eq!(
            super::super::SetupNext::of(&rows),
            super::super::SetupNext::Done
        );
    }

    #[test]
    fn developer_tools_on_a_build_whose_identity_changes_is_not_needed_like_disk_access() {
        for devtools in [DevTools::Off, DevTools::NotListed] {
            let unstable = Grants {
                stable: false,
                devtools,
                ..grants()
            };
            let tools = observation(Access::DeveloperTools, &unstable, managed());
            assert_eq!(tools.standing, Standing::NotNeeded, "{devtools:?}");
            assert!(tools.detail.is_some());
            let stable = Grants {
                stable: true,
                ..unstable
            };
            assert_eq!(
                observation(Access::DeveloperTools, &stable, managed()).standing,
                Standing::NeedsYou
            );
        }
    }

    #[test]
    fn an_opt_out_is_the_rows_wanted_state_and_never_a_standing_nag() {
        let why = "You turned debugger access off in aterm.";
        let mut opted = grants();
        opted.opted_out[2] = Some(why); // Debugger
        opted.opted_out[0] = Some(NOT_NOW); // Full Disk Access
        // A definite reading is what the person wants: in place, as they chose.
        let debugger = observation(Access::Debugger, &opted, managed());
        assert_eq!(
            (
                debugger.standing,
                debugger.now.as_deref(),
                debugger.wanted.as_str()
            ),
            (Standing::InPlace, Some("Off"), "Off")
        );
        assert_eq!(debugger.detail.as_deref(), Some(why));
        // An unconfirmed one is not claimed either way: not needed.
        let fda = observation(Access::FullDiskAccess, &opted, managed());
        assert_eq!(fda.standing, Standing::NotNeeded);
        assert_eq!(fda.detail.as_deref(), Some(NOT_NOW));
        // A row nobody manages keeps its plain reading.
        let mut none = Selection::none();
        none.set(Setting::Permissions, false);
        assert_eq!(
            observation(Access::Debugger, &opted, none).standing,
            Standing::Differs
        );
        // The stage counts opted-out rows complete and says so.
        let rows = stage_rows(&opted, &[]);
        assert!(rows.iter().all(|row| row.status == "ok"), "{rows:?}");
        let debugger = rows
            .iter()
            .find(|row| row.name == "Debugger access")
            .unwrap();
        assert_eq!(debugger.detail, format!("as you chose: {why}"));
    }

    #[test]
    fn a_not_now_in_this_approvals_record_is_an_opt_out() {
        let records = [
            (Access::FullDiskAccess, AccessOutcome::Handed),
            (Access::DeveloperTools, AccessOutcome::Declined),
            (Access::Accessibility, AccessOutcome::Declined),
            // The latest step for a row decides it.
            (Access::Accessibility, AccessOutcome::Handed),
        ];
        let opted = opted_out([None; 4], &records, [false; 4]);
        assert_eq!(opted[0], None);
        assert_eq!(opted[1], Some(NOT_NOW));
        assert_eq!(opted[3], None);
        // What the window published comes first.
        let switch = "You turned aterm's privacy steps off.";
        let mut published = [None; 4];
        published[1] = Some(OptOut::Always(switch));
        assert_eq!(opted_out(published, &records, [false; 4])[1], Some(switch));
    }

    /// The window publishes a debugger opt-out at every launch, before any probe (§4), so it is
    /// said only of an Off reading; an On or unread debugger keeps its own row.
    #[test]
    fn a_turned_off_debugger_is_said_only_of_an_off_reading() {
        let why = "You turned debugger access off, so aterm leaves it off.";
        let mut published = [None; 4];
        published[2] = Some(OptOut::WhileOff(why));
        for (debugger, standing, now, stage) in [
            (
                Debugger::Off,
                Standing::InPlace,
                Some("Off"),
                format!("as you chose: {why}"),
            ),
            (Debugger::On, Standing::InPlace, Some("On"), "on".to_owned()),
            (
                Debugger::Unknown,
                Standing::Unknown,
                None,
                "could not be read here, so aterm leaves it: macOS did not say".to_owned(),
            ),
        ] {
            let mut g = Grants {
                debugger,
                ..grants()
            };
            g.opted_out = opted_out(published, &[], Access::ALL.map(|a| g.reads_off(a)));
            let row = observation(Access::Debugger, &g, managed());
            assert_eq!(
                (row.standing, row.now.as_deref()),
                (standing, now),
                "{debugger:?}"
            );
            assert_eq!(
                row.detail.as_deref() == Some(why),
                debugger == Debugger::Off,
                "{debugger:?}: {:?}",
                row.detail
            );
            let rows = stage_rows(&g, &[]);
            let row = rows
                .iter()
                .find(|row| row.name == "Debugger access")
                .unwrap();
            assert_eq!(
                (row.status.as_str(), &row.detail),
                ("ok", &stage),
                "{debugger:?}"
            );
        }
        // A `[privacy]` switch off is true of any reading.
        let switch = "You turned aterm's privacy steps off.";
        let mut g = Grants {
            debugger: Debugger::Unknown,
            ..grants()
        };
        let mut published = [None; 4];
        published[2] = Some(OptOut::Always(switch));
        g.opted_out = opted_out(published, &[], Access::ALL.map(|a| g.reads_off(a)));
        let row = observation(Access::Debugger, &g, managed());
        assert_eq!(
            (row.standing, row.detail.as_deref()),
            (Standing::NotNeeded, Some(switch))
        );
    }

    /// Nothing asks for debugger access on its own, so a declined Debugger record an earlier
    /// aterm's step wrote is no opt-out: the row reads as setup's elevation leaves it.
    /// NEGATIVE CONTROL: a declined Full Disk Access record is the person's Not now.
    #[test]
    fn a_declined_debugger_record_is_no_opt_out() {
        let records = [
            (Access::FullDiskAccess, AccessOutcome::Declined),
            (Access::Debugger, AccessOutcome::Declined),
        ];
        let opted = opted_out([None; 4], &records, [false; 4]);
        assert_eq!(opted[0], Some(NOT_NOW));
        assert_eq!(opted[2], None);
        let mut g = grants();
        g.opted_out = opted;
        let row = observation(Access::Debugger, &g, managed());
        assert_ne!(row.standing, Standing::InPlace);
    }
}
