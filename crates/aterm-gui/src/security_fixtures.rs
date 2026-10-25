// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Named security postures shared by unit tests and `security-posture` ([`crate::security_posture`]).
//!
//! A posture is everything the Security page draws from: the MACOS ACCESS block, the DEVELOPER
//! TOOLS card and the debugger row. As on the host, one privacy gate (`[privacy]`'s `enabled`
//! and `check`) decides whether any of them was checked, so only `privacy-off` and
//! `privacy-checks-off` say the checks are switched off.

use aterm_containment::consent::{
    DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, DrClass, FdaState, Folder,
    ProbeLabel, ResetSubject, SelfHardening, SpikeEvidence, TccutilPresence,
};
use aterm_containment::{
    DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel, DevToolState,
    FdaScope, devtool_grant,
};

use crate::debugger_access::{DebuggerFacts, InFlightFacts, ObservedMarker, Origin};
use crate::native_settings::{DevToolsFacts, MacosAccess, MacosAccessRow};

/// When tccd last answered for the fixtures' subject, in unix milliseconds.
pub(crate) const ANSWERED_AT_MS: i64 = 1_791_381_501_820;

/// When the fixtures' Developer Tools row last changed, and when debugger access was first seen
/// in its mode, in unix seconds (before [`ANSWERED_AT_MS`]).
const CHANGED_AT_S: i64 = 1_790_000_000;

fn fixture_subject() -> Option<ResetSubject> {
    static SUBJECT: std::sync::OnceLock<Option<ResetSubject>> = std::sync::OnceLock::new();
    SUBJECT
        .get_or_init(|| {
            let scratch = aterm_tempfile::TempDir::new().ok()?;
            let root = scratch.path().join("Fixture.app");
            let contents = root.join("Contents");
            std::fs::create_dir_all(&contents)
                .and_then(|()| {
                    std::fs::write(
                        contents.join("Info.plist"),
                        "<plist><dict><key>CFBundleIdentifier</key>\
                         <string>com.example.fixture</string></dict></plist>",
                    )
                })
                .ok()?;
            ResetSubject::read(&root, &[])
        })
        .clone()
}

/// The Developer Tools list as read whole: aterm's row `listing`, last changed `modified_at_s`,
/// in a list of `rows` apps.
pub(crate) const fn devtools_list(
    listing: DevToolListing,
    label: DevToolListLabel,
    modified_at_s: Option<i64>,
    rows: Option<u32>,
) -> DevToolList {
    DevToolList {
        listing,
        label,
        modified_at_s,
        rows,
    }
}

/// Tccd's newest Developer Tools answer for aterm, `state`, read from its log.
fn devtools_answer(state: DevToolState) -> DevToolProbe {
    DevToolProbe {
        state,
        label: DevToolProbeLabel::Observed,
        requests: Some(1),
        not_allowed: Some(u32::from(state != DevToolState::Allowed)),
        answered_at_ms: Some(ANSWERED_AT_MS),
    }
}

/// The DEVELOPER TOOLS card's facts with the checks on: the `list` and the `probe` joined as the
/// host joins them ([`devtool_grant`]).
fn devtools(list: DevToolList, probe: DevToolProbe) -> DevToolsFacts {
    DevToolsFacts {
        enabled: true,
        probe,
        answer_minutes: probe.answered_at_ms.map(|_| 3),
        list,
        grant: devtool_grant(&list, &probe),
        opened_minutes: None,
        declined: None,
    }
}

/// The debugger row's facts with the checks on, after a settled read of `mode` for an account
/// that is `member`, in an aterm whose hardening reads `hardening`.
pub(crate) const fn debugger(
    mode: DebuggerMode,
    member: DebuggerMember,
    hardening: SelfHardening,
) -> DebuggerFacts {
    DebuggerFacts {
        enabled: true,
        probe: DebuggerProbe {
            mode,
            member,
            label: if matches!(mode, DebuggerMode::Unknown) {
                DebuggerProbeLabel::StatusUnrecognized
            } else {
                DebuggerProbeLabel::StatusOk
            },
        },
        self_hardened: hardening,
        in_flight: None,
        busy: false,
        recorded: None,
        drift_since: None,
        last_outcome: None,
        refusal: None,
    }
}

