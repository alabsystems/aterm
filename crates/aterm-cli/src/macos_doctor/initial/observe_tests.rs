// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::alab_join::JoinPlace;
use aterm_types::macos_setup::BASE_PLAN;

/// Ordinary reads admit each private batch only where the page is still in front. A
/// queued visibility observation and an earlier safe or private batch grant nothing to
/// the next one. An explicit Check retains its existing intent instead.
#[test]
fn ordinary_private_reads_require_current_visibility_at_each_batch() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FRONT: AtomicBool = AtomicBool::new(false);
    fn front() -> bool {
        FRONT.load(Ordering::SeqCst)
    }
    let fda = Subject::Access(Access::FullDiskAccess);
    let safe = vec![
        fda,
        Subject::Setting(Setting::Secrets),
        Subject::Setting(Setting::Repositories),
    ];
    let workstation = vec![
        Subject::Setting(Setting::Dock),
        Subject::Setting(Setting::Messages),
    ];
    let spotlight = vec![Subject::Setting(Setting::Spotlight)];
    let subjects: Vec<_> = safe
        .iter()
        .chain(&workstation)
        .chain(&spotlight)
        .copied()
        .collect();
    for (queued, current, after_safe, after_workstation, check, ws_read, sp_read) in [
        (false, false, None, None, false, false, false),
        (true, false, None, None, false, false, false),
        (true, true, Some(false), None, false, false, false),
        (true, true, None, Some(false), false, true, false),
        (false, false, Some(true), None, false, true, true),
        (false, false, None, None, true, true, true),
    ] {
        FRONT.store(queued, Ordering::SeqCst);
        let admission = if check {
            ObservationRead::ExplicitCheck
        } else {
            ObservationRead::Ordinary { page_front: front }
        };
        FRONT.store(current, Ordering::SeqCst);
        let mut calls = Vec::new();
        let seen = read_batches(&subjects, true, admission, |batch, network| {
            calls.push((batch.to_vec(), network));
            let change = if batch == safe {
                after_safe
            } else if batch == workstation {
                after_workstation
            } else {
                None
            };
            if let Some(front) = change {
                FRONT.store(front, Ordering::SeqCst);
            }
            batch
                .iter()
                .map(|subject| Observation {
                    standing: Standing::InPlace,
                    now: Some("verified current state".into()),
                    ..Observation::unknown(*subject)
                })
                .collect()
        });
        let mut expected = vec![(safe.clone(), true)];
        if ws_read {
            expected.push((workstation.clone(), false));
        }
        if sp_read {
            expected.push((spotlight.clone(), false));
        }
        assert_eq!(
            calls, expected,
            "private readers must not be invoked without admission"
        );
        for subject in &subjects {
            let row = seen.iter().find(|row| row.subject == *subject).unwrap();
            let was_read = safe.contains(subject)
                || (ws_read && workstation.contains(subject))
                || (sp_read && spotlight.contains(subject));
            assert_eq!(
                row.standing,
                if was_read {
                    Standing::InPlace
                } else {
                    Standing::Unknown
                }
            );
            assert_eq!(row.now.is_some(), was_read);
            // A private row nothing read is withheld, not failed: its earlier reading
            // stands, and no failure is said of it.
            assert_eq!(row.is_withheld(), !was_read, "{subject:?}");
        }
        assert_eq!(
            seen.iter().find(|row| row.subject == fda).unwrap().standing,
            Standing::InPlace,
            "a verified FDA row never admits the other-app readers"
        );
    }
    FRONT.store(false, Ordering::SeqCst);
    let seen = read_batches(
        &workstation,
        false,
        ObservationRead::Ordinary { page_front: front },
        |_, _| panic!("a hidden private-only refresh invoked its native reader"),
    );
    assert!(
        seen.iter()
            .all(|row| row.standing == Standing::Unknown && row.now.is_none() && row.is_withheld())
    );
    let attempted = read_batches(
        &workstation,
        false,
        ObservationRead::ExplicitCheck,
        |subjects, _| subjects.iter().copied().map(Observation::unknown).collect(),
    );
    assert_eq!(attempted.len(), workstation.len());
    assert!(
        attempted
            .iter()
            .all(|row| row.standing == Standing::Unknown && !row.is_withheld()),
        "failed attempted reads remain invalidating observations"
    );
}

