// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_agent::harness::contract::{Scope, Words};
use aterm_agent::harness::native_act::Reply;
use aterm_phase::Program;

/// The fixture's catalog: the shipping roster's claude-code-alab row alone, so the fixture and
/// the roster cannot disagree about what that harness may claim.
fn claude() -> &'static [Row] {
    std::slice::from_ref(contract::row_named("claude-code-alab").expect("rostered"))
}

/// The constructors a session with no fixture answers ([`SessionPolicy::constructor_enabled`]):
/// the own-image constructor is not among them on any host.
fn shipping_constructors() -> Vec<Constructor> {
    if cold_restart_qualified() {
        vec![
            Constructor::ColdRestart,
            Constructor::AskedUpgrade,
            Constructor::AskedRelaunch,
        ]
    } else {
        Vec::new()
    }
}

#[test]
fn retained_service_requires_explicit_debug_factory_authorization() {
    assert!(!SessionPolicy::default().retained_constructor_enabled());
    assert_eq!(SessionPolicy::default().retained_startup_mode(), "embedded");
    let fixture = fixture();
    let constructors = fixture.constructors();
    let policy = SessionPolicy {
        fixture: Some(fixture),
        ..Default::default()
    };
    assert!(!policy.retained_constructor_enabled());
    assert_eq!(policy.retained_startup_mode(), "embedded");
    constructors.set_enabled(&[Constructor::ColdRestart]);
    assert!(!policy.retained_constructor_enabled());
    constructors.set_enabled(&[Constructor::PrivateRestart]);
    assert!(policy.retained_constructor_enabled());
    assert_eq!(policy.retained_startup_mode(), "retained");
    constructors.set_enabled(&[]);
    assert!(!policy.retained_constructor_enabled());
    assert_eq!(policy.retained_startup_mode(), "embedded");
}

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
        claude(),
        vec![FixtureImages {
            row: claude()[0].id,
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
        if claim.row == &claude()[0] && claim.pid == 42 && claim.scopes.contains(Scope::Upgrade)));
    assert_eq!(
        policy.read(holder, &words),
        contract::read(holder, &words),
        "the fixture's row is the roster's: one claim, one reading"
    );
    assert!(policy.holder("claude-code-alab@042").is_none());
    assert!(matches!(
        policy.read(holder, &["contract=3", "scope=relaunch,upgrade"]),
        Words::Scoped(claim) if claim.scopes.contains(Scope::Relaunch)
    ));
    // The roster's row allows every duty, a typing one included, and the fixture reads it so.
    assert!(matches!(
        policy.read(holder, &["contract=3", "scope=walls"]),
        Words::Scoped(claim) if claim.scopes.contains(Scope::Walls)
    ));
    assert!(matches!(
        policy.read(holder, &["contract=3", "scope=walls,bogus"]),
        Words::Refused(_)
    ));
    // The catalog is the fixture's alone: a seed id the roster names and it does not has no row,
    // so its claim is refused unregistered (never stored as a foreign claim taking every duty),
    // with or without `contract=`; a holder no seed names is foreign.
    assert!(matches!(
        policy.read("grok-alab@42", &["contract=3", "scope=upgrade"]),
        Words::Unregistered
    ));
    assert!(matches!(
        policy.read("grok-alab@42", &[]),
        Words::Unregistered
    ));
    assert!(matches!(
        policy.read("other-alab@42", &["contract=3", "scope=upgrade"]),
        Words::Foreign
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
    assert!(policy.target_image(&claude()[0], &image(11)));
    assert!(!policy.target_image(&claude()[0], &image(13)));
    let mut other_device = image(11);
    other_device.device += 1;
    assert!(!policy.target_image(&claude()[0], &other_device));
    let mut other_path = image(11);
    other_path.path = b"/private/fixture/other-image".to_vec();
    assert!(!policy.target_image(&claude()[0], &other_path));
    assert!(!policy.target_image(
        contract::row_named("grok-alab").expect("rostered"),
        &image(11)
    ));
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
        Reply::Constructors(shipping_constructors())
    );
    assert!(
        matches!(native_reply(&own.ctx, "info"), Reply::Info { own_image, .. }
        if own_image == vec!["claude-code-alab"])
    );
    // The sibling answers from the whole roster, which the one-row fixture did not narrow.
    assert!(
        matches!(native_reply(&sibling.ctx, "info"), Reply::Info { own_image, .. }
        if own_image.len() == atpkg::harness_record::SEEDS.len()
            && own_image.iter().any(|row| row == "grok-alab"))
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
    assert!(own.ctx.native_policy.target_image(&claude()[0], &image(12)));
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
fn a_cold_fixture_constructor_does_not_enable_generic_or_renewal_constructors() {
    let fixture = fixture().with_quota_rotation();
    let switch = fixture.constructors();
    switch.set_enabled(&[Constructor::ColdRestart]);
    let policy = SessionPolicy {
        fixture: Some(fixture),
        ..Default::default()
    };
    assert!(policy.constructor_enabled(Constructor::ColdRestart));
    assert!(!policy.constructor_enabled(Constructor::OwnImage));
    assert!(!policy.constructor_enabled(Constructor::PrivateRestart));
    assert!(!policy.renewal_constructor_enabled());
    assert!(!policy.quota_rotation_enabled());
    switch.set_enabled(&[Constructor::PrivateRestart]);
    assert!(!policy.constructor_enabled(Constructor::ColdRestart));
    assert!(policy.renewal_constructor_enabled());
    assert!(policy.quota_rotation_enabled());
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
    assert!(handle.ctx.native_policy.fixture.is_none());
    assert_eq!(
        handle.ctx.native_policy.rows().len(),
        atpkg::harness_record::SEEDS.len(),
        "the refused fixture's one-row catalog is not the session's"
    );
}

/// THE ROW IS DISCOVERY, NOT ADMISSION (`docs/DESIGN-harness-repos-2026-10-24.md` §11 D53): a
/// shipping session's `native-act info` names `claude-code-alab:upgrade` among the roster's rows
/// that may claim `upgrade`, which is what claude-code-alab reads before it claims. The row
/// enables no constructor: the session answers the constructors it answered before, the
/// own-image one not among them.
#[test]
fn native_act_info_names_claude_code_alab_for_upgrade_and_enables_no_constructor() {
    let handle = crate::session_store::test_handle(5);
    let line = crate::native_act::serve(&handle.ctx, -1, "info", &mut |_| {
        panic!("observation has no continuation")
    });
    let rows = line
        .split_whitespace()
        .find_map(|word| word.strip_prefix("own-image="))
        .expect("the own-image list");
    assert!(
        rows.split(',').any(|row| row == "claude-code-alab:upgrade"),
        "{line}"
    );
    assert_eq!(
        rows.split(',').count(),
        atpkg::harness_record::SEEDS.len(),
        "one entry for each row that may claim upgrade: {line}"
    );
    assert!(
        matches!(Reply::parse(line.trim_end()), Some(Reply::Info { own_image, .. })
        if own_image.iter().any(|row| row == "claude-code-alab")),
        "the client's parser reads the row: {line}"
    );
    let policy = &handle.ctx.native_policy;
    assert!(policy.fixture.is_none());
    assert!(!policy.constructor_enabled(Constructor::OwnImage));
    assert!(!policy.constructor_enabled(Constructor::PrivateRestart));
    assert_eq!(
        native_reply(&handle.ctx, "constructors"),
        Reply::Constructors(shipping_constructors())
    );
    assert!(matches!(
        policy.read("claude-code-alab@42", &["contract=3", "scope=upgrade"]),
        Words::Scoped(claim) if claim.row.program == Program::Claude
    ));
    assert!(matches!(
        policy.read("claude-code-alab@42", &[]),
        Words::Refused(_)
    ));
}

#[test]
fn fixture_policy_refuses_ambiguous_or_unbounded_catalogs() {
    let create = |rows, images| FixturePolicy::new(rows, images, SupervisorConfig::default(), &[]);
    assert!(create(&[], Vec::new()).is_err());
    assert!(create(claude(), Vec::new()).is_err());
    assert!(
        create(
            claude(),
            vec![FixtureImages {
                row: "other",
                images: vec![image(11)]
            }]
        )
        .is_err()
    );
    assert!(
        create(
            claude(),
            vec![FixtureImages {
                row: claude()[0].id,
                images: vec![image(11); 9]
            }]
        )
        .is_err()
    );
    let mut relative = image(11);
    relative.path = b"relative".to_vec();
    assert!(
        create(
            claude(),
            vec![FixtureImages {
                row: claude()[0].id,
                images: vec![relative]
            }]
        )
        .is_err()
    );
}

/// A TEST BINARY'S REGISTRY IS THE SEEDS': the window's tests read no store and no aterm.toml,
/// so what they assert of `registry::current()` (the counts above, a seed's row by name, a
/// seed's claim proven) holds whatever the machine running them holds, and the strip list they
/// install is the default. NEGATIVE CONTROL (2026-11-14): while only aterm-agent's own unit
/// tests read no store, the aterm.toml below made this binary's list `ZZONLY_` alone.
#[test]
fn a_test_binarys_registry_is_the_seeds_whatever_the_machine_holds() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    crate::test_env::scoped("XDG_CONFIG_HOME", dir.path(), || {
        let path = atpkg::config::config_path().expect("a config path");
        std::fs::create_dir_all(path.parent().expect("its folder")).expect("the folder");
        std::fs::write(
            &path,
            "shell_env_strip = [\"ZZONLY_\"]\n\n[harness]\ndev = [\"agy-alab\"]\n",
        )
        .expect("aterm.toml");
        assert_eq!(
            atpkg::config::load_harness().env_strip,
            Some(vec!["ZZONLY_".to_string()]),
            "CONTROL: the file reads"
        );
        let registry = aterm_agent::harness::registry::current();
        let ids: Vec<&str> = registry.rows().iter().map(|row| row.id).collect();
        let mut seeds: Vec<&str> = atpkg::harness_record::SEEDS.iter().map(|s| s.id).collect();
        seeds.sort_unstable();
        assert_eq!(ids, seeds);
        assert_eq!(
            *registry.env_strip(),
            aterm_types::domain::EnvStrip::default()
        );
        assert_eq!(
            *aterm_types::domain::effective_env_strip(),
            aterm_types::domain::EnvStrip::default(),
            "the list it installs"
        );
    });
}
