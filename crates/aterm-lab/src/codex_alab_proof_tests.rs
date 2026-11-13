// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn a_retargeted_alias_does_not_prove_a_running_image_upgraded() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes};
    use aterm_uds::spawnfd::{Stdio, command};
    let mut scratch = KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
    let old = scratch.path().join("old");
    let new = scratch.path().join("new");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let old = old.join("cat");
    let new = new.join("cat");
    ad_hoc_copy(Path::new("/bin/cat"), &old);
    ad_hoc_copy(Path::new("/bin/cat"), &new);
    let alias = scratch.path().join("selected");
    std::os::unix::fs::symlink(&old, &alias).unwrap();
    let mut child = command(&old)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = cat_echoes(&mut child);
    scratch.ran();
    let pid = i32::try_from(child.id()).unwrap();
    let record = J::obj([
        ("execGeneration", J::Int(2)),
        ("exe", J::s(alias.to_str().unwrap())),
    ]);
    let matched_old = observed_upgrade(&record, pid, 1, &old);
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&new, &alias).unwrap();
    let matched_new = observed_upgrade(&record, pid, 1, &new);
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(matched_old);
    assert!(
        !matched_new,
        "retargeting a name cannot replace a loaded image"
    );
}

/// A WORKER'S LANDING READS LANDED WHATEVER BECAME OF ITS TAB: live or reaped. NEGATIVE CONTROLS:
/// a closed tab that did not land, a failed conclusion, and a row still running.
#[test]
fn a_landed_row_reads_landed_live_or_reaped() {
    let row = |rest: &str| format!("@s-1 rev=2 {rest} task_news=1");
    for landed in [
        row("state=done conclusion=finished why=landed"),
        row("state=closed conclusion=finished why=landed"),
    ] {
        assert!(concluded(&landed) && landed_row(&landed), "{landed}");
    }
    for ended in [
        row("state=closed conclusion=failed why=exited"),
        row("state=closed conclusion=finished why=exited"),
    ] {
        assert!(concluded(&ended) && !landed_row(&ended), "{ended}");
    }
    for running in [row("state=working"), row("state=working conclusion=-")] {
        assert!(!concluded(&running) && !landed_row(&running), "{running}");
    }
}

/// EITHER FORM OF THE CLEARED LINE COUNTS AS THE PACKAGE HOLDING IT. NEGATIVE CONTROL: Cruise's
/// continuation, which a package from before the line types, does not.
#[test]
fn a_package_holds_the_cleared_line_in_either_form() {
    for line in crate::codex_alab_model::LIMIT_CLEARED {
        let program = format!("\u{0}\u{1}[codex-alab] {line}\u{0}");
        assert!(holds_cleared_line(program.as_bytes()), "{line}");
    }
    assert!(!holds_cleared_line(
        crate::codex_alab_model::KEEP_GOING.as_bytes()
    ));
    assert!(!holds_cleared_line(b""));
}

/// A PASS NAMES WHAT IT DID NOT OBSERVE: the land check's skip (no codex-alab build types it
/// yet) rides the verdict line, never a bare `PASS`. Negative control: the line without the
/// skips.
#[test]
fn a_pass_names_what_it_did_not_observe() {
    assert_eq!(verdict_line(&[]), "PASS");
    let line = verdict_line(&["land: no land-check@v1 typed".to_string()]);
    assert_eq!(line, "PASS, 1 unobserved: land: no land-check@v1 typed");
}

/// CODEX-ALAB'S CONTRADICTION IS JUDGED ONLY WHERE ITS FORK TYPED ONE (aterm's slice 4c): no
/// request tagged with it is a named skip (`None`); one tagged in aterm's words passes; one
/// in other words names its tag. Negative control: the want unconditional (always the skip)
/// never judges the wrong words.
#[test]
fn the_contradiction_is_judged_only_where_it_was_typed() {
    let tags = |t: &[&str]| t.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        contradiction_seen(&tags(&["land:land", "land:nudge1+repo"])),
        None
    );
    assert_eq!(
        contradiction_seen(&tags(&["land:land", "land:other+contradicted"])),
        Some(Vec::new())
    );
    assert_eq!(
        contradiction_seen(&tags(&["land:land", "land:other+contradicted-wrong"])),
        Some(vec!["land:other+contradicted-wrong"])
    );
}

fn reached(profile: &str, tag: &str, a: &Path, s: &Path, w: &Path) -> Vec<String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
    cmd.args(["-p", profile, "/bin/sh", "-c", FENCE_SCRIPT])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("A", a)
        .env("S", s)
        .env("W", w)
        .env("TAG", tag)
        .stdin(Stdio::null());
    let out = aterm_uds::deadline::outputs_until(
        &mut [&mut cmd],
        Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    )
    .pop()
    .expect("one child")
    .expect("the child ran");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

const FENCE_SCRIPT: &str = r#"try() { if "$@" >/dev/null 2>&1; then echo ok; else echo no; fi; }
try sh -c 'echo x > "$1/new-$2"' _ "$A" "$TAG"
try sh -c 'echo x > "$1/new-$2"' _ "$S" "$TAG"
try cat "$A/.ssh/id"
try cat "$S/.ssh/id"
try cat "$A/Library/Application Support/aterm/pkg/store/codex/1/bin/codex"
try cat "$A/plain"
try sh -c 'echo x > "$1/new-$2"' _ "$W" "$TAG"
"#;

#[test]
fn the_proofs_sandbox_fences_the_account_home_and_a_scratch_home() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-alab-fence-")
        .tempdir()
        .unwrap();
    let real = std::fs::canonicalize(top.path()).unwrap();
    let (account, scratch, world) = (real.join("account"), real.join("scratch"), real.join("w"));
    for h in [&account, &scratch] {
        for f in [
            ".ssh/id",
            "Library/Application Support/aterm/pkg/store/codex/1/bin/codex",
            "plain",
        ] {
            let path = h.join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "owned replica\n").unwrap();
        }
    }
    std::fs::create_dir_all(&world).unwrap();
    let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
    let fenced = policy(4242, &p, &world).unwrap();
    assert!(
        fenced.starts_with(&codex_rig::jail_policy(4242)),
        "the network rule stays"
    );
    assert_eq!(
        reached(&fenced, "fenced", &account, &scratch, &world),
        ["no", "no", "no", "no", "no", "ok", "ok"]
    );
    assert!(!account.join("new-fenced").exists() && !scratch.join("new-fenced").exists());
    assert_eq!(
        reached(
            &codex_rig::jail_policy(4242),
            "control",
            &account,
            &scratch,
            &world
        ),
        ["ok"; 7]
    );
    assert!(policy(4242, &p, &account.join("w")).is_err());
    assert!(policy(4242, &p, &scratch.join("w")).is_err());
}