#[test]
fn an_unwound_batch_retires_only_its_attempted_rows_and_preserves_independent_readings() {
    let safe = Subject::Access(Access::FullDiskAccess);
    let workstation = Subject::Setting(Setting::Dock);
    let spotlight = Subject::Setting(Setting::Spotlight);
    let subjects = [safe, workstation, spotlight];
    for private in [false, true] {
        for failed in subjects {
            let mut calls = Vec::new();
            let rows = read_batches(
                &subjects,
                true,
                ObservationRead::Ordinary {
                    page_front: if private { || true } else { || false },
                },
                |batch, _| {
                    calls.extend_from_slice(batch);
                    assert!(!batch.contains(&failed), "injected native unwind");
                    batch
                        .iter()
                        .map(|subject| Observation {
                            standing: Standing::InPlace,
                            ..Observation::unknown(*subject)
                        })
                        .collect()
                },
            );
            let expected: Vec<_> = subjects
                .into_iter()
                .filter(|subject| private || *subject == safe)
                .collect();
            assert_eq!(calls, expected, "each admitted native reader runs once");
            assert_eq!(rows.len(), subjects.len());
            for subject in subjects {
                let row = rows.iter().find(|row| row.subject == subject).unwrap();
                if !expected.contains(&subject) {
                    // Withheld, not failed and not absent: no evidence either way, and the
                    // row reads unread where no earlier reading stands.
                    assert!(row.is_withheld(), "an unattempted private read is withheld");
                    assert_eq!(
                        (row.standing, row.note.as_deref()),
                        (Standing::Unknown, None)
                    );
                } else {
                    assert!(!row.is_withheld());
                    assert_eq!(
                        row.standing,
                        if subject == failed {
                            Standing::Unknown
                        } else {
                            Standing::InPlace
                        }
                    );
                    assert_eq!(row.note.is_some(), subject == failed);
                }
            }
        }
    }
}

#[test]
fn a_local_batch_unwind_produces_no_repository_observation() {
    let access = Subject::Access(Access::FullDiskAccess);
    let repositories = Subject::Setting(Setting::Repositories);
    let mut called = Vec::new();
    let rows = read_batches(
        &[access, repositories],
        false,
        ObservationRead::ExplicitCheck,
        |batch, _| {
            called.extend_from_slice(batch);
            panic!("injected native unwind");
        },
    );
    assert_eq!(called, [access]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, access);
    assert_eq!(rows[0].standing, Standing::Unknown);
}

#[test]
fn a_restore_refused_before_effects_keeps_the_pending_login_reading() {
    for failure in ["stop", "backup", "activation"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let mut trackpad = Selection::none();
        trackpad.set(Setting::TrackpadScrolling, true);
        write_json(
            &root.join(AT_LOGIN),
            &json!({"profile":PROFILE,"session":"1.0:7","settings":trackpad.bits()}),
        )
        .unwrap();
        let pending = std::fs::read(root.join(AT_LOGIN)).unwrap();
        let _lock = HeldLock::try_take(root).unwrap().unwrap();
        match failure {
            "stop" => std::fs::create_dir(root.join("first-run.json")).unwrap(),
            "backup" => std::fs::write(root.join("original-preferences.json"), b"invalid").unwrap(),
            "activation" => {
                std::fs::write(root.join("pending-refresh.json"), b"invalid").unwrap();
            }
            _ => unreachable!(),
        }
        assert!(super::super::super::tests::restore_without_effects(root).is_err());
        let unchanged = Observation {
            standing: Standing::InPlace,
            now: Some("All on".into()),
            ..Observation::unknown(Subject::Setting(Setting::TrackpadScrolling))
        };
        let seen = at_login(
            unchanged,
            held_at_login_in(root, "1.0:7").contains(Setting::TrackpadScrolling),
        );
        assert_eq!(seen.standing, Standing::AtLogin, "{failure}");
        assert_eq!(std::fs::read(root.join(AT_LOGIN)).unwrap(), pending);
        assert_eq!(held_at_login_in(root, "1.0:8"), Selection::none());
    }
}

