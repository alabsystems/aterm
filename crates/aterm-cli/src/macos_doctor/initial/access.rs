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
use aterm_json::{Value, json};
use aterm_types::macos_setup::opt_out::OptOut;
use aterm_types::macos_setup::{
    Access, AccessOutcome, LOCAL_NETWORK_ASKED, LOCAL_NETWORK_UNTOLD, Observation, Selection,
    Setting, Standing, Subject, access_record, opt_out,
};
use std::path::Path;
use std::sync::Mutex;

/// What a look at the LAN heard of macOS's Local Network switch, the only evidence an app has
/// of it (§3.3): macOS never reports the switch itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LanAnswer {
    /// Something on this LAN answered a request to a `.local` address: the switch is on.
    Reached,
    /// macOS refused aterm a `.local` name (`aterm_secrets::client::refused_by_local_network`):
    /// the person answered Don't Allow, or turned aterm off since.
    Refused,
}

impl LanAnswer {
    const fn word(self) -> &'static str {
        match self {
            Self::Reached => "reached",
            Self::Refused => "refused",
        }
    }

    fn of(word: &str) -> Option<Self> {
        match word {
            "reached" => Some(Self::Reached),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }
}

/// The last answer a LAN look heard in this process, with its time (Unix seconds).
static HEARD: Mutex<Option<(LanAnswer, u64)>> = Mutex::new(None);

/// The record, in aterm's doctor state, of the last answer a LAN look heard and when: a later
/// launch reads Local Network from it, since a joined ALab Mac's launch looks for nothing and a
/// Mac with no ALab vault on its LAN hears no answer to a look.
const KEPT: &str = "local-network.json";

/// Note what a look at the LAN heard, now: for this process, and kept for later launches unless
/// the record holds a later answer. A record that cannot be written leaves later launches what
/// they read before; this process still reads its own answer.
pub(super) fn lan_answered(answer: LanAnswer) {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    if let Ok(mut heard) = HEARD.lock() {
        *heard = Some((answer, at));
    }
    let _kept = super::state_root().and_then(|root| keep_answer(&root, answer, at));
}

/// Keep `answer`, heard at `at`, in `root` unless the record there holds a later one.
fn keep_answer(root: &Path, answer: LanAnswer, at: u64) -> Result<(), String> {
    if kept_answer(root).is_some_and(|(_, kept)| kept > at) {
        return Ok(());
    }
    super::write_json(
        &root.join(KEPT),
        &json!({"profile":super::PROFILE,"answer":answer.word(),"at":at}),
    )
}

/// The answer kept in `root`, and when it was heard; `None` when there is none, or the record
/// cannot be read (a reading that says nothing, never a guess).
fn kept_answer(root: &Path) -> Option<(LanAnswer, u64)> {
    let bytes = std::fs::read(root.join(KEPT)).ok()?;
    let value: Value = aterm_json::from_slice(&bytes).ok()?;
    if value["profile"].as_str() != Some(super::PROFILE) {
        return None;
    }
    Some((
        LanAnswer::of(value["answer"].as_str()?)?,
        value["at"].as_u64()?,
    ))
}

/// The later of this process's last answer and the one kept from earlier launches.
fn last_answer() -> Option<LanAnswer> {
    let heard = HEARD.lock().ok().and_then(|heard| *heard);
    let kept = super::state_root().ok().and_then(|root| kept_answer(&root));
    latest([heard, kept])
}

/// The answer heard last of `answers`, each with when it was heard.
fn latest(answers: [Option<(LanAnswer, u64)>; 2]) -> Option<LanAnswer> {
    answers
        .into_iter()
        .flatten()
        .max_by_key(|(_, at)| *at)
        .map(|(answer, _)| answer)
}

/// Local Network as aterm knows it (§3.3), from the last answer a LAN look heard (`heard`) and
/// this approval's step (`records`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lan {
    /// A look reached the LAN, or the window recorded the grant in this approval.
    On,
    /// macOS refused a look the LAN: the person's answer, which aterm leaves as it is.
    Off,
    /// The window raised macOS's question in this approval, and no answer since says how it
    /// went.
    Asked,
    /// Nothing says.
    Untold,
}

impl Lan {
    pub(super) fn of(heard: Option<LanAnswer>, records: &[(Access, AccessOutcome)]) -> Self {
        let recorded = records
            .iter()
            .rev()
            .find_map(|(row, outcome)| (*row == Access::LocalNetwork).then_some(*outcome));
        match (recorded, heard) {
            (_, Some(LanAnswer::Reached)) => Self::On,
            (_, Some(LanAnswer::Refused)) => Self::Off,
            (Some(AccessOutcome::Granted), None) => Self::On,
            (Some(AccessOutcome::Triggered), None) => Self::Asked,
            _ => Self::Untold,
        }
    }
}

