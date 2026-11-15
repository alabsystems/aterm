// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{BufRead as _, BufReader};

#[test]
fn metrics_stay_on_the_identified_connection_and_reject_invalid_selectors() {
    let identity = Identity::parse_shaped("10@100:1", true).unwrap();
    let after_exec = Identity::parse_shaped("10@100:2", true).unwrap();
    for (sid, current_identity) in [
        ("t0000000abc", Some(identity)),
        ("invalid-selector", Some(identity)),
        ("t0000000abc", Some(after_exec)),
        ("t0000000abc", None),
    ] {
        let (ours, mut peer) = aterm_uds::CtlStream::pair().unwrap();
        peer.set_read_timeout(Some(WAIT)).unwrap();
        let server = std::thread::spawn(move || {
            let mut input = BufReader::new(peer.try_clone().unwrap());
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            assert_eq!(line, "sessions bridge\n");
            write!(peer, "OK 1 roster=2\n0 {sid} nonce=123\n").unwrap();
            line.clear();
            input.read_line(&mut line).unwrap();
            if sid == "t0000000abc" {
                assert_eq!(line, "@t0000000abc metrics\n");
                writeln!(peer, "OK handoff_claim_stamps=journal:+5.3").unwrap();
            } else {
                assert!(line.is_empty(), "invalid selectors must not be queried");
            }
        });
        let mut client = RelayClient::new(ours).bounded(WAIT).reads_only();
        let result = collect(&mut client, identity, || current_identity);
        drop(client);
        server.join().unwrap();
        if sid == "t0000000abc" && current_identity == Some(identity) {
            let record = result.unwrap();
            assert_eq!(record["peer"].as_str(), Some("10@100:1"));
            assert!(record["metrics"].as_str().unwrap().contains("journal:+5.3"));
        } else {
            assert!(result.is_err());
        }
    }
}