#[test]
fn a_change_for_the_next_login_holds_only_in_the_session_that_made_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut caps = Selection::none();
    caps.set(Setting::CapsLock, true);
    write_json(
        &root.path().join(AT_LOGIN),
        &json!({"profile":PROFILE,"session":"1.0:7","settings":caps.bits()}),
    )
    .unwrap();
    assert_eq!(held_at_login_in(root.path(), "1.0:7"), caps);
    assert_eq!(held_at_login_in(root.path(), "1.0:8"), Selection::none());

    let differs = Observation {
        standing: Standing::Differs,
        now: Some("Caps Lock".into()),
        ..Observation::unknown(Subject::Setting(Setting::CapsLock))
    };
    // A row that reads differently since the change (a Restore, a change by
    // hand) says so: only the value the run wrote, read in place, is "at login".
    assert_eq!(at_login(differs.clone(), true), differs);
    let unread = Observation::unknown(Subject::Setting(Setting::CapsLock));
    assert_eq!(at_login(unread.clone(), true), unread);
    let written = Observation {
        standing: Standing::InPlace,
        now: Some("Control".into()),
        ..Observation::unknown(Subject::Setting(Setting::CapsLock))
    };
    let held = at_login(written.clone(), true);
    assert_eq!(held.standing, Standing::AtLogin);
    assert_eq!(held.now.as_deref(), Some("Control"));
    assert_eq!(at_login(written.clone(), false), written);
}

/// A kept row that still differs reads "Changed since aterm set it"; one read in place
/// again, unread, or not kept keeps its reading.
#[test]
fn a_kept_row_that_differs_reads_changed_since() {
    let dock = Subject::Setting(Setting::Dock);
    let differs = Observation {
        standing: Standing::Differs,
        now: Some("Recent apps shown".into()),
        ..Observation::unknown(dock)
    };
    assert_eq!(
        changed_since_note(differs.clone(), true).note.as_deref(),
        Some(aterm_types::macos_setup::CHANGED_SINCE)
    );
    assert_eq!(changed_since_note(differs.clone(), false), differs);
    let in_place = Observation {
        standing: Standing::InPlace,
        ..differs
    };
    assert_eq!(changed_since_note(in_place.clone(), true), in_place);
    let unread = Observation::unknown(dock);
    assert_eq!(changed_since_note(unread.clone(), true), unread);
}

/// The Caps Lock key a run set reads in place at once, its live mappings read as wanted:
/// in effect now, never "at login", though the run's change of its saved mappings is held
/// for the next login. Trackpad scrolling, whose keys macOS loads only then, keeps it.
#[test]
fn only_a_row_with_nothing_live_waits_for_the_login() {
    use aterm_machines::model::{Effect, ItemReport, Outcome};
    let item = |effect: Effect, to: &str| ItemReport {
        id: "item".into(),
        family: "test".into(),
        title: "item".into(),
        outcome: Outcome::Ok,
        from: None,
        to: Some(to.to_owned()),
        why: None,
        revert: None,
        effect,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    };
    let saved = item(Effect::Login, "Caps Lock→Left Control");
    let live = item(Effect::Now, "Caps Lock acts as Control");
    let mut held = Selection::none();
    held.set(Setting::CapsLock, true);
    held.set(Setting::TrackpadScrolling, true);
    let caps = workstation_observation(Setting::CapsLock, &[&saved, &live], held);
    assert_eq!(
        (caps.standing, caps.now.as_deref()),
        (Standing::InPlace, Some("Control"))
    );
    // No live mapping read (no keyboard attached): nothing is in effect before the login.
    let caps = workstation_observation(Setting::CapsLock, &[&saved], held);
    assert_eq!(caps.standing, Standing::AtLogin);
    let gestures = item(Effect::Login, "true");
    let trackpad = workstation_observation(Setting::TrackpadScrolling, &[&gestures], held);
    assert_eq!(
        (trackpad.standing, trackpad.now.as_deref()),
        (Standing::AtLogin, Some("All on"))
    );
    // Outside the session that made the change, each is in place.
    let trackpad =
        workstation_observation(Setting::TrackpadScrolling, &[&gestures], Selection::none());
    assert_eq!(trackpad.standing, Standing::InPlace);
}

#[test]
fn this_login_has_one_readable_session_id() {
    let first = login_session();
    assert!(
        first.is_some(),
        "getaudit_addr and kern.boottime read without privilege"
    );
    assert_eq!(login_session(), first);
}

