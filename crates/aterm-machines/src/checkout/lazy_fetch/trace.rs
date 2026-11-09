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
mod tests {
    use super::*;
    use crate::checkout::lazy_fetch::OBJECT_NAMED;

    fn events(format: &str, attempted: bool) -> Vec<aterm_json::Value> {
        let mut events = vec![
            aterm_json::json!({"event":"version","sid":"root","evt":format}),
            aterm_json::json!({"event":"start","sid":"root","argv":["git","cat-file","-t",OBJECT_NAMED]}),
            aterm_json::json!({"event":"cmd_name","sid":"root","name":"cat-file"}),
        ];
        if attempted {
            events.push(
                aterm_json::json!({"event":"child_start","sid":"root","argv":["git","fetch"]}),
            );
        }
        events.extend([
            aterm_json::json!({"event":"exit","sid":"root","code":128}),
            aterm_json::json!({"event":"atexit","sid":"root","code":128}),
        ]);
        events
    }

    fn encode(events: &[aterm_json::Value]) -> Vec<u8> {
        format!(
            "{}\n",
            events
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
        .into_bytes()
    }

    #[test]
    fn complete_reads_distinguish_quiet_refusals_from_failed_fetch_attempts() {
        for format in ["3", "4"] {
            for attempted in [false, true] {
                let got = observe(&encode(&events(format, attempted)), OBJECT_NAMED, 128).unwrap();
                assert_eq!(got.attempted, attempted);
                assert_eq!(got.format.to_string(), format);
            }
        }
        let mut exec = events("4", true);
        exec[3]
            .as_object_mut()
            .unwrap()
            .insert("event".into(), "exec".into());
        assert!(
            observe(&encode(&exec), OBJECT_NAMED, 128)
                .unwrap()
                .attempted
        );
    }

    #[test]
    fn every_process_indication_refuses_quiet_even_without_its_start_record() {
        for event in [
            "child_start",
            "child_exit",
            "child_ready",
            "exec",
            "exec_result",
        ] {
            let mut trace = events("4", false);
            trace.insert(3, aterm_json::json!({"event":event,"sid":"root"}));
            assert!(
                observe(&encode(&trace), OBJECT_NAMED, 128)
                    .unwrap()
                    .attempted,
                "{event}"
            );
        }
        let mut trace = events("4", false);
        trace.insert(
            3,
            aterm_json::json!({"event":"version","sid":"root/child","evt":"4"}),
        );
        assert!(
            observe(&encode(&trace), OBJECT_NAMED, 128)
                .unwrap()
                .attempted
        );
    }

    #[test]
    fn missing_mismatched_and_truncated_evidence_establishes_no_read() {
        let valid = events("4", false);
        for index in 0..valid.len() {
            let mut missing = valid.clone();
            missing.remove(index);
            assert!(
                observe(&encode(&missing), OBJECT_NAMED, 128).is_err(),
                "missing {index}"
            );
        }
        let bytes = encode(&valid);
        assert!(observe(&bytes[..bytes.len() - 1], OBJECT_NAMED, 128).is_err());
        assert!(observe(&bytes, OBJECT_NAMED, 1).is_err());
        for (index, field, value) in [
            (0, "evt", aterm_json::json!("unknown")),
            (
                1,
                "argv",
                aterm_json::json!(["git", "cat-file", "-t", "another-object"]),
            ),
            (2, "sid", aterm_json::json!("unrelated")),
            (2, "name", aterm_json::json!("fetch")),
            (3, "code", aterm_json::json!(0)),
            (4, "code", aterm_json::json!(0)),
        ] {
            let mut changed = valid.clone();
            changed[index]
                .as_object_mut()
                .unwrap()
                .insert(field.into(), value);
            assert!(
                observe(&encode(&changed), OBJECT_NAMED, 128).is_err(),
                "{index}.{field}"
            );
        }
        let mut duplicate = valid.clone();
        duplicate.insert(2, valid[1].clone());
        assert!(observe(&encode(&duplicate), OBJECT_NAMED, 128).is_err());
        let mut late = valid.clone();
        late.push(aterm_json::json!({"event":"child_start","sid":"root"}));
        assert!(observe(&encode(&late), OBJECT_NAMED, 128).is_err());
    }
}
