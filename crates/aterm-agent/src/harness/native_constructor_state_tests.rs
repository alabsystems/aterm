// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
