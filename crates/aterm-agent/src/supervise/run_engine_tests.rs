// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The engine over the scripted server (`Mock`): the approval policy in the
// loop, the press guard and fence, the wake on a ticking screen, the
// lifecycle (a hold, a refusal, a stale badge, the end) and the escalation
// (questions, the fabric's state), and the hosted entry. Included into
// `run.rs`'s test module (`mod engine`), where `Mock` and its helpers live.

use crate::supervise::policy::WorkerEnv;

/// The measured 2.1.280 rm circuit-breaker box (`policy/fixtures/cap-rm.txt`)
/// with its command row replaced by `cmd`.
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

/// The owner's process inputs, fixed: a hermetic worker.
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
    let dir = std::env::temp_dir().join(format!("aterm-ledger-{tag}-{}", std::process::id()));
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

/// THE SWAP (audit APR-6): the box judged read-only is replaced by another
/// between the loop's read and its press. The press is guarded by the judged
/// row, so the server writes nothing (`OK skipped`); the loop reads again,
/// decides the NEW box — a write — and hands it over. The old guard
/// (`Do.you.want.to.proceed`) matched the swapped box and approved it.
#[test]
fn a_box_swapped_between_the_read_and_the_press_is_not_pressed() {
    let (dir, notes) = notes_file("swap");
    let mut m = Mock::new(true, vec![bash_one_row()]);
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
    assert_eq!(m.presses(), [G_PRESS], "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert!(
        !lines.iter().any(|l| l.contains("approved")),
        "nothing approved: {lines:?}"
    );
    assert!(
        lines[0].ends_with("nothing pressed, the box had left the screen: git log --oneline -5"),
        "{lines:?}"
    );

    // Negative control: the guard of the judged row matches no row of the
    // swapped box on the server's own matcher, and matches its own box.
    let m =
        aterm_observe::row_matcher(G_PRESS.trim_start_matches("key if=").trim_end_matches(" 1"))
            .expect("compiles");
    assert!(!write_prompt().iter().any(|r| m.matches(r)));
    assert!(bash_one_row().iter().any(|r| m.matches(r)));
}

