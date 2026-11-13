// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The saved worker lifetime through real keeper recovery and the new host's event loop.

use super::*;
use aterm_agent::harness::relaunch::{ProcessLifetime, ProcessLiveness};
use aterm_sysprobe::jobs::{FdKind, FdReading, JobsReader, KnoteFilter, MAX_KNOTES, ReadReq};
use aterm_toml::Value;

const DESK_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

fn journal(state: &Path, pid: u32) -> Option<PathBuf> {
    let prefix = format!("journal-{pid}-");
    let mut paths: Vec<_> = std::fs::read_dir(state)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".toml"))
        })
        .collect();
    paths.sort();
    assert!(paths.len() <= 1, "one journal for this instance: {paths:?}");
    paths.pop()
}

/// Change only the terminal leaf played by the real foreground actor. The dead host's
/// journal header, session identity, screen, other leaves and metadata stay as written.
fn with_worker(value: Value, local: u64, agent: &Value, claim: &str) -> (Value, usize) {
    match value {
        Value::Table(mut table)
            if table.get("kind").and_then(Value::as_str) == Some("terminal")
                && table.get("local_id").and_then(Value::as_integer)
                    == i64::try_from(local).ok() =>
        {
            assert!(
                table.get("agent").is_none(),
                "the fixture started no real agent"
            );
            assert!(
                table.get("task").is_none(),
                "the fixture started no real task"
            );
            table.insert("agent".into(), agent.clone());
            table.insert("claim_worker".into(), Value::String(claim.into()));
            (Value::Table(table), 1)
        }
        Value::Table(table) => {
            let mut changed = 0;
            let table = table
                .into_iter()
                .map(|(key, value)| {
                    let (value, count) = with_worker(value, local, agent, claim);
                    changed += count;
                    (key, value)
                })
                .collect();
            (Value::Table(table), changed)
        }
        Value::Array(values) => {
            let mut changed = 0;
            let values = values
                .into_iter()
                .map(|value| {
                    let (value, count) = with_worker(value, local, agent, claim);
                    changed += count;
                    value
                })
                .collect();
            (Value::Array(values), changed)
        }
        value => (value, 0),
    }
}

fn plant_worker(
    path: &Path,
    writer: u32,
    local: u64,
    lifetime: &ProcessLifetime,
    claim: &str,
    program: &str,
) {
    let text = std::fs::read_to_string(path).expect("the positively reaped host's journal");
    let before: Value = aterm_toml::from_str(&text).expect("the host wrote canonical TOML");
    let header = before
        .get("journal")
        .cloned()
        .expect("the original journal header");
    assert_eq!(
        header.get("pid").and_then(Value::as_integer),
        Some(i64::from(writer))
    );
    let saved_lifetime = aterm_toml::from_str(&aterm_toml::to_string(lifetime).unwrap()).unwrap();
    let agent = Value::Table(
        [
            ("pid".into(), Value::Integer(i64::from(lifetime.pid()))),
            (
                "start".into(),
                Value::String(
                    aterm_agent::harness::restart_stamp::kernel_start(lifetime.pid()).unwrap(),
                ),
            ),
            ("lifetime".into(), saved_lifetime),
            ("program".into(), Value::String(program.into())),
            (
                "argv".into(),
                Value::Array(vec![Value::String(program.into())]),
            ),
            ("cwd".into(), Value::String(claim.into())),
            ("word".into(), Value::String("claude".into())),
        ]
        .into_iter()
        .collect(),
    );
    let (after, changed) = with_worker(before, local, &agent, claim);
    assert_eq!(
        changed, 1,
        "augment exactly the actor's original terminal leaf"
    );
    assert_eq!(
        after.get("journal"),
        Some(&header),
        "preserve the actual crash evidence"
    );
    std::fs::write(path, aterm_toml::to_string(&after).unwrap()).unwrap();
    assert_eq!(
        lifetime.observe(),
        ProcessLiveness::Alive,
        "capture persisted while still live"
    );
}

