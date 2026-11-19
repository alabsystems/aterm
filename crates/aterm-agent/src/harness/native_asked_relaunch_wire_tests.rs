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
        receipt_mode: None,
        rejection_domain: None,
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
        boot: None,
        custody: None,
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

/// A MEMORY RESTART RE-ENTERS A HOST THAT BINDS NO SUCCESSION AS AN UPGRADE DOES (a coordinated
/// session's memory restart after a seamless update named the old epoch's predecessor and was
/// denied for good): its re-entry carries the ask on the relaunch duty alone, so it needs the
/// asked-relaunch constructor and the host's scoped checked-producer authorization; an upgrade's
/// re-entry names no ask. Neither crosses to the other duty, names a predecessor beside its
/// registration, or carries a malformed ask.
#[test]
fn a_memory_restart_reenters_on_the_relaunch_duty_with_its_ask_alone() {
    let observation = super::super::native_observe::ObservationBinding {
        id: "78".repeat(16),
        host_epoch: "34".repeat(16),
        capability: "9a".repeat(32),
    };
    let mut memory = prepare();
    memory.asked = Some(AskedIntent::Reentry(AskedReentryIntent {
        observation: observation.clone(),
        ask: Some("memory:fixture".into()),
    }));
    assert!(memory.asked_restart_shape());
    assert!(memory.checked_relaunch());
    assert_eq!(memory.constructor(), Constructor::AskedRelaunch);
    let request = Request::prepare(memory.clone());
    let wire = request.encode().expect("a memory restart's re-entry");
    assert!(wire.contains(" asked-reentry="), "{wire}");
    assert!(wire.contains(" asked-relaunch=memory "), "{wire}");
    assert!(!wire.contains("asked-predecessor="), "{wire}");
    assert_eq!(Request::parse(&wire), Some(request));

    let mut upgrade = memory.clone();
    upgrade.duty = Scope::Upgrade;
    upgrade.asked = Some(AskedIntent::Reentry(AskedReentryIntent {
        observation,
        ask: None,
    }));
    assert!(upgrade.asked_restart_shape());
    assert!(!upgrade.checked_relaunch());
    assert_eq!(upgrade.constructor(), Constructor::AskedUpgrade);
    let upgrade_request = Request::prepare(upgrade.clone());
    let upgrade_wire = upgrade_request.encode().expect("an upgrade's re-entry");
    assert!(!upgrade_wire.contains("asked-relaunch="), "{upgrade_wire}");
    assert!(!upgrade_wire.contains("asked-ask="), "{upgrade_wire}");
    assert_eq!(Request::parse(&upgrade_wire), Some(upgrade_request));

    // Neither intent crosses to the other duty.
    let mut crossed = memory.clone();
    crossed.duty = Scope::Upgrade;
    assert!(!crossed.asked_restart_shape());
    assert!(Request::prepare(crossed).encode().is_none());
    let mut crossed = upgrade;
    crossed.duty = Scope::Relaunch;
    assert!(!crossed.asked_restart_shape());
    assert!(Request::prepare(crossed).encode().is_none());
    assert!(Request::parse(&wire.replace("duty=relaunch", "duty=upgrade")).is_none());
    assert!(Request::parse(&upgrade_wire.replace("duty=upgrade", "duty=relaunch")).is_none());
    // One complete, bounded purpose, and no predecessor beside the registration.
    for key in ["asked-relaunch", "asked-ask"] {
        assert!(Request::parse(&field(&wire, key, None)).is_none(), "{key}");
    }
    for ask in ["", "00", "0a", "ff"] {
        assert!(Request::parse(&field(&wire, "asked-ask", Some(ask))).is_none());
    }
    assert!(Request::parse(&format!("{wire} asked-predecessor={}", "56".repeat(16))).is_none());
    let mut long = memory;
    let Some(AskedIntent::Reentry(intent)) = &mut long.asked else {
        unreachable!();
    };
    intent.ask = Some("x".repeat(257));
    assert!(!long.asked_restart_shape());
    assert!(Request::prepare(long).encode().is_none());
}

/// THE HOST'S SUCCESSION ANSWER IS DATA FOR ONE PREDECESSOR, OR NONE, IN ONE EPOCH: the read
/// names exactly one canonical predecessor id, or `-` for a record that completed no native
/// operation, and the answer one canonical host epoch and one of its three moves, a bound one with
/// the one stage word that holds it; anything else is refused rather than guessed into a move.
#[test]
fn the_succession_read_names_one_predecessor_and_its_answer_one_epoch_and_move() {
    let predecessor = "56".repeat(16);
    let request = Request::Succession(Some(predecessor.clone()));
    let wire = request.encode().expect("a canonical predecessor");
    assert_eq!(wire, format!("succession predecessor={predecessor}"));
    assert_eq!(Request::parse(&wire), Some(request));
    let none = Request::Succession(None);
    assert_eq!(none.encode().as_deref(), Some("succession predecessor=-"));
    assert_eq!(Request::parse("succession predecessor=-"), Some(none));
    for invalid in [
        "succession".to_string(),
        "succession predecessor=56".to_string(),
        "succession predecessor=".to_string(),
        "succession predecessor=--".to_string(),
        format!("succession predecessor={}", "AB".repeat(16)),
        format!("{wire} epoch={}", "34".repeat(16)),
        format!("succession predecessor=- epoch={}", "34".repeat(16)),
    ] {
        assert!(Request::parse(&invalid).is_none(), "{invalid}");
    }
    assert!(Request::Succession(Some("56".into())).encode().is_none());
    let epoch = "34".repeat(16);
    let bound = Succession::Bound("qualification-foreground".into());
    for admits in [Succession::Predecessor, Succession::Reentry, bound.clone()] {
        let line = succession_reply(&epoch, &admits).expect("a canonical epoch");
        assert_eq!(
            Reply::parse(&line),
            Some(Reply::Succession {
                epoch: epoch.clone(),
                admits
            })
        );
    }
    assert_eq!(
        succession_reply(&epoch, &bound).as_deref(),
        Some(
            format!(
                "OK native-act state=succession epoch={epoch} admits=bound \
                 why=qualification-foreground\n"
            )
            .as_str()
        )
    );
    assert!(succession_reply("34", &bound).is_none());
    for stage in ["", "Qualification", "two words", &"a".repeat(65)] {
        assert!(
            succession_reply(&epoch, &Succession::Bound(stage.into())).is_none(),
            "{stage:?}"
        );
    }
    for invalid in [
        format!("OK native-act state=succession epoch={epoch} admits=maybe"),
        format!("OK native-act state=succession epoch={epoch} admits=bound"),
        format!("OK native-act state=succession epoch={epoch} admits=bound why="),
        format!("OK native-act state=succession epoch={epoch} admits=bound why=Upper"),
        format!("OK native-act state=succession epoch={epoch} admits=bound why=a extra=1"),
        format!("OK native-act state=succession epoch={epoch} admits=reentry why=a"),
        format!("OK native-act state=succession epoch={epoch}"),
        "OK native-act state=succession epoch=34 admits=bound why=a".to_string(),
    ] {
        assert!(Reply::parse(&invalid).is_none(), "{invalid}");
    }
}
