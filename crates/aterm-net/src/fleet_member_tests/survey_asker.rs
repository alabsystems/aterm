// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A survey that names the session that asked for it is still read from an aterm from before
//! that word.

use super::*;
use std::sync::Mutex;

/// A peer that answers a survey as an aterm of `knows_asker` does: one that does not know the
/// word answers a line that carries it with its usage and keeps the connection. What it was
/// asked, line by line.
fn peer(knows_asker: bool) -> (u16, Arc<Stop>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(Stop::new());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let listening = Listening {
        secret: secret(5),
        scope: FleetScope::Facts,
        node: None,
        serves: &[FleetOp::Survey],
        slots: AT_ONCE,
        per_source: AT_ONCE,
        lifetime: HANG,
    };
    std::thread::spawn({
        let (stop, asked) = (Arc::clone(&stop), Arc::clone(&asked));
        move || {
            serve_with_handshake(
                &listener,
                &listening,
                &stop,
                HANG,
                move |served, _| {
                    served.answer(|stream| {
                        stream.set_read_timeout(Some(HANG))?;
                        loop {
                            let mut line = Vec::new();
                            let mut byte = [0u8; 1];
                            while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {
                                line.push(byte[0]);
                            }
                            if line.is_empty() {
                                return Ok(());
                            }
                            let line = String::from_utf8_lossy(&line).into_owned();
                            asked.lock().unwrap().push(line.clone());
                            let known = line.split(' ').skip(1).all(|word| {
                                knows_asker
                                    && word.starts_with(aterm_types::fleet_activity::SURVEY_BY)
                            });
                            let reply: &[u8] = if known {
                                b"OK 1\ninstance pid=1 host=m version=0.199.0 sessions=0 \
                                  level=facts at_ms=1\n"
                            } else {
                                b"ERR usage: survey [facts|content] \
                                  [holder=<pid>.<boot>.<start>]\n"
                            };
                            stream.write_all(reply)?;
                            stream.flush()?;
                        }
                    })
                },
                |_: FleetEvent| {},
            );
        }
    });
    (port, stop, asked)
}

#[test]
fn a_survey_naming_its_asker_is_asked_again_without_it_of_an_older_aterm() {
    const MARKED: &str = "survey by=s-0123456789abcdef0123";
    for (knows_asker, lines) in [(true, vec![MARKED]), (false, vec![MARKED, "survey"])] {
        let (port, stop, asked) = peer(knows_asker);
        let held = Arc::new(HeldAddrs::default());
        let read = read_one(
            &on_loopback(port),
            &Keyring::default(),
            (&secret(5), None),
            HANG,
            MARKED,
            &held,
        );
        stop.set();
        let surveyed = read.unwrap_or_else(|why| panic!("knows_asker={knows_asker}: {why:?}"));
        assert_eq!(surveyed.lines.len(), 1, "{surveyed:?}");
        assert_eq!(*asked.lock().unwrap(), lines, "knows_asker={knows_asker}");
    }
}

/// A usage answer to a line that names no asker is the peer's refusal, asked once.
#[test]
fn a_usage_answer_to_an_unmarked_survey_is_not_asked_again() {
    let (port, stop, asked) = peer(false);
    let held = Arc::new(HeldAddrs::default());
    let read = read_one(
        &on_loopback(port),
        &Keyring::default(),
        (&secret(5), None),
        HANG,
        "survey sideways",
        &held,
    );
    stop.set();
    assert!(matches!(read, Err(Unreached::Reply(..))), "{read:?}");
    assert_eq!(*asked.lock().unwrap(), ["survey sideways"]);
}
