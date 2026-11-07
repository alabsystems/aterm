// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_completed_cold_preparation_survives_target_rotation_and_durable_reload() {
    use super::super::native_act::{
        self, ClaimOutcome, ColdBilling, ColdIntent, NativePlan, Reply, Request,
    };
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = Opts {
        home: world.path().join("home"),
        state: world.path().join("state"),
        account_roster: None,
        sock: None,
        token: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
        tell: Tell::Off,
        press: None,
    };
    let vendor = native_act::Process::observe(std::process::id()).unwrap();
    let mut shell = vendor.clone();
    shell.pid = if vendor.pid == 2 { 3 } else { 2 };
    shell.group = shell.pid;
    let command = native_act::RestartCommand {
        image: vendor.image.clone(),
        argv: Vec::new(),
        env: Vec::new(),
        program: String::from_utf8(vendor.image.path.clone()).unwrap(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: super::super::upgrade::Dialect::Zsh,
        account_intent: None,
        settles: None,
    };
    let predecessor = native_act::Prepare {
        asked_upgrade: None,
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "aterm-harness@fixture".into(),
        revision: 7,
        expected_human_seq: Some(0),
        deadline_us: 123_000_000,
        duty: super::super::contract::Scope::Relaunch,
        plan: NativePlan::Restart {
            process: vendor,
            shell,
            launcher: command.image.clone(),
            socket: "/fixture-native.sock".into(),
            forward: Box::new(command),
            rollback: None,
            observation: None,
            cold: Some(Box::new(ColdIntent {
                login: None,
                home: None,
                auth_observed: true,
                route: super::super::accounts::AuthRoute::Stored,
                conversation: Some("conversation".into()),
                profile: None,
                mode: super::super::accounts::Mode::Wall,
                billing: ColdBilling::Keep,
            })),
        },
    };
    let mut prior = St {
        native_coordination: true,
        native_prepare: Request::prepare(predecessor.clone()).encode().unwrap(),
        from: "2.1.281".into(),
        to: "2.1.318".into(),
        phase: Phase::Exiting { at_s: 1 },
        ..St::default()
    };
    assert_eq!(
        super::super::native_restart::terminal_for_test(
            &mut prior,
            &Reply::ColdRestartCommitted {
                claim: ClaimOutcome::Builtin,
                observation: None,
                intent: Box::new(super::super::accounts_pool::ExecutionIntent::Stored {
                    namespace: "11".repeat(32),
                    login: None,
                    owner: None,
                    generation: "22".repeat(32),
                }),
            }
        ),
        Some(super::super::native_restart::Recovery::Completed)
    );
    let candidate = Candidate {
        exe: PathBuf::from("/different/managed/build"),
        version: Version::parse("2.1.319").unwrap(),
        source: Source::Managed,
    };
    let (retargeted, how) = St::retarget(
        Some(prior),
        &Version::parse("2.1.318").unwrap(),
        &candidate,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(how, Retarget::Anew);
    assert_eq!(retargeted.to, "2.1.319");
    assert_eq!(
        retargeted.native_completed_prepare.as_deref(),
        Some(&predecessor)
    );
    assert!(retargeted.native_coordination);
    assert!(retargeted.native.is_none() && retargeted.native_prepare.is_empty());
    assert!(retargeted.native_observation.is_none());
    save_checked(&opts, "conversation", &retargeted).unwrap();
    let loaded = load(&opts, "conversation").unwrap();
    assert_eq!(
        loaded.native_completed_prepare.as_deref(),
        Some(&predecessor)
    );
    assert!(loaded.native_coordination);
    assert_eq!(loaded.to, "2.1.319");
}

#[test]
fn native_coordination_survives_terminal_restore_and_old_record_migration() {
    let mut st = St {
        native_coordination: true,
        ..St::default()
    };
    let back = st.signalled(400, "", 300, "s-coordination", "fixed".into(), 2);
    st.unsent(back);
    assert!(St::from_json(&st.to_json()).unwrap().native_coordination);
    let legacy = St::default().to_json();
    assert!(!St::from_json(&legacy).unwrap().native_coordination);
    let mut old: Value = aterm_json::from_str(&legacy).unwrap();
    old.as_object_mut().unwrap().insert(
        "native_prepare".into(),
        Value::from("old possibly-dispatched preparation"),
    );
    let loaded = St::from_json(&aterm_json::to_string(&old).unwrap()).unwrap();
    assert!(
        loaded.native_coordination,
        "old persisted native work cannot regain raw fallback"
    );
    assert!(
        St::from_json(&loaded.to_json())
            .unwrap()
            .native_coordination
    );
    let mut retried = St::default();
    loaded.retried_by(&mut retried);
    assert!(retried.native_coordination);
    let mut carried = St::default();
    loaded.kept_by_restart("s-coordination", &mut carried);
    assert!(carried.native_coordination);
    old.as_object_mut()
        .unwrap()
        .insert("native_coordination".into(), Value::from("false"));
    assert!(St::from_json(&aterm_json::to_string(&old).unwrap()).is_none());
}

#[test]
fn retarget_and_restart_preserve_the_entire_unresolved_native_operation() {
    let mut prior = St {
        native_coordination: true,
        native_prepare: "original fixed id/plan/deadline".into(),
        native: Some(super::super::native_act::Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        }),
        pid: 400,
        shell: 300,
        tab: "s-original".into(),
        line: "fixed original".into(),
        from: "1.0.0".into(),
        to: "2.0.0".into(),
        phase: Phase::Exiting { at_s: 20 },
        ..St::default()
    };
    for phase in [
        Phase::Exiting { at_s: 20 },
        Phase::Failed("unknown native outcome".into()),
    ] {
        prior.phase = phase;
        let candidate = Candidate {
            exe: PathBuf::from("/different/current"),
            version: Version::parse("3.0.0").unwrap(),
            source: Source::Managed,
        };
        let (retargeted, how) = St::retarget(
            Some(prior.clone()),
            &Version::parse("2.0.0").unwrap(),
            &candidate,
            None,
            Proc::default(),
            900,
        );
        assert_eq!(how, Retarget::Kept);
        assert_eq!(retargeted, prior);
        let mut retried = St::default();
        prior.retried_by(&mut retried);
        assert_eq!(retried, prior);
        let mut carried = St::default();
        prior.kept_by_restart("s-new", &mut carried);
        assert_eq!(carried, prior);
    }
}