#[test]
fn the_vault_row_says_what_the_records_hold_in_its_own_words() {
    let words = |read: VaultRead, here: Option<&str>| {
        let seen = vault_observation(&read, Selection::all(), here, false);
        (seen.standing, seen.now.unwrap(), seen.note)
    };
    let local = vault_observation(
        &VaultRead::Member { cache_ready: true },
        Selection::all(),
        None,
        false,
    );
    assert_eq!(local.standing, Standing::InPlace);
    assert_eq!(local.now.as_deref(), Some(local.wanted.as_str()));
    assert!(local.note.is_some());
    assert_eq!(
        words(VaultRead::Member { cache_ready: false }, None),
        (
            Standing::Waiting,
            "Out of sync".into(),
            Some(SYNCING.into())
        )
    );
    let pending = |code: Option<&str>, with_password| VaultRead::Pending {
        approve: code.map(|code| format!("aterm secrets approve {code}")),
        with_password,
    };
    assert_eq!(
        words(pending(None, false), None),
        (
            Standing::Waiting,
            "Awaiting code".into(),
            Some(APPROVAL.into())
        )
    );
    assert_eq!(
        words(pending(Some("abcd-efgh"), false), None),
        (
            Standing::NeedsYou,
            "Awaiting code".into(),
            Some("On an admin Mac: aterm secrets approve abcd-efgh".into())
        )
    );
    assert_eq!(
        words(pending(Some("abcd-efgh"), true), None).2.as_deref(),
        Some("On an admin Mac: aterm secrets approve abcd-efgh, or retype the join password")
    );
    // A refused password needs the person: no note, so the row's fields open for it.
    assert_eq!(
        words(VaultRead::Refused, None),
        (Standing::NeedsYou, WRONG_PASSWORD.into(), None)
    );
    assert_eq!(
        vault_observation(&VaultRead::Refused, Selection::none(), None, false).standing,
        Standing::Differs
    );
    assert_eq!(scope::VaultHere::Absent.note(), Some(scope::NO_VAULT));
    assert_eq!(scope::VaultHere::Unreached.note(), Some(scope::NOT_REACHED));
    assert_eq!(scope::VaultHere::Found.note(), None);
    assert_eq!(scope::VaultHere::Unasked.note(), None);
    assert_eq!(
        words(VaultRead::Unconfigured, None),
        (Standing::Differs, "Not joined".into(), None)
    );
    assert_eq!(
        words(VaultRead::OtherVault, None),
        (Standing::Differs, "Other vault".into(), None)
    );
    for read in [
        VaultRead::Unconfigured,
        VaultRead::OtherVault,
        pending(None, true),
        VaultRead::Asking {
            with_password: true,
            at: JoinPlace::Away,
        },
        VaultRead::Allowed,
        VaultRead::Revoked,
        VaultRead::Dropped,
        VaultRead::Member { cache_ready: false },
        VaultRead::Stopped,
        VaultRead::NotSetUp,
        VaultRead::TurnedOff,
        VaultRead::PendingOff { approve: None },
        VaultRead::Fleeting,
        VaultRead::Refused,
    ] {
        let now = vault_observation(&read, Selection::all(), None, false)
            .now
            .unwrap();
        assert!(now.chars().count() <= 14, "{now}");
    }
}

/// A vault not joined where a server at the ALab vault's address holds none waits on
/// nothing for aterm's own join, whatever else the plan sets up: the row reads Not joined in
/// place, the value it then wants. Where nobody answered, the row waits, calm, never in place,
/// since no join happened and the stage asks again by itself. Either keeps what the probe
/// heard as its note, on the base plan, on it with any one group of the ALab profile, and on
/// the whole profile. A join the person typed a password for waits for that vault with the
/// same note, on the base plan too.
#[test]
fn a_vault_none_answered_for_waits_only_for_a_join_the_person_typed() {
    let mut narrower = BASE_PLAN;
    narrower.set(Setting::Spotlight, false);
    let mut plans = vec![BASE_PLAN, narrower, Selection::all()];
    for setting in Setting::ALL {
        if !BASE_PLAN.contains(setting) {
            let mut beyond = BASE_PLAN;
            beyond.set(setting, true);
            plans.push(beyond);
        }
    }
    let reads = |selection, here, typed| {
        let seen = vault_observation(&VaultRead::Unconfigured, selection, here, typed);
        (seen.standing, seen.now.unwrap(), seen.wanted, seen.note)
    };
    for note in [scope::NO_VAULT, scope::NOT_REACHED] {
        for selection in plans.iter().copied() {
            let settled = reads(selection, Some(note), false);
            if note == scope::NOT_REACHED {
                // Nothing answered: no join happened and the stage asks again by itself, so
                // the row is asked, never in place and never a wait. Before, it read in
                // place, and then waited on every Mac off the ALab LAN for good.
                assert_eq!(
                    settled,
                    (
                        Standing::Asking,
                        "Not joined".into(),
                        Setting::Secrets.wanted().into(),
                        Some(note.into())
                    ),
                    "{note} {selection:?}"
                );
                assert!(!settled.0.settled(), "{note} {selection:?}");
            } else {
                assert_eq!(
                    settled,
                    (
                        Standing::InPlace,
                        "Not joined".into(),
                        "Not joined".into(),
                        Some(note.into())
                    ),
                    "{note} {selection:?}"
                );
                assert!(settled.0.settled(), "{note} {selection:?}");
            }
            assert_eq!(
                reads(selection, Some(note), true),
                (
                    Standing::Waiting,
                    "Not joined".into(),
                    Setting::Secrets.wanted().into(),
                    Some(note.into())
                ),
                "{note} {selection:?}"
            );
        }
        // A row nobody manages makes no claim about this network, typed or not.
        for typed in [false, true] {
            let unmanaged = reads(Selection::none(), Some(note), typed);
            assert_eq!((unmanaged.0, unmanaged.3), (Standing::Differs, None));
        }
    }
    // With no word from the probe (none ran, or it found the vault) the join is a change to
    // make, typed or not.
    for typed in [false, true] {
        let unheard = reads(BASE_PLAN, None, typed);
        assert_eq!((unheard.0, unheard.3), (Standing::Differs, None));
    }
}

