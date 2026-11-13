// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::control_verbs::input_of;

#[cfg(unix)]
#[test]
fn signal_parses_the_bare_form_and_the_exact_pid_form() {
    assert_eq!(parse_signal("int"), Ok((libc::SIGINT, None, None)));
    assert_eq!(parse_signal(" term "), Ok((libc::SIGTERM, None, None)));
    assert_eq!(
        parse_signal("int pid=52489"),
        Ok((libc::SIGINT, Some(52489), None))
    );
    assert_eq!(
        parse_signal("term pid=52489 quiet=120"),
        Ok((libc::SIGTERM, Some(52489), Some(120)))
    );
    assert_eq!(
        parse_signal("term quiet=0"),
        Ok((libc::SIGTERM, None, Some(0)))
    );
    assert_eq!(
        parse_signal("nope pid=5"),
        Err("ERR unknown signal: nope pid=5\n".to_string())
    );
    for bad in [
        "int pid=",
        "int pid=x",
        "int pid=1",
        "int pid=0",
        "int pid=-3",
        "int 5",
        "int pid=5 pid=6",
        "term quiet=",
        "term quiet=-1",
        "term quiet=5 quiet=6",
    ] {
        assert!(parse_signal(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_cross_session_px_resize_is_refused_by_name_and_the_cell_form_is_not() {
    for px in [
        "px 1400 900",
        "  px  1400   900  ",
        "px 1400",
        "px wide tall",
    ] {
        assert_eq!(
            cross_resize_px_refusal(px).as_deref(),
            Some(RESIZE_PX_FRONT_ONLY),
            "{px:?}: every px spelling is refused by target, before its arguments"
        );
    }
    assert_ne!(
        RESIZE_PX_FRONT_ONLY, "ERR bad args\n",
        "the refusal names the target rule, not the cell parser's verdict"
    );
    assert!(RESIZE_PX_FRONT_ONLY.ends_with('\n') && RESIZE_PX_FRONT_ONLY.starts_with("ERR "));
    for cell in ["24 80", "x y", ""] {
        assert_eq!(
            cross_resize_px_refusal(cell),
            None,
            "{cell:?}: the cell form keeps its own parser and its own replies"
        );
    }
}

#[test]
fn the_control_fence_is_the_seam_s_fence_of_the_parsed_body() {
    let key = |body: &str| control_fence(input_of("key"), "key", body, false);
    for body in [
        "enter",
        "tab",
        "shift+tab",
        "alt+tab",
        "a",
        "space",
        "ctrl+v",
    ] {
        assert_eq!(key(body), None, "`key {body}` arms its own licence class");
    }
    for body in [
        "shiftleft",
        "altright",
        "left type=release",
        "a type=release",
    ] {
        assert_eq!(
            key(body),
            None,
            "`key {body}` is fenced no more than a physical one"
        );
    }
    for body in [
        "backspace",
        "left",
        "right",
        "up",
        "down",
        "home",
        "end",
        "delete",
        "ctrl+a",
        "ctrl+e",
        "ctrl+w",
        "ctrl+u",
        "ctrl+k",
        "alt+b",
        "alt+d",
        "left type=repeat",
    ] {
        assert_eq!(key(body), Some(true), "`key {body}` edits the line");
    }
    for body in [
        "ctrl+r",
        "ctrl+s",
        "esc",
        "f1",
        "f5",
        "ctrl+enter",
        "shift+enter",
        "ctrl+c",
        "alt+x",
        "super+left",
        "super+home",
        "",
        "notakey",
        "shift",
    ] {
        assert_eq!(
            key(body),
            Some(false),
            "`key {body}` takes the hand off its row"
        );
    }
    for c in ["w", "u", "a", "e", "b", "k", "r", "c", "v", "x"] {
        assert_eq!(
            control_fence(input_of("ctrl"), "ctrl", c, false),
            key(&format!("ctrl+{c}")),
            "`ctrl {c}` and `key ctrl+{c}` are one press"
        );
    }
    assert_eq!(
        control_fence(input_of("ctrl"), "ctrl", "ww", false),
        Some(false),
        "malformed"
    );
    assert_eq!(
        control_fence(input_of("paste"), "paste", "hello", false),
        Some(true)
    );
    for body in ["in", "out"] {
        assert_eq!(
            control_fence(input_of("focus"), "focus", body, false),
            Some(true)
        );
    }
    for (body, event) in [
        (
            "14 80",
            InputEvent::Resize {
                rows: 14,
                cols: 80,
                echo_to_window: true,
            },
        ),
        (
            "px 1144 378",
            InputEvent::ResizeWindowPx {
                width: 1144,
                height: 378,
            },
        ),
    ] {
        assert_eq!(control_press("resize", body), Some(event));
        for fenced_write in [false, true] {
            assert_eq!(
                control_fence(input_of("resize"), "resize", body, fenced_write),
                Some(true),
                "the valid geometry request keeps the hand through its ingress fence"
            );
        }
    }
    for body in [
        "".to_owned(),
        "14".into(),
        "bad 80".into(),
        "0 80".into(),
        format!("{} 80", u32::from(MAX_GRID_ROWS) + 1),
        format!("14 {}", u32::from(MAX_GRID_COLS) + 1),
        "px 0 378".into(),
        "px 1144 0".into(),
        "px 1144".into(),
        "px nope 378".into(),
        "px 4294967296 378".into(),
    ] {
        assert_eq!(control_press("resize", &body), None);
        for fenced_write in [false, true] {
            assert_eq!(
                control_fence(input_of("resize"), "resize", &body, fenced_write),
                Some(false),
                "malformed or range-invalid geometry retains the generic attempt fence"
            );
        }
    }
    for body in [
        "press left 2 4",
        "release left 2 4",
        "move left 2 5",
        "wheelup left 2 4",
    ] {
        assert_eq!(
            control_fence(input_of("mouse"), "mouse", body, false),
            Some(true),
            "`mouse {body}` keeps the hand on its row"
        );
    }
    assert_eq!(
        control_fence(input_of("mouse"), "mouse", "move 2 5", false),
        None,
        "a hover holds no button and draws no fence"
    );
    assert_eq!(
        control_fence(input_of("key"), "key", "a", true),
        Some(false)
    );
    assert_eq!(
        control_fence(input_of("key"), "key", "enter", true),
        Some(false)
    );
    assert_eq!(
        control_fence(input_of("key"), "key", "left", true),
        Some(true)
    );
    assert_eq!(
        control_fence(input_of("send"), "send", "ls", true),
        Some(false)
    );
    for verb in [
        "send", "feed", "signal", "restart", "mouse", "focus", "turn", "resize", "scroll",
    ] {
        assert_eq!(
            control_fence(input_of(verb), verb, "", false),
            Some(false),
            "{verb}"
        );
    }
    for verb in ["text", "screen", "cursor", "image", "metrics"] {
        assert_eq!(
            control_fence(input_of(verb), verb, "left", false),
            None,
            "{verb}"
        );
    }
}

#[test]
fn leading_options_compose_in_either_order_and_a_body_option_is_body() {
    let opts = |idem: Option<&str>, guard: Option<&str>| LeadingInputOptions {
        idem: idem.map(str::to_string),
        guard: guard.map(str::to_string),
        ..LeadingInputOptions::default()
    };
    assert_eq!(
        take_leading_options(input_of("send"), "id=a:b:c hello there"),
        (opts(Some("a:b:c"), None), "hello there")
    );
    assert_eq!(
        take_leading_options(input_of("send"), "hello id=a:b:c"),
        (opts(None, None), "hello id=a:b:c")
    );
    assert_eq!(
        take_leading_options(input_of("send"), "  two spaces"),
        (opts(None, None), "  two spaces")
    );
    assert_eq!(
        take_leading_options(input_of("send"), "-- if=literal"),
        (opts(None, None), "if=literal")
    );
    assert_eq!(
        take_leading_options(input_of("paste"), "if=x id=a:b:c hi"),
        (opts(None, None), "if=x id=a:b:c hi")
    );
    assert_eq!(
        take_leading_options(input_of("turn"), "id=a:b:c"),
        (opts(Some("a:b:c"), None), "")
    );
    assert_eq!(
        take_leading_options(input_of("key"), "if=Do.you.want.to.proceed 1"),
        (opts(None, Some("Do.you.want.to.proceed")), "1")
    );
    assert_eq!(
        take_leading_options(input_of("key"), "id=a:b:c if=Do.you.want 1"),
        (opts(Some("a:b:c"), Some("Do.you.want")), "1")
    );
    assert_eq!(
        take_leading_options(input_of("key"), "if=Do.you.want id=a:b:c 1"),
        (opts(Some("a:b:c"), Some("Do.you.want")), "1")
    );
    assert_eq!(
        take_leading_options(input_of("send"), "if=proceed id=a:b:c y\\n"),
        (opts(Some("a:b:c"), Some("proceed")), "y\\n")
    );
    assert_eq!(
        take_leading_options(input_of("turn"), "if=x id=a:b:c hello"),
        (opts(None, None), "if=x id=a:b:c hello")
    );
    assert_eq!(
        take_leading_options(input_of("turn"), "id=a:b:c if=x hello"),
        (opts(Some("a:b:c"), None), "if=x hello")
    );
    let (o, tail) = take_leading_options(input_of("key"), "if= 1");
    assert!(
        o.refusal
            .as_deref()
            .is_some_and(|r| r.starts_with("ERR usage: if="))
    );
    assert_eq!(tail, "1");
    let (o, _) = take_leading_options(input_of("key"), "if=a if=b 1");
    assert_eq!(o.refusal.as_deref(), Some("ERR usage: if= given twice\n"));
    let (o, _) = take_leading_options(input_of("key"), "id=a:b:c id=d:e:f 1");
    assert_eq!(o.refusal.as_deref(), Some("ERR usage: id= given twice\n"));
}

#[test]
#[cfg(unix)]
fn signal_cont_is_a_known_signal() {
    for name in ["cont", "sigcont"] {
        assert_eq!(
            cmd_signal(-1, name, &SinkWriter::new(-1), None, None),
            "ERR no foreground process group\n",
            "{name}"
        );
    }
    assert_eq!(
        cmd_signal(-1, "continue", &SinkWriter::new(-1), None, None),
        "ERR unknown signal: continue\n"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn signal_term_drops_a_frozen_programs_keys_before_the_shell_can_run_them() {
    let (ran, reply) = typeahead_after_restart(Remedy::BareKillpg, Typed::Direct);
    assert!(
        ran,
        "the control: a bare killpg lets the shell run the line"
    );
    assert_eq!(reply, None);
    let (ran, reply) = typeahead_after_restart(Remedy::Verb, Typed::Direct);
    assert!(!ran, "signal term dropped the line before the shell got it");
    let reply = reply.expect("the verb's reply");
    assert!(
        reply.starts_with("OK signalled pgrp ") && reply.ends_with(" discarded=17\n"),
        "{reply}"
    );
    let (ran, reply) = typeahead_after_restart(Remedy::VerbPid, Typed::Direct);
    assert!(!ran, "signal term pid= dropped the line too");
    let reply = reply.expect("the verb's reply");
    assert!(
        reply.starts_with("OK signalled pid ") && reply.ends_with(" discarded=17\n"),
        "{reply}"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn signal_term_drops_a_spill_behind_a_full_queue_and_the_session_still_hears() {
    let (ran, reply) = typeahead_after_restart(Remedy::BareKillpg, Typed::ThroughASpill);
    assert!(ran, "the control: the spilled line runs in the shell");
    assert_eq!(reply, None);
    let (ran, reply) = typeahead_after_restart(Remedy::Verb, Typed::ThroughASpill);
    assert!(!ran, "signal term dropped the spilled line too");
    let reply = reply.expect("the verb's reply");
    assert!(
        reply.starts_with("OK signalled pgrp ") && reply.ends_with(" discarded=1118\n"),
        "{reply}"
    );
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Remedy {
    BareKillpg,
    Verb,
    VerbPid,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Typed {
    Direct,
    ThroughASpill,
}

#[cfg(target_os = "macos")]
fn typeahead_after_restart(remedy: Remedy, typed: Typed) -> (bool, Option<String>) {
    use std::os::fd::IntoRawFd as _;
    use std::time::Duration;
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let (master, slave, child) = aterm_pty::spawn_session_leader_on_pty(|slave| {
        let mut shell = std::process::Command::new("/bin/zsh");
        shell
            .args(["-f", "-i"])
            .envs(aterm_spec::host::hermetic_env(home.path()))
            .env("TERM", "xterm")
            .stdout(std::process::Stdio::from(slave.try_clone()?))
            .stderr(std::process::Stdio::from(slave.try_clone()?));
        for key in aterm_spec::host::SHELL_START_POINTERS {
            shell.env_remove(key);
        }
        Ok(shell)
    })
    .expect("spawn zsh");
    drop(slave);
    let master = master.into_raw_fd();
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let shell = Reap(child);
    aterm_pty::set_nonblocking(master, true).expect("nonblocking master");
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drawer = {
        let (seen, stop) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let mut buf = [0u8; 4096];
                // SAFETY: a bounded read into a live stack buffer from the master, which outlives this thread (joined before close).
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                match usize::try_from(n) {
                    Ok(n) if n > 0 => seen.lock().expect("seen").extend_from_slice(&buf[..n]),
                    _ => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        })
    };
    let drawn = |needle: &str| {
        let seen = seen.lock().expect("seen");
        seen.windows(needle.len()).any(|w| w == needle.as_bytes())
    };
    let wait_for = |what: &str, done: &dyn Fn() -> bool| {
        let drawn = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(10),
            || done().then_some(()),
        );
        assert!(
            drawn.is_some(),
            "timed out waiting for {what}: {:?}",
            String::from_utf8_lossy(&seen.lock().expect("seen"))
        );
    };
    let type_in = |bytes: &[u8]| {
        // SAFETY: a bounded write of a live slice to the live master.
        let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(usize::try_from(n).ok(), Some(bytes.len()), "write(master)");
    };

    wait_for("the prompt", &|| drawn("\x1b[?2004h"));
    type_in(
        format!(
            "(stty raw -echo; exec sleep {})\r",
            aterm_spec::wait::PARKED_SECS
        )
        .as_bytes(),
    );
    let shell_pgrp = libc::pid_t::try_from(shell.0.id()).expect("pid");
    wait_for("the job's raw mode", &|| {
        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        let fg = unsafe { libc::tcgetpgrp(master) };
        fg > 0
            && fg != shell_pgrp
            && aterm_pty::tty_echo(master)
                .is_some_and(|e| !e.canonical && !e.echo && e.signals.is_none())
    });
    let sink = SinkWriter::new(master);
    sink.note_master_nonblocking(true);
    match typed {
        Typed::Direct => {
            type_in(b"echo TYPE''AHEAD\r");
            assert_eq!(aterm_pty::input_queue_len(master), Some(17));
        }
        Typed::ThroughASpill => {
            let mut paste = vec![b'#'; 1100];
            paste.extend_from_slice(b"\recho TYPE''AHEAD\r");
            assert_eq!(paste.len(), 1118);
            assert_eq!(sink.write_frame_nonparking(&paste).expect("paste"), 1118);
            wait_for("the paste parked", &|| {
                sink.input_backlog()
                    .is_some_and(|b| b.spilled.is_some() && b.unread() == 1118)
            });
            let parked = sink.input_backlog().expect("a pty is measured");
            assert_eq!(parked.queued, 1022, "the raw queue is full: {parked:?}");
        }
    }

    // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
    let pgrp = unsafe { libc::tcgetpgrp(master) };
    assert!(pgrp > 0, "a foreground job");
    let reply = match remedy {
        Remedy::Verb => Some(cmd_signal(master, "term", &sink, None, None)),
        Remedy::VerbPid => {
            let unread = sink.input_backlog().expect("measured").unread();
            assert_eq!(
                cmd_signal(
                    master,
                    &format!("term pid={pgrp} quiet=0"),
                    &sink,
                    None,
                    None
                ),
                format!("ERR busy input-unread bytes={unread}\n")
            );
            assert_eq!(
                cmd_signal(
                    master,
                    &format!("term pid={pgrp} quiet=120"),
                    &sink,
                    Some(5_000),
                    None,
                ),
                "ERR busy person human_ms=5000\n"
            );
            assert_eq!(
                cmd_signal(
                    master,
                    &format!("term pid={pgrp} quiet=1"),
                    &sink,
                    Some(5_000),
                    None,
                ),
                format!("ERR busy input-unread bytes={unread}\n"),
                "a person past the grace is no hold; the unread input still is"
            );
            assert_eq!(
                sink.input_backlog().expect("measured").unread(),
                unread,
                "nothing dropped"
            );
            Some(cmd_signal(
                master,
                &format!("term pid={pgrp}"),
                &sink,
                None,
                None,
            ))
        }
        Remedy::BareKillpg => {
            // SAFETY: `pgrp` is this test's own job.
            assert_eq!(unsafe { libc::killpg(pgrp, libc::SIGTERM) }, 0);
            None
        }
    };
    sink.write_frame_nonparking(b"echo MARK''ALIVE\r")
        .expect("the marker");
    wait_for("the shell's marker", &|| drawn("MARKALIVE"));
    let ran = drawn("TYPEAHEAD");

    drop(shell);
    stop.store(true, std::sync::atomic::Ordering::Release);
    drawer.join().expect("the drawer");
    aterm_pty::close_fd(master);
    (ran, reply)
}

#[test]
fn the_app_lane_takes_a_leading_unread_ok_on_its_gesture_verbs() {
    use super::take_app_unread_ok;
    assert_eq!(
        take_app_unread_ok(input_of("invoke"), "unread=ok Paste"),
        (true, "Paste")
    );
    assert_eq!(
        take_app_unread_ok(input_of("hwkey"), " unread=ok v mods=cmd"),
        (true, "v mods=cmd")
    );
    assert_eq!(
        take_app_unread_ok(input_of("invoke"), "Paste"),
        (false, "Paste")
    );
    assert_eq!(
        take_app_unread_ok(input_of("invoke"), "Paste unread=ok"),
        (false, "Paste unread=ok")
    );
    assert_eq!(
        take_app_unread_ok(input_of("hwkey"), "unread=yes v"),
        (false, "unread=yes v")
    );
    assert_eq!(
        take_app_unread_ok(input_of("tab"), "unread=ok close"),
        (false, "unread=ok close")
    );
}

#[test]
fn take_leading_options_takes_unread_ok_on_send_and_key_only() {
    let (o, tail) = take_leading_options(input_of("key"), "unread=ok down");
    assert!(o.unread_ok);
    assert_eq!(o.refusal, None);
    assert_eq!(tail, "down");
    let (o, tail) = take_leading_options(input_of("send"), "id=a:b:c unread=ok if=x hi");
    assert!(o.unread_ok);
    assert_eq!(o.idem.as_deref(), Some("a:b:c"));
    assert_eq!(o.guard.as_deref(), Some("x"));
    assert_eq!(tail, "hi");
    let (o, tail) = take_leading_options(input_of("key"), "unread=ok unread=ok down");
    assert_eq!(
        o.refusal.as_deref(),
        Some("ERR usage: unread= given twice\n")
    );
    assert_eq!(tail, "down");
    for bad in ["yes", "", "OK", "ok1"] {
        let (o, _) = take_leading_options(input_of("key"), &format!("unread={bad} down"));
        assert_eq!(
            o.refusal.as_deref(),
            Some("ERR usage: unread=ok is the only value\n"),
            "unread={bad:?}"
        );
        assert!(!o.unread_ok, "unread={bad:?} must not lift the gate");
    }
    let (o, tail) = take_leading_options(input_of("send"), "-- unread=ok");
    assert!(!o.unread_ok);
    assert_eq!(tail, "unread=ok");
    let (o, tail) = take_leading_options(input_of("send"), "hello unread=ok");
    assert!(!o.unread_ok);
    assert_eq!(tail, "hello unread=ok");
    for verb in ["turn", "paste", "feed", "ctrl"] {
        let (o, tail) = take_leading_options(input_of(verb), "unread=ok x");
        assert!(!o.unread_ok, "{verb}");
        assert_eq!(tail, "unread=ok x", "{verb}");
    }
}

#[test]
fn typed_supervisor_input_options_are_complete_exact_and_preserve_paste_bytes() {
    use aterm_agent::harness::contract::Scope;
    for verb in ["send", "key", "paste", "turn"] {
        let (options, body) = take_leading_options(
            input_of(verb),
            "harness-duty=approvals if-supervisor-revision=42 if-supervisor=h@1 enter",
        );
        assert_eq!(body, "enter", "{verb}");
        assert_eq!(options.refusal, None, "{verb}");
        let request = options.supervisor_request().unwrap();
        assert_eq!(
            (request.holder.as_str(), request.revision, request.duty),
            ("h@1", 42, Scope::Approvals)
        );
        for malformed in [
            "harness-duty=approvals enter",
            "if-supervisor-revision=42 enter",
            "if-supervisor=h@1 harness-duty=approvals enter",
            "if-supervisor=h@1 if-supervisor-revision=42 enter",
            "if-supervisor=h@1 if-supervisor-revision=+42 harness-duty=approvals enter",
            "if-supervisor=h@1 if-supervisor-revision=18446744073709551616 harness-duty=approvals enter",
            "if-supervisor=h@1 if-supervisor-revision=42 harness-duty=unknown enter",
            "if-supervisor=h@1 if-supervisor-revision=42 if-supervisor-revision=42 harness-duty=approvals enter",
            "if-supervisor=h@1 if-supervisor-revision=42 harness-duty=approvals harness-duty=approvals enter",
        ] {
            assert!(
                take_leading_options(input_of(verb), malformed)
                    .0
                    .refusal
                    .is_some(),
                "{verb}: {malformed}"
            );
        }
    }
    let (options, body) = take_leading_options(
        input_of("paste"),
        "if-supervisor=h@1 if-supervisor-revision=42 harness-duty=approvals --  literal if-supervisor=x\n tail",
    );
    assert_eq!(options.refusal, None);
    assert_eq!(body, " literal if-supervisor=x\n tail");
    assert_eq!(
        take_leading_options(input_of("paste"), "-- untouched").1,
        "-- untouched"
    );
    for verb in ["send", "key"] {
        assert!(
            take_leading_options(
                input_of(verb),
                "as=person if-supervisor=h@1 if-supervisor-revision=42 harness-duty=approvals enter"
            )
            .0
            .refusal
            .is_some()
        );
    }
}

#[test]
fn if_supervisor_is_the_claim_fence_on_every_typing_verb() {
    for verb in ["key", "send", "turn"] {
        let (o, tail) = take_leading_options(
            input_of(verb),
            "if-supervisor=aterm-harness@9 if-gen=3.15 1",
        );
        assert_eq!(
            o.if_supervisor.as_deref(),
            Some("aterm-harness@9"),
            "{verb}"
        );
        assert_eq!(o.refusal, None, "{verb}");
        let want = if verb == "turn" { "if-gen=3.15 1" } else { "1" };
        assert_eq!(tail, want, "{verb}");
    }
    let (o, tail) = take_leading_options(input_of("paste"), "if-supervisor=h@1  two spaces");
    assert_eq!(o.if_supervisor.as_deref(), Some("h@1"));
    assert_eq!(tail, " two spaces", "only the separating space goes");
    let (o, tail) = take_leading_options(input_of("paste"), "if=x if-supervisor=h@1 hi");
    assert_eq!((o.if_supervisor, tail), (None, "if=x if-supervisor=h@1 hi"));
    assert_eq!(
        take_leading_options(input_of("send"), "hello if-supervisor=h@1").1,
        "hello if-supervisor=h@1"
    );
    assert_eq!(
        take_leading_options(input_of("feed"), "if-supervisor=h@1 0d").1,
        "if-supervisor=h@1 0d"
    );
    for bad in [
        "if-supervisor= 1",
        "if-supervisor=- 1",
        "if-supervisor=a\u{7f} 1",
    ] {
        assert_eq!(
            take_leading_options(input_of("key"), bad)
                .0
                .refusal
                .as_deref(),
            Some(IF_SUPERVISOR_USAGE),
            "{bad:?}"
        );
    }
    assert_eq!(
        take_leading_options(input_of("key"), "if-supervisor=a if-supervisor=a 1")
            .0
            .refusal
            .as_deref(),
        Some("ERR usage: if-supervisor= given twice\n")
    );
    assert!(
        KEY_USAGE.contains("[if-supervisor=<holder>]"),
        "{KEY_USAGE}"
    );
}

#[test]
fn as_person_is_the_seam_on_key_and_send_and_takes_no_fence() {
    for verb in ["key", "send"] {
        let (o, tail) = take_leading_options(input_of(verb), "as=person enter");
        assert!(o.as_person, "{verb}");
        assert_eq!(o.refusal, None, "{verb}");
        assert_eq!(tail, "enter", "{verb}");
    }
    let (o, _) = take_leading_options(input_of("key"), "as=person as=person 1");
    assert_eq!(
        o.refusal.as_deref(),
        Some("ERR usage: as=person given twice\n")
    );
    for fenced in [
        "as=person if=x 1",
        "if-gen=3.15 as=person 1",
        "as=person if-fp=ab 1",
        "as=person if-human=2 1",
        "id=a:b:c as=person 1",
        "as=person act=answer 1",
        "as=person if-supervisor=h 1",
    ] {
        let (o, tail) = take_leading_options(input_of("key"), fenced);
        assert!(
            o.refusal
                .as_deref()
                .is_some_and(|r| r.starts_with("ERR usage: as=person takes no fence")),
            "{fenced:?} must be refused: {:?}",
            o.refusal
        );
        assert_eq!(tail, "1", "{fenced:?}");
    }
    let (o, _) = take_leading_options(input_of("send"), "as=person cause=@1 hi");
    assert!(o.refusal.is_some(), "a person's write names no cause");
    let mut off = LeadingInputOptions::default();
    take_as_person(&mut off, false);
    assert!(!off.as_person);
    assert_eq!(off.refusal.as_deref(), Some(PERSON_SEAM_REFUSED));
    let (o, tail) = take_leading_options(input_of("send"), "as=persona hi");
    assert!(!o.as_person);
    assert_eq!(o.refusal, None);
    assert_eq!(tail, "as=persona hi");
    assert_eq!(
        take_leading_options(input_of("paste"), "as=person x").1,
        "as=person x"
    );
    assert_eq!(
        take_leading_options(input_of("send"), "hello as=person").1,
        "hello as=person"
    );
    assert!(
        !take_leading_options(input_of("send"), "hello as=person")
            .0
            .as_person
    );
}

#[test]
fn if_human_is_the_person_fence_on_key_and_send() {
    for verb in ["key", "send"] {
        let (o, tail) = take_leading_options(input_of(verb), "if-human=7 if-gen=3.15 1");
        assert_eq!(o.if_human, Some(7), "{verb}");
        assert_eq!(o.refusal, None, "{verb}");
        assert_eq!(tail, "1", "{verb}");
    }
    for bad in ["", "-1", "x", "1.5", "0x7"] {
        let line = format!("if-human={bad} 1");
        let (o, tail) = take_leading_options(input_of("key"), &line);
        assert!(
            o.refusal
                .as_deref()
                .is_some_and(|r| r.starts_with("ERR usage: if-human=")),
            "if-human={bad:?} must be refused"
        );
        assert_eq!(tail, "1");
    }
    let (o, _) = take_leading_options(input_of("key"), "if-human=1 if-human=1 1");
    assert_eq!(
        o.refusal.as_deref(),
        Some("ERR usage: if-human= given twice\n")
    );
    assert_eq!(
        take_leading_options(input_of("paste"), "if-human=1 x").1,
        "if-human=1 x"
    );
    assert_eq!(
        take_leading_options(input_of("send"), "hello if-human=1").1,
        "hello if-human=1"
    );
    assert!(
        InputFence {
            human: Some(1),
            ..InputFence::default()
        }
        .is_armed()
    );
    assert!(KEY_USAGE.contains("[if-human=<n>]"), "{KEY_USAGE}");
}

#[test]
fn if_gen_is_the_fence_and_if_seq_is_refused_not_typed() {
    use crate::control::ScreenGen;
    let (o, tail) = take_leading_options(input_of("key"), "if-gen=3.15 if-fp=00ff 1");
    assert_eq!(o.if_gen, Some(ScreenGen { epoch: 3, seq: 15 }));
    assert_eq!(o.if_fp, Some(0xff));
    assert_eq!(o.refusal, None);
    assert_eq!(tail, "1");
    assert_eq!(ScreenGen { epoch: 3, seq: 15 }.to_string(), "3.15");
    for bad in ["15", "3.", ".15", "3.15.1", "-3.15", "3.+15", "x.1", ""] {
        let (o, _) = take_leading_options(input_of("key"), &format!("if-gen={bad} 1"));
        assert!(
            o.refusal
                .as_deref()
                .is_some_and(|r| r.starts_with("ERR usage: if-gen=")),
            "if-gen={bad:?} must be refused"
        );
    }
    let (o, _) = take_leading_options(input_of("key"), "if-gen=1.2 if-gen=1.2 1");
    assert_eq!(
        o.refusal.as_deref(),
        Some("ERR usage: if-gen= given twice\n")
    );
    for verb in ["key", "send"] {
        let (o, tail) = take_leading_options(input_of(verb), "if-seq=15 1");
        assert!(
            o.refusal
                .as_deref()
                .is_some_and(|r| r.starts_with("ERR usage: if-seq= is not a fence")),
            "{verb}: {o:?}"
        );
        assert_eq!(tail, "1", "{verb}: the option is taken, not left as body");
    }
    assert_eq!(
        take_leading_options(input_of("send"), "hello if-seq=15").1,
        "hello if-seq=15"
    );
}

#[test]
fn a_guard_compiles_outside_the_lock_and_a_bad_one_is_badregex() {
    assert!(compile_guard(None).unwrap().is_none());
    assert!(compile_guard(Some("Do.you.want")).unwrap().is_some());
    assert_eq!(compile_guard(Some("(")).unwrap_err(), "ERR badregex\n");
}

#[test]
fn a_find_reply_names_no_match_position_when_there_is_no_match() {
    let status = crate::app_search::FindStatus {
        open: true,
        query: "needle".to_string(),
        matches: 0,
        current: None,
        current_match: None,
        ..crate::app_search::FindStatus::CLOSED
    };
    let line = find_status_line(&status);
    assert_eq!(
        line,
        "OK open=1 query=needle case=0 regex=0 regex_error=0 matches=0 current=- \
         row=- col=- len=- truncated=0 stale=0\n"
    );
    let line = find_status_line(&crate::app_search::FindStatus {
        open: true,
        query: "needle".to_string(),
        matches: 7,
        current: Some(2),
        current_match: Some((-4, 11, 16)),
        ..crate::app_search::FindStatus::CLOSED
    });
    assert!(
        line.contains("matches=7 current=3 row=-4 col=11 len=6"),
        "{line}"
    );
}

#[test]
fn a_find_reply_marks_a_count_the_terminal_has_already_moved_past() {
    let counted = crate::app_search::FindStatus {
        open: true,
        query: "needle".to_string(),
        matches: 3,
        current: Some(0),
        current_match: Some((1, 0, 5)),
        ..crate::app_search::FindStatus::CLOSED
    };
    assert!(
        find_status_line(&counted).contains("matches=3 current=1")
            && find_status_line(&counted).contains("stale=0"),
        "a fresh count is stated plainly: {}",
        find_status_line(&counted)
    );

    let moved = crate::app_search::FindStatus {
        stale: true,
        ..counted
    };
    let line = find_status_line(&moved);
    assert!(
        line.contains("matches=3 current=1"),
        "the last real census is still reported: {line}"
    );
    assert!(
        line.contains("stale=1"),
        "…and is marked as older than the screen: {line}"
    );
}

#[test]
fn a_closed_find_bar_answers_only_that_it_is_closed() {
    assert_eq!(
        find_status_line(&crate::app_search::FindStatus::CLOSED),
        "OK open=0\n"
    );
}

#[test]
fn find_type_carries_its_text_verbatim_including_the_spaces() {
    use crate::app_search::FindAction;
    assert_eq!(
        parse_find("type hello world"),
        Ok(FindAction::Type("hello world".to_string()))
    );
    assert_eq!(
        parse_find("type  leading"),
        Ok(FindAction::Type(" leading".to_string()))
    );
    assert_eq!(
        parse_find("type trailing "),
        Ok(FindAction::Type("trailing ".to_string()))
    );
}

#[test]
fn a_bare_find_form_refuses_an_argument_it_cannot_honour() {
    for line in ["next 3", "prev 2", "open now", "cancel please", "case on"] {
        assert!(
            parse_find(line).is_err(),
            "find {line:?} must not be accepted"
        );
    }
    use crate::app_search::FindAction;
    assert_eq!(parse_find("next"), Ok(FindAction::Step { forward: true }));
    assert_eq!(parse_find("prev"), Ok(FindAction::Step { forward: false }));
    assert_eq!(parse_find("case"), Ok(FindAction::ToggleCase));
    assert_eq!(parse_find(""), Ok(FindAction::Status));
}

#[test]
fn pointer_move_refuses_a_cell_it_cannot_address() {
    use crate::app_mouse::{PointerAction, PointerAt};
    assert_eq!(
        parse_pointer("move 3 9"),
        Ok(PointerAction::Move { row: 3, col: 9 })
    );
    assert_eq!(parse_pointer("leave"), Ok(PointerAction::Leave));
    assert_eq!(
        parse_pointer("move key=settings/cursor-style/comet"),
        Ok(PointerAction::MoveNative(
            crate::app_mouse::NativePointerTarget::Key("settings/cursor-style/comet".into())
        ))
    );
    assert_eq!(
        parse_pointer("move xy=120.5,64"),
        Ok(PointerAction::MoveNative(
            crate::app_mouse::NativePointerTarget::Point(120.5, 64.0)
        ))
    );
    assert_eq!(parse_pointer("move item=4"), Ok(PointerAction::MoveItem(4)));
    for junk in [
        "move item=",
        "move item=-1",
        "move key=",
        "move xy=1",
        "move xy=a,b",
        "move xy=inf,2",
        "move 3",
    ] {
        assert!(parse_pointer(junk).is_err(), "{junk}");
    }
    assert_eq!(parse_pointer(""), Ok(PointerAction::Status));
    assert_eq!(
        parse_pointer(&format!("move {MAX_GRID_ROWS} 0")),
        Err("ERR out of range\n".to_string())
    );
    assert_eq!(
        parse_pointer(&format!("move 0 {MAX_GRID_COLS}")),
        Err("ERR out of range\n".to_string())
    );
    for line in [
        "move 1",
        "move 1 2 3",
        "move a b",
        "leave now",
        "wiggle",
        "mods",
        "mods hyper",
        "mods cmd+",
        "mods cmd shift",
    ] {
        assert!(parse_pointer(line).is_err(), "pointer {line:?}");
    }
    use aterm_window::keyboard::ModifiersState as M;
    assert_eq!(
        parse_pointer("mods none"),
        Ok(PointerAction::Mods(M::empty()))
    );
    assert_eq!(
        parse_pointer("mods cmd+alt"),
        Ok(PointerAction::Mods(M::SUPER | M::ALT))
    );
    let link = parse_pointer("mods link");
    assert!(
        matches!(link, Ok(PointerAction::Mods(m)) if crate::app_mouse::link_modifier_held(m)),
        "{link:?}"
    );
    assert_eq!(
        parse_pointer("move -1 5"),
        Ok(PointerAction::MoveChrome { up: 1, col: 5 })
    );
    assert_eq!(
        parse_pointer(&format!("move -{} 0", u16::MAX)),
        Ok(PointerAction::MoveChrome {
            up: u16::MAX,
            col: 0
        })
    );
    assert_eq!(
        parse_pointer(&format!("move -{} 0", u32::from(u16::MAX) + 1)),
        Err("ERR out of range\n".to_string())
    );
    assert_eq!(parse_pointer("click"), Ok(PointerAction::Click));
    assert!(parse_pointer("click right").is_err());
    for (reply, expected) in [
        (
            Ok(PointerAt::Grid {
                cell: (3, 9),
                px: Some((100, 120)),
            }),
            "OK at=3,9 px=100,120\n",
        ),
        (
            Ok(PointerAt::Grid {
                cell: (-1, 5),
                px: None,
            }),
            "OK at=-1,5\n",
        ),
        (
            Ok(PointerAt::Native {
                view: 2,
                x: 120.5,
                y: 64.0,
                key: Some("editor/buffer".to_string()),
                px: Some((241, 128)),
            }),
            "OK at=native view=2 x=120.5 y=64.0 key=editor/buffer px=241,128\n",
        ),
        (
            Ok(PointerAt::Native {
                view: 2,
                x: 0.0,
                y: 1.0,
                key: None,
                px: None,
            }),
            "OK at=native view=2 x=0.0 y=1.0 key=-\n",
        ),
        (Ok(PointerAt::Absent), "OK at=-\n"),
        (Err("refused".to_string()), "ERR refused\n"),
    ] {
        assert_eq!(pointer_reply_line(reply), expected);
    }
    let detail = "frame\n\t100% λ";
    let line = pointer_reply_line(Ok(PointerAt::Dispatched {
        readback_error: detail.to_string(),
    }));
    assert_eq!(
        line,
        "OK dispatched=1 readback=unavailable detail=frame%0A%09100%25%20%CE%BB\n"
    );
    let encoded = line
        .split_whitespace()
        .find_map(|word| word.strip_prefix("detail="))
        .unwrap();
    assert_eq!(aterm_control::wire::pct_decode(encoded), detail);
}

#[test]
fn pointer_wheel_parses_its_deltas_unit_and_aim() {
    use crate::app_mouse::{NativePointerTarget, PointerAction, WheelUnit};
    let wheel = |dx: f64, dy: f64, unit, aim| PointerAction::Wheel { dx, dy, unit, aim };
    assert_eq!(
        parse_pointer("wheel dy=3"),
        Ok(wheel(0.0, 3.0, WheelUnit::Lines, None))
    );
    assert_eq!(
        parse_pointer("wheel unit=pt dx=-12.5 dy=40 key=settings/choice/Nord"),
        Ok(wheel(
            -12.5,
            40.0,
            WheelUnit::Points,
            Some(NativePointerTarget::Key("settings/choice/Nord".into()))
        ))
    );
    assert_eq!(
        parse_pointer("wheel xy=300,120 dy=-0.5 unit=lines"),
        Ok(wheel(
            0.0,
            -0.5,
            WheelUnit::Lines,
            Some(NativePointerTarget::Point(300.0, 120.0))
        ))
    );
    assert_eq!(
        parse_pointer(&format!("wheel dy={MAX_POINTER_WHEEL}")),
        Ok(wheel(0.0, MAX_POINTER_WHEEL, WheelUnit::Lines, None))
    );
    assert_eq!(
        parse_pointer("wheel dy=10001"),
        Err("ERR out of range\n".to_string())
    );
    for junk in [
        "wheel",
        "wheel dy=0",
        "wheel dy=0 dx=0",
        "wheel 3",
        "wheel dy=3 dy=4",
        "wheel dy=nan",
        "wheel dy=inf",
        "wheel dy=x",
        "wheel dy=3 unit=px",
        "wheel dy=3 unit=pt unit=lines",
        "wheel dy=3 key=",
        "wheel dy=3 key=a xy=1,2",
        "wheel dy=3 xy=1",
        "wheel dy=3 xy=1,inf",
        "wheel dy=3 down",
    ] {
        assert_eq!(
            parse_pointer(junk),
            Err(POINTER_WHEEL_USAGE.to_string()),
            "pointer {junk:?}"
        );
    }
}

#[test]
fn parse_mouse_wheel_lines() {
    let Ok(InputEvent::Wheel { lines, dir, .. }) = parse_mouse("wheeldown left 2 4 lines=8") else {
        panic!("wheeldown lines=8 parses");
    };
    assert_eq!(lines, 8);
    assert_eq!(dir, aterm_types::mouse::WheelDir::Down);
    let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse("wheelup left 2 4") else {
        panic!("bare wheelup parses");
    };
    assert_eq!(lines, 1);
    let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse("wheelup left 0 0 lines=100000") else {
        panic!("over-cap parses");
    };
    assert_eq!(lines, MAX_WHEEL_LINES);
    for line in ["wheelup left 0 0 lines=0", "wheelup left 0 0 lines=abc"] {
        let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse(line) else {
            panic!("{line} parses");
        };
        assert_eq!(lines, 1, "{line}");
    }
}

#[test]
fn parse_mouse_covers_every_wheel_direction_and_button() {
    use aterm_types::mouse::{MouseButton, WheelDir};
    for (line, want) in [
        ("wheelup left 2 4", WheelDir::Up),
        ("wheeldown left 2 4", WheelDir::Down),
        ("wheelleft left 2 4", WheelDir::Left),
        ("wheelright left 2 4", WheelDir::Right),
    ] {
        let Ok(InputEvent::Wheel { dir, .. }) = parse_mouse(line) else {
            panic!("{line} parses");
        };
        assert_eq!(dir, want, "{line}");
    }
    for (line, want) in [
        ("press back 2 4", MouseButton::Back),
        ("press forward 2 4", MouseButton::Forward),
    ] {
        let Ok(InputEvent::MouseButton { button, .. }) = parse_mouse(line) else {
            panic!("{line} parses");
        };
        assert_eq!(button, want, "{line}");
    }
    assert!(parse_mouse("press thumb3 2 4").is_err());
}
