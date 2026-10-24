// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Access rows of System State (`docs/DESIGN-system-state-2026-10-21.md`
//! §3.3 and §6): what each grant reads, read without a prompt, and when the
//! Permissions stage of one revision is complete.
//!
//! The readings are typed facts the doctor already gathers; nothing here opens
//! System Settings or raises a dialog. The window's access lanes hand each step
//! to macOS and record it in [`access_record`]; the stage only reads that record,
//! so a row waiting on a person never makes it retry.
//!
//! A grant the person opted out of (§4: the `[privacy]` switches and a debugger
//! access turned off, which the window publishes through
//! [`aterm_types::macos_setup::opt_out`], and a Not now in this approval) is
//! honored as the row's wanted state: it reads in place, or not needed when its
//! value is unread, and never asks for a person. A debugger access turned off is
//! said only of a debugger that reads off ([`OptOut::WhileOff`]).

use super::{Row, WAITING};
use aterm_types::macos_setup::opt_out::OptOut;
use aterm_types::macos_setup::{
    Access, AccessOutcome, Observation, Selection, Setting, Standing, Subject, access_record,
    opt_out,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a LAN reach to the ALab vault succeeded in this process: the only
/// evidence an app has that macOS lets it use the local network (§3.3).
static LAN_REACHED: AtomicBool = AtomicBool::new(false);

/// Record that a request to a `.local` address on this LAN was answered.
pub(super) fn lan_reached() {
    LAN_REACHED.store(true, Ordering::Release);
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
    /// A build that is not hardened lets a debugger attach without it, and an
    /// account that is neither an admin nor a developer cannot turn it on.
    NotNeeded(&'static str),
    Unknown,
}

/// Every grant, read once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Grants {
    pub fda: Fda,
    /// This build's identity survives the next build, so a grant to it lasts.
    pub stable: bool,
    pub devtools: DevTools,
    pub debugger: Debugger,
    pub lan_reached: bool,
    /// AXIsProcessTrusted, read only while a sign-out is managed.
    pub accessibility: Option<bool>,
    /// Why the person opted each grant out, in [`Access::ALL`] order ([`opted_out`]).
    pub opted_out: [Option<&'static str>; 5],
}

/// A row's description after a Not now in this approval: a new approval
/// (Permissions turned off and on again) asks again (§6).
pub(super) const NOT_NOW: &str = "You chose Not now. Setting Permissions up again asks again.";

/// The Debugger row's description once this approval's step is declined: Cancel
/// in macOS's dialog, an answer on aterm's Settings ▸ Security, or this Mac's
/// step already over from an earlier approval. aterm's own step asks once per
/// Mac (the window's `privacy-debugger-asked` record), so a new approval does
/// not ask again, and [`NOT_NOW`]'s promise would be false here.
pub(super) const DEBUGGER_ANSWERED: &str = "aterm asks for debugger access once per Mac, and this Mac has answered: \
     setting Permissions up again does not ask again.";

/// Why the person opted each grant out, in [`Access::ALL`] order, as its row
/// says it: what the window published (`published`, [`opt_out::get`]: the
/// config's switches, and a debugger access turned off, which is said only of a
/// grant that reads off, `reads_off`), or a Not now the access record holds for
/// this approval (`records`; the debugger's in [`DEBUGGER_ANSWERED`]'s words).
pub(super) fn opted_out(
    published: [Option<OptOut>; 5],
    records: &[(Access, AccessOutcome)],
    reads_off: [bool; 5],
) -> [Option<&'static str>; 5] {
    let mut opted = [None; 5];
    for (slot, access) in Access::ALL.into_iter().enumerate() {
        let declined = records
            .iter()
            .rev()
            .find_map(|(row, outcome)| (*row == access).then_some(*outcome))
            == Some(AccessOutcome::Declined);
        let not_now = if access == Access::Debugger {
            DEBUGGER_ANSWERED
        } else {
            NOT_NOW
        };
        opted[slot] = published[slot]
            .and_then(|why| why.holds(reads_off[slot]))
            .or(declined.then_some(not_now));
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
            Debugger::NotNeeded("this aterm is not hardened, so a debugger attaches without it")
        }
        (_, DebuggerMember::NotMember, _) => Debugger::NotNeeded(
            "this account is neither an admin nor a developer, so macOS does not let it turn on",
        ),
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
    let mut grants = Grants {
        fda,
        stable: !matches!(privacy.dr, DrClass::Cdhash | DrClass::Unsigned),
        devtools: devtools_listing(),
        debugger,
        lan_reached: LAN_REACHED.load(Ordering::Acquire),
        accessibility,
        opted_out: [None; 5],
    };
    grants.opted_out = opted_out(
        Access::ALL.map(opt_out::get),
        records,
        Access::ALL.map(|access| grants.reads_off(access)),
    );
    grants
}

impl Grants {
    /// Whether `access` reads off: a definite reading, not an unread or
    /// unconfirmed one. Full Disk Access reads `Not confirmed` at most (a
    /// denied probe is no proof the switch is off), and macOS does not tell an
    /// app whether it may use the local network.
    pub(super) fn reads_off(&self, access: Access) -> bool {
        match access {
            Access::DeveloperTools => self.devtools == DevTools::Off,
            Access::Debugger => self.debugger == Debugger::Off,
            Access::Accessibility => self.accessibility == Some(false),
            Access::FullDiskAccess | Access::LocalNetwork => false,
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
                DevTools::Off => (person, Some("Off"), None, None),
                DevTools::NotListed => (person, Some("Not listed"), None, None),
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
                Debugger::Off => (
                    person,
                    Some("Off"),
                    None,
                    Some(
                        "Turning it on lets debuggers attach without a password. macOS asks \
                         for an administrator password in a dialog named SamplingTools.",
                    ),
                ),
                Debugger::NotNeeded(why) => (Standing::NotNeeded, None, None, Some(why)),
                Debugger::Unknown => (Standing::Unknown, None, None, None),
            },
            Access::LocalNetwork if grants.lan_reached => {
                (Standing::InPlace, Some("On"), None, None)
            }
            // macOS does not tell an app whether it may use the local network.
            Access::LocalNetwork => (Standing::Unknown, None, None, None),
            Access::Accessibility if !managed => (
                Standing::NotNeeded,
                None,
                None,
                Some("Only the Messages and FaceTime sign-outs need it."),
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
    let subject = Subject::Access(access);
    let seen = Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
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
/// definite reading is what they want, so it reads in place; an unread or
/// unconfirmed one is not needed. Either way nothing asks for a person.
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

/// The rows the Permissions stage reports for `revision`, complete per §6 once
/// every grant is in place, not needed, waiting on Full Disk Access, unreadable
/// here (no step can be taken on it), or handed, declined or triggered in this
/// revision. A row that is none of these waits for
/// the window's lane or a person ([`WAITING`]): the pass parks, never retries it.
pub(super) fn stage_rows(grants: &Grants, records: &[(Access, AccessOutcome)]) -> Vec<Row> {
    let selection = {
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        selection
    };
    [
        Access::LocalNetwork,
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
            // Full Disk Access, Developer Tools or the debugger rule that this
            // Mac does not let aterm read: the window takes no step on an unread
            // grant, so waiting for one would park the run for good. It counts
            // as complete and the row says it could not be read.
            (Standing::Unknown, None) if access != Access::LocalNetwork => (
                true,
                format!(
                    "could not be read here, so aterm leaves it: {}",
                    seen.detail.as_deref().unwrap_or("macOS did not say")
                ),
            ),
            (_, Some(outcome)) => (
                true,
                match outcome {
                    AccessOutcome::Handed => "handed to macOS in this revision",
                    AccessOutcome::Declined => "declined in this revision",
                    AccessOutcome::Triggered => "macOS's own step started in this revision",
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
            lan_reached: false,
            accessibility: Some(false),
            opted_out: [None; 5],
        }
    }

    fn managed() -> Selection {
        Selection::all()
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
            (Standing::NeedsYou, Some("Off"))
        );
        let lan = observation(Access::LocalNetwork, &g, managed());
        assert_eq!((lan.standing, lan.now), (Standing::Unknown, None));
        let reached = Grants {
            lan_reached: true,
            ..g
        };
        let lan = observation(Access::LocalNetwork, &reached, managed());
        assert_eq!(
            (lan.standing, lan.now.as_deref()),
            (Standing::InPlace, Some("On"))
        );
        for access in Access::ALL {
            assert_eq!(observation(access, &g, managed()).wanted, "On");
        }
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
    fn an_unreadable_grant_never_parks_the_run_but_an_unasked_network_does() {
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
        // Local Network reads Unknown until the window asks: that one waits.
        assert_eq!(row("Local Network").status, WAITING);
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
        assert_eq!(status(&rows, "Local Network"), WAITING);
        assert_eq!(status(&rows, "Debugger access"), WAITING);
        // Developer Tools waits on Full Disk Access: complete for this stage.
        assert_eq!(status(&rows, "Developer Tools"), "ok");
        assert_eq!(
            super::super::SetupNext::of(&rows),
            super::super::SetupNext::Park
        );

        let handed = [
            (Access::LocalNetwork, AccessOutcome::Triggered),
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
        opted.opted_out[3] = Some(NOT_NOW); // Local Network
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
            (Access::LocalNetwork, AccessOutcome::Declined),
            // The latest step for a row decides it.
            (Access::LocalNetwork, AccessOutcome::Triggered),
        ];
        let opted = opted_out([None; 5], &records, [false; 5]);
        assert_eq!(opted[0], None);
        assert_eq!(opted[1], Some(NOT_NOW));
        assert_eq!(opted[3], None);
        // What the window published comes first.
        let switch = "You turned aterm's privacy steps off.";
        let mut published = [None; 5];
        published[1] = Some(OptOut::Always(switch));
        assert_eq!(opted_out(published, &records, [false; 5])[1], Some(switch));
    }

    /// A DEBUGGER TURNED OFF IS SAID ONLY OF A DEBUGGER THAT READS OFF: the
    /// window publishes it from the record at every launch, before any probe
    /// (System State design §4), and the record says what an off reading means,
    /// not what the rule reads now. A debugger aterm turned on reads On and in
    /// place, with no opt-out words; one whose rule could not be read here (the
    /// probe timed out, a build outside the app bundle) reads unknown and says
    /// so; only an Off reading is in place as the person chose.
    #[test]
    fn a_turned_off_debugger_is_said_only_of_an_off_reading() {
        let why = "You turned debugger access off, so aterm leaves it off.";
        let mut published = [None; 5];
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
        let mut published = [None; 5];
        published[2] = Some(OptOut::Always(switch));
        g.opted_out = opted_out(published, &[], Access::ALL.map(|a| g.reads_off(a)));
        let row = observation(Access::Debugger, &g, managed());
        assert_eq!(
            (row.standing, row.detail.as_deref()),
            (Standing::NotNeeded, Some(switch))
        );
    }

    /// THE DEBUGGER'S DECLINED ROW SAYS WHAT IS TRUE OF IT: its step asks once
    /// per Mac, so after a Cancel in macOS's dialog (or an earlier approval's
    /// step) setting Permissions up again does not ask again, and the row never
    /// promises that it does; the other rows' Not now still says a new approval
    /// asks again.
    #[test]
    fn a_declined_debugger_step_never_promises_to_ask_again() {
        let records = [
            (Access::FullDiskAccess, AccessOutcome::Declined),
            (Access::Debugger, AccessOutcome::Declined),
        ];
        let opted = opted_out([None; 5], &records, [false; 5]);
        assert_eq!(opted[0], Some(NOT_NOW));
        assert_eq!(opted[2], Some(DEBUGGER_ANSWERED));
        assert!(!DEBUGGER_ANSWERED.contains("asks again"));
        let mut g = grants();
        g.opted_out = opted;
        let row = observation(Access::Debugger, &g, managed());
        assert_eq!(
            (row.standing, row.now.as_deref(), row.wanted.as_str()),
            (Standing::InPlace, Some("Off"), "Off")
        );
        assert_eq!(row.detail.as_deref(), Some(DEBUGGER_ANSWERED));
    }
}
