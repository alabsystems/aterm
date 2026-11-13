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
#[path = "control_metrics_tests.rs"]
mod tests;
