// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! E2E: a real `Turn` driven over an L3 loopback-TLS relay, wired as production
//! does: `RelayClient` -> `dial_and_relay_pinned` -> TLS 1.3 with a channel-bound
//! capability -> `accept_and_relay` -> the remote's control socket (a mock).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use aterm_agent::{RelayClient, SelfGovernor, Turn};
use aterm_net::drive::{accept_and_relay, dial_and_relay_pinned};
use aterm_net::tls::{cert_fingerprint, client_config, server_config};
use aterm_session::EdgeToken;
use aterm_uds::CtlStream;

const TEST_CERT_DER: &[u8] = include_bytes!("testdata/cert.der");
const TEST_KEY_DER: &[u8] = include_bytes!("testdata/key.pkcs8.der");

/// The remote host's authoritative answer for one control verb.
fn mock_reply(line: &str) -> &'static [u8] {
    if line.starts_with("send ") || line == "key enter" {
        b"OK\n"
    } else if line.starts_with("await idle") {
        b"OK idle 7\n"
    } else if line.starts_with("await match") {
        b"OK match 1\n"
    } else if line == "text" {
        "OK 2\n\u{23fa} ANSWER: 391\n\u{276f} \n".as_bytes()
    } else {
        b"ERR unknown verb\n"
    }
}

/// Stands in for the remote's control socket: answers each request line with
/// [`mock_reply`].
fn spawn_mock_remote(mut s: CtlStream) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&raw).trim_end().to_string();
                if s.write_all(mock_reply(&line))
                    .and_then(|()| s.flush())
                    .is_err()
                {
                    return;
                }
            }
            match s.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
    })
}

#[test]
fn remote_turn_over_loopback_tls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
    let ccfg = client_config(cert_fingerprint(TEST_CERT_DER));
    let token = EdgeToken::generate();

    // Host: the remote's authoritative control socket is the mock responder; the
    // listener verifies the channel-bound capability, then relays to it.
    let (svc_a, svc_b) = CtlStream::pair().unwrap();
    let mock = spawn_mock_remote(svc_b);
    let svc_a = Arc::new(Mutex::new(Some(svc_a)));
    let host = thread::spawn({
        let scfg = Arc::clone(&scfg);
        let svc_a = Arc::clone(&svc_a);
        move || {
            let (tcp, _) = listener.accept().unwrap();
            accept_and_relay(
                tcp,
                scfg,
                |src, op| (src == "driver-1" && op == "drive").then_some(token),
                || Ok(svc_a.lock().unwrap().take().unwrap()),
            )
        }
    });

    // Driver: relay the local end over TLS; the test wraps the peer in RelayClient.
    let (drv_local, drv_client) = CtlStream::pair().unwrap();
    let driver = thread::spawn(move || {
        dial_and_relay_pinned(
            addr, ccfg, "driver-1", "drive", &token, b"", drv_local, None,
        )
    });

    // Drive a real Turn against the remote, as on the local path.
    let mut client = RelayClient::new(drv_client);
    let mut gov = SelfGovernor::disabled(8, 1, 1_000_000);
    gov.enable_self_write();
    let screen = Turn::default()
        .run(&mut client, &mut gov, b"what is 17*23?")
        .expect("remote turn completes");
    assert!(
        screen.contains("ANSWER: 391"),
        "the settled REMOTE screen came back over TLS: {screen:?}"
    );
    assert_eq!(screen, "\u{23fa} ANSWER: 391\n\u{276f} \n");

    // Teardown: dropping the client closes the relay; both ends return.
    drop(client);
    let granted = host.join().unwrap();
    assert!(
        granted.is_ok(),
        "the capability was granted and relayed: {granted:?}"
    );
    let _ = driver.join().unwrap();
    let _ = mock.join();
}
