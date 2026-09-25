// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The engine over the scripted server (`Mock`): the approval policy in the
// loop, the press guard and fence, the wake on a ticking screen, the
// lifecycle (a hold, a refusal, a stale badge, the end) and the escalation
// (questions, the fabric's state), and the hosted entry. Included into
// `run.rs`'s test module (`mod engine`), where `Mock` and its helpers live.

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

/// The owner's process inputs, fixed.
fn owner_env() -> ApprovalEnv {
    ApprovalEnv {
        home: Some(PathBuf::from("/Users/_owner")),
        uid: 502,
        tmpdir: None,
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
    let mut s = Session::new(&mut m, None);
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
    let mut s = Session::new(&mut m, None);
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
    let mut s = Session::new(&mut m, None);
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
    let mut s = Session::new(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(m.presses(), [G_PRESS]);
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.help = "key [id=<key>] [if-seq=<n>] [if=<re>] <name>: send a named key\n".to_string();
    m.sends_gen = true;
    let mut s = Session::new(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(s.caps().gen_fence, Some(false));
    assert_eq!(m.presses(), [G_PRESS]);
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.help.clone_from(&help);
    m.gen_fence = true;
    let mut s = Session::new(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(m.presses(), [G_PRESS], "{:?}", m.requests);
    let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
    m.help.clone_from(&help);
    m.sends_gen = true;
    let mut s = Session::new(&mut m, None);
    s.supervise(&auto(30, None)).expect("supervise");
    assert_eq!(s.caps().gen_fence, Some(false));
    let presses = m.presses();
    assert_eq!(presses.len(), 2, "{:?}", m.requests);
    assert!(presses[0].starts_with("key if-gen=1.101 "));
    assert_eq!(presses[1], G_PRESS);
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
            "help key",
        ]
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");

    let mut m = Mock::new(false, vec![busy_screen(), bash_one_row()]);
    m.idle_never = true;
    let mut s = Session::new(&mut m, None);
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
    assert!(rows[0].contains("\"decision\":\"refused\""), "{rows:?}");
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
/// first wait. Negative controls: another writer's badge is left alone; a
/// badge of ours whose box is still on the screen is ADOPTED — no second
/// badge and no second ask for that box — and cleared when the box goes.
#[test]
fn a_stale_attention_is_reconciled_at_the_start() {
    let (dir, journal) = journal_file("stale");
    let mut m = Mock::new(true, vec![idle_screen()]);
    m.attention = Some("limited: You've hit your weekly limit reset=Sep 19".to_string());
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
    m.attention = Some("aterm harness: model bucket".to_string());
    m.vanish_after = Some(0);
    watch_lines(&mut m, &auto(30, None));
    assert_eq!(count(&m, "meta unset"), 0, "{:?}", m.requests);
    assert_eq!(m.attention.as_deref(), Some("aterm harness: model bucket"));

    let rm = write_prompt();
    let ours = super::super::escalate::attention_text(&turn_of_rows(&rm), "a restart");
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
    assert_eq!(
        lines[0],
        format!("APPROVED seq=102 {SCRATCH_RM}"),
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

/// A QUESTION is escalated like a box — badged `claude question: <what was
/// asked> (the worker asked a question)` and asked of the manager once while
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
        let text = "claude question: Keep the harness or rewrite it? (the worker asked a question)";
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
    let mut s = Session::new(&mut m, Some("@s-1".to_string()));
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

    // Without --auto-reads nothing is decided by policy, and nothing is
    // ledgered.
    let (ldir, ledger) = ledger_file("off");
    let mut m = Mock::new(true, vec![bash_one_row()]);
    let mut s = Session::new(&mut m, None);
    s.set_approval_ledger(Some(ledger.clone()));
    let opts = SuperviseOpts {
        auto_reads: false,
        ..auto(30, None)
    };
    s.supervise(&opts).expect("supervise");
    assert!(ledger_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&ldir);
}

/// The in-GUI host's entry: `SuperviseOpts::hosted()` is the unbounded
/// watch with the policy on, and `run_hosted` ends — `Ok`, `EXIT stopped` —
/// at the next look after its stop flag is set, clearing the badge it
/// raised.
#[test]
fn run_hosted_runs_until_it_is_stopped_and_clears_its_badge() {
    let hosted = SuperviseOpts::hosted();
    assert!(hosted.auto_reads && hosted.dismiss_surveys);
    assert_eq!(hosted.max, UNBOUNDED);
    assert_eq!(hosted.policy.approvals, ApprovalToggles::default());
    assert!(hosted.resume.is_some() && hosted.mail.is_none());

    // THE STOP IS RAISED ON EVIDENCE, never on a clock (2026-09-24): right
    // after the loop has raised its badge for the question it read. A fixed
    // 100 ms timer raced the whole first look — the sibling hand-over test lost
    // exactly that race at load ~40 with 662 tests in the process — and this
    // test's first assertion needs the escalation to land before the stop.
    struct StopAfterBadge<'a> {
        inner: &'a mut Mock,
        stop: Arc<AtomicBool>,
    }
    impl Ctl for StopAfterBadge<'_> {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let r = self.inner.call(args);
            if args.windows(3).any(|w| w == ["meta", "set", "attention"]) {
                self.stop.store(true, Ordering::SeqCst);
            }
            r
        }
    }
    let mut m = Mock::new(true, vec![question_screen()]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let stop = Arc::new(AtomicBool::new(false));
    let mut ctl = StopAfterBadge {
        inner: &mut m,
        stop: Arc::clone(&stop),
    };
    let mut out: Vec<u8> = Vec::new();
    let (ldir, ledger) = ledger_file("hosted");
    let mut s = Session::new(&mut ctl, Some("@s-1".to_string()));
    s.set_approval_ledger(Some(ledger));
    let started = Instant::now();
    let r = s.run_hosted(&hosted, stop, &mut out);
    assert_eq!(r, Ok(()));
    assert!(started.elapsed() < Duration::from_secs(5));
    let text = String::from_utf8(out).expect("utf-8");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("EVENT question "), "{lines:?}");
    assert_eq!(lines.last(), Some(&"EXIT stopped"), "{lines:?}");
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
    // the wait for it, not only at a look.
    let mut m = Mock::new(true, vec![busy_screen()]);
    m.stall_sleep = Some(Duration::from_millis(5));
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let setter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::SeqCst);
    });
    let mut out: Vec<u8> = Vec::new();
    let started = Instant::now();
    let r = Session::new(&mut m, None).run_hosted(&hosted, stop, &mut out);
    setter.join().expect("setter");
    assert_eq!(r, Ok(()));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(String::from_utf8(out).expect("utf-8"), "EXIT stopped\n");
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
        let r = Session::new(&mut ctl, Some("@s-1".to_string())).run_hosted(
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

/// `--dismiss-surveys` under a hold: the `0` refused `ERR halted` parks the
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
    let opts = SuperviseOpts {
        dismiss_surveys: true,
        ..auto(30, None)
    };
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

/// THE HOST'S `[harness] trust_roots` reach the decision: the options the
/// in-GUI host builds (`SuperviseOpts::hosted_with`, the whole table as
/// `policy`) put the configured roots into the approval context
/// (`ApprovalCtx::set_trust_roots`), so a root the owner wrote covers the
/// folder and one that does not leaves the dialog to the human — the same
/// dialog, the same session folder, only the roots differ. (Until the engine
/// lane's merge the host masked this key: the engine ran on its built-in
/// roots whatever the file said.)
#[test]
fn the_hosts_trust_roots_decide_the_trust_dialog() {
    for (roots, approved) in [
        ("/private/tmp/claude-502/scratch/work*", true),
        ("/nowhere/at/all*", false),
    ] {
        let mut cfg = crate::supervise::SupervisorConfig::default();
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
        let mut s = Session::new(&mut m, None);
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
    Session::new(&mut m, None)
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
    let mut s = Session::new(&mut m, Some("@s-1".to_string()));
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
    let mut s = Session::new(&mut m, Some("@s-1".to_string()));
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
