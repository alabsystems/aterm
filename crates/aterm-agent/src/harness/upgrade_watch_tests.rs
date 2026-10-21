// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The record's progress stamp, its deadlines and the watch: the single writer stamps
//! [`St::progress_at`] iff the progress key moves and a rollback never stamps; every record
//! owed a step has a deadline ([`due_by`], [`watch_at`]), older builds' shapes included; and
//! the watch records off any point with no hands.

use super::*;

/// A scratch state directory removed on drop, and options that write into it (not a dry run:
/// the writer is under test).
fn scratch(tag: &str) -> (crate::supervise::ScratchDir, Opts) {
    let dir = crate::supervise::ScratchDir::new("upgrade-watch", tag);
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
        // The ask-and-drain flow these tests pin (`upgrade_ask = true`).
        ask: true,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
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

/// The negative control's writer: it stamps the progress on every write (a
/// clock every look restarts bounds nothing).
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

/// Writes every transition a step makes of the record through `w`, one second apart, in an
/// upgrade's order: the waits a look records, the notices, a limit's hold of the re-ask clock,
/// the give-up, the void, the re-arm, the restart's record and its rollback. Errs at the
/// first write where the stamp and the key disagree, or where a rollback stamped; `Ok` with
/// how many writes moved the key.
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
    let fresh = St::fresh(&from, &to, Proc::default(), now);
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
    // The rollback: the restart's record is written (progress), its signal is
    // never sent, and the prior record is put back with its own stamp.
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

/// The rollback's negative control: the real [`save_at`], with the rollback routed through it
/// (stamping the put-back record), so only the rollback can catch it.
const RESTORES_THROUGH_SAVE: Writer = Writer {
    save: save_at,
    restore: |opts, session, prior, now| {
        if let Some(st) = prior {
            save_at(opts, session, st, now);
        }
    },
};

/// Over every transition a step makes, [`St::progress_at`] moves iff the progress key does;
/// [`restore`] puts the prior record back with its own stamp. Negative controls: a writer that
/// stamps every write (caught at the first wait), and the rollback routed through [`save_at`]
/// (caught at the rollback alone).
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

/// A notice its own fence refuses is no progress: the refusals are counted ([`St::refused`],
/// [`St::refused_at`]) and the step clock stays, so the deadline and the watch stay where the
/// round last moved. The notice that lands moves the key and resets the count. Negative
/// control: the writer that stamps every write pushes the deadline at the first refusal.
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
        let fresh = St::fresh(&from, &to, Proc::default(), T0);
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
    // Negative control.
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

/// Over every phase × owner's word × the record as this build and an older one wrote it (no
/// progress or stop stamp), a record owed a step has a step deadline and a watch time. Only a
/// finished move with no model confirm owed, and a round not yet restarting that `--skip` of
/// this target holds, have none; a `--skip` of another build holds nothing.
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
                // And as an older build wrote it: without the stamp fields.
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

/// Tab one's record as a 0.94.0 host wrote it.
const TAB_ONE_RECORD: &str = r#"{"phase":"announced","why":"","from":"2.1.280","to":"2.1.283","source":"managed","marker":"ATERM-UPGRADE-READY-5f3a1c9e","notice_start":"Wed Sep 23 17:13:02 2026","tab":"s-3de30c3c66bf5c4496f8","line":"","noted":"","cause":"","model_before":"","launch_model":"","model_list":"","resumed_on":"","wait":"background","request":"-","request_tab":"","outcome":"","at":1790531333,"asks":1,"salt":1790373043,"last_seq":0,"seq_since":1790373043,"pid":52489,"notice_pid":52489,"shell":0,"mark":0,"confirm_by":0,"resumed_pid":0,"hold_since":0,"hold_seen":0,"ready_since":0,"pending_since":0,"wait_since":1790531333,"done_at":0,"request_at":0}"#;

/// Tab one's 0.94 record (announced, one ask, no later fields) reads as last moved at its
/// notice, re-asked [`upgrade::REASK_S`] plus slack after, and past due once its move clock
/// ran out a day after minting (`salt`).
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

// ---------------------------------------------------------------- the watch

/// Tab one's id.
const TAB_ONE: &str = "s-3de30c3c66bf5c4496f8";

/// A clock at which tab one's record is past its watch.
const NOW_28TH: u64 = 1_790_632_822;

/// The weekly limit's reset the transcript names: "resets Oct 4 at 12am
/// (America/Los_Angeles)".
const RESETS: u64 = 1_791_097_200;

/// The control verbs that only read: all a dry-run visit may send.
const READS: [&str; 5] = ["sessions", "text", "cell", "status", "help"];

/// A control socket at `<dir>/t.sock` standing in for tab #1's aterm with no
/// hands: `sessions` names `tab`, `text --json` is an idle Claude's screen, a
/// `cell` a plain blank, `status` holds nothing, and every other request
/// (`send`, `key`, `turn`, `paste`, `signal`, a lease, …) is refused. Every
/// request is logged, in order.
fn no_hands_instance(dir: &Path, tab: &'static str) -> (String, crate::harness::FakeInstanceLog) {
    crate::harness::fake_instance(&dir.join("t.sock"), TOKEN, move |line| {
        let verb = line
            .split_whitespace()
            .find(|w| !w.starts_with('@'))
            .unwrap_or("");
        match verb {
            "sessions" => format!("OK 1\nlocal {tab} 0 idle claude"),
            "text" => format!("OK {}", idle_screen()),
            "cell" => "OK %20 d0d0d0 111318 none".to_string(),
            "status" => "OK schema=1 hold=0 hand=-".to_string(),
            other => format!("ERR the no-hands stand-in refuses {other}"),
        }
    })
}

/// Tab one, stood up for the watch: a stand-in agent running Claude Code 2.1.280 under
/// [`SESSION`] (its session file busy), a transcript ending in the weekly limit's row (reset
/// [`RESETS`]), tab one's 0.94 record with its notice's process rewritten to the stand-in's (a
/// notice process of another machine reads as owned elsewhere), and the no-hands instance.
/// `managed` is the build aterm's managed twin names.
struct TabOne {
    dir: crate::supervise::ScratchDir,
    opts: Opts,
    agent: aterm_uds::spawnfd::Child,
    sf: SessionFile,
    asked: crate::harness::FakeInstanceLog,
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

    /// The watch's read: the real [`visit`] of tab one, with [`Self::targets`]' managed twin
    /// and a kernel that reads the stand-in as the tab's foreground job. Asserts it is handed
    /// a dry run of this tab, and counts it.
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

/// Who looks: a build, and the loop's word that the limit's wall withheld its last point.
fn looker() -> Watcher {
    Watcher {
        by: "0.99.0+gtest".to_string(),
        guard: crate::supervise::Guard::Wall.word().to_string(),
        point_at: 1_790_547_006,
        told: false,
    }
}

/// The watch over tab one (a 0.94 record announced for 2.1.283, a weekly limit in the
/// transcript, 2.1.284 installed, no point offered), through the real visit as a dry run and
/// a socket refusing all but reads: the record is retargeted to 2.1.284 in place (still
/// announced, marker kept, no release owed, `retargeted:2.1.283->2.1.284` ledgered), its
/// re-ask clock held to the limit's reset, and stamped `looked` with the loop's guard, with
/// zero input requests. With 2.1.283 still newest it holds the clock and retargets nothing.
/// Negative controls: the read alone changes no byte; a held sweep lock writes and ledgers
/// nothing; a record not yet due is not read.
#[test]
fn the_watch_retargets_and_holds_tab_ones_record_off_any_point_with_no_hands() {
    // The retarget.
    let one = TabOne::new("t1c-retarget", "2.1.284");
    let reads = std::cell::Cell::new(0);
    let before = one.raw();
    // Negative control: the read alone moves nothing.
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
    // Negative control: a real visit holds the sweep lock; the watch reads and
    // writes nothing.
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
    // The watch.
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
    // Retargeted in place, the round is still announced, so the same watch
    // also holds its re-ask clock to the limit's named reset.
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: RESETS,
            asks: 1
        },
        "retargeted in place: the round kept, its clock held to the reset"
    );
    assert_eq!(
        st.marker, "ATERM-UPGRADE-READY-5f3a1c9e",
        "its notice's marker kept"
    );
    assert!(st.release.is_empty(), "no release owed: {:?}", st.release);
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
    // Negative control: once looked at, the record is not due again for a gap,
    // and a watch that is not due does not read.
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

/// An announced round whose model rides its restart is retargeted in place by
/// the watch as the visit does: tab one's record with a model promised
/// (`model_list`) and a newer build installed. The watch hands
/// [`St::for_target`] the round's own promise ([`St::promised_model`]), so the
/// round stays announced, marker and promise kept, no release owed, and the
/// ledger says `in place`. Negative control: `for_target` handed no model or
/// another one abandons the round (pending again, marker gone, a release
/// owed).
#[test]
fn the_watch_keeps_an_announced_round_whose_model_rides_the_restart_in_place() {
    let one = TabOne::new("t1c-model", "2.1.284");
    let promised = one
        .raw()
        .replace("\"model_list\":\"\"", "\"model_list\":\"claude-opus-5-5\"");
    std::fs::write(state_path(&one.opts, SESSION), promised).expect("the promise");
    let prior = one.record();
    assert_eq!(prior.promised_model(), Some("claude-opus-5-5"));
    let reads = std::cell::Cell::new(0);
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert!(
        w.wrote.iter().any(|x| x == "retargeted:2.1.283->2.1.284"),
        "{w:?}"
    );
    let st = one.record();
    assert_eq!(st.to, "2.1.284");
    assert!(
        matches!(st.phase, Phase::Announced { asks: 1, .. }),
        "still announced: {:?}",
        st.phase
    );
    assert_eq!(st.marker, "ATERM-UPGRADE-READY-5f3a1c9e", "its marker kept");
    assert_eq!(st.model_list, "claude-opus-5-5", "its promise kept");
    assert!(st.release.is_empty(), "no release owed: {:?}", st.release);
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
    // Negative control: no promise handed, or another model, abandons it.
    let from = Version::parse("2.1.280").expect("from");
    let to = Candidate {
        exe: PathBuf::new(),
        version: Version::parse("2.1.284").expect("to"),
        source: Source::Managed,
    };
    for model in [None, Some("claude-opus-4-1")] {
        let other = St::for_target(
            Some(prior.clone()),
            &from,
            &to,
            model,
            Proc::default(),
            NOW_28TH,
        );
        assert_eq!(other.phase, Phase::Pending, "{model:?}");
        assert!(other.marker.is_empty(), "{model:?}");
        assert_eq!(other.release, "retargeted", "{model:?}");
    }
}

/// A limit row naming no reset (an API rate limit, no `quotaLimits`) holds the round's clock
/// for a re-ask's wait (`held:<until>`) but leaves the record no reset (`St::limit_until` `0`,
/// the watch's `limit_until` `None`), so the owner's view never says a reset passed. A row
/// naming its reset is the retarget test's (`RESETS`).
#[test]
fn the_watch_keeps_no_reset_for_a_limit_row_that_names_none() {
    let one = TabOne::new("t1c-api-limit", "2.1.283");
    // 21:50:00Z on the 28th, ten minutes before the watch.
    let row_at: u64 = 1_790_632_200;
    let until = row_at + upgrade::REASK_S;
    std::fs::write(
        one.opts
            .home
            .join(format!(".claude/projects/p/{SESSION}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":"keep the disk watch going"}}
{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"2026-09-28T21:50:00.000Z","message":{"id":"m","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","usage":{"input_tokens":0,"output_tokens":0},"content":[{"type":"text","text":"API Error: Rate limit reached for requests"}]},"requestId":"req_0","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.280"}
"#,
    )
    .expect("the API limit's row");
    let reads = std::cell::Cell::new(0);
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert_eq!(w.limit_until, None, "no reset named: {w:?}");
    assert!(
        w.wrote.iter().any(|x| *x == format!("held:{until}")),
        "the wait still holds the round's clock: {w:?}"
    );
    let mut st = one.record();
    assert_eq!(st.limit_until, 0, "no reset kept");
    // A later look records the wait as `limited`.
    st.wait = "limited".to_string();
    save(&one.opts, SESSION, &st);
    let row = rows(&one.opts)
        .into_iter()
        .find(|r| r.session == SESSION)
        .expect("the row");
    let later = until + LIMIT_GRACE_S;
    assert!(!row.limit_passed(later), "no named reset has passed");
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
}

/// A told watch makes the changes it must but stamps no look before the record is due (else
/// frequent told watches would starve the due watch, the only one that re-parks); with
/// nothing to change it writes nothing. A due watch whose loop offered a point after its last
/// guard clears the guard. Zero input requests.
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

/// A record with no tab is watched where the read finds its conversation, which records the
/// tab. Negative control: another tab's watch, whose read finds no conversation of it, leaves
/// it alone.
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

/// A word the act's own last checks recorded stands over a dry read that stops short of them:
/// a notice its fence refused (`wait:announce-refused:changed`) under a `would-announce` read,
/// its `TELL_S` warning (`Row::refused_notice`) still told, and a restart's last look's
/// `changed` under a `would-restart` read (`St::restart_look`). Negative controls, each
/// recorded `next-point`: a gate's word (`not-idle:busy`), the refused notice under another
/// act's read, and a `changed` no restart's look said.
#[test]
fn a_word_the_acts_own_last_check_recorded_stands_over_a_read_short_of_it() {
    let one = TabOne::new("t1c-past-go", "2.1.283");
    let said = |step: &'static str| {
        move |o: &Opts| Report {
            pid: 0,
            tab: o.only_sid.clone().unwrap_or_default(),
            session: SESSION.to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: step.to_string(),
        }
    };
    let base = one.record();
    // Each case: the record as a visit left it, then one due watch.
    let watched = |word: &str, restart_look: bool, read: &'static str| {
        let mut st = base.clone();
        let at = NOW_28TH - 3 * 3_600;
        for i in 0..3 {
            st.note_step(&format!("wait:{word}"), "idle", at + 60 * i);
        }
        st.restart_look = restart_look;
        st.looked_at = 0;
        write_verbatim(&one.opts, SESSION, &st);
        let now = watch_at(&st).expect("owed a watch").max(NOW_28TH);
        let w = watch_with(&one.opts, TAB_ONE, &looker(), now, &said(read));
        assert!(w.wrote.iter().any(|x| x == "looked"), "{word}: {w:?}");
        (w, one.record(), now)
    };
    let refused_row = |now: u64| {
        rows(&one.opts)
            .into_iter()
            .find(|r| r.session == SESSION)
            .expect("tab #1's row")
            .refused_notice(now)
    };

    // The notice its own fence refused, under a read that would announce.
    let (w, st, now) = watched("announce-refused:changed", false, "would-announce");
    assert_eq!(w.wait, "announce-refused:changed", "{w:?}");
    assert_eq!(st.wait, "announce-refused:changed", "the fence's word kept");
    assert_eq!((st.refused, st.refused_at), (3, NOW_28TH - 3 * 3_600));
    assert!(
        refused_row(now),
        "aterm could not type its notice: still told"
    );
    // The restart's last look, under a read that would restart.
    let (w, st, _) = watched("changed", true, "would-restart");
    assert_eq!(w.wait, "changed", "{w:?}");
    assert_eq!(st.wait, "changed", "the restart's own word kept");
    assert!(st.restart_look, "still the restart's");

    // Negative control: a gate's word, which the read sees past.
    let (_, st, _) = watched("not-idle:busy", false, "would-announce");
    assert_eq!(st.wait, upgrade::NEXT_POINT, "the stale gate word replaced");
    // Negative control: the refused notice under another act's read.
    let (_, st, now) = watched("announce-refused:changed", false, "would-restart");
    assert_eq!(st.wait, upgrade::NEXT_POINT, "the READY's restart is due");
    assert!(!refused_row(now), "no notice is owed now");
    // Negative control: a `changed` no restart's look said.
    let (_, st, _) = watched("changed", false, "would-restart");
    assert_eq!(st.wait, upgrade::NEXT_POINT);
    assert!(!st.restart_look);
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
}

