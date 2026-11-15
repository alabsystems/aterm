// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn an_actual_private_socket_releases_only_after_parent_admission() {
    let (mut parent, child) = Parent::new().unwrap();
    let (ended, done) = std::sync::mpsc::channel();
    let helper = std::thread::spawn(move || {
        let result = await_release(child.into());
        ended.send(result).unwrap();
    });
    parent.ready().unwrap();
    assert!(matches!(
        done.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    parent.release().unwrap();
    done.recv_timeout(WAIT).unwrap().unwrap();
    helper.join().unwrap();
}

#[test]
fn parent_withdrawal_never_releases_target_execution() {
    let (mut parent, child) = Parent::new().unwrap();
    let helper = std::thread::spawn(move || await_release(child.into()));
    parent.ready().unwrap();
    drop(parent);
    assert_eq!(
        helper.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}
