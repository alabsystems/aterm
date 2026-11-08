// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Purpose codec controls only; these requests provide no checked-producer authority.

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
    Prepare {
        id: "12".repeat(16),
        host_epoch: "34".repeat(16),
        holder: "aterm-harness@4242".into(),
        revision: 2,
        expected_human_seq: Some(3),
        deadline_us: 91,
        duty: Scope::Relaunch,
        asked: Some(AskedIntent::Relaunch(AskedRelaunchIntent {
            predecessor: "56".repeat(16),
            ask: "memory:fixture".into(),
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
            launcher: image.clone(),
            socket: "/fixture/control.sock".into(),
            forward: Box::new(RestartCommand {
                image,
                program: "/fixture/claude".into(),
                argv: vec!["--resume".into(), "conversation".into()],
                env: Vec::new(),
                unset: None,
                cwd: None,
                heal: None,
                dialect: super::super::upgrade::Dialect::Zsh,
                account_intent: None,
                settles: None,
            }),
            rollback: None,
            cold: None,
        },
    }
}

fn field(wire: &str, key: &str, replacement: Option<&str>) -> String {
    wire.split_ascii_whitespace()
        .filter_map(|word| {
            if word.split_once('=').is_some_and(|(name, _)| name == key) {
                replacement.map(|value| format!("{key}={value}"))
            } else {
                Some(word.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn memory_roundtrips_without_changing_other_constructor_wire() {
    let plan = prepare();
    assert_eq!(plan.constructor(), Constructor::AskedRelaunch);
    let request = Request::prepare(plan.clone());
    let wire = request.encode().expect("valid typed relaunch purpose");
    assert!(wire.contains(" asked-relaunch=memory "));
    assert_eq!(Request::parse(&wire), Some(request));
    assert_eq!(
        Request::parse(&wire).unwrap().encode().as_deref(),
        Some(wire.as_str())
    );
    let mut ordinary = plan.clone();
    ordinary.asked = None;
    assert_eq!(ordinary.constructor(), Constructor::PrivateRestart);
    let ordinary = Request::prepare(ordinary).encode().unwrap();
    let mut upgrade = plan;
    upgrade.duty = Scope::Upgrade;
    upgrade.asked = Some(AskedIntent::Upgrade(AskedUpgradeIntent {
        predecessor: "56".repeat(16),
    }));
    assert_eq!(upgrade.constructor(), Constructor::AskedUpgrade);
    let upgrade_wire = Request::prepare(upgrade).encode().unwrap();
    assert!(!upgrade_wire.contains("asked-relaunch="));
    assert!(!upgrade_wire.contains("asked-ask="));
    assert_eq!(
        field(&upgrade_wire, "asked-predecessor", None),
        ordinary.replace("duty=relaunch", "duty=upgrade")
    );
}

#[test]
fn relaunch_purpose_cannot_be_attached_to_another_plan_shape_or_duty() {
    let base = prepare();
    let wire = Request::prepare(base.clone()).encode().unwrap();
    let purpose = wire
        .split_ascii_whitespace()
        .filter(|word| word.starts_with("asked-"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut variants = Vec::new();
    let mut upgrade = base.clone();
    upgrade.duty = Scope::Upgrade;
    variants.push(upgrade);
    let mut own = base.clone();
    let NativePlan::Restart {
        process, launcher, ..
    } = &base.plan
    else {
        unreachable!();
    };
    own.plan = NativePlan::OwnImage {
        child: process.clone(),
        target: launcher.clone(),
        version: "1.0.0".into(),
    };
    own.expected_human_seq = None;
    own.duty = Scope::Upgrade;
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
    for mut invalid in variants {
        assert!(Request::prepare(invalid.clone()).encode().is_none());
        invalid.asked = None;
        let ordinary = Request::prepare(invalid)
            .encode()
            .expect("valid ordinary shape");
        assert!(Request::parse(&format!("{ordinary} {purpose}")).is_none());
    }
}

#[test]
fn relaunch_wire_requires_one_complete_bounded_purpose() {
    let memory = Request::prepare(prepare()).encode().unwrap();
    for key in ["asked-predecessor", "asked-relaunch", "asked-ask"] {
        assert!(Request::parse(&field(&memory, key, None)).is_none());
    }
    for purpose in ["account", "input-lost"] {
        assert!(Request::parse(&field(&memory, "asked-relaunch", Some(purpose))).is_none());
    }
    for ask in ["", "00", "0a", "ff"] {
        assert!(Request::parse(&field(&memory, "asked-ask", Some(ask))).is_none());
    }
    assert!(Request::parse(&format!("{memory} asked-relaunch=input-lost")).is_none());
    let mut invalid = prepare();
    let Some(AskedIntent::Relaunch(intent)) = &mut invalid.asked else {
        unreachable!();
    };
    intent.ask = "x".repeat(257);
    assert!(Request::prepare(invalid).encode().is_none());
    for extra in [
        "asked-origin-pid=100",
        "asked-origin-birth=2",
        "asked-line-ms=456",
        "asked-line-nth=3",
    ] {
        assert!(Request::parse(&format!("{memory} {extra}")).is_none());
    }
}
