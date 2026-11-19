// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::tab_model::ViewId;

#[test]
fn cpu_policy_fixed_sudo_arguments_reach_authentication_without_a_prompt() {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/sudo");
    command
        .args(SUDO_ARGS.map(|arg| if arg == "-A" { "-n" } else { arg }))
        .arg("/usr/bin/true")
        .env("LC_ALL", "C");
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60))
        .expect("fixed noninteractive sudo invocation completed");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !error.contains("usage:") && !error.contains("Only one of"),
        "sudo rejected its fixed arguments: {error}"
    );
    assert!(
        output.status.success()
            || error.contains("a password is required")
            || error.contains("not allowed to execute")
            || error.contains("not in the sudoers file")
            || error.contains("authentication failure"),
        "sudo did not reach command authorization: {error}"
    );
}

#[test]
fn cpu_policy_no_askpass_ack_cannot_bypass_current_admission() {
    let claims = Claims::default();
    assert!(!claims.released());
    let writes = std::cell::Cell::new(0);
    for (live, halted, off) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        assert!(!issue_ack(
            || admission_refusal(live, halted, off).is_none(),
            || false,
            || {
                writes.set(writes.get() + 1);
                true
            }
        ));
    }
    assert_eq!(writes.get(), 0, "NOPASSWD still cannot reach the ACK write");
    assert!(!issue_ack(
        || true,
        || true,
        || {
            writes.set(writes.get() + 1);
            true
        }
    ));
    assert_eq!(writes.get(), 0, "cancellation after permit forbids ACK");
    assert!(issue_ack(
        || true,
        || false,
        || {
            writes.set(writes.get() + 1);
            true
        }
    ));
    assert_eq!(writes.get(), 1);
}

#[test]
fn cpu_policy_no_askpass_still_requires_live_origin_halt_and_relay_admission() {
    assert_eq!(admission_refusal(true, false, false), None);
    assert_eq!(
        admission_refusal(false, false, false),
        Some(Refusal::NotInFront)
    );
    assert_eq!(admission_refusal(true, true, false), Some(Refusal::Halted));
    assert_eq!(
        admission_refusal(true, false, true),
        Some(Refusal::RelayOff)
    );
    assert!(
        !owns_no_live_collector(None, false, false, false, false, false),
        "NOPASSWD may start a collector without any secret"
    );
}

#[test]
fn cpu_policy_control_is_owner_only_and_has_no_command_or_path_payload() {
    use crate::control::Scope;
    assert_eq!(admit(Scope::Owner, "policy"), Ok(Action::Status));
    assert_eq!(
        admit(Scope::Owner, "policy collect --json"),
        Ok(Action::Collect)
    );
    assert_eq!(admit(Scope::Owner, "policy cancel"), Ok(Action::Cancel));
    assert!(admit(Scope::Bridge, "policy collect").is_err());
    for (words, action) in [
        ("policy caller=s-origin_1", Action::Status),
        ("policy status caller=s-origin_1 --json", Action::Status),
        ("policy status --json caller=s-origin_1", Action::Status),
        ("policy collect caller=s-origin_1 --json", Action::Collect),
        ("policy collect --json caller=s-origin_1", Action::Collect),
        ("policy cancel caller=s-origin_1 --json", Action::Cancel),
        ("policy cancel --json caller=s-origin_1", Action::Cancel),
    ] {
        assert_eq!(admit(Scope::Owner, words), Ok(action), "{words}");
        assert!(admit(Scope::Bridge, words).is_err(), "{words}");
    }
    for arg in [
        "policy collect /bin/sh",
        "policy collect path=/tmp/report",
        "policy collect selector=7",
        "policy collect instances=all",
        "policy collect --force",
        "policy collect caller=",
        "policy collect caller=s/other",
        "policy collect caller=a%20b",
        "policy collect caller=s-1 caller=s-2",
        "policy collect caller=s-1 --json caller=s-1",
    ] {
        assert!(admit(Scope::Owner, arg).is_err(), "{arg}");
    }
    let boundary = format!("policy collect caller={}", "a".repeat(64));
    assert_eq!(admit(Scope::Owner, &boundary), Ok(Action::Collect));
    let too_long = format!("policy collect caller={}", "a".repeat(65));
    assert!(admit(Scope::Owner, &too_long).is_err());
}

