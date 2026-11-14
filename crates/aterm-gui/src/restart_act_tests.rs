// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_changed_primary_cannot_publish_or_offer_an_older_root_restart() {
    use aterm_agent::harness::{relaunch, self_claimed};
    let sid = "s-primary-restart-switch";
    let primary = "00000000-1111-2222-3333-444455556666";
    let next = "00000001-1111-2222-3333-444455556666";
    let resume = |thread: &str| self_claimed::Resume {
        argv: vec![
            "/opt/codex/bin/codex".to_string(),
            "resume".to_string(),
            thread.to_string(),
        ],
        session: thread.to_string(),
        codex_home: None,
        codex_auth_path: None,
        cwd: Some("/w".to_string()),
    };
    let copy = |thread: &str| self_claimed::Copy {
        pid: 0x3fff_ff69,
        lifetime: None,
        start: "same process".to_string(),
        home: std::path::PathBuf::from("/private/tmp/codex"),
        seen: self_claimed::Seen::Never,
        resume: Ok(resume(thread)),
    };
    let initial = copy(primary);
    let mut snap =
        relaunch::snapshot_of_copy(sid, &initial, initial.resume.as_ref().unwrap(), 42, None);
    crate::self_claim::set_copy_for_test(sid, Some(initial));
    crate::harness_host::publish_snapshot_for_test(sid, Some(&snap));
    assert!(
        offered_for(sid, snap.pid),
        "the exact current primary is offered"
    );
    crate::self_claim::set_copy_for_test(sid, Some(copy(next)));
    assert!(
        !offered_for(sid, snap.pid),
        "a root change is fenced before a later snapshot is published"
    );
    crate::harness_host::publish_snapshot_for_test(sid, Some(&snap));
    assert!(
        crate::harness_host::published_snapshot(sid)
            .as_ref()
            .and_then(relaunch::self_claimed_line)
            .is_none(),
        "publication cannot graft the new root line onto the old snapshot"
    );
    assert!(!offered_for(sid, snap.pid));
    snap.session = Some(next.to_string());
    snap.codex_mut().unwrap().self_claimed = Some(resume(next));
    crate::harness_host::publish_snapshot_for_test(sid, Some(&snap));
    assert!(
        offered_for(sid, snap.pid),
        "a fresh snapshot bound to the new root is offered"
    );
    crate::harness_host::publish_snapshot_for_test(sid, None);
    crate::self_claim::forget(sid);
}

#[test]
fn a_scoped_relaunch_claim_gets_a_restart_event_not_a_signal() {
    let ctx = crate::session_store::test_handle_on(4242, -1).ctx;
    crate::session_timeline::scoped_input_tests::install(
        &ctx,
        71,
        &[aterm_agent::harness::contract::Scope::Relaunch],
    );
    assert!(scoped_relaunch(&ctx, 4242));
    assert!(!scoped_relaunch(&ctx, 4243));
    assert_eq!(
        offered(&ctx, None, crate::metrics::now_us()),
        Some(Offer {
            pid: 4242,
            promised: true,
        }),
        "an automatic restart needs no built-in vendor snapshot"
    );
    record_restart(&ctx, Cause::Asked);
    let events = ctx.timeline.lock().unwrap().view_copy(None, 0).rows();
    let event = events.last().expect("the restart event");
    assert_eq!(event.kind, RESTART_EVENT);
    assert_eq!(event.payload.as_ref(), "cause=asked");
}

#[test]
fn a_scoped_account_event_carries_only_the_row_and_its_home() {
    let ctx = crate::session_store::test_handle_on(4242, -1).ctx;
    crate::session_timeline::scoped_input_tests::install(
        &ctx,
        71,
        &[aterm_agent::harness::contract::Scope::Relaunch],
    );
    let env = aterm_agent::harness::accounts_pool::LoginEnv {
        label: "work".to_string(),
        var: "KIMI_CODE_HOME",
        value: Some(crate::test_host::abs("/tmp/account home")),
    };
    assert!(record_scoped_account(&ctx, "account", &env));
    let events = ctx.timeline.lock().unwrap().view_copy(None, 0).rows();
    let event = events.last().expect("the account event");
    assert_eq!(event.kind, RESTART_EVENT);
    assert_eq!(
        event.payload.as_ref(),
        format!(
            "cause=account to=work home={}",
            crate::test_host::abs("/tmp/account%20home")
        )
    );
    assert!(!record_scoped_account(&ctx, "asked", &env));
    assert!(!record_scoped_account(
        &ctx,
        "account",
        &aterm_agent::harness::accounts_pool::LoginEnv {
            value: Some("relative".to_string()),
            ..env
        }
    ));
}

