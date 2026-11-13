// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm status`'s judgement of each area, over facts a test writes: who takes each next step,
//! that a step is there exactly when one is due, and that no email reaches the output.

use super::*;
use aterm_converge::{Effect, Explain};

const SECRETS: &str = aterm_types::macos_setup::SECRETS_VAULT_ROW;

fn stage(name: &str, status: &str, detail: &str) -> StageRow {
    StageRow {
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
    }
}

fn setup_with(stages: Vec<StageRow>, finished: bool) -> Setup {
    Setup::Read {
        begun: true,
        finished,
        cancelled: false,
        stages,
        plan: Selection::all(),
        current: None,
        clock: Some(NOW),
    }
}

/// The clock as a test's running aterm answers.
const NOW: u64 = 1_800_000_000;

/// A finished [`setup_with`] whose running aterm answered `current` of the Mac settings at
/// [`NOW`], its plan `plan`.
fn setup_reading(
    stages: Vec<StageRow>,
    plan: Selection,
    current: Option<WorkstationObservation>,
) -> Setup {
    Setup::Read {
        begun: true,
        finished: true,
        cancelled: false,
        stages,
        plan,
        current,
        clock: Some(NOW),
    }
}

/// The running aterm's read-back of `verified` as wanted `ago` seconds before [`NOW`].
fn read_back(verified: Selection, ago: u64) -> aterm_machines::init::ReadBack {
    aterm_machines::init::ReadBack {
        rows: verified,
        read_at: Some(NOW - ago),
        ..aterm_machines::init::ReadBack::default()
    }
}

/// The running aterm's completion with `verified` read as wanted `ago` seconds before [`NOW`].
fn complete(verified: Selection, ago: u64) -> WorkstationObservation {
    WorkstationObservation::Complete(read_back(verified, ago))
}

#[test]
fn a_finished_record_does_not_hide_unfinished_stages() {
    let setup = setup_with(
        vec![stage(
            "GitHub repositories",
            "waiting",
            "Waiting for credentials",
        )],
        true,
    );
    let area = setup_area(&setup).unwrap();
    assert_eq!(area.level, Level::Due);
    assert!(area.says.contains("GitHub repositories"));
}

/// The setup records before aterm.app's first launch: none, the default plan's stages pending.
fn unbegun() -> Setup {
    Setup::Read {
        begun: false,
        finished: false,
        cancelled: false,
        stages: vec![
            stage(SECRETS, "pending", ""),
            stage("GitHub repositories", "pending", ""),
        ],
        plan: Selection::all(),
        current: None,
        clock: None,
    }
}

/// What `sync.clone` reads when `aterm init` owes no clone.
fn none_owed() -> Result<OwedClone, String> {
    Ok(OwedClone::Nothing("aterm init owes no clone".into()))
}

fn row(outcome: Outcome, remedy: Option<Remedy>) -> ItemReport {
    ItemReport {
        id: "pkg.toolset".into(),
        family: "pkg".into(),
        title: "toolset".into(),
        outcome,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: remedy.map(|remedy| Explain {
            why: "members are missing".into(),
            remedy,
        }),
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    }
}

fn who(area: &Area) -> Option<Who> {
    area.next.as_ref().map(|step| step.who)
}

/// A step is there exactly when the area is working, due or unknown, and a working area's step
/// is always aterm's own.
fn consistent(area: &Area) -> bool {
    let wants_step = matches!(area.level, Level::Working | Level::Due | Level::Unknown);
    wants_step == area.next.is_some()
        && (area.level != Level::Working || who(area) == Some(Who::Aterm))
}