#[test]
fn cpu_policy_closed_or_replaced_origin_cannot_authorize() {
    assert!(origin_matches("origin", Some("origin"), true));
    assert!(!origin_matches("origin", Some("replacement"), true));
    assert!(!origin_matches("origin", Some("origin"), false));
    assert!(!origin_matches("origin", None, true));
}

#[test]
fn cpu_policy_request_and_secret_are_each_claimed_once() {
    let claims = Arc::new(Claims::default());
    assert!(!claims.secret(), "no unrequested secret release");
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let claims = Arc::clone(&claims);
            std::thread::spawn(move || claims.request())
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert!(claims.secret());
    assert!(!claims.secret());
    assert!(
        !claims.request(),
        "sudo password retries do not create another consent"
    );
}

#[test]
fn cpu_policy_wrapper_exit_does_not_prove_collector_exit() {
    let id = Identity {
        pid: 1,
        start_s: 100,
        start_us: 0,
    };
    assert!(!owns_no_live_collector(
        Some(id),
        false,
        true,
        true,
        false,
        false
    ));
    assert!(owns_no_live_collector(
        Some(id),
        true,
        true,
        true,
        false,
        false
    ));
    assert!(!owns_no_live_collector(
        None, false, true, true, false, false
    ));
    assert!(owns_no_live_collector(
        None, false, false, true, false, false
    ));
    assert!(
        owns_no_live_collector(None, false, true, true, true, false),
        "a refused retry occurs before sudo can execute the collector"
    );
    let stdout = PipeState::default();
    let stderr = PipeState::default();
    stdout.done.store(true, Ordering::Release);
    assert!(
        !readers_finished(&stdout, &stderr, &None, &None),
        "stderr may still have a writer"
    );
    stderr.done.store(true, Ordering::Release);
    assert!(readers_finished(&stdout, &stderr, &None, &None));
    assert!(
        !stderr.eof.load(Ordering::Acquire),
        "completion and true EOF remain distinct"
    );
}

#[test]
fn cpu_policy_unbound_sudo_refusal_requires_reaping_and_true_owned_pipe_eof() {
    let stdout = PipeState::default();
    let stderr = PipeState::default();
    let drained = || owned_pipes_drained(true, &stdout, &stderr, &None, &None);
    assert!(!drained(), "wrapper exit alone leaves an unknown collector");
    stdout.done.store(true, Ordering::Release);
    stderr.done.store(true, Ordering::Release);
    assert!(!drained(), "read errors may finish without EOF");
    stdout.eof.store(true, Ordering::Release);
    assert!(!drained(), "one pipe still has an unknown writer");
    stderr.eof.store(true, Ordering::Release);
    assert!(!owned_pipes_drained(false, &stdout, &stderr, &None, &None));
    assert!(drained());
    assert!(
        owns_no_live_collector(None, false, false, false, false, drained()),
        "sudo can refuse before requesting any password"
    );
    assert!(
        owns_no_live_collector(None, false, true, true, false, drained()),
        "an authorized collector may exit before its hello"
    );
    let id = Identity {
        pid: 1,
        start_s: 100,
        start_us: 0,
    };
    assert!(
        !owns_no_live_collector(Some(id), false, true, true, false, drained()),
        "a bound generation still needs its independent exit observation"
    );
}

#[test]
fn cpu_policy_output_limit_and_partial_line_do_not_forge_eof_or_capture() {
    let (tx, rx) = std::sync::mpsc::channel();
    let state = PipeState::default();
    read_output(
        std::io::Cursor::new(b"{\"x\":1}\n{\"x\":2}\n"),
        &tx,
        &state,
        9,
    );
    assert!(state.eof.load(Ordering::Acquire));
    assert!(state.done.load(Ordering::Acquire));
    assert_eq!(rx.try_recv().unwrap(), Err("collector_output_limit"));
    assert!(rx.try_recv().is_err());
    let (tx, rx) = std::sync::mpsc::channel();
    let state = PipeState::default();
    read_output(std::io::Cursor::new(b"{\"x\":1}"), &tx, &state, 100);
    assert_eq!(rx.try_recv().unwrap(), Err("collector_output_incomplete"));
}