/// Only the queue-specific reader's complete scope can establish absence. Ordinary descriptor
/// sampling may cut its shared detail budget on unrelated files, pipes or sockets.
fn actor_watches(queues: &[FdReading], pid: u32) -> Option<usize> {
    let mut count = 0;
    for fd in queues {
        let FdKind::EventQueue {
            knotes: Some(knotes),
            ..
        } = &fd.kind
        else {
            return None;
        };
        if knotes.len() >= MAX_KNOTES {
            return None;
        }
        count += knotes
            .iter()
            .filter(|note| note.filter == KnoteFilter::Proc && note.ident == u64::from(pid))
            .count();
    }
    Some(count)
}

/// A's absence of actor watches is the negative control; B's live kernel registration is
/// the barrier before exit. Working/Restored alone could race the old one-shot reader.
fn await_exit_watches(host: u32, lifetime: &ProcessLifetime, expected: usize) {
    let mut reader = JobsReader::new();
    let lane = reader.lanes().kqueue_detail;
    if !lane.is_on() {
        aterm_spec::host::could_not_run(format!(
            "cannot observe the live exit registration: {}",
            lane.word()
        ));
    }
    let mut last = None;
    let armed = aterm_spec::wait::within(HANG, GAP, || {
        assert_eq!(
            lifetime.observe(),
            ProcessLiveness::Alive,
            "no exit before the first live look"
        );
        last = reader.event_queues(host).and_then(|queues| {
            actor_watches(&queues, lifetime.pid()).map(|count| (queues.len(), count))
        });
        last.filter(|(_, count)| *count == expected).map(|_| ())
    });
    let diagnostic = if armed.is_none() {
        let sampled = reader.read(ReadReq {
            pid: host,
            fds: true,
        });
        format!(
            "sampled_fds={:?} sample_cut={:?}",
            sampled
                .as_ref()
                .and_then(|facts| facts.fds.as_ref())
                .map(Vec::len),
            sampled.as_ref().map(|facts| facts.fds_truncated)
        )
    } else {
        String::new()
    };
    assert!(
        armed.is_some(),
        "host={host} actor={} expected={expected} complete_queue_count_and_actor_notes={last:?} {diagnostic}",
        lifetime.pid()
    );
}

fn task(ctl: &mut Ctl, sid: &str) -> String {
    ctl.ok(&format!("@{sid} task"))
}

fn stable_task(line: &str) -> BTreeMap<&str, &str> {
    let mut record = fields(line);
    record.remove("since_ms"); // This read's elapsed age advances without a task revision.
    record
}

