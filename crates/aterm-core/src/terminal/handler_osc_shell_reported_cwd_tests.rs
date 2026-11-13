// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::{ShellIntegrationNonce, Terminal};

const KEY: [u8; 32] = [0x5Du8; 32];

fn signed(mark: &str) -> Vec<u8> {
    format!(
        "\x1b]133;{mark};id={}\x07",
        ShellIntegrationNonce(KEY).to_hex()
    )
    .into_bytes()
}

fn osc7(path: &str) -> Vec<u8> {
    format!("\x1b]7;file://host{path}\x07").into_bytes()
}

fn gated_at_first_prompt() -> Terminal {
    let mut t = Terminal::new(6, 60);
    t.set_osc_event_polling(true);
    t.authorize_shell_integration(KEY);
    t.set_require_shell_integration_nonce(true);
    t.process(&osc7("/home/me"));
    t.process(&signed("A"));
    t.process(b"$ ");
    t.process(&signed("B"));
    t
}

fn at_prompt() -> Terminal {
    let mut t = gated_at_first_prompt();
    t.process(b"true\r\n");
    t.process(&signed("C"));
    t.process(&signed("D;0"));
    t.process(&osc7("/home/me"));
    t.process(&signed("A"));
    t.process(b"$ ");
    t.process(&signed("B"));
    while t.take_osc_event().is_some() {}
    t
}

fn own_report(path: &str) -> Vec<u8> {
    format!(
        "\x1b]633;P;AtermCwd=file://host{path};id={}\x07\x1b]7;file://host{path}\x07",
        ShellIntegrationNonce(KEY).to_hex()
    )
    .into_bytes()
}

fn signing_at_prompt() -> Terminal {
    let mut t = Terminal::new(6, 60);
    t.set_osc_event_polling(true);
    t.authorize_shell_integration(KEY);
    t.set_require_shell_integration_nonce(true);
    for line in ["", "true\r\n"] {
        if !line.is_empty() {
            t.process(line.as_bytes());
            t.process(&signed("C"));
            t.process(&signed("D;0"));
        }
        t.process(&own_report("/home/me"));
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed("B"));
    }
    while t.take_osc_event().is_some() {}
    t
}

fn cwd_events(t: &mut Terminal) -> Vec<String> {
    std::iter::from_fn(|| t.take_osc_event())
        .filter(|(code, _)| *code == 7)
        .map(|(_, payload)| payload)
        .collect()
}

