// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn state(node: &str, at: u64) -> State {
    State {
        line: crate::line::Line::start("0.156.0", "base", 1),
        holder: Holder {
            node: node.into(),
            host: node.into(),
            sid: node.into(),
            pid: 1,
            boot: "boot".into(),
            start: 1,
        },
        step: "verify".into(),
        boundary: "test-binary-finished".into(),
        at,
        p95_s: 0,
        evidence: String::new(),
        rows: Vec::new(),
    }
}

/// The exact holder survives native exec, and the authenticated pre-B1 parent may cross the
/// one child-process deployment boundary. NEGATIVE CONTROLS: without local continuation or
/// the independently live parent proof, and with another node, session or boot, the flag
/// grants no fleet-line authority.
#[test]
fn an_exec_or_authenticated_pre_b1_parent_continues_the_fleet_holder() {
    let previous = state("m3", 1).holder;
    let no_parent = |_: &Holder| false;
    assert!(continues_holder_with(
        true, &previous, &previous, &no_parent
    ));
    assert!(!continues_holder_with(false, &previous, &previous, &|_| {
        true
    }));

    let mut child = previous.clone();
    child.pid += 1;
    child.start += 1;
    let parent = |holder: &Holder| holder.pid == previous.pid && holder.start == previous.start;
    assert!(continues_holder_with(true, &previous, &child, &parent));
    assert!(!continues_holder_with(true, &previous, &child, &no_parent));
    for field in ["node", "host", "sid", "boot"] {
        let mut changed = child.clone();
        match field {
            "node" => changed.node.push('x'),
            "host" => changed.host.push('x'),
            "sid" => changed.sid.push('x'),
            _ => changed.boot.push('x'),
        }
        assert!(
            !continues_holder_with(true, &previous, &changed, &parent),
            "changed {field}"
        );
    }
}

/// The line as 0.199.0's orphaned claim recorded it: holder m9, last renewed at 1794418967, in a
/// step with no p95 history.
fn orphan() -> State {
    let mut orphan = state("m9", 1_794_418_967);
    orphan.line = crate::line::Line::start("0.199.0", "base", 1);
    orphan.step = "qualify".into();
    orphan
}

fn unreachable() -> Observation {
    Observation::Unproven("no route to host".into())
}

/// A lease lapses a whole bound after its last renewal, whatever its holder's Mac shows; until
/// then only that Mac's kernel proving the holder dead admits another cutter, and an expired
/// lease is taken without asking that Mac at all.
/// NEGATIVE CONTROLS: one second inside the bound, an unreachable, running or unknown holder is
/// refused, with when its lease runs out; a long step's p95 stretches the bound.
#[test]
fn an_expired_lease_is_taken_and_a_renewed_one_is_not() {
    let old = state("m3", 1);
    let line = &old.line;
    let asked = std::cell::Cell::new(0);
    let observe = |seen: Observation| {
        let asked = &asked;
        move || {
            asked.set(asked.get() + 1);
            seen
        }
    };
    assert!(admit(&old, line, 1802, observe(unreachable())).is_ok());
    assert_eq!(
        asked.get(),
        0,
        "an expired lease asks nothing of its holder's Mac"
    );
    for seen in [
        unreachable(),
        Observation::Unproven("its process runs".into()),
    ] {
        let refused = admit(&old, line, 1801, observe(seen)).unwrap_err();
        assert!(
            refused.starts_with("CUTTING ELSEWHERE: v0.156.0 by m3")
                && refused.contains("runs until 1801 (0s from now"),
            "{refused}"
        );
    }
    assert!(admit(&old, line, 2, observe(Observation::Dead)).is_ok());
    let mut long_step = old.clone();
    long_step.p95_s = 2000;
    assert!(admit(&long_step, line, 4001, observe(unreachable())).is_err());
    assert!(admit(&long_step, line, 4002, observe(unreachable())).is_ok());
    // A clock a whole lease ahead of this one renews nothing; a little skew is a renewal.
    assert!(
        admit(
            &state("m3", 1802 + 1800),
            line,
            1801,
            observe(unreachable())
        )
        .is_ok()
    );
    assert!(
        admit(
            &state("m3", 1801 + 1800),
            line,
            1801,
            observe(unreachable())
        )
        .is_err()
    );
}

/// 0.199.0's orphaned line is takeable from any Mac by the lease alone: nothing is asked of m9.
/// NEGATIVE CONTROL: the same claim inside its lease, with m9 unreachable, is refused.
#[test]
fn the_orphaned_0_199_0_line_is_taken_by_its_lapsed_lease() {
    let orphan = orphan();
    let never = || -> Observation { panic!("an expired lease asks nothing of m9") };
    assert!(admit(&orphan, &orphan.line, NOW, never).is_ok());
    assert!(admit(&orphan, &orphan.line, orphan.at + 1800, unreachable).is_err());
}

/// The holder host's two kernel reads combine soundly: death from either is death, a record a
/// moment old cannot outvote a fresh judgement, and every other answer is unproven, with why.
/// NEGATIVE CONTROLS: a live process, an unknown judgement, an unknown record and an answer with
/// neither are never death.
#[test]
fn a_holders_answer_proves_death_only_from_its_own_kernel_reads() {
    let seen = |record, judged| observation(&HostAnswer { record, judged });
    assert!(matches!(seen(None, Some(Some(false))), Observation::Dead));
    assert!(matches!(seen(Some(Some(false)), None), Observation::Dead));
    assert!(matches!(
        seen(Some(Some(true)), Some(Some(false))),
        Observation::Dead
    ));
    for (record, judged, why) in [
        (Some(Some(true)), Some(Some(true)), "runs"),
        (None, Some(Some(true)), "runs"),
        (None, Some(None), "could not establish"),
        (Some(None), None, "could not establish"),
        (None, None, "neither"),
    ] {
        let seen = seen(record, judged);
        assert!(
            matches!(&seen, Observation::Unproven(said) if said.contains(why)),
            "{record:?} {judged:?} {seen:?}"
        );
    }
}

const NOW: u64 = 1_794_443_000;

fn peer_json(holder: &Holder, proven: Option<&str>, holder_alive: &str, rows: &str) -> String {
    format!(
        r#"[{{"label":"m9","fleet":true,"proven_node":{},"instance":{{"at_ms":"{}"}},"release_lines":{rows},"release_holder":{holder_alive},"sessions":[]}}]"#,
        proven.map_or_else(|| "null".to_owned(), |node| format!("\"{node}\"")),
        NOW * 1000,
    )
    .replace("HOLDER", &aterm_json::to_string(holder).unwrap())
}