/// Base posture with healthy anchor, Full Disk Access `fda`, requirement `dr`, and `scope`. The
/// checks are on, as `[privacy]` is by default: with Full Disk Access the Developer Tools list is
/// read (aterm's row on), without it only tccd's newest answer (not on the list); debugger access
/// reads macOS's default (not allowed) for an administrator in a hardened aterm. An unknown `fda`
/// is this build running outside an application bundle, which every check refuses.
pub(crate) fn access(fda: FdaState, dr: DrClass, scope: FdaScope) -> MacosAccess {
    let (probe, devtools, debugger) = match fda {
        FdaState::Granted => (
            ProbeLabel::OpenOk,
            self::devtools(
                devtools_list(
                    DevToolListing::On,
                    DevToolListLabel::Read,
                    Some(CHANGED_AT_S),
                    Some(2),
                ),
                devtools_answer(DevToolState::Allowed),
            ),
            self::debugger(
                DebuggerMode::Disabled,
                DebuggerMember::Admin,
                SelfHardening::Hardened,
            ),
        ),
        FdaState::Denied => (
            ProbeLabel::OpenEperm,
            self::devtools(
                DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
                devtools_answer(DevToolState::Unlisted),
            ),
            self::debugger(
                DebuggerMode::Disabled,
                DebuggerMember::Admin,
                SelfHardening::Hardened,
            ),
        ),
        FdaState::Unknown => (
            ProbeLabel::RefusedOutOfBundle,
            self::devtools(
                DevToolList::refused(DevToolListLabel::RefusedOutOfBundle),
                DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle),
            ),
            DebuggerFacts {
                probe: DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle),
                ..self::debugger(
                    DebuggerMode::Unknown,
                    DebuggerMember::Unknown,
                    SelfHardening::Unknown,
                )
            },
        ),
    };
    MacosAccess {
        enabled: true,
        anchor: aterm_containment::ImageAnchor::Live,
        fda,
        probe,
        dr,
        evidence: SpikeEvidence {
            fda_scope: scope,
            ..SpikeEvidence::UNMEASURED
        },
        bundle_id: fixture_subject(),
        install: "installed",
        running: Some("/Applications/fixture.app/Contents/MacOS/fixture".to_string()),
        sessions_total: 2,
        sessions_adopted: 1,
        covers: Vec::new(),
        uncovered: vec!["file-provider-domains"],
        unmeasured: vec![
            "documents",
            "desktop",
            "downloads",
            "network-volumes",
            "removable-volumes",
            "app-data",
        ],
        tccutil: TccutilPresence::Executable,
        warmup_offered: true,
        warmup_live: false,
        warmup_rows: Vec::new(),
        reset: None,
        claimants: None,
        trash_tool: true,
        retire_live: false,
        retired: None,
        retire_status: None,
        debugger,
        devtools,
        askpass_relay_off: false,
    }
}

/// `[privacy] check = false`: the block is on, but nothing was checked, so its Full Disk Access
/// probe and both cards say the checks are switched off.
fn privacy_checks_off() -> MacosAccess {
    MacosAccess {
        fda: FdaState::Unknown,
        probe: ProbeLabel::RefusedDisabled,
        debugger: DebuggerFacts::default(),
        devtools: DevToolsFacts::default(),
        ..access(FdaState::Unknown, DrClass::Identity, FdaScope::Unknown)
    }
}

/// `[privacy] enabled = false`: nothing was checked, so every block says so, and Ask for Folder
/// Access is not offered.
fn privacy_off() -> MacosAccess {
    MacosAccess {
        enabled: false,
        sessions_adopted: 0,
        warmup_offered: false,
        ..privacy_checks_off()
    }
}

/// The postures that switch the checks off, in [`NAMES`] order: only these say so.
#[cfg(test)]
pub(crate) const CHECKS_OFF: [&str; 2] = ["privacy-checks-off", "privacy-off"];

/// Census with the running copy, an ad-hoc `.rollback` beside it, and a foreign Developer-ID copy.
pub(crate) fn census(running_stable: bool) -> aterm_containment::consent::Claimants {
    use aterm_containment::consent::{Claimant, Claimants, Enumeration, classify_dr};
    let copy = |path: &str, dr_text: &str, signing: &'static str, team: Option<&str>| Claimant {
        path: std::path::PathBuf::from(path),
        dr: classify_dr(dr_text),
        dr_text: dr_text.to_string(),
        signing,
        team: team.map(str::to_string),
        running: path == "/Applications/aterm.app",
        links: Vec::new(),
    };
    let dev_id = |team: &str| {
        format!(
            "designated => identifier \"x\" and anchor apple generic and certificate \
             leaf[subject.OU] = \"{team}\""
        )
    };
    let running = if running_stable {
        copy(
            "/Applications/aterm.app",
            &dev_id("T"),
            "developer-id",
            Some("T"),
        )
    } else {
        copy(
            "/Applications/aterm.app",
            "designated => cdhash H\"00\"",
            "adhoc",
            None,
        )
    };
    Claimants {
        found: vec![
            running,
            copy(
                "/Applications/aterm.app.rollback",
                "designated => cdhash H\"aa\"",
                "adhoc",
                None,
            ),
            copy(
                "/Users//a/Old/aterm.app",
                &dev_id("U"),
                "developer-id",
                Some("U"),
            ),
        ],
        enumeration: Enumeration::Complete,
    }
}