/// A server that fences on the screen generation (its `help key` names
/// `if-gen=`, and `text --json` carries `"gen"`): the press carries the judged
/// read's generation; a row that ticked between the read and the press is `OK
/// skipped reason=changed`, and the loop reads and decides again and presses
/// the box on the fresh read's generation. A screen that ticks under every
/// press is HANDED OVER after five such skips — never pressed with the fence
/// dropped (the safety review of 2026-09-24: the row guard alone binds only
/// the box's first command row, which a swapped box can show), never
/// answered on a stale read, never stuck. The probe reads the server's REAL
/// `help key` answer (the entry wrapped over many rows, the fence named on a
/// continuation row).
#[test]
fn a_fenced_press_on_a_moved_screen_is_decided_again_from_a_fresh_read() {
    let help = aterm_types::control_verbs::spec("key")
        .expect("the key verb")
        .entry_lines()
        .join("\n");
    let fenced = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(true, screens);
        m.help.clone_from(&help);
        m.gen_fence = true;
        m.sends_gen = true;
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
    assert_eq!(*presses[0], format!("key if-gen=1.101 {g}"));
    assert_eq!(*presses[1], format!("key if-gen=1.103 {g}"));
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
        presses.iter().all(|p| p.starts_with("key if-gen=")),
        "never unfenced: {presses:?}"
    );

    // Negative controls. A server whose `help key` names no fence gets the
    // row guard alone. So does one whose help names only the retired
    // `if-seq=` (the probe does not mistake it for the fence). A read that
    // carries no generation sends no fence, whatever the help says. And a
    // server that does not know the fence form (a bare `ERR`) is asked again
    // without it — never unguarded.
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.sends_gen = true;
    let mut s = session(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(m.presses(), [G_PRESS]);
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.help = "key [id=<key>] [if-seq=<n>] [if=<re>] <name>: send a named key\n".to_string();
    m.sends_gen = true;
    let mut s = session(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(s.caps().gen_fence, Some(false));
    assert_eq!(m.presses(), [G_PRESS]);
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.help.clone_from(&help);
    m.gen_fence = true;
    let mut s = session(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(m.presses(), [G_PRESS], "{:?}", m.requests);
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.help.clone_from(&help);
    m.sends_gen = true;
    let mut s = session(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(s.caps().gen_fence, Some(false));
    let presses = m.presses();
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert!(presses[0].starts_with("key if-gen=1.101 "));
    assert_eq!(presses[1], G_PRESS);
}

/// D9 in the loop: a retired key's limit reaches the box. Under `approve =
/// "safe"` a Read box of `/etc/hosts` (a system root) is pressed; with
/// 0.93.0's `read_outside_cwd = false` in the file the same box goes to the
/// manager, the Read rule left no root. Likewise `rm_breaker = false` leaves
/// the rm breaker no scratch root.
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
        let mut m = Mock::new(true, vec![read_box.clone()]);
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
    // The breaker's roots go the same way (the rule itself is
    // `approval_tests`'): the policy carries it to the context.
    let (p, _) = SupervisorConfig::from_aterm_toml("[harness]\nrm_breaker = false\n");
    assert!(p.withheld.rm_breaker && p.approve == Approve::Safe, "{p:?}");
}

/// THE WAKE (audit LV-3): a box drawn where the busy footer was, under a
/// screen that animates so `await idle` never latches, is read the moment
/// the footer leaves — the busy read re-arms `await gone`, and no idle step
/// comes between the busy read and the press. Measured before: 12-18 s per
/// box. The negative control is an older host without `await gone`: its
/// busy read is followed by `await idle 2000`, the step that waits out an
/// animating screen.
#[test]
fn a_read_box_under_a_ticking_screen_is_approved_within_one_wake() {
    let mut m = Mock::new(
        true,
        vec![busy_screen(), busy_screen(), bash_one_row(), busy_screen()],
    );
    m.idle_never = true;
    m.vanish_after = Some(0);
    let started = Instant::now();
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    let elapsed = started.elapsed();
    assert_eq!(
        lines[0], "APPROVED seq=103 git log --oneline -5",
        "{lines:?}"
    );
    let press = m
        .requests
        .iter()
        .position(|r| r == G_PRESS)
        .expect("pressed");
    assert!(
        !m.requests[..press]
            .iter()
            .any(|r| r.starts_with("await idle")),
        "no idle step before the press: {:?}",
        m.requests
    );
    assert_eq!(
        m.requests[..press],
        [
            "meta",
            "await gone esc.to.interrupt timeout 20000",
            "text --json tail=40",
            "await gone esc.to.interrupt timeout 20000",
            "text --json tail=40",
            "await gone esc.to.interrupt timeout 20000",
            "text --json tail=40",
            "status",
            "meta",
            "help key",
        ]
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");

    let mut m = Mock::new(false, vec![busy_screen(), bash_one_row()]);
    m.idle_never = true;
    let mut s = session(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert!(
        m.requests
            .windows(2)
            .any(|w| w[0] == "await seq 101 timeout 20000"
                && w[1] == "await idle 2000 timeout 20000"),
        "an older host steps on idle: {:?}",
        m.requests
    );
}

/// `ERR halted` — an owner's `hold` — neither ends the watch nor is pressed
/// through: the loop parks (`status hold=1`, then the hold's transition,
/// `await inbox since=0 kinds=hold`), and once `status` says `hold=0` it
/// reads and decides the box again and presses it. Journaled `HALTED` and
/// `UNHALTED`, and the refused press is a ledger row.
#[test]
fn err_halted_parks_until_the_hold_lifts_and_the_loop_goes_on() {
    let (jdir, journal) = journal_file("halted");
    let (ldir, ledger) = ledger_file("halted");
    let mut m = Mock::new(
        true,
        vec![bash_one_row(), bash_one_row(), busy_screen(), idle_screen()],
    );
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
    assert_eq!(m.presses(), [G_PRESS, G_PRESS], "{:?}", m.requests);
    let first = m.requests.iter().position(|r| r == G_PRESS).expect("press");
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
    assert!(notes[0].starts_with("HALTED seq=101 key if="), "{notes:?}");
    assert!(notes[1].starts_with("UNHALTED seq=101 after "), "{notes:?}");
    let rows = ledger_rows(&ledger);
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
    let mut m = Mock::new(
        true,
        vec![bash_one_row(), bash_one_row(), busy_screen(), idle_screen()],
    );
    m.key_replies.push_back(err("busy lease=s-9"));
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert_eq!(
        lines[0], "APPROVED seq=102 git log --oneline -5",
        "{lines:?}"
    );
    let first = m.requests.iter().position(|r| r == G_PRESS).expect("press");
    assert!(
        m.requests[first + 1].starts_with("await gone ^\\x20{3}git"),
        "the back-off waits on the box leaving: {:?}",
        m.requests
    );
}

/// THE STALE BADGE (audit SUP-6): a watch that starts over a `limited:`
/// badge a previous watcher left, with the screen idle, unsets it before its
/// first wait. Negative controls: another owner's badge is left alone (the
/// key is the proof of ownership, never the text); a
/// badge of ours whose box is still on the screen is ADOPTED — no second
/// badge and no second ask for that box — and cleared when the box goes.
#[test]
fn a_stale_attention_is_reconciled_at_the_start() {
    let (dir, journal) = journal_file("stale");
    let mut m = Mock::new(true, vec![idle_screen()]);
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

    let mut m = Mock::new(true, vec![idle_screen()]);
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
    let mut m = Mock::new(true, vec![rm.clone(), rm, busy_screen(), idle_screen()]);
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

/// THE RM BREAKER in the loop (owner decision 1): in a bypass session (its
/// footer read before the box) with the session's cwd known (`meta`), a
/// breaker whose every operand resolves under a scratch root is pressed under
/// its command row and ledgered `rm-breaker@v2`; `S=/usr` escalates, badged
/// `claude rm-breaker: <command> (<reason>)`. Negative controls: the same
/// scratch command escalates with the cwd unknown, and outside bypass.
#[test]
fn the_rm_breaker_is_approved_under_a_scratch_root_and_escalated_otherwise() {
    let (ldir, ledger) = ledger_file("rm");
    let mut m = Mock::new(true, vec![bypass_busy(), rm_box(SCRATCH_RM), busy_screen()]);
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
    let press = format!("key if={guard} 1");
    assert_eq!(m.presses(), [press.as_str()], "{:?}", m.requests);
    let rows = ledger_rows(&ledger);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("\"rule_id\":\"rm-breaker@v2\""),
        "{rows:?}"
    );
    assert!(rows[0].contains("\"decision\":\"approved\""), "{rows:?}");
    let _ = std::fs::remove_dir_all(&ldir);

    let usr = "S=/usr; rm -rf \"$S/t5\"";
    let mut m = Mock::new(true, vec![bypass_busy(), rm_box(usr)]);
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
        let mut m = Mock::new(true, vec![first, rm_box(SCRATCH_RM)]);
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

/// An idle bypass session as Claude Code draws it: a reply, the composer,
/// and the footer that names the mode.
fn bypass_idle() -> Vec<String> {
    let mut r = rows(&["⏺ Ready.", ""]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// THE FOOTER RACE (host_live's
/// `the_host_hands_over_what_it_cannot_prove_and_types_nothing`, red under a
/// loaded gate on 2026-09-26 and 2026-09-27 with `the rm circuit breaker
/// outside a bypass session`). The rm breaker's bypass proof is the mode an
/// EARLIER read's footer showed (a 2.1.280 box is drawn where the footer
/// was). An idle point read before that footer was on the screen — an agent
/// still starting (its first frame later than the loop's idle window), or a
/// frame caught between the composer and its footer row — was waited on by
/// the server's verdict alone, and the verdict does not move on the frame
/// that draws the footer (idle to idle): the loop slept through that frame,
/// read the box with no mode known, and handed a scratch removal to a
/// person. Now an idle point that shows no mode, with none read yet, is
/// waited on by its CONTENT ([`Session::agent_wake`]): the footer's frame is
/// read and the breaker is pressed. The script's verdict passes over every
/// frame whose phase is not waited for, as the server's does, so the old
/// wait loses the race here every time.
///
/// NEGATIVE CONTROL: a session whose footer was read at its first idle point
/// is waited on by the verdict (one `await agent`), its idle twin passed over
/// unread, and pressed the same.
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
        // The point with no footer was waited on by its content, and the
        // footer's frame by the verdict.
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

/// THE CONTENT WAIT IS THE START'S ALONE (the 2026-09-27 review of the
/// footer race above): the wait on the content holds only until the session
/// shows its mode OR a busy frame. A busy footer names every mode but the
/// default, so a session read busy with no mode named has no footer race
/// left: its idle points are reached by a verdict that moves (busy to idle).
/// Here a session that never names a mode — its idle footer row not drawn,
/// its busy one `esc to interrupt` alone — gets the verdict wait back at its
/// first busy read: the idle point after it is waited on by ONE `await
/// agent`, and a screen change that moves no verdict wakes nothing. Without
/// the bound it was waited on by its content for the session's whole life.
/// NEGATIVE CONTROL: the same idle point read before any busy frame — the
/// agent still starting — is waited on by its content.
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

/// A git box, in the loop, is judged where the session's Bash tool stands as
/// its Claude Code transcript last recorded it — the directory an earlier
/// `cd sub` left it in, which neither the box nor `meta cwd=` shows (the
/// 2026-09-26 review). The loop reads `~/.claude` (here a scratch one) for
/// the live session launched in the reported directory: this test process
/// stands in for it. NEGATIVE CONTROL: with no Claude directory to read —
/// what the loop did before — the same box is pressed, the nested
/// repository's fsmonitor unseen.
#[test]
fn a_git_box_is_judged_where_the_transcript_puts_the_bash_tool() {
    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    };
    let root = std::env::temp_dir().join(format!("aterm-loop-shell-cwd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch");
    let root = std::fs::canonicalize(&root).expect("canonical");
    let launch = root.join("w");
    let nested = launch.join("sub");
    std::fs::create_dir_all(&nested).expect("dirs");
    git(&launch, &["init", "-q", "."]);
    git(&nested, &["init", "-q", "."]);
    git(&nested, &["config", "core.fsmonitor", "./evil.sh"]);
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
    std::fs::write(
        claude
            .join("sessions")
            .join(format!("{}.json", std::process::id())),
        format!(
            r#"{{"pid":{},"sessionId":"t-1","cwd":"{}"}}"#,
            std::process::id(),
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
    let run = |claude_dir: Option<PathBuf>| {
        let mut m = Mock::new(true, vec![bash_one_row()]);
        m.cwd = Some(launch.display().to_string());
        let mut s = session(&mut m, None);
        let mut worker = WorkerEnv::hermetic(None);
        if let Some(dir) = claude_dir {
            worker = worker.with(&format!("CLAUDE_CONFIG_DIR={}", dir.display()));
        }
        s.set_approval_env(ApprovalEnv {
            worker: WorkerSource::Fixed(Ok(worker)),
            ..owner_env()
        });
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                approve: Approve::Safe,
                ..auto(30, None).policy
            },
            ..auto(30, None)
        };
        s.supervise(&opts).expect("supervise");
        m.presses().len()
    };
    assert_eq!(
        run(None),
        1,
        "the launch directory alone loads nothing that runs"
    );
    assert_eq!(
        run(Some(claude)),
        0,
        "the Bash tool's repository runs its fsmonitor on the read"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// What tells [`park_as_worker`] to park.
const WORKER_PARK: &str = "SUPERVISE_ENGINE_TEST_PARK";

/// The stand-in worker's body: idle unless asked to park.
#[test]
fn park_as_worker() {
    if std::env::var_os(WORKER_PARK).is_some() {
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// A stand-in worker: THIS test binary, parked, with exactly `env` (and the
/// word that parks it) — not `/bin/sleep`, whose environment macOS hides as
/// a platform binary's. Killed on drop.
struct StandIn(std::process::Child);

impl StandIn {
    fn spawn(env: &[(String, String)]) -> Self {
        let mut c = std::process::Command::new(std::env::current_exe().expect("exe"));
        c.args(["supervise::run::tests::engine::park_as_worker", "--exact"])
            .env_clear()
            .env(WORKER_PARK, "1")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Owned at once, so every path out of here reaps it.
        let worker = Self(c.spawn().expect("the stand-in worker"));
        // Until the exec, the kernel shows this process's environment.
        for _ in 0..500 {
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
    let dir = std::env::temp_dir().join(format!("aterm-loop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::canonicalize(&dir).expect("canonical")
}

/// `git <args>` in `dir`, hermetic, asserted ok.
fn loop_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}");
}

/// A worker's hermetic environment in the session `sid`: this process's
/// `PATH`, no system or global git config.
fn worker_vars(sid: &str) -> Vec<(String, String)> {
    vec![
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
        ("ATERM_PARENT_SESSION_ID".to_string(), sid.to_string()),
    ]
}

/// One `approve = "safe"` loop over a `git log` box in `cwd`, the session
/// `3`/`sid` on the roster with `worker` as its foreground process, the
/// worker read from the session ([`WorkerSource::Session`]): the presses and
/// the approval ledger's rows (an escalation's carries its reason).
fn judge_git_box_with(worker: &StandIn, sid: &str, cwd: &Path) -> (usize, Vec<String>) {
    let (ldir, ledger) = ledger_file(&format!("worker-{}", worker.pid()));
    let mut m = Mock::new(true, vec![bash_one_row()]);
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

/// THE WORKER'S OWN ENVIRONMENT (the landing's gap, 2026-09-27): a git box is
/// judged in the environment the command will run with — the worker's, read
/// from the session's foreground process (`who`'s `fgpgid=`, bound by its
/// `ATERM_PARENT_SESSION_ID`) — so the worker's own `GIT_CONFIG_*` is seen,
/// though this process has none. A process that names another session is
/// not taken for the worker. NEGATIVE CONTROL: the same box, the worker
/// without the variables, is pressed.
#[test]
fn a_git_box_is_judged_in_the_workers_own_environment() {
    let repo = loop_scratch("worker-env");
    loop_git(&repo, &["init", "-q", "."]);
    let sid = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 1);
    let clean = StandIn::spawn(&worker_vars(&sid));
    assert_eq!(
        judge_git_box_with(&clean, &sid, &repo).0,
        1,
        "a worker whose configuration runs nothing: pressed"
    );
    drop(clean);

    let mut vars = worker_vars(&sid);
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

    // Another session's process is no worker of this one.
    let stranger = StandIn::spawn(&worker_vars("s-99999999999999999999"));
    let (presses, rows) = judge_git_box_with(&stranger, &sid, &repo);
    assert_eq!(presses, 0, "{rows:?}");
    assert!(
        rows.iter().any(|r| r.contains("belongs to session")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// THE WORKER'S OWN CLAUDE DIRECTORY (the landing's gap, 2026-09-27): where
/// the Bash tool stands is read from the transcripts under the WORKER's
/// `CLAUDE_CONFIG_DIR` — a session started under another one (an identity
/// spawn) — not this process's `~/.claude`. NEGATIVE CONTROL: the same
/// worker without the variable (its `$HOME` holds no Claude directory) is
/// judged by its launch directory alone, and pressed.
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

    let mut plain = worker_vars(&sid);
    plain.push(("HOME".to_string(), home.display().to_string()));
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

/// Which `who` row is the session's, and the process group it names: by the
/// `s-…` id the loop addresses where it has one, else by the local id
/// `status` gave; refused when the group is unknown or another session's too,
/// and when the roster is not one.
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
    // The stable id wins over a local one: a `status` another instance
    // answered (local ids are per instance, and reused) cannot pick this
    // session's row for it.
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

/// THE ADDRESSED SESSION (review, 2026-09-27): a loop that addresses its
/// session by its `s-…` id binds the worker by that id — the row, and the
/// process's `ATERM_PARENT_SESSION_ID` — not by the local id `status`
/// answered, which another instance's session may hold. Here `status` names
/// local `3`, whose row is a STRANGER's; the addressed session is `2`'s. The
/// worker's configuration is seen (escalated); NEGATIVE CONTROL: the same loop
/// by local id alone takes the stranger's clean row and process and presses.
#[test]
fn a_loop_addressing_its_session_binds_the_worker_by_that_id() {
    let repo = loop_scratch("worker-stable-id");
    loop_git(&repo, &["init", "-q", "."]);
    let ours = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 3);
    let theirs = format!("s-{:020x}", u64::from(std::process::id()) << 8 | 4);
    let mut vars = worker_vars(&ours);
    for (k, v) in [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.fsmonitor"),
        ("GIT_CONFIG_VALUE_0", "./evil.sh"),
    ] {
        vars.push((k.to_string(), v.to_string()));
    }
    let worker = StandIn::spawn(&vars);
    let stranger = StandIn::spawn(&worker_vars(&theirs));
    let run = |selector: Option<String>| {
        let (ldir, ledger) = ledger_file(&format!("stable-{}", worker.pid()));
        let mut m = Mock::new(true, vec![bash_one_row()]);
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

/// Under `answer_questions = false` (the owner's limit) a QUESTION is
/// escalated like a box — badged `claude question: <what was asked> (<why>;
/// answer_questions is off)` and asked of the manager once while
/// the fabric is connected — and the badge goes when the question does.
/// With `fabric=absent` nothing is posted (an `ask` would be refused
/// `no-bridge`, a queued post is no delivery), and the journal says why:
/// `mail=skipped: no fabric (fabric=absent)`.
#[test]
fn a_question_escalates_and_mail_goes_only_over_a_connected_fabric() {
    for fabric in ["connected", "absent"] {
        let (dir, journal) = journal_file(&format!("question-{fabric}"));
        let mut m = Mock::new(true, vec![question_screen(), busy_screen(), idle_screen()]);
        m.fabric = fabric;
        m.vanish_after = Some(0);
        m.verb_replies
            .insert("post", VecDeque::from([ok("OK 7 off=91\n")]));
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_manager(Some("@s-9".to_string()));
        });
        assert!(lines[0].starts_with("EVENT question "), "{lines:?}");
        let text = "claude question: Did the suite pass on your machine? (the worker asked a \
                    question; answer_questions is off)";
        assert_eq!(
            count(&m, &format!("meta set attention owner=supervisor {text}")),
            1,
            "{:?}",
            m.requests
        );
        assert_eq!(
            count(&m, "meta unset attention owner=supervisor"),
            1,
            "{:?}",
            m.requests
        );
        // `--wait=0`: an ask never holds the single-threaded loop (main's
        // fed20fadf, merged into lane B's escalation).
        let posts = count(&m, &format!("post to=@s-9 kind=ask --wait=0 {text}"));
        let (records, _) = journal_records(&journal);
        let escalated = records
            .iter()
            .find(|r| r.kind == "escalated")
            .expect("ESCALATED");
        if fabric == "connected" {
            assert_eq!(posts, 1, "{:?}", m.requests);
            assert_eq!(escalated.summary, "attention=OK mail=OK 7 off=91");
        } else {
            assert_eq!(count(&m, "post"), 0, "{:?}", m.requests);
            assert_eq!(
                escalated.summary,
                "attention=OK mail=skipped: no fabric (fabric=absent)"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The approval ledger: one row per decision — an approval under its rule,
/// an escalation with the reason — each with the whole command's SHA-256.
#[test]
fn every_decision_is_a_ledger_row() {
    let (ldir, ledger) = ledger_file("rows");
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), write_prompt()]);
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
    assert!(
        rows[1].contains("\"reason\":\"not read-only (space reading): rm\""),
        "{rows:?}"
    );
    assert!(
        rows.iter().all(|r| r.contains("\"cmd_sha256\":\"")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);

    // Under `approve = "none"` nothing is decided by a rule or pressed, and
    // the escalation is still a row.
    let (ldir, ledger) = ledger_file("off");
    let mut m = Mock::new(true, vec![bash_one_row()]);
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

/// The in-GUI host's entry: `SuperviseOpts::hosted()` is the unbounded
/// watch with the policy on, and `run_hosted` ends — `Ok`, `EXIT stopped` —
/// within one wait of its stop flag being set, clearing the badge it raised.
/// The stop is the host's, set from another thread once the loop has
/// reported the point, raised its badge and is watching past it
/// ([`host_stops`]) — not 100 ms in, which a loaded machine outran before
/// the loop's first look (the run then ended, correctly, with nothing
/// raised: 11 in 40, measured).
#[test]
fn run_hosted_runs_until_it_is_stopped_and_clears_its_badge() {
    let hosted = SuperviseOpts::hosted();
    assert_eq!(hosted.policy, SupervisorConfig::default(), "full power");
    assert_eq!(hosted.max, UNBOUNDED);
    assert!(hosted.mail.is_none());

    let mut m = Mock::new(true, vec![question_screen()]);
    let (ldir, ledger) = ledger_file("hosted");
    // The owner's limit: a question is escalated, so a badge is raised and
    // cleared (under full power it would be answered).
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

    // A turn that never ends (the footer never leaves): the stop is seen in
    // the wait for it, not only at a look — every wait the loop began was
    // for the turn (`await gone` on the busy footer), the host stopped it
    // there, and it ended within one wait of that.
    let mut m = Mock::new(true, vec![busy_screen()]);
    let (ldir, ledger) = ledger_file("hosted-busy");
    let (r, lines) = host_stops(&mut m, None, ledger, &hosted, false);
    assert_eq!(r, Ok(()));
    assert_eq!(lines, ["EXIT stopped"]);
    let waits: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("await"))
        .collect();
    assert!(waits.len() > WATCHING, "{:?}", m.requests);
    assert!(
        waits
            .iter()
            .all(|w| w.starts_with("await gone esc.to.interrupt ")),
        "{:?}",
        m.requests
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// A STOP THAT LANDS BETWEEN THE LOOK AND THE WRITE writes nothing: the
/// host's stop flag is set while the loop DECIDES the box it has read (its
/// `status` request for the session's program, after the look's own stop
/// check has passed), and neither the guarded `1` nor any `send`/`turn`/
/// `key` reaches the transport — `Session::call` refuses it before it,
/// whatever the transport would have done (this one accepts everything).
/// The run still ends as a stop, not a failure. NEGATIVE CONTROL: the same
/// box with no stop is pressed through the same wrapper (and with the
/// fence in `call` removed, the armed run presses: measured).
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
            // The stop lands while the loop decides the box it read: its
            // first `status` after a screen read.
            self.read |= args.contains(&"text");
            if self.arm && self.read && args.contains(&"status") {
                self.stop.store(true, Ordering::SeqCst);
            }
            r
        }
    }
    let hosted = SuperviseOpts::hosted();
    for arm in [true, false] {
        let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row(), idle_screen()]);
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

/// A build's output on a screen without Claude Code's composer frame: a
/// program still writing, which the turn's wait reads as busy until the
/// output holds still.
fn writing_screen() -> Vec<String> {
    rows(&[
        "$ targo --unverified build -p aterm-agent",
        "   Compiling aterm-core v0.92.0",
        "   Compiling aterm-agent v0.92.0",
    ])
}

/// A STOP IN ANY WAIT ends the hosted run before another wait begins (the
/// review of 2026-09-24): `run_hosted` ends "within one wait" of its stop,
/// and here the stop is set as each wait of a path goes out in turn — past
/// every check the loop makes before it ([`stop_in_every_wait`]). After it,
/// no wait and no input reaches the transport, and the run is `EXIT
/// stopped`. The paths: a question escalated and watched past; a turn that
/// never ends, on a host with `await gone` and on one without (the `gone`
/// refused, then the idle wait); a screen without the composer frame whose
/// output never holds still (the turn's idle wait, then its wait for the
/// next change — the review's probe); a read box approved (the press, its
/// settle). Each path names the waits it exists for, so a script that stops
/// reaching them fails here.
///
/// NEGATIVE CONTROL, measured: with `Session::call`'s refusal of a wait
/// after the stop removed, and the turn's own checks before its later
/// waits, the host without `await gone` fails at its first wait — the stop
/// set in `await gone`, then `await idle 2000 timeout 20000` and `await seq
/// 101 timeout 20000` both went out, 40 s of waiting after the stop — and,
/// run past that, the writing screen's idle wait was followed by `await seq
/// … timeout 20000` at every round. The question, the busy turn and the
/// approved box were within one wait before the fix too.
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
        let mut m = Mock::new(true, vec![question_screen()]);
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await seq ");

    let armed = stop_in_every_wait("busy", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(true, vec![busy_screen()]);
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await gone esc.to.interrupt ");

    let armed = stop_in_every_wait("busy-no-gone", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(false, vec![busy_screen()]);
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await gone ");
    named(&armed, "@s-1 await idle 2000 ");

    let armed = stop_in_every_wait("writing", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(true, vec![writing_screen()]);
        m.idle_never = true;
        m.vanish_after = Some(2);
        m
    });
    named(&armed, "@s-1 await idle 2000 timeout 3000");
    named(&armed, "@s-1 await seq ");

    let armed = stop_in_every_wait("approved", &hosted, Some("@s-1"), || {
        let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row(), idle_screen()]);
        m.vanish_after = Some(0);
        m
    });
    named(&armed, "@s-1 await idle 500 ");
}

/// A stop in one of a TURN's waits ends the turn, spent — the look it
/// returns to reads the stop — before the turn's next wait: `await_turn_from`
/// checks the stop before every wait it begins, the one after a read (its
/// wait for the next change) and the one after a refused `await gone` (its
/// idle wait) as well as the one at the top of each round. Those are the two
/// places a turn's wait followed another with no check between them (the
/// review of 2026-09-24). NEGATIVE CONTROLS, measured: without either check,
/// its case ends the turn as the error `Session::call` refuses the next wait
/// with ("no wait begins after the stop"); without that refusal too, the
/// writing screen's `await seq 102 timeout 20000` goes out after the stop.
#[test]
fn a_stop_in_a_turns_wait_ends_the_turn_spent_before_its_next_wait() {
    for (tag, modern, screen, arm_at, wait) in [
        (
            "writing",
            true,
            writing_screen(),
            2,
            "await idle 2000 timeout 3000",
        ),
        ("no gone", false, busy_screen(), 1, "await gone "),
    ] {
        let mut m = Mock::new(modern, vec![screen]);
        m.idle_never = true;
        let stop = Arc::new(AtomicBool::new(false));
        let mut side = StopInWait {
            inner: &mut m,
            stop: Arc::clone(&stop),
            arm_at,
            waits: 0,
            armed_in: None,
            after: Vec::new(),
        };
        let mut s = session(&mut side, None);
        s.stop = Some(stop);
        let mut moved = false;
        let turn = match s.await_turn_from(Duration::from_secs(3600), true, &mut moved, &mut Alone)
        {
            Ok(turn) => turn,
            Err(e) => panic!("{tag}: the turn ended as {:?}", String::from(e)),
        };
        assert!(turn.timed_out, "{tag}: the spent turn");
        assert!(
            side.armed_in
                .as_deref()
                .is_some_and(|w| w.starts_with(wait)),
            "{tag}: {:?} {:#?}",
            side.armed_in,
            side.inner.requests
        );
        assert!(
            !side.after.iter().any(|r| r.starts_with("await")),
            "{tag}: {:#?}",
            side.after
        );
    }
}

/// The survey's `0` under a hold: the `0` refused `ERR halted` parks the
/// loop like the approval press, and the survey is tried again after.
#[test]
fn a_halted_survey_dismissal_parks_and_tries_again() {
    let mut m = Mock::new(
        true,
        vec![
            with_survey(idle_screen()),
            with_survey(idle_screen()),
            idle_screen(),
        ],
    );
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

/// The measured 2.1.280 folder-trust dialog (`policy/fixtures/cap-trust.txt`,
/// folder `/private/tmp/claude-502/scratch/work1`), the focus on `No, exit`
/// — or, `focused_yes`, moved onto `Yes, I trust this folder`.
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

/// A host with the generation fence (`key if-gen=`, `text --json "gen"`).
fn fenced(screens: Vec<Vec<String>>) -> Mock {
    let mut m = Mock::new(true, screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m
}

/// THE TRUST DIALOG in the loop (owner decision 1): for the session's own
/// folder (`meta cwd=`) under a trust root, the focus is moved to `Yes, I
/// trust this folder` with a fenced `down` guarded on the folder's row, a
/// fresh read confirms the focus landed, and Enter goes fenced on THAT
/// read's generation, guarded on the focused row itself — never Esc, which
/// exits Claude Code.
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
            format!("key if-gen=1.101 if={folder} down").as_str(),
            format!("key if-gen=1.102 if={yes} enter").as_str(),
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

/// The live re-run of 2026-09-24: Claude Code 2.1.281 drew the trust dialog
/// and the loop's `down` came 0.3 s later — before the dialog took input —
/// and was dropped; the focus still read `No, exit`, and the dialog went to
/// the owner ("the focus did not land"). A key the SAME dialog did not take
/// is moved again from a fresh read, and the second lands: approved.
#[test]
fn a_focus_move_the_dialog_dropped_is_made_again_from_a_fresh_read() {
    let mut m = fenced(vec![
        trust_dialog(false),
        trust_dialog(false),
        trust_dialog(false),
        trust_dialog(true),
        idle_screen(),
    ]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(
        lines.contains(&format!("APPROVED seq=104 {TRUST_FOLDER}")),
        "{lines:?}\n{:?}",
        m.requests
    );
    let presses: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("key "))
        .collect();
    assert_eq!(presses.len(), 3, "{presses:?}");
    assert!(presses[0].ends_with(" down") && presses[1].ends_with(" down"));
    assert!(presses[2].ends_with(" enter"), "{presses:?}");
    assert_eq!(m.attention, None);
}

/// THE HOST'S `[harness] trust_roots` reach the decision under the safe
/// rules (`approve = "safe"`): the options the in-GUI host builds
/// (`SuperviseOpts::hosted_with`, the whole table as `policy`) put the
/// configured roots into the approval context
/// (`ApprovalCtx::set_trust_roots`), so a root the owner wrote covers the
/// folder and one that does not leaves the dialog to the human — the same
/// dialog, the same session folder, only the roots differ. (Until the engine
/// lane's merge the host masked this key: the engine ran on its built-in
/// roots whatever the file said.) Under the default, full power, the roots
/// are not consulted: [`full_power_trusts_the_folder_whatever_the_roots`].
#[test]
fn the_hosts_trust_roots_decide_the_trust_dialog() {
    for (roots, approved) in [
        ("/private/tmp/claude-502/scratch/work*", true),
        ("/nowhere/at/all*", false),
    ] {
        // The safe rule's roots; full power trusts any folder (below).
        let mut cfg = crate::supervise::SupervisorConfig::default();
        cfg.set("approve", "safe").expect("a level");
        cfg.set("trust_roots", roots).expect("a root list");
        let opts = SuperviseOpts {
            max: Duration::from_secs(30),
            ..SuperviseOpts::hosted_with(&cfg)
        };
        let mut m = fenced(vec![trust_dialog(false), trust_dialog(true), idle_screen()]);
        m.cwd = Some(TRUST_FOLDER.to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
        });
        if approved {
            assert_eq!(
                lines[0],
                format!("APPROVED seq=102 {TRUST_FOLDER}"),
                "{roots}: {lines:?}"
            );
            assert!(m.presses().iter().any(|p| p.ends_with(" enter")));
        } else {
            assert!(m.presses().is_empty(), "{roots}: {:?}", m.requests);
            assert!(
                m.requests.iter().any(|r| r.contains("under no trust root")),
                "{roots}: {:?}",
                m.requests
            );
        }
    }
}

/// A SHELL WITH NO OSC 7 REPORTS NO FOLDER, BUT THE AGENT HAS ONE (the
/// harness audit, 2026-09-25: under `approve = "safe"` a trust dialog in a
/// `/bin/sh` tab escalated even inside `trust_roots`, `meta cwd=-`). The
/// server now reports the foreground program's own directory (`meta
/// agent_cwd=`), the folder Claude Code's trust dialog asks about, and the
/// safe rule reads it where the shell reported none. NEGATIVE CONTROL: with
/// neither, the dialog is still escalated as the cwd unknown.
#[test]
fn a_trust_dialog_under_a_shell_with_no_osc7_is_decided_by_the_agents_own_folder() {
    let mut cfg = crate::supervise::SupervisorConfig::default();
    cfg.set("approve", "safe").expect("a level");
    cfg.set("trust_roots", "/private/tmp/claude-502/scratch/work*")
        .expect("a root list");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&cfg)
    };
    for (agent_cwd, approved) in [(Some(TRUST_FOLDER), true), (None, false)] {
        let mut m = fenced(vec![trust_dialog(false), trust_dialog(true), idle_screen()]);
        m.cwd = None;
        m.agent_cwd = agent_cwd.map(str::to_string);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
        });
        if approved {
            assert_eq!(
                lines[0],
                format!("APPROVED seq=102 {TRUST_FOLDER}"),
                "{lines:?}"
            );
        } else {
            assert!(m.presses().is_empty(), "{:?}", m.requests);
            assert!(
                m.requests.iter().any(|r| r.contains("cwd unknown")),
                "{:?}",
                m.requests
            );
        }
    }
}

/// The owner's default (2026-09-24) on the same dialog: the folder is
/// trusted with the roots naming nowhere near it — the session's own folder
/// or another — the confirmed focus move then Enter, ledgered
/// `trust-any@v1` with the safe rule's reason as `unproven:`. Negative
/// control: `approve = "safe"`, nothing typed
/// ([`the_hosts_trust_roots_decide_the_trust_dialog`]).
#[test]
fn full_power_trusts_the_folder_whatever_the_roots() {
    let mut cfg = crate::supervise::SupervisorConfig::default();
    cfg.set("trust_roots", "/nowhere/at/all*")
        .expect("a root list");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&cfg)
    };
    for (cwd, why) in [
        (TRUST_FOLDER, "under no trust root"),
        (
            "/private/tmp/claude-502/scratch/work2",
            "not the session's cwd",
        ),
    ] {
        let (ldir, ledger) = ledger_file("trust-all");
        let mut m = fenced(vec![trust_dialog(false), trust_dialog(true), idle_screen()]);
        m.cwd = Some(cwd.to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_approval_ledger(Some(ledger.clone()));
        });
        assert_eq!(
            lines[0],
            format!("APPROVED seq=102 {TRUST_FOLDER}"),
            "{cwd}: {lines:?}"
        );
        assert!(m.presses().iter().any(|p| p.ends_with(" enter")), "{cwd}");
        let rows = ledger_rows(&ledger);
        assert!(
            rows.iter()
                .any(|r| r.contains("\"rule_id\":\"trust-any@v1\"")
                    && r.contains("\"decision\":\"approved\"")
                    && r.contains("unproven: a folder-trust dialog for")
                    && r.contains(why)),
            "{cwd}: {rows:?}"
        );
        let _ = std::fs::remove_dir_all(&ldir);
    }
}

/// THE OWNER'S DEFAULT in the host's loop (`SuperviseOpts::hosted_with` of
/// `SupervisorConfig::default()`): a `touch x` box — a write decision 1
/// hands over — is pressed `1` under its command row's guard; the ledger row
/// is `allow-once@v1` with `unproven: not read-only …` as its reason, the
/// `--notes` line says the same, and the journal carries the journal-only
/// `UNPROVEN` line after the `APPROVED` one. Negative control: the same box
/// under `approve = "safe"` is escalated, nothing pressed.
#[test]
fn the_hosts_default_presses_a_write_and_ledgers_it_unproven() {
    let touch = {
        let mut r = bash_one_row();
        r[4] = "   touch x".to_string();
        r
    };
    let (ldir, ledger) = ledger_file("full-power");
    let (jdir, journal) = journal_file("full-power");
    let notes = ldir.join("notes.txt");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        journal: Some(journal.clone()),
        notes: Some(notes.clone()),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let mut m = Mock::new(true, vec![touch.clone(), busy_screen()]);
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
        [format!("key if={guard} 1").as_str()],
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
    let noted = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(
        noted.contains("approved (allow-once@v1; unproven: not read-only"),
        "{noted}"
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

    let mut cfg = crate::supervise::SupervisorConfig::default();
    cfg.set("approve", "safe").expect("the limit");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&cfg)
    };
    let mut m = Mock::new(true, vec![touch]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
}

/// A QUESTION BOX in a shape 2.1.282 does not draw (the hand-built
/// column-1 `Yes`/`No`) under the owner's default: detected as a box, read
/// as a question aterm-phase did not read whole, escalated — badged `claude
/// question: …` with that reason — ledgered `-` escalated, and nothing
/// typed; and under the owner's limit `answer_questions = false` handed over
/// as that limit's. A live-geometry question is answered
/// ([`a_live_question_box_is_answered_under_the_default`]).
#[test]
fn a_column_one_question_box_is_escalated_and_a_limit_hands_it_over() {
    use aterm_phase::prompt::fixtures as f;
    let (ldir, ledger) = ledger_file("question-box");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let limited = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig {
            answer_questions: false,
            ..crate::supervise::SupervisorConfig::default()
        })
    };
    for (opts, why) in [
        (opts, "did not read whole"),
        (limited, "answer_questions is off"),
    ] {
        let mut m = Mock::new(true, vec![f::screen(f::QUESTION_YES_NO)]);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(owner_env());
            s.set_approval_ledger(Some(ledger.clone()));
        });
        assert!(lines[0].starts_with("EVENT prompt "), "{why}: {lines:?}");
        assert!(m.presses().is_empty(), "{why}: {:?}", m.requests);
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("send ") || r.starts_with("turn ")),
            "{why}: {:?}",
            m.requests
        );
        let set = m
            .requests
            .iter()
            .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
            .expect("escalated");
        assert!(
            set.starts_with("claude question:") && set.contains(why),
            "{set}"
        );
    }
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter()
            .any(|r| r.contains("\"rule_id\":\"-\"") && r.contains("\"decision\":\"escalated\"")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// A BOX THE PANE SHOWS WHOLE IS READ WHOLE (the validate drive of
/// 2026-09-24, a private headless aterm at 130x49): the loop reads the last
/// 40 rows, and a 43-row Bash box whose title sits on row 3 of a 49-row grid
/// came back with its head cut — read as "taller than the pane" and
/// escalated where the owner's default presses it. A `tail=` read that cuts
/// a box's head is taken again whole: the box is approved at full power,
/// a live-geometry question drawn at the top of the grid is answered with
/// one fenced Enter, and the column-1 Yes/No question drawn at the top of a
/// 49-row grid is escalated as the question aterm-phase did not read whole.
/// The control: a box truly
/// taller than the grid (its title on no row of the full read either) is
/// read with its head off the screen — and at full power answered by its
/// options (the E2E probe of 2026-09-25: escalated, it waited 3 h), the
/// missing title said in its `unproven` (the policy's tests pin it).
#[test]
fn a_box_cut_by_the_tail_is_read_again_whole() {
    use aterm_phase::prompt::fixtures as f;
    let hosted = || SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let bottom = |mut r: Vec<String>, rows: usize| {
        assert!(r.len() <= rows);
        let mut grid = vec![String::new(); rows - r.len()];
        grid.append(&mut r);
        grid
    };
    // Whole on a 49-row grid, its title more than 40 rows above the footer.
    let tall = bottom(f::tall_bash_box(33), 49);
    assert!(
        tall.iter()
            .position(|r| r.starts_with(" Bash command"))
            .is_some_and(|t| t < 49 - 40),
        "{tall:?}"
    );
    let mut m = Mock::new(true, vec![tall]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let reads: Vec<&str> = m
        .requests
        .iter()
        .map(String::as_str)
        .filter(|r| r.starts_with("text"))
        .take(2)
        .collect();
    assert_eq!(
        reads,
        ["text --json tail=40", "text --json"],
        "{:?}",
        m.requests
    );

    // A live-geometry question at the top of a 56-row grid: the last 40
    // rows start at its first option, so the tail read cuts its head; read
    // again whole, it is answered.
    let mut live = f::screen(f::QUESTION_ONE);
    live.resize(56, String::new());
    let mut m = questioned(vec![live]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(m.presses()[0].ends_with(" enter"), "{:?}", m.presses());
    let reads: Vec<&str> = m
        .requests
        .iter()
        .map(String::as_str)
        .filter(|r| r.starts_with("text"))
        .take(2)
        .collect();
    assert_eq!(
        reads,
        ["text --json tail=40", "text --json"],
        "{:?}",
        m.requests
    );

    let mut question = f::screen(f::QUESTION_YES_NO);
    question.resize(49, String::new());
    let mut m = Mock::new(true, vec![question]);
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
        set.starts_with("claude question:") && set.contains("did not read whole"),
        "{set}"
    );

    // The control: taller than the grid, the head off every read.
    let heredoc = f::tall_bash_box(62);
    let cut = heredoc[heredoc.len() - 49..].to_vec();
    let mut m = Mock::new(true, vec![cut]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
}

/// THE E2E PROBE OF 2026-09-25 (X1): Codex 0.157's folder-trust gate drawn
/// at the top of a 45-row pane — its title on row 1 — read from the loop's
/// 40-row tail had its title cut, and the cut was asked of Claude Code's
/// grammar alone, which sees no Codex box: the gate was titled by a body row
/// and escalated as a dialog of no kind (2 min 15 s, until a resize). The
/// tail read is now judged by the session's program's reader, taken again
/// whole, and the gate is answered (its cursor already on `Trust and
/// continue`: Enter, fenced). NEGATIVE CONTROL: the whole gate in a 40-row
/// pane is read once, from the tail.
#[test]
fn a_codex_box_cut_by_the_tail_is_read_again_whole() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let hosted = || SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let tall = screen(cx::TRUST_TALL_PANE);
    assert_eq!(tall.len(), 45);
    for (rows, reads) in [
        (tall.clone(), vec!["text --json tail=40", "text --json"]),
        (
            tall[5..].to_vec(),
            vec!["text --json tail=40", "text --json tail=40"],
        ),
    ] {
        let mut m = fenced(vec![rows]);
        m.program = "codex";
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
            s.set_approval_env(owner_env());
        });
        let got: Vec<&str> = m
            .requests
            .iter()
            .map(String::as_str)
            .filter(|r| r.starts_with("text"))
            .take(2)
            .collect();
        if reads[1] == "text --json" {
            assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
            assert!(
                m.presses().iter().any(|p| p.ends_with(" enter")),
                "{:?}",
                m.requests
            );
            assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
            assert_eq!(got, reads, "{:?}", m.requests);
        } else {
            // Cut in the pane itself: nothing more to read, never re-read
            // whole — and nothing pressed on a body row's title.
            assert!(!got.contains(&"text --json"), "{:?}", m.requests);
            assert!(m.presses().is_empty(), "{:?}", m.requests);
        }
    }
}

/// The live session of 2026-09-26 (Claude Code 2.1.283 in a private
/// headless aterm at 149x62, a fresh pane) as the loop reads it: the host's
/// default options, and the rows a 40-row tail of the pane holds from.
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

/// The live subagent box's own rows (the rule to `Esc to cancel`, rows
/// 22-40 of its capture) drawn at the TOP of a fresh 62-row pane under one
/// blank row — HAND-BUILT from the capture: a box that appears before any
/// transcript, wholly above a 40-row tail, the hidden cursor on its focused
/// `❯ 1. Yes` (row 16).
fn rm_box_above_the_tail() -> Vec<String> {
    use aterm_phase::prompt::fixtures as f;
    let live = f::screen(f::BOX_RM_SUBAGENT_FRESH_PANE);
    let mut rows = vec![String::new()];
    rows.extend_from_slice(&live[22..=40]);
    assert!(rows.len() < FRESH_PANE_ROWS - 40, "wholly above the tail");
    assert_eq!(rows[16], " ❯ 1. Yes");
    rows.resize(FRESH_PANE_ROWS, String::new());
    rows
}

/// The first `n` `text` requests the loop made.
fn text_reads(m: &Mock, n: usize) -> Vec<&str> {
    m.requests
        .iter()
        .map(String::as_str)
        .filter(|r| r.starts_with("text"))
        .take(n)
        .collect()
}

/// THE FRESH PANE (the live run of 2026-09-26: Claude Code 2.1.283 in a
/// private headless aterm built from main, 149x62, a fresh pane whose shell
/// prompt sat at the top). The folder-trust dialog was drawn on rows 5-20,
/// wholly above the loop's 40-row tail: the server answered `text --json
/// tail=40` with 40 blank rows from row 22 and the hidden cursor on row 17
/// above them (`TRUST_FRESH_PANE_TAIL40`, served here byte for byte), the
/// loop journaled `EVENT idle seq=248`, and nothing was pressed for 3+
/// minutes. A tail that holds none of the live rows — the cursor above it,
/// or every row blank — is now taken again whole, and the dialog is
/// answered under the host's default as every trust dialog is: the focus
/// moved onto `Yes` (fenced, guarded on the folder's row) from the whole
/// read, then Enter from a fresh read — itself a blank tail, read again
/// whole. NEGATIVE CONTROL: the subagent's rm-breaker box of the same
/// session sat INSIDE the tail, the cursor on its focused row: it is read
/// once, from the tail — no whole read anywhere — and pressed.
#[test]
fn a_dialog_above_the_tail_of_a_fresh_pane_is_read_whole_and_answered() {
    use aterm_phase::prompt::fixtures as f;
    let no = f::screen(f::TRUST_FRESH_PANE);
    assert_eq!(no.len(), FRESH_PANE_ROWS);
    let yes: Vec<String> = no
        .iter()
        .map(|r| match r.as_str() {
            " ❯ No, exit" => "   No, exit".to_string(),
            "   Yes, I trust this folder" => " ❯ Yes, I trust this folder".to_string(),
            _ => r.clone(),
        })
        .collect();
    assert_ne!(no, yes);
    let mut m = fenced(vec![no, yes.clone(), yes, idle_screen()]);
    m.tail_replies
        .push_back(f::TRUST_FRESH_PANE_TAIL40.to_string());
    m.cursor_row = Some(17);
    m.cwd = Some(FRESH_PANE_FOLDER.to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
        s.set_approval_env(owner_env());
    });
    assert_eq!(
        lines[0],
        format!("APPROVED seq=103 {FRESH_PANE_FOLDER}"),
        "{lines:?}\n{:?}",
        m.requests
    );
    let folder = crate::supervise::policy::row_guard(&format!(" {FRESH_PANE_FOLDER}"));
    let yes = crate::supervise::policy::row_guard(" ❯ Yes, I trust this folder");
    assert_eq!(
        m.presses(),
        [
            format!("key if-gen=1.101 if={folder} down").as_str(),
            format!("key if-gen=1.103 if={yes} enter").as_str(),
        ],
        "{:?}",
        m.requests
    );
    assert_eq!(
        text_reads(&m, 4),
        [
            "text --json tail=40",
            "text --json",
            "text --json tail=40",
            "text --json",
        ],
        "{:?}",
        m.requests
    );
    assert!(m.tail_replies.is_empty(), "the measured reply was served");
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);

    // The control: the box in the tail, the cursor on it — read once.
    let live = f::screen(f::BOX_RM_SUBAGENT_FRESH_PANE);
    assert_eq!(live.len(), FRESH_PANE_ROWS);
    let mut m = fenced(vec![bypass_busy(), live, busy_screen()]);
    m.cursor_row = Some(37);
    m.cwd = Some(FRESH_PANE_FOLDER.to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    let press = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .expect("the press");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(m.presses()[0].ends_with(" 1"), "{:?}", m.presses());
    assert_eq!(
        m.requests[..press]
            .iter()
            .filter(|r| r.starts_with("text"))
            .collect::<Vec<_>>(),
        ["text --json tail=40", "text --json tail=40"],
        "{:?}",
        m.requests
    );
    assert_eq!(
        count(&m, "text --json tail"),
        count(&m, "text"),
        "{:?}",
        m.requests
    );
}

/// The owner's case in the fresh pane: a subagent's Bash box carrying the
/// rm circuit breaker's note, drawn wholly above the 40-row tail of a fresh
/// 62-row pane (the live box's rows at the top: [`rm_box_above_the_tail`]),
/// is read whole — its tail blank, the cursor above it — and pressed `1`
/// once by the host's default, no badge. Before the fix the tail read it
/// as a blank, idle screen and nothing was pressed.
#[test]
fn an_rm_breaker_box_above_the_tail_of_a_fresh_pane_is_pressed() {
    let boxed = rm_box_above_the_tail();
    let first_row = boxed
        .iter()
        .find(|r| r.starts_with("   │ cd "))
        .expect("the first command row")
        .clone();
    let mut m = fenced(vec![bypass_busy(), boxed.clone(), boxed, busy_screen()]);
    m.cwd = Some(FRESH_PANE_FOLDER.to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    let guard = crate::supervise::policy::row_guard(&first_row);
    let presses = m.presses();
    assert_eq!(presses.len(), 1, "{:?}", m.requests);
    assert!(
        presses[0].starts_with("key if-gen=1.") && presses[0].ends_with(&format!(" if={guard} 1")),
        "{presses:?}"
    );
    assert_eq!(
        text_reads(&m, 3),
        ["text --json tail=40", "text --json tail=40", "text --json"],
        "{:?}",
        m.requests
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
}

/// WHAT THE LOOP WAITS ON (fix 2's reach, 2026-09-26): at an idle point a
/// host that pushes its agent verdict is waited on with ONE `await agent
/// <every word but idle>` — no screen read until the SERVER's verdict moves
/// ([`Session::agent_wake`]). A fresh pane idle at its top, then a box drawn
/// wholly above the last 40 rows: a server whose verdict read only the last
/// 40 rows (blank, so `idle`) never moved it, and the loop slept through
/// the box — nothing read, nothing pressed. With the verdict reaching the
/// box, the wait wakes, the loop reads (its tail blank, then whole) and
/// presses.
#[test]
fn a_box_drawn_above_the_tail_after_the_idle_point_wakes_the_loop_only_through_the_verdict() {
    let mut idle = idle_screen();
    idle.resize(FRESH_PANE_ROWS, String::new());
    let boxed = rm_box_above_the_tail();
    for zone in [None, Some(40)] {
        let mut m = fenced(vec![
            idle.clone(),
            idle.clone(),
            boxed.clone(),
            boxed.clone(),
        ]);
        m.agent_pushes = true;
        m.agent_zone = zone;
        m.cwd = Some(FRESH_PANE_FOLDER.to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &fresh_pane_opts(), |s| {
            s.set_approval_env(owner_env());
        });
        let wait = m
            .requests
            .iter()
            .position(|r| r.starts_with("await agent busy,prompt,question,survey"))
            .unwrap_or_else(|| panic!("{zone:?}: the idle point's wait: {:?}", m.requests));
        assert_eq!(
            text_reads(&m, 2),
            ["text --json tail=40", "text --json"],
            "{zone:?}: {:?}",
            m.requests
        );
        let after: Vec<&String> = m.requests[wait + 1..]
            .iter()
            .filter(|r| r.starts_with("text"))
            .collect();
        if zone.is_none() {
            assert!(
                lines.iter().any(|l| l.starts_with("APPROVED seq=")),
                "{lines:?}"
            );
            assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
            assert_eq!(
                after[..2],
                ["text --json tail=40", "text --json"],
                "{:?}",
                m.requests
            );
        } else {
            // The blind verdict: asleep until the session ended.
            assert!(m.presses().is_empty(), "{:?}", m.requests);
            assert!(after.is_empty(), "no read after the wait: {:?}", m.requests);
            assert!(
                !lines.iter().any(|l| l.starts_with("APPROVED")),
                "{lines:?}"
            );
        }
    }
}

/// The harness final review (2026-09-24, major): the same for a FOOTERLESS
/// box. A network box taller than the 40-row tail, whole on a 49-row grid,
/// read from its last 40 rows has its title and rule off them: before the
/// fix that read was authoritative idle and nothing re-read it, so the box
/// full power presses sat unanswered with no escalation. Now the tail read
/// is taken again whole and the box is pressed; a held message the same
/// height is re-read whole and handled as its own kind — its delivery a
/// focus move this unfenced host cannot make, so it is handed over as a
/// held message, never as a cut box;
/// and a short pane that shows no more than the network box's title row
/// (its rule cut, `first == 0`) escalates it as a box whose title is off
/// the screen from the one read.
#[test]
fn a_footerless_box_cut_by_the_tail_is_read_again_whole() {
    use aterm_phase::prompt::fixtures as f;
    let hosted = || SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let bottom = |mut r: Vec<String>, rows: usize| {
        assert!(r.len() <= rows);
        let mut grid = vec![String::new(); rows - r.len()];
        grid.append(&mut r);
        grid
    };
    let tall = bottom(f::tall_footerless(f::BOX_NETWORK, 33), 49);
    assert!(
        tall.iter()
            .position(|r| r.starts_with(" Network request"))
            .is_some_and(|t| t < 49 - 40),
        "{tall:?}"
    );
    let mut m = Mock::new(true, vec![tall]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let reads: Vec<&str> = m
        .requests
        .iter()
        .map(String::as_str)
        .filter(|r| r.starts_with("text"))
        .take(2)
        .collect();
    assert_eq!(
        reads,
        ["text --json tail=40", "text --json"],
        "{:?}",
        m.requests
    );

    let held = bottom(f::tall_footerless(f::HELD_MESSAGE, 33), 49);
    let mut m = Mock::new(true, vec![held]);
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
    assert!(
        set.contains("held") && !set.contains("title row is not on the screen"),
        "{set}"
    );

    // The control: the pane is no taller than the cut, so nothing more is
    // on the screen to re-read: the box is read with its head off the
    // screen, never re-read, and at full power answered by its options
    // (the E2E probe of 2026-09-25), the missing title said.
    let net = f::screen(f::BOX_NETWORK);
    let short = net[net.len() - 8..].to_vec();
    assert!(short[0].starts_with(" Network request"), "{short:?}");
    let mut m = Mock::new(true, vec![short]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &hosted(), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("APPROVED seq="), "{lines:?}");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(
        !m.requests.iter().any(|r| r == "text --json"),
        "{:?}",
        m.requests
    );
}

/// THE PHILOSOPHY REVIEW OF 2026-09-25 (major): a press that did not land —
/// here the box does not change after it — was handed to a person and never
/// tried again (`state.handed` skipped the box until the screen moved);
/// headless, nobody could answer. At full power, unattended, the box is now
/// read and pressed again on a back-off that doubles from 250 ms, journaled
/// `WAITING … the press did not land`, and the session is badged once it
/// has missed for a while — the tries going on behind the badge. NEGATIVE
/// CONTROL: under `approve = "safe"` the same box is handed over after its
/// one press, never pressed again.
#[test]
fn a_press_that_did_not_land_is_tried_again_at_full_power() {
    let (jdir, journal) = journal_file("press-missed");
    let mut m = Mock::new(true, vec![bash_one_row()]);
    m.stall_sleep = Some(Duration::from_millis(50));
    let base = auto(3, None);
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

    // NEGATIVE CONTROL: the safe rules hand it over after one press.
    let mut m = Mock::new(true, vec![bash_one_row()]);
    m.stall_sleep = Some(Duration::from_millis(50));
    let _ = watch_lines_with(&mut m, &auto(3, None), |s| s.set_approval_env(owner_env()));
    assert_eq!(m.presses().len(), 1, "{:#?}", m.requests);
}

/// NO REPEAT CAP AT FULL POWER (`approval_loop`'s module header): the same
/// box that comes back is pressed every time — a build or a test the worker
/// runs again and again is its business, and nobody else is there to answer
/// it — past the safe rules' two, a proven read too, and past upstream's
/// approve-all cap of twenty (2026-09-24), which the owner's "no
/// interruptions" retired. Control: the safe rules alone keep their cap of
/// two (`a_read_that_keeps_coming_back_is_approved_twice_then_handed_over`).
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
    let mut m = Mock::new(true, script(3, &bash_one_row));
    let (_, code) = session(&mut m, None)
        .supervise(&full_power_opts(None))
        .expect("supervise");
    assert_eq!(m.presses().len(), 3, "{:?}", m.requests);
    assert_eq!(code, 0);
    let touch = || {
        let mut r = bash_one_row();
        r[4] = "   touch x".to_string();
        r
    };
    let (dir, notes) = notes_file("repeat-all");
    let times = 25;
    let mut boxes = script(times, &touch);
    boxes.push(idle_screen());
    let mut m = Mock::new(true, boxes);
    session(&mut m, None)
        .supervise(&full_power_opts(Some(notes.clone())))
        .expect("supervise");
    assert_eq!(m.presses().len(), times, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert!(
        !lines.iter().any(|l| l.contains("handed to the manager")),
        "{lines:?}"
    );
}

/// Every way the trust press stops short is a negative control: a focus
/// that did not land (no Enter at all), another folder than the session's
/// (no key), a host with no generation fence (no key), and a Codex session
/// (its gate is escalated, never pressed).
#[test]
fn the_trust_dialog_is_never_entered_unconfirmed() {
    // The `down` is never taken — the focus still reads `No, exit` after
    // every move: moved again from a fresh read each time, then handed
    // over; never Enter.
    let mut m = fenced(vec![trust_dialog(false); 16]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
    });
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" enter")),
        "{:?}",
        m.requests
    );
    assert_eq!(count(&m, "key "), 5, "{:?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .unwrap_or_default()
            .contains("did not land"),
        "{:?}\n{:?}",
        m.attention,
        m.requests
    );
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
    let mut m = Mock::new(true, vec![trust_dialog(false)]);
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    watch_lines_with(&mut m, &auto(30, None), |s| s.set_approval_env(owner_env()));
    assert!(m.presses().is_empty(), "{:?}", m.requests);

    // Codex: its reader sees the gate, and every option is `Other`.
    let codex = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::CODEX_TRUST);
    let mut m = fenced(vec![codex]);
    m.program = "codex";
    m.cwd = Some(TRUST_FOLDER.to_string());
    m.vanish_after = Some(0);
    let d = {
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        let screen = s.read_screen().expect("read");
        s.decide_box(&screen, &auto(30, None), &[])
            .expect("decided")
    };
    assert!(
        matches!(&d, crate::supervise::policy::approval::Decision::Escalate { reason } if reason.contains("codex")),
        "{d:?}"
    );
}

