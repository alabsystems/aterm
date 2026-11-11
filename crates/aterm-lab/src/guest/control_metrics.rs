// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain metrics from the process on this connection, rather than a later socket owner.

use std::io::Write as _;
use std::time::Duration;

use aterm_agent::RelayClient;
use aterm_uds::process::Identity;

const WAIT: Duration = Duration::from_secs(60);

pub(super) fn run(path: &str) -> Result<(), String> {
    let (mut stream, token) =
        aterm_ctl::connect_then_read_token(path).map_err(|e| e.to_string())?;
    let token = token.map_err(|e| format!("control token unavailable: {e}"))?;
    let pid = aterm_uds::fdpass::peer_pid(&stream).map_err(|e| e.to_string())?;
    let pid = i32::try_from(pid).map_err(|e| e.to_string())?;
    let identity = aterm_uds::process::read_identity(pid).ok_or("peer identity unavailable")?;
    #[cfg(target_vendor = "apple")]
    {
        let audit = aterm_uds::peer::peer_audit_token(&stream)
            .ok_or("connected peer audit token unavailable")?;
        if i32::try_from(audit.pid()).ok() != Some(identity.pid)
            || i32::try_from(audit.pid_version()).ok() != identity.idversion
        {
            return Err("connected peer image changed before metrics collection".into());
        }
    }
    stream
        .set_write_timeout(Some(WAIT))
        .and_then(|()| writeln!(stream, "AUTH {token}"))
        .map_err(|e| e.to_string())?;
    let mut client = RelayClient::new(stream).bounded(WAIT).reads_only();
    let record = collect(&mut client, identity, || {
        aterm_uds::process::read_identity(pid)
    })?;
    println!("{record}");
    Ok(())
}

fn collect(
    client: &mut RelayClient<aterm_uds::CtlStream>,
    identity: Identity,
    current_identity: impl FnOnce() -> Option<Identity>,
) -> Result<aterm_json::Value, String> {
    // The bridge roster needs no placement hop onto a potentially stalled main thread.
    let (header, sessions) = client
        .request_counted("sessions bridge")
        .map_err(|e| e.to_string())?;
    if !header.starts_with("OK ") {
        return Err(format!("session list refused: {header}"));
    }
    let sid = sessions.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        fields.next()?.parse::<u64>().ok()?;
        let sid = fields.next()?;
        (sid.starts_with("s-")
            && (1..=62).contains(&sid[2..].len())
            && sid[2..].bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(sid)
    });
    let sid = sid.ok_or("no session belonging to the connected GUI")?;
    let metrics = client
        .request_line(&format!("@{sid} metrics"))
        .map_err(|e| e.to_string())?;
    if !metrics.starts_with("OK ") {
        return Err(format!("metrics refused: {metrics}"));
    }
    if current_identity() != Some(identity) {
        return Err(
            "connected peer identity changed or became unavailable during metrics collection"
                .into(),
        );
    }
    Ok(aterm_json::json!({
        "schema": 1, "peer": identity.token(), "sid": sid, "metrics": metrics,
        "scope": "connected GUI boot; not evidence about another successor"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader};

    #[test]
    fn metrics_stay_on_the_identified_connection_and_reject_invalid_selectors() {
        let identity = Identity::parse_shaped("10@100:1", true).unwrap();
        let after_exec = Identity::parse_shaped("10@100:2", true).unwrap();
        for (sid, current_identity) in [
            ("s-abc", Some(identity)),
            ("invalid-selector", Some(identity)),
            ("s-abc", Some(after_exec)),
            ("s-abc", None),
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
                if sid == "s-abc" {
                    assert_eq!(line, "@s-abc metrics\n");
                    writeln!(peer, "OK handoff_claim_stamps=journal:+5.3").unwrap();
                } else {
                    assert!(line.is_empty(), "invalid selectors must not be queried");
                }
            });
            let mut client = RelayClient::new(ours).bounded(WAIT).reads_only();
            let result = collect(&mut client, identity, || current_identity);
            drop(client);
            server.join().unwrap();
            if sid == "s-abc" && current_identity == Some(identity) {
                let record = result.unwrap();
                assert_eq!(record["peer"].as_str(), Some("10@100:1"));
                assert!(record["metrics"].as_str().unwrap().contains("journal:+5.3"));
            } else {
                assert!(result.is_err());
            }
        }
    }
}