/// Every area a sweep of facts produces is [`consistent`]; the checker rejects an `ok` area that
/// carries a step and a working one whose step is a person's (the negative controls).
#[test]
fn a_step_is_there_exactly_when_one_is_due() {
    let setups = [
        Setup::NotHere,
        Setup::Changing,
        Setup::Newer,
        Setup::Unread("torn".into()),
        unbegun(),
        setup_with(vec![stage(SECRETS, "pending", "")], false),
        setup_with(
            vec![stage(SECRETS, "stopped", "Not applied by this run.")],
            true,
        ),
        setup_with(vec![stage("GitHub repositories", "applied", "")], true),
        setup_with(
            vec![stage(
                "GitHub repositories",
                "waiting",
                &format!(
                    "Stopped twice while cloning GitHub repositories; {}",
                    aterm_types::macos_setup::StageRow::PARKED_TAIL
                ),
            )],
            false,
        ),
    ];
    let standings = [
        Standing::Member("m12".into()),
        Standing::Allowed {
            fix: None,
            held: false,
        },
        Standing::Allowed {
            fix: Some("`aterm secrets agent on` starts the one that does".into()),
            held: false,
        },
        Standing::Allowed {
            fix: Some("Sync is off".into()),
            held: true,
        },
        Standing::Asking(JoinPlace::Home),
        Standing::Asking(JoinPlace::Away),
        Standing::Seatless,
        Standing::Dropped("dropped: no seat; …".into()),
        Standing::NotIn,
        Standing::Unknown("unreadable".into()),
    ];
    let vaults = [
        Vault::Unread("torn".into()),
        Vault::Unconfigured,
        Vault::Pending,
        Vault::Plain,
        Vault::Revoked(None),
        Vault::Dropped("dropped: no seat; …".into()),
        Vault::In {
            name: "alab".into(),
            entries: 3,
            follows: Follows::Absent,
            conflicts: Vec::new(),
        },
        Vault::In {
            name: "alab".into(),
            entries: 3,
            follows: Follows::Off,
            conflicts: vec![conflict("[everyone] gh-token", false)],
        },
    ];
    let mut areas = Vec::new();
    for setup in &setups {
        areas.extend(setup_area(setup));
        areas.extend(
            standings
                .iter()
                .map(|standing| membership_area(standing, setup)),
        );
        areas.extend(vaults.iter().map(|vault| vault_area(vault, setup)));
        for (git, token) in [(false, false), (true, false), (true, true)] {
            for owed in [
                none_owed(),
                Ok(OwedClone::Off("off".into())),
                Ok(OwedClone::Held("held".into())),
                Ok(OwedClone::Waiting("waits".into())),
                Ok(OwedClone::Due("due".into())),
                Ok(OwedClone::Person("sign in".into())),
                Err("torn".into()),
            ] {
                areas.push(repos_area(setup, git, token, &owed));
            }
        }
    }
    for check in [
        Ok(FleetCheck::Current),
        Ok(FleetCheck::Due("rekey".into())),
        Ok(FleetCheck::Blocked("another fleet".into())),
        Ok(FleetCheck::Idle(aterm_agent::FleetSync::SwarmOff)),
        Ok(FleetCheck::Idle(aterm_agent::FleetSync::NoCode)),
        Err("no state".into()),
    ] {
        areas.push(fleet_area(&check, "m1 up at 0.159.0; 2 up of 2 members"));
    }
    for clt in [
        Clt::NotHere,
        Clt::Present,
        Clt::Installing,
        Clt::Missing { may_install: true },
        Clt::Missing { may_install: false },
    ] {
        for toolset in [
            Ok(None),
            Ok(Some(row(Outcome::Ok, None))),
            Ok(Some(row(Outcome::Planned, Some(Remedy::Repair)))),
            Ok(Some(row(
                Outcome::Person,
                Some(Remedy::Person {
                    pane: "Privacy".into(),
                    how: "on".into(),
                }),
            ))),
            Ok(Some(row(Outcome::Blocked, Some(Remedy::Network)))),
            Ok(Some(row(Outcome::Blocked, Some(Remedy::None)))),
            Err("torn".into()),
        ] {
            areas.push(tools_area(clt, &toolset));
        }
    }
    areas.push(agents_area(&Agents::default()));
    for area in &areas {
        assert!(consistent(area), "{area:?}");
    }

    let mut stray = Area::ok("fleet", "in");
    stray.next = Some(Step {
        who: Who::Agent,
        act: "x".into(),
    });
    assert!(
        !consistent(&stray),
        "an ok area with a step reads consistent"
    );
    let mut handed = Area::working("fleet", "syncing", "x");
    handed.next = Some(Step {
        who: Who::Person,
        act: "x".into(),
    });
    assert!(
        !consistent(&handed),
        "a working area with a person's step reads consistent"
    );
}

/// A Mac not in ALab is aterm's to join while the setup still runs its Secrets vault step, and
/// an agent's (`aterm members join`) once it does not; a person only off the ALab LAN.
#[test]
fn membership_names_a_person_only_where_only_a_person_lets_the_mac_in() {
    let joining = setup_with(vec![stage(SECRETS, "running", "")], false);
    let done = setup_with(vec![stage(SECRETS, "applied", "")], true);
    assert_eq!(
        who(&membership_area(&Standing::NotIn, &joining)),
        Some(Who::Aterm)
    );
    let after = membership_area(&Standing::NotIn, &done);
    assert_eq!(who(&after), Some(Who::Agent));
    assert!(after.next.unwrap().act.contains("aterm members join"));
    assert_eq!(
        who(&membership_area(&Standing::Asking(JoinPlace::Home), &done)),
        Some(Who::Aterm)
    );
    assert_eq!(
        who(&membership_area(&Standing::Asking(JoinPlace::Away), &done)),
        Some(Who::Person)
    );
    // A person's own Sync off is their hold: off, and no step that would undo it.
    let held = Standing::Allowed {
        fix: Some("Sync is off".into()),
        held: true,
    };
    assert_eq!(membership_area(&held, &done).level, Level::Off);
    assert_eq!(
        membership_area(&Standing::Member("m1".into()), &done).level,
        Level::Ok
    );
}

/// A Mac a rotation dropped (one-mesh E2) is due in both areas, which say `aterm secrets
/// status`'s words, with one step: set the vault folder aside and join as a new Mac, never an
/// admin's approve or a join with the same key, and never the setup's, whose Secrets step reads
/// a dropped Mac blocked and plans no join.
///
/// CONTROL: a revoked vault keeps its approve step, and a Mac not in ALab its join.
#[test]
fn a_dropped_mac_is_told_to_come_back_as_a_new_mac() {
    let words = "dropped: no seat; a rotation left this Mac out with no Remove (out as of seq 6)";
    let joining = setup_with(vec![stage(SECRETS, "running", "")], false);
    let done = setup_with(vec![stage(SECRETS, "applied", "")], true);
    for setup in [&joining, &done] {
        for area in [
            vault_area(&Vault::Dropped(words.into()), setup),
            membership_area(&Standing::Dropped(words.into()), setup),
        ] {
            assert_eq!(area.level, Level::Due, "{area:?}");
            assert_eq!(area.says, words);
            let next = area.next.expect("a step");
            assert_eq!(next.who, Who::Agent);
            assert!(
                next.act.contains("aside") && next.act.contains("new Mac"),
                "{}",
                next.act
            );
            assert!(!next.act.contains("approve"), "{}", next.act);
        }
    }
    let revoked = vault_area(&Vault::Revoked(None), &done);
    assert!(revoked.next.expect("a step").act.contains("approve"));
    assert_eq!(
        who(&membership_area(&Standing::NotIn, &joining)),
        Some(Who::Aterm)
    );
}