#[test]
fn cpu_policy_identity_parser_rejects_missing_reused_and_bad_generation() {
    let value = json!({"pid":1,"sec":"100","usec":5});
    assert_eq!(
        parsed_identity(&value, "pid", "sec", "usec"),
        Some(Identity {
            pid: 1,
            start_s: 100,
            start_us: 5
        })
    );
    assert!(
        parsed_identity(&json!({"pid":0,"sec":"100","usec":5}), "pid", "sec", "usec").is_none()
    );
    assert!(parsed_identity(&json!({"pid":1,"sec":"0","usec":5}), "pid", "sec", "usec").is_none());
    assert!(
        parsed_identity(
            &json!({"pid":1,"sec":"100","usec":1000000}),
            "pid",
            "sec",
            "usec"
        )
        .is_none()
    );
}

/// The front window's tab whose view is `view`, by its place among the window's tabs.
fn tab_index_of(app: &App, wid: WindowId, view: ViewId) -> usize {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| {
            let mut found = false;
            tab.root.any_leaf(&mut |&leaf| {
                found = leaf == view;
                found
            });
            found
        })
        .expect("the view's tab")
}

/// A PAGE'S COLLECT BINDS CPU POLICY TO THE PAGE'S VIEW: with the System Resources page in front
/// of the window, and so no terminal session in front, a collect from the page's view is
/// admitted and its operation stands for that view. It ends when the view leaves the front: a
/// terminal tab brought in front, the view's tab closed and the view's window closed each cancel
/// the operation then standing.
/// CONTROL: today's session-bound collect, from the same window with the page in front, is
/// refused for want of a terminal session in front and starts nothing.
#[test]
fn a_page_collect_binds_cpu_policy_to_the_pages_view_and_ends_when_the_view_leaves_the_front() {
    const WID: WindowId = WindowId(0);
    let slot = slot_for_test();
    let mut app = App::headless_for_test();
    let terminal = app.windows[&WID].tab_set.active().expect("a tab").focus;
    let page_in_front = |app: &mut App| {
        app.open_resources_tab().expect("the page opens");
        let (_, view) = app.active_native_view(WID).expect("the page is in front");
        view
    };
    let collect = |app: &mut App, view: ViewId| {
        assert_eq!(
            app.collect_cpu_policy_from_view(WID, view, Starter::Person(Instant::now())),
            Ok(()),
            "the page's collect is admitted"
        );
        let op = active_for_test().expect("the operation stands in the slot");
        assert_eq!(op.origin(), &Origin::View { window: WID, view });
        assert!(
            app.cpu_policy_ui_admission(&op) && !op.cancelled_for_test(),
            "it is admitted while the view is in front"
        );
        op
    };

    let view = page_in_front(&mut app);
    assert_eq!(
        app.focused_session_id(WID),
        None,
        "PRECONDITION: no terminal session is in front"
    );
    assert_eq!(
        app.collect_cpu_policy(WID, Starter::Control),
        Err(Refusal::NoSession),
        "CONTROL: the session-bound collect is refused with the page in front"
    );
    assert!(!app.cpu_policy_pending(), "CONTROL: and starts nothing");

    let op = collect(&mut app, view);
    assert!(app.cpu_policy_pending());
    let back = tab_index_of(&app, WID, terminal);
    app.switch_tab_in(WID, back);
    assert!(
        op.cancelled_for_test() && !app.cpu_policy_ui_admission(&op),
        "a terminal tab in front cancels it"
    );

    slot.empty();
    let view = page_in_front(&mut app);
    let op = collect(&mut app, view);
    let at = tab_index_of(&app, WID, view);
    let tabs = app.windows[&WID].tab_set.len();
    app.close_tab_at(WID, at);
    assert_eq!(
        app.windows[&WID].tab_set.len(),
        tabs - 1,
        "PRECONDITION: the view's tab closed"
    );
    assert!(op.cancelled_for_test(), "closing the view cancels it");

    slot.empty();
    let view = page_in_front(&mut app);
    let op = collect(&mut app, view);
    app.close_window_logical(WID);
    assert!(
        op.cancelled_for_test() && !app.cpu_policy_ui_admission(&op),
        "closing the view's window cancels it"
    );
}

