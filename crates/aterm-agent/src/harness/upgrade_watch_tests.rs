// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The record's progress stamp and its deadlines (design record 2026-09-28,
//! "No upgrade stuck forever", rollout step 5): T1-d, the single writer
//! stamps [`St::progress_at`] if and only if the progress key moves and a
//! rollback never stamps; T1-e, every record that is owed a step has a
//! deadline ([`due_by`], [`watch_at`]), the shapes older builds wrote
//! included — tab #1's record of 2026-09-27 among them.

use super::*;

/// A scratch state directory of this test's own, and the options that write
/// into it (not a dry run: the writer is what is under test).
fn scratch(tag: &str) -> (PathBuf, Opts) {
    let dir =
        std::env::temp_dir().join(format!("aterm-upgrade-watch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
    };
    (dir, opts)
}

const SESSION: &str = "446a0b3c-1979-414f-ae20-e06e26a2ef44";
const T0: u64 = 1_790_531_000;

/// How a record is written, as the check below drives it: the real writer
/// ([`save_at`], [`restore`]) or a broken one the check must catch.
struct Writer {
    save: fn(&Opts, &str, &St, u64),
    restore: fn(&Opts, &str, Option<&St>, u64),
}

const REAL: Writer = Writer {
    save: save_at,
    restore: |opts, session, prior, _now| restore(opts, session, prior),
};

/// THE NEGATIVE CONTROL's writer: it stamps the progress on EVERY write —
/// the flaw the single writer exists to rule out (a clock every look
/// restarts bounds nothing).
const STAMPS_EVERY_WRITE: Writer = Writer {
    save: |opts, session, st, now| {
        let mut out = st.clone();
        out.progress_at = now;
        out.progress_key = st.progress_key();
        write_verbatim(opts, session, &out);
    },
    restore: |opts, session, prior, now| {
        if let Some(st) = prior {
            let mut out = st.clone();
            out.progress_at = now;
            write_verbatim(opts, session, &out);
        }
    },
};

/// Every transition a step makes of the record, in the order an upgrade's
/// life takes them — the waits a look records (the words of
/// `upgrade_tests.rs` and `upgrade_stall_tests.rs`: `not-idle:busy`,
/// `background`, `attended`, `held`, `announce-refused:changed`, …), the
/// notices, a limit's hold of the re-ask clock, the give-up, the void, the
/// re-arm, the restart's record and its rollback — each written through
/// `w`, at one second each. Answers the first write where the stamp moved
/// and the key did not, or the key moved and the stamp did not, or a
/// rollback stamped; `Ok` with how many writes moved the key.
fn walk(w: &Writer, tag: &str) -> Result<usize, String> {
    let (dir, opts) = scratch(tag);
    let to = Candidate {
        exe: PathBuf::from("/x/claude"),
        version: Version::parse("2.1.283").expect("version"),
        source: Source::Managed,
    };
    let from = Version::parse("2.1.280").expect("version");
    let held = [upgrade::Held {
        pid: 51_435,
        name: "zsh".to_string(),
        age_s: 4 * 86_400,
        command: String::new(),
    }];
    type Act = Box<dyn Fn(&mut St, u64)>;
    let wait = |word: &'static str| -> Act {
        Box::new(move |st: &mut St, now| st.note_step(&format!("wait:{word}"), "busy", now))
    };
    let script: Vec<(&str, Act)> = vec![
        ("wait:not-idle", wait("not-idle")),
        ("wait:background", wait("background")),
        (
            "held-by",
            Box::new(move |st: &mut St, now| st.note_held("wait:held", &held, now)),
        ),
        ("wait:announce-refused", wait("announce-refused:changed")),
        (
            "announced:1",
            Box::new(|st: &mut St, now| st.announced("ATERM-UPGRADE-READY-1".into(), now, 1)),
        ),
        ("wait:attended", wait("attended")),
        (
            "hold-clock",
            Box::new(|st: &mut St, now| {
                st.hold_clock(now + 600);
            }),
        ),
        ("wait:limited", wait("limited")),
        (
            "announced:2",
            Box::new(|st: &mut St, now| st.announced("ATERM-UPGRADE-READY-2".into(), now, 2)),
        ),
        ("gave-up", Box::new(|st: &mut St, now| st.give_up(now))),
        ("wait:failed", wait("failed")),
        ("void", Box::new(|st: &mut St, now| st.void(now))),
        (
            "rearm",
            Box::new(|st: &mut St, now| {
                st.rearm(now);
            }),
        ),
        (
            "stop",
            Box::new(|st: &mut St, now| st.stop("signal-refused", now)),
        ),
        ("wait:held", wait("held")),
    ];
    let mut now = T0;
    let mut moved = 0;
    let check = |before: Option<&St>, written: &St, now: u64, what: &str| -> Result<bool, String> {
        let after = load(&opts, SESSION).ok_or(format!("{what}: nothing on disk"))?;
        let key_moved = before.is_none_or(|b| b.progress_key != written.progress_key());
        let stamp_moved = before.is_none_or(|b| b.progress_at != after.progress_at);
        if key_moved != stamp_moved {
            return Err(format!(
                "{what}: key moved {key_moved}, stamp moved {stamp_moved} ({:?} -> {})",
                before.map(|b| b.progress_at),
                after.progress_at
            ));
        }
        if key_moved && after.progress_at != now {
            return Err(format!("{what}: stamped {} at {now}", after.progress_at));
        }
        Ok(key_moved)
    };
    let fresh = St::fresh(&from, &to, now);
    (w.save)(&opts, SESSION, &fresh, now);
    if check(None, &fresh, now, "minted")? {
        moved += 1;
    }
    for (what, act) in &script {
        now += 1;
        let before = load(&opts, SESSION).expect("the record");
        let mut st = before.clone();
        act(&mut st, now);
        (w.save)(&opts, SESSION, &st, now);
        if check(Some(&before), &st, now, what)? {
            moved += 1;
        }
    }
    // A copy read long ago and written again unchanged keeps the stamp on
    // disk, whatever the copy's own says.
    now += 1;
    let before = load(&opts, SESSION).expect("the record");
    let stale = St {
        progress_at: 1,
        ..before.clone()
    };
    (w.save)(&opts, SESSION, &stale, now);
    check(Some(&before), &stale, now, "a stale copy")?;
    // THE ROLLBACK: the restart's record is written (progress), its signal
    // is never sent, and the record it found is put back — with the stamp
    // it had, never this second's.
    now += 1;
    let prior = load(&opts, SESSION).expect("the record");
    let mut st = prior.clone();
    let _ = st.signalled(
        52_489,
        9_001,
        "s-3de30c3c66bf5c4496f8",
        "claude".into(),
        now,
    );
    (w.save)(&opts, SESSION, &st, now);
    if check(Some(&prior), &st, now, "signalled")? {
        moved += 1;
    }
    now += 1;
    (w.restore)(&opts, SESSION, Some(&prior), now);
    let back = load(&opts, SESSION).expect("the record");
    if back.progress_at != prior.progress_at {
        return Err(format!(
            "restore: stamped {} over {}",
            back.progress_at, prior.progress_at
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(moved)
}

/// THE ROLLBACK'S NEGATIVE CONTROL's writer (the watch's review, 2026-09-28):
/// the real [`save_at`], and a rollback routed back through it — the flaw
/// C1 names, a put-back record stamped with this second as though it moved.
/// Its every save is the real one, so only the rollback can catch it.
const RESTORES_THROUGH_SAVE: Writer = Writer {
    save: save_at,
    restore: |opts, session, prior, now| {
        if let Some(st) = prior {
            save_at(opts, session, st, now);
        }
    },
};

/// T1-d, THE SINGLE WRITER (design record 2026-09-28, §3.2 C1): over every
/// transition a step makes, [`St::progress_at`] moves if and only if the
/// progress key does — the waits, the held-by list and a stale copy never
/// move it; the notices, a limit's hold of the re-ask clock, the give-up,
/// the re-arm, a stop and the restart's record do — and [`restore`] puts
/// the prior record back with its own stamp. NEGATIVE CONTROLS: a writer
/// that stamps on every write is caught at the first wait; and the real
/// writer with its rollback routed through [`save_at`] passes every write
/// and is caught at the rollback alone (it had no control of its own).
#[test]
fn the_progress_stamp_moves_iff_the_key_moves_and_a_rollback_never_stamps() {
    assert_eq!(
        walk(&REAL, "real"),
        // Minted, announced:1, the hold, announced:2, gave-up, re-arm, stop,
        // signalled: eight moves, and the void of a gave-up round (its
        // release still owed, its phase the same) is none.
        Ok(8)
    );
    let caught = walk(&STAMPS_EVERY_WRITE, "every-write").expect_err("caught");
    assert!(caught.starts_with("wait:not-idle:"), "{caught}");
    let caught = walk(&RESTORES_THROUGH_SAVE, "restore-through-save").expect_err("caught");
    assert!(caught.starts_with("restore:"), "{caught}");
}

/// A NOTICE ITS OWN FENCE REFUSED IS NO PROGRESS (the merge of main's
/// `St::refused`, 2026-09-29): a pending round whose notice every look
/// refuses (`wait:announce-refused:changed`) counts the refusals
/// ([`St::refused`], [`St::refused_at`]) and keeps its step clock — its
/// progress stamp, and so its deadline ([`due_by`]) and its watch
/// ([`watch_at`]), stay where the round last moved. A key that moved on a
/// refusal would push the deadline back at every look that could not type,
/// and the watch would never come for the one failure it exists to see.
/// The notice that lands moves the key, and its step (an act) starts the
/// count over.
/// NEGATIVE CONTROL: the writer that stamps every write pushes the deadline
/// on the first refusal.
#[test]
fn a_refused_notice_counts_and_never_moves_the_step_clock() {
    let run = |w: &Writer, tag: &str| {
        let (dir, opts) = scratch(tag);
        let to = Candidate {
            exe: PathBuf::from("/x/claude"),
            version: Version::parse("2.1.284").expect("version"),
            source: Source::Managed,
        };
        let from = Version::parse("2.1.280").expect("version");
        let fresh = St::fresh(&from, &to, T0);
        (w.save)(&opts, SESSION, &fresh, T0);
        let minted = load(&opts, SESSION).expect("the record");
        let deadline = due_by(&minted).expect("owed its notice");
        let mut deadlines = Vec::new();
        for i in 1..=3 {
            let mut st = load(&opts, SESSION).expect("the record");
            st.note_step("wait:announce-refused:changed", "busy", T0 + 60 * i);
            (w.save)(&opts, SESSION, &st, T0 + 60 * i);
            let back = load(&opts, SESSION).expect("the record");
            deadlines.push(due_by(&back).expect("still owed"));
        }
        let refused = load(&opts, SESSION).expect("the record");
        let _ = std::fs::remove_dir_all(&dir);
        (deadline, deadlines, refused)
    };
    let (deadline, deadlines, st) = run(&REAL, "refused-real");
    assert_eq!(deadline, T0 + upgrade::REASK_S);
    assert_eq!(deadlines, [deadline; 3], "no refusal pushes it back");
    assert_eq!((st.refused, st.refused_at), (3, T0 + 60), "counted");
    assert_eq!(st.progress_at, T0);
    assert!(watch_at(&st).is_some_and(|w| w <= deadline));
    // The notice that lands is progress, and the count starts over.
    let mut landed = st.clone();
    landed.announced("ATERM-UPGRADE-READY-1".into(), T0 + 240, 1);
    landed.note_step("announced:1", "idle", T0 + 240);
    assert_ne!(landed.progress_key(), st.progress_key());
    assert_eq!((landed.refused, landed.refused_at), (0, 0));
    // NEGATIVE CONTROL.
    let (_, pushed, _) = run(&STAMPS_EVERY_WRITE, "refused-every-write");
    assert_eq!(pushed[0], T0 + 60 + upgrade::REASK_S, "caught");
}

/// Every phase, so a new one does not compile without joining the list
/// below (the exhaustive match of [`due_by`] is the other half).
fn every_phase() -> Vec<Phase> {
    let all = vec![
        Phase::Pending,
        Phase::Announced {
            at_s: T0 - 60,
            asks: 2,
        },
        Phase::Exiting { at_s: T0 - 30 },
        Phase::Relaunched { at_s: T0 - 20 },
        Phase::Done,
        Phase::Failed(upgrade::GAVE_UP.to_string()),
        Phase::Failed("signal-refused".to_string()),
    ];
    for p in &all {
        match p {
            Phase::Pending
            | Phase::Announced { .. }
            | Phase::Exiting { .. }
            | Phase::Relaunched { .. }
            | Phase::Done
            | Phase::Failed(_) => {}
        }
    }
    all
}

/// T1-e, TOTALITY (design record 2026-09-28, §3.2 C2): over every phase ×
/// every word of the owner's × the record as this build writes it and as an
/// older one did (no progress stamp, no stop stamp), a record that is owed a
/// step has a step deadline and a watch time — the only ones without are a
/// finished move with no model confirm owed, and a round the owner's `--skip`
/// of THIS target holds before it began restarting. A `--skip` of another
/// build holds nothing, and neither word holds a restart in flight.
#[test]
fn every_record_owed_a_step_has_a_deadline_the_legacy_shapes_included() {
    let requests = [
        Request::None,
        Request::Now,
        Request::DeferUntil(T0 + 7_200),
        Request::Skip("2.1.283".to_string()),
        Request::Skip("2.1.284".to_string()),
    ];
    let mut checked = 0;
    for phase in every_phase() {
        for request in &requests {
            for confirming in [false, true] {
                let st = St {
                    phase: phase.clone(),
                    from: "2.1.280".to_string(),
                    to: "2.1.283".to_string(),
                    tab: "s-3de30c3c66bf5c4496f8".to_string(),
                    salt: T0 - 86_400,
                    failed_at: if matches!(phase, Phase::Failed(_)) {
                        T0 - 10
                    } else {
                        0
                    },
                    confirm_by: if confirming { T0 + 120 } else { 0 },
                    request: request.clone(),
                    ..St::default()
                };
                // As this build writes it, and as a build before the stamps
                // did: the same record with the new fields (and the stop's
                // stamp) left out.
                let mut legacy: Value = aterm_json::from_str(&st.to_json()).expect("json");
                if let Value::Object(o) = &mut legacy {
                    for k in [
                        "progress_at",
                        "progress_key",
                        "looked_at",
                        "looked_by",
                        "point_at",
                        "guard",
                        "failed_at",
                    ] {
                        o.remove(k);
                    }
                }
                let old = St::from_json(&aterm_json::to_string(&legacy).expect("text"))
                    .expect("an older build's record reads");
                for rec in [&st, &old] {
                    let held_by_skip = *request == Request::Skip("2.1.283".to_string())
                        && matches!(
                            rec.phase,
                            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
                        );
                    let finished = rec.phase == Phase::Done && !rec.confirming();
                    let owed = !held_by_skip && !finished;
                    assert_eq!(
                        due_by(rec).is_some(),
                        owed,
                        "{:?} {request:?} confirming={confirming}",
                        rec.phase
                    );
                    assert_eq!(watch_at(rec).is_some(), owed);
                    if let (Some(step), Some(watch)) = (due_by(rec), watch_at(rec)) {
                        // Never later than the move clock, never sooner than
                        // the gap after the last watch.
                        assert!(watch <= step.max(rec.looked_at + WATCH_GAP));
                        assert!(watch <= rec.behind_since() + MOVE_BUDGET_S);
                    }
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 7 * 5 * 2 * 2);
    // The owner's --defer holds the deadline to its second, not sooner.
    let deferred = St {
        progress_at: T0,
        request: Request::DeferUntil(T0 + 7_200),
        ..St::default()
    };
    assert_eq!(due_by(&deferred), Some(T0 + 7_200 + WATCH_SLACK));
    // A watch waits the gap after the last one, however far past due.
    let looked = St {
        progress_at: T0,
        looked_at: T0 + 10_000,
        ..St::default()
    };
    assert_eq!(watch_at(&looked), Some(T0 + 10_000 + WATCH_GAP));
}

/// Tab #1's record as the 0.94.0 host wrote it on 2026-09-27 (T1-e below
/// says what it holds, and how it was rebuilt).
const TAB_ONE_RECORD: &str = r#"{"phase":"announced","why":"","from":"2.1.280","to":"2.1.283","source":"managed","marker":"ATERM-UPGRADE-READY-5f3a1c9e","notice_start":"Wed Sep 23 17:13:02 2026","tab":"s-3de30c3c66bf5c4496f8","line":"","noted":"","cause":"","model_before":"","launch_model":"","model_list":"","resumed_on":"","wait":"background","request":"-","request_tab":"","outcome":"","at":1790531333,"asks":1,"salt":1790373043,"last_seq":0,"seq_since":1790373043,"pid":52489,"notice_pid":52489,"shell":0,"mark":0,"confirm_by":0,"resumed_pid":0,"hold_since":0,"hold_seen":0,"ready_since":0,"pending_since":0,"wait_since":1790531333,"done_at":0,"request_at":0}"#;

/// T1-e, TAB #1'S RECORD (design record 2026-09-28, §1): the record the
/// 0.94.0 host wrote on 2026-09-27 — announced at 10:48:53 PDT
/// (`at` 1790531333), one ask, to 2.1.283, `wait=background`, minted
/// 2026-09-25 (`salt` 1790373043) — with none of the fields a 0.95+ build
/// writes. Rebuilt from those fields as the brief gives them (the file
/// itself was not copied). Read by this build, it last moved at its notice,
/// its re-ask is 35 minutes after, and its move clock had run out a day
/// after it was minted: it is PAST DUE at 1790632822 (09-28 15:00 PDT) —
/// and was already at 20:07 on 09-27, when 0.95.0 took over the window and
/// nothing looked at it for eighteen hours.
#[test]
fn tab_ones_record_of_the_27th_is_past_due() {
    let text = TAB_ONE_RECORD;
    let st = St::from_json(text).expect("the 0.94 record reads");
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: 1_790_531_333,
            asks: 1
        }
    );
    assert_eq!(st.progress_at, 1_790_531_333, "max(at, failed_at, salt)");
    assert_eq!(
        due_by(&st),
        Some(1_790_531_333 + upgrade::REASK_S + WATCH_SLACK)
    );
    let watch = watch_at(&st).expect("owed a step");
    assert_eq!(watch, 1_790_373_043 + MOVE_BUDGET_S, "the move clock first");
    assert!(watch <= 1_790_632_822, "past due at 15:00 on the 28th");
    assert!(watch <= 1_790_564_820, "past due when 0.95.0 took over");
    // Re-saved unchanged by this build, it keeps the stamp its fields gave
    // it: the first write of a watching build restarts no clock.
    let (dir, opts) = scratch("tab-one");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), text).expect("write");
    save_at(&opts, SESSION, &st, 1_790_632_822);
    let back = load(&opts, SESSION).expect("the record");
    assert_eq!(back.progress_at, 1_790_531_333);
    assert!(watch_at(&back).is_some_and(|w| w <= 1_790_632_822));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- T1-c the watch

/// Tab #1 (design record 2026-09-28, §1).
const TAB_ONE: &str = "s-3de30c3c66bf5c4496f8";

/// 15:00 PDT on 2026-09-28: tab #1's record is past its watch (T1-e).
const NOW_28TH: u64 = 1_790_632_822;

/// The weekly limit's reset the transcript names: "resets Oct 4 at 12am
/// (America/Los_Angeles)".
const RESETS: u64 = 1_791_097_200;

/// The control verbs that only READ: all a dry-run visit may send.
const READS: [&str; 5] = ["sessions", "text", "cell", "status", "help"];

/// A control socket at `<dir>/t.sock` standing in for tab #1's aterm with
/// NO HANDS: `sessions` names `tab`, `text --json` is an idle Claude's
/// screen, a `cell` a plain blank, `status` holds nothing — and every other
/// request (`send`, `key`, `turn`, `paste`, `signal`, a lease, …) is REFUSED,
/// typing, pressing and signalling nothing. Every request is logged, in
/// order.
fn no_hands_instance(
    dir: &Path,
    tab: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::BufRead as _;
    use std::io::Write as _;
    let sock = dir.join("t.sock");
    std::fs::write(dir.join("t.sock.token"), format!("{TOKEN}\n")).expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&asked);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let Ok(mut out) = conn.try_clone() else {
                continue;
            };
            let mut lines = std::io::BufReader::new(conn).lines();
            let auth = format!("AUTH {TOKEN}");
            if !lines.next().is_some_and(|l| l.is_ok_and(|l| l == auth)) {
                continue;
            }
            for line in lines {
                let Ok(line) = line else { break };
                let verb = line
                    .split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .unwrap_or("")
                    .to_string();
                let reply = match verb.as_str() {
                    "sessions" => format!("OK 1\nlocal {tab} 0 idle claude"),
                    "text" => format!("OK {}", idle_screen()),
                    "cell" => "OK %20 d0d0d0 111318 none".to_string(),
                    "status" => "OK schema=1 hold=0 hand=-".to_string(),
                    other => format!("ERR the no-hands stand-in refuses {other}"),
                };
                if let Ok(mut log) = log.lock() {
                    log.push(line);
                }
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
        }
    });
    (sock.to_string_lossy().into_owned(), asked)
}