/// `aterm status` reads a Mac its vault dropped (one-mesh E2) as `Vault::Dropped` in `aterm
/// secrets status`'s words, one read through `client::dropped_words`.
/// `client::test_read_dropped` stands in for the judgment, which aterm-secrets'
/// `a_recipient_a_rotation_drops_with_no_remove_reads_out_and_says_dropped` holds to a real
/// rotation.
///
/// CONTROL: the same revoked Mac, not dropped, reads `Vault::Revoked`.
#[test]
fn read_vault_reads_a_dropped_mac_dropped_where_a_revoked_one_reads_revoked() {
    use aterm_secrets::client::{self, Membership, Paths};
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&scratch.path().join("home"));
    let source = scratch.path().join("scratch.vault");
    client::create(&paths, source.to_str().expect("utf-8"), None, Some("m1")).expect("vault");
    let at = paths.dir.join("state.toml");
    let mut state: aterm_toml::Table =
        aterm_toml::from_str(&std::fs::read_to_string(&at).expect("state")).expect("toml");
    state.insert(
        "membership".into(),
        aterm_toml::Value::String("revoked".into()),
    );
    aterm_secrets::fsx::write_atomic(
        &at,
        aterm_toml::to_string(&state).expect("toml").as_bytes(),
        0o600,
    )
    .expect("state written");
    let Ok(Membership::Revoked { seq }) = client::status(&paths).map(|status| status.membership)
    else {
        panic!("the Mac reads itself out");
    };
    assert!(
        matches!(read_vault(&paths), Vault::Revoked(_)),
        "the control"
    );
    let (vault, words) =
        client::test_read_dropped(|| (read_vault(&paths), client::out_words(&paths, seq)));
    assert!(words.starts_with("dropped"), "{words}");
    assert_eq!(vault, Vault::Dropped(words));
}

/// A conflicted vault entry, `token` where it is a Claude Code token's.
fn conflict(entry: &str, token: bool) -> aterm_secrets::client::Conflict {
    aterm_secrets::client::Conflict {
        entry: entry.into(),
        token,
    }
}

/// The vault: a missing subscriber is an agent's `secrets agent on`, a person's Sync off is off,
/// and a conflict is a person's pick, a Claude Code token's by the token call (no `aterm secrets
/// set` writes one); an unconfigured vault is aterm's while setup joins it.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with cmp):
/// with `vault_area` naming `aterm secrets set` for every conflict, a token's conflict's step reads
/// "pick each value with `aterm secrets set <name>`" where the test expects the token call.
#[test]
fn vault_steps_follow_who_can_take_them() {
    let none = Setup::NotHere;
    let vault = |follows, conflicts: Vec<aterm_secrets::client::Conflict>| Vault::In {
        name: "alab".into(),
        entries: 2,
        follows,
        conflicts,
    };
    let absent = vault_area(&vault(Follows::Absent, Vec::new()), &none);
    assert_eq!((absent.level, who(&absent)), (Level::Due, Some(Who::Agent)));
    assert!(absent.next.unwrap().act.contains("aterm secrets agent on"));
    assert_eq!(
        vault_area(&vault(Follows::Off, Vec::new()), &none).level,
        Level::Off
    );
    assert_eq!(
        vault_area(&vault(Follows::Runs, Vec::new()), &none).level,
        Level::Ok
    );
    let picked = |conflicts| {
        let area = vault_area(&vault(Follows::Runs, conflicts), &none);
        assert_eq!(who(&area), Some(Who::Person));
        area.next.expect("a step").act
    };
    let token = "[everyone] aterm-claude-token-ac-0123456789abcdef";
    let plain = picked(vec![conflict("[everyone] gh-token", false)]);
    assert_eq!(plain, "pick each value with `aterm secrets set <name>`");
    let tokens = picked(vec![conflict(token, true)]);
    assert_eq!(tokens, aterm_secrets::client::TOKEN_CONFLICT_PICK);
    assert!(!tokens.contains("aterm secrets set"), "{tokens}");
    let both = picked(vec![
        conflict("[everyone] gh-token", false),
        conflict(token, true),
    ]);
    assert!(
        both.starts_with("pick each value with `aterm secrets set <name>`; ")
            && both.ends_with(aterm_secrets::client::TOKEN_CONFLICT_PICK),
        "{both}"
    );
    let joining = setup_with(vec![stage(SECRETS, "pending", "")], false);
    assert_eq!(
        who(&vault_area(&Vault::Unconfigured, &joining)),
        Some(Who::Aterm)
    );
    assert_eq!(
        who(&vault_area(&Vault::Unconfigured, &none)),
        Some(Who::Agent)
    );
}

