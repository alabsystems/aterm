// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The dedicated install shell's real child lifetime, without running an installer.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::spawn::{WakePost, WakeTarget};
use crate::{App, Wake, WindowId};

struct ChildWakes(WakePost);

#[test]
fn clt_child_recovery_refusal_prints_error_as_data_and_exits() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let sentinel = directory.path().join("must-not-exist");
    let reason = format!(
        "can't save '$(/usr/bin/touch {})' ` /usr/bin/touch {} `\n",
        sentinel.display(),
        sentinel.display()
    );
    let error = std::io::Error::other(reason.clone());
    let line = super::recovery_refusal_line(&error);
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command.args(["-c", &line]);
    let output =
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "Installer not started: could not save its recovery state.\n{}\n",
            reason.replace(char::is_control, " ")
        )
    );
    assert!(
        !sentinel.exists(),
        "an error message executes no shell substitution"
    );
}

impl WakeTarget for ChildWakes {
    fn wake_post(&self) -> WakePost {
        Arc::clone(&self.0)
    }
}

#[test]
fn clt_child_waits_for_dispatch_then_reports_the_exec_result_without_shell_integration() {
    let app = App::headless_for_test();
    let missing = aterm_tempfile::tempdir().expect("a directory with no installer");
    let missing_program = missing.path().join("missing-installer");

    for (id, program, expected) in [
        (951, std::path::Path::new("/usr/bin/false"), Some(1)),
        (952, missing_program.as_path(), None),
    ] {
        let (posted, exits) = mpsc::channel();
        let target = ChildWakes(Arc::new(move |wake| {
            if let Wake::Exit { session, .. } = wake {
                let _ = posted.send(session);
            }
            true
        }));
        let mut factory = app.session_factory.clone();
        factory.exec_command = Some(super::waiting_shell_command());
        factory.integrate = false;
        factory.byte_arm = false;
        let session = crate::spawn::spawn_session(
            id,
            WindowId(0),
            24,
            240,
            None,
            &factory,
            &target,
            None,
            None,
            None,
            None,
        )
        .expect("the dedicated waiting shell starts");
        let identity = session.shell_identity;
        assert!(identity.own_child && identity.has_positive_pid());
        assert!(!session.rekey_channel, "no shell integration is needed");
        let command = super::install_line(
            &crate::ai_screen::launch_line(program, &[])
                .expect("the inert program has a shell line"),
        );
        // A completed terminal line is the admission boundary. Its echoed, incomplete
        // input proves the real reader is running while the owned child still waits.
        aterm_pty::write_all(session.master, command.as_bytes());
        let echoed =
            aterm_spec::wait::within(aterm_spec::wait::MUST_HAPPEN, Duration::ZERO, || {
                let terminal = crate::term_lock(&session.term);
                let shown: String = (0..24).filter_map(|row| terminal.row_text(row)).collect();
                shown.contains(&command).then_some(())
            });
        assert!(echoed.is_some(), "the incomplete command reached the PTY");
        assert_eq!(exits.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(session.reap_child(), None, "the shell waits for dispatch");
        assert!(
            identity.verify(),
            "the waiting child is still the recorded one"
        );

        aterm_pty::write_all(session.master, b"\r");
        assert_eq!(
            exits.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
            Ok(id),
            "even a missing executable ends this session's child"
        );
        assert_eq!(session.shell_identity, identity);
        let result = session.reap_child();
        if let Some(expected) = expected {
            assert_eq!(result, Some(aterm_pty::ChildExit::Code(expected)));
        } else {
            assert!(
                matches!(result, Some(aterm_pty::ChildExit::Code(126 | 127))),
                "failed exec ends the owned child, whether refused or missing: {result:?}"
            );
        }
    }
}