/// A vault configuration aterm cannot read is no failure and no reading under way: the row
/// is not read, with one calm note in the page's words and the reason kept as its detail,
/// never on the page.
#[test]
fn an_unreadable_vault_config_reads_calmly_in_page_words() {
    let why = "/Users//m/.config/aterm/secrets/source.toml: Permission denied (os error 13)";
    let observation = vault_unread(why);
    assert_eq!(observation.subject, Subject::Setting(Setting::Secrets));
    assert_eq!(observation.standing, Standing::Unknown);
    assert!(!observation.standing.attention());
    assert_eq!(observation.now, None);
    assert_eq!(observation.note.as_deref(), Some(VAULT_UNREAD));
    assert!(!VAULT_UNREAD.contains('/') && !VAULT_UNREAD.contains("toml"));
    assert_eq!(observation.detail.as_deref(), Some(why));
}

/// Every Secrets read, under the whole profile, the base plan alone and a plan that leaves
/// the vault as is, with and without a vault this network did not answer for, for aterm's own
/// join and for one the person typed a password for.
fn every_vault_observation() -> Vec<(VaultRead, Observation)> {
    let mut out = Vec::new();
    for read in [
        VaultRead::Unconfigured,
        VaultRead::OtherVault,
        VaultRead::Pending {
            approve: None,
            with_password: false,
        },
        VaultRead::Pending {
            approve: None,
            with_password: true,
        },
        VaultRead::Pending {
            approve: Some("aterm secrets approve abcd-efgh".into()),
            with_password: false,
        },
        VaultRead::Pending {
            approve: Some("aterm secrets approve abcd-efgh".into()),
            with_password: true,
        },
        VaultRead::Asking {
            with_password: false,
            at: JoinPlace::Home,
        },
        VaultRead::Asking {
            with_password: true,
            at: JoinPlace::Home,
        },
        VaultRead::Asking {
            with_password: false,
            at: JoinPlace::Away,
        },
        VaultRead::Asking {
            with_password: true,
            at: JoinPlace::Unknown,
        },
        VaultRead::Allowed,
        VaultRead::Member { cache_ready: false },
        VaultRead::Member { cache_ready: true },
        VaultRead::Stopped,
        VaultRead::NotSetUp,
        VaultRead::TurnedOff,
        VaultRead::PendingOff { approve: None },
        VaultRead::PendingOff {
            approve: Some("aterm secrets approve abcd-efgh".into()),
        },
        VaultRead::Fleeting,
        VaultRead::Revoked,
        VaultRead::Dropped,
        VaultRead::Refused,
    ] {
        for selection in [Selection::all(), BASE_PLAN, Selection::none()] {
            for here in [None, scope::VaultHere::Absent.note()] {
                for typed in [false, true] {
                    out.push((
                        read.clone(),
                        vault_observation(&read, selection, here, typed),
                    ));
                }
            }
        }
    }
    out
}

/// Every waiting vault row has a note that says what it waits on (§3.3).
#[test]
fn every_waiting_vault_row_says_what_it_waits_on() {
    let mut waiting = 0;
    for (read, seen) in every_vault_observation() {
        if seen.standing != Standing::Waiting {
            continue;
        }
        waiting += 1;
        let note = seen
            .note
            .unwrap_or_else(|| panic!("{read:?} waits on nothing"));
        assert!(
            !note.trim().is_empty(),
            "{read:?} waits without an explanation"
        );
        assert!(!note.contains('/') && !note.contains('~'), "{note}");
    }
    assert!(waiting >= 4, "the waiting rows were read: {waiting}");
}

