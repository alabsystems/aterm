// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_agent_is_the_sessions_record_whose_pid_is_in_the_tree() {
    let outer = crate::TestScratch::new("sessions");
    let dir = outer.join("sessions");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("100.json"), r#"{"pid":100,"sessionId":"conv-a"}"#).unwrap();
    fs::write(dir.join("200.json"), r#"{"pid":200,"sessionId":"conv-b"}"#).unwrap();
    fs::write(dir.join("notes.txt"), "x").unwrap();
    assert_eq!(
        agents_of(&dir, &[200, 300]),
        vec![(200, "conv-b".to_string())]
    );
    assert!(agents_of(&dir, &[1]).is_empty());
    let link = outer.join("sessions.link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(agents_of(&link, &[200]).is_empty());
    assert_eq!(since(1000, 3350), "2.4 s");
}

#[test]
fn the_carry_on_step_is_read_off_a_host_line() {
    assert_eq!(
        carry_on_step("HOST seq=12 carry-on step=done:said-done"),
        Some(aterm_agent::harness::relaunch::CARRIED_DONE)
    );
    assert_eq!(
        carry_on_step("HOST seq=3 carry-on step=continued"),
        Some("continued")
    );
    assert_eq!(carry_on_step("HOST seq=3 carry-on step=wait:held"), None);
    assert_eq!(carry_on_step("HOST seq=3 upgrade step=announced:1"), None);
    assert_eq!(carry_on_step("DONE seq=4 carry-on step=continued"), None);
}

#[test]
fn the_carry_on_settles_past_another_sweeps_lock() {
    use aterm_agent::harness::relaunch::CARRIED_DONE;
    let lines = |words: &[&str]| -> Vec<String> {
        words
            .iter()
            .enumerate()
            .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
            .collect()
    };
    assert_eq!(
        carry_on_settled(&lines(&["busy:another-sweep", CARRIED_DONE])),
        Some(CARRIED_DONE)
    );
    assert_eq!(
        carry_on_settled(&lines(&["wait:resume", "busy:another-sweep", "continued"])),
        Some("continued")
    );
    for lone in ["busy:another-sweep", "wait:held", "wait:restore"] {
        assert_eq!(carry_on_step(&lines(&[lone])[0]), None, "{lone}");
        assert_eq!(
            carry_on_settled(&lines(&[lone])),
            None,
            "{lone}: not settled"
        );
    }
    assert_eq!(
        carry_on_settled(&lines(&["busy:state-unwritable"])),
        Some("busy:state-unwritable")
    );
    assert_eq!(carry_on_settled(&[]), None);
}

#[test]
fn a_failed_refused_or_untaught_carry_on_is_the_step() {
    let lines = |words: &[&str]| -> Vec<String> {
        words
            .iter()
            .enumerate()
            .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
            .collect()
    };
    for word in [
        "failed:no-resume",
        "failed:exited-before-continuing",
        "failed:signal-refused",
        "refused:one-shot",
        "refused:shell-gone",
        "typed:a-word-never-taught",
    ] {
        assert_eq!(carry_on_step(&lines(&[word])[0]), Some(word), "{word}");
        assert_eq!(
            carry_on_settled(&lines(&["busy:another-sweep", word, CONTINUED])),
            Some(word),
            "{word}: the step, not a later continued"
        );
    }
}

#[test]
fn the_land_verdicts_read_the_settled_word_on_a_tab_under_land() {
    use aterm_agent::harness::relaunch::CARRIED_DONE;
    let lines = |words: &[&str]| -> Vec<String> {
        words
            .iter()
            .enumerate()
            .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
            .collect()
    };
    let open =
        |words: &[&str], autopilot: &str| carry_on_judged(&lines(words), CONTINUED, autopilot);
    assert_eq!(open(&["busy:another-sweep", CONTINUED], "land"), Ok(()));
    assert_eq!(
        carry_on_judged(&lines(&["wait:resume", CARRIED_DONE]), CARRIED_DONE, "land"),
        Ok(())
    );
    for wrong in [
        &["failed:no-resume", CONTINUED][..],
        &["typed:a-word-never-taught", CONTINUED],
        &[CONTINUED, CARRIED_DONE],
        &[CARRIED_DONE],
        &["busy:another-sweep"],
        &[],
    ] {
        let said = open(wrong, "land").expect_err(&format!("{wrong:?}"));
        assert!(said.contains("carry-on settled on"), "{said}");
    }
    assert!(open(&[CONTINUED], "cruise").is_err(), "not under Land");
    assert!(open(&[CONTINUED], "?").is_err(), "no autopilot word");
    assert!(
        carry_on_judged(&lines(&[CONTINUED]), CARRIED_DONE, "land").is_err(),
        "--land settles on done:said-done"
    );
    assert!(
        carry_on_judged(&lines(&[CARRIED_DONE, CONTINUED]), CARRIED_DONE, "land").is_err(),
        "--land types nothing"
    );
    assert_eq!(land_proves_the_journal("land").map_err(|f| f.code), Err(3));
    for before in ["cruise", "off", "?"] {
        assert!(land_proves_the_journal(before).is_ok(), "{before}");
    }
}

#[test]
fn a_failed_reply_is_no_reading() {
    let reply = |code: i32, stdout: &str, stderr: &str| -> Result<Reply, String> {
        Ok(Reply {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    };
    let status = "OK schema=1 hold=0 supervisor=aterm-harness@1 autopilot=land task=open\n";
    assert_eq!(
        status_word_of(reply(0, status, ""), "autopilot"),
        Ok("land".to_string())
    );
    assert_eq!(
        status_word_of(reply(0, status, ""), "task"),
        Ok("open".to_string())
    );
    for failed in [
        reply(1, "", "aterm-ctl: ERR no such session autopilot=land"),
        reply(1, status, ""),
        reply(0, "ERR no such session autopilot=land\n", ""),
        reply(0, "", ""),
        reply(0, "OK schema=1 hold=0 subject=autopilot%3Dland\n", ""),
        Err("could not run aterm: No such file or directory".to_string()),
    ] {
        let why = status_word_of(failed, "autopilot").expect_err("failed");
        assert!(why.starts_with("`status` "), "{why}");
    }
    let why = answered(
        "harness ledger @s-1",
        reply(1, "", "aterm harness ledger: denied\n"),
    )
    .expect_err("exit 1");
    assert_eq!(
        why,
        "`harness ledger @s-1` exited 1: aterm harness ledger: denied"
    );
    let body = answered("text", reply(0, "ERR at the top row\n", ""));
    assert_eq!(
        body.map(|r| r.stdout),
        Ok("ERR at the top row\n".to_string()),
        "a body's first line is data, not a verdict"
    );
}

#[test]
fn the_cruise_controls_read_a_settled_continued() {
    use aterm_agent::harness::relaunch::CARRIED_DONE;
    let lines = |words: &[&str]| -> Vec<String> {
        words
            .iter()
            .enumerate()
            .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
            .collect()
    };
    let plain = cruise_step(false);
    let cleared = cruise_step(true);
    assert_eq!((plain, cleared), (CONTINUED, "done:taskless"));
    let cruise =
        |words: &[&str], typed: usize| cruise_judged(&lines(words), plain, typed, "cruise");
    assert_eq!(cruise(&["continued", "done"], 1), Ok(()));
    assert_eq!(cruise(&["busy:another-sweep", "continued"], 1), Ok(()));
    for (wrong, typed) in [
        (&[CARRIED_DONE][..], 0),
        (&["busy:another-sweep", CARRIED_DONE], 0),
        (&["continued", CARRIED_DONE], 1),
        (&["done:taskless"], 0),
        (&["failed:no-resume", "continued"], 1),
        (&["busy:another-sweep"], 0),
        (&[], 0),
        (&["continued"], 0),
    ] {
        let said = cruise(wrong, typed).expect_err(&format!("{wrong:?} typed {typed}"));
        assert!(said.contains("carry-on settled on"), "{said}");
    }
    assert!(
        cruise_judged(&lines(&["continued"]), plain, 1, "land").is_err(),
        "not under Cruise"
    );
    assert!(
        cruise_judged(&lines(&["continued"]), plain, 1, "?").is_err(),
        "no autopilot word"
    );
    assert_eq!(
        cruise_judged(
            &lines(&["done:taskless", "done:lost"]),
            cleared,
            0,
            "cruise"
        ),
        Ok(())
    );
    assert!(cruise_judged(&lines(&["continued"]), cleared, 1, "cruise").is_err());
    assert!(cruise_judged(&lines(&["done:taskless"]), cleared, 1, "cruise").is_err());
    assert_eq!(cruise_is_the_control("land").map_err(|f| f.code), Err(3));
    assert_eq!(cruise_is_the_control("off").map_err(|f| f.code), Err(3));
    assert_eq!(cruise_is_the_control("?").map_err(|f| f.code), Err(3));
    assert!(cruise_is_the_control("cruise").is_ok());
    assert_eq!(
        continued_typed(&[
            "relaunched  x".to_string(),
            "continued  the continuation is typed".to_string(),
            "continued:check  y".to_string(),
            "done  z".to_string(),
        ]),
        2
    );
}

#[test]
fn the_owners_word_is_found_on_the_tabs_leaf_of_the_live_journal_or_fails() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("lab-owner-word-")
        .tempdir()
        .expect("exclusive scratch");
    let root = scratch.path().canonicalize().expect("physical scratch");
    let run = Run {
        w: root.clone(),
        instance_pid: 4242,
        ..Run::default()
    };
    let state = run.state().join("aterm");
    fs::create_dir_all(&state).unwrap();
    let journal = |sid: &str, words: &str| {
        format!(
            "schema = 2\n\n[[windows]]\nrows = 50\ncols = 144\n\n[[windows.restored_tabs]]\n\
             zoomed = false\n\n[windows.restored_tabs.root]\nnode = \"leaf\"\n\n\
             [windows.restored_tabs.root.view]\nkind = \"terminal\"\nlocal_id = 0\n\
             sid = \"{sid}\"\n{words}\n\n[windows.restored_tabs.root.view.agent]\n\
             tab = \"s-tab\"\n\n[journal]\npid = 4242\n"
        )
    };
    let land = "autopilot = \"land\"";
    let held = "holds = [\"local review\"]";
    let found = |on_leaf| journal_carries_within(&run, "s-tab", "the word", on_leaf, now_ms(), 1);
    let failed = |r: Result<(), Fail>| {
        let f = r.expect_err("not carried");
        assert_eq!(f.code, 1);
        assert!(
            f.msg
                .unwrap_or_default()
                .contains("FAIL: the crash journal never carried the word on tab s-tab"),
        );
    };

    fs::write(state.join("session.toml"), journal("s-tab", land)).unwrap();
    fs::write(state.join("journal-99-1.toml"), journal("s-tab", land)).unwrap();
    fs::write(state.join("journal-4242-1.toml"), journal("s-other", land)).unwrap();
    failed(found(leaf_under_land));
    assert!(!leaf_carries(&journal("s-tab", land), "s-tab", leaf_held));
    assert!(!leaf_carries("no toml [", "s-tab", leaf_under_land));

    fs::write(
        state.join("journal-4242-1.toml"),
        journal("s-tab", &format!("{land}\n{held}")),
    )
    .unwrap();
    assert!(found(leaf_under_land).is_ok());
    assert!(found(leaf_held).is_ok());
    assert!(own_journal_carries(&run, "s-tab", leaf_held).is_some());
}

#[test]
fn a_restore_takes_claude_codes_own_drives_and_refuses_the_retired_codex() {
    let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let asked = |s: &str| parse_args(&words(s)).map_err(|f| (f.code, f.msg));
    assert!(asked("crash --clear").unwrap().unwrap().clear);
    assert_eq!(
        asked("quit --land").unwrap().unwrap().land,
        Some(Land::Landed)
    );
    assert_eq!(
        asked("quit --land-open").unwrap().unwrap().land,
        Some(Land::Open)
    );
    assert!(asked("t crash --hold").unwrap().unwrap().hold);
    assert!(asked("quit --hold").is_err(), "--hold takes crash");
    let (code, msg) = asked("crash --codex").unwrap_err();
    assert_eq!(code, 2);
    assert!(msg.unwrap_or_default().contains("unknown option '--codex'"));
    assert!(asked("--keep").is_err(), "no quit or crash");
    assert_eq!(asked("--help").unwrap(), None);
}

#[test]
fn a_cleared_conversation_takes_no_land_run() {
    let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let asked = |s: &str| parse_args(&words(s)).map_err(|f| (f.code, f.msg));
    for refused in [
        "crash --clear --land",
        "quit --clear --land",
        "crash --clear --land-open",
        "quit --land-open --clear",
    ] {
        let (code, msg) = asked(refused).unwrap_err();
        assert_eq!(code, 2, "{refused}");
        assert!(
            msg.unwrap_or_default()
                .contains("--clear takes no --land or --land-open"),
            "{refused}"
        );
    }
    assert!(asked("quit --clear").unwrap().unwrap().clear);
    let held = asked("crash --clear --hold").unwrap().unwrap();
    assert!(held.clear && held.hold);
    assert_eq!(
        asked("crash --land-open").unwrap().unwrap().land,
        Some(Land::Open)
    );
    assert_eq!(
        asked("crash --land").unwrap().unwrap().land,
        Some(Land::Landed)
    );
}

#[test]
fn the_lift_is_read_off_the_held_tabs_record() {
    let messages = "message 3 at=1 ago_ms=1 tag=harness sev=info origin=host state=recorded \
                    rep=1 key=harness.restored-held.s-1 title=Tab%201:%20old detail=x \
                    actions=-\n\
                    message 7 at=2 ago_ms=1 tag=harness sev=info origin=host state=recorded \
                    rep=1 key=harness.restored-held.s-1 \
                    title=Tab%201:%20Claude%20was%20not%20restarted \
                    detail=To%20pick%20it%20up:%0Aclaude%20--resume%20c-1%0AYour%20hold%20came\
                    %20back:%20lift%20it%20with%20the%20menu,%20or%20aterm%20ctl%20hold%20s-1\
                    %20off,%20when%20drivers%20may%20type%20there%20again%25 actions=-";
    let (title, lines) = message_keyed(messages, "harness.restored-held.s-1").unwrap();
    assert_eq!(
        title, "Tab 1: Claude was not restarted",
        "the newest row of the key"
    );
    assert_eq!(lines[1], "claude --resume c-1");
    assert!(
        lines[2].ends_with("again%"),
        "a decoded %25: {:?}",
        lines[2]
    );
    assert_eq!(
        lift_named(&lines),
        Some(vec![
            "hold".to_string(),
            "s-1".to_string(),
            "off".to_string()
        ])
    );
    assert_eq!(message_keyed(messages, "harness.restored-held.s-2"), None);
    assert_eq!(lift_named(&["lift it with the menu".to_string()]), None);
    assert_eq!(pct_decode("%4"), "%4", "a % without two hex digits stays");
}

#[test]
fn the_land_drills_stub_answer_is_a_clean_defense() {
    use aterm_agent::supervise::policy::turn_end::clean_defense;
    assert!(clean_defense(Some(LANDED_TEXT)), "{LANDED_TEXT}");
    assert!(!clean_defense(Some(
        "Committed and pushed the lab's work; nothing is left to do.\nATERM-TASK-DONE"
    )));
}

#[test]
fn the_land_drills_stub_answers_the_land_check_with_the_word_that_lands() {
    use aterm_agent::supervise::policy::land_check::LAND_CHECK;
    let mode = landed_mode();
    let reply = |last: &str| {
        crate::stub::scripted_reply(&mode, &["the task".to_string(), last.to_string()])
            .and_then(|rule| rule.get("text"))
            .and_then(J::as_str)
            .map(str::to_string)
    };
    assert_eq!(reply(LAND_CHECK).as_deref(), Some(LANDED));
    assert_ne!(reply("Land it").as_deref(), Some(LANDED));
}