/// Tools: the Command Line Tools come before anything else is said. aterm's own tab installing
/// them is aterm's; missing with nothing installing them (never opened, stopped, or an account
/// that may not) is a person's, who types the password or is an administrator. The toolset's
/// remedy names its command for an agent, and a System Settings switch a person.
/// NEGATIVE CONTROL: an install under way names aterm, and no agent step is offered for a
/// password only a person types.
#[test]
fn tools_put_the_command_line_tools_first() {
    let healthy = Ok(Some(row(Outcome::Ok, None)));
    let installing = tools_area(Clt::Installing, &healthy);
    assert_eq!(
        (installing.level, who(&installing)),
        (Level::Working, Some(Who::Aterm))
    );
    let missing = tools_area(Clt::Missing { may_install: true }, &healthy);
    assert_eq!(
        (missing.level, who(&missing)),
        (Level::Due, Some(Who::Person))
    );
    assert!(missing.says.contains("Command Line Tools"));
    assert!(missing.says.contains("nothing is installing them"));
    let act = missing.next.as_ref().unwrap().act.clone();
    assert!(act.contains("aterm init --only base.clt"), "{act}");
    let standard = tools_area(Clt::Missing { may_install: false }, &healthy);
    assert_eq!(who(&standard), Some(Who::Person));
    let act = standard.next.as_ref().unwrap().act.clone();
    assert!(act.contains("only an administrator"), "{act}");
    assert_eq!(tools_area(Clt::Present, &healthy).level, Level::Ok);
    let install = tools_area(
        Clt::NotHere,
        &Ok(Some(row(
            Outcome::Planned,
            Some(Remedy::Command("aterm pkg install --default-set".into())),
        ))),
    );
    assert_eq!(who(&install), Some(Who::Agent));
    assert!(
        install
            .next
            .unwrap()
            .act
            .contains("aterm pkg install --default-set")
    );
    let switch = tools_area(
        Clt::Present,
        &Ok(Some(row(
            Outcome::Person,
            Some(Remedy::Person {
                pane: "Privacy & Security".into(),
                how: "turn aterm on".into(),
            }),
        ))),
    );
    assert_eq!(who(&switch), Some(Who::Person));
}

/// Repositories wait on the Command Line Tools, then on the token; cloned is ok; a stage the
/// plan does not select is off; a stage parked after two crashes waits for a person's Check
/// again.
#[test]
fn repos_say_what_they_wait_on() {
    let pending = setup_with(vec![stage("GitHub repositories", "pending", "")], false);
    assert!(
        repos_area(&pending, false, true, &none_owed())
            .says
            .contains("Command Line Tools")
    );
    assert!(
        repos_area(&pending, true, false, &none_owed())
            .says
            .contains("GitHub token")
    );
    assert_eq!(
        repos_area(&pending, true, true, &none_owed()).level,
        Level::Working
    );
    let cloned = setup_with(vec![stage("GitHub repositories", "applied", "")], true);
    assert_eq!(
        repos_area(
            &cloned,
            true,
            true,
            &Ok(OwedClone::Verified {
                present: 3,
                total: 3
            })
        )
        .level,
        Level::Ok
    );
    let unborn = repos_area(
        &cloned,
        true,
        true,
        &Ok(OwedClone::Unobserved {
            present: 50,
            unborn: 3,
            total: 53,
        }),
    );
    assert_eq!(
        unborn.level,
        Level::Ok,
        "an empty remote checked out is no clone owed: {unborn:?}"
    );
    assert!(
        unborn.says.contains("3 more checked out without a commit"),
        "{unborn:?}"
    );
    assert_eq!(
        repos_area(&cloned, true, true, &none_owed()).level,
        Level::Due,
        "a historical applied stage does not verify current repository contents"
    );
    assert_eq!(
        repos_area(
            &cloned,
            true,
            true,
            &Ok(OwedClone::Due("0/53 present".into()))
        )
        .level,
        Level::Working,
        "a historical applied stage cannot hide missing repositories"
    );
    assert_eq!(
        repos_area(&setup_with(Vec::new(), true), true, true, &none_owed()).level,
        Level::Off
    );
    // A stage aterm parked after it stopped there twice waits out its pause and runs again by
    // itself: aterm's own step, never a person's.
    let parked = setup_with(
        vec![stage(
            "GitHub repositories",
            "waiting",
            &format!(
                "Stopped twice while cloning GitHub repositories; {}",
                aterm_types::macos_setup::StageRow::PARKED_TAIL
            ),
        )],
        false,
    );
    let area = repos_area(&parked, true, true, &none_owed());
    assert_eq!((area.level, who(&area)), (Level::Working, Some(Who::Aterm)));
    let area = setup_area(&parked).unwrap();
    assert_eq!((area.level, who(&area)), (Level::Working, Some(Who::Aterm)));
}

/// Where no setup clones them (off macOS), the repositories read the clone `aterm init` owes: one
/// already run owes none, as does one never run. A token cannot distinguish them, so neither
/// is reported as verified. An owed clone remains the vault sync's own step.
#[test]
fn repos_off_macos_read_the_owed_clone() {
    let here = Setup::NotHere;
    assert_eq!(
        repos_area(&here, true, true, &none_owed()).level,
        Level::Due,
        "absence of clone debt does not prove the inventory is present"
    );
    let first = repos_area(&here, true, false, &none_owed());
    assert_eq!((first.level, who(&first)), (Level::Due, Some(Who::Agent)));
    assert!(first.next.unwrap().act.contains("aterm init"));
    for owed in [OwedClone::Waiting("w".into()), OwedClone::Due("d".into())] {
        assert_eq!(
            who(&repos_area(&here, true, false, &Ok(owed))),
            Some(Who::Aterm)
        );
    }
    assert_eq!(
        who(&repos_area(
            &here,
            true,
            true,
            &Ok(OwedClone::Person("p".into()))
        )),
        Some(Who::Person)
    );
    assert_eq!(
        repos_area(&here, true, true, &Ok(OwedClone::Held("h".into()))).level,
        Level::Off
    );
}