#[test]
fn a_kept_workers_later_exit_closes_its_task_once_through_the_real_event_loop() {
    let world = world("worker");
    let root = &world.0;
    let state = root.join("astate");
    let state_text = state.to_str().unwrap();
    let claim = state
        .join(aterm_agent::harness::cli::HARNESS)
        .join(aterm_agent::harness::credit_offers::CLAIM_FOLDER);
    std::fs::create_dir_all(&claim).unwrap();
    let claim_text = claim.to_str().unwrap();
    let _keeper = keeper(root);
    // The ordinary exec-edge gatherer also follows the foreground holder. Keep it off so its
    // Proc knote cannot stand in for the restored worker's own exit registration. This actor
    // is a shell job, not a telemetry AgentCall, and makes no native-observe registration.
    let (mut a, sock) = boot(
        root,
        "worker-a",
        &[
            ("ATERM_STATE_HOME", state_text),
            (DESK_SEAM, "1"),
            ("ATERM_DEV_NO_PROCWATCH", "1"),
        ],
    );
    let mut ctl = Ctl::connect(&sock);
    await_prompt(&mut ctl, None);
    let shell_pid = u32::try_from(ctl.fg_pgrp().expect("the original shell leads")).unwrap();
    let shell_lifetime =
        ProcessLifetime::capture(shell_pid).expect("the original shell's exact birth");
    let (_, rows) = ctl.counted("sessions");
    let sessions: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            let mut words = row.split_whitespace();
            Some((words.next()?.parse::<u64>().ok()?, words.next()?.to_owned()))
        })
        .collect();
    let [(local, sid)] = sessions.as_slice() else {
        panic!("one original session: {rows:?}")
    };
    // Finish the compound builtin command before launching the actor: its foreground group
    // must name the actor itself, never an extra shell executing `cd && program`.
    ctl.type_line(&format!(
        "cd '{}' && printf 'KEPT-CWD-%s\\n' READY",
        claim_text.replace('\'', "'\\''")
    ));
    ctl.expect_on_screen("KEPT-CWD-READY");
    await_prompt(&mut ctl, Some("KEPT-CWD-READY"));
    let program =
        std::fs::canonicalize(aterm_uds::deadline::stand_in(Path::new("/bin/bash"))).unwrap();
    let program_text = program.to_str().unwrap();
    ctl.type_line(&format!(
        "'{}' --noprofile --norc -c 'printf \"KEPT-WORKER-%s\\n\" READY; IFS= read -r finish'",
        program_text.replace('\'', "'\\''")
    ));
    ctl.expect_on_screen("KEPT-WORKER-READY");
    let actor = aterm_spec::wait::within(HANG, GAP, || {
        let pid = u32::try_from(ctl.fg_pgrp()?).ok()?;
        (pid != shell_pid)
            .then(|| ProcessLifetime::capture(pid))
            .flatten()
    })
    .expect("the foreground actor is live before the crash");
    assert_ne!(
        actor.pid(),
        shell_pid,
        "the exit target is a job, not the kept shell"
    );
    assert_eq!(
        aterm_sysprobe::process_path(actor.pid()).as_deref(),
        Some(program.as_path())
    );
    await_exit_watches(a.child.id(), &actor, 0);
    await_keeper(root, "claimed=1");
    await_keeper(root, "journals=1");
    let writer = a.child.id();
    let path = aterm_spec::wait::within(HANG, GAP, || journal(&state, writer))
        .expect("A's own disk journal before its crash");
    drop(ctl);
    a.child.kill().expect("SIGKILL only this fixture's host A");
    assert!(!a.child.wait().expect("positive owning reap of A").success());
    drop(a);
    await_keeper(root, "orphaned=1");
    assert_eq!(
        actor.observe(),
        ProcessLiveness::Alive,
        "the keeper preserved A's foreground job"
    );
    plant_worker(&path, writer, *local, &actor, claim_text, program_text);
    let (b, sock) = boot(
        root,
        "worker-b",
        &[
            ("ATERM_STATE_HOME", state_text),
            (DESK_SEAM, "1"),
            (QUIET_SEAM, AN_HOUR_MS),
            ("ATERM_DEV_NO_PROCWATCH", "1"),
        ],
    );
    let mut ctl = admitted(&sock);
    await_status_of(&mut ctl, sid, "restore", "exact");
    let working = task(&mut ctl, sid);
    assert_eq!(fields(&working).get("state"), Some(&"working"), "{working}");
    assert_eq!(fields(&working).get("why"), Some(&"restored"), "{working}");
    assert_eq!(
        ctl.fg_pgrp(),
        Some(actor.pid() as i32),
        "B kept the original foreground actor"
    );
    assert_eq!(
        shell_lifetime.observe(),
        ProcessLiveness::Alive,
        "B kept the original shell birth"
    );
    await_exit_watches(b.child.id(), &actor, 1);
    assert_eq!(
        stable_task(&task(&mut ctl, sid)),
        stable_task(&working),
        "the first live look cannot finish the task"
    );
    let rev = fields(&working)["rev"].parse::<u64>().unwrap();
    ctl.type_line_to(sid, "finish");
    let mut closed = String::new();
    let ended = aterm_spec::wait::within(HANG, GAP, || {
        closed = task(&mut ctl, sid);
        (fields(&closed).get("state") == Some(&"exited")).then_some(())
    });
    assert!(
        ended.is_some(),
        "the real event loop never closed the restored worker: {closed}"
    );
    assert_eq!(fields(&closed).get("why"), Some(&"exit"), "{closed}");
    assert_eq!(
        fields(&closed)["rev"].parse::<u64>().unwrap(),
        rev + 1,
        "one conclusion"
    );
    assert_eq!(actor.observe(), ProcessLiveness::Ended);
    assert_eq!(
        ctl.fg_pgrp(),
        Some(shell_pid as i32),
        "the same shell regained the terminal"
    );
    assert_eq!(
        stable_task(&task(&mut ctl, sid)),
        stable_task(&closed),
        "subsequent real loop turns cannot conclude it twice"
    );
}