/// Tab #1 on 2026-09-28, stood up for the watch: a stand-in agent in the
/// tab, running Claude Code 2.1.280 under conversation [`SESSION`] (its
/// session file, busy); the conversation's transcript ending in the weekly
/// limit's row (`quotaLimits.resetsAt` [`RESETS`], the shape Claude Code
/// 2.1.280 writes, `upgrade_queued_tests.rs`); tab #1's 0.94 record
/// ([`TAB_ONE_RECORD`]) with its notice's process rewritten to the
/// stand-in's pid and start — the process the notice reached is not on this
/// machine, and a record whose notice process is another's is read as
/// owned elsewhere (`wait:notice-owned-by-other-process`); and the no-hands
/// instance. `managed` is the build aterm's managed twin names.
struct TabOne {
    dir: PathBuf,
    opts: Opts,
    agent: std::process::Child,
    sf: SessionFile,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    targets: Targets,
}

impl TabOne {
    fn new(tag: &str, managed: &str) -> TabOne {
        let (dir, mut opts) = scratch(tag);
        std::fs::create_dir_all(&dir).expect("dir");
        let (sock, asked) = no_hands_instance(&dir, TAB_ONE);
        opts.sock = Some(sock);
        let agent = parked()
            .env("ATERM_PARENT_SESSION_ID", TAB_ONE)
            .spawn()
            .expect("the stand-in agent");
        wait_exec(agent.id());
        let pid = agent.id();
        let start = kernel_start(pid).expect("lstart");
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("sessions");
        std::fs::write(
            opts.home.join(format!(".claude/sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"/","version":"2.1.280","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("session file");
        let sf = session_file_of(&opts.home, pid).expect("the file parses");
        let project = opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"keep the disk watch going"}}}}
{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"2026-09-28T00:49:50.000Z","message":{{"id":"m","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your weekly limit · resets Oct 4 at 12am (America/Los_Angeles)"}}]}},"requestId":"req_0","quotaLimits":{{"status":"rejected","resetsAt":{RESETS},"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.280"}}
"#
            ),
        )
        .expect("transcript");
        let record = TAB_ONE_RECORD
            .replace("\"pid\":52489", &format!("\"pid\":{pid}"))
            .replace("\"notice_pid\":52489", &format!("\"notice_pid\":{pid}"))
            .replace("Wed Sep 23 17:13:02 2026", &squash(&sf.proc_start));
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), record).expect("the 0.94 record");
        let targets = Targets {
            managed: Some(Candidate {
                exe: PathBuf::from("/nonexistent/claude"),
                version: Version::parse(managed).expect("version"),
                source: Source::Managed,
            }),
            native: None,
            unanswered: false,
        };
        TabOne {
            dir,
            opts,
            agent,
            sf,
            asked,
            targets,
        }
    }

    /// The watch's read: the REAL visit of tab #1 ([`visit`]), with the
    /// managed twin [`Self::targets`] names and a kernel that reads the
    /// stand-in as its shell's foreground job on the tab's terminal —
    /// asserting it is handed a dry run of this tab, and counting it.
    fn read<'a>(&'a self, reads: &'a std::cell::Cell<u32>) -> impl Fn(&Opts) -> Report + 'a {
        move |o: &Opts| {
            assert!(o.dry_run, "the watch reads through a dry run");
            assert_eq!(o.only_sid.as_deref(), Some(TAB_ONE));
            reads.set(reads.get() + 1);
            visit(
                o,
                &self.sf,
                &table(),
                &self.targets,
                &Script::new(std::process::id(), usize::MAX, None),
            )
        }
    }

    fn record(&self) -> St {
        load(&self.opts, SESSION).expect("the record")
    }

    fn raw(&self) -> String {
        std::fs::read_to_string(state_path(&self.opts, SESSION)).expect("the record")
    }

    /// Every request the watch sent that is not a read: none may be.
    fn hands(&self) -> Vec<String> {
        self.asked
            .lock()
            .expect("log")
            .iter()
            .filter(|l| {
                let verb = l.split_whitespace().find(|w| !w.starts_with('@'));
                !verb.is_some_and(|v| READS.contains(&v))
            })
            .cloned()
            .collect()
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(state_dir(&self.opts).join("ledger.jsonl")).unwrap_or_default()
    }
}

