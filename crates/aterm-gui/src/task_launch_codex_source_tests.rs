// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};

pub(super) fn fixture() -> (
    aterm_tempfile::TempDir,
    Preparation,
    Admitted,
    Arc<Registry>,
    aterm_agent::harness::account_work::Lifetime,
) {
    // Advertised-capability/layout fixture only: nothing executes this file
    // and no qualified SDK peer, source grant or activation is fabricated.
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let auth = home.join(".codex");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&auth)
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(auth.join("auth.json"))
        .unwrap()
        .write_all(br#"{"OPENAI_API_KEY":"synthetic-constructor-fixture"}"#)
        .unwrap();
    let package = root.path().join("package");
    let binary = aterm_agent::harness::codex_alab::lay_package(&package, true);
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::OpenOptions::new().append(true).open(&binary).unwrap()
        .write_all(b"aterm machine credential slot v1\naterm machine credential slot pending v1\naterm session credential source v1\n").unwrap();
    let alias = root.path().join("selected-codex");
    std::os::unix::fs::symlink(&binary, &alias).unwrap();
    let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
    let mut starter = cli.plain();
    starter.exe = alias;
    let admitted = aterm_agent::harness::worker::admit(
        cli,
        starter,
        Some(&home),
        |_| true,
        |_| None,
        |_| Ok(()),
    )
    .unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(home.clone());
    env.cwd = root.path().into();
    env.config = None;
    env.state = root.path().join("harness");
    env.aterm_state = Some(home.join(".aterm"));
    let lifetime = aterm_agent::harness::account_work::Lifetime::default();
    let host_registry = Arc::new(Registry::default());
    let preparation = Preparation::with_context(
        host_registry.clone(),
        env,
        "s-constructor-fixture".into(),
        lifetime.enter().unwrap(),
    )
    .unwrap();
    (root, preparation, admitted, host_registry, lifetime)
}

fn exclusive(home: &Path) -> bool {
    let gate = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".aterm/codex-source-custody/gate"))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "observe the real constructor kernel exclusion"
    )]
    gate.try_lock().is_ok()
}

#[test]
fn unobserved_constructor_outcome_revokes_only_unclaimed_permission() {
    let (_root, mut preparation, admitted, _host_registry, _lifetime) = fixture();
    let home = preparation.env.home.clone().unwrap();
    assert!(!exclusive(&home));
    let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
    let admitted = preparation.arm(admitted, cli).unwrap();
    assert!(
        admitted
            .starter()
            .exe
            .starts_with(_root.path().join("package").canonicalize().unwrap())
    );
    preparation.check(&admitted).unwrap();
    let token = &admitted
        .starter()
        .env
        .iter()
        .find(|(name, _)| name == aterm_agent::harness::codex_source_registry::LAUNCH_ENV)
        .unwrap()
        .1;
    assert_eq!(token.len(), 32);
    let fresh = worker_fresh(
        None,
        false,
        false,
        aterm_agent::supervise::config::Approve::None,
    );
    let line = aterm_agent::harness::worker::line(cli, Dialect::Zsh, &admitted, &fresh).unwrap();
    assert!(line.contains(token));
    assert!(!exclusive(&home));
    drop(preparation); // also the uncertain-main-thread-reply path
    assert!(exclusive(&home));
}

#[test]
fn observed_write_retains_native_reservation_after_launcher_returns() {
    let (_root, mut preparation, admitted, _host_registry, _lifetime) = fixture();
    let home = preparation.env.home.clone().unwrap();
    let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
    let admitted = preparation.arm(admitted, cli).unwrap();
    preparation.typed().unwrap();
    assert!(preparation.check(&admitted).is_err());
    let reservation = preparation.reservation.take().unwrap();
    drop(preparation);
    assert!(!exclusive(&home));
    // Explicit permission revocation, not inferred task completion/death.
    reservation.revoke_unclaimed().unwrap();
    assert!(exclusive(&home));
}
#[test]
fn unsupported_legacy_constructor_never_adopts_native_owned_source() {
    let (root, mut preparation, admitted, _host_registry, _lifetime) = fixture();
    let cli = aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Codex).unwrap();
    // Remove the SOURCE advertisement from fixture bytes, not a live image.
    let binary = std::fs::canonicalize(&admitted.starter().exe).unwrap();
    std::fs::write(&binary, b"legacy fixture").unwrap();
    let custom = root.path().join("custom.credentials");
    let mut starter = admitted.starter().clone();
    starter
        .env
        .push(("CODEX_AUTH_PATH".into(), custom.to_str().unwrap().into()));
    starter.unset.extend(["CODEX_AUTH_SLOT", "CODEX_HOME"]);
    let admitted = aterm_agent::harness::worker::admit(
        cli,
        starter,
        preparation.env.home.as_deref(),
        |_| true,
        |_| None,
        |_| Ok(()),
    )
    .unwrap();
    let admitted = preparation.arm(admitted, cli).unwrap();
    assert!(preparation.reservation.is_none());
    assert!(
        !admitted
            .starter()
            .env
            .iter()
            .any(|(key, _)| key == aterm_agent::harness::codex_source_registry::LAUNCH_ENV)
    );
    preparation.check(&admitted).unwrap();
    let marker = PathBuf::from(format!("{}.aterm-owner", custom.display()));
    std::fs::write(&marker, b"malformed-is-still-owned").unwrap();
    assert!(preparation.check(&admitted).is_err());
    assert!(preparation.arm(admitted.clone(), cli).is_err());
    std::fs::remove_file(&marker).unwrap();
    let (slot, ()) = admitted
        .prepare(cli, preparation.env.home.as_deref(), |mut starter| {
            starter
                .env
                .push(("CODEX_AUTH_SLOT".into(), "native-slot-declaration".into()));
            Ok((starter, ()))
        })
        .unwrap();
    assert!(preparation.arm(slot, cli).is_err());
}
