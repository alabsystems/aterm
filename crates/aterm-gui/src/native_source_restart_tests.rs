// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A managed-credential codex-alab package laid out at `root` (`codex_alab::lay_package`), its
/// binary spelling each source-credential mark `codex_alab::managed_credential_capable` reads.
fn managed_package(root: &std::path::Path) -> (std::path::PathBuf, Image) {
    let exe = aterm_agent::harness::codex_alab::lay_package(root, true);
    std::fs::write(
        &exe,
        b"\xcf\xfa\xed\xfe stand-in: aterm machine credential slot v1 \
          aterm machine credential slot pending v1 aterm session credential source v1\0",
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let image = Image::at(&exe).unwrap();
    (exe, image)
}

/// A renewal Attach reaches the constructor with the registry its dispatch lends: every server's
/// dispatch (the app's control lanes, its fleet and bridge connections, each fixture's
/// connection) is built from [`crate::control`]'s `AccountServices::serving`, and a fixture's
/// services are bound to an app lifetime, so the reservation is admitted. CONTROLS: outside a
/// dispatch (a lane that lent nothing, as every app lane did) and with the lifetime-less
/// registry the fixtures used to build, the same Attach is refused, which
/// `serve_launcher_step` answers DENIED.
#[test]
fn a_dispatch_admits_a_renewal_constructor_from_its_own_app_registry() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let (exe, image) = managed_package(&root.path().join("package"));
    let mut env = aterm_agent::harness::cli::Env::from_process().unwrap();
    env.home = Some(home.clone());
    env.state = root.path().join("state");
    env.config = None;
    let child = Process::observe(std::process::id()).unwrap();
    let command = RestartCommand {
        settles: None,
        account_intent: None,
        image,
        program: exe.to_string_lossy().into_owned(),
        argv: vec!["resume".into(), "fixture-thread".into()],
        env: vec![(
            "CODEX_AUTH_PATH".into(),
            home.join(".codex/auth.json").to_string_lossy().into_owned(),
        )],
        unset: None,
        cwd: Some(root.path().to_string_lossy().into_owned()),
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
    };
    let sid = "s-fixture-renewal-attach";
    assert!(
        prepare_in(&env, sid, &command, &child).is_err(),
        "no dispatch lends a registry"
    );
    let reserved = crate::control::native_host_fixture::serve_as_fixture_dispatch(|| {
        prepare_in(&env, sid, &command, &child)
    });
    assert!(
        matches!(&reserved, Ok(Some(reservation)) if reservation.matches(&command, &child)),
        "the dispatch's own registry admits the exact renewal constructor"
    );
    drop(reserved);
    assert!(
        prepare_in(&env, sid, &command, &child).is_err(),
        "the dispatch takes its registry back"
    );
    let unbound = Arc::new(aterm_agent::harness::codex_source_registry::Registry::default());
    assert!(
        crate::control::native_host_fixture::serve_with_registry(&unbound, || {
            prepare_in(&env, sid, &command, &child)
        })
        .is_err(),
        "a registry bound to no app lifetime refuses the constructor"
    );
}