impl Drop for TabOne {
    fn drop(&mut self) {
        let _ = self.agent.kill();
        let _ = self.agent.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Who looks in T1-c: a build, and the loop's word that the limit's wall
/// withheld its last point.
fn looker() -> Watcher {
    Watcher {
        by: "0.99.0+gtest".to_string(),
        guard: crate::supervise::Guard::Wall.word().to_string(),
        point_at: 1_790_547_006,
        told: false,
    }
}

/// T1-c, THE WATCH (design record 2026-09-28, "No upgrade stuck forever",
/// §3.2 C4; rollout step 7), over tab #1 as it stood on 2026-09-28: its 0.94
/// record announced for 2.1.283, a weekly limit in the transcript, 2.1.284
/// installed — and a loop that offered no point, so no step ran for it for
/// three days. ONE watch, at an injected clock ([`NOW_28TH`]), through the
/// REAL visit as a dry run and a control socket that refuses every request
/// but a read: the record is retargeted to 2.1.284 (pending again, the old
/// notice's release carried, `retargeted:2.1.283->2.1.284` in the ledger),
/// stamped `looked` by the build that looked, with the loop's guard — and
/// the tab was sent ZERO input requests. With 2.1.283 still the newest, the
/// same watch holds the announced round's re-ask clock to the limit's reset
/// instead. NEGATIVE CONTROLS: what main has — the visit's read alone, no
/// watch to write — leaves the record at 2.1.283 waiting `background`, byte
/// for byte; a watch whose write finds the sweep lock held (a real visit
/// running) writes nothing and ledgers nothing; and a record not yet due is
/// not even read.
#[test]
fn the_watch_retargets_and_holds_tab_ones_record_off_any_point_with_no_hands() {
    // The retarget.
    let one = TabOne::new("t1c-retarget", "2.1.284");
    let reads = std::cell::Cell::new(0);
    let before = one.raw();
    // NEGATIVE CONTROL (main): the read alone moves nothing.
    let dry = Opts {
        dry_run: true,
        only_sid: Some(TAB_ONE.to_string()),
        ..one.opts.clone()
    };
    let r = one.read(&reads)(&dry);
    assert_eq!(r.to, "2.1.284(managed)", "the read finds the newer build");
    assert_eq!(one.raw(), before, "main: the record stays as it was");
    assert_eq!(one.record().to, "2.1.283");
    assert_eq!(one.record().wait, "background");
    // NEGATIVE CONTROL: a real visit holds the sweep lock — the watch reads,
    // and writes nothing.
    {
        let _visit = sweep_lock(&one.opts)
            .expect("the lock")
            .expect("not a dry run");
        let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
        assert_eq!(w.skipped.as_deref(), Some("busy"), "{w:?}");
        assert!(!w.wrote());
        assert_eq!(one.raw(), before, "a busy lock: nothing written");
        assert!(one.ledger().is_empty(), "and nothing ledgered");
    }
    // THE WATCH.
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert_eq!(w.skipped, None, "{w:?}");
    assert_eq!(w.to, "2.1.284(managed)");
    assert_eq!(w.limit_until, Some(RESETS), "the transcript's limit");
    assert!(
        w.wrote.iter().any(|x| x == "retargeted:2.1.283->2.1.284"),
        "{w:?}"
    );
    assert!(w.wrote.iter().any(|x| x == "looked"), "{w:?}");
    assert!(
        w.step.starts_with("wait:") || w.step.starts_with("would-"),
        "the read's word is a wait or what it would do: {}",
        w.step
    );
    let st = one.record();
    assert_eq!(st.to, "2.1.284", "retargeted off any point");
    assert_eq!(st.phase, Phase::Pending);
    assert_eq!(st.release, "retargeted", "the old notice's release carried");
    assert_eq!(st.tab, TAB_ONE);
    assert_eq!(st.looked_at, NOW_28TH, "looked, at the watch's clock");
    assert_eq!(st.looked_by, "0.99.0+gtest");
    assert_eq!(st.guard, "wall");
    assert_eq!(st.point_at, 1_790_547_006);
    assert_eq!(st.progress_at, NOW_28TH, "a retarget is progress");
    assert!(
        one.ledger()
            .contains("\"step\":\"retargeted:2.1.283->2.1.284\""),
        "said once in the ledger: {}",
        one.ledger()
    );
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
    assert!(
        !one.asked.lock().expect("log").is_empty(),
        "the read did reach the tab"
    );
    // NEGATIVE CONTROL: looked at, the record is not due again for a gap —
    // and a watch that is not due does not even read.
    let n = reads.get();
    let again = watch_with(
        &one.opts,
        TAB_ONE,
        &looker(),
        NOW_28TH + 60,
        &one.read(&reads),
    );
    assert_eq!(again.skipped.as_deref(), Some("not-due"));
    assert_eq!(reads.get(), n, "not read");
    drop(one);

    // The limit hold: no newer build, the announced round's re-ask clock is
    // held to the limit's reset.
    let one = TabOne::new("t1c-hold", "2.1.283");
    let reads = std::cell::Cell::new(0);
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert_eq!(w.skipped, None, "{w:?}");
    assert!(
        w.wrote.iter().any(|x| *x == format!("held:{RESETS}")),
        "{w:?}"
    );
    let st = one.record();
    assert_eq!(st.to, "2.1.283");
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: RESETS,
            asks: 1
        },
        "the re-ask clock starts at the limit's reset"
    );
    assert_eq!(st.looked_at, NOW_28TH);
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
}

