// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cancel only the receipt-bound installer program, never a terminal's replacement job.
//!
//! The program is proved by what stays true of it across an update: the pid and birth its tab
//! session holds, its fixed argv, and the code it runs ([`runs_this_app`]). Its loaded image
//! being the file at its path proves it too, but cannot be the only proof: an update or a
//! rebuild replaces the bundle under a running tab while the tab keeps the image it loaded.

use aterm_uds::process::Identity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Program {
    Tools,
    #[cfg(target_os = "macos")]
    NameRule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Running {
    pub(super) identity: Identity,
    pub(super) program: Option<Program>,
}

/// Native process evidence and one exact-process interrupt; inert in headless fixtures.
#[derive(Clone, Copy, Default)]
pub(crate) struct Reach {
    pub(super) read: Option<fn(i32) -> Option<Running>>,
    pub(super) interrupt: Option<fn(&Identity) -> std::io::Result<()>>,
    /// Whether the tab's terminal reads a password now: canonical input with no echo, as `sudo`
    /// sets it while it waits at its prompt ([`aterm_pty::TtyEcho::swallows_input`]).
    #[cfg(target_os = "macos")]
    pub(super) reads_secret: Option<fn(&aterm_session::sink::SinkWriter) -> bool>,
}

impl Reach {
    pub(crate) fn for_launch(headless: bool) -> Self {
        #[cfg(target_os = "macos")]
        if !headless && !cfg!(test) {
            return Self {
                read: Some(read),
                interrupt: Some(interrupt),
                reads_secret: Some(reads_secret),
            };
        }
        let _ = headless;
        Self::default()
    }
}

#[cfg(target_os = "macos")]
fn reads_secret(sink: &aterm_session::sink::SinkWriter) -> bool {
    sink.tty_echo()
        .is_some_and(aterm_pty::TtyEcho::swallows_input)
}

/// Whether a Mac Setup tab's program sits at its password prompt untouched past
/// [`super::UNTOUCHED_PROMPT_BACKSTOP`]: it is `at_prompt` (its terminal reads a password, its
/// program has not taken one, no installer of its ran and no stop was sent it), and its session
/// has had no person's gesture for the backstop (`idle_ms`: the last gesture's age, or with none
/// ever the time it has been served). A gesture of any kind in the tab, a key above all, holds it
/// for the backstop again.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn untouched_prompt_overdue(at_prompt: bool, idle_ms: Option<u64>) -> bool {
    at_prompt
        && idle_ms.is_some_and(|ms| u128::from(ms) >= super::UNTOUCHED_PROMPT_BACKSTOP.as_millis())
}

/// Who signed valid running code: its signing identifier and its team.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Signer {
    pub(super) identifier: String,
    pub(super) team: String,
}

/// The [`Signer`] the kernel's reading of a process names: `None` unless the kernel holds its
/// signature valid now and names both an identifier and a developer's team (an ad-hoc or
/// linker-signed image has none).
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(super) fn signer_of(identity: &aterm_sysprobe::identity::Identity) -> Option<Signer> {
    (identity.cs_status? & aterm_sysprobe::identity::CS_VALID != 0).then_some(())?;
    Some(Signer {
        identifier: identity.signing_id.clone()?,
        team: identity.team.clone()?,
    })
}

/// The code a process runs, as the kernel holds it from its exec (so it stays readable after an
/// update replaces the file at its path): its CodeDirectory hash and, where its signature is
/// valid and names a team, its signer.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Code {
    pub(super) hash: Option<[u8; 20]>,
    pub(super) signer: Option<Signer>,
}

#[cfg(target_os = "macos")]
impl Code {
    fn of(pid: i32) -> Self {
        Self {
            hash: aterm_uds::process::read_code_directory_hash(pid),
            signer: u32::try_from(pid)
                .ok()
                .and_then(aterm_sysprobe::identity::identity)
                .and_then(|identity| signer_of(&identity)),
        }
    }
}

/// Whether a process whose argv is a setup command's runs this app: its loaded image is the
/// file at its path (`at_path`), or `theirs`, the code it runs, is this window's own (`ours`)
/// by its hash, or valid code of this window's own signer. The last two hold where the file at
/// the path is a newer build's: a window whose own executable is the tab's `aterm` runs the
/// same code as a tab it started and whose bundle was rebuilt under both, and the window an
/// update handed a tab to runs the same signer's. Code the kernel names neither a hash nor a
/// signer for proves nothing, and neither does any code to a window with none of its own.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(super) fn runs_this_app(at_path: Option<bool>, theirs: &Code, ours: &Code) -> bool {
    at_path == Some(true)
        || theirs.hash.is_some_and(|hash| ours.hash == Some(hash))
        || theirs
            .signer
            .as_ref()
            .is_some_and(|signer| ours.signer.as_ref() == Some(signer))
}