/// Only an answer from the very node the claim names, proven in the handshake, is read, and a
/// record only about exactly the claim's holder.
/// NEGATIVE CONTROLS: an error row, an unproven answer, one proving another node and an aterm
/// too old to name the proof give no answer; a record of another holder is ignored.
#[test]
fn only_the_proven_holder_node_answers_for_its_holder() {
    let old = state("m9", 1);
    let holder = &old.holder;
    let dead = peer_json(holder, Some("m9"), r#"{"alive":false}"#, "null");
    let answer = holder_answer(&dead, holder).unwrap();
    assert_eq!(answer.judged, Some(Some(false)));
    assert!(answer.record.is_none());
    let record =
        r#"[{"alive":true,"holder":HOLDER,"step":"verify","boundary":"b","at":5,"p95_s":7}]"#;
    let live = holder_answer(&peer_json(holder, Some("m9"), "null", record), holder).unwrap();
    assert_eq!(live.judged, None);
    assert_eq!(live.record, Some(Some(true)));
    let mut other = holder.clone();
    other.start += 1;
    let theirs = peer_json(&other, Some("m9"), "null", record);
    assert!(holder_answer(&theirs, holder).unwrap().record.is_none());
    assert!(
        holder_answer(
            &peer_json(holder, Some("m7"), r#"{"alive":false}"#, "null"),
            holder
        )
        .is_err()
    );
    assert!(
        holder_answer(
            &peer_json(holder, None, r#"{"alive":false}"#, "null"),
            holder
        )
        .is_err()
    );
    let unproven = dead.replace(r#""fleet":true"#, r#""fleet":true,"unproven":true"#);
    assert!(holder_answer(&unproven, holder).is_err());
    assert!(
        holder_answer(
            r#"[{"label":"m9","fleet":true,"error":"timed out"}]"#,
            holder
        )
        .is_err()
    );
    // 0.198.0's ctl: no proof field and no release rows.
    assert!(
        holder_answer(
            r#"[{"label":"m9","fleet":true,"instance":{},"sessions":[]}]"#,
            holder
        )
        .is_err()
    );
    assert!(holder_answer("not json", holder).is_err());
}

/// The remote path asks the judgement first and falls back to the plain survey an older
/// installed aterm understands; neither failing read is ever death.
#[test]
fn a_remote_holder_is_judged_through_the_installed_aterm() {
    let old = state("m9", 1);
    let holder = old.holder.clone();
    let dead = peer_json(&holder, Some("m9"), r#"{"alive":false}"#, "null");
    let asked = std::cell::RefCell::new(Vec::new());
    let judging = |args: &[&str]| {
        asked.borrow_mut().push(args.join(" "));
        Ok(dead.clone())
    };
    assert!(matches!(
        remote_observation(&old, &judging),
        Observation::Dead
    ));
    assert_eq!(
        asked.borrow().as_slice(),
        [format!(
            "survey --host m9 --timeout 3 --json --holder {}.boot.1",
            holder.pid
        )]
    );
    // A holder's Mac older than `holder=` answers the request with an error row; the plain
    // survey's record still counts.
    let record =
        r#"[{"alive":true,"holder":HOLDER,"step":"verify","boundary":"b","at":1,"p95_s":0}]"#;
    let plain = peer_json(&holder, Some("m9"), "null", record);
    let old_peer = |args: &[&str]| {
        Ok(if args.contains(&"--holder") {
            r#"[{"label":"m9","fleet":true,"error":"ERR usage: survey [facts|content]"}]"#
                .to_owned()
        } else {
            plain.clone()
        })
    };
    assert!(matches!(
        remote_observation(&old, &old_peer),
        Observation::Unproven(why) if why == "its process runs"
    ));
    let gone = plain.replace(r#""alive":true"#, r#""alive":false"#);
    assert!(matches!(
        remote_observation(&old, &|_: &[&str]| Ok(gone.clone())),
        Observation::Dead
    ));
    // NEGATIVE CONTROL: an installed ctl older than `--holder` refuses it, and its plain
    // survey names no proven node, so it is never an answer.
    let old_ctl = |args: &[&str]| {
        if args.contains(&"--holder") {
            Err("ERR usage: survey".to_owned())
        } else {
            Ok(r#"[{"label":"m9","fleet":true,"instance":{},"sessions":[]}]"#.to_owned())
        }
    };
    assert!(matches!(
        remote_observation(&old, &old_ctl),
        Observation::Unproven(_)
    ));
    // NEGATIVE CONTROLS: an unreachable Mac or an unreadable fleet is unknown, never dead.
    let unreachable = |_: &[&str]| Err("no route to host".to_owned());
    let seen = remote_observation(&old, &unreachable);
    assert!(
        matches!(&seen, Observation::Unproven(why) if why.contains("no route to host")),
        "{seen:?}"
    );
    assert!(admit(&old, &old.line, 1801, || seen).is_err());
    let silent = |_: &[&str]| Ok(r#"[{"label":"m9","fleet":true,"error":"timed out"}]"#.to_owned());
    assert!(matches!(
        remote_observation(&old, &silent),
        Observation::Unproven(_)
    ));
}

/// A claim naming this Mac's node from another host (a cloned state folder) is not this
/// kernel's to judge.
/// NEGATIVE CONTROL: the same claim from this host is judged here, and a reaped holder is dead.
#[test]
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn a_holder_naming_this_node_from_another_host_is_unknown() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut gone = state("m3", 1);
    gone.holder.pid = child.id();
    let identity = aterm_agent::release_progress::identity(gone.holder.pid);
    child.kill().unwrap();
    child.wait().unwrap();
    (gone.holder.boot, gone.holder.start) = identity.unwrap();
    let here = gone.holder.clone();
    assert!(matches!(
        observe(&gone, &here, dir.path()),
        Observation::Dead
    ));
    let mut elsewhere = here;
    elsewhere.host = "m4".into();
    assert!(matches!(
        observe(&gone, &elsewhere, dir.path()),
        Observation::Unproven(_)
    ));
}

/// A holder on this Mac whose record is gone is still judged by this kernel: an exited holder
/// is dead. NEGATIVE CONTROLS: the live process itself, and a pid the judgement refuses, are
/// unproven and never shorten its lease.
#[test]
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn a_local_holder_without_a_record_is_judged_by_this_kernel() {
    use aterm_agent::release_progress::identity;
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut live = state("m3", 1);
    live.holder.pid = std::process::id();
    (live.holder.boot, live.holder.start) = identity(live.holder.pid).unwrap();
    let observed = local_observation(&live, dir.path());
    assert!(
        matches!(&observed, Observation::Unproven(_)),
        "{observed:?}"
    );
    assert!(admit(&live, &live.line, live.at, || observed).is_err());

    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut gone = live.clone();
    gone.holder.pid = child.id();
    let start = identity(gone.holder.pid).map(|(_, start)| start);
    child.kill().unwrap();
    child.wait().unwrap();
    gone.holder.start = start.unwrap();
    assert!(matches!(
        local_observation(&gone, dir.path()),
        Observation::Dead
    ));

    // A pid the judgement refuses before any kernel read (launchd's or init's) is no death.
    // Failed kernel reads themselves are aterm-agent's `a_failed_kernel_read_is_unknown_and_never_death`.
    let mut refused = live;
    refused.holder.pid = 1;
    assert!(matches!(
        local_observation(&refused, dir.path()),
        Observation::Unproven(_)
    ));
}

#[test]
fn activity_without_a_completed_row_cannot_extend_the_progress_bound() {
    assert!(completed_row("compiler still busy").is_none());
    assert!(completed_row(r#"{"event":"stage-started","stage":"compile"}"#).is_none());
    assert!(
        completed_row(r#"{"event":"test-binary-finished","test":"db","outcome":"FAIL"}"#).is_some()
    );
    assert_eq!(
        p95(
            "1\t0.155.0\tverify\t2000\n2\t0.155.0\tbuild\t9000",
            "verify"
        ),
        2000
    );
}

#[test]
fn only_the_private_events_stream_advances_progress_and_partial_rows_wait() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
    let output = dir.path().join("06-verify.log");
    std::fs::write(output, format!("{event}\n")).unwrap();
    let events = dir.path().join("06-verify.events");
    std::fs::write(&events, event).unwrap();
    let mut rows = Rows::default();
    // Negative controls: output that resembles a row, and an incomplete private row.
    assert!(rows.read(dir.path()).is_empty());
    std::fs::write(&events, format!("{event}\n")).unwrap();
    assert_eq!(rows.read(dir.path()).len(), 1);
    assert!(
        rows.read(dir.path()).is_empty(),
        "a poll is not another boundary"
    );
}

#[test]
fn reported_failed_cases_survive_remote_history_without_inventing_names() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cases = [
        Some(aterm_json::json!("first::case")),
        Some(aterm_json::json!("second::case")),
        None,
        Some(aterm_json::Value::Null),
        Some(aterm_json::json!({"unsupported": "name"})),
    ];
    let mut events = String::new();
    for case in &cases {
        let mut event = aterm_json::json!({
            "event": "test-case-failed", "test": "-p sample --lib",
            "outcome": "FAIL", "detail": "its block decides its binary a FAIL",
        });
        if let Some(case) = case {
            event
                .as_object_mut()
                .unwrap()
                .insert("case".into(), case.clone());
        }
        events.push_str(&format!("verify-event: {event}\n"));
    }
    events.push_str(
        "verify-event: {\"event\":\"test-case-failed\",\"test\":\"-p sample --lib\",\"case\":\"spared\",\"outcome\":\"NOT RUN\"}\n",
    );
    std::fs::write(dir.path().join("verify.events"), events).unwrap();
    let mut original = state("holder", 123);
    original.rows = Rows::default().read(dir.path());
    assert_eq!(original.rows.len(), 6);
    let encoded = aterm_toml::to_string(&original).unwrap();
    let decoded = parse_state(&encoded, &reference("0.156.0").unwrap()).unwrap();
    assert_eq!(decoded, original);
    let red = red_items(&decoded);
    let red: Vec<aterm_json::Value> = red
        .split("; ")
        .map(|row| aterm_json::from_str(row).unwrap())
        .collect();
    assert_eq!(red.len(), 5);
    assert_eq!(red[0]["case"].as_str(), Some("first::case"));
    assert_eq!(red[1]["case"].as_str(), Some("second::case"));
    assert_ne!(red[0], red[1]);
    assert!(red[2..].iter().all(|row| row["case"].is_null()));
    assert!(
        red.iter()
            .all(|row| row["outcome"].as_str() == Some("FAIL"))
    );
}

#[test]
fn reported_stage_failures_are_red_before_the_aggregate_check_finishes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut failed = aterm_verify::Report::new("finished stage with sibling pending");
    failed.fail("reported failure");
    failed.cannot_run("unavailable sibling work");
    let mut passed = aterm_verify::Report::new("passing stage");
    passed.pass("checked");
    passed.raw("error: raw text alone is not a verdict");
    let mut unavailable = aterm_verify::Report::new("unavailable stage");
    unavailable.cannot_run("not executed");
    let mut events = String::new();
    for report in [&failed, &passed, &unavailable] {
        events.push_str(&aterm_verify::report::stage_event(
            0,
            Some(aterm_verify::checks::Check::Test),
            report,
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            None,
        ));
    }
    events.push_str("verify-event: {\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"FAIL\",\"detail\":12}]}\n");
    std::fs::write(dir.path().join("verify.events"), events).unwrap();
    let mut original = state("holder", 123);
    original.rows = Rows::default().read(dir.path());
    assert_eq!(original.rows.len(), 4);
    let decoded = parse_state(
        &aterm_toml::to_string(&original).unwrap(),
        &reference("0.156.0").unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, original);
    let red: aterm_json::Value = aterm_json::from_str(&red_items(&decoded)).unwrap();
    assert_eq!(red["event"].as_str(), Some("stage-finished"));
    assert!(red["outcome"].is_null());
    assert_eq!(red["decisions"][0]["outcome"].as_str(), Some("FAIL"));
    assert_eq!(
        red["decisions"][0]["detail"].as_str(),
        Some("reported failure")
    );
    assert_eq!(red["decisions"][1]["outcome"].as_str(), Some("NOT RUN"));
    // An unsupported neighbor cannot erase a separately reported failure.
    let mixed = completed_row(
        r#"{"event":"stage-finished","decisions":[null,{"outcome":"FAIL","detail":"observed"}]}"#,
    )
    .unwrap();
    let mut state = original;
    state.rows = vec![mixed];
    assert!(!red_items(&state).is_empty());
}

#[test]
fn progress_reader_discards_large_lines_without_rereading_partial_bytes() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("06-verify.events");
    let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
    let mut reader = Rows::default();
    // Complete arbitrary output is bounded too, even though it is not JSON.
    std::fs::write(
        &path,
        format!("{}\n{event}\n", "x".repeat(PROGRESS_LINE_BYTES * 3)),
    )
    .unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert!(reader.read(dir.path()).is_empty());

    std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES * 2)).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    let cursor = reader.0.get(&path).unwrap();
    let consumed = cursor.offset;
    assert_eq!(consumed, (PROGRESS_LINE_BYTES * 2) as u64);
    assert!(cursor.partial.is_empty());
    assert!(cursor.discarding);
    assert!(reader.read(dir.path()).is_empty());
    assert_eq!(reader.0[&path].offset, consumed);
    // A valid-looking suffix of the oversized record must not become a row.
    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(append, "{event}\n{event}\n").unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert!(!reader.0[&path].discarding);
    assert!(reader.0[&path].partial.is_empty());
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_reader_keeps_bounded_fragments_and_seeds_old_completed_rows() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.log");
    let event =
        "verify-event: {\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
    std::fs::write(&path, event).unwrap();
    let mut reader = Rows::default();
    reader.seed(dir.path());
    assert!(reader.read(dir.path()).is_empty());
    let split = event.len() / 2;
    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    append.write_all(&event.as_bytes()[..split]).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    assert_eq!(reader.0[&path].partial, event.as_bytes()[..split]);
    assert_eq!(reader.0[&path].offset, (event.len() + split) as u64);
    append.write_all(&event.as_bytes()[split..]).unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_seed_skips_dense_history_and_keeps_its_incomplete_tail() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("06-verify.events");
    let event = "{\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
    let next = "{\"event\":\"check-finished\",\"check\":\"doc\",\"outcome\":\"PASS\"}\n";
    let split = event.len() / 2;
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..10_000 {
        file.write_all(event.as_bytes()).unwrap();
    }
    file.write_all(&event.as_bytes()[..split]).unwrap();
    let mut reader = Rows::default();
    reader.seed(dir.path());
    assert_eq!(reader.0[&path].offset, file.metadata().unwrap().len());
    assert_eq!(reader.0[&path].partial, event.as_bytes()[..split]);
    assert!(reader.read(dir.path()).is_empty());
    file.write_all(&event.as_bytes()[split..]).unwrap();
    file.write_all(next.as_bytes()).unwrap();
    assert_eq!(
        reader.read(dir.path()),
        [completed_row(event).unwrap(), completed_row(next).unwrap()]
    );
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_seed_keeps_oversize_discard_through_the_next_newline() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.log");
    std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES * 2)).unwrap();
    let mut reader = Rows::default();
    reader.seed(dir.path());
    assert_eq!(reader.0[&path].offset, (PROGRESS_LINE_BYTES * 2) as u64);
    assert!(reader.0[&path].partial.is_empty());
    assert!(reader.0[&path].discarding);
    assert!(reader.read(dir.path()).is_empty());
    let event = "{\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(file, "{event}{event}").unwrap();
    assert_eq!(reader.read(dir.path()), [completed_row(event).unwrap()]);
    assert!(!reader.0[&path].discarding);
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_reader_never_joins_fragments_across_replacement_or_observed_truncation() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("06-verify.events");
    let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
    let split = event.len() - 3;
    let mut reader = Rows::default();
    std::fs::write(&path, &event[..split]).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    let old_identity = reader.0[&path].identity;
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, format!("{}\n{event}\n", &event[split..])).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert_ne!(reader.0[&path].identity, old_identity);
    assert!(reader.read(dir.path()).is_empty());

    // Observe a shortened same-inode file while an old prefix is retained.
    std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES / 2)).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    let same_identity = reader.0[&path].identity;
    std::fs::write(&path, format!("{}\n{event}\n", &event[split..])).unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert_eq!(reader.0[&path].identity, same_identity);
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn blocked_publication_cannot_hide_completed_progress_from_the_fleet() {
    const COMPLETIONS: usize = 16_384;
    let dir = aterm_tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    let event = aterm_json::json!({
        "event":"test-binary-finished", "test":"db", "outcome":"FAIL",
        "detail":"x".repeat(256),
    })
    .to_string();
    let recording = format!("{event}\n").repeat(COMPLETIONS);
    let log = logs.join("06-verify.events");
    std::fs::write(&log, &recording).unwrap();
    let mut old = state("m3", 1);
    old.holder.pid = std::process::id();
    (old.holder.boot, old.holder.start) =
        aterm_agent::release_progress::identity(old.holder.pid).unwrap();
    let local = Arc::new(Local::new(
        old.clone(),
        dir.path()
            .join(aterm_agent::release_progress::DIRECTORY)
            .join("line.json"),
    ));
    let lease = Lease {
        repo: dir.path().into(),
        name: "unused".into(),
        sha: "old".into(),
        state: old.clone(),
        local: local.path.clone(),
        previous_holder: None,
    };
    local.update(|state| state.step = "build".into()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publishing = Arc::clone(&local);
    let writer = std::thread::spawn(move || {
        publish_progress_with(lease, &publishing, &rx, &mut |_| {
            entered_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            Ok(true)
        })
    });
    tx.send(Request::Wake).unwrap();
    let entered = entered_rx.recv_timeout(std::time::Duration::from_secs(60));
    // This is the production sampler path, while the publisher is held by a channel.
    let sampled = local.sample(&mut Rows::default(), &logs);
    let observed = recorded(&old.holder, dir.path());
    let current = local.snapshot();
    let local_bytes = std::fs::read(&local.path);
    let recorded_bytes = std::fs::read(&log);
    // Retire the held publisher before any assertion, including I/O failures.
    let stopped = tx.send(Request::Stop);
    let released = release_tx.send(());
    let joined = writer.join();
    entered.unwrap();
    stopped.unwrap();
    released.unwrap();
    joined.unwrap();
    sampled.unwrap();
    let current = current.unwrap();
    let local_bytes = local_bytes.unwrap();
    let full_history = aterm_json::to_vec(&current).unwrap();
    assert!(full_history.len() > 4 * 1024 * 1024);
    assert!(local_bytes.len() < 4 * 1024 * 1024);
    assert_eq!(current.rows.len(), COMPLETIONS);
    let mut expected_local = current.clone();
    expected_local.rows.clear();
    let local_record: State = aterm_json::from_slice(&local_bytes).unwrap();
    assert_eq!(local_record, expected_local);
    assert_eq!(recorded_bytes.unwrap(), recording.as_bytes());
    // Remote serialization still carries every outcome, including red details.
    let remote_text = aterm_toml::to_string(&current).unwrap();
    let remote_record =
        parse_state(&remote_text, &reference(&current.line.version).unwrap()).unwrap();
    assert_eq!(remote_record, current);
    assert!(!red_items(&remote_record).is_empty());
    assert_eq!(observed, Some(expected_local));
    // Negative control: the previous full-history registry shape is omitted by
    // the real survey reader before its own rows projection can run.
    std::fs::write(&local.path, full_history).unwrap();
    assert_eq!(recorded(&old.holder, dir.path()), None);
}

/// The record of `holder` the fleet survey reads from this host's registry.
fn recorded(holder: &Holder, directory: &Path) -> Option<State> {
    aterm_agent::release_progress::survey(directory)
        .iter()
        .filter_map(|line| line.split_once(" data=").map(|(_, data)| data))
        .filter_map(|data| {
            aterm_json::from_str::<aterm_json::Value>(&aterm_codec::pct::decode(data)).ok()
        })
        .find(|record| names(record, holder))
        .and_then(|record| aterm_json::from_value(record).ok())
}

#[cfg(unix)]
fn repository(root: &Path, name: &str, origin: &Path) -> Network<'static> {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    let path = root.join(name);
    std::fs::create_dir(&path).unwrap();
    let git = GitCli::new(&path);
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Andrew Yates"],
        vec!["config", "user.email", "test@example.invalid"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/fixture/app.git",
        ],
    ] {
        git_ok(&git, &args).unwrap();
    }
    Network {
        remote: crate::github_remote::claim_fixture(&path, origin),
        repo: path,
        stopped: &RUNNING,
    }
}