/// One App's operation is no other App's: a second headless App beside the one that started it
/// does not read it as pending, does not cancel it by closing a window or a tab of the same ids,
/// by its own Cancel, or by its withdrawal of a view that is not in front, and refuses a collect
/// of its own while it stands. The App that started it still reads it pending and cancels it.
#[test]
fn another_apps_cpu_policy_operation_is_not_this_ones() {
    const WID: WindowId = WindowId(0);
    let _slot = slot_for_test();
    let mut ours = App::headless_for_test();
    ours.open_resources_tab().expect("the page opens");
    let (_, view) = ours.active_native_view(WID).expect("the page is in front");
    assert_eq!(
        ours.collect_cpu_policy_from_view(WID, view, Starter::Person(Instant::now())),
        Ok(())
    );
    let op = active_for_test().expect("the operation stands in the slot");

    let mut theirs = App::headless_for_test();
    assert_ne!(theirs.cpu_policy_owner(), ours.cpu_policy_owner());
    assert!(
        !theirs.cpu_policy_pending(),
        "it is not the other App's pending one"
    );
    assert_eq!(pending_view(theirs.cpu_policy_owner()), None);
    assert!(
        !theirs.cpu_policy_ui_admission(&op),
        "the other App admits no origin of it"
    );
    theirs.withdraw_askpass_view_unless_front();
    theirs.withdraw_askpass_view(view);
    theirs.cancel_cpu_policy_from_page();
    theirs.cancel_cpu_policy(WID);
    theirs.close_window_logical(WID);
    assert!(
        !op.cancelled_for_test(),
        "nothing the other App does cancels it"
    );
    let mut third = App::headless_for_test();
    third.open_resources_tab().expect("the page opens");
    let (_, other_view) = third.active_native_view(WID).expect("the page is in front");
    assert_eq!(
        third.collect_cpu_policy_from_view(WID, other_view, Starter::Person(Instant::now())),
        Err(Refusal::Pending),
        "the machine runs one collection at a time"
    );

    assert!(
        ours.cpu_policy_pending(),
        "CONTROL: it is its own App's pending one"
    );
    ours.cancel_cpu_policy_from_page();
    assert!(
        op.cancelled_for_test(),
        "CONTROL: its own App's Cancel ends it"
    );
}

/// The collector's `sudo` asks this instance, by its own control socket, for the password, and
/// carries no session id that could route its askpass helper to another aterm.
#[test]
fn the_collectors_sudo_asks_this_instance_by_its_socket() {
    let command = sudo_command(
        Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
        Path::new("/Applications/aterm.app/Contents/MacOS/aterm-askpass"),
        "/tmp/aterm-test.sock",
    );
    let envs: Vec<(String, Option<String>)> = command
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let value = |key: &str| envs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    assert_eq!(
        value(aterm_uds::sudo::ASKPASS_SOCK_ENV),
        Some(Some("/tmp/aterm-test.sock".into())),
        "the helper asks this instance's socket"
    );
    assert_eq!(
        value("ATERM_PARENT_SESSION_ID"),
        Some(None),
        "an inherited session id is removed"
    );
    assert_eq!(
        value("SUDO_ASKPASS"),
        Some(Some(
            "/Applications/aterm.app/Contents/MacOS/aterm-askpass".into()
        ))
    );
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "-k",
            "-A",
            "--",
            "/Applications/aterm.app/Contents/MacOS/aterm",
            COLLECTOR_VERB
        ]
    );
}