#[test]
fn the_restart_verb_parses_its_three_arguments() {
    assert_eq!(parse(""), Ok(Ask::default()));
    assert_eq!(
        parse("pid=4242 if-stalled quiet=120"),
        Ok(Ask {
            pid: Some(4242),
            if_stalled: true,
            quiet: Some(120),
            account: None,
        })
    );
    assert_eq!(
        parse("quiet=0 pid=7"),
        Ok(Ask {
            pid: Some(7),
            if_stalled: false,
            quiet: Some(0),
            account: None,
        })
    );
    for bad in [
        "pid=1",
        "pid=x",
        "pid=4 pid=5",
        "quiet=-1",
        "quiet=1 quiet=2",
        "if-stalled if-stalled",
        "term",
    ] {
        assert!(
            parse(bad).is_err_and(|e| e.starts_with("ERR bad restart argument: ")),
            "{bad}"
        );
    }
}

#[test]
fn another_hand_is_what_the_acts_kill_waits_out() {
    let now = crate::metrics::now_us();
    let fresh = |n: u64| crate::session_store::test_handle_on(7400 + n, -1).ctx;
    let claimed = |ctx: &SessionCtx, holder: &str| {
        assert_eq!(
            crate::session_timeline::claim_supervisor(ctx, holder, None, None, now),
            Ok(true),
            "{holder}"
        );
    };
    let leased = |ctx: &SessionCtx, lease: crate::Lease| {
        *ctx.turn_lease.lock().unwrap() = Some(lease);
    };
    let drive = |holder: &str, expires_us: u64| crate::Lease::Drive {
        holder: holder.to_string(),
        expires_us,
        conn: None,
        hard: true,
        here: false,
        holder_pid: None,
    };
    let driver = fresh(0).self_id.clone();
    let mut n = 1;
    let mut case = |set: &dyn Fn(&SessionCtx), want: bool, what: &str| {
        let ctx = fresh(n);
        n += 1;
        set(ctx.as_ref());
        assert_eq!(another_hand(&ctx, now), want, "{what}");
    };
    case(&|_| {}, false, "nothing on the session");
    case(
        &|ctx| claimed(ctx, &crate::harness_host::holder()),
        false,
        "this host's own claim",
    );
    case(
        &|ctx| claimed(ctx, "aterm-harness@1"),
        false,
        "a predecessor host's claim, carried",
    );
    case(
        &|ctx| claimed(ctx, "watcher@elsewhere"),
        true,
        "another supervisor's claim",
    );
    case(
        &|ctx| {
            assert!(crate::fabric::apply_hold_for_test(
                ctx,
                Some(crate::fabric::Hold {
                    reason: "-".to_string(),
                    origin: "fleet".to_string(),
                }),
            ));
        },
        true,
        "a halt",
    );
    case(
        &|ctx| {
            leased(
                ctx,
                crate::Lease::Turn {
                    id: 1,
                    driver: Some(driver.clone()),
                    typing: false,
                },
            );
        },
        true,
        "a named driver's turn",
    );
    case(
        &|ctx| {
            leased(
                ctx,
                crate::Lease::Turn {
                    id: 2,
                    driver: None,
                    typing: true,
                },
            );
        },
        false,
        "an owner-class turn",
    );
    case(
        &|ctx| leased(ctx, drive("someone", now + 60_000_000)),
        true,
        "another driver's live lease",
    );
    case(
        &|ctx| leased(ctx, drive("someone", now.saturating_sub(1))),
        false,
        "a lapsed lease",
    );
    case(
        &|ctx| leased(ctx, drive(&crate::harness_host::holder(), now + 60_000_000)),
        false,
        "this host's own lease",
    );
}