#[test]
fn the_relaunched_holders_act_is_read_from_the_log() {
    let mark = "OK log ep=3\n41 mark t2 act word=dispatch rule=harness-turn.v1 \
                holder=codex-alab@4242 harness_act=harness-turn@v1 by=s-abc src=mark \
                conf=claim t=9\n";
    assert!(acted_in_log(mark, "harness-turn@v1", "codex-alab@4242"));
    assert!(
        !acted_in_log(
            &mark.replace(" harness_act=harness-turn@v1", ""),
            "harness-turn@v1",
            "codex-alab@4242"
        ),
        "a mark with no harness_act="
    );
    let unworded = "OK log ep=3\n42 mark t2 act word=unknown holder=codex-alab@4242 \
                    harness_act=finished@v1 by=s-abc src=mark conf=claim t=9\n";
    assert!(acted_in_log(unworded, "finished@v1", "codex-alab@4242"));
    assert!(!acted_in_log(unworded, "finished@v2", "codex-alab@4242"));
    assert!(
        !acted_in_log(
            &mark.replace(" holder=codex-alab@4242", ""),
            "harness-turn@v1",
            "codex-alab@4242"
        ),
        "a mark with no holder="
    );
    assert!(!acted_in_log(mark, "harness-turn@v1", "codex-alab@1"));
    assert!(!acted_in_log(mark, "land@v1", "codex-alab@4242"));
    assert!(!acted_in_log(
        "OK 7 t=1 kind=harness-act act=harness-turn@v1 holder=codex-alab@4242\n",
        "harness-turn@v1",
        "codex-alab@4242"
    ));
}

#[test]
fn the_verdicts_read_aterm_s_own_lines() {
    let raised = "OK title=- attention=Codex%20needs%20you attention_owner=needs \
                  attention_owners=2 attention_kinds=ask:codex-alab,ask:needs supervisor=-";
    assert!(needs_raised(raised));
    assert!(
        needs_raised("OK attention_kinds=ask%3Aneeds"),
        "pct-encoded too"
    );
    assert!(
        !needs_raised("OK attention_kinds=ask:codex-alab"),
        "another owner's ask"
    );
    assert!(
        !needs_raised("OK attention_kinds=wait:needs"),
        "a wait is no need"
    );
    assert!(!needs_raised("OK attention_kinds=-"));
    assert_eq!(task_face("OK x=1 task=landed y=2"), "landed");
    assert_eq!(
        task_face("OK x=1 task_state=done"),
        "-",
        "task_state= is no face"
    );
    assert_eq!(
        nudges_seen(&[
            "land:person".to_string(),
            "land:land".to_string(),
            "land:nudge1".to_string(),
            "land:nudge2+ack".to_string(),
            "land:nudge3".to_string(),
        ]),
        vec![1, 2, 3]
    );
    assert!(nudges_seen(&["land:land".to_string()]).is_empty());
    let acts = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        nudges_before_landing(&acts(&[
            "land@v1",
            "land-nudge@v1",
            "land-nudge@v1",
            "landed@v1",
            "land-nudge@v1",
        ])),
        2,
        "a nudge after the landing is none before it"
    );
    assert_eq!(
        nudges_before_landing(&acts(&["land@v1", "land-nudge@v1"])),
        1,
        "no landing: every nudge"
    );
    assert_eq!(
        nudges_before_landing(&acts(&["land@v1", "landed@v1", "land-nudge@v1"])),
        0
    );
    let rows = vec![
        J::obj([("t", J::Int(10)), ("tag", J::s("first:person"))]),
        J::obj([("t", J::Int(130_010)), ("tag", J::s("first:defend"))]),
    ];
    assert_eq!(first_at(&rows, "first:defend"), Some(130_010));
    assert_eq!(first_at(&rows, "first:nudge"), None);
    assert_eq!(secs(130_000), "130.0 s");
}

#[test]
fn a_continuation_is_quiet_through_its_own_reply_window() {
    let row = |id: i64, at: i64, tag: &str| {
        J::obj([
            ("request_id", J::Int(id)),
            ("mono_ms", J::Int(at)),
            ("t", J::Int(i64::MAX)),
            ("tag", J::s(tag)),
            ("hold_ms", J::Int(0)),
            ("status", J::Int(200)),
        ])
    };
    let response = |id, at, status| {
        J::obj([
            ("event", J::s("response_written")),
            ("request_id", J::Int(id)),
            ("mono_ms", J::Int(at)),
            ("status", J::Int(status)),
        ])
    };
    let asked = vec![
        row(1, 0, "continue:person"),
        row(2, 120_000, "continue:fail"),
        row(3, 125_000, "continue:continue"),
    ];
    let sent = response(3, 190_000, 200);
    let initial = with_replies(asked.clone(), std::slice::from_ref(&sent));
    let seen = observe_reply(&initial, "continue:continue", 200_000);
    assert!(seen.is_some());
    let until = 205_000;
    let mut later = initial.clone();
    later.push(row(4, 370_000, "continue:defend+repo"));
    assert_eq!(continued_quiet(&later, seen, 500_000), Ok(()));
    assert_eq!(continued_quiet(&initial, seen, until), Ok(()));
    for extra_at in [191_000, until] {
        let mut spin = initial.clone();
        spin.push(row(4, extra_at, "continue:defend+repo"));
        assert!(continued_quiet(&spin, seen, 500_000).is_err(), "{extra_at}");
    }
    assert!(continued_quiet(&initial, seen, until - 1).is_err());
    assert!(continued_quiet(&initial[..1], seen, 500_000).is_err());
    let unfailed: Vec<J> = initial
        .iter()
        .filter(|r| str_of(r, "tag") != "continue:fail")
        .cloned()
        .collect();
    assert!(
        continued_quiet(&unfailed, seen, 500_000).is_err(),
        "the continuation's failing command is part of its quiet turn"
    );
    assert_eq!(observe_reply(&initial, "continue:continue", 189_999), None);
    for replies in [
        vec![],
        vec![response(4, 190_000, 200)],
        vec![response(3, 190_000, 429)],
        vec![response(3, 124_999, 200)],
        vec![sent.clone(), sent],
    ] {
        let rows = with_replies(asked.clone(), &replies);
        let seen = observe_reply(&rows, "continue:continue", 500_000);
        assert_eq!(seen, None);
        assert!(continued_quiet(&rows, seen, i64::MAX).is_err());
    }
    let mut retried = asked;
    retried.push(row(4, 195_000, "continue:continue"));
    let rows = with_replies(retried, &[response(4, 196_000, 200)]);
    assert_eq!(observe_reply(&rows, "continue:continue", 500_000), None);
    let duplicate = vec![
        row(3, 125_000, "continue:continue"),
        row(3, 125_000, "continue:continue"),
    ];
    let rows = with_replies(duplicate, &[response(3, 190_000, 200)]);
    assert_eq!(observe_reply(&rows, "continue:continue", 500_000), None);
}

