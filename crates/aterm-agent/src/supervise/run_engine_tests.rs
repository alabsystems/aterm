// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The engine over the scripted server (`Mock`): the approval policy in the loop, the press
// guard and fence, the wake, the lifecycle, the escalation and the hosted entry. Included
// into `run.rs`'s test module (`mod engine`), where `Mock` and its helpers live.

use crate::supervise::policy::WorkerEnv;

/// The rm circuit-breaker box (`policy/fixtures/cap-rm.txt`) with its command
/// row replaced by `cmd`.
fn rm_box(cmd: &str) -> Vec<String> {
    const ROW: &str = "   S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done";
    let rows: Vec<String> = include_str!("policy/fixtures/cap-rm.txt")
        .lines()
        .map(str::to_string)
        .collect();
    assert!(rows.iter().any(|r| r == ROW), "the capture's command row");
    rows.into_iter()
        .map(|r| if r == ROW { format!("   {cmd}") } else { r })
        .collect()
}

/// A busy screen in a bypass-permissions session: the footer names the mode.
fn bypass_busy() -> Vec<String> {
    let mut r = rows(&["⏺ Cleaning up.", "", "✻ Tidying… (4s)", ""]);
    r.extend(composer(
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt",
    ));
    r
}

/// Fixed process inputs: a hermetic worker.
fn owner_env() -> ApprovalEnv {
    ApprovalEnv {
        home: Some(PathBuf::from("/Users/_owner")),
        uid: 502,
        tmpdir: None,
        worker: WorkerSource::Fixed(Ok(WorkerEnv::hermetic(None))),
    }
}

const SCRATCH_RM: &str = "S=/private/tmp/claude-502/x; rm -rf \"$S/t5\"";

fn ledger_file(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("ledger", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-1.jsonl");
    (dir, path)
}

fn ledger_rows(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A box swapped between the read and the press is not pressed: the press is guarded by the
/// judged row, so the server writes nothing (`OK skipped`) and the new box is decided afresh.
/// A generic guard (`Do.you.want.to.proceed`) would match the swapped box.
#[test]
fn a_box_swapped_between_the_read_and_the_press_is_not_pressed() {
    let (dir, notes) = notes_file("swap");
    let mut m = fenced(vec![bash_one_row()]);
    m.swap_at_key = Some(write_prompt());
    let mut s = session(&mut m, None);
    let (out, code) = s
        .supervise(&auto(30, Some(notes.clone())))
        .expect("supervise");
    assert_eq!(code, 0);
    assert!(
        out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
        "the new box is the manager's: {out}"
    );
    let expected = format!(
        "key if-human=7 if-gen=1.101 {}",
        G_PRESS.trim_start_matches("key ")
    );
    assert_eq!(m.presses(), [&expected], "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert!(
        !lines.iter().any(|l| l.contains("approved")),
        "nothing approved: {lines:?}"
    );
    // Negative control: the guard of the judged row matches no row of the
    // swapped box on the server's own matcher, and matches its own box.
    let m =
        aterm_observe::row_matcher(G_PRESS.trim_start_matches("key if=").trim_end_matches(" 1"))
            .expect("compiles");
    assert!(!write_prompt().iter().any(|r| m.matches(r)));
    assert!(bash_one_row().iter().any(|r| m.matches(r)));
}

/// A server that fences on the screen generation (`help key` names `if-gen=`, `text --json`
/// carries `"gen"`): the press carries the judged read's generation, and a row that ticked
/// meanwhile (`OK skipped reason=changed`) is decided again from a fresh read; a screen
/// ticking under every press is handed over after five skips, never pressed unfenced (the row
/// guard binds only the first command row). The probe reads the real `help key` text, wrapped
/// as the server prints it.
#[test]
fn a_fenced_press_on_a_moved_screen_is_decided_again_from_a_fresh_read() {
    let help = aterm_types::control_verbs::spec("key")
        .expect("the key verb")
        .entry_lines()
        .join("\n");
    let fenced = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        m.help.clone_from(&help);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(7);
        m.human = vec!["null"];
        m
    };
    let mut m = fenced(vec![
        bash_one_row(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.tick_at_key = 1;
    let mut s = session(&mut m, None);
    let (out, code) = s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("idle\n"), "{out}");
    assert_eq!(s.caps().gen_fence, Some(true));
    let g = G_PRESS.trim_start_matches("key ");
    let presses = m.presses();
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert_eq!(*presses[0], format!("key if-human=7 if-gen=1.101 {g}"));
    assert_eq!(*presses[1], format!("key if-human=7 if-gen=1.103 {g}"));
    assert_eq!(count(&m, "help key"), 1, "asked once");
    assert!(
        !m.requests.iter().any(|r| r.contains("if-seq=")),
        "the retired seq fence is never sent: {:?}",
        m.requests
    );

    // Every press finds the screen moved: five fenced tries, then the box
    // is the manager's — no press without the fence.
    let mut m = fenced(vec![bash_one_row(); 8]);
    m.tick_at_key = 10;
    let mut s = session(&mut m, None);
    let (out, _) = s.supervise(&auto(30, None)).expect("supervise");
    assert!(out.starts_with("prompt\n"), "handed over: {out}");
    let presses = m.presses();
    assert_eq!(presses.len(), 5, "{:?}", m.requests);
    assert!(
        presses
            .iter()
            .all(|p| p.starts_with("key if-human=7 if-gen=")),
        "never unfenced: {presses:?}"
    );
}

/// A disabled rule's limit reaches the box: under `approve = "safe"` a Read of `/etc/hosts`
/// is pressed, and with `read_outside_cwd = false` handed over.
#[test]
fn a_retired_key_takes_its_safe_rule_away_in_the_loop() {
    let read_box: Vec<String> = aterm_phase::prompt::fixtures::read_box()
        .into_iter()
        .map(|r| {
            if r == "   ~/.ssh/config" {
                "   /etc/hosts".to_string()
            } else {
                r
            }
        })
        .collect();
    let run = |text: &str| {
        let (policy, _) = SupervisorConfig::from_aterm_toml(text);
        let mut m = fenced(vec![read_box.clone()]);
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                approve: policy.approve,
                withheld: policy.withheld,
                ..auto(30, None).policy
            },
            ..auto(30, None)
        };
        s.supervise(&opts).expect("supervise");
        m.presses().len()
    };
    assert_eq!(
        run("[harness]\napprove = \"safe\"\n"),
        1,
        "the safe rule proves it"
    );
    assert_eq!(run("[harness]\nread_outside_cwd = false\n"), 0);
}

/// The wake: a box drawn where the busy footer was, under a screen that never holds still, is
/// read the moment the footer leaves (`await gone`), with no idle step before the press.
#[test]
fn a_read_box_under_a_ticking_screen_is_approved_within_one_wake() {
    let mut m = fenced(vec![
        busy_screen(),
        busy_screen(),
        bash_one_row(),
        busy_screen(),
    ]);
    m.idle_never = true;
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert_eq!(
        lines[0], "APPROVED seq=103 git log --oneline -5",
        "{lines:?}"
    );
    let press = m
        .requests
        .iter()
        .position(|r| {
            r == &format!(
                "key if-human=7 if-gen=1.103 {}",
                G_PRESS.trim_start_matches("key ")
            )
        })
        .expect("pressed");
    assert!(
        !m.requests[..press]
            .iter()
            .any(|r| r.starts_with("await idle")),
        "no idle step before the press: {:?}",
        m.requests
    );
}

/// `ERR halted` (a `hold`) parks the loop (`await inbox since=0 kinds=hold`) until `status`
/// says `hold=0`, then the box is decided again and pressed; journaled `HALTED`/`UNHALTED`,
/// the refused press ledgered.
#[test]
fn err_halted_parks_until_the_hold_lifts_and_the_loop_goes_on() {
    let (jdir, journal) = journal_file("halted");
    let (ldir, ledger) = ledger_file("halted");
    let mut m = fenced(vec![
        bash_one_row(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.key_replies
        .push_back(err("halted reason=owner origin=local"));
    m.hold = "1";
    // The box's own `status` read (its program) is the first.
    m.hold_lifts_after = Some(2);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30, None)
    };
    let (lines, code) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(code, 1, "{lines:?}");
    assert_eq!(
        lines[0], "APPROVED seq=102 git log --oneline -5",
        "{lines:?}"
    );
    let first_press = format!(
        "key if-human=7 if-gen=1.101 {}",
        G_PRESS.trim_start_matches("key ")
    );
    let second_press = format!(
        "key if-human=7 if-gen=1.102 {}",
        G_PRESS.trim_start_matches("key ")
    );
    assert_eq!(
        m.presses(),
        [&first_press, &second_press],
        "{:?}",
        m.requests
    );
    let first = m
        .requests
        .iter()
        .position(|r| r.ends_with(G_PRESS.trim_start_matches("key ")))
        .expect("press");
    assert_eq!(
        m.requests[first + 1..first + 4],
        [
            "status",
            "await inbox since=0 kinds=hold timeout 20000",
            "status",
        ],
        "{:?}",
        m.requests
    );
    let (records, _) = journal_records(&journal);
    let notes: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "other")
        .map(|r| r.line.as_str())
        .collect();
    assert!(
        notes[0].starts_with("HALTED seq=101 key if-human=7 if-gen=1.101 if="),
        "{notes:?}"
    );
    assert!(notes[1].starts_with("UNHALTED seq=101 after "), "{notes:?}");
    // The box's rows (the turn-end policy's record of the idle point after
    // it is no decision of the box's).
    let rows: Vec<String> = ledger_rows(&ledger)
        .into_iter()
        .filter(|r| {
            !r.contains(&format!(
                "\"rule_id\":\"{}\"",
                crate::supervise::approvals::RULE_POINT
            ))
        })
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains("\"decision\":\"deferred\""), "{rows:?}");
    assert!(rows[1].contains("\"decision\":\"approved\""), "{rows:?}");
    assert!(
        rows[1].contains("\"rule_id\":\"read-only@classify-v3\""),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&ldir);

    // `ERR busy lease=…` (another driver holds the keyboard) backs off and
    // looks again; never the loop's end.
    let mut m = fenced(vec![
        bash_one_row(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.key_replies.push_back(err("busy lease=s-9"));
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert_eq!(
        lines[0], "APPROVED seq=102 git log --oneline -5",
        "{lines:?}"
    );
    let first = m
        .requests
        .iter()
        .position(|r| r.ends_with(G_PRESS.trim_start_matches("key ")))
        .expect("press");
    assert!(
        m.requests[first + 1].starts_with("await gone ^\\x20{3}git"),
        "the back-off waits on the box leaving: {:?}",
        m.requests
    );
}

/// A watch that starts over a stale `limited:` badge with the screen idle unsets it first.
/// Negative controls: another owner's badge is left alone (the key proves ownership); ours
/// over a box still up is adopted and cleared when the box goes.
#[test]
fn a_stale_attention_is_reconciled_at_the_start() {
    let (dir, journal) = journal_file("stale");
    let mut m = Mock::new(vec![idle_screen()]);
    m.attention = Some("limited: You've hit your weekly limit".to_string());
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(lines[0], "EVENT idle seq=102 ⏺ Done.", "{lines:?}");
    assert_eq!(
        m.requests[..3],
        [
            "meta",
            "text --json tail=40",
            "meta unset attention owner=supervisor"
        ],
        "{:?}",
        m.requests
    );
    assert_eq!(m.attention, None);
    let (records, _) = journal_records(&journal);
    assert!(
        records[0]
            .line
            .starts_with("CLEARED seq=101 stale attention=OK"),
        "{records:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let mut m = Mock::new(vec![idle_screen()]);
    m.human_attention = Some("limited: a person's own note".to_string());
    m.vanish_after = Some(0);
    watch_lines(&mut m, &auto(30, None));
    assert_eq!(count(&m, "meta unset"), 0, "{:?}", m.requests);
    assert_eq!(
        m.human_attention.as_deref(),
        Some("limited: a person's own note")
    );

    let rm = write_prompt();
    let ours = super::super::escalate::attention_text(
        &aterm_phase::ClaudeReader,
        &turn_of_rows(&rm),
        "a restart",
    );
    // Read once by the reconcile, once by the first look.
    let mut m = Mock::new(vec![rm.clone(), rm, busy_screen(), idle_screen()]);
    m.attention = Some(ours);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_manager(Some("@s-9".to_string()));
    });
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert_eq!(
        count(&m, "meta set attention"),
        0,
        "adopted: {:?}",
        m.requests
    );
    assert_eq!(count(&m, "post"), 0, "no second ask: {:?}", m.requests);
    assert_eq!(
        count(&m, "meta unset attention owner=supervisor"),
        1,
        "cleared when it went"
    );
    assert_eq!(m.attention, None);
}

fn turn_of_rows(rows: &[String]) -> Turn {
    Turn {
        phase: worker_phase(rows),
        screen: Screen {
            rows: rows.to_vec(),
            ..Screen::default()
        },
        timed_out: false,
    }
}

/// The rm breaker in a bypass session with the cwd known: every operand under a scratch root,
/// pressed and ledgered `rm-breaker@v3`; `S=/usr`, escalated. Negative controls: the cwd
/// unknown, and outside bypass.
#[test]
fn the_rm_breaker_is_approved_under_a_scratch_root_and_escalated_otherwise() {
    let (ldir, ledger) = ledger_file("rm");
    let mut m = fenced(vec![bypass_busy(), rm_box(SCRATCH_RM), busy_screen()]);
    m.cwd = Some("/Users/_owner/proj".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    // The decision's subject: the command and where its operands resolve.
    assert_eq!(
        lines[0],
        format!("APPROVED seq=102 {SCRATCH_RM} => /private/tmp/claude-502/x/t5"),
        "{lines:?}"
    );
    let guard = crate::supervise::policy::row_guard(&format!("   {SCRATCH_RM}"));
    let press = format!("key if-human=7 if-gen=1.102 if={guard} 1");
    assert_eq!(m.presses(), [press.as_str()], "{:?}", m.requests);
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("\"rule_id\":\"rm-breaker@v3\""),
        "{rows:?}"
    );
    assert!(rows[0].contains("\"decision\":\"approved\""), "{rows:?}");
    let _ = std::fs::remove_dir_all(&ldir);

    let usr = "S=/usr; rm -rf \"$S/t5\"";
    let mut m = fenced(vec![bypass_busy(), rm_box(usr)]);
    m.cwd = Some("/Users/_owner/proj".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let set = m
        .requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("escalated");
    assert!(
        set.starts_with(&format!("claude rm-breaker: {usr} (rm circuit breaker")),
        "{set}"
    );

    for (why, cwd, first) in [
        ("cwd unknown", None, bypass_busy()),
        (
            "outside a bypass session",
            Some("/Users/_owner/proj"),
            busy_screen(),
        ),
    ] {
        let mut m = fenced(vec![first, rm_box(SCRATCH_RM)]);
        m.cwd = cwd.map(str::to_string);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
            s.set_approval_env(owner_env());
        });
        assert!(lines[0].starts_with("EVENT prompt "), "{why}: {lines:?}");
        assert!(m.presses().is_empty(), "{why}: {:?}", m.requests);
        let set = m
            .requests
            .iter()
            .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
            .expect("escalated");
        assert!(set.contains(why), "{why}: {set}");
    }
}

/// An idle bypass session: a reply, the composer, and the footer naming the mode.
fn bypass_idle() -> Vec<String> {
    let mut r = rows(&["⏺ Ready.", ""]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// The footer race: the rm breaker's bypass proof is a footer an earlier read showed, so an
/// idle point read before the footer appeared is waited on by its content
/// ([`Session::agent_wake`]): the verdict does not move on the frame that draws the footer.
/// Negative control: a footer read at the first idle point is waited on by the verdict.
#[test]
fn an_idle_point_read_before_the_footer_waits_for_the_footer_frame() {
    // Still starting: blank on the first read and again once the idle
    // window has passed. Mid-draw: the composer drawn, its footer row not yet.
    let blank = rows(&["", ""]);
    let mut mid_draw = bypass_idle();
    mid_draw.pop();
    for (why, before) in [
        ("still starting", vec![blank.clone(), blank]),
        ("mid-draw", vec![mid_draw]),
    ] {
        let mut script = before;
        script.extend([bypass_idle(), rm_box(SCRATCH_RM), busy_screen()]);
        let mut m = fenced(script);
        m.agent_pushes = true;
        m.cwd = Some("/Users/_owner/proj".to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
            s.set_approval_env(owner_env());
        });
        assert!(
            lines.iter().any(|l| l.starts_with("APPROVED seq=")),
            "{why}: {lines:?}\n{:?}",
            m.requests
        );
        assert_eq!(m.presses().len(), 1, "{why}: {:?}", m.requests);
        assert_eq!(
            count(&m, "meta set attention"),
            0,
            "{why}: {:?}",
            m.requests
        );
        // The footerless point waited on by its content, the footer's frame by the verdict.
        let waits: Vec<&str> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("await seq") || r.starts_with("await agent"))
            .map(|r| {
                if r.starts_with("await seq") {
                    "seq"
                } else {
                    "agent"
                }
            })
            .collect();
        assert_eq!(waits[..2], ["seq", "agent"], "{why}: {:?}", m.requests);
    }

    let mut m = fenced(vec![
        bypass_idle(),
        bypass_idle(),
        rm_box(SCRATCH_RM),
        busy_screen(),
    ]);
    m.agent_pushes = true;
    m.cwd = Some("/Users/_owner/proj".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(
        lines.iter().any(|l| l.starts_with("APPROVED seq=")),
        "{lines:?}"
    );
    let first_wait = m
        .requests
        .iter()
        .find(|r| r.starts_with("await seq") || r.starts_with("await agent"))
        .expect("the idle point's wait");
    assert!(
        first_wait.starts_with("await agent busy,prompt"),
        "{:?}",
        m.requests
    );
}

/// The content wait is the start's alone: once a busy frame shows (a busy footer names every
/// mode but the default), a session that names no mode gets the verdict wait back. Negative
/// control: the idle point read before any busy frame is waited on by its content.
#[test]
fn a_session_naming_no_mode_gets_the_verdict_wait_back_at_its_first_busy_read() {
    let mut no_mode = idle_screen();
    no_mode.pop();
    assert_eq!(footer_mode(&no_mode), None, "the idle point names no mode");
    assert_eq!(footer_mode(&busy_screen()), None, "nor does the busy frame");
    for (why, script, expected) in [
        (
            "after a busy read",
            vec![busy_screen(), no_mode.clone(), no_mode.clone()],
            "await agent busy,prompt",
        ),
        (
            "still starting",
            vec![no_mode.clone(), no_mode.clone()],
            "await seq",
        ),
    ] {
        let mut m = fenced(script);
        m.agent_pushes = true;
        m.vanish_after = Some(0);
        let _ = watch_lines_with(&mut m, &auto(30, None), |s| {
            s.set_approval_env(owner_env());
        });
        // The idle point's wait: the first after the first read of it.
        let idle_read = m
            .requests
            .iter()
            .enumerate()
            .filter(|(_, r)| r.starts_with("text"))
            .map(|(i, _)| i)
            .nth(usize::from(why == "after a busy read"))
            .unwrap_or_else(|| panic!("{why}: the idle point's read: {:?}", m.requests));
        let wait = m.requests[idle_read..]
            .iter()
            .find(|r| r.starts_with("await seq") || r.starts_with("await agent"))
            .unwrap_or_else(|| panic!("{why}: the idle point's wait: {:?}", m.requests));
        assert!(
            wait.starts_with(expected),
            "{why}: {wait}\n{:#?}",
            m.requests
        );
    }
}

/// What tells [`park_as_worker`] to park.
const WORKER_PARK: &str = "SUPERVISE_ENGINE_TEST_PARK";

/// The stand-in worker's body: a no-op unless asked to park; parked, it sleeps 120 s, past
/// the minute [`StandIn::spawn`] may wait for it to start. Every [`StandIn`] is killed on drop.
#[test]
fn park_as_worker() {
    if std::env::var_os(WORKER_PARK).is_some() {
        std::thread::sleep(Duration::from_secs(120));
    }
}

/// A stand-in worker: this test binary, parked, with exactly `env` (and the
/// word that parks it); not `/bin/sleep`, whose environment macOS hides as a
/// platform binary's. Killed on drop.
struct StandIn(aterm_uds::spawnfd::Child);

impl StandIn {
    fn spawn(env: &[(String, String)]) -> Self {
        let mut c = aterm_uds::spawnfd::command(std::env::current_exe().expect("exe"));
        c.args(["supervise::run::tests::engine::park_as_worker", "--exact"])
            .env_clear()
            .env(WORKER_PARK, "1")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        // Owned at once, so every path out of here reaps it.
        let worker = Self(c.spawn().expect("the stand-in worker"));
        // Until the exec, the kernel shows this process's environment.
        for _ in 0..3_000 {
            if atpkg::caller_shell::process_args(worker.pid())
                .is_some_and(|a| a.env_var(WORKER_PARK).is_some())
            {
                return worker;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the stand-in worker {} never started", worker.pid());
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A scratch directory for a loop test, canonical, removed by the caller.
fn loop_scratch(tag: &str) -> PathBuf {
    let dir = crate::supervise::test_scratch_path("loop", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::canonicalize(&dir).expect("canonical")
}

/// `git <args>` in `dir`, hermetic, asserted ok.
fn loop_git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::spawnfd::command("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}");
}

/// A shell snapshot as Claude Code 2.1.284 writes it for a zsh whose
/// startup sets `options` (`setopt …` rows) beyond a login shell's defaults.
fn zsh_snapshot(options: &str) -> String {
    format!(
        "# Snapshot file\nunalias -a 2>/dev/null || true\n# Functions\n# Shell Options\n\
         setopt nohashdirs\nsetopt login\n{options}# Aliases\nalias -- run-help=man\n\
         # Check for rg availability\nexport PATH='/usr/bin:/bin'\n"
    )
}

/// One shell snapshot of a zsh started with `options`, in the Claude Code directory `claude`.
fn write_snapshot(claude: &Path, options: &str) {
    let dir = claude.join("shell-snapshots");
    std::fs::create_dir_all(&dir).expect("shell-snapshots");
    std::fs::write(
        dir.join("snapshot-zsh-1790000000000-a1b2c3.sh"),
        zsh_snapshot(options),
    )
    .expect("snapshot");
}

/// A worker's scratch `$HOME` (never the developer's), its Claude Code directory holding a
/// default zsh snapshot; removed on drop, so the test holds it while its workers run.
fn worker_home() -> crate::supervise::ScratchDir {
    let home = crate::supervise::ScratchDir::new("loop", "worker-home");
    write_snapshot(&home.join(".claude"), "");
    home
}

/// A worker's hermetic environment in session `sid`: this process's `PATH`, no system or
/// global git config, and `home` ([`worker_home`]) as `$HOME`.
fn worker_vars(sid: &str, home: &Path) -> Vec<(String, String)> {
    vec![
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
        ("ATERM_PARENT_SESSION_ID".to_string(), sid.to_string()),
        ("HOME".to_string(), home.display().to_string()),
    ]
}

/// One `approve = "safe"` loop over a `git log` box in `cwd`, `worker` the foreground process
/// of session `3`/`sid` ([`WorkerSource::Session`]): the presses and the ledger's rows.
fn judge_git_box_with(worker: &StandIn, sid: &str, cwd: &Path) -> (usize, Vec<String>) {
    let (ldir, ledger) = ledger_file(&format!("worker-{}", worker.pid()));
    let mut m = fenced(vec![bash_one_row()]);
    m.cwd = Some(cwd.display().to_string());
    m.status_sid = "3";
    m.who = Some(format!(
        "2 s-00000000000000000002 driving=- watchers=0 turns=0 alive nonce=0 fgpgid=77\n\
         3 {sid} driving=- watchers=0 turns=0 alive nonce=0 fgpgid={}\n",
        worker.pid()
    ));
    let mut s = session(&mut m, None);
    s.set_approval_env(ApprovalEnv {
        worker: WorkerSource::Session,
        ..owner_env()
    });
    s.set_approval_ledger(Some(ledger.clone()));
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            approve: Approve::Safe,
            ..auto(30, None).policy
        },
        ..auto(30, None)
    };
    s.supervise(&opts).expect("supervise");
    assert!(
        m.requests.iter().any(|r| r == "who"),
        "the worker is read from the roster: {:?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    let _ = std::fs::remove_dir_all(&ldir);
    (m.presses().len(), rows)
}

/// A git box is judged in the worker's own environment, read from the session's foreground
/// process (`who`'s `fgpgid=`, bound by `ATERM_PARENT_SESSION_ID`), so its `GIT_CONFIG_*` is
/// seen; another session's process is no worker. Negative control: without them, pressed.
#[test]
fn a_git_box_is_judged_in_the_workers_own_environment() {
    let repo = loop_scratch("worker-env");
    loop_git(&repo, &["init", "-q", "."]);
    let sid = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 1);
    let home = worker_home();
    let clean = StandIn::spawn(&worker_vars(&sid, &home));
    assert_eq!(
        judge_git_box_with(&clean, &sid, &repo).0,
        1,
        "a worker whose configuration runs nothing: pressed"
    );
    drop(clean);

    let mut vars = worker_vars(&sid, &home);
    for (k, v) in [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.fsmonitor"),
        ("GIT_CONFIG_VALUE_0", "./evil.sh"),
    ] {
        vars.push((k.to_string(), v.to_string()));
    }
    let configured = StandIn::spawn(&vars);
    let (presses, rows) = judge_git_box_with(&configured, &sid, &repo);
    assert_eq!(presses, 0, "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains("core.fsmonitor")),
        "escalated naming the worker's key: {rows:?}"
    );
    drop(configured);

    // Another session's process is no worker of this one — and the reason
    // says so without that session's id, a variable of the process's.
    let stranger = StandIn::spawn(&worker_vars("s-99999999999999999999", &home));
    let (presses, rows) = judge_git_box_with(&stranger, &sid, &repo);
    assert_eq!(presses, 0, "{rows:?}");
    assert!(
        rows.iter()
            .any(|r| r.contains("belongs to another session")),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("s-99999999999999999999")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// A RECORDED GIT BOX KEEPS THE WORKER'S SECRETS OFF ITS TAPE: a whole
/// `approve = "safe"` run, recorded, over a `git log` box whose worker is a
/// real process planting an API key in its environment (read from the
/// session's foreground group through the kernel: `probe.worker_proc`), in
/// a repository whose configuration plants a token in an extra header, a
/// user and password in a URL rewrite and a credential helper
/// (`probe.git_view`, `probe.settings_env`). The box is pressed — none of
/// those keys runs a program — and the tape holds the worker as a digest and
/// the view as its projection: no planted secret anywhere on it. Negative
/// controls: the probes' round trips ARE on the tape (the test is not
/// vacuous), and `policy::probe`'s own test shows the same detector finding
/// a key that is logged through the door.
#[test]
fn a_recorded_git_box_keeps_the_workers_secrets_off_its_tape() {
    use crate::supervise::policy::probe::tests::{
        PLANTED_KEY, exchanges, planted, planted_repo, tape_holds,
    };
    use crate::supervise::tape::doors;
    let repo = loop_scratch("worker-secrets");
    planted_repo(&repo);
    let sid = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 5);
    let home = worker_home();
    let mut vars = worker_vars(&sid, &home);
    vars.push(("ANTHROPIC_API_KEY".to_string(), PLANTED_KEY.to_string()));
    let worker = StandIn::spawn(&vars);
    let (ldir, ledger) = ledger_file("worker-secrets");
    let mut m = fenced(vec![bash_one_row()]);
    m.cwd = Some(repo.display().to_string());
    m.status_sid = "3";
    m.who = Some(format!(
        "3 {sid} driving=- watchers=0 turns=0 alive nonce=0 fgpgid={}\n",
        worker.pid()
    ));
    let (tape, mut taped) = memory_tape(&mut m);
    let mut s = taped_session(&mut taped, &tape);
    s.set_approval_env(ApprovalEnv {
        worker: WorkerSource::Session,
        ..owner_env()
    });
    s.set_approval_ledger(Some(ledger));
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            approve: Approve::Safe,
            ..auto(30, None).policy
        },
        ..auto(30, None)
    };
    s.supervise(&opts).expect("supervise");
    drop(s);
    drop(taped);
    drop(worker);
    let read = tape.finish("done");
    assert_eq!(
        m.presses().len(),
        1,
        "nothing planted runs a program: pressed"
    );
    for door in [
        doors::PROBE_WORKER_PROC,
        doors::PROBE_GIT_VIEW,
        doors::PROBE_SETTINGS_ENV,
    ] {
        assert!(!exchanges(&read, door).is_empty(), "{door} is on the tape");
    }
    for secret in planted() {
        assert!(!tape_holds(&read, secret), "{secret} reached the tape");
    }
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&ldir);
}

/// Where the Bash tool stands is read from the transcripts under the worker's
/// `CLAUDE_CONFIG_DIR`, not this process's `~/.claude`. Negative control: without the
/// variable the worker is judged by its launch directory alone, and pressed.
#[test]
fn a_git_box_reads_the_transcripts_under_the_workers_claude_config_dir() {
    let root = loop_scratch("worker-claude-dir");
    let launch = root.join("w");
    let nested = launch.join("sub");
    std::fs::create_dir_all(&nested).expect("dirs");
    loop_git(&launch, &["init", "-q", "."]);
    loop_git(&nested, &["init", "-q", "."]);
    loop_git(&nested, &["config", "core.fsmonitor", "./evil.sh"]);
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let claude = root.join("identity-claude");
    let sid = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 2);
    let register = |pid: u32| {
        write_snapshot(&claude, "");
        std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
        std::fs::write(
            claude.join("sessions").join(format!("{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"t-1","cwd":"{}"}}"#,
                launch.display()
            ),
        )
        .expect("session file");
        let project = claude
            .join("projects")
            .join(crate::harness::footer::project_slug(&launch));
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join("t-1.jsonl"),
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\"}}\n{{\"type\":\"assistant\",\"cwd\":\"{}\"}}\n",
                launch.display(),
                nested.display()
            ),
        )
        .expect("transcript");
    };

    write_snapshot(&home.join(".claude"), "");
    let plain = worker_vars(&sid, &home);
    let worker = StandIn::spawn(&plain);
    register(worker.pid());
    assert_eq!(
        judge_git_box_with(&worker, &sid, &launch).0,
        1,
        "no Claude directory of the worker's: the launch directory alone"
    );
    drop(worker);

    let mut identity = plain.clone();
    identity.push((
        "CLAUDE_CONFIG_DIR".to_string(),
        claude.display().to_string(),
    ));
    let worker = StandIn::spawn(&identity);
    register(worker.pid());
    let (presses, rows) = judge_git_box_with(&worker, &sid, &launch);
    assert_eq!(
        presses, 0,
        "the Bash tool's repository runs its fsmonitor on the read: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("core.fsmonitor")),
        "{rows:?}"
    );
    drop(worker);
    let _ = std::fs::remove_dir_all(&root);
}

/// A Bash box is judged under the worker's shell startup (the shell snapshot in its Claude
/// Code directory): an option the classifier does not model (`setopt globsubst`), or no
/// snapshot, escalates naming why; an inert option is pressed.
#[test]
fn a_bash_box_is_judged_under_the_workers_shell_startup() {
    let repo = loop_scratch("worker-shell");
    loop_git(&repo, &["init", "-q", "."]);
    let sid = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 5);
    let home = loop_scratch("worker-shell-home");
    let vars = worker_vars(&sid, &home);

    let worker = StandIn::spawn(&vars);
    let (presses, rows) = judge_git_box_with(&worker, &sid, &repo);
    assert_eq!(presses, 0, "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains("no shell snapshot of zsh")),
        "no snapshot: unknown: {rows:?}"
    );
    drop(worker);

    write_snapshot(&home.join(".claude"), "setopt globsubst\n");
    let worker = StandIn::spawn(&vars);
    let (presses, rows) = judge_git_box_with(&worker, &sid, &repo);
    assert_eq!(presses, 0, "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains("`setopt globsubst`")),
        "escalated naming the option: {rows:?}"
    );
    drop(worker);

    write_snapshot(&home.join(".claude"), "setopt autocd\n");
    let worker = StandIn::spawn(&vars);
    assert_eq!(
        judge_git_box_with(&worker, &sid, &repo).0,
        1,
        "an inert option: pressed"
    );
    drop(worker);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);
}

/// The session's `who` row and its process group: by the addressed `s-…` id, else the local
/// id `status` gave; refused when the group is unknown or shared, or the roster is not one.
#[test]
fn the_sessions_foreground_group_is_read_off_the_roster() {
    let rows = "0 s-aaaaaaaaaaaaaaaaaaaa driving=- watchers=1 turns=0 alive nonce=00 fgpgid=101\n\
                1 s-bbbbbbbbbbbbbbbbbbbb driving=- watchers=0 turns=0 alive nonce=00 fgpgid=-\n\
                2 s-cccccccccccccccccccc driving=- watchers=0 turns=0 alive nonce=00 fgpgid=303\n\
                3 s-dddddddddddddddddddd driving=- watchers=0 turns=0 alive nonce=00 fgpgid=303\n\
                4 s-eeeeeeeeeeeeeeeeeeee driving=- watchers=0 turns=0 alive nonce=00 fgpgid=505\n";
    let group = approval_loop::session_group;
    assert_eq!(
        group(rows, None, Some("0")),
        Ok(("s-aaaaaaaaaaaaaaaaaaaa".to_string(), 101))
    );
    assert!(group(rows, None, Some("1")).is_err(), "unknown group");
    assert!(group(rows, None, Some("2")).is_err(), "a shared group");
    assert!(group(rows, None, Some("9")).is_err(), "not on the roster");
    assert!(group(rows, None, None).is_err(), "no id at all");
    assert!(
        group("OK 1\nnot a roster\n", None, Some("0")).is_err(),
        "no roster"
    );
    // The stable id wins over a local one: a `status` another instance answered (local ids
    // are per instance, and reused) cannot pick this session's row.
    assert_eq!(
        group(rows, Some("s-eeeeeeeeeeeeeeeeeeee"), Some("0")),
        Ok(("s-eeeeeeeeeeeeeeeeeeee".to_string(), 505))
    );
    assert!(
        group(rows, Some("s-ffffffffffffffffffff"), Some("0")).is_err(),
        "an addressed session missing from the roster is not stood in for by the local id"
    );
    assert!(
        group(rows, Some("s-dddddddddddddddddddd"), None).is_err(),
        "a shared group"
    );
}

/// A loop addressing its session by its `s-…` id binds the worker by that id, not by the
/// local id `status` answered, which another instance's session may hold. Negative control:
/// by local id alone it takes the stranger's row.
#[test]
fn a_loop_addressing_its_session_binds_the_worker_by_that_id() {
    let repo = loop_scratch("worker-stable-id");
    loop_git(&repo, &["init", "-q", "."]);
    let ours = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 3);
    let theirs = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 4);
    let home = worker_home();
    let mut vars = worker_vars(&ours, &home);
    for (k, v) in [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.fsmonitor"),
        ("GIT_CONFIG_VALUE_0", "./evil.sh"),
    ] {
        vars.push((k.to_string(), v.to_string()));
    }
    let worker = StandIn::spawn(&vars);
    let stranger = StandIn::spawn(&worker_vars(&theirs, &home));
    let run = |selector: Option<String>| {
        let (ldir, ledger) = ledger_file(&format!("stable-{}", worker.pid()));
        let mut m = fenced(vec![bash_one_row()]);
        m.cwd = Some(repo.display().to_string());
        m.status_sid = "3";
        m.who = Some(format!(
            "2 {ours} driving=- watchers=0 turns=0 alive nonce=0 fgpgid={}\n\
             3 {theirs} driving=- watchers=0 turns=0 alive nonce=0 fgpgid={}\n",
            worker.pid(),
            stranger.pid()
        ));
        let mut s = session(&mut m, selector);
        s.set_approval_env(ApprovalEnv {
            worker: WorkerSource::Session,
            ..owner_env()
        });
        s.set_approval_ledger(Some(ledger.clone()));
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                approve: Approve::Safe,
                ..auto(30, None).policy
            },
            ..auto(30, None)
        };
        s.supervise(&opts).expect("supervise");
        let rows = ledger_rows(&ledger);
        let _ = std::fs::remove_dir_all(&ldir);
        (m.presses().len(), rows)
    };
    let (presses, rows) = run(Some(format!("@{ours}")));
    assert_eq!(presses, 0, "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains("core.fsmonitor")),
        "the addressed session's worker is the one judged: {rows:?}"
    );
    let (presses, rows) = run(None);
    assert_eq!(
        presses, 1,
        "by local id the stranger's row is taken: {rows:?}"
    );
    drop(worker);
    drop(stranger);
    let _ = std::fs::remove_dir_all(&repo);
}

/// The approval ledger: one row per decision — an approval under its rule,
/// an escalation with the reason — each with the whole command's SHA-256.
#[test]
fn every_decision_is_a_ledger_row() {
    let (ldir, ledger) = ledger_file("rows");
    let mut m = fenced(vec![bash_one_row(), busy_screen(), write_prompt()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_approval_ledger(Some(ledger.clone()));
    s.supervise(&auto(30, None)).expect("supervise");
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains("\"sid\":\"s-1\""), "{rows:?}");
    assert!(rows[0].contains("\"decision\":\"approved\""), "{rows:?}");
    assert!(
        rows[0].contains("\"command\":\"git log --oneline -5\""),
        "{rows:?}"
    );
    assert!(rows[1].contains("\"rule_id\":\"-\""), "{rows:?}");
    assert!(rows[1].contains("\"decision\":\"escalated\""), "{rows:?}");
    assert!(rows[1].contains("\"reason\":\"not read-only"), "{rows:?}");
    assert!(
        rows.iter().all(|r| r.contains("\"cmd_sha256\":\"")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);

    // Under `approve = "none"` nothing is decided by a rule or pressed, and
    // the escalation is still a row.
    let (ldir, ledger) = ledger_file("off");
    let mut m = fenced(vec![bash_one_row()]);
    let mut s = session(&mut m, None);
    s.set_approval_ledger(Some(ledger.clone()));
    let mut opts = auto(30, None);
    opts.policy.approve = Approve::None;
    s.supervise(&opts).expect("supervise");
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("\"decision\":\"escalated\""), "{rows:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let _ = std::fs::remove_dir_all(&ldir);
}

/// `run_hosted` ends (`EXIT stopped`) on its stop flag, clearing the badge it raised; the stop
/// is set once the loop watches past the point ([`host_stops`]). A stop in each wait:
/// [`a_stop_in_any_wait_ends_the_hosted_run_before_another_begins`].
#[test]
fn run_hosted_runs_until_it_is_stopped_and_clears_its_badge() {
    let mut m = Mock::new(vec![question_screen()]);
    let (ldir, ledger) = ledger_file("hosted");
    // A question escalated, so a badge is raised and cleared (full power would answer it).
    let limited = SuperviseOpts::hosted_with(&SupervisorConfig {
        answer_questions: false,
        ..SupervisorConfig::default()
    });
    let (r, lines) = host_stops(&mut m, Some("@s-1"), ledger, &limited, false);
    assert_eq!(r, Ok(()));
    assert!(
        lines
            .first()
            .is_some_and(|l| l.starts_with("EVENT question ")),
        "{lines:?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("EXIT stopped"),
        "{lines:?}"
    );
    assert_eq!(
        count(&m, "@s-1 meta set attention owner=supervisor"),
        1,
        "{:?}",
        m.requests
    );
    assert_eq!(
        count(&m, "@s-1 meta unset attention owner=supervisor"),
        1,
        "{:?}",
        m.requests
    );
    assert_eq!(m.attention, None);
    let _ = std::fs::remove_dir_all(&ldir);
}

/// A stop that lands while the loop decides the box it read writes nothing (`Session::call`
/// refuses every input) and ends the run as a stop. Negative control: with no stop the box
/// is pressed through the same wrapper.
#[test]
fn a_stop_between_the_look_and_the_press_writes_nothing() {
    struct StopOnRead<'a> {
        inner: &'a mut Mock,
        stop: Arc<AtomicBool>,
        arm: bool,
        read: bool,
    }
    impl Ctl for StopOnRead<'_> {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let r = self.inner.call(args);
            // The stop lands while the loop decides the box it read: its first `status` after
            // a read.
            self.read |= args.contains(&"text");
            if self.arm && self.read && args.contains(&"status") {
                self.stop.store(true, Ordering::SeqCst);
            }
            r
        }
    }
    let hosted = SuperviseOpts::hosted();
    for arm in [true, false] {
        let mut m = fenced(vec![bash_one_row(), bash_one_row(), idle_screen()]);
        m.vanish_after = Some(0);
        let stop = Arc::new(AtomicBool::new(false));
        let mut ctl = StopOnRead {
            inner: &mut m,
            stop: Arc::clone(&stop),
            arm,
            read: false,
        };
        let mut out: Vec<u8> = Vec::new();
        let r = session(&mut ctl, Some("@s-1".to_string())).run_hosted(
            &hosted,
            Arc::clone(&stop),
            &mut out,
        );
        let text = String::from_utf8_lossy(&out).to_string();
        let inputs: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| {
                let verb = r.split_whitespace().find(|w| !w.starts_with('@'));
                matches!(verb, Some("key" | "send" | "turn" | "paste" | "feed-bin"))
            })
            .collect();
        if arm {
            assert_eq!(r, Ok(()), "a stop, not a failure: {text}");
            assert!(inputs.is_empty(), "written after the stop: {inputs:?}");
            assert!(!text.contains("APPROVED"), "{text}");
        } else {
            assert!(text.starts_with("APPROVED"), "{text}");
            assert!(!inputs.is_empty(), "{:?}", m.requests);
        }
    }
}

/// A build's output on a screen without Claude Code's composer frame: a program still
/// writing, which the turn's wait reads as busy until the output holds still.
fn writing_screen() -> Vec<String> {
    rows(&[
        "$ targo --unverified build -p aterm-agent",
        "   Compiling aterm-core v0.92.0",
        "   Compiling aterm-agent v0.92.0",
    ])
}

/// A stop in any wait ends the hosted run before another wait begins: set as each wait of a
/// path goes out ([`stop_in_every_wait`]), no wait or input follows and the run is `EXIT
/// stopped`. Each path names the waits it exists for, so a script that stops reaching them
/// fails.
#[test]
fn a_stop_in_any_wait_ends_the_hosted_run_before_another_begins() {
    let hosted = SuperviseOpts::hosted();
    let named = |armed: &[String], wait: &str| {
        assert!(
            armed.iter().any(|w| w.starts_with(wait)),
            "the stop was never set in `{wait}`: {armed:#?}"
        );
    };

    let armed = stop_in_every_wait("question", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(vec![question_screen()]);
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await seq ");

    let armed = stop_in_every_wait("busy", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(vec![busy_screen()]);
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await gone esc.to.interrupt ");

    let armed = stop_in_every_wait("writing", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(vec![writing_screen()]);
        m.idle_never = true;
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await idle 2000 timeout 3000");
    named(&armed, "@s-1 await seq ");

    let armed = stop_in_every_wait("approved", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(vec![bash_one_row(), bash_one_row(), idle_screen()]);
        m.vanish_after = Some(0);
        m
    });
    named(&armed, "@s-1 await idle 500 ");
}

/// The survey's `0` under a hold: the `0` refused `ERR halted` parks the
/// loop like the approval press, and the survey is tried again after.
#[test]
fn a_halted_survey_dismissal_parks_and_tries_again() {
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        idle_screen(),
    ]);
    m.key_replies
        .push_back(err("halted reason=owner origin=local"));
    m.hold = "1";
    m.hold_lifts_after = Some(1);
    m.vanish_after = Some(0);
    let opts = surveys(auto(30, None), true);
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(
        m.presses(),
        [
            "key if=^●.How.is.Claude.doing 0",
            "key if=^●.How.is.Claude.doing 0"
        ],
        "{:?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.starts_with("DISMISSED survey")),
        "{lines:?}"
    );
}

/// A person's first key inside the ~0.44 s the `0` spends pending abandons the card and
/// leaves the `0` at the head of their text (`0hello`): said `ABANDONED`, never `DISMISSED`,
/// the text left alone. Control: text typed after the card took the `0` is a dismissal.
#[test]
fn a_survey_that_left_under_typed_text_is_not_said_dismissed() {
    use aterm_phase::prompt::fixtures::{SURVEY_RACE_0HELLO_MEASURED, screen};
    // Claude Code draws the caret and its text apart by a no-break space.
    let run = |text: &str| {
        let mut after = screen(SURVEY_RACE_0HELLO_MEASURED);
        let caret = after
            .iter()
            .position(|r| r == "❯\u{a0}0hello")
            .expect("the race's composer");
        after[caret] = format!("❯\u{a0}{text}");
        let mut before = after.clone();
        before[caret] = "❯".to_string();
        let mut m = Mock::new(vec![with_survey(before), after]);
        // The cursor behind the text, as the program parks it after typing.
        m.screen_cols.insert(1, text.chars().count() + 2);
        m.cursor_on_caret = true;
        m.vanish_after = Some(0);
        let (dir, notes) = notes_file("survey-race");
        let (jdir, journal) = journal_file("survey-race");
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..surveys(auto(30, Some(notes.clone())), true)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        (lines, records, read_notes(&dir, &notes))
    };
    let (lines, records, noted) = run("0hello");
    assert!(
        !lines.iter().any(|l| l.starts_with("DISMISSED")),
        "{lines:?}"
    );
    assert!(
        !records.iter().any(|r| r.kind == "dismissed"),
        "{records:#?}"
    );
    assert!(
        records.iter().any(|r| r.line.starts_with("ABANDONED seq=")),
        "{records:#?}"
    );
    assert_eq!(noted.len(), 1, "{noted:?}");
    // Control.
    let (lines, records, noted) = run("hello");
    assert!(
        lines.iter().any(|l| l.starts_with("DISMISSED survey seq=")),
        "{lines:?}"
    );
    assert!(
        !records.iter().any(|r| r.line.starts_with("ABANDONED")),
        "{records:#?}"
    );
    assert_eq!(noted.len(), 1, "{noted:?}");
}

/// The survey's `0` carries the judged read's person count (`if-human=<n>`); a person keying
/// in the gap makes the server skip it, which spends no try: the next look's `0` carries the
/// new count and goes.
#[test]
fn a_person_who_keys_before_the_survey_press_skips_it_and_costs_no_try() {
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        idle_screen(),
    ]);
    m.human_seq = Some(7);
    m.person_at_key = 1;
    m.vanish_after = Some(0);
    let (jdir, journal) = journal_file("survey-person-fence");
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..surveys(auto(30, None), true)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(
        m.presses(),
        [
            "key if-human=7 if=^●.How.is.Claude.doing 0",
            "key if-human=8 if=^●.How.is.Claude.doing 0"
        ],
        "{:?}",
        m.requests
    );
    assert_eq!(m.person_skipped.len(), 1, "{:?}", m.requests);
    assert!(
        lines.iter().any(|l| l.starts_with("DISMISSED survey seq=")),
        "{lines:?}"
    );
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    assert!(
        !records.iter().any(|r| r.line.contains("did not take")),
        "a person's skip is no failed try: {records:#?}"
    );
}

/// The measured 2.1.280 folder-trust dialog (`policy/fixtures/cap-trust.txt`, folder
/// [`TRUST_FOLDER`]), the focus on `No, exit` or, `focused_yes`, on `Yes, I trust this folder`.
fn trust_dialog(focused_yes: bool) -> Vec<String> {
    include_str!("policy/fixtures/cap-trust.txt")
        .lines()
        .map(|r| match (r, focused_yes) {
            (" ❯ No, exit", true) => "   No, exit".to_string(),
            ("   Yes, I trust this folder", true) => " ❯ Yes, I trust this folder".to_string(),
            _ => r.to_string(),
        })
        .collect()
}

const TRUST_FOLDER: &str = "/private/tmp/claude-502/scratch/work1";

/// The trust dialog for the session's own folder under a trust root: a fenced, guarded
/// `down` onto `Yes, I trust this folder`, confirmed by a fresh read, then a fenced, guarded
/// Enter; never Esc, which exits Claude Code.
#[test]
fn the_trust_dialog_is_answered_by_a_confirmed_focus_move_then_enter() {
    let mut m = fenced(vec![trust_dialog(false), trust_dialog(true), idle_screen()]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
    });
    assert_eq!(
        lines[0],
        format!("APPROVED seq=102 {TRUST_FOLDER}"),
        "{lines:?}"
    );
    let folder = crate::supervise::policy::row_guard(&format!(" {TRUST_FOLDER}"));
    let yes = crate::supervise::policy::row_guard(" ❯ Yes, I trust this folder");
    assert_eq!(
        m.presses(),
        [
            format!("key if-human=7 if-gen=1.101 if={folder} down").as_str(),
            format!("key if-human=7 if-gen=1.102 if={yes} enter").as_str(),
        ],
        "{:?}",
        m.requests
    );
    assert!(
        !m.presses()
            .iter()
            .any(|p| p.ends_with(" escape") || p.ends_with(" esc")),
        "never Esc: {:?}",
        m.requests
    );
}

/// A recently published trust verdict. Its age dates publication, not the
/// first drawing of this dialog.
const TRUST_PUBLISHED_1S: &str = "prompt agent_detail=trust agent_rev=1 agent_since_ms=1000";

/// A watch at full power with the default grace, ending when the scripted
/// session does. Its minute max is a hang detector.
fn full_power_watch(m: &mut Mock, tag: &str) -> (Vec<String>, Vec<String>) {
    full_power_watch_with(m, tag, |_| {})
}

/// [`full_power_watch`] with the session set up first.
fn full_power_watch_with(
    m: &mut Mock,
    tag: &str,
    setup: impl FnOnce(&mut Watched<'_, '_>),
) -> (Vec<String>, Vec<String>) {
    let (jdir, journal) = journal_file(tag);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        journal: Some(journal.clone()),
        ..auto(60, None)
    };
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(m, &opts, setup);
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    (lines, records.into_iter().map(|r| r.line).collect())
}

/// A manager's one look (`supervise`: attended, `review.unattended()`
/// false) at full power with the default grace: the journal's lines.
fn full_power_look(m: &mut Mock, tag: &str) -> Vec<String> {
    let (jdir, journal) = journal_file(tag);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        journal: Some(journal.clone()),
        ..auto(60, None)
    };
    m.vanish_after = Some(0);
    let _ = session(m, None).supervise(&opts);
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    records.into_iter().map(|r| r.line).collect()
}

/// Held for the person: nothing pressed, `HELD` journaled, no `LAUNCH`.
fn assert_held(what: &str, m: &Mock, noted: &[String]) {
    assert!(
        m.presses().is_empty(),
        "{what}: {noted:#?}\n{:#?}",
        m.requests
    );
    assert!(
        noted.iter().any(|l| l.starts_with("HELD ")),
        "{what}: {noted:#?}"
    );
    assert!(
        !noted.iter().any(|l| l.starts_with("LAUNCH ")),
        "{what}: {noted:#?}"
    );
}

fn held_for_the_person(what: &str, m: &mut Mock) {
    let (_, noted) = full_power_watch(m, "trust-held");
    assert_held(what, m, &noted);
    assert_eq!(
        m.attention, None,
        "{what}: a held box is nobody's to be told of"
    );
}

/// A recent gesture and an observed agent verdict, without a causal launch
/// witness. Neither observation states when this dialog was first drawn.
fn recent_person(screens: Vec<Vec<String>>, agent: &'static str) -> Mock {
    let mut m = fenced(screens);
    m.agent = agent;
    m.human_ms = Some(60_000);
    m.human_seq = Some(4);
    m
}

#[test]
fn a_recent_persons_trust_dialog_keeps_their_grace() {
    for agent in [
        TRUST_PUBLISHED_1S,
        "prompt agent_detail=trust agent_rev=1 agent_since_ms=120000",
        "prompt agent_detail=trust agent_rev=1",
        "unknown agent_detail=- agent_rev=0 agent_since_ms=1500",
    ] {
        let mut m = recent_person(vec![trust_dialog(false)], agent);
        held_for_the_person(agent, &mut m);
    }
    let mut m = recent_person(vec![trust_dialog(false)], TRUST_PUBLISHED_1S);
    m.hand = "lease:mgr";
    held_for_the_person("another driver's hand", &mut m);
    let mut m = recent_person(vec![trust_dialog(false)], TRUST_PUBLISHED_1S);
    m.human_seq = None;
    held_for_the_person("a host with no person-count fence", &mut m);
    let mut m = recent_person(
        vec![bash_box(&["⏺ Cleaning."], "rm -rf build")],
        "prompt agent_detail=bash:not-read-only agent_rev=1 agent_since_ms=1000",
    );
    held_for_the_person("the ordinary Bash hold", &mut m);
}

/// Unknown and unread input cannot release a person's trust dialog early.
#[test]
fn unknown_input_keeps_the_persons_trust_dialog_held() {
    for input in [
        "",
        "input=- input_bytes=-",
        "input=pending input_bytes=1 input_wait_ms=40 fg_rss_mb=-",
        "input=typeahead input_bytes=9",
    ] {
        let mut m = recent_person(vec![trust_dialog(false)], TRUST_PUBLISHED_1S);
        m.input = input;
        held_for_the_person(input, &mut m);
    }
}

/// The first observed trust dialog, a clear input queue and a server
/// publication younger than the person's key do not say when the box was
/// drawn. The same observations can follow a key on an already drawn box.
#[test]
fn publication_after_a_key_does_not_prove_a_launch() {
    let mut m = fenced(vec![trust_dialog(true)]);
    m.agent = TRUST_PUBLISHED_1S;
    m.human_ms = Some(60_000);
    m.human_seq = Some(4);
    m.input = INPUT_CLEAR;
    held_for_the_person("publication after a key proves no launch", &mut m);
}

/// After the earlier gesture's grace, every trust answer still fences its
/// keys on that gesture count, including the backstop.
#[test]
fn a_trust_answer_after_the_persons_grace_fences_every_key() {
    use aterm_phase::prompt::fixtures as f;
    let backstop = f::screen(f::TRUST_BACKSTOP);
    let on_yes: Vec<String> = backstop
        .iter()
        .map(|r| match r.as_str() {
            " ❯ No, continue without these permissions" => {
                "   No, continue without these permissions".to_string()
            }
            "   Yes, I trust this folder" => " ❯ Yes, I trust this folder".to_string(),
            _ => r.clone(),
        })
        .collect();
    for (program, screens, expected_keys) in [
        (
            "claude",
            vec![trust_dialog(false), trust_dialog(true), idle_screen()],
            2,
        ),
        ("claude", vec![backstop, on_yes, idle_screen()], 2),
    ] {
        let mut m = recent_person(screens, TRUST_PUBLISHED_1S);
        m.program = program;
        m.human_ms = Some(180_000);
        let (lines, noted) = full_power_watch(&mut m, "trust-post-grace");
        assert_eq!(
            m.presses().len(),
            expected_keys,
            "{lines:#?}\n{noted:#?}\n{:#?}",
            m.requests
        );
        assert!(
            m.presses()
                .iter()
                .all(|p| p.starts_with("key if-human=4 if-gen=")),
            "{:#?}",
            m.requests
        );
        assert!(
            lines.iter().any(|l| l.starts_with("APPROVED ")),
            "{lines:#?}"
        );
        assert!(!noted.iter().any(|l| l.starts_with("HELD ")), "{noted:#?}");
    }
}

/// The publication wait has been removed. At the reachable decision's
/// metadata read, a newly selected Off still prevents every trust key;
/// unchanged Cruise permits the ordinary answer after the person's grace.
#[test]
fn a_trust_answer_obeys_the_current_autopilot_word() {
    for word in ["off", "cruise"] {
        let mut m = recent_person(
            vec![trust_dialog(false), trust_dialog(true), idle_screen()],
            "unknown agent_detail=- agent_rev=0 agent_since_ms=1500",
        );
        m.human_ms = Some(180_000);
        m.autopilot = Some("cruise");
        m.autopilot_at = vec![(2, Some(word))];
        let (lines, noted) = full_power_watch(&mut m, "trust-owner-word");
        assert_eq!(m.autopilot, Some(word), "the owner controls the word");
        if word == "off" {
            assert!(m.presses().is_empty(), "{:#?}", m.requests);
            assert!(!lines.iter().any(|l| l.starts_with("APPROVED ")));
            assert!(noted.iter().any(|l| l.starts_with("PARKED ")));
            assert_eq!(m.attention, None);
        } else {
            assert_eq!(m.presses().len(), 2, "{:#?}", m.requests);
            assert!(lines.iter().any(|l| l.starts_with("APPROVED ")));
            assert!(!noted.iter().any(|l| l.starts_with("PARKED ")));
        }
    }
}

/// A person keying after the read gets the trust dialog, even when the
/// earlier gesture's grace had expired: both arrow and Enter yield without
/// a failed-press escalation.
#[test]
fn a_person_who_keys_while_a_trust_answer_is_pressed_gets_the_dialog() {
    for (focused_yes, key) in [(false, " down"), (true, " enter")] {
        let mut m = recent_person(vec![trust_dialog(focused_yes)], TRUST_PUBLISHED_1S);
        m.human_ms = Some(180_000);
        m.status_human_from = BTreeMap::from([(2, 0)]);
        m.human_seq = Some(0);
        m.person_at_key = 1;
        let (lines, noted) = full_power_watch(&mut m, "trust-yield");
        assert_eq!(m.person_skipped.len(), 1, "{key}: {:#?}", m.requests);
        let presses = m.presses();
        assert!(
            presses.len() == 1 && presses[0].ends_with(key),
            "{key}: one key tried: {:#?}",
            m.requests
        );
        assert!(
            noted.iter().any(|l| l.starts_with("HELD ")),
            "{key}: {noted:#?}\n{:#?}",
            m.requests
        );
        assert!(
            !noted
                .iter()
                .any(|l| l.contains("did not take") || l.contains("did not land")),
            "{key}: a person's skip is no failed try: {noted:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("APPROVED ")),
            "{key}: {lines:#?}"
        );
        assert_eq!(m.attention, None, "{key}: {lines:#?}");
    }
}

/// THE LAUNCH (`Session::launch_call`): `program` typed at an integrated
/// prompt by the recent person (count 4), and the server's launch witness for
/// it as aterm-gui's `launch_witness` publishes it — the count its signed
/// command start recorded, `person`, judged at each read against the
/// server's count.
fn launched(screens: Vec<Vec<String>>, program: &'static str) -> Mock {
    let mut m = recent_person(screens, TRUST_PUBLISHED_1S);
    m.program = program;
    m.launch = Some(ServerLaunch {
        id: 1,
        seq: 4,
        word: program,
        why: "person",
    });
    m
}

/// Claude Code's folder-trust dialog drawn with numbered options: the trust
/// rule presses a digit, which carries no person fence.
fn numbered_trust_dialog() -> Vec<String> {
    trust_dialog(false)
        .into_iter()
        .map(|r| match r.as_str() {
            " ❯ No, exit" => " ❯ 1. No, exit".to_string(),
            "   Yes, I trust this folder" => "   2. Yes, I trust this folder".to_string(),
            _ => r,
        })
        .collect()
}

/// THE LAUNCH, answered: the folder-trust dialog drawn by the `claude` a
/// person's Enter started, which the server's
/// witness shows they have not keyed since, is answered under their grace,
/// at once — every key fenced on the witnessed count, `LAUNCH` journaled,
/// nothing held or escalated — with no wait between the Autopilot read and
/// the first key. A witness whose program has not yet read its input
/// (`input=pending`) or holds lines it has not asked for (`typeahead`: the
/// `claude⏎⏎` residual) is looked at again, not held for the grace, and no
/// key is written before a `status` says `input=clear`.
#[test]
fn a_launch_the_server_witnesses_is_answered_within_the_grace() {
    let pending = "input=pending input_bytes=1 input_wait_ms=40 fg_rss_mb=-";
    let typeahead = "input=typeahead input_bytes=9 input_wait_ms=40 fg_rss_mb=-";
    let looked_again = || {
        vec![
            trust_dialog(false),
            trust_dialog(false),
            trust_dialog(true),
            idle_screen(),
        ]
    };
    for (what, program, screens, input, keys) in [
        (
            "claude",
            "claude",
            vec![trust_dialog(false), trust_dialog(true), idle_screen()],
            INPUT_CLEAR,
            2,
        ),
        ("input not yet read", "claude", looked_again(), pending, 2),
        ("typeahead", "claude", looked_again(), typeahead, 2),
    ] {
        let mut m = launched(screens, program);
        m.input = input;
        m.input_after = vec![(1, INPUT_CLEAR)];
        let (lines, noted) = full_power_watch(&mut m, "launch");
        // REQUEST ORDER: the first key follows the `status` that said
        // `input=clear` (the first, or after `input_after`'s one read the
        // second), and no wait stands between the last Autopilot read (a
        // bare `meta`) and that key.
        let statuses: Vec<usize> = (0..m.requests.len())
            .filter(|&i| verb_of(&m.requests[i]) == "status")
            .collect();
        let clear = statuses[usize::from(input != INPUT_CLEAR)];
        let first_key = m
            .requests
            .iter()
            .position(|r| verb_of(r) == "key")
            .unwrap_or_else(|| panic!("{what}: a key: {:#?}", m.requests));
        assert!(
            first_key > clear,
            "{what}: a key before input=clear: {:#?}",
            m.requests
        );
        let autopilot_read = m.requests[..first_key]
            .iter()
            .rposition(|r| {
                r.split_whitespace()
                    .filter(|w| !w.starts_with('@'))
                    .eq(["meta"])
            })
            .unwrap_or_else(|| panic!("{what}: an Autopilot read: {:#?}", m.requests));
        assert!(
            !m.requests[autopilot_read..first_key]
                .iter()
                .any(|r| verb_of(r) == "await"),
            "{what}: a wait between the Autopilot read and the first key: {:#?}",
            m.requests
        );
        assert_eq!(
            m.presses().len(),
            keys,
            "{what}: {lines:#?}\n{noted:#?}\n{:#?}",
            m.requests
        );
        assert!(
            m.presses()
                .iter()
                .all(|p| p.starts_with("key if-human=4 if-gen=")),
            "{what}: {:#?}",
            m.requests
        );
        assert!(
            lines.iter().any(|l| l.starts_with("APPROVED ")),
            "{what}: {lines:#?}"
        );
        assert_eq!(
            noted
                .iter()
                .filter(
                    |l| l.starts_with("LAUNCH ") && l.contains(&format!(" id=1 word={program} "))
                )
                .count(),
            1,
            "{what}: {noted:#?}"
        );
        assert!(
            !noted.iter().any(|l| l.starts_with("HELD ")),
            "{what}: {noted:#?}"
        );
        assert_eq!(m.attention, None, "{what}");
    }
}

/// Each premise of THE LAUNCH has its negative control, held for the grace
/// with nothing pressed or escalated: the count moved since the mark (the
/// server says `since`) before the read or between the read and the
/// decision's `status`, no witness (an older host, or `null`), each word
/// but `person`, another driver's hand, a read with no person count, a
/// `program=` that is no agent or another than the witness's, a Bash box
/// under the witness, a numbered trust dialog (a digit carries no person
/// fence), and input the program may not have read (missing, `-`). A
/// stalled or stopped worker gets nothing either (`stall.rs` holds it).
#[test]
fn a_launch_missing_any_premise_keeps_the_persons_grace() {
    let with = |change: &dyn Fn(&mut Mock)| {
        let mut m = launched(vec![trust_dialog(false)], "claude");
        change(&mut m);
        m
    };
    let word = |why: &'static str| {
        move |m: &mut Mock| {
            m.launch = m.launch.map(|l| ServerLaunch { why, ..l });
        }
    };
    let held: Vec<(&str, Mock)> = vec![
        ("the count moved", with(&|m| m.human_seq = Some(5))),
        ("no witness", with(&|m| m.launch = None)),
        ("no mark", with(&word("none"))),
        ("typed", with(&word("typed"))),
        ("boxed", with(&word("boxed"))),
        ("other", with(&word("other"))),
        ("pending", with(&word("pending"))),
        (
            "a gesture before the decision's status",
            with(&|m| m.person_at_status = Some(1)),
        ),
        ("a driver's hand", with(&|m| m.hand = "lease:mgr")),
        (
            "no person count on the read",
            with(&|m| m.human_seq_on = BTreeMap::from([(0, None)])),
        ),
        ("program=ssh", with(&|m| m.program = "ssh")),
        (
            "the witness names another agent",
            with(&|m| m.launch = m.launch.map(|l| ServerLaunch { word: "codex", ..l })),
        ),
        (
            "a Bash box",
            with(&|m| m.screens = vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]),
        ),
        (
            "a numbered trust dialog",
            with(&|m| m.screens = vec![numbered_trust_dialog()]),
        ),
        ("no input field", with(&|m| m.input = "")),
        ("input=-", with(&|m| m.input = "input=- input_bytes=-")),
    ];
    for (what, mut m) in held {
        held_for_the_person(what, &mut m);
    }
    for input in [
        INPUT_STALLED,
        "input=stopped input_bytes=1 input_wait_ms=900 fg_rss_mb=-",
    ] {
        let mut m = with(&|m| m.input = input);
        let (lines, noted) = full_power_watch(&mut m, "launch-stall");
        assert!(m.presses().is_empty(), "{input}: {:#?}", m.requests);
        assert!(
            !noted.iter().any(|l| l.starts_with("LAUNCH ")),
            "{input}: {noted:#?}"
        );
        assert!(!lines.iter().any(|l| l.starts_with("APPROVED ")), "{input}");
    }
}

/// THE LAUNCH answers a TRUST box alone: the boxes the policy also approves
/// by a focus move — Claude Code's usage-limit options dialog
/// (`limit-wait@v1`) and its autocompact offer — keep the person's grace
/// under the same whole witness and `input=clear` that answers the trust
/// dialog.
#[test]
fn a_launch_answers_no_box_but_its_trust_dialog() {
    use aterm_phase::prompt::fixtures as f;
    for (what, agent, screens) in [
        (
            "usage-limit",
            "prompt agent_detail=usage-limit agent_rev=1 agent_since_ms=1000",
            vec![
                f::screen(f::LIMIT_OPTIONS_DIALOG),
                limit_dialog(&[LIMIT_STOP, LIMIT_WAIT, LIMIT_UPGRADE], 1),
                idle_screen(),
            ],
        ),
        (
            "autocompact-offer",
            "prompt agent_detail=autocompact-offer agent_rev=1 agent_since_ms=1000",
            vec![
                autocompact_offer(false, "400k"),
                autocompact_offer(true, "400k"),
                idle_screen(),
            ],
        ),
    ] {
        let mut m = launched(screens, "claude");
        m.agent = agent;
        m.input = INPUT_CLEAR;
        let (_, noted) = full_power_watch_with(&mut m, "launch-not-trust", |s| {
            s.set_approval_env(owner_env());
            s.set_autocompact_settle(Duration::ZERO);
        });
        assert_held(what, &m, &noted);
    }
}

/// THE LAUNCH in a manager's one look (`supervise`, attended): no Autopilot
/// park and no stall hold stand before it there, so its own reads decide. A
/// whole witness with `input=clear` is answered, every key fenced; the
/// owner's Autopilot `off`, and a worker not reading its input (`stalled`,
/// `stopped`), keep the person's grace.
#[test]
fn an_attended_look_answers_a_launch_on_its_own_reads() {
    let mut m = launched(
        vec![trust_dialog(false), trust_dialog(true), idle_screen()],
        "claude",
    );
    m.input = INPUT_CLEAR;
    let noted = full_power_look(&mut m, "launch-look");
    assert_eq!(m.presses().len(), 2, "{noted:#?}\n{:#?}", m.requests);
    assert!(
        m.presses()
            .iter()
            .all(|p| p.starts_with("key if-human=4 if-gen=")),
        "{:#?}",
        m.requests
    );
    assert_eq!(
        noted.iter().filter(|l| l.starts_with("LAUNCH ")).count(),
        1,
        "{noted:#?}"
    );
    assert!(!noted.iter().any(|l| l.starts_with("HELD ")), "{noted:#?}");
    let stopped = "input=stopped input_bytes=1 input_wait_ms=900 fg_rss_mb=-";
    for (what, autopilot, input) in [
        ("Autopilot off", Some("off"), INPUT_CLEAR),
        ("stalled", None, INPUT_STALLED),
        ("stopped", None, stopped),
    ] {
        let mut m = launched(vec![trust_dialog(false)], "claude");
        m.autopilot = autopilot;
        m.input = input;
        let noted = full_power_look(&mut m, "launch-look-held");
        assert_held(what, &m, &noted);
    }
}

/// THE LAUNCH's own controls: one witness answers one dialog (a second trust
/// box under the same launch is held for the grace); the owner's Autopilot
/// `off` parks the loop with nothing pressed; and a person who keys during
/// the press gets the dialog — the fenced key is skipped (`Yielded`, no
/// failed press), and the next read's witness says `since`, so it is held.
#[test]
fn a_launch_answers_one_dialog_under_autopilot_and_yields_to_the_person() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = launched(
        vec![
            trust_dialog(false),
            trust_dialog(true),
            f::screen(f::TRUST_BACKSTOP),
        ],
        "claude",
    );
    let (_, noted) = full_power_watch(&mut m, "launch-once");
    assert_eq!(m.presses().len(), 2, "{noted:#?}\n{:#?}", m.requests);
    assert_eq!(
        noted.iter().filter(|l| l.starts_with("LAUNCH ")).count(),
        1,
        "{noted:#?}"
    );
    assert!(noted.iter().any(|l| l.starts_with("HELD ")), "{noted:#?}");

    let mut m = launched(vec![trust_dialog(false)], "claude");
    m.autopilot = Some("off");
    let (lines, noted) = full_power_watch(&mut m, "launch-off");
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(noted.iter().any(|l| l.starts_with("PARKED ")), "{noted:#?}");
    assert!(!lines.iter().any(|l| l.starts_with("APPROVED ")));

    for (focused_yes, key) in [(false, " down"), (true, " enter")] {
        let mut m = launched(vec![trust_dialog(focused_yes)], "claude");
        m.person_at_key = 1;
        let (lines, noted) = full_power_watch(&mut m, "launch-yield");
        assert_eq!(m.person_skipped.len(), 1, "{key}: {:#?}", m.requests);
        let presses = m.presses();
        assert!(
            presses.len() == 1 && presses[0].ends_with(key),
            "{key}: {:#?}",
            m.requests
        );
        assert!(
            noted.iter().any(|l| l.starts_with("HELD ")),
            "{key}: {noted:#?}"
        );
        assert!(
            !noted
                .iter()
                .any(|l| l.contains("did not take") || l.contains("did not land")),
            "{key}: {noted:#?}"
        );
        assert!(!lines.iter().any(|l| l.starts_with("APPROVED ")), "{key}");
        assert_eq!(m.attention, None, "{key}");
    }
}

/// A launch in a new project folder with a `.mcp.json`: the folder-trust dialog, then Claude
/// Code's MCP server dialog (MEASURED on 2.1.301, drawn about 0.3 s after the trust answer in
/// the m8 run's `watch` stream). At full power the trust dialog is answered (`trust-any@v1`: a
/// fenced `down`, then Enter) and the MCP dialog is ESCALATED — the owner badged on it — and
/// never pressed: no key after the trust Enter, never Esc. Both for a person whose grace has run out (every trust
/// key fenced on their count) and for nobody at all. Before, full power pressed Enter on its
/// focused `Continue without using this MCP server` (`allow-once@v1`), which disabled the
/// server for good.
#[test]
fn a_launch_answers_the_trust_dialog_and_hands_the_mcp_dialog_to_a_person() {
    use aterm_phase::prompt::fixtures::{MCP_SERVER_ONE, screen};
    for person in [true, false] {
        let screens = vec![
            trust_dialog(false),
            trust_dialog(true),
            screen(MCP_SERVER_ONE),
        ];
        let mut m = if person {
            let mut m = recent_person(screens, TRUST_PUBLISHED_1S);
            m.human_ms = Some(180_000);
            m
        } else {
            fenced(screens)
        };
        m.cwd = Some(TRUST_FOLDER.to_string());
        let (lines, noted) = full_power_watch(&mut m, "trust-then-mcp");
        let presses = m.presses();
        assert_eq!(presses.len(), 2, "{person}: {:#?}", m.requests);
        assert!(
            presses[0].ends_with(" down") && presses[1].ends_with(" enter"),
            "{person}: the trust dialog's two keys alone: {presses:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("APPROVED seq=102 {TRUST_FOLDER}"))),
            "{person}: {lines:#?}"
        );
        assert!(
            noted.iter().any(|l| l.contains("rule=trust-any@v1")),
            "{person}: {noted:#?}"
        );
        if person {
            assert!(
                presses
                    .iter()
                    .all(|p| p.starts_with("key if-human=4 if-gen=")),
                "{presses:#?}"
            );
        }
        // The badge is the MCP dialog's.
        let attention = m.attention.clone().unwrap_or_default();
        assert!(
            attention.starts_with("claude mcp-server: lab-probe ("),
            "{person}: {attention:?}\n{noted:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("EVENT prompt ") && l.contains("kind=mcp-server")),
            "{person}: {lines:#?}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("lab-probe") && l.starts_with("APPROVED")),
            "{person}: {lines:#?}"
        );
    }
}

/// THE MCP SERVER DIALOGS' BADGE SAYS WHAT A PERSON MUST KNOW, at every `approve` level the
/// loop runs (`none`, safe, full power): every answer, Esc too, is saved for good in the
/// project's `.claude/settings.local.json`, and `claude mcp reset-project-choices` undoes it —
/// both still in the badge after it cuts a reason to its cells (`escalate::fit_reason`). No key
/// is pressed. Before, under `none` the loop badged `every box is the owner's` and never asked
/// the policy, so the person got no word of what their answer keeps.
#[test]
fn the_mcp_server_dialogs_badge_says_every_answer_is_kept_at_every_level() {
    use aterm_phase::prompt::fixtures::{MCP_SERVER_ONE, MCP_SERVER_TWO, screen};
    for fixture in [MCP_SERVER_ONE, MCP_SERVER_TWO] {
        for approve in [Approve::None, Approve::Safe, Approve::All] {
            let mut m = fenced(vec![screen(fixture)]);
            m.vanish_after = Some(0);
            let mut opts = auto(60, None);
            opts.policy.approve = approve;
            let (lines, _) = watch_lines(&mut m, &opts);
            assert!(m.presses().is_empty(), "{approve:?}: {:#?}", m.requests);
            let attention = m.attention.clone().unwrap_or_default();
            for fact in [
                ".claude/settings.local.json",
                "claude mcp reset-project-choices",
            ] {
                assert!(
                    attention.contains(fact),
                    "{approve:?} lacks {fact}: {attention:?}\n{lines:#?}"
                );
            }
        }
    }
}

/// The usage-limit dialog (aterm-phase's capture of Claude Code 2.1.295) with
/// its rows replaced by `labels`, the `❯` on option `focus`.
fn limit_dialog(labels: &[&str], focus: usize) -> Vec<String> {
    aterm_phase::prompt::fixtures::limit_options_dialog(labels, focus)
}

const LIMIT_STOP: &str = "Stop and wait for limit to reset";
const LIMIT_WAIT: &str = "Wait here, then continue automatically at 12:23pm";
const LIMIT_UPGRADE: &str = "Upgrade your plan";

/// The usage-limit dialog under the window's defaults: a fenced `down` onto `Wait here, then
/// continue automatically …`, confirmed by a fresh read, then an Enter fenced on it and
/// guarded on that row; never a digit or Esc. Ledgered `limit-wait@v1`, no badge.
#[test]
fn the_limit_options_dialog_waits_for_the_reset_by_itself() {
    let (dir, ledger) = ledger_file("limit-wait");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let first =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LIMIT_OPTIONS_DIALOG);
    let mut m = fenced(vec![
        first,
        limit_dialog(&[LIMIT_STOP, LIMIT_WAIT, LIMIT_UPGRADE], 1),
        idle_screen(),
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(
        lines[0],
        format!("APPROVED seq=102 {LIMIT_WAIT}"),
        "{lines:?}"
    );
    let title = crate::supervise::policy::row_guard("  What do you want to do?");
    let wait = crate::supervise::policy::row_guard(&format!("  ❯ 2. {LIMIT_WAIT}"));
    assert_eq!(
        m.presses(),
        [
            format!("key if-gen=1.101 if={title} down").as_str(),
            format!("key if-gen=1.102 if={wait} enter").as_str(),
        ],
        "{:?}",
        m.requests
    );
    assert_eq!(m.attention, None, "nothing asked of the owner");
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter()
            .any(|r| r.contains("limit-wait@v1") && r.contains("approved")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // The wait row first, the focus already on it: no move, Enter alone.
    let mut m = fenced(vec![
        limit_dialog(&[LIMIT_WAIT, LIMIT_STOP, LIMIT_UPGRADE], 0),
        idle_screen(),
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    assert_eq!(
        lines[0],
        format!("APPROVED seq=101 {LIMIT_WAIT}"),
        "{lines:?}"
    );
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{:?}", m.requests);
    assert!(presses[0].ends_with(" enter"), "{presses:?}");

    // The dialog stays after the Enter: at full power it is answered again, but only by the
    // wait row, each Enter fenced on a fresh read and guarded on that row.
    let mut m = fenced(vec![
        limit_dialog(&[LIMIT_STOP, LIMIT_WAIT, LIMIT_UPGRADE], 0),
        limit_dialog(&[LIMIT_STOP, LIMIT_WAIT, LIMIT_UPGRADE], 1),
    ]);
    m.vanish_after = Some(2);
    watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    let presses = m.presses();
    assert!(
        presses.iter().any(|p| p.ends_with(" enter")),
        "{:?}",
        m.requests
    );
    for p in &presses {
        assert!(
            p.ends_with(&format!("if={title} down")) || p.ends_with(&format!("if={wait} enter")),
            "a press that is not the wait row's: {p} ({:?})",
            m.requests
        );
    }
}

/// With no OSC 7 from the shell, the trust dialog is decided by the agent's own folder
/// (`meta agent_cwd=`), which comes before the shell's (the OSC 7 directory is where the last
/// prompt was, not where the agent was launched); the shell's alone still decides. The host's
/// `trust_roots` reach the decision through `SuperviseOpts::hosted_with`. Negative controls:
/// with neither folder, escalated as the cwd unknown; under a root that does not cover the
/// folder, escalated as under no trust root.
#[test]
fn a_trust_dialog_under_a_shell_with_no_osc7_is_decided_by_the_agents_own_folder() {
    const COVERS: &str = "/private/tmp/claude-502/scratch/work*";
    for (roots, cwd, agent_cwd, escalated) in [
        (COVERS, None, Some(TRUST_FOLDER), None),
        (COVERS, None, None, Some("cwd unknown")),
        (
            COVERS,
            Some("/private/tmp/claude-502/elsewhere"),
            Some(TRUST_FOLDER),
            None,
        ),
        (COVERS, Some(TRUST_FOLDER), None, None),
        (
            "/nowhere/at/all*",
            Some(TRUST_FOLDER),
            None,
            Some("under no trust root"),
        ),
    ] {
        let mut cfg = crate::supervise::SupervisorConfig::default();
        cfg.set("approve", "safe").expect("a level");
        cfg.set("trust_roots", roots).expect("a root list");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted_with(&cfg)
        };
        let mut m = fenced(vec![trust_dialog(false), trust_dialog(true), idle_screen()]);
        m.cwd = cwd.map(str::to_string);
        m.agent_cwd = agent_cwd.map(str::to_string);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
        });
        if let Some(why) = escalated {
            assert!(m.presses().is_empty(), "{why}: {:?}", m.requests);
            assert!(
                m.requests.iter().any(|r| r.contains(why)),
                "{why}: {:?}",
                m.requests
            );
        } else {
            assert_eq!(
                lines[0],
                format!("APPROVED seq=102 {TRUST_FOLDER}"),
                "{roots} {cwd:?} {agent_cwd:?}: {lines:?}"
            );
        }
    }
}

/// A file box the agent's own ask rule sends (bare `Write`/`Edit` asks in its
/// `CLAUDE_CONFIG_DIR` settings, Claude Code 2.1.295) is held by the host's default, naming
/// the rule. The box draws no reason row, so only the settings tell it from an unruled call's.
/// Negative control: with the rules gone it is pressed `1`.
#[test]
fn a_file_box_the_agents_own_ask_rule_sends_is_held_in_the_loop() {
    const WRITE: &str = include_str!("policy/fixtures/claude-2.1.295-box-write-ask-rule.txt");
    const EDIT: &str = include_str!("policy/fixtures/claude-2.1.295-box-edit-ask-rule.txt");
    let scratch = crate::supervise::ScratchDir::new("ask-rule", "loop");
    let claude = scratch.join("claude");
    std::fs::create_dir_all(&claude).expect("the Claude Code directory");
    let settings = claude.join("settings.json");
    let worker = WorkerEnv::hermetic(None).with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()));
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    for (name, text, rule) in [("write", WRITE, "Write"), ("edit", EDIT, "Edit")] {
        let rows = aterm_phase::prompt::fixtures::screen(text);
        for ruled in [true, false] {
            if ruled {
                std::fs::write(&settings, r#"{"permissions":{"ask":["Write","Edit"]}}"#)
                    .expect("the owner's settings");
            } else {
                std::fs::remove_file(&settings).expect("the rules gone");
            }
            let mut m = fenced(vec![rows.clone(), busy_screen()]);
            m.vanish_after = Some(0);
            let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
                s.set_approval_env(ApprovalEnv {
                    worker: WorkerSource::Fixed(Ok(worker.clone())),
                    ..owner_env()
                });
            });
            if ruled {
                assert!(m.presses().is_empty(), "{name}: {:?}", m.requests);
                assert!(lines[0].starts_with("EVENT prompt "), "{name}: {lines:?}");
                let held = m
                    .requests
                    .iter()
                    .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
                    .unwrap_or_else(|| panic!("{name}: no badge: {:?}", m.requests));
                assert!(
                    held.contains(&format!("(an ask rule): `{rule}` in ")),
                    "{held}"
                );
            } else {
                assert_eq!(m.presses().len(), 1, "{name}: {:?}", m.requests);
                assert!(m.presses()[0].ends_with(" 1"), "{name}: {:?}", m.requests);
                assert!(lines[0].starts_with("APPROVED seq="), "{name}: {lines:?}");
            }
        }
    }
}

// The former transcript-candidate authority/mount-spending fixtures are
// retired with that authority: the on-screen box has no call identity.
// policy::approval::tests keeps the ordinary admission and hold regression.

/// The host's default presses a write (`touch x`) `1`, ledgered `allow-once@v1` with
/// `unproven: not read-only …` (no rule proved it), `UNPROVEN` journaled.
#[test]
fn the_hosts_default_presses_a_write_and_ledgers_it_unproven() {
    let touch = {
        let mut r = bash_one_row();
        r[4] = "   touch x".to_string();
        r
    };
    let (ldir, ledger) = ledger_file("full-power");
    let (jdir, journal) = journal_file("full-power");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        journal: Some(journal.clone()),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let mut m = fenced(vec![touch, busy_screen()]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert!(
        lines[0].starts_with("APPROVED seq=") && lines[0].ends_with(" touch x"),
        "{lines:?}"
    );
    let guard = crate::supervise::policy::row_guard("   touch x");
    assert_eq!(
        m.presses(),
        [format!("key if-human=7 if-gen=1.101 if={guard} 1").as_str()],
        "{:?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("\"rule_id\":\"allow-once@v1\"")
            && rows[0].contains("\"decision\":\"approved\"")
            && rows[0].contains("\"reason\":\"unproven: not read-only"),
        "{rows:?}"
    );
    let (records, _) = journal_records(&journal);
    let kinds: Vec<&str> = records.iter().map(|r| r.kind.as_str()).collect();
    let at = kinds
        .iter()
        .position(|k| *k == "approved")
        .expect("APPROVED");
    let unproven = &records[at + 1];
    assert_eq!(
        (unproven.kind.as_str(), unproven.phase.as_str()),
        ("unproven", "allow-once@v1"),
        "{records:?}"
    );
    assert!(
        unproven.summary.starts_with("unproven: not read-only"),
        "{unproven:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("UNPROVEN")),
        "journal-only: {lines:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
    let _ = std::fs::remove_dir_all(&jdir);
}

/// What a `tail=40` read cut and no read shows whole is the person's (a box the whole read
/// shows is answered: `every_cut_of_the_tail_across_a_fresh_panes_box_is_answered`): the
/// column-1 Yes/No at the top of a 49-row grid is escalated as unread, never as a box cut
/// short; a box taller than the grid is pressed by no level, the missing title said.
#[test]
fn what_the_tail_cut_and_no_read_shows_whole_is_escalated() {
    use aterm_phase::prompt::fixtures as f;
    let hosted = || SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let mut question = f::screen(f::QUESTION_YES_NO);
    question.resize(49, String::new());
    let mut m = fenced(vec![question]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    // Read again whole, it is the question it is — escalated as one not
    // read whole, never as a box cut short.
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let set = m
        .requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("escalated");
    assert!(
        set.starts_with("claude question:") && set.contains("could not read whole"),
        "{set}"
    );

    // The control: taller than the grid, the head off every read.
    let heredoc = f::tall_bash_box(62);
    let cut = heredoc[heredoc.len() - 49..].to_vec();
    let mut m = fenced(vec![cut]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let set = m
        .requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("escalated");
    assert!(set.contains("title row is not on the screen"), "{set}");
}

/// The host's default options, for the tests of a fresh 149x62 pane running Claude Code.
fn fresh_pane_opts() -> SuperviseOpts {
    SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&SupervisorConfig::default())
    }
}
const FRESH_PANE_ROWS: usize = 62;
/// The session folder of the live fresh pane, redacted as its fixtures are.
const FRESH_PANE_FOLDER: &str =
    "/private/var/folders/xx/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx/T/tmp.XXXXXXXXXX/proj";

/// The first `n` `text` requests the loop made.
fn text_reads(m: &Mock, n: usize) -> Vec<&str> {
    m.requests
        .iter()
        .map(String::as_str)
        .filter(|r| r.starts_with("text"))
        .take(n)
        .collect()
}

/// At full power, unattended, a press that did not land is pressed again on a back-off
/// doubling from 250 ms and badged once it has missed a while. Negative control: the safe
/// rules hand it over after one press.
#[test]
fn a_press_that_did_not_land_is_tried_again_at_full_power() {
    let (jdir, journal) = journal_file("press-missed");
    let mut m = fenced(vec![bash_one_row()]);
    m.stall_sleep = Some(Duration::from_millis(50));
    // The session ends once the box was pressed three times and badged, so the run is
    // decided by what happened, not by how many back-off turns fit under load.
    m.exit_when = Some(|m| {
        m.presses().len() >= 3
            && m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention owner=supervisor"))
    });
    let base = auto(60, None);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        policy: crate::supervise::SupervisorConfig {
            approve: Approve::All,
            ..base.policy.clone()
        },
        ..base
    };
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_press_badge_after(Duration::from_millis(600));
    });
    let presses = m.presses().len();
    assert!(presses >= 3, "tried again: {presses} {:#?}", m.requests);
    let (records, _) = journal_records(&journal);
    assert!(
        records
            .iter()
            .any(|r| r.line.starts_with("WAITING ") && r.line.contains("the press did not land")),
        "{records:#?}"
    );
    let set: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention owner=supervisor"))
        .collect();
    assert_eq!(set.len(), 1, "badged once: {:#?}", m.requests);
    assert!(set[0].contains("still trying"), "{set:?}");
    let _ = std::fs::remove_dir_all(&jdir);

    // Negative control: the safe rules hand it over after one press.
    let mut m = fenced(vec![bash_one_row()]);
    m.stall_sleep = Some(Duration::from_millis(50));
    let _ = watch_lines_with(&mut m, &auto(1, None), |s| s.set_approval_env(owner_env()));
    assert_eq!(m.presses().len(), 1, "{:#?}", m.requests);
}

/// No repeat cap at full power (`approval_loop`'s module header): the same box that comes
/// back is pressed every time, past the safe rules' two. Control: the safe rules' cap
/// (`a_read_that_keeps_coming_back_is_approved_twice_then_handed_over`).
#[test]
fn full_power_presses_the_same_box_every_time_it_comes_back() {
    let script = |boxes: usize, shown: &dyn Fn() -> Vec<String>| {
        let mut v = Vec::new();
        for k in 0..boxes {
            if k > 0 {
                v.push(busy_screen());
            }
            v.push(shown());
        }
        v
    };
    // Three reads: past the safe rules' two.
    let mut m = fenced(script(3, &bash_one_row));
    let (_, code) = session(&mut m, None)
        .supervise(&full_power_opts(None))
        .expect("supervise");
    assert_eq!(m.presses().len(), 3, "{:?}", m.requests);
    assert_eq!(code, 0);
}

/// Every way the trust press stops short: a focus that never lands (moved again from each
/// fresh read five times, then handed over, never Enter:
/// `tier1_enter_goes_only_on_a_confirmed_focus`), one that landed elsewhere (no Enter),
/// another folder than the session's, and a host with no generation fence (no key).
#[test]
fn the_trust_dialog_is_never_entered_unconfirmed() {
    let mut m = fenced(vec![trust_dialog(false); 16]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &auto(30, None), |s| s.set_approval_env(owner_env()));
    assert_eq!(count(&m, "key "), 5, "{:?}", m.requests);
    assert!(m.attention.is_some(), "handed over: {:?}", m.requests);

    // The focus moved somewhere else than it was sent: the manager's at once.
    let mut elsewhere = trust_dialog(false);
    let yes = elsewhere
        .iter()
        .position(|r| r == "   Yes, I trust this folder")
        .expect("the trust option");
    let no = elsewhere
        .iter()
        .position(|r| r == " ❯ No, exit")
        .expect("the exit option");
    elsewhere[no] = "   No, exit".to_string();
    elsewhere[yes] = "   Yes, I trust this folder".to_string();
    elsewhere.insert(yes + 1, " ❯ Something new".to_string());
    let mut m = fenced(vec![trust_dialog(false), elsewhere]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &auto(30, None), |s| s.set_approval_env(owner_env()));
    assert!(!m.presses().iter().any(|p| p.ends_with(" enter")));

    // Another folder than the session's launch directory.
    let mut m = fenced(vec![trust_dialog(false)]);
    m.cwd = Some("/private/tmp/claude-502/scratch/work2".to_string());
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &auto(30, None), |s| s.set_approval_env(owner_env()));
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains("not the session's cwd")),
        "{:?}",
        m.requests
    );

    // A host without the generation fence: nothing is moved.
    let mut m = Mock::new(vec![trust_dialog(false)]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &auto(30, None), |s| s.set_approval_env(owner_env()));
    assert!(m.presses().is_empty(), "{:?}", m.requests);
}

/// A box is read by the reader for the session's foreground program (`status program=`): a
/// shell showing a captured Claude box is never pressed into. Negative control: under
/// `claude` the same screen is approved.
#[test]
fn a_box_on_a_shell_is_never_pressed() {
    for program in ["zsh", "less"] {
        let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
        m.program = program;
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert!(m.presses().is_empty(), "{program}: {:?}", m.requests);
        assert!(
            !lines.iter().any(|l| l.starts_with("APPROVED")),
            "{lines:?}"
        );
    }
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
    // An agent just started reads as its shell until the server resolves its name: the box
    // waits for the verdict (`await agent prompt`, bounded) and is judged under the new name.
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.program = "zsh";
    m.program_resolves_to = Some("claude");
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
    assert!(
        m.requests
            .iter()
            .any(|r| r == "await agent prompt timeout 2000"),
        "{:?}",
        m.requests
    );
    // An agent exec'ed in place keeps its launcher's name (`bash -c 'exec -a claude …'` reads
    // `program=bash` for seconds) while the server's verdict already says `agent=prompt`:
    // the verdict decides.
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.program = "bash";
    m.agent = "prompt";
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
}

/// An unattended loop takes the session's supervisor claim at the start as a lease (renewed
/// before every press:
/// `tier1_the_claim_is_renewed_before_every_press_and_none_goes_under_anothers`) and releases
/// it at the end, only while it is still its own; `supervise` (one look) takes none.
#[test]
fn a_watch_claims_the_session_renews_and_releases_it() {
    let mut m = fenced(vec![bash_one_row(), busy_screen(), idle_screen()]);
    // The loop ends on its budget, not on the session going: a session gone
    // has no claim left to release.
    m.stall_sleep = Some(Duration::from_millis(20));
    let opts = SuperviseOpts {
        max: Duration::from_millis(400),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_supervisor_name(Some("gui:7".to_string()));
        s.set_claim_timing(Duration::ZERO, 90_000);
    });
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
    assert_eq!(m.claims[0], "meta set supervisor gui:7 ttl=90000");
    // Given back only while still this loop's (`holder=`): a bare unset would clear the claim
    // of a supervisor that took the session after this loop's lease lapsed.
    assert_eq!(
        m.claims.last().map(String::as_str),
        Some("meta unset supervisor holder=gui:7")
    );
    // Negative control: `supervise` claims nothing.
    let mut m = fenced(vec![bash_one_row(), busy_screen(), idle_screen()]);
    session(&mut m, None)
        .supervise(&auto(30, None))
        .expect("supervise");
    assert!(m.claims.is_empty(), "{:?}", m.claims);
}

/// Behind another supervisor's live claim (`ERR busy supervisor=<holder>`) the loop says so
/// and only watches (nothing pressed, no badge raised or cleared), and takes over the moment
/// the claim is free. It never releases a claim it does not hold.
#[test]
fn behind_another_supervisors_claim_the_loop_only_watches() {
    let mut m = fenced(vec![bash_one_row(), bash_one_row(), question_screen()]);
    m.claim_replies = VecDeque::from([err("busy supervisor=gui%3A7")]);
    m.vanish_after = Some(0);
    let (dir, path) = journal_file("behind-claim");
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_claim_timing(Duration::from_secs(3600), 90_000);
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        lines[0].starts_with("WATCHING another supervisor holds this session: gui:7"),
        "{lines:?}"
    );
    // The question is escalated by nobody here, and the journal says so.
    assert!(
        records
            .iter()
            .any(|r| r.summary.starts_with("attention=skipped")
                && r.summary.contains(" mail=skipped")),
        "{records:#?}"
    );
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")
                || r.starts_with("meta unset attention")
                || r.starts_with("post")),
        "{:?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.starts_with("EVENT prompt ")),
        "the point is still reported to the watch's reader: {lines:?}"
    );
    assert!(
        !m.claims.iter().any(|c| c.contains("unset")),
        "{:?}",
        m.claims
    );

    // The claim frees up at the first renewal: the loop takes over.
    let mut m = fenced(vec![busy_screen(), bash_one_row(), bash_one_row()]);
    m.claim_replies = VecDeque::from([err("busy supervisor=gui%3A7")]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_claim_timing(Duration::ZERO, 90_000);
    });
    assert!(lines[0].starts_with("WATCHING"), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.starts_with("APPROVED")),
        "taken over once the claim was free: {lines:?} {:?}",
        m.requests
    );
    assert!(!m.presses().is_empty(), "{:?}", m.requests);
}

/// The hosted loop yields (`SuperviseOpts::yield_when_held`): behind another holder's claim
/// it ends with `CLAIM_HELD` + the holder, nothing pressed, so the host parks the session.
/// Negative control: with the claim free the box is approved under the host's one claim.
/// ON THE PRODUCTION CLOCK (`Timings::PRODUCTION`, the supervisor family's real-clock
/// test): the claim asks the shipped 90 s lease.
#[test]
fn a_hosted_loop_behind_another_claim_ends_and_presses_nothing() {
    let hosted = SuperviseOpts::hosted();
    assert!(hosted.yield_when_held);
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.claim_replies = VecDeque::from([err("busy supervisor=someone-else")]);
    let stop = Arc::new(AtomicBool::new(false));
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_timings(crate::harness::Timings::PRODUCTION);
    s.set_supervisor_name(Some("aterm-harness@7".to_string()));
    let r = s.run_hosted(&hosted, stop, &mut out);
    assert_eq!(
        r,
        Err(format!("{CLAIM_HELD}someone-else")),
        "{}",
        String::from_utf8_lossy(&out)
    );
    let claim = "@s-1 meta set supervisor aterm-harness@7 ttl=90000";
    assert_eq!(
        m.claims,
        [claim],
        "one claim, the host's name, never released (it was never held)"
    );
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(
        !m.requests.iter().any(|r| r.contains("attention")),
        "{:?}",
        m.requests
    );

    // Negative control: the claim free, the same box is approved under the
    // same single claim, and given back holder-conditionally at the end.
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.vanish_after = Some(0);
    let stop = Arc::new(AtomicBool::new(false));
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_timings(crate::harness::Timings::PRODUCTION);
    s.set_supervisor_name(Some("aterm-harness@7".to_string()));
    let _ = s.run_hosted(&hosted, stop, &mut out);
    let text = String::from_utf8_lossy(&out).to_string();
    assert!(text.starts_with("APPROVED"), "{text}");
    assert!(!m.presses().is_empty(), "{:?}", m.requests);
    assert_eq!(
        m.claims.first().map(String::as_str),
        Some(claim),
        "{:?}",
        m.claims
    );
    assert!(
        m.claims
            .iter()
            .all(|c| c.contains("aterm-harness@7") && !c.contains("aterm-supervise")),
        "every claim request names the host: {:?}",
        m.claims
    );
}

/// A host without the claim (`ERR usage`) acts as if the claim did not exist:
/// the negative control that the claim is no gate the loop needs.
#[test]
fn a_host_without_the_claim_is_supervised_as_before() {
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.claim_replies = VecDeque::from([usage("meta [set|unset] <field> [text]")]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
    assert_eq!(
        m.claims.len(),
        1,
        "never released, never renewed: {:?}",
        m.claims
    );
}

/// How the scripted server answers one claim request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimAnswer {
    /// `OK`: the claim was free or the loop's.
    Ok,
    /// `ERR busy supervisor=gui:7`: it lapsed and another took it.
    Busy,
    /// Not served (the connection closed): an outage.
    Lost,
}

/// Tier-1 for `SupervisorClaim` (aterm-spec `supervisor_claim_model`): the real `Session` with
/// the claim renewed before every request, projected onto the model. A claim request is
/// `Renewal` (its answer fixing what the environment did before it: `OK` free or the loop's,
/// `ERR busy` lapsed and taken) or `Unserved`; a `key` is `Press`, which must be enabled.
/// `script`: the claim answers in order, `OK` after.
fn claim_conformance(script: &[ClaimAnswer]) -> (Mock, aterm_spec::derive::Model) {
    let model = aterm_models_harness::supervisor_claim_model();
    let mut m = fenced(vec![bash_one_row(), bash_one_row(), bash_one_row()]);
    m.claim_replies = script
        .iter()
        .map(|a| match a {
            ClaimAnswer::Ok => ok("OK\n"),
            ClaimAnswer::Busy => err("busy supervisor=gui%3A7"),
            ClaimAnswer::Lost => closed(),
        })
        .collect();
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_claim_timing(Duration::ZERO, 90_000);
    });
    let mut st = model.init_state();
    let fire = |st: &mut std::collections::BTreeMap<&'static str, i64>, a: &str| {
        assert!(model.fire(a, st), "the model refused {a} at {st:?}");
    };
    // The scripted server's loop is an external supervisor's: unfenced.
    fire(&mut st, "External");
    let mut asked = 0;
    for (req, _) in &m.order {
        if req.starts_with("meta set supervisor") {
            let answer = script.get(asked).copied().unwrap_or(ClaimAnswer::Ok);
            asked += 1;
            match answer {
                ClaimAnswer::Ok => {
                    if st["held"] == 2 {
                        fire(&mut st, "OtherReleases");
                    }
                }
                ClaimAnswer::Busy => {
                    while st["age"] < 3 {
                        fire(&mut st, "Tick");
                    }
                    if st["held"] == 1 {
                        fire(&mut st, "Lapse");
                    }
                    if st["held"] == 0 {
                        fire(&mut st, "OtherClaims");
                    }
                }
                ClaimAnswer::Lost => {}
            }
            if st["age"] < 1 && st["view"] != 3 {
                fire(&mut st, "Tick");
            }
            fire(
                &mut st,
                if answer == ClaimAnswer::Lost {
                    "Unserved"
                } else {
                    "Renewal"
                },
            );
        } else if req.starts_with("key ") {
            assert!(
                model.action_enabled("Press", &st),
                "the real loop pressed at a state the model forbids: {st:?} ({req})"
            );
            fire(&mut st, "Press");
        }
        assert!(
            model.check_invariant("NoPressUnderAnothersClaim", &st),
            "{st:?}"
        );
    }
    (m, model)
}

#[test]
fn tier1_the_claim_is_renewed_before_every_press_and_none_goes_under_anothers() {
    use ClaimAnswer::{Busy, Lost, Ok as Yes};
    // Held throughout: every press is at a state the model permits.
    let (m, _) = claim_conformance(&[]);
    assert!(!m.presses().is_empty(), "{:?}", m.requests);
    // Lost to another supervisor after the start: nothing more is pressed.
    let (m, model) = claim_conformance(&[Yes, Busy, Busy, Busy, Busy, Busy, Busy, Busy]);
    assert!(m.presses().is_empty(), "{:?}", m.order);
    // Pending: renewals unserved through the moment the loop would press, then answered:
    // nothing pressed while it does not know whose the session is, then pressed.
    let lost = [Yes, Lost, Lost, Lost, Lost, Lost, Lost, Lost, Lost];
    let (m, _) = claim_conformance(&lost);
    let claims: Vec<usize> = m
        .order
        .iter()
        .enumerate()
        .filter(|(_, (r, _))| r.starts_with("meta set supervisor"))
        .map(|(i, _)| i)
        .collect();
    let (from, to) = (claims[1], claims[lost.len()]);
    assert!(
        !m.order[from..to].iter().any(|(r, _)| r.starts_with("key ")),
        "a press while pending: {:?}",
        m.order
    );
    assert!(
        m.order[to..].iter().any(|(r, _)| r.starts_with("key ")),
        "pressed once the claim was back: {:?}",
        m.order
    );
    // Negative control: the stale view (lease lapsed, another holds it, not asked since) is
    // refused by the model and admitted with the renewal skipped (`Buggy = 1`).
    let mut stale = model.init_state();
    for (k, v) in [
        ("started", 1),
        ("fenced", 0),
        ("held", 2),
        ("view", 1),
        ("age", 3),
    ] {
        stale.insert(k, v);
    }
    assert!(!model.action_enabled("Press", &stale));
    assert!(aterm_spec::interp::with_buggy(&model, 1).action_enabled("Press", &stale));
    // And a pending view never presses, Buggy or not.
    let mut pending = model.init_state();
    for (k, v) in [("started", 1), ("held", 2), ("view", 3), ("age", 0)] {
        pending.insert(k, v);
    }
    assert!(!aterm_spec::interp::with_buggy(&model, 1).action_enabled("Press", &pending));
    // THE PROVEN REPLACEMENT: the hosted loop's claim replaced while it still
    // reads it as its own; its fenced press is enabled and lands nothing.
    // NEGATIVE CONTROL: unfenced (`Buggy`), the same press is under another's.
    let mut replaced = model.init_state();
    for a in ["Hosted", "Tick", "Renewal", "ProvenReplaces"] {
        assert!(model.fire(a, &mut replaced), "{a} at {replaced:?}");
    }
    assert!(model.action_enabled("Press", &replaced), "{replaced:?}");
    assert!(model.fire("Press", &mut replaced));
    assert!(model.check_invariant("NoPressUnderAnothersClaim", &replaced));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut unfenced = buggy.init_state();
    for a in ["Hosted", "Tick", "Renewal", "ProvenReplaces", "Press"] {
        assert!(buggy.fire(a, &mut unfenced), "{a} at {unfenced:?}");
    }
    assert!(!buggy.check_invariant("NoPressUnderAnothersClaim", &unfenced));
    // An external loop's claim is never replaced so.
    let mut external = model.init_state();
    for a in ["External", "Tick", "Renewal"] {
        assert!(model.fire(a, &mut external), "{a} at {external:?}");
    }
    assert!(!model.action_enabled("ProvenReplaces", &external));
}

/// Tier-1 for `SupervisorFocusChoice` (aterm-spec `supervisor_focus_choice_model`): the real
/// trust-dialog press projected request by request; Enter goes only where the model enables
/// it, after a move that landed, never after one lost.
#[test]
fn tier1_enter_goes_only_on_a_confirmed_focus() {
    let model = aterm_models_harness::supervisor_focus_choice_model();
    for lands in [true, false] {
        // Landed, the dialog leaves (Claude Code starts); lost, it stays.
        let after = if lands {
            idle_screen()
        } else {
            trust_dialog(false)
        };
        let mut m = fenced(vec![trust_dialog(false), trust_dialog(lands), after]);
        m.cwd = Some(TRUST_FOLDER.to_string());
        m.vanish_after = Some(0);
        watch_lines_with(&mut m, &auto(30, None), |s| s.set_approval_env(owner_env()));
        let mut st = model.init_state();
        let mut moved = false;
        let mut entered = false;
        for (req, _) in &m.order {
            if req.starts_with("key ") && req.ends_with(" down") {
                let a = if lands { "MoveLands" } else { "MoveLost" };
                assert!(model.fire(a, &mut st), "{a} at {st:?}");
                moved = true;
            } else if moved && !entered && req.starts_with("text ") {
                assert!(model.fire("Reread", &mut st), "{st:?}");
            } else if req.starts_with("key ") && req.ends_with(" enter") {
                assert!(
                    model.action_enabled("Enter", &st),
                    "the real loop pressed Enter at a state the model forbids: {st:?}"
                );
                assert!(model.fire("Enter", &mut st));
                entered = true;
            }
            assert!(model.check_invariant("NeverEnterOffTheChosenOption", &st));
        }
        assert!(moved, "{:?}", m.order);
        assert_eq!(entered, lands, "{:?}", m.order);
        if !lands {
            assert!(!model.action_enabled("Enter", &st), "{st:?}");
            // Negative control: Enter on the move alone, unread.
            assert!(aterm_spec::interp::with_buggy(&model, 1).action_enabled("Enter", &st));
        }
    }
}

/// Neither a question nor the usage-limit dialog is a permission, so no `approve` level limits
/// either: under `approve = "none"` the question dialog's second tab and its review are each
/// answered by one Enter, and the limit dialog still gets its wait row; nothing escalated.
/// (Guards and ledger rows: `a_four_tab_question_is_answered_tab_by_tab_and_submitted`,
/// `the_limit_options_dialog_waits_for_the_reset_by_itself`; the switch off:
/// `the_sessions_word_decides_its_question_dialog_over_the_switch`.)
#[test]
fn a_question_and_the_limit_dialog_are_answered_under_approve_none() {
    use aterm_phase::prompt::fixtures::{ASK_SUBMIT, ASK_TWO_SECOND, screen};
    let mut opts = answering(30, None);
    opts.policy.approve = Approve::None;
    let mut m = questioned(vec![
        screen(ASK_TWO_SECOND),
        screen(ASK_SUBMIT),
        idle_screen(),
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &opts);
    let presses = m.presses();
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert!(presses.iter().all(|p| p.ends_with(" enter")), "{presses:?}");
    assert!(
        !lines.iter().any(|l| l.starts_with("EVENT prompt")),
        "{lines:?}"
    );

    let mut cfg = crate::supervise::SupervisorConfig::default();
    cfg.set("approve", "none").expect("a level");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&cfg)
    };
    let mut m = fenced(vec![
        screen(aterm_phase::prompt::fixtures::LIMIT_OPTIONS_DIALOG),
        limit_dialog(&[LIMIT_STOP, LIMIT_WAIT, LIMIT_UPGRADE], 1),
        idle_screen(),
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    assert_eq!(
        lines[0],
        format!("APPROVED seq=102 {LIMIT_WAIT}"),
        "{lines:?}"
    );
    let presses = m.presses();
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert!(presses[0].ends_with(" down") && presses[1].ends_with(" enter"));
    assert_eq!(m.attention, None, "nothing asked of the owner");
}

/// A host at the loop's idle points ([`IdleHost`]) as a test scripts it: it `wants` while its
/// flag is set, and each `at_idle` is counted and clears the flag (unless `again` is left).
#[derive(Debug, Default)]
struct TestHost {
    wants: AtomicBool,
    owns: AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
    /// What each restart asked for answers, in turn (`None`, or spent: no restart made).
    restarts: std::sync::Mutex<VecDeque<Option<&'static str>>>,
    /// The restarts asked for, by their word.
    restarted: std::sync::Mutex<Vec<&'static str>>,
    /// `can_restart` holds even once [`Self::restarts`] is spent, as the window's host's
    /// does: a refusal does not change its word that it restarts.
    restartable: AtomicBool,
    /// Breaks of background work offered ([`IdleHost::at_background`]), each answered.
    backgrounds: std::sync::atomic::AtomicUsize,
    /// Its word that nobody has asked the session anything ([`IdleHost::taskless`]).
    taskless: AtomicBool,
    /// What its record says of the session's task ([`IdleHost::finished`]),
    /// and how often the loop asked.
    finished: std::sync::Mutex<Option<bool>>,
    finished_asks: std::sync::atomic::AtomicUsize,
    /// The record says the task is open (`Some(false)`) until this many
    /// turns ran ([`Self::turns_ran`]), then [`Self::finished`].
    finished_from: std::sync::atomic::AtomicUsize,
    /// Its step lets the turn ends go: `owns` cleared by `at_idle`.
    lets_go: AtomicBool,
    /// Its step is a person's hold at the point (`wait:attended`), owning the turn ends.
    holds: AtomicBool,
    /// Each hold after the first is said as a repeat ([`HostStep::repeat`]).
    marks: AtomicBool,
    /// The turns the loop said ran ([`IdleHost::turn_ran`]).
    turns_ran: std::sync::atomic::AtomicUsize,
    /// The decided idle turn ends the loop offered its usage tap ([`IdleHost::usage_tap`]).
    taps: std::sync::atomic::AtomicUsize,
    /// Its step types into the agent and owns nothing after it: a relaunch's carry-on
    /// ([`HostStep::typed`]).
    types: AtomicBool,
    /// Its moving step ends the agent and types nothing into it: a restart
    /// ([`HostStep::typed`] false).
    ends: AtomicBool,
    /// Steps it asks for again after the one taken (the upgrade parked after its notice).
    again: std::sync::atomic::AtomicUsize,
    /// The limit episodes the loop told of ([`IdleHost::limited`]), in order.
    limits: std::sync::Mutex<Vec<bool>>,
    /// The exceptions the loop told of ([`IdleHost::exception`]), in order:
    /// `Some(kind word)` as one appears, `None` as it ends.
    exceptions: std::sync::Mutex<Vec<Option<&'static str>>>,
    /// The loop's asks whether the host owns a turn end: evidence it reached one.
    turn_ends: std::sync::atomic::AtomicUsize,
    /// The guards the loop said withheld a point ([`IdleHost::withheld`]), in order.
    withheld: std::sync::Mutex<Vec<Guard>>,
    /// How many of the loop's first `wants` asks it answers `false` whatever [`Self::wants`]
    /// says: a host that asks for no point until the loop has acted at the first one.
    deaf_for: std::sync::atomic::AtomicUsize,
    /// How many times the loop asked ([`IdleHost::wants`]).
    asked: std::sync::atomic::AtomicUsize,
    /// Its first step relaunches the agent (`upgrade step=adopted`) and asks for the next idle
    /// point, where its second types the carry-on (`carry-on step=continued`).
    adopts: AtomicBool,
    /// The agent's memory it measures ([`IdleHost::memory`]), in MiB, on a 128
    /// GiB machine ([`Self::trouble`] says in what state); `0`: none.
    memory_mib: std::sync::atomic::AtomicU64,
    /// The machine is in trouble: memory pressure warn while it swaps 89
    /// MB/s, 2 GiB available of 128, 120 GiB in use. `false`: memory to spare
    /// (normal pressure, no swap, 60 GiB available and 60 in use).
    trouble: AtomicBool,
    /// The machine falls into trouble at this memory ask (1-based); `0`: [`Self::trouble`]
    /// alone decides.
    trouble_from_ask: std::sync::atomic::AtomicUsize,
    /// How many times the loop asked it for the agent's memory.
    memory_asks: std::sync::atomic::AtomicUsize,
    /// The memory it measures from its first restart asked on, in MiB
    /// (`0`: unchanged) — a measure that fell while the restart was tried.
    memory_at_restart: std::sync::atomic::AtomicU64,
    /// The memory it measures once it has made a restart (`adopted`), in MiB (`0`:
    /// unchanged): the relaunched agent a fresh, small one.
    memory_once_made: std::sync::atomic::AtomicU64,
    /// What the loop warned a person of ([`IdleHost::warn`]), in order.
    warned: std::sync::Mutex<Vec<String>>,
    /// The usage limit's reset its agent's transcript names ([`IdleHost::limit_until`];
    /// `None`: none), and how many times the loop asked for it.
    limit_until: std::sync::Mutex<Option<u64>>,
    limit_asks: std::sync::atomic::AtomicUsize,
    /// The limit row its agent's transcript stands at
    /// ([`IdleHost::standing_limit`]; `None`: none, or one the host moved the
    /// conversation off).
    standing: std::sync::Mutex<Option<crate::harness::upgrade::StandingLimit>>,
    /// Its agent's transcript tail ([`IdleHost::transcript_tail`]) before
    /// its first restart and after it; `None`: none to read.
    tails: std::sync::Mutex<Option<(String, String)>>,
    /// How many times the loop asked for it.
    tail_asks: std::sync::atomic::AtomicUsize,
}

/// A healthy Claude Code's footprint.
const HEALTHY_MIB: u64 = 450;
/// A runaway Claude Code's footprint on a 128 GB machine.
const RUNAWAY_MIB: u64 = 220_279;

impl IdleHost for TestHost {
    fn wants(&self) -> bool {
        let n = self.asked.fetch_add(1, Ordering::SeqCst);
        n >= self.deaf_for.load(Ordering::SeqCst) && self.wants.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        let again = self.again.load(Ordering::SeqCst) > 0;
        if again {
            self.again.fetch_sub(1, Ordering::SeqCst);
        }
        self.wants.store(again, Ordering::SeqCst);
        self.steps.fetch_add(1, Ordering::SeqCst);
        let step = |line: &str, moved: bool| {
            Some(HostStep {
                line: line.to_string(),
                moved,
                typed: (moved && !line.contains("done:fresh")).then(|| {
                    use crate::supervise::policy::turn_end::HostTurn;
                    if line.ends_with("=continued") {
                        HostTurn::CarryOn
                    } else {
                        HostTurn::Notice
                    }
                }),
                repeat: false,
            })
        };
        if self.adopts.load(Ordering::SeqCst) {
            self.owns.store(false, Ordering::SeqCst);
            if self.steps.load(Ordering::SeqCst) == 1 {
                self.wants.store(true, Ordering::SeqCst);
                return Some(HostStep {
                    line: "upgrade step=adopted".to_string(),
                    moved: true,
                    typed: None,
                    repeat: false,
                });
            }
            return step("carry-on step=continued", true);
        }
        if self.ends.load(Ordering::SeqCst) {
            self.owns.store(false, Ordering::SeqCst);
            return step("upgrade step=done:fresh", true);
        }
        if self.types.load(Ordering::SeqCst) {
            self.owns.store(false, Ordering::SeqCst);
            return step("carry-on step=continued", true);
        }
        if self.lets_go.load(Ordering::SeqCst) {
            self.owns.store(false, Ordering::SeqCst);
            return step("upgrade step=wait:settling", false);
        }
        if self.holds.load(Ordering::SeqCst) {
            self.owns.store(true, Ordering::SeqCst);
            return Some(HostStep {
                line: "upgrade step=wait:attended".to_string(),
                moved: false,
                typed: None,
                repeat: self.marks.load(Ordering::SeqCst) && self.steps.load(Ordering::SeqCst) > 1,
            });
        }
        step("upgrade step=announced:1", true)
    }
    fn owns_turn_end(&self) -> bool {
        self.turn_ends.fetch_add(1, Ordering::SeqCst);
        self.owns.load(Ordering::SeqCst)
    }
    fn restart(&self, why: &crate::supervise::policy::turn_end::Restart) -> Option<String> {
        self.restarted.lock().unwrap().push(why.word());
        let fell = self.memory_at_restart.swap(0, Ordering::SeqCst);
        if fell > 0 {
            self.memory_mib.store(fell, Ordering::SeqCst);
        }
        let step = self.restarts.lock().unwrap().pop_front().flatten();
        let made = self.memory_once_made.swap(0, Ordering::SeqCst);
        if step == Some("adopted") && made > 0 {
            self.memory_mib.store(made, Ordering::SeqCst);
        } else {
            self.memory_once_made.store(made, Ordering::SeqCst);
        }
        step.map(str::to_string)
    }
    /// A host that restarts: one with an answer scripted, or one that
    /// always does ([`Self::restartable`]).
    fn can_restart(&self) -> bool {
        self.restartable.load(Ordering::SeqCst) || !self.restarts.lock().unwrap().is_empty()
    }
    fn at_background(&self) -> Option<String> {
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
        Some("upgrade step=announced:1".to_string())
    }
    fn taskless(&self) -> bool {
        self.taskless.load(Ordering::SeqCst)
    }
    fn finished(&self) -> Option<bool> {
        self.finished_asks.fetch_add(1, Ordering::SeqCst);
        if self.turns_ran.load(Ordering::SeqCst) < self.finished_from.load(Ordering::SeqCst) {
            return Some(false);
        }
        *self.finished.lock().unwrap()
    }
    fn turn_ran(&self) {
        self.turns_ran.fetch_add(1, Ordering::SeqCst);
    }
    fn usage_tap(&self) {
        self.taps.fetch_add(1, Ordering::SeqCst);
    }
    fn limited(&self, open: bool) {
        self.limits.lock().unwrap().push(open);
    }
    fn withheld(&self, guard: Guard) {
        self.withheld.lock().unwrap().push(guard);
    }
    fn exception(&self, standing: Option<&crate::supervise::policy::exception::Exception>) {
        self.exceptions
            .lock()
            .unwrap()
            .push(standing.map(|e| e.kind.word()));
    }
    fn memory(&self) -> Option<crate::supervise::memory::AgentMemory> {
        let ask = self.memory_asks.fetch_add(1, Ordering::SeqCst) + 1;
        use crate::supervise::memory::{AgentMemory, Pressure};
        let mib = self.memory_mib.load(Ordering::SeqCst);
        let from = self.trouble_from_ask.load(Ordering::SeqCst);
        let trouble = self.trouble.load(Ordering::SeqCst) || (from > 0 && ask >= from);
        (mib > 0).then_some(AgentMemory {
            footprint_mib: mib,
            physical_mib: Some(128 * 1024),
            available_mib: Some(if trouble { 2 * 1024 } else { 60 * 1024 }),
            used_mib: Some(if trouble { 120 * 1024 } else { 60 * 1024 }),
            pressure: Some(if trouble {
                Pressure::Warn
            } else {
                Pressure::Normal
            }),
            swap_kib_s: Some(if trouble { 89 * 1024 } else { 0 }),
            largest_other_mib: None,
        })
    }
    fn warn(&self, text: &str) {
        self.warned.lock().unwrap().push(text.to_string());
    }
    fn limit_until(&self) -> Option<u64> {
        self.limit_asks.fetch_add(1, Ordering::SeqCst);
        *self.limit_until.lock().unwrap()
    }
    fn standing_limit(&self) -> Option<crate::harness::upgrade::StandingLimit> {
        self.standing.lock().unwrap().clone()
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail_asks.fetch_add(1, Ordering::SeqCst);
        let (before, after) = self.tails.lock().unwrap().clone()?;
        Some(if self.restarted.lock().unwrap().is_empty() {
            before
        } else {
            after
        })
    }
}

/// A limit episode is the host's to hear and the loop's to wait out, even while the upgrade
/// owns the turn ends: the host is told it opened and closed (`limited`), and past the reset
/// the continuation is typed.
#[test]
fn a_limit_episode_is_told_to_the_host_and_waited_out_over_its_turn_ends() {
    let host = Arc::new(TestHost::default());
    host.owns.store(true, Ordering::SeqCst);
    let (dir, path) = journal_file("limit-host");
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        busy_screen(),
        answered("keep going"),
    ]);
    m.vanish_after = Some(3);
    m.turn_releases = Some(1);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &resuming(&opts, None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
    });
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=102 rule=usage-resume")),
        "the limit is continued past its reset: {lines:#?}"
    );
    assert_eq!(*host.limits.lock().unwrap(), [true, false], "{lines:#?}");
    assert_eq!(host.steps.load(Ordering::SeqCst), 0, "no step at a wall");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host owed a relaunched agent's last step, as the window's host is after
/// a crash's relaunch (`adopted`): it asks for every idle point while owed
/// (the real host's park), counts the points offered it
/// ([`IdleHost::at_idle`], where its park clears), and is asked at every
/// point a guard withheld where the agent rests
/// ([`IdleHost::at_withheld_point`]) — each ask kept with the guard said
/// before it and its `wake`. It answers from its script while one is left
/// (`None`: a miss — another sweep's lock, a relaunch not adopted yet),
/// then as `relaunch::settle` does a carry-on the loop's own act made moot:
/// `done:carried` once the loop's act is on its journal, nothing
/// (`wait:owed`) before — or, `done_word` set, that word once the loop's
/// `DONE` is (a settle that reads the task's end). `late`: it wants no
/// point until the loop's wait first asks when it looks again
/// ([`IdleHost::wants_at`]) after the limit's episode opened — a look its
/// idle step left coming due partway through that wait, as the window's
/// host's does (F1-R5-A1) — and it then answers only at a point's ask
/// (`wake` false), as that host does when its last settle left nothing a
/// later one can settle.
#[derive(Debug, Default)]
struct QuietHost {
    owed: AtomicBool,
    late: AtomicBool,
    episode: AtomicBool,
    due: std::sync::Mutex<Option<Instant>>,
    steps: std::sync::atomic::AtomicUsize,
    journal: std::sync::Mutex<Option<PathBuf>>,
    script: std::sync::Mutex<VecDeque<Option<&'static str>>>,
    done_word: std::sync::Mutex<Option<&'static str>>,
    asks: std::sync::Mutex<Vec<(Option<&'static str>, bool)>>,
    guard: std::sync::Mutex<Option<&'static str>>,
    limits: std::sync::Mutex<Vec<bool>>,
}

impl QuietHost {
    fn owed(journal: &Path) -> Arc<Self> {
        let host = Arc::new(Self::default());
        host.owed.store(true, Ordering::SeqCst);
        *host.journal.lock().unwrap() = Some(journal.to_path_buf());
        host
    }
    /// Whether the loop's journal holds a line starting `prefix` yet.
    fn journaled(&self, prefix: &str) -> bool {
        let path = self.journal.lock().unwrap().clone();
        path.and_then(|p| crate::supervise::journal::read_journal(&p).ok())
            .is_some_and(|(records, _)| records.iter().any(|r| r.line.starts_with(prefix)))
    }
    fn asks(&self) -> Vec<(Option<&'static str>, bool)> {
        self.asks.lock().unwrap().clone()
    }
}

impl IdleHost for QuietHost {
    fn wants(&self) -> bool {
        self.owed.load(Ordering::SeqCst)
            && (!self.late.load(Ordering::SeqCst) || self.due.lock().unwrap().is_some())
    }
    fn wants_at(&self) -> Option<Instant> {
        if !self.late.load(Ordering::SeqCst) || !self.episode.load(Ordering::SeqCst) {
            return None;
        }
        Some(*self.due.lock().unwrap().get_or_insert_with(Instant::now))
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        self.owed.store(false, Ordering::SeqCst);
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn withheld(&self, guard: Guard) {
        *self.guard.lock().unwrap() = Some(guard.word());
    }
    fn at_withheld_point(&self, wake: bool) -> Option<String> {
        let guard = *self.guard.lock().unwrap();
        self.asks.lock().unwrap().push((guard, wake));
        if wake && self.late.load(Ordering::SeqCst) {
            return None;
        }
        let scripted = self.script.lock().unwrap().pop_front();
        let word = match scripted {
            Some(word) => word,
            None => match *self.done_word.lock().unwrap() {
                Some(word) => self.journaled("DONE ").then_some(word),
                None => self.journaled("CONTINUED ").then_some("done:carried"),
            },
        }?;
        self.owed.store(false, Ordering::SeqCst);
        Some(format!("carry-on step={word}"))
    }
    fn limited(&self, open: bool) {
        self.limits.lock().unwrap().push(open);
        self.episode.store(open, Ordering::SeqCst);
    }
}

/// One watch of a relaunched agent under `host`: a USAGE LIMIT's notice its
/// loop resumes at the reset (`resumes`), answered at once with `answer` —
/// no busy read between, so the limit's episode stays open and withholds
/// every point from the host (the lab's L7) — or, `limited` false, a turn
/// that ends with no limit at all. What the loop said, what it typed, and
/// the host's journaled lines.
fn quiet_run(
    tag: &str,
    host: &Arc<QuietHost>,
    journal: &Path,
    limited: bool,
    resumes: bool,
    answer: Vec<String>,
    cursor: bool,
) -> (Vec<String>, usize, Vec<String>) {
    let screens = if limited {
        vec![busy_screen(), weekly_limited(), answer]
    } else {
        vec![busy_screen(), answer]
    };
    let mut m = Mock::new(screens);
    m.vanish_after = Some(6);
    m.cursor_on_caret = cursor;
    let mut opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(journal.to_path_buf()),
        ..auto(30, None)
    };
    if resumes {
        // The notice sits until the loop's resume is typed.
        m.turn_releases = Some(1);
        opts = resuming(&opts, None);
    } else {
        opts.policy.continue_policy = false;
    }
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
    });
    let (records, _) = journal_records(journal);
    let host_lines = records
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("HOST "))
        .collect();
    let _ = tag;
    (lines, count(&m, "turn"), host_lines)
}

/// A RELAUNCH'S LAST STEP IS SETTLED BY THE LOOP'S OWN ACT UNDER A LIMIT
/// EPISODE (F1 of the live lab of 2026-10-16, Claude Code 2.1.301, the lab's
/// L7): a crash relaunched the agent into a usage limit, and the episode
/// withheld every point from the host until its reset plus `LIMIT_GRACE_S`
/// — while the loop typed its own acts on the resumed agent (the limit's
/// resume, the done check) and ended the task. The step the host owed was
/// never taken: its record stayed `relaunched`, the tab's automatic update
/// and the restore look held behind it. The loop asks its host where the
/// agent rests at a point a guard withheld from it
/// ([`IdleHost::at_withheld_point`]): at the notice, before the resume is
/// typed, nothing is settled (the carry-on would still type); as the wait
/// after it begins (`wake` false) the loop's act is on the record, and the
/// step is settled with nothing typed (`HOST seq=<n> carry-on
/// step=done:carried`), no point ever offered and nothing asked once
/// nothing is owed. NEGATIVE CONTROL: the same screens with nothing typed
/// (no limit resume, no continuation) settle nothing — asked at the notice,
/// the step stays owed.
#[test]
fn a_relaunchs_last_step_is_settled_by_the_loops_own_act_under_a_limit_episode() {
    let (dir, path) = journal_file("owed-under-limit-acts");
    let host = QuietHost::owed(&path);
    let (lines, typed, settled) = quiet_run(
        "acts",
        &host,
        &path,
        true,
        true,
        answered("keep going"),
        true,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=102 rule=usage-resume")),
        "the loop's own act at the notice: {lines:#?}"
    );
    assert_eq!(typed, 1, "{lines:#?}");
    assert_eq!(
        host.limits.lock().unwrap().first(),
        Some(&true),
        "the episode opened: {lines:#?}"
    );
    assert_eq!(
        host.steps.load(Ordering::SeqCst),
        0,
        "no point offered: {lines:#?}"
    );
    assert!(
        !host.owed.load(Ordering::SeqCst),
        "the step settled: {lines:#?}"
    );
    assert_eq!(
        settled,
        ["HOST seq=102 carry-on step=done:carried"],
        "{lines:#?}"
    );
    assert_eq!(
        host.asks(),
        [(Some("not-idle"), false), (Some("not-idle"), false)],
        "at the notice, then as its wait began: nothing asked once settled"
    );
    // NEGATIVE CONTROL: nothing typed, nothing settled.
    let (dir, path) = journal_file("owed-under-limit-none");
    let host = QuietHost::owed(&path);
    let (lines, typed, settled) = quiet_run(
        "none",
        &host,
        &path,
        true,
        false,
        answered("keep going"),
        true,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(!host.asks().is_empty(), "asked at the notice: {lines:#?}");
    assert!(host.owed.load(Ordering::SeqCst), "still owed: {lines:#?}");
    assert!(settled.is_empty(), "{settled:#?}");
}

/// The resumed agent's answer: a turn that ended after work, its last words
/// `said`, at an empty composer.
fn resumed_answer(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 12m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// THE TASK'S END AT A WITHHELD POINT IS ASKED AS ITS WAIT BEGINS (F1 of the
/// live lab of 2026-10-16; F1-R4): the agent reports its task finished at a
/// point a guard withholds from the host — here the Mac's wake, inside its
/// settle (`guard=wake-settle`) — and the loop says `DONE` there. The host
/// is asked at that point before the decision, and again — `wake` false —
/// as its wait begins, the `DONE` said: what it owed there that types
/// nothing and reads the task's end (a relaunch's carry-on into a finished
/// task, `done:finished`) is settled then, journaled once (`HOST seq=<n>
/// …`), nothing typed. NEGATIVE CONTROL: a turn end the loop neither ends
/// nor acts on (the continue policy off) says no `DONE`, and the host's
/// settle that waits on one settles nothing.
#[test]
fn the_tasks_end_at_a_withheld_point_is_asked_as_its_wait_begins() {
    use crate::supervise::policy::turn_end::FINISHED;
    for done in [true, false] {
        let (dir, path) = journal_file(&format!("end-asks-host-{done}"));
        let host = QuietHost::owed(&path);
        *host.done_word.lock().unwrap() = Some("done:finished");
        let said = if done {
            FINISHED
        } else {
            "Fixed the parser; the suite is green."
        };
        let mut m = Mock::new(vec![busy_screen(), resumed_answer(said)]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let mut opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..auto(30, None)
        };
        opts.policy.continue_policy = done;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            // The Mac woke a moment ago: its settle holds every point.
            s.settle_wake(TEST_NOW, Instant::now());
            s.seeds_learned_state("the Mac's wake settled before the run");
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let records: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let told = usize::from(done);
        assert_eq!(count(&m, "turn"), 0, "done={done}: {records:#?}");
        assert_eq!(
            records.iter().filter(|l| l.starts_with("DONE ")).count(),
            told,
            "done={done}: {records:#?}"
        );
        let settled: Vec<&String> = records.iter().filter(|l| l.starts_with("HOST ")).collect();
        assert_eq!(settled.len(), told, "done={done}: {records:#?}");
        assert_eq!(
            host.owed.load(Ordering::SeqCst),
            !done,
            "done={done}: {lines:#?}"
        );
        let asks = host.asks();
        assert!(
            asks.iter().all(|(guard, _)| *guard == Some("wake-settle")),
            "done={done}: {asks:?}"
        );
        if done {
            assert_eq!(
                *settled[0], "HOST seq=102 carry-on step=done:finished",
                "{records:#?}"
            );
            assert_eq!(
                asks,
                [(Some("wake-settle"), false), (Some("wake-settle"), false)],
                "at the point, then as the DONE's wait began"
            );
        } else {
            assert!(asks.len() >= 2, "asked, settled nothing: {asks:?}");
        }
    }
}

/// A SETTLE A WITHHELD POINT MISSED IS SETTLED AGAIN AT THE LOOP'S LATER
/// WAKES, AND ONLY WHERE THE AGENT RESTS (F1-R2 and F1-R4, the second and
/// fourth verifications of F1's fix, 2026-10-16): the settle at the loop's
/// last point and as its wait begins may both miss (another sweep's lock, a
/// relaunch not adopted yet), and the episode withholds every later point.
/// The later wakes of that wait ask again (`wake` true), each at most the
/// loop's step apart — no timer of the host's — until one settles, and then
/// none: here the one after the notice's two asks, and one after the next
/// point's two misses. Never a spin: asked once per point or wake while it
/// keeps missing. NEGATIVE CONTROLS: an idle point no guard withholds is
/// the host's step (offered once, the quiet point never asked), and an
/// idle point its reader does not vouch for (no cursor in the prompt box,
/// the start-up read) is asked nothing.
#[test]
fn a_settle_a_withheld_point_missed_is_settled_again_at_the_loops_later_wakes() {
    let run = |tag: &str, script: Vec<Option<&'static str>>| {
        let (dir, path) = journal_file(&format!("quiet-{tag}"));
        let host = QuietHost::owed(&path);
        *host.script.lock().unwrap() = script.into();
        let (lines, typed, settled) =
            quiet_run(tag, &host, &path, true, true, answered("keep going"), true);
        let _ = std::fs::remove_dir_all(&dir);
        (host, lines, typed, settled)
    };
    // MISSED AT THE NOTICE AND AS ITS WAIT BEGAN: the next point settles it.
    let (host, lines, typed, settled) = run("missed-twice", vec![None, None, Some("done:carried")]);
    assert_eq!(typed, 1, "the loop's resume alone: {lines:#?}");
    assert_eq!(host.steps.load(Ordering::SeqCst), 0, "no idle step offered");
    assert!(!host.owed.load(Ordering::SeqCst), "{:?}", host.asks());
    assert_eq!(
        host.asks(),
        [
            (Some("not-idle"), false),
            (Some("not-idle"), false),
            (Some("limit-episode"), false),
        ],
        "{lines:#?}"
    );
    assert_eq!(settled, ["HOST seq=103 carry-on step=done:carried"]);
    // MISSED AT THE LAST POINT AND AS ITS WAIT BEGAN TOO: a later wake of
    // that wait settles it, once (F1-R2-A1's shape).
    let (host, lines, typed, settled) = run(
        "last-point",
        vec![None, None, None, None, Some("done:finished")],
    );
    assert_eq!(typed, 1, "{lines:#?}");
    assert!(!host.owed.load(Ordering::SeqCst), "{:?}", host.asks());
    let asks = host.asks();
    assert_eq!(asks.len(), 5, "{asks:?}");
    assert_eq!(
        asks[4],
        (Some("limit-episode"), true),
        "a later wake: {asks:?}"
    );
    assert_eq!(settled, ["HOST seq=103 carry-on step=done:finished"]);
    // NEVER SETTLES (a carry-on that would type): asked at each point and
    // wake the loop has anyway, nothing typed by the host, still owed.
    let (host, lines, typed, settled) = run("never", vec![None; 1000]);
    assert_eq!(typed, 1, "{lines:#?}");
    assert!(host.owed.load(Ordering::SeqCst));
    assert!(settled.is_empty(), "{settled:?}");
    let asks = host.asks();
    assert!(
        asks.len() >= 3 && asks.len() <= 12,
        "paced by the loop's own points and wakes: {asks:?}"
    );
    assert!(asks.iter().any(|(_, wake)| *wake), "{asks:?}");
    // NEGATIVE CONTROL: no guard — the host's own step, never the quiet
    // point.
    let (dir, path) = journal_file("quiet-unguarded");
    let host = QuietHost::owed(&path);
    let (lines, typed, settled) = quiet_run(
        "unguarded",
        &host,
        &path,
        false,
        false,
        answered("keep going"),
        true,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(host.steps.load(Ordering::SeqCst), 1, "{lines:#?}");
    assert!(host.asks().is_empty(), "{:?}", host.asks());
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(settled.is_empty());
    // NEGATIVE CONTROL: an idle point the reader does not vouch for — no
    // step, no quiet point.
    let (dir, path) = journal_file("quiet-unvouched");
    let host = QuietHost::owed(&path);
    let (lines, _, settled) = quiet_run(
        "unvouched",
        &host,
        &path,
        false,
        false,
        answered("keep going"),
        false,
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        *host.guard.lock().unwrap(),
        Some("not-authoritative"),
        "{lines:#?}"
    );
    assert_eq!(host.steps.load(Ordering::SeqCst), 0, "{lines:#?}");
    assert!(host.asks().is_empty(), "{:?}", host.asks());
    assert!(settled.is_empty());
    // NEGATIVE CONTROL: a box held for a person is no point the agent rests
    // at — withheld, and nothing asked there or at its wakes.
    let (dir, path) = journal_file("quiet-box");
    let host = QuietHost::owed(&path);
    let mut m = Mock::new(vec![bypass_busy(), rm_box("S=/usr; rm -rf \"$S/t5\"")]);
    m.cwd = Some("/Users/_owner/proj".to_string());
    m.vanish_after = Some(3);
    m.cursor_on_caret = true;
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert_eq!(*host.guard.lock().unwrap(), Some("not-idle"), "{lines:#?}");
    assert!(host.asks().is_empty(), "{:?}", host.asks());
}

/// THE FIRST QUIET ASK OF A WAIT IS A POINT'S, WHENEVER THE HOST BEGINS
/// WANTING ONE (F1-R5-A1, the fifth verification of F1's fix, 2026-10-16):
/// the window's host wants no point while the look its idle step left open
/// is not due (`After::Finished` with the relaunch still owed: `park`
/// cleared, the look handed to `look_at`), and its loop's act there may meet
/// a usage limit at once. That notice's point and the first step of its
/// wait came while the host wanted nothing, and `wake` was spent on them:
/// once the look came due partway through the wait, every ask was a wake,
/// and a host whose last settle left nothing for a later one asks nothing
/// at a wake — the look never read, the record owed until the loop's next
/// point (for good, where the limit names no reset). The wait's first ASK is
/// the point's own (`wake` false), not its first step: here the look comes
/// due at the wait's first step, the next step asks as at a point and the
/// host settles (`HOST seq=<n> carry-on step=done:carried`), nothing asked
/// once settled. NEGATIVE CONTROL: a host that wants nothing is never
/// asked.
#[test]
fn the_first_quiet_ask_of_a_wait_the_host_began_wanting_in_is_a_points() {
    let run = |tag: &str, owed: bool| {
        let (dir, path) = journal_file(&format!("quiet-late-{tag}"));
        let host = QuietHost::owed(&path);
        host.owed.store(owed, Ordering::SeqCst);
        host.late.store(true, Ordering::SeqCst);
        *host.script.lock().unwrap() = [Some("done:carried")].into();
        let mut m = Mock::new(vec![busy_screen(), weekly_limited()]);
        m.vanish_after = Some(4);
        m.cursor_on_caret = true;
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..auto(30, None)
        };
        opts.policy.continue_policy = false;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(AFTER_RESET, PDT);
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let settled: Vec<String> = records
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("HOST "))
            .collect();
        (host, lines, count(&m, "turn"), settled)
    };
    let (host, lines, typed, settled) = run("owed", true);
    assert_eq!(typed, 0, "{lines:#?}");
    assert_eq!(
        *host.limits.lock().unwrap(),
        [true],
        "the episode opened and stood: {lines:#?}"
    );
    assert!(
        host.due.lock().unwrap().is_some(),
        "the look came due within the wait: {lines:#?}"
    );
    let asks = host.asks();
    assert_eq!(
        asks,
        [(Some("not-idle"), false)],
        "the wait's first ask is the point's own, and settles: nothing asked \
         once settled"
    );
    assert!(!host.owed.load(Ordering::SeqCst), "settled: {asks:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert!(
        settled[0].ends_with(" carry-on step=done:carried"),
        "{settled:?}"
    );
    // NEGATIVE CONTROL: nothing owed, no point wanted — never asked.
    let (host, lines, typed, settled) = run("nothing", false);
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(host.asks().is_empty(), "{:?}", host.asks());
    assert!(settled.is_empty(), "{settled:?}");
}

/// A spend limit naming no reset is an exception journaled once (`EXCEPTION … => <action>`),
/// the host told once; the next busy read clears it (`CLEARED … exception=spend`, the host
/// told `None`). A `later` exception raises nothing.
#[test]
fn an_exception_is_journaled_once_and_cleared_when_the_worker_works() {
    let host = Arc::new(TestHost::default());
    let (dir, path) = journal_file("exception-once");
    let spend = |extra: &str| {
        let mut r = rows(&[
            "⏺ Running the A/B on m7 now.",
            "  ⎿  You're out of extra usage · add funds to continue",
            extra,
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let mut m = Mock::new(vec![
        busy_screen(),
        spend(""),
        spend("(still)"),
        busy_screen(),
        busy_screen(),
    ]);
    m.vanish_after = Some(5);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &resuming(&opts, None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
    });
    let (records, _) = journal_records(&path);
    let lines_of = |kind: &str| -> Vec<&str> {
        records
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.line.as_str())
            .collect()
    };
    let told = lines_of("exception");
    assert_eq!(told.len(), 1, "one EXCEPTION line: {records:#?} {lines:#?}");
    assert!(
        told[0].contains("kind=spend when=later") && told[0].contains(" => "),
        "{}",
        told[0]
    );
    assert!(
        lines_of("cleared")
            .iter()
            .any(|l| l.contains(" exception=spend after=") && l.ends_with("the agent working")),
        "the busy read closes it: {records:#?}"
    );
    assert_eq!(
        *host.exceptions.lock().unwrap(),
        [Some("spend"), None],
        "the host is told when it appears and when it ends: {lines:#?}"
    );
    assert!(
        lines_of("escalated").is_empty(),
        "nothing is raised for a Later exception: {records:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An exception stands while its wall is out of view: once the person scrolls the wall away,
/// a screen busy on background work alone is no answer to it; a spinner is. Negative control:
/// the live turn clears it.
#[test]
fn an_exception_stands_while_its_wall_is_out_of_view_and_ends_when_the_agent_works() {
    const WALL: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    let spend: Vec<String> = aterm_phase::prompt::fixtures::screen(WALL)
        .into_iter()
        .map(|r| {
            r.replace(
                "You've hit your weekly limit · resets 4pm (America/Los_Angeles)",
                "You're out of extra usage · add funds to continue",
            )
        })
        .collect();
    let from = spend
        .iter()
        .position(|r| r.starts_with("⏺ Dynamic workflow"))
        .unwrap();
    let to = spend
        .iter()
        .position(|r| r.starts_with("⏺ Goal paused"))
        .unwrap();
    let mut scrolled = spend.clone();
    scrolled.drain(from..=to);
    let mut live = spend.clone();
    let foot = live
        .iter()
        .position(|r| r.contains("bypass permissions"))
        .unwrap();
    live[foot] = "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell".to_string();
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(TestHost::default());
        let (dir, path) = journal_file("exception-hidden");
        let mut m = Mock::new(screens);
        m.vanish_after = Some(24);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &resuming(&opts, None), |s| {
            s.set_clock(AFTER_RESET, PDT);
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let cleared = records
            .iter()
            .filter(|r| r.kind == "cleared")
            .map(|r| r.line.clone())
            .collect::<Vec<_>>();
        let told = host.exceptions.lock().unwrap().clone();
        (told, cleared, lines)
    };
    let hidden = |tail: &Vec<String>| {
        let mut all = vec![busy_screen(), spend.clone()];
        all.extend(std::iter::repeat_n(scrolled.clone(), 6));
        all.extend(std::iter::repeat_n(tail.clone(), 6));
        all
    };
    let (told, cleared, lines) = run(hidden(&scrolled));
    assert_eq!(told.first(), Some(&Some("spend")), "{lines:#?}");
    assert!(
        cleared.iter().all(|l| l.contains("the loop ended")),
        "nothing but the loop's end clears it while the wall is out of view: {cleared:#?}"
    );
    // Negative control: the agent works again under the wall.
    let (told, cleared, lines) = run(hidden(&live));
    assert_eq!(told, [Some("spend"), None], "{lines:#?}");
    assert!(
        cleared
            .iter()
            .any(|l| l.contains(" exception=spend after=") && l.ends_with("the agent working")),
        "{cleared:#?}"
    );
}

/// The host takes no step at the login wall: a `continue` answered by `⏺ Login expired ·
/// Please run /login` is no idle point, so the upgrade's notice is never typed into it.
/// Negative control: once `/login` succeeds (`⎿  Login successful`) the host steps there.
#[test]
fn the_host_takes_no_step_at_the_login_wall_and_steps_once_it_is_back() {
    use aterm_phase::prompt::fixtures::{LOGIN_EXPIRED, screen};
    let walled = || {
        let mut r = screen(LOGIN_EXPIRED);
        let at = r
            .iter()
            .position(|row| row.starts_with("⏺ Login expired"))
            .expect("the wall row");
        r.splice(
            at + 1..at + 1,
            rows(&["", "✻ Worked for 3m 2s · done 5:00 AM"]),
        );
        r
    };
    let back = || {
        let mut r = screen(LOGIN_EXPIRED);
        let at = r
            .iter()
            .position(|row| row.starts_with("⏺ Login expired"))
            .expect("the wall row");
        r.splice(
            at + 1..at + 1,
            rows(&["", "❯ /login", "  ⎿  Login successful"]),
        );
        r
    };
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let (dir, journal) = journal_file("login-wall-host");
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        // The cursor in the prompt box, where Claude Code keeps it at an idle point (the
        // server's idle is the box that holds the cursor).
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let _ = watch_lines(&mut m, &opts);
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (host.steps.load(Ordering::SeqCst), lines)
    };
    let (steps, lines) = run(vec![busy_screen(), walled(), walled()]);
    assert_eq!(steps, 0, "no step at the wall: {lines:#?}");
    assert!(!lines.iter().any(|l| l.starts_with("HOST")), "{lines:#?}");
    let (steps, lines) = run(vec![busy_screen(), back()]);
    assert_eq!(steps, 1, "the login back is the host's point: {lines:#?}");
}

/// The loop tells a host that asked for a point which guard withheld it
/// ([`IdleHost::withheld`]), once while the guard stands; the weekly notice at a break is a
/// wall. Negative controls: the bare break is offered; a host asking nothing is told nothing.
#[test]
fn the_loop_says_which_guard_withheld_the_point_the_host_asked_for() {
    let bg = |wall: bool| {
        let mut r = rows(&["⏺ Waiting for the build."]);
        if wall {
            r.push(WEEKLY.to_string());
        }
        r.extend(rows(&[
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]));
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    let run = |screens: Vec<Vec<String>>, settle: Duration, wants: bool| {
        let host = Arc::new(TestHost::default());
        host.wants.store(wants, Ordering::SeqCst);
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| s.set_background_settle(settle));
        let withheld = host.withheld.lock().unwrap().clone();
        (
            withheld,
            host.backgrounds.load(Ordering::SeqCst),
            host.steps.load(Ordering::SeqCst),
        )
    };
    use Guard::{NoBackgroundWait, NotIdle, Settle};
    // The weekly notice at a break of the agent's own work reads as a limited turn end, not
    // a busy break (`NotIdle`), said once over every read; no break offered.
    assert_eq!(
        run(vec![bg(true)], Duration::ZERO, true),
        (vec![NotIdle], 0, 0)
    );
    // Control: the same break with no notice is offered, nothing withheld, and a break is no
    // idle point (`at_idle` never asked there).
    let (withheld, offered, idles) = run(vec![bg(false)], Duration::ZERO, true);
    assert!(
        withheld.is_empty() && offered >= 1 && idles == 0,
        "{withheld:?} {offered} {idles}"
    );
    // A break that has not stood its settle.
    assert_eq!(
        run(vec![bg(false)], Duration::from_secs(3_600), true),
        (vec![Settle], 0, 0)
    );
    // A running turn (a spinner): more than the agent's background work.
    assert_eq!(
        run(vec![busy_screen()], Duration::ZERO, true),
        (vec![NoBackgroundWait], 0, 0)
    );
    // The turn ends at an idle point: the host's step, and no guard after.
    assert_eq!(
        run(vec![busy_screen(), idle_screen()], Duration::ZERO, true),
        (vec![NoBackgroundWait], 0, 1)
    );
    // A box, and the weekly limit at a turn end: no idle point.
    for screen in [bash_one_row(), weekly_limited()] {
        assert_eq!(
            run(vec![busy_screen(), screen], Duration::ZERO, true),
            (vec![NoBackgroundWait, NotIdle], 0, 0)
        );
    }
    // A host that asks for no point is told nothing.
    assert_eq!(run(vec![bg(true)], Duration::ZERO, false), (vec![], 0, 0));
}

/// A usage limit's notice left on the screen a grace past its reset is an idle point (Claude
/// Code leaves it up until something is typed): offered by the episode's own reset, or by the
/// transcript's ([`IdleHost::limit_until`]) where it names none. Negative controls
/// (`NotIdle`): a minute short of the grace, no reset anywhere, a live limit naming next
/// week's. Also a shell running under the notice. (The episode closing on the model's answer:
/// [`a_limit_episode_is_told_to_the_host_and_waited_out_over_its_turn_ends`].)
#[test]
fn a_limit_notice_left_past_its_reset_and_its_grace_is_an_idle_point() {
    // The weekly notice's reset (`WEEKLY`), Sep 19 at 11am PDT, and its grace.
    let reset = AFTER_RESET - 30;
    let grace = i64::try_from(crate::harness::upgrade_drive::LIMIT_GRACE_S).expect("grace");
    let past = reset + grace + 30;
    let short = reset + grace - 60;
    let unnamed = "  ⎿  You've hit your weekly limit";
    let next_week =
        "  ⎿  You've hit your weekly limit · resets Sep 26 at 11am (America/Los_Angeles)";
    let notice = |row: &str| {
        let mut r = rows(&["⏺ Running the A/B on m7 now.", row, ""]);
        r.extend(rows(&["✻ Worked for 3m 2s · done 4:51 PM", ""]));
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let at_break = |row: &str| {
        let mut r = rows(&["⏺ Waiting for the build.", row, ""]);
        r.extend(rows(&[
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]));
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    assert!(
        matches!(
            aterm_phase::worker_phase(&notice(unnamed)),
            Phase::Limited { reset: None, .. }
        ),
        "a notice naming no reset"
    );
    struct Ran {
        withheld: Vec<Guard>,
        steps: usize,
        breaks: usize,
        asks: usize,
        lines: Vec<String>,
    }
    let run = |screens: Vec<Vec<String>>, now: i64, named: Option<i64>| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        *host.limit_until.lock().unwrap() = named.map(|t| u64::try_from(t).expect("unix"));
        let (jdir, journal) = journal_file("limit-past-grace");
        let mut m = Mock::new(screens);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(now, PDT);
            s.set_background_settle(Duration::ZERO);
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        Ran {
            withheld: host.withheld.lock().unwrap().clone(),
            steps: host.steps.load(Ordering::SeqCst),
            breaks: host.backgrounds.load(Ordering::SeqCst),
            asks: host.limit_asks.load(Ordering::SeqCst),
            lines,
        }
    };
    use Guard::{NoBackgroundWait, NotIdle};
    // Past the grace: the episode opens, and the notice is the point offered.
    let r = run(vec![busy_screen(), weekly_limited()], past, None);
    assert_eq!(
        (r.withheld, r.steps),
        (vec![NoBackgroundWait], 1),
        "{:#?}",
        r.lines
    );
    assert!(
        r.lines.iter().any(|l| l.starts_with("EVENT limited")),
        "the episode opens as ever: {:#?}",
        r.lines
    );
    assert!(
        r.lines
            .iter()
            .any(|l| l.starts_with("HOST seq=") && l.ends_with(" upgrade step=announced:1")),
        "{:#?}",
        r.lines
    );
    assert_eq!(
        r.asks, 0,
        "the episode's own reset: the transcript is not asked"
    );
    // No reset on the screen: the transcript's names the grace past.
    let r = run(vec![busy_screen(), notice(unnamed)], past, Some(reset));
    assert_eq!(
        (r.withheld, r.steps),
        (vec![NoBackgroundWait], 1),
        "{:#?}",
        r.lines
    );
    assert!(r.asks >= 1, "the transcript asked");
    // Negative control: a minute short of the grace, withheld.
    let r = run(vec![busy_screen(), weekly_limited()], short, None);
    assert_eq!(
        (r.withheld, r.steps),
        (vec![NoBackgroundWait, NotIdle], 0),
        "{:#?}",
        r.lines
    );
    // Negative control: no reset read anywhere, never guessed.
    let r = run(vec![busy_screen(), notice(unnamed)], past, None);
    assert_eq!(
        (r.withheld, r.steps),
        (vec![NoBackgroundWait, NotIdle], 0),
        "{:#?}",
        r.lines
    );
    assert!(r.asks >= 1, "the transcript asked, and it names none");
    // Negative control: a live limit; the notice names next week's reset,
    // and the transcript's old one, long past, is not what the loop reads.
    let r = run(vec![busy_screen(), notice(next_week)], past, Some(reset));
    assert_eq!(
        (r.withheld, r.steps),
        (vec![NoBackgroundWait, NotIdle], 0),
        "{:#?}",
        r.lines
    );
    assert_eq!(r.asks, 0, "the episode's own reset stands");
    // A shell still running under the notice: the reader sees the wall through it, so it is
    // withheld short of the grace (`NotIdle`, no break offered) and offered past it.
    let r = run(vec![at_break(unnamed)], short, Some(reset));
    assert_eq!(
        (r.withheld, r.steps, r.breaks),
        (vec![NotIdle], 0, 0),
        "{:#?}",
        r.lines
    );
    let r = run(vec![at_break(unnamed)], past, Some(reset));
    assert!(
        r.withheld.is_empty() && r.steps == 1 && r.breaks == 0,
        "{:#?}",
        r.lines
    );
}

/// A LIMIT ONLY THE TRANSCRIPT SHOWS WITHHOLDS THE HOST'S STEP (LIM-1): the
/// real loop over scripted screens — a busy read, then an idle point that
/// draws no wall (a relaunched conversation's rows) — under a host that
/// wants a point (a relaunch's carry-on owed) and whose agent's transcript
/// stands on a weekly limit, its reset three hours ahead. The decision
/// reads the row as the wall: the point is the limit's, its episode opens
/// (`EVENT limited`), the host is told the session is limited, and the step
/// is withheld (`NotIdle` at the limit's point), never taken. NEGATIVE
/// CONTROL: a transcript that stands on no limit (or one the host moved the
/// conversation off) — the step is taken at that point.
#[test]
fn a_limit_only_the_transcript_shows_withholds_the_hosts_step() {
    let now = TEST_NOW;
    let run = |standing: bool| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        *host.standing.lock().unwrap() = standing.then(|| crate::harness::upgrade::StandingLimit {
            text: "You've hit your weekly limit · resets Sep 20 at 6pm (America/Los_Angeles)"
                .to_string(),
            until: u64::try_from(now + 3 * 3600).ok(),
            window: Some("seven_day".to_string()),
            at: u64::try_from(now - 3600).ok(),
            row: None,
        });
        let (jdir, journal) = journal_file("standing-limit");
        let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(now, PDT);
            s.set_background_settle(Duration::ZERO);
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let limited = records.iter().any(|r| r.line.starts_with("EVENT limited"));
        let withheld = host.withheld.lock().unwrap().clone();
        let steps = host.steps.load(Ordering::SeqCst);
        let told = host.limits.lock().unwrap().clone();
        (withheld, steps, limited, told)
    };
    let (withheld, steps, limited, told) = run(true);
    // The point the decision read as the limit's (`NotIdle`), or the
    // episode it opened (`LimitEpisode`).
    assert!(
        withheld.contains(&Guard::NotIdle) || withheld.contains(&Guard::LimitEpisode),
        "{withheld:?}"
    );
    assert_eq!(steps, 0, "the carry-on is not typed into the limit");
    assert!(limited, "the episode opens on the transcript's limit");
    assert_eq!(told.first(), Some(&true), "{told:?}");
    // NEGATIVE CONTROL: no limit stands.
    let (withheld, steps, limited, _) = run(false);
    assert!(!withheld.contains(&Guard::LimitEpisode), "{withheld:?}");
    assert_eq!(steps, 1);
    assert!(!limited);
}

/// A live limit that names no reset (`API Error: Rate limit reached for requests`) is never
/// spent by the weekly notice's reset its episode opened on: withheld (`NotIdle`). The same
/// weekly notice again after the retry is still spent.
#[test]
fn a_live_limit_naming_no_reset_is_never_spent_by_an_earlier_notices_reset() {
    let reset = AFTER_RESET - 30;
    let grace = i64::try_from(crate::harness::upgrade_drive::LIMIT_GRACE_S).expect("grace");
    let past = reset + grace + 30;
    let rate_limited = {
        let mut r = rows(&[
            "⏺ Running the A/B on m7 now.",
            "  ⎿  API Error: Rate limit reached for requests",
            "",
            "✻ Worked for 2s · done 4:58 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    assert!(
        matches!(
            aterm_phase::worker_phase(&rate_limited),
            Phase::Limited { reset: None, .. }
        ),
        "a live rate limit, naming no reset"
    );
    let run = |last: Vec<String>| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.again.store(1, Ordering::SeqCst);
        let mut m = Mock::new(vec![busy_screen(), weekly_limited(), busy_screen(), last]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(past, PDT);
            s.set_background_settle(Duration::ZERO);
        });
        let steps = host.steps.load(Ordering::SeqCst);
        let withheld = host.withheld.lock().unwrap().clone();
        (
            steps,
            withheld,
            host.limit_asks.load(Ordering::SeqCst),
            lines,
        )
    };
    let (steps, withheld, asks, lines) = run(rate_limited);
    assert_eq!(steps, 1, "the weekly notice alone: {lines:#?}");
    assert_eq!(
        withheld.last(),
        Some(&Guard::NotIdle),
        "the live wall withheld: {withheld:?} {lines:#?}"
    );
    assert!(asks >= 1, "the transcript asked, and it names none");
    // The same notice again after the retry: the episode's own reset.
    let (steps, _, _, lines) = run(weekly_retried());
    assert_eq!(steps, 2, "the same spent notice, offered: {lines:#?}");
}

/// A bare clock time (`resets 3pm`) is placed on the day the transcript names
/// ([`IdleHost::limit_until`]): printed yesterday and read this morning on a redrawn screen,
/// it is offered, not read as today's 3 pm (withheld, the budget stretched). Negative
/// controls: a transcript naming none, another time, or a day after. A drawn done row (`done
/// 1:02 PM`) places it by itself ([`Session::reset_unix_for`]).
#[test]
fn a_bare_clock_time_is_placed_on_the_day_the_transcript_names() {
    // Today's 3 pm PDT, six hours and five minutes after the clock.
    let today = TEST_NOW + 6 * 3_600 + 5 * 60;
    let yesterday = today - 86_400;
    // `printed`: the done row that says when the notice was drawn.
    let notice = |printed: bool| {
        let mut r = rows(&[
            "⏺ Running the A/B on m7 now.",
            "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
            "",
        ]);
        if printed {
            r.extend(rows(&["✻ Worked for 3m 2s · done 1:02 PM", ""]));
        }
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    for printed in [false, true] {
        assert!(
            matches!(
                aterm_phase::worker_phase(&notice(printed)),
                Phase::Limited { reset: Some(r), .. } if r == "3pm (America/Los_Angeles)"
            ),
            "a notice naming a clock time and no day (done row: {printed})"
        );
    }
    // The journal's lines (where alone the host's step is noted), then the printed ones.
    let run = |named: Option<i64>, printed: bool| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        *host.limit_until.lock().unwrap() = named.map(|t| u64::try_from(t).expect("unix"));
        let (jdir, journal) = journal_file("bare-clock-day");
        let mut m = Mock::new(vec![busy_screen(), notice(printed)]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_background_settle(Duration::ZERO);
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let noted: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let steps = host.steps.load(Ordering::SeqCst);
        let withheld = host.withheld.lock().unwrap().clone();
        (steps, withheld, noted, lines)
    };
    let (steps, withheld, noted, lines) = run(Some(yesterday), false);
    assert_eq!(steps, 1, "offered: {withheld:?} {noted:#?}");
    assert!(
        noted.iter().any(|l| l.starts_with("HOST seq=")),
        "{noted:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("EXTEND ")),
        "no budget stretched to today's 3 pm: {lines:#?}"
    );
    for (why, named) in [
        ("none named", None),
        ("another time", Some(yesterday - 3_600)),
        ("a day after", Some(today + 86_400)),
    ] {
        let (steps, withheld, noted, lines) = run(named, false);
        assert_eq!(steps, 0, "{why}: {noted:#?}");
        assert_eq!(
            withheld.last(),
            Some(&Guard::NotIdle),
            "{why}: {withheld:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("EXTEND until=2026-09-17T22:10:00Z ")),
            "{why}: today's 3 pm, the loop's own reading: {lines:#?}"
        );
    }
    // The done row drawn: its 1:02 PM print puts 3 pm on yesterday, whatever the transcript
    // names.
    for (why, named) in [
        ("none named", None),
        ("the day before", Some(yesterday)),
        ("a day after", Some(today + 86_400)),
    ] {
        let (steps, withheld, noted, lines) = run(named, true);
        assert_eq!(steps, 1, "{why}: offered: {withheld:?} {noted:#?}");
        assert!(
            !lines.iter().any(|l| l.starts_with("EXTEND ")),
            "{why}: no budget stretched to today's 3 pm: {lines:#?}"
        );
    }
}

/// A break after the loop's own act is offered to the host once a live turn shows the agent
/// took the act; otherwise an agent that orchestrates all day never gives the upgrade a look.
/// Negative control: a break straight after the act is not. Tier-1 for `SupervisorBreakOffer`:
/// the loop offers exactly where `Offer` is enabled; the `Judged` and `Blind` dials are caught.
#[test]
fn a_break_after_the_loops_own_taken_act_is_offered_to_the_host() {
    let ended = || {
        let mut r = rows(&[
            "⏺ Fixed the parser; the suite is green.",
            "",
            "✻ Worked for 3m 2s · done 4:24 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let bg = || {
        let mut r = rows(&[
            "⏺ Waiting for the build.",
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        // Deaf at the first point, where the loop's own act goes.
        host.deaf_for.store(1, Ordering::SeqCst);
        let (jdir, journal) = journal_file("taken-act-break");
        let mut m = Mock::new(screens);
        m.turn_releases = Some(1);
        m.vanish_after = Some(3);
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                answer_questions: false,
                ..SupervisorConfig::default()
            },
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| s.set_background_settle(Duration::ZERO));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("CONTINUED ") && l.contains("keep going")),
            "the loop's own act: {lines:#?}"
        );
        let withheld = host.withheld.lock().unwrap().clone();
        (host.backgrounds.load(Ordering::SeqCst), lines, withheld)
    };
    let model = aterm_models_harness::supervisor_break_offer_model();
    let judged = aterm_spec::interp::with_consts(&model, &[("Judged", 1)]);
    let blind = aterm_spec::interp::with_consts(&model, &[("Blind", 1)]);
    // (the screens after the loop's act, the model's path, why)
    for (after, path, why) in [
        (
            vec![busy_screen(), bg()],
            &["LoopActs", "LiveTurn", "BreakSettles"][..],
            "taken: a live turn, then its break",
        ),
        (
            vec![bg()],
            &["LoopActs", "BreakSettles"][..],
            "not taken: the break straight after the act",
        ),
    ] {
        let mut screens = vec![busy_screen(), ended()];
        screens.extend(after);
        let (offered, lines, withheld) = run(screens);
        let walk = |m: &aterm_spec::derive::Model| {
            let mut st = m.init_state();
            for action in path {
                assert!(m.fire(action, &mut st), "{why}: {action} at {st:?}");
            }
            st
        };
        let st = walk(&model);
        assert_eq!(
            offered > 0,
            model.action_enabled("Offer", &st),
            "{why}: the real loop offered {offered} at {st:?}: {lines:#?}"
        );
        if offered > 0 {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with("HOST seq=") && l.contains(" background ")),
                "{why}: journaled: {lines:#?}"
            );
            // Negative control: the judged-point guard withholds it.
            assert!(!judged.action_enabled("Offer", &walk(&judged)), "{why}");
            assert!(
                !withheld.contains(&Guard::ActUntaken),
                "{why}: {withheld:?}"
            );
        } else {
            // The host is told which guard withheld the break.
            assert!(withheld.contains(&Guard::ActUntaken), "{why}: {withheld:?}");
            // Negative control: a guard that asks nothing stacks on it.
            assert!(blind.action_enabled("Offer", &walk(&blind)), "{why}");
        }
    }
}

/// When [`hosted_with_host_ledger`] stops the loop it runs. A condition, not a
/// fixed window: mock round trips with stalls and back-offs stretch on a
/// loaded machine.
enum Stop {
    /// After a fixed window: only for arms that assert something did not happen, where a
    /// longer run can only find more.
    After(Duration),
    /// Once the host shows `reached`, then `tail` more for the steps it cannot
    /// see (a journal line, a typed turn). Bounded at 60 s, so a loop that
    /// never gets there still ends and fails on what it did.
    Reached(Box<dyn Fn(&TestHost) -> bool + Send>, Duration),
}

impl Stop {
    fn reached(reached: impl Fn(&TestHost) -> bool + Send + 'static, tail: Duration) -> Self {
        Self::Reached(Box::new(reached), tail)
    }
}

/// Whether the journal at `path` holds the usage rule's continuation (`CONTINUED …
/// rule=usage-resume@v1`), for an arm's [`Stop::reached`].
fn continued_on_usage(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|j| {
        j.lines()
            .any(|l| l.contains("CONTINUED seq=") && l.contains("rule=usage-resume@v1"))
    })
}

/// `run_hosted` under `opts` with `host`, stopped at `stop`: the outcome and
/// the lines.
fn hosted_with_host(
    m: &mut Mock,
    opts: &SuperviseOpts,
    host: &Arc<TestHost>,
    stop: Stop,
    timing: Option<TurnEndTiming>,
) -> (Result<(), String>, Vec<String>) {
    let (ldir, ledger) = ledger_file("idle-host");
    let out = hosted_with_host_ledger(m, opts, host, stop, timing, &ledger);
    let _ = std::fs::remove_dir_all(&ldir);
    out
}

/// [`hosted_with_host`] over the approval ledger at `ledger`.
fn hosted_with_host_ledger(
    m: &mut Mock,
    opts: &SuperviseOpts,
    host: &Arc<TestHost>,
    stop_at: Stop,
    timing: Option<TurnEndTiming>,
    ledger: &std::path::Path,
) -> (Result<(), String>, Vec<String>) {
    hosted_with_host_at(m, opts, host, stop_at, timing, ledger, None)
}

/// [`hosted_with_host`] with the loop's wall clock set to `clock` (unix seconds, PDT,
/// [`test_zones`]) and running from it: a limit's reset is read against it and waited for in
/// real time.
fn hosted_with_host_clocked(
    m: &mut Mock,
    opts: &SuperviseOpts,
    host: &Arc<TestHost>,
    stop: Stop,
    timing: Option<TurnEndTiming>,
    clock: i64,
) -> (Result<(), String>, Vec<String>) {
    let (ldir, ledger) = ledger_file("idle-host-clocked");
    let out = hosted_with_host_at(m, opts, host, stop, timing, &ledger, Some(clock));
    let _ = std::fs::remove_dir_all(&ldir);
    out
}

/// [`hosted_with_host_ledger`], the clock set when `clock` is `Some`.
fn hosted_with_host_at(
    m: &mut Mock,
    opts: &SuperviseOpts,
    host: &Arc<TestHost>,
    stop_at: Stop,
    timing: Option<TurnEndTiming>,
    ledger: &std::path::Path,
    clock: Option<i64>,
) -> (Result<(), String>, Vec<String>) {
    let stop = Arc::new(AtomicBool::new(false));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..opts.clone()
    };
    let halt = Arc::clone(&stop);
    let watched = Arc::clone(host);
    let setter = std::thread::spawn(move || {
        match stop_at {
            Stop::After(window) => std::thread::sleep(window),
            Stop::Reached(reached, tail) => {
                let backstop = Instant::now() + Duration::from_secs(60);
                while !reached(&watched) && Instant::now() < backstop {
                    std::thread::sleep(Duration::from_millis(1));
                }
                std::thread::sleep(tail);
            }
        }
        halt.store(true, Ordering::SeqCst);
    });
    let mut out: Vec<u8> = Vec::new();
    // Recorded, as every whole run here ([`taped_session`]), and replayed
    // from the tape alone (below): the host's every answer and notification
    // crossed its door, and the stop the setter's thread raised at a moment
    // of its own is the loop's own read of it — so the replay asks no host,
    // waits on no thread, and must reproduce the run.
    let from = m.order.len();
    let (tape, mut taped) = memory_tape(m);
    let mut s = session(&mut taped, Some("@s-1".to_string()));
    s.set_fx(tape.fx.clone());
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    if let Some(t) = timing {
        s.set_turn_end_timing(t);
    }
    if let Some(now) = clock {
        s.set_clock(now, PDT);
        s.set_clock_running();
        s.set_zone_lookup(test_zones);
    }
    let header = hosted_header(&s, &opts);
    tape.core().reroot(header.roots.clone());
    let r = s.run_hosted(&opts, stop, &mut out);
    setter.join().expect("setter");
    drop(s);
    drop(taped);
    tape.finish(&crate::supervise::tape::replay::hosted_end(&r));
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    crate::supervise::tape::testing::assert_replays(&tape, &header, &sent(m, from), &lines);
    (r, lines)
}

/// Claude Code's critical-memory banner at an idle point is the host's restart
/// ([`IdleHost::restart`]; a `wait:held` tried again on the back-off), nothing typed or
/// escalated. Negative controls: no restart, or a refused one, hands the point to a person.
#[test]
fn a_memory_banner_is_the_hosts_restart_and_escalated_only_when_it_cannot_be() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    let run = |answers: Vec<Option<&'static str>>| {
        let host = Arc::new(TestHost::default());
        let asked = answers.len();
        *host.restarts.lock().unwrap() = answers.into();
        let (jdir, journal) = journal_file("memory-restart");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), screen(MEMORY_BANNER_IDLE)]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let (r, _) = hosted_with_host(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                move |h| h.restarted.lock().unwrap().len() >= asked,
                Duration::from_millis(300),
            ),
            Some(TurnEndTiming {
                restart_backoff: vec![Duration::from_millis(30)],
                ..TurnEndTiming::default()
            }),
        );
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let restarted = host.restarted.lock().unwrap().clone();
        (m, lines, restarted)
    };
    let escalated = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.contains("meta set attention owner=supervisor"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let typed = |m: &Mock| count(m, "@s-1 turn") + count(m, "@s-1 send") + count(m, "@s-1 key");

    // The host restarts it: once it waited, then made.
    let (m, lines, restarted) = run(vec![Some("wait:held"), Some("adopted")]);
    assert_eq!(restarted, ["memory", "memory"], "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("HOST seq=") && l.ends_with("restart:memory step=wait:held")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("restart:memory waits: wait:held")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("HOST seq=") && l.ends_with("restart:memory step=adopted")),
        "{lines:#?}"
    );
    assert!(escalated(&m).is_empty(), "{:#?}", m.requests);
    assert_eq!(typed(&m), 0, "nothing typed: {:#?}", m.requests);

    // Negative controls: no restart here, and one that can never be made.
    for (answers, why) in [
        (vec![None], "memory critical: restart it"),
        (
            vec![Some("refused:line")],
            "the restart could not be made (refused:line)",
        ),
    ] {
        let (m, _, _) = run(answers);
        let set = escalated(&m);
        assert!(
            set.iter().any(|r| r.contains(why)),
            "{why}: {:#?}",
            m.requests
        );
        assert_eq!(typed(&m), 0, "{:#?}", m.requests);
    }
}

/// The memory ceiling in the loop warns first, the host measuring the agent past `[harness]
/// memory_ceiling` with no banner on the screen:
///
/// * With memory to spare (a 46 GiB ceiling), the 215 GiB runaway is a warning said once
///   (`MEMORY seq=<n> …`, [`IdleHost::warn`]), `a warning only: nothing is restarted`, and
///   the point continued.
/// * In trouble (a 17 GiB ceiling) with this agent the largest, it is the banner's wall: the
///   host's restart (`RESTARTED … the agent, for its memory: …`), nothing typed or escalated.
///
/// Negative controls: the ceiling `off` in trouble: continued, nothing said; in trouble with
/// no host restart, the point is the person's. (The verdict's other inputs, a healthy agent,
/// a larger one beside it and no measure, are `memory.rs`'s.)
#[test]
fn a_measure_past_the_ceiling_warns_and_restarts_only_when_the_mac_is_in_trouble() {
    use crate::supervise::memory::MemoryCeiling;
    struct Run {
        m: Mock,
        lines: Vec<String>,
        restarted: Vec<&'static str>,
        warned: Vec<String>,
    }
    let run = |ceiling: MemoryCeiling, trouble: bool, answers: Vec<Option<&'static str>>| {
        let host = Arc::new(TestHost::default());
        host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
        host.trouble.store(trouble, Ordering::SeqCst);
        let restarts = !answers.is_empty() && trouble;
        if restarts {
            // The restarted agent is a fresh, small one, so the episode ends (else the spent
            // test host reads as one that restarts no more).
            host.memory_at_restart.store(HEALTHY_MIB, Ordering::SeqCst);
        }
        *host.restarts.lock().unwrap() = answers.into();
        let (jdir, journal) = journal_file("memory-ceiling");
        let mut opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        opts.policy.memory_ceiling = ceiling;
        let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let stop = if restarts {
            Stop::reached(
                |h| !h.restarted.lock().unwrap().is_empty(),
                Duration::from_millis(300),
            )
        } else {
            Stop::reached(
                |h| h.turn_ends.load(Ordering::SeqCst) >= 1,
                Duration::from_millis(500),
            )
        };
        // Any turn is long enough to be continued: the controls' point is
        // an ordinary one, not a short turn's back-off.
        let timing = TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        };
        let (r, _) = hosted_with_host(&mut m, &opts, &host, stop, Some(timing));
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        Run {
            m,
            lines: records.into_iter().map(|r| r.line).collect(),
            restarted: host.restarted.lock().unwrap().clone(),
            warned: host.warned.lock().unwrap().clone(),
        }
    };
    let escalated = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.contains("meta set attention owner=supervisor"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let typed = |m: &Mock| count(m, "@s-1 turn") + count(m, "@s-1 send") + count(m, "@s-1 key");
    let memory_lines = |r: &Run| {
        r.lines
            .iter()
            .filter(|l| l.starts_with("MEMORY "))
            .cloned()
            .collect::<Vec<_>>()
    };
    let continued = |r: &Run| {
        r.lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("rule=continue@v1"))
    };
    let adopted = || vec![Some("adopted")];

    // Memory to spare: a warning, and the point continued.
    let r = run(MemoryCeiling::Auto, false, adopted());
    assert!(r.restarted.is_empty(), "{:#?}", r.lines);
    assert_eq!(r.warned.len(), 1, "{:?}", r.warned);
    assert!(
        r.warned[0].contains("a warning only: nothing is restarted"),
        "{:?}",
        r.warned
    );
    assert_eq!(memory_lines(&r).len(), 1, "said once: {:#?}", r.lines);
    assert!(continued(&r), "continued as any point: {:#?}", r.lines);
    assert!(escalated(&r.m).is_empty(), "{:#?}", r.m.requests);

    // In trouble, the largest: the restart at the idle point.
    let r = run(MemoryCeiling::Auto, true, adopted());
    assert_eq!(r.restarted, ["memory"], "{:#?}", r.lines);
    assert!(
        r.lines
            .iter()
            .any(|l| l.starts_with("HOST seq=") && l.ends_with("restart:memory step=adopted")),
        "{:#?}",
        r.lines
    );
    assert!(
        r.lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains("rule=memory-restart@v1")
            && l.contains("the agent, for its memory: Memory past aterm's ceiling")),
        "{:#?}",
        r.lines
    );
    let told = memory_lines(&r);
    assert_eq!(told.len(), 1, "said once: {:#?}", r.lines);
    assert!(
        told[0].ends_with("aterm restarts it on its conversation at its next idle point"),
        "{told:?}"
    );
    assert_eq!(r.warned.len(), 1, "{:?}", r.warned);
    assert!(escalated(&r.m).is_empty(), "{:#?}", r.m.requests);
    assert_eq!(typed(&r.m), 0, "nothing typed: {:#?}", r.m.requests);

    // Negative control: the ceiling off, in trouble: the point is continued
    // and nothing about memory is said.
    let r = run(MemoryCeiling::Off, true, Vec::new());
    assert!(r.restarted.is_empty(), "{:#?}", r.lines);
    assert!(memory_lines(&r).is_empty(), "{:#?}", r.lines);
    assert!(r.warned.is_empty(), "{:?}", r.warned);
    assert!(escalated(&r.m).is_empty(), "{:#?}", r.m.requests);
    assert!(continued(&r), "continued as any point: {:#?}", r.lines);

    // Negative control: in trouble, no host restart: the person's.
    let r = run(MemoryCeiling::Auto, true, Vec::new());
    assert!(r.restarted.is_empty(), "{:#?}", r.lines);
    let set = escalated(&r.m);
    assert!(
        set.iter()
            .any(|e| e.contains("memory critical: restart it") && e.contains("Memory past aterm")),
        "{:#?}",
        r.m.requests
    );
    assert_eq!(typed(&r.m), 0, "{:#?}", r.m.requests);
    assert_eq!(r.warned.len(), 1, "{:?}", r.warned);
    assert!(r.warned[0].contains(PERSONS_ROW), "{:?}", r.warned);
}

/// The Mac's trouble coming to a standing idle point (an idle agent warned about with memory
/// to spare) is acted on there, whether found while the loop waits or by the point's own
/// reading: the host's restart, the row standing at the ask naming aterm's restart. (Memory
/// to spare throughout is a warning only:
/// `a_measure_past_the_ceiling_warns_and_restarts_only_when_the_mac_is_in_trouble`.)
#[test]
fn trouble_that_comes_while_idle_decides_the_standing_point_again() {
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Trouble {
        /// The Mac falls into trouble while the loop waits at the point.
        WhileWaiting,
        /// The Mac falls into trouble between the turn's busy read (the
        /// host's first measure) and the point's own reading (its second).
        AtThePoint,
    }
    let run = |trouble: Trouble| {
        let host = Arc::new(TestHost::default());
        host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
        host.taskless.store(true, Ordering::SeqCst);
        if trouble == Trouble::AtThePoint {
            host.trouble_from_ask.store(2, Ordering::SeqCst);
        }
        *host.restarts.lock().unwrap() = vec![Some("adopted")].into();
        let (jdir, journal) = journal_file("memory-trouble-comes");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let stop = match trouble {
            Trouble::WhileWaiting => Stop::reached(
                |h| {
                    // The point read (a warning only), and a step waited
                    // past it: the Mac falls into trouble.
                    if h.turn_ends.load(Ordering::SeqCst) >= 1
                        && h.memory_asks.load(Ordering::SeqCst) >= 3
                    {
                        h.trouble.store(true, Ordering::SeqCst);
                    }
                    !h.restarted.lock().unwrap().is_empty()
                },
                Duration::from_millis(300),
            ),
            Trouble::AtThePoint => Stop::reached(
                |h| !h.restarted.lock().unwrap().is_empty(),
                Duration::from_millis(300),
            ),
        };
        let (r, lines) = hosted_with_host(&mut m, &opts, &host, stop, None);
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let restarted = host.restarted.lock().unwrap().clone();
        let warned = host.warned.lock().unwrap().clone();
        (
            m,
            lines,
            records.into_iter().map(|r| r.line).collect::<Vec<_>>(),
            restarted,
            warned,
        )
    };
    let typed = |m: &Mock| count(m, "@s-1 turn") + count(m, "@s-1 send") + count(m, "@s-1 key");
    let aterms = "it holds the most memory, so aterm restarts it on its conversation at its next \
                  idle point";
    for trouble in [Trouble::WhileWaiting, Trouble::AtThePoint] {
        let (m, lines, journal, restarted, warned) = run(trouble);
        assert_eq!(restarted, ["memory"], "{trouble:?}: {lines:#?}");
        assert_eq!(typed(&m), 0, "{trouble:?}: {:#?}", m.requests);
        // Said again when the machine falls into trouble, the row naming aterm's restart. A
        // third may follow: this host restarts no more once its one answer is spent.
        assert!(
            warned.len() >= 2,
            "{trouble:?}: said again on the new verdict: {warned:#?}"
        );
        assert!(
            warned[0].contains("a warning only: nothing is restarted"),
            "{trouble:?}: {warned:#?}"
        );
        assert!(warned[1].ends_with(aterms), "{trouble:?}: {warned:#?}");
        // …said before the host is asked: the row standing at the restart.
        let asked = journal
            .iter()
            .position(|l| l.starts_with("HOST seq=") && l.contains(" restart:memory step="))
            .unwrap_or_else(|| panic!("{trouble:?}: the host asked: {journal:#?}"));
        let row = journal[..asked]
            .iter()
            .rev()
            .find(|l| l.starts_with("MEMORY "))
            .unwrap_or_else(|| panic!("{trouble:?}: a row before the ask: {journal:#?}"));
        assert!(
            row.ends_with(aterms),
            "{trouble:?}: the row standing at the restart: {row}\n{journal:#?}"
        );
    }
}

/// A lost login's end of turn, idle to the eye (`Not logged in`).
fn login_lost_screen() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Pushing the branch.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}

/// The ceiling behind a wall (a session limit hours off, a lost login, a 529): an agent the
/// Mac's trouble is acted on for is restarted there as at a plain idle point.
#[test]
fn a_measure_acted_on_restarts_an_agent_idle_behind_a_wall() {
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let run = |wall: Vec<String>| {
        let host = Arc::new(TestHost::default());
        host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
        host.trouble.store(true, Ordering::SeqCst);
        *host.restarts.lock().unwrap() = vec![Some("adopted")].into();
        let (jdir, journal) = journal_file("memory-wall");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), wall]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let stop = Stop::reached(
            |h| !h.restarted.lock().unwrap().is_empty(),
            Duration::from_millis(300),
        );
        let (r, _) = hosted_with_host(&mut m, &opts, &host, stop, None);
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let restarted = host.restarted.lock().unwrap().clone();
        (lines, restarted)
    };
    for (wall, why) in [
        (limited_hours_off(), "session limit"),
        (login_lost_screen(), "lost login"),
        (end_529, "529 overloaded"),
    ] {
        let (lines, restarted) = run(wall);
        assert_eq!(restarted, ["memory"], "{why}: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("RESTARTED seq=") && l.contains("rule=memory-restart@v1")),
            "{why}: {lines:#?}"
        );
    }
}

/// A session limit whose reset is hours off whatever the wall clock reads (`limited_screen`'s
/// `resets 7:30pm` would read as passed in the hour after 7:30pm local).
fn limited_hours_off() -> Vec<String> {
    limited_screen()
        .into_iter()
        .map(|r| r.replace("resets 7:30pm (America/Los_Angeles)", "resets in 3h"))
        .collect()
}

/// The Mac's trouble coming while a limit is waited out decides the limited point again at
/// the next step (the restart, not hours later at the reset).
#[test]
fn trouble_that_comes_behind_a_limit_decides_the_standing_point_again() {
    let host = Arc::new(TestHost::default());
    host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
    *host.restarts.lock().unwrap() = vec![Some("adopted")].into();
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let mut m = Mock::new(vec![busy_screen(), limited_hours_off()]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let stop = Stop::reached(
        |h| {
            if h.limits.lock().unwrap().contains(&true) && h.memory_asks.load(Ordering::SeqCst) >= 3
            {
                h.trouble.store(true, Ordering::SeqCst);
            }
            !h.restarted.lock().unwrap().is_empty()
        },
        Duration::from_millis(300),
    );
    let (r, lines) = hosted_with_host(&mut m, &opts, &host, stop, None);
    assert_eq!(r, Ok(()));
    assert!(
        host.limits.lock().unwrap().contains(&true),
        "the episode opened: {lines:#?}"
    );
    assert_eq!(*host.restarted.lock().unwrap(), ["memory"], "{lines:#?}");
}

/// The weekly limit's reset (`resets Sep 19 at 11am`, PDT): 2026-09-19T18:00Z.
const WEEKLY_RESET_AT: i64 = AFTER_RESET - 30;

/// The continuation a memory wall stood in for: once the measure that took the weekly
/// limit's place is no longer acted on (the footprint back under the ceiling), the limit
/// continues at its reset (`CONTINUED … usage-resume@v1`). Negative control: the measure
/// staying acted on continues nothing.
#[test]
fn a_memory_wall_no_longer_acted_on_hands_the_limit_back() {
    use std::sync::atomic::AtomicUsize;
    /// How the measure moves once the point is decided on it.
    #[derive(Clone, Copy, PartialEq)]
    enum Then {
        /// It stays acted on.
        Stays,
        /// The footprint drops under the ceiling.
        Drops,
    }
    // The run ends once the journal holds the continuation, or `window` after the move (a
    // control needs no more).
    let run = |tag: &str,
               clock: i64,
               restarts: Vec<Option<&'static str>>,
               crossing: bool,
               then: Then,
               fell_at_restart: bool,
               window: Duration| {
        let host = Arc::new(TestHost::default());
        host.trouble.store(true, Ordering::SeqCst);
        host.memory_mib.store(
            if crossing { HEALTHY_MIB } else { RUNAWAY_MIB },
            Ordering::SeqCst,
        );
        *host.restarts.lock().unwrap() = restarts.into();
        if fell_at_restart {
            host.memory_at_restart.store(HEALTHY_MIB, Ordering::SeqCst);
        }
        let (jdir, journal) = journal_file(tag);
        let opts = resuming(
            &SuperviseOpts {
                max: Duration::from_secs(30),
                journal: Some(journal.clone()),
                ..SuperviseOpts::hosted()
            },
            None,
        );
        let mut m = Mock::new(vec![busy_screen(), weekly_limited()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        // The asks at which the measure crossed (0: not yet).
        let crossed_at = Arc::new(AtomicUsize::new(0));
        // When the measure moved (or, for a control, would have).
        let moved: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        let watched = journal.clone();
        let stop = Stop::reached(
            move |h| {
                if let Some(at) = *moved.lock().unwrap() {
                    return at.elapsed() >= window
                        || std::fs::read_to_string(&watched)
                            .is_ok_and(|j| j.contains("rule=usage-resume@v1"));
                }
                if !h.limits.lock().unwrap().contains(&true) {
                    return false;
                }
                let asks = h.memory_asks.load(Ordering::SeqCst);
                if crossing {
                    // Decided healthy with the reset seconds off: the memory crosses, is
                    // decided on, and falls back under.
                    let at = crossed_at.load(Ordering::SeqCst);
                    if at == 0 {
                        h.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
                        crossed_at.store(asks.max(1), Ordering::SeqCst);
                        return false;
                    }
                    if asks < at + 3 {
                        return false;
                    }
                }
                match then {
                    Then::Stays => {}
                    Then::Drops => h.memory_mib.store(HEALTHY_MIB, Ordering::SeqCst),
                }
                *moved.lock().unwrap() = Some(Instant::now());
                false
            },
            Duration::from_millis(200),
        );
        let (r, lines) = hosted_with_host_clocked(
            &mut m,
            &opts,
            &host,
            stop,
            Some(quick_turn_end(&[Duration::from_secs(60)])),
            clock,
        );
        assert_eq!(r, Ok(()));
        let _ = std::fs::remove_dir_all(&jdir);
        let sets: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.contains("meta set attention owner=supervisor"))
            .cloned()
            .collect();
        let restarted = host.restarted.lock().unwrap().clone();
        (lines, sets, restarted)
    };
    let continued = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("rule=usage-resume@v1"))
    };
    let escalated = |sets: &[String]| {
        sets.iter()
            .any(|s| s.contains("memory critical: restart it") && s.contains("Memory past aterm"))
    };
    // Bounds a working loop never reaches: it ends at the continuation.
    let bound = Duration::from_secs(20);

    // (1) Past the reset, no host restarts: escalated, then the drop.
    let (lines, sets, _) = run(
        "mem-down-1",
        AFTER_RESET,
        vec![],
        false,
        Then::Drops,
        false,
        bound,
    );
    assert!(
        escalated(&sets),
        "(1) escalated first: {sets:#?}\n{lines:#?}"
    );
    assert!(continued(&lines), "(1) the drop: {sets:#?}\n{lines:#?}");

    // (2) A healthy agent 3 s before the reset; the memory crosses briefly
    // while the loop waits for it.
    let (lines, sets, _) = run(
        "mem-down-2",
        WEEKLY_RESET_AT - 3,
        vec![],
        true,
        Then::Drops,
        false,
        bound,
    );
    assert!(
        escalated(&sets),
        "(2) escalated first: {sets:#?}\n{lines:#?}"
    );
    assert!(continued(&lines), "(2) at the reset: {sets:#?}\n{lines:#?}");

    // (3b) A restarting host that refuses, the measure having fallen while the restart was
    // tried: the refused restart's own read is already under the ceiling, and the point is
    // handed back.
    let (lines, sets, restarted) = run(
        "mem-down-3b",
        AFTER_RESET,
        vec![Some("refused:one-shot")],
        false,
        Then::Stays,
        true,
        bound,
    );
    assert_eq!(restarted, ["memory"], "(3b) asked: {lines:#?}");
    assert!(
        sets.iter()
            .any(|s| s.contains("the restart could not be made (refused:one-shot)")),
        "(3b) escalated first: {sets:#?}\n{lines:#?}"
    );
    assert!(continued(&lines), "(3b) the fall: {sets:#?}\n{lines:#?}");

    // Negative control: the measure stays acted on: the person's restart stands, nothing
    // typed over it — for a second past the move, two hundred of the loop's 5 ms steps (a
    // hand-back comes at the first).
    let (lines, sets, _) = run(
        "mem-down-ctl",
        AFTER_RESET,
        vec![],
        false,
        Then::Stays,
        false,
        Duration::from_secs(1),
    );
    assert!(escalated(&sets), "control: {sets:#?}\n{lines:#?}");
    assert!(!continued(&lines), "control: {sets:#?}\n{lines:#?}");
}

/// A plain idle point the memory escalated (this agent the largest in trouble, no restart
/// made: none offered, or refused; `relaunch = false` is the decision's, `turn_end_tests`) is
/// handed back when the Mac recovers: the badge comes down (`CLEARED … memory attention=OK`),
/// the point is continued, the warning said again as `a warning only`. Negative control (the
/// host that makes none): the trouble stays, and so does the badge.
#[test]
fn a_plain_idle_point_the_memory_escalated_is_handed_back_when_the_mac_recovers() {
    use std::sync::atomic::AtomicUsize;
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Way {
        /// The host makes no restart (`can_restart` false).
        NoRestart,
        /// The host's restart could not be made.
        Refused,
    }
    let run = |tag: &str, way: Way, recovers: bool, window: Duration| {
        let host = Arc::new(TestHost::default());
        host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
        host.trouble.store(true, Ordering::SeqCst);
        // A refusal leaves the host one that restarts, as the window's is.
        host.restartable
            .store(way == Way::Refused, Ordering::SeqCst);
        *host.restarts.lock().unwrap() = match way {
            Way::NoRestart => Vec::new(),
            Way::Refused => vec![Some("refused:one-shot")],
        }
        .into();
        let (jdir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        // The memory asks at which the point was reached (0: not yet).
        let reached_at = Arc::new(AtomicUsize::new(0));
        let moved: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        let watched = journal.clone();
        let stop = Stop::reached(
            move |h| {
                if let Some(at) = *moved.lock().unwrap() {
                    return at.elapsed() >= window
                        || std::fs::read_to_string(&watched)
                            .is_ok_and(|j| j.contains("rule=continue@v1"));
                }
                if h.turn_ends.load(Ordering::SeqCst) < 1
                    || !std::fs::read_to_string(&watched).is_ok_and(|j| j.contains("ESCALATED"))
                {
                    return false;
                }
                // The point escalated, and a few steps waited past it.
                let asks = h.memory_asks.load(Ordering::SeqCst);
                let at = reached_at.load(Ordering::SeqCst);
                if at == 0 {
                    reached_at.store(asks.max(1), Ordering::SeqCst);
                    return false;
                }
                if asks < at + 3 {
                    return false;
                }
                if recovers {
                    h.trouble.store(false, Ordering::SeqCst);
                }
                *moved.lock().unwrap() = Some(Instant::now());
                false
            },
            Duration::from_millis(200),
        );
        let timing = TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        };
        let (r, _) = hosted_with_host(&mut m, &opts, &host, stop, Some(timing));
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let warned = host.warned.lock().unwrap().clone();
        (m, lines, warned)
    };
    let set_at = |m: &Mock| {
        m.requests.iter().position(|r| {
            r.contains("meta set attention owner=supervisor")
                && r.contains("memory critical: restart it")
        })
    };
    let unset_at = |m: &Mock| {
        m.requests
            .iter()
            .position(|r| r.contains("meta unset attention owner=supervisor"))
    };
    let continued = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("rule=continue@v1"))
    };
    let bound = Duration::from_secs(20);
    for way in [Way::NoRestart, Way::Refused] {
        let (m, lines, warned) = run("mem-plain-back", way, true, bound);
        let set = set_at(&m);
        assert!(set.is_some(), "{way:?}: escalated first: {lines:#?}");
        if way == Way::Refused {
            assert!(
                m.requests
                    .iter()
                    .any(|r| r.contains("the restart could not be made (refused:one-shot)")),
                "{way:?}: {:#?}",
                m.requests
            );
        }
        assert!(continued(&lines), "{way:?}: handed back: {lines:#?}");
        let unset = unset_at(&m);
        assert!(
            unset.is_some_and(|u| Some(u) > set),
            "{way:?}: the memory's badge taken down after it was raised: {:#?}",
            m.requests
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("CLEARED seq=") && l.contains("memory attention=OK")),
            "{way:?}: {lines:#?}"
        );
        // Said again with the verdict that holds now: every row before the recovery names
        // the memory as acted on (the escalation's, the person's restart); the last is a
        // warning only.
        let (last, before) = warned.split_last().expect("warned");
        assert!(
            before.last().is_some_and(|w| w.contains(PERSONS_ROW)),
            "{way:?}: the escalated point's row is the person's: {warned:#?}"
        );
        assert!(
            last.contains("a warning only: nothing is restarted"),
            "{way:?}: {warned:#?}"
        );
    }

    // Negative control: the trouble stays, for a second past the move (two hundred of the
    // loop's 5 ms steps; a hand-back comes at the first).
    let (m, lines, warned) = run(
        "mem-plain-stays",
        Way::NoRestart,
        false,
        Duration::from_secs(1),
    );
    assert!(set_at(&m).is_some(), "control: {lines:#?}");
    assert!(!continued(&lines), "control: {lines:#?}");
    // The badge stands until the loop exits (its exit unsets its own).
    let exit_unset = m
        .requests
        .iter()
        .rposition(|r| r.contains("meta unset attention owner=supervisor"));
    assert_eq!(
        unset_at(&m),
        exit_unset,
        "control: only the exit's unset: {:#?}",
        m.requests
    );
    assert!(
        warned.last().is_some_and(|w| w.contains(PERSONS_ROW)),
        "control: the standing row is the person's: {warned:#?}"
    );
}

/// The warning rows said for a person's restart and for aterm's
/// (`MemoryOver::warning`).
const PERSONS_ROW: &str = "it holds the most memory; restart it yourself";
const ATERMS_ROW: &str = "it holds the most memory, so aterm restarts it";

/// A refused memory restart stands until the host is asked again (the refusals stand for the
/// agent's life): the row stays the person's restart across turns, said once, until a restart
/// adopted at a later point names aterm's again.
#[test]
fn a_refused_memory_restart_keeps_the_persons_row_until_the_host_is_asked_again() {
    let run = |tag: &str, second: &'static str| {
        let host = Arc::new(TestHost::default());
        host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
        host.memory_once_made.store(HEALTHY_MIB, Ordering::SeqCst);
        host.trouble.store(true, Ordering::SeqCst);
        host.restartable.store(true, Ordering::SeqCst);
        *host.restarts.lock().unwrap() = vec![Some("refused:one-shot"), Some(second)].into();
        let (jdir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        // The point the restart is refused at, the next turn, its point.
        let mut m = Mock::new(vec![
            busy_screen(),
            idle_screen(),
            busy_screen(),
            idle_screen(),
        ]);
        m.stall_sleep = Some(Duration::from_millis(5));
        // The host asked again, and a few steps waited past it.
        let stop = Stop::reached(
            |h| h.restarted.lock().unwrap().len() >= 2,
            Duration::from_millis(500),
        );
        let timing = TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        };
        let (r, _) = hosted_with_host(&mut m, &opts, &host, stop, Some(timing));
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let restarted = host.restarted.lock().unwrap().clone();
        let warned = host.warned.lock().unwrap().clone();
        (lines, restarted, warned)
    };
    // The journal's `MEMORY` rows saying `row`.
    let said = |l: &String, row: &str| l.starts_with("MEMORY ") && l.contains(row);
    // The two asks, at two points (the second a later screen).
    let asked_at = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|l| l.starts_with("HOST seq=") && l.contains(" restart:memory step="))
            .map(|l| l.split_whitespace().nth(1).unwrap_or_default().to_string())
            .collect()
    };

    // Refused again at the next point: the person's row from the first
    // refusal on, never aterm's between.
    let (lines, restarted, warned) = run("mem-refused-again", "refused:one-shot");
    assert_eq!(restarted, ["memory", "memory"], "{lines:#?}");
    let asks = asked_at(&lines);
    assert!(
        asks.len() == 2 && asks[0] != asks[1],
        "asked at two points: {lines:#?}"
    );
    assert!(
        warned.len() == 2 && warned[0].contains(ATERMS_ROW) && warned[1].contains(PERSONS_ROW),
        "aterm's restart until the refusal, then the person's, said once: {warned:#?}"
    );

    // Adopted at the next point: the person's row until the restart is made.
    let (lines, restarted, warned) = run("mem-refused-then-made", "adopted");
    assert_eq!(restarted, ["memory", "memory"], "{lines:#?}");
    let refused = lines
        .iter()
        .position(|l| said(l, PERSONS_ROW))
        .expect("the person's row said at the refusal");
    let made = lines
        .iter()
        .position(|l| l.starts_with("RESTARTED ") && l.contains("for its memory"))
        .expect("the restart made");
    assert!(refused < made, "{lines:#?}");
    assert!(
        !lines[refused..made].iter().any(|l| said(l, ATERMS_ROW)),
        "no promise of aterm's restart between the refusal and the restart: {lines:#?}"
    );
    assert!(
        lines[made..].iter().any(|l| said(l, ATERMS_ROW)),
        "aterm's restart named again once one is made: {lines:#?}"
    );
    assert!(
        warned.last().is_some_and(|w| w.contains(ATERMS_ROW)),
        "{warned:#?}"
    );
}

/// A refused restart at Claude Code's own memory banner: nothing asks the host again while
/// the banner stands, and when the measure dips and comes back the row is said again with the
/// person's restart, as the badge says, never aterm's.
#[test]
fn a_refused_restart_at_the_memory_banner_keeps_the_persons_row_when_the_measure_comes_back() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    use std::sync::atomic::AtomicUsize;
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Dip {
        /// The Mac out of trouble, the footprint as it was.
        Recovers,
        /// The footprint an eighth under the ceiling: the episode ends.
        Drops,
    }
    let run = |tag: &str, dip: Dip| {
        let host = Arc::new(TestHost::default());
        host.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
        host.trouble.store(true, Ordering::SeqCst);
        host.restartable.store(true, Ordering::SeqCst);
        *host.restarts.lock().unwrap() = vec![Some("refused:one-shot")].into();
        let (jdir, journal) = journal_file(tag);
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), screen(MEMORY_BANNER_IDLE)]);
        m.stall_sleep = Some(Duration::from_millis(5));
        // The point escalated, then three memory asks apart each: the dip,
        // the measure back, the end.
        let stage = Arc::new(AtomicUsize::new(0));
        let at = Arc::new(AtomicUsize::new(0));
        let watched = journal.clone();
        let stop = Stop::reached(
            move |h| {
                let asks = h.memory_asks.load(Ordering::SeqCst);
                let s = stage.load(Ordering::SeqCst);
                if s == 0 {
                    if std::fs::read_to_string(&watched).is_ok_and(|j| j.contains("ESCALATED")) {
                        at.store(asks, Ordering::SeqCst);
                        stage.store(1, Ordering::SeqCst);
                    }
                    return false;
                }
                if asks < at.load(Ordering::SeqCst) + 3 {
                    return false;
                }
                at.store(asks, Ordering::SeqCst);
                stage.store(s + 1, Ordering::SeqCst);
                match (s, dip) {
                    (1, Dip::Recovers) => h.trouble.store(false, Ordering::SeqCst),
                    (1, Dip::Drops) => h.memory_mib.store(HEALTHY_MIB, Ordering::SeqCst),
                    (2, _) => {
                        h.trouble.store(true, Ordering::SeqCst);
                        h.memory_mib.store(RUNAWAY_MIB, Ordering::SeqCst);
                    }
                    _ => return true,
                }
                false
            },
            Duration::from_millis(200),
        );
        let timing = TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        };
        let (r, _) = hosted_with_host(&mut m, &opts, &host, stop, Some(timing));
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let restarted = host.restarted.lock().unwrap().clone();
        let warned = host.warned.lock().unwrap().clone();
        (m, lines, restarted, warned)
    };
    for dip in [Dip::Recovers, Dip::Drops] {
        let (m, lines, restarted, warned) = run("mem-banner-refused", dip);
        assert_eq!(restarted, ["memory"], "{dip:?}: asked once: {lines:#?}");
        assert!(
            m.requests.iter().any(|r| {
                r.contains("meta set attention owner=supervisor")
                    && r.contains("the restart could not be made (refused:one-shot)")
            }),
            "{dip:?}: escalated: {:#?}",
            m.requests
        );
        let refused = warned
            .iter()
            .position(|w| w.contains(PERSONS_ROW))
            .unwrap_or_else(|| panic!("{dip:?}: the person's row: {warned:#?}"));
        assert!(
            warned[..refused].iter().all(|w| w.contains(ATERMS_ROW)),
            "{dip:?}: aterm's restart promised until the refusal: {warned:#?}"
        );
        // Said again once the measure came back (the dip's own row, or the
        // episode's end), and never as aterm's restart.
        assert!(
            warned.len() > refused + 1,
            "{dip:?}: said again: {warned:#?}"
        );
        assert!(
            !warned[refused..].iter().any(|w| w.contains(ATERMS_ROW)),
            "{dip:?}: no promise of aterm's restart beside the badge: {warned:#?}"
        );
        assert!(
            warned.last().is_some_and(|w| w.contains(PERSONS_ROW)),
            "{dip:?}: the row agrees with the badge: {warned:#?}"
        );
    }
}

/// A restart that typed its carry-on (`continued`) typed a harness turn, so its short answer
/// is no short turn of the worker's and the next point is continued at once. Negative
/// control: one that typed nothing (`adopted`) leaves a worker's short turn, backed off.
#[test]
fn a_restart_that_typed_its_carry_on_typed_a_harness_turn() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    let run = |word: &'static str| {
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = vec![Some(word)].into();
        let (jdir, journal) = journal_file("restart-typed");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![
            busy_screen(),
            screen(MEMORY_BANNER_IDLE),
            busy_screen(),
            idle_screen(),
        ]);
        m.stall_sleep = Some(Duration::from_millis(5));
        // Stop at the carry-on's answer (the host told a second turn ran), with a tail for
        // what is decided there.
        let (r, _) = hosted_with_host(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                |h| h.turns_ran.load(Ordering::SeqCst) >= 2,
                Duration::from_millis(300),
            ),
            Some(TurnEndTiming {
                min_work: Duration::from_secs(3600),
                short_backoff: Duration::from_secs(3600),
                ..TurnEndTiming::default()
            }),
        );
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        records.into_iter().map(|r| r.line).collect::<Vec<String>>()
    };
    let waiting = |lines: &[String], n: u32| {
        lines.iter().any(|l| {
            l.starts_with("WAITING ") && l.contains(&format!(" {n} short turn(s) in a row"))
        })
    };
    let continued = |lines: &[String]| {
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("rule=continue@v1"))
    };
    let lines = run("continued");
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("restart:memory step=continued")),
        "{lines:#?}"
    );
    assert!(continued(&lines), "the carry-on's answer: {lines:#?}");
    assert!(!waiting(&lines, 1), "{lines:#?}");
    let lines = run("adopted");
    assert!(waiting(&lines, 1), "the worker's own turn: {lines:#?}");
    assert!(!continued(&lines), "{lines:#?}");
}

/// A Fable limit that names its reset (`resets Oct 30 at 9pm`), at a turn end.
fn fable_named() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've hit your Fable limit · resets Oct 30 at 9pm (America/Los_Angeles)",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}

/// A transcript whose newest entry is [`fable_named`]'s notice: the wall
/// CONFIRMED.
const FABLE_NOTICE: &str = r#"{"parentUuid":"a","isSidechain":false,"type":"user","message":{"role":"user","content":"run the suite"},"timestamp":"2026-10-08T16:35:31.654Z"}
{"parentUuid":"b","isSidechain":false,"type":"assistant","timestamp":"2026-10-08T16:35:31.676Z","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your Fable limit · resets Oct 30 at 9pm (America/Los_Angeles)"}]},"error":"rate_limit","isApiErrorMessage":true}
{"parentUuid":"c","isSidechain":false,"type":"system","subtype":"turn_duration","timestamp":"2026-10-08T16:35:31.677Z"}"#;

/// The decision's verdict is kept for the loop's books: a confirmed Fable limit relaunched on
/// opus records the same switch (from fable, back at the reset) after the host's carry-on has
/// moved the transcript on; the transcript is asked at the decision only.
#[test]
fn a_model_buckets_relaunch_keeps_its_way_back_when_the_transcript_moves_on() {
    let carried = format!(
        "{FABLE_NOTICE}\n{}",
        r#"{"parentUuid":"d","isSidechain":false,"type":"user","message":{"role":"user","content":"aterm relaunched you on opus; carry on"},"timestamp":"2026-10-08T16:36:00.000Z"}"#
    );
    let run = |tails: Option<(String, String)>| {
        let (dir, ledger) = ledger_file("bucket-books");
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = vec![Some("continued")].into();
        *host.tails.lock().unwrap() = tails;
        let mut m = Mock::new(vec![busy_screen(), fable_named()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        // 2026-10-08 12:00 PDT: the reset is ahead.
        let (r, lines) = hosted_with_host_at(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                |h| !h.restarted.lock().unwrap().is_empty(),
                Duration::from_millis(500),
            ),
            None,
            &ledger,
            Some(1_791_486_000),
        );
        assert_eq!(r, Ok(()));
        let open = crate::supervise::approvals::open_model_switch(&ledger, Some("@s-1"));
        let _ = std::fs::remove_dir_all(&dir);
        let asks = host.tail_asks.load(Ordering::SeqCst);
        (lines, open.map(|o| (o.from, o.back_at_unix)), asks)
    };
    let want = Some((Some("fable".to_string()), Some(1_793_419_200)));
    let (lines, open, _) = run(None);
    assert_eq!(open, want, "with no transcript: {lines:#?}");
    let (lines, open, asks) = run(Some((FABLE_NOTICE.to_string(), carried)));
    assert_eq!(open, want, "with the transcript moved on: {lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains("relaunch --model opus (from fable, back at ")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("rule=wall-confirm@v1")),
        "nothing refused: {lines:#?}"
    );
    assert!(asks >= 1, "the decision asked: {asks}");
}

/// A confirmed Fable limit whose relaunch is not made (`wait:changed`, `refused:off`) keeps the
/// decision's verdict too: the transcript is asked at the decision only, nothing refused.
#[test]
fn a_model_buckets_relaunch_not_made_keeps_the_decisions_verdict() {
    let moved = format!(
        "{FABLE_NOTICE}\n{}\n{}",
        r#"{"parentUuid":"d","isSidechain":false,"type":"user","message":{"role":"user","content":"carry on"},"timestamp":"2026-10-08T16:36:00.000Z"}"#,
        r#"{"parentUuid":"e","isSidechain":false,"type":"system","subtype":"turn_duration","timestamp":"2026-10-08T16:36:01.000Z"}"#
    );
    for answer in ["wait:changed", "refused:off"] {
        let (dir, ledger) = ledger_file("bucket-unmade-books");
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = vec![Some(answer)].into();
        *host.tails.lock().unwrap() = Some((FABLE_NOTICE.to_string(), moved.clone()));
        let mut m = Mock::new(vec![busy_screen(), fable_named()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        let (r, lines) = hosted_with_host_at(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                |h| !h.restarted.lock().unwrap().is_empty(),
                Duration::from_millis(500),
            ),
            None,
            &ledger,
            Some(1_791_486_000),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r, Ok(()), "{answer}");
        assert_eq!(
            *host.restarted.lock().unwrap(),
            ["model"],
            "{answer}: the relaunch asked once: {lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("rule=wall-confirm@v1")),
            "{answer}: nothing refused: {lines:#?}"
        );
        assert_eq!(
            host.tail_asks.load(Ordering::SeqCst),
            1,
            "{answer}: the transcript asked at the decision only: {lines:#?}"
        );
    }
}

/// A model bucket's fallback and its way back are relaunches the host makes (`--model`,
/// session-only), never Claude's own `/model`, the switch kept in the ledger. Negative
/// control: a fallback the host could not make is waited out, nothing typed, no badge.
#[test]
fn a_model_bucket_is_relaunched_on_the_fallback_and_back_never_by_model() {
    let fable = || {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch \
             models with /model.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let run = |screens: Vec<Vec<String>>,
               answers: Vec<Option<&'static str>>,
               ledger: &std::path::Path| {
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = answers.into();
        let mut m = Mock::new(screens);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        let (r, lines) = hosted_with_host_ledger(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                |h| !h.restarted.lock().unwrap().is_empty(),
                Duration::from_millis(300),
            ),
            None,
            ledger,
        );
        assert_eq!(r, Ok(()));
        let restarted = host.restarted.lock().unwrap().clone();
        (m, lines, restarted)
    };
    let no_model = |m: &Mock| !m.requests.iter().any(|r| r.contains("/model"));

    // The fallback, made.
    let (dir, ledger) = ledger_file("bucket-fallback");
    let (m, lines, restarted) = run(vec![busy_screen(), fable()], vec![Some("adopted")], &ledger);
    assert_eq!(restarted, ["model"], "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.contains("rule=model-fallback@v1 relaunch --model opus (from fable, ")),
        "the notice names no reset: none is guessed (BK): {lines:#?}"
    );
    assert!(no_model(&m), "{:#?}", m.requests);
    let open = crate::supervise::approvals::open_model_switch(&ledger, Some("@s-1"))
        .expect("the open switch");
    assert_eq!(
        (open.from.as_deref(), open.to.as_str(), open.back_at_unix),
        (Some("fable"), "opus", None)
    );
    let _ = std::fs::remove_dir_all(&dir);

    // The way back, by a loop that starts after the reset.
    let (dir, ledger) = ledger_file("bucket-back");
    let unix_now = crate::supervise::journal::unix_ms() / 1000;
    super::turn_end::seed_switch(&ledger, Some("s-1"), unix_now - 60, false, false);
    let (m, lines, restarted) = run(
        vec![busy_screen(), idle_screen()],
        vec![Some("adopted")],
        &ledger,
    );
    assert_eq!(restarted, ["model-back"], "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.ends_with("rule=model-restore@v1 relaunch --model fable")),
        "{lines:#?}"
    );
    assert!(no_model(&m), "{:#?}", m.requests);
    let _ = std::fs::remove_dir_all(&dir);

    // Negative control: a fallback not made: the reset waited out, nothing typed, no badge.
    let (dir, ledger) = ledger_file("bucket-refused");
    let (m, lines, restarted) = run(
        vec![busy_screen(), fable()],
        vec![Some("refused:line")],
        &ledger,
    );
    assert_eq!(restarted, ["model"]);
    assert!(no_model(&m), "{:#?}", m.requests);
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("RESTARTED") || l.starts_with("TYPED")),
        "{lines:#?}"
    );
    assert_eq!(
        m.requests
            .iter()
            .filter(|r| r.contains("meta set attention owner=supervisor"))
            .count(),
        0,
        "{:#?}",
        m.requests
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host's step is taken in place, at the first idle point it asked for and after the box
/// there is answered; nothing is typed at that point, and the loop runs on. (The same point
/// with no step asked for is continued:
/// `a_session_its_host_says_nobody_asked_anything_gets_nothing_typed`'s control; a host owning
/// the turn end gets nothing typed: [`a_turn_end_the_host_let_go_is_decided_again`].)
#[test]
fn the_hosts_step_is_taken_at_the_idle_point_and_the_loop_runs_on() {
    let host = Arc::new(TestHost::default());
    host.wants.store(true, Ordering::SeqCst);
    let mut m = fenced(vec![
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(5));
    let (jdir, journal) = journal_file("idle-host");
    let journaled = SuperviseOpts {
        max: Duration::from_secs(30),
        journal: Some(journal.clone()),
        ..SuperviseOpts::hosted()
    };
    let (r, _) = hosted_with_host(
        &mut m,
        &journaled,
        &host,
        Stop::reached(
            |h| h.steps.load(Ordering::SeqCst) >= 1,
            Duration::from_millis(300),
        ),
        None,
    );
    // The host's step is in the session's journal beside the loop's own.
    let (records, _) = journal_records(&journal);
    assert!(
        records.iter().any(
            |r| r.line.starts_with("HOST seq=") && r.line.ends_with("upgrade step=announced:1")
        ),
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(r, Ok(()));
    assert_eq!(host.steps.load(Ordering::SeqCst), 1, "{:?}", m.requests);
    assert_eq!(
        m.presses().len(),
        1,
        "the box answered first: {:?}",
        m.requests
    );
    assert_eq!(
        count(&m, "@s-1 turn") + count(&m, "@s-1 send"),
        0,
        "{:?}",
        m.requests
    );
    // The loop ran on after the step: its claim was released only at the
    // stop, once, and no wait was cut to a park step.
    assert_eq!(
        m.claims
            .iter()
            .filter(|c| c.contains("meta unset supervisor"))
            .count(),
        1,
        "{:?}",
        m.claims
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.ends_with("timeout 2000") && r.contains("await seq")),
        "{:?}",
        m.requests
    );
}

/// The inline REPL is an idle point: drawn at the top of a 150x50 pane, its caret is above
/// the 40-row tail ([`tail_misses_the_live_rows`]), so the tail is read again whole and cut to
/// the live zone. So is the same REPL drawn lower, its caret the tail's first row and its top
/// rule the row above it ([`drawn_above_tail`]). Controls: the fullscreen REPL (read whole
/// only where its tail holds a blank gap); a half-drawn REPL and an inline relaunch before its
/// new REPL are no idle point.
#[test]
fn an_inline_repl_above_the_tail_is_the_hosts_idle_point() {
    use aterm_phase::prompt::fixtures::{
        INLINE_RELAUNCH_BEFORE_REPL, INLINE_RELAUNCH_REPL_READY, INLINE_REPL_HALF_DRAWN,
        INLINE_REPL_READY, LAUNCH_REPL_READY, cursor, screen,
    };
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run_rows = |repl: Vec<String>, row: usize, step: bool| {
        assert_eq!(repl.len(), 50, "a 50-row pane");
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let mut m = Mock::new(vec![busy_screen(), repl]);
        m.screen_cursors.insert(1, row);
        m.stall_sleep = Some(Duration::from_millis(5));
        let stop = if step {
            Stop::reached(
                |h| h.steps.load(Ordering::SeqCst) >= 1,
                Duration::from_millis(300),
            )
        } else {
            Stop::After(Duration::from_millis(400))
        };
        let (r, _) = hosted_with_host(&mut m, &full, &host, stop, None);
        assert_eq!(r, Ok(()));
        let reads: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.contains(" text ") || r.starts_with("text "))
            .map(|r| r.trim_start_matches("@s-1 ").to_string())
            .collect();
        (host.steps.load(Ordering::SeqCst), reads, m.requests.clone())
    };
    let run = |text: &str, step: bool| {
        let (row, _) = cursor(text).expect("a measured cursor");
        run_rows(screen(text), row, step)
    };
    let (steps, reads, requests) = run(INLINE_REPL_READY, true);
    assert_eq!(
        steps, 1,
        "the host's step at the inline REPL: {requests:#?}"
    );
    assert!(
        reads
            .windows(2)
            .any(|w| w[0] == "text --json tail=40" && w[1] == "text --json"),
        "a blank tail read again whole: {reads:#?}"
    );
    let (caret, _) = cursor(INLINE_REPL_READY).expect("a measured cursor");
    let down = 50 - 40 - caret;
    let ready = screen(INLINE_REPL_READY);
    let mut lower = vec![String::new(); down];
    lower.extend_from_slice(&ready[..ready.len() - down]);
    assert!(
        lower[caret + down].starts_with('❯'),
        "{:?}",
        lower[caret + down]
    );
    let (steps, reads, requests) = run_rows(lower, caret + down, true);
    assert_eq!(steps, 1, "the caret on the tail's first row: {requests:#?}");
    assert!(
        reads
            .windows(2)
            .any(|w| w[0] == "text --json tail=40" && w[1] == "text --json"),
        "the undecided tail read again whole: {reads:#?}"
    );
    // Negative controls.
    // The fullscreen launch, drawn to its last row with a blank gap between banner and
    // composer: the tail holding the gap ([`gap_above_tail`]) is read again whole.
    let (steps, reads, requests) = run(LAUNCH_REPL_READY, true);
    assert_eq!(steps, 1, "the fullscreen REPL: {requests:#?}");
    assert!(
        reads
            .windows(2)
            .any(|w| w[0] == "text --json tail=40" && w[1] == "text --json"),
        "drawn to its last row with a gap in the tail: read again whole: {reads:#?}"
    );
    // The same screen with the gap filled (a full transcript): the tail alone.
    let launch = screen(LAUNCH_REPL_READY);
    let (row, _) = cursor(LAUNCH_REPL_READY).expect("a measured cursor");
    let filled: Vec<String> = launch
        .iter()
        .map(|r| {
            if r.trim().is_empty() {
                "· x".to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    let (steps, reads, requests) = run_rows(filled, row, true);
    assert_eq!(steps, 1, "the fullscreen REPL, no gap: {requests:#?}");
    assert!(
        !reads.iter().any(|r| r == "text --json"),
        "drawn to its last row, no gap: the tail alone: {reads:#?}"
    );
    let (steps, _, requests) = run(INLINE_REPL_HALF_DRAWN, false);
    assert_eq!(steps, 0, "the REPL half drawn: {requests:#?}");
    let (steps, _, requests) = run(INLINE_RELAUNCH_BEFORE_REPL, false);
    assert_eq!(steps, 0, "the previous run's box: {requests:#?}");
    let (steps, _, requests) = run(INLINE_RELAUNCH_REPL_READY, true);
    assert_eq!(steps, 1, "the relaunched REPL: {requests:#?}");
}

/// A rating card above the tail (Claude Code 2.1.286, a 144x53 pane, the card on rows 11-12)
/// is dismissed: the 40-row tail holds only the idle composer over blank rows, which read
/// alone would leave the card up for good, so the screen is read whole and cut to the live
/// zone.
#[test]
fn a_rating_card_above_the_tail_is_dismissed() {
    use aterm_phase::prompt::fixtures::{SURVEY_CARD_ABOVE_THE_TAIL_MEASURED, screen};
    let pane = |mut rows: Vec<String>| {
        rows.resize(53, String::new());
        rows
    };
    let card = pane(screen(SURVEY_CARD_ABOVE_THE_TAIL_MEASURED));
    assert!(
        card[11].starts_with("● How is Claude doing"),
        "{:?}",
        card[11]
    );
    assert!(
        card[14].starts_with('─') && card[15] == "❯",
        "{:?}",
        &card[14..16]
    );
    let mut gone = screen(SURVEY_CARD_ABOVE_THE_TAIL_MEASURED);
    gone.drain(11..14);
    let mut m = Mock::new(vec![card.clone(), card, pane(gone)]);
    m.cursor_on_caret = true;
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines(&mut m, &surveys(auto(30, None), true));
    assert!(
        lines
            .first()
            .is_some_and(|l| l.starts_with("DISMISSED survey seq=")),
        "{lines:?}\n{:#?}",
        m.requests
    );
    assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
    // The tail, then the whole read the idle composer under it asks for.
    assert_eq!(
        text_reads(&m, 2),
        ["text --json tail=40", "text --json"],
        "{:#?}",
        m.requests
    );
}

/// A turn end the host let go is decided again: owned and then let go, or passed over for a
/// step that owns nothing, it is continued once the host owns nothing; one the host keeps, or
/// a point a host step moved, gets nothing. Tier-1 for `SupervisorHostTurnEnd`: the loop
/// continues exactly where `Redecide` is enabled, and the `Buggy` loop is caught.
#[test]
fn a_turn_end_the_host_let_go_is_decided_again() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run = |owns: bool, step: &str| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.owns.store(owns, Ordering::SeqCst);
        host.lets_go.store(step == "HostLetsGo", Ordering::SeqCst);
        host.types.store(step == "HostMoves", Ordering::SeqCst);
        let mut m = Mock::new(vec![busy_screen(), idle_screen()]);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        m.turn_releases = Some(1);
        let (r, _) = hosted_with_host(
            &mut m,
            &full,
            &host,
            Stop::reached(
                |h| h.steps.load(Ordering::SeqCst) >= 1,
                Duration::from_millis(300),
            ),
            Some(TurnEndTiming {
                short_backoff: Duration::from_millis(10),
                ..TurnEndTiming::default()
            }),
        );
        assert_eq!(r, Ok(()));
        assert_eq!(host.steps.load(Ordering::SeqCst), 1, "{:?}", m.requests);
        count(&m, "@s-1 turn")
    };
    let model = aterm_models_harness::supervisor_host_turn_end_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    // (owns at the turn end, the host's step — the model's action — why)
    for (owns, step, why) in [
        (true, "HostLetsGo", "owned, then let go"),
        (false, "HostLetsGo", "passed over, owns nothing"),
        (true, "HostKeeps", "the upgrade keeps it"),
        (false, "HostMoves", "a carry-on typed: the point is gone"),
    ] {
        let mut st = model.init_state();
        st.insert("owns", i64::from(owns));
        for action in ["TurnEnds", step] {
            assert!(model.fire(action, &mut st), "{why}: {action} at {st:?}");
        }
        let typed = run(owns, step);
        assert_eq!(
            typed == 1,
            model.action_enabled("Redecide", &st),
            "{why}: the real loop typed {typed} at {st:?}"
        );
        let mut waited = st.clone();
        if model.action_enabled("Redecide", &st) {
            // Negative control: the loop that waits there is stranded.
            assert!(!model.action_enabled("LoopWaits", &st), "{why}");
            assert!(buggy.fire("LoopWaits", &mut waited), "{why}");
            assert!(
                !buggy.check_invariant("NoTurnEndLeftToNobody", &waited),
                "{why}"
            );
        } else if step == "HostMoves" {
            // Negative control: the loop that decides it again decides a
            // point that is gone.
            assert_eq!(typed, 0, "{why}: the step's turn is under way");
            let mut decided = st.clone();
            assert!(buggy.fire("Redecide", &mut decided), "{why}");
            assert!(
                !buggy.check_invariant("NeverDecideAPointAStepMoved", &decided),
                "{why}"
            );
        } else {
            assert_eq!(typed, 0, "{why}: still the host's");
        }
    }
}

/// The host is told when a turn ran since the last point ([`IdleHost::turn_ran`]), so its
/// looks start over. Negative control: repaints with no busy read between tell it nothing.
#[test]
fn the_host_is_told_a_turn_ran_only_when_one_did() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run = |screens: Vec<Vec<String>>, stop: Stop| {
        let host = Arc::new(TestHost::default());
        host.owns.store(true, Ordering::SeqCst);
        let mut m = Mock::new(screens);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(0);
        // The script's end is the session's (`ERR exited`): what was said
        // before it is what counts.
        let _ = hosted_with_host(&mut m, &full, &host, stop, None);
        host.turns_ran.load(Ordering::SeqCst)
    };
    let ran = run(
        vec![idle_screen(), busy_screen(), idle_screen()],
        Stop::reached(
            |h| h.turns_ran.load(Ordering::SeqCst) >= 1,
            Duration::from_millis(300),
        ),
    );
    assert_eq!(ran, 1, "one turn between two points");
    let ticking = |i: u32| {
        let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        r.extend(composer(&format!("  ? for shortcuts · {i}s")));
        r
    };
    assert_eq!(
        run(
            vec![ticking(0), ticking(1), ticking(2)],
            Stop::After(Duration::from_millis(300)),
        ),
        0,
        "repaints only"
    );
}

/// The host reads its usage tap at each decided idle turn end of a Claude Code
/// ([`IdleHost::usage_tap`]), not at the books' reread. Negative control: a session at a
/// usage limit has no idle point, and the host is never asked.
#[test]
fn the_host_reads_its_usage_tap_at_each_decided_idle_turn_end() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run = |screens: Vec<Vec<String>>, stop: Stop| {
        let host = Arc::new(TestHost::default());
        host.owns.store(true, Ordering::SeqCst);
        let mut m = Mock::new(screens);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(0);
        let _ = hosted_with_host(&mut m, &full, &host, stop, None);
        (
            host.taps.load(Ordering::SeqCst),
            host.turn_ends.load(Ordering::SeqCst),
        )
    };
    let (taps, _) = run(
        vec![idle_screen(), busy_screen(), idle_screen()],
        Stop::reached(
            |h| h.taps.load(Ordering::SeqCst) >= 2,
            Duration::from_millis(300),
        ),
    );
    assert!(taps >= 2, "each decided idle point: {taps}");
    let (taps, _) = run(
        vec![busy_screen(), weekly_limited()],
        Stop::After(Duration::from_millis(300)),
    );
    assert_eq!(taps, 0, "a limit is no idle point");
}

/// A turn the host typed ([`HostStep::typed`]) is the harness's own: its short answer leaves
/// the worker's short-turn streak as it was, and real work ends it. Negative control: after a
/// step that typed nothing (`done:fresh`), the short turn is the worker's.
#[test]
fn a_turn_the_host_typed_is_no_short_turn_of_the_workers() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run = |ends: bool, min_work: Duration| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.types.store(true, Ordering::SeqCst);
        host.ends.store(ends, Ordering::SeqCst);
        let mut m = Mock::new(vec![idle_screen(), busy_screen(), idle_screen()]);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        let (jdir, journal) = journal_file("host-typed");
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..full.clone()
        };
        // Stop at the answer's point (the host told a turn ran), with a tail for what is
        // decided there.
        let (r, _) = hosted_with_host(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                |h| h.steps.load(Ordering::SeqCst) >= 1 && h.turns_ran.load(Ordering::SeqCst) >= 1,
                Duration::from_millis(300),
            ),
            Some(TurnEndTiming {
                min_work,
                short_backoff: Duration::from_secs(3600),
                ..TurnEndTiming::default()
            }),
        );
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        assert_eq!(r, Ok(()));
        assert_eq!(host.steps.load(Ordering::SeqCst), 1, "{:?}", m.requests);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (count(&m, "@s-1 turn"), lines)
    };
    let waiting = |lines: &[String], n: u32| {
        lines.iter().any(|l| {
            l.starts_with("WAITING ") && l.contains(&format!(" {n} short turn(s) in a row"))
        })
    };
    let hour = Duration::from_secs(3600);
    let (typed, lines) = run(false, hour);
    assert_eq!(typed, 0, "the first point's back-off stands: {lines:#?}");
    assert!(waiting(&lines, 1), "the streak stays one: {lines:#?}");
    assert!(
        !waiting(&lines, 2),
        "no short turn of the worker's: {lines:#?}"
    );
    let (typed, lines) = run(false, Duration::ZERO);
    assert_eq!(typed, 1, "real work ends the streak: {lines:#?}");
    let (typed, lines) = run(true, hour);
    assert_eq!(typed, 0, "{lines:#?}");
    assert!(
        waiting(&lines, 2),
        "the worker's own short turn after a restart: {lines:#?}"
    );
}

/// WHICH turn the host typed reaches the policy from the host's own step
/// ([`HostStep::typed`]): its carry-on (`continued`) answered with real work
/// that says done is a done report, and the loop's next act is the done
/// check; its notice (`announced:<n>`) answered the same way is no done
/// report (the notice asks for a stopping point), and the loop continues
/// (2026-10-04 02:08: a carry-on's `All queued work has landed.` got `keep
/// going`). The first `CONTINUED` row is waited for (a hang detector, never
/// a budget).
#[test]
fn the_host_tells_a_carry_on_from_a_notice_and_only_the_carry_on_is_checked() {
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, RULE_DONE_CHECK};
    let full = SuperviseOpts {
        max: Duration::from_secs(60),
        ..SuperviseOpts::hosted()
    };
    let done_screen = || {
        let mut r = rows(&["⏺ All done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let run = |carry_on: bool| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.types.store(carry_on, Ordering::SeqCst);
        let mut m = Mock::new(vec![idle_screen(), busy_screen(), done_screen()]);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        let (jdir, journal) = journal_file("host-turn-kind");
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..full.clone()
        };
        let watched = journal.clone();
        let (r, _) = hosted_with_host(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                move |_| {
                    std::fs::read_to_string(&watched)
                        .is_ok_and(|j| j.lines().any(|l| l.contains("CONTINUED seq=")))
                },
                Duration::from_millis(20),
            ),
            // Any work is real work, and the first point's back-off is gone
            // by the answer's point: the act there is the point's own.
            Some(TurnEndTiming {
                min_work: Duration::ZERO,
                short_backoff: Duration::from_millis(1),
                ..TurnEndTiming::default()
            }),
        );
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        assert_eq!(r, Ok(()));
        assert_eq!(host.steps.load(Ordering::SeqCst), 1, "{:?}", m.requests);
        records
            .into_iter()
            .map(|r| r.line)
            .find(|l| l.starts_with("CONTINUED "))
            .unwrap_or_default()
    };
    let first = run(true);
    assert!(
        first.contains(&format!(" rule={RULE_DONE_CHECK} ")),
        "the carry-on's done report is checked: {first}"
    );
    let first = run(false);
    assert!(
        first.contains(&format!(" rule={RULE_CONTINUE} ")),
        "the notice's answer is continued: {first}"
    );
}

/// A turn the host typed holds the policy's acts, never the host's own next step: on a
/// screen that never moves, the host's notice and then its second step are both taken, and
/// the policy types nothing meanwhile.
#[test]
fn a_turn_the_host_typed_never_holds_the_hosts_next_step() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let host = Arc::new(TestHost::default());
    host.wants.store(true, Ordering::SeqCst);
    host.owns.store(true, Ordering::SeqCst);
    host.again.store(1, Ordering::SeqCst);
    let mut m = Mock::new(vec![idle_screen()]);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(5));
    let (r, _) = hosted_with_host(
        &mut m,
        &full,
        &host,
        Stop::reached(
            |h| h.steps.load(Ordering::SeqCst) >= 2,
            Duration::from_millis(300),
        ),
        None,
    );
    assert_eq!(r, Ok(()));
    assert_eq!(
        host.steps.load(Ordering::SeqCst),
        2,
        "the step after its own typed turn: {:?}",
        m.requests
    );
    assert_eq!(count(&m, "@s-1 turn"), 0, "the upgrade owns the point");
}

/// A host's same wait said again at one point is journaled once: a person's hold is looked at
/// every twenty seconds, so each later look is marked a repeat ([`HostStep::repeat`]).
/// Negative control: unmarked, each is journaled.
#[test]
fn a_hosts_same_wait_said_again_at_one_point_is_journaled_once() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run = |repeats: bool| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.again.store(2, Ordering::SeqCst);
        host.holds.store(true, Ordering::SeqCst);
        host.marks.store(repeats, Ordering::SeqCst);
        let mut m = Mock::new(vec![idle_screen()]);
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        let (jdir, journal) = journal_file("idle-host-repeat");
        let journaled = SuperviseOpts {
            journal: Some(journal.clone()),
            ..full.clone()
        };
        let (r, _) = hosted_with_host(
            &mut m,
            &journaled,
            &host,
            Stop::reached(
                |h| h.steps.load(Ordering::SeqCst) >= 3,
                Duration::from_millis(300),
            ),
            None,
        );
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let rows = records
            .iter()
            .filter(|r| r.line.starts_with("HOST seq="))
            .count();
        (
            host.steps.load(Ordering::SeqCst),
            rows,
            count(&m, "@s-1 turn"),
        )
    };
    let (steps, rows, typed) = run(true);
    assert_eq!(steps, 3, "every look is taken");
    assert_eq!(rows, 1, "the first look alone is journaled");
    assert_eq!(typed, 0, "the point is the upgrade's throughout");
    // Negative control: unmarked, each step is a row.
    let (steps, rows, typed) = run(false);
    assert_eq!(steps, 3);
    assert_eq!(rows, 3, "each step journaled");
    assert_eq!(typed, 0);
}

/// A session its host says nobody has asked anything ([`IdleHost::taskless`]) gets nothing at
/// its turn ends: no continuation, no answer, no escalation. Negative control: on a host that
/// does not say so, both points are acted on.
#[test]
fn a_session_its_host_says_nobody_asked_anything_gets_nothing_typed() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let asked = || {
        let mut r = rows(&["⏺ What would you like me to help you with?", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    for (what, point) in [("a report", idle_screen()), ("a question", asked())] {
        let run = |taskless: bool| {
            let host = Arc::new(TestHost::default());
            host.taskless.store(taskless, Ordering::SeqCst);
            let mut m = Mock::new(vec![busy_screen(), point.clone()]);
            m.stall_sleep = Some(Duration::from_millis(5));
            m.turn_releases = Some(1);
            // Both arms reach the turn end (the host asked whether it owns
            // it): the tail is where the point's act — or its absence — shows.
            let (r, _) = hosted_with_host(
                &mut m,
                &full,
                &host,
                Stop::reached(
                    |h| h.turn_ends.load(Ordering::SeqCst) >= 1,
                    Duration::from_millis(300),
                ),
                Some(TurnEndTiming {
                    short_backoff: Duration::from_millis(10),
                    ..TurnEndTiming::default()
                }),
            );
            assert_eq!(r, Ok(()), "{what}");
            m
        };
        let m = run(true);
        assert_eq!(
            count(&m, "@s-1 turn") + count(&m, "@s-1 send"),
            0,
            "{what}: nothing typed: {:?}",
            m.requests
        );
        assert!(
            !m.requests.iter().any(|r| r.contains("meta set attention")),
            "{what}: nothing escalated: {:?}",
            m.requests
        );
        // Negative control: asked something, the point is acted on.
        let m = run(false);
        assert_eq!(count(&m, "@s-1 turn"), 1, "{what}: {:?}", m.requests);
    }
}

/// A SESSION WHOSE RECORD SAYS ITS TASK IS FINISHED GETS NOTHING TYPED
/// ([`IdleHost::finished`], [`TurnEndReading::finished_record`]): a new loop
/// whose first screen's last words are not the worker's — Claude Code's
/// post-resume paragraph — backs off into no `keep going`, and a turn Claude
/// Code woke the agent for is not continued: the host's record is read at
/// each. NEGATIVE CONTROL: the same points on a host whose record says
/// someone asked since are continued (the screen moves once the turn is
/// typed, and the point after it, where the loop says a turn ran, is the
/// hang detector's mark).
#[test]
fn a_session_whose_record_says_finished_gets_nothing_typed() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let resumed = || {
        let mut r = rows(&[
            "⏺ Auto mode lets Claude handle permission prompts automatically for this session.",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    // (what, the script, the screen that sits until a turn is typed, the
    // busy reads once it was)
    for (what, script, sits, ran) in [
        (
            "a new loop's first point",
            vec![resumed(), busy_screen(), resumed()],
            0,
            1,
        ),
        (
            "a turn Claude Code woke it for",
            vec![busy_screen(), resumed(), busy_screen(), resumed()],
            1,
            2,
        ),
    ] {
        let run = |finished: Option<bool>| {
            let host = Arc::new(TestHost::default());
            *host.finished.lock().unwrap() = finished;
            let mut m = Mock::new(script.clone());
            m.stall_sleep = Some(Duration::from_millis(5));
            m.turn_releases = Some(sits);
            let stop = if finished == Some(true) {
                Stop::reached(
                    |h| h.finished_asks.load(Ordering::SeqCst) >= 1,
                    Duration::from_millis(300),
                )
            } else {
                Stop::reached(
                    move |h| h.turns_ran.load(Ordering::SeqCst) >= ran,
                    Duration::from_millis(50),
                )
            };
            let (r, _) = hosted_with_host(
                &mut m,
                &full,
                &host,
                stop,
                Some(TurnEndTiming {
                    short_backoff: Duration::from_millis(10),
                    ..TurnEndTiming::default()
                }),
            );
            assert_eq!(r, Ok(()), "{what}");
            assert!(
                host.finished_asks.load(Ordering::SeqCst) >= 1,
                "{what}: the record is read"
            );
            m
        };
        let m = run(Some(true));
        assert_eq!(
            count(&m, "@s-1 turn") + count(&m, "@s-1 send"),
            0,
            "{what}: nothing typed: {:?}",
            m.requests
        );
        // At least one: the point after the typed turn is the next one's.
        let m = run(Some(false));
        assert!(count(&m, "@s-1 turn") >= 1, "{what}: {:?}", m.requests);
    }
}

/// THE RECORD IS READ AT EVERY POINT A TURN RAN, NOT ONLY AT A NEW LOOP'S
/// FIRST ([`TurnEndState::reads_record`]): a loop that continued its worker
/// once, the record saying the task open then, reads the record again at the
/// next point a turn ran — where the worker said the token while a shell
/// kept the screen busy, and the point's last words are a background job's
/// line, never the token — and types no second turn. NEGATIVE CONTROL: the
/// record still saying the task is open there is continued again (the
/// point after that turn, where the loop says a third turn ran, is the hang
/// detector's mark).
#[test]
fn the_record_is_read_at_every_point_a_turn_ran() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let report = |said: &str| {
        let mut r = rows(&[said, ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let script = vec![
        busy_screen(),
        report("⏺ Landed the first half; the parser is next."),
        busy_screen(),
        report("⏺ The background worktree removal finished; 744 GiB reclaimed."),
        busy_screen(),
        report("⏺ Started on the docs pass."),
    ];
    let run = |later: Option<bool>| {
        let host = Arc::new(TestHost::default());
        *host.finished.lock().unwrap() = later;
        host.finished_from.store(2, Ordering::SeqCst);
        let mut m = Mock::new(script.clone());
        m.stall_sleep = Some(Duration::from_millis(5));
        // Each point sits until the loop types its turn.
        m.turn_gates = vec![1, 3];
        let stop = if later == Some(true) {
            Stop::reached(
                |h| h.turns_ran.load(Ordering::SeqCst) >= 2,
                Duration::from_millis(300),
            )
        } else {
            Stop::reached(
                |h| h.turns_ran.load(Ordering::SeqCst) >= 3,
                Duration::from_millis(50),
            )
        };
        let (r, _) = hosted_with_host(
            &mut m,
            &full,
            &host,
            stop,
            Some(TurnEndTiming {
                short_backoff: Duration::from_millis(10),
                ..TurnEndTiming::default()
            }),
        );
        assert_eq!(r, Ok(()));
        assert!(
            host.finished_asks.load(Ordering::SeqCst) >= 2,
            "read at each point"
        );
        m
    };
    let m = run(Some(true));
    assert_eq!(
        count(&m, "@s-1 turn"),
        1,
        "one turn, then none: {:?}",
        m.requests
    );
    assert_eq!(count(&m, "@s-1 send"), 0, "{:?}", m.requests);
    // NEGATIVE CONTROL: still open at the second point.
    let m = run(Some(false));
    assert!(count(&m, "@s-1 turn") >= 2, "{:?}", m.requests);
}

/// A point read before the agent drew itself (its launch line at the shell prompt) is waited
/// on by its content, never by the verdict, already `idle`, which never moves as the composer
/// comes up. Negative control: a first screen the reader vouches for takes the step at once.
#[test]
fn a_point_read_before_the_agent_drew_itself_is_waited_on_by_its_content() {
    let launch = rows(&["% cd /w && '/Users//a/pkg/agents/claude' --model opus", ""]);
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.lets_go.store(true, Ordering::SeqCst);
        let mut m = fenced(screens);
        m.cursor_on_caret = true;
        m.agent_pushes = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted()
        };
        let (r, _) = hosted_with_host(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                |h| h.steps.load(Ordering::SeqCst) >= 1,
                Duration::from_millis(300),
            ),
            None,
        );
        assert_eq!(r, Ok(()));
        (host.steps.load(Ordering::SeqCst), m.requests)
    };
    // A busy frame (past the start), then the launch line through the loop's settling reads,
    // then the composer.
    let mut drawn = vec![busy_screen()];
    drawn.extend(vec![launch; 8]);
    drawn.push(idle_screen());
    let (steps, requests) = run(drawn);
    assert_eq!(steps, 1, "the step on the composer: {requests:#?}");
    // Negative control: the composer from the start.
    let (steps, _) = run(vec![idle_screen()]);
    assert_eq!(steps, 1);
}

/// The vendor's key guard (a box ignores keys the instant it is drawn): a box is judged only
/// on a read made once it has settled, so the press is fenced on that fresh read. Negative
/// control: with no settle the press follows the first read.
#[test]
fn a_box_is_pressed_only_once_it_has_shown_the_settle() {
    for settle in [Duration::from_millis(300), Duration::ZERO] {
        let mut m = fenced(vec![bash_one_row()]);
        // A wait on the sitting box sleeps what it asks, as a real one does.
        m.stall_sleep = Some(settle);
        m.vanish_after = Some(u32::from(!settle.is_zero()));
        let mut s = session(&mut m, None);
        s.set_box_settle(settle);
        let _ = s.supervise(&auto(30, None));
        drop(s);
        assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
        let press = m
            .requests
            .iter()
            .position(|r| r.starts_with("key "))
            .expect("the press");
        let before = &m.requests[..press];
        let reads = before.iter().filter(|r| r.starts_with("text ")).count();
        if settle.is_zero() {
            assert_eq!(reads, 1, "{before:?}");
            // No settle, no wait: read off the requests, not a stopwatch.
            assert!(
                !before.iter().any(|r| r.starts_with("await seq ")),
                "a zero settle waited before the press: {before:?}"
            );
        } else {
            assert_eq!(reads, 2, "judged on a read after the settle: {before:?}");
            let wait = before
                .iter()
                .position(|r| r.starts_with("await seq "))
                .expect("the settle's wait");
            let last_read = before.iter().rposition(|r| r.starts_with("text ")).unwrap();
            assert!(
                wait < last_read,
                "the wait comes before the read: {before:?}"
            );
            // Fenced on the fresh read: the press landed, never `changed`.
            assert!(m.presses()[0].contains("if-gen="), "{:?}", m.presses());
        }
    }
}

/// A subagent's rm-breaker box (`policy/fixtures/owner-rm-breaker-subagent-2026-09-26.txt`)
/// is pressed `1` once by the host's default on a current host (the digit fenced on the
/// read's generation and person count), no badge, one `allow-once@v1` row naming the breaker
/// and the operand. Under `approve = "safe"` it is pressed with a `WARN`. (`rm_breaker_ask`
/// escalating it: `policy/approval_tests.rs`, and in the loop
/// `the_rm_breaker_is_approved_under_a_scratch_root_and_escalated_otherwise`.)
#[test]
fn the_owners_subagent_rm_box_is_pressed_once_by_the_hosts_default() {
    let owners_box = || -> Vec<String> {
        include_str!("policy/fixtures/owner-rm-breaker-subagent-2026-09-26.txt")
            .lines()
            .map(str::to_string)
            .collect()
    };
    let first_row = "   │ cd /Users/_owner/aterm-wt-gap && git branch -m \
                     fix/rainbow-reflow-echo-and-degraded-notice fix/rainbow-relayout-echo && \
                     git branch";
    assert!(
        owners_box().iter().any(|r| r == first_row),
        "the fixture's first command row"
    );
    let owner = owner_env;
    let guard = crate::supervise::policy::row_guard(first_row);
    let focus = owners_box()
        .iter()
        .position(|r| r == " ❯ 1. Yes")
        .expect("the focused option");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&SupervisorConfig::default())
    };
    let (ldir, ledger) = ledger_file("owners-subagent-box");
    let mut m = fenced(vec![bypass_busy(), owners_box(), busy_screen()]);
    // The hidden cursor on the box's focused `❯ 1. Yes`, inside the tail,
    // where Claude Code parks it: one read.
    m.cursor_row = Some(focus);
    m.cwd = Some("/Users/_owner/clean".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert!(
        lines[0].starts_with("APPROVED seq=")
            && lines[0].contains(" cd /Users/_owner/aterm-wt-gap "),
        "{lines:?}"
    );
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{:?}", m.requests);
    assert!(
        presses[0].starts_with("key if-human=7 if-gen=1.")
            && presses[0].ends_with(&format!(" if={guard} 1")),
        "{presses:?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge: {:?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("\"rule_id\":\"allow-once@v1\"")
            && rows[0].contains("\"decision\":\"approved\"")
            && rows[0].contains("\"reason\":\"unproven: the rm circuit breaker (possibly-empty")
            && rows[0].contains("/Users/_owner/aterm-wt-gap-target"),
        "{rows:?}"
    );
    assert!(!rows[0].contains("is not a Bash header"), "{rows:?}");
    let _ = std::fs::remove_dir_all(&ldir);

    // `approve = "safe"` as the default reads it: the box the safe rules cannot prove is
    // pressed once with a `WARN` line naming its rule, never handed to a person.
    let mut cfg = SupervisorConfig::default();
    cfg.set("approve", "safe").expect("the limit");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&cfg)
    };
    let mut m = fenced(vec![bypass_busy(), owners_box(), busy_screen()]);
    m.cursor_row = Some(focus);
    m.cwd = Some("/Users/_owner/clean".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(
        lines.iter().any(|l| l.starts_with("WARN seq=")
            && l.contains("rule=rm-breaker-warn@v1")
            && l.contains("/Users/_owner/aterm-wt-gap-target")
            && l.contains("rm_breaker_ask = true")),
        "{lines:?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge: {:?}",
        m.requests
    );
}

/// An escalated box a person answers is cleared at the first busy read after it left (its
/// row leaving is the push that wakes the loop), never at the next idle point. Negative
/// control: a box nobody answers keeps its badge.
#[test]
fn an_escalated_box_a_person_answered_is_cleared_while_the_worker_stays_busy() {
    let none = SuperviseOpts {
        policy: SupervisorConfig {
            approve: Approve::None,
            ..auto(30, None).policy
        },
        ..auto(30, None)
    };
    let owners_box =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::BOX_RM_WORKFLOW);
    let mut m = Mock::new(vec![busy_screen(), owners_box.clone(), busy_screen()]);
    m.vanish_after = Some(3);
    let (lines, _) = watch_lines(&mut m, &none);
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    let at = |what: &str| {
        m.requests
            .iter()
            .position(|r| r.starts_with(what))
            .unwrap_or_else(|| panic!("no `{what}` in {:#?}", m.requests))
    };
    let set = at("meta set attention owner=supervisor claude rm-breaker:");
    let unset = at("meta unset attention owner=supervisor");
    assert!(set < unset, "{:#?}", m.requests);
    // After the escalation, the first read (busy) takes the badge down, before the busy turn's
    // own wait.
    let reads = m.requests[set + 1..unset]
        .iter()
        .filter(|r| r.starts_with("text "))
        .count();
    assert_eq!(reads, 1, "{:#?}", m.requests);
    assert_eq!(m.attention, None, "the badge is gone");
    assert!(
        m.requests[unset + 1..]
            .iter()
            .all(|r| !r.starts_with("meta set attention")),
        "raised once: {:#?}",
        m.requests
    );

    // Negative control: the box nobody answers keeps its badge.
    let mut m = Mock::new(vec![busy_screen(), owners_box]);
    m.vanish_after = Some(3);
    let (lines, _) = watch_lines(&mut m, &none);
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta unset attention")),
        "{:#?}",
        m.requests
    );
    assert!(m.attention.is_some(), "still badged");
}

/// [`auto`] at the default approval level, full power (its turn-end acts still limited).
fn full_power_opts(notes: Option<PathBuf>) -> SuperviseOpts {
    let mut o = auto(60, notes);
    o.policy.approve = Approve::All;
    o
}

/// What a full-power press names (the ledger row, `APPROVED`) is the decision's subject, not
/// the parser's command: a workflow launch by its title. (The trust backstop's subject is the
/// decision's: `approval_tests`.)
#[test]
fn full_power_ledgers_the_decisions_subject() {
    use aterm_phase::prompt::fixtures as f;
    let (ldir, ledger) = ledger_file("subject");
    let opts = full_power_opts(None);
    let mut m = fenced(vec![f::workflow_box(), busy_screen()]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(
        lines[0].starts_with("APPROVED seq=") && lines[0].ends_with(" Run a dynamic workflow?"),
        "{lines:?}"
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows[0].contains("\"command\":\"Run a dynamic workflow?\""),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// A ticking default-No box whose Enter is not taken is pressed once (the focus moved to
/// `Yes`, one fenced Enter, `vendor default was No` ledgered) and handed over: the moved focus
/// is not the box's identity. Negative control: a busy screen after it, not handed over.
#[test]
fn a_ticking_default_no_box_whose_enter_is_not_taken_is_pressed_once() {
    use aterm_phase::prompt::fixtures as f;
    let at = |left: &str, on_yes: bool| -> Vec<String> {
        let mut r = f::screen(f::BOX_TOOL_DEFAULT_NO);
        let q = r
            .iter()
            .position(|x| x == " Do you want to proceed?")
            .expect("the question");
        r.insert(
            q,
            format!(
                " ⚠ Claude Code will automatically deny this request in {left}, to avoid \
                 blocking progress on an unattended session"
            ),
        );
        if on_yes {
            for x in &mut r {
                if x == " ❯ No" {
                    *x = "   No".to_string();
                } else if x == "   Yes" {
                    *x = " ❯ Yes".to_string();
                }
            }
        }
        r
    };
    let p = aterm_phase::parse_prompt_v2(&at("1:59", false)).expect("the box");
    assert!(p.auto_deny.is_some(), "{p:?}");
    let (ldir, ledger) = ledger_file("default-no-ticking");
    let (dir, notes) = notes_file("default-no-ticking");
    let mut m = fenced(vec![
        at("1:59", false),
        at("1:58", true),
        at("1:57", true),
        at("1:56", true),
        at("1:55", true),
    ]);
    let mut s = session(&mut m, None);
    s.set_approval_ledger(Some(ledger.clone()));
    let (out, _) = s
        .supervise(&full_power_opts(Some(notes.clone())))
        .expect("supervise");
    let enters = m
        .requests
        .iter()
        .filter(|r| r.starts_with("key ") && r.ends_with(" enter"))
        .count();
    assert_eq!(enters, 1, "{:?}", m.requests);
    assert!(out.starts_with("prompt\n"), "{out}");
    let rows = ledger_rows(&ledger);
    let approved: Vec<&String> = rows
        .iter()
        .filter(|r| r.contains("\"decision\":\"approved\""))
        .collect();
    assert_eq!(approved.len(), 1, "{rows:?}");
    assert!(approved[0].contains("vendor default was No"), "{rows:?}");
    let lines = read_notes(&dir, &notes);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("handed to the manager (the box did not change after the press)")),
        "{lines:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);

    let (dir, notes) = notes_file("default-no-taken");
    let mut m = fenced(vec![at("1:59", false), at("1:58", true), busy_screen()]);
    m.vanish_after = Some(0);
    let _ = session(&mut m, None).supervise(&full_power_opts(Some(notes.clone())));
    let enters = m
        .requests
        .iter()
        .filter(|r| r.starts_with("key ") && r.ends_with(" enter"))
        .count();
    assert_eq!(enters, 1, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert!(
        !lines.iter().any(|l| l.contains("did not change")),
        "{lines:?}"
    );
}

// ---- the question answer in the loop (answer-recommended@v1) ------------

/// A host with the generation fence and the person stamp, no person ever
/// having keyed the session (`"human_ms":null`).
fn questioned(screens: Vec<Vec<String>>) -> Mock {
    let mut m = fenced(screens);
    m.human = vec!["null"];
    m
}

/// `help send` of a current host: its generation fence named (the person
/// fence, `if-human=`, rides the read's count: no probe).
const FENCED_SEND_HELP: &str =
    "send [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] [--] <text>: write text\n";

/// A fixture's rows.
fn q(text: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(text)
}

/// `rows` with the `❯` focus moved onto the first row whose trimmed text
/// starts with `prefix` (the option's `N. label`, `Next`).
fn focused_on(rows: &[String], prefix: &str) -> Vec<String> {
    let at = rows
        .iter()
        .position(|r| r.trim_start_matches('❯').trim_start().starts_with(prefix))
        .unwrap_or_else(|| panic!("no row {prefix:?}"));
    rows.iter()
        .enumerate()
        .map(|(k, r)| {
            let r = r
                .strip_prefix('❯')
                .map_or_else(|| r.clone(), |rest| format!(" {rest}"));
            if k == at {
                format!("❯{}", &r[1..])
            } else {
                r
            }
        })
        .collect()
}

/// The safe rules with the question answer on (a question is no permission, so no level
/// limits it) and a 10 s person's grace: the loop presses only the question answer.
fn answering(max_s: u64, notes: Option<PathBuf>) -> SuperviseOpts {
    let mut opts = auto(max_s, notes);
    opts.policy.answer_questions = true;
    opts.policy.human_grace_s = 10;
    opts
}

/// The row guard of a focused row as drawn.
fn guarded(row: &str) -> String {
    crate::supervise::policy::row_guard(row)
}

/// The four-tab question dialog's screens (S6-01 to S6-05), then an idle point.
fn four_tab_screens() -> Vec<Vec<String>> {
    use aterm_phase::prompt::fixtures as f;
    vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        q(f::QUESTION_TABS_THIRD),
        q(f::QUESTION_TABS_FOURTH),
        q(f::QUESTION_REVIEW),
        idle_screen(),
    ]
}

/// A RECORDED `supervise` over the four-tab question dialog: the `supervise`
/// entry's path crossed with the recorder on — the nesting rule's assertion
/// holding along it — changes nothing (the same requests, claims, output, code
/// and answered rows as unrecorded), and each box is marked on the tape as the
/// QUESTION it is (`decide.question`, five, each naming its rule and the
/// guarded Enter that followed), never as an approval. A question box marked
/// `decide.approval`, or an approval `decide.question`, fails here.
#[test]
fn a_recorded_supervise_marks_each_question_box_a_question() {
    let run = |tape: Option<&MemoryTape>| {
        let (ldir, ledger) = ledger_file("four-tabs-taped");
        let mut m = questioned(four_tab_screens());
        let opts = answering(30, None);
        let (out, code) = match tape {
            None => {
                let mut s = session(&mut m, None);
                s.set_approval_env(owner_env());
                s.set_approval_ledger(Some(ledger.clone()));
                s.supervise(&opts).expect("supervise")
            }
            Some(tape) => {
                let mut taped =
                    TapedCtl::new(&mut m, tape.fx.clone(), crate::supervise::tape::doors::CTL);
                let mut s = session(&mut taped, None);
                s.set_fx(tape.fx.clone());
                s.set_approval_env(owner_env());
                s.set_approval_ledger(Some(ledger.clone()));
                s.supervise(&opts).expect("supervise")
            }
        };
        let answered = ledger_rows(&ledger)
            .iter()
            .filter(|r| r.contains("\"rule_id\":\"answer-recommended@v1\""))
            .count();
        let _ = std::fs::remove_dir_all(&ldir);
        (out, code, m.requests, m.claims, answered)
    };
    let off = run(None);
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(Arc::default())));
    let on = run(Some(&tape));
    assert_eq!(on, off, "recording changes nothing the loop does");
    assert_eq!((off.1, off.4), (0, 5), "{}", off.0);
    let _ = tape.finish("exit 0");
    let read = crate::supervise::tape::show::open(&tape.sink.bytes(), false).expect("reads");
    let decisions = crate::supervise::tape::show::decisions(&read);
    let of = |door: &str| decisions.iter().filter(|d| d.door == door).count();
    use crate::supervise::tape::doors::{DECIDE_APPROVAL, DECIDE_QUESTION};
    assert_eq!(
        (of(DECIDE_QUESTION), of(DECIDE_APPROVAL)),
        (5, 0),
        "{decisions:#?}"
    );
    for d in decisions.iter().filter(|d| d.door == DECIDE_QUESTION) {
        assert_eq!(
            d.rule.as_deref(),
            Some(crate::supervise::policy::RULE_ANSWER_RECOMMENDED),
            "{d:#?}"
        );
        assert!(
            d.screen.is_some(),
            "the box it judged is on the tape: {d:#?}"
        );
        let act = d.act.as_ref().map(|w| w.join(" ")).unwrap_or_default();
        assert!(
            act.starts_with("key if-human=7 if-gen=1.") && act.ends_with(" enter"),
            "{d:#?}"
        );
    }
}

/// A four-tab question dialog (S6-01 to S6-05) is answered tab by tab: a fenced, guarded
/// Enter on each recommended option and the review's `Submit answers`, five
/// `answer-recommended@v1` rows, each answer's `UNPROVEN` and `DIALOG` journaled. Under
/// `recommended` (the default priority) nothing is typed and `help send` is never asked (only
/// a text word writes into the free-text row: `run_question_text_tests.rs`).
#[test]
fn a_four_tab_question_is_answered_tab_by_tab_and_submitted() {
    let (ldir, ledger) = ledger_file("four-tabs");
    let (jdir, journal) = journal_file("four-tabs");
    let mut m = questioned(four_tab_screens());
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.set_approval_ledger(Some(ledger.clone()));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..answering(30, None)
    };
    let (out, code) = s.supervise(&opts).expect("supervise");
    assert_eq!(code, 0, "{out}");
    let presses = m.presses();
    let rows = [
        "❯ 1. Outlined on meters (Recommended)",
        "❯ 1. Fade to grey (Recommended)",
        "❯ 1. Procedural (Recommended)",
        "❯ 1. Bottom (Recommended)",
        "❯ 1. Submit answers",
    ];
    assert_eq!(presses.len(), rows.len(), "{:?}", m.requests);
    for (p, row) in presses.iter().zip(rows) {
        assert!(p.starts_with("key if-human=7 if-gen=1."), "{p}");
        assert!(p.ends_with(&format!(" if={} enter", guarded(row))), "{p}");
    }
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")
                || r.starts_with("send ")
                || r.starts_with("help send")),
        "{:?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    let answered = rows
        .iter()
        .filter(|r| {
            r.contains("\"rule_id\":\"answer-recommended@v1\"")
                && r.contains("\"decision\":\"approved\"")
        })
        .count();
    assert_eq!(answered, 5, "{rows:?}");
    let (records, _) = journal_records(&journal);
    let of = |kind: &str| {
        records
            .iter()
            .filter(|r| {
                r.kind == kind && r.phase == crate::supervise::policy::RULE_ANSWER_RECOMMENDED
            })
            .count()
    };
    assert_eq!((of("unproven"), of("dialog")), (5, 5), "{records:?}");
    assert!(
        records.iter().any(|r| r.kind == "dialog"
            && r.summary.contains("│ On a row with a progress bar")
            && r.summary.contains("❯ 1. Outlined on meters (Recommended)")),
        "the incident's rows are kept: {records:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
    let _ = std::fs::remove_dir_all(&jdir);
}

/// A host without the generation fence never gets a question key: the box is handed over
/// naming why, after the stand-in quiet (`await idle 10000`: no person stamp) once.
#[test]
fn a_question_on_a_host_without_the_generation_fence_is_handed_over() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = Mock::new(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&SupervisorConfig::default())
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    // (`presses()` would count the badge: its reason names the key.)
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key ")),
        "{:?}",
        m.requests
    );
    assert_eq!(
        m.requests
            .iter()
            .filter(|r| r.starts_with("await idle 10000 "))
            .count(),
        1,
        "{:?}",
        m.requests
    );
    let set = m
        .requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("escalated");
    assert!(
        set.starts_with("claude question: On a row with a progress bar")
            && set.contains("cannot fence a question's key"),
        "{set}"
    );
}

/// The free-text row focused (S1-03): the focus moves up one row at a time, each move fenced,
/// guarded on the row it leaves and seen landing, then Enter on `❯ 1. Dark (Recommended)`.
/// Never a digit.
#[test]
fn the_free_text_row_is_left_by_arrows_then_enter_never_a_digit() {
    use aterm_phase::prompt::fixtures as f;
    let base = q(f::QUESTION_FREE_TEXT_FOCUSED);
    let mut m = questioned(vec![
        base.clone(),
        focused_on(&base, "3. High contrast"),
        focused_on(&base, "2. Light"),
        focused_on(&base, "1. Dark"),
        idle_screen(),
    ]);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    let keys: Vec<(&str, &str)> = m
        .presses()
        .iter()
        .map(|p| {
            let (head, key) = p.rsplit_once(' ').expect("a key");
            let guard = head.rsplit_once(" if=").expect("guarded").1;
            (guard, key)
        })
        .collect();
    let want = [
        (guarded("❯ 4. Type something."), "up"),
        (guarded("❯ 3. High contrast"), "up"),
        (guarded("❯ 2. Light"), "up"),
        (guarded("❯ 1. Dark (Recommended)"), "enter"),
    ];
    assert_eq!(
        keys,
        want.iter()
            .map(|(g, k)| (g.as_str(), *k))
            .collect::<Vec<_>>(),
        "{:?}",
        m.requests
    );
    assert!(
        m.presses()
            .iter()
            .all(|p| p.starts_with("key if-human=7 if-gen=1.")),
        "every key fenced: {:?}",
        m.presses()
    );
}

/// A person's text in the free-text row (S1-04): the question is
/// theirs — badged with the question and the reason, nothing keyed.
#[test]
fn a_person_typing_in_the_free_text_row_gets_the_question() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = questioned(vec![q(f::QUESTION_FREE_TEXT_TYPED)]);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&SupervisorConfig::default())
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let set = m
        .requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("escalated");
    assert!(
        set.starts_with("claude question: Which color scheme")
            && set.contains("a person has begun answering"),
        "{set}"
    );
}

/// On a multi-select, a check the supervisor did not make is a person's (even on a
/// recommended option), as is its own check undone: handed over, never toggled over. Shapes:
/// the S3-11 capture, a fresh tab a person checked, the loop's check undone.
#[test]
fn a_check_the_supervisor_did_not_make_hands_the_multiselect_over() {
    use aterm_phase::prompt::fixtures as f;
    let fresh = q(f::QUESTION_MULTISELECT);
    let persons_one: Vec<String> = fresh
        .iter()
        .map(|r| r.replace("❯ 1. [ ] Box drawing", "❯ 1. [✔] Box drawing"))
        .collect();
    assert_ne!(fresh, persons_one, "PRECONDITION: the check landed");
    for (screens, human, keys, why) in [
        (
            vec![q(f::QUESTION_MULTISELECT_CHECKED)],
            vec!["12000"],
            0,
            "option 1 is checked, and the supervisor did not check it",
        ),
        (
            vec![persons_one],
            vec!["12000"],
            0,
            "option 1 is checked, and the supervisor did not check it",
        ),
        (
            vec![fresh.clone(), focused_on(&fresh, "2. [ ] Emoji")],
            vec!["null", "12000"],
            1,
            "option 1, which the supervisor checked, reads unchecked",
        ),
    ] {
        let (dir, notes) = notes_file("q-not-its-check");
        let mut m = questioned(screens);
        m.human = human;
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.supervise(&answering(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(m.presses().len(), keys, "{why}: {:?}", m.requests);
        let lines = read_notes(&dir, &notes);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("handed to the manager") && l.contains(why)),
            "{why}: {lines:?}"
        );
    }
}

/// A question key is sent again only after the screen held still: on one that never does,
/// the box is handed over after [`MAX_UNSETTLED_WAITS`]; a retry is counted when written, so
/// one the fence skipped is tried again first.
#[test]
fn a_question_key_is_sent_again_only_after_the_screen_held_still_and_counted_when_written() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.idle_never = true;
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);

    // The retry skipped by the fence: tried again, then handed over.
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.key_replies.push_back(ok("OK seq=101\n"));
    m.key_replies
        .push_back(ok("OK skipped reason=changed seq=101\n"));
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    assert_eq!(
        m.presses().len(),
        3,
        "the Enter, the retry the fence skipped, the retry written: {:?}",
        m.requests
    );
}

/// A question key refused `ERR busy input-unread` is backed off, never re-sent at once or
/// escalated: the loop reads `status` and the box again and keys it once the worker reads.
#[test]
fn a_question_key_refused_busy_input_unread_is_backed_off_then_answered() {
    use aterm_phase::prompt::fixtures as f;
    let one = q(f::QUESTION_ONE);
    let mut m = questioned(vec![one.clone(), one, idle_screen()]);
    m.key_replies
        .push_back(err("busy input-unread bytes=1 wait_ms=1200"));
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    let presses = m.presses();
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert!(presses.iter().all(|p| p.ends_with(" enter")), "{presses:?}");
    let first = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .expect("keyed");
    assert!(
        m.requests[first + 1..]
            .iter()
            .take_while(|r| !r.starts_with("key "))
            .any(|r| r == "status"),
        "status is read before the box is keyed again: {:?}",
        m.requests
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "{:?}",
        m.requests
    );
}

/// A dialog drawn over a composer draft is answered with one fenced Enter, and the draft is
/// never submitted, backspaced or typed over.
#[test]
fn a_question_drawn_over_a_draft_is_answered_and_the_draft_left_alone() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = questioned(vec![
        q(f::QUESTION_OVER_DRAFT),
        q(f::QUESTION_OVER_DRAFT_AFTER),
    ]);
    m.vanish_after = Some(1);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    let _ = s.supervise(&answering(30, None));
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{:?}", m.requests);
    assert!(
        presses[0].ends_with(&format!(" if={} enter", guarded("❯ 1. Dark (Recommended)"))),
        "{presses:?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("send") || r.starts_with("turn") || r.contains("backspace")),
        "nothing typed into or taken from the person's draft: {:?}",
        m.requests
    );
}

/// Unattended (`watch`), a dialog that takes no key is keyed again for as long as it stands,
/// each try after the screen held still and the press back-off, badged once as information,
/// never handed over; under `approve = "safe"` too (a question is no permission).
#[test]
fn unattended_a_dialog_that_takes_no_key_is_tried_on_the_back_off_and_never_handed_over() {
    use aterm_phase::prompt::fixtures as f;
    let (jdir, journal) = journal_file("q-tried-again");
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.stall_sleep = Some(Duration::from_millis(50));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..answering(3, None)
    };
    assert_eq!(opts.policy.approve, Approve::Safe, "PRECONDITION");
    let _ = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_press_badge_after(Duration::from_millis(600));
    });
    let presses = m.presses();
    assert!(
        presses.len() >= 4,
        "tried again and again: {:#?}",
        m.requests
    );
    assert!(presses.iter().all(|p| p.ends_with(" enter")), "{presses:?}");
    let (records, _) = journal_records(&journal);
    assert!(
        records
            .iter()
            .any(|r| r.line.starts_with("WAITING ") && r.line.contains("(try ")),
        "{records:#?}"
    );
    let set: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention owner=supervisor"))
        .collect();
    assert_eq!(
        set.len(),
        1,
        "badged once, as information: {:#?}",
        m.requests
    );
    assert!(set[0].contains("still trying"), "{set:?}");
    assert!(
        !records
            .iter()
            .any(|r| r.line.contains("handed to the manager")),
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&jdir);
}

/// With nobody to hand it to, a screen that never holds still after the key is waited on the
/// press back-off and the unseen Enter is never sent again; nothing is handed over.
#[test]
fn unattended_a_screen_that_never_holds_still_is_waited_on_the_back_off_never_keyed_again() {
    use aterm_phase::prompt::fixtures as f;
    let (jdir, journal) = journal_file("q-never-still-watch");
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.idle_never = true;
    m.stall_sleep = Some(Duration::from_millis(20));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..answering(2, None)
    };
    let _ = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    assert_eq!(m.presses().len(), 1, "{:#?}", m.requests);
    let (records, _) = journal_records(&journal);
    assert!(
        records.iter().any(|r| r.line.starts_with("WAITING ")),
        "{records:#?}"
    );
    assert!(
        !records
            .iter()
            .any(|r| r.line.contains("handed to the manager")),
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&jdir);
}

/// The same question answered twice since the last review point is the model re-asking it:
/// its third return is answered again after a pause (`WAITING … re-asking it`), never handed
/// over.
#[test]
fn a_question_asked_a_third_time_is_answered_again_after_a_pause() {
    use aterm_phase::prompt::fixtures as f;
    let (jdir, journal) = journal_file("q-reask");
    let (dir, notes) = notes_file("q-reask");
    let one = q(f::QUESTION_ONE);
    // Its third return stands through the pause: read before it and after.
    let mut m = questioned(vec![
        one.clone(),
        q(f::QUESTION_TABS_FOURTH),
        one.clone(),
        q(f::QUESTION_TABS_FOURTH),
        one.clone(),
        one,
        idle_screen(),
    ]);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..answering(30, Some(notes.clone()))
    };
    s.supervise(&opts).expect("supervise");
    assert_eq!(m.presses().len(), 5, "{:?}", m.requests);
    let (records, _) = journal_records(&journal);
    let wait = records
        .iter()
        .position(|r| r.line.starts_with("WAITING ") && r.line.contains("re-asking it"))
        .unwrap_or_else(|| panic!("{records:#?}"));
    assert!(
        records[wait..]
            .iter()
            .any(|r| r.line.starts_with("APPROVED ")),
        "answered after the pause: {records:#?}"
    );
    let lines = read_notes(&dir, &notes);
    assert!(
        !lines.iter().any(|l| l.contains("handed to the manager")),
        "{lines:?}"
    );
    let _ = std::fs::remove_dir_all(&jdir);
}

/// The text press is never unfenced and its Enter never blind: a policy text on a host whose
/// `send` names no generation fence or whose reads carry no person count is `Unconfirmed`,
/// never retried; so is an Enter on own text the loop does not remember writing. Negative
/// controls: with both fences one `send` goes; the recommended option's Enter is pressed.
#[test]
fn the_text_press_refuses_a_host_that_cannot_fence_it_and_an_enter_it_did_not_earn() {
    use crate::supervise::policy::{Answer, AnswerTarget, QuestionPriority};
    use aterm_phase::prompt::fixtures as f;
    const GRACE: Duration = Duration::from_secs(10);
    let text = Answer::Type {
        steps: 0,
        text: QuestionPriority::LongTerm,
    };
    let press = |m: &mut Mock, answer: &Answer| {
        let mut s = session(m, None);
        let screen = s.screen().expect("a read");
        s.press_question(answer, &screen, GRACE)
            .expect("no transport failure")
    };
    // No `send if-gen=`, and — a `send` that names it — no person count.
    for with_send in [false, true] {
        let mut m = questioned(vec![q(f::QUESTION_283_FREE_TEXT_FOCUSED)]);
        m.human_seq = None;
        if with_send {
            m.help_send = Some(FENCED_SEND_HELP.to_string());
            m.human_seq = None;
        }
        match press(&mut m, &text) {
            Pressing::Unconfirmed { why, retry } => {
                assert!(why.contains("cannot fence a typed answer"), "{why}");
                assert!(!retry, "a text is never tried again");
            }
            _ => panic!("an unfenced text was not handed over"),
        }
        assert!(m.presses().is_empty(), "{:?}", m.requests);
        assert!(
            !m.requests.iter().any(|r| r.starts_with("send ")),
            "{:?}",
            m.requests
        );
    }
    // The field's own text, not remembered: no Enter.
    let own = aterm_phase::prompt::fixtures::rewrap_free_text(
        f::QUESTION_283_TYPED_LONG_TERM,
        crate::supervise::policy::question::LONG_TERM_TEXT,
        120,
    );
    let mut m = questioned(vec![own]);
    match press(
        &mut m,
        &Answer::FocusEnter {
            steps: 0,
            target: AnswerTarget::OwnText(QuestionPriority::LongTerm),
        },
    ) {
        Pressing::Unconfirmed { why, .. } => {
            assert!(why.contains("not one the supervisor remembers"), "{why}")
        }
        _ => panic!("an Enter on text the loop did not write was not handed over"),
    }
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    // A text the loop remembers writing into this question is never
    // written again, whatever the screen shows.
    let mut m = questioned(vec![q(f::QUESTION_283_FREE_TEXT_FOCUSED)]);
    m.help_send = Some(FENCED_SEND_HELP.to_string());
    m.human_seq = Some(0);
    {
        let mut s = session(&mut m, None);
        let screen = s.screen().expect("a read");
        let dialog = aterm_phase::parse_prompt_v2(&screen.rows)
            .and_then(|p| p.question_dialog)
            .expect("read whole");
        s.question.typed.insert(
            approval_loop::question_key(&dialog, &screen.rows),
            QuestionPriority::LongTerm,
        );
        match s
            .press_question(&text, &screen, GRACE)
            .expect("no transport failure")
        {
            Pressing::Unconfirmed { why, .. } => assert!(why.contains("already"), "{why}"),
            _ => panic!("a text written twice"),
        }
    }
    assert!(
        !m.requests.iter().any(|r| r.starts_with("send ")),
        "{:?}",
        m.requests
    );
    // Negative controls: a host that names the fences gets the text (one
    // send, never an Enter), and the recommended option is pressed.
    let mut m = questioned(vec![q(f::QUESTION_283_FREE_TEXT_FOCUSED)]);
    m.help_send = Some(FENCED_SEND_HELP.to_string());
    m.human_seq = Some(0);
    let _ = press(&mut m, &text);
    assert_eq!(
        m.requests.iter().filter(|r| r.starts_with("send ")).count(),
        1,
        "{:?}",
        m.requests
    );
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let mut m = questioned(vec![q(f::QUESTION_283_FREE_TEXT_FOCUSED)]);
    let _ = press(
        &mut m,
        &Answer::FocusEnter {
            steps: -2,
            target: AnswerTarget::Option(1),
        },
    );
    assert!(!m.presses().is_empty(), "{:?}", m.requests);
}

// ---- the session's own `questions` word, and the told choice -------------

/// The window's policy (`session_questions` on) with the global switch `global`.
fn hosted_answering(global: bool) -> SuperviseOpts {
    let policy = SupervisorConfig {
        answer_questions: global,
        ..SupervisorConfig::default()
    };
    SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&policy)
    }
}

/// The session's word over the global switch: `ask` hands the dialog over though the switch
/// answers; `recommended`, `long-term`, `short-term` answer though it is off; an unknown word
/// hands it over. Negative controls: unset, the switch decides; `session_questions` off.
#[test]
fn the_sessions_word_decides_its_question_dialog_over_the_switch() {
    use aterm_phase::prompt::fixtures as f;
    let run = |global: bool, word: Option<&'static str>, session_questions: bool| {
        let mut m = questioned(vec![q(f::QUESTION_ONE)]);
        m.vanish_after = Some(0);
        m.questions = word;
        let mut opts = hosted_answering(global);
        opts.policy.session_questions = session_questions;
        let _ = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
        let badge: Vec<String> = m
            .requests
            .iter()
            .filter_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
            .map(str::to_string)
            .collect();
        (m.presses().len(), badge)
    };
    let (keys, badge) = run(true, Some("ask"), true);
    assert_eq!(keys, 0, "the session's ask types nothing");
    assert!(
        badge
            .iter()
            .any(|b| b.contains("this session's `questions` word is ask")),
        "{badge:?}"
    );
    let (keys, _) = run(false, Some("recommended"), true);
    assert_eq!(
        keys, 1,
        "the session's recommended answers under a switch that is off"
    );
    // The priority words answer too under a switch that is off, by the recommended option:
    // this host's `help send` names no fence (`run_question_text_tests.rs` types them).
    for word in ["long-term", "short-term"] {
        assert_eq!(run(false, Some(word), true).0, 1, "{word}");
    }
    // A word this supervisor does not know is the person's, named in the badge.
    let (keys, badge) = run(true, Some("sometimes"), true);
    assert_eq!(keys, 0, "an unknown word types nothing");
    assert!(
        badge.iter().any(|b| b.contains(
            "this session's `questions` word sometimes is not one this supervisor knows"
        )),
        "{badge:?}"
    );
    // Negative controls.
    assert_eq!(run(true, None, true).0, 1, "unset: the switch answers");
    assert_eq!(run(false, None, true).0, 0, "unset: the switch hands over");
    let (keys, badge) = run(false, Some("recommended"), false);
    assert_eq!(keys, 0, "no session has a say under this policy");
    assert!(
        badge.iter().any(|b| b.contains("answer_questions is off")),
        "{badge:?}"
    );
}

/// Handed back: a dialog held for the person by the policy is answered once the word changes
/// after the badge went up, read again while the loop waits (`HANDBACK_POLL`), and told as the
/// policy's choice (`a_dialog_the_policy_held_to_its_end_is_not_told_as_its_choice`'s
/// control). Negative control: never handed back, never typed.
#[test]
fn a_dialog_held_by_the_policy_is_answered_when_handed_back() {
    use aterm_phase::prompt::fixtures as f;
    let run = |back: Option<Option<&'static str>>| {
        let mut m = questioned(vec![q(f::QUESTION_ONE)]);
        m.vanish_after = Some(2);
        m.questions = Some("ask");
        m.questions_after_badge = back;
        let _ = watch_lines_with(&mut m, &hosted_answering(true), |s| {
            s.set_approval_env(owner_env())
        });
        let badge = m
            .requests
            .iter()
            .position(|r| r.starts_with("meta set attention owner=supervisor "));
        let key = m.requests.iter().position(|r| r.starts_with("key "));
        let presses: Vec<String> = m.presses().into_iter().map(|p| p.to_string()).collect();
        (presses, badge, key)
    };
    for back in [None, Some("recommended")] {
        let (presses, badge, key) = run(Some(back));
        // The mock's dialog never leaves, so the Enter is retried once on the
        // screen that held still (R3a): one Enter, maybe twice.
        assert!(
            (1..=2).contains(&presses.len()),
            "handed back ({back:?}): {presses:?}"
        );
        assert!(presses.iter().all(|p| p.ends_with(" enter")), "{presses:?}");
        assert!(
            badge.is_some() && key > badge,
            "raised while it was the person's, typed only after: {badge:?} {key:?}"
        );
    }
    // Negative control.
    assert!(run(None).0.is_empty(), "never handed back");
}

/// The told choice (its sound and animation) is the policy's alone: a person's input after
/// the loop keyed the dialog makes its end theirs, so no `CHOSE` and no `story chose`.
#[test]
fn a_dialog_a_person_keyed_after_the_loop_is_not_told_as_its_choice() {
    use aterm_phase::prompt::fixtures as f;
    // The person keyed the dialog after the loop's key (their stamp young on
    // the second read): the end is theirs.
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        q(f::QUESTION_TABS_SECOND),
        busy_screen(),
        idle_screen(),
    ]);
    m.human = vec!["null", "2000", "12000"];
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines_with(&mut m, &hosted_answering(true), |s| {
        s.set_approval_env(owner_env())
    });
    assert!(!lines.iter().any(|l| l.starts_with("CHOSE ")), "{lines:#?}");

    // As the real server reports the person: the second tab's `status` sees them too, so the
    // answer is held (`HELD`); the loop answers after the hold, but the end is theirs. It stops
    // at the worker's first busy read, where a choice is told.
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        busy_screen(),
        idle_screen(),
    ]);
    m.human = vec!["null", TYPED_NOW, QUIET_PAST_GRACE];
    m.status_human_from = [(2, stamp_ms(TYPED_NOW)), (3, stamp_ms(QUIET_PAST_GRACE))]
        .into_iter()
        .collect();
    m.key_gates = vec![(1, 1)];
    m.stall_sleep = Some(Duration::from_millis(5));
    let (jdir, journal) = journal_file("tabs-person-held");
    let mut opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted_answering(true)
    };
    opts.policy.human_grace_s = PERSON_GRACE_S;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut out: Vec<u8> = Vec::new();
    {
        let mut ctl = StopAtBusy {
            mock: &mut m,
            stop: Some(stop.clone()),
        };
        let mut s = session(&mut ctl, None);
        s.set_approval_env(owner_env());
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_person_hold_wait(Duration::ZERO);
        let _ = s.run_hosted(&opts, stop, &mut out);
    }
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    assert!(
        records.iter().any(|r| r.line.starts_with("HELD ")),
        "held for the person: {records:#?}"
    );
    let lines = String::from_utf8(out).expect("utf-8");
    assert!(
        m.requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" enter"))
            .count()
            == 2,
        "both tabs answered: {:#?}",
        m.requests
    );
    assert!(!lines.contains("CHOSE "), "{lines}");
    assert!(
        !m.requests.iter().any(|r| r.contains("story chose")),
        "{:#?}",
        m.requests
    );
}

/// The person's grace in tests that hold the loop for a person (`[harness] human_grace_s`,
/// the default's two minutes). [`TYPED_NOW`] is inside it by the whole grace, so only a hang
/// ages it out; [`QUIET_PAST_GRACE`] is past it. The hold is looked at again at once
/// ([`Session::set_person_hold_wait`]), so no grace is waited out in real time.
const PERSON_GRACE_S: u32 = 120;
/// A person who typed just now ([`PERSON_GRACE_S`]).
const TYPED_NOW: &str = "0";
/// A person quiet past their grace ([`PERSON_GRACE_S`]).
const QUIET_PAST_GRACE: &str = "180000";

/// A person's stamp as `status` reports it (`human_ms=`).
fn stamp_ms(stamp: &str) -> u64 {
    stamp.parse().expect("an age in milliseconds")
}

/// [`Mock`] with the hosted loop's stop set the moment a read serves the worker busy: a
/// loop stopped mid-turn.
struct StopAtBusy<'m> {
    mock: &'m mut Mock,
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Ctl for StopAtBusy<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let r = self.mock.call(args)?;
        if args.first() == Some(&"text")
            && r.stdout.contains("esc to interrupt")
            && let Some(stop) = &self.stop
        {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(r)
    }
}

/// The told choice: `CHOSE`, its journal note and `story chose` go at the first busy read
/// after the loop's last key, naming that read's seq, also for a loop stopped there; on a
/// short reply (no read serves it busy), at the settled read showing the composer, before the
/// continuation. Negative control: told once, never again at the turn's end.
#[test]
fn a_choice_is_told_at_the_first_busy_read_and_before_a_stop() {
    use aterm_phase::prompt::fixtures as f;
    let run = |after: Vec<Vec<String>>, stop_at_busy: bool| {
        let mut screens = vec![q(f::QUESTION_ONE)];
        screens.extend(after);
        let mut m = questioned(screens);
        m.vanish_after = Some(1);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut out: Vec<u8> = Vec::new();
        {
            let mut ctl = StopAtBusy {
                mock: &mut m,
                stop: stop_at_busy.then(|| stop.clone()),
            };
            let mut s = session(&mut ctl, None);
            s.set_approval_env(owner_env());
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            let _ = s.run_hosted(&hosted_answering(true), stop, &mut out);
        }
        let lines: Vec<String> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        let enter = m
            .requests
            .iter()
            .position(|r| r.starts_with("key ") && r.ends_with(" enter"))
            .expect("the Enter");
        let stories: Vec<usize> = m
            .requests
            .iter()
            .enumerate()
            .filter(|(_, r)| r.contains("story chose"))
            .map(|(i, _)| i)
            .collect();
        let told: Vec<String> = lines
            .iter()
            .filter(|l| l.starts_with("CHOSE "))
            .cloned()
            .collect();
        assert_eq!(told.len(), 1, "{lines:#?}");
        (lines, told[0].clone(), enter, stories, m)
    };
    let busy = || vec![busy_screen(), busy_screen(), busy_screen(), idle_screen()];
    let (_, told, enter, stories, m) = run(busy(), false);
    assert_eq!(stories.len(), 1, "told once: {:#?}", m.requests);
    let story = stories[0];
    assert!(m.requests[story].ends_with("story chose recommended"));
    let reads_between = m.requests[enter..story]
        .iter()
        .filter(|r| r.starts_with("text "))
        .count();
    assert_eq!(
        reads_between, 1,
        "told at the first busy read: {:#?}",
        m.requests
    );
    assert!(told.starts_with("CHOSE seq=102 "), "{told}");

    // Stopped at that very read: told before the loop ends.
    let (lines, told, ..) = run(busy(), true);
    assert!(told.starts_with("CHOSE seq=102 "), "{told}");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("EXIT stopped"),
        "{lines:#?}"
    );

    // The short reply: told at the idle read, before the continuation.
    let (_, told, enter, stories, m) =
        run(vec![idle_screen(), busy_screen(), idle_screen()], false);
    assert_eq!(stories.len(), 1, "told once: {:#?}", m.requests);
    let story = stories[0];
    assert!(
        told.starts_with("CHOSE seq=102 rule=answer-recommended@v1 policy=recommended ")
            && told.ends_with("Which color scheme should the demo page use? → Dark (Recommended)"),
        "{told}"
    );
    let continued = m
        .requests
        .iter()
        .position(|r| r.starts_with("turn "))
        .unwrap_or(m.requests.len());
    assert!(
        enter < story && story < continued,
        "told at the idle read, before the continuation: {:#?}",
        m.requests
    );
}

/// A dialog the policy held to its end is not its choice: held at tab 2 (`ask`) and finished
/// by a controller, or answered while held, it gets no `CHOSE`. Negative control: handed back
/// as held, the loop finishes it and tells it once.
#[test]
fn a_dialog_the_policy_held_to_its_end_is_not_told_as_its_choice() {
    use aterm_phase::prompt::fixtures as f;
    let run = |m: &mut Mock| {
        m.questions = Some("recommended");
        m.vanish_after = Some(1);
        let (lines, _) = watch_lines_with(m, &hosted_answering(true), |s| {
            s.set_approval_env(owner_env())
        });
        lines
    };
    let chose = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|l| l.starts_with("CHOSE "))
            .cloned()
            .collect()
    };
    // Held at tab 2, and finished by a controller (no person stamp).
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        busy_screen(),
        idle_screen(),
    ]);
    m.questions_at = vec![(3, Some("ask"))];
    let lines = run(&mut m);
    assert_eq!(m.presses().len(), 1, "tab 1 only: {:#?}", m.requests);
    assert!(
        badge_of(&m).is_some_and(|b| b.contains("this session's `questions` word is ask")),
        "{:#?}",
        m.requests
    );
    assert!(chose(&lines).is_empty(), "{lines:#?}");
    assert!(
        !m.requests.iter().any(|r| r.contains("story chose")),
        "{:#?}",
        m.requests
    );

    // Tab 2 answered by someone else while held, then handed back: the loop
    // answers what is left, and tells none of it.
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        q(f::QUESTION_TABS_THIRD),
        busy_screen(),
        idle_screen(),
    ]);
    m.questions_at = vec![(3, Some("ask"))];
    m.questions_after_badge = Some(Some("recommended"));
    // The third tab sits until the loop keys it.
    m.key_gates = vec![(2, 1)];
    let lines = run(&mut m);
    assert_eq!(m.presses().len(), 2, "{:#?}", m.requests);
    assert!(chose(&lines).is_empty(), "{lines:#?}");

    // Negative control: handed back as held, the loop answers tab 2: the policy's dialog.
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        busy_screen(),
        idle_screen(),
    ]);
    m.questions_at = vec![(3, Some("ask"))];
    m.questions_after_badge = Some(Some("recommended"));
    // Tab 2 sits until the loop's second key.
    m.key_gates = vec![(1, 1)];
    let lines = run(&mut m);
    assert_eq!(m.presses().len(), 2, "{:#?}", m.requests);
    let told = chose(&lines);
    assert_eq!(told.len(), 1, "{lines:#?}");
    assert!(
        told[0].contains("policy=recommended ") && told[0].contains(" | "),
        "both tabs named: {}",
        told[0]
    );
}

/// The loop's own badge text (`meta set attention owner=supervisor …`).
fn badge_of(m: &Mock) -> Option<String> {
    m.requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .map(str::to_string)
}

// ---- `aterm drive answer`: a controller's own choice ----------------------

/// Run `answer` once over `m`: the printed line and the exit code.
fn controller_answer(m: &mut Mock, answer: &str, box_token: Option<&str>) -> (String, u8) {
    let mut out: Vec<u8> = Vec::new();
    let mut s = Session::new(m, Some("@s-w".to_string()));
    let code = s
        .answer(
            &crate::supervise::AnswerOpts {
                answer: answer.to_string(),
                box_token: box_token.map(str::to_string),
                grace: Duration::from_secs(120),
            },
            &mut out,
        )
        .expect("answer");
    (String::from_utf8(out).expect("utf-8"), code)
}

/// A controller's own choice: `answer @w 2` moves the focus one row (guarded, fenced), sees it
/// land and presses Enter guarded on `❯ 2. Light`; never a digit. `--box` with another token
/// is refused, as is a free-text answer; `human` types nothing; no dialog is NO-BOX.
#[test]
fn a_controllers_answer_moves_the_focus_and_presses_enter_on_its_choice() {
    use aterm_phase::prompt::fixtures as f;
    let one = q(f::QUESTION_ONE);
    let token = crate::supervise::run::question_box_token(
        &aterm_phase::parse_prompt_v2(&one)
            .and_then(|p| p.question_dialog)
            .expect("read whole"),
    );
    let mut m = questioned(vec![one.clone(), focused_on(&one, "2."), idle_screen()]);
    let (line, code) = controller_answer(&mut m, "2", Some(&token));
    assert_eq!(
        (line.as_str(), code),
        ("ANSWERED @s-w enters=1 answer=Light next=none\n", 0),
        "{:#?}",
        m.requests
    );
    let keys: Vec<&String> = m.requests.iter().filter(|r| r.contains("key ")).collect();
    assert_eq!(keys.len(), 2, "{keys:#?}");
    assert!(
        keys[0].ends_with(&format!(" if={} down", guarded("❯ 1. Dark (Recommended)"))),
        "{}",
        keys[0]
    );
    assert!(
        keys[1].ends_with(&format!(" if={} enter", guarded("❯ 2. Light"))),
        "{}",
        keys[1]
    );
    assert!(
        keys.iter().all(|k| k.contains("if-gen=")),
        "fenced: {keys:#?}"
    );

    // Negative controls: nothing typed.
    for (answer, want, code) in [
        ("2", "REFUSED @s-w ", 2),
        ("make it pop", "REFUSED @s-w ", 2),
        ("human", "LEFT @s-w ", 0),
    ] {
        let mut m = questioned(vec![one.clone()]);
        let box_token = (answer == "2").then_some("deadbeef");
        let (line, c) = controller_answer(&mut m, answer, box_token);
        assert!(line.starts_with(want), "{answer}: {line}");
        assert_eq!(c, code, "{answer}");
        assert!(
            !m.requests.iter().any(|r| r.contains("key ")),
            "{answer}: {:#?}",
            m.requests
        );
    }
    let mut m = questioned(vec![idle_screen()]);
    let (line, c) = controller_answer(&mut m, "1", None);
    assert!(line.starts_with("NO-BOX @s-w"), "{line}");
    assert_eq!(c, 1);
    // A text priority is the window's, never a controller's answer: REFUSED
    // naming the session word, nothing typed or sent.
    let mut m = questioned(vec![one]);
    let (line, c) = controller_answer(&mut m, "long-term", None);
    assert!(line.starts_with("REFUSED @s-w "), "{line}");
    assert_eq!(c, 2);
    assert!(
        !m.requests
            .iter()
            .any(|r| r.contains("key ") || r.starts_with("send ")),
        "{:#?}",
        m.requests
    );
}

/// A controller's answer to MUSE CODE's question (`status program=muse`, 1.4.10): read by its
/// reader, by its answers' roles, and chosen as the window chooses it — by the cursor and
/// Enter, never a digit (one only moves its cursor, measured): `2` moves the focus one row
/// down, guarded on the question's own row and fenced on the generation, sees it land on
/// `Blue`, then presses Enter
/// guarded on that row; `recommended` is Enter on the focused `Red (Recommended)` alone.
/// Negative controls: a person who keyed within the grace gets it (LEFT, nothing typed), and
/// `3` (`None of the above`, the person's own words) is refused with nothing typed.
#[test]
fn a_controllers_answer_to_a_muse_question_moves_the_focus_and_presses_enter() {
    use aterm_phase::muse::fixtures as u;
    let question_row = guarded("  Which color should the probe use?");
    for (answer, screens, keys, said) in [
        (
            "2",
            vec![q(u::QUESTION), q(u::QUESTION_DOWN), q(u::QUESTION_ANSWERED)],
            vec![
                format!(" if={question_row} down"),
                format!(
                    " if={} enter",
                    guarded("  ⟩ 2. Blue               Cool and calm.")
                ),
            ],
            "Blue",
        ),
        (
            "recommended",
            vec![q(u::QUESTION), q(u::QUESTION_ANSWERED)],
            vec![format!(
                " if={} enter",
                guarded("  ⟩ 1. Red (Recommended)  Warm and loud.")
            )],
            "Request user input Color → Red (Recommended)",
        ),
    ] {
        let mut m = questioned(screens);
        m.program = "muse";
        m.human_seq = Some(7);
        let (line, code) = controller_answer(&mut m, answer, None);
        assert_eq!(
            (line.as_str(), code),
            (
                format!("ANSWERED @s-w enters=1 answer={said} next=none\n").as_str(),
                0
            ),
            "{answer}: {:#?}",
            m.requests
        );
        let pressed: Vec<&String> = m.presses();
        assert_eq!(pressed.len(), keys.len(), "{answer}: {pressed:#?}");
        for (press, want) in pressed.iter().zip(&keys) {
            assert!(
                press.contains("if-human=7 ")
                    && press.contains(" if-gen=")
                    && press.ends_with(want.as_str()),
                "{answer}: {press} / {want}"
            );
        }
    }
    for (answer, human, want, code) in [
        ("2", "1500", "LEFT @s-w a person gave the session input", 0),
        (
            "3",
            "null",
            "REFUSED @s-w \"None of the above\" asks for the person's own words",
            2,
        ),
    ] {
        let mut m = questioned(vec![q(u::QUESTION)]);
        m.program = "muse";
        m.human_seq = Some(7);
        m.human = vec![human];
        let (line, c) = controller_answer(&mut m, answer, None);
        assert!(line.starts_with(want), "{answer}: {line}");
        assert_eq!(c, code, "{answer}: {line}");
        assert!(m.presses().is_empty(), "{answer}: {:#?}", m.requests);
    }
    // Neither an absent person epoch nor an unreadable quiet stamp licenses a key.
    for (what, person, human, reason) in [
        (
            "missing person count",
            None,
            vec!["null"],
            "no person count",
        ),
        ("unknown quiet stamp", Some(7), vec![], "no quiet stamp"),
    ] {
        let mut m = questioned(vec![q(u::QUESTION), q(u::QUESTION_ANSWERED)]);
        m.program = "muse";
        m.human_seq = person;
        m.human = human;
        let (line, code) = controller_answer(&mut m, "recommended", None);
        assert!(
            line.starts_with("NOT-SERVED @s-w ") && line.contains(reason),
            "{what}: {line}"
        );
        assert_eq!(code, 3, "{what}: {line}");
        assert!(m.presses().is_empty(), "{what}: {:#?}", m.requests);
    }
}

/// MUSE CODE'S QUESTION HEAD COUNTS ITS SECONDS (`— running (6s)`), so its screen moves once a
/// second and a key fenced on a read can miss that tick (measured live: `aterm drive answer`
/// answered `NOT-SERVED … changed` once). A key the generation skipped is tried again on a
/// fresh read of the same question, the answer planned on it: one tick between the read and
/// the key, and the answer still lands (`ANSWERED`, the second Enter). NEGATIVE CONTROL: a
/// screen that moves under every key is `NOT-SERVED` after three, nothing more sent.
#[test]
fn a_controllers_answer_to_a_muse_question_outlasts_its_ticking_head() {
    use aterm_phase::muse::fixtures as u;
    for (ticks, want, keys) in [
        (
            1,
            "ANSWERED @s-w enters=1 answer=Request user input Color → Red (Recommended) next=none",
            2,
        ),
        (
            3,
            "NOT-SERVED @s-w the question changed under the key (a person, or the worker) \
             (enters=0)",
            3,
        ),
    ] {
        let mut screens = vec![q(u::QUESTION); ticks + 1];
        screens.push(q(u::QUESTION_ANSWERED));
        let mut m = questioned(screens);
        m.program = "muse";
        m.human_seq = Some(7);
        m.tick_at_key = ticks;
        let (line, _) = controller_answer(&mut m, "recommended", None);
        assert_eq!(line.trim_end(), want, "{ticks}: {:#?}", m.requests);
        assert_eq!(m.presses().len(), keys, "{ticks}: {:#?}", m.requests);
    }
}

/// A controller's `recommended` on a multi-select answers the whole tab: both recommended
/// options toggled, then `Next` (the explicit `1, 3`'s seven fenced keys), and `ANSWERED …
/// enters=3 … next=<the next question>`.
#[test]
fn a_controllers_recommended_on_a_multi_select_answers_the_whole_tab() {
    use aterm_phase::prompt::fixtures as f;
    let screens = || {
        let fresh = q(f::QUESTION_MULTISELECT);
        let one: Vec<String> = fresh
            .iter()
            .map(|r| r.replace("❯ 1. [ ] Box drawing", "❯ 1. [✔] Box drawing"))
            .collect();
        let checked = q(f::QUESTION_MULTISELECT_CHECKED);
        vec![
            fresh,
            one.clone(),
            focused_on(&one, "2. [ ] Emoji"),
            focused_on(&one, "3. [ ] Powerline"),
            checked.clone(),
            focused_on(&checked, "4. [ ] Type something"),
            q(f::QUESTION_MULTISELECT_NEXT),
            q(f::QUESTION_TABS_FOURTH),
        ]
    };
    let mut m = questioned(screens());
    let (line, code) = controller_answer(&mut m, "recommended", None);
    assert_eq!(code, 0, "{line}");
    assert!(
        line.starts_with(
            "ANSWERED @s-w enters=3 answer=Which glyph sets should be bundled? → Next (Box \
             drawing (Recommended), Powerline (Recommended)) next="
        ) && !line.contains("next=Which glyph sets"),
        "{line}\n{:#?}",
        m.requests
    );
    let keys: Vec<String> = m.presses().into_iter().cloned().collect();
    assert_eq!(keys.len(), 7, "{keys:#?}");
    assert!(
        keys.last()
            .is_some_and(|k| k.ends_with(&format!(" if={} enter", guarded("❯    Next")))),
        "{keys:#?}"
    );
    // Negative control: the explicit answer naming them keys the same.
    let mut m = questioned(screens());
    let (line, _) = controller_answer(&mut m, "1, 3", None);
    assert!(line.starts_with("ANSWERED @s-w enters=3 "), "{line}");
    let explicit: Vec<String> = m.presses().into_iter().cloned().collect();
    assert_eq!(keys, explicit);
}

// ---- Tier-1: the loop bound to `SupervisorQuestionAnswer` ---------------

use std::collections::BTreeSet;

type QState = BTreeMap<&'static str, i64>;

/// The environment's actions in the question answer's machine: Claude Code reading the
/// oldest key (the policy text whole or its head dropped; a key read after the dialog closed
/// is `KeyRefused`), a person keying (their own copy of the policy text among it), their
/// quiet returning, and a tab falling back under a text word.
const QUESTION_ENV: [&str; 17] = [
    "OpensOffChoice",
    "FitLostOnIt",
    "FitLostOffIt",
    "PersonKey",
    "QuietReturns",
    "PersonMovesOff",
    "PersonMovesOnto",
    "PersonTypes",
    "PersonTypesPolicyText",
    "PersonAnswers",
    "PersonKeyIgnored",
    "MoveLands",
    "MoveShort",
    "EnterTaken",
    "TextLands",
    "TextPartly",
    "KeyRefused",
];

/// [`QUESTION_ENV`]'s actions that `model` has: the recommended path's model leaves out the
/// policy text's.
fn q_env(model: &aterm_spec::derive::Model) -> impl Iterator<Item = &'static str> + '_ {
    QUESTION_ENV
        .into_iter()
        .filter(|a| model.actions.iter().any(|b| b.name == *a))
}

/// A read as [`q_projected`] sees it: tab, focus, parity, `typed`, `own`, `exact`, and the
/// fallback where the read's decision says (`None`: a hand-over).
type QRead = (i64, i64, i64, i64, i64, i64, Option<i64>);

/// A read as the model sees it: the tab (chips answered plus a multi-select's checked
/// options, each toggle a tab in the model, less `offset`; `Tabs` the review, `Tabs + 1` no
/// dialog), the focus (0 on the row the real decider chooses on this read, else 1), its
/// focus-order parity, others' text in the free-text row (`typed`), the supervisor's own
/// (`own`), a person's exact copy of the policy text (`exact`), and whether the tab falls
/// back to its recommended option (`question::falls_back`).
///
/// Under a text priority, `text` carries the questions a `send` was really made on (from the
/// requests, never the loop's memory): `own` is the field holding exactly the priority's text
/// on such a question, `exact` the same text on any other. A multi-select the text goes to is
/// one tab (its chip ticking itself is no tab answered); one that falls back toggles its
/// recommended options, each a tab.
fn q_projected(
    rows: &[String],
    tabs: i64,
    offset: i64,
    text: Option<(
        crate::supervise::policy::QuestionPriority,
        &BTreeSet<String>,
    )>,
) -> QRead {
    use crate::supervise::policy::QuestionPriority;
    use crate::supervise::policy::approval::{Choice, Decision};
    use crate::supervise::policy::question::{answer_question, target_focus};
    use aterm_phase::{QuestionFocus, QuestionForm};
    let Some(p) = aterm_phase::parse_prompt_v2(rows) else {
        return (tabs + 1, 0, 0, 0, 0, 0, None);
    };
    let Some(d) = p.question_dialog.as_ref() else {
        return (tabs + 1, 0, 0, 0, 0, 0, None);
    };
    let focus = d.focus();
    let review = matches!(d.form, QuestionForm::Review { .. });
    let (priority, memory) = match text {
        Some((priority, sent)) => (
            priority,
            d.question().is_some_and(|q| sent.contains(&q.text)),
        ),
        None => (QuestionPriority::Recommended, false),
    };
    let free = match &d.form {
        QuestionForm::Single { free_text, .. } | QuestionForm::Multi { free_text, .. } => {
            Some(free_text)
        }
        _ => None,
    };
    let multi = matches!(d.form, QuestionForm::Multi { .. });
    let own = text.is_some()
        && memory
        && free.is_some_and(|f| {
            !f.pristine
                && Some(f.text.as_str()) == priority.text()
                && (!multi || f.checked == Some(true))
        });
    // A text answer toggles nothing (a checked option on its tab is handed
    // over); a tab that falls back toggles as the recommended path does.
    let checked = match &d.form {
        QuestionForm::Multi { options, .. } => {
            options.iter().filter(|o| o.checked == Some(true)).count()
        }
        _ => 0,
    };
    // The active multi-select chip ticks itself as a text lands in its row
    // (c4-09): that is not a tab answered.
    let self_ticked =
        usize::from(text.is_some() && multi && free.is_some_and(|f| f.checked == Some(true)));
    let tab = if review {
        tabs
    } else {
        i64::try_from(d.tabs.iter().filter(|t| t.answered).count() + checked - self_ticked)
            .expect("tabs")
            - offset
    };
    let (chosen, fallback) = match answer_question(
        &p,
        rows,
        priority,
        memory.then_some(priority),
        false,
        crate::supervise::policy::question::Pane::of(rows),
    ) {
        Decision::Approve {
            choice: Choice::Answer(answer),
            ..
        } => (
            match answer.enter_target() {
                Some(target) => focus == target_focus(target),
                // The policy text goes to the free-text row.
                None => focus == QuestionFocus::FreeText,
            },
            Some(i64::from(
                text.is_some() && crate::supervise::policy::question::falls_back(d, &answer),
            )),
        ),
        // Escalated: the row the decider would choose were it answered: the free-text row
        // under a text priority, else option 1 (the review's submit).
        _ if text.is_some() && !review => (focus == QuestionFocus::FreeText, None),
        _ => (
            matches!(
                focus,
                QuestionFocus::Option(1) | QuestionFocus::ReviewSubmit
            ),
            None,
        ),
    };
    let pos = crate::supervise::policy::question::focus_position(d, focus).expect("a focus");
    let typed = match (&d.form, text) {
        (QuestionForm::Single { free_text, .. }, _)
        | (QuestionForm::Multi { free_text, .. }, Some(_)) => {
            i64::from(!free_text.pristine && !own)
        }
        _ => 0,
    };
    let exact = text.is_some()
        && !own
        && free.is_some_and(|f| !f.pristine && Some(f.text.as_str()) == priority.text());
    (
        tab,
        i64::from(!chosen),
        i64::from(pos % 2),
        typed,
        i64::from(own),
        i64::from(exact),
        if review { Some(0) } else { fallback },
    )
}

/// The fewest [`QUESTION_ENV`] actions from `st` to a state that draws `want` with the
/// person's quiet `quiet`; a person keys only where the read's stamp says one did
/// (`new_input`). Panics when none does: the loop saw what the model cannot reach.
fn q_explained(
    model: &aterm_spec::derive::Model,
    st: &QState,
    want: QRead,
    quiet: i64,
    new_input: bool,
    tabs: i64,
) -> QState {
    let draws = |s: &QState| {
        if want.0 == tabs + 1 {
            s["tab"] == tabs + 1
        } else {
            let (tab, focus, row, typed, own, exact, fallback) = want;
            (
                s["tab"], s["focus"], s["row"], s["typed"], s["own"], s["exact"],
            ) == (tab, focus, row, typed, own, exact)
                && fallback.is_none_or(|f| s["fallback"] == f)
        }
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = VecDeque::from([(st.clone(), 0)]);
    while let Some((s, depth)) = queue.pop_front() {
        if draws(&s) && s["quiet"] == quiet {
            return s;
        }
        if depth == 7 || !seen.insert(s.clone()) {
            continue;
        }
        for a in q_env(model) {
            if (a == "PersonKey" && !new_input) || (a == "QuietReturns" && quiet == 0) {
                continue;
            }
            let mut next = s.clone();
            if model.fire(a, &mut next) {
                queue.push_back((next, depth + 1));
            }
        }
    }
    panic!("no behaviour the model allows explains the read {want:?} (quiet {quiet}) from {st:?}");
}

/// One scenario of the bind: the real loop over `m` projected onto `model` (the committed one
/// or a variant of its constants), the dialog's tabs counted from `offset`. Returns the end
/// state, the keys sent, and the state at the first read that showed a person.
fn q_bound_on(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    offset: i64,
) -> (QState, Vec<String>, Option<QState>) {
    q_bound_with(m, model, offset, None, &answering(30, None), |_| {})
}

/// [`q_bound_on`] under `opts`, the session set up by `setup`, and the text priority `text`:
/// each `send` is `HarnessType`, must be enabled where it goes, and carries the generation,
/// row and person fences with exactly that priority's text.
fn q_bound_with(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    offset: i64,
    text: Option<crate::supervise::policy::QuestionPriority>,
    opts: &SuperviseOpts,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (QState, Vec<String>, Option<QState>) {
    {
        let mut s = session(m, None);
        s.set_approval_env(owner_env());
        setup(&mut s);
        s.supervise(opts).expect("supervise");
    }
    q_projected_run_as(m, model, offset, text)
}

/// [`q_bound_on`] over a loop that has already run on `m` — `supervise`'s
/// one look, or `watch` with nobody to hand the box to.
fn q_projected_run(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    offset: i64,
) -> (QState, Vec<String>, Option<QState>) {
    q_projected_run_as(m, model, offset, None)
}

/// [`q_projected_run`] under the text priority `text`, if any.
fn q_projected_run_as(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    offset: i64,
    text: Option<crate::supervise::policy::QuestionPriority>,
) -> (QState, Vec<String>, Option<QState>) {
    let tabs = model
        .consts
        .iter()
        .find(|(n, _)| *n == "Tabs")
        .map(|(_, v)| *v)
        .expect("Tabs");
    let mut st = model.init_state();
    let mut reads = m.reads.iter();
    let mut last_stamp: Option<u64> = None;
    let mut keys = Vec::new();
    let mut person_seen = None;
    let mut sent: BTreeSet<String> = BTreeSet::new();
    let mut last_question: Option<String> = None;
    for (at, (req, _)) in m.order.iter().enumerate() {
        if m.person_skipped.contains(&at) {
            // A person keyed between the read and this write and the person fence skipped it
            // (`reason=person`): `PersonKey`, after which no harness key or text is enabled.
            assert!(model.fire("PersonKey", &mut st), "PersonKey at {st:?}");
            for action in ["HarnessMove", "HarnessEnter", "HarnessType"] {
                if !model.actions.iter().any(|a| a.name == action) {
                    continue;
                }
                assert!(
                    !model.action_enabled(action, &st),
                    "the model allows {action} past a person's key: {st:?}"
                );
            }
            if person_seen.is_none() {
                person_seen = Some(st.clone());
            }
            continue;
        }
        if req.starts_with("text ") {
            let (i, stamp) = *reads.next().expect("a served read");
            let age = stamp.parse::<u64>().ok();
            let quiet = i64::from(age.is_none_or(|ms| ms >= 10_000));
            let new_input = age.is_some_and(|ms| last_stamp.is_none_or(|was| ms < was));
            last_stamp = age.or(last_stamp);
            let want = q_projected(&m.screens[i], tabs, offset, text.map(|p| (p, &sent)));
            last_question = aterm_phase::parse_prompt_v2(&m.screens[i])
                .and_then(|p| p.question_dialog)
                .and_then(|d| d.question().map(|q| q.text.clone()));
            st = q_explained(model, &st, want, quiet, new_input, tabs);
            if want.0 <= tabs {
                assert!(model.fire("Read", &mut st), "Read at {st:?}");
            }
            if quiet == 0 && person_seen.is_none() {
                person_seen = Some(st.clone());
            }
        } else if req.starts_with("send ") {
            let p = text.expect("a policy text sent under a text priority only");
            let (head, body) = req.split_once(" -- ").expect("send … -- <text>");
            assert_eq!(
                Some(body),
                p.text(),
                "the send's body is the policy text: {req}"
            );
            for fence in ["if-human=", "if-gen=", "if="] {
                assert!(
                    head.split(' ').any(|w| w.starts_with(fence)),
                    "the policy text is written under {fence}: {req}"
                );
            }
            assert!(
                model.action_enabled("HarnessType", &st),
                "the real loop sent {req:?} where the model forbids HarnessType: {st:?}"
            );
            assert!(model.fire("HarnessType", &mut st));
            sent.extend(last_question.clone());
            keys.push("text".to_string());
        } else if req.starts_with("key ") {
            let key = req.rsplit_once(' ').expect("a key").1;
            // The model's move has no direction: one row toward the chosen
            // row, `↑` or `↓`.
            let action = match key {
                "up" | "down" => "HarnessMove",
                "enter" => "HarnessEnter",
                other => panic!("a question keyed with {other:?}: {req}"),
            };
            assert!(
                model.action_enabled(action, &st),
                "the real loop sent {req:?} where the model forbids {action}: {st:?}"
            );
            assert!(model.fire(action, &mut st));
            keys.push(key.to_string());
        } else if req.starts_with("await idle 2000 ") && st["mine"] == 1 && st["fresh"] == 1 {
            // The screen held still 2 s after a read showed the box as
            // keyed: by the model's premise, the key was read by then.
            if st["q1"] >= 2 {
                assert!(model.fire("KeyRefused", &mut st), "{st:?}");
            }
            assert!(model.fire("StillScreen", &mut st), "StillScreen at {st:?}");
        }
        for inv in &model.invariants {
            assert!(
                model.check_invariant(inv.name, &st),
                "{} at {st:?}",
                inv.name
            );
        }
    }
    (st, keys, person_seen)
}

/// [`q_bound_on`] with the committed model and the four-tab dialog from its
/// third tab (two chips answered).
fn q_bound(m: &mut Mock) -> (QState, Vec<String>, Option<QState>) {
    q_bound_on(
        m,
        &aterm_models_harness::supervisor_question_answer_model(),
        2,
    )
}

/// The round trip: a person's key between the loop's last read and its question key is
/// skipped by the person fence (`if-human=<n>`), the model's `PersonKey`. Negative controls:
/// the model without the fence (`PersonFence = 0`) keys there; a host sending no count gets none.
#[test]
fn a_persons_key_between_the_read_and_the_write_skips_the_question_key() {
    use aterm_phase::prompt::fixtures as f;
    let model = aterm_models_harness::supervisor_question_answer_model();
    let third = q(f::QUESTION_TABS_THIRD);
    let on_free = focused_on(&third, "3. Type something");
    let on_two = focused_on(&third, "2. Font");
    // The first `↑` is raced; the person's own key moved the focus (the
    // next read shows it on option 2), and the answer goes on from there.
    let screens = || {
        vec![
            on_free.clone(),
            on_two.clone(),
            third.clone(),
            q(f::QUESTION_TABS_FOURTH),
            q(f::QUESTION_REVIEW),
            idle_screen(),
        ]
    };
    let mut m = questioned(screens());
    m.human = vec!["15000"];
    m.human_seq = Some(4);
    m.person_at_key = 1;
    let (_, keys, raced) = q_bound(&mut m);
    let fenced: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("key "))
        .collect();
    assert!(
        fenced
            .first()
            .is_some_and(|r| r.starts_with("key if-human=4 if-gen=")),
        "{fenced:#?}"
    );
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    assert!(
        keys.len() < fenced.len(),
        "the skipped key is not counted as written"
    );
    let raced = raced.expect("the person's key is in the bind");
    let unfenced = aterm_spec::interp::with_consts(&model, &[("PersonFence", 0)]);
    assert!(
        unfenced.action_enabled("HarnessMove", &raced),
        "without the fence the model allows the key the server refused: {raced:?}"
    );
    // Negative control: a host that sends no count gets no person fence.
    let mut m = questioned(screens());
    m.human = vec!["15000"];
    m.human_seq = None;
    let _ = q_bound(&mut m);
    assert!(
        m.requests
            .iter()
            .filter(|r| r.starts_with("key "))
            .all(|r| !r.contains("if-human=")),
        "{:#?}",
        m.requests
    );
}

/// Tier-1 for `SupervisorQuestionAnswer` (aterm-spec `supervisor_question_answer_model`): the
/// real loop projected request by request (each read `Read` after the fewest environment
/// actions that explain it, each key `HarnessMove`/`HarnessEnter`, enabled where it goes):
///
/// 1. The free-text row a person left focused, then quiet: `↑ ↑` Enter, Enter, Enter.
/// 2. A person keys while the focus is moved: nothing more until they are quiet; the
///    fence-only model (`Buggy = 1`) would key there.
/// 3. A dialog that takes no key: two Enters in one look (`MaxRetry = 1`); unattended, more.
#[test]
fn tier1_a_question_key_goes_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures as f;
    let model = aterm_models_harness::supervisor_question_answer_model();
    let third = q(f::QUESTION_TABS_THIRD);
    let on_free = focused_on(&third, "3. Type something");
    let on_two = focused_on(&third, "2. Font");

    // 1. Off the free-text row, then every tab and the review.
    let mut m = questioned(vec![
        on_free.clone(),
        on_two.clone(),
        third.clone(),
        q(f::QUESTION_TABS_FOURTH),
        q(f::QUESTION_REVIEW),
        idle_screen(),
    ]);
    m.human = vec!["15000"];
    let (st, keys, _) = q_bound(&mut m);
    assert_eq!(
        keys,
        ["up", "up", "enter", "enter", "enter"],
        "{:?}",
        m.requests
    );
    assert_eq!(st["tab"], 3, "the dialog closed: {st:?}");

    // 2. A person keys mid-answer.
    let mut m = questioned(vec![
        on_free,
        on_two.clone(),
        on_two.clone(),
        on_two,
        third.clone(),
        q(f::QUESTION_TABS_FOURTH),
        q(f::QUESTION_REVIEW),
        idle_screen(),
    ]);
    m.human = vec!["15000", "400", "400", "12000"];
    let (st, keys, person) = q_bound(&mut m);
    assert_eq!(
        keys,
        ["up", "up", "enter", "enter", "enter"],
        "{:?}",
        m.requests
    );
    assert_eq!(st["tab"], 3, "{st:?}");
    let person = person.expect("a read showed the person");
    assert!(!model.action_enabled("HarnessMove", &person), "{person:?}");
    assert!(
        aterm_spec::interp::with_buggy(&model, 1).action_enabled("HarnessMove", &person),
        "the fence alone would have keyed there: {person:?}"
    );

    // 3. A dialog that takes no key.
    let mut m = questioned(vec![third.clone()]);
    let (st, keys, _) = q_bound(&mut m);
    assert_eq!(keys, ["enter", "enter"], "{:?}", m.requests);
    assert_eq!(st["retried"], 1, "{st:?}");
    let one_look = aterm_spec::interp::with_consts(&model, &[("MaxRetry", 1)]);
    assert!(!one_look.action_enabled("HarnessEnter", &st), "{st:?}");

    // 3, unattended: tried on the back-off for as long as it stands.
    let mut m = questioned(vec![third]);
    m.stall_sleep = Some(Duration::from_millis(20));
    let _ = watch_lines_with(&mut m, &answering(2, None), |s| {
        s.set_approval_env(owner_env());
    });
    let tries = i64::try_from(m.presses().len()).expect("a count");
    assert!(tries >= 3, "tried again: {:#?}", m.requests);
    let unbounded = aterm_spec::interp::with_consts(&model, &[("MaxRetry", tries)]);
    let (st, keys, _) = q_projected_run(&mut m, &unbounded, 2);
    assert!(keys.iter().all(|k| k == "enter"), "{keys:?}");
    assert_eq!(st["retried"], tries - 1, "{st:?}");
}

/// The lone multi-select question (a Submit tab, a `Submit` button, a review) keyed through:
/// the hand-built fixture with focus and checks moved as each key would, then its review.
fn lone_multiselect_screens() -> Vec<Vec<String>> {
    use aterm_phase::prompt::fixtures as f;
    let fresh = q(f::QUESTION_MULTISELECT_ALONE);
    let check = |rows: &[String], n: &str| -> Vec<String> {
        rows.iter()
            .map(|r| r.replace(&format!("{n}. [ ] "), &format!("{n}. [✔] ")))
            .collect()
    };
    let one = check(&fresh, "1");
    let both = focused_on(&check(&one, "3"), "3. [✔] Powerline");
    assert_ne!(fresh, one, "PRECONDITION: the toggle landed");
    // The review of one multi-select question: its chip answered, its one
    // `●` row and the labels checked.
    let incident_review = q(f::QUESTION_REVIEW);
    let start = incident_review
        .iter()
        .position(|r| r.trim() == "Review your answers")
        .expect("the review's title");
    let ready = incident_review
        .iter()
        .position(|r| r.trim() == "Ready to submit your answers?")
        .expect("the review's ask");
    let mut review: Vec<String> = Vec::new();
    for (k, r) in incident_review.iter().enumerate() {
        if r.starts_with("←") {
            review.push("←  ☒ Glyphs  ✔ Submit  →".to_string());
        } else if k == start + 2 {
            review.push(" ● Which glyph sets should be bundled?".to_string());
            review.push("   → Box drawing (Recommended), Powerline (Recommended)".to_string());
            review.push(String::new());
        } else if k > start + 2 && k < ready {
            continue;
        } else {
            review.push(r.clone());
        }
    }
    vec![
        fresh,
        one.clone(),
        focused_on(&one, "2. [ ] Emoji"),
        focused_on(&one, "3. [ ] Powerline"),
        both.clone(),
        focused_on(&both, "4. [ ] Type something"),
        focused_on(&both, "Submit"),
        review,
        idle_screen(),
    ]
}

/// Tier-1 for the downward moves (a `↓` is `HarnessMove` as an `↑` is), each on the model with
/// the dialog's own constants:
///
/// 4. A lone single-select recommending option 2, the focus on 1 (`OpensOffChoice`;
///    `Tabs = 1`, `Review = 0`): `↓`, then Enter.
/// 5. The lone multi-select (`Tabs = 3`, each toggle a tab): Enter, `↓ ↓` Enter, `↓ ↓` Enter
///    on `Submit`, the review's Enter, each Enter guarded on its row; never a digit.
#[test]
fn tier1_a_downward_move_and_a_multiselect_go_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures as f;
    let model = aterm_models_harness::supervisor_question_answer_model();

    // 4. Recommended second.
    let second = q(f::QUESTION_RECOMMENDED_SECOND);
    let mut m = questioned(vec![
        second.clone(),
        focused_on(&second, "2. Nextest"),
        idle_screen(),
    ]);
    let lone = aterm_spec::interp::with_consts(&model, &[("Tabs", 1), ("Review", 0)]);
    let (st, keys, _) = q_bound_on(&mut m, &lone, 0);
    assert_eq!(keys, ["down", "enter"], "{:?}", m.requests);
    assert_eq!(st["tab"], 2, "the dialog closed: {st:?}");

    // 5. The lone multi-select.
    let screens = lone_multiselect_screens();
    assert!(
        aterm_phase::parse_prompt_v2(&screens[7])
            .and_then(|p| p.question_dialog)
            .is_some_and(|d| matches!(d.form, aterm_phase::QuestionForm::Review { .. })),
        "PRECONDITION: the built review reads as one"
    );
    let mut m = questioned(screens);
    let three = aterm_spec::interp::with_consts(&model, &[("Tabs", 3)]);
    let (st, keys, _) = q_bound_on(&mut m, &three, 0);
    assert_eq!(
        keys,
        [
            "enter", "down", "down", "enter", "down", "down", "enter", "enter"
        ],
        "{:?}",
        m.requests
    );
    assert_eq!(st["tab"], 4, "the dialog closed: {st:?}");
    let presses = m.presses();
    for (at, row) in [
        (0, "❯ 1. [ ] Box drawing (Recommended)"),
        (3, "❯ 3. [ ] Powerline (Recommended)"),
        (6, "❯    Submit"),
        (7, "❯ 1. Submit answers"),
    ] {
        assert!(presses[at].contains(&guarded(row)), "{presses:?}");
    }
}

/// A person who keys during the first key's wait gets the dialog first: the stamp asked after
/// the wait says 300 ms ago, so the loop decides again and keys once, fenced on that read.
/// Control: a host whose `status` carries no stamp keys on the first read.
#[test]
fn a_person_who_keys_during_the_first_key_wait_gets_the_dialog_first() {
    use aterm_phase::prompt::fixtures as f;
    for stamped in [true, false] {
        let incident = q(f::QUESTION_TABS_INCIDENT);
        let mut m = questioned(vec![incident.clone(), incident, idle_screen()]);
        // The decision read's `status` says no person; the one after the wait, 300 ms ago.
        if stamped {
            m.human_ms_reads = VecDeque::from([None, Some(300)]);
        }
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.supervise(&answering(30, None)).expect("supervise");
        assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
        let first_wait = m
            .requests
            .iter()
            .position(|r| r.starts_with("await idle 1000 "))
            .expect("the first-key wait");
        assert_eq!(
            m.requests.get(first_wait + 1).map(String::as_str),
            Some("status"),
            "the stamp is asked right after the wait: {:?}",
            m.requests
        );
        let key_at = m
            .requests
            .iter()
            .position(|r| r.starts_with("key "))
            .expect("keyed");
        let reads_before = m.requests[..key_at]
            .iter()
            .filter(|r| r.starts_with("text "))
            .count();
        assert_eq!(
            reads_before,
            if stamped { 2 } else { 1 },
            "{:?}",
            m.requests
        );
        let fenced_on = format!("key if-human=7 if-gen=1.{} ", 100 + reads_before);
        assert!(
            m.presses()[0].starts_with(&fenced_on),
            "fenced on the read it was decided on: {:?}",
            m.presses()
        );
    }
}

// --- the decline's keystrokes (`press.rs`) -----------------------------------

/// A host with both generation fences (`key` and `send` name `if-gen=`, `text --json` carries
/// `"gen"`) and a person stamp (`"human_ms": null`: nobody has keyed the session).
fn fenced_both(screens: Vec<Vec<String>>) -> Mock {
    let mut m = fenced(screens);
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n\
              send [id=<key>] [if=<re>] [if-gen=<e.s>] <text>: write text\n"
        .to_string();
    m.human = vec!["null"];
    m
}

/// The decline states (`cap-decline-1..3`) and the fourth with `text` typed
/// into it: every screen a decline walks through.
pub(super) fn decline_states(text: &str) -> [Vec<String>; 4] {
    use crate::supervise::policy::approval::tests as at;
    let l = |s: &str| s.lines().map(str::to_string).collect::<Vec<String>>();
    [
        l(at::CAP_DECLINE_1),
        l(at::CAP_DECLINE_2),
        l(at::CAP_DECLINE_3),
        at::typed_box(text),
    ]
}

/// The writes a mock took: `key` and `send` requests, in order.
pub(super) fn writes(m: &Mock) -> Vec<String> {
    m.requests
        .iter()
        .filter(|r| r.starts_with("key ") || r.starts_with("send "))
        .cloned()
        .collect()
}

/// A reason as a rule words one.
fn press_text() -> String {
    crate::supervise::policy::decline_text(
        "this command",
        "the press under test says why",
        "Run it as shorter commands",
    )
}

/// The person's grace these tests press under: a stamp inside it is a
/// person at the keyboard (`[harness] human_grace_s`).
const DECLINE_GRACE: Duration = Duration::from_secs(10);

/// `press_decline` of option 2 with `text`, on the box a fresh session over
/// `m` reads first.
fn press_decline_on(m: &mut Mock, text: &str) -> Pressing {
    let mut s = session(m, None);
    let seen = s.screen().expect("a read");
    assert!(parse_prompt(&seen.rows).is_some(), "a box");
    s.press_decline(1, text, &seen, DECLINE_GRACE)
        .expect("no failure")
}

/// The decline's keystrokes over the four screens Claude Code draws: one per fresh read, each
/// fenced on its generation and guarded on the row it answers. Never a digit: on the refusal
/// that is the bare `No`, which stops the worker for a person.
#[test]
fn a_decline_is_one_fenced_guarded_keystroke_per_fresh_read() {
    let text = press_text();
    let [box1, box2, box3, box4] = decline_states(&text);
    let typed_row = box4
        .iter()
        .find(|r| r.starts_with(" ❯ 2. No, aterm harness (not the user):"))
        .expect("the typed row")
        .clone();
    let mut m = fenced_both(vec![box1, box2, box3, box4]);
    let pressing = press_decline_on(&mut m, &text);
    assert!(
        matches!(pressing, Pressing::Done(Press::Pressed { seq: 104 })),
        "{:#?}",
        m.requests
    );
    let g = |row: &str| crate::supervise::policy::row_guard(row);
    assert_eq!(
        writes(&m),
        [
            format!("key if-human=7 if-gen=1.101 if={} down", g(" ❯ 1. Yes")),
            format!("key if-human=7 if-gen=1.102 if={} tab", g(" ❯ 2. No")),
            format!(
                "send if-human=7 if-gen=1.103 if={} -- {text}",
                g(" ❯ 2. No, and tell Claude what to do differently")
            ),
            format!("key if-human=7 if-gen=1.104 if={} enter", g(&typed_row)),
        ],
        "{:#?}",
        m.requests
    );
}

/// A stale screen refuses the keystroke: the box ticked between the read and the `down`, the
/// fence answers `OK skipped reason=changed` (`Press::Changed`), and nothing follows.
#[test]
fn a_decline_keystroke_on_a_stale_screen_is_refused_and_nothing_follows_it() {
    let text = press_text();
    let [box1, box2, box3, box4] = decline_states(&text);
    let mut m = fenced_both(vec![box1, box2, box3, box4]);
    m.tick_at_key = 1;
    let pressing = press_decline_on(&mut m, &text);
    assert!(
        matches!(pressing, Pressing::Done(Press::Changed { .. })),
        "{:#?}",
        m.requests
    );
    let w = writes(&m);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(
        w[0].starts_with("key if-human=7 if-gen=1.101 ") && w[0].ends_with(" down"),
        "{w:#?}"
    );
}

/// Negative controls: a decline goes only as far as each read proves; what it cannot finish
/// is `Unconfirmed` (handed over) with no `enter`, and a box gone is `Skipped`.
#[test]
fn a_decline_the_screen_does_not_confirm_is_never_entered() {
    use crate::supervise::policy::approval::tests as at;
    let text = press_text();
    let [box1, box2, box3, _] = decline_states(&text);
    let stand_in: Vec<String> = at::CAP_DECLINE_4.lines().map(str::to_string).collect();
    let unconfirmed = |p: &Pressing| match p {
        Pressing::Unconfirmed { why, .. } => why.clone(),
        _ => panic!("expected the decline handed over"),
    };
    let no_enter = |m: &Mock| {
        let w = writes(m);
        assert!(!w.iter().any(|r| r.ends_with(" enter")), "{w:#?}");
        assert!(
            !w.iter().any(|r| r.ends_with(" 1") || r.ends_with(" 2")),
            "a digit: {w:#?}"
        );
        w
    };

    // `send` without the fence: nothing is written.
    let mut m = fenced(vec![box1.clone()]);
    let why = unconfirmed(&press_decline_on(&mut m, &text));
    assert!(why.contains("cannot fence"), "{why}");
    assert!(no_enter(&m).is_empty(), "{:#?}", m.requests);

    // A read with no generation to fence on: nothing is written.
    let mut m = fenced_both(vec![box1.clone()]);
    m.sends_gen = false;
    let why = unconfirmed(&press_decline_on(&mut m, &text));
    assert!(why.contains("no screen generation"), "{why}");
    assert!(no_enter(&m).is_empty(), "{:#?}", m.requests);

    // The text does not show: typed once, never Entered.
    let mut m = fenced_both(vec![box1.clone(), box2.clone(), box3.clone(), box3.clone()]);
    let why = unconfirmed(&press_decline_on(&mut m, &text));
    assert!(why.contains("did not show"), "{why}");
    let w = no_enter(&m);
    assert_eq!(
        w.iter().filter(|r| r.starts_with("send ")).count(),
        1,
        "{w:#?}"
    );

    // Another text shows: never Entered.
    let mut m = fenced_both(vec![box1.clone(), box2.clone(), box3.clone(), stand_in]);
    let why = unconfirmed(&press_decline_on(&mut m, &text));
    assert!(
        why.contains("the refusal's input holds `No, aterm harness: this rm"),
        "{why}"
    );
    no_enter(&m);

    // The box leaves after the `down`: nothing more is sent.
    let mut m = fenced_both(vec![box1, idle_screen()]);
    let pressing = press_decline_on(&mut m, &text);
    assert!(
        matches!(pressing, Pressing::Done(Press::Skipped { .. })),
        "{:#?}",
        m.requests
    );
    assert_eq!(no_enter(&m).len(), 1, "{:#?}", m.requests);
}

/// A keystroke the box never draws is handed over, never re-sent, and the memory outlives the
/// call (the reason too is typed once). Negative control: a fresh session on the same screen
/// writes the Tab.
#[test]
fn a_decline_keystroke_the_box_never_draws_is_handed_over_and_never_resent() {
    let text = press_text();
    let [box1, box2, box3, _] = decline_states(&text);
    let mut m = fenced_both(vec![box1, box2.clone()]);
    let first = {
        let mut s = session(&mut m, None);
        let seen = s.screen().expect("a read");
        let first = s
            .press_decline(1, &text, &seen, DECLINE_GRACE)
            .expect("no failure");
        let again = s.screen().expect("a read");
        let second = s
            .press_decline(1, &text, &again, DECLINE_GRACE)
            .expect("no failure");
        (first, second)
    };
    for p in [&first.0, &first.1] {
        match p {
            Pressing::Unconfirmed { why, .. } => {
                assert!(why.contains("never written twice"), "{why}");
            }
            other => panic!("expected the box handed over, got {other:?}"),
        }
    }
    let w = writes(&m);
    assert_eq!(w.len(), 2, "{w:#?}");
    assert!(w[0].ends_with(" down") && w[1].ends_with(" tab"), "{w:#?}");

    // The reason, typed once across calls.
    let mut m = fenced_both(vec![box3.clone()]);
    {
        let mut s = session(&mut m, None);
        for _ in 0..2 {
            let seen = s.screen().expect("a read");
            let p = s
                .press_decline(1, &text, &seen, DECLINE_GRACE)
                .expect("no failure");
            assert!(matches!(p, Pressing::Unconfirmed { .. }), "{p:?}");
        }
    }
    let sends = writes(&m).iter().filter(|w| w.starts_with("send ")).count();
    assert_eq!(sends, 1, "{:#?}", m.requests);

    // The control: no memory, the same screen, the Tab is written.
    let mut m = fenced_both(vec![box2]);
    let _ = press_decline_on(&mut m, &text);
    assert!(
        writes(&m).first().is_some_and(|w| w.ends_with(" tab")),
        "{:#?}",
        m.requests
    );
}

/// A keystroke whose answer never came may have been written: after a `send` lost to a closed
/// connection, or an Enter that landed, the same press writes no second one. Negative
/// control: a `send` answered `OK skipped` is typed on the next press.
#[test]
fn a_decline_keystroke_whose_answer_never_came_is_never_written_again() {
    let text = press_text();
    let [_, _, box3, box4] = decline_states(&text);
    let sends = |m: &Mock| writes(m).iter().filter(|w| w.starts_with("send ")).count();
    let run = |lost: bool| {
        // Where the first `send` falls, from a run that answers it.
        let mut probe = fenced_both(vec![box3.clone()]);
        let _ = press_decline_on(&mut probe, &text);
        let at = probe
            .requests
            .iter()
            .position(|r| r.starts_with("send "))
            .expect("a send");
        let mut m = fenced_both(vec![box3.clone()]);
        m.by_index.insert(
            at,
            if lost {
                closed()
            } else {
                ok(&format!("OK skipped seq={}\n", m.seq))
            },
        );
        let answers = {
            let mut s = session(&mut m, None);
            let seen = s.screen().expect("a read");
            let first = s
                .press_decline(1, &text, &seen, DECLINE_GRACE)
                .expect("no failure");
            let again = s.screen().expect("a read");
            let second = s
                .press_decline(1, &text, &again, DECLINE_GRACE)
                .expect("no failure");
            (first, second)
        };
        (answers, sends(&m))
    };
    let ((first, second), n) = run(true);
    assert!(matches!(first, Pressing::Lost { .. }), "{first:?}");
    assert!(matches!(second, Pressing::Unconfirmed { .. }), "{second:?}");
    assert_eq!(n, 1, "the lost send is never written again");
    let ((first, _), n) = run(false);
    assert!(
        matches!(first, Pressing::Done(Press::Skipped { .. })),
        "{first:?}"
    );
    assert_eq!(
        n, 2,
        "a skipped send wrote nothing, and is typed on the next press"
    );

    // The Enter landed; the box still shows the reason: no second Enter.
    let mut m = fenced_both(vec![box4.clone()]);
    {
        let mut s = session(&mut m, None);
        let seen = s.screen().expect("a read");
        let first = s
            .press_decline(1, &text, &seen, DECLINE_GRACE)
            .expect("no failure");
        assert!(
            matches!(first, Pressing::Done(Press::Pressed { .. })),
            "{first:?}"
        );
        let again = s.screen().expect("a read");
        let second = s
            .press_decline(1, &text, &again, DECLINE_GRACE)
            .expect("no failure");
        assert!(matches!(second, Pressing::Unconfirmed { .. }), "{second:?}");
    }
    let enters = writes(&m).iter().filter(|w| w.ends_with(" enter")).count();
    assert_eq!(enters, 1, "{:#?}", m.requests);
}

/// A person keying the session gets the decline's box: the read the press starts on, stamped
/// 400 ms ago, writes nothing (`Press::Yielded`); a host that stamps no input hands the box
/// over unwritten. (A person after the `↓`: the Tier-1's shape 3.)
#[test]
fn a_person_keying_the_session_gets_the_decline_s_box() {
    let text = press_text();
    let [box1, box2, box3, box4] = decline_states(&text);
    let states = vec![box1, box2, box3, box4];

    let mut m = fenced_both(states.clone());
    m.human = vec!["400"];
    let p = press_decline_on(&mut m, &text);
    assert!(matches!(p, Pressing::Done(Press::Yielded { .. })), "{p:?}");
    assert!(writes(&m).is_empty(), "{:#?}", m.requests);

    let mut m = fenced_both(states);
    m.human = Vec::new();
    match press_decline_on(&mut m, &text) {
        Pressing::Unconfirmed { why, .. } => assert!(why.contains("does not stamp"), "{why}"),
        other => panic!("expected the box handed over, got {other:?}"),
    }
    assert!(writes(&m).is_empty(), "{:#?}", m.requests);
}

// --- A box taller than the screen, under the safe rules (`policy/approval.rs` tall_box)

/// What the loop's decider tells the worker on the tall box: a
/// literal, so the loop is checked against the words, not against itself.
const TALL_RM_TEXT: &str = "aterm harness (not the user): this command was not run: it is taller \
     than the screen, so the harness cannot read all of it to check it. Run `rm -rf $S/$1` on \
     its own as a short command, separate from the rest, its paths written as literal absolute \
     paths (a command run on its own keeps none of this one's shell variables).";

/// The tall Bash box (`policy/fixtures/owner-bash-command-2026-09-24.txt`) on a 48-row
/// screen, its foot from the question down taken from a decline state (`cap-decline-1..4`),
/// its note's `$S` made `var`.
fn tall_state(foot_from: &[String], var: &str) -> Vec<String> {
    use crate::supervise::policy::approval::tests as at;
    let question = |rows: &[String]| {
        rows.iter()
            .rposition(|r| r.trim() == "Do you want to proceed?")
            .expect("the question")
    };
    let mut all: Vec<String> = at::owner_box(at::OWNER_COMMAND)
        .into_iter()
        .map(|r| {
            if r.starts_with(" │ Dangerous rm operation") {
                r.replace("$S", var)
            } else {
                r
            }
        })
        .collect();
    all.truncate(question(&all));
    all.extend(foot_from[question(foot_from)..].iter().cloned());
    all.split_off(all.len() - 48)
}

/// The four screens a decline of the tall box whose note names `var` walks
/// through, the reason typed into the fourth.
fn tall_states(var: &str) -> [Vec<String>; 4] {
    let text = TALL_RM_TEXT.replace("$S", var);
    decline_states(&text).map(|s| tall_state(&s, var))
}

/// Each tall screen served twice: the loop reads the tail, then (the box's head cut) the
/// whole screen (`tail_misses_the_live_rows`), and the mock serves one screen per read.
fn each_read_twice(screens: &[Vec<String>]) -> Vec<Vec<String>> {
    screens
        .iter()
        .flat_map(|s| [s.clone(), s.clone()])
        .collect()
}

/// A write without its `if-gen=` fence, and the fence's sequence.
fn unfenced(write: &str) -> (String, Option<u64>) {
    let mut gen_seq = None;
    let words: Vec<&str> = write
        .split(' ')
        .filter(|w| match w.strip_prefix("if-gen=1.") {
            Some(n) => {
                gen_seq = n.parse().ok();
                false
            }
            None => true,
        })
        .collect();
    (words.join(" "), gen_seq)
}

/// The tall box under the safe rules, in its bypass session, cannot be judged (its head is off
/// the screen), so it is declined with the removal its note flags, each write fenced on a
/// fresh read; nobody is asked, and the ledger says `declined` under `tall-box@v1`.
#[test]
fn a_box_taller_than_the_screen_is_declined_in_the_loop_and_nobody_is_asked() {
    let [s1, s2, s3, s4] = tall_states("$S");
    for s in [&s1, &s2, &s3, &s4] {
        assert!(
            aterm_phase::parse_prompt_v2(s).is_some_and(|p| p.head_off_screen),
            "{s:#?}"
        );
    }
    let typed_row = s4
        .iter()
        .find(|r| r.starts_with(" ❯ 2. No, aterm harness (not the user):"))
        .expect("the typed row")
        .clone();
    let (ldir, ledger) = ledger_file("tall-safe");
    let (jdir, journal) = journal_file("tall-safe");
    let mut screens = vec![bypass_busy()];
    screens.extend(each_read_twice(&[
        s1.clone(),
        s2.clone(),
        s3.clone(),
        s4.clone(),
    ]));
    screens.push(bypass_busy());
    let mut m = fenced_both(screens);
    m.cwd = Some("/Users/_owner/aterm".to_string());
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let g = |row: &str| crate::supervise::policy::row_guard(row);
    let (sent, gens): (Vec<String>, Vec<Option<u64>>) =
        writes(&m).iter().map(|w| unfenced(w)).unzip();
    assert_eq!(
        sent,
        [
            format!("key if-human=7 if={} down", g(" ❯ 1. Yes")),
            format!("key if-human=7 if={} tab", g(" ❯ 2. No")),
            format!(
                "send if-human=7 if={} -- {TALL_RM_TEXT}",
                g(" ❯ 2. No, and tell Claude what to do differently")
            ),
            format!("key if-human=7 if={} enter", g(&typed_row)),
        ],
        "{:#?}",
        m.requests
    );
    assert!(
        gens.iter().all(Option::is_some) && gens.windows(2).all(|w| w[0] < w[1]),
        "every write fenced, each on a later read: {gens:?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")
                || r.starts_with("post ")
                || r.ends_with(" 1")
                || r.ends_with(" 2")),
        "nobody asked, no Yes, no bare No: {:#?}",
        m.requests
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("EVENT prompt")),
        "no review point: {lines:?}"
    );
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("\"rule_id\":\"tall-box@v1\"")
            && rows[0].contains("\"decision\":\"declined\"")
            && rows[0].contains("Run `rm -rf $S/$1` on its own"),
        "{rows:?}"
    );
    let (records, _) = journal_records(&journal);
    let declined: Vec<_> = records.iter().filter(|r| r.kind == "declined").collect();
    assert_eq!(declined.len(), 1, "{records:#?}");
    assert_eq!(declined[0].phase, "tall-box@v1");
    let _ = std::fs::remove_dir_all(&ldir);
    let _ = std::fs::remove_dir_all(&jdir);
}

/// The same tall box back after two declines is declined again after a pause (`WAITING …
/// declined again in <n> ms`; `approval_loop`'s module header), never handed over for the
/// count. Control: a different tall box after the same two is declined at once.
#[test]
fn the_same_box_back_after_its_declines_is_declined_again_after_a_pause() {
    let same = tall_states("$S");
    let other = tall_states("$T");
    for (third, again) in [(&same, true), (&other, false)] {
        let (jdir, journal) = journal_file(&format!("tall-again-{again}"));
        let mut screens = vec![bypass_busy()];
        for _ in 0..2 {
            screens.extend(each_read_twice(&same));
            screens.push(bypass_busy());
        }
        screens.extend(each_read_twice(third));
        screens.push(bypass_busy());
        let mut m = fenced_both(screens);
        m.cwd = Some("/Users/_owner/aterm".to_string());
        m.vanish_after = Some(0);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
        });
        let enters = writes(&m).iter().filter(|w| w.ends_with(" enter")).count();
        assert_eq!(enters, 3, "{:#?}", m.requests);
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention")),
            "never badged for the count: {:#?}",
            m.requests
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("EVENT prompt")),
            "{lines:?}"
        );
        let (records, _) = journal_records(&journal);
        let paused = records
            .iter()
            .any(|r| r.kind == "waiting" && r.summary.contains("declined again in"));
        assert_eq!(paused, again, "{records:#?}");
        if !again {
            assert!(
                writes(&m)
                    .iter()
                    .any(|w| w.contains("Run `rm -rf $T/$1` on its own")),
                "{:#?}",
                m.requests
            );
        }
        let _ = std::fs::remove_dir_all(&jdir);
    }
}

/// A decline stops where the box is not the same: after the `↓` on the tall box, another tall
/// box with another note (`PromptV2::foot_note`) gets no Tab (`Press::Skipped`). Whatever note
/// it shows, a box is also its kind, whether its head is off the screen, and its command
/// (`decline_box`): each of the five look-alikes below gets the `↓` and nothing more.
#[test]
fn a_decline_goes_no_further_once_a_readable_box_with_the_same_note_stands_there() {
    use crate::supervise::policy::approval::tests as at;
    use aterm_phase::prompt::PromptKind;
    let [s1, s2, _, _] = tall_states("$S");
    let [_, other_tall, _, _] = tall_states("$T");
    let question = |rows: &[String]| {
        rows.iter()
            .rposition(|r| r.trim() == "Do you want to proceed?")
            .expect("the question")
    };
    let mut whole = at::owner_box(at::OWNER_COMMAND);
    whole.truncate(question(&whole));
    whole.extend(s2[question(&s2)..].iter().cloned());
    let note_at = s2
        .iter()
        .position(|r| r.starts_with(" │ Dangerous rm operation"))
        .expect("the note");
    let mut other = vec![
        "─".repeat(120),
        " Some tool nobody names".to_string(),
        String::new(),
        "   what it would do".to_string(),
        String::new(),
    ];
    other.extend(s2[note_at..].iter().cloned());
    let key = |rows: &[String]| {
        let p = aterm_phase::parse_prompt_v2(rows).expect("a box");
        (p.kind, p.head_off_screen, p.foot_note(rows))
    };
    let (tall_kind, tall_head, note) = key(&s1);
    assert!(tall_head && note.is_some(), "{note:?}");
    assert_eq!(key(&whole), (PromptKind::Bash, false, note.clone()));
    assert_eq!(key(&other), (tall_kind, false, note.clone()));
    // A whole box's decline: (iii) another kind, (iv) another command, (v) another shell.
    let text = press_text();
    let [box1, box2, _, _] = decline_states(&text);
    let note_at = box2
        .iter()
        .position(|r| r.starts_with(" │ Dangerous rm operation"))
        .expect("the note");
    let mut other_kind = vec![
        "─".repeat(120),
        " Some tool nobody names".to_string(),
        String::new(),
        "   what it would do".to_string(),
        String::new(),
    ];
    other_kind.extend(box2[note_at..].iter().cloned());
    let other_command: Vec<String> = box2
        .iter()
        .map(|r| {
            if r == "   for p in zz-aterm-probe; do set -- $p; rm -rf $NOPE/$1; done" {
                "   for p in zz-other; do set -- $p; rm -rf $NOPE/$1; done".to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    let other_shell: Vec<String> = box2
        .iter()
        .map(|r| {
            if r == " Bash command" {
                " PowerShell command".to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    let whole_note = key(&box1).2;
    assert!(whole_note.is_some());
    assert_eq!(
        key(&other_kind),
        (PromptKind::Other, false, whole_note.clone())
    );
    assert_eq!(
        key(&other_command),
        (PromptKind::Bash, false, whole_note.clone())
    );
    assert_eq!(
        key(&other_shell),
        (PromptKind::PowerShell, false, whole_note)
    );
    let command = |rows: &[String]| aterm_phase::parse_prompt_v2(rows).expect("a box").command;
    assert_eq!(
        command(&other_shell),
        command(&box2),
        "only the kind differs"
    );
    for (name, first, next, text) in [
        (
            "another tall box",
            &s1,
            other_tall,
            TALL_RM_TEXT.to_string(),
        ),
        ("the box whole", &s1, whole, TALL_RM_TEXT.to_string()),
        ("another kind", &s1, other, TALL_RM_TEXT.to_string()),
        (
            "a whole box, then another kind",
            &box1,
            other_kind,
            text.clone(),
        ),
        (
            "a whole box, then another command",
            &box1,
            other_command,
            text.clone(),
        ),
        (
            "a whole box, then another shell",
            &box1,
            other_shell,
            text.clone(),
        ),
    ] {
        let mut m = fenced_both(vec![first.clone(), first.clone(), next]);
        let pressing = press_decline_on(&mut m, &text);
        assert!(
            matches!(pressing, Pressing::Done(Press::Skipped { .. })),
            "{name}: {pressing:?}"
        );
        let w = writes(&m);
        assert_eq!(w.len(), 1, "{name}: {w:#?}");
        assert!(w[0].ends_with(" down"), "{name}: {w:#?}");
    }
}

/// The taken box, redrawn before it leaves (`cap-taken-box-1..2`: the countdown and a blank
/// row wiped, the transcript reflowed, the box up ~550 ms more), is pressed and ledgered once,
/// as is a box whose transcript reflows. Negative control: another command is a new box.
#[test]
fn a_taken_box_redrawn_before_it_leaves_is_pressed_once() {
    use aterm_phase::prompt::fixtures as f;
    const BEFORE: &str = include_str!("policy/fixtures/cap-taken-box-1-before.txt");
    const REDRAWN: &str = include_str!("policy/fixtures/cap-taken-box-2-redrawn.txt");
    let (before, redrawn) = (f::screen(BEFORE), f::screen(REDRAWN));
    assert_ne!(before, redrawn, "the redraw");
    let blanks = |s: &[String]| s.iter().filter(|r| r.trim().is_empty()).count();
    assert_ne!(blanks(&before), blanks(&redrawn), "a blank row wiped");
    let approvals = |lines: &[String]| lines.iter().filter(|l| l.contains("approved (")).count();

    let (dir, notes) = notes_file("taken-redrawn");
    // The redrawn box stays up for more than one read, as it does live.
    let script = vec![
        before.clone(),
        redrawn.clone(),
        redrawn.clone(),
        busy_screen(),
    ];
    let mut m = fenced(script);
    m.vanish_after = Some(0);
    let _ = session(&mut m, None).supervise(&full_power_opts(Some(notes.clone())));
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert_eq!(approvals(&lines), 1, "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("did not change")),
        "{lines:?}"
    );

    let quiet = f::screen(f::BOX_RM);
    assert!(aterm_phase::parse_prompt_v2(&quiet).is_some_and(|p| p.auto_deny.is_none()));
    let mut reflowed = quiet.clone();
    reflowed.remove(1);
    reflowed.insert(3, "  Listed 1 directory".to_string());
    assert_ne!(quiet, reflowed, "the reflow");
    let (dir, notes) = notes_file("quiet-reflowed");
    let mut m = fenced(vec![quiet, reflowed.clone(), reflowed, busy_screen()]);
    m.vanish_after = Some(0);
    let _ = session(&mut m, None).supervise(&full_power_opts(Some(notes.clone())));
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert_eq!(approvals(&lines), 1, "{lines:?}");

    let other: Vec<String> = before
        .iter()
        .map(|r| {
            r.replace("for p in a z", "for p in b y")
                .replace("a and z", "b and y")
        })
        .collect();
    let (dir, notes) = notes_file("taken-then-another");
    // The other box: read as the first box leaves, and decided.
    let script = vec![
        before,
        redrawn.clone(),
        redrawn,
        other.clone(),
        other,
        busy_screen(),
    ];
    let mut m = fenced(script);
    m.vanish_after = Some(0);
    let _ = session(&mut m, None).supervise(&full_power_opts(Some(notes.clone())));
    assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert_eq!(approvals(&lines), 2, "{lines:?}");
}

// ---- Tier-1: the decline's press bound to `SupervisorDeclineKeys` ---------

/// The environment's actions in `SupervisorDeclineKeys`: Claude Code reading
/// the oldest key, a person keying, their quiet returning.
const DECLINE_ENV: [&str; 18] = [
    "PersonKey",
    "QuietReturns",
    "PersonMoves",
    "PersonTabs",
    "PersonTypes",
    "PersonClears",
    "PersonAnswers",
    "PersonKeyIgnored",
    "DownTaken",
    "TabTaken",
    "TextIntoTheInput",
    "TextOnTheSelect",
    "EnterSubmitsTheReason",
    "EnterSubmitsAnotherText",
    "EnterOnTheEmptyInput",
    "EnterOnTheSelect",
    "KeyRefused",
    "KeyIntoTheComposer",
];

/// A read as the model sees it: the focus (1 on the refusal), its input open, what it holds
/// (0 nothing or the placeholder, 1 exactly `text`, 2 else), and whether the box is gone.
fn d_projected(rows: &[String], text: &str) -> (i64, i64, i64, i64) {
    use crate::supervise::policy::REFUSAL_LABEL;
    use crate::supervise::policy::approval::squashed;
    let Some(p) = aterm_phase::parse_prompt_v2(rows) else {
        return (0, 0, 0, 1);
    };
    let refusal = &p.options[1];
    let open = refusal.label != REFUSAL_LABEL;
    let placeholder = format!(
        "{REFUSAL_LABEL}, {}",
        aterm_phase::anchor("prompt.amend_no")
    );
    let held = if !open || refusal.label == placeholder {
        0
    } else if squashed(&refusal.label) == squashed(&format!("{REFUSAL_LABEL}, {text}")) {
        1
    } else {
        2
    };
    (i64::from(refusal.focused), i64::from(open), held, 0)
}

/// The fewest [`DECLINE_ENV`] actions from `st` to a state that draws `want` with the person's
/// quiet `quiet`; a person keys only where the read's stamp says one did (`new_input`).
fn d_explained(
    model: &aterm_spec::derive::Model,
    st: &QState,
    want: (i64, i64, i64, i64),
    quiet: i64,
    new_input: bool,
) -> QState {
    let draws = |s: &QState| {
        (s["focus"], s["open"], s["text"], i64::from(s["gone"] != 0)) == want && s["quiet"] == quiet
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = VecDeque::from([(st.clone(), 0)]);
    while let Some((s, depth)) = queue.pop_front() {
        if draws(&s) {
            return s;
        }
        if depth == 6 || !seen.insert(s.clone()) {
            continue;
        }
        for a in DECLINE_ENV {
            if (a == "PersonKey" && !new_input) || (a == "QuietReturns" && quiet == 0) {
                continue;
            }
            let mut next = s.clone();
            if model.fire(a, &mut next) {
                queue.push_back((next, depth + 1));
            }
        }
    }
    panic!("no behaviour the model allows explains the read {want:?} (quiet {quiet}) from {st:?}");
}

/// The real press of option 2 with `text` over `m`, projected onto `model`: each read is
/// `Read` after the fewest environment actions that explain it, each write its `Harness…`
/// action, which must be enabled. Returns the answer, end state, keys and first person state.
fn d_bound(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    text: &str,
) -> (Pressing, QState, Vec<String>, Option<QState>) {
    let pressing = press_decline_on(m, text);
    let mut st = model.init_state();
    let mut reads = m.reads.iter();
    let mut last_stamp: Option<u64> = None;
    let mut keys = Vec::new();
    let mut person_seen = None;
    for (req, ok) in &m.order {
        if req.starts_with("text ") {
            let (i, stamp) = *reads.next().expect("a served read");
            let age = stamp.parse::<u64>().ok();
            let quiet = i64::from(age.is_none_or(|ms| ms >= 10_000));
            let new_input = age.is_some_and(|ms| last_stamp.is_none_or(|was| ms < was));
            last_stamp = age.or(last_stamp);
            let want = d_projected(&m.screens[i], text);
            st = d_explained(model, &st, want, quiet, new_input);
            if want.3 == 0 {
                assert!(model.fire("Read", &mut st), "Read at {st:?}");
            }
            if quiet == 0 && person_seen.is_none() {
                person_seen = Some(st.clone());
            }
        } else if req.starts_with("key ") || req.starts_with("send ") {
            assert!(ok, "a refused write: {req}");
            let action = if req.starts_with("send ") {
                "HarnessText"
            } else {
                match req.rsplit_once(' ').expect("a key").1 {
                    "down" => "HarnessDown",
                    "tab" => "HarnessTab",
                    "enter" => "HarnessEnter",
                    other => panic!("a decline keyed with {other:?}: {req}"),
                }
            };
            assert!(
                model.action_enabled(action, &st),
                "the real press sent {req:?} where the model forbids {action}: {st:?}"
            );
            assert!(model.fire(action, &mut st));
            keys.push(action.to_string());
        }
        for inv in &model.invariants {
            assert!(
                model.check_invariant(inv.name, &st),
                "{} at {st:?}",
                inv.name
            );
        }
    }
    (pressing, st, keys, person_seen)
}

/// Tier-1 for `SupervisorDeclineKeys` (aterm-spec `supervisor_decline_keys_model`): the real
/// press bound request by request in the four shapes below. Negative controls: where it
/// wrote nothing in (3) and (4), the fence-only model (`Buggy = 1`) enables the Tab again.
#[test]
fn tier1_a_decline_keystroke_goes_only_where_the_model_allows() {
    let model = aterm_models_harness::supervisor_decline_keys_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let text = press_text();
    let [b1, b2, b3, b4] = decline_states(&text);

    // 1. The four states.
    let mut m = fenced_both(vec![b1.clone(), b2.clone(), b3.clone(), b4.clone()]);
    let (p, st, keys, _) = d_bound(&mut m, &model, &text);
    assert!(matches!(p, Pressing::Done(Press::Pressed { .. })), "{p:?}");
    assert_eq!(
        keys,
        ["HarnessDown", "HarnessTab", "HarnessText", "HarnessEnter"]
    );
    assert_eq!(st["q1"], 4, "the Enter written: {st:?}");

    // 2. The Tab not drawn on the first read after it.
    let mut m = fenced_both(vec![b1.clone(), b2.clone(), b2.clone(), b3, b4]);
    let (p, _, keys, _) = d_bound(&mut m, &model, &text);
    assert!(matches!(p, Pressing::Done(Press::Pressed { .. })), "{p:?}");
    assert_eq!(
        keys,
        ["HarnessDown", "HarnessTab", "HarnessText", "HarnessEnter"]
    );

    // 3. A person keys after the `↓`.
    let mut m = fenced_both(vec![b1.clone(), b2.clone()]);
    m.human = vec!["null", "400"];
    let (p, _, keys, person) = d_bound(&mut m, &model, &text);
    assert!(matches!(p, Pressing::Done(Press::Yielded { .. })), "{p:?}");
    assert_eq!(keys, ["HarnessDown"]);
    let person = person.expect("a read showed the person");
    assert!(!model.action_enabled("HarnessTab", &person), "{person:?}");
    assert!(
        buggy.action_enabled("HarnessTab", &person),
        "the fence alone would have keyed there: {person:?}"
    );

    // 4. The Tab never drawn.
    let mut m = fenced_both(vec![b1, b2]);
    let (p, st, keys, _) = d_bound(&mut m, &model, &text);
    assert!(matches!(p, Pressing::Unconfirmed { .. }), "{p:?}");
    assert_eq!(keys, ["HarnessDown", "HarnessTab"]);
    assert!(!model.action_enabled("HarnessTab", &st), "{st:?}");
    assert!(
        buggy.action_enabled("HarnessTab", &st),
        "the fence alone would have written the Tab again: {st:?}"
    );
}

/// Each live-row rule alone (`tail_misses_the_live_rows`) reads the fresh pane's trust
/// dialog whole: (1) two rows lower, the cursor on `No, exit` above the tail, only the
/// cursor rule; (2) the hidden cursor on the pane's last row, only the blank-tail rule.
#[test]
fn each_live_row_rule_alone_reads_the_fresh_panes_dialog_whole() {
    use aterm_phase::prompt::fixtures as f;
    let base = f::screen(f::TRUST_FRESH_PANE);
    let flip = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|r| match r.as_str() {
                " ❯ No, exit" => "   No, exit".to_string(),
                "   Yes, I trust this folder" => " ❯ Yes, I trust this folder".to_string(),
                _ => r.clone(),
            })
            .collect()
    };
    let mut lower = vec![
        "user@host-redacted-at-equal-length proj % clear".to_string(),
        "user@host-redacted-at-equal-length proj % cd proj".to_string(),
    ];
    lower.extend_from_slice(&base[..FRESH_PANE_ROWS - 2]);
    assert_eq!(lower.len(), FRESH_PANE_ROWS);
    assert!(
        lower[22].starts_with(" Enter to confirm"),
        "{:?}",
        lower[22]
    );
    for (label, no, cursor) in [("cursor", lower, 19), ("blank", base, 61)] {
        let yes = flip(&no);
        let mut m = fenced(vec![no.clone(), no, yes.clone(), yes, idle_screen()]);
        m.cursor_row = Some(cursor);
        m.cwd = Some(FRESH_PANE_FOLDER.to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
            s.set_approval_env(owner_env());
        });
        assert!(
            lines.iter().any(|l| l.starts_with("APPROVED seq=")),
            "{label}: {lines:?}\n{:?}",
            m.requests
        );
        assert_eq!(m.presses().len(), 2, "{label}: {:?}", m.requests);
    }
}

/// The tail's cut swept across the fresh pane's rm box and trust dialog, the cursor on the
/// focused option: every position is answered.
#[test]
fn every_cut_of_the_tail_across_a_fresh_panes_box_is_answered() {
    use aterm_phase::prompt::fixtures as f;
    let live = f::screen(f::BOX_RM_SUBAGENT_FRESH_PANE);
    let body: Vec<String> = live[22..=40].to_vec();
    let focus_in_body = body.iter().position(|r| r == " ❯ 1. Yes").unwrap();
    let mut misses = Vec::new();
    // box top row at 22 - s (s = rows of the box above the tail).
    for s in 0..=body.len() + 2 {
        let top = 22usize.saturating_sub(s);
        let mut rows = vec![String::new(); top];
        rows.extend(body.iter().cloned());
        rows.resize(FRESH_PANE_ROWS, String::new());
        let mut m = fenced(vec![
            bypass_busy(),
            rows.clone(),
            rows.clone(),
            rows,
            busy_screen(),
        ]);
        m.cursor_row = Some(top + focus_in_body);
        m.cwd = Some(FRESH_PANE_FOLDER.to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
            s.set_approval_env(owner_env());
        });
        if !lines.iter().any(|l| l.starts_with("APPROVED seq=")) || m.presses().len() != 1 {
            misses.push(format!(
                "rm s={s} top={top}: {:?} presses={:?}",
                lines,
                m.presses()
            ));
        }
    }
    let base = f::screen(f::TRUST_FRESH_PANE);
    for d in 0..=18usize {
        let mut no = vec![String::new(); d];
        no.extend_from_slice(&base[..FRESH_PANE_ROWS - d]);
        let yes: Vec<String> = no
            .iter()
            .map(|r| match r.as_str() {
                " ❯ No, exit" => "   No, exit".to_string(),
                "   Yes, I trust this folder" => " ❯ Yes, I trust this folder".to_string(),
                _ => r.clone(),
            })
            .collect();
        let mut m = fenced(vec![no.clone(), no, yes.clone(), yes, idle_screen()]);
        m.cursor_row = Some(17 + d);
        m.cwd = Some(FRESH_PANE_FOLDER.to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
            s.set_approval_env(owner_env());
        });
        if !lines.iter().any(|l| l.starts_with("APPROVED seq=")) {
            misses.push(format!(
                "trust d={d}: {:?} presses={:?}",
                lines,
                m.presses()
            ));
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// The hosted loop ledgers where its host says (`opts.ledger`), never in the process's own
/// state directory (in a test, the developer's); with none named, it has no ledger.
#[test]
fn a_hosted_loop_ledgers_where_its_host_says_and_nowhere_else() {
    let (dir, ledger) = ledger_file("hosted-ledger");
    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        ledger: Some(ledger.clone()),
        ..SuperviseOpts::hosted()
    };
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let _ = s.run_hosted(&opts, Arc::new(AtomicBool::new(false)), &mut out);
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter().any(|r| r.contains("\"decision\":\"approved\"")),
        "{rows:?}\n{}",
        String::from_utf8_lossy(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);

    let mut m = fenced(vec![bash_one_row(), bash_one_row()]);
    m.vanish_after = Some(0);
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let _ = s.run_hosted(
        &SuperviseOpts::hosted(),
        Arc::new(AtomicBool::new(false)),
        &mut out,
    );
    assert_eq!(s.approval_ledger(), None, "no ledger unless one is named");
}

/// Tier-1 for `SupervisorSurveyPress` (aterm-spec `supervisor_survey_press_model`): the `key`
/// requests ([`Mock::key_probes`]) the model forbids at the server's state when each arrived.
/// A guarded `key … 0` is `Press`, `key backspace` is `Backspace`; the state is the card's age
/// against the 300 ms mount window, a person's hand, a draft, a box, and the `0` (pending
/// while the debounce may still take it). Empty for the real loop, not for each mutant.
fn survey_press_violations(m: &Mock, hand: bool) -> Vec<String> {
    let model = aterm_models_harness::supervisor_survey_press_model();
    let window = Duration::from_millis(300);
    let mut bad = Vec::new();
    for p in &m.key_probes {
        let action = if p.request.contains("if=^●") && p.request.ends_with(" 0") {
            "Press"
        } else if p.request == "key backspace" {
            "Backspace"
        } else {
            continue;
        };
        let mut st = model.init_state();
        let card = match p.card_age {
            None => 0,
            Some(age) if age < window => 1,
            Some(_) => 2,
        };
        let (zero, takes) = match (p.zero, p.digit) {
            (false, _) => (0, 0),
            (true, Digit::Pending { take, .. }) => (1, i64::from(take)),
            (true, _) => (2, 0),
        };
        for (k, v) in [
            ("card", card),
            ("hand", i64::from(hand)),
            ("draft", i64::from(p.draft)),
            ("boxed", i64::from(p.boxed)),
            ("zero", zero),
            ("takes", takes),
            ("owed", i64::from(zero > 0)),
        ] {
            st.insert(k, v);
        }
        if !model.action_enabled(action, &st) {
            bad.push(format!("{action} at {st:?} ({})", p.request));
        }
    }
    bad
}

/// The card's guarded `0` presses a run made.
fn survey_presses(m: &Mock) -> usize {
    m.presses()
        .iter()
        .filter(|r| r.contains("if=^●") && r.ends_with(" 0"))
        .count()
}

/// The backspaces a run made.
fn survey_backspaces(m: &Mock) -> usize {
    m.requests.iter().filter(|r| *r == "key backspace").count()
}

/// The vendor's mount window at test scale is 300 ms (the projection's `window`); the loop
/// presses on a card that has shown 350 ms, unless a world says otherwise.
const SURVEY_AGED: Duration = Duration::from_millis(350);

/// A card that sits (one screen, so a wait on it blocks as the real server's does).
fn survey_sitting(screen: Vec<String>, tweak: &dyn Fn(&mut Mock, &mut SuperviseOpts)) -> Mock {
    let mut m = Mock::new(vec![screen]);
    m.card_ignores_within = Some(Duration::from_millis(300));
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(400));
    let mut opts = dismissing(None);
    tweak(&mut m, &mut opts);
    let _ = watch_lines_with(&mut m, &opts, |s| s.set_survey_settle(SURVEY_AGED));
    m
}

/// The reactive worlds: a card that answers the `0` in real time ([`Mock::card_debounce`]).
fn survey_reactive(debounce_ms: u64, land: Duration, tweak: &dyn Fn(&mut Mock)) -> Mock {
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_zero(with_survey(idle_screen())),
        idle_screen(),
    ]);
    m.card_debounce = Some(Duration::from_millis(debounce_ms));
    m.vanish_after = Some(24);
    m.stall_sleep = Some(Duration::from_millis(400));
    // Once the card has taken its digit and left, the loop parks: the run
    // ends there (`Mock::end_when_parked`). A backspace is due at the landing
    // time, sooner, and is met first.
    m.end_when_parked = true;
    tweak(&mut m);
    let _ = watch_lines_with(&mut m, &dismissing(None), |s| {
        s.set_survey_retry([Duration::ZERO; SURVEY_TRIES]);
        s.set_survey_settle(SURVEY_AGED);
        s.set_survey_land(land);
    });
    m
}

// The worlds of the survey card's Tier-1 ([`survey_press_violations`]) run as tests of
// their own: each waits in real time.

/// World 1, a settled card and nobody about: one press the model allows. World 2, a young
/// card (the vendor drops a digit inside 300 ms): with the settle skipped, the model forbids
/// the press.
#[test]
fn tier1_the_cards_0_goes_only_where_the_model_allows() {
    let m = survey_sitting(with_survey(idle_screen()), &|_, _| {});
    assert_eq!(survey_presses(&m), 1, "{:#?}", m.requests);
    assert_eq!(survey_press_violations(&m, false), Vec::<String>::new());

    let mut m = Mock::new(vec![with_survey(idle_screen())]);
    m.card_ignores_within = Some(Duration::from_millis(300));
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(400));
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, None);
    s.set_survey_settle(Duration::ZERO);
    let _ = s.watch(&dismissing(None), &mut out);
    assert_eq!(m.early_zeroes, 1);
    assert_eq!(
        survey_press_violations(&m, false).len(),
        1,
        "the model forbids the press inside the settle: {:#?}",
        m.key_probes
    );
}

/// World 3, a person typed 0.5 s ago (grace 120 s): no press. The mutant ignores the grace
/// (`human_grace_s = 0`) and presses, which the model forbids.
#[test]
fn tier1_the_cards_0_waits_out_a_persons_hand() {
    let hand = |grace_s: u32| {
        survey_sitting(with_survey(idle_screen()), &|m, opts| {
            m.human_ms = Some(500);
            opts.policy.human_grace_s = grace_s;
        })
    };
    let held = hand(120);
    assert_eq!(survey_presses(&held), 0, "{:#?}", held.requests);
    let ignored = hand(0);
    assert_eq!(survey_presses(&ignored), 1, "{:#?}", ignored.requests);
    assert_eq!(survey_press_violations(&ignored, true).len(), 1);
}

/// Worlds 4 and 5, a typed draft in the composer and a box up: nothing pressed.
#[test]
fn tier1_the_cards_0_never_goes_over_a_draft_or_a_box() {
    let mut draft = with_survey(idle_screen());
    let c = draft.iter().position(|r| r == "❯").expect("composer");
    draft[c] = "❯ hello".to_string();
    // (The cursor sits after the typed text, off the placeholder's column 2.)
    let m = survey_sitting(draft.clone(), &|m, _| m.cursor_col = Some(9));
    assert_eq!(survey_presses(&m), 0, "{:#?}", m.requests);
    let m = survey_sitting(with_survey(bash_one_row()), &|_, _| {});
    assert_eq!(survey_presses(&m), 0, "{:#?}", m.requests);
}

/// World 4b, a draft with its caret moved home: the cursor sits at column 2 like the
/// placeholder's, and only that cell tells them apart. Plain, it is the person's text (no
/// press); dim, it is Claude's suggestion (the `0` goes).
#[test]
fn tier1_the_cards_0_tells_a_homed_draft_from_the_dim_suggestion() {
    let mut draft = with_survey(idle_screen());
    let c = draft.iter().position(|r| r == "❯").expect("composer");
    draft[c] = "❯ hello".to_string();
    let homed = |attrs: &'static str| {
        survey_sitting(draft.clone(), &move |m, _| {
            m.cursor_col = Some(2);
            m.cell_attrs = attrs;
        })
    };
    let m = homed("none");
    assert_eq!(
        survey_presses(&m),
        0,
        "a homed draft is not the placeholder: {:#?}",
        m.requests
    );
    let m = homed("dim");
    assert_eq!(
        survey_presses(&m),
        1,
        "the dim suggestion is no draft: {:#?}",
        m.requests
    );
}

/// World 6, a card that drops the digit: the `0` is backspaced past the landing time. World
/// 8, a backspace the server refuses (`busy`): the `0` stays owed and goes on the next try.
#[test]
fn tier1_the_cards_dropped_0_is_backspaced_where_the_model_allows() {
    let m = survey_reactive(30, Duration::from_millis(100), &|m| {
        m.card_drops_digits = true
    });
    assert!(survey_backspaces(&m) >= 1, "{:#?}", m.requests);
    assert_eq!(survey_press_violations(&m, false), Vec::<String>::new());

    let m = survey_reactive(30, Duration::from_millis(100), &|m| {
        m.card_drops_digits = true;
        m.backspace_replies.push_back(err("busy input-unread"));
    });
    assert!(survey_backspaces(&m) >= 2, "{:#?}", m.requests);
    assert_eq!(survey_press_violations(&m, false), Vec::<String>::new());
}

/// World 7, a card that takes the digit 300 ms after it is written: a landing time that
/// outlasts the debounce never backspaces; the mutant's shorter one backspaces a pending `0`,
/// which the model forbids.
#[test]
fn tier1_the_cards_taken_0_is_never_backspaced_under_its_debounce() {
    let outlasting = survey_reactive(300, Duration::from_millis(400), &|_| {});
    assert_eq!(
        survey_backspaces(&outlasting),
        0,
        "{:#?}",
        outlasting.requests
    );
    assert_eq!(survey_presses(&outlasting), 1, "{:#?}", outlasting.requests);
    assert_eq!(
        survey_press_violations(&outlasting, false),
        Vec::<String>::new()
    );
    // The mutant: a landing time shorter than the debounce.
    let short = survey_reactive(300, Duration::from_millis(60), &|_| {});
    assert!(survey_backspaces(&short) >= 1, "{:#?}", short.requests);
    assert!(
        !survey_press_violations(&short, false).is_empty(),
        "the model forbids a backspace under a pending 0: {:#?}",
        short.key_probes
    );
}

/// The probe's gate reads the point as it was decided: a lost login's words in an MCP
/// result, which the transcript refuses, are an ordinary idle point where the probe runs,
/// and the transcript is asked once, at the decision, never by the gate.
#[test]
fn the_probes_gate_reads_a_refused_wall_as_it_was_decided() {
    use aterm_phase::prompt::fixtures as f;
    let host = ProbeHost::probing(true);
    *host.tail.lock().unwrap() =
        Some(include_str!("fixtures/claude-2.1.294-transcript-mcp-wall-phrase.jsonl").to_string());
    let mut spoof = rows(&[
        "❯ toolstart",
        "",
        "⏺ github - get_issue (MCP)(n: 7)",
        "  ⎿  Issue 7: the parser drops a row",
        "  ⎿  Not logged in · Please run /login",
        "",
        "✻ Cogitated for 4s · done 2:41 PM",
        "",
    ]);
    spoof.extend(composer("  ? for shortcuts"));
    let mut m = probe_mock(
        vec![busy_screen(), spoof],
        &[f::USAGE_REFRESHING, f::USAGE_SETTLED],
        &host,
    );
    let lines = probe_walk("probe-refused-wall", &mut m, &host, 120, &|_| {});
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert_eq!(
        *host.asks_at_claims.lock().unwrap(),
        [1],
        "the decision asked, the gate did not: {lines:#?}"
    );
}

/// One hosted watch of the probe's Tier-1 walk: `m` (a [`probe_mock`]) under `host` with the
/// person's grace `grace_s`, the probe's waits at test scale, and a ledger of its own.
fn probe_walk(
    tag: &str,
    m: &mut Mock,
    host: &Arc<ProbeHost>,
    grace_s: u32,
    seed: &dyn Fn(&Path),
) -> Vec<String> {
    let (dir, ledger) = ledger_file(tag);
    seed(&ledger);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            human_grace_s: grace_s,
            ..SupervisorConfig::default()
        },
        ..auto(60, None)
    };
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        probe_walk_waits(s);
    });
    let _ = std::fs::remove_dir_all(&dir);
    lines
}

/// The probe's waits at a scripted server's scale: 60 ms to open, 100 ms to close, a read
/// every 10 ms, and half a minute to settle (a hang detector, not a latency budget).
fn probe_walk_waits<C: Ctl>(s: &mut Session<'_, C>) {
    let ms = Duration::from_millis;
    s.set_probe_waits(ms(60), Duration::from_secs(30), ms(10), ms(100));
}

/// Tier-1 for `SupervisorUsageProbe` (aterm-spec `supervisor_usage_probe_model`): every
/// `/usage`, Escape and sample of the real loop over the panel's world ([`PanelWorld`]) is
/// projected onto the model ([`usage_probe_violations`]) and must be enabled there. Each
/// mutant (two knobs, five by hand) is caught.
#[test]
fn tier1_the_probes_writes_go_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures as f;
    let settled = [f::USAGE_REFRESHING, f::USAGE_SETTLED];
    let live = || vec![busy_screen(), idle_screen()];
    let none = |_: &Path| {};
    let clean = |m: &Mock, host: &ProbeHost, lines: &[String]| {
        assert_eq!(
            usage_probe_violations(m, host),
            Vec::<String>::new(),
            "{lines:#?}\n{:#?}",
            m.requests
        );
    };

    // World 1, the panel opens and settles: one `/usage`, one sample, one Escape.
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(live(), &settled, &host);
    let lines = probe_walk("t1-probe-settled", &mut m, &host, 120, &none);
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert_eq!(host.samples.lock().unwrap().len(), 1);
    clean(&m, &host, &lines);

    // World 2, the panel never opens: the `/usage` alone (`GiveUpNotOpened`), no key.
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(live(), &settled, &host);
    world(&mut m).opens = false;
    let lines = probe_walk("t1-probe-not-opened", &mut m, &host, 120, &none);
    assert_eq!(probe_writes(&m), ["/usage"], "{lines:#?}");
    clean(&m, &host, &lines);

    // World 3, a person keys while the panel is read (a one-second grace): no sample, and
    // the Escape only once their hand is off. The mutant ignores the grace.
    let reading = |grace_s: u32| {
        let host = ProbeHost::probing(true);
        let mut m = probe_mock(live(), &settled, &host);
        world(&mut m).person_after = Some(2);
        world(&mut m).grace = Duration::from_secs(1);
        let lines = probe_walk("t1-probe-person", &mut m, &host, grace_s, &none);
        assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
        assert!(host.samples.lock().unwrap().is_empty());
        usage_probe_violations(&m, &host)
    };
    assert_eq!(reading(1), Vec::<String>::new());
    let bad = reading(0);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(
        bad[0].starts_with("EscCloses at ") && bad[0].contains("(\"hand\", 1)"),
        "{bad:#?}"
    );

    // World 4, a person typed half a second before the point (grace 120 s): nothing is
    // written. The mutant ignores the grace: its `/usage` is `Type` under a hand.
    let typed = |grace_s: u32| {
        let host = ProbeHost::probing(true);
        let mut m = probe_mock(live(), &settled, &host);
        m.human_ms = Some(500);
        let lines = probe_walk("t1-probe-hand", &mut m, &host, grace_s, &none);
        (probe_writes(&m), usage_probe_violations(&m, &host), lines)
    };
    let (wrote, bad, lines) = typed(120);
    assert_eq!(wrote, Vec::<&str>::new(), "{lines:#?}");
    assert_eq!(bad, Vec::<String>::new());
    let (wrote, bad, lines) = typed(0);
    assert_eq!(wrote.first(), Some(&"/usage"), "{lines:#?}");
    assert!(
        bad.first()
            .is_some_and(|b| b.starts_with("Type at ") && b.contains("(\"hand\", 1)")),
        "{bad:#?}"
    );

    // World 5, the previous loop died mid-probe (`LoopRestarts`: `ph` 2, `own` 0): the panel
    // up, one Escape after a grace in this loop's own sight, no sample.
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![f::screen(f::USAGE_SETTLED)],
        &[f::USAGE_SETTLED],
        &host,
    );
    world(&mut m).at = PanelAt::Open;
    world(&mut m).rest = Some(idle_screen());
    let lines = probe_walk("t1-probe-successor", &mut m, &host, 1, &|ledger| {
        let row = approvals::Row {
            rule_id: "usage-probe@v1",
            outcome: approvals::Outcome::Typed,
            command: "/usage",
            reason: approvals::PROBE_TYPED_REASON,
            box_seq: 7,
        };
        std::fs::write(ledger, format!("{}\n", row.to_json(1_000, None))).expect("the ledger");
    });
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    assert!(host.said("probe").is_empty());
    clean(&m, &host, &lines);

    // World 6, an Escape the panel does not take, twice: three Escapes, each allowed
    // (`EscFails`, then `EscCloses`), and the host told the panel is stuck.
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(live(), &settled, &host);
    world(&mut m).esc_fails = 2;
    let (dir, ledger) = ledger_file("t1-probe-ladder");
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig::default(),
        ..auto(60, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        probe_walk_waits(s);
        s.set_survey_retry([Duration::from_millis(20); SURVEY_TRIES]);
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        probe_writes(&m),
        ["/usage", "esc", "esc", "esc"],
        "{lines:#?}"
    );
    assert_eq!(host.said("inform").len(), 1);
    clean(&m, &host, &lines);

    // Mutants no knob makes, written by hand against the same server.
    let by_hand = |opens: bool| {
        let host = ProbeHost::probing(true);
        let mut m = probe_mock(vec![idle_screen()], &[f::USAGE_REFRESHING], &host);
        world(&mut m).opens = opens;
        assert!(host.account_probe().is_ok(), "the claim");
        for request in [&["send", "--", "/usage"][..], &["key", "enter"]] {
            assert!(m.call(request).expect("served").ok());
        }
        (host, m)
    };
    // The open wait ran out and the loop pressed anyway: an Escape with no panel.
    let (host, mut m) = by_hand(false);
    host.account_probed(Err(ProbeMiss::NotOpened));
    let _ = m.call(&["key", "esc"]);
    let bad = usage_probe_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(
        bad[0].starts_with("EscCloses at ") && bad[0].contains("(\"pan\", 0)"),
        "{bad:#?}"
    );
    // An Escape before the host was told what the read came to.
    let (host, mut m) = by_hand(true);
    let _ = m.call(&["key", "esc"]);
    let bad = usage_probe_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(
        bad[0].starts_with("EscCloses at ")
            && bad[0].contains("(\"ph\", 2)")
            && bad[0].contains("(\"own\", 1)"),
        "{bad:#?}"
    );
    // A second `/usage` over the open panel.
    let (host, mut m) = by_hand(true);
    host.account_probed(Err(ProbeMiss::Unsettled));
    assert!(host.account_probe().is_ok());
    let _ = m.call(&["send", "--", "/usage"]);
    let bad = usage_probe_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(
        bad[0].starts_with("Type at ") && bad[0].contains("(\"ph\", 2)"),
        "{bad:#?}"
    );
    // A sample handed over from the refreshing panel.
    let (host, mut m) = by_hand(true);
    let _ = m.call(&["text", "--json"]);
    host.account_probed(Ok(&crate::harness::accounts::Sample::default()));
    let bad = usage_probe_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(
        bad[0].starts_with("Sample at ") && bad[0].contains("(\"pan\", 1)"),
        "{bad:#?}"
    );
    // An Escape at the panel while a turn runs under it (the composed `panel_under_a_turn`):
    // `NoEscapeIntoARunningTurn`.
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(vec![idle_screen()], &[f::USAGE_SETTLED], &host);
    world(&mut m).frames = vec![panel_under_a_turn()];
    assert!(host.account_probe().is_ok(), "the claim");
    for request in [&["send", "--", "/usage"][..], &["key", "enter"]] {
        assert!(m.call(request).expect("served").ok());
    }
    host.account_probed(Err(ProbeMiss::Unsettled));
    let _ = m.call(&["key", "esc"]);
    let bad = usage_probe_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(
        bad[0].starts_with("EscCloses at ") && bad[0].contains("(\"turn\", 1)"),
        "{bad:#?}"
    );
}

/// Tier-1 for `SupervisorLimitReset` (aterm-spec `supervisor_limit_reset_model`): every key
/// the real loop writes over a composed reset dialog ([`ResetWorld`]), and each host step, is
/// projected onto the model ([`reset_violations`]) and must be enabled there; typing the
/// reset command is `TypesTheCommand`. Each mutant (two knobs, five by hand) is caught by name.
#[test]
fn tier1_the_resets_keys_go_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::reset::fixtures as f;
    let clean = |m: &Mock, host: &ResetHost, run: &ResetRun| {
        assert_eq!(
            reset_violations(m, host),
            Vec::<String>::new(),
            "{:#?}\n{:#?}",
            run.journal,
            m.requests
        );
    };
    let named = |bad: &[String], defect: &str, law: &str| {
        let name = format!("{defect} breaks {law}");
        assert!(bad.iter().any(|b| b.contains(&name)), "{name}: {bad:#?}");
    };

    // World 1, the whole path: the row's two keys (`TakeRow`), the Enter, and the host's
    // `Claim` and `ReadLine`.
    let host = ResetHost::reading(ResetOutcome::Spent {
        asks: false,
        left: Some(1),
        cleared_at: TEST_NOW,
    });
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    let run = reset_watch(
        "t1-reset-whole",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    clean(&m, &host, &run);
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
    let steps: Vec<&str> = host
        .steps
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.action)
        .collect();
    assert_eq!(steps, ["Claim", "ReadLine"]);

    // World 2, the early-use confirmation (`dlg = 5`): the loop Escapes it; the mutant enters.
    let early = |host: &Arc<ResetHost>| {
        let mut m = reset_mock(vec![], &RESET_ROWS, 0, host);
        reset_world(&mut m).confirm = screen(f::CONFIRM_EARLY);
        reset_world(&mut m).exact = false;
        m
    };
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = early(&host);
    let run = reset_watch(
        "t1-reset-early",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    clean(&m, &host, &run);
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "esc@confirm"]
    );
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = early(&host);
    let ask = ResetAsk {
        left: 2,
        day: "Oct 5".to_string(),
        day_start: None,
    };
    assert!(m.call(&["text", "--json"]).expect("served").ok());
    assert!(
        matches!(host.reset_take(&ask), ResetWord::Take(_)),
        "the claim"
    );
    for request in [&["key", "down"][..], &["key", "enter"]] {
        assert!(m.call(request).expect("served").ok());
    }
    // `Checking…`, then the confirmation.
    for _ in 0..2 {
        assert!(m.call(&["text", "--json"]).expect("served").ok());
    }
    assert_eq!(reset_world(&mut m).at, ResetAt::Confirm);
    assert!(m.call(&["key", "enter"]).expect("served").ok());
    assert_eq!(reset_world(&mut m).yes_taken, 1, "the mutant spent a reset");
    let bad = reset_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(bad[0].starts_with("Enter at "), "{bad:#?}");
    named(
        &bad,
        "EnterTheInexactConfirmation",
        "EnterOnlyOnTheExactConfirmation",
    );

    // World 3, the weekly reset's row in the grant row's slot (`dlg = 2`): the loop asks the
    // host nothing and presses the wait row. The mutant takes the slot's row by its place.
    const WEEKLY_ROW: &str = "Reset your session limit now";
    let weekly = [RESET_STOP, WEEKLY_ROW, RESET_WAIT, RESET_CREDITS];
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &weekly, 0, &host);
    let run = reset_watch(
        "t1-reset-weekly",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    clean(&m, &host, &run);
    assert_eq!(reset_world(&mut m).chosen, [RESET_WAIT]);
    assert!(host.said("").is_empty(), "the host is never asked");
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &weekly, 0, &host);
    assert!(m.call(&["text", "--json"]).expect("served").ok());
    host.shared.claim.store(1, Ordering::SeqCst);
    for request in [&["key", "down"][..], &["key", "enter"]] {
        assert!(m.call(request).expect("served").ok());
    }
    assert_eq!(reset_world(&mut m).chosen, [WEEKLY_ROW]);
    let bad = reset_violations(&m, &host);
    assert_eq!(bad.len(), 2, "{bad:#?}");
    assert!(bad.iter().all(|b| b.starts_with("TakeRow at ")), "{bad:#?}");
    named(&bad, "TakeRowByItsPlace", "NeverTheWeeklyRow");

    // World 4, a person typed half a second before the dialog was judged (grace 120 s):
    // nothing is written. The mutant ignores the grace.
    let typed = |grace_s: u32| {
        let host = ResetHost::reading(ResetOutcome::Kept);
        let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
        m.human_ms = Some(500);
        // Held, the dialog never leaves: the session ends once it was judged.
        if grace_s > 0 {
            reset_world(&mut m).exits_when = Some(ResetExit::Statuses(1));
        }
        reset_watch(
            "t1-reset-hand",
            &mut m,
            reset_opts(&host, true, grace_s),
            &fresh,
            |_| {},
        );
        (reset_keys(&m), reset_violations(&m, &host))
    };
    assert_eq!(typed(120), (Vec::<String>::new(), Vec::<String>::new()));
    let (wrote, bad) = typed(0);
    assert_eq!(wrote.first().map(String::as_str), Some("down@dialog"));
    named(&bad, "TakeRowUnderAHand", "NothingKeyedUnderAHand");
    named(&bad, "EnterUnderAHand", "NothingKeyedUnderAHand");

    // World 5, the vendor's guard on the confirmation: the loop's settle keeps every key past
    // it. The mutant, against a guard that outlasts its settle, enters inside the guard.
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).guard = Duration::from_millis(30);
    let run = reset_watch(
        "t1-reset-guard",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| {
            s.set_box_settle(Duration::from_millis(60));
        },
    );
    clean(&m, &host, &run);
    assert_eq!(reset_world(&mut m).refused, Vec::<String>::new());
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).guard = Duration::from_secs(600);
    reset_world(&mut m).exits_when = Some(ResetExit::Refused);
    reset_watch(
        "t1-reset-in-guard",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |_| {},
    );
    let bad = reset_violations(&m, &host);
    assert!(
        bad.first().is_some_and(|b| b.starts_with("Enter at ")),
        "{bad:#?}"
    );
    named(&bad, "EnterInsideTheGuard", "EnterOnlyPastTheGuardOnYes");

    // World 6, the previous loop died after the row (`LoopRestarts`: `ph` stays 1, `own` 0):
    // one Escape, after a grace in this loop's own sight. The mutant enters as a successor.
    let taken = |ledger: &Path| {
        let row = approvals::Row {
            rule_id: "limit-reset@v1",
            outcome: approvals::Outcome::Approved,
            command: RESET_GRANT,
            reason: "taken: wall until 1790265300",
            box_seq: 7,
        };
        let now = crate::supervise::journal::unix_ms();
        std::fs::write(ledger, format!("{}\n", row.to_json(now, None))).expect("the ledger");
    };
    let confirming = || {
        let host = ResetHost::reading(ResetOutcome::Kept);
        host.shared.claim.store(1, Ordering::SeqCst);
        host.shared.ph.store(1, Ordering::SeqCst);
        let mut m = reset_mock(vec![], &RESET_ROWS, 1, &host);
        let world = reset_world(&mut m);
        world.at = ResetAt::Confirm;
        world.grace = Duration::from_secs(1);
        let frame = world.frame();
        let last = m.screens.len() - 1;
        m.screens[last] = frame;
        (host, m)
    };
    let (host, mut m) = confirming();
    let run = reset_watch(
        "t1-reset-successor",
        &mut m,
        reset_opts(&host, true, 1),
        &taken,
        |_| {},
    );
    assert_eq!(reset_keys(&m), ["esc@confirm"]);
    clean(&m, &host, &run);
    let (host, mut m) = confirming();
    assert!(m.call(&["text", "--json"]).expect("served").ok());
    assert!(m.call(&["key", "enter"]).expect("served").ok());
    let bad = reset_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    named(&bad, "EnterAsASuccessor", "EnterOnlyPastTheGuardOnYes");

    // World 7, the Enter dropped once and an Escape dropped twice, each written again where
    // the model allows it (`EnterDropped` then `Enter`; `EscapeDropped` then `Escape`).
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![], &RESET_ROWS, 0, &host);
    reset_world(&mut m).enter_drops = usize::MAX;
    reset_world(&mut m).esc_drops = 2;
    let run = reset_watch(
        "t1-reset-drops",
        &mut m,
        reset_opts(&host, true, 120),
        &fresh,
        |s| s.set_reset_waits(reset_waits_cut(|w| w.enter = Duration::from_millis(20))),
    );
    assert_eq!(
        reset_keys(&m)[2..],
        [
            "enter@confirm",
            "enter@confirm",
            "enter@confirm",
            "esc@confirm",
            "esc@confirm",
            "esc@confirm"
        ]
    );
    clean(&m, &host, &run);

    // World 8, a person keys after the dialog was judged (in the claim's round trip): the
    // row's first key, fenced on the judged read's person count, is skipped, and the row is
    // taken once their grace has passed. The mutant writes the arrow with no person fence.
    let keyed = |host: &Arc<ResetHost>| {
        let mut m = reset_mock(vec![], &RESET_ROWS, 0, host);
        reset_world(&mut m).person_at_row_key = 1;
        reset_world(&mut m).grace = Duration::from_secs(1);
        m
    };
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = keyed(&host);
    let run = reset_watch(
        "t1-reset-row-hand",
        &mut m,
        reset_opts(&host, true, 1),
        &fresh,
        |_| {},
    );
    clean(&m, &host, &run);
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", run.journal);
    assert_eq!(
        reset_keys(&m),
        ["down@dialog", "enter@dialog", "enter@confirm"]
    );
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = keyed(&host);
    assert!(m.call(&["text", "--json"]).expect("served").ok());
    assert!(
        matches!(host.reset_take(&ask), ResetWord::Take(_)),
        "the claim"
    );
    assert!(m.call(&["key", "down"]).expect("served").ok());
    let bad = reset_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    assert!(bad[0].starts_with("TakeRow at "), "{bad:#?}");
    named(&bad, "TakeRowUnderAHand", "NothingKeyedUnderAHand");

    // The mutant: the command typed at a composer with no dialog up, where it falls through
    // to the weekly reset unconfirmed.
    let host = ResetHost::reading(ResetOutcome::Kept);
    let mut m = reset_mock(vec![idle_screen()], &RESET_ROWS, 0, &host);
    assert!(m.call(&["text", "--json"]).expect("served").ok());
    assert!(
        m.call(&["send", "--", "/limit-reset"])
            .expect("served")
            .ok()
    );
    let bad = reset_violations(&m, &host);
    assert_eq!(bad.len(), 1, "{bad:#?}");
    named(&bad, "TypesTheCommand", "NeverTheTypedCommand");
}

/// A model fallback the host keeps deferring (`wait:…`) at a wall that names no reset and a
/// `keep going` cannot lift is asked again on the last pause of
/// [`TurnEndTiming::restart_backoff`], nothing typed meanwhile.
#[test]
fn a_model_fallback_the_host_keeps_deferring_is_asked_again_and_nothing_is_typed_blind() {
    use aterm_phase::prompt::fixtures::screen;
    const FABLE: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-fable-goal-rate-limited-shell.txt"
    );
    let run = |wall: Vec<String>| {
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = std::iter::repeat_n(Some("wait:changed"), 400).collect();
        let (jdir, journal) = journal_file("fallback-waits");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let mut m = Mock::new(vec![busy_screen(), wall]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let (r, _) = hosted_with_host(
            &mut m,
            &opts,
            &host,
            Stop::reached(
                move |h| h.restarted.lock().unwrap().len() >= 12,
                Duration::from_millis(100),
            ),
            Some(TurnEndTiming {
                restart_backoff: vec![Duration::from_millis(20)],
                reset_grace: Duration::ZERO,
                reset_jitter: Duration::ZERO,
                ..TurnEndTiming::default()
            }),
        );
        assert_eq!(r, Ok(()));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let asked = host.restarted.lock().unwrap().clone();
        (lines, asked, m.requests.clone())
    };
    let (lines, asked, requests) = run(screen(FABLE));
    assert!(
        asked.len() >= 12 && asked.iter().all(|w| *w == "model"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("restart:model waits")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(
            |l| l.starts_with("CONTINUED") || l.starts_with("WAITING ") && l.contains("resets")
        ),
        "nothing typed, no reset waited for, at a wall no reset ends: {lines:#?}"
    );
    assert_eq!(
        requests.iter().filter(|r| r.starts_with("turn")).count(),
        0,
        "{requests:#?}"
    );
}

/// The same deferred fallback at a bucket that names its reset: asked again until the
/// reset, then the continuation goes first (the wall may have lifted) while the host still
/// answers `wait:changed`.
#[test]
fn a_fallback_the_host_keeps_deferring_gives_way_to_the_continuation_at_the_reset() {
    const FABLE: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-fable-goal-rate-limited-shell.txt"
    );
    let named: Vec<String> = aterm_phase::prompt::fixtures::screen(FABLE)
        .into_iter()
        .map(|r| {
            r.replace(
                "Run /usage-credits to continue or switch models with /model.",
                "· resets in 4s",
            )
        })
        .collect();
    let host = Arc::new(TestHost::default());
    *host.restarts.lock().unwrap() = std::iter::repeat_n(Some("wait:changed"), 4000).collect();
    let (jdir, journal) = journal_file("fallback-named");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        journal: Some(journal.clone()),
        ..SuperviseOpts::hosted()
    };
    let mut m = Mock::new(vec![busy_screen(), named]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let watched = journal.clone();
    let (r, _) = hosted_with_host(
        &mut m,
        &opts,
        &host,
        Stop::reached(
            move |_| continued_on_usage(&watched),
            Duration::from_millis(300),
        ),
        Some(TurnEndTiming {
            restart_backoff: vec![Duration::from_millis(20)],
            reset_grace: Duration::ZERO,
            reset_jitter: Duration::ZERO,
            limit_backoff: vec![Duration::from_secs(600)],
            ..TurnEndTiming::default()
        }),
    );
    assert_eq!(r, Ok(()));
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
    let asked = host.restarted.lock().unwrap().len();
    assert!(asked >= 2, "asked again before the reset: {lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("usage-resume@v1")),
        "the continuation at the reset, the host still deferring: {lines:#?}"
    );
    let first_turn = lines
        .iter()
        .position(|l| l.starts_with("CONTINUED "))
        .unwrap();
    assert!(
        lines[..first_turn]
            .iter()
            .any(|l| l.contains("restart:model step=wait:changed")),
        "{lines:#?}"
    );
}

/// A deferred fallback's retry is due at the reset plus the grace plus the session's share
/// of the spread ([`TurnEndTiming::reset_jitter`]), where its continuation goes: cut earlier,
/// the retry would meet that same wait and be put off a whole back-off. Negative control: with
/// no share, at the grace.
#[test]
fn a_deferred_fallback_is_retried_at_the_reset_the_grace_and_the_sessions_share() {
    use super::super::turn_end_loop::resume_jitter;
    use crate::supervise::policy::turn_end::{RULE_MODEL_FALLBACK, Restart};
    const FABLE: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-fable-goal-rate-limited-shell.txt"
    );
    let named: Vec<String> = aterm_phase::prompt::fixtures::screen(FABLE)
        .into_iter()
        .map(|r| {
            r.replace(
                "Run /usage-credits to continue or switch models with /model.",
                "· resets in 3h",
            )
        })
        .collect();
    let grace = Duration::from_secs(150);
    let reset_unix = TEST_NOW + 3 * 3600;
    let run = |share: Duration| {
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = [Some("wait:changed")].into();
        let mut m = Mock::new(vec![named.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            restart_backoff: vec![Duration::from_secs(10 * 3600)],
            reset_grace: grace,
            reset_jitter: share,
            ..TurnEndTiming::default()
        });
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..SuperviseOpts::hosted()
        };
        let point = Turn {
            phase: Phase::Idle,
            screen: Screen {
                rows: named.clone(),
                seq: 102,
                cursor_col: 2,
                ..Screen::default()
            },
            timed_out: false,
        };
        let action = TurnEndAction::Restart {
            why: Restart::Model {
                to: "opus".to_string(),
            },
            rule_id: RULE_MODEL_FALLBACK,
            otherwise: Box::new(TurnEndAction::Nothing),
        };
        // The due is read off the monotonic clock inside the call, so it lies within the
        // call's own measured time past the reset's offset.
        let began = Instant::now();
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("the restart is deferred, not failed");
        let took = began.elapsed();
        let Due { at, why, .. } = s.turn_end_due.clone().expect("a retry is due");
        assert!(why.contains("restart:model waits"), "{why}");
        (at.duration_since(began), took)
    };
    // A share the test can see: the session's own for this reset.
    let span = Duration::from_secs(60);
    let share = resume_jitter("@s-1", reset_unix, span);
    assert!(
        share >= Duration::from_secs(5),
        "pick another sid: {share:?}"
    );
    let (due, took) = run(span);
    let want = Duration::from_secs(3 * 3600) + grace + share;
    assert!(
        due >= want && due <= want + took,
        "{due:?} vs {want:?} (the call took {took:?})"
    );
    let (due, took) = run(Duration::ZERO);
    let want = Duration::from_secs(3 * 3600) + grace;
    assert!(
        due >= want && due <= want + took,
        "no share: {due:?} vs {want:?} (the call took {took:?})"
    );
}

/// A return to the bucket's model the host keeps deferring (`wait:changed`) never holds the
/// point: the continuation is typed and the return is asked again at the next idle point.
/// Negative control: a return the host makes (`adopted`) types nothing of the loop's own.
#[test]
fn a_return_to_the_bucket_model_the_host_defers_never_holds_the_continuation() {
    let run = |answers: Vec<Option<&'static str>>| {
        let (dir, ledger) = ledger_file("back-waits");
        let unix_now = crate::supervise::journal::unix_ms() / 1000;
        super::turn_end::seed_switch(&ledger, Some("s-1"), unix_now - 60, false, false);
        let host = Arc::new(TestHost::default());
        *host.restarts.lock().unwrap() = answers.into();
        // A three-minute turn: continued at once, not after a short turn's back-off.
        let mut ended = rows(&["⏺ Done.", "", "✻ Worked for 3m 2s · done 4:24 PM", ""]);
        ended.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        let mut m = Mock::new(vec![busy_screen(), ended]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let (jdir, journal) = journal_file("back-waits");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let (r, lines) = hosted_with_host_ledger(
            &mut m,
            &opts,
            &host,
            Stop::After(Duration::from_millis(1500)),
            Some(TurnEndTiming {
                restart_backoff: vec![Duration::from_millis(20)],
                ..TurnEndTiming::default()
            }),
            &ledger,
        );
        assert_eq!(r, Ok(()));
        let asked = host.restarted.lock().unwrap().clone();
        let (records, _) = journal_records(&journal);
        let journaled: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&jdir);
        (asked, lines, journaled)
    };
    let (asked, lines, journaled) = run(vec![Some("wait:changed"); 200]);
    let continued = lines.iter().filter(|l| l.starts_with("CONTINUED")).count();
    assert_eq!(continued, 1, "{lines:#?}");
    assert_eq!(
        asked,
        ["model-back"],
        "asked at the point, not in a loop: {lines:#?}"
    );
    assert!(
        journaled
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.contains("restart:model-back")),
        "{journaled:#?}"
    );
    let (asked, lines, _) = run(vec![Some("adopted")]);
    assert_eq!(asked, ["model-back"]);
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CONTINUED")).count(),
        0,
        "{lines:#?}"
    );
}

/// A loop starting at a wall its predecessor already continued past (the ledger's `typed`
/// row) waits out the back-off from that act. Negative control: no such row, continued at once.
#[test]
fn a_restarted_loop_at_a_wall_its_predecessor_continued_past_waits_out_the_back_off() {
    let run = |seed: bool| {
        let (dir, ledger) = ledger_file("restart-rs");
        let (jdir, journal) = journal_file("restart-rs");
        if seed {
            let ts = crate::supervise::journal::unix_ms() - 3 * 60 * 1000;
            let row = format!(
                "{{\"ts\":{ts},\"sid\":\"s-1\",\"rule_id\":\"usage-resume@v1\",\"decision\":\"typed\",\"command\":\"keep going\",\"reason\":\"the turn-end policy\",\"seq\":1}}\n"
            );
            std::fs::write(&ledger, row).expect("seed the ledger");
        }
        let host = Arc::new(TestHost::default());
        let mut wall = rows(&[
            "⏺ Running the A/B on m7 now.",
            "  ⎿  You've hit your weekly limit · resets in 1s",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        wall.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        let mut m = Mock::new(vec![busy_screen(), wall]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            journal: Some(journal.clone()),
            ..SuperviseOpts::hosted()
        };
        let stop = if seed {
            Stop::After(Duration::from_secs(3))
        } else {
            Stop::reached(
                move |_| continued_on_usage(&journal),
                Duration::from_millis(300),
            )
        };
        let (r, lines) = hosted_with_host_ledger(
            &mut m,
            &opts,
            &host,
            stop,
            Some(TurnEndTiming {
                reset_grace: Duration::ZERO,
                reset_jitter: Duration::ZERO,
                ..TurnEndTiming::default()
            }),
            &ledger,
        );
        assert_eq!(r, Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&jdir);
        lines.iter().filter(|l| l.starts_with("CONTINUED")).count()
    };
    assert_eq!(run(false), 1, "unseeded: continued at once");
    assert_eq!(
        run(true),
        0,
        "seeded: the predecessor's act stands, ten minutes"
    );
}

// ---- the person fence at the write (`key|send if-human=<n>`) ------------

/// A person who keys between the loop's last read and its key gets the question first: the
/// fenced key is `OK skipped reason=person` ([`Press::Yielded`], ledgered, never escalated),
/// and the loop keys again once they are quiet, fenced on the new read's count. This host's
/// `send` names no generation fence, so `long-term` answers with the recommended option. Not
/// read as the person's, the skip would look like a guard that missed on the parsed screen and
/// hand the dialog over.
#[test]
fn a_person_the_server_sees_at_the_write_gets_the_question_first() {
    use crate::supervise::policy::QuestionPriority;
    use aterm_phase::prompt::fixtures as f;
    let (ldir, ledger) = ledger_file("person-at-write");
    let one = q(f::QUESTION_ONE);
    let mut m = questioned(vec![one.clone(), one.clone(), one, idle_screen()]);
    m.human = vec!["null", "200", "15000"];
    m.human_seq = Some(4);
    m.person_at_key = 1;
    let mut opts = answering(30, None);
    opts.policy.question_priority = QuestionPriority::LongTerm;
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.set_approval_ledger(Some(ledger.clone()));
    s.supervise(&opts).expect("supervise");
    drop(s);
    let presses = m.presses();
    let enter = format!(" if={} enter", guarded("❯ 1. Dark (Recommended)"));
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert!(
        presses.iter().all(|p| p.ends_with(&enter)),
        "both on the recommended option: {presses:?}"
    );
    assert!(
        presses[0].starts_with("key if-human=4 if-gen="),
        "the first fenced on the decision read's count: {presses:?}"
    );
    assert!(
        presses[1].starts_with("key if-human=5 if-gen="),
        "the second on the count of the read after the person's key: {presses:?}"
    );
    assert_eq!(m.person_skipped.len(), 1, "{:?}", m.requests);
    let reads_between = m
        .requests
        .iter()
        .skip_while(|r| !r.starts_with("key "))
        .skip(1)
        .take_while(|r| !r.starts_with("key "))
        .filter(|r| r.starts_with("text "))
        .count();
    assert_eq!(
        reads_between, 2,
        "the person's stamp read, then their quiet, before the key again: {:?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter().any(|r| r.contains("\"decision\":\"skipped\"")),
        "the yield is ledgered: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("\"rule_id\":\"answer-long-term@v1\"")
                && r.contains("\"decision\":\"approved\"")),
        "then answered: {rows:?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "never handed over for a person at the keys: {:?}",
        m.requests
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// How many steps a test's host takes before it stops wanting a point: a
/// loop that offers it the same point again at once runs to it in a few
/// milliseconds and then waits as any other, so the run ends as the session
/// goes and the bound on its steps fails — counted, never timed.
const HOST_STEP_CAP: usize = 64;

/// A host whose idle step takes nothing and then — `rewant` — asks for a
/// point again at once, as the window's host did for an agent whose
/// conversation its snapshot could not name yet (a Codex before its first
/// message): F1-R6-A1. Counts its steps, and stops wanting at
/// [`HOST_STEP_CAP`].
#[derive(Debug, Default)]
struct RewantHost {
    wants: AtomicBool,
    rewant: AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
}

impl IdleHost for RewantHost {
    fn wants(&self) -> bool {
        self.wants.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        let steps = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
        if !self.rewant.load(Ordering::SeqCst) || steps >= HOST_STEP_CAP {
            self.wants.store(false, Ordering::SeqCst);
        }
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

/// THE POINT A HOST'S STEP PASSED OVER IS NOT OFFERED IT AGAIN AT ONCE
/// (F1-R6-A1): a host whose step at an idle point takes nothing and still
/// wants a point is asked again only after a step of the loop's wait (or once
/// the screen moves), never in the same instant. Offered at once, the same
/// point went to the host thousands of times a second — 4718 steps and 14159
/// requests in two seconds, with not one wait between — for as long as
/// nobody prompted the agent: a busy loop on the loop's thread and on the
/// server. Here the steps are at most one per step of the wait — the waits
/// on the unchanged screen the Mock counts (`stalls`), never a look's own
/// `await idle` settle, which the spin runs once per step too (F1-R7 V-5) —
/// and more than one: the host's want is put off, never dropped. The run
/// ends as the session goes (the Mock's `vanish_after`), never at its budget
/// (the budget is the hang detector). CONTROL: a host that stops wanting
/// after its step is asked once.
#[test]
fn a_point_the_hosts_step_passed_over_waits_a_step_before_it_is_offered_again() {
    for rewant in [false, true] {
        let host = Arc::new(RewantHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.rewant.store(rewant, Ordering::SeqCst);
        let mut m = Mock::new(vec![busy_screen(), answered("keep going")]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(60, None)
        };
        opts.policy.continue_policy = false;
        let (lines, code) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
        });
        let steps = host.steps.load(Ordering::SeqCst);
        let waits = m.stalls as usize;
        assert_ne!(
            code, EXIT_TIMEOUT,
            "rewant={rewant}: ended by its budget: {lines:?}"
        );
        assert!(steps >= 1, "rewant={rewant}: the host is offered its point");
        assert!(
            steps <= waits + 1,
            "rewant={rewant}: one step per step of the wait at most: steps={steps} \
             waits={waits} requests={}",
            m.requests.len()
        );
        if rewant {
            // …and is asked again after a wait: its want is never dropped.
            assert!(
                steps >= 2,
                "asked again after a wait: steps={steps} waits={waits}"
            );
        } else {
            assert_eq!(steps, 1, "CONTROL: asked once");
        }
    }
}

/// A host as the window's runs its upgrade lane (`WorkerIdle::idle_step`):
/// it wants a point while its `park` stands, cleared as each step begins and
/// set again where the lane's word reads `After::NextIdle`
/// (`upgrade_drive::after`); it owns the turn ends as
/// `upgrade_drive::owns_turn_ends` says of the word, and its step's `moved`
/// and `typed` are the lane's (`upgrade_drive::moved`, `host_turn`). Each step
/// says the next word of its script, the last one again and again. A word
/// written `^word` is said as `word`, its park set all the same, whatever
/// it reads — a RACE: the host thread set it while the step ran (an
/// activation notice, the upgrade watch landing, a look turned into a park).
/// It stops wanting at [`HOST_STEP_CAP`]. Keeps the words it said, in order.
#[derive(Debug, Default)]
struct LaneHost {
    park: AtomicBool,
    owns: AtomicBool,
    script: std::sync::Mutex<VecDeque<&'static str>>,
    said: std::sync::Mutex<Vec<&'static str>>,
}

impl LaneHost {
    fn new(script: &[&'static str]) -> Arc<Self> {
        let host = Arc::new(Self::default());
        host.park.store(true, Ordering::SeqCst);
        *host.script.lock().unwrap() = script.iter().copied().collect();
        host
    }
    fn said(&self) -> Vec<&'static str> {
        self.said.lock().unwrap().clone()
    }
}

impl IdleHost for LaneHost {
    fn wants(&self) -> bool {
        self.park.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        use crate::harness::upgrade_drive as ud;
        self.park.store(false, Ordering::SeqCst);
        let word = {
            let mut script = self.script.lock().unwrap();
            if script.len() > 1 {
                script.pop_front().unwrap()
            } else {
                *script.front().unwrap()
            }
        };
        let (word, raced) = word.strip_prefix('^').map_or((word, false), |w| (w, true));
        let mut said = self.said.lock().unwrap();
        said.push(word);
        self.owns
            .store(ud::owns_turn_ends(word, 0, 0), Ordering::SeqCst);
        if said.len() < HOST_STEP_CAP && (raced || ud::after(word, 0) == ud::After::NextIdle) {
            self.park.store(true, Ordering::SeqCst);
        }
        Some(HostStep {
            line: format!("upgrade step={word}"),
            moved: ud::moved(word),
            typed: ud::host_turn(word),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        self.owns.load(Ordering::SeqCst)
    }
}

/// Claude Code idle at its composer after a finished turn: the point every
/// lane test's host is offered.
fn lane_point() -> Vec<String> {
    let mut worked = rows(&[
        "⏺ Fixed the parser; the suite is green.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    worked.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    worked
}

/// One run of `host` at [`lane_point`], the policy as the window's host runs
/// it (it continues an idle point: `keep going`) unless `continues` is
/// false, the session gone after `waits` waits on the unchanged screen. The
/// journal's lines in order (its `WAIT` rows left out), the Mock and the exit
/// code.
fn lane_run(
    tag: &str,
    host: &Arc<LaneHost>,
    waits: u32,
    continues: bool,
) -> (Vec<String>, Mock, u8) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(vec![busy_screen(), lane_point()]);
    m.cursor_on_caret = true;
    m.vanish_after = Some(waits);
    let mut opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        policy: SupervisorConfig::default(),
        ..auto(60, None)
    };
    opts.policy.continue_policy = continues;
    let (_, code) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let lines = records
        .into_iter()
        .map(|r| r.line)
        .filter(|l| !l.starts_with("WAIT"))
        .collect();
    (lines, m, code)
}

/// A GIVE-UP'S RELEASE GOES BEFORE THE POLICY'S CONTINUATION, AT ONCE
/// (F1-R7-B1): a give-up owns no turn end and owes the agent its release at
/// the next idle point (`gave-up` reads `After::NextIdle`, and its visit
/// leaves the release owed). Once the host owns nothing the wait decides the
/// point again, so a belt that passed every step that moved nothing over
/// had the policy type `keep going` there first and left the release owed:
/// two turns where one was owed. The point is offered the host again at
/// once, once, and its release is typed there; the policy types nothing.
#[test]
fn a_give_ups_release_goes_before_the_policys_continuation() {
    let host = LaneHost::new(&["gave-up", "released:gave-up", "current"]);
    let (lines, m, code) = lane_run("gave-up", &host, 6, true);
    assert_ne!(code, EXIT_TIMEOUT, "ended by its budget: {lines:?}");
    assert_eq!(host.said(), ["gave-up", "released:gave-up"], "{lines:?}");
    let release = lines
        .iter()
        .position(|l| l.contains("HOST ") && l.ends_with("upgrade step=released:gave-up"));
    assert!(release.is_some(), "the release is journaled: {lines:?}");
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "the policy typed nothing: {lines:?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{lines:?}");
}

/// THE FOLLOW-UPS OFFERED AT ONCE ARE COUNTED (F1-R7-B1, F1-R8-L1): a host
/// whose step takes something that moves nothing while it owns no turn end
/// gets the point back at once for its follow-up — but only
/// [`HOST_FOLLOW_UPS`] times: one more such step there passes it over, so no
/// word of that shape (a give-up said again and again, its release never
/// typed) loops — at most `HOST_FOLLOW_UPS + 1` steps per step of the wait,
/// the point decided between (the policy here continues nothing, so the
/// host is asked again after every wait; one that continues types once and
/// the host waits on that turn). CONTROL: the release that moves the session
/// ends the host's steps.
#[test]
fn the_follow_ups_offered_at_once_are_counted() {
    let burst = usize::from(HOST_FOLLOW_UPS) + 1;
    for (script, stuck) in [
        (&["gave-up"][..], true),
        (&["gave-up", "released:gave-up"][..], false),
    ] {
        let host = LaneHost::new(script);
        let (lines, m, code) = lane_run("follow-counted", &host, 4, false);
        let said = host.said();
        let waits = m.stalls as usize;
        assert_ne!(
            code, EXIT_TIMEOUT,
            "{script:?}: ended by its budget: {lines:?}"
        );
        assert!(
            said.len() <= burst * (waits + 1),
            "{script:?}: {burst} steps per step of the wait at most: steps={} waits={waits}",
            said.len()
        );
        if stuck {
            // EXACTLY the count's steps at once, then one passed over, at
            // the point and again after every step of the wait (F1-R9-L2:
            // an upper bound alone let a count of one or two through).
            assert_eq!(
                said.len(),
                burst * (waits + 1),
                "{burst} steps at the point and after each step of the wait: waits={waits}"
            );
        } else {
            assert_eq!(said, ["gave-up", "released:gave-up"], "CONTROL");
        }
    }
}

/// A RACED WAIT DOES NOT SPEND THE GIVE-UP'S FOLLOW-UP (F1-R8-L1): a wait
/// whose step a race on the host thread left wanting a point (here
/// `wait:failed`, owning nothing, its park set while it ran) has the
/// follow-up's shape — it took something, moved nothing, owns no turn end —
/// and took the point back at once. Offered so ONCE, the give-up that came
/// due behind it then passed the point over: the host owning nothing, the
/// wait decided it at once and the policy typed `keep going` before the
/// release that give-up owed (the release never offered: two turns where one
/// was owed). The follow-ups are counted now: the release is typed at the
/// point, the policy types nothing — behind TWO raced waits too, the count's
/// one to spare (F1-R9-L2: [`HOST_FOLLOW_UPS`] steps that move nothing, then
/// the release; a count of two passed the point over at the give-up, and the
/// policy typed `keep going`). CONTROL: the same lane with no race.
#[test]
fn a_raced_wait_does_not_spend_the_give_ups_follow_up() {
    for (script, said) in [
        (
            &["^wait:failed", "gave-up", "released:gave-up", "current"][..],
            &["wait:failed", "gave-up", "released:gave-up"][..],
        ),
        (
            &[
                "^wait:failed",
                "^wait:failed",
                "gave-up",
                "released:gave-up",
                "current",
            ][..],
            &["wait:failed", "wait:failed", "gave-up", "released:gave-up"][..],
        ),
        (
            &["gave-up", "released:gave-up", "current"][..],
            &["gave-up", "released:gave-up"][..],
        ),
    ] {
        let host = LaneHost::new(script);
        let (lines, m, code) = lane_run("raced-follow", &host, 6, true);
        assert_ne!(
            code, EXIT_TIMEOUT,
            "{script:?}: ended by its budget: {lines:?}"
        );
        assert_eq!(host.said(), said, "{script:?}: {lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("HOST ") && l.ends_with("upgrade step=released:gave-up")),
            "{script:?}: the release is journaled: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("CONTINUED")),
            "{script:?}: the policy typed nothing: {lines:?}"
        );
        assert_eq!(count(&m, "turn"), 0, "{script:?}: {lines:?}");
    }
}

/// THE AUTOCOMPACT OFFER's keep-first screen (aterm-phase's HAND-BUILT 2.1.302
/// fixture), and the same with the focus moved onto the accept row (`moved`)
/// and the stepper at `size`.
fn autocompact_offer(moved: bool, size: &str) -> Vec<String> {
    use aterm_phase::prompt::fixtures as f;
    f::screen(f::AUTOCOMPACT_KEEP_FIRST)
        .into_iter()
        .map(|r| match r.trim() {
            "❯ Keep auto (1m tokens)" if moved => "     Keep auto (1m tokens)".to_string(),
            "Autocompact at ‹ 400k › tokens" if moved => {
                format!("   ❯ Autocompact at ‹ {size} › tokens")
            }
            "Autocompact at ‹ 400k › tokens" => {
                format!("     Autocompact at ‹ {size} › tokens")
            }
            _ if r.contains("Want to autocompact at 400k") => r.replace("400k", size),
            _ => r,
        })
        .collect()
}

/// The offer is answered as the owner directed (2026-10-16): a fenced `down`
/// guarded on its title, then — on a fresh read showing the focus there —
/// Enter on the accept row itself, every key fenced on the judged read's person
/// count; ledgered `autocompact-offer@v1`, nothing asked of the owner, and
/// never Esc or the keep row. Under `approve = "none"` the same: no level limits
/// it.
#[test]
fn the_autocompact_offer_is_answered_at_400k_without_the_owner() {
    for approve in ["all", "none"] {
        let (dir, ledger) = ledger_file("autocompact-offer");
        let mut cfg = crate::supervise::SupervisorConfig::default();
        cfg.set("approve", approve).expect("a level");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted_with(&cfg)
        };
        let mut m = fenced(vec![
            autocompact_offer(false, "400k"),
            autocompact_offer(true, "400k"),
            idle_screen(),
        ]);
        m.human_seq = Some(3);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_autocompact_settle(Duration::ZERO);
        });
        assert!(
            lines[0].starts_with("APPROVED seq=") && lines[0].contains("Autocompact at ‹ 400k ›"),
            "{approve}: {lines:?}"
        );
        let title = crate::supervise::policy::row_guard(" We recommend compacting earlier");
        let accept = crate::supervise::policy::row_guard("   ❯ Autocompact at ‹ 400k › tokens");
        assert_eq!(
            m.presses(),
            [
                format!("key if-human=3 if-gen=1.101 if={title} down").as_str(),
                format!("key if-human=3 if-gen=1.102 if={accept} enter").as_str(),
            ],
            "{approve}: {:?}",
            m.requests
        );
        assert_eq!(m.attention, None, "{approve}: nothing asked of the owner");
        let rows = ledger_rows(&ledger);
        assert!(
            rows.iter()
                .any(|r| r.contains("autocompact-offer@v1") && r.contains("approved")),
            "{rows:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The stepper away from the target is stepped ONE key at a time, each on a
/// fresh read, and Enter only once the accept row reads the target: 500k →
/// `left` → 400k → Enter. CONTROL: the key off, the offer is the owner's (a
/// badge under its kind), nothing pressed.
#[test]
fn the_autocompact_offer_steps_to_400k_before_its_enter_and_off_is_the_owners() {
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let mut m = fenced(vec![
        autocompact_offer(true, "500k"),
        autocompact_offer(true, "400k"),
        autocompact_offer(true, "400k"),
        autocompact_offer(true, "400k"),
        idle_screen(),
    ]);
    m.human_seq = Some(0);
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_autocompact_settle(Duration::ZERO);
    });
    let at = |size: &str| {
        crate::supervise::policy::row_guard(&format!("   ❯ Autocompact at ‹ {size} › tokens"))
    };
    assert_eq!(
        m.presses(),
        [
            format!("key if-human=0 if-gen=1.101 if={} left", at("500k")).as_str(),
            format!("key if-human=0 if-gen=1.103 if={} enter", at("400k")).as_str(),
        ],
        "{:?}",
        m.requests
    );

    let mut off = crate::supervise::SupervisorConfig::default();
    off.set("autocompact_offer", "off").expect("an off");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&off)
    };
    let mut m = fenced(vec![autocompact_offer(false, "400k")]);
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_autocompact_settle(Duration::ZERO);
    });
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let badge = m.attention.clone().unwrap_or_default();
    assert!(
        badge.starts_with("claude autocompact-offer: ")
            && badge.contains("autocompact_offer is off"),
        "{badge:?}"
    );
}

/// A size list without the target (the server's: Claude Code accepts any
/// ascending list that holds the default) is never stepped round in circles:
/// 500k → `left` lands on 300k, and the `right` back is not pressed — the
/// offer is the owner's, said under its kind. And a list whose smallest size
/// is above the target: `left` at 500k moves nothing, and is not pressed again.
/// CONTROL: a list that holds the target is stepped onto it and entered
/// ([`the_autocompact_offer_steps_to_400k_before_its_enter_and_off_is_the_owners`]).
#[test]
fn the_autocompact_offer_stepper_stops_where_the_target_is_not_offered() {
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let at = |size: &str| {
        crate::supervise::policy::row_guard(&format!("   ❯ Autocompact at ‹ {size} › tokens"))
    };
    for (screens, why) in [
        (
            vec!["500k", "300k", "500k", "300k", "500k", "300k"],
            "turned back",
        ),
        (vec!["500k"], "at the end of the list"),
    ] {
        let mut m = fenced(
            screens
                .iter()
                .map(|size| autocompact_offer(true, size))
                .collect(),
        );
        m.human_seq = Some(0);
        // The moving screens end the session past their last; the one 500k
        // screen sits, each `await seq` on it timing out.
        m.vanish_after = (screens.len() > 1).then_some(0);
        m.stall_sleep = Some(Duration::from_millis(50));
        // Ended by the badge, or by a second stepper key (the regression).
        m.exit_when = Some(|m| {
            m.presses().len() >= 3
                || m.requests
                    .iter()
                    .any(|r| r.starts_with("meta set attention owner=supervisor"))
        });
        watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_autocompact_settle(Duration::ZERO);
        });
        let presses = m.presses();
        assert_eq!(presses.len(), 1, "{why}: {:?}", m.requests);
        assert!(
            presses[0].ends_with(&format!("if={} left", at("500k"))),
            "{why}: {presses:?}"
        );
        assert!(!presses.iter().any(|p| p.ends_with("enter")), "{presses:?}");
        let badge = m.attention.clone().unwrap_or_default();
        assert!(
            badge.starts_with("claude autocompact-offer: ")
                && badge.contains("400k is not among the sizes"),
            "{why}: {badge:?} {:?}",
            m.requests
        );
    }
}

/// The offer's own key guard: it is judged only once it has shown
/// [`AUTOCOMPACT_SETTLE`] (here shortened), however short the box settle — the
/// vendor refuses keys for 500 ms after it mounts and re-arms on each refused
/// one. An `await seq` stands between its first read and its first key.
#[test]
fn the_autocompact_offer_waits_its_own_settle_before_any_key() {
    let settle = Duration::from_millis(200);
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    // One screen that sits: each `await seq` on it times out after sleeping.
    let mut m = fenced(vec![autocompact_offer(true, "400k")]);
    m.human_seq = Some(0);
    m.stall_sleep = Some(settle);
    m.vanish_after = Some(2);
    let start = std::time::Instant::now();
    watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_autocompact_settle(settle);
    });
    let first_read = m
        .requests
        .iter()
        .position(|r| r.starts_with("text "))
        .expect("a read");
    let key = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .unwrap_or_else(|| panic!("a key: {:?}", m.requests));
    assert!(
        m.requests[first_read..key]
            .iter()
            .any(|r| r.starts_with("await seq ")),
        "{:?}",
        m.requests
    );
    assert!(start.elapsed() >= settle, "the settle was waited out");
}

/// A hold (`ERR halted`) on the offer's key parks the loop; no further key is sent
/// while it stands, and once it lifts the offer is decided again and answered.
#[test]
fn a_hold_on_the_autocompact_offer_parks_and_sends_nothing_more() {
    let (ldir, ledger) = ledger_file("autocompact-halted");
    let mut m = fenced(vec![
        autocompact_offer(true, "400k"),
        autocompact_offer(true, "400k"),
        idle_screen(),
    ]);
    m.human_seq = Some(0);
    m.key_replies
        .push_back(err("halted reason=owner origin=local"));
    m.hold = "1";
    m.hold_lifts_after = Some(2);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_autocompact_settle(Duration::ZERO);
    });
    let presses = m.presses();
    let first = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .expect("a key");
    assert_eq!(
        m.requests[first + 1..first + 3],
        ["status", "await inbox since=0 kinds=hold timeout 20000"],
        "{:?}",
        m.requests
    );
    assert!(presses.iter().all(|p| p.ends_with(" enter")), "{presses:?}");
    let rows = ledger_rows(&ledger);
    assert!(rows[0].contains("\"decision\":\"deferred\""), "{rows:?}");
    let _ = std::fs::remove_dir_all(&ldir);
}

/// The real Muse reader over the existing scripted control server, with an
/// old person stamp outside the default grace; only its count is varied.
fn muse_focus_control(screens: Vec<Vec<String>>) -> Mock {
    let mut m = fenced(screens);
    m.program = "muse";
    m.human = vec!["150000"];
    m.human_ms = Some(150000);
    m.vanish_after = Some(0);
    m.exit_when = Some(|m| {
        !m.person_skipped.is_empty()
            || m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention owner=supervisor"))
    });
    m
}

fn watch_muse_focus(m: &mut Mock) {
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    watch_lines_with(m, &opts, |s| s.set_approval_env(owner_env()));
}

#[test]
fn a_muse_question_without_a_person_count_is_not_focused_or_entered() {
    use aterm_phase::muse::fixtures as u;
    let screens = || vec![q(u::QUESTION_DOWN), q(u::QUESTION), q(u::QUESTION_ANSWERED)];
    let mut known = muse_focus_control(screens());
    known.human_seq = Some(3);
    watch_muse_focus(&mut known);
    assert_eq!(known.presses().len(), 2, "CONTROL: {:?}", known.requests);
    assert!(known.presses()[0].ends_with(" up"));
    assert!(known.presses()[1].ends_with(" enter"));

    let mut unknown = muse_focus_control(screens());
    unknown.human_seq = None;
    watch_muse_focus(&mut unknown);
    assert!(unknown.reads.iter().any(|(i, _)| *i == 0));
    assert!(
        unknown.presses().is_empty(),
        "a missing count cannot start a Muse question answer: {:?}",
        unknown.requests
    );
    assert!(
        known
            .presses()
            .iter()
            .all(|k| k.starts_with("key if-human=3 if-gen="))
    );
}

#[test]
fn a_muse_permission_without_a_person_count_is_not_entered() {
    use aterm_phase::muse::fixtures as u;
    let screens = || vec![q(u::BOX_SHELL), q(u::BUSY_1_4_10)];
    let mut known = muse_focus_control(screens());
    known.human_seq = Some(3);
    watch_muse_focus(&mut known);
    assert_eq!(known.presses().len(), 1, "CONTROL: {:?}", known.requests);
    assert!(known.presses()[0].ends_with(" enter"));

    let mut unknown = muse_focus_control(screens());
    unknown.human_seq = None;
    watch_muse_focus(&mut unknown);
    assert!(unknown.reads.iter().any(|(i, _)| *i == 0));
    assert!(
        unknown.presses().is_empty(),
        "a missing count cannot start a Muse permission answer: {:?}",
        unknown.requests
    );
    assert!(known.presses()[0].starts_with("key if-human=3 if-gen="));
}

/// A real counter change at dispatch is already respected by the one-shot
/// answer. The watching loop must carry the same count to its first key.
#[test]
fn a_person_at_a_muse_focus_key_gets_the_question_before_another_key() {
    use aterm_phase::muse::fixtures as u;
    let mut controlled = muse_focus_control(vec![q(u::QUESTION), q(u::QUESTION_DOWN)]);
    controlled.human_seq = Some(3);
    controlled.person_at_key = 1;
    let (line, code) = controller_answer(&mut controlled, "2", None);
    assert_eq!(code, 0, "CONTROL: {line}");
    assert!(line.starts_with("LEFT "), "CONTROL: {line}");
    assert_eq!(controlled.human_seq, Some(4));
    assert_eq!(controlled.person_skipped.len(), 1);
    assert_eq!(controlled.presses().len(), 1);

    let mut watched = muse_focus_control(vec![
        q(u::QUESTION_DOWN),
        q(u::QUESTION),
        q(u::QUESTION_ANSWERED),
    ]);
    watched.human_seq = Some(3);
    watched.person_at_key = 1;
    watched.human_on.insert(1, "0");
    watched.status_human_from.insert(2, 0);
    watch_muse_focus(&mut watched);
    assert_eq!(watched.human_seq, Some(4));
    assert_eq!(
        watched.person_skipped.len(),
        1,
        "the watched Muse focus key must yield to the actual counter: {:?}",
        watched.requests
    );
    assert_eq!(watched.presses().len(), 1, "{:?}", watched.requests);
    assert!(watched.presses()[0].starts_with("key if-human=3 if-gen="));
}

/// The answer is for one question, even when a different question happens
/// to put the same label under its cursor after the accepted focus move.
#[test]
fn a_muse_focus_move_cannot_answer_a_replacement_question() {
    use aterm_phase::muse::fixtures as u;
    let mut same = muse_focus_control(vec![
        q(u::QUESTION),
        q(u::QUESTION_DOWN),
        q(u::QUESTION_ANSWERED),
    ]);
    same.human_seq = Some(3);
    let (line, code) = controller_answer(&mut same, "2", None);
    assert_eq!(code, 0, "CONTROL: {line}");
    assert_eq!(same.presses().len(), 2);

    let first = q(u::QUESTION);
    let replacement: Vec<String> = q(u::QUESTION_DOWN)
        .into_iter()
        .map(|r| {
            r.replace(
                "Which color should the probe use?",
                "Which color should the next task use?",
            )
        })
        .collect();
    let reader = aterm_phase::Program::Muse.reader();
    let old = reader.prompt(&first).expect("the first question");
    let new = reader
        .prompt(&replacement)
        .expect("a different complete question");
    assert_ne!(
        crate::supervise::run::prompt_box_token(&old),
        crate::supervise::run::prompt_box_token(&new),
        "the actual questions differ"
    );
    assert_eq!(old.command, new.command);
    assert_eq!(old.options[1].label, new.focused().unwrap().label);

    let mut changed = muse_focus_control(vec![first, replacement, q(u::QUESTION_ANSWERED)]);
    changed.human_seq = Some(3);
    let token = crate::supervise::run::prompt_box_token(&old).expect("the original box");
    let (line, code) = controller_answer(&mut changed, "2", Some(&token));
    assert_eq!(
        (
            code,
            changed.presses().iter().any(|k| k.ends_with(" enter"))
        ),
        (3, false),
        "a replacement question cannot inherit the previous answer: {line} {:?}",
        changed.requests
    );
    assert_eq!(changed.presses().len(), 1);
    assert!(changed.presses()[0].ends_with(" down"));
}

/// A generation tick may need another read of the same question. That
/// read omitting the field cannot remove the first read's person fence.
#[test]
fn a_controllers_muse_answer_keeps_its_count_after_a_generation_retry() {
    use aterm_phase::muse::fixtures as u;
    let mut m = muse_focus_control(vec![
        q(u::QUESTION),
        q(u::QUESTION),
        q(u::QUESTION_ANSWERED),
    ]);
    m.human_seq = Some(3);
    m.human_seq_on.insert(1, None);
    m.tick_at_key = 1;
    let (line, code) = controller_answer(&mut m, "recommended", None);
    assert_eq!(
        code, 0,
        "the retry must still answer: {line} {:?}",
        m.requests
    );
    assert!(line.starts_with("ANSWERED "), "{line}");
    assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    assert!(m.reads.iter().any(|(i, _)| *i == 1));
    assert!(
        m.presses().iter().all(|k| {
            let key = k.strip_prefix("@s-w ").unwrap_or(k.as_str());
            key.starts_with("key if-human=3 if-gen=")
        }),
        "a generation retry cannot discard the captured person count: {:?}",
        m.requests
    );
}

/// The actual Session over its scripted control server, with a known old
/// person stamp. Only the wire counter is varied by the tests below.
fn watch_autocompact_person(m: &mut Mock) {
    m.human = vec!["150000"];
    m.human_ms = Some(150000);
    m.vanish_after = Some(0);
    m.exit_when = Some(|m| {
        !m.person_skipped.is_empty()
            || m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention owner=supervisor"))
    });
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    watch_lines_with(m, &opts, |s| {
        s.set_approval_env(owner_env());
        s.set_autocompact_settle(Duration::ZERO);
    });
}

/// CONTROL: a known count permits the real confirmed focus move and Enter.
/// With that count absent on the initial read, neither key may be requested.
#[test]
fn an_autocompact_offer_without_a_person_count_is_not_focused_or_entered() {
    let screens = || {
        vec![
            autocompact_offer(false, "400k"),
            autocompact_offer(true, "400k"),
            idle_screen(),
        ]
    };
    let mut known = fenced(screens());
    known.human_seq = Some(3);
    watch_autocompact_person(&mut known);
    assert_eq!(known.presses().len(), 2, "CONTROL: {:?}", known.requests);
    assert!(
        known.presses()[0].ends_with(" down"),
        "CONTROL: {:?}",
        known.requests
    );
    assert!(
        known.presses()[1].ends_with(" enter"),
        "CONTROL: {:?}",
        known.requests
    );
    assert!(
        known
            .presses()
            .iter()
            .all(|p| p.starts_with("key if-human=3 if-gen="))
    );

    let mut unknown = fenced(screens());
    unknown.human_seq = None;
    watch_autocompact_person(&mut unknown);
    assert!(
        unknown.reads.iter().any(|(i, _)| *i == 0),
        "the real offer was read"
    );
    assert!(
        unknown.presses().is_empty(),
        "no unfenced focus move or Enter: {:?}",
        unknown.requests
    );
}

/// CONTROL: with a count the real stepper moves, then confirms the target.
/// A missing initial count permits neither the horizontal key nor Enter.
#[test]
fn an_autocompact_stepper_without_a_person_count_is_not_moved() {
    let screens = || {
        vec![
            autocompact_offer(true, "500k"),
            autocompact_offer(true, "400k"),
            autocompact_offer(true, "400k"),
            autocompact_offer(true, "400k"),
            idle_screen(),
        ]
    };
    let mut known = fenced(screens());
    known.human_seq = Some(3);
    watch_autocompact_person(&mut known);
    assert_eq!(known.presses().len(), 2, "CONTROL: {:?}", known.requests);
    assert!(
        known.presses()[0].ends_with(" left"),
        "CONTROL: {:?}",
        known.requests
    );
    assert!(
        known.presses()[1].ends_with(" enter"),
        "CONTROL: {:?}",
        known.requests
    );
    assert!(
        known
            .presses()
            .iter()
            .all(|p| p.starts_with("key if-human=3 if-gen="))
    );

    let mut unknown = fenced(screens());
    unknown.human_seq = None;
    watch_autocompact_person(&mut unknown);
    assert!(
        unknown.reads.iter().any(|(i, _)| *i == 0),
        "the real stepper was read"
    );
    assert!(
        unknown.presses().is_empty(),
        "no unfenced stepper key or Enter: {:?}",
        unknown.requests
    );
}

/// The count captured before the focus move remains the answering key's
/// fence even when that move's readback omits the formerly supplied field.
#[test]
fn the_autocompact_person_count_survives_an_omitted_readback_field() {
    let mut m = fenced(vec![
        autocompact_offer(false, "400k"),
        autocompact_offer(true, "400k"),
        idle_screen(),
    ]);
    m.human_seq = Some(3);
    m.human_seq_on.insert(1, None);
    watch_autocompact_person(&mut m);
    assert!(
        m.reads.iter().any(|(i, _)| *i == 1),
        "actual moved-focus readback"
    );
    assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    assert!(m.presses()[0].ends_with(" down"));
    assert!(m.presses()[1].ends_with(" enter"));
    assert!(
        m.presses()
            .iter()
            .all(|p| p.starts_with("key if-human=3 if-gen=")),
        "{:?}",
        m.requests
    );
}

/// A real Mock server counter change at dispatch yields the first focus
/// arrow, or an already-focused Enter, without requesting another key.
#[test]
fn a_person_at_an_autocompact_key_gets_it_before_any_other_key() {
    for moved in [false, true] {
        let mut m = fenced(vec![
            autocompact_offer(moved, "400k"),
            autocompact_offer(true, "400k"),
            idle_screen(),
        ]);
        m.human_seq = Some(3);
        m.person_at_key = 1;
        // The gesture advances both the count and its real recent-input age.
        // The next text read and following status must agree about that hand.
        m.human_on.insert(1, "0");
        m.status_human_from.insert(2, 0);
        watch_autocompact_person(&mut m);
        assert_eq!(m.human_seq, Some(4), "the server's actual count advanced");
        assert_eq!(
            m.person_skipped.len(),
            1,
            "the actual if-human guard skipped: {:?}",
            m.requests
        );
        assert_eq!(
            m.presses().len(),
            1,
            "nothing requested after the yield: {:?}",
            m.requests
        );
        assert!(m.presses()[0].starts_with("key if-human=3 if-gen="));
        let key = if moved { " enter" } else { " down" };
        assert!(m.presses()[0].ends_with(key), "{:?}", m.requests);
    }
}

/// A WORD THAT MOVED BETWEEN FOLLOW-UPS DOES NOT START THEIR COUNT AGAIN
/// (F1-R9-L1): a step that moved the session (`adopted`) and left the host
/// wanting a point reset the count of the point's follow-ups
/// ([`HOST_FOLLOW_UPS`]), so any run of up to that many steps that moved
/// nothing (a give-up said again, a raced wait), each followed by such a
/// word, went back to the host at once with no step of the wait between —
/// to the host's own cap, the round seven residual made a loop of three
/// words where it had been one of two. The count is the point's: a step
/// that moved nothing spends one, a step that moved spends none and keeps
/// it, and only a step of the wait, another point or a turn the host typed
/// starts it again — so at most `2 * (HOST_FOLLOW_UPS + 1)` steps between
/// two steps of the wait whatever the words (a moved word before each step
/// that moved nothing, the last of those passed over), the point asked again
/// after each wait. CONTROL: steps that each move nothing are bounded as
/// before.
#[test]
fn a_word_that_moved_between_follow_ups_does_not_start_their_count_again() {
    let burst = 2 * (usize::from(HOST_FOLLOW_UPS) + 1);
    for cycle in [
        &["gave-up", "gave-up", "^adopted"][..],
        &["gave-up", "gave-up", "gave-up", "^adopted"][..],
        &["gave-up", "^adopted"][..],
        &["gave-up"][..],
    ] {
        let script: Vec<&'static str> = cycle
            .iter()
            .copied()
            .cycle()
            .take(HOST_STEP_CAP + 1)
            .collect();
        let host = LaneHost::new(&script);
        let (lines, m, code) = lane_run("moved-follow", &host, 4, false);
        let said = host.said();
        let waits = m.stalls as usize;
        assert_ne!(
            code, EXIT_TIMEOUT,
            "{cycle:?}: ended by its budget: {lines:?}"
        );
        assert!(
            said.len() < HOST_STEP_CAP,
            "{cycle:?}: the point went back to the host to its cap: steps={} waits={waits}",
            said.len()
        );
        assert!(
            said.len() <= burst * (waits + 1),
            "{cycle:?}: {burst} steps per step of the wait at most: steps={} waits={waits}",
            said.len()
        );
        assert!(
            said.len() > burst,
            "{cycle:?}: asked again after a wait: {said:?} waits={waits}"
        );
        assert_eq!(count(&m, "turn"), 0, "{cycle:?}: nothing typed: {lines:?}");
    }
}

/// Gemini's automatic digit routes: one-shot tool permission, folder trust,
/// and a question. Each fixture must reach the actual watch policy before a
/// guard-negative case can say anything about its press.
const GEMINI_DIGIT_BOXES: [(&str, &str); 3] = [
    ("tool", aterm_phase::gemini::fixtures::BOX_SHELL),
    ("trust", aterm_phase::gemini::fixtures::TRUST),
    ("question", aterm_phase::gemini::fixtures::QUESTION),
];

fn gemini_digit_server(fixture: &str) -> Mock {
    let mut m = questioned(vec![
        q(fixture),
        q(aterm_phase::gemini::fixtures::BUSY_THINKING),
    ]);
    m.program = "node";
    m.human = vec!["150000"];
    m.human_ms = Some(150_000);
    m.human_seq = Some(7);
    m.key_releases = Some(0);
    m.vanish_after = Some(2);
    m
}

fn watch_gemini_digit(m: &mut Mock) -> Vec<String> {
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    watch_lines(m, &opts).0
}

fn gemini_digit_positive(fixture: &str) {
    let mut m = gemini_digit_server(fixture);
    let lines = watch_gemini_digit(&mut m);
    assert_eq!(m.presses().len(), 1, "{lines:#?}\n{:#?}", m.requests);
    assert!(m.presses()[0].ends_with(" 1"), "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.starts_with("APPROVED ")),
        "the real watch approved the control: {lines:#?}\n{:#?}",
        m.requests
    );
}

/// All three automatic routes have an admitted control, then the same
/// prompt read with no person count. The aggregate keeps every negative
/// reached even when the first route wrongly writes a key.
#[test]
fn an_automatic_digit_requires_the_judged_reads_person_count() {
    let mut wrote = Vec::new();
    for (name, fixture) in GEMINI_DIGIT_BOXES {
        gemini_digit_positive(fixture);
        let mut m = gemini_digit_server(fixture);
        m.human_seq = None;
        let lines = watch_gemini_digit(&mut m);
        if !m.presses().is_empty() {
            wrote.push((name, lines, m.requests));
        }
    }
    assert!(wrote.is_empty(), "missing-count digit writes: {wrote:#?}");
}

/// A person's input after the judged read is checked by the actual Mock
/// key dispatcher. The next read/status also names that fresh gesture, so
/// this does not ask a later, separately quiet decision never to act.
#[test]
fn a_person_who_keys_before_an_automatic_digit_gets_the_box() {
    let mut crossed = Vec::new();
    for (name, fixture) in GEMINI_DIGIT_BOXES {
        gemini_digit_positive(fixture);
        let mut m = gemini_digit_server(fixture);
        m.person_at_key = 1;
        m.human_on.insert(1, "0");
        m.status_human_from.insert(2, 0);
        let lines = watch_gemini_digit(&mut m);
        assert_eq!(m.human_seq, Some(8), "the race reached the dispatcher");
        if m.person_skipped.len() != 1 || lines.iter().any(|l| l.starts_with("APPROVED ")) {
            crossed.push((name, lines, m.requests, m.person_skipped));
        }
    }
    assert!(
        crossed.is_empty(),
        "the person's digit crossed: {crossed:#?}"
    );
}

/// The automatic route must not turn an unsupported generation capability,
/// an omitted generation, or an explicit guarded-form refusal into a row-only
/// or plain digit. The old host's own reply remains the actual probe.
#[test]
fn an_automatic_digit_never_downgrades_its_generation_guard() {
    let mut downgraded = Vec::new();
    for (name, fixture) in GEMINI_DIGIT_BOXES {
        gemini_digit_positive(fixture);
        for case in ["unsupported", "omitted", "refused"] {
            let mut m = gemini_digit_server(fixture);
            match case {
                "unsupported" => {
                    m.gen_fence = false;
                    m.help = "key [if=<re>] <name>: send a named key\n".to_string();
                }
                "omitted" => m.sends_gen = false,
                "refused" => m.key_replies.push_back(bare_err()),
                _ => unreachable!(),
            }
            let lines = watch_gemini_digit(&mut m);
            let weaker: Vec<String> = m
                .presses()
                .into_iter()
                .filter(|r| {
                    !r.contains("if-human=") || !r.contains("if-gen=") || !r.contains(" if=")
                })
                .cloned()
                .collect();
            let missing_proof_pressed = case != "refused" && !m.presses().is_empty();
            if missing_proof_pressed
                || !weaker.is_empty()
                || lines.iter().any(|l| l.starts_with("APPROVED "))
            {
                downgraded.push((name, case, lines, m.requests, weaker));
            }
        }
    }
    assert!(downgraded.is_empty(), "guard downgrades: {downgraded:#?}");
}

fn gemini_controller_digit() -> Mock {
    let mut m = questioned(vec![
        q(aterm_phase::gemini::fixtures::QUESTION),
        q(aterm_phase::gemini::fixtures::QUESTION_ANSWERED),
    ]);
    m.program = "node";
    m.human = vec!["150000"];
    m.human_seq = Some(7);
    m
}

fn gemini_controller_positive() {
    let mut m = gemini_controller_digit();
    let (line, code) = controller_answer(&mut m, "1", None);
    assert_eq!(code, 0, "{line}\n{:#?}", m.requests);
    assert!(line.starts_with("ANSWERED "), "{line}");
    assert_eq!(m.presses().len(), 1, "{:#?}", m.requests);
    assert!(m.presses()[0].ends_with(" 1"), "{:#?}", m.requests);
}

/// A controller's explicit choice is still an automated write: there is no
/// trusted manual-human origin in AnswerOpts that could waive the fence.
#[test]
fn a_controllers_digit_answer_requires_a_known_person_count() {
    gemini_controller_positive();
    let mut m = gemini_controller_digit();
    m.human_seq = None;
    let (line, code) = controller_answer(&mut m, "1", None);
    assert!(
        m.presses().is_empty(),
        "missing-count controller digit wrote: {line} code={code}\n{:#?}",
        m.requests
    );
    assert_eq!(code, EXIT_NOT_SERVED, "{line}");
}

#[test]
fn a_person_who_keys_before_a_controllers_digit_keeps_the_question() {
    gemini_controller_positive();
    let mut m = gemini_controller_digit();
    m.person_at_key = 1;
    let (line, code) = controller_answer(&mut m, "1", None);
    assert_eq!(m.human_seq, Some(8), "the race reached the dispatcher");
    assert_eq!(
        m.person_skipped.len(),
        1,
        "the person's controller digit crossed: {line}\n{:#?}",
        m.requests
    );
    assert!(line.starts_with("LEFT "), "{line}");
    assert_eq!(code, 0, "{line}");
}

#[test]
fn a_controllers_digit_answer_requires_a_known_quiet_stamp() {
    gemini_controller_positive();
    let mut m = gemini_controller_digit();
    m.human.clear();
    let (line, code) = controller_answer(&mut m, "1", None);
    assert!(
        m.presses().is_empty(),
        "unknown-quiet controller digit wrote: {line} code={code}\n{:#?}",
        m.requests
    );
    assert_eq!(code, EXIT_NOT_SERVED, "{line}");
}
