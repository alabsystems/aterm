// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Command Line Tools install a setup run asks for. When a pass's Repositories stage ends
//! waiting on Apple's Command Line Tools (git, cc, the macOS SDK), which only a root install
//! with the person's password adds, the window opens a terminal tab that runs
//! `aterm init --only base.clt`: its one sudo prompt is the person's, in the tab, and the
//! run's park wakes once the tools are there (System State design §3.3).
//!
//! aterm opens the tab by itself once per Mac ([`Mark`]: a person who closed it without
//! installing is not asked again by a later launch), and only while System State is the front
//! view of a focused window, no setup stage or macOS prompt is running, and the keyboard was
//! quiet for 2.5 s. The GitHub repositories row's Install… opens it on a person's press at any
//! time ([`App::press_developer_tools_install`]), and the page reads where the install stands
//! ([`App::developer_tools_install_step`]): the tab asking for the password, the installer
//! running, or neither.
//! Keys typed while it spawns stay in System State ([`crate::tab_spawn::SpawnAsk::unasked`]).
//! The same checks run before landing: a busy, hidden or typed-into destination defers the
//! request without opening an empty tab or consuming its attempt. Stop or a pass that no
//! longer needs the tools withdraws a request not yet opened.
//! A dedicated shell waits until admission, then `exec`s the installer. Its exact child
//! lifetime holds off other setup prompts; the finished terminal keeps the result visible.

use crate::App;
use crate::Wake;
use crate::system_state::ToolsInstall;

mod probe;
mod receipt;

/// Where this launch's install stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Install {
    /// No setup pass waits on the tools.
    #[default]
    Unasked,
    /// This exact run's last pass waits on them: its tab opens once it is due ([`Install::due`]).
    Wanted(u64),
    /// A shell is starting, with the latest request retained until it can land.
    /// Stop or a pass that no longer needs the tools can withdraw it meanwhile.
    Opening { wanted: Option<u64> },
    /// A person's press of Install… started a shell, which lands once its own check passes
    /// ([`App::press_developer_tools_install`]), whatever run is admitted.
    Pressed,
    /// The tab was opened (or its opening failed): never again this launch, so a closed tab
    /// or a password left untyped costs one tab per launch at most.
    Opened,
}

impl Install {
    /// A pass ended waiting on the tools: wanted, unless its tab already opened.
    #[must_use]
    pub(crate) const fn asked(self, revision: u64) -> Self {
        match self {
            Self::Unasked => Self::Wanted(revision),
            Self::Wanted(previous) if revision >= previous => Self::Wanted(revision),
            Self::Opening { wanted } => Self::Opening {
                wanted: Some(match wanted {
                    Some(previous) if previous > revision => previous,
                    _ => revision,
                }),
            },
            other => other,
        }
    }

    /// A pass no longer waits on them. It cannot withdraw a newer run's request.
    #[must_use]
    pub(crate) const fn withdrawn_by(self, revision: u64) -> Self {
        match self {
            Self::Wanted(previous) if revision >= previous => Self::Unasked,
            Self::Opening {
                wanted: Some(previous),
            } if revision >= previous => Self::Opening { wanted: None },
            other => other,
        }
    }

    /// A pass ended without waiting on the tools, or a person stopped the run: a want not yet
    /// opened is dropped.
    #[must_use]
    pub(crate) const fn withdrawn(self) -> Self {
        match self {
            Self::Wanted(_) => Self::Unasked,
            Self::Opening { .. } => Self::Opening { wanted: None },
            other => other,
        }
    }

    /// Whether the tab opens now: wanted, with System State the front view of a focused window
    /// (`front`), no setup stage running (`running`) and no macOS dialog this process started
    /// up (`dialog_up`).
    #[must_use]
    pub(crate) const fn due(self, front: bool, running: bool, dialog_up: bool) -> bool {
        matches!(self, Self::Wanted(_)) && front && !running && !dialog_up
    }
}

/// What the tab's line gives `aterm`: the install item alone, with its one password prompt.
pub(crate) const ARGS: [&str; 3] = ["init", "--only", "base.clt"];