/// A box is read by the reader for the session's FOREGROUND program
/// (`status program=`, lane C): a shell that shows a captured Claude box is
/// never pressed into. Negative control: the same screen under `claude` is
/// approved (every other test of the loop).
#[test]
fn a_box_on_a_shell_is_never_pressed() {
    for program in ["zsh", "less"] {
        let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
        m.program = program;
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert!(m.presses().is_empty(), "{program}: {:?}", m.requests);
        assert!(
            !lines.iter().any(|l| l.starts_with("APPROVED")),
            "{lines:?}"
        );
    }
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
    // An agent just started still reads as its shell until the server
    // resolves its name: the box waits for the server's verdict (`await
    // agent prompt`, bounded) and is judged under the name it then has.
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
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
    // An agent exec'ed in place keeps its launcher's name while the
    // server's verdict already names the screen an agent's: the verdict
    // decides (measured live: `bash -c 'exec -a claude …'` read
    // `program=bash` for seconds under `agent=prompt`).
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
    m.program = "bash";
    m.agent = "prompt";
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30, None));
    assert!(lines[0].starts_with("APPROVED"), "{lines:?}");
}

/// THE SUPERVISOR CLAIM (lane C): an unattended loop takes the session's
/// claim at the start as a lease, renews it with its requests, and releases
/// it at the end; `supervise` (one look for a manager who is right there)
/// takes none.
#[test]
fn a_watch_claims_the_session_renews_and_releases_it() {
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
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
    assert!(
        m.claims
            .iter()
            .filter(|c| c.starts_with("meta set supervisor"))
            .count()
            > 2,
        "renewed with the requests: {:?}",
        m.claims
    );
    // Given back only while it is still this loop's (`holder=`): a bare
    // unset would clear the claim of a supervisor that took the session
    // after this loop's lease lapsed.
    assert_eq!(
        m.claims.last().map(String::as_str),
        Some("meta unset supervisor holder=gui:7")
    );
    // Negative control: `supervise` claims nothing.
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    session(&mut m, None)
        .supervise(&auto(30, None))
        .expect("supervise");
    assert!(m.claims.is_empty(), "{:?}", m.claims);
}

