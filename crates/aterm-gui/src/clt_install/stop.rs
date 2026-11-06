// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cancel only the receipt-bound installer program, never a terminal's replacement job.

use aterm_uds::process::Identity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Running {
    pub(super) identity: Identity,
    pub(super) installer: bool,
}

/// Native process evidence and one exact-process interrupt; inert in headless fixtures.
#[derive(Clone, Copy, Default)]
pub(crate) struct Reach {
    pub(super) read: Option<fn(i32) -> Option<Running>>,
    pub(super) interrupt: Option<fn(&Identity) -> std::io::Result<()>>,
}

impl Reach {
    pub(crate) fn for_launch(headless: bool) -> Self {
        #[cfg(target_os = "macos")]
        if !headless && !cfg!(test) {
            return Self {
                read: Some(read),
                interrupt: Some(interrupt),
            };
        }
        let _ = headless;
        Self::default()
    }
}

#[cfg(target_os = "macos")]
fn read(pid: i32) -> Option<Running> {
    use aterm_uds::process::{image_matches_file, read_identity, read_process_argv};
    let identity = read_identity(pid)?;
    let argv = read_process_argv(pid)?;
    let installer = match argv.as_slice() {
        [program, init, only, item] => {
            let path = std::path::Path::new(program);
            path.is_absolute()
                && path.file_name() == Some(std::ffi::OsStr::new("aterm"))
                && init == "init"
                && only == "--only"
                && item == "base.clt"
                && image_matches_file(pid, path) == Some(true)
        }
        _ => false,
    };
    (read_identity(pid) == Some(identity)).then_some(Running {
        identity,
        installer,
    })
}

#[cfg(target_os = "macos")]
fn interrupt(identity: &Identity) -> std::io::Result<()> {
    use aterm_uds::process::{EndControl as _, EndControlLive};
    let mut control = EndControlLive;
    if control.identity(identity.pid) != Some(*identity) {
        return Err(std::io::ErrorKind::NotFound.into());
    }
    control.signal(identity, libc::SIGINT)
}

impl crate::App {
    /// Stop the current owned installer without taking command or signal authority over any
    /// other program. The CLI forwards this interrupt to its own attached sudo child.
    pub(crate) fn stop_developer_tools_install(&mut self) {
        let setup = &mut self.workstation_setup;
        let (Some(read), Some(interrupt)) = (
            setup.developer_tools_stop.read,
            setup.developer_tools_stop.interrupt,
        ) else {
            return;
        };
        let Some(terminal) = setup.developer_tools_session.iter_mut().find(|terminal| {
            Some(terminal.session) == setup.developer_tools_attempt
                && !terminal.finished
                && !terminal.ended
                && !terminal.stop_sent
        }) else {
            return;
        };
        let Some(child) = self
            .pool
            .get(terminal.session)
            .filter(|child| !super::child_ended(child))
        else {
            return;
        };
        let owns = || {
            child.pid == child.shell_identity.pid
                && terminal.receipt.as_ref().is_some_and(|receipt| {
                    receipt.owns(
                        child.ctx.self_id.as_str(),
                        child.pid,
                        child.shell_identity.birth,
                    )
                })
        };
        if !owns() {
            return;
        }
        let Some(running) = read(child.pid).filter(|running| {
            running.installer && Some(running.identity.birth.0) == child.shell_identity.birth
        }) else {
            return;
        };
        // Re-read the program too: an exec can preserve pid and birth. Custody is checked at
        // the signal boundary, so a changed receipt or replacement session gets no interrupt.
        if read(child.pid) != Some(running) || !owns() {
            return;
        }
        match interrupt(&running.identity) {
            Ok(()) => terminal.stop_sent = true,
            Err(error) => aterm_log::info!(
                "System State: the exact Mac Setup child could not be interrupted: {error}"
            ),
        }
    }
}