#[cfg(unix)]
#[test]
fn native_line_claim_retries_inventory_with_fresh_policy_and_a_bound() {
    use std::sync::{OnceLock, Weak};
    for mode in [
        "forward",
        "twice",
        "early",
        "conflict",
        "rewind",
        "diverge",
        "bound",
        "every",
        "between",
        "between-ref",
        "policy",
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
        git_ok(
            &GitCli::new(&origin),
            &["config", "user.name", "Andrew Yates"],
        )
        .unwrap();
        git_ok(
            &GitCli::new(&origin),
            &["config", "user.email", "test@example.invalid"],
        )
        .unwrap();
        let mut writer = repository(dir.path(), "writer", &origin);
        let source = GitCli::new(&writer.repo);
        git_ok(&source, &["checkout", "-b", "main"]).unwrap();
        git_ok(&source, &["commit", "--allow-empty", "-m", "base"]).unwrap();
        let base = git_ok(&source, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned();
        git_ok(&source, &["commit", "--allow-empty", "-m", "tip"]).unwrap();
        let initial = git_ok(&source, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned();
        git_ok(&source, &["push", origin.to_str().unwrap(), "main"]).unwrap();
        let name = reference("0.156.0").unwrap();
        let rival = commit(&writer, "", &state("other", 124)).unwrap();
        git_ok(
            &GitCli::new(&origin),
            &[
                "fetch",
                "--no-write-fetch-head",
                writer.repo.to_str().unwrap(),
                &rival,
            ],
        )
        .unwrap();
        let slot = Arc::new(OnceLock::<Weak<agh::Fx>>::new());
        let observed_slot = slot.clone();
        let reads = Arc::new(Mutex::new(0_usize));
        let observed_reads = reads.clone();
        let bare = origin.clone();
        let observed_name = name.clone();
        let early = AtomicBool::new(false);
        let fx = Arc::new(
            agh::Fx::simulated(1_760_000_000).with_git_child_observer_for_tests(move |span| {
                let specs = observed_slot
                    .get()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .children_run();
                let args = &specs[span.ordinal].args;
                let early_move = mode == "early"
                    && args.iter().any(|arg| arg == "--is-shallow-repository")
                    && !early.swap(true, Ordering::SeqCst);
                if !early_move
                    && (!args.iter().any(|arg| arg == "ls-remote")
                        || !args.iter().any(|arg| arg == observed_name.as_str())
                        || !args.iter().any(|arg| arg == "refs/heads/main"))
                {
                    return;
                }
                let mut reads = observed_reads.lock().unwrap();
                if !early_move {
                    *reads += 1;
                }
                let git = GitCli::new(&bare);
                if mode == "between-ref" && *reads == 2 {
                    git_ok(&git, &["update-ref", &observed_name, &rival]).unwrap();
                    return;
                }
                if mode == "between" && *reads == 2 {
                    git_ok(&git, &["update-ref", "refs/heads/main", &initial]).unwrap();
                    return;
                }
                if mode == "early" && !early_move {
                    return;
                }
                if !early_move
                    && *reads != 1
                    && mode != "every"
                    && !(mode == "twice" && *reads == 2)
                    && !(mode == "bound" && *reads % 2 == 1)
                {
                    return;
                }
                let current = git_ok(&git, &["rev-parse", "main"])
                    .unwrap()
                    .stdout_utf8()
                    .trim()
                    .to_owned();
                let tree = git_ok(&git, &["rev-parse", "main^{tree}"])
                    .unwrap()
                    .stdout_utf8()
                    .trim()
                    .to_owned();
                let message = if mode == "policy" {
                    let record = crate::line::Supersession {
                        previous: state("holder", 123).line,
                        replacement: crate::line::Line::start("0.156.0", &current, 2),
                        reason: "concurrent retirement".into(),
                    };
                    format!(
                        "land\n\nAterm-Line-Supersession: {}",
                        aterm_json::to_string(&record).unwrap()
                    )
                } else {
                    "land".into()
                };
                let args = if matches!(mode, "rewind" | "diverge") {
                    vec!["commit-tree", tree.as_str(), "-m", "unrelated"]
                } else {
                    vec![
                        "commit-tree",
                        tree.as_str(),
                        "-p",
                        current.as_str(),
                        "-m",
                        message.as_str(),
                    ]
                };
                let next = if mode == "rewind" {
                    base.clone()
                } else {
                    git_ok(&git, &args).unwrap().stdout_utf8().trim().to_owned()
                };
                let reference = if mode == "conflict" {
                    observed_name.as_str()
                } else {
                    "refs/heads/main"
                };
                git_ok(&git, &["update-ref", reference, &next]).unwrap();
            }),
        );
        assert!(slot.set(Arc::downgrade(&fx)).is_ok());
        writer.remote =
            crate::github_remote::claim_fixture_with_fx(&writer.repo, &origin, fx.clone());
        let mut attempts = 0;
        let mut expected_ref = None;
        let result = inventory_retry(|floor| {
            attempts += 1;
            let (main, _) = claim_main(
                &writer.remote,
                &writer.repo,
                &state("holder", 123).line,
                floor,
            )?;
            remote_expected(&writer, &name, expected_ref.as_deref())?;
            match replace_at_main(&writer, &name, "", &state("holder", 123), Some(&main)) {
                Write::Written(sha) => Ok(ClaimAttempt::Done(sha)),
                Write::InventoryChanged { main, why } => {
                    expected_ref = Some(String::new());
                    Ok(ClaimAttempt::InventoryChanged { main, why })
                }
                Write::Race => Err("conflict".into()),
                Write::Offline(why) => Err(why),
            }
        });
        if matches!(mode, "forward" | "early" | "twice") {
            let sha = result.unwrap();
            assert_eq!(
                git_ok(&GitCli::new(&origin), &["rev-parse", &name])
                    .unwrap()
                    .stdout_utf8()
                    .trim(),
                sha
            );
            assert_eq!(attempts, 2);
            assert_eq!(*reads.lock().unwrap(), if mode == "early" { 3 } else { 4 });
        } else {
            let why = result.unwrap_err();
            assert_eq!(
                attempts,
                match mode {
                    "bound" | "every" => 3,
                    "between" | "between-ref" | "policy" => 2,
                    _ => 1,
                },
                "{mode}: {why}"
            );
            if matches!(mode, "bound" | "every") {
                assert!(why.contains("outside the requested refs"));
            }
            if mode == "policy" {
                assert!(why.contains("superseded on main"));
            }
            if mode == "between-ref" {
                assert!(why.contains("conflict; side_effect=none"));
            }
            if mode == "between" {
                assert!(why.contains("did not advance fast-forward"), "{why}");
            }
            assert!(
                !fx.children_run()
                    .iter()
                    .any(|s| s.args.iter().any(|a| a == "push"))
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn native_fleet_status_reads_the_actual_claim_without_changing_its_holder() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let writer = repository(dir.path(), "writer", &origin);
    let name = reference("0.156.0").unwrap();
    let initial = state("original-holder", 123);
    let Write::Written(sha) = replace(&writer, &name, "", &initial) else {
        panic!("fixture claim must be written");
    };
    let next_name = reference("0.157.0").unwrap();
    let mut next = state("next-holder", 456);
    next.line.version = "0.157.0".into();
    assert!(matches!(
        replace(&writer, &next_name, "", &next),
        Write::Written(_)
    ));
    let reader = repository(dir.path(), "reader", &origin);
    let root = dir.path().join("reader");
    let effects = Arc::new(agh::Fx::simulated(agh::Fx::live().wall_secs().unwrap()));
    let remote = crate::github_remote::tests::local_with_fx(&root, &origin, effects.clone());
    assert_eq!(
        status_rows(&remote).unwrap(),
        [
            "v0.156.0 original-holder original-holder step=verify boundary=test-binary-finished at=123 head=base",
            "v0.157.0 next-holder next-holder step=verify boundary=test-binary-finished at=456 head=base"
        ]
    );
    let children = effects.children_run();
    for (verb, expected) in [("ls-remote", 1), ("fetch", 1)] {
        assert_eq!(
            children
                .iter()
                .filter(|child| child.args.iter().any(|arg| arg == verb))
                .count(),
            expected,
            "fleet status must import one advertised snapshot: {verb}"
        );
    }
    assert_eq!(
        crate::ledger::rev_parse(&GitCli::new(&origin), &name).unwrap(),
        sha
    );
    assert!(
        git_ok(&reader, &["for-each-ref", "--format=%(refname)"])
            .unwrap()
            .stdout_utf8()
            .lines()
            .all(|name| name.starts_with("refs/agh/"))
    );
    assert!(!root.join(".git/FETCH_HEAD").exists());
    // Move the real fixture ref after Git has produced the namespace advertisement.
    // The report must keep that advertisement's immutable record rather than mix in
    // the holder that a later exact ref lookup would see.
    let advertisement = children
        .iter()
        .find(|child| child.args.iter().any(|arg| arg == "ls-remote"))
        .unwrap();
    let moving_writer = repository(dir.path(), "moving-writer", &origin);
    moving_writer.import(&sha).unwrap();
    let moved_name = name.clone();
    let moved_from = sha.clone();
    let moved = state("moved-holder", 789);
    let race = agh::Fx::simulated(agh::Fx::live().wall_secs().unwrap())
        .with_git_intercept_once_for_tests(
            advertisement.program.clone(),
            advertisement.args.clone(),
            || {},
            move |_| {
                let written = replace(&moving_writer, &moved_name, &moved_from, &moved);
                assert!(matches!(written, Write::Written(_)), "{written:?}");
            },
        );
    let racing = crate::github_remote::tests::local_with_fx(&root, &origin, Arc::new(race));
    assert_eq!(
        status_rows(&racing).unwrap(),
        [
            "v0.156.0 original-holder original-holder step=verify boundary=test-binary-finished at=123 head=base",
            "v0.157.0 next-holder next-holder step=verify boundary=test-binary-finished at=456 head=base"
        ]
    );
    // Negative control: a fresh exact lookup really sees the concurrent replacement.
    let moved_to = racing.get(&name).unwrap().unwrap();
    assert_ne!(moved_to, sha);
    let moved_bytes = racing
        .fetch_exact(&moved_to)
        .and_then(|()| racing.blob(&moved_to, Path::new("line.toml")))
        .unwrap();
    assert_eq!(
        parse_state(std::str::from_utf8(&moved_bytes).unwrap(), &name)
            .unwrap()
            .holder
            .host,
        "moved-holder"
    );
    writer.import(&moved_to).unwrap();
    let mut wrong = initial;
    wrong.line.version = "0.155.0".into();
    assert!(matches!(
        replace(&writer, &name, &moved_to, &wrong),
        Write::Written(_)
    ));
    assert!(
        status_rows(&remote)
            .unwrap_err()
            .contains("version does not match")
    );
    git_ok(&GitCli::new(&origin), &["update-ref", "-d", &name]).unwrap();
    git_ok(&GitCli::new(&origin), &["update-ref", "-d", &next_name]).unwrap();
    let before = effects.children_run().len();
    assert!(status_rows(&remote).unwrap().is_empty());
    let children = effects.children_run();
    for (verb, expected) in [("ls-remote", 1), ("fetch", 0)] {
        assert_eq!(
            children[before..]
                .iter()
                .filter(|child| child.args.iter().any(|arg| arg == verb))
                .count(),
            expected,
            "an empty namespace needs no object transfer: {verb}"
        );
    }
}

#[cfg(unix)]
#[test]
fn two_macs_have_one_holder_and_stalled_cut_is_not_lost() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let a = repository(dir.path(), "a", &origin);
    let b = repository(dir.path(), "b", &origin);
    // Before B6 each clone could acquire its own run claim for the same version.
    // These are the existing production local locks, not a model of them.
    let local_a = crate::release::RunClaim::take(
        &dir.path().join("a/running"),
        (101, "start-a"),
        dir.path(),
        &mut |_| None,
    )
    .unwrap();
    let local_b = crate::release::RunClaim::take(
        &dir.path().join("b/running"),
        (102, "start-b"),
        dir.path(),
        &mut |_| None,
    )
    .unwrap();
    let baseline_holders = [&local_a, &local_b].len();
    assert_eq!(baseline_holders, 2);
    let name = reference("0.156.0").unwrap();
    let initial = state("m3", 1);
    // Both Macs read the absent ref. Exactly one CAS can install a holder.
    let first = match replace(&a, &name, "", &initial) {
        Write::Written(sha) => sha,
        other => panic!("{other:?}"),
    };
    let mut holder = Lease {
        repo: dir.path().join("a"),
        name: name.clone(),
        sha: first.clone(),
        state: initial.clone(),
        local: dir.path().join("local.json"),
        previous_holder: None,
    };
    git_ok(
        &a,
        &[
            "remote",
            "set-url",
            "origin",
            dir.path().join("unavailable").to_str().unwrap(),
        ],
    )
    .unwrap();
    let stopped = AtomicBool::new(false);
    assert_eq!(
        holder.write(&stopped),
        Ok(false),
        "a transport fault is pending, never ownership loss"
    );
    assert_eq!(holder.sha, first);
    git_ok(
        &a,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture/app.git",
        ],
    )
    .unwrap();
    assert_eq!(
        holder.write_with(&a),
        Ok(true),
        "the same candidate continues once transport returns"
    );
    let first = holder.sha.clone();
    assert_eq!(replace(&b, &name, "", &state("m22", 1)), Write::Race);
    let (old_sha, old) = remote(&b, &name).unwrap().unwrap();
    assert_eq!(old_sha, first);
    assert!(admit(&old, &old.line, 1801, unreachable).is_err());
    assert!(admit(&old, &old.line, 1802, unreachable).is_ok());
    let mut adopted = old.clone();
    adopted.holder = state("m22", 1802).holder;
    adopted.at = 1802;
    let new = match replace(&b, &name, &old_sha, &adopted) {
        Write::Written(sha) => sha,
        other => panic!("{other:?}"),
    };
    assert_eq!(replace(&a, &name, &first, &initial), Write::Race);
    assert!(
        holder.write_with(&a).is_err(),
        "positive ownership loss stops the old holder"
    );
    assert_eq!(
        remote(&a, &name).unwrap().unwrap().1.line,
        initial.line,
        "the exact candidate survives takeover"
    );
    // Negative control: removing the expected-old lease lets the stale holder overwrite
    // the adopter. The same stale write with the lease above was refused.
    let stale = commit(&a, &new, &initial).unwrap();
    git_ok(
        &a,
        &[
            "-c",
            "protocol.file.allow=always",
            "push",
            "--force",
            origin.to_str().unwrap(),
            &format!("{stale}:{name}"),
        ],
    )
    .unwrap();
    assert_eq!(remote(&b, &name).unwrap().unwrap().1.holder, initial.holder);
    println!(
        "B6 measured: before holders/version={baseline_holders}; after holders/version=1; unfenced stale overwrite=1; leased stale overwrite=0; stalled adoptions=1; cuts lost=0"
    );
}

#[cfg(unix)]
#[test]
fn a_stopped_or_retargeted_monitor_cannot_publish_a_claim() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let initial = repository(dir.path(), "writer", &origin);
    let stopped = AtomicBool::new(true);
    let writer = Network {
        repo: initial.repo,
        remote: initial.remote,
        stopped: &stopped,
    };
    let name = reference("0.156.0").unwrap();
    assert!(matches!(
        replace(&writer, &name, "", &state("m3", 1)),
        Write::Offline(_)
    ));
    stopped.store(false, Ordering::SeqCst);
    git_ok(
        &writer,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/foreign/app.git",
        ],
    )
    .unwrap();
    assert!(matches!(
        replace(&writer, &name, "", &state("m3", 1)),
        Write::Offline(_)
    ));
    assert!(
        git_ok(&GitCli::new(&origin), &["for-each-ref"])
            .unwrap()
            .stdout
            .is_empty()
    );
}

/// A fresh git-backed origin and two Macs' clones of it, with the 0.199.0 orphan installed.
#[cfg(unix)]
fn orphaned_origin(dir: &Path) -> (Network<'static>, Network<'static>, String, String) {
    let origin = dir.join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let a = repository(dir, "a", &origin);
    let b = repository(dir, "b", &origin);
    let name = reference("0.199.0").unwrap();
    let Write::Written(sha) = replace(&a, &name, "", &orphan()) else {
        panic!("the orphan's claim is written");
    };
    (a, b, name, sha)
}

/// A claim store whose next compare-and-swap first lets `before_write` run: another cutter's
/// whole take, landing between this cutter's read and its write.
#[cfg(unix)]
struct Racing<'a> {
    inner: &'a Network<'a>,
    before_write: std::cell::RefCell<Option<Box<dyn FnOnce() + 'a>>>,
}

