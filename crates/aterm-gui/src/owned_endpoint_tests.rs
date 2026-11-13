// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use super::imp::{Owned, Published, claim_quit, hold_for_exec, quit_claimed};

const STAND_IN_DIR: &str = "ATERM_TEST_OWNED_ENDPOINT_SIGNAL_DIR";
const STAND_IN_HOLD: &str = "ATERM_TEST_OWNED_ENDPOINT_SIGNAL_HOLD";

fn run_signal_stand_in(dir: &Path) -> ! {
    use std::io::{Read as _, Write as _};

    // Ensure SIGTERM starts unblocked and at SIG_DFL so `publish` installs `on_exit_signal`.
    // SAFETY: zeroed `sigaction` with `SIG_DFL` and an empty signal set are valid POD inputs.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        let mut dfl: libc::sigaction = std::mem::zeroed();
        dfl.sa_sigaction = libc::SIG_DFL;
        libc::sigaction(libc::SIGTERM, &dfl, std::ptr::null_mut());
    }

    let sock = dir.join("aterm-sig.sock");
    let token = dir.join("aterm-sig.token");
    let link = dir.join("latest-sig");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::fs::write(&token, "t").unwrap();
    std::os::unix::fs::symlink("aterm-sig.sock", &link).unwrap();
    let plan = crate::control_auth::SocketPlan {
        sock_path: sock.to_string_lossy().into_owned(),
        token_path: token,
        latest_link: Some(link),
    };
    super::publish(&plan);
    std::io::stdout().flush().unwrap();
    std::fs::write(dir.join("published"), "1").unwrap();

    let mut hold = std::fs::File::from(crate::test_alone::handed(STAND_IN_HOLD));
    let mut buf = [0u8; 1];
    let _ = hold.read(&mut buf);
    std::process::exit(0);
}

fn verify_real_sigterm_cleanup() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::ExitStatusExt as _;

    let scratch = crate::test_scratch::short_dir("the signal stand-in's socket");
    let dir = scratch.path();
    let (hold_rx, _hold_tx) = std::io::pipe().unwrap();
    let mut stand_in = crate::test_alone::StandIn::this_test();
    stand_in.command().env(STAND_IN_DIR, dir.as_os_str());
    stand_in.hand(STAND_IN_HOLD, hold_rx.as_raw_fd());
    let mut child = stand_in.spawn();
    crate::test_alone::until_or_ended(
        "the stand-in published its endpoint",
        std::slice::from_mut(&mut child),
        || dir.join("published").exists(),
    );

    let sock = dir.join("aterm-sig.sock");
    let token = dir.join("aterm-sig.token");
    let link = dir.join("latest-sig");
    let replacement = dir.join("token.new");
    std::fs::write(&replacement, "u").unwrap();
    std::fs::rename(&replacement, &token).unwrap();

    // SAFETY: `child.id()` is a live un-reaped child of this process.
    assert_eq!(
        unsafe { libc::kill(child.id().cast_signed(), libc::SIGTERM) },
        0
    );
    let ran = child.selected();
    assert_eq!(
        ran.status.and_then(|s| s.signal()),
        Some(libc::SIGTERM),
        "on_exit_signal must re-raise SIGTERM with SIG_DFL:\n{}\n{}",
        ran.stdout,
        ran.stderr
    );
    assert!(
        !sock.exists(),
        "SIGTERM handler must unlink the owned socket"
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "SIGTERM handler must unlink the owned alias"
    );
    assert_eq!(
        std::fs::read_to_string(&token).unwrap(),
        "u",
        "SIGTERM handler must leave a replaced file untouched"
    );
}

/// NEGATIVE CONTROL: files replaced, rebound, or repointed after `publish` (and records owned
/// by another pid) survive both direct `Published::remove` and a real `SIGTERM` exit.
#[test]
fn a_terminating_signal_removes_only_what_this_instance_still_owns() {
    if let Some(dir) = std::env::var_os(STAND_IN_DIR) {
        run_signal_stand_in(Path::new(&dir));
    }

    let scratch = crate::test_scratch::short_dir("the own test's socket");
    let dir = scratch.path().to_path_buf();
    let sock = dir.join("aterm-1.sock");
    let token = dir.join("aterm-1.token");
    let link = dir.join("latest");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::fs::write(&token, "t").unwrap();
    std::os::unix::fs::symlink("aterm-1.sock", &link).unwrap();
    let plan = crate::control_auth::SocketPlan {
        sock_path: sock.to_string_lossy().into_owned(),
        token_path: token.clone(),
        latest_link: Some(link.clone()),
    };
    Published::capture(&plan).remove();
    assert!(!sock.exists() && !token.exists());
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the alias went too"
    );

    let _old = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::fs::write(&token, "t").unwrap();
    std::os::unix::fs::symlink("aterm-1.sock", &link).unwrap();
    let published = Published::capture(&plan);
    std::fs::remove_file(&sock).unwrap();
    let _successor = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let replacement = dir.join("token.new");
    std::fs::write(&replacement, "u").unwrap();
    std::fs::rename(&replacement, &token).unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("aterm-2.sock", &link).unwrap();
    let foreign = Published {
        pid: published.pid + 1,
        socket: Owned::capture(&sock),
        token: None,
        alias: None,
    };
    foreign.remove();
    assert!(sock.exists(), "another process's record removes nothing");
    published.remove();
    assert!(sock.exists(), "the successor's socket is not ours");
    assert_eq!(std::fs::read_to_string(&token).unwrap(), "u");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("aterm-2.sock")
    );

    verify_real_sigterm_cleanup();
}