/// P1'S CHECK (gaps 20, 25): the continue leg's turn lines hold what the build's gate reads,
/// and nothing it does not; `cause=` is never needed.
#[test]
fn the_gated_reads_hold_as_the_build_reads_them() {
    let l = |s: &str| s.to_string();
    let open1 = l("4 open t1 turn by=- src=transcript conf=exact t=1");
    let open2 = l("7 open t2 turn by=- src=transcript conf=exact t=2");
    let amend2 = l("8 amend t2 turn turn_by=harness by=vendor src=transcript conf=exact t=2");
    let amend1 = l("5 amend t1 turn turn_by=harness by=vendor src=transcript conf=exact t=1");
    let close1 = l("6 close t1 turn out=done steps=1 src=transcript conf=exact t=1");
    let failed2 = l("9 close t2 turn out=done steps=2 errs=1 src=transcript conf=exact t=3");
    let both = [
        open1.clone(),
        close1.clone(),
        open2.clone(),
        amend2.clone(),
        failed2.clone(),
    ];
    assert_eq!(gated_holds(Gate::ALL, &both), Ok(()));
    let apart = [
        open1.clone(),
        amend1,
        close1.clone(),
        open2.clone(),
        failed2.clone(),
    ];
    assert!(
        gated_holds(Gate::ALL, &apart).is_err(),
        "two different turns"
    );
    let exit = Gate {
        exit: true,
        autopilot: false,
    };
    let failed = [
        open1.clone(),
        close1.clone(),
        open2.clone(),
        failed2.clone(),
    ];
    assert_eq!(gated_holds(exit, &failed), Ok(()));
    assert!(
        gated_holds(exit, &both).is_err(),
        "a turn_by= line its gate does not read"
    );
    assert!(
        gated_holds(exit, &[open1.clone(), close1.clone()]).is_err(),
        "no errs=1"
    );
    assert!(gated_holds(Gate::ALL, &failed).is_err(), "no harness amend");
    let quiet = [
        open1.clone(),
        close1,
        open2,
        l("9 close t2 turn out=done steps=2 t=3"),
    ];
    assert_eq!(gated_holds(Gate::NONE, &quiet), Ok(()));
    for line in [failed2, amend2] {
        let mut said = quiet.to_vec();
        said.push(line.clone());
        assert!(gated_holds(Gate::NONE, &said).is_err(), "{line}");
    }
    assert!(
        both.iter().chain(&failed).all(|l| !l.contains(" cause=")),
        "no case names a cause"
    );
}

/// P1'S GATE: a run expecting `partial` reads none; one expecting `bound` reads what the
/// checkout's step testdata shows now (not the lab's compiled table, which predates a new
/// measurement); one with no expectation reads the compiled `CODEX_SHOWN`.
#[test]
fn the_proof_reads_a_bound_builds_gate_from_the_checkout() {
    use aterm_agent::harness::codex_steps::CODEX_MEASURED_BUILDS;
    let measured = CODEX_MEASURED_BUILDS
        .last()
        .copied()
        .expect("a measured build");
    let scratch = aterm_tempfile::tempdir().expect("a scratch steps dir");
    let fresh = "0.999.0-dev.1+gabc";
    std::fs::write(
        scratch.path().join("codex-alab-0.999.0-dev.1-continue.jsonl"),
        format!(
            "# codex {fresh} · lab reduce\n{}\n",
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_attribution":{"turn_trigger":"alab_autopilot"}}}"#
        ),
    )
    .expect("the planted reduction");
    for build in [Some(fresh), Some(measured), None] {
        assert_eq!(gated_for(Some(false), build, scratch.path()), Gate::NONE);
        assert_eq!(gated_for(None, build, scratch.path()), Gate::of(build));
    }
    assert_eq!(
        gated_for(Some(true), Some(fresh), scratch.path()),
        Gate {
            exit: false,
            autopilot: true
        },
        "the checkout's new reduction, which no compiled table holds"
    );
    assert_eq!(Gate::of(Some(fresh)), Gate::NONE);
    assert_eq!(
        gated_for(Some(true), Some(measured), scratch.path()),
        Gate::NONE,
        "a build the scratch steps dir holds nothing of"
    );
    assert_eq!(gated_for(Some(true), None, scratch.path()), Gate::NONE);
}

#[test]
fn a_done_claim_is_met_only_by_the_defend_prompt_within_its_back_off() {
    let row = |t: i64, tag: &str, said: &str, hold: i64| {
        J::obj([
            ("t", J::Int(t)),
            ("tag", J::s(tag)),
            ("said", J::s(said)),
            ("hold_ms", J::Int(hold)),
            ("status", J::Int(200)),
        ])
    };
    let rows = vec![
        row(0, "first:person", model::FIRST_DONE, 0),
        row(120_000, "first:defend+repo", model::CRUISE_DEFENSE, 0),
    ];
    assert_eq!(
        claims_of("first", &rows, 200_000),
        Claims {
            met: 1,
            pending: 1,
            unmet: Vec::new()
        }
    );
    let late = claims_of("first", &rows, 120_000 + 240_000 + CLAIM_SLACK_MS + 1);
    assert_eq!(
        (late.met, late.pending, late.unmet.len()),
        (1, 0, 1),
        "{late:?}"
    );
    let other = vec![
        row(0, "first:person", model::FIRST_DONE, 0),
        row(120_000, "first:continue", "Kept going.", 0),
    ];
    assert_eq!(
        claims_of("first", &other, 130_000).unmet,
        vec!["first:person then first:continue".to_string()]
    );
    let worked = vec![
        row(0, "hold:person", model::STEP_ONE, 130_000),
        row(191_000, "hold:continue", model::CONTINUED_DONE, 0),
    ];
    let due = 191_000 + 120_000 + CLAIM_SLACK_MS;
    let c = claims_of("hold", &worked, due - 1);
    assert_eq!((c.pending, c.unmet.len()), (1, 0));
    assert_eq!(claims_of("hold", &worked, due + 1).unmet.len(), 1);
    let mut wall = row(0, "usage:limit", "", 0);
    wall.set("status", J::Int(429));
    let none = vec![
        wall,
        row(120_000, "usage:continue", "Resumed after the reset.", 0),
    ];
    assert_eq!(claims_of("usage", &none, i64::MAX), Claims::default());
    assert_eq!(
        (backoff_ms(0), backoff_ms(1), backoff_ms(3), backoff_ms(9)),
        (0, 120_000, 480_000, 3_600_000)
    );
    assert_eq!(tail("abcdef", 3), "def");
    assert_eq!(tail("ab", 3), "ab");
}

#[test]
fn a_legs_journal_is_the_file_its_config_logs_to_and_no_start_removes_it() {
    let home = Path::new("/s/legs/exchange/codex-home");
    let config = codex_config(
        1234,
        Path::new("/s/legs/exchange/proj"),
        home,
        CodexSandbox::WorkspaceWrite,
        "",
    );
    let dir = config
        .lines()
        .find_map(|l| l.strip_prefix("log_dir = "))
        .map(|v| v.trim_matches('"'));
    assert_eq!(
        dir.map(|d| Path::new(d).join("codex-tui.log")),
        Some(journal_path(home))
    );
    let removed = home.join("log").join("codex-tui.log");
    assert_ne!(journal_path(home), removed);
    assert_eq!(home.join("log").join("codex-tui.log"), removed);
}