// ------------------------------------------------ the derived machine's look

/// Tab one at an idle point the loop never takes: the stand-in's Claude reads
/// `idle` and the transcript holds no limit, so the real read's reducer would
/// ask again (its window long run): `would-announce`.
fn tab_one_idle(one: &mut TabOne) {
    let pid = one.agent.id();
    let file = one.opts.home.join(format!(".claude/sessions/{pid}.json"));
    let text = std::fs::read_to_string(&file).expect("session file");
    std::fs::write(
        &file,
        text.replace("\"status\":\"busy\"", "\"status\":\"idle\""),
    )
    .expect("idle");
    one.sf.status = "idle".to_string();
    let project = one.opts.home.join(".claude/projects/p");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"keep the disk watch going\"}}\n",
    )
    .expect("transcript");
}

/// The derived machine's state for one look at tab one's record: its projection
/// ([`progress_projection`] at `now`), the look's `posture`, the limit's reset still ahead,
/// nothing under the stand-in agent, no READY, and the `keep` gap latch clear.
fn tab_one_state(one: &TabOne, posture: i64, now: u64) -> aterm_spec::interp::State {
    let row = rows(&one.opts)
        .into_iter()
        .find(|r| r.session == SESSION)
        .expect("tab #1's row");
    let mut s = aterm_models_harness::harness_upgrade_progress_model().init_state();
    for (k, v) in progress_projection(&row, now) {
        s.insert(k, v);
    }
    s.insert("posture", posture);
    for (k, v) in [("reset", 0), ("work", 0), ("ready", 0), ("keep", 0)] {
        s.insert(k, v);
    }
    s
}