/// Every protected folder's warm-up row read Denied: macOS stopped asking.
pub(crate) fn denied_rows() -> Vec<MacosAccessRow> {
    Folder::ALL
        .iter()
        .map(|folder| (*folder, crate::consent_warmup::WarmupRow::Denied))
        .collect()
}

/// The catalog's names, in the order `security-posture list` says them.
pub(crate) const NAMES: &[&str] = &[
    "granted",
    "denied",
    "denied-folders",
    "rebuild",
    "anchor-fault",
    "other-copies",
    "no-tccutil",
    "no-bundle-id",
    "warmup-live",
    "devtools-unlisted",
    "devtools-off",
    "devtools-still-checks",
    "devtools-checking",
    "devtools-unconfirmed",
    "devtools-declined",
    "devtools-cant-tell",
    "debugger-allowed",
    "debugger-checking",
    "debugger-waiting",
    "debugger-not-member",
    "debugger-unhardened",
    "privacy-checks-off",
    "privacy-off",
];

/// The posture named `name`, or `None` for a name the catalog does not hold.
pub(crate) fn named(name: &str) -> Option<MacosAccess> {
    let granted = || access(FdaState::Granted, DrClass::Identity, FdaScope::Unknown);
    let denied = || access(FdaState::Denied, DrClass::Identity, FdaScope::Unknown);
    let denied_folders = || MacosAccess {
        warmup_rows: denied_rows(),
        ..denied()
    };
    let with_devtools =
        |base: MacosAccess, devtools: DevToolsFacts| MacosAccess { devtools, ..base };
    let with_debugger = |debugger: DebuggerFacts| MacosAccess {
        debugger,
        ..granted()
    };
    let read = |listing, modified_at_s| {
        devtools_list(listing, DevToolListLabel::Read, modified_at_s, Some(2))
    };
    let unlisted = || {
        devtools(
            read(DevToolListing::Absent, None),
            devtools_answer(DevToolState::Unlisted),
        )
    };
    let not_allowed = debugger(
        DebuggerMode::Disabled,
        DebuggerMember::Admin,
        SelfHardening::Hardened,
    );
    let allowed = debugger(
        DebuggerMode::Enabled,
        DebuggerMember::Admin,
        SelfHardening::Hardened,
    );
    Some(match name {
        "granted" => granted(),
        "denied" => denied(),
        "denied-folders" => denied_folders(),
        "rebuild" => MacosAccess {
            dr: DrClass::Cdhash,
            ..denied_folders()
        },
        "anchor-fault" => MacosAccess {
            anchor: aterm_containment::ImageAnchor::Deleted,
            ..granted()
        },
        "other-copies" => MacosAccess {
            claimants: Some(census(true)),
            ..granted()
        },
        "no-tccutil" => MacosAccess {
            tccutil: TccutilPresence::Missing,
            ..denied_folders()
        },
        "no-bundle-id" => MacosAccess {
            bundle_id: None,
            ..denied_folders()
        },
        "warmup-live" => MacosAccess {
            warmup_live: true,
            ..denied()
        },
        "devtools-unlisted" => with_devtools(granted(), unlisted()),
        "devtools-off" => with_devtools(
            granted(),
            devtools(
                read(DevToolListing::Off, Some(CHANGED_AT_S)),
                devtools_answer(DevToolState::Denied),
            ),
        ),
        // Switched on before tccd's newest answer, which did not allow it.
        "devtools-still-checks" => with_devtools(
            granted(),
            devtools(
                read(DevToolListing::On, Some(CHANGED_AT_S)),
                devtools_answer(DevToolState::Denied),
            ),
        ),
        "devtools-checking" => with_devtools(
            granted(),
            devtools(DevToolList::pending(), DevToolProbe::pending()),
        ),
        // Opened in System Settings after tccd's newest answer, with the list unreadable.
        "devtools-unconfirmed" => {
            let base = denied();
            let devtools = DevToolsFacts {
                answer_minutes: Some(45),
                opened_minutes: Some(10),
                ..base.devtools
            };
            with_devtools(base, devtools)
        }
        "devtools-declined" => with_devtools(
            granted(),
            DevToolsFacts {
                declined: Some((
                    CHANGED_AT_S.unsigned_abs(),
                    CHANGED_AT_S.unsigned_abs() + 7 * 86_400,
                )),
                ..unlisted()
            },
        ),
        "devtools-cant-tell" => with_devtools(
            granted(),
            devtools(
                DevToolList::refused(DevToolListLabel::TimedOut),
                DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
            ),
        ),
        "debugger-allowed" => with_debugger(DebuggerFacts {
            recorded: Some(ObservedMarker {
                mode: DebuggerMode::Enabled,
                since: CHANGED_AT_S.unsigned_abs(),
                origin: Origin::Aterm,
            }),
            ..allowed
        }),
        "debugger-checking" => with_debugger(DebuggerFacts {
            probe: DebuggerProbe::pending(),
            busy: true,
            ..not_allowed
        }),
        "debugger-waiting" => with_debugger(DebuggerFacts {
            in_flight: Some(InFlightFacts {
                change: aterm_containment::consent::DebuggerChange::On,
                since: CHANGED_AT_S.unsigned_abs(),
                inherited: false,
            }),
            busy: true,
            ..not_allowed
        }),
        "debugger-not-member" => with_debugger(debugger(
            DebuggerMode::Disabled,
            DebuggerMember::NotMember,
            SelfHardening::Hardened,
        )),
        "debugger-unhardened" => with_debugger(debugger(
            DebuggerMode::Enabled,
            DebuggerMember::Admin,
            SelfHardening::NotHardened,
        )),
        "privacy-checks-off" => privacy_checks_off(),
        "privacy-off" => privacy_off(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_settings::{debugger_row_copy, devtools_card_copy};

    #[test]
    fn every_listed_name_is_a_distinct_posture() {
        let postures: Vec<MacosAccess> = NAMES
            .iter()
            .map(|name| named(name).unwrap_or_else(|| panic!("{name} is in the catalog")))
            .collect();
        for (name, access) in NAMES.iter().zip(&postures) {
            let expected = (*name != "no-bundle-id").then_some("com.example.fixture");
            assert_eq!(
                access.bundle_id.as_ref().map(ResetSubject::as_str),
                expected,
                "{name}: the subject is read from the owned fixture bundle"
            );
        }
        for (i, a) in postures.iter().enumerate() {
            for b in &postures[i + 1..] {
                assert_ne!(a, b, "two names, one posture");
            }
        }
        assert!(named("nonsense").is_none());
    }

    /// ONE PRIVACY GATE (s-24): the host checks the MACOS ACCESS block's Full Disk Access, the
    /// DEVELOPER TOOLS card and the debugger row under one gate (`[privacy]`'s `enabled` and
    /// `check`; a refused gate leaves the probe `RefusedDisabled`), but every posture once paired
    /// a checked block with two cards saying the checks were switched off in aterm.toml. Each
    /// posture now keeps the host's coupling, and only the postures that switch the checks off
    /// (the negative controls) say so.
    #[test]
    fn only_the_switched_off_postures_say_the_checks_are_off() {
        const OFF: &str = "switched off in aterm.toml";
        let mut unchecked = Vec::new();
        for name in NAMES {
            let access = named(name).expect("a catalog name");
            let checked = access.probe != ProbeLabel::RefusedDisabled;
            assert_eq!(
                access.devtools.enabled, checked,
                "{name}: the DEVELOPER TOOLS card is checked with the block's probe"
            );
            assert_eq!(
                access.debugger.enabled, checked,
                "{name}: the debugger row is checked with the block's probe"
            );
            assert!(
                access.enabled || !checked,
                "{name}: nothing is checked with [privacy] switched off"
            );
            assert_eq!(
                devtools_card_copy(&access).state.contains(OFF),
                !checked,
                "{name}: the DEVELOPER TOOLS card"
            );
            assert_eq!(
                debugger_row_copy(&access).headline.contains(OFF),
                !checked,
                "{name}: the debugger row"
            );
            if !checked {
                unchecked.push(*name);
            }
        }
        assert_eq!(
            unchecked, CHECKS_OFF,
            "only the postures that switch the checks off leave them off"
        );
    }
}