#[test]
fn the_act_is_not_offered_for_an_agent_its_relaunch_refuses() {
    let now = crate::metrics::now_us();
    let other = aterm_tempfile::TempDir::new().expect("another home");
    let snap = |claude_dir: Option<std::path::PathBuf>| aterm_agent::harness::relaunch::Snapshot {
        launch_env: None,
        auth_observed: true,
        tab: String::new(),
        pid: 0x3fff_ff01,
        lifetime: None,
        start: "t".to_string(),
        shell: 1,
        program: std::path::PathBuf::from("/opt/claude"),
        argv: vec!["/opt/claude".to_string()],
        session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
        cwd: "/".to_string(),
        version: Some("2.1.300".to_string()),
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
        login: None,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        home: None,
        claude_dir,
    };
    let refusal = |n: u64, claude_dir: Option<std::path::PathBuf>| {
        let ctx = crate::session_store::test_handle_on(7600 + n, -1).ctx;
        assert_eq!(
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                now
            ),
            Ok(true),
            "the host's claim"
        );
        let snap = aterm_agent::harness::relaunch::Snapshot {
            tab: ctx.self_id.as_str().to_string(),
            ..snap(claude_dir)
        };
        let sid = ctx.self_id.as_str();
        crate::harness_host::publish_snapshot_for_test(sid, Some(&snap));
        let why = unavailable(&ctx, 0x3fff_ff01, "t", now);
        assert_eq!(
            super::offered_for(sid, 0x3fff_ff01),
            why.is_none(),
            "the word outside the session agrees with the act"
        );
        assert!(
            !super::offered_for(sid, 0x3fff_ff02),
            "another process: not-the-hosts-agent"
        );
        crate::harness_host::publish_snapshot_for_test(sid, None);
        assert!(
            !super::offered_for(sid, 0x3fff_ff01),
            "no snapshot published, as in an Autopilot-off tab: not-the-hosts-agent"
        );
        why
    };
    assert_eq!(
        refusal(1, Some(other.path().join("records"))),
        Some("not-relaunchable"),
        "its relaunch refuses it: the act is not offered"
    );
    assert_eq!(refusal(2, None), None, "CONTROL: the user's own home");
    assert_eq!(
        refusal(3, Some(other.path().join(".claude"))),
        None,
        "CONTROL: another HOME's .claude, which its line carries"
    );
}