#[test]
fn the_exit_removes_the_socket_a_rebind_put_at_the_same_path() {
    use crate::control_listener::{rebind_socket, socket_file_id};
    let scratch = crate::test_scratch::short_dir("the rebind test's socket");
    let dir = scratch.path().to_path_buf();
    let sock = dir.join("aterm-1.sock");
    let _first = aterm_uds::CtlListener::bind(&sock).unwrap();
    let plan = crate::control_auth::SocketPlan {
        sock_path: sock.to_string_lossy().into_owned(),
        token_path: dir.join("aterm-1.token"),
        latest_link: None,
    };
    let published = Published::capture(&plan);
    let previous = socket_file_id(&plan.sock_path).unwrap();
    let mut expected = Some(previous);
    let _fresh = rebind_socket(&plan.sock_path, &mut expected).expect("rebind");
    let current = expected.unwrap();
    assert_ne!(previous, current, "the rebind put a new socket at the path");
    published.rebound(previous, current);
    published.remove();
    assert!(!sock.exists(), "the exit removes the rebound socket");

    let _other = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let other = socket_file_id(&plan.sock_path).unwrap();
    assert!(!published.rebound(previous, other));
    published.remove();
    assert!(sock.exists(), "another instance's socket at the path stays");
}

#[test]
fn the_graceful_exit_removes_nothing_it_did_not_record() {
    let scratch = crate::test_scratch::short_dir("the grace test's socket");
    let dir = scratch.path().to_path_buf();
    let sock = dir.join("aterm.sock");
    let token = dir.join("aterm.token");
    let _other = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::fs::write(&token, "t").unwrap();
    let plan = crate::control_auth::SocketPlan {
        sock_path: sock.to_string_lossy().into_owned(),
        token_path: token.clone(),
        latest_link: None,
    };
    super::release(&plan);
    assert!(sock.exists() && token.exists());
    Published::capture(&plan).remove();
    assert!(!sock.exists() && !token.exists());
}

#[test]
fn a_cold_exec_and_a_quit_signal_never_both_take_the_claim() {
    use std::sync::atomic::{AtomicI32, Ordering};
    let state = AtomicI32::new(0);
    {
        let held = hold_for_exec(&state);
        assert!(held.is_some(), "nothing claimed: the exec may run");
        assert!(
            !claim_quit(&state, libc::SIGTERM),
            "a signal during the exec takes the immediate path"
        );
        assert!(hold_for_exec(&state).is_none(), "one exec holds it");
    }
    assert!(
        claim_quit(&state, libc::SIGTERM),
        "the exec returned: the next signal's quit is orderly"
    );
    assert!(
        hold_for_exec(&state).is_none(),
        "a claimed quit refuses the exec"
    );
    assert_eq!(
        state.load(Ordering::Acquire),
        libc::SIGTERM,
        "and keeps its signal"
    );
    assert!(
        !claim_quit(&state, libc::SIGINT),
        "a second signal is not claimed"
    );
}

/// The process reads as ending of a signal from the moment its handler claims the orderly
/// quit, never while a cold `exec` holds the claim or before anything claimed it.
#[test]
fn a_process_is_ending_of_its_signal_once_the_handler_claims_the_quit() {
    use std::sync::atomic::AtomicI32;
    let state = AtomicI32::new(0);
    assert!(!quit_claimed(&state), "nothing claimed");
    {
        let _held = hold_for_exec(&state).expect("the exec may run");
        assert!(!quit_claimed(&state), "an exec in flight is no ending");
    }
    assert!(!quit_claimed(&state), "the exec returned");
    assert!(claim_quit(&state, libc::SIGTERM));
    assert!(quit_claimed(&state), "the handler's claim: ending");
}
