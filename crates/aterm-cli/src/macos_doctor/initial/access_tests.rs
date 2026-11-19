// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn grants() -> Grants {
    Grants {
        // These tests vary legacy privacy grants; the independent setup requirements are
        // confirmed unless a test explicitly supplies unknown or incomplete evidence.
        additional: [
            Access::LocalNetwork,
            Access::NameRule,
            Access::SpotlightScope,
            Access::NetworkAuth,
            Access::NetworkHelper,
        ]
        .map(|access| {
            if access == Access::LocalNetwork {
                return Some(additional::local_network(Ok(Some(false))));
            }
            let mut seen = Observation::unknown(Subject::Access(access));
            seen.standing = Standing::InPlace;
            seen.now = Some(access.wanted().into());
            Some(seen)
        }),
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

/// H16: developer mode off while setup's one Mac password is not coming by itself (a person
/// declined its last ask, or its record cannot be read) is the person's row, naming their
/// routes, never a calm wait on an elevation that will not come: asked of them while a person
/// acquires grants, offered (never a Needs you) while nobody does. The stage completes on it
/// either way, as on a row only an administrator moves, so nothing parks.
/// NEGATIVE CONTROL: while the ask is coming the row waits on it calmly.
#[test]
fn developer_mode_off_with_no_ask_coming_is_the_persons_row() {
    for acquiring in [true, false] {
        let declined = Grants {
            debugger: off(false),
            acquiring,
            ..grants()
        };
        let seen = observation(Access::Debugger, &declined, managed());
        assert_eq!(
            seen.standing,
            if acquiring {
                Standing::NeedsYou
            } else {
                Standing::NotNeeded
            },
            "{acquiring}"
        );
        assert_eq!(
            seen.note.as_deref(),
            Some(DEBUGGER_NOT_ASKED),
            "{acquiring}"
        );
        let rows = stage_rows(&declined, &[]);
        let debugger = rows
            .iter()
            .find(|row| row.name.contains("Debugger") || row.name.contains("debugger"))
            .unwrap_or_else(|| panic!("{rows:?}"));
        // Offered with its reading and the person's routes, never `ok`, never a wait, and
        // never the password that will not come.
        assert_eq!(debugger.status, OFFERED, "{acquiring}: {rows:?}");
        assert_eq!(
            debugger.detail,
            format!("Off; {DEBUGGER_NOT_ASKED}"),
            "{acquiring}"
        );
        let coming = Grants {
            debugger: off(true),
            acquiring,
            ..grants()
        };
        let seen = observation(Access::Debugger, &coming, managed());
        assert_eq!(seen.standing, Standing::Waiting, "{acquiring}");
        assert_eq!(
            seen.note.as_deref(),
            Some(DEBUGGER_BY_ELEVATION),
            "{acquiring}"
        );
    }
    assert!(
        Grants {
            debugger: off(false),
            ..grants()
        }
        .reads_off(Access::Debugger)
    );
}

/// The Debugger row reads the account's record as the automatic asks do: developer mode off
/// waits on setup's password only while an ask comes by itself and setup's elevation never
/// finished; a decline, an unreadable record, or a finish (developer mode turned off since,
/// which the automatic asks leave as it reads) make it the person's.
/// NEGATIVE CONTROL: no record, or an unfinished try with no finish, still turns it on.
#[test]
fn developer_mode_waits_on_the_password_only_while_it_comes_and_never_finished() {
    use aterm_types::clt_mark::{Asked, Record};
    let record = |asked: Asked, finished: bool| Record { asked, finished };
    for (record, comes) in [
        (record(Asked::Available, false), true),
        (record(Asked::Unfinished { due: 1, tries: 1 }, false), true),
        (record(Asked::Asking, false), true),
        (
            record(
                Asked::Declined {
                    due: 1,
                    declines: 1,
                },
                false,
            ),
            false,
        ),
        (
            record(Asked::Unread(std::io::Error::other("bad")), false),
            false,
        ),
        (record(Asked::Done { due: 1 }, true), false),
        (record(Asked::Unfinished { due: 1, tries: 2 }, true), false),
    ] {
        assert_eq!(turned_on_by_itself(&record), comes, "{record:?}");
        let grants = Grants {
            debugger: off(turned_on_by_itself(&record)),
            ..grants()
        };
        let seen = observation(Access::Debugger, &grants, managed());
        assert_eq!(
            seen.standing == Standing::Waiting,
            comes,
            "{record:?}: {seen:?}"
        );
    }
}

/// With nobody asking, what only a person can give is on offer and the stage completes: the
/// run finishes by itself, every row saying it is offered, none `ok`. Developer mode is never
/// on offer: setup's one early Mac password turns it on, so its row says so and waits on
/// nothing. Negative control: a person's ask parks on the same person's grants.
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
    assert!(
        rows.iter()
            .filter(|row| ![
                "Local Network access",
                "Mac naming permission",
                "Spotlight indexing",
                "Network authorization",
                "Network helper"
            ]
            .contains(&row.name.as_str()))
            .all(|row| row.status == OFFERED),
        "{rows:?}"
    );
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Done
    );
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
        assert_eq!(observation(access, &g, managed()).wanted, access.wanted());
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
    let unstable = Grants {
        stable: false,
        ..g.clone()
    };
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
        assert_eq!(
            row(name).status,
            UNREAD,
            "{name}: no step can be taken on it"
        );
        assert!(
            row(name).detail.starts_with("could not be read here"),
            "{name}"
        );
    }
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Done
    );
}