#[test]
fn an_agy_is_offered_the_act_where_its_relaunch_resumes_it() {
    use aterm_agent::harness::relaunch::{AgentRun, AgyRun, MuseRun};
    let now = crate::metrics::now_us();
    let tab_home = aterm_tempfile::tempdir().expect("a scratch home");
    let conversation = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";
    let refusal =
        |n: u64,
         session: Option<&str>,
         extra: &[&str],
         muse: bool,
         home: &str,
         (dialect, cwd): (aterm_agent::harness::upgrade::Dialect, &str)| {
            let ctx = crate::session_store::test_handle_on(7800 + n, -1).ctx;
            assert_eq!(
                crate::session_timeline::claim_supervisor(
                    &ctx,
                    &crate::harness_host::holder(),
                    None,
                    None,
                    now
                ),
                Ok(true),
                "the host's claim"
            );
            let mut argv = vec!["/opt/agy".to_string()];
            argv.extend(extra.iter().map(|w| (*w).to_string()));
            let run = if muse {
                AgentRun::Muse(MuseRun {
                    data: "/h/.local/share".into(),
                    env: Vec::new(),
                    workspace: None,
                    lock: None,
                    started_ms: 0,
                    block: None,
                })
            } else {
                AgentRun::Agy(AgyRun {
                    config: "/h/.gemini/antigravity-cli".into(),
                    env: if home.is_empty() {
                        Vec::new()
                    } else {
                        vec![("HOME".to_string(), home.to_string())]
                    },
                    lock: None,
                    crash: None,
                    started_ms: 0,
                    block: None,
                })
            };
            let snap = aterm_agent::harness::relaunch::Snapshot {
                launch_env: None,
                auth_observed: true,
                tab: ctx.self_id.as_str().to_string(),
                pid: 0x3fff_ff21,
                lifetime: None,
                start: "t".to_string(),
                shell: 1,
                program: std::path::PathBuf::from("/opt/agy"),
                argv,
                session: session.map(str::to_string),
                cwd: cwd.to_string(),
                version: None,
                run,
                dialect: Some(dialect),
                login: None,
                route: aterm_agent::harness::accounts::AuthRoute::Outside,
                profile: None,
                home: None,
                claude_dir: None,
            };
            crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(&snap));
            let why = unavailable_for(&ctx, 0x3fff_ff21, "t", now, Some(tab_home.path()));
            crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
            why
        };
    use aterm_agent::harness::upgrade::Dialect;
    let zsh = (Dialect::Zsh, "/");
    assert_eq!(
        refusal(1, Some(conversation), &["--effort=high"], false, "", zsh),
        None,
        "a witnessed agy whose line carries its launch"
    );
    assert_eq!(
        refusal(2, None, &[], false, "", zsh),
        Some("not-relaunchable"),
        "no conversation proven"
    );
    assert_eq!(
        refusal(3, Some(conversation), &["--remote-control"], false, "", zsh),
        Some("not-relaunchable"),
        "its line refuses the launch"
    );
    assert_eq!(
        refusal(4, Some(conversation), &[], true, "", zsh),
        Some("not-relaunchable"),
        "a Muse Code: never yet"
    );
    assert_eq!(
        refusal(5, Some(conversation), &[], false, "/h/a\nb", zsh),
        Some("not-relaunchable"),
        "a HOME its relaunch line cannot hold"
    );
    assert_eq!(
        refusal(6, Some(conversation), &[], false, "/h", zsh),
        None,
        "CONTROL: another HOME its line carries"
    );
    assert_eq!(
        refusal(
            7,
            Some(conversation),
            &[],
            false,
            "/h",
            (Dialect::Fish, "/")
        ),
        None,
        "a fish shell"
    );
    for (n, dialect, want) in [
        (8, Dialect::Fish, None),
        (9, Dialect::Zsh, Some("not-relaunchable")),
    ] {
        assert_eq!(
            refusal(n, Some(conversation), &[], false, "", (dialect, "/w/a\nb")),
            want,
            "{dialect:?}: a directory no `cd` names"
        );
    }
}

#[test]
fn a_codex_with_no_validated_copy_is_not_offered_the_act() {
    let now = crate::metrics::now_us();
    let home = aterm_primer::home_dir().expect("a home");
    let pid = 0x3fff_ff11;
    let refusal = |n: u64, copy: Option<u32>| {
        let ctx = crate::session_store::test_handle_on(7700 + n, -1).ctx;
        let sid = ctx.self_id.as_str().to_string();
        let snap = aterm_agent::harness::relaunch::Snapshot {
            launch_env: None,
            auth_observed: true,
            tab: sid.clone(),
            pid,
            lifetime: None,
            start: "t".to_string(),
            shell: 1,
            program: std::path::PathBuf::from("/opt/codex/bin/codex"),
            argv: vec!["/opt/codex/bin/codex".to_string()],
            session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
            cwd: "/".to_string(),
            version: None,
            run: aterm_agent::harness::relaunch::AgentRun::Codex(
                aterm_agent::harness::relaunch::CodexRun {
                    home: home.join(".codex"),
                    home_env: None,
                    self_claimed: None,
                },
            ),
            dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
            login: None,
            route: aterm_agent::harness::accounts::AuthRoute::Outside,
            profile: None,
            home: None,
            claude_dir: None,
        };
        assert!(
            !aterm_agent::harness::relaunch::carries_restart(&home, &snap),
            "no line rides the snapshot: the relaunch's own predicate carries nothing"
        );
        let copy = copy.map(|of| aterm_agent::harness::self_claimed::Copy {
            pid: of,
            lifetime: None,
            start: "t".to_string(),
            home: home.join(".codex"),
            seen: aterm_agent::harness::self_claimed::Seen::Removed,
            resume: Ok(aterm_agent::harness::self_claimed::Resume {
                argv: vec!["/opt/codex/bin/codex".to_string()],
                session: "0badf00d-1111-2222-3333-444455556666".to_string(),
                codex_home: None,
                codex_auth_path: None,
                cwd: None,
            }),
        });
        crate::self_claim::set_copy_for_test(&sid, copy);
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
        let why = unavailable(&ctx, i32::try_from(pid).unwrap(), "t", now);
        crate::harness_host::publish_snapshot_for_test(&sid, None);
        crate::self_claim::set_copy_for_test(&sid, None);
        why
    };
    assert_eq!(refusal(1, None), Some("not-relaunchable"), "no copy");
    assert_eq!(
        refusal(2, Some(pid + 1)),
        Some("not-relaunchable"),
        "a copy of another process"
    );
    assert_ne!(
        refusal(3, Some(pid)),
        Some("not-relaunchable"),
        "CONTROL: the copy of that very process validated its line"
    );
}

