// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Asked-upgrade intent is accepted only on its live Upgrade restart shape.

use super::*;

fn prepare() -> Prepare {
    let image = Image {
        path: b"/fixture/claude".to_vec(),
        device: 7,
        inode: 19,
    };
    let process = Process {
        pid: 100,
        birth: 2,
        parent: 90,
        group: 100,
        session: 90,
        image: image.clone(),
    };
    let command = RestartCommand {
        image: image.clone(),
        program: "/fixture/claude".into(),
        argv: vec!["--resume".into(), "conversation".into()],
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: super::super::upgrade::Dialect::Zsh,
        account_intent: None,
        settles: None,
    };
    Prepare {
        receipt_mode: None,
        rejection_domain: None,
        id: "12".repeat(16),
        host_epoch: "34".repeat(16),
        holder: "aterm-harness@4242".into(),
        revision: 2,
        expected_human_seq: Some(3),
        deadline_us: 91,
        duty: Scope::Upgrade,
        asked: Some(AskedIntent::Upgrade(AskedUpgradeIntent {
            predecessor: "56".repeat(16),
        })),
        plan: NativePlan::Restart {
            process: process.clone(),
            observation: None,
            shell: Process {
                pid: 90,
                group: 90,
                parent: 80,
                ..process
            },
            launcher: image,
            socket: "/fixture/control.sock".into(),
            forward: Box::new(command),
            rollback: None,
            cold: None,
        },
        boot: None,
        custody: None,
    }
}

#[test]
fn complete_receipt_mode_is_anchored_explicit_and_not_a_legacy_spelling() {
    let mut original = prepare();
    original.boot = Some(BootStamp {
        host: format!("macos:{}", "ab".repeat(16)),
        boot: "cd".repeat(16),
    });
    original.custody = Some("ef".repeat(32));
    original.rejection_domain = Some("12".repeat(32));
    let legacy = Request::prepare(original.clone()).encode().unwrap();
    original.receipt_mode = Some(ReceiptMode::Recipients5);
    let request = Request::prepare(original.clone());
    let wire = request.encode().unwrap();
    assert!(wire.contains(" receipts=recipients5"));
    assert_eq!(Request::parse(&wire), Some(request.clone()));
    assert_eq!(wire.replace(" receipts=recipients5", ""), legacy);
    assert!(!request.matches_retained(&legacy));
    assert!(!Request::parse(&legacy).unwrap().matches_retained(&wire));
    for bad in [
        wire.replace("receipts=recipients5", "receipts=legacy"),
        wire.replace("receipts=recipients5", "receipts=recipients6"),
        wire.replace("receipts=recipients5", "receipts=Recipients5"),
        format!("{wire} receipts=recipients5"),
    ] {
        assert!(Request::parse(&bad).is_none(), "{bad}");
    }
    for missing in 0..3 {
        let mut incomplete = original.clone();
        match missing {
            0 => incomplete.boot = None,
            1 => incomplete.custody = None,
            _ => incomplete.rejection_domain = None,
        }
        assert!(Request::prepare(incomplete).encode().is_none());
    }
    let Request::Prepare(mut own) = super::tests::fixture() else {
        unreachable!()
    };
    own.receipt_mode = original.receipt_mode;
    own.boot = original.boot;
    own.custody = original.custody;
    own.rejection_domain = original.rejection_domain;
    assert!(Request::Prepare(own).encode().is_none());
}

#[test]
fn asked_upgrade_roundtrips_the_exact_predecessor_and_original_recipe() {
    let plan = prepare();
    assert_eq!(plan.constructor(), Constructor::AskedUpgrade);
    let request = Request::prepare(plan);
    let wire = request.encode().expect("valid asked upgrade");
    assert_eq!(Request::parse(&wire), Some(request.clone()));
    assert_eq!(Request::parse(&wire).unwrap().encode(), Some(wire.clone()));
    for predecessor in [
        "",
        "-",
        "foreign",
        &"AB".repeat(16),
        &"ab".repeat(15),
        &"ab".repeat(17),
    ] {
        let mut invalid_plan = prepare();
        let Some(AskedIntent::Upgrade(intent)) = invalid_plan.asked.as_mut() else {
            unreachable!()
        };
        intent.predecessor = predecessor.into();
        assert!(Request::prepare(invalid_plan).encode().is_none());
        let invalid = wire.replace(
            &format!("asked-predecessor={}", "56".repeat(16)),
            &format!("asked-predecessor={predecessor}"),
        );
        assert!(Request::parse(&invalid).is_none());
    }
    assert!(
        Request::parse(&(wire.clone() + &format!(" asked-predecessor={}", "56".repeat(16))))
            .is_none()
    );
    assert!(Request::parse(&(wire + &format!(" asked-predecessor={}", "78".repeat(16)))).is_none());
}

#[test]
fn asked_upgrade_refuses_foreign_plan_shapes_in_parser_and_encoder() {
    let base = prepare();
    let mut variants = Vec::new();
    let mut relaunch = base.clone();
    relaunch.duty = Scope::Relaunch;
    variants.push(relaunch);
    let mut own = base.clone();
    let NativePlan::Restart {
        process, launcher, ..
    } = &base.plan
    else {
        unreachable!()
    };
    own.plan = NativePlan::OwnImage {
        child: process.clone(),
        target: launcher.clone(),
        version: "0.1.0+g123".into(),
    };
    own.expected_human_seq = None;
    variants.push(own);
    let mut observed = base.clone();
    if let NativePlan::Restart { observation, .. } = &mut observed.plan {
        *observation = Some(super::super::native_observe::ObservationBinding {
            id: "78".repeat(16),
            host_epoch: base.host_epoch.clone(),
            capability: "9a".repeat(32),
        });
    }
    variants.push(observed);
    let mut cold = base;
    if let NativePlan::Restart { cold, .. } = &mut cold.plan {
        *cold = Some(Box::new(ColdIntent {
            login: None,
            home: None,
            auth_observed: false,
            route: super::super::accounts::AuthRoute::Stored,
            conversation: None,
            profile: None,
            mode: super::super::accounts::Mode::Wall,
            billing: ColdBilling::Keep,
        }));
    }
    variants.push(cold);
    for invalid in variants {
        assert!(Request::prepare(invalid.clone()).encode().is_none());
        let mut ordinary = invalid;
        ordinary.asked = None;
        assert_eq!(ordinary.constructor(), ordinary.plan.constructor());
        let wire = Request::prepare(ordinary)
            .encode()
            .expect("otherwise valid plan shape");
        assert!(
            Request::parse(&(wire + &format!(" asked-predecessor={}", "56".repeat(16)))).is_none()
        );
    }
}

#[test]
fn absent_asked_intent_preserves_existing_constructor_selection() {
    let mut ordinary = prepare();
    ordinary.asked = None;
    assert_eq!(ordinary.constructor(), Constructor::PrivateRestart);
    let request = Request::prepare(ordinary);
    let wire = request.encode().unwrap();
    assert_eq!(Request::parse(&wire), Some(request));
    assert!(!wire.contains("asked-predecessor="));
}
