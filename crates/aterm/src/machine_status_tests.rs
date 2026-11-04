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
    }
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
                "Stopped twice while cloning GitHub repositories; it waits for Check again.",
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
        Standing::NotIn,
        Standing::Unknown("unreadable".into()),
    ];
    let vaults = [
        Vault::Unread("torn".into()),
        Vault::Unconfigured,
        Vault::Pending,
        Vault::Plain,
        Vault::Revoked(None),
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
            conflicts: vec!["[everyone] gh-token".into()],
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
        areas.push(fleet_area(&check, &Ok(2)));
    }
    for clt in [Clt::NotHere, Clt::Present, Clt::Installing, Clt::Missing] {
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

/// The vault: a missing subscriber is an agent's `secrets agent on`, a person's Sync off is off,
/// and a conflict is a person's pick; an unconfigured vault is aterm's while setup joins it.
#[test]
fn vault_steps_follow_who_can_take_them() {
    let none = Setup::NotHere;
    let vault = |follows, conflicts: Vec<String>| Vault::In {
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
    let conflict = vault_area(
        &vault(Follows::Runs, vec!["[everyone] gh-token".into()]),
        &none,
    );
    assert_eq!(who(&conflict), Some(Who::Person));
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

/// Tools: missing Command Line Tools are aterm's to install before anything else is said; the
/// toolset's remedy names its command for an agent, and a System Settings switch a person.
#[test]
fn tools_put_the_command_line_tools_first() {
    let healthy = Ok(Some(row(Outcome::Ok, None)));
    let missing = tools_area(Clt::Missing, &healthy);
    assert_eq!(
        (missing.level, who(&missing)),
        (Level::Due, Some(Who::Aterm))
    );
    assert!(missing.says.contains("Command Line Tools"));
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
        repos_area(&cloned, true, true, &none_owed()).level,
        Level::Ok
    );
    assert_eq!(
        repos_area(&setup_with(Vec::new(), true), true, true, &none_owed()).level,
        Level::Off
    );
    let held = setup_with(
        vec![stage(
            "GitHub repositories",
            "waiting",
            "Stopped twice while cloning GitHub repositories; it waits for Check again.",
        )],
        false,
    );
    assert_eq!(
        who(&repos_area(&held, true, true, &none_owed())),
        Some(Who::Person)
    );
}

/// Where no setup clones them (off macOS), the repositories read the clone `aterm init` owes: one
/// already run owes none and reads ok with the token here, forever after; one owed is the vault
/// sync's own; only with neither a clone owed nor a token is `aterm init` an agent's step.
#[test]
fn repos_off_macos_read_the_owed_clone() {
    let here = Setup::NotHere;
    assert_eq!(
        repos_area(&here, true, true, &none_owed()).level,
        Level::Ok,
        "a clone aterm init ran owes nothing"
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
    let applied = setup_with(vec![stage(SECRETS, "applied", "")], true);
    assert_eq!(setup_area(&applied).unwrap().level, Level::Ok);
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

/// Fleet: current is ok, a blocked fleet is a person's decision, swarm off is off.
#[test]
fn fleet_reads_the_sync_check() {
    assert_eq!(
        fleet_area(&Ok(FleetCheck::Current), &Ok(1)).says,
        "in the vault's fleet; 1 peer known"
    );
    assert_eq!(
        who(&fleet_area(&Ok(FleetCheck::Blocked("x".into())), &Ok(0))),
        Some(Who::Person)
    );
    assert_eq!(
        fleet_area(
            &Ok(FleetCheck::Idle(aterm_agent::FleetSync::SwarmOff)),
            &Ok(0)
        )
        .level,
        Level::Off
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
/// still follows is aterm's; the Mac's own names that differ are a person's password; a taken
/// fleet name is an agent's pick; no label yet is aterm's at admission. NEGATIVE CONTROL: a
/// person's own fleet name is no step.
#[test]
fn name_reads_one_name_across_vault_fleet_and_the_mac() {
    use crate::converge::MacName;
    use aterm_agent::FleetName;
    let named = |fleet: FleetName, pending: Option<&str>| {
        name_area(&Ok(MacName::Named {
            target: "m42".to_owned(),
            fleet,
            pending: pending.map(str::to_owned),
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
        Some("this Mac's own name is Mac, not m42"),
    );
    assert_eq!((host.level, who(&host)), (Level::Due, Some(Who::Person)));
    assert!(host.next.unwrap().act.contains("--name m42"));
    let taken = named(FleetName::Taken("node-77".to_owned()), None);
    assert_eq!(who(&taken), Some(Who::Agent));
    let yours = named(FleetName::Kept("studio".to_owned()), None);
    assert_eq!((yours.level, who(&yours)), (Level::Ok, None));
    assert_eq!(who(&name_area(&Ok(MacName::Unlabelled))), Some(Who::Aterm));
    assert_eq!(
        name_area(&Ok(MacName::Off("swarm off".to_owned()))).level,
        Level::Off
    );
    for area in [ok, following, taken, yours] {
        assert!(consistent(&area), "{area:?}");
    }
}