const TAB: &str = "s-watch";

fn stamped(state: &aterm_tempfile::TempDir, stamp: &Stamp) {
    restart_stamp::write(state.path(), TAB, stamp).expect("the stamp");
}

#[test]
fn a_watch_that_lets_go_is_unlisted_in_that_look_and_a_raise_before_it_keeps_it() {
    let pid = 0x3fff_fff1_i32;
    let state = aterm_tempfile::TempDir::new().expect("a state dir");
    let now = aterm_time::unix_secs().expect("clock");
    let stamp = |at: u64| Stamp {
        pid: pid.unsigned_abs(),
        start: "t".to_string(),
        cause: Cause::Asked,
        at,
        wanted: None,
        input: None,
    };
    let short = stamp(now + 10 - aterm_agent::harness::relaunch::STALE_S);
    stamped(&state, &short);
    let w = watch(state.path(), pid, &short).expect("the first watch");
    assert!(w.lapsed(), "10 s left: inside the read margin");
    stamped(&state, &stamp(now));
    assert!(
        watch(state.path(), pid, &stamp(now)).is_none(),
        "a join finds it running"
    );
    assert!(!w.lapsed(), "its KILL may go: by the join's stamp");
    assert!(w.left(TAB).is_ok(), "raised before its look: it waits on");
    assert_eq!(watches(pid), 1);
    drop(w);
    assert_eq!(watches(pid), 0, "unlisted as its thread drops it");
    stamped(&state, &short);
    let w = watch(state.path(), pid, &short).expect("a watch again");
    assert_eq!(w.left(TAB), Err(LetGo::Lapsed), "not raised: it lets go");
    assert_eq!(
        watches(pid),
        0,
        "unlisted in that same look, the watch itself still held"
    );
    assert!(w.lapsed(), "let go: no KILL");
    stamped(&state, &stamp(now));
    let join = watch(state.path(), pid, &stamp(now)).expect("a join after the let-go gets its own");
    assert_eq!(watches(pid), 1);
    drop(w);
    assert_eq!(watches(pid), 1, "the let-go watch's end leaves the join's");
    drop(join);
    assert_eq!(watches(pid), 0);
}