/// Before aterm.app's first launch (installed from a terminal, the app never opened) nothing
/// runs the stages the rows show pending: the setup is an agent's `aterm init`, and membership,
/// the vault and the repositories name their own commands, never "aterm is on it".
/// NEGATIVE CONTROL: the same rows from a begun setup read as aterm's.
#[test]
fn an_unopened_app_names_aterm_init() {
    let setup = setup_area(&unbegun()).unwrap();
    assert_eq!((setup.level, who(&setup)), (Level::Due, Some(Who::Agent)));
    assert!(setup.next.unwrap().act.contains("aterm init"));
    for area in [
        membership_area(&Standing::NotIn, &unbegun()),
        vault_area(&Vault::Unconfigured, &unbegun()),
        repos_area(&unbegun(), true, false, &none_owed()),
    ] {
        assert_eq!(who(&area), Some(Who::Agent), "{area:?}");
    }
    let begun = setup_with(
        vec![
            stage(SECRETS, "pending", ""),
            stage("GitHub repositories", "pending", ""),
        ],
        false,
    );
    assert_eq!(who(&setup_area(&begun).unwrap()), Some(Who::Aterm));
    assert_eq!(
        who(&membership_area(&Standing::NotIn, &begun)),
        Some(Who::Aterm)
    );
}

/// A finished setup whose rows a person's Stop left unapplied is held by their choice: off,
/// never ok. NEGATIVE CONTROL: every row applied reads ok.
#[test]
fn a_stopped_setup_is_off_not_finished() {
    let stopped = setup_with(
        vec![
            stage(SECRETS, "stopped", "Not applied by this run."),
            stage("GitHub repositories", "stopped", "Not applied by this run."),
        ],
        true,
    );
    let area = setup_area(&stopped).unwrap();
    assert_eq!(area.level, Level::Off, "{area:?}");
    assert!(area.says.contains("GitHub repositories"), "{}", area.says);
    let mut vault_only = Selection::none();
    vault_only.set(Setting::Secrets, true);
    let applied = setup_reading(vec![stage(SECRETS, "applied", "")], vault_only, None);
    assert_eq!(setup_area(&applied).unwrap().level, Level::Ok);
}

/// A finished run's record says only that its steps ended: its stage rows read `applied` from
/// the run's ledger, never from this Mac's settings or grants, so on their own they claim no
/// current setting, and the area is not `ok`.
#[test]
fn a_finished_record_alone_claims_no_current_settings() {
    let finished = setup_with(
        vec![
            stage("Selected Mac settings", "applied", ""),
            stage("aterm permissions", "applied", ""),
        ],
        true,
    );
    let area = setup_area(&finished).unwrap();
    assert_ne!(area.level, Level::Ok, "{area:?}");
    assert!(!area.says.contains("completed"), "{}", area.says);
    assert!(consistent(&area), "{area:?}");
}

/// The setup area is `ok` on the running aterm's own reading alone: every selected row it reads
/// privately read back as wanted, told with its age, and as set up when last read once that
/// reading is past its standing, never as in place now. Anything short of a dated read-back (no
/// answer, one that verifies only some of them, rows nothing read, one dated after this Mac's
/// clock, which has no age to tell, a wait, a person's step) is `unknown`, never `working` or a
/// person's `due`: a run with no step left runs nothing. NEGATIVE CONTROL: the young reading that
/// covers the plan is `ok` as of now.
#[test]
fn the_setup_area_is_ok_only_on_the_running_aterms_reading() {
    let stages = || {
        vec![
            stage("Selected Mac settings", "applied", ""),
            stage("aterm permissions", "applied", ""),
        ]
    };
    let mut plan = Selection::none();
    plan.set(Setting::Dock, true);
    plan.set(Setting::CapsLock, true);
    plan.set(Setting::Permissions, true);
    let mut dock = Selection::none();
    dock.set(Setting::Dock, true);
    let stands = aterm_types::macos_setup::READING_STANDS_SECS;
    for (current, level, said) in [
        (None, Level::Unknown, ""),
        (
            Some(WorkstationObservation::Unavailable("no answer".into())),
            Level::Unknown,
            "",
        ),
        (Some(complete(dock, 60)), Level::Unknown, ""),
        (
            Some(WorkstationObservation::Waiting("checking".into())),
            Level::Unknown,
            "",
        ),
        (
            Some(WorkstationObservation::Permission("needs: dock".into())),
            Level::Unknown,
            "",
        ),
        (
            Some(WorkstationObservation::Unread {
                rows: "capslock".into(),
                read_back: read_back(dock, 60),
            }),
            Level::Unknown,
            "not read yet: capslock",
        ),
        (
            Some(complete(plan.private(), stands + 1)),
            Level::Ok,
            "last read each selected setting, 11 min ago; not read since",
        ),
        (
            Some(WorkstationObservation::Complete(
                aterm_machines::init::ReadBack {
                    read_at: Some(NOW + 60),
                    ..read_back(plan.private(), 0)
                },
            )),
            Level::Unknown,
            "at a time this Mac's clock cannot date",
        ),
        (
            Some(complete(plan.private(), 60)),
            Level::Ok,
            "read each selected setting as wanted 60 s ago",
        ),
    ] {
        let area = setup_area(&setup_reading(stages(), plan, current.clone())).unwrap();
        assert_eq!(area.level, level, "{current:?}: {area:?}");
        assert!(consistent(&area), "{area:?}");
        assert!(!area.says.contains("completed"), "{}", area.says);
        assert!(area.says.contains(said), "{current:?}: {}", area.says);
        if area.says.contains("last read") {
            assert!(
                !area.says.contains("as wanted"),
                "a reading past its standing is never one of now: {}",
                area.says
            );
        }
    }
}

