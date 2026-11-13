// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::hang_up;
use std::io::Read as _;
use std::time::Duration;

/// A HANG-UP ENDS A READ WAITING ON THE SAME TCP SOCKET, its peer open and silent: on
/// Windows the read waited out its timeout. The pause only lets the read reach its wait;
/// a hang-up that lands first ends it as well, so the order never fails the test.
#[test]
fn a_hang_up_ends_a_read_waiting_on_a_tcp_socket() {
    const HANG: Duration = Duration::from_secs(60);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let peer = std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (near, _) = crate::accept(
        &listener,
        crate::AcceptWait::Forever,
        crate::AcceptedMode::Blocking,
    )
    .expect("accept");
    near.set_read_timeout(Some(HANG * 2)).expect("read timeout");
    let reading = near.try_clone().expect("the reader's copy");
    let (started_tx, started) = std::sync::mpsc::channel();
    let (read_tx, read) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = started_tx.send(());
        let got = (&reading).read(&mut [0u8; 1]).map_err(|e| e.kind());
        let _ = read_tx.send(got);
    });
    started.recv_timeout(HANG).expect("the reader starts");
    std::thread::sleep(Duration::from_millis(100));
    assert!(hang_up(&near), "both halves were open");
    let got = read.recv_timeout(HANG);
    assert!(
        matches!(got, Ok(Ok(0) | Err(_))),
        "the waiting read ends, with nothing read: {got:?}"
    );
    reader.join().expect("the reader");
    drop(peer);
}