#[test]
fn a_watch_whose_tabs_stamp_names_another_lets_go_in_that_look() {
    let pid = 0x3fff_fff3_i32;
    let state = aterm_tempfile::TempDir::new().expect("a state dir");
    let now = aterm_time::unix_secs().expect("clock");
    let ours = Stamp {
        pid: pid.unsigned_abs(),
        start: "t".to_string(),
        cause: Cause::Stall,
        at: now,
        wanted: None,
        input: None,
    };
    stamped(&state, &ours);
    let w = watch(state.path(), pid, &ours).expect("a watch");
    assert!(w.left(TAB).is_ok(), "its stamp names it: it waits on");
    stamped(
        &state,
        &Stamp {
            pid: pid.unsigned_abs() + 1,
            ..ours.clone()
        },
    );
    assert_eq!(w.left(TAB), Err(LetGo::Unnamed), "another act's stamp");
    assert_eq!(watches(pid), 0, "unlisted in that same look");
    stamped(&state, &ours);
    let join = watch(state.path(), pid, &ours).expect("a join gets its own");
    assert!(join.left(TAB).is_ok());
    let _ = restart_stamp::take_if(state.path(), TAB, |_| true);
    assert_eq!(join.left(TAB), Err(LetGo::Unnamed), "its stamp taken");
    assert_eq!(watches(pid), 0);
    drop((w, join));
}

#[test]
fn a_watch_waits_on_while_another_take_judges_its_tabs_stamp() {
    let pid = 0x3fff_fff4_i32;
    let state = aterm_tempfile::TempDir::new().expect("a state dir");
    let ours = Stamp {
        pid: pid.unsigned_abs(),
        start: "t".to_string(),
        cause: Cause::Stall,
        at: aterm_time::unix_secs().expect("clock"),
        wanted: None,
        input: None,
    };
    stamped(&state, &ours);
    let w = watch(state.path(), pid, &ours).expect("a watch");
    let mut looked = None;
    let taken = restart_stamp::take_if(state.path(), TAB, |s| {
        looked = Some(w.left(TAB));
        s.names(pid.unsigned_abs() + 1, "t")
    });
    assert_eq!(taken, None, "not that snapshot's stamp");
    assert!(
        looked.is_some_and(|left| left.is_ok()),
        "a look while it was judged: {looked:?}"
    );
    assert_eq!(watches(pid), 1, "still listed");
    assert!(w.left(TAB).is_ok(), "it waits on");
    assert_eq!(restart_stamp::read(state.path(), TAB), Some(ours));
    drop(w);
    assert_eq!(watches(pid), 0);
}

#[test]
fn a_stamp_dated_past_any_instant_gives_its_watch_no_time() {
    let pid = 0x3fff_fff2_i32;
    let state = aterm_tempfile::TempDir::new().expect("a state dir");
    let w = watch(
        state.path(),
        pid,
        &Stamp {
            pid: pid.unsigned_abs(),
            start: "t".to_string(),
            cause: Cause::Asked,
            at: u64::MAX,
            wanted: None,
            input: None,
        },
    )
    .expect("a watch");
    assert!(w.lapsed(), "no KILL");
    assert_eq!(w.left(TAB), Err(LetGo::Lapsed), "let go at its first look");
    assert_eq!(watches(pid), 0);
}

/// `restart` takes one account word: a roster row's label or the pool's choice, in any place
/// among `pid=` and `quiet=`. A row may be labelled `auto`, so the pool's choice is its own bare
/// word and `account=auto` names that row. Refused as a bad argument: either word twice, both
/// together, either with `if-stalled` in either order, and a label outside the roster grammar.
#[test]
fn the_restart_verb_parses_one_account_word() {
    let row = |label: &str| Some(Wanted::Row(label.to_string()));
    assert_eq!(
        parse("account=work"),
        Ok(Ask {
            account: row("work"),
            ..Ask::default()
        })
    );
    assert_eq!(
        parse("quiet=30 account=work pid=4242"),
        Ok(Ask {
            pid: Some(4242),
            if_stalled: false,
            quiet: Some(30),
            account: row("work"),
        })
    );
    assert_eq!(
        parse("account-auto pid=4242"),
        Ok(Ask {
            pid: Some(4242),
            account: Some(Wanted::Pool),
            ..Ask::default()
        })
    );
    assert_eq!(
        parse("account=auto").map(|ask| ask.account),
        Ok(row("auto")),
        "a row labelled `auto` is a row"
    );
    for (bad, word) in [
        ("account=work account=home", "account=home"),
        ("account-auto account-auto", "account-auto"),
        ("account=work account-auto", "account-auto"),
        ("account-auto account=work", "account=work"),
        ("if-stalled account=work", "account=work"),
        ("account=work if-stalled", "if-stalled"),
        ("if-stalled account-auto", "account-auto"),
        ("account-auto if-stalled", "if-stalled"),
        ("account=", "account="),
        ("account=a/b", "account=a/b"),
        ("account=a:b", "account=a:b"),
        ("account-auto=work", "account-auto=work"),
        ("account", "account"),
    ] {
        assert_eq!(
            parse(bad),
            Err(format!("ERR bad restart argument: {word}\n")),
            "{bad}"
        );
    }
}