#[cfg(unix)]
impl GitRunner for Racing<'_> {
    fn git(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
        self.inner.git(args)
    }
    fn git_input(
        &self,
        args: &[&str],
        input: &[u8],
    ) -> crate::ledger::Result<crate::ledger::RunOut> {
        self.inner.git_input(args, input)
    }
}

#[cfg(unix)]
impl ClaimStore for Racing<'_> {
    fn ref_oid(&self, name: &str) -> Result<Option<String>, String> {
        self.inner.ref_oid(name)
    }
    fn import(&self, oid: &str) -> Result<(), String> {
        self.inner.import(oid)
    }
    fn compare_and_swap(&self, name: &str, before: &str, after: &str, main: Option<&str>) -> Write {
        if let Some(race) = self.before_write.borrow_mut().take() {
            race();
        }
        self.inner.compare_and_swap(name, before, after, main)
    }
}

/// A taker on `node` whose clock reads `at` and whose view of every holder's Mac is `seen`.
#[cfg(unix)]
fn taker<'a>(
    dir: &'a Path,
    holder: &'a Holder,
    at: &'a dyn Fn() -> u64,
    seen: &'a dyn Fn(&State) -> Observation,
) -> Taker<'a> {
    Taker {
        repo: dir,
        holder,
        registry: dir,
        take: Take::Contend,
        now: at,
        observe: seen,
    }
}