/// An account that is neither an admin nor a developer cannot turn on debugger access or
/// aterm's Developer Tools switch: both rows say their reading, need an administrator, name
/// the one command one runs, and are offered, so the stage completes on the grants the person
/// gives without saying either is on.
#[test]
fn a_standard_account_completes_the_stage_without_the_admin_only_grants() {
    let standard = Grants {
        fda: Fda::On,
        devtools: DevTools::Off,
        debugger: Debugger::NeedsAdmin,
        ..grants()
    };
    let rows = stage_rows(&standard, &[]);
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Done,
        "{rows:?}"
    );
    for name in ["Developer Tools", "Debugger access"] {
        let row = rows.iter().find(|row| row.name == name).unwrap();
        assert_eq!(row.status, OFFERED, "{row:?}");
        assert!(
            row.detail.starts_with("Off; Needs an administrator")
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
    assert_eq!(row("Debugger access").status, OFFERED);
    assert_eq!(
        row("Debugger access").detail,
        format!("Off; {DEBUGGER_BY_ELEVATION}")
    );
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
    assert_eq!(status(&rows, "Debugger access"), OFFERED);
    // Developer Tools reads only with Full Disk Access: complete for this stage, unread.
    assert_eq!(status(&rows, "Developer Tools"), UNREAD);
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Park
    );

    let handed = [
        (Access::FullDiskAccess, AccessOutcome::Handed),
        (Access::Debugger, AccessOutcome::Declined),
    ];
    let rows = stage_rows(&g, &handed);
    for name in ["Full Disk Access", "Developer Tools", "Debugger access"] {
        assert_ne!(status(&rows, name), "ok", "{rows:?}");
    }
    // A recorded handoff is no reading of these grants. Their independent observations in
    // this fixture already confirm them, so the remaining legacy handoff can complete setup.
    for access in [
        Access::NameRule,
        Access::SpotlightScope,
        Access::NetworkAuth,
        Access::NetworkHelper,
    ] {
        assert_eq!(status(&rows, access.name()), "ok", "{rows:?}");
    }
    assert_eq!(status(&rows, "Full Disk Access"), OFFERED);
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Done
    );
}

/// [`Grants`] as [`read`] reads them over `records`: a declined step is the person's Not now.
fn over(g: Grants, records: &[(Access, AccessOutcome)]) -> Grants {
    Grants {
        opted_out: opted_out(
            [None; Access::ALL.len()],
            records,
            Access::ALL.map(|a| g.reads_off(a)),
        ),
        ..g
    }
}