/// A published snapshot of tab `sid`'s self-claimed codex-alab `pid` started at `start`, with
/// the shell `shell` under it.
fn publish_codex(sid: &str, pid: u32, start: &str, shell: u32) {
    let copy = aterm_agent::harness::self_claimed::Copy {
        lifetime: None,
        pid,
        start: start.to_string(),
        home: std::path::PathBuf::from("/u/.codex"),
        seen: aterm_agent::harness::self_claimed::Seen::Removed,
        resume: Ok(aterm_agent::harness::self_claimed::Resume {
            argv: vec!["/opt/codex/bin/codex".to_string()],
            session: "0badf00d-1111-2222-3333-444455556666".to_string(),
            codex_home: None,
            codex_auth_path: None,
            cwd: None,
        }),
    };
    let resume = copy.resume.clone().expect("a line");
    let snap = aterm_agent::harness::relaunch::snapshot_of_copy(sid, &copy, &resume, shell, None);
    crate::harness_host::publish_snapshot_for_test(sid, Some(&snap));
}

/// THE ACCOUNT FENCE REFUSES IN ITS ORDER, AFTER THE ACT'S OWN FENCES ([`account_fence`]): a
/// judgement of another process, or none, is `account-unread`; a process the tab holds no
/// validated self-claim copy of is `account-unmanaged`; a session with no shell under it is
/// `account-not-resumable`; then the pool's own word; then a standing restart of the process
/// that names another destination, or none, is `account-restart-pending`. CONTROLS: with each of
/// those in order the ask passes, and it joins a standing stamp that names the same account.
#[test]
fn an_account_ask_is_refused_in_the_order_of_its_fences() {
    let ctx = crate::session_store::test_handle_on(7741, -1).ctx;
    let sid = ctx.self_id.as_str().to_string();
    let (pid, start) = (0x3fff_ff21_u32, "t");
    let work = Wanted::Row("work".to_string());
    let judged = |copied: bool, refusal: Option<&'static str>| Judged {
        pid,
        start: start.to_string(),
        copied,
        home: None,
        refusal,
    };
    let stamp = |wanted: Option<Wanted>| Stamp {
        pid,
        start: start.to_string(),
        cause: Cause::Asked,
        at: 1,
        wanted,
        input: None,
    };
    let fence = |judged: Option<&Judged>, standing: Option<&Stamp>| {
        account_fence(&ctx, (pid, start), &work, judged, standing)
    };
    publish_codex(&sid, pid, start, 1);
    let ok = judged(true, None);
    assert_eq!(fence(Some(&ok), None), None, "CONTROL: nothing refuses it");
    assert_eq!(fence(None, None), Some(accounts_pool::MOVE_UNREAD));
    let other = Judged {
        start: "u".to_string(),
        ..ok.clone()
    };
    assert_eq!(
        fence(Some(&other), None),
        Some(accounts_pool::MOVE_UNREAD),
        "the judgement was of another process"
    );
    let limited = Some(accounts_pool::MOVE_LIMITED);
    assert_eq!(
        fence(Some(&judged(false, limited)), None),
        Some(accounts_pool::MOVE_UNMANAGED),
        "no validated copy comes before the pool's word"
    );
    publish_codex(&sid, pid, start, aterm_agent::harness::relaunch::NO_SHELL);
    assert_eq!(
        fence(Some(&judged(true, limited)), None),
        Some(ACCOUNT_NOT_RESUMABLE),
        "no shell comes before the pool's word"
    );
    publish_codex(&sid, pid + 1, start, 1);
    assert_eq!(
        fence(Some(&ok), None),
        Some(ACCOUNT_NOT_RESUMABLE),
        "the host's snapshot is of another process"
    );
    publish_codex(&sid, pid, start, 1);
    assert_eq!(fence(Some(&judged(true, limited)), None), limited);
    assert_eq!(
        fence(Some(&judged(true, limited)), Some(&stamp(None))),
        limited,
        "the pool's word comes before a standing restart"
    );
    assert_eq!(
        fence(Some(&ok), Some(&stamp(None))),
        Some(ACCOUNT_RESTART_PENDING),
        "a plain restart stands"
    );
    assert_eq!(
        fence(Some(&ok), Some(&stamp(Some(Wanted::Pool)))),
        Some(ACCOUNT_RESTART_PENDING),
        "a restart for another destination stands"
    );
    assert_eq!(
        fence(Some(&ok), Some(&stamp(Some(work.clone())))),
        None,
        "CONTROL: a restart for the same account is joined"
    );
    crate::harness_host::publish_snapshot_for_test(&sid, None);
}