/// The variables a look moves, as the machine names them.
const LOOKED: [&str; 4] = ["phase", "rearmed", "cause", "clock"];

fn looked(s: &aterm_spec::interp::State) -> Vec<(&'static str, i64)> {
    LOOKED.iter().map(|k| (*k, s[*k])).collect()
}

/// The real `watch_with` (the real visit as a dry run, a socket refusing all but reads, an
/// injected clock) moves tab one's record as `HarnessUpgradeProgress`'s `Watch` and
/// `WatchRearms` move the machine, the record projected before and after
/// ([`progress_projection`]), with zero input requests:
///
/// * (a) at the wall, its reset ahead: the look records the limit and the reset the transcript
///   names (`St::limit_until`), holds the announced round's clock to the reset and leaves the
///   move clock where it was;
/// * (b) at an idle point the loop never takes: the read would ask again (`would-announce`)
///   and nothing holds its point, so the look records [`upgrade::NEXT_POINT`] over
///   `background` and clears the processes a waiting look named (the machine's round, at its
///   last ask, gives up where this one asks again: both act, with the point open). Negative
///   control: the `keep` latch's machine (`RecordedWordStands`) leaves `background`;
/// * (c) a round that gave up and has rested, at the wall: the watch re-arms it with no hands
///   and records what holds the new round (the limit, `would-rearm:unanswered@limited`),
///   never the neutral word: `WatchRearms` then `Watch` in one pass. The move clock is kept:
///   it is the classifier's horizon for the agent's own work (`Row::own_work`).
#[test]
fn the_real_watch_is_the_derived_look() {
    use aterm_models_harness::harness_upgrade_progress_model;
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let today = aterm_spec::interp::with_buggy(&harness_upgrade_progress_model(), 0);
    let fire = |m: &aterm_spec::derive::Model, action: &str, s: &aterm_spec::interp::State| {
        let mut next = s.clone();
        assert!(m.fire(action, &mut next), "{action} at {s:?}");
        next
    };

    // (a) At the wall.
    let one = TabOne::new("t1c-derived-wall", "2.1.283");
    let reads = std::cell::Cell::new(0);
    let before = tab_one_state(&one, 3, NOW_28TH);
    assert_eq!(
        looked(&before),
        [("phase", 1), ("rearmed", 0), ("cause", 0), ("clock", 1)],
        "tab #1 as it stood: a day behind, held by its own work"
    );
    assert_eq!(one.record().limit_until, 0, "a 0.94 record keeps no reset");
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert_eq!(w.wait, "limited", "{w:?}");
    assert_eq!(
        one.record().limit_until,
        RESETS,
        "the reset the transcript names, kept for the view"
    );
    let row = rows(&one.opts)
        .into_iter()
        .find(|r| r.session == SESSION)
        .expect("tab #1's row");
    assert!(!row.limit_passed(NOW_28TH), "its reset is ahead");
    assert!(
        row.limit_passed(RESETS + LIMIT_GRACE_S),
        "and a grace past it, it is told"
    );
    let after = tab_one_state(&one, 3, NOW_28TH);
    assert_eq!(
        looked(&after),
        looked(&fire(&today, "Watch", &before)),
        "{w:?}"
    );
    assert_eq!(after["cause"], 2, "the limit, recorded");
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
    drop(one);

    // (b) At an idle point the loop never takes.
    let mut one = TabOne::new("t1c-derived-idle", "2.1.283");
    tab_one_idle(&mut one);
    let before = tab_one_state(&one, 2, NOW_28TH);
    // [`TabOne::read`] with the agent launched as a plain `claude` from a `zsh` (a dead pid
    // named in the table), so the notice's line is planned and the read says what it would do.
    let shell = dead_pid();
    let mut t = table();
    t.push((shell, 1, "zsh".into()));
    let mut args = atpkg::caller_shell::process_args(one.agent.id()).expect("argv");
    args.argv = vec!["claude".to_string()];
    let read = |o: &Opts| {
        assert!(o.dry_run, "the watch reads through a dry run");
        let files = session_files(&o.home);
        visit_with_claim(
            o,
            &one.sf,
            files.as_deref(),
            &t,
            &one.targets,
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    assert_eq!(one.record().wait, "background", "the last word, before");
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &read);
    assert_eq!(w.step, "would-announce", "{w:?}: nothing holds its point");
    assert_eq!(w.wait, upgrade::NEXT_POINT, "{w:?}");
    let st = one.record();
    assert_eq!(st.wait, upgrade::NEXT_POINT, "the stale word replaced");
    assert!(
        st.held_by.is_empty(),
        "no holder beside it: {:?}",
        st.held_by
    );
    let after = tab_one_state(&one, 2, NOW_28TH);
    assert_eq!(
        looked(&after),
        looked(&fire(&today, "Watch", &before)),
        "{w:?}"
    );
    assert_eq!(
        after["cause"], 3,
        "aterm's own class: nothing holds the point"
    );
    let row = rows(&one.opts)
        .into_iter()
        .find(|r| r.session == SESSION)
        .expect("tab #1's row");
    assert!(
        row.line(NOW_28TH).contains(" cause=next-point "),
        "{}",
        row.line(NOW_28TH)
    );
    // Negative control: the `keep` latch's look leaves `background`.
    let kept = fire(&today, "Watch", &with_latch(&before, "keep"));
    assert_eq!(kept["cause"], 0, "the old look");
    assert_ne!(
        kept["cause"], after["cause"],
        "main's look records the change"
    );
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
    drop(one);

    // (c) A round that gave up and has rested.
    let one = TabOne::new("t1c-derived-rearm", "2.1.283");
    let mut st = one.record();
    st.phase = Phase::Failed(upgrade::GAVE_UP.to_string());
    st.failed_at = NOW_28TH - upgrade::RETRY_S - 60;
    st.stop_streak = 1;
    st.streak_why = upgrade::GAVE_UP.to_string();
    write_verbatim(&one.opts, SESSION, &st);
    let reads = std::cell::Cell::new(0);
    let before = tab_one_state(&one, 3, NOW_28TH);
    assert_eq!((before["phase"], before["clock"]), (2, 1), "rested");
    let w = watch_with(&one.opts, TAB_ONE, &looker(), NOW_28TH, &one.read(&reads));
    assert!(w.wrote.iter().any(|x| x == "rearmed:unanswered"), "{w:?}");
    assert_eq!(w.step, "would-rearm:unanswered@limited", "{w:?}");
    let rearmed = one.record();
    let after = tab_one_state(&one, 3, NOW_28TH);
    assert_eq!(
        looked(&after),
        looked(&fire(
            &today,
            "Watch",
            &fire(&today, "WatchRearms", &before)
        )),
        "{w:?}"
    );
    assert_eq!(rearmed.wait, "limited", "the new round's word: the wall");
    assert_eq!(after["cause"], 2, "a limit: the wall, recorded");
    assert_eq!(
        rearmed.behind_since(),
        st.behind_since(),
        "the move clock kept"
    );
    assert_eq!(one.hands(), Vec::<String>::new(), "ZERO input requests");
    drop(one);

    let entered = xref::entered_anchor_ids();
    for id in [
        "HarnessUpgradeProgress::Watch @ watch_with",
        "HarnessUpgradeProgress::WatchRearms @ watch_with",
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
    }
}

/// The watch reads the limit at the second it is given, its read included, never the wall
/// clock's (else every watch here handed [`NOW_28TH`] would read the limit as over once the
/// real clock passes [`RESETS`]). Before its reset: `limited` and the clock held; past it,
/// neither. The thread's clock is the wall's again after.
#[test]
fn the_watch_reads_the_limit_at_the_second_it_is_given() {
    let before = TabOne::new("t1c-clock-before", "2.1.283");
    let reads = std::cell::Cell::new(0);
    let w = watch_with(
        &before.opts,
        TAB_ONE,
        &looker(),
        NOW_28TH,
        &before.read(&reads),
    );
    assert_eq!(w.wait, "limited", "before its reset: {w:?}");
    assert!(
        w.wrote.iter().any(|x| *x == format!("held:{RESETS}")),
        "{w:?}"
    );
    assert_ne!(now_s(), NOW_28TH, "the wall clock again, after the watch");
    drop(before);

    let past = TabOne::new("t1c-clock-past", "2.1.283");
    let w = watch_with(
        &past.opts,
        TAB_ONE,
        &looker(),
        RESETS + LIMIT_GRACE_S,
        &past.read(&reads),
    );
    assert_ne!(w.wait, "limited", "past its reset: {w:?}");
    assert!(!w.wrote.iter().any(|x| x.starts_with("held:")), "{w:?}");
    assert_eq!(w.limit_until, Some(RESETS), "the reset, still named");
    assert_eq!(reads.get(), 2);
}

/// `s` with the gap latch `name` set.
fn with_latch(s: &aterm_spec::interp::State, name: &'static str) -> aterm_spec::interp::State {
    let mut s = s.clone();
    s.insert(name, 1);
    s
}