/// A TOLD WATCH CHANGES WHAT IT MUST AND STAMPS NO LOOK BEFORE THE RECORD IS
/// DUE; A DUE ONE NAMES NO GUARD A LATER POINT OUTLIVED (the watch's review,
/// 2026-09-28). Over tab #1, through the real visit as a dry run and the
/// no-hands instance: a due watch holds the round to the limit's reset and
/// is stamped `looked`; a newer build appears, and a TOLD watch a minute
/// later — the record not due for a gap — retargets it, and leaves
/// `looked_at` as it was: a told watch stamped every record of its tab, so
/// worker starts and activation notices more often than `WATCH_GAP` held off
/// the due watch (the only one that re-parks) for ever. A told watch with
/// nothing to change writes nothing at all. The next due watch, whose loop
/// offered a point after its last guard, clears the guard the record kept.
/// NEGATIVE CONTROLS, on the code before the fix: the told watch stamped
/// `looked_at` a minute on, the nothing-to-change one rewrote the record,
/// and `guard=wall` stood beside the later point. ZERO input requests.
#[test]
fn a_told_watch_stamps_no_look_before_due_and_a_later_point_clears_the_guard() {
    let mut one = TabOne::new("t1c-told", "2.1.283");
    let reads = std::cell::Cell::new(0);
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert!(w.wrote.iter().any(|x| x == "looked"), "{w:?}");
    assert_eq!(one.record().looked_at, NOW_28TH);
    let told = Watcher {
        told: true,
        ..looker()
    };
    // Told, with nothing to change: nothing written.
    let before = one.raw();
    let w = watch_with(&one.opts, TAB_ONE, &told, NOW_28TH + 30, &one.read(&reads));
    assert_eq!(w.skipped.as_deref(), Some("not-due"), "{w:?}");
    assert_eq!(one.raw(), before, "a told watch with nothing to change");
    // A newer build: the told watch retargets, and stamps no look.
    one.targets.managed = Some(Candidate {
        exe: PathBuf::from("/nonexistent/claude"),
        version: Version::parse("2.1.284").expect("version"),
        source: Source::Managed,
    });
    let w = watch_with(&one.opts, TAB_ONE, &told, NOW_28TH + 60, &one.read(&reads));
    assert!(
        w.wrote.iter().any(|x| x == "retargeted:2.1.283->2.1.284"),
        "{w:?}"
    );
    assert!(!w.wrote.iter().any(|x| x == "looked"), "{w:?}");
    let st = one.record();
    assert_eq!(st.to, "2.1.284");
    assert_eq!(st.looked_at, NOW_28TH, "the due watch's clock is its own");
    assert_eq!(st.guard, "wall");
    // The next due watch: a point came after the last guard said.
    let later = Watcher {
        guard: String::new(),
        point_at: NOW_28TH + 120,
        told: false,
        ..looker()
    };
    let at = watch_at(&st).expect("owed a watch");
    let w = watch_with(&one.opts, TAB_ONE, &later, at, &one.read(&reads));
    assert!(w.wrote.iter().any(|x| x == "looked"), "{w:?}");
    let st = one.record();
    assert_eq!((st.guard.as_str(), st.point_at), ("", NOW_28TH + 120));
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
}