/// A recorded step is no reading: whatever this revision's record says of Full Disk Access
/// (its page came up, the person chose Not now, or it read granted once and a later update,
/// switch or reset took it away), a probe that reads `Not confirmed` keeps the row off `ok`,
/// opens its detail with that reading, never says `granted`, and the stage still completes,
/// waiting for nobody. Developer Tools behind Full Disk Access and developer mode behind
/// setup's one password are not in place either, a Not now of the Developer Tools step
/// included. NEGATIVE CONTROL: every grant reading On is `ok` with the same records.
#[test]
fn a_grant_the_probe_reads_missing_is_never_ok_and_never_parks() {
    let row = |rows: &[Row], name: &str| rows.iter().find(|row| row.name == name).cloned();
    for outcome in [
        AccessOutcome::Granted,
        AccessOutcome::Handed,
        AccessOutcome::Declined,
    ] {
        let records = [(Access::FullDiskAccess, outcome)];
        let g = over(grants(), &records);
        let rows = stage_rows(&g, &records);
        let fda = row(&rows, "Full Disk Access").unwrap();
        assert_ne!(fda.status, "ok", "{outcome:?}: {fda:?}");
        assert!(!fda.detail.contains("granted"), "{outcome:?}: {fda:?}");
        assert!(
            fda.detail.starts_with("Not confirmed"),
            "{outcome:?}: {fda:?}"
        );
        for name in ["Developer Tools", "Debugger access"] {
            let other = row(&rows, name).unwrap();
            assert_ne!(other.status, "ok", "{outcome:?}: {other:?}");
        }
        assert_eq!(
            super::super::SetupNext::of(&rows),
            super::super::SetupNext::Done,
            "{outcome:?}: {rows:?}"
        );
    }
    // Developer Tools, readable only with Full Disk Access, after a Not now of its step (and
    // Full Disk Access's page handed to macOS, so nothing else holds the stage).
    let records = [
        (Access::FullDiskAccess, AccessOutcome::Handed),
        (Access::DeveloperTools, AccessOutcome::Declined),
    ];
    let rows = stage_rows(&over(grants(), &records), &records);
    let tools = row(&rows, "Developer Tools").unwrap();
    assert_eq!(tools.status, UNREAD, "{tools:?}");
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Done,
        "{rows:?}"
    );
    let on = Grants {
        fda: Fda::On,
        devtools: DevTools::On,
        debugger: Debugger::On,
        ..grants()
    };
    for outcome in [
        AccessOutcome::Granted,
        AccessOutcome::Handed,
        AccessOutcome::Declined,
    ] {
        let records = [(Access::FullDiskAccess, outcome)];
        let rows = stage_rows(&over(on.clone(), &records), &records);
        assert!(
            rows.iter()
                .filter(
                    |row| ["Full Disk Access", "Developer Tools", "Debugger access"]
                        .contains(&row.name.as_str())
                )
                .all(|row| row.status == "ok"),
            "CONTROL: {rows:?}"
        );
    }
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
    // The stage counts opted-out rows complete. Only the definite reading is in place: Full
    // Disk Access, `Not confirmed`, is left to the person, and Developer Tools, readable only
    // with Full Disk Access, stays unread.
    let rows = stage_rows(&opted, &[]);
    let status = |name: &str| {
        rows.iter()
            .find(|row| row.name == name)
            .map(|row| row.status.as_str())
    };
    assert_eq!(status("Full Disk Access"), Some(OFFERED), "{rows:?}");
    assert_eq!(status("Debugger access"), Some("ok"), "{rows:?}");
    assert_eq!(status("Developer Tools"), Some(UNREAD), "{rows:?}");
    assert_eq!(
        super::super::SetupNext::of(&rows),
        super::super::SetupNext::Done
    );
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
    let opted = opted_out(
        [None; Access::ALL.len()],
        &records,
        [false; Access::ALL.len()],
    );
    assert_eq!(opted[0], None);
    assert_eq!(opted[1], Some(NOT_NOW));
    assert_eq!(opted[3], None);
    // What the window published comes first.
    let switch = "You turned aterm's privacy steps off.";
    let mut published = [None; Access::ALL.len()];
    published[1] = Some(OptOut::Always(switch));
    assert_eq!(
        opted_out(published, &records, [false; Access::ALL.len()])[1],
        Some(switch)
    );
}