/// What the running aterm answers of a finished Mac short of a full reading, as
/// `check_background_workstation` parses its `setup status`: a grant that reads off and waits on
/// a person's press (a Permission), a `done` whose verified bits leave the Dock out (no single
/// dated reading of it), and a front door that took the connection and said nothing in time
/// (Silent, a Waiting). None is aterm at work or a person's step this area would take, so none
/// reads `working` or `due`, and none `ok`.
#[test]
fn a_finished_macs_answer_short_of_a_full_reading_is_neither_working_nor_due() {
    let stages = || {
        vec![
            stage("Selected Mac settings", "applied", ""),
            stage("aterm permissions", "applied", ""),
        ]
    };
    let mut plan = Selection::none();
    plan.set(Setting::Dock, true);
    plan.set(Setting::Permissions, true);
    let mut not_dock = plan.private();
    not_dock.set(Setting::Dock, false);
    for current in [
        WorkstationObservation::Permission("background setup needs a person's action: fda".into()),
        complete(not_dock, 60),
        WorkstationObservation::Waiting("background setup has not answered yet".into()),
    ] {
        let area = setup_area(&setup_reading(stages(), plan, Some(current.clone()))).unwrap();
        assert_eq!(area.level, Level::Unknown, "{current:?}: {area:?}");
        assert_ne!(who(&area), Some(Who::Person), "{current:?}: {area:?}");
        assert!(consistent(&area), "{area:?}");
    }
}

/// The window's own `setup status` answer, read as `aterm status` reads it: one grammar from
/// writer to reader. A finished Mac nobody watches (Full Disk Access and the debugger on offer,
/// the Dock as the person changed it, trackpad scrolling set for the next login, every private
/// row read back two hours ago) is `ok`, said as set up when last read with its age and what each
/// row is, never as in place now; before schema 4 the same Mac read `pending:fda;needs:fda`, then
/// `unread:` once ten minutes passed, and stayed unknown forever. NEGATIVE CONTROLS: the base
/// plan with Rust in Spotlight never read is not `ok` (its `verified` speaks for Spotlight too, so
/// a Mac with no profile is not read as set up on rows nothing read), and a grant that reads off
/// is a person's step that keeps it from `ok`.
#[cfg(target_os = "macos")]
#[test]
fn the_windows_answer_of_a_finished_mac_nobody_watches_reads_set_up_when_last_read() {
    use aterm_types::macos_setup::{
        ACCESS_OFFERED, Access, CHANGED_SINCE, DEBUGGER_NOT_ASKED, Observation, Plan, Read,
        Standing, Subject,
    };
    const READ: u64 = NOW - 2 * 3600;
    let reading = |subject: Subject, standing: Standing, note: Option<&str>| Observation {
        subject,
        standing,
        now: None,
        after_login: None,
        wanted: subject.wanted().to_string(),
        note: note.map(str::to_owned),
        detail: None,
        read: Read::At(READ),
    };
    let finished = |plan: Selection, changes: Vec<Observation>| {
        let mut observed: Vec<_> = Subject::all()
            .into_iter()
            .map(|subject| reading(subject, Standing::InPlace, None))
            .collect();
        for change in changes {
            if let Some(slot) = observed
                .iter_mut()
                .find(|row| row.subject == change.subject)
            {
                *slot = change;
            }
        }
        aterm_gui::SetupStatus {
            observed_vault_url: Some(Plan::default().vault_url),
            plan: Some(Plan::from(plan)),
            observed,
            finished: true,
            ..aterm_gui::SetupStatus::default()
        }
    };
    let read_by_status = |status: &aterm_gui::SetupStatus, plan: Selection| {
        let answer = aterm_gui::setup_status_answer_for_test(status, NOW);
        let seen = aterm_cli::macos_doctor::background_workstation_of_reply(&answer, judged(plan));
        (
            answer,
            setup_area(&setup_reading(Vec::new(), plan, Some(seen))).unwrap(),
        )
    };
    let nobody_watches = finished(
        Selection::all(),
        vec![
            reading(
                Subject::Access(Access::FullDiskAccess),
                Standing::NotNeeded,
                Some(ACCESS_OFFERED),
            ),
            reading(
                Subject::Access(Access::Debugger),
                Standing::NotNeeded,
                Some(DEBUGGER_NOT_ASKED),
            ),
            reading(
                Subject::Setting(Setting::Dock),
                Standing::Differs,
                Some(CHANGED_SINCE),
            ),
            reading(
                Subject::Setting(Setting::TrackpadScrolling),
                Standing::AtLogin,
                None,
            ),
        ],
    );
    let (answer, area) = read_by_status(&nobody_watches, Selection::all());
    assert!(
        answer.contains("setup=read-back:") && answer.contains(";offered:fda,debugger"),
        "{answer}"
    );
    assert_eq!(area.level, Level::Ok, "{answer}: {area:?}");
    assert!(consistent(&area), "{area:?}");
    for words in [
        format!(
            "set up as System State last read each selected setting other than Dock, {} ago; \
             not read since",
            aterm_machines::init::age_words(2 * 3600)
        ),
        "Trackpad scrolling takes full effect at your next login".to_owned(),
        "Dock changed since aterm set it, which setup leaves as it is".to_owned(),
    ] {
        assert!(area.says.contains(&words), "{words}: {}", area.says);
    }
    assert!(
        !area.says.contains("as wanted") && !area.says.contains("you changed"),
        "{}",
        area.says
    );

    let mut base = Selection::none();
    base.set(Setting::Spotlight, true);
    base.set(Setting::Permissions, true);
    let spotlight_unread = finished(
        base,
        vec![Observation::withheld(Subject::Setting(Setting::Spotlight))],
    );
    let (answer, area) = read_by_status(&spotlight_unread, base);
    assert_ne!(area.level, Level::Ok, "CONTROL {answer}: {area:?}");
    assert!(area.says.contains("spotlight"), "{}", area.says);

    let off = finished(
        Selection::all(),
        vec![reading(
            Subject::Access(Access::FullDiskAccess),
            Standing::NeedsYou,
            None,
        )],
    );
    let (answer, area) = read_by_status(&off, Selection::all());
    assert!(answer.contains(";needs:fda"), "CONTROL {answer}");
    assert_ne!(area.level, Level::Ok, "CONTROL {answer}: {area:?}");
}