/// Another supervisor's live claim (`ERR busy supervisor=<holder>`): the
/// loop says so and WATCHES ONLY — the read box it would approve is
/// pressed by nobody here, and no badge is raised or cleared — and it
/// takes over, saying so, the moment the claim is free. It never releases
/// a claim it does not hold.
#[test]
fn behind_another_supervisors_claim_the_loop_only_watches() {
    let mut m = Mock::new(
        true,
        vec![bash_one_row(), bash_one_row(), question_screen()],
    );
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
    // The question is escalated by nobody here, and the journal says so in
    // one plain line (lane B2's review: a lost line continuation had left a
    // run of spaces inside it).
    assert!(
        records.iter().any(|r| r.summary.ends_with(
            "attention=skipped: another supervisor (gui:7) answers this session mail=skipped: \
             the same"
        )),
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
    let mut m = Mock::new(true, vec![busy_screen(), bash_one_row(), bash_one_row()]);
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

/// THE HOSTED LOOP YIELDS (`SuperviseOpts::yield_when_held`, set by
/// `hosted()`): behind another holder's claim it ENDS with
/// `CLAIM_HELD` + the holder — no box judged, nothing pressed, no badge —
/// so the in-GUI host parks the session until a claim is released, instead
/// of a loop idling behind the claim. Its own claim is the only one taken,
/// under the host's name (`set_supervisor_name`). NEGATIVE CONTROL: the same
/// screen with the claim free is approved under that one claim.
#[test]
fn a_hosted_loop_behind_another_claim_ends_and_presses_nothing() {
    let hosted = SuperviseOpts::hosted();
    assert!(hosted.yield_when_held);
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
    m.claim_replies = VecDeque::from([err("busy supervisor=someone-else")]);
    let stop = Arc::new(AtomicBool::new(false));
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_supervisor_name(Some("aterm-harness@7".to_string()));
    let r = s.run_hosted(&hosted, stop, &mut out);
    assert_eq!(
        r,
        Err(format!("{CLAIM_HELD}someone-else")),
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(
        m.claims,
        ["@s-1 meta set supervisor aterm-harness@7 ttl=90000"],
        "one claim, the host's name, never released (it was never held)"
    );
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(
        !m.requests.iter().any(|r| r.contains("attention")),
        "{:?}",
        m.requests
    );

    // NEGATIVE CONTROL: the claim free, the same box is approved, under the
    // same single claim, and given back holder-conditionally at the end.
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
    m.vanish_after = Some(0);
    let stop = Arc::new(AtomicBool::new(false));
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_supervisor_name(Some("aterm-harness@7".to_string()));
    let _ = s.run_hosted(&hosted, stop, &mut out);
    let text = String::from_utf8_lossy(&out).to_string();
    assert!(text.starts_with("APPROVED"), "{text}");
    assert!(!m.presses().is_empty(), "{:?}", m.requests);
    assert_eq!(
        m.claims.first().map(String::as_str),
        Some("@s-1 meta set supervisor aterm-harness@7 ttl=90000"),
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

/// A host without the claim (`ERR usage`: before lane C) acts as before
/// the claim existed — the negative control that the claim is no gate the
/// loop needs.
#[test]
fn a_host_without_the_claim_is_supervised_as_before() {
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
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

/// TIER-1 for `SupervisorClaim` (aterm-spec `supervisor_claim_model`): the
/// real `Session`, over the scripted server, with the claim renewed before
/// every request (a zero renewal step), projected onto the model request by
/// request. A claim request is the model's `Renewal`, the server's answer
/// fixing what the environment did before it (`OK`: the claim was free or
/// the loop's; `ERR busy`: it lapsed and another supervisor took it — `Tick`s
/// to the lease's end, `Lapse`, `OtherClaims`), or its `Unserved` when the
/// request was not served; a `key` is a `Press`, which must be ENABLED at
/// the projected state, the invariant checked after each. `script`: the
/// answers to the claim requests in order, `OK` after it.
fn claim_conformance(script: &[ClaimAnswer]) -> (Mock, aterm_spec::derive::Model) {
    let model = aterm_spec::derive::supervisor_claim_model();
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row(), bash_one_row()]);
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
    // Lost to another supervisor after the start: the renewal before the
    // next request finds it, and nothing more is pressed.
    let (m, model) = claim_conformance(&[Yes, Busy, Busy, Busy, Busy, Busy, Busy, Busy]);
    assert!(m.presses().is_empty(), "{:?}", m.order);
    // PENDING (lane B2's review): renewals not served through the moment
    // the loop would press, then answered — the loop presses nothing while
    // it does not know whose the session is (the walk above fails on a
    // press at a pending view), and presses once its claim is back.
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
    // Negative control: the stale view — the lease lapsed, another holds
    // it, the loop has not asked since — is refused by the model and
    // admitted with the renewal skipped (Buggy = 1), so the pass above is
    // not vacuous.
    let mut stale = model.init_state();
    for (k, v) in [("held", 2), ("view", 1), ("age", 3)] {
        stale.insert(k, v);
    }
    assert!(!model.action_enabled("Press", &stale));
    assert!(aterm_spec::interp::with_buggy(&model, 1).action_enabled("Press", &stale));
    // And a pending view never presses, Buggy or not.
    let mut pending = model.init_state();
    for (k, v) in [("held", 2), ("view", 3), ("age", 0)] {
        pending.insert(k, v);
    }
    assert!(!aterm_spec::interp::with_buggy(&model, 1).action_enabled("Press", &pending));
}

/// TIER-1 for `SupervisorFocusChoice` (aterm-spec
/// `supervisor_focus_choice_model`): the real trust-dialog press over the
/// scripted server, projected request by request — a `key … down` is the
/// move (landed or lost, as the next read shows), a `text` read after it
/// the `Reread`, a `key … enter` the `Enter`, which must be ENABLED there.
/// Both measured shapes: the move lands (one Enter), the move is lost —
/// made again from each fresh read that shows the focus unmoved, then
/// handed over (no Enter, and the model's Enter is disabled where the real
/// loop stopped).
#[test]
fn tier1_enter_goes_only_on_a_confirmed_focus() {
    let model = aterm_spec::derive::supervisor_focus_choice_model();
    for lands in [true, false] {
        // Landed, the dialog then leaves (Claude Code starts); lost, it
        // stays as it was.
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

/// THE LOOP ON 2.1.281's QUESTION DIALOG (measured): the second tab, no
/// answer marked, and its review — each answered by one fenced Enter on the
/// focused row (`❯ 1. Small`, then `❯ 1. Submit answers`), never a digit,
/// ledgered `answer-recommended@v1`, nothing escalated — at every `approve`
/// level (a question is no permission). NEGATIVE CONTROL: the owner's
/// `answer_questions = false` (`--no-answer`) hands the dialog over,
/// unpressed.
#[test]
fn the_2_1_281_question_dialog_is_answered_by_enter_at_every_level() {
    use aterm_phase::prompt::fixtures::{ASK_SUBMIT, ASK_TWO_SECOND, screen};
    let dialog = || vec![screen(ASK_TWO_SECOND), screen(ASK_SUBMIT), idle_screen()];
    for approve in [Approve::All, Approve::Safe, Approve::None] {
        let mut opts = answering(30, None);
        opts.policy.approve = approve;
        let (ldir, ledger) = ledger_file("question");
        let mut m = questioned(dialog());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let presses = m.presses();
        assert_eq!(presses.len(), 2, "{approve:?}: {:?}", m.requests);
        for (p, row) in presses.iter().zip(["❯ 1. Small", "❯ 1. Submit answers"]) {
            assert!(p.starts_with("key if-gen="), "{p}");
            assert!(p.ends_with(&format!(" if={} enter", guarded(row))), "{p}");
        }
        assert!(
            !lines.iter().any(|l| l.starts_with("EVENT prompt")),
            "{approve:?}: {lines:?}"
        );
        let rows = ledger_rows(&ledger);
        assert!(
            rows.iter()
                .filter(|r| r.contains("\"decision\":\"approved\""))
                .all(|r| r.contains("\"rule_id\":\"answer-recommended@v1\"")),
            "{rows:?}"
        );
        let _ = std::fs::remove_dir_all(&ldir);
    }

    let mut opts = answering(30, None);
    opts.policy.approve = Approve::All;
    opts.policy.answer_questions = false;
    let mut m = questioned(dialog());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(lines[0].starts_with("EVENT prompt"), "{lines:?}");
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention") && r.contains("answer_questions is off")),
        "{:?}",
        m.requests
    );
}

/// A host at the loop's idle points ([`IdleHost`]), as a test drives it:
/// `wants` while its flag is set; each `at_idle` counted, the flag cleared
/// (the step taken), and the requests the loop had made by then kept.
#[derive(Debug, Default)]
struct TestHost {
    wants: AtomicBool,
    owns: AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
    /// What each restart the loop asks for answers, in turn (`None`: this
    /// host makes none); `None` once spent.
    restarts: std::sync::Mutex<VecDeque<Option<&'static str>>>,
    /// The restarts asked for, by their word.
    restarted: std::sync::Mutex<Vec<&'static str>>,
    /// Breaks of the agent's own background work offered
    /// ([`IdleHost::at_background`]), each answered with the notice.
    backgrounds: std::sync::atomic::AtomicUsize,
    /// Its word that nobody has asked the session anything
    /// ([`IdleHost::taskless`]).
    taskless: AtomicBool,
    /// Its step lets the turn ends go: `owns` cleared by `at_idle`.
    lets_go: AtomicBool,
    /// The turns the loop said ran ([`IdleHost::turn_ran`]).
    turns_ran: std::sync::atomic::AtomicUsize,
    /// Its step TYPES into the agent and owns nothing after it — a
    /// relaunch's carry-on ([`HostStep::moved`]).
    types: AtomicBool,
    /// Its moving step ENDS the agent and types nothing into it — a restart
    /// ([`HostStep::typed`] false).
    ends: AtomicBool,
    /// Steps it asks for again after the one taken (the upgrade parked for
    /// the next idle point after its notice).
    again: std::sync::atomic::AtomicUsize,
    /// The limit episodes the loop told of ([`IdleHost::limited`]), in order.
    limits: std::sync::Mutex<Vec<bool>>,
    /// How many times the loop asked whether the host owns a turn end — the
    /// host-side evidence that the loop reached one ([`Stop::Reached`]).
    turn_ends: std::sync::atomic::AtomicUsize,
}

impl IdleHost for TestHost {
    fn wants(&self) -> bool {
        self.wants.load(Ordering::SeqCst)
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
                typed: moved && !line.contains("done:fresh"),
            })
        };
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
        step("upgrade step=announced:1", true)
    }
    fn owns_turn_end(&self) -> bool {
        self.turn_ends.fetch_add(1, Ordering::SeqCst);
        self.owns.load(Ordering::SeqCst)
    }
    fn restart(&self, why: &crate::supervise::policy::turn_end::Restart) -> Option<String> {
        self.restarted.lock().unwrap().push(why.word());
        self.restarts
            .lock()
            .unwrap()
            .pop_front()
            .flatten()
            .map(str::to_string)
    }
    /// A host that restarts: one with an answer scripted.
    fn can_restart(&self) -> bool {
        !self.restarts.lock().unwrap().is_empty()
    }
    fn at_background(&self) -> Option<String> {
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
        Some("upgrade step=announced:1".to_string())
    }
    fn taskless(&self) -> bool {
        self.taskless.load(Ordering::SeqCst)
    }
    fn turn_ran(&self) {
        self.turns_ran.fetch_add(1, Ordering::SeqCst);
    }
    fn limited(&self, open: bool) {
        self.limits.lock().unwrap().push(open);
    }
}

/// A LIMIT EPISODE IS THE HOST'S TO HEAR, AND THE LOOP'S TO WAIT OUT, even
/// while the upgrade owns the session's turn ends (the review of
/// 2026-09-26). A notice is out (the host owns the turn ends) and the
/// wind-down turn hits the weekly limit: the loop opens its episode and says
/// so to the host (`limited(true)`: the host holds the upgrade's clocks,
/// whose own looks never happen during an episode) and closes it when the
/// worker answers after the continuation (`limited(false)`, the clocks held
/// again) — and past the reset the continuation IS typed (`CONTINUED …
/// usage-resume`): the upgrade's ownership held that point for good, nobody
/// typing and the host never stepping at a wall.
#[test]
fn a_limit_episode_is_told_to_the_host_and_waited_out_over_its_turn_ends() {
    let host = Arc::new(TestHost::default());
    host.owns.store(true, Ordering::SeqCst);
    let (dir, path) = journal_file("limit-host");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            weekly_limited(),
            busy_screen(),
            answered("keep going"),
        ],
    );
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

/// THE HOST TAKES NO STEP AT THE LOGIN WALL (the incident of 2026-09-27):
/// the screen the session showed for nine hours — the supervisor's
/// `continue` answered by `⏺ Login expired · Please run /login` — is no
/// idle point for the host, so the live upgrade's notice is never typed
/// into it (main read it idle with no wall, the host took its step there,
/// and four notices went into the wall, each met by the same row). Once the
/// person's `/login` is done (`⎿  Login successful`) the point is the
/// host's — the NEGATIVE CONTROL: the host does step there.
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
        let mut m = Mock::new(true, screens);
        m.vanish_after = Some(3);
        // The cursor in the prompt box, where Claude Code keeps it at an idle
        // point: main reads `idle` only where the box holds the cursor.
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

/// THE AGENT'S OWN BACKGROUND WORK IS A BREAK THE HOST IS OFFERED (the
/// owner's answer of 2026-09-26: "Busy agentic sessions get upgraded at
/// their next natural break. The notice interrupts the agent's orchestration
/// once"): a busy read whose ONLY busy signal is work the agent left running
/// (`✻ Crunched for … · done … · 1 shell still running`, the footer's `· 1
/// shell ·`), stood [`BACKGROUND_SETTLE`], is offered to the host that asks
/// for a point ([`IdleHost::at_background`]) — journaled `HOST seq=<n>
/// background <word>` — and is no idle point (`at_idle` is never asked
/// there). NEGATIVE CONTROLS: a live turn's spinner is never a break; a
/// break that has not stood its settle is not offered; a host that asks for
/// no point is offered nothing.
#[test]
fn a_break_of_the_agents_background_work_is_offered_to_the_host_once_it_stood() {
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
    let run = |screen: Vec<String>, settle: Duration, wants: bool| {
        let host = Arc::new(TestHost::default());
        host.wants.store(wants, Ordering::SeqCst);
        let (jdir, journal) = journal_file("background-break");
        let mut m = Mock::new(true, vec![screen]);
        m.vanish_after = Some(3);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| s.set_background_settle(settle));
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        (
            host.backgrounds.load(Ordering::SeqCst),
            host.steps.load(Ordering::SeqCst),
            lines,
        )
    };
    let (offered, idles, lines) = run(bg(), Duration::ZERO, true);
    assert!(offered >= 1, "offered: {offered}");
    assert_eq!(idles, 0, "a break is no idle point");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("HOST seq=")
                && l.ends_with(" background upgrade step=announced:1")),
        "{lines:#?}"
    );
    assert_eq!(run(busy_screen(), Duration::ZERO, true).0, 0, "a live turn");
    assert_eq!(
        run(bg(), Duration::from_secs(3_600), true).0,
        0,
        "not stood its settle"
    );
    assert_eq!(run(bg(), Duration::ZERO, false).0, 0, "no point asked for");
}

/// When [`hosted_with_host_ledger`] stops the loop it runs.
///
/// A fixed window alone read a slow machine as a wrong loop: every step the
/// arm asserts had to fit a 300-500 ms wall-clock window of mock round trips
/// with 5 ms stalls and back-offs, which timer coalescing and preemption
/// stretch on a loaded gate (the load-sensitive test audit of 2026-09-27).
enum Stop {
    /// After a fixed window — only for arms that assert something did NOT
    /// happen, where a longer run can only find more.
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
    let mut s = session(m, Some("@s-1".to_string()));
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    if let Some(t) = timing {
        s.set_turn_end_timing(t);
    }
    let r = s.run_hosted(&opts, stop, &mut out);
    setter.join().expect("setter");
    drop(s);
    let lines = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    (r, lines)
}

/// D3 IN THE LOOP: Claude Code's critical-memory banner at an idle point is
/// the HOST'S RESTART — the agent ended and relaunched on its conversation
/// ([`IdleHost::restart`]) — journaled `HOST seq=<n> restart:memory
/// step=<word>`, nothing typed into the process past saving, nothing
/// escalated. A restart that must wait (`wait:held`: a person's hand) is
/// tried again on the back-off, `WAITING … restart:memory waits: …`, never
/// escalated for it. NEGATIVE CONTROLS: a host that makes no restart, and one
/// whose restart can never be made (`refused:…`), hand the point to a person
/// with the banner's remedy — the escalation the policy made before D3.
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
        let mut m = Mock::new(true, vec![busy_screen(), screen(MEMORY_BANNER_IDLE)]);
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

    // NEGATIVE CONTROLS: no restart here, and one that can never be made.
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

/// A RESTART THAT TYPED ITS CARRY-ON (the review of the N1 fix, 2026-09-26):
/// a host whose restart types the relaunched agent's carry-on itself says
/// `continued` ([`IdleHost::restart`]) — a turn it typed, whose answer is
/// the harness's: answered short, it is no short turn of the worker's. Here
/// the memory banner's point starts no streak (a wall's point leaves it as
/// it was); the host restarts and types the carry-on, and after its short
/// answer the point is continued at once. NEGATIVE CONTROL: a restart that
/// typed nothing (`adopted`) leaves the next turn the worker's own — a
/// short turn, backed off.
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
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                screen(MEMORY_BANNER_IDLE),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.stall_sleep = Some(Duration::from_millis(5));
        // The restart at the banner's point, then the carry-on's answer at
        // the next (the host told a second turn ran), and a tail for what is
        // decided there.
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