/// The window publishes a debugger opt-out at every launch, before any probe (§4), so it is
/// said only of an Off reading; an On or unread debugger keeps its own row.
#[test]
fn a_turned_off_debugger_is_said_only_of_an_off_reading() {
    let why = "You turned debugger access off, so aterm leaves it off.";
    let mut published = [None; Access::ALL.len()];
    published[2] = Some(OptOut::WhileOff(why));
    for (debugger, standing, now, status, stage) in [
        (
            Debugger::Off,
            Standing::InPlace,
            Some("Off"),
            "ok",
            format!("as you chose: {why}"),
        ),
        (
            Debugger::On,
            Standing::InPlace,
            Some("On"),
            "ok",
            "on".to_owned(),
        ),
        (
            Debugger::Unknown,
            Standing::Unknown,
            None,
            UNREAD,
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
            (status, &stage),
            "{debugger:?}"
        );
    }
    // A `[privacy]` switch off is true of any reading.
    let switch = "You turned aterm's privacy steps off.";
    let mut g = Grants {
        debugger: Debugger::Unknown,
        ..grants()
    };
    let mut published = [None; Access::ALL.len()];
    published[2] = Some(OptOut::Always(switch));
    g.opted_out = opted_out(published, &[], Access::ALL.map(|a| g.reads_off(a)));
    let row = observation(Access::Debugger, &g, managed());
    assert_eq!(
        (row.standing, row.detail.as_deref()),
        (Standing::NotNeeded, Some(switch))
    );
    // Honored, yet unread: the stage never says it is in place.
    let rows = stage_rows(&g, &[]);
    let row = rows
        .iter()
        .find(|row| row.name == "Debugger access")
        .unwrap();
    assert_eq!(row.status, UNREAD, "{row:?}");
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
    let opted = opted_out(
        [None; Access::ALL.len()],
        &records,
        [false; Access::ALL.len()],
    );
    assert_eq!(opted[0], Some(NOT_NOW));
    assert_eq!(opted[2], None);
    let mut g = grants();
    g.opted_out = opted;
    let row = observation(Access::Debugger, &g, managed());
    assert_ne!(row.standing, Standing::InPlace);
}

