// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_agent::harness::contract::{Scope, Words};
use aterm_agent::harness::native_act::Reply;
use aterm_phase::Program;

static CLAUDE: &[Row] = &[Row {
    id: "claude-code-alab",
    program: Program::Claude,
    package: "claude-code-alab",
    vendor: "claude",
    allows: &[Scope::Upgrade],
}];

// Policy DTOs only: no test feeds these identities to a kernel/process proof.
fn image(inode: u64) -> Image {
    Image {
        path: b"/private/fixture/original-image".to_vec(),
        device: 7,
        inode,
    }
}

fn fixture() -> FixturePolicy {
    FixturePolicy::new(
        CLAUDE,
        vec![FixtureImages {
            row: CLAUDE[0].id,
            images: vec![image(11), image(12)],
        }],
        SupervisorConfig {
            headless: true,
            ..SupervisorConfig::default()
        },
        &[Constructor::OwnImage],
    )
    .expect("bounded policy")
}

#[test]
fn quota_fixture_rotation_requires_its_live_private_constructor() {
    let fixture = fixture().with_quota_rotation();
    let constructors = fixture.constructors();
    let policy = SessionPolicy {
        fixture: Some(fixture),
        ..Default::default()
    };
    assert!(!policy.quota_rotation_enabled());
    constructors.set_enabled(&[Constructor::PrivateRestart]);
    assert!(policy.quota_rotation_enabled());
    constructors.set_enabled(&[]);
    assert!(!policy.quota_rotation_enabled());
}

#[test]
fn fixture_catalog_uses_the_shipping_claim_grammar() {
    let policy = SessionPolicy {
        fixture: Some(fixture()),
        ..SessionPolicy::default()
    };
    let holder = "claude-code-alab@42";
    let words = ["contract=3", "scope=upgrade", "version=0.1.0+fixture"];
    assert!(matches!(policy.read(holder, &words), Words::Scoped(claim)
        if claim.row == &CLAUDE[0] && claim.pid == 42 && claim.scopes.contains(Scope::Upgrade)));
    assert!(matches!(contract::read(holder, &words), Words::Foreign));
    assert!(policy.holder("claude-code-alab@042").is_none());
    assert!(matches!(
        policy.read(holder, &["contract=3", "scope=relaunch"]),
        Words::Refused(_)
    ));
    assert!(matches!(
        policy.read(holder, &["contract=3", "scope=upgrade,upgrade"]),
        Words::Refused(_)
    ));
    assert!(matches!(
        policy.read(holder, &["contract=3", "scope=upgrade", "ttl=100"]),
        Words::Refused(_)
    ));
    assert!(matches!(policy.read(holder, &[]), Words::Refused(_)));
    assert!(matches!(
        SessionPolicy::default().read("grok-alab@42", &["contract=3", "scope=upgrade"]),
        Words::Scoped(_)
    ));
}

#[test]
fn a_matching_path_without_the_captured_device_and_inode_is_not_allowed() {
    let policy = SessionPolicy {
        fixture: Some(fixture()),
        ..SessionPolicy::default()
    };
    assert!(policy.target_image(&CLAUDE[0], &image(11)));
    assert!(!policy.target_image(&CLAUDE[0], &image(13)));
    let mut other_device = image(11);
    other_device.device += 1;
    assert!(!policy.target_image(&CLAUDE[0], &other_device));
    let mut other_path = image(11);
    other_path.path = b"/private/fixture/other-image".to_vec();
    assert!(!policy.target_image(&CLAUDE[0], &other_path));
    assert!(!policy.target_image(&contract::ROSTER[0], &image(11)));
}

fn native_reply(ctx: &Arc<crate::SessionCtx>, verb: &str) -> Reply {
    let line = crate::native_act::serve(ctx, -1, verb, &mut |_| {
        panic!("observation has no continuation")
    });
    Reply::parse(line.trim_end()).expect("actual native discovery reply")
}