/// D7 IN THE LOOP: a model bucket's fallback and its way back are RELAUNCHES
/// the host makes — `--model` on the relaunch line, session-only — never
/// Claude's own `/model`. The Fable limit is relaunched on opus
/// (`RESTARTED … rule=model-fallback@v1 relaunch --model opus (from fable,
/// back at …; session-only)`), its ledger row read back as the OPEN switch;
/// a loop that starts after a switch whose reset has passed relaunches the
/// session back on fable (`model-restore@v1`). NEGATIVE CONTROL: a fallback
/// the host could not make is waited out to the reset — nothing typed, no
/// badge — and nothing anywhere types `/model`.
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
        let mut m = Mock::new(true, screens);
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
            && l.contains("rule=model-fallback@v1 relaunch --model opus (from fable, back at ")
            && l.ends_with("; session-only)")),
        "{lines:#?}"
    );
    assert!(no_model(&m), "{:#?}", m.requests);
    let open = crate::supervise::approvals::open_model_switch(&ledger, Some("@s-1"))
        .expect("the open switch");
    assert_eq!(
        (open.from.as_deref(), open.to.as_str()),
        (Some("fable"), "opus")
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

    // NEGATIVE CONTROL: the fallback could not be made — the reset waited
    // out, nothing typed, no badge.
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

/// THE HOST'S STEP IS TAKEN IN PLACE (the elegance review of 2026-09-25,
/// two majors): the loop used to END at an idle point the host asked for —
/// its claim released, its badges cleared — and a new loop start after the
/// step with none of its policy's memory (the per-hour window, the back-off,
/// the walls), while every wait under a park flag was capped at 2 s to
/// re-read the flag. Now: the host's step is taken AT the first
/// authoritative idle point, before the point's own act (the box is
/// answered first; nothing is typed at the point the host took), the loop
/// runs on — its claim kept — and its waits stay WAIT_STEP long. NEGATIVE
/// CONTROL: a host that asks for nothing gets no step, and the point is
/// continued.
#[test]
fn the_hosts_step_is_taken_at_the_idle_point_and_the_loop_runs_on() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let host = Arc::new(TestHost::default());
    host.wants.store(true, Ordering::SeqCst);
    let mut m = fenced(vec![
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    // The cursor in the prompt box, where Claude Code keeps it at an idle
    // point: the host's step reads the point with it.
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(5));
    let (jdir, journal) = journal_file("idle-host");
    let journaled = SuperviseOpts {
        journal: Some(journal.clone()),
        ..full.clone()
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

    // NEGATIVE CONTROL: the host asks for nothing — the point is continued.
    let host = Arc::new(TestHost::default());
    let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
    m.stall_sleep = Some(Duration::from_millis(5));
    m.turn_releases = Some(1);
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
    assert_eq!(r, Ok(()));
    assert_eq!(host.steps.load(Ordering::SeqCst), 0);
    assert_eq!(count(&m, "@s-1 turn"), 1, "{:?}", m.requests);

    // The host owns the turn end (the upgrade's wind-down): nothing typed.
    let host = Arc::new(TestHost::default());
    host.owns.store(true, Ordering::SeqCst);
    let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let (_, _) = hosted_with_host(
        &mut m,
        &full,
        &host,
        Stop::After(Duration::from_millis(300)),
        Some(TurnEndTiming {
            short_backoff: Duration::from_millis(10),
            ..TurnEndTiming::default()
        }),
    );
    assert_eq!(count(&m, "@s-1 turn"), 0, "{:?}", m.requests);
}

/// THE INLINE REPL IS AN IDLE POINT (the review of 2026-09-26): Claude
/// Code's inline renderer draws its REPL at the top of the pane, blank rows
/// below it, and the loop's 40-row tail read of a 150x50 pane held the
/// prompt box's bottom rule, its footer and blank rows — never its caret —
/// so its `idle` was no evidence and the host's idle-point step (the
/// upgrade, a relaunch's carry-on) never ran there. That tail — the cursor
/// on the caret row above it ([`tail_misses_the_live_rows`]), its last row
/// blank — is read again whole and cut to the screen's live zone, its last 40
/// rows of content (the server's own cut): the measured inline REPL is the
/// idle point, and the host's step is taken. (The caret on the tail's first
/// row, its top rule cut: [`drawn_above_tail`], the next test.) The point is read with the
/// terminal's cursor ([`aterm_phase::read_at`]), each screen's measured one.
/// NEGATIVE CONTROLS: the fullscreen REPL, drawn to its last row, is read
/// from the tail alone and is the idle point as before; the inline REPL half
/// drawn (its bottom rule begun) is read whole and is no idle point; and the
/// inline renderer RELAUNCHED IN THE SAME TAB (2026-09-27) — the previous
/// run's prompt box whole above the new launch line, the cursor under that
/// line — is no idle point until the new REPL is drawn, where it is.
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
    // A step expected: stop once it is taken (a positive arm must not race a fixed
    // window under load); none expected: a fixed window, which a longer run can only
    // fail by finding one.
    let run = |text: &str, step: bool| {
        let repl = screen(text);
        assert_eq!(repl.len(), 50, "a 50-row pane");
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let mut m = Mock::new(true, vec![busy_screen(), repl]);
        let (row, _) = cursor(text).expect("a measured cursor");
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
    // NEGATIVE CONTROLS.
    let (steps, reads, requests) = run(LAUNCH_REPL_READY, true);
    assert_eq!(steps, 1, "the fullscreen REPL: {requests:#?}");
    assert!(
        !reads.iter().any(|r| r == "text --json"),
        "drawn to its last row: the tail alone: {reads:#?}"
    );
    let (steps, _, requests) = run(INLINE_REPL_HALF_DRAWN, false);
    assert_eq!(steps, 0, "the REPL half drawn: {requests:#?}");
    let (steps, _, requests) = run(INLINE_RELAUNCH_BEFORE_REPL, false);
    assert_eq!(steps, 0, "the previous run's box: {requests:#?}");
    let (steps, _, requests) = run(INLINE_RELAUNCH_REPL_READY, true);
    assert_eq!(steps, 1, "the relaunched REPL: {requests:#?}");
}

/// THE TAIL THAT CUTS THE PROMPT BOX'S TOP RULE (the merge of the branch's
/// live-zone read with main's `tail_misses_the_live_rows`, 2026-09-27): the
/// measured inline REPL one row lower in its 50-row pane, so its caret row —
/// the cursor on it — is the tail's FIRST row and its top rule the row above
/// the tail. Main's rules do not see it: the cursor is not above the tail,
/// the tail is not blank, and it holds no box. The tail, read with the
/// cursor, decides nothing (the prompt box that holds the cursor has no top
/// rule on it), and it ends on a blank row: it is read again whole and cut to
/// the live zone ([`drawn_above_tail`]), where the REPL is the idle point
/// and the host's step is taken. CONTROL: one row lower again, the whole box
/// is in the tail with the cursor on its caret — decided, so the tail
/// stands and is read once, no whole read.
#[test]
fn a_tail_that_cuts_the_prompt_boxs_top_rule_is_read_in_the_live_zone() {
    use aterm_phase::prompt::fixtures::{INLINE_REPL_READY, cursor, screen};
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let (caret, _) = cursor(INLINE_REPL_READY).expect("a measured cursor");
    let measured = screen(INLINE_REPL_READY);
    assert_eq!(measured.len(), 50, "a 50-row pane");
    let run = |down: usize| {
        let mut repl = vec![String::new(); down];
        repl.extend_from_slice(&measured[..measured.len() - down]);
        assert!(
            repl[caret + down].starts_with('❯'),
            "{:?}",
            repl[caret + down]
        );
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let mut m = Mock::new(true, vec![busy_screen(), repl]);
        m.screen_cursors.insert(1, caret + down);
        m.stall_sleep = Some(Duration::from_millis(5));
        let (r, _) = hosted_with_host(
            &mut m,
            &full,
            &host,
            Stop::reached(
                |h| h.steps.load(Ordering::SeqCst) >= 1,
                Duration::from_millis(300),
            ),
            None,
        );
        assert_eq!(r, Ok(()));
        let reads: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.contains(" text ") || r.starts_with("text "))
            .map(|r| r.trim_start_matches("@s-1 ").to_string())
            .collect();
        (host.steps.load(Ordering::SeqCst), reads, m.requests.clone())
    };
    // The caret on the tail's first row (row 10 of 50), the top rule above.
    let down = 50 - 40 - caret;
    let (steps, reads, requests) = run(down);
    assert_eq!(steps, 1, "the host's step at the REPL: {requests:#?}");
    assert!(
        reads
            .windows(2)
            .any(|w| w[0] == "text --json tail=40" && w[1] == "text --json"),
        "the undecided tail read again whole: {reads:#?}"
    );
    // CONTROL: the whole box in the tail, the cursor on its caret.
    let (steps, reads, requests) = run(down + 1);
    assert_eq!(steps, 1, "the REPL in the tail: {requests:#?}");
    assert!(
        !reads.iter().any(|r| r == "text --json"),
        "a decided tail stands: {reads:#?}"
    );
}

/// D3(c) OF THE LIVE E2E OF 2026-09-26: A TURN END THE HOST LET GO IS DECIDED
/// AGAIN. The point was decided once, as it came: while the upgrade owned the
/// session's turn ends (its settle), or passed over for the host's step — and
/// once the upgrade owned nothing (its ownership lapsed after
/// `OWNED_SETTLE_LOOKS`), nobody decided it again: from 18:37:44 to 18:42:49 the
/// E2E's Stage-1 end sat, neither continued nor upgraded. Now the loop decides
/// the point again the moment the host owns nothing: continued. Both ways in:
/// decided under the host's ownership that its step then lets go, and passed
/// over for a step that owns nothing. NEGATIVE CONTROLS: a host that keeps
/// owning it gets nothing typed; nor does a point a host step MOVED — its
/// carry-on typed, owning nothing after it, the screen still showing the old
/// point (the review of 2026-09-26: that point was decided again at once,
/// while the agent began its answer). TIER-1 for `SupervisorHostTurnEnd`
/// (aterm-spec `supervisor_host_turn_end_model`): each run is a path of the
/// model's, and the real loop continues exactly where the model's `Redecide`
/// is enabled at its end — and the `Buggy` loop is caught at each: stranded
/// where it waits on a let-go point, deciding a point that is gone where it
/// decides a moved one.
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
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        // The cursor in the prompt box, where Claude Code keeps it at an idle
        // point (the server's idle is the box that holds the cursor).
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        m.turn_releases = Some(1);
        // Every arm takes the host's step: stop once it is taken, then long
        // enough for the point decided again to be typed — or, where nothing
        // is typed, for a turn typed in error to show.
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
    let model = aterm_spec::derive::supervisor_host_turn_end_model();
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
            // NEGATIVE CONTROL: the loop that waits there is stranded.
            assert!(!model.action_enabled("LoopWaits", &st), "{why}");
            assert!(buggy.fire("LoopWaits", &mut waited), "{why}");
            assert!(
                !buggy.check_invariant("NoTurnEndLeftToNobody", &waited),
                "{why}"
            );
        } else if step == "HostMoves" {
            // NEGATIVE CONTROL: the loop that decides it again decides a
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

/// D3(a) IN THE LOOP: the host is told when a turn RAN since the last point
/// ([`IdleHost::turn_ran`]) — at the point a busy read came before — so the
/// looks it counted at the point before start over. NEGATIVE CONTROL: a point
/// read again with no busy read between (a repaint) tells it nothing.
#[test]
fn the_host_is_told_a_turn_ran_only_when_one_did() {
    let full = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted()
    };
    let run = |screens: Vec<Vec<String>>, stop: Stop| {
        let host = Arc::new(TestHost::default());
        host.owns.store(true, Ordering::SeqCst);
        let mut m = Mock::new(true, screens);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(0);
        // The script's end is the session's (`ERR exited`): what was said
        // before it is what counts.
        let _ = hosted_with_host(&mut m, &full, &host, stop, None);
        host.turns_ran.load(Ordering::SeqCst)
    };
    // A turn expected: stop once the host is told, then long enough for a
    // second telling to show; none expected: a fixed window, which a longer
    // run can only fail by finding one.
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

/// N1 OF THE LIVE E2E OF 2026-09-26, IN THE LOOP: a turn the host TYPED —
/// a carry-on, a notice ([`HostStep::typed`]) — is the harness's own, and
/// its SHORT answer is no short turn of the worker's: the streak stands as
/// the worker's turns left it. Here the streak is one (the first point
/// seen, its work unknown) and the answer is short (`min_work` an hour):
/// were it the worker's own turn, the streak would be two; it stays one,
/// and the back-off is the first point's. Its answer of REAL WORK
/// (`min_work` zero: the carry-on's answer is the worker's own work,
/// resumed) ends the streak as any turn of real work does: continued at
/// once. NEGATIVE CONTROL: after a step that ENDED the agent and typed
/// nothing (`done:fresh`), the short turn is the worker's own — a streak of
/// two.
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
        let mut m = Mock::new(true, vec![idle_screen(), busy_screen(), idle_screen()]);
        // The cursor in the prompt box, where Claude Code keeps it at an idle
        // point (the server's idle is the box that holds the cursor).
        m.cursor_on_caret = true;
        m.stall_sleep = Some(Duration::from_millis(5));
        let (jdir, journal) = journal_file("host-typed");
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..full.clone()
        };
        // The host's step at the first point, then its answer's point (a
        // busy read before it: the host is told a turn ran), and a tail for
        // what is decided there.
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

/// A TURN THE HOST TYPED HOLDS THE POLICY'S ACTS, NEVER THE HOST'S OWN NEXT
/// STEP: the upgrade's step after its notice reads its own evidence (the
/// READY answer in the conversation's record), and an answer no read saw
/// busy — the screen never moved — must not hold it until the screen moves
/// (the live headless fake's READY, which draws nothing: the upgrade sat
/// announced for the whole run). Here the host types its notice and asks for
/// the next idle point on a screen that never changes: its second step is
/// taken — and the policy types nothing meanwhile (the upgrade owns the
/// point). With the host's turn counted as an act of the policy's in flight,
/// the second step never comes (checked in place).
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
    let mut m = Mock::new(true, vec![idle_screen()]);
    // The cursor in the prompt box, where Claude Code keeps it at an idle
    // point (the server's idle is the box that holds the cursor).
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(5));
    // Stop once the second step is taken, then long enough for a turn the
    // policy must not type to show.
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

/// D1 OF THE LIVE E2E OF 2026-09-26, IN THE LOOP: a session its host says
/// nobody has asked anything ([`IdleHost::taskless`]: its conversation holds
/// only the harness's own turns, whatever the screen shows of them) gets
/// NOTHING at its turn ends — the report after a turn of work is not
/// continued, the question is not answered — and nothing is escalated.
/// NEGATIVE CONTROL: the same points on a host that says nothing of it are
/// continued and answered, as before.
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
            let mut m = Mock::new(true, vec![busy_screen(), point.clone()]);
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
        // NEGATIVE CONTROL: asked something, the point is acted on.
        let m = run(false);
        assert_eq!(count(&m, "@s-1 turn"), 1, "{what}: {:?}", m.requests);
    }
}

/// THE WAKE, measured: one turn — busy, the idle point, then the idle screen
/// repainting (a timer ticking, nothing the point is made of changing) —
/// and the next turn, under a policy that acts on nothing (so only the
/// waking is counted). On a host that pushes its agent verdict the idle
/// point is waited on with ONE `await agent` that wakes when the verdict
/// leaves `idle`, and no repaint costs a request; on one that does not, each
/// repaint is an `await seq` that latches, a settle and a screen read.
/// NEGATIVE CONTROL: the same script on the older host takes the old
/// path, and the requests it makes are what the push saves.
#[test]
fn the_idle_wait_wakes_on_the_servers_verdict_not_on_every_repaint() {
    let ticking = |i: u32| {
        let mut r = rows(&[
            "⏺ Stage 1 done.",
            "",
            "✻ Worked for 3m 2s · done 4:24 PM",
            "",
        ]);
        r.extend(composer(&format!("  ? for shortcuts · {i}s")));
        r
    };
    let script = || {
        let mut v = vec![busy_screen(), busy_screen(), ticking(0)];
        v.extend((1..=6).map(ticking));
        v.push(busy_screen());
        v.push(idle_screen());
        v
    };
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        policy: powerless(),
        ..SuperviseOpts::default()
    };
    let mut counts = Vec::new();
    for pushes in [true, false] {
        // Both hosts carry the screen generation; one pushes the verdict,
        // the other refuses the wait (a host between the two builds).
        let mut m = fenced(script());
        m.agent_pushes = pushes;
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &opts);
        let events: Vec<&String> = lines.iter().filter(|l| l.starts_with("EVENT")).collect();
        assert_eq!(events.len(), 2, "{pushes}: {lines:#?}");
        counts.push((
            pushes,
            m.requests.len(),
            count(&m, "text"),
            m.requests.clone(),
        ));
    }
    let (_, with, reads_with, reqs) = &counts[0];
    let (_, without, reads_without, _) = &counts[1];
    assert!(
        reqs.iter()
            .any(|r| r.starts_with("await agent busy,prompt,question,survey")),
        "{reqs:#?}"
    );
    assert!(
        with < without,
        "{with} requests with the push, {without} without"
    );
    assert!(
        reads_with < reads_without,
        "{reads_with} reads vs {reads_without}"
    );
    eprintln!(
        "ROUND TRIPS for one turn and six idle repaints: {without} requests ({reads_without} \
         screen reads) waking on the content, {with} ({reads_with}) waking on the verdict"
    );
}

/// A POINT READ BEFORE THE AGENT DREW ITSELF IS WAITED ON BY ITS CONTENT,
/// never by the server's verdict (the harness's live tests of 2026-09-26,
/// under load: the loop's first point caught the tab with the agent's launch
/// line still at the shell's prompt, the program not named yet — a screen
/// the loop's reader cannot vouch for, so no host step goes there — and the
/// server's verdict, already `idle`, never moved as the agent's composer came
/// up: the loop waited on it for ever and a fresh session's upgrade was
/// never taken). Here the host asks for a point from the start; the session
/// is PAST its start — a busy frame first, as a relaunch in the same tab is
/// (a fresh session's start window answers `None` before the vouch guard is
/// asked, which would pass this test with the guard gone) — then the shell's
/// launch line, then the agent's idle composer, and the host pushes its
/// verdict: the host's step is taken on the composer, which only the vouch
/// guard makes it wait for (deleting it leaves the loop on the verdict:
/// no step). NEGATIVE CONTROL: a first screen the reader vouches for (the
/// agent's idle composer) takes the step as it comes.
#[test]
fn a_point_read_before_the_agent_drew_itself_is_waited_on_by_its_content() {
    let launch = rows(&["% cd /w && '/Users//a/pkg/agents/claude' --model opus", ""]);
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(TestHost::default());
        host.wants.store(true, Ordering::SeqCst);
        host.lets_go.store(true, Ordering::SeqCst);
        let mut m = fenced(screens);
        // The cursor in the prompt box, where Claude Code keeps it at an idle
        // point (the server's idle is the box that holds the cursor).
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
    // A busy frame (the session past its start), then the launch line through
    // the loop's settling reads (each read serves the next screen), then the
    // composer comes up.
    let mut drawn = vec![busy_screen()];
    drawn.extend(vec![launch; 8]);
    drawn.push(idle_screen());
    let (steps, requests) = run(drawn);
    assert_eq!(steps, 1, "the step on the composer: {requests:#?}");
    // NEGATIVE CONTROL: the composer from the start.
    let (steps, _) = run(vec![idle_screen()]);
    assert_eq!(steps, 1);
}