/// The once-per-Mac mark that the Command Line Tools tab has opened on this Mac: a small file
/// beside aterm's config, written as the tab lands. While it is there aterm never opens the tab
/// by itself again, so a person who closed it without installing is not asked again at the next
/// launch; the GitHub repositories row's Install… still opens it on their press.
#[derive(Debug, Default)]
pub(crate) struct Mark {
    path: Option<std::path::PathBuf>,
    opened: bool,
}

/// The mark's file name, beside aterm's config.
#[cfg(not(test))]
const MARK: &str = "command-line-tools-tab";

impl Mark {
    /// This Mac's mark, read once at launch. A mark that cannot be read counts as there: aterm
    /// asks no one twice for want of a reading. Tests read none.
    pub(crate) fn for_launch() -> Self {
        #[cfg(test)]
        {
            Self::default()
        }
        #[cfg(not(test))]
        {
            let path = crate::app_config::config_path()
                .and_then(|config| config.parent().map(|dir| dir.join(MARK)));
            let opened = path.as_deref().is_some_and(|path| {
                !matches!(std::fs::symlink_metadata(path), Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound)
            });
            Self { path, opened }
        }
    }

    /// A mark kept at `path`, as a launch reads it (a test's).
    #[cfg(all(test, unix))]
    pub(crate) fn at(path: std::path::PathBuf) -> Self {
        let opened = path.exists();
        Self {
            path: Some(path),
            opened,
        }
    }

    /// Whether the tab has opened on this Mac before.
    pub(crate) const fn opened(&self) -> bool {
        self.opened
    }

    /// The tab landed: never by itself again on this Mac. A write that fails leaves the mark for
    /// this launch alone, and says so in the log.
    fn note_opened(&mut self) {
        if std::mem::replace(&mut self.opened, true) {
            return;
        }
        if let Some(path) = &self.path
            && let Err(error) = crate::config_marker::write_marker(
                path,
                "opened\n",
                64,
                crate::durable::Flush::Ordered,
            )
        {
            aterm_log::warn!(
                "System State: the Command Line Tools tab's mark was not written: {error}"
            );
        }
    }
}

/// Whether Apple's Command Line Tools are installing on this Mac now: the on-demand placeholder
/// with a live `softwareupdate`, or the exact executable of Apple's installer app. A stale
/// placeholder alone says nothing ([`probe`]).
pub(crate) fn installing_now() -> bool {
    probe::installing_now()
}

/// The owned installer child, including its retained result after an exit notification.
pub(crate) struct Terminal {
    session: u64,
    finished: bool,
    recovered: bool,
    receipt: Option<receipt::Receipt>,
}

fn waiting_shell_command() -> Vec<std::ffi::OsString> {
    ["/usr/bin/env", "-u", "ENV", "-u", "BASH_ENV", "/bin/sh"]
        .into_iter()
        .map(Into::into)
        .collect()
}

fn install_line(line: &str) -> String {
    // An interactive shell can survive failed exec. End it with that failure status, too.
    format!("exec {line}; exit $?")
}

fn recovery_refusal_line(error: &std::io::Error) -> String {
    #[cfg(not(windows))]
    {
        let reason = error.to_string().replace(char::is_control, " ");
        let reason = format!("'{}'", reason.replace('\'', "'\\''"));
        let printf = crate::ai_screen::launch_line(
            std::path::Path::new("/usr/bin/printf"),
            &[
                "'%s\\n'",
                "'Installer not started: could not save its recovery state.'",
                &reason,
            ],
        );
        printf.map_or_else(|| "exit 1".into(), |line| format!("{line}; exit 1"))
    }
    #[cfg(windows)]
    {
        let _ = error;
        "exit 1".into()
    }
}

/// Tell the window whether the setup pass that just ended waits on Apple's Command Line Tools
/// (`wanted`), with that pass's exact `revision`: the setup run's hook
/// (`aterm_cli::macos_doctor::SetupHooks::developer_tools`), called from its worker.
pub fn offer_developer_tools_install(revision: u64, wanted: bool) {
    if !crate::workstation_setup::send_setup_wake(Wake::DeveloperToolsInstall(revision, wanted))
        && wanted
    {
        aterm_log::info!("System State: no window to install the Command Line Tools in");
    }
}