/// The Local Network row's description once macOS refused aterm the LAN: the person's answer,
/// honored as the row's wanted state (§4), so it never asks them again.
pub(super) const LAN_REFUSED: &str =
    "You answered Don't Allow or turned it off, so aterm leaves it off.";

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
    pub lan: Lan,
    /// AXIsProcessTrusted, read only while a sign-out is managed.
    pub accessibility: Option<bool>,
    /// Why the person opted each grant out, in [`Access::ALL`] order ([`opted_out`]).
    pub opted_out: [Option<&'static str>; 5],
}

/// A row's description after a Not now in this approval: a new approval
/// (Permissions turned off and on again) asks again (§6).
pub(super) const NOT_NOW: &str = "You chose Not now. Setting Permissions up again asks again.";

/// The Debugger row's description once its step is declined (Cancel in macOS's dialog, an answer
/// on aterm's Settings ▸ Security, or an earlier approval's step): the step asks once per Mac
/// (the window's `privacy-debugger-asked` record), so [`NOT_NOW`]'s promise would be false.
pub(super) const DEBUGGER_ANSWERED: &str = "aterm asks for debugger access once per Mac, and this Mac has answered: \
     setting Permissions up again does not ask again.";

/// Why the person opted each grant out, in [`Access::ALL`] order, as its row says it: what the
/// window published ([`opt_out::get`]; an [`OptOut::WhileOff`] only where `reads_off`), else a
/// Not now in this approval's `records` (the debugger's in [`DEBUGGER_ANSWERED`]'s words).
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
            Debugger::NotNeeded("This aterm is not hardened, so a debugger attaches without it.")
        }
        (_, DebuggerMember::NotMember, _) => Debugger::NotNeeded(
            "This account is neither an admin nor a developer, so macOS does not let it turn on.",
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
        lan: Lan::of(last_answer(), records),
        accessibility,
        opted_out: [None; 5],
    };
    grants.opted_out = opted_out(
        Access::ALL.map(opt_out::get),
        records,
        Access::ALL.map(|access| grants.reads_off(access)),
    );
    grants.refused_lan();
    grants
}

impl Grants {
    /// Whether `access` reads a definite off. Full Disk Access never does (a refused probe reads
    /// `Not confirmed`); Local Network only where macOS refused aterm the LAN.
    pub(super) fn reads_off(&self, access: Access) -> bool {
        match access {
            Access::DeveloperTools => self.devtools == DevTools::Off,
            Access::Debugger => self.debugger == Debugger::Off,
            Access::Accessibility => self.accessibility == Some(false),
            Access::LocalNetwork => self.lan == Lan::Off,
            Access::FullDiskAccess => false,
        }
    }