/// THE VENDOR'S KEY GUARD (measured live 2026-09-24: plan mode's approval,
/// pressed the instant it was read, ignored the `1`): a box is judged only
/// on a read made once it has shown the box settle since a read FIRST
/// showed it — the young box is waited on (`await seq`, bounded by what is
/// left of the settle) and READ AGAIN, so the press is fenced on that fresh
/// read, never on one a sleep made stale (lane P's review: the settle slept
/// between the judged read and the press, and the blinking tool row above a
/// box then moved the fence). NEGATIVE CONTROL: with no settle the press
/// follows the first read at once.
#[test]
fn a_box_is_pressed_only_once_it_has_shown_the_settle() {
    for settle in [Duration::from_millis(300), Duration::ZERO] {
        let mut m = fenced(vec![bash_one_row()]);
        // A wait on the sitting box sleeps what it asks, as a real one does.
        m.stall_sleep = Some(settle);
        m.vanish_after = Some(u32::from(!settle.is_zero()));
        let started = Instant::now();
        let mut s = session(&mut m, None);
        s.set_box_settle(settle);
        let _ = s.supervise(&auto(30, None));
        let took = started.elapsed();
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
            // No settle, no wait — read off the requests, not a stopwatch: a
            // 250 ms bound on a whole in-process run with first-use setup read
            // one deschedule as a settle (the load-sensitive test audit of
            // 2026-09-27), and with `stall_sleep` zero a spurious wait would
            // not even have cost time.
            assert!(
                !before.iter().any(|r| r.starts_with("await seq ")),
                "a zero settle waited before the press: {before:?}"
            );
        } else {
            assert!(took >= settle, "{took:?}");
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

/// THE OWNER'S BOX in the loop (2026-09-24, live, `BOX_RM_WORKFLOW`): the
/// 0.92.0 host escalated this workflow subagent's rm-breaker box in the
/// owner's session and he had to press `1`. At default power the engine
/// presses `1` — the one-shot `Yes`, under the row guard of the box's first
/// command row — raises no badge, and ledgers the breaker's refusal it
/// answered over. NEGATIVE CONTROLS: under `approve = "safe"` (an operand
/// under no scratch root) and `"none"` nothing is pressed and it is badged.
#[test]
fn the_owners_workflow_rm_box_is_pressed_at_default_power() {
    let owners_box =
        || aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::BOX_RM_WORKFLOW);
    let first_row = owners_box()
        .into_iter()
        .find(|r| r.contains("rm -rf /Users//example/aterm-auto-target-x.noindex;"))
        .expect("the first command row");
    let at = |approve: Approve| SuperviseOpts {
        policy: SupervisorConfig {
            approve,
            ..auto(30, None).policy
        },
        ..auto(30, None)
    };
    let (ldir, ledger) = ledger_file("owners-box");
    let mut m = Mock::new(true, vec![busy_screen(), owners_box(), busy_screen()]);
    m.cwd = Some("/Users//example/aterm".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &at(Approve::All), |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert!(lines[0].starts_with("APPROVED "), "{lines:?}");
    let press = format!(
        "key if={} 1",
        crate::supervise::policy::row_guard(&first_row)
    );
    assert_eq!(m.presses(), [press.as_str()], "{:?}", m.requests);
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
        rows[0].contains("\"rule_id\":\"allow-once@v1\""),
        "{rows:?}"
    );
    assert!(
        rows[0].contains("aterm-auto-target-x.noindex"),
        "the overridden refusal is in the row: {rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);

    for approve in [Approve::Safe, Approve::None] {
        let mut m = Mock::new(true, vec![busy_screen(), owners_box()]);
        m.cwd = Some("/Users//example/aterm".to_string());
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines_with(&mut m, &at(approve), |s| {
            s.set_approval_env(owner_env());
        });
        assert!(
            lines[0].starts_with("EVENT prompt "),
            "{approve:?}: {lines:?}"
        );
        assert!(m.presses().is_empty(), "{approve:?}: {:?}", m.requests);
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention owner=supervisor ")),
            "{approve:?}: badged"
        );
    }
}

/// THE OWNER'S SUBAGENT BOX in the host's loop (2026-09-26,
/// `policy/fixtures/owner-rm-breaker-subagent-2026-09-26.txt`, the owner's
/// screenshot rebuilt: Claude Code 2.1.283, a `general-purpose` subagent's
/// Bash box carrying the rm circuit breaker's possibly-empty-variable note,
/// in a bypass session launched in `/Users/_owner/clean` by uid 502, a stand-in). The
/// installed 0.93.0 host escalated it (`the box header is not a Bash
/// header`) and it waited ~78 minutes for a person. Under the host's
/// default config (`SuperviseOpts::hosted_with` of
/// `SupervisorConfig::default()`: the owner has no `aterm.toml`), bypass
/// read off the footer before the box, the loop presses `1` ONCE under the
/// row guard of the box's first command row (and the generation fence, on a
/// host that has it), raises no badge, and writes ONE ledger row:
/// `allow-once@v1`, approved, its reason the `unproven:`
/// naming the rm circuit breaker's kind and the operand under no scratch
/// root. NEGATIVE CONTROL: the same box under `approve = "safe"` is
/// escalated, badged as the rm breaker's, nothing pressed.
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
    // On a host without the generation fence, and on one with it (the
    // owner's): the press is the same guarded `1`, fenced where it can be.
    for gen_fence in [false, true] {
        let (ldir, ledger) = ledger_file(&format!("owners-subagent-box-{gen_fence}"));
        let script = vec![bypass_busy(), owners_box(), busy_screen()];
        let mut m = if gen_fence {
            fenced(script)
        } else {
            Mock::new(true, script)
        };
        // The hidden cursor on the box's focused `❯ 1. Yes`, inside the
        // tail, where 2.1.283 parks it (measured 2026-09-26): one read.
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
            "fence={gen_fence}: {lines:?}"
        );
        let presses = m.presses();
        assert_eq!(presses.len(), 1, "fence={gen_fence}: {:?}", m.requests);
        if gen_fence {
            assert!(
                presses[0].starts_with("key if-gen=1.")
                    && presses[0].ends_with(&format!(" if={guard} 1")),
                "{presses:?}"
            );
        } else {
            assert_eq!(*presses[0], format!("key if={guard} 1"));
        }
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("meta set attention")),
            "fence={gen_fence}: no badge: {:?}",
            m.requests
        );
        let rows = ledger_rows(&ledger);
        assert_eq!(rows.len(), 1, "fence={gen_fence}: {rows:?}");
        assert!(
            rows[0].contains("\"rule_id\":\"allow-once@v1\"")
                && rows[0].contains("\"decision\":\"approved\"")
                && rows[0]
                    .contains("\"reason\":\"unproven: the rm circuit breaker (possibly-empty")
                && rows[0].contains("/Users/_owner/aterm-wt-gap-target"),
            "fence={gen_fence}: {rows:?}"
        );
        assert!(!rows[0].contains("is not a Bash header"), "{rows:?}");
        let _ = std::fs::remove_dir_all(&ldir);
    }

    let mut cfg = SupervisorConfig::default();
    cfg.set("approve", "safe").expect("the limit");
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&cfg)
    };
    let mut m = Mock::new(true, vec![bypass_busy(), owners_box()]);
    m.cursor_row = Some(focus);
    m.cwd = Some("/Users/_owner/clean".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_env(owner());
    });
    assert!(lines[0].starts_with("EVENT prompt "), "{lines:?}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let set = m
        .requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .expect("escalated");
    assert!(
        set.starts_with("claude rm-breaker: cd /Users/_owner/aterm-wt-gap "),
        "{set}"
    );
}

/// THE SILENT LOOP (the live 0.92.0 host's journal, 2026-09-24: `ESCALATED
/// seq=9246`, then nothing): a box the policy hands over is escalated, a
/// PERSON answers it, and the worker goes busy on what they allowed — for
/// as long as a workflow runs. The badge goes at the FIRST busy read after
/// the box left (the box's own row leaving is the server's push that wakes
/// the loop), never at the next idle point, which that turn may keep hours
/// off. NEGATIVE CONTROL: a box nobody answers keeps its badge while it
/// shows.
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
    let mut m = Mock::new(true, vec![busy_screen(), owners_box.clone(), busy_screen()]);
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
    // After the escalation: the wait on the box's own row (it LEAVES: the
    // person answered), the short settle, ONE read — busy — and the badge
    // goes, before the loop settles into the busy turn's own wait.
    let after: Vec<&str> = m.requests[set + 1..=unset]
        .iter()
        .map(String::as_str)
        .collect();
    assert!(after[0].starts_with("await gone "), "{after:#?}");
    assert_eq!(
        &after[1..],
        [
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "meta unset attention owner=supervisor",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(m.attention, None, "the badge is gone");
    assert!(
        m.requests[unset + 1..]
            .iter()
            .all(|r| !r.starts_with("meta set attention")),
        "raised once: {:#?}",
        m.requests
    );

    // NEGATIVE CONTROL: the box nobody answers keeps its badge.
    let mut m = Mock::new(true, vec![busy_screen(), owners_box]);
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

/// The options of the owner's default in a `supervise` run: full power
/// ([`auto`]'s every turn-end act limited, as its tests pin the approvals).
fn full_power_opts(notes: Option<PathBuf>) -> SuperviseOpts {
    let mut o = auto(60, notes);
    o.policy.approve = Approve::All;
    o
}

/// HARNESS ROUND-1 REVIEW (major, D2/D3): what a full-power press names —
/// the ledger row, the notes line, the `APPROVED` line — is the decision's
/// subject, not the parser's command: a workflow launch (whose command is
/// empty) is named by its title, and the trust dialog's backstop names its
/// warnings.
#[test]
fn full_power_ledgers_the_decisions_subject() {
    use aterm_phase::prompt::fixtures as f;
    let (ldir, ledger) = ledger_file("subject");
    let notes = ldir.join("notes.txt");
    let opts = full_power_opts(Some(notes.clone()));
    let mut m = Mock::new(true, vec![f::workflow_box(), busy_screen()]);
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
    let noted = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(noted.contains("): Run a dynamic workflow?"), "{noted}");
    let _ = std::fs::remove_dir_all(&ldir);

    // The backstop: the session's own folder, its warning in the subject.
    let (ldir, ledger) = ledger_file("subject-backstop");
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
    assert_ne!(on_yes, backstop, "the focus moved");
    let mut m = fenced(vec![backstop, on_yes, idle_screen()]);
    m.cwd = Some("/Users//user00/aterm".to_string());
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines_with(&mut m, &full_power_opts(None), |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert!(
        lines[0].contains("/Users//user00/aterm (backstop: This folder pre-approves 3 tool"),
        "{lines:?}"
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter()
            .any(|r| r.contains("\"decision\":\"approved\"") && r.contains("(backstop: ")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// HARNESS ROUND-1 REVIEW (minor, D6): a 2.1.281+ box ticks its auto-deny
/// countdown every second, and a tick is no sign the press was taken. A
/// `1` the box never took — the countdown ticking on under the same box —
/// is pressed ONCE and handed over as "the box did not change after the
/// press" (before the fix each tick read as the box leaving, and it was
/// pressed again and ledgered approved each time). Negative control: the
/// same box followed by a busy screen (the press taken) is pressed once
/// and not handed over.
#[test]
fn a_ticking_countdown_is_not_the_box_leaving() {
    use aterm_phase::prompt::fixtures as f;
    let at = |left: &str| -> Vec<String> {
        f::screen(f::BOX_RM_AUTO_DENY)
            .into_iter()
            .map(|r| r.replace("request in 1:59,", &format!("request in {left},")))
            .collect()
    };
    assert_ne!(at("1:58"), at("1:59"), "the countdown row");
    let (dir, notes) = notes_file("countdown");
    let mut m = Mock::new(true, vec![at("1:59"), at("1:58"), at("1:57"), at("1:56")]);
    let (out, _) = session(&mut m, None)
        .supervise(&full_power_opts(Some(notes.clone())))
        .expect("supervise");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(out.starts_with("prompt\n"), "{out}");
    let lines = read_notes(&dir, &notes);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("approved (allow-once@v1"), "{lines:?}");
    assert!(
        lines[1].contains("handed to the manager (the box did not change after the press)"),
        "{lines:?}"
    );

    // The round-2 review (major): under a grid narrower than the ~113-cell
    // countdown its tail wraps onto a row of its own. The countdown is then
    // the two rows joined, and neither is part of the box's identity: the
    // same box ticking is still pressed once and handed over.
    let wrapped = |left: &str| -> Vec<String> {
        let mut r = at(left);
        let i = r
            .iter()
            .position(|x| x.contains("automatically deny"))
            .expect("the row");
        r[i] = r[i].replace(" progress on an unattended session", "");
        r.insert(i + 1, " progress on an unattended session".to_string());
        r
    };
    let p = aterm_phase::parse_prompt_v2(&wrapped("1:59")).expect("the box");
    assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
    let (dir, notes) = notes_file("countdown-wrapped");
    let mut m = Mock::new(
        true,
        vec![
            wrapped("1:59"),
            wrapped("1:58"),
            wrapped("1:57"),
            wrapped("1:56"),
        ],
    );
    let (out, _) = session(&mut m, None)
        .supervise(&full_power_opts(Some(notes.clone())))
        .expect("supervise");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(out.starts_with("prompt\n"), "{out}");
    let lines = read_notes(&dir, &notes);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("did not change after the press")),
        "{lines:?}"
    );

    let (dir, notes) = notes_file("countdown-taken");
    let mut m = Mock::new(true, vec![at("1:59"), busy_screen()]);
    m.vanish_after = Some(0);
    let _ = session(&mut m, None).supervise(&full_power_opts(Some(notes.clone())));
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert!(
        !lines.iter().any(|l| l.contains("did not change")),
        "{lines:?}"
    );
}

/// The harness round-3 review (2026-09-24, minor): a ticking default-No
/// box — the irreversible tool's unnumbered `Tool use` box, focus on `No`,
/// with the 2.1.281+ auto-deny countdown — whose Enter the box never takes
/// is pressed ONCE: the focus moved to `Yes`, one fenced Enter, one
/// `approved` ledger row carrying `vendor default was No`, then "the box
/// did not change after the press". Before the fix the moved focus changed
/// the box's identity, so the countdown's next tick read as the box
/// leaving: it was decided again (the note gone) and Enter pressed and
/// ledgered a second time. Negative control: the box followed by a busy
/// screen (the Enter taken) is approved once and not handed over.
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

/// The safe rules (`approve = "safe"`) with the question answer on — a
/// question is no permission, so no level limits it — and a person's quiet
/// window of 10 s (`human_grace_s`): what the loop presses is the question
/// answer alone.
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

/// F3: THE INCIDENT, ANSWERED. The four-tab dialog of 2026-09-25 (S6-01 to
/// S6-05) is answered tab by tab — one fenced Enter each on the focused
/// recommended option, then one on the review's `Submit answers` — five keys,
/// every one `key if-gen=… if=<the focused row as drawn> enter`, no digit, no
/// escalation, five `answer-recommended@v1` rows in the ledger, and each
/// answer's `UNPROVEN` and `DIALOG` (its rows, O3) in the journal.
#[test]
fn a_four_tab_question_is_answered_tab_by_tab_and_submitted() {
    use aterm_phase::prompt::fixtures as f;
    let (ldir, ledger) = ledger_file("four-tabs");
    let (jdir, journal) = journal_file("four-tabs");
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        q(f::QUESTION_TABS_THIRD),
        q(f::QUESTION_TABS_FOURTH),
        q(f::QUESTION_REVIEW),
        idle_screen(),
    ]);
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
        assert!(p.starts_with("key if-gen=1."), "{p}");
        assert!(p.ends_with(&format!(" if={} enter", guarded(row))), "{p}");
    }
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
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

/// A host without the generation fence never gets a question key — the box
/// is handed over naming why. The host sends no person stamp either, so the
/// stand-in quiet (the screen still for 10 s, `await idle 10000`) is waited
/// out first, once.
#[test]
fn a_question_on_a_host_without_the_generation_fence_is_handed_over() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = Mock::new(true, vec![q(f::QUESTION_TABS_INCIDENT)]);
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

/// The free-text row focused (S1-03): the focus moves UP one row at a time —
/// each move fenced, guarded on the row it leaves and seen landing before
/// the next — then Enter on `❯ 1. Dark (Recommended)`. Never a digit, never
/// Enter on `Type something.`.
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
        m.presses().iter().all(|p| p.starts_with("key if-gen=1.")),
        "every key fenced: {:?}",
        m.presses()
    );
}

/// R2 (d): a person's text in the free-text row (S1-04): the question is
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

/// Multi-select (S3-06 → S3-16): Enter toggles option 1, the focus moves
/// down to option 3 and Enter toggles it, then down to `Next` and Enter —
/// and the next tab is answered in turn.
#[test]
fn multiselect_is_toggled_then_advanced() {
    use aterm_phase::prompt::fixtures as f;
    let fresh = q(f::QUESTION_MULTISELECT);
    let one: Vec<String> = fresh
        .iter()
        .map(|r| r.replace("❯ 1. [ ] Box drawing", "❯ 1. [✔] Box drawing"))
        .collect();
    assert_ne!(fresh, one, "PRECONDITION: the toggle landed");
    let checked = q(f::QUESTION_MULTISELECT_CHECKED);
    let mut m = questioned(vec![
        fresh,
        one.clone(),
        focused_on(&one, "2. [ ] Emoji"),
        focused_on(&one, "3. [ ] Powerline"),
        checked.clone(),
        focused_on(&checked, "4. [ ] Type something"),
        q(f::QUESTION_MULTISELECT_NEXT),
        q(f::QUESTION_TABS_FOURTH),
        idle_screen(),
    ]);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    let keys: Vec<&str> = m
        .presses()
        .iter()
        .map(|p| p.rsplit_once(' ').expect("a key").1)
        .collect();
    assert_eq!(
        keys,
        [
            "enter", "down", "down", "enter", "down", "down", "enter", "enter"
        ],
        "{:?}",
        m.requests
    );
    assert!(
        m.presses()[3].contains(&guarded("❯ 3. [ ] Powerline (Recommended)")),
        "the toggle is guarded on the unchecked box: {:?}",
        m.presses()
    );
    assert!(
        m.presses()[6].contains(&guarded("❯    Next")),
        "{:?}",
        m.presses()
    );
}

/// R2 (d), in full (the adversarial review of 2026-09-25): on a
/// multi-select, a check the SUPERVISOR did not make is a person's, even on
/// a recommended option — and so is a check it made that reads unchecked
/// again. Each is handed over, never toggled over. Three shapes: the S3-11
/// capture read first (its checks were the capture script's, a person's);
/// a fresh tab whose recommended option 1 a person checked; and option 1
/// the loop checked, then a person unchecked and moved off, quiet since.
/// NEGATIVE CONTROL: `multiselect_is_toggled_then_advanced` — every check
/// the loop's own — is answered through.
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
            lines.iter().any(|l| l.contains(&format!(
                "handed to the manager (a question a person has begun answering ({why}): theirs \
                 to finish)"
            ))),
            "{why}: {lines:?}"
        );
    }
}

/// The retry's premise is a screen that HELD STILL after the key (the
/// review of 2026-09-25): on one that never does — a row ticking outside
/// the box's identity — the unseen Enter is never sent again; after
/// [`MAX_UNSETTLED_WAITS`] waits that ran out the box is handed over. And
/// the retry is counted when it is WRITTEN: a retry the fence skipped
/// (`reason=changed`) is tried again before the box is handed over.
/// NEGATIVE CONTROL: `a_dialog_that_did_not_take_the_key_is_keyed_again_
/// once_then_handed_over`, where the screen holds still: two Enters.
#[test]
fn a_question_key_is_sent_again_only_after_the_screen_held_still_and_counted_when_written() {
    use aterm_phase::prompt::fixtures as f;
    let (dir, notes) = notes_file("q-never-still");
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.idle_never = true;
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, Some(notes.clone())))
        .expect("supervise");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("the screen never held still after it")),
        "{lines:?}"
    );

    // The retry skipped by the fence: tried again, then handed over.
    let (dir, notes) = notes_file("q-retry-skipped");
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    m.key_replies.push_back(ok("OK seq=101\n"));
    m.key_replies
        .push_back(ok("OK skipped reason=changed seq=101\n"));
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, Some(notes.clone())))
        .expect("supervise");
    assert_eq!(
        m.presses().len(),
        3,
        "the Enter, the retry the fence skipped, the retry written: {:?}",
        m.requests
    );
    let lines = read_notes(&dir, &notes);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("nothing changed after it, nor after it was sent once more")),
        "{lines:?}"
    );
}

/// A question key the server refuses `ERR busy input-unread` (a worker that
/// has left its input unread, 2026-09-24) is ledgered refused and backed
/// off, never re-sent at once and never escalated for it: the loop reads
/// `status` and the box again, and keys it once the worker reads again.
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

/// R5 as MEASURED on 2.1.282 (the live E2E of 2026-09-25): a draft typed
/// into the composer while the turn ran did not withhold the dialog — it was
/// drawn in place of the composer, the draft hidden under it — and one
/// fenced Enter on the recommended option answered it; the draft was back
/// in the composer afterwards, never submitted, never backspaced, never
/// typed over.
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

/// C6.2: under `approve = "safe"` a question's repeats are never counted by
/// the parser's (empty) command — counted so, the third tab would be "the
/// same prompt after two approvals".
#[test]
fn questions_are_not_capped_by_command_under_the_safe_rules() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = questioned(vec![
        q(f::QUESTION_TABS_INCIDENT),
        q(f::QUESTION_TABS_SECOND),
        q(f::QUESTION_TABS_THIRD),
        idle_screen(),
    ]);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    let opts = answering(30, None);
    assert_eq!(
        opts.policy.approve,
        Approve::Safe,
        "PRECONDITION: the safe rules"
    );
    s.supervise(&opts).expect("supervise");
    assert_eq!(m.presses().len(), 3, "{:?}", m.requests);
}

/// R3a: a dialog that did not take the Enter (nothing changed after it) is
/// not keyed again at once: the loop waits for the screen to hold still and
/// reads again, sends the key ONCE more, and after that hands the box over
/// — two keys in all, never a third.
#[test]
fn a_dialog_that_did_not_take_the_key_is_keyed_again_once_then_handed_over() {
    use aterm_phase::prompt::fixtures as f;
    let (dir, notes) = notes_file("q-not-taken");
    let mut m = questioned(vec![q(f::QUESTION_TABS_INCIDENT)]);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    let (out, code) = s
        .supervise(&answering(30, Some(notes.clone())))
        .expect("supervise");
    assert_eq!(code, 0, "{out}");
    assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    assert!(
        m.presses().iter().all(|p| p.ends_with(" enter")),
        "{:?}",
        m.presses()
    );
    assert!(
        m.requests.iter().any(|r| r.starts_with("await idle 2000 ")),
        "the wait for the screen to hold still: {:?}",
        m.requests
    );
    let lines = read_notes(&dir, &notes);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("handed to the manager (the dialog did not take the key")),
        "{lines:?}"
    );
}