fn install_tab_then(revision: u64, line: String) -> crate::tab_spawn::TabSpawnThen {
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new(move |app, window| {
            app.prepare_developer_tools_install_landing(window, revision)
        }),
        then: Box::new(move |app, landed| {
            // A quiet refusal restored Wanted/Unasked before this callback. Only a real
            // opening (or its failure) consumes the launch's attempt.
            if matches!(
                app.workstation_setup.developer_tools,
                Install::Opening { .. }
            ) {
                app.workstation_setup.developer_tools = Install::Opened;
                match landed {
                    Ok(sid) => {
                        app.workstation_setup.developer_tools_mark.note_opened();
                        app.dispatch_developer_tools_install(revision, |app| {
                            app.start_developer_tools_install(&sid, &line);
                        });
                        // Only a real landing changes frontness. A refused spawn during
                        // shutdown must not republish the still-retained setup window.
                        app.publish_system_state_front();
                    }
                    Err(why) => aterm_log::warn!(
                        "System State: the Command Line Tools install tab did not open: {why}"
                    ),
                }
            }
            app.release_unused_access_dialog(std::time::Instant::now());
        }),
    }
}

/// [`install_tab_then`] for a person's press of Install…: its own landing check, and no run's
/// revision to answer to, since the press is the person's yes.
fn pressed_tab_then(line: String) -> crate::tab_spawn::TabSpawnThen {
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new(move |app, window| app.prepare_pressed_install_landing(window)),
        then: Box::new(move |app, landed| {
            if app.workstation_setup.developer_tools == Install::Pressed {
                app.workstation_setup.developer_tools = Install::Opened;
                match landed {
                    Ok(sid) => {
                        app.workstation_setup.developer_tools_mark.note_opened();
                        app.start_developer_tools_install(&sid, &line);
                        app.publish_system_state_front();
                    }
                    Err(why) => aterm_log::warn!(
                        "System State: the Command Line Tools install tab did not open: {why}"
                    ),
                }
            }
            app.release_unused_access_dialog(std::time::Instant::now());
        }),
    }
}

