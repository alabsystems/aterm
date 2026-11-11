// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_command_runs_on_its_connection_and_ends_with_its_exit() {
    let (ours, mut guest) = UnixStream::pair().unwrap();
    let agent = std::thread::spawn(move || {
        let Some(Frame::Exec(exec)) = wire::read_frame(&mut guest).unwrap() else {
            panic!("no Exec");
        };
        assert_eq!(exec.argv, [b"uname".to_vec(), b"-r".to_vec()]);
        assert_eq!(exec.env, [(b"PATH".to_vec(), b"/usr/bin".to_vec())]);
        wire::write_frame(&mut guest, &Frame::Out(b"6.8.0".to_vec())).unwrap();
        wire::write_frame(&mut guest, &Frame::Err(b"warn".to_vec())).unwrap();
        wire::write_frame(&mut guest, &Frame::Out(b"-142-generic\n".to_vec())).unwrap();
        wire::write_frame(&mut guest, &Frame::Exit(Exit::Exited(0))).unwrap();
    });
    let exec = exec_of(&["uname", "-r"], "/", &[("PATH", "/usr/bin")]);
    let got = run(OwnedFd::from(ours), &exec, Duration::from_secs(60)).unwrap();
    agent.join().unwrap();
    assert_eq!(
        got,
        Output {
            exit: Exit::Exited(0),
            out: b"6.8.0-142-generic\n".to_vec(),
            err: b"warn".to_vec(),
        }
    );
}

#[test]
fn a_connection_that_ends_before_its_exit_or_stays_silent_is_named() {
    let (ours, guest) = UnixStream::pair().unwrap();
    drop(guest);
    let exec = exec_of(&["true"], "/", &[]);
    let e = run(OwnedFd::from(ours), &exec, Duration::from_secs(60)).unwrap_err();
    // macOS refuses even the read timeout on a socket whose peer is gone (EINVAL).
    assert!(
        e == "the agent closed the command connection before the command"
            || e.contains("Exec")
            || e.contains("before its Exit"),
        "{e}"
    );
    let (ours, _guest) = UnixStream::pair().unwrap();
    let e = run(OwnedFd::from(ours), &exec, Duration::from_millis(20)).unwrap_err();
    assert!(e.contains("sent nothing for 0.02 s"), "{e}");
}