/// P3 (the philosophy review of 2026-09-25): with nobody to hand it to —
/// `watch`, the window's host — a dialog that takes no key is keyed again
/// and again for as long as it stands, never at once: each try only after
/// the screen held still since the last (`await idle 2000` latched), each
/// after the press back-off (`WAITING … (try <n>)`), the session badged as
/// information once it has missed for a while, and nothing handed over.
/// The same holds under `approve = "safe"`: a question is no permission.
/// NEGATIVE CONTROL:
/// `a_dialog_that_did_not_take_the_key_is_keyed_again_once_then_handed_over`,
/// the one look, two keys and its manager.
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
    // Never two keys without the screen holding still between them.
    let mut still = true;
    for r in &m.requests {
        if r.starts_with("key ") {
            assert!(
                still,
                "a key before the screen held still: {:#?}",
                m.requests
            );
            still = false;
        } else if r.starts_with("await idle 2000 ") {
            still = true;
        }
    }
    let (records, _) = journal_records(&journal);
    assert!(
        records.iter().any(|r| r.line.starts_with("WAITING ")
            && r.line.contains("the dialog took none of the 2 keys sent")
            && r.line.contains("(try ")),
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

/// P3, the retry's premise kept: with nobody to hand it to, a screen that
/// never holds still after the key — a row ticking outside the box's
/// identity — is waited on the press back-off for as long as it lasts, and
/// the unseen Enter is never sent again; nothing is handed over. NEGATIVE
/// CONTROL: `a_question_key_is_sent_again_only_after_the_screen_held_still_
/// and_counted_when_written`, the one look, which hands it to its manager.
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
        records
            .iter()
            .any(|r| r.line.starts_with("WAITING ")
                && r.line.contains("the screen has not held still")),
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

/// R1: a PERSON keyed the session 3 s before the read: no key until they
/// have been quiet 10 s — the loop waits (woken early by any change, at
/// most the 7 s left) and reads again, and never escalates for it; the
/// next read says 12 s, and the question is answered.
#[test]
fn a_question_waits_for_a_persons_quiet_and_is_never_escalated_for_it() {
    use aterm_phase::prompt::fixtures as f;
    let incident = q(f::QUESTION_TABS_INCIDENT);
    let mut m = questioned(vec![incident.clone(), incident, idle_screen()]);
    m.human = vec!["3000", "12000"];
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    let first_read_seq = 101;
    let waited = format!("await seq {first_read_seq} timeout 7000");
    let wait_at = m
        .requests
        .iter()
        .position(|r| *r == waited)
        .unwrap_or_else(|| panic!("no {waited:?}: {:?}", m.requests));
    let key_at = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .expect("keyed");
    assert!(wait_at < key_at, "the wait came first: {:?}", m.requests);
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "{:?}",
        m.requests
    );
}

/// O5 under P3: the same question answered twice since the last review
/// point is the model re-asking it, and nobody else is there to answer it:
/// its third return is answered again — after a pause, journaled `WAITING …
/// re-asking it` — never handed over. The pause doubles with each further
/// return (`reask_pause`), from the press back-off's first to its minute.
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
        .position(|r| {
            r.line.starts_with("WAITING ")
                && r.line
                    .contains("the same question came back after 2 answers")
                && r.line.contains("re-asking it): answered again in 250 ms")
        })
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
    assert_eq!(
        (
            approval_loop::reask_pause(2),
            approval_loop::reask_pause(3),
            approval_loop::reask_pause(40)
        ),
        (
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(60)
        )
    );
    let _ = std::fs::remove_dir_all(&jdir);
}

/// A live-geometry question under the owner's default (the window's host):
/// answered, `APPROVED seq=…`, one fenced Enter.
#[test]
fn a_live_question_box_is_answered_under_the_default() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = questioned(vec![q(f::QUESTION_ONE)]);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&SupervisorConfig::default())
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_approval_env(owner_env()));
    assert!(
        lines[0].starts_with("APPROVED seq=")
            && lines[0]
                .ends_with("Which color scheme should the demo page use? → Dark (Recommended)"),
        "{lines:?}"
    );
    assert_eq!(m.presses().len(), 1, "{:?}", m.requests);
    assert!(m.presses()[0].ends_with(&format!(" if={} enter", guarded("❯ 1. Dark (Recommended)"))));
}

// ---- the session's own `questions` word, and the told choice -------------

/// The window's policy (`session_questions` on) with the global switch as
/// given.
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

/// THE SESSION'S WORD (owner, 2026-09-23: "global, and then session"): a
/// session's `meta set questions ask` hands its dialog to a person although
/// the global switch answers, naming the SESSION'S word; its `recommended`
/// answers it although the switch is off. NEGATIVE CONTROLS: unset, the
/// switch decides both ways; and a loop whose policy gives no session a say
/// (`session_questions` off: a loop's own `--no-answer`, an `answer_questions`
/// value the reader could not take) ignores the word.
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
    // NEGATIVE CONTROLS.
    assert_eq!(run(true, None, true).0, 1, "unset: the switch answers");
    assert_eq!(run(false, None, true).0, 0, "unset: the switch hands over");
    let (keys, badge) = run(false, Some("recommended"), false);
    assert_eq!(keys, 0, "no session has a say under this policy");
    assert!(
        badge.iter().any(|b| b.contains("answer_questions is off")),
        "{badge:?}"
    );
}

/// HANDED BACK: a dialog held for the person by the policy is answered once
/// the owner hands it back — the word changing AFTER the badge went up (the
/// mock's `questions_after_badge`), read again while the loop waits
/// (`HANDBACK_POLL`), the dialog decided afresh rather than passed over as the
/// point already handed. NEGATIVE CONTROL: never handed back, never typed.
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
        // screen that held still (main's R3a): one Enter, maybe twice.
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
    // NEGATIVE CONTROL.
    assert!(run(None).0.is_empty(), "never handed back");

    // Answered after the hand-back, it is the POLICY's answer: told once.
    let mut m = questioned(vec![q(f::QUESTION_ONE), busy_screen(), idle_screen()]);
    m.key_releases = Some(0);
    m.vanish_after = Some(2);
    m.questions = Some("ask");
    m.questions_after_badge = Some(None);
    let (lines, _) = watch_lines_with(&mut m, &hosted_answering(true), |s| {
        s.set_approval_env(owner_env())
    });
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CHOSE ")).count(),
        1,
        "{lines:#?}"
    );
}

/// THE TOLD CHOICE (owner, 2026-09-23: "a little sound and animation for
/// choosing"): a dialog the loop's own Enter answered, once the worker works
/// again, is said ONCE — `CHOSE seq=<n> rule=answer-recommended@v1
/// policy=recommended <question → answer>` — and the hosted loop, which has
/// no teller, tells the window `story chose recommended` on its own
/// connection (the chime and the rim pulse). NEGATIVE CONTROL: a person's
/// input after the loop keyed the dialog makes its end theirs — nothing told.
#[test]
fn an_answered_dialog_is_told_once_as_chose_and_a_persons_is_not() {
    use aterm_phase::prompt::fixtures as f;
    let (lines, m) = {
        let mut m = questioned(vec![q(f::QUESTION_ONE), busy_screen(), idle_screen()]);
        m.vanish_after = Some(1);
        let opts = hosted_answering(true);
        let mut out: Vec<u8> = Vec::new();
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _ = s.run_hosted(&opts, stop, &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        (text.lines().map(str::to_string).collect::<Vec<_>>(), m)
    };
    let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert_eq!(told.len(), 1, "{lines:#?}");
    assert!(
        told[0].contains("rule=answer-recommended@v1 policy=recommended ")
            && told[0]
                .ends_with("Which color scheme should the demo page use? → Dark (Recommended)"),
        "{}",
        told[0]
    );
    let stories: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.contains("story chose"))
        .collect();
    assert_eq!(stories.len(), 1, "{:#?}", m.requests);
    assert!(
        stories[0].ends_with("story chose recommended"),
        "{}",
        stories[0]
    );

    // NEGATIVE CONTROL: the person keyed the dialog after the loop's key
    // (their stamp young on the second read): the end is theirs.
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

/// A CONTROLLER'S OWN CHOICE (owner, 2026-09-23: a session under another's
/// control "needs to get the selection by the other controller session"):
/// `answer @w 2` moves the focus one row — an arrow guarded on the focused
/// row, fenced — sees it land, and presses Enter guarded on `❯ 2. Light`;
/// never a digit. `--box` with the dialog's own token passes; another is
/// REFUSED with nothing typed; so is a free-text answer; `human` types nothing;
/// no dialog is NO-BOX.
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

    // NEGATIVE CONTROLS: nothing typed.
    for (answer, want, code) in [
        ("2", "REFUSED @s-w the dialog on the screen is box=", 2),
        (
            "make it pop",
            "REFUSED @s-w \"make it pop\" names no option",
            2,
        ),
        ("human", "LEFT @s-w human: nothing typed", 0),
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
}

// ---- Tier-1: the loop bound to `SupervisorQuestionAnswer` ---------------

type QState = BTreeMap<&'static str, i64>;

/// The environment's actions in `SupervisorQuestionAnswer`: Claude Code
/// reading the oldest key, a person keying, their quiet returning.
const QUESTION_ENV: [&str; 13] = [
    "OpensOffChoice",
    "PersonKey",
    "QuietReturns",
    "PersonMovesOff",
    "PersonMovesOnto",
    "PersonTypes",
    "PersonAnswers",
    "PersonKeyIgnored",
    "MoveLands",
    "MoveShort",
    "EnterTaken",
    "KeyRefused",
    "KeyIntoTheComposer",
];

/// A read as the model sees it: the tab (the chips answered plus — a
/// multi-select's toggles being tabs of their own in the model — its
/// checked options, less `offset`; `Tabs` the review, `Tabs + 1` no
/// dialog), the focus (0 on the row the real decider chooses on this very
/// read, 1 anywhere else), its focus-order parity, a person's text in the
/// free-text row.
fn q_projected(rows: &[String], tabs: i64, offset: i64) -> (i64, i64, i64, i64) {
    use crate::supervise::policy::approval::{Answer, Choice, Decision};
    use crate::supervise::policy::question::{answer_question, target_focus};
    use aterm_phase::{QuestionFocus, QuestionForm};
    let Some(p) = aterm_phase::parse_prompt_v2(rows) else {
        return (tabs + 1, 0, 0, 0);
    };
    let Some(d) = p.question_dialog.as_ref() else {
        return (tabs + 1, 0, 0, 0);
    };
    let focus = d.focus();
    let checked = match &d.form {
        QuestionForm::Multi { options, .. } => {
            options.iter().filter(|o| o.checked == Some(true)).count()
        }
        _ => 0,
    };
    let tab = match d.form {
        QuestionForm::Review { .. } => tabs,
        _ => {
            i64::try_from(d.tabs.iter().filter(|t| t.answered).count() + checked).expect("tabs")
                - offset
        }
    };
    let chosen = match answer_question(&p, rows) {
        Decision::Approve {
            choice: Choice::Answer(Answer::FocusEnter { target, .. }),
            ..
        } => focus == target_focus(target),
        _ => matches!(
            focus,
            QuestionFocus::Option(1) | QuestionFocus::ReviewSubmit
        ),
    };
    let pos = crate::supervise::policy::question::focus_position(d, focus).expect("a focus");
    let typed = match &d.form {
        QuestionForm::Single { free_text, .. } => i64::from(!free_text.pristine),
        _ => 0,
    };
    (tab, i64::from(!chosen), i64::from(pos % 2), typed)
}

/// The environment behaviour the model allows that explains a read: the
/// fewest of [`QUESTION_ENV`] from `st` to a state that draws `want` with
/// the person's quiet `quiet` — a person keying only where the read's stamp
/// says one did (`new_input`). None explains it: the real loop saw what the
/// model cannot reach.
fn q_explained(
    model: &aterm_spec::derive::Model,
    st: &QState,
    want: (i64, i64, i64, i64),
    quiet: i64,
    new_input: bool,
    tabs: i64,
) -> QState {
    let draws = |s: &QState| {
        if want.0 == tabs + 1 {
            s["tab"] == tabs + 1
        } else {
            (s["tab"], s["focus"], s["row"], s["typed"]) == want
        }
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = VecDeque::from([(st.clone(), 0)]);
    while let Some((s, depth)) = queue.pop_front() {
        if draws(&s) && s["quiet"] == quiet {
            return s;
        }
        if depth == 6 || !seen.insert(s.clone()) {
            continue;
        }
        for a in QUESTION_ENV {
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

/// One scenario of the bind: the real loop over `m`, projected request by
/// request onto `model` (the committed one, or a variant of its constants
/// for the dialog's shape), the dialog's tabs counted from `offset`.
/// Returns the model state at its end, the keys the loop sent, and the
/// state at the first read that showed a person not quiet (the negative
/// control's).
fn q_bound_on(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    offset: i64,
) -> (QState, Vec<String>, Option<QState>) {
    let mut s = session(m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    q_projected_run(m, model, offset)
}

/// [`q_bound_on`] over a loop that has already run on `m` — `supervise`'s
/// one look, or `watch` with nobody to hand the box to (P3).
fn q_projected_run(
    m: &mut Mock,
    model: &aterm_spec::derive::Model,
    offset: i64,
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
    for (req, _) in &m.order {
        if req.starts_with("text ") {
            let (i, stamp) = *reads.next().expect("a served read");
            let age = stamp.parse::<u64>().ok();
            let quiet = i64::from(age.is_none_or(|ms| ms >= 10_000));
            let new_input = age.is_some_and(|ms| last_stamp.is_none_or(|was| ms < was));
            last_stamp = age.or(last_stamp);
            let want = q_projected(&m.screens[i], tabs, offset);
            st = q_explained(model, &st, want, quiet, new_input, tabs);
            if want.0 <= tabs {
                assert!(model.fire("Read", &mut st), "Read at {st:?}");
            }
            if quiet == 0 && person_seen.is_none() {
                person_seen = Some(st.clone());
            }
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

/// [`q_bound_on`] the committed model, the incident's dialog from its third
/// tab (two chips answered).
fn q_bound(m: &mut Mock) -> (QState, Vec<String>, Option<QState>) {
    q_bound_on(
        m,
        &aterm_spec::derive::supervisor_question_answer_model(),
        2,
    )
}

/// TIER-1 for `SupervisorQuestionAnswer` (aterm-spec
/// `supervisor_question_answer_model`), the LOOP: the real `Session` over
/// the scripted server, projected request by request — each read is the
/// model's `Read` after the fewest environment actions that explain what
/// it shows (the loop's own key taken or not yet, a person's key where the
/// read's stamp says one came, their quiet where it says it returned), each
/// `key … up|down|enter` the model's `HarnessMove`/`HarnessEnter`, which must be
/// ENABLED there, and the `await idle 2000` after a read that showed the
/// box as keyed its `StillScreen`; every invariant is checked after every
/// request. Three shapes:
///
/// 1. The free-text row a person left focused, then quiet: `↑` twice (each
///    seen landing), Enter, the next tab's Enter, the review's Enter.
/// 2. A person keys while the focus is being moved (the read inside the
///    answer says 400 ms): nothing more is sent until their stamp says
///    they are quiet, then the answer goes on. NEGATIVE CONTROL: where the
///    real loop sent nothing, the model with the fence alone (`Buggy = 1`)
///    enables the `↑`, and the model as committed does not.
/// 3. A dialog that takes no key: Enter, the screen still, Enter once more,
///    and no third key in the one look — the model's is disabled there at
///    the one look's bound (`MaxRetry = 1`). Unattended (`watch`, P3), the
///    loop keeps trying on the press back-off: every one of its keys is
///    enabled in the model at a bound that admits that many tries, each
///    written only after the screen held still.
#[test]
fn tier1_a_question_key_goes_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures as f;
    let model = aterm_spec::derive::supervisor_question_answer_model();
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

/// The lone multi-select question (R13: it draws the Submit tab, its button
/// reads `Submit`, and a review follows), keyed through: nothing checked
/// with the focus on 1 → 1 checked → the focus on 2, on 3 → 1 and 3
/// checked → the focus on the free-text row, on `Submit` → its review →
/// no dialog. The screens are the hand-built fixture (whose geometry the
/// live E2E of 2026-09-25 matched on 2.1.283) with the focus and the checks
/// moved as each key would, and a one-question review built from the
/// incident's.
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

/// R13 in the loop: a lone multi-select question is answered as its
/// dialog draws it — each recommended option toggled (Enter on its `[ ]`
/// row), the `Submit` button pressed, then the review's `1. Submit
/// answers` — Enter and arrows only, never a digit, never the free-text or
/// chat row.
#[test]
fn a_lone_multiselect_is_toggled_submitted_and_its_review_submitted() {
    let screens = lone_multiselect_screens();
    let review = aterm_phase::parse_prompt_v2(&screens[7])
        .and_then(|p| p.question_dialog)
        .expect("the built review reads as a question");
    assert!(
        matches!(review.form, aterm_phase::QuestionForm::Review { .. }),
        "PRECONDITION: {:?}",
        review.form
    );
    let mut m = questioned(screens);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering(30, None)).expect("supervise");
    let keys: Vec<&str> = m
        .presses()
        .iter()
        .map(|p| p.rsplit_once(' ').expect("a key").1)
        .collect();
    assert_eq!(
        keys,
        [
            "enter", "down", "down", "enter", "down", "down", "enter", "enter"
        ],
        "{:?}",
        m.requests
    );
    let presses = m.presses();
    assert!(
        presses[0].contains(&guarded("❯ 1. [ ] Box drawing (Recommended)")),
        "{presses:?}"
    );
    assert!(
        presses[3].contains(&guarded("❯ 3. [ ] Powerline (Recommended)")),
        "{presses:?}"
    );
    assert!(presses[6].contains(&guarded("❯    Submit")), "{presses:?}");
    assert!(
        presses[7].contains(&guarded("❯ 1. Submit answers")),
        "{presses:?}"
    );
}

/// TIER-1, the moves the first bind did not drive (the review of
/// 2026-09-25: its projection took only `↑`): a `↓` is the model's
/// `HarnessMove` as much as an `↑` is. Two more shapes, each on the model
/// with the dialog's own constants:
///
/// 4. S2-01, the live capture of a lone single-select question whose
///    recommendation is option 2 with the focus on option 1 (the model's
///    `OpensOffChoice`; `Tabs = 1`, `Review = 0`: it draws no Submit tab):
///    `↓`, seen landing, then Enter, and the dialog closes.
/// 5. The lone multi-select (R13; `Tabs = 3`, a toggle being a tab of the
///    model's own): Enter, `↓ ↓` Enter, `↓ ↓` Enter on `Submit`, the
///    review's Enter.
#[test]
fn tier1_a_downward_move_and_a_multiselect_go_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures as f;
    let model = aterm_spec::derive::supervisor_question_answer_model();

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
    let mut m = questioned(lone_multiselect_screens());
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
}

/// R1 across the first-key wait (O4): the decision read said no person, but
/// the tab's first key waits about a second first, and the server's stamp
/// asked after it (`status`'s `human_ms=`) says a person keyed 300 ms ago —
/// nothing is sent; the loop reads again, decides again on that read, and
/// keys once, fenced on it. Control: a host whose `status` carries no stamp
/// keys on the first decision read.
#[test]
fn a_person_who_keys_during_the_first_key_wait_gets_the_dialog_first() {
    use aterm_phase::prompt::fixtures as f;
    for stamped in [true, false] {
        let incident = q(f::QUESTION_TABS_INCIDENT);
        let mut m = questioned(vec![incident.clone(), incident, idle_screen()]);
        // The decision read's `status` says no person; the one asked after
        // the first-key wait says a person keyed 300 ms ago.
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
        let fenced_on = format!("key if-gen=1.{} ", 100 + reads_before);
        assert!(
            m.presses()[0].starts_with(&fenced_on),
            "fenced on the read it was decided on: {:?}",
            m.presses()
        );
    }
}

// --- THE DECLINE's keystrokes (`press.rs`) -----------------------------------

/// A host with BOTH generation fences: `help` names `if-gen=` for `key` and
/// for `send`, and `text --json` carries `"gen"` — and its person stamp
/// (`"human_ms": null`: no person has keyed the session), as a host of that
/// age sends.
fn fenced_both(screens: Vec<Vec<String>>) -> Mock {
    let mut m = fenced(screens);
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n\
              send [id=<key>] [if=<re>] [if-gen=<e.s>] <text>: write text\n"
        .to_string();
    m.human = vec!["null"];
    m
}

/// The measured decline states (`cap-decline-1..3`) and the fourth with
/// `text` typed into it — every screen a decline walks through.
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

/// THE DECLINE'S KEYSTROKES over the four screens Claude Code 2.1.282 drew
/// (measured): one keystroke per fresh read of the box, each fenced on THAT
/// read's generation and guarded on the row that shows the state it
/// answers — `down` from `❯ 1. Yes`, `tab` on `❯ 2. No`, the text sent into
/// the open, empty input, `enter` on the typed row — and the Enter is the
/// answer (`Press::Pressed`). Never a digit: on the refusal it is the bare
/// `No`, which stops the worker for a person. (The mock's guards are the
/// server's matcher over the screen last served, so a guard that matched no
/// row would have come back `OK skipped`, not `Pressed`.)
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
            format!("key if-gen=1.101 if={} down", g(" ❯ 1. Yes")),
            format!("key if-gen=1.102 if={} tab", g(" ❯ 2. No")),
            format!(
                "send if-gen=1.103 if={} -- {text}",
                g(" ❯ 2. No, and tell Claude what to do differently")
            ),
            format!("key if-gen=1.104 if={} enter", g(&typed_row)),
        ],
        "{:#?}",
        m.requests
    );
}

/// A STALE SCREEN REFUSES THE KEYSTROKE: the box ticks between the read and
/// the `down` (the 2.1.281 countdown does, every second), so the server
/// answers the generation fence `OK skipped reason=changed` and NOTHING
/// further is sent on that read — no Tab after a `down` that did not land.
/// The press says so (`Press::Changed`); the loop reads and decides again.
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
        w[0].starts_with("key if-gen=1.101 ") && w[0].ends_with(" down"),
        "{w:#?}"
    );
}

/// NEGATIVE CONTROLS — a decline is carried out only as far as each read
/// proves, and whatever it cannot finish is `Unconfirmed` (the loop hands the
/// box over), never guessed at; no `enter` is sent in any of them. A host
/// whose `send` takes no generation fence writes nothing at all, and neither
/// does a read that carried no generation. A text that does not show in the
/// input is not typed again. Text in the input that is not the decline's
/// (the measured stand-in) is never Entered. A box gone from a read is
/// `Skipped`, with nothing more sent.
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

/// ONE KEYSTROKE IN FLIGHT, NEVER WRITTEN TWICE (the review of 2026-09-25,
/// F1). The read after the Tab still shows `❯ 2. No` shut — Claude Code has
/// not drawn the Tab yet (a whole-frame redraw of a tall box on a loaded
/// machine) — and the fence cannot see a keystroke written and not yet
/// read. A second Tab would shut the input the first opens, and the reason
/// sent after it would meet the closed Select, whose `1` (the text quotes
/// `rm -rf $S/$1`) chooses `1. Yes`. So nothing is written on that read:
/// the screen is read again, and the reason goes once the input shows open.
/// Exactly ONE Tab (the reproduction wrote two, then the text, then Enter).
#[test]
fn a_decline_keystroke_not_drawn_yet_is_never_written_again() {
    let text = crate::supervise::policy::decline_text(
        "this command",
        "it is taller than the screen",
        "Run `rm -rf $S/$1` on its own as a short command",
    );
    assert!(text.contains('1'), "the reason carries the digit: {text}");
    let [box1, box2, box3, box4] = decline_states(&text);
    let typed_row = box4
        .iter()
        .find(|r| r.starts_with(" ❯ 2. No, aterm harness (not the user):"))
        .expect("the typed row")
        .clone();
    let mut m = fenced_both(vec![box1, box2.clone(), box2, box3, box4]);
    let pressing = press_decline_on(&mut m, &text);
    assert!(
        matches!(pressing, Pressing::Done(Press::Pressed { .. })),
        "{:#?}",
        m.requests
    );
    let g = |row: &str| crate::supervise::policy::row_guard(row);
    let (w, _): (Vec<String>, Vec<Option<u64>>) = writes(&m).iter().map(|w| unfenced(w)).unzip();
    assert_eq!(
        w,
        [
            format!("key if={} down", g(" ❯ 1. Yes")),
            format!("key if={} tab", g(" ❯ 2. No")),
            format!(
                "send if={} -- {text}",
                g(" ❯ 2. No, and tell Claude what to do differently")
            ),
            format!("key if={} enter", g(&typed_row)),
        ],
        "{:#?}",
        m.requests
    );
}

/// A KEYSTROKE THE BOX NEVER DRAWS IS HANDED OVER, NEVER RE-SENT (F1): the
/// Tab's effect never shows, so after the bounded reads the box is handed
/// over naming why — one Tab written, no reason, no Enter. The memory
/// outlives the call: the loop deciding the same decline again on the same
/// unmoved box writes nothing more. And the reason is typed ONCE across
/// calls too: an open, empty input that never shows the text is never typed
/// into a second time (the port's once-guard lasted one call). NEGATIVE
/// CONTROL: a fresh session (no memory) on the same unmoved screen does
/// write the Tab — the refusal is the memory's, not the screen's.
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

/// A KEYSTROKE WHOSE ANSWER NEVER CAME MAY HAVE BEEN WRITTEN (F1): the
/// connection closes on the reason's `send`, so the server may have typed it
/// — and the next press, on the same open input still showing nothing,
/// writes no second one (the box is handed over after the bounded reads).
/// And the ENTER stays remembered after it lands: a read still showing the
/// reason is the Enter not drawn yet, never one to send again. NEGATIVE
/// CONTROL: the same open input, the first `send` answered `OK skipped`
/// (nothing written), is typed into on the next press.
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

/// A PERSON KEYING THE SESSION GETS IT (F3; R1 of the question answer). A
/// read whose person stamp says a person keyed 400 ms ago writes nothing
/// more (`Press::Yielded`: the loop waits for their quiet): after the `↓`,
/// no Tab. The read the press starts on is checked like every other: a
/// person who keyed since the decision gets the box before any keystroke. A host
/// that stamps no person's input cannot say, and the box is handed over
/// with nothing written. NEGATIVE CONTROL: the same box with no person
/// (`null`) is declined all the way.
#[test]
fn a_person_keying_the_session_gets_the_decline_s_box() {
    let text = press_text();
    let [box1, box2, box3, box4] = decline_states(&text);
    let states = vec![box1.clone(), box2, box3, box4];

    let mut m = fenced_both(states.clone());
    m.human = vec!["null", "400"];
    let p = press_decline_on(&mut m, &text);
    assert!(
        matches!(p, Pressing::Done(Press::Yielded { .. })),
        "{p:?}: {:#?}",
        m.requests
    );
    let w = writes(&m);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].ends_with(" down"), "{w:#?}");

    let mut m = fenced_both(states.clone());
    m.human = vec!["400"];
    let p = press_decline_on(&mut m, &text);
    assert!(matches!(p, Pressing::Done(Press::Yielded { .. })), "{p:?}");
    assert!(writes(&m).is_empty(), "{:#?}", m.requests);

    let mut m = fenced_both(states.clone());
    m.human = Vec::new();
    match press_decline_on(&mut m, &text) {
        Pressing::Unconfirmed { why, .. } => assert!(why.contains("does not stamp"), "{why}"),
        other => panic!("expected the box handed over, got {other:?}"),
    }
    assert!(writes(&m).is_empty(), "{:#?}", m.requests);

    let mut m = fenced_both(states);
    let p = press_decline_on(&mut m, &text);
    assert!(matches!(p, Pressing::Done(Press::Pressed { .. })), "{p:?}");
    assert_eq!(writes(&m).len(), 4, "{:#?}", m.requests);
}