/// A JUDGEMENT THAT DOES NOT ANSWER IN TIME IS NO ANSWER ([`bounded`]): the wait is bounded, so
/// a pool whose files hang holds no ask, and no more than [`ACCOUNT_JUDGES`] such reads wait at
/// once: one more is answered at once. CONTROLS: one that answers is its answer, and a slot
/// freed by an answer is taken again.
#[test]
fn an_account_judgement_is_waited_for_only_so_long() {
    static RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let brief = Duration::from_millis(50);
    let long = aterm_spec::wait::MUST_HAPPEN;
    assert_eq!(
        bounded_among(&RUNNING, long, || 7),
        Some(7),
        "CONTROL: an answer in time"
    );
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Arc::new(std::sync::Mutex::new(held));
    let hang = || {
        let held = Arc::clone(&held);
        move || held.lock().unwrap().recv().is_ok()
    };
    for n in 0..ACCOUNT_JUDGES {
        let began = Instant::now();
        assert_eq!(bounded_among(&RUNNING, brief, hang()), None, "no answer");
        assert!(began.elapsed() >= brief, "judgement {n} was waited for");
        assert!(
            began.elapsed() < long,
            "and for no longer than it was told: {:?}",
            began.elapsed()
        );
    }
    let began = Instant::now();
    assert_eq!(
        bounded_among(&RUNNING, long, hang()),
        None,
        "one more than may wait"
    );
    assert!(
        began.elapsed() < long,
        "it is answered at once, not waited for: {:?}",
        began.elapsed()
    );
    drop(release);
    let answered = aterm_spec::wait::within(long, Duration::from_millis(5), || {
        bounded_among(&RUNNING, long, || 7)
    });
    assert_eq!(answered, Some(7), "CONTROL: a freed slot is taken again");
}

/// A MOVE WAITS FOR ITS SESSION'S RECORD NO LONGER THAN 1 S, NEVER THE POOL'S 5 S: the read is
/// made under the act's lock, so its bound is how long a home that does not answer holds every
/// other tab's restart; a test build's timing keeps it at its floor or above. CONTROL: a test's
/// own read replaces it until its guard drops.
#[test]
fn a_moves_rest_read_waits_no_longer_than_account_rest() {
    assert_eq!(
        rest_wait(aterm_gui_timing::Timing::PRODUCTION),
        Duration::from_secs(1)
    );
    assert_eq!(
        rest_wait(aterm_gui_timing::Timing::FAST),
        ACCOUNT_REST_TEST_FLOOR
    );
    let (_, wait) = rest_read();
    assert_eq!(wait, rest_wait(aterm_gui_timing::current()));
    {
        let _rest = rest_for_test(Arc::new(|_, _, _| None), Duration::from_millis(3));
        assert_eq!(rest_read().1, Duration::from_millis(3), "CONTROL");
    }
    assert_eq!(rest_read().1, wait, "CONTROL: the guard dropped");
}
