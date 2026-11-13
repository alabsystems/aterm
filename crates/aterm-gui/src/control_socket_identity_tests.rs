// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;

fn fixture() -> (aterm_tempfile::TempDir, SocketPlan, CtlListener, String) {
    let dir = crate::test_scratch::short_dir("the control socket");
    let path = dir.path().join("control.sock");
    let plan = SocketPlan {
        sock_path: path.to_str().unwrap().to_string(),
        token_path: crate::control_auth::token_path_for_socket(path.to_str().unwrap()),
        latest_link: None,
    };
    let token = crate::control_auth::provision_token(&plan.token_path).unwrap();
    let listener = CtlListener::bind(&path).unwrap();
    (dir, plan, listener, token)
}

#[test]
fn consume_incoming_takes_the_witness_once_through_the_blessed_helper() {
    let Some(_env) = crate::seamless::tests::env_alone() else {
        return;
    };
    aterm_log::env::unset(ENV_IDENTITY);
    assert!(consume_incoming() == Ok(None), "absent: nothing to consume");
    assert!(std::env::var_os(ENV_IDENTITY).is_none());

    aterm_log::env::set(ENV_IDENTITY, "");
    assert!(consume_incoming() == Ok(None), "empty: an explicit nothing");
    assert!(
        std::env::var_os(ENV_IDENTITY).is_none(),
        "and it is consumed"
    );

    aterm_log::env::set(ENV_IDENTITY, "{not a witness");
    assert!(
        consume_incoming() == Err("invalid handoff control-socket identity".to_string()),
        "malformed: a refused handoff, never an absent witness"
    );
    assert!(
        std::env::var_os(ENV_IDENTITY).is_none(),
        "consumed even when refused"
    );

    let (_dir, plan, listener, token) = fixture();
    let identity = SocketIdentity::capture(&plan, &listener, &token).expect("a witness");
    aterm_log::env::set(ENV_IDENTITY, identity.encode().expect("encodes"));
    assert!(
        consume_incoming() == Ok(Some(identity)),
        "a real witness decodes to what the parent captured"
    );
    assert!(
        std::env::var_os(ENV_IDENTITY).is_none(),
        "taken once: a second reader sees nothing"
    );
    assert!(consume_incoming() == Ok(None));
}

#[test]
fn relative_listener_witness_restores_directory_across_transport() {
    // Cwd is process-wide: this runs only as a single-test child.
    if !crate::test_alone::alone() {
        return;
    }
    let original = crate::test_scratch::short_dir("the original socket");
    let foreign = crate::test_scratch::short_dir("the foreign socket");
    std::env::set_current_dir(original.path()).unwrap();
    let original_dir = std::env::current_dir().unwrap();
    std::fs::create_dir("nested").unwrap();
    let plan = SocketPlan {
        sock_path: "nested/control.sock".into(),
        token_path: crate::control_auth::token_path_for_socket("nested/control.sock"),
        latest_link: None,
    };
    let token = crate::control_auth::provision_token(&plan.token_path).unwrap();
    let listener = CtlListener::bind(&plan.sock_path).unwrap();
    let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
    assert_eq!(identity.directory.as_ref(), Some(&original_dir));
    let mut command = std::process::Command::new("unused-test-command");
    bind_command_identity(&mut command, Some(&identity));
    let wire = command
        .get_envs()
        .find(|(key, _)| *key == ENV_IDENTITY.name())
        .and_then(|(_, value)| value)
        .unwrap()
        .to_str()
        .unwrap();
    let received = SocketIdentity::decode(wire).unwrap();
    assert!(!wire.contains(&token));

    std::env::set_current_dir(foreign.path()).unwrap();
    let foreign_dir = std::env::current_dir().unwrap();
    assert!(
        !received.matches_current(&plan),
        "old cwd-free witness would reject here"
    );
    std::fs::create_dir("nested").unwrap();
    std::fs::write(&plan.token_path, &token).unwrap();
    let foreign_listener = CtlListener::bind(&plan.sock_path).unwrap();
    let mut redirected = received.clone();
    redirected.directory = Some(foreign_dir.clone());
    // SAFETY: isolated single-test child; no concurrent filesystem workers.
    assert!(!unsafe { redirected.prepare_incoming_directory(&plan) });
    assert_eq!(std::env::current_dir().unwrap(), foreign_dir);
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token.as_bytes());

    // SAFETY: same isolated child and the fixture's admitted witness.
    assert!(unsafe { received.prepare_incoming_directory(&plan) });
    assert_eq!(std::env::current_dir().unwrap(), original_dir);
    assert!(received.matches_current(&plan));
    let client = aterm_uds::CtlStream::connect(&plan.sock_path).unwrap();
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    assert!(crate::control_auth::peer_check(&accepted).is_ok());
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token.as_bytes());
    let (_absolute_dir, absolute, absolute_listener, absolute_token) = fixture();
    let absolute_identity =
        SocketIdentity::capture(&absolute, &absolute_listener, &absolute_token).unwrap();
    assert!(!absolute_identity.encode().unwrap().contains("directory"));
    drop((client, accepted, listener, foreign_listener));
    std::env::set_current_dir("/tmp").unwrap();
}