/// A RECORD NO BUILD GAVE A TAB IS WATCHED WHERE THE READ FINDS ITS
/// CONVERSATION (the watch's review, 2026-09-28): an older build recorded the
/// tab only at the announcement (measured 2026-09-25 on the owner's machine),
/// and the watch took only records whose `tab` was the one it watched — so
/// such a record, in a session offering no point, was never watched. Tab #1's
/// record with its `tab` emptied is watched through the real visit's dry
/// read, which finds its conversation in the tab, and records the tab.
/// NEGATIVE CONTROL: another tab's watch — whose read finds no conversation
/// of this record — does not touch it; and on the code before the fix the
/// watch of tab #1 answered `no-record`.
#[test]
fn a_record_with_no_recorded_tab_is_watched_where_the_read_finds_it() {
    let one = TabOne::new("t1c-untabbed", "2.1.284");
    let untabbed = one
        .raw()
        .replace(&format!("\"tab\":\"{TAB_ONE}\""), "\"tab\":\"\"");
    assert_ne!(untabbed, one.raw(), "the tab emptied");
    std::fs::write(state_path(&one.opts, SESSION), &untabbed).expect("the record");
    let reads = std::cell::Cell::new(0);
    let other = watch_with(
        &one.opts,
        "s-elsewhere",
        &looker(),
        NOW_28TH,
        &|o: &Opts| Report {
            pid: 0,
            tab: o.only_sid.clone().unwrap_or_default(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: "skip:no-agent".to_string(),
        },
    );
    assert_eq!(other.skipped.as_deref(), Some("no-record"), "{other:?}");
    assert_eq!(one.raw(), untabbed, "another tab's watch leaves it");
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert_eq!(w.skipped, None, "{w:?}");
    assert!(w.wrote.iter().any(|x| x == "looked"), "{w:?}");
    let st = one.record();
    assert_eq!(st.tab, TAB_ONE, "the tab the read proved");
    assert_eq!(st.looked_at, NOW_28TH);
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
}