/// A record with no step left whose run is not marked finished (not admitted, or a Workstation
/// row a pending Restore stopped) says what it holds, never that the run reached its end.
/// NEGATIVE CONTROL: the finished record says so.
#[test]
fn an_unfinished_record_with_nothing_left_never_says_its_run_ended() {
    let stages = vec![
        stage("Selected Mac settings", "applied", ""),
        stage("aterm permissions", "stopped", "Not applied by this run."),
    ];
    let open = setup_with(stages.clone(), false);
    let area = setup_area(&open).unwrap();
    assert_ne!(area.level, Level::Ok, "{area:?}");
    assert!(!area.says.contains("reached its end"), "{}", area.says);
    let ended = setup_with(vec![stage("Selected Mac settings", "applied", "")], true);
    assert!(
        setup_area(&ended).unwrap().says.contains("reached its end"),
        "CONTROL"
    );
}

/// A CLI nobody asked for is a note on demand and never a step, whatever its state; one that
/// was asked for and only a person signs in is a person's step; one `aterm ai doctor` signs in
/// is an agent's.
#[test]
fn an_unasked_signed_out_cli_is_no_step() {
    let quiet = agents_area(&Agents {
        asked: vec![("claude", "ok")],
        unasked: 3,
        ..Agents::default()
    });
    assert_eq!((quiet.level, quiet.next.is_none()), (Level::Ok, true));
    assert!(quiet.says.contains("3 more on demand"), "{}", quiet.says);
    let person = agents_area(&Agents {
        asked: vec![("codex", "signed-out")],
        by_person: vec!["codex"],
        person_steps: vec!["aterm ai login codex".into()],
        ..Agents::default()
    });
    assert_eq!(who(&person), Some(Who::Person));
    assert!(person.next.unwrap().act.contains("aterm ai login codex"));
    let doctor = agents_area(&Agents {
        asked: vec![("grok", "not-installed")],
        by_aterm: vec!["grok"],
        ..Agents::default()
    });
    assert_eq!(who(&doctor), Some(Who::Agent));
}

/// Fleet: current is ok, a blocked fleet is a person's decision, swarm off is off; each says this
/// Mac's own presence row.
#[test]
fn fleet_reads_the_sync_check() {
    assert_eq!(
        who(&fleet_area(&Ok(FleetCheck::Blocked("x".into())), "-")),
        Some(Who::Person)
    );
    assert_eq!(
        fleet_area(&Ok(FleetCheck::Idle(aterm_agent::FleetSync::SwarmOff)), "-").level,
        Level::Off
    );
}

/// `aterm status`'s fleet line is this Mac's row of the presence view (one-mesh §4.16, B9): its
/// label, node id, up and build, and the seats up of the members; where the view holds an old
/// build of this Mac's, that it needs an update.
///
/// NEGATIVE CONTROL: have `fleet_area` say `in the vault's fleet` without `own`: neither line
/// carries the row.
#[test]
fn the_fleet_line_is_this_macs_presence_row() {
    use aterm_agent::fleet_view::{Fleet, SeatRow, own_line};
    use aterm_mesh::presence::{Behind, Standing as Presence};
    use aterm_netplace::words::{InStep, Seats};
    let row = |label: &str, me: bool, build: &str, needs_update: bool| SeatRow {
        label: label.to_owned(),
        name: format!("{label}-mini"),
        node: Some(format!("n-{label}00000000000000")),
        node_key: None,
        me,
        presence: Some(Presence::Up),
        build: Some(build.to_owned()),
        needs_update,
        behind: Some(Behind::InStep),
    };
    let fleet = |own_build: &str, needs_update: bool| Fleet {
        seats: Seats {
            members: 3,
            up: Some(2),
            needs_update: if needs_update {
                vec![("m1".to_owned(), own_build.to_owned())]
            } else {
                Vec::new()
            },
            in_step: Some(InStep {
                with: 1,
                of: 1,
                slowest_ms: None,
                apart: 0,
            }),
            log: Some((40, 16_384)),
        },
        rows: vec![
            row("m1", true, own_build, needs_update),
            row("m2", false, "0.159.0+g4ef9cd6144", false),
        ],
        unserved: None,
        observations: Default::default(),
    };
    assert_eq!(
        fleet_area(
            &Ok(FleetCheck::Current),
            &own_line(&fleet("0.159.0+g4ef9cd6144", false), Some(0))
        )
        .says,
        "in the vault's fleet; m1 n-m100000000000000 up at 0.159.0+g4ef9cd6144; 2 up of 3 members"
    );
    assert_eq!(
        fleet_area(
            &Ok(FleetCheck::Idle(aterm_agent::FleetSync::SwarmOff)),
            &own_line(&fleet("0.150.0", true), Some(0))
        )
        .says,
        format!(
            "{}; m1 n-m100000000000000 up at 0.150.0, needs update; 2 up of 3 members",
            aterm_agent::FleetSync::SwarmOff.report()
        )
    );
}