// --- A BOX TALLER THAN THE SCREEN, under the safe rules (`policy/approval.rs` tall_box)

/// What the loop's decider tells the worker on the owner's tall box: a
/// literal, so the loop is checked against the words, not against itself.
const TALL_RM_TEXT: &str = "aterm harness (not the user): this command was not run: it is taller \
     than the screen, so the harness cannot read all of it to check it. Run `rm -rf $S/$1` on \
     its own as a short command, separate from the rest, its paths written as literal absolute \
     paths (a command run on its own keeps none of this one's shell variables).";

/// The owner's box (`policy/fixtures/owner-bash-command-2026-09-24.txt`,
/// 544 rows) with its FOOT — from the question down — taken from one of the
/// measured decline states (`cap-decline-1..4`: the box as drawn, the focus
/// on No, the amend input open, the reason typed), on a 48-row screen; with
/// its note's `$S` made `var` (`$S` itself: the owner's box).
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

/// Each tall screen served twice: the loop reads the last 40 rows and —
/// the box's head cut by that tail — the whole screen again
/// (`tail_misses_the_live_rows`), and the mock serves one screen per read.
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

/// THE OWNER'S TALL BOX IN THE LOOP. Under the safe rules (`watch`'s
/// `approve = "safe"`), in its bypass session, it is declined and nobody is
/// asked: a box whose header is above the screen cannot be judged, so it is
/// refused WITH the removal its note flags — `down` off `❯ 1. Yes`, `tab` on
/// `❯ 2. No`, the reason sent into the open input, `enter` on the typed row
/// — each on a fresh read (the tail, then the whole screen), fenced on that
/// read's generation. The decline is an answer, not a review point: no
/// badge, no mail, no `EVENT prompt`, never the digit `1` and never a bare
/// `2`; the ledger says `declined` under `tall-box@v1` with the text, the
/// journal `DECLINED`. Every state is still taller than the screen. THE
/// CONTROL, full power (the window's default): the same box is answered by
/// its options, its one-shot `1`, once, and nobody is asked either.
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
            format!("key if={} down", g(" ❯ 1. Yes")),
            format!("key if={} tab", g(" ❯ 2. No")),
            format!(
                "send if={} -- {TALL_RM_TEXT}",
                g(" ❯ 2. No, and tell Claude what to do differently")
            ),
            format!("key if={} enter", g(&typed_row)),
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
    assert!(
        declined[0]
            .summary
            .starts_with("a box taller than the screen, its note: Dangerous rm operation")
            && declined[0].summary.ends_with(&format!("=> {TALL_RM_TEXT}")),
        "{}",
        declined[0].summary
    );
    let _ = std::fs::remove_dir_all(&ldir);
    let _ = std::fs::remove_dir_all(&jdir);

    // THE CONTROL: full power answers it by its options — its `1`, once.
    let (ldir, ledger) = ledger_file("tall-full");
    let mut screens = vec![bypass_busy()];
    screens.extend(each_read_twice(&[s1]));
    screens.push(bypass_busy());
    let mut m = fenced_both(screens);
    m.cwd = Some("/Users/_owner/aterm".to_string());
    m.vanish_after = Some(0);
    let hosted = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::hosted_with(&crate::supervise::SupervisorConfig::default())
    };
    let _ = watch_lines_with(&mut m, &hosted, |s| {
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let w = writes(&m);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].ends_with(" 1"), "{w:#?}");
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "{:#?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter()
            .any(|r| r.contains("\"rule_id\":\"allow-once@v1\"")
                && r.contains("unproven: the box's title row is not on the screen")),
        "{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// A PERSON AT THE TALL BOX IS NOT TYPED OVER (the review of 2026-09-25,
/// F3), in the loop: the reads the decline's keystrokes would go on say a
/// person keyed the session 400 ms ago, so nothing is written on them
/// (`Press::Yielded`) — the loop decides again from a fresh read — and once
/// the stamp says no person, the decline goes as ever. Every write is fenced
/// on a read AFTER the last one that showed the person (the screen
/// generation counts the reads), and nobody is asked for the wait.
/// NEGATIVE CONTROL: the loop test above, no person, declines on the first
/// reads.
#[test]
fn a_person_at_the_tall_box_is_waited_for_and_then_it_is_declined() {
    let [s1, s2, s3, s4] = tall_states("$S");
    let mut screens = vec![bypass_busy()];
    screens.extend(each_read_twice(&[s1.clone(), s1, s2, s3, s4]));
    screens.push(bypass_busy());
    let mut m = fenced_both(screens);
    m.cwd = Some("/Users/_owner/aterm".to_string());
    m.vanish_after = Some(0);
    m.human = vec!["null", "400", "400", "null"];
    let (lines, _) = watch_lines_with(&mut m, &auto(30, None), |s| {
        s.set_approval_env(owner_env());
    });
    let last_person = m
        .reads
        .iter()
        .rposition(|(_, stamp)| *stamp == "400")
        .expect("reads that showed the person");
    // The mock's generation is `1.<seq>`, one on per read from 100.
    let person_seq = 101 + u64::try_from(last_person).expect("an index");
    let gens: Vec<u64> = writes(&m)
        .iter()
        .map(|w| unfenced(w).1.expect("fenced"))
        .collect();
    assert_eq!(gens.len(), 4, "{:#?}", m.requests);
    assert!(
        gens.iter().all(|&g| g > person_seq),
        "a write fenced on a read that showed the person: {gens:?} (last person read seq \
         {person_seq})"
    );
    assert!(
        writes(&m).last().is_some_and(|w| w.ends_with(" enter")),
        "{:#?}",
        m.requests
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention") || r.starts_with("post ")),
        "nobody asked: {:#?}",
        m.requests
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("EVENT prompt")),
        "{lines:?}"
    );
}

/// THE SAME BOX BACK AFTER ITS DECLINES IS DECLINED AGAIN (`approval_loop`'s
/// module header): a worker told twice why its box was declined, and what to
/// do instead, that raises the SAME tall box a third time has not acted on
/// the reason yet — and nobody else is there to answer it. It is declined
/// again after a pause (`WAITING … declined again in <n> ms`), never handed
/// over for the count: no badge, no `EVENT prompt`. THE CONTROL: a
/// DIFFERENT tall box (its note flags another removal) after the same two is
/// declined at once, with its own removal.
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

/// A DECLINE STOPS WHERE THE BOX IS NOT THE SAME: after the `down` on the
/// owner's tall box, another tall box stands there — its note flags another
/// removal, so the text decided for the first would misname it — and
/// nothing more is sent: no Tab (`Press::Skipped`; the loop decides the new
/// box afresh). A tall box has no command to tell it by: the note at its
/// foot does (`PromptV2::foot_note`).
#[test]
fn a_decline_goes_no_further_once_another_tall_box_stands_there() {
    let [s1, _, _, _] = tall_states("$S");
    let [_, other2, _, _] = tall_states("$T");
    let mut m = fenced_both(each_read_twice(&[s1, other2]));
    let pressing = press_decline_on(&mut m, TALL_RM_TEXT);
    assert!(
        matches!(pressing, Pressing::Done(Press::Skipped { .. })),
        "{:#?}",
        m.requests
    );
    let w = writes(&m);
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].ends_with(" down"), "{w:#?}");
}

/// A DECLINE STOPS WHERE THE BOX IS NOT THE SAME, WHATEVER NOTE IT SHOWS
/// (the review of 2026-09-25, F5): a tall box is told by its note alone, so
/// its identity is also its kind, whether its head is off the screen, and
/// its command (`press.rs`, `decline_box`). After the `↓` on the owner's
/// tall box, (i) the owner's box WHOLE — a Bash box, its head on the screen,
/// the SAME note at its foot — or (ii) a whole box of another kind with the
/// same note stands there, and nothing more is sent: the text decided for a
/// box nobody could read ("it is taller than the screen") is no answer to a
/// box the rules can read. And on a WHOLE box's decline, (iii) a box of
/// another kind, (iv) the same box asking about another command, or (v) the
/// same command in another shell's box, with the same note. Each is checked to show the same note first, so only the
/// kind, the command or the head tells it apart.
#[test]
fn a_decline_goes_no_further_once_a_readable_box_with_the_same_note_stands_there() {
    use crate::supervise::policy::approval::tests as at;
    use aterm_phase::prompt::PromptKind;
    let [s1, s2, _, _] = tall_states("$S");
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
    // A WHOLE box's decline (the press is every decline's): after the `↓`
    // on the measured Bash box, (iii) a box of another kind with its note,
    // or (iv) the same Bash box asking about another command under the same
    // note — the head on the screen in all three, so only the kind or the
    // command tells them apart.
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

/// THE TAKEN BOX, REDRAWN BEFORE IT LEAVES (measured live 2026-09-25, Claude
/// Code 2.1.283, the proof run's rm boxes: `cap-taken-box-1..2`): within 5 ms
/// of a taken `1` the vendor wipes the auto-deny countdown and one of the two
/// blank rows under it, reflows the transcript above, and keeps the box drawn
/// some 550 ms more. That is the box taken, not a new one: it is pressed and
/// ledgered ONCE (before the fix every approved rm box of the run was pressed
/// and ledgered twice, the second `1` racing the box's leaving). A box with
/// no countdown (2.1.280) whose transcript reflows above it is the same box
/// too: any move was its leaving, and it was pressed again. Negative
/// control: a box with another command after the press is a new box, decided
/// and pressed afresh.
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
    let mut m = Mock::new(true, script);
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
    let mut m = Mock::new(true, vec![quiet, reflowed.clone(), reflowed, busy_screen()]);
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
    let mut m = Mock::new(true, script);
    m.vanish_after = Some(0);
    let _ = session(&mut m, None).supervise(&full_power_opts(Some(notes.clone())));
    assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    let lines = read_notes(&dir, &notes);
    assert_eq!(approvals(&lines), 2, "{lines:?}");
}

// ---- Tier-1: the decline's press bound to `SupervisorDeclineKeys` ---------

/// The environment's actions in `SupervisorDeclineKeys`: Claude Code reading
/// the oldest key, a person keying, their quiet returning.
const DECLINE_ENV: [&str; 17] = [
    "PersonKey",
    "QuietReturns",
    "PersonMoves",
    "PersonTabs",
    "PersonTypes",
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

/// A read as the model sees it: the focus (1 on the refusal), its input
/// open, what the input holds (0 nothing or the placeholder, 1 exactly
/// `text`, 2 anything else), and whether the box is gone (these scripts end
/// on the reason submitted).
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

/// The environment behaviour the model allows that explains a read: the
/// fewest of [`DECLINE_ENV`] from `st` to a state that draws `want` with the
/// person's quiet `quiet` — a person keying only where the read's stamp says
/// one did (`new_input`).
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

/// The real press of option 2 with `text` over `m`, projected request by
/// request onto `model`: each read the model's `Read` after the fewest
/// environment actions that explain it, each `key … down|tab|enter` and
/// `send …` the model's `HarnessDown`/`HarnessTab`/`HarnessEnter`/
/// `HarnessText`, which must be ENABLED there; every invariant checked after
/// every request. Returns the press's answer, the state at its end, the
/// keystrokes written, and the state at the first read that showed a person.
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

/// TIER-1 for `SupervisorDeclineKeys` (aterm-spec
/// `supervisor_decline_keys_model`; the review of 2026-09-25, F1 and F3):
/// the real press over the scripted server, bound request by request, in
/// four shapes — (1) the measured four states, declined; (2) the Tab not
/// drawn on the first read after it (the reproduction): one Tab, the reason
/// once the input shows open; (3) a person keys after the `↓`: nothing more
/// written; (4) the Tab never drawn: nothing more written, handed over.
/// NEGATIVE CONTROLS: where the real press wrote nothing in (3) and (4), the
/// model with the fence alone (`Buggy = 1`) enables the Tab again and the
/// model as committed does not; so a pass is never vacuous.
#[test]
fn tier1_a_decline_keystroke_goes_only_where_the_model_allows() {
    let model = aterm_spec::derive::supervisor_decline_keys_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let text = press_text();
    let [b1, b2, b3, b4] = decline_states(&text);

    // 1. The measured states.
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

/// EACH LIVE-ROW RULE ALONE (run.rs's `tail_misses_the_live_rows`): the
/// trust dialog of the fresh pane answered where only ONE of the two rules
/// sees it — so dropping either is caught (the adversarial review of
/// 2026-09-26: every other test here makes both true at once).
/// (1) The dialog two rows lower, its footer on the tail's first row and
/// the cursor on the focused `No, exit` above the tail: the tail is not
/// blank and holds no box, so only the cursor rule reads it whole.
/// (2) The dialog where it was, the hidden cursor parked on the pane's last
/// row (inside the tail): only the blank-tail rule reads it whole.
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

/// The tail's cut swept across the fresh pane's rm box (its top from row 22
/// up past the box's whole height) and across the trust dialog (shifted 0 to
/// 18 rows down), the cursor on the focused option where Claude Code 2.1.283
/// parks it (measured live 2026-09-26): every position is answered, the box
/// wholly in the tail, cut by it, or wholly above it.
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

/// THE HOSTED LOOP KEEPS ITS LEDGER WHERE ITS HOST SAYS, AND NOWHERE ELSE
/// (the 2026-09-25 audit: a hosted loop given no ledger fell back to the
/// PROCESS's own state directory, so every test that ran one wrote into the
/// owner's `~/Library/Application Support/aterm/drive/s-1.jsonl` — measured
/// on this Mac, rows from a test's `git log --oneline -5`). `opts.ledger`
/// is the host's: the approved box is ledgered there. NEGATIVE CONTROL: with
/// none, nothing names a ledger, and the loop has none to write.
#[test]
fn a_hosted_loop_ledgers_where_its_host_says_and_nowhere_else() {
    let (dir, ledger) = ledger_file("hosted-ledger");
    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
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

    let mut m = Mock::new(true, vec![bash_one_row(), bash_one_row()]);
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