/// Two cutters take 0.199.0's expired lease through the production attempt, the second landing
/// between the first's read and its write: exactly one is written, the other's write races, and
/// its next attempt reads the winner's fresh lease and is refused. The old holder, waking, cannot
/// renew and stops.
/// NEGATIVE CONTROL: the loser's write from the stale read races and leaves the winner's ref; the
/// same write from the fresh read lands, so the expected-old ref alone decides the winner.
#[cfg(unix)]
#[test]
fn two_cutters_racing_for_an_expired_lease_produce_one_winner() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, name, orphan_sha) = orphaned_origin(dir.path());
    let line = orphan().line;
    let (m73, m74) = (state("m73", NOW).holder, state("m74", NOW).holder);
    let clock = || NOW;
    let unseen = |_: &State| unreachable();
    let won = std::cell::RefCell::new(None);
    let racing = Racing {
        inner: &a,
        before_write: std::cell::RefCell::new(Some(Box::new(|| {
            let taken = attempt(
                &b,
                &taker(dir.path(), &m74, &clock, &unseen),
                &name,
                &line,
                None,
                None,
                None,
            );
            *won.borrow_mut() = Some(taken);
        }))),
    };
    let first = attempt(
        &racing,
        &taker(dir.path(), &m73, &clock, &unseen),
        &name,
        &line,
        None,
        None,
        None,
    )
    .unwrap();
    assert!(matches!(first, Attempt::Race));
    let Some(Ok(Attempt::Taken(winner))) = won.take() else {
        panic!("the cutter that wrote first holds the line");
    };
    assert_eq!(winner.state.holder, m74);
    let (winner_sha, held) = remote(&a, &name).unwrap().unwrap();
    assert_eq!(
        (winner_sha.as_str(), &held.holder),
        (winner.sha.as_str(), &m74)
    );
    let later = || NOW + 60;
    let Err(refused) = attempt(
        &a,
        &taker(dir.path(), &m73, &later, &unseen),
        &name,
        &line,
        None,
        None,
        None,
    ) else {
        panic!("the loser's next attempt is refused");
    };
    assert!(
        refused.starts_with("CUTTING ELSEWHERE: v0.199.0 by m74"),
        "{refused}"
    );

    // m9 wakes: its next renewal or audit finds the lease taken and refuses to go on.
    let mut m9 = Lease {
        repo: dir.path().join("a"),
        name: name.clone(),
        sha: orphan_sha.clone(),
        state: orphan(),
        local: dir.path().join("m9.json"),
        previous_holder: None,
    };
    m9.state.at = NOW + 60;
    assert_eq!(
        m9.write_with(&a),
        Err("COULD NOT RUN: fleet line adopted by another holder".into())
    );
    // Negative control: the CAS, not the order of the calls, picks the winner.
    let mut loser = held.clone();
    loser.holder = m73.clone();
    assert_eq!(replace(&a, &name, &orphan_sha, &loser), Write::Race);
    assert_eq!(remote(&a, &name).unwrap().unwrap().0, winner_sha);
    assert!(matches!(
        replace(&a, &name, &winner_sha, &loser),
        Write::Written(_)
    ));
    assert_eq!(remote(&b, &name).unwrap().unwrap().1.holder, m73);
}