#[test]
fn init_keeps_network_and_password_requirements_visible_without_opt_in() {
    let mut grants = grants();
    grants.acquiring = false;
    for (permission, now) in [
        (Ok(Some(true)), "Allowed"),
        (Ok(Some(false)), "Denied"),
        (Ok(None), "Not answered"),
        (
            Err("planted app permission read refusal".into()),
            "Not read",
        ),
    ] {
        grants.additional[additional::index(Access::LocalNetwork).unwrap()] =
            Some(additional::local_network(permission));
        let seen = observation(Access::LocalNetwork, &grants, managed());
        assert_eq!(seen.standing, Standing::NotNeeded, "{seen:?}");
        assert_eq!(seen.now.as_deref(), Some(now));
        assert!(!super::managed(Access::LocalNetwork, managed()));
        assert!(seen.detail.as_deref().unwrap().contains("helper"));
        if now == "Not read" {
            assert!(
                seen.detail
                    .as_deref()
                    .unwrap()
                    .contains("planted app permission read refusal")
            );
        }
        assert!(
            stage_rows(&grants, &[])
                .iter()
                .all(|row| row.name != Access::LocalNetwork.name()),
            "raw app consent must never be an automatic setup step"
        );
    }
    for access in [
        Access::NameRule,
        Access::SpotlightScope,
        Access::NetworkAuth,
        Access::NetworkHelper,
    ] {
        let subject = Subject::Access(access);
        let mut required = Observation::unknown(subject);
        required.standing = Standing::NeedsYou;
        required.now = Some("Incomplete".into());
        required.note = Some("Finish this requirement".into());
        grants.additional[additional::index(access).unwrap()] = Some(required);
        assert_eq!(
            observation(access, &grants, managed()).standing,
            Standing::NeedsYou
        );
        assert!(
            stage_rows(&grants, &[])
                .iter()
                .any(|row| row.name == access.name() && row.status != "ok")
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn broker_custody_and_network_membership_are_required_independently_of_app_consent() {
    use aterm_uds::lan::{LanError, PROTOCOL, Unavailable, broker::BrokerHello};
    let hello = || BrokerHello {
        protocol: PROTOCOL,
        build: "fixture".into(),
        direct: None,
    };
    let helper_slot = additional::index(Access::NetworkHelper).unwrap();
    let network_slot = additional::index(Access::NetworkAuth).unwrap();
    let app_slot = additional::index(Access::LocalNetwork).unwrap();
    let ready = || {
        let mut grants = grants();
        grants.fda = Fda::On;
        grants.devtools = DevTools::On;
        grants.debugger = Debugger::On;
        grants.additional[helper_slot] = Some(additional::network_helper(Ok(hello()), || {
            panic!("a current root hello needs no installer observation")
        }));
        grants
    };
    for permission in [Some(false), None, Some(true)] {
        let mut grants = ready();
        grants.additional[app_slot] = Some(additional::local_network(Ok(permission)));
        let rows = stage_rows(&grants, &[]);
        assert!(rows.iter().all(super::super::row_ready), "{rows:?}");
        assert_eq!(
            observation(Access::NetworkHelper, &grants, managed()).standing,
            Standing::InPlace
        );

        // A12's lab identity cannot supply live ALab authorization, even with a working broker.
        let mut auth = Observation::unknown(Subject::Access(Access::NetworkAuth));
        auth.standing = Standing::NeedsYou;
        auth.now = Some("Not joined".into());
        grants.additional[network_slot] = Some(auth);
        let rows = stage_rows(&grants, &[]);
        let auth = rows
            .iter()
            .find(|row| row.name == Access::NetworkAuth.name())
            .unwrap();
        assert_eq!(auth.status, WAITING, "{rows:?}");
        assert!(!super::super::row_ready(auth));
    }

    let direct = BrokerHello {
        direct: Some("AllowedDirect".into()),
        ..hello()
    };
    let mut helpers = vec![
        None,
        Some(additional::network_helper(Ok(direct), || {
            panic!("direct app consent is never root helper custody")
        })),
    ];
    for why in [
        Unavailable::NotInstalled,
        Unavailable::NotRunning,
        Unavailable::SwitchedOff,
        Unavailable::Outdated {
            ours: PROTOCOL,
            theirs: PROTOCOL + 1,
            build: "0.1.0+gfixture".into(),
        },
        Unavailable::Refused("the peer is not root".into()),
        Unavailable::Unchecked("the peer check did not finish".into()),
    ] {
        helpers.push(Some(additional::network_helper(
            Err(LanError::Unavailable(why)),
            || false,
        )));
    }
    helpers.push(Some(additional::network_helper(
        Err(LanError::Unavailable(Unavailable::NotInstalled)),
        || true,
    )));
    for helper in helpers {
        let mut grants = ready();
        // Even an Allowed app grant cannot fill in missing, held, incompatible or non-root proof.
        grants.additional[app_slot] = Some(additional::local_network(Ok(Some(true))));
        grants.additional[helper_slot] = helper;
        let rows = stage_rows(&grants, &[]);
        let helper = rows
            .iter()
            .find(|row| row.name == Access::NetworkHelper.name())
            .unwrap();
        assert_eq!(helper.status, WAITING, "{rows:?}");
        assert!(!super::super::row_ready(helper));
        assert_ne!(
            observation(Access::NetworkHelper, &grants, managed()).standing,
            Standing::InPlace
        );
    }
}

#[test]
fn unread_required_setup_evidence_never_completes_the_permissions_stage() {
    for access in [
        Access::NameRule,
        Access::SpotlightScope,
        Access::NetworkAuth,
        Access::NetworkHelper,
    ] {
        for acquiring in [false, true] {
            let mut grants = grants();
            grants.acquiring = acquiring;
            let mut unread = Observation::unknown(Subject::Access(access));
            unread.note = Some("planted read refusal".into());
            grants.additional[additional::index(access).expect("required reading")] = Some(unread);
            assert_eq!(
                observation(access, &grants, managed()).standing,
                Standing::Unknown,
                "{access:?} retains its actual reading"
            );
            for opted_out in [None, Some(NOT_NOW)] {
                grants.opted_out[Access::ALL.iter().position(|row| *row == access).unwrap()] =
                    opted_out;
                let seen = observation(access, &grants, managed());
                assert_eq!(seen.standing, Standing::Unknown, "{access:?}");
                assert_eq!(seen.note.as_deref(), Some("planted read refusal"));
                let rows = stage_rows(&grants, &[]);
                let row = rows.iter().find(|row| row.name == access.name()).unwrap();
                assert_eq!(row.status, WAITING, "{access:?}: {row:?}");
                assert!(!super::super::row_ready(row), "{access:?}: {row:?}");
                assert!(row.detail.contains("planted read refusal"), "{row:?}");
            }
        }
    }
    // Existing privacy probes can be unreadable to this process without parking setup.
    let rows = stage_rows(
        &Grants {
            fda: Fda::Unknown,
            ..grants()
        },
        &[],
    );
    for access in [Access::FullDiskAccess, Access::DeveloperTools] {
        let row = rows.iter().find(|row| row.name == access.name()).unwrap();
        assert_eq!(row.status, UNREAD, "{row:?}");
        assert!(super::super::row_ready(row), "{row:?}");
    }
}

/// FL-188: the Spotlight indexing row says how FL-127's scope stands, in the words System State
/// shows: Off and the scope in place; Needs your password only where setup's automatic ask owes
/// it; newcomers and folders an apply already failed to keep out read in place, as FL-127's
/// rule has it (nothing automatic waits on them, so nothing here waits or asks), never a password
/// card each time a folder is made, and `aterm init`'s Permissions stage says so in the row's
/// own words, done. No switch governs the row: with Permissions off an owed scope still needs
/// the person.
/// NEGATIVE CONTROL: the same owed reading for Network helper, which Permissions governs, reads
/// Differs with Permissions off.
#[test]
fn spotlight_indexing_asks_only_where_setup_asks_and_no_switch_mutes_it() {
    use aterm_types::macos_setup::spotlight_scope::State;
    use aterm_types::macos_setup::{
        SPOTLIGHT_OFF, SPOTLIGHT_OWED, SPOTLIGHT_SCOPED, spotlight_newcomers, spotlight_refused,
    };
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    let folders: BTreeSet<PathBuf> = ["/Users//a/trust", "/Users//a/aterm"]
        .map(PathBuf::from)
        .into();
    let none = BTreeSet::new();
    let indexed = State::Indexed {
        folders: folders.clone(),
        asks: false,
    };
    let said = |state: &State, asks: bool, refused: &BTreeSet<PathBuf>| {
        let (standing, now, _) = additional::spotlight(state, asks, refused, None);
        (standing, now)
    };
    assert_eq!(
        said(&State::Disabled, true, &none),
        (Standing::InPlace, SPOTLIGHT_OFF.into())
    );
    assert_eq!(
        said(&State::Scoped(9), false, &none),
        (Standing::InPlace, SPOTLIGHT_SCOPED.into())
    );
    assert_eq!(
        said(&indexed, true, &none),
        (Standing::NeedsYou, SPOTLIGHT_OWED.into())
    );
    assert_eq!(
        said(&indexed, false, &none),
        (Standing::InPlace, spotlight_newcomers(2)),
        "a newcomer asks for no password by itself, and nothing waits on it"
    );
    assert_eq!(
        said(&indexed, true, &folders),
        (Standing::InPlace, spotlight_refused(2)),
        "folders an apply already failed to keep out are not asked for again"
    );

    // `aterm init`'s Permissions stage reads a newcomer as done, in the row's own words.
    let (standing, now, note) = additional::spotlight(&indexed, false, &none, None);
    let mut g = grants();
    let index = additional::index(Access::SpotlightScope).expect("an additional requirement");
    let mut seen = Observation::unknown(Subject::Access(Access::SpotlightScope));
    seen.standing = standing;
    seen.now = Some(now.clone());
    seen.note = note;
    g.additional[index] = Some(seen);
    for acquiring in [false, true] {
        let g = Grants {
            acquiring,
            ..g.clone()
        };
        let rows = stage_rows(&g, &[]);
        let row = rows
            .iter()
            .find(|row| row.name == Access::SpotlightScope.name())
            .expect("the stage lists the scope");
        assert_eq!(row.status, "ok", "acquiring {acquiring}: {row:?}");
        assert!(
            row.detail.starts_with(&now)
                && row.detail.contains("Made since setup scoped Spotlight"),
            "{row:?}"
        );
    }

    let mut off = Selection::all();
    off.set(Setting::Permissions, false);
    let owed_row = |access: Access| {
        let mut g = grants();
        let index = additional::index(access).expect("an additional requirement");
        let mut seen = Observation::unknown(Subject::Access(access));
        seen.standing = Standing::NeedsYou;
        seen.now = Some(SPOTLIGHT_OWED.into());
        g.additional[index] = Some(seen);
        observation(access, &g, off).standing
    };
    assert_eq!(owed_row(Access::SpotlightScope), Standing::NeedsYou);
    assert_eq!(
        owed_row(Access::NetworkHelper),
        Standing::Differs,
        "NEGATIVE CONTROL: a row Permissions governs differs with it off"
    );
}

/// SD-96: indexing off by the person's own pause (their record says Paused) reads Paused by you,
/// in place, its note naming where Resume indexing is; the record makes no other reading paused.
/// CONTROLS: indexing off with no record, or with a record of a Resume, reads Off, left as found;
/// a Paused record over a reading that says indexing is on reads that reading's own word.
#[test]
fn spotlight_indexing_reads_paused_by_you_only_off_by_the_persons_pause() {
    use aterm_types::macos_setup::spotlight_scope::{Mark, PauseRecord, State};
    use aterm_types::macos_setup::{SPOTLIGHT_OFF, SPOTLIGHT_PAUSED, SPOTLIGHT_SCOPED};
    use std::collections::BTreeSet;
    let none = BTreeSet::new();
    let paused = PauseRecord {
        mark: Mark::Paused,
        at_unix_ms: 1_763_300_000_000,
    };
    let resumed = PauseRecord {
        mark: Mark::Resumed,
        ..paused
    };
    let (standing, now, note) = additional::spotlight(&State::Disabled, true, &none, Some(&paused));
    assert_eq!(
        (standing, now.as_str()),
        (Standing::InPlace, SPOTLIGHT_PAUSED)
    );
    assert!(
        note.is_some_and(|note| note.contains("Resume indexing")),
        "the note says how to resume"
    );
    let said = |state: &State, record: Option<&PauseRecord>| {
        let (standing, now, _) = additional::spotlight(state, false, &none, record);
        (standing, now)
    };
    assert_eq!(
        said(&State::Disabled, None),
        (Standing::InPlace, SPOTLIGHT_OFF.into()),
        "CONTROL: off, no record"
    );
    assert_eq!(
        said(&State::Disabled, Some(&resumed)),
        (Standing::InPlace, SPOTLIGHT_OFF.into()),
        "CONTROL: off after a resume"
    );
    assert_eq!(
        said(&State::Scoped(9), Some(&paused)),
        (Standing::InPlace, SPOTLIGHT_SCOPED.into()),
        "CONTROL: on, whatever the record says"
    );
}