/// A LEG'S CODEX RUNS UNDER THE SANDBOX ITS LANE CHOSE: workspace-write, codex's own, unless
/// the lane takes the jail's fence alone (danger-full-access), and exactly one mode either way.
#[test]
fn a_legs_config_names_the_sandbox_its_lane_chose() {
    let modes = |sandbox| {
        codex_config(1234, Path::new("/p"), Path::new("/h"), sandbox, "")
            .lines()
            .filter_map(|l| l.strip_prefix("sandbox_mode = "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(modes(CodexSandbox::WorkspaceWrite), ["\"workspace-write\""]);
    assert_eq!(modes(CodexSandbox::Jail), ["\"danger-full-access\""]);
}

#[test]
fn a_person_s_exchange_is_left_only_beside_a_control_that_was_continued() {
    let row = |t: i64, tag: &str, hold: i64| {
        J::obj([
            ("t", J::Int(t)),
            ("request_id", J::Int(t + 1)),
            ("mono_ms", J::Int(t)),
            ("reply_ms", J::Int(t + hold)),
            ("tag", J::s(tag)),
            ("hold_ms", J::Int(hold)),
            ("status", J::Int(200)),
        ])
    };
    let acts = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let reply = ms_of(SLOW_TURN);
    let control = vec![
        row(0, "control:person", reply),
        row(reply + 2_000, "control:continue", 0),
    ];
    assert_eq!(
        continued_after("control", &control, &acts(&["continue@v1"])),
        Ok(2_000)
    );
    assert!(continued_after("control", &control[..1], &[]).is_err());
    let defended = vec![
        row(0, "control:person", reply),
        row(reply + 2_000, "control:defend", 0),
    ];
    assert!(continued_after("control", &defended, &acts(&["continue-done-check@v1"])).is_err());
    let late = vec![
        row(0, "control:person", reply),
        row(reply + CONTROL_ACT_MS + 1, "control:continue", 0),
    ];
    assert!(continued_after("control", &late, &acts(&["continue@v1"])).is_err());
    assert!(continued_after("control", &control, &[]).is_err());
    assert!(continued_after("control", &[], &[]).is_err());
    let person = vec![row(0, "exchange:person", reply)];
    let seen = observe_reply(&person, "exchange:person", reply);
    let now = reply + 70_000;
    let held = left_to_person("exchange", &person, &[], seen, now, Some(2_000));
    assert!(
        held.as_ref().is_ok_and(|s| s.contains("70.0 s")),
        "{held:?}"
    );
    let typed = vec![
        row(0, "exchange:person", reply),
        row(reply + 2_000, "exchange:continue", 0),
    ];
    let why = left_to_person(
        "exchange",
        &typed,
        &acts(&["continue@v1"]),
        seen,
        now,
        Some(2_000),
    )
    .unwrap_err();
    assert!(
        why.contains("TYPED") && why.contains("exchange:continue"),
        "{why}"
    );
    assert!(
        left_to_person(
            "exchange",
            &person,
            &acts(&["answer@v1"]),
            seen,
            now,
            Some(2_000)
        )
        .is_err()
    );
    let left = acts(&[RULE_LEFT]);
    assert!(left_to_person("exchange", &person, &left, seen, now, Some(2_000)).is_ok());
    let after = acts(&[RULE_LEFT, "continue@v1"]);
    assert!(left_to_person("exchange", &person, &after, seen, now, Some(2_000)).is_err());
    let upgraded = acts(&[RULE_LEFT, RULE_UPGRADE]);
    assert!(left_to_person("exchange", &person, &upgraded, seen, now, Some(2_000)).is_ok());
    assert_eq!(
        typed_acts(&acts(&[RULE_UPGRADE, "continue@v1", RULE_LEFT])),
        acts(&["continue@v1"])
    );
    assert!(left_to_person("exchange", &person, &[], seen, reply + 59_000, Some(2_000)).is_err());
    assert!(left_to_person("exchange", &person, &[], seen, now, Some(75_000)).is_err());
    assert!(left_to_person("exchange", &person, &[], seen, now, None).is_err());
    assert!(left_to_person("exchange", &[], &[], seen, now, Some(2_000)).is_err());
    assert!(left_to_person("exchange", &person, &[], None, i64::MAX, Some(2_000)).is_err());
    let late_seen = observe_reply(&person, "exchange:person", now);
    assert!(left_to_person("exchange", &person, &[], late_seen, now, Some(2_000)).is_ok());
    assert!(
        left_to_person(
            "exchange",
            &person,
            &[],
            late_seen,
            reply + 59_999,
            Some(2_000)
        )
        .is_err()
    );
    let before_reply = vec![
        row(0, "control:person", reply),
        row(reply - 1, "control:continue", 0),
    ];
    assert!(continued_after("control", &before_reply, &acts(&["continue@v1"])).is_err());
    let mut no_reply = person.clone();
    if let J::Obj(fields) = &mut no_reply[0] {
        fields.retain(|(k, _)| k != "reply_ms");
    }
    assert!(person_reply("exchange", &no_reply).is_none());
    assert!(left_to_person("exchange", &no_reply, &[], seen, i64::MAX, Some(2_000)).is_err());
}

#[test]
fn the_journal_names_the_rule_and_status_the_hand() {
    let left = "2026-10-21T10:00:00.000Z DEBUG codex_alab_autopilot: 019a-thread: nothing (a \
                person's own exchange: theirs to answer)";
    assert_eq!(
        exchange_rule(left).as_deref(),
        Some("019a-thread: nothing (a person's own exchange: theirs to answer)")
    );
    assert!(
        exchange_rule("LEFT seq=4 rule=left-to-person@v1").is_some(),
        "aterm's id names it too"
    );
    let acted = "2026-10-21T10:00:00.000Z DEBUG codex_alab_autopilot: act continue@v1: typed \
                 \"keep going\" (continue after a plain turn; turn 7)\n\
                 2026-10-21T10:02:00.000Z DEBUG codex_alab_autopilot: 019a: wait 120s for \
                 continue-done-check@v1 (a done claim)";
    assert_eq!(exchange_rule(acted), None);
    assert_eq!(journal_typed(acted), vec!["continue@v1".to_string()]);
    assert!(journal_typed(left).is_empty());
    assert!(journal_typed("act landed@v1 not recorded: no proven claim is held").is_empty());
    assert!(journal_typed("exact continue@v1: typed").is_empty());
    assert!(journal_typed("the act after it: typed").is_empty());
    assert_eq!(
        person_hand("OK hold=0 human_ms=412 human_seq=3 task=-"),
        (Some(412), 3)
    );
    assert_eq!(
        person_hand("OK hold=0 human_ms=- human_seq=0"),
        (None, 0),
        "a control write stamps none"
    );
    assert_eq!(person_hand("OK hold=0"), (None, 0));
    assert_eq!(task_face("OK x=1 task=theirs pause=-"), "theirs");
}

#[test]
fn the_persons_enter_is_judged_past_the_sends_own_stamp() {
    let typed = |before: &str, sent: &str, after: &str| Typed {
        pressed_ms: 0,
        before: format!("OK hold=0 {before} task=open"),
        sent: format!("OK hold=0 {sent} task=open"),
        after: format!("OK hold=0 {after} task=open"),
    };
    let theirs = typed(
        "human_ms=- human_seq=0",
        "human_ms=40 human_seq=1",
        "human_ms=3 human_seq=2",
    );
    assert_eq!(theirs.enter_stamped(), (true, [0, 1, 2]));
    assert!(!theirs.stamped_nothing());
    let unstamped = typed(
        "human_ms=- human_seq=0",
        "human_ms=40 human_seq=1",
        "human_ms=95 human_seq=1",
    );
    assert_eq!(
        unstamped.enter_stamped(),
        (false, [0, 1, 1]),
        "the send's stamp is no Enter's"
    );
    let ours = typed(
        "human_ms=- human_seq=0",
        "human_ms=- human_seq=0",
        "human_ms=- human_seq=0",
    );
    assert!(ours.stamped_nothing());
    assert_eq!(ours.enter_stamped(), (false, [0, 0, 0]));
    let stamped = typed(
        "human_ms=- human_seq=0",
        "human_ms=- human_seq=0",
        "human_ms=2 human_seq=1",
    );
    assert!(
        !stamped.stamped_nothing(),
        "a control write that stamped one"
    );
}

#[test]
fn a_failed_reply_is_no_reading() {
    let reply = |code: i32, stdout: &str, stderr: &str| instance::Reply {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    };
    let line = "OK hold=0 human_ms=40 human_seq=1 task=open";
    assert_eq!(
        line_reading("@s status", reply(0, &format!("{line}\n"), "")),
        Ok(line.to_string())
    );
    for failed in [
        reply(1, "", "aterm-ctl: ERR no session s"),
        reply(0, "ERR no session s\n", ""),
        reply(124, "", ""),
        reply(0, "", ""),
    ] {
        assert!(
            line_reading("@s status", failed.clone()).is_err(),
            "a failed reply stood in for a status line: {failed:?}"
        );
        assert_eq!(
            person_hand(&failed.first()),
            (None, 0),
            "which would read as no stamp"
        );
    }
    let why = reading("@s timeline", reply(1, "", "ERR no session s"))
        .err()
        .unwrap_or_default();
    assert!(
        why.contains("ERR no session s"),
        "the failed reply is said, not read: {why}"
    );
    assert!(
        reading("@s timeline", reply(0, "", "")).is_ok(),
        "a timeline with no rows is a reading"
    );
}

#[test]
fn no_land_leg_drops_out_of_the_past_upgrade_check() {
    let leg = |name: &'static str| Leg {
        name,
        sid: format!("s-{name}"),
        home: PathBuf::from("/nonexistent/home"),
        auth_path: None,
        proj: PathBuf::from("/nonexistent/proj"),
    };
    let (land, reland, repo) = (leg("land"), leg("reland"), leg("repo"));
    let read = landed_baselines(&[&land, &reland, &repo], |leg| {
        Ok(if leg.name == "reland" {
            Err("status task=-; proven: None".to_string())
        } else {
            Ok(leg.name.len())
        })
    });
    let Ok(read) = read else {
        panic!("no stop was asked for");
    };
    let names: Vec<&str> = read.iter().map(|(leg, _)| leg.name).collect();
    assert_eq!(names, ["land", "reland", "repo"], "none left out");
    assert_eq!(read[0].1, Ok(4));
    assert_eq!(read[1].1, Err("status task=-; proven: None".to_string()));
    assert_eq!(read[2].1, Ok(4));
    let stopped = landed_baselines(&[&land, &reland], |_| {
        Err::<Result<(), String>, _>(Stopped(2))
    });
    assert!(stopped.is_err(), "a stop ends the read");
}

#[test]
fn a_resumed_images_first_decision_is_read_after_its_resume() {
    let before = "2026-10-21T10:00:00.000Z DEBUG codex_alab_autopilot: 019a: resumed \
                  (task=none, relaunched=false, owed=none)\n\
                  2026-10-21T10:00:01.000Z DEBUG codex_alab_autopilot: 019a: nothing (a \
                  person's own exchange: theirs to answer)\n\
                  2026-10-21T10:00:02.000Z DEBUG codex_core: 019a: resumed (task=x)\n\
                  2026-10-21T10:09:00.000Z DEBUG codex_alab_autopilot: act upgrade@v1: \
                  re-exec onto a new build at rest\n";
    assert_eq!(resumes(before), 1, "codex_core's line is no resume");
    assert_eq!(decided_after_resume(before, 1), None, "not resumed yet");
    assert_eq!(
        decided_after_resume(before, 0).as_deref(),
        Some("019a: nothing (a person's own exchange: theirs to answer)"),
        "the image before's own decision"
    );
    let after = format!(
        "{before}2026-10-21T10:09:03.000Z DEBUG codex_alab_autopilot: 019a: resumed \
         (task=open, relaunched=false, owed=none)\n\
         2026-10-21T10:09:03.100Z DEBUG codex_alab_autopilot: 019a: passive (no proven \
         claim)\n"
    );
    assert_eq!(resumes(&after), 2);
    assert_eq!(
        decided_after_resume(&after, 1),
        None,
        "a passive image decided nothing"
    );
    let left = format!(
        "{after}2026-10-21T10:09:04.000Z DEBUG codex_alab_autopilot: 019a: nothing (a \
         person's own exchange: theirs to answer)\n"
    );
    assert_eq!(
        decided_after_resume(&left, 1).as_deref(),
        Some("019a: nothing (a person's own exchange: theirs to answer)")
    );
    let owed = format!(
        "{after}2026-10-21T10:09:04.000Z DEBUG codex_alab_autopilot: 019a: wait 120s for \
         continue@v1 (a plain turn)\n"
    );
    assert_eq!(
        decided_after_resume(&owed, 1).as_deref(),
        Some("019a: wait 120s for continue@v1 (a plain turn)")
    );
    let typed = format!(
        "{after}2026-10-21T10:09:04.000Z DEBUG codex_alab_autopilot: act continue@v1: typed \
         \"keep going\" (continue after a plain turn; turn 9)\n"
    );
    assert!(
        decided_after_resume(&typed, 1).is_some_and(|d| d.starts_with("act continue@v1")),
        "an act typed is a decision"
    );
    assert_eq!(decided_after_resume(&left, 2), None);
}

#[test]
fn a_package_without_the_alab_mark_is_a_vendor_codex() {
    let scratch = aterm_tempfile::tempdir().expect("exclusive package fixture");
    let dir = scratch.path();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/codex"), "").unwrap();
    std::fs::write(
        dir.join("codex-package.json"),
        r#"{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    let bin = dir.join("bin/codex").to_string_lossy().into_owned();
    let vendor = package(Some(&bin), "/nonexistent").unwrap_err();
    assert!(vendor.contains("vendor Codex"), "{vendor}");
    std::fs::write(dir.join("alab-build.json"), "{}").unwrap();
    let root = package(Some(&bin), "/nonexistent").unwrap();
    assert_eq!(root, std::fs::canonicalize(dir).unwrap());
    assert!(package(Some("/nonexistent/codex"), "/nonexistent").is_err());
}

struct Script {
    word: &'static str,
    asks: std::cell::Cell<u64>,
    looked: std::cell::Cell<bool>,
    late: Option<u64>,
    stuck: bool,
    log: std::cell::RefCell<Vec<String>>,
}

const LATE_TURN: &str =
    "12 open t1 turn par=r1 vref=turn-1 src=transcript conf=exact t=1760000001000";

/// A turn a partial rollout opened where no claim's turn ran (fix E1).
const MISSED_TURN: &str =
    "11 open t2 turn par=r1 vref=turn-2 src=transcript conf=strong t=1760000005000";

/// A leg whose claims opened no turn, its partial rollout opening each (fix E1), as the live
/// first leg's two turns (39 ms and 8 ms) did.
const ROLLOUT_OPENED: [&str; 3] = [
    "22 open t1 turn par=r2 vref=turn-1 src=transcript conf=strong t=1760000000000",
    "24 close t1 turn out=done dur_ms=39 by=vendor src=transcript conf=strong t=1760000000039",
    "25 amend t1 turn vdur_ms=132 tok_in=1 tok_out=1 by=vendor src=transcript conf=strong t=1760000000039",
];

/// A turn a build nobody measured logs: its claim's, which its rollout names.
const CLAIMED: [&str; 2] = [
    "9 open t1 turn par=r1 src=engine conf=claim t=1760000000000",
    "10 amend t1 turn vref=turn-1 src=transcript conf=strong t=1760000000100",
];

impl Script {
    fn new(word: &'static str, late: Option<u64>) -> Script {
        Script {
            word,
            asks: std::cell::Cell::new(40),
            looked: std::cell::Cell::new(false),
            late,
            stuck: false,
            log: std::cell::RefCell::new(vec![
                "9 open t1 turn par=r1 src=screen conf=strong t=1760000000000".to_string(),
            ]),
        }
    }

    /// [`Script::new`] whose log holds the [`CLAIMED`] turn in place of the screen's.
    fn claimed(word: &'static str, late: Option<u64>) -> Script {
        let script = Script::new(word, late);
        *script.log.borrow_mut() = CLAIMED.map(str::to_string).to_vec();
        script
    }

    fn look(&self) -> Option<TapSaid> {
        if self.looked.replace(true) && !self.stuck {
            let next = self.asks.get() + 1;
            if self.late == Some(next) {
                self.log.borrow_mut().push(LATE_TURN.to_string());
            }
            self.asks.set(next);
        }
        tap_said(&format!(
            "OK schema=1 need=- need_since_ms=- rollout={} rollout_asks={} editing=0",
            self.word,
            self.asks.get()
        ))
    }

    fn read(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

fn ten_looks(ok: &mut dyn FnMut() -> bool) -> Result<bool, Stopped> {
    Ok((0..10).any(|_| ok()))
}

#[test]
fn the_rollout_leg_reads_past_the_taps_fence() {
    let verdict = |measured: bool, script: &Script| match past_the_fence(
        || script.look(),
        || script.read(),
        ten_looks,
    )
    .expect("not stopped")
    {
        Fenced::Past { from, to, got } => {
            assert_eq!(to.asks, from.asks + FENCE_ASKS, "read at the fence");
            Some(rollout_holds(measured, &to, &got))
        }
        Fenced::NoTap | Fenced::Stuck { .. } => None,
    };
    assert_eq!(
        verdict(false, &Script::claimed("partial", None)),
        Some(true)
    );
    assert_eq!(
        verdict(false, &Script::new("partial", None)),
        Some(false),
        "NEGATIVE CONTROL: a log the rollout reached no turn of is the gap, never a pass"
    );
    assert_eq!(
        verdict(false, &Script::claimed("unmeasured", None)),
        Some(false),
        "an unmeasured build is read partial"
    );
    let late = Script::claimed("partial", Some(42));
    assert_eq!(verdict(false, &late), Some(false));
    assert!(late.read().iter().any(|l| l == LATE_TURN));
    let said = |word: &str| TapSaid {
        word: word.to_string(),
        asks: 42,
    };
    let lines = |ls: &[&str]| ls.iter().map(|l| (*l).to_string()).collect::<Vec<_>>();
    assert!(rollout_holds(false, &said("partial"), &lines(&CLAIMED)));
    assert!(
        !rollout_holds(false, &said("partial"), &lines(&CLAIMED[..1])),
        "a claim's turn the rollout amends nothing of: the rollout reached no turn"
    );
    assert!(
        !rollout_holds(false, &said("partial"), &lines(&CLAIMED[1..])),
        "an amend of a turn the log never opened"
    );
    let screen = [
        "9 open t1 turn par=r1 src=screen conf=strong t=1760000000000",
        CLAIMED[1],
    ];
    assert!(
        !rollout_holds(false, &said("partial"), &lines(&screen)),
        "NEGATIVE CONTROL: a turn neither a claim nor the rollout opened, though the rollout \
         names it"
    );
    assert!(
        rollout_holds(false, &said("partial"), &lines(&ROLLOUT_OPENED)),
        "a leg whose every turn the partial rollout opened, none its claims', is no gap"
    );
    assert!(
        !rollout_holds(true, &said("bound"), &lines(&CLAIMED[1..])),
        "an amend is no turn the rollout opened or closed"
    );
    let missed = [CLAIMED[0], CLAIMED[1], MISSED_TURN];
    assert!(
        rollout_holds(false, &said("partial"), &lines(&missed)),
        "a turn the claims missed, the partial rollout's own (fix E1), is no exact read"
    );
    assert!(
        !rollout_holds(false, &said("partial"), &lines(&[CLAIMED[0], LATE_TURN])),
        "NEGATIVE CONTROL: a rollout turn read exact on a build nobody measured"
    );
    let at_once = Script::claimed("partial", Some(42));
    let first = at_once.look().expect("an answer");
    assert!(rollout_holds(false, &first, &at_once.read()));
    let mut stuck = Script::new("partial", None);
    stuck.stuck = true;
    let fenced = past_the_fence(|| stuck.look(), || stuck.read(), ten_looks).expect("ran");
    assert!(
        matches!(fenced, Fenced::Stuck { ref from, ref to } if from.asks == 40 && to.asks == 40),
        "{fenced:?}"
    );
    assert_eq!(verdict(false, &Script::new("bound", None)), Some(false));
    assert_eq!(
        past_the_fence(|| None, Vec::<String>::new, ten_looks).expect("ran"),
        Fenced::NoTap
    );
    assert_eq!(tap_said("OK rollout=bound rollout_asks=x"), None);
    assert_eq!(verdict(true, &Script::new("bound", Some(41))), Some(true));
    assert_eq!(verdict(true, &Script::new("bound", None)), Some(false));
    assert_eq!(
        verdict(true, &Script::new("partial", Some(41))),
        Some(false)
    );
}

#[test]
fn the_rollout_leg_expects_what_it_is_told_over_the_compiled_list() {
    let listed = ["0.165.0-dev.13594+g870dabaa6"];
    let other = "0.165.0-dev.13586+g133286d36";
    assert!(expects_bound(None, Some(listed[0]), &listed));
    assert!(!expects_bound(None, Some(other), &listed));
    assert!(
        !expects_bound(None, Some("0.165.0-dev.13594"), &listed),
        "matched whole"
    );
    assert!(!expects_bound(None, None, &listed));
    for build in [Some(listed[0]), Some(other), None] {
        assert!(expects_bound(Some(true), build, &listed), "{build:?}");
        assert!(!expects_bound(Some(false), build, &listed), "{build:?}");
    }
    assert!(matches!(expect_rollout(None), Ok(None)));
    assert!(matches!(expect_rollout(Some("bound")), Ok(Some(true))));
    assert!(matches!(expect_rollout(Some("partial")), Ok(Some(false))));
    let f = expect_rollout(Some("measured")).expect_err("no such word");
    assert_eq!(f.code, 2);
    assert!(
        f.msg
            .as_deref()
            .is_some_and(|m| m.contains("not 'measured'")),
        "{:?}",
        f.msg
    );
}

/// ONE LANE A RUN: each `--*-only` flag alone narrows the run to its lane and none runs the
/// full proof; any two together are refused, `--capacity-only` beside `--terminals-only`,
/// `--facts-only` or `--window-only` as beside the lanes its first port knew. Only a run that
/// hands off takes `--upgrade-package`, and `--capacity-only`, which starts no aterm and has
/// no rollout leg, takes neither `--aterm` nor `--expect-rollout`.
#[test]
fn a_run_takes_one_lane_and_only_the_options_that_lane_reads() {
    let run = |words: &[&str]| asked(&words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>());
    let usage = |words: &[&str]| {
        run(words).map_or_else(|f| f.code, |got| panic!("{words:?} was taken: {got:?}"))
    };
    assert_eq!(run(&[]).expect("the full proof").lane, Lane::Full);
    for (flag, lane) in ONLY {
        assert_eq!(run(&[flag]).expect(flag).lane, lane, "{flag}");
        for (other, _) in ONLY.into_iter().filter(|(o, _)| *o != flag) {
            assert_eq!(usage(&[flag, other]), 2, "{flag} {other}");
        }
    }
    for lane in ["--upgrade-only", "--raw-upgrade-only"] {
        assert!(run(&[lane, "--upgrade-package", "b"]).is_ok(), "{lane}");
    }
    assert!(
        run(&["--upgrade-package", "b"]).is_ok(),
        "the full proof hands off"
    );
    for lane in [
        "--policy-only",
        "--terminals-only",
        "--facts-only",
        "--restart-only",
        "--window-only",
        "--capacity-only",
    ] {
        assert_eq!(usage(&[lane, "--upgrade-package", "b"]), 2, "{lane}");
    }
    assert_eq!(usage(&["--capacity-only", "--aterm", "/a"]), 2);
    assert_eq!(usage(&["--capacity-only", "--expect-rollout", "bound"]), 2);
    let alone = run(&["--capacity-only", "--package", "/p", "--keep"]).expect("its package");
    assert_eq!(
        alone,
        Asked {
            keep: true,
            lane: Lane::Capacity,
            aterm: None,
            package: Some("/p".to_string()),
            upgrade_package: None,
            expect: None,
        }
    );
    assert_eq!(usage(&["--capacity-only", "extra"]), 2);
}

/// THE RUN'S END (review F2, F4): the capacity lane runs alone and as the full proof's last
/// leg once its legs held, each of its checks counted among the run's (a failed one named,
/// the run then exit 1); never after a leg failed or was interrupted, nor in another lane;
/// an interrupt during it ends the run as one (128 + the signal), never PASS. A leg's other
/// `Err` ends the run unless checks failed, which are then its verdict.
#[test]
fn the_runs_end_runs_the_capacity_lane_last_and_counts_its_checks() {
    /// End a run in `lane` whose legs came back `legs` with `fails` failed and 5 held, the
    /// capacity lane answering `capacity`: its result, whether the lane ran, and the
    /// tally.
    fn end(
        lane: Lane,
        legs: Result<(), Fail>,
        fails: &[&str],
        capacity: Result<Vec<(String, bool)>, Stopped>,
    ) -> (Res, bool, usize, Vec<String>) {
        let ran = std::cell::Cell::new(false);
        let (mut oks, mut fails) = (5, fails.iter().map(ToString::to_string).collect());
        let res = settle(lane, legs, (&mut oks, &mut fails, &[]), || {
            ran.set(true);
            capacity
        });
        (res, ran.get(), oks, fails)
    }
    let checks = |held: &[bool]| -> Result<Vec<(String, bool)>, Stopped> {
        Ok(held
            .iter()
            .enumerate()
            .map(|(i, h)| (format!("capacity {i}"), *h))
            .collect())
    };

    let (res, ran, oks, fails) = end(Lane::Full, Ok(()), &[], checks(&[true, true]));
    assert!(ran && fails.is_empty());
    assert_eq!(
        (res.ok(), oks),
        (Some(0), 7),
        "the full proof counts its checks"
    );
    let (res, ran, oks, fails) = end(Lane::Full, Ok(()), &[], checks(&[true, false]));
    assert!(ran);
    assert_eq!((res.ok(), oks), (Some(1), 6));
    assert_eq!(fails, ["capacity 1"], "a failed check is named");
    let (res, ran, _, fails) = end(Lane::Capacity, Ok(()), &[], checks(&[false]));
    assert!(ran);
    assert_eq!((res.ok(), fails.len()), (Some(1), 1), "alone");
    let (res, ran, ..) = end(Lane::Capacity, Ok(()), &[], checks(&[true]));
    assert!(ran);
    assert_eq!(res.ok(), Some(0));

    let (res, ran, ..) = end(Lane::Full, Err(Fail::die("a leg")), &[], checks(&[true]));
    assert!(!ran, "after a leg that failed");
    assert_eq!(res.err().map(|f| (f.code, f.stopped)), Some((1, None)));
    let stopped = Fail::from(Stopped(libc::SIGINT));
    let (res, ran, ..) = end(
        Lane::Full,
        Err(stopped),
        &["a leg's check"],
        checks(&[true]),
    );
    assert!(!ran, "after an interrupted leg");
    assert_eq!(
        res.err().and_then(|f| f.stopped),
        Some(libc::SIGINT),
        "the interrupt wins over the failed checks"
    );
    let (res, ran, ..) = end(
        Lane::Full,
        Err(Fail::die("a leg")),
        &["a leg's check"],
        checks(&[true]),
    );
    assert!(!ran);
    assert_eq!(res.ok(), Some(1), "the failed checks are the verdict");
    for (flag, lane) in ONLY.into_iter().filter(|(_, l)| *l != Lane::Capacity) {
        let (res, ran, oks, _) = end(lane, Ok(()), &[], checks(&[false]));
        assert!(!ran, "{flag}");
        assert_eq!((res.ok(), oks), (Some(0), 5), "{flag}");
    }

    let (res, ran, ..) = end(Lane::Full, Ok(()), &[], Err(Stopped(libc::SIGINT)));
    assert!(ran);
    assert_eq!(
        res.err().map(|f| (f.code, f.stopped)),
        Some((128 + libc::SIGINT, Some(libc::SIGINT))),
        "an interrupt during the capacity lane ends the run as one"
    );
}

#[test]
fn a_proof_takes_the_lock_another_holds_only_once_it_is_let_go() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let path = tmp.path().join(PROOF_LOCK);
    let first = proof_lock(&path, Duration::ZERO).expect("free");
    let f = proof_lock(&path, Duration::ZERO).expect_err("held");
    assert_eq!(f.code, 3);
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.starts_with("NOT RUN: another codex-alab proof runs")
            && msg.contains(&path.display().to_string()),
        "{msg}"
    );
    drop(first);
    proof_lock(&path, Duration::ZERO).expect("let go");
}

/// A RIG WHOSE BUS CANNOT STAND LEAVES NOTHING BEHIND: an aterm whose broker refuses its
/// lifeline as one older than the lifeline does is NOT RUN with nothing copied into the world,
/// which goes with it; one whose broker listens stands the bus, its table named in the world's
/// `aterm.toml`, with nothing copied in yet. Each is a stand-in aterm whose `link mint`
/// answers every grant.
#[test]
fn a_rig_whose_bus_cannot_stand_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let aterm = |name: &str, broker: &str| {
        let path = tmp.path().join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncase \"$2\" in\nmint) echo \"$3 00\" ;;\nbroker) {broker} ;;\nesac\n"
            ),
        )
        .expect("the stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
        path
    };
    let old = aterm(
        "old",
        "echo 'aterm link broker: unknown flag `--lifeline-fd`' >&2; exit 2",
    );
    let world = World::new("").expect("a world");
    let dir = world.dir.clone();
    let f = stand_rig_bus(world, &old).err().expect("NOT RUN");
    assert_eq!(f.code, 3, "{:?}", f.msg);
    assert!(!dir.exists(), "{} is left", dir.display());
    let current = aterm("current", ": > \"$3\"; read x");
    let (world, fabric) =
        stand_rig_bus(World::new("").expect("a world"), &current).expect("the bus stands");
    let toml = std::fs::read_to_string(world.cfg.join("aterm/aterm.toml")).expect("its toml");
    assert!(toml.contains(&fabric.table), "{toml}");
    assert!(!world.dir.join("bin").exists(), "nothing copied in yet");
}

/// A LAB ACT A MODEL'S HOLD TIMES SAYS WHO WAS LATE: a lab that came and acted inside the hold
/// passes; one that came inside it and acted past it fails naming the act; one that came
/// already past it fails naming the lab's own earlier legs, never Land's repo logic.
#[test]
fn a_hold_timed_act_names_who_was_late() {
    let (ok, line) = hold_line(1_000, 1_500, 1_800, REPO_COMMIT_HOLD);
    assert!(ok, "{line}");
    assert_eq!(
        line,
        "0.8 s into the model's 60 s hold (the lab came 0.5 s after the model's nudge)"
    );
    let (ok, line) = hold_line(1_000, 900, 1_300, REPO_PUSH_HOLD);
    assert!(ok, "{line}");
    assert!(
        line.contains("the lab was waiting for the model's nudge"),
        "{line}"
    );

    let (ok, line) = hold_line(0, 10_000, 61_000, REPO_COMMIT_HOLD);
    assert!(!ok, "{line}");
    assert!(
        line.starts_with("61.0 s into the model's 60 s hold: the lab came 10.0 s after")
            && line.contains("the act itself took 1.0 s past it")
            && !line.contains("earlier leg")
            && !line.contains("Land"),
        "{line}"
    );

    let (ok, line) = hold_line(0, 182_000, 182_400, REPO_COMMIT_HOLD);
    assert!(!ok, "{line}");
    assert!(
        line.starts_with("182.4 s into the model's 60 s hold: the lab came 182.0 s after")
            && line.contains("held by an earlier leg of the lab's run")
            && line.contains("not Land's repo logic")
            && !line.contains("the act itself"),
        "{line}"
    );
    let (ok, line) = hold_line(0, 20_000, 20_100, REPO_PUSH_HOLD);
    assert!(!ok && line.contains("earlier leg"), "{line}");
}

/// THE LAB'S COMMIT IS TIMED TO THE MILLISECOND ([`committed_ms`]): one 60.4 s after the
/// model's nudge, whose own whole-second date reads 59.7 s, is past the 60 s hold, so
/// `repo_pushed`'s line can say why the next nudge names the untracked file; one 59.4 s after
/// is inside it. The commit's time is a hand-set stamp on `main`'s reflog.
#[test]
fn a_commit_under_a_second_past_the_hold_reads_past_it() {
    const T: i64 = 1_700_000_000_000;
    let root = aterm_tempfile::tempdir().unwrap();
    let proj = root.path();
    let git = |args: &[&str], date: Option<&str>| {
        let mut cmd = aterm_uds::fixture_git::git();
        cmd.args([
            "-c",
            "user.name=jsmith",
            "-c",
            "user.email=jsmith@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(proj)
        .stdin(Stdio::null());
        if let Some(date) = date {
            cmd.env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    git(&["init", "-q", "-b", "main"], None);
    std::fs::write(proj.join(NOTES), "# release notes\n").unwrap();
    git(&["add", NOTES], None);
    git(
        &["commit", "-q", "-m", "lab: the release notes"],
        Some("@1700000060 +0000"),
    );
    let (asked, made) = (T + 300, T + 60_700);
    std::fs::File::options()
        .append(true)
        .open(proj.join(".git/logs/refs/heads/main"))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(made.unsigned_abs()))
        .unwrap();
    let own: i64 = git(&["log", "-1", "--format=%ct"], None)
        .trim()
        .parse()
        .unwrap();
    assert!(
        own * 1000 - asked < ms_of(REPO_COMMIT_HOLD),
        "the commit's own date reads inside the hold"
    );
    assert_eq!(committed_ms(proj), Some(made));
    assert_eq!(committed_past(asked, proj, REPO_COMMIT_HOLD), Some(60_400));
    assert_eq!(
        committed_past(asked + 1_000, proj, REPO_COMMIT_HOLD),
        None,
        "59.4 s: inside"
    );
}