#[test]
fn actual_listener_identity_roundtrips_and_command_clears_inherited_authority() {
    let (_dir, plan, listener, token) = fixture();
    let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
    let wire = identity.encode().unwrap();
    assert!(
        !wire.contains(&token),
        "token plaintext must not enter the wire"
    );
    let decoded = SocketIdentity::decode(&wire).unwrap();
    assert!(decoded == identity);
    assert!(decoded.matches_current(&plan));
    let mut command = std::process::Command::new("unused-test-command");
    command.env(ENV_IDENTITY, "inherited-authority");
    bind_command_identity(&mut command, None);
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == ENV_IDENTITY.name()
                && value == Some(std::ffi::OsStr::new("")))
    );
    bind_command_identity(&mut command, Some(&identity));
    assert!(command.get_envs().any(
        |(key, value)| key == ENV_IDENTITY.name() && value == Some(std::ffi::OsStr::new(&wire))
    ));
    assert!(SocketIdentity::decode("").is_none());
    assert!(SocketIdentity::decode("{}").is_none());
    assert!(SocketIdentity::decode(&"x".repeat(MAX_WIRE_BYTES + 1)).is_none());
    let mut unsupported = identity.clone();
    unsupported.schema += 1;
    assert!(SocketIdentity::decode(&unsupported.encode().unwrap()).is_none());
}

#[test]
fn capture_requires_the_actual_listener_path_and_provisioned_token() {
    let (dir, plan, listener, token) = fixture();
    assert!(SocketIdentity::capture(&plan, &listener, "a different token").is_none());
    let other = CtlListener::bind(dir.path().join("other.sock")).unwrap();
    assert!(SocketIdentity::capture(&plan, &other, &token).is_none());
    let mut per_process = plan.clone();
    per_process.latest_link = Some(dir.path().join("latest.sock"));
    assert!(SocketIdentity::capture(&per_process, &listener, &token).is_none());
    let mut wrong_token_path = plan.clone();
    wrong_token_path.token_path = dir.path().join("other.token");
    assert!(SocketIdentity::capture(&wrong_token_path, &listener, &token).is_none());
}

#[test]
fn token_rewrite_replacement_symlink_and_oversize_all_refuse() {
    let (dir, plan, listener, token) = fixture();
    let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
    std::fs::write(&plan.token_path, "changed token").unwrap();
    assert!(!identity.matches_current(&plan));
    std::fs::write(&plan.token_path, &token).unwrap();
    assert!(identity.matches_current(&plan));
    let saved = dir.path().join("original.token");
    std::fs::rename(&plan.token_path, &saved).unwrap();
    std::fs::write(&plan.token_path, &token).unwrap();
    assert!(
        !identity.matches_current(&plan),
        "same bytes in a replacement inode are not the captured file"
    );
    std::fs::remove_file(&plan.token_path).unwrap();
    symlink(&saved, &plan.token_path).unwrap();
    assert!(!identity.matches_current(&plan));
    assert!(SocketIdentity::capture(&plan, &listener, &token).is_none());
    std::fs::remove_file(&plan.token_path).unwrap();
    std::fs::write(&plan.token_path, vec![b'x'; MAX_TOKEN_BYTES + 1]).unwrap();
    assert!(SocketIdentity::capture(&plan, &listener, &token).is_none());
    std::fs::remove_file(&plan.token_path).unwrap();
    assert!(!identity.matches_current(&plan));
}

#[test]
fn a_witness_the_parents_rebind_moved_is_filed_a_moment() {
    use crate::app_native::PhysicalFailureShape as Shape;
    let (_dir, plan, listener, token) = fixture();
    let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
    assert!(
        identity.matches_current(&plan),
        "PRECONDITION: the witness holds"
    );
    let mut published = crate::control_listener::socket_file_id(&plan.sock_path);
    let _fresh = crate::control_listener::rebind_socket(&plan.sock_path, &mut published)
        .expect("the rebind");
    assert!(
        !identity.matches_current(&plan),
        "the carried witness no longer matches: the successor refuses"
    );
    let filed = |parent_reads_passing| {
        Shape::of_child_death(crate::ChildDeathEvidence::Exited {
            code: crate::seamless::endpoint_refusal_exit(parent_reads_passing),
        })
    };
    assert_eq!(filed(true), Shape::Transient, "a moment, retried");
    assert_eq!(
        crate::seamless::endpoint_refusal_exit(false),
        74,
        "an older parent is sent what it always was"
    );
}

#[test]
fn a_replaced_socket_refuses_even_with_a_prefilled_foreign_backlog() {
    let (dir, plan, listener, token) = fixture();
    let identity = SocketIdentity::capture(&plan, &listener, &token).unwrap();
    let saved = dir.path().join("retiring.sock");
    std::fs::rename(&plan.sock_path, &saved).unwrap();
    let foreign = CtlListener::bind(&plan.sock_path).unwrap();
    // SAFETY: this test owns the listener whose backlog it reduces.
    assert_eq!(unsafe { libc::listen(foreign.as_raw_fd(), 1) }, 0);
    let mut pending = Vec::new();
    for _ in 0..16 {
        if let Ok(stream) = crate::control_auth::connect_socket_nonblocking(&plan.sock_path) {
            pending.push(stream);
        }
    }
    assert!(
        !pending.is_empty(),
        "the fixture must really reach the foreign listener"
    );
    assert!(!identity.matches_current(&plan));
    assert_eq!(read_token(&plan).unwrap().digest, identity.token_sha256);
    assert!(std::fs::read(&plan.token_path).unwrap() == token.as_bytes());
    drop((pending, foreign));
    std::fs::remove_file(&plan.sock_path).unwrap();
    assert!(!identity.matches_current(&plan));
    symlink(&saved, &plan.sock_path).unwrap();
    assert!(!identity.matches_current(&plan));
}
