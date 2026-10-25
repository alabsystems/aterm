// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keybinding and menu action to export and open a session's harness ledger as HTML.

use std::path::{Path, PathBuf};

/// Command plan and output path for a session ledger export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerPlan {
    /// The focused session's stable id (`s-…`), as `@<sid>` names it.
    pub(crate) sid: String,
    /// The HTML file the CLI writes.
    pub(crate) out: PathBuf,
    /// The CLI's argv after the program name.
    pub(crate) argv: Vec<String>,
}

/// The argv `aterm` runs for the ledger of `sid`, written to `out`.
pub(crate) fn ledger_argv(sid: &str, out: &Path) -> Vec<String> {
    vec![
        "drive".to_string(),
        "ledger".to_string(),
        format!("@{sid}"),
        "--format".to_string(),
        "html".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Output path for `sid`'s ledger in the system temp directory.
pub(crate) fn ledger_out_path(sid: &str, unix_s: u64) -> PathBuf {
    let safe: String = sid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    std::env::temp_dir().join(format!("aterm-ledger-{safe}-{unix_s}.html"))
}

/// The plan for `sid`, now.
pub(crate) fn plan_for(sid: &str) -> LedgerPlan {
    let unix_s = aterm_time::unix_secs().unwrap_or(0);
    let out = ledger_out_path(sid, unix_s);
    LedgerPlan {
        sid: sid.to_string(),
        argv: ledger_argv(sid, &out),
        out,
    }
}

/// The front door, `aterm` (`aterm.exe` on Windows), beside `exe`, when `is_file` finds it there:
/// the name the router answers with the front door's verbs. A window started under its alias
/// (`aterm-gui`, which the Windows Start Menu and jump list launch) has the alias as its own path,
/// and the router hands a window alias's arguments to a window, which refuses `pkg`, `ai` and the
/// rest.
fn front_door_beside(exe: &Path, is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let name = format!("aterm{}", std::env::consts::EXE_SUFFIX);
    exe.parent()
        .map(|dir| dir.join(name))
        .filter(|path| is_file(path))
}

/// Resolve the `aterm` CLI executable path: [`front_door_beside`] this process's own, else the
/// bare name for the shell's `PATH`.
pub(crate) fn aterm_cli() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| front_door_beside(&exe, Path::is_file))
        .unwrap_or_else(|| PathBuf::from(format!("aterm{}", std::env::consts::EXE_SUFFIX)))
}

/// This process's front door by its full path: [`front_door_beside`] its own path, else that
/// path itself (a build with no `aterm` beside it).
pub(crate) fn own_front_door() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(front_door_beside(&exe, Path::is_file).unwrap_or(exe))
}

/// Prepend `--socket <sock>` after `drive` when `sock` is present.
pub(crate) fn with_socket(argv: &[String], sock: Option<&str>) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len() + 2);
    out.extend(argv.first().cloned());
    if let Some(sock) = sock {
        out.push("--socket".to_string());
        out.push(sock.to_string());
    }
    out.extend(argv.iter().skip(1).cloned());
    out
}

/// Run `plan` on a background thread and open the resulting HTML file.
pub(crate) fn launch(plan: LedgerPlan, sock: Option<String>) {
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-ledger-key".to_string(),
        crate::qos::Role::Background,
        move || {
            let cli = aterm_cli();
            let mut cmd = aterm_uds::spawnfd::command(&cli);
            cmd.args(with_socket(&plan.argv, sock.as_deref()));
            match cmd.output() {
                Ok(out) if out.status.success() => {
                    aterm_log::info!("ledger key: wrote {} for {}", plan.out.display(), plan.sid);
                    open_file(&plan.out);
                }
                Ok(out) => aterm_log::warn!(
                    "ledger key: `{} {}` exited {}: {}",
                    cli.display(),
                    plan.argv.join(" "),
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => aterm_log::warn!("ledger key: cannot run {}: {e}", cli.display()),
            }
        },
    );
    if let Err(e) = spawned {
        aterm_log::warn!("ledger key: cannot spawn its thread: {e}");
    }
}

fn open_file(path: &Path) {
    let url = format!("file://{}", path.display());
    crate::app_mouse::open_url_external(&url);
}

impl crate::App {
    /// Compose the ledger plan for the focused session and launch it on real windows.
    pub(crate) fn open_session_ledger(&mut self, wid: crate::WindowId) {
        if let Some(session) = self.focused_session_id(wid) {
            self.open_session_ledger_for(wid, session);
        }
    }

    /// Compose the ledger plan for `session` and launch it on real windows.
    pub(crate) fn open_session_ledger_for(&mut self, wid: crate::WindowId, session: u64) {
        let Some(sid) = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
        else {
            return;
        };
        let plan = plan_for(&sid);
        let real_window = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.os_window.is_some());
        self.last_ledger_plan = Some(plan.clone());
        if real_window {
            launch(plan, crate::proxy::self_sock_path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_runs_drive_ledger_for_the_session_as_html_into_a_private_file() {
        let out = Path::new("/tmp/aterm-ledger-s-1e91-1.html");
        assert_eq!(
            ledger_argv("s-1e918c4662a1b7b8bd43", out),
            [
                "drive",
                "ledger",
                "@s-1e918c4662a1b7b8bd43",
                "--format",
                "html",
                "--out",
                "/tmp/aterm-ledger-s-1e91-1.html",
            ]
        );
        let p = ledger_out_path("s-ab/../cd", 7);
        let name = p
            .file_name()
            .expect("a file")
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "aterm-ledger-s-abcd-7.html", "the sid is sanitized");
        assert!(p.starts_with(std::env::temp_dir()));
        let plan = plan_for("s-1");
        assert_eq!(plan.sid, "s-1");
        assert_eq!(plan.argv[2], "@s-1");
        assert_eq!(plan.argv[6], plan.out.to_string_lossy());
        assert_eq!(
            with_socket(&plan.argv, Some("/run/aterm/aterm-7.sock"))[..4],
            ["drive", "--socket", "/run/aterm/aterm-7.sock", "ledger"]
        );
        assert_eq!(with_socket(&plan.argv, None), plan.argv);
        assert!(!aterm_cli().to_string_lossy().contains("aterm-gui"));
    }

    #[test]
    fn a_window_alias_finds_the_front_door_beside_it() {
        let dir = Path::new(if cfg!(windows) {
            r"C:\Program Files\aterm"
        } else {
            "/Applications/aterm.app/Contents/MacOS"
        });
        let suffix = std::env::consts::EXE_SUFFIX;
        let door = dir.join(format!("aterm{suffix}"));
        let alias = dir.join(format!("aterm-gui{suffix}"));
        assert_eq!(front_door_beside(&alias, |_| true), Some(door.clone()));
        assert_eq!(
            front_door_beside(&alias, |path| path == door),
            Some(door.clone())
        );
        assert_eq!(front_door_beside(&alias, |_| false), None);
        assert_eq!(front_door_beside(&door, |_| true), Some(door.clone()));
    }
}