/// The production renewal: a completed row the sampler reads advances the lease, and the
/// progress monitor writes it to the ref, where another cutter is refused until a whole bound
/// after it.
/// NEGATIVE CONTROL: with no completed row the monitor renews nothing, and the same cutter takes
/// the same claim.
#[cfg(unix)]
#[test]
fn a_holder_renewing_by_completed_rows_is_never_taken() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, name, sha) = orphaned_origin(dir.path());
    let logs = dir.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    let events = logs.join("06-verify.events");
    std::fs::write(&events, "").unwrap();
    let local = Local::new(orphan(), dir.path().join("holder.json"));
    let lease = || Lease {
        repo: dir.path().join("a"),
        name: name.clone(),
        sha: sha.clone(),
        state: orphan(),
        local: local.path.clone(),
        previous_holder: None,
    };
    let monitor = |lease: Lease| {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Request::Wake).unwrap();
        drop(tx);
        publish_progress_with(lease, &local, &rx, &mut |lease| lease.write_with(&a));
    };
    let contender = orphan().line;
    let mut rows = Rows::default();
    rows.seed(&logs);

    local.sample(&mut rows, &logs).unwrap();
    monitor(lease());
    let (read, held) = remote(&b, &name).unwrap().unwrap();
    assert_eq!((read.as_str(), held.at), (sha.as_str(), orphan().at));
    assert!(admit(&held, &contender, NOW, unreachable).is_ok());

    let row = aterm_json::json!({"event": "check-finished", "check": "appkit", "outcome": "ok"});
    std::fs::write(&events, format!("{row}\n")).unwrap();
    let before = now();
    local.sample(&mut rows, &logs).unwrap();
    monitor(lease());
    let (read, held) = remote(&b, &name).unwrap().unwrap();
    assert_ne!(read, sha);
    assert!(held.at >= before, "{} < {before}", held.at);
    assert!(admit(&held, &contender, held.at + 1800, unreachable).is_err());
    assert!(admit(&held, &contender, held.at + 1801, unreachable).is_ok());
}

