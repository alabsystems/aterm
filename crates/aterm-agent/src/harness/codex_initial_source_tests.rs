// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn plain_source() -> (
    aterm_tempfile::TempDir,
    Env,
    PathBuf,
    super::super::codex_source_gate::SourceWorkLease,
) {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(home.clone());
    env.cwd = root.path().into();
    env.config = None;
    env.state = root.path().join("harness");
    env.aterm_state = Some(home.join(".aterm"));
    let work = super::super::codex_source_gate::SourceWorkLease::for_home(&home).unwrap();
    let auth = home.join(".codex/auth.json");
    (root, env, auth, work)
}

#[test]
fn absent_configuration_is_not_an_owned_source_or_an_implicit_owner_adoption() {
    let (_root, env, auth, work) = plain_source();
    assert!(
        capture_canonical_launch_choice(&env, &auth, &work)
            .unwrap()
            .is_none()
    );
    assert!(!auth.exists());
    std::fs::create_dir(auth.parent().unwrap()).unwrap();
    let marker = auth.with_file_name("auth.json.aterm-owner");
    std::fs::write(marker, b"marker is negative, never an issuer receipt").unwrap();
    assert!(capture_canonical_launch_choice(&env, &auth, &work).is_err());
    assert!(!auth.exists());
}

#[test]
fn malformed_configured_roster_never_becomes_an_unmanaged_fallback() {
    let (root, mut env, auth, work) = plain_source();
    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    env.config = Some(config.join("aterm.toml"));
    let roster = env
        .accounts_roster_for(super::super::accounts::Cli::Codex)
        .unwrap();
    std::fs::write(roster, b"[[account]\ninvalid = [").unwrap();
    assert!(capture_canonical_launch_choice(&env, &auth, &work).is_err());
    assert!(!auth.exists());
}
