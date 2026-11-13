// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A complete cat-file Trace2 session, independent of localized diagnostics.

#[derive(serde::Deserialize)]
struct Event {
    event: String,
    sid: String,
    argv: Option<Vec<String>>,
    code: Option<i32>,
    name: Option<String>,
    evt: Option<String>,
}

#[derive(Debug)]
pub(super) struct Observation {
    pub format: u8,
    pub attempted: bool,
}

pub(super) fn observe(bytes: &[u8], object: &str, code: i32) -> Result<Observation, &'static str> {
    if !bytes.ends_with(b"\n") {
        return Err("incomplete Trace2 record");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "non-UTF-8 Trace2 record")?;
    let mut events = text
        .lines()
        .map(|line| aterm_json::from_str::<Event>(line).map_err(|_| "invalid Trace2 event"));
    let first = events.next().ok_or("empty Trace2 session")??;
    let format = match first.evt.as_deref() {
        Some("3") => 3,
        Some("4") => 4,
        _ => return Err("unsupported Trace2 event format"),
    };
    if first.event != "version" || first.sid.is_empty() {
        return Err("Trace2 session has no initial version identity");
    }
    let root = first.sid;
    let descendant = format!("{root}/");
    let (mut started, mut named, mut exited, mut ended, mut attempted) =
        (false, false, false, false, false);
    for event in events {
        let event = event?;
        if ended {
            return Err("Trace2 events follow the root shutdown");
        }
        if event.sid != root && !event.sid.starts_with(&descendant) {
            return Err("Trace2 contains another session");
        }
        attempted |= event.sid != root
            || matches!(
                event.event.as_str(),
                "child_start" | "child_exit" | "child_ready" | "exec" | "exec_result"
            );
        if event.sid != root {
            continue;
        }
        match event.event.as_str() {
            "version" => return Err("duplicate Trace2 root version"),
            "start" => {
                let argv = event.argv.ok_or("Trace2 start has no arguments")?;
                if started || argv.len() < 3 || argv[argv.len() - 3..] != ["cat-file", "-t", object]
                {
                    return Err("Trace2 does not name the probe read");
                }
                started = true;
            }
            "cmd_name" => {
                if !started || named || event.name.as_deref() != Some("cat-file") {
                    return Err("Trace2 does not identify cat-file");
                }
                named = true;
            }
            "exit" => {
                if !named || exited || event.code != Some(code) {
                    return Err("Trace2 exit disagrees with the read");
                }
                exited = true;
            }
            "atexit" => {
                if !exited || event.code != Some(code) {
                    return Err("Trace2 shutdown disagrees with the read");
                }
                ended = true;
            }
            _ => {}
        }
    }
    if !ended {
        return Err("Trace2 root did not finish");
    }
    Ok(Observation { format, attempted })
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