impl App {
    /// Type the install's line into its landed tab `sid`, recording the tab first: even an
    /// immediate exec failure's exit must release the slot and leave its explanation in the tab.
    fn start_developer_tools_install(&mut self, sid: &str, line: &str) {
        let session = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(sid))
            .map(|handle| handle.local_id);
        let Some(session) = session else {
            return;
        };
        let Some(child) = self.pool.get(session) else {
            return;
        };
        let recovery = self
            .workstation_setup
            .developer_tools_mark
            .path
            .as_deref()
            .and_then(|mark| {
                (child.pid == child.shell_identity.pid && child.ctx.self_id.as_str() == sid)
                    .then(|| {
                        receipt::Receipt::for_child(
                            mark,
                            sid,
                            child.pid,
                            child.shell_identity.birth,
                        )
                    })
                    .flatten()
            });
        let recovery = match recovery {
            Some(receipt) => receipt.write_exclusive().map(|()| Some(receipt)),
            None if self.config.keeper_enabled() => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the installer child has no verified recovery identity or state directory",
            )),
            None => Ok(None),
        };
        let (receipt, line) = match recovery {
            Ok(receipt) => (receipt, install_line(line)),
            Err(error) => {
                aterm_log::warn!(
                    "System State: installer not started: could not save its recovery state: {error}"
                );
                (None, recovery_refusal_line(&error))
            }
        };
        self.workstation_setup
            .developer_tools_session
            .push(Terminal {
                session,
                finished: false,
                recovered: false,
                receipt,
            });
        self.type_into_new_session(sid, &line);
    }

    /// Recover custody only for an adopted exact child with its own immutable record. This
    /// restores neither setup admission nor a command: adoption never types installer bytes.
    pub(crate) fn recover_developer_tools_install(&mut self, local: u64) {
        if self
            .workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| terminal.session == local)
        {
            return;
        }
        let Some(child) = self.pool.get(local) else {
            return;
        };
        if child.handoff_local_id.is_none() || child.pid != child.shell_identity.pid {
            return;
        }
        let Some(receipt) = self
            .workstation_setup
            .developer_tools_mark
            .path
            .as_deref()
            .and_then(|mark| {
                receipt::Receipt::for_child(
                    mark,
                    child.ctx.self_id.as_str(),
                    child.pid,
                    child.shell_identity.birth,
                )
            })
            .filter(receipt::Receipt::matches)
        else {
            return;
        };
        let finished = child.child_exit.get().is_some()
            || child
                .child_reaped
                .load(std::sync::atomic::Ordering::Acquire)
            || child.pty_closed_at.get().is_some();
        self.workstation_setup
            .developer_tools_session
            .push(Terminal {
                session: local,
                finished,
                recovered: true,
                receipt: Some(receipt),
            });
        self.workstation_setup.developer_tools = Install::Opened;
        if !finished {
            self.recover_access_dialog();
        }
    }

    /// Where an install of Apple's Command Line Tools stands for System State's page (design
    /// §3.3): a live installer, else the tab's installer child alive, asking for the password,
    /// else neither.
    pub(crate) fn developer_tools_install_step(&self) -> Option<ToolsInstall> {
        if self.developer_tools_installing() {
            Some(ToolsInstall::Installing)
        } else if self
            .workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| terminal.recovered && !terminal.finished)
        {
            Some(ToolsInstall::Recovered)
        } else if self.developer_tools_install_active() {
            Some(ToolsInstall::Password)
        } else {
            None
        }
    }

    /// The same local installer evidence drives the displayed wait and each dispatch boundary.
    fn developer_tools_installing(&self) -> bool {
        self.workstation_setup
            .developer_tools_probe
            .is_some_and(|installing| installing())
    }

    /// A person's press of the GitHub repositories row's Install… (design §3.3): the tab opens
    /// now, whatever run is admitted and whether or not it opened on this Mac before, landing
    /// once System State is still the front view of its window and no macOS dialog this process
    /// started is up. `true` when its shell started; `false` starts nothing: a headless
    /// instance, an installer already running, a tab open or opening, a dialog up, or no front
    /// page.
    pub(crate) fn press_developer_tools_install(&mut self) -> bool {
        if self.headless
            || self.developer_tools_install_active()
            || matches!(
                self.workstation_setup.developer_tools,
                Install::Opening { .. } | Install::Pressed
            )
            || self.access_dialog_up()
        {
            return false;
        }
        let Some(window) = self
            .windows
            .keys()
            .find(|id| self.system_state_frontmost_in(**id))
            .copied()
        else {
            return false;
        };
        if self.developer_tools_installing() {
            return false;
        }
        #[cfg(not(windows))]
        let line = crate::ledger_key::own_front_door()
            .and_then(|exe| crate::ai_screen::launch_line(&exe, &ARGS));
        #[cfg(windows)]
        let line: Option<String> = None;
        let Some(line) = line else {
            aterm_log::warn!(
                "System State: this program's path has no line to type; Install… opens no tab"
            );
            return false;
        };
        self.workstation_setup.developer_tools = Install::Pressed;
        aterm_log::info!(
            "System State: Install… starts the Command Line Tools tab (`aterm {}`)",
            ARGS.join(" ")
        );
        self.open_off_loop(
            window,
            crate::tab_spawn::SpawnPlace::Tab,
            crate::tab_spawn::SpawnAsk {
                unasked: true,
                exec_command: Some(waiting_shell_command()),
                ..crate::tab_spawn::SpawnAsk::default()
            },
            pressed_tab_then(line),
        );
        true
    }

    /// The landing check of a pressed tab: System State still the front view of `window`, no
    /// installer running and the dialog slot free, which the child holds through its lifetime.
    /// A tab that cannot land opens nothing, and the button stays for another press.
    fn prepare_pressed_install_landing(&mut self, window: crate::WindowId) -> bool {
        if self.workstation_setup.developer_tools != Install::Pressed {
            return false;
        }
        if self.system_state_frontmost_in(window)
            && !self.access_dialog_up()
            && !self.developer_tools_installing()
            && self.reserve_access_dialog()
        {
            return true;
        }
        self.workstation_setup.developer_tools = Install::Opened;
        aterm_log::info!(
            "System State: the Command Line Tools tab Install… asked for did not land"
        );
        false
    }

    pub(crate) fn developer_tools_install_active(&self) -> bool {
        self.workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| !terminal.finished)
    }

    /// Only the installer's own exit or final view detachment ends its prompt ownership.
    /// Returning true also keeps its completed terminal available for reading the result.
    pub(crate) fn finish_developer_tools_install(&mut self, session: u64) -> bool {
        let Some(terminal) = self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .find(|terminal| terminal.session == session)
        else {
            return false;
        };
        terminal.finished = true;
        if let Some(receipt) = terminal.receipt.take() {
            receipt.remove_if_matching();
        }
        self.release_unused_access_dialog(std::time::Instant::now());
        true
    }

    pub(crate) fn retire_developer_tools_install(&mut self, session: u64) {
        if self.finish_developer_tools_install(session) {
            self.workstation_setup
                .developer_tools_session
                .retain(|terminal| terminal.session != session);
        }
    }

    /// A setup pass ended waiting on the Command Line Tools (`wanted`), or not
    /// ([`offer_developer_tools_install`]).
    pub(crate) fn want_developer_tools_install(&mut self, revision: u64, wanted: bool) {
        let install = &mut self.workstation_setup.developer_tools;
        *install = if wanted {
            install.asked(revision)
        } else {
            install.withdrawn_by(revision)
        };
        self.tick_developer_tools_install();
    }

    /// [`Install::due`] for a want this run still authorizes, once the keyboard was quiet for
    /// 2.5 s ([`crate::hands_off_keys`]): the landed tab takes the window's keys. Never once the
    /// tab has opened on this Mac ([`Mark`]): Install… is the way then. A live installer holds
    /// the request until it ends, without consuming this Mac's automatic attempt.
    fn developer_tools_install_due(&self, front: bool, running: bool, dialog_up: bool) -> bool {
        !self.workstation_setup.developer_tools_mark.opened()
            && crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            && matches!(self.workstation_setup.developer_tools, Install::Wanted(revision)
            if self.developer_tools_install_authorized(revision))
            && self
                .workstation_setup
                .developer_tools
                .due(front, running, dialog_up)
            && !self.developer_tools_installing()
    }

    fn developer_tools_install_authorized(&self, revision: u64) -> bool {
        let status = self.system_state_host.latest_status();
        self.workstation_setup.setup_unheld()
            && !status.changing
            && status
                .run
                .is_some_and(|run| run.allowed && run.revision == revision)
    }

    /// Recheck before registration changes the active tab, reserving the worker's dialog
    /// slot through the installer child's lifetime. A key typed while the shell started
    /// defers it too; a newer want survives a refused shell.
    fn prepare_developer_tools_install_landing(
        &mut self,
        window: crate::WindowId,
        revision: u64,
    ) -> bool {
        let Install::Opening { wanted } = self.workstation_setup.developer_tools else {
            return false;
        };
        if wanted == Some(revision)
            && crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            && self.developer_tools_install_authorized(revision)
            && self.system_state_frontmost_in(window)
            && self.system_state_host.latest_status().running.is_empty()
            && !self.access_dialog_up()
            && !self.developer_tools_installing()
            && self.reserve_access_dialog()
        {
            return true;
        }
        self.workstation_setup.developer_tools = wanted.map_or(Install::Unasked, Install::Wanted);
        aterm_log::info!(
            "System State: Command Line Tools install {}",
            if wanted.is_some() {
                "deferred"
            } else {
                "withdrawn"
            }
        );
        false
    }

    /// A tab may land after Stop or a newer choice. Its old command then stays untyped.
    fn dispatch_developer_tools_install(&mut self, revision: u64, start: impl FnOnce(&mut Self)) {
        if self.developer_tools_install_authorized(revision) {
            start(self);
        }
    }

    /// Open the install's tab if it is due ([`Install::due`]); else it waits for the next
    /// status, which comes each second while a window is up.
    pub(crate) fn tick_developer_tools_install(&mut self) {
        if self.headless {
            return;
        }
        let Some((window, ask, then)) = self.begin_developer_tools_install(|| {
            #[cfg(not(windows))]
            let line = crate::ledger_key::own_front_door()
                .and_then(|exe| crate::ai_screen::launch_line(&exe, &ARGS));
            #[cfg(windows)]
            let line: Option<String> = None;
            line
        }) else {
            return;
        };
        aterm_log::info!(
            "System State: starting the Command Line Tools install tab (`aterm {}`)",
            ARGS.join(" ")
        );
        self.open_off_loop(window, crate::tab_spawn::SpawnPlace::Tab, ask, then);
    }

    /// When the install is due, the window its tab opens in, how it spawns and what it does once
    /// it lands (typing `line`, read only then), with the request now Opening. A program with no
    /// line to type spends the launch's attempt and opens nothing.
    fn begin_developer_tools_install(
        &mut self,
        line: impl FnOnce() -> Option<String>,
    ) -> Option<(
        crate::WindowId,
        crate::tab_spawn::SpawnAsk,
        crate::tab_spawn::TabSpawnThen,
    )> {
        if !self.developer_tools_install_due(
            self.system_state_frontmost(),
            !self.system_state_host.latest_status().running.is_empty(),
            self.access_dialog_up(),
        ) {
            return None;
        }
        let window = self
            .windows
            .keys()
            .find(|id| self.system_state_frontmost_in(**id))
            .copied()?;
        let Install::Wanted(revision) = self.workstation_setup.developer_tools else {
            return None;
        };
        self.workstation_setup.developer_tools = Install::Opened;
        let Some(line) = line() else {
            aterm_log::warn!(
                "System State: this program's path has no line to type; the Command Line Tools \
                 install opens no tab"
            );
            return None;
        };
        self.workstation_setup.developer_tools = Install::Opening {
            wanted: Some(revision),
        };
        let ask = crate::tab_spawn::SpawnAsk {
            unasked: true,
            // The worker must not start installation before the UI's landing check.
            // Use a known shell without owner rc hooks or the app's global `-e`.
            exec_command: Some(waiting_shell_command()),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        Some((window, ask, install_tab_then(revision, line)))
    }
}