/// An audit, which precedes each signature and publication, renews the lease it proves: the
/// holder passing it holds the line a whole bound from then.
/// NEGATIVE CONTROL: a plain wake with nothing new writes nothing and renews nothing.
#[test]
fn an_audit_renews_the_lease() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let old = state("m3", 1);
    let local = Arc::new(Local::new(old.clone(), dir.path().join("line.json")));
    let lease = Lease {
        repo: dir.path().into(),
        name: "unused".into(),
        sha: "old".into(),
        state: old,
        local: local.path.clone(),
        previous_holder: None,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let (wrote_tx, wrote_rx) = std::sync::mpsc::channel();
    let publishing = Arc::clone(&local);
    let writer = std::thread::spawn(move || {
        publish_progress_with(lease, &publishing, &rx, &mut |lease| {
            wrote_tx.send(lease.state.at).unwrap();
            Ok(true)
        });
    });
    tx.send(Request::Wake).unwrap();
    let before = now();
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    tx.send(Request::Audit(reply_tx)).unwrap();
    let audited = reply_rx.recv_timeout(std::time::Duration::from_secs(60));
    tx.send(Request::Stop).unwrap();
    writer.join().unwrap();
    assert_eq!(audited, Ok(Ok(true)));
    let written: Vec<u64> = wrote_rx.try_iter().collect();
    assert_eq!(written.len(), 1, "the wake wrote nothing: {written:?}");
    assert!(written[0] >= before, "{written:?}");
    assert!(local.snapshot().unwrap().at >= before);
}

/// `--supersede` takes the line from any holder, a renewing one included, through the same
/// compare-and-swap; a contender and a continuation that is not proven do not.
/// NEGATIVE CONTROLS: a contender against the same fresh lease is refused, and a `Continue` take
/// whose identity does not continue the holder is judged as a contender.
#[cfg(unix)]
#[test]
fn a_supersession_takes_the_line_from_any_holder_and_a_contender_does_not() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, _b, name, orphan_sha) = orphaned_origin(dir.path());
    let line = orphan().line;
    let mut renewing = orphan();
    renewing.at = NOW;
    let Write::Written(fresh) = replace(&a, &name, &orphan_sha, &renewing) else {
        panic!("the holder renews");
    };
    let m73 = state("m73", NOW).holder;
    let clock = || NOW + 1;
    let alive = |_: &State| Observation::Unproven("its process runs".into());
    let with = |take| Taker {
        take,
        ..taker(dir.path(), &m73, &clock, &alive)
    };
    for take in [Take::Contend, Take::Continue] {
        let refused = attempt(&a, &with(take), &name, &line, None, None, None);
        assert!(
            matches!(&refused, Err(why) if why.starts_with("CUTTING ELSEWHERE")),
            "{take:?}"
        );
        assert_eq!(remote(&a, &name).unwrap().unwrap().0, fresh);
    }
    let Ok(Attempt::Taken(taken)) =
        attempt(&a, &with(Take::Supersede), &name, &line, None, None, None)
    else {
        panic!("a supersession takes the line");
    };
    assert_eq!(remote(&a, &name).unwrap().unwrap().1.holder, m73);
    assert_eq!(taken.state.holder, m73);
}

/// The holder of the 0.199.0 fixture line with its heartbeat's clock and process tree in the
/// test's hands: `t` is seconds since the claim's last renewal, and its tree is three processes
/// that used a core since the look before, or did nothing.
#[cfg(unix)]
struct Heart<'a> {
    store: &'a Network<'static>,
    name: String,
    lease: Lease,
    local: Local,
    pulse: Pulse,
    base: std::time::Instant,
    ran_s: u64,
}

#[cfg(unix)]
impl<'a> Heart<'a> {
    fn new(dir: &Path, store: &'a Network<'static>, name: &str, sha: &str) -> Self {
        let local = Local::new(orphan(), dir.join("holder.json"));
        Self {
            store,
            name: name.into(),
            lease: Lease {
                repo: dir.join("a"),
                name: name.into(),
                sha: sha.into(),
                state: orphan(),
                local: local.path.clone(),
                previous_holder: None,
            },
            local,
            pulse: Pulse::new(1),
            base: std::time::Instant::now(),
            ran_s: 0,
        }
    }

    /// The production heartbeat's one look at `t`: whether it renewed the holder's lease.
    fn look(&mut self, t: u64, working: bool) -> bool {
        if working {
            self.ran_s += PULSE_LOOK.as_secs();
        }
        let at = self.base + std::time::Duration::from_secs(t);
        let work = aterm_toolchain::stall::Work {
            cpu_ns: self.ran_s * 1_000_000_000,
            runnable_ns: self.ran_s * 1_000_000_000,
            members: 3,
            at: Some(at),
            ..Default::default()
        };
        self.local
            .beat(&mut self.pulse, &mut || (Some(work), at), orphan().at + t)
    }

    /// The production monitor's write of the holder's state as it stands, to the ref.
    fn write(&mut self) -> Result<bool, String> {
        self.lease.state = self.local.snapshot().unwrap();
        self.lease.write_with(self.store)
    }

    /// Looks every [`PULSE_LOOK`] over `(from, to]`, each renewal written to the ref: the times
    /// it renewed at.
    fn run(&mut self, from: u64, to: u64, working: bool) -> Vec<u64> {
        let step = PULSE_LOOK.as_secs();
        (from / step + 1..=to / step)
            .map(|look| look * step)
            .filter(|t| {
                let renewed = self.look(*t, working);
                if renewed {
                    assert_eq!(self.write(), Ok(true), "the renewal at {t} is written");
                }
                renewed
            })
            .collect()
    }

    /// The claim as the ref holds it.
    fn held(&self) -> State {
        remote(self.store, &self.name).unwrap().unwrap().1
    }
}

/// What a contender on m73 gets of the fixture line at `t`: the production take, whose view of
/// the holder's Mac is no answer.
#[cfg(unix)]
fn contend(dir: &Path, store: &Network<'static>, name: &str, t: u64) -> Result<Attempt, String> {
    let m73 = state("m73", 0).holder;
    let clock = move || orphan().at + t;
    let unseen = |_: &State| unreachable();
    attempt(
        store,
        &taker(dir, &m73, &clock, &unseen),
        name,
        &orphan().line,
        None,
        None,
        None,
    )
}

/// R24 (0.200.0, 2026-11-12: m73, 36 minutes into a qualification row, lost its line to m86's
/// pump). A holder that spends three whole leases in one row, completing nothing, renews by its
/// heartbeat every five minutes while its processes work, and a contender is refused throughout,
/// told when the lease runs out and what the holder last showed.
/// NEGATIVE CONTROL: the same holder with no heartbeat renews nothing, and the same contender
/// takes its line one second after the first lease.
#[cfg(unix)]
#[test]
fn a_holder_in_one_long_row_renews_by_heartbeat_and_is_not_taken() {
    let lease = lease_s(&orphan());
    assert_eq!(lease, 1800, "the fixture's step has no recorded p95");
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, name, sha) = orphaned_origin(dir.path());
    let mut holder = Heart::new(dir.path(), &a, &name, &sha);
    let mut renewals = Vec::new();
    for end in (600..=3 * lease).step_by(600) {
        renewals.extend(holder.run(end - 600, end, true));
        let Err(refused) = contend(dir.path(), &b, &name, end) else {
            panic!("the contender took a working holder's line at {end}");
        };
        let until = orphan().at + end + lease;
        assert!(
            refused.starts_with("CUTTING ELSEWHERE: v0.199.0 by m9")
                && refused.contains(&format!("runs until {until} ({lease}s from now; "))
                && refused.contains(
                    "its last evidence of progress: its process tree, 3 processes (3 60 s \
                     earlier), ran 60 s on cores and wrote 0 MiB in those 60 s; "
                ),
            "{refused}"
        );
    }
    let every_five_minutes: Vec<u64> = (PULSE_RENEW_S..=3 * lease).step_by(300).collect();
    assert_eq!(renewals, every_five_minutes);
    assert_eq!(holder.held().holder, orphan().holder);
    assert!(holder.held().rows.is_empty(), "no row completed");

    // Negative control: no heartbeat, the same row, the same contender.
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, name, _) = orphaned_origin(dir.path());
    assert!(contend(dir.path(), &b, &name, lease).is_err());
    let Ok(Attempt::Taken(taken)) = contend(dir.path(), &b, &name, lease + 1) else {
        panic!("without a heartbeat the line is taken in the middle of the row");
    };
    assert_eq!(taken.state.holder.node, "m73");
    assert_eq!(remote(&a, &name).unwrap().unwrap().1.holder.node, "m73");
    // The taker's own claim is never written with no p95.
    assert_eq!(taken.state.p95_s, default_p95_s("commit"));
    assert_ne!(taken.state.p95_s, 0);
}