#[test]
fn a_session_policy_does_not_enable_its_sibling_and_disable_keeps_reconciliation_catalog() {
    let mut own = crate::session_store::test_handle(1);
    let sibling = crate::session_store::test_handle(2);
    let policy = fixture();
    let switch = policy.constructors();
    assert!(
        Arc::get_mut(&mut own.ctx)
            .expect("exclusive before registration")
            .install_fixture_policy(policy)
            .is_ok()
    );
    assert_eq!(
        native_reply(&own.ctx, "constructors"),
        Reply::Constructors(vec![Constructor::OwnImage])
    );
    assert_eq!(
        native_reply(&sibling.ctx, "constructors"),
        Reply::Constructors(Vec::new())
    );
    assert!(
        matches!(native_reply(&own.ctx, "info"), Reply::Info { own_image, .. }
        if own_image == vec!["claude-code-alab"])
    );
    assert!(
        matches!(native_reply(&sibling.ctx, "info"), Reply::Info { own_image, .. }
        if !own_image.iter().any(|row| row == "claude-code-alab"))
    );
    switch.set_enabled(&[]);
    assert_eq!(
        native_reply(&own.ctx, "constructors"),
        Reply::Constructors(Vec::new())
    );
    assert!(
        own.ctx
            .native_policy
            .holder("claude-code-alab@42")
            .is_some()
    );
    assert!(own.ctx.native_policy.target_image(&CLAUDE[0], &image(12)));
    assert!(
        matches!(native_reply(&own.ctx, "info"), Reply::Info { own_image, .. }
        if own_image == vec!["claude-code-alab"])
    );
    assert!(
        Arc::get_mut(&mut own.ctx)
            .expect("still exclusive")
            .install_fixture_policy(fixture())
            .is_err()
    );
}

#[test]
fn configured_policy_still_obeys_the_sessions_actual_autopilot() {
    let mut handle = crate::session_store::test_handle(3);
    assert!(
        Arc::get_mut(&mut handle.ctx)
            .expect("exclusive")
            .install_fixture_policy(fixture())
            .is_ok()
    );
    let config = handle
        .ctx
        .native_policy
        .config()
        .expect("configured fixture");
    let facts =
        || crate::harness_policy_verb::configured_facts(&handle.ctx, config, 1, true, true, false);
    assert_eq!(facts().effective, (true, "enabled"));
    handle
        .ctx
        .meta
        .lock()
        .unwrap()
        .set_autopilot(Some("off".into()), None);
    assert_eq!(facts().effective, (false, "autopilot-off"));
    let mut disabled = config.clone();
    disabled.enabled = false;
    assert_eq!(
        crate::harness_policy_verb::configured_facts(&handle.ctx, &disabled, 1, true, true, false)
            .effective,
        (false, "disabled")
    );
}

#[test]
fn registration_prevents_later_install_even_after_exclusive_ownership_returns() {
    let mut handle = crate::session_store::test_handle(4);
    let mut store = crate::session_store::SessionStore::default();
    store.register(handle.clone());
    drop(store);
    assert!(
        Arc::get_mut(&mut handle.ctx)
            .expect("registry ownership returned")
            .install_fixture_policy(fixture())
            .is_err()
    );
    assert!(
        handle
            .ctx
            .native_policy
            .holder("claude-code-alab@42")
            .is_none()
    );
}

#[test]
fn fixture_policy_refuses_ambiguous_or_unbounded_catalogs() {
    let create = |rows, images| FixturePolicy::new(rows, images, SupervisorConfig::default(), &[]);
    assert!(create(&[], Vec::new()).is_err());
    assert!(create(CLAUDE, Vec::new()).is_err());
    assert!(
        create(
            CLAUDE,
            vec![FixtureImages {
                row: "other",
                images: vec![image(11)]
            }]
        )
        .is_err()
    );
    assert!(
        create(
            CLAUDE,
            vec![FixtureImages {
                row: CLAUDE[0].id,
                images: vec![image(11); 9]
            }]
        )
        .is_err()
    );
    let mut relative = image(11);
    relative.path = b"relative".to_vec();
    assert!(
        create(
            CLAUDE,
            vec![FixtureImages {
                row: CLAUDE[0].id,
                images: vec![relative]
            }]
        )
        .is_err()
    );
}