#[cfg(all(test, unix))]
#[path = "clt_install/landing_tests.rs"]
mod landing_tests;

#[cfg(all(test, unix))]
#[path = "clt_install/child_tests.rs"]
mod child_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(revision: u64) -> Option<crate::InitialSetupPermissionPolicy> {
        Some(crate::InitialSetupPermissionPolicy {
            revision,
            allowed: false,
        })
    }

    fn observe_run(
        app: &mut App,
        revision: u64,
        allowed: bool,
        permissions: Option<crate::InitialSetupPermissionPolicy>,
    ) {
        let mut status = app.system_state_host.latest_status().clone();
        status
            .plan
            .get_or_insert_with(aterm_types::macos_setup::Plan::default);
        status.permissions = permissions;
        status.run = Some(crate::InitialSetupRunPolicy { revision, allowed });
        app.observe_initial_setup_permissions(permissions);
        app.publish_system_state(status);
    }

    fn stopped() -> App {
        let mut app = App::headless_for_test();
        observe_run(&mut app, 4, true, policy(4));
        app.stop_initial_setup_permissions();
        app
    }

    fn accept_later_apply(app: &mut App) {
        use crate::native_app::{ActionInvocation, AppEvent};
        use crate::native_ui::ActionId;
        use crate::workstation_setup::{Request, WorkstationSetupDecision};
        use aterm_types::macos_setup::{Plan, Selection, Setting};

        let wid = crate::WindowId(0);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        app.set_system_state_handlers_for_test(crate::InitialSetupHandlers::inert_for_test());
        let mut plan = Plan {
            selection: Selection::none(),
            ..Plan::default()
        };
        plan.selection.set(Setting::Repositories, true);
        let (send, receive) = std::sync::mpsc::channel();
        let request = Request::new(plan, send);
        app.headless = false;
        app.offer_system_state(request.clone());
        app.headless = true;
        assert!(request.pending());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.with_gesture_source(false, |app| {
            app.interact_system_state(wid);
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    // A press on a group answers the offer.
                    id: ActionId::new("system-state/group/1/leave"),
                    value: None,
                }),
            )
        })
        .unwrap();
        let mut answer = receive.try_recv().unwrap();
        assert!(matches!(
            answer.decision,
            WorkstationSetupDecision::Begin(_)
        ));
        answer.receipt.take().unwrap().accept();
        app.resolve_system_state_apply();
    }

    #[test]
    fn clt_install_a_pass_withdraws_its_own_revisions_want_and_never_a_newer_one() {
        assert_eq!(Install::Wanted(4).withdrawn_by(4), Install::Unasked);
        assert_eq!(Install::Wanted(4).withdrawn_by(5), Install::Unasked);
        assert_eq!(Install::Wanted(5).withdrawn_by(4), Install::Wanted(5));
        let opening = |wanted| Install::Opening { wanted };
        assert_eq!(opening(Some(4)).withdrawn_by(4), opening(None));
        assert_eq!(opening(Some(5)).withdrawn_by(4), opening(Some(5)));
        assert_eq!(Install::Opened.withdrawn_by(9), Install::Opened);
    }

    #[test]
    fn clt_install_a_delayed_want_cannot_rearm_a_stopped_run() {
        let mut app = stopped();
        app.want_developer_tools_install(4, true);
        assert!(
            !app.developer_tools_install_due(true, false, false),
            "a delayed pass callback must not reopen installation after Stop"
        );
    }

    /// A person typing (a vault password into System State, say) holds the tab back until the
    /// keyboard was quiet for 2.5 s, so the tab that takes the window's keys catches none of it.
    #[test]
    fn clt_install_the_tab_waits_out_a_typing_person() {
        let mut app = App::headless_for_test();
        observe_run(&mut app, 4, true, policy(4));
        app.want_developer_tools_install(4, true);
        assert!(app.developer_tools_install_due(true, false, false));
        let now = std::time::Instant::now();
        app.last_keystroke_at = Some(now);
        assert!(
            !app.developer_tools_install_due(true, false, false),
            "a key typed just now holds the tab"
        );
        app.last_keystroke_at = now.checked_sub(std::time::Duration::from_secs(3));
        assert!(
            app.developer_tools_install_due(true, false, false),
            "a quiet keyboard lets it open"
        );
    }

    #[test]
    fn clt_install_a_run_can_keep_an_older_permissions_approval() {
        let mut app = App::headless_for_test();
        let permissions = Some(crate::InitialSetupPermissionPolicy {
            revision: 4,
            allowed: true,
        });
        app.observe_initial_setup_permissions(permissions);
        app.publish_system_state(crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions,
            run: Some(crate::InitialSetupRunPolicy {
                revision: 5,
                allowed: true,
            }),
            ..crate::SetupStatus::default()
        });
        app.want_developer_tools_install(5, true);
        assert!(
            app.developer_tools_install_due(true, false, false),
            "the current run's repository tools are independent of its carried Permissions approval"
        );
    }

    #[test]
    fn clt_install_a_durable_stop_does_not_reopen_tools_without_a_local_stop() {
        let mut app = App::headless_for_test();
        app.observe_initial_setup_permissions(policy(4));
        app.publish_system_state(crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions: policy(4),
            run: Some(crate::InitialSetupRunPolicy {
                revision: 4,
                allowed: false,
            }),
            finished: true,
            ..crate::SetupStatus::default()
        });
        app.want_developer_tools_install(4, true);
        assert!(
            !app.developer_tools_install_due(true, false, false),
            "a Stop read from durable records withdraws installation even when another process asked for it"
        );
    }

    #[test]
    fn clt_install_a_new_apply_only_rearms_its_own_repository_request() {
        let mut app = stopped();
        accept_later_apply(&mut app);
        app.want_developer_tools_install(5, true);
        assert!(
            !app.developer_tools_install_due(true, false, false),
            "the new admission still needs its fresh policy"
        );
        observe_run(&mut app, 5, true, policy(5));
        assert!(
            app.developer_tools_install_due(true, false, false),
            "Repositories may install tools with Permissions left as is"
        );
        app.want_developer_tools_install(4, false);
        assert!(
            app.developer_tools_install_due(true, false, false),
            "an older pass cannot withdraw the new run's request"
        );
        app.want_developer_tools_install(4, true);
        assert!(
            app.developer_tools_install_due(true, false, false),
            "an older pass cannot replace the new run's request"
        );
        assert_eq!(app.workstation_setup.developer_tools, Install::Wanted(5));
        observe_run(&mut app, 4, false, policy(4));
        assert!(
            !app.developer_tools_install_due(true, false, false),
            "an older denied run policy cannot authorize the current run's install"
        );
        observe_run(&mut app, 5, true, policy(5));
        assert!(app.developer_tools_install_due(true, false, false));
        app.want_developer_tools_install(6, false);
        assert!(
            !app.developer_tools_install_due(true, false, false),
            "a newer pass that no longer waits withdraws the older want"
        );
    }

    #[test]
    fn clt_install_a_stopped_or_replaced_tab_never_starts_the_install_command() {
        for replaced in [false, true] {
            let mut app = App::headless_for_test();
            observe_run(&mut app, 4, true, policy(4));
            app.workstation_setup.developer_tools = Install::Opened;
            let mut commands = 0;
            app.dispatch_developer_tools_install(4, |_| commands += 1);
            assert_eq!(commands, 1, "the live install has an inert command sink");
            app.stop_initial_setup_permissions();
            if replaced {
                accept_later_apply(&mut app);
                observe_run(&mut app, 5, true, policy(5));
            }
            app.dispatch_developer_tools_install(4, |_| commands += 1);
            assert_eq!(
                commands, 1,
                "Stop or a newer accepted plan retires the old tab's command"
            );
            app.want_developer_tools_install(5, true);
            assert_eq!(
                app.workstation_setup.developer_tools,
                Install::Opened,
                "an opened tab retains the once-per-launch bound"
            );
        }
    }

    #[test]
    fn clt_install_a_changing_read_withholds_only_run_eligibility_until_a_settled_read() {
        let mut app = App::headless_for_test();
        let status = crate::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            permissions: policy(4),
            run: Some(crate::InitialSetupRunPolicy {
                revision: 5,
                allowed: true,
            }),
            ..crate::SetupStatus::default()
        };
        app.receive_system_state_status(Some(0), status.clone());
        app.want_developer_tools_install(5, true);
        assert!(app.developer_tools_install_due(true, false, false));
        let changing = crate::SetupStatus {
            changing: true,
            ..crate::SetupStatus::default()
        };
        app.receive_system_state_status(Some(1), changing.clone());
        assert!(
            app.developer_tools_install_due(true, false, false),
            "a stale status generation cannot withdraw the current run"
        );
        app.receive_system_state_status(Some(0), changing);
        assert!(!app.developer_tools_install_due(true, false, false));
        let mut commands = 0;
        app.dispatch_developer_tools_install(5, |_| commands += 1);
        assert_eq!(
            commands, 0,
            "a tab landing during the unknown read stays untyped"
        );
        assert_eq!(app.system_state_host.latest_status().plan, status.plan);
        assert_eq!(
            app.system_state_host.latest_status().permissions,
            status.permissions
        );
        app.receive_system_state_status(Some(0), status);
        assert!(app.developer_tools_install_due(true, false, false));
        app.dispatch_developer_tools_install(5, |_| commands += 1);
        assert_eq!(
            commands, 1,
            "the settled current run restores the same deferred work"
        );
    }

    /// A pass's ask makes the install wanted; its tab opens only while System State is in
    /// front, no stage runs and no macOS dialog is up, and never twice in a launch. A pass
    /// that no longer waits on the tools, or a Stop, drops a want not yet opened.
    #[test]
    fn the_install_tab_opens_once_while_system_state_is_in_front_and_nothing_else_runs() {
        let wanted = Install::default().asked(4);
        assert_eq!(wanted, Install::Wanted(4));
        assert_eq!(wanted.asked(4), Install::Wanted(4));
        assert!(wanted.due(true, false, false));
        assert!(
            !wanted.due(false, false, false),
            "System State not in front"
        );
        assert!(!wanted.due(true, true, false), "a setup stage is running");
        assert!(!wanted.due(true, false, true), "a macOS dialog is up");
        assert!(!Install::Unasked.due(true, false, false), "no pass asked");
        assert_eq!(
            wanted.withdrawn(),
            Install::Unasked,
            "withdrawn before it opened"
        );
        assert_eq!(
            Install::Opened.asked(4),
            Install::Opened,
            "never twice a launch"
        );
        assert_eq!(Install::Opened.withdrawn(), Install::Opened);
        assert!(!Install::Opened.due(true, false, false));
    }

    /// The tab types the install item alone, which asks the one password.
    #[test]
    fn the_install_tab_types_the_command_line_tools_item() {
        let line = crate::ai_screen::launch_line(
            std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
            &ARGS,
        );
        assert_eq!(
            line.as_deref(),
            Some("'/Applications/aterm.app/Contents/MacOS/aterm' init --only base.clt")
        );
    }
}