/// A hung holder still loses its line. Its heartbeat renews only on a look that found its
/// processes working, so once they stop, it stops: the last renewal (here the look half a
/// minute after, which still reads the minute before it) is no later than a minute after the
/// work ended, and a contender takes the line one lease after that renewal, by the lease
/// alone. A tree that cannot be read is no evidence either.
/// NEGATIVE CONTROL: while the same holder's processes work, the same looks renew.
#[cfg(unix)]
#[test]
fn a_holder_whose_processes_stop_stops_renewing_and_is_taken_after_one_lease() {
    let lease = lease_s(&orphan());
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, name, sha) = orphaned_origin(dir.path());
    let mut holder = Heart::new(dir.path(), &a, &name, &sha);
    let stopped = 1170;
    assert_eq!(holder.run(0, stopped, true), [300, 600, 900]);
    // Its processes hang: of every later look, for a lease and more, one renews.
    assert_eq!(holder.run(stopped, stopped + lease + 900, false), [1200]);
    let last = holder.held().at - orphan().at;
    assert_eq!(last, 1200);
    assert!(last <= stopped + 60);
    assert!(contend(dir.path(), &b, &name, last + lease).is_err());
    let Ok(Attempt::Taken(taken)) = contend(dir.path(), &b, &name, last + lease + 1) else {
        panic!("a hung holder's line is taken once its lease lapses");
    };
    assert_eq!(taken.state.holder.node, "m73");

    // A look that reads no tree is no evidence, and the read after it is a first read again.
    let mut pulse = Pulse::new(1);
    let base = std::time::Instant::now();
    let at = |t| base + std::time::Duration::from_secs(t);
    let work = |ran_s: u64, t| {
        Some(aterm_toolchain::stall::Work {
            runnable_ns: ran_s * 1_000_000_000,
            members: 3,
            at: Some(at(t)),
            ..Default::default()
        })
    };
    assert_eq!(
        pulse.look(work(0, 0), at(0)),
        None,
        "a first read shows no rate"
    );
    assert!(pulse.look(work(30, 30), at(30)).is_some());
    assert_eq!(pulse.look(None, at(60)), None);
    assert_eq!(pulse.look(work(90, 90), at(90)), None);
    assert!(pulse.look(work(120, 120), at(120)).is_some());
}

/// A heartbeat renewal's write that fails is retried and never ends the cut: a transport fault
/// and a ref this same holder already moved both leave it pending. One that finds another
/// holder on the ref is the loss of the line, at once: the monitor ends the run on that error
/// (`fail`, the stop `release_flow`'s lost-line tests drive).
/// NEGATIVE CONTROL: until another holder is on the ref, the same renewal is written.
#[cfg(unix)]
#[test]
fn a_heartbeat_renewal_that_meets_another_holder_ends_the_run() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, name, sha) = orphaned_origin(dir.path());
    let mut holder = Heart::new(dir.path(), &a, &name, &sha);
    assert_eq!(holder.run(0, 300, true), [300]);
    let first = holder.lease.sha.clone();

    // The network goes: the renewal at 600 is pending, never ownership loss.
    let origin = |url: &str| git_ok(&a, &["remote", "set-url", "origin", url]).unwrap();
    origin(dir.path().join("unavailable").to_str().unwrap());
    let offline = AtomicBool::new(false);
    for t in (330..=600).step_by(30) {
        assert_eq!(holder.look(t, true), t == 600);
    }
    holder.lease.state = holder.local.snapshot().unwrap();
    assert_eq!(holder.lease.write(&offline), Ok(false));
    assert_eq!(holder.lease.sha, first);
    assert_eq!(remote(&b, &name).unwrap().unwrap().1.at, orphan().at + 300);
    origin("https://github.com/fixture/app.git");
    assert_eq!(
        holder.write(),
        Ok(true),
        "the retry lands once transport returns"
    );
    assert_eq!(holder.held().at, orphan().at + 600);

    // The ref moved under this holder's own name (a write whose answer was lost): pending
    // too, and the next try is written from the ref as it stands.
    let mut answered = holder.local.snapshot().unwrap();
    answered.boundary = "written, its answer lost".into();
    let Write::Written(moved) = replace(&a, &name, &holder.lease.sha, &answered) else {
        panic!("the holder's own write lands");
    };
    for t in (630..=900).step_by(30) {
        assert_eq!(holder.look(t, true), t == 900);
    }
    assert_eq!(
        holder.write(),
        Ok(false),
        "a ref this holder moved is no loss"
    );
    assert_eq!(holder.lease.sha, moved);
    assert_eq!(holder.write(), Ok(true));
    assert_eq!(holder.held().at, orphan().at + 900);

    // Another holder takes the line by a person's decision; the next heartbeat's renewal
    // meets it.
    let m73 = state("m73", 0).holder;
    let clock = || orphan().at + 901;
    let unseen = |_: &State| unreachable();
    let superseding = Taker {
        take: Take::Supersede,
        ..taker(dir.path(), &m73, &clock, &unseen)
    };
    let line = orphan().line;
    let taken = attempt(&b, &superseding, &name, &line, None, None, None);
    assert!(matches!(taken, Ok(Attempt::Taken(_))));
    for t in (930..=1200).step_by(30) {
        assert_eq!(holder.look(t, true), t == 1200);
    }
    assert_eq!(
        holder.write(),
        Err("COULD NOT RUN: fleet line adopted by another holder".into())
    );
    assert_eq!(holder.held().holder, m73);
}

/// A heartbeat alone does not hold a line for ever: processes that work without the run ever
/// completing a row or a step are renewed for [`PULSE_ALONE_S`] and no longer, and the run's
/// next row starts that count again. Nothing renews twice within [`PULSE_RENEW_S`].
#[test]
fn a_tree_that_works_without_finishing_anything_is_not_renewed_for_ever() {
    let mut pulse = Pulse::new(1);
    let mut held = orphan();
    let start = held.at;
    let evidence = || Some("its processes work".to_string());
    assert!(!pulse.beat(&mut held, evidence(), start + PULSE_RENEW_S - 1));
    assert!(!pulse.beat(&mut held, None, start + PULSE_RENEW_S));
    assert!(pulse.beat(&mut held, evidence(), start + PULSE_RENEW_S));
    assert_eq!(
        (held.at, held.evidence.as_str()),
        (start + 300, "its processes work")
    );
    let last = start + PULSE_RENEW_S - 1 + PULSE_ALONE_S;
    assert!(pulse.beat(&mut held, evidence(), last));
    assert!(!pulse.beat(&mut held, evidence(), last + PULSE_RENEW_S));
    assert_eq!(held.at, last);
    held.rows.push("a row".into());
    assert!(pulse.beat(&mut held, evidence(), last + PULSE_RENEW_S));
}

/// The p95 a holder records for its step is real. A step this Mac's history holds no time for
/// gets its stated default, so its lease is not the half-hour floor; a step whose own row is
/// missing or a resume's remainder is as long as its longest part.
/// NEGATIVE CONTROLS: a step with history takes its history, not the default; another step's
/// rows, and a step whose name only begins the same, are not its history.
#[test]
fn the_p95_of_a_step_without_history_is_its_default_not_zero() {
    for step in [
        "checks", "sync", "commit", "qualify", "bundle", "ready", "publish", "cut",
    ] {
        assert_eq!(p95("", step), default_p95_s(step), "{step}");
        assert!(default_p95_s(step) >= 900, "{step}");
    }
    assert_eq!(p95("", "qualify"), 5_400);
    let mut first_cut = orphan();
    first_cut.p95_s = p95("1\t0.200.0\tchecks\t1200\n", &first_cut.step);
    assert_eq!(lease_s(&first_cut), 10_800);
    // This Mac's steps.tsv after 0.199.0: three qualifications that went red or resumed, and
    // one whole `qualify` row, of a resume.
    let m86 = "1794517762\t0.199.0\tqualify-appkit\t417\n\
               1794524749\t0.199.0\tqualify-export\t956\n\
               1794524749\t0.199.0\tqualify-build\t4718\n\
               1794530028\t0.199.0\tqualify-build\t917\n\
               1794538309\t0.199.0\tqualify-tree\t3902\n\
               1794540381\t0.199.0\tcheck\t1\n\
               1794540381\t0.199.0\tchecks\t1357\n\
               1794540799\t0.199.0\tqualify\t405\n\
               1794541954\t0.199.0\tpublish\t655\n";
    assert_eq!(p95(m86, "qualify"), 4_718);
    assert_eq!(p95(m86, "checks"), 1_357);
    assert_eq!(p95(m86, "check"), 1);
    assert_eq!(p95(m86, "publish"), 655);
    assert_eq!(p95(m86, "bundle"), 900);
}