#[cfg(target_os = "macos")]
fn read(pid: i32) -> Option<Running> {
    use aterm_uds::process::{image_matches_file, read_identity, read_process_argv};
    let identity = read_identity(pid)?;
    let argv = read_process_argv(pid)?;
    let program = argv.split_first().and_then(|(exe, words)| {
        let path = std::path::Path::new(exe);
        if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new("aterm")) {
            return None;
        }
        let exact = |expected: &[&str]| {
            words
                .iter()
                .map(|word| word.as_os_str())
                .eq(expected.iter().map(std::ffi::OsStr::new))
        };
        let program = if exact(&["init", "--only", "base.clt"]) {
            Program::Tools
        } else if exact(&["machines", "here", "apply", "--plain", "--name-rule-only"]) {
            Program::NameRule
        } else {
            return None;
        };
        let at_path = image_matches_file(pid, path);
        // The kernel is asked for code only where the file does not already prove it.
        (at_path == Some(true)
            || runs_this_app(
                at_path,
                &Code::of(pid),
                &Code::of(i32::try_from(std::process::id()).ok()?),
            ))
        .then_some(program)
    });
    (read_identity(pid) == Some(identity)).then_some(Running { identity, program })
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
    /// The sessions of the Mac Setup tabs that hold an update's handoff (unfinished and unable
    /// to travel with it, [`Self::developer_tools_install_holds_handoff`]) and that sit at their
    /// password prompt untouched past [`super::UNTOUCHED_PROMPT_BACKSTOP`] at `now_us`
    /// ([`untouched_prompt_overdue`]), when every holding tab is one: `None` when none holds or
    /// any other does, which still holds the attempt, so none of them is stopped. The stop
    /// proves each tab's fixed setup command against its recorded purpose before signaling it.
    #[cfg(target_os = "macos")]
    pub(crate) fn developer_tools_prompts_untouched(&self, now_us: u64) -> Option<Vec<u64>> {
        let reads_secret = self.workstation_setup.developer_tools_stop.reads_secret;
        let mut holding = false;
        let mut overdue = Vec::new();
        let mut all = true;
        for terminal in self
            .workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| !terminal.finished)
            .filter(|terminal| !self.developer_tools_terminal_transferable(terminal))
        {
            holding = true;
            let child = self
                .pool
                .get(terminal.session)
                .filter(|child| !super::child_ended(child));
            let at_prompt = !terminal.ended
                && !terminal.past_password
                && !terminal.installer_seen
                && !terminal.stop_sent
                && child
                    .is_some_and(|child| reads_secret.is_some_and(|reads| reads(&child.ctx.sink)));
            let idle_ms = child.and_then(|child| {
                let stamp = &child.ctx.human_input;
                stamp.ms_since(now_us).or_else(|| stamp.unkeyed_ms(now_us))
            });
            if untouched_prompt_overdue(at_prompt, idle_ms) {
                overdue.push(terminal.session);
            } else {
                all = false;
            }
        }
        (holding && all).then_some(overdue)
    }

    /// Stop each Mac Setup tab `sessions` names, which an update waiting on it found at its
    /// password prompt untouched past [`super::UNTOUCHED_PROMPT_BACKSTOP`]
    /// ([`Self::developer_tools_prompts_untouched`]): its exact child (its tab session's own
    /// child, running its fixed tools-install or name-rule command from the birth the session
    /// recorded, read twice)
    /// is sent the interrupt setup's Stop sends, and nothing is typed into the tab. Its receipt
    /// is not read, since a tab that cannot travel is one whose receipt may no longer adopt it.
    /// A tools install ends unfinished, asked again by itself ([`super::unattended_tab_end`]);
    /// a name repair ends without naming authority or a tools-install decision. Returns how
    /// many were sent the stop.
    #[cfg(target_os = "macos")]
    pub(crate) fn stop_untouched_developer_tools_prompts(&mut self, sessions: &[u64]) -> usize {
        let setup = &mut self.workstation_setup;
        let (Some(read), Some(interrupt)) = (
            setup.developer_tools_stop.read,
            setup.developer_tools_stop.interrupt,
        ) else {
            return 0;
        };
        let backstop = super::UNTOUCHED_PROMPT_BACKSTOP.as_secs();
        let mut stopped = 0;
        for terminal in setup.developer_tools_session.iter_mut().filter(|terminal| {
            sessions.contains(&terminal.session)
                && !terminal.finished
                && !terminal.ended
                && !terminal.stop_sent
        }) {
            let Some(child) = self
                .pool
                .get(terminal.session)
                .filter(|child| !super::child_ended(child))
                .filter(|child| child.pid == child.shell_identity.pid)
            else {
                continue;
            };
            let expected = match terminal.purpose {
                super::TerminalPurpose::Tools => Program::Tools,
                super::TerminalPurpose::NameRule { .. } => Program::NameRule,
            };
            let exact = || {
                read(child.pid).filter(|running| {
                    running.program == Some(expected)
                        && Some(running.identity.birth.0) == child.shell_identity.birth
                })
            };
            // Re-read the program too: an exec can preserve pid and birth.
            let Some(running) = exact().filter(|running| exact() == Some(*running)) else {
                aterm_log::info!(
                    "System State: the untouched Mac Setup tab was sent nothing: its child is \
                     not proved to be its setup program"
                );
                continue;
            };
            match interrupt(&running.identity) {
                Ok(()) => {
                    terminal.stop_sent = true;
                    terminal.unattended = true;
                    stopped += 1;
                    aterm_log::info!(
                        "System State: the Mac Setup tab's password prompt stood untouched for \
                         {backstop}s while an update waited; aterm stopped its exact setup program"
                    );
                }
                Err(error) => aterm_log::info!(
                    "System State: the untouched Mac Setup child could not be interrupted: {error}"
                ),
            }
        }
        stopped
    }

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
            aterm_log::info!(
                "System State: Stop sent the Mac Setup tab nothing: its receipt does not name \
                 the tab's child"
            );
            return;
        }
        let Some(running) = read(child.pid).filter(|running| {
            running.program == Some(Program::Tools)
                && Some(running.identity.birth.0) == child.shell_identity.birth
        }) else {
            aterm_log::info!(
                "System State: Stop sent the Mac Setup tab nothing: its child is not proved to \
                 be the setup program"
            );
            return;
        };
        // Re-read the program too: an exec can preserve pid and birth. Custody is checked at
        // the signal boundary, so a changed receipt or replacement session gets no interrupt.
        if read(child.pid) != Some(running) || !owns() {
            aterm_log::info!(
                "System State: Stop sent the Mac Setup tab nothing: its child changed while it \
                 was read"
            );
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
