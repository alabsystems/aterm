// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The install tab's real child lifetime, without running an installer.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::spawn::{WakePost, WakeTarget};
use crate::{App, Wake, WindowId};

struct ChildWakes(WakePost);

impl WakeTarget for ChildWakes {
    fn wake_post(&self) -> WakePost {
        Arc::clone(&self.0)
    }
}

/// A stand-in for the tab's program: it ends 0 only when it runs as the tab's own program
/// (the tab's variable set) and the go line typed into the tab reaches it.
fn stand_in() -> Vec<std::ffi::OsString> {
    let check = format!(
        "IFS= read -r go; test \"${}:$go\" = \"1:{}\"",
        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB,
        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO,
    );
    ["/bin/sh", "-c", check.as_str()]
        .map(std::ffi::OsString::from)
        .into()
}

#[test]
fn the_tabs_program_waits_for_its_go_line_and_its_exit_status_is_read() {
    let app = App::headless_for_test();
    let missing = aterm_tempfile::tempdir().expect("a directory with no installer");
    let missing_program = missing.path().join("missing-installer");

    for (id, program, expected) in [
        (951, stand_in(), Some(0)),
        (952, vec![missing_program.into_os_string()], None),
    ] {
        let (posted, exits) = mpsc::channel();
        let target = ChildWakes(Arc::new(move |wake| {
            if let Wake::Exit { session, .. } = wake {
                let _ = posted.send(session);
            }
            true
        }));
        let mut factory = app.session_factory.clone();
        factory.exec_command = Some(super::tab_command_running(program));
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
        .expect("the tab's program starts");
        let identity = session.shell_identity;
        assert!(identity.own_child && identity.has_positive_pid());
        assert!(!session.rekey_channel, "no shell integration is needed");
        if expected.is_some() {
            assert_eq!(exits.try_recv(), Err(mpsc::TryRecvError::Empty));
            assert_eq!(
                session.reap_child(),
                None,
                "the program waits for its go line"
            );
            assert!(
                identity.verify(),
                "the waiting child is still the recorded one"
            );
            let go = format!("{}\r", aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO);
            aterm_pty::write_all(session.master, go.as_bytes());
        }
        assert_eq!(
            exits.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
            Ok(id),
            "even a missing program ends this session's child"
        );
        assert_eq!(session.shell_identity, identity);
        let result =
            aterm_spec::wait::within(aterm_spec::wait::MUST_HAPPEN, Duration::ZERO, || {
                session.reap_child()
            });
        if let Some(expected) = expected {
            assert_eq!(result, Some(aterm_pty::ChildExit::Code(expected)));
        } else {
            assert!(
                matches!(result, Some(aterm_pty::ChildExit::Code(126 | 127))),
                "a program that cannot start ends the owned child: {result:?}"
            );
        }
    }
}