/// A pending join whose sync the person turned off reads Sync off in place, with a note that
/// an approval lands only once sync is on.
#[test]
fn a_pending_join_with_sync_off_reads_sync_off_and_says_what_that_means() {
    for (approve, note) in [
        (None, PENDING_OFF.to_owned()),
        (
            Some("aterm secrets approve abcd-efgh".to_owned()),
            "You turned sync off; once it's on, aterm secrets approve abcd-efgh on an admin Mac finishes the join".to_owned(),
        ),
    ] {
        for selection in [Selection::all(), BASE_PLAN, Selection::none()] {
            let seen = vault_observation(
                &VaultRead::PendingOff {
                    approve: approve.clone(),
                },
                selection,
                None,
                false,
            );
            assert_eq!(
                (seen.standing, seen.now.as_deref(), seen.wanted.as_str()),
                (Standing::InPlace, Some(SYNC_OFF), SYNC_OFF)
            );
            assert_eq!(seen.note.as_deref(), Some(note.as_str()));
        }
    }
}

/// A Mac a rotation dropped with no `Remove` (one-mesh E2) needs a person on a row Set up now
/// manages, with the way back as a new Mac, and never reads Not joined, which Set up now's
/// Secrets stage, planning no join for it, would not change.
/// CONTROL: a revoked Mac's row reads Not joined, as before.
#[test]
fn a_dropped_vault_row_needs_a_person_and_says_how_it_comes_back() {
    let seen = vault_observation(&VaultRead::Dropped, Selection::all(), None, false);
    assert_eq!(
        (seen.standing, seen.now.as_deref(), seen.note.as_deref()),
        (Standing::NeedsYou, Some("Dropped"), Some(DROPPED))
    );
    assert_eq!(
        vault_observation(&VaultRead::Dropped, Selection::none(), None, false).standing,
        Standing::Differs
    );
    let revoked = vault_observation(&VaultRead::Revoked, Selection::all(), None, false);
    assert_eq!(
        (revoked.standing, revoked.now.as_deref()),
        (Standing::Differs, Some("Not joined"))
    );
}

/// A member whose subscriber cannot run from a disk image waits for the installed app;
/// a running subscriber there only waits for the sync.
#[test]
fn a_vault_member_run_from_a_disk_image_waits_for_the_installed_app() {
    let image = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    let installed = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    for off in [Subscriber::Stopped, Subscriber::Removed] {
        assert_eq!(member_read(true, off, Some(image)), VaultRead::Fleeting);
        assert_eq!(member_read(false, off, Some(image)), VaultRead::Fleeting);
    }
    assert_eq!(
        member_read(false, Subscriber::Running, Some(image)),
        VaultRead::Member { cache_ready: false }
    );
    assert_eq!(
        member_read(true, Subscriber::Stopped, Some(installed)),
        VaultRead::Stopped
    );
    assert_eq!(
        member_read(true, Subscriber::Stopped, None),
        VaultRead::Stopped
    );
    // The person turned it off: their choice, wherever aterm runs from.
    assert_eq!(
        member_read(true, Subscriber::TurnedOff, Some(image)),
        VaultRead::TurnedOff
    );
    assert_eq!(
        member_read(true, Subscriber::Running, Some(installed)),
        VaultRead::Member { cache_ready: true }
    );
    let seen = vault_observation(&VaultRead::Fleeting, Selection::all(), None, false);
    assert_eq!(
        (seen.standing, seen.now.as_deref(), seen.note.as_deref()),
        (Standing::NeedsYou, Some("Out of sync"), Some(FLEETING))
    );
    // A Secrets group left as is asks nothing of anyone.
    assert_eq!(
        vault_observation(&VaultRead::Fleeting, Selection::none(), None, false).standing,
        Standing::Differs
    );
}

