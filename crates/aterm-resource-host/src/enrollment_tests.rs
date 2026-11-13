// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn native_gui_enrollment_waits_for_an_unchanged_authenticated_endpoint() {
    exchange(Duration::from_secs(5), false).unwrap();
    assert_eq!(
        exchange(Duration::from_secs(61), false).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        exchange(Duration::from_secs(5), true).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
}

fn exchange(age: Duration, wrong_nonce: bool) -> io::Result<()> {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("enroll-")
        .suffix(".noindex")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let store_path = scratch.path().join("store");
    let store = aterm_capacity::host_executable::Store::prepare_at(&store_path).unwrap();
    aterm_capacity::host_executable::narrow_test_image();
    let program = store.cache_current().unwrap();
    let endpoint = scratch.path().join("gui.sock");
    let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&endpoint)).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let token = "ab".repeat(32);
    let token_name = aterm_types::control_socket::token_names_for_sock("gui.sock").remove(0);
    let token_path = scratch.path().join(token_name);
    std::fs::write(&token_path, &token).unwrap();
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let nonce = "cd".repeat(32);
    let offered = nonce.clone();
    let response = if wrong_nonce {
        "ef".repeat(32)
    } else {
        nonce.clone()
    };
    let (finished, wait_finished) = std::sync::mpsc::sync_channel(1);
    let server = std::thread::spawn(move || -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(deadline),
            aterm_uds::AcceptedMode::Blocking,
        )?;
        assert_eq!(wire::read_line(&stream, deadline)?, format!("AUTH {token}"));
        assert_eq!(wire::read_line(&stream, deadline)?, "version");
        wire::write_all(&stream, b"OK native fixture\n", deadline)?;
        assert_eq!(
            wire::read_line(&stream, deadline)?,
            format!("resource-admission-v3 enroll nonce={offered}")
        );
        wire::write_all(
            &stream,
            format!(
                "OK resource-admission-v3 enroll\n{{\"version\":3,\"nonce\":\"{response}\"}}\n"
            )
            .as_bytes(),
            deadline,
        )?;
        wait_finished
            .recv_timeout(Duration::from_secs(60))
            .map_err(io::Error::other)?;
        drop(stream);
        Ok(())
    });
    let pid = std::process::id();
    let peer = Peer {
        pid,
        birth: aterm_uds::process::read_birth(pid as i32).unwrap().0,
    };
    let result = verify_gui_at(peer, &program, &endpoint, &nonce, Instant::now() - age);
    if result.is_err() {
        // Wake only this failed fixture's private accept; no enrollment is replayed.
        let _ = aterm_uds::connect::until(&endpoint, Instant::now() + Duration::from_secs(60));
    }
    let _ = finished.send(());
    let served = server.join().unwrap();
    // These cached images were read, never executed, by this fixture.
    for entry in std::fs::read_dir(store_path.join("images")).unwrap() {
        std::fs::set_permissions(
            entry.unwrap().path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let authority = result?;
    served?;
    authority.check(peer)
}