#[test]
fn a_stray_report_at_the_idle_prompt_sets_no_folder() {
    let mut t = at_prompt();
    t.process(&osc7("/elsewhere"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    assert_eq!(cwd_events(&mut t), Vec::<String>::new());
    t.process(&osc7("/home/me"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    assert_eq!(cwd_events(&mut t), vec!["//host/home/me".to_owned()]);
    let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
    assert_eq!(mark.working_directory.as_deref(), Some("/home/me"));

    let mut t = at_prompt();
    t.process(&osc7("/elsewhere"));
    t.process(b"ls\r\n");
    t.process(&signed("C"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    t.process(&signed("D;0"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    assert_eq!(cwd_events(&mut t), Vec::<String>::new());
}

#[test]
fn a_folder_change_at_the_prompt_lands_with_the_redrawn_prompt() {
    let mut t = at_prompt();
    t.process(&osc7("/home/me/src"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me/src"));
    let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
    assert_eq!(mark.working_directory.as_deref(), Some("/home/me/src"));
    assert_eq!(cwd_events(&mut t), vec!["//host/home/me/src".to_owned()]);
}

#[test]
fn a_report_while_a_command_runs_or_after_it_ends_lands_at_once() {
    let mut t = at_prompt();
    t.process(b"ssh build\r\n");
    t.process(&signed("C"));
    t.process(b"\x1b]133;D;0\x07");
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    t.process(b"\x1b]133;A\x07remote$ \x1b]133;B\x07");
    t.process(&osc7("/srv/app/logs"));
    assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
    assert_eq!(
        cwd_events(&mut t),
        vec![
            "//host/srv/app".to_owned(),
            "//host/srv/app/logs".to_owned()
        ]
    );
    t.process(&signed("D;0"));
    t.process(&osc7("/home/me"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
}

#[test]
fn without_the_nonce_gate_every_report_lands_at_once() {
    let mut t = Terminal::new(6, 60);
    t.set_osc_event_polling(true);
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    t.process(&osc7("/elsewhere"));
    assert_eq!(t.current_working_directory(), Some("/elsewhere"));
}

#[test]
fn a_signed_cwd_property_lands_at_once_over_a_stray_report() {
    let mut t = at_prompt();
    t.process(&osc7("/elsewhere"));
    t.process(
        format!(
            "\x1b]633;P;Cwd=/home/me/docs;id={}\x07",
            ShellIntegrationNonce(KEY).to_hex()
        )
        .as_bytes(),
    );
    assert_eq!(t.current_working_directory(), Some("/home/me/docs"));
    let oversized = format!("/{}", "a".repeat(super::MAX_CWD_PATH_BYTES));
    t.process(
        format!(
            "\x1b]633;P;Cwd={oversized};id={}\x07",
            ShellIntegrationNonce(KEY).to_hex()
        )
        .as_bytes(),
    );
    assert_eq!(t.current_working_directory(), Some("/home/me/docs"));
    assert_eq!(
        t.shell
            .current_mark
            .as_ref()
            .and_then(|m| m.working_directory.as_deref()),
        Some("/home/me/docs"),
        "the prompt's mark keeps the stored folder"
    );
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me/docs"));
}

#[test]
fn a_shell_that_marks_no_command_holds_no_report() {
    let mut t = gated_at_first_prompt();
    t.process(b"ssh build\r\n");
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    t.process(&osc7("/srv/app/logs"));
    assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
    t.process(&signed("D;0"));
    t.process(&osc7("/home/me"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
}

#[test]
fn a_degraded_successor_holds_no_report() {
    use crate::terminal::ShellIntegrationPosture;
    const REKEY: [u8; 32] = [0xA7u8; 32];
    let signed_rekeyed = |mark: &str| {
        format!(
            "\x1b]133;{mark};id={}\x07",
            ShellIntegrationNonce(REKEY).to_hex()
        )
        .into_bytes()
    };

    let t = at_prompt();
    let mut cp = t.checkpoint_carry(0).expect("an idle prompt carries");
    cp.shell_integration_nonce = None;
    let mut s = Terminal::from_checkpoint(&cp);
    assert!(s.is_require_shell_integration_nonce(), "the gate stands");
    assert_eq!(
        s.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    s.process(b"cd /srv\r\n");
    s.process(&signed("C"));
    s.process(&signed("D;0"));
    s.process(&osc7("/srv"));
    assert_eq!(s.current_working_directory(), Some("/srv"));
    s.process(&signed("A"));
    s.process(b"$ ");
    s.process(&signed("B"));
    s.process(b"ssh build\r\n");
    s.process(&signed("C"));
    s.process(&osc7("/srv/app"));
    assert_eq!(s.current_working_directory(), Some("/srv/app"));

    s.authorize_shell_integration_on_first_mark(REKEY);
    s.process(&osc7("/srv/app/logs"));
    assert_eq!(s.current_working_directory(), Some("/srv/app/logs"));
    s.process(&signed_rekeyed("D;0"));
    s.process(&osc7("/srv"));
    s.process(&signed_rekeyed("A"));
    assert_eq!(s.shell_integration_posture(), ShellIntegrationPosture::On);
    assert_eq!(s.current_working_directory(), Some("/srv"));
    s.process(b"$ ");
    s.process(&signed_rekeyed("B"));
    s.process(&osc7("/elsewhere"));
    assert_eq!(s.current_working_directory(), Some("/srv"));
}

/// A reset while the shell sits at its prompt (the GUI's manual reset is a RIS) keeps the cwd
/// the shell reported there for its next `A`, as it keeps the phase that report waits in.
/// NEGATIVE CONTROL: with `ShellIntegrationState::reset` dropping `held_cwd`, `` the next A
/// applies it `` fails.
#[test]
fn a_reset_at_the_prompt_keeps_the_shells_held_cwd() {
    for host in [false, true] {
        let mut t = at_prompt();
        t.process(&osc7("/held"));
        assert_eq!(
            t.current_working_directory(),
            Some("/home/me"),
            "PRECONDITION: held"
        );
        if host {
            t.reset();
        } else {
            t.process(b"\x1bc");
        }
        t.process(&signed("A"));
        assert_eq!(
            t.current_working_directory(),
            Some("/held"),
            "host reset {host}: the next A applies it"
        );
    }
}

/// The host's word that a program runs since the prompt outlives a reset: replayed from the
/// journal (no live probe), `probe_job_active` is the only record of it, and a job's report at
/// the prompt lands. NEGATIVE CONTROL: with `ShellIntegrationState::reset` clearing
/// `probe_job_active`, `` the job's report lands `` fails.
#[test]
fn a_reset_keeps_the_hosts_probe_verdict() {
    for host in [false, true] {
        let mut t = at_prompt();
        t.apply_host_op(crate::terminal::HostOp::ForegroundJob {
            active: false,
            probed: true,
            restore_cwd: false,
        });
        if host {
            t.reset();
        } else {
            t.process(b"\x1bc");
        }
        t.process(&osc7("/srv/app"));
        assert_eq!(
            t.current_working_directory(),
            Some("/srv/app"),
            "host reset {host}: the job's report lands"
        );
    }
}

/// A probe edge the batch recorded before its RIS still reaches the host journal at the
/// batch's end, so a replay sees the verdict the reset kept. NEGATIVE CONTROL: with
/// `ShellIntegrationState::reset` clearing `probe_transitioned`, `` RIS true: the edge is
/// journaled `` fails.
#[test]
fn a_ris_keeps_the_batchs_probe_edge_for_the_journal() {
    for ris in [false, true] {
        let mut t = at_prompt();
        t.set_program_probe(Some(Box::new(|_: u64| true)));
        let from = t.host_ordinal();
        let tail: &[u8] = if ris { b"\x1bc" } else { b"" };
        t.process(&[osc7("/srv/app").as_slice(), tail].concat());
        let edges = t
            .host_journal_since(from)
            .0
            .into_iter()
            .filter(|r| {
                matches!(
                    r.op,
                    crate::terminal::HostOp::ForegroundJob {
                        active: false,
                        probed: true,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(edges, 1, "RIS {ris}: the edge is journaled");
    }
}

#[test]
fn an_in_place_restore_drops_the_previous_prompts_held_cwd() {
    let mut ordinary = at_prompt();
    ordinary.process(&osc7("/same-prompt"));
    ordinary.process(b"\x1b]7;https://not-a-folder\x07");
    assert_eq!(ordinary.current_working_directory(), Some("/home/me"));
    assert!(cwd_events(&mut ordinary).is_empty());
    ordinary.process(&signed("A"));
    assert_eq!(ordinary.current_working_directory(), Some("/same-prompt"));
    assert_eq!(cwd_events(&mut ordinary), vec!["//host/same-prompt"]);

    let mut source = at_prompt();
    source.process(&signed("C"));
    source.process(&signed("D;0"));
    source.process(&osc7("/adopted"));
    source.process(&signed("A"));
    source.process(&signed("B"));
    assert_eq!(source.current_working_directory(), Some("/adopted"));
    let checkpoint = source.checkpoint_carry(0).expect("a prompt carries");

    let mut fresh = Terminal::from_checkpoint(&checkpoint);
    fresh.authorize_shell_integration(KEY);
    fresh.process(&signed("A"));
    assert_eq!(fresh.current_working_directory(), Some("/adopted"));
    assert!(cwd_events(&mut fresh).is_empty());

    let mut target = at_prompt();
    target.process(&osc7("/previous-prompt"));
    assert_eq!(target.current_working_directory(), Some("/home/me"));
    assert!(cwd_events(&mut target).is_empty(), "the old report is held");
    target.restore_checkpoint(&checkpoint);
    assert_eq!(target.current_working_directory(), Some("/adopted"));
    assert_eq!(target.shell_integration_nonce_in_use(), Some(KEY));
    target.process(&signed("A"));
    assert_eq!(
        target.current_working_directory(),
        Some("/adopted"),
        "an adopted shell must not commit the previous prompt's held cwd"
    );
    assert!(cwd_events(&mut target).is_empty());
    assert_eq!(
        target
            .shell
            .current_mark
            .as_ref()
            .expect("adopted prompt")
            .working_directory
            .as_deref(),
        Some("/adopted")
    );
}

#[test]
fn the_shells_own_report_lands_at_once_with_no_nonce_in_its_uri() {
    let mut t = signing_at_prompt();
    assert!(t.shell.signs_cwd);
    t.process(&own_report("/home/me/src"));
    assert_eq!(t.current_working_directory(), Some("/home/me/src"));
    assert_eq!(cwd_events(&mut t), vec!["//host/home/me/src".to_owned()]);
    t.process(
        format!(
            "\x1b]633;P;AtermCwd=file://host/forged;id={}\x07",
            ShellIntegrationNonce([0x11; 32]).to_hex()
        )
        .as_bytes(),
    );
    assert_eq!(t.current_working_directory(), Some("/home/me/src"));
    t.process(&own_report("/home/me/src"));
    assert!(cwd_events(&mut t).is_empty(), "unchanged");
    let mut t = gated_at_first_prompt();
    while t.take_osc_event().is_some() {}
    t.process(&own_report("/srv"));
    assert_eq!(t.current_working_directory(), Some("/srv"));
    assert_eq!(cwd_events(&mut t), vec!["//host/srv".to_owned()]);
    let mut t = Terminal::new(6, 60);
    t.set_osc_event_polling(true);
    t.process(b"\x1b]7;file://host/a;id=b\x07");
    assert_eq!(t.current_working_directory(), Some("/a;id=b"));
}

#[test]
fn a_stray_between_the_shells_signed_report_and_its_prompt_sets_nothing() {
    let mut t = signing_at_prompt();
    t.process(b"cd src\r\n");
    t.process(&signed("C"));
    t.process(&signed("D;0"));
    t.process(&own_report("/home/me/src"));
    t.process(&osc7("/elsewhere"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me/src"));
    let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
    assert_eq!(mark.working_directory.as_deref(), Some("/home/me/src"));
    assert_eq!(cwd_events(&mut t), vec!["//host/home/me/src".to_owned()]);

    let mut t = signing_at_prompt();
    t.process(b"\r\n");
    t.process(&own_report("/home/me"));
    t.process(&osc7("/elsewhere"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    assert!(cwd_events(&mut t).is_empty(), "nothing changed");

    let mut t = at_prompt();
    t.process(b"\r\n");
    t.process(&osc7("/home/me"));
    t.process(&osc7("/elsewhere"));
    t.process(&signed("A"));
    assert_eq!(t.current_working_directory(), Some("/elsewhere"));
}

#[test]
fn a_key_bound_programs_report_lands_while_it_holds_the_foreground() {
    let mut t = signing_at_prompt();
    t.set_foreground_job(true);
    t.process(b"\x1b]133;A\x07remote$ ");
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    assert_eq!(t.reported_cwd_host(), Some("host"));
    t.process(&osc7("/srv/app/logs"));
    assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
    t.set_foreground_job(false);
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    assert_eq!(
        cwd_events(&mut t),
        vec![
            "//host/srv/app".to_owned(),
            "//host/srv/app/logs".to_owned(),
            "//host/home/me".to_owned()
        ]
    );
    t.set_foreground_job(false);
    assert!(cwd_events(&mut t).is_empty());
    t.process(&osc7("/elsewhere"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));

    let mut t = signing_at_prompt();
    t.process(b"ssh build\r\n");
    t.process(&signed("C"));
    t.set_foreground_job(true);
    t.process(&osc7("/srv/app"));
    t.set_foreground_job(false);
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
}

#[test]
fn a_program_the_probe_sees_since_the_prompt_reports_its_cwd() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
    let started = Arc::new(AtomicU64::new(0));
    let asked = Arc::new(AtomicU64::new(0));
    let mut t = signing_at_prompt();
    let (child, count) = (started.clone(), asked.clone());
    t.set_program_probe(Some(Box::new(move |since| {
        count.fetch_add(1, SeqCst);
        let at = child.load(SeqCst);
        at != 0 && at >= since
    })));
    started.store(1, SeqCst);
    t.process(&osc7("/elsewhere"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    started.store(u64::MAX, SeqCst);
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    t.set_foreground_job(false);
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    started.store(0, SeqCst);
    t.set_foreground_job(false);
    assert_eq!(t.current_working_directory(), Some("/home/me"));
    let before = asked.load(SeqCst);
    t.set_foreground_job(false);
    t.process(&own_report("/home/me/src"));
    assert_eq!(asked.load(SeqCst), before);
    t.process(b"true\r\n");
    t.process(&signed("C"));
    t.process(&signed("D;0"));
    started.store(u64::MAX, SeqCst);
    t.process(&osc7("/srv/logs"));
    assert_eq!(t.current_working_directory(), Some("/srv/logs"));
}

#[test]
fn a_shell_that_stops_marking_commands_holds_no_report_once_it_says_so() {
    let mut t = signing_at_prompt();
    t.process(b"ssh build\r\n");
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/home/me"), "the first");
    t.process(&signed("D;0"));
    assert!(t.shell.unmarked_run);
    t.process(&own_report("/home/me"));
    t.process(&signed("A"));
    t.process(b"$ ");
    t.process(&signed("B"));
    t.process(b"ssh build\r\n");
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
    t.process(&signed("D;0"));
    t.process(&own_report("/home/me"));
    t.process(&signed("A"));
    t.process(b"$ ");
    t.process(&signed("B"));
    t.process(b"true\r\n");
    t.process(&signed("C"));
    assert!(!t.shell.unmarked_run);
    t.process(&signed("D;0"));
    t.process(&own_report("/home/me"));
    t.process(&signed("A"));
    t.process(&osc7("/elsewhere"));
    assert_eq!(t.current_working_directory(), Some("/home/me"));

    let mut t = signing_at_prompt();
    t.process(b"ssh build\r\n");
    t.set_foreground_job(true);
    t.process(&osc7("/srv/app"));
    assert_eq!(t.current_working_directory(), Some("/srv/app"));
}

#[test]
fn what_the_prompt_left_pending_survives_a_checkpoint() {
    fn carried(t: &Terminal) -> Vec<Terminal> {
        let cp = t.checkpoint_carry(0).expect("a prompt carries");
        let key = cp
            .shell_integration_nonce
            .expect("an authenticated prompt carries its key")
            .0;
        #[allow(unused_mut)]
        let mut out = vec![Terminal::from_checkpoint(&cp)];
        #[cfg(feature = "serde")]
        {
            let meta = crate::terminal::CheckpointMeta::from_checkpoint(&cp);
            out.push(Terminal::from_checkpoint(
                &meta.into_checkpoint(cp.grid.clone(), cp.alt_grid.clone()),
            ));
        }
        for s in &mut out {
            s.authorize_shell_integration(key);
        }
        out
    }

    let mut held = at_prompt();
    held.process(b"\r\n");
    held.process(&osc7("/home/me/src"));
    for mut s in carried(&held) {
        s.process(&signed("A"));
        assert_eq!(s.current_working_directory(), Some("/home/me/src"));
    }

    let mut signing = signing_at_prompt();
    signing.process(b"cd src\r\n");
    signing.process(&signed("C"));
    signing.process(&signed("D;0"));
    for mut s in carried(&signing) {
        s.process(&osc7("/elsewhere"));
        assert_eq!(s.current_working_directory(), Some("/home/me"));
        assert!(cwd_events(&mut s).is_empty());
        s.process(&own_report("/home/me/src"));
        s.process(&signed("A"));
        assert_eq!(s.current_working_directory(), Some("/home/me/src"));
    }

    const REKEY: [u8; 32] = [0xA7; 32];
    let hex = ShellIntegrationNonce(REKEY).to_hex();
    let mut rekeying = signing_at_prompt();
    rekeying.authorize_shell_integration_on_first_mark(REKEY);
    rekeying.process(format!("\x1b]633;P;AtermCwd=file://host/home/me;id={hex}\x07").as_bytes());
    let mut continuations = carried(&rekeying);
    continuations.push(rekeying);
    for mut s in continuations {
        s.process(&osc7("/srv/app"));
        assert_eq!(s.current_working_directory(), Some("/srv/app"));
        s.process(format!("\x1b]133;A;id={hex}\x07").as_bytes());
        s.process(&osc7("/elsewhere"));
        assert_eq!(s.current_working_directory(), Some("/srv/app"));
    }

    let mut unmarked = signing_at_prompt();
    unmarked.process(&signed("D;0"));
    unmarked.process(&own_report("/home/me"));
    unmarked.process(&signed("A"));
    unmarked.process(&signed("B"));
    for mut s in carried(&unmarked) {
        s.process(&osc7("/srv/app"));
        assert_eq!(s.current_working_directory(), Some("/srv/app"));
    }

    let mut job = signing_at_prompt();
    job.set_foreground_job(true);
    job.process(&osc7("/srv/app"));
    for mut s in carried(&job) {
        assert_eq!(s.current_working_directory(), Some("/srv/app"));
        s.set_foreground_job(true);
        s.set_foreground_job(false);
        assert_eq!(s.current_working_directory(), Some("/home/me"));
    }
}