/// A stopped or never-installed subscriber is a change Set up now makes, not a wait; one the
/// person turned off reads Sync off in place, and an unmanaged group gets no note.
#[test]
fn a_vault_member_whose_sync_does_not_run_is_a_change_set_up_now_makes() {
    let installed = Some(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
    let subject = Subject::Setting(Setting::Secrets);
    let words = |read: VaultRead, managed| {
        let selection = if managed {
            Selection::all()
        } else {
            Selection::none()
        };
        let seen = vault_observation(&read, selection, None, false);
        (seen.standing, seen.now.unwrap(), seen.note)
    };
    for written in [true, false] {
        assert_eq!(
            words(member_read(written, Subscriber::Stopped, installed), true),
            (
                Standing::Differs,
                "Not syncing".into(),
                Some(STOPPED.into())
            ),
            "written {written}"
        );
        assert_eq!(
            words(member_read(written, Subscriber::Removed, installed), true),
            (
                Standing::Differs,
                "Not set up".into(),
                Some(NOT_SET_UP.into())
            ),
            "written {written}"
        );
        let off = vault_observation(
            &member_read(written, Subscriber::TurnedOff, installed),
            Selection::all(),
            None,
            false,
        );
        assert_eq!(
            (off.standing, off.now.as_deref(), off.note.as_deref()),
            (Standing::InPlace, Some(SYNC_OFF), Some(TURNED_OFF)),
            "written {written}"
        );
        assert_eq!(
            off.wanted, SYNC_OFF,
            "the person's choice is the wanted state"
        );
        assert_ne!(off.wanted, subject.wanted());
    }
    assert_eq!(
        words(member_read(false, Subscriber::Running, installed), true),
        (
            Standing::Waiting,
            "Out of sync".into(),
            Some(SYNCING.into())
        )
    );
    for off in [Subscriber::Stopped, Subscriber::Removed] {
        let (standing, _, note) = words(member_read(true, off, installed), false);
        assert_eq!((standing, note), (Standing::Differs, None), "{off:?}");
    }
}

/// A join waiting on the ALab vault is admitted automatically: its row waits on the
/// automatic-admission note, naming no person step, no command and no admin
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10: the joiner's words drop "admin"), with
/// sync on or off. Another vault, at a URL or at a path, has no automatic admission to wait
/// for, so its row still names the approve an admin Mac runs. A Mac already let in waits
/// for the vault to record it. Read on a scratch home. NEGATIVE CONTROL: a read that
/// takes no vault for ALab's names the admin's approve on the ALab row; one that takes
/// every vault at a URL for ALab's tells a team vault its join is automatic; one that
/// reads no `Allowed` leaves a let-in Mac at the automatic wait; an approve kept for ALab
/// with sync off names it in the sync-off note.
#[test]
fn a_join_waiting_on_the_alab_vault_is_automatic_and_names_no_admin() {
    use aterm_secrets::client::{Config, Membership, Paths, Status};
    use aterm_secrets::commons::bootstrap::Device;
    use aterm_secrets::members::Allowed;
    use aterm_secrets::transport::Source;
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let alab: [u8; 32] = aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
        .unwrap()
        .try_into()
        .unwrap();
    let waiting = |source: Source, signer: [u8; 32]| Status {
        config: Some(Config {
            source,
            mirrors: Vec::new(),
            name: "alab".into(),
            signer: Some(signer),
            followed: Vec::new(),
            host: "m99".into(),
        }),
        machine: Some((aterm_secrets::age::Recipient([7; 32]), "abcd-efgh".into())),
        membership: Membership::Pending,
        names: Vec::new(),
    };
    let row = |status: &Status| {
        vault_observation(
            &pending_read(&paths, status, || JoinPlace::Home),
            Selection::all(),
            None,
            false,
        )
    };
    let alab_at = || Source::Url(aterm_types::macos_setup::DEFAULT_VAULT_URL.into());
    let asking = row(&waiting(alab_at(), alab));
    let note = asking.note.unwrap();
    assert_eq!(asking.standing, Standing::Waiting, "{note}");
    assert_eq!(
        (asking.now.as_deref(), note.as_str()),
        (
            Some(aterm_types::macos_setup::VAULT_ASKING),
            aterm_types::macos_setup::VAULT_ASKING_NOTE
        ),
    );
    assert!(
        !note.contains("aterm secrets"),
        "no command on the page: {note}"
    );
    assert!(
        !note.contains("admin") && !note.contains("approve"),
        "{note}"
    );
    // CONTROL: another vault, at a URL or at a path, is let in only by an admin Mac's approve
    // of this Mac's code or key, so its row keeps naming it.
    for source in [
        Source::Url("http://vault.example:7484/v1/team".into()),
        Source::Path("/Volumes/team/team.vault".into()),
    ] {
        let team = waiting(source, [9; 32]);
        let approve = team.approve_command().unwrap();
        assert_eq!(
            row(&team).note.as_deref(),
            Some(format!("{APPROVE} {approve}").as_str())
        );
    }
    // A member let it in: it holds that member's `Allowed`.
    let member = Paths::for_home(&home.path().join("member"));
    let device = |paths: &Paths| {
        aterm_secrets::client::machine_identity(paths).unwrap();
        Device::open(paths).unwrap().unwrap()
    };
    let request = device(&paths).request([1; 16], [2; 32]).unwrap();
    let allowed = device(&member)
        .with_seed(|seed| Allowed::sign(request.binding(), "m99", seed))
        .unwrap();
    aterm_secrets::fsx::ensure_private_dir(&paths.dir).unwrap();
    aterm_secrets::fsx::write_atomic(
        &paths.dir.join("allowed"),
        format!("{}\n", allowed.to_text()).as_bytes(),
        0o600,
    )
    .unwrap();
    let let_in = row(&waiting(alab_at(), alab));
    assert_eq!(
        (
            let_in.standing,
            let_in.now.as_deref(),
            let_in.note.as_deref()
        ),
        (
            Standing::Waiting,
            Some(aterm_types::macos_setup::VAULT_ALLOWED),
            Some(ALLOWED)
        )
    );
    // Sync turned off: the ALab join lands once it is on, still with no admin in the words.
    // A scratch home has no launchd job of its own, so this reads no launchd state.
    let off = aterm_secrets::service::subscriber_off_path(&paths.home);
    std::fs::create_dir_all(off.parent().unwrap()).unwrap();
    std::fs::write(&off, b"").unwrap();
    let off = row(&waiting(alab_at(), alab));
    assert_eq!(
        (off.now.as_deref(), off.note.as_deref()),
        (Some(SYNC_OFF), Some(PENDING_OFF))
    );
}

/// The ALab join's note follows where this Mac is, as its network place reads it
/// ([`aterm_secrets::alab_join::JoinPlace::of`], here a scripted place): the note is
/// `asking_note` of that place, the automatic one on the ALab LAN only (a phone hotspot with no
/// fleet book gets the away words, one holding the book the proof's, a plain LAN with no book
/// both). The row waits, calm, in every place: being away alerts nothing.
/// NEGATIVE CONTROL: the Asking arm wording `VAULT_ASKING_NOTE` whatever `at` says gives the
/// hotspot the automatic note, and this test fails at "a hotspot with no book".
#[test]
fn the_alab_joins_note_follows_the_place() {
    use aterm_secrets::alab_join::asking_note;
    const T: u64 = 1_800_000_000_000;
    let home = aterm_netplace::sample::home(T, 3);
    let bookless = |place: aterm_netplace::Place| aterm_netplace::Place {
        known: None,
        ..place
    };
    let lan = aterm_netplace::Place {
        at: aterm_netplace::Where::Network(aterm_netplace::Kind::Lan),
        recognised: None,
        ..home.clone()
    };
    for (place, expected, why) in [
        (home, JoinPlace::Home, "home"),
        (
            bookless(aterm_netplace::sample::hotspot(T, T)),
            JoinPlace::Away,
            "a hotspot with no book",
        ),
        (
            aterm_netplace::sample::hotspot(T, T),
            JoinPlace::Proves,
            "a hotspot, holding the book",
        ),
        (bookless(lan), JoinPlace::Unknown, "a LAN with no book"),
    ] {
        let at = JoinPlace::of(&place);
        assert_eq!(at, expected, "{why}");
        let seen = vault_observation(
            &VaultRead::Asking {
                with_password: false,
                at,
            },
            Selection::all(),
            None,
            false,
        );
        assert_eq!(
            (seen.standing, seen.now.as_deref(), seen.note.as_deref()),
            (
                Standing::Waiting,
                Some(aterm_types::macos_setup::VAULT_ASKING),
                Some(asking_note(expected))
            ),
            "{why}"
        );
    }
}

/// Only a pending join sent with a password offers to retype it, beside the code or the wait.
#[test]
fn a_pending_join_sent_with_a_password_says_it_can_be_retyped() {
    for (read, seen) in every_vault_observation() {
        let (VaultRead::Pending { with_password, .. } | VaultRead::Asking { with_password, .. }) =
            read
        else {
            continue;
        };
        let Some(note) = seen.note else {
            assert_eq!(seen.standing, Standing::Differs, "unmanaged");
            continue;
        };
        assert_eq!(note.contains(RETYPE), with_password, "{note}");
        // The one command a note names is the one an admin Mac runs for this Mac's code.
        assert_eq!(
            note.contains("aterm secrets"),
            note.starts_with(APPROVE),
            "{note}"
        );
    }
    let code = VaultRead::Pending {
        approve: Some("aterm secrets approve abcd-efgh".into()),
        with_password: true,
    };
    assert_eq!(
        vault_observation(&code, Selection::all(), None, false)
            .note
            .as_deref(),
        Some("On an admin Mac: aterm secrets approve abcd-efgh, or retype the join password")
    );
}
