// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass's window lane (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5): a
//! windowed instance starts `aterm init --converge --trigger window` off the event loop at once
//! and every [`crate::secrets_join::MEND_EVERY`] after, beside the vault subscriber's mend. It
//! covers a Mac whose subscriber runs a held copy of an older aterm, which an update never
//! restarts onto new code, and it is the trigger that joins an open vault (Local Network is
//! aterm.app's). The first-launch Secrets stage starts one pass of its own as it ends
//! ([`start_after_secrets`]).

#![cfg_attr(not(unix), allow(dead_code))]

/// Whether this process runs the converge pass's window lane, or the Secrets stage's pass: a
/// windowed instance (not `headless`) on a Unix desktop whose first-run setup is not running now
/// (`unified_setup`: the setup's Secrets stage then starts the pass as it ends, and the lane
/// starts once the setup finished), outside a private state root and CI (`ATERM_STATE_HOME`,
/// `CI`, as the Commons runtime), `set` saying which of those variables are set.
pub(crate) fn runs(headless: bool, unified_setup: bool, set: &dyn Fn(&str) -> bool) -> bool {
    cfg!(unix) && !headless && !unified_setup && !set("ATERM_STATE_HOME") && !set("CI")
}

/// Whether the environment variable `name` is set.
pub(crate) fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// Start the lane: a pass at once, then one every [`crate::secrets_join::MEND_EVERY`], each
/// waited for on the lane's own thread (never the event loop) and its exit said once per change.
/// Whether the thread started; never joined.
pub(crate) fn spawn_lane() -> bool {
    #[cfg(unix)]
    {
        let Ok(exe) = std::env::current_exe() else {
            aterm_log::warn!("converge: no path to this aterm, so the window's pass did not start");
            return false;
        };
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-converge",
            crate::qos::Role::Background,
            move || {
                let mut said = None;
                loop {
                    let ended = pass(&exe, "window");
                    if said.as_ref() != Some(&ended) {
                        if let Some(line) = ended_line(&ended) {
                            aterm_log::info!("{line}");
                        }
                        said = Some(ended);
                    }
                    std::thread::sleep(crate::secrets_join::MEND_EVERY);
                }
            },
        );
        match spawned {
            Ok(_) => true,
            Err(error) => {
                aterm_log::warn!("converge: the window's lane did not start: {error}");
                false
            }
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Start one pass for the first-launch Secrets stage that just ended (`--trigger setup`),
/// without waiting for it: the setup's later stages go on. The process's reaper collects it.
pub(crate) fn start_after_secrets() {
    #[cfg(unix)]
    if runs(false, false, &env_set) {
        let started =
            std::env::current_exe().and_then(|exe| aterm_secrets::converge::start(&exe, "setup"));
        if let Err(error) = started {
            aterm_log::warn!("converge: the Secrets stage's pass did not start: {error}");
        }
    }
}

/// How one pass of the lane ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    /// It ran (its own aterm.log line says what it did), or another pass held the lock.
    Ran,
    /// It exited with this status (a failed row, a refusal): the runner's log line says why.
    Failed(Option<i32>),
    /// It could not be started or waited for: why.
    Unstarted(String),
}

/// Run one pass for `trigger` from `exe` and wait for it.
#[cfg(unix)]
fn pass(exe: &std::path::Path, trigger: &str) -> Ended {
    let mut child = match aterm_secrets::converge::start(exe, trigger) {
        Ok(child) => child,
        Err(error) => return Ended::Unstarted(error.to_string()),
    };
    match child.wait() {
        Ok(status) if status.success() || status.code() == Some(aterm_secrets::converge::HELD) => {
            Ended::Ran
        }
        Ok(status) => Ended::Failed(status.code()),
        Err(error) => Ended::Unstarted(error.to_string()),
    }
}

/// The aterm.log line for a lane pass that ended as `ended`; none for one that ran.
fn ended_line(ended: &Ended) -> Option<String> {
    match ended {
        Ended::Ran => None,
        Ended::Failed(code) => Some(format!(
            "converge: the window's pass ended {} (its own aterm.log line says which row)",
            code.map_or_else(
                || "by a signal".to_owned(),
                |code| format!("with exit {code}")
            )
        )),
        Ended::Unstarted(why) => Some(format!("converge: the window's pass did not run: {why}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window's lane runs for a windowed launch with no first-run setup running, outside a
    /// private state root and CI; headless, a setup running now, `ATERM_STATE_HOME` or `CI` each
    /// skip it.
    /// NEGATIVE CONTROL: a gate that ignores `unified_setup` starts the lane beside a setup.
    #[test]
    fn the_window_lane_is_skipped_headless_under_setup_and_in_a_private_root_or_ci() {
        let none = |_: &str| false;
        assert_eq!(runs(false, false, &none), cfg!(unix), "an ordinary window");
        assert!(!runs(true, false, &none), "headless");
        assert!(!runs(false, true, &none), "under unified_setup");
        assert!(
            !runs(false, false, &|name| name == "ATERM_STATE_HOME"),
            "a private state root"
        );
        assert!(!runs(false, false, &|name| name == "CI"), "CI");
        assert!(
            runs(false, false, &|name| name == "HOME") || !cfg!(unix),
            "another variable does not skip it"
        );
    }

    /// A pass that ran, or met another pass's lock, says nothing (its runner logs its own line);
    /// one that failed or did not start says so.
    #[test]
    fn only_a_pass_that_did_not_run_right_is_logged_by_the_lane() {
        assert_eq!(ended_line(&Ended::Ran), None);
        assert!(ended_line(&Ended::Failed(Some(1))).is_some_and(|line| line.contains("exit 1")));
        assert!(
            ended_line(&Ended::Unstarted("no such file".to_owned()))
                .is_some_and(|line| line.contains("no such file"))
        );
    }
}