/// No email address reaches either report; `@self` and `user@host` (no dotted domain) are
/// left as they are.
#[test]
fn no_email_reaches_the_output() {
    assert_eq!(
        scrub("signed in as a.b+c@example.co.uk."),
        "signed in as <email>."
    );
    assert_eq!(
        scrub("`aterm ctl @self status`"),
        "`aterm ctl @self status`"
    );
    assert_eq!(scrub("ssh me@host"), "ssh me@host");
    assert_eq!(scrub("x @ y.z"), "x @ y.z");
    let areas = [Area::due(
        "agents",
        "claude ok (person@example.com)",
        Who::Person,
        "mend person@example.com's row",
    )];
    for out in [render_text(&areas), render_json(&areas)] {
        assert!(!out.contains("person@example.com"), "{out}");
        assert!(out.contains("<email>"), "{out}");
    }
}

/// The JSON names each area with its state and its step's who and act, and `null` where there
/// is no step.
#[test]
fn json_carries_who_acts() {
    let areas = [
        Area::ok("fleet", "in"),
        Area::due("vault", "no vault", Who::Agent, "`aterm secrets join`"),
    ];
    let value: aterm_json::Value = aterm_json::from_str(&render_json(&areas)).expect("json");
    assert_eq!(value["schema"].as_u64(), Some(1));
    assert_eq!(value["areas"][0]["state"].as_str(), Some("ok"));
    assert!(value["areas"][0]["next"].is_null());
    assert_eq!(value["areas"][1]["next"]["who"].as_str(), Some("agent"));
    assert_eq!(
        value["areas"][1]["next"]["act"].as_str(),
        Some("`aterm secrets join`")
    );
}

/// The words: nothing, `--json`, `-h` and `--help`; anything else is a usage error.
#[test]
fn words_parse() {
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(asked(&words(&[])), Some(Asked::Text));
    assert_eq!(asked(&words(&["--json"])), Some(Asked::Json));
    assert_eq!(asked(&words(&["--help"])), Some(Asked::Help));
    assert_eq!(asked(&words(&["-h"])), Some(Asked::Help));
    assert_eq!(asked(&words(&["--jsn"])), None);
    assert_eq!(asked(&words(&["--json", "x"])), None);
}

/// Name: one name in the roster, the fleet and the Mac's own names is ok; a fleet name aterm
/// still follows and a repair through its naming rule are aterm's; a missing, older or
/// unreadable rule is the person's (no automatic pass asks for that password): setup's
/// password step leaves a missing one, and only `aterm init` renews an older or unreadable one,
/// which that step never offers. A taken fleet name is an agent's pick; no label yet is aterm's
/// at admission. A personal fleet name is no step.
#[test]
fn name_reads_one_name_across_vault_fleet_and_the_mac() {
    use crate::converge::{MacName, MacNamePending};
    use aterm_agent::FleetName;
    let named = |fleet: FleetName, pending: Option<MacNamePending>| {
        name_area(&Ok(MacName::Named {
            target: "m42".to_owned(),
            fleet,
            pending,
        }))
    };
    let ok = named(FleetName::Current, None);
    assert_eq!(ok.level, Level::Ok);
    assert_eq!(ok.says, "m42; fleet name m42");
    let following = named(
        FleetName::Due {
            from: "node-3fa2".to_owned(),
        },
        None,
    );
    assert_eq!(who(&following), Some(Who::Aterm));
    let host = named(
        FleetName::Current,
        Some(MacNamePending::Elevation(
            "this Mac's own name is Mac, not m42".to_owned(),
        )),
    );
    assert_eq!((host.level, who(&host)), (Level::Due, Some(Who::Person)));
    assert!(host.says.contains("not in place"), "{}", host.says);
    let act = host.next.unwrap().act;
    assert!(
        act.contains("password step") && act.contains("aterm init"),
        "{act}"
    );
    let older = named(
        FleetName::Current,
        Some(MacNamePending::Renewal(
            "this Mac's own name is m42, not m42-heron".to_owned(),
        )),
    );
    assert_eq!((older.level, who(&older)), (Level::Due, Some(Who::Person)));
    assert!(older.says.contains("out of date"), "{}", older.says);
    let act = older.next.unwrap().act;
    assert!(
        act.contains("aterm init") && !act.contains("password step"),
        "{act}"
    );
    let automatic = named(
        FleetName::Current,
        Some(MacNamePending::ThroughRule("own name differs".to_owned())),
    );
    assert_eq!(
        (automatic.level, who(&automatic)),
        (Level::Working, Some(Who::Aterm))
    );
    let unread = named(
        FleetName::Current,
        Some(MacNamePending::Unread(
            "own name differs; the existing naming permission could not be read".to_owned(),
        )),
    );
    assert_eq!(
        (unread.level, who(&unread)),
        (Level::Due, Some(Who::Person))
    );
    assert!(unread.says.contains("could not be read"), "{}", unread.says);
    let act = &unread.next.as_ref().unwrap().act;
    assert!(
        act.contains("renew") && !act.contains("password step"),
        "{act}"
    );
    let collision = named(
        FleetName::Current,
        Some(MacNamePending::Collision("Bonjour collision".into())),
    );
    assert_eq!(
        (collision.level, who(&collision)),
        (Level::Due, Some(Who::Person))
    );
    assert!(!collision.next.unwrap().act.contains("password"));
    let taken = named(FleetName::Taken("node-77".to_owned()), None);
    assert_eq!(who(&taken), Some(Who::Agent));
    let yours = named(FleetName::Kept("studio".to_owned()), None);
    assert_eq!((yours.level, who(&yours)), (Level::Ok, None));
    assert_eq!(who(&name_area(&Ok(MacName::Unlabelled))), Some(Who::Aterm));
    assert_eq!(
        name_area(&Ok(MacName::Off("swarm off".to_owned()))).level,
        Level::Off
    );
    for area in [ok, following, taken, yours, automatic, unread] {
        assert!(consistent(&area), "{area:?}");
    }
}