    /// A Local Network macOS refused aterm is the person's answer ([`LAN_REFUSED`]), unless
    /// they opted it out otherwise already.
    pub(super) fn refused_lan(&mut self) {
        if self.lan == Lan::Off {
            let slot = Access::ALL
                .iter()
                .position(|row| *row == Access::LocalNetwork)
                .unwrap_or_default();
            self.opted_out[slot].get_or_insert(LAN_REFUSED);
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
            // macOS does not tell an app whether it may use the local network: only a look
            // that reached the LAN, or one macOS refused, says. Otherwise the row is `—` with a
            // calm note, and nothing in this launch reads more of it.
            Access::LocalNetwork => match grants.lan {
                Lan::On => (Standing::InPlace, Some("On"), None, None),
                Lan::Off => (person, Some("Off"), None, None),
                Lan::Asked => (Standing::Untold, None, Some(LOCAL_NETWORK_ASKED), None),
                Lan::Untold => (Standing::Untold, None, Some(LOCAL_NETWORK_UNTOLD), None),
            },
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
/// definite reading is what they want, so it reads in place; an unread,
/// untold or unconfirmed one is not needed. Either way nothing asks for a person.
fn honor_opt_out(seen: Observation, why: &'static str) -> Observation {
    if matches!(seen.standing, Standing::InPlace | Standing::NotNeeded) {
        return seen;
    }
    let definite = seen.now.clone().filter(|now| {
        now != "Not confirmed" && !matches!(seen.standing, Standing::Unknown | Standing::Untold)
    });
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
/// grant is in place, not needed, waiting on Full Disk Access, unreadable here (all but Local
/// Network, which macOS never reports), or has a step recorded. Any other row waits for the
/// window's lane or a person ([`WAITING`]): the pass parks, never retries it.
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
            // The window takes no step on a grant this Mac does not let aterm read, so waiting
            // on one would park the run for good.
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
            lan: Lan::Untold,
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
        assert_eq!((lan.standing, lan.now), (Standing::Untold, None));
        let reached = Grants { lan: Lan::On, ..g };
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
        // Local Network, which nothing told aterm of, waits for the window to ask.
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

    /// The window publishes a debugger opt-out at every launch, before any probe (§4), so it is
    /// said only of an Off reading; an On or unread debugger keeps its own row.
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

    /// The debugger's step asks once per Mac, so its declined row never promises to ask again.
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

    #[test]
    fn local_network_reads_the_last_answer_heard_else_this_approvals_step() {
        use AccessOutcome::{Granted, Triggered};
        use LanAnswer::{Reached, Refused};
        for (heard, step, lan) in [
            (None, None, Lan::Untold),
            (None, Some(Triggered), Lan::Asked),
            (None, Some(Granted), Lan::On),
            (Some(Reached), None, Lan::On),
            (Some(Reached), Some(Triggered), Lan::On),
            (Some(Refused), None, Lan::Off),
            (Some(Refused), Some(Triggered), Lan::Off),
            (Some(Refused), Some(Granted), Lan::Off),
        ] {
            let records: Vec<_> = step
                .into_iter()
                .map(|outcome| (Access::LocalNetwork, outcome))
                .collect();
            assert_eq!(Lan::of(heard, &records), lan, "{heard:?} {step:?}");
        }
        // Another row's step says nothing of the network.
        assert_eq!(
            Lan::of(None, &[(Access::FullDiskAccess, Granted)]),
            Lan::Untold
        );
    }

    #[test]
    fn a_kept_answer_is_read_back_and_only_a_later_one_replaces_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert_eq!(kept_answer(root.path()), None, "nothing kept yet");
        keep_answer(root.path(), LanAnswer::Reached, 100).unwrap();
        assert_eq!(kept_answer(root.path()), Some((LanAnswer::Reached, 100)));
        keep_answer(root.path(), LanAnswer::Refused, 50).unwrap();
        assert_eq!(
            kept_answer(root.path()),
            Some((LanAnswer::Reached, 100)),
            "an earlier answer never replaces a later one"
        );
        keep_answer(root.path(), LanAnswer::Refused, 200).unwrap();
        assert_eq!(kept_answer(root.path()), Some((LanAnswer::Refused, 200)));
        std::fs::write(root.path().join(KEPT), b"{").unwrap();
        assert_eq!(
            kept_answer(root.path()),
            None,
            "an unreadable record says nothing"
        );
        // This process's answer and the kept one: the later decides.
        let reached = Some((LanAnswer::Reached, 10));
        let refused = Some((LanAnswer::Refused, 20));
        assert_eq!(latest([reached, refused]), Some(LanAnswer::Refused));
        assert_eq!(latest([refused, reached]), Some(LanAnswer::Refused));
        assert_eq!(latest([None, reached]), Some(LanAnswer::Reached));
        assert_eq!(latest([None, None]), None);
    }

    /// macOS never tells an app its Local Network switch: the row reads what a look heard, or
    /// `—` with one calm note that settles, never a check under way; a refusal is the person's.
    #[test]
    fn local_network_reads_on_off_or_a_calm_dash_and_never_a_check() {
        let triggered = [(Access::LocalNetwork, AccessOutcome::Triggered)];
        for (lan, records, standing, now, note, stage) in [
            (
                Lan::On,
                &[][..],
                Standing::InPlace,
                Some("On"),
                None,
                "on".to_owned(),
            ),
            (
                Lan::Off,
                &[][..],
                Standing::InPlace,
                Some("Off"),
                None,
                format!("as you chose: {LAN_REFUSED}"),
            ),
            (
                Lan::Asked,
                &triggered[..],
                Standing::Untold,
                None,
                Some(LOCAL_NETWORK_ASKED),
                "macOS's own step started in this revision".to_owned(),
            ),
            (
                Lan::Untold,
                &[][..],
                Standing::Untold,
                None,
                Some(LOCAL_NETWORK_UNTOLD),
                "waits for System State to hand its step to macOS while it is the visible view"
                    .to_owned(),
            ),
        ] {
            let mut g = Grants { lan, ..grants() };
            g.refused_lan();
            let row = observation(Access::LocalNetwork, &g, managed());
            assert_eq!(
                (row.standing, row.now.as_deref(), row.note.as_deref()),
                (standing, now, note),
                "{lan:?}"
            );
            assert!(row.standing.settled(), "{lan:?} settles the page");
            assert_eq!(
                row.detail.as_deref() == Some(LAN_REFUSED),
                lan == Lan::Off,
                "{lan:?}: {:?}",
                row.detail
            );
            let rows = stage_rows(&g, records);
            let row = rows.iter().find(|row| row.name == "Local Network").unwrap();
            assert_eq!(row.detail, stage, "{lan:?}");
        }
    }
}
