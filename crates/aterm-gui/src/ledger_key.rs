// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ledger key: with a session focused, ⇧⌘L (`cmd+shift+l`; `ctrl+shift+l` off
//! macOS; the `open_ledger` action in `[keybindings]`) runs
//! `aterm drive ledger @<sid> --format html --out <file>` and opens the file.
//!
//! The ledger is the watcher's timeline: the human's turns, reply sizes, the
//! watcher's journal lines and the session's mail with the worker (the long form of
//! the presence band's `◇ quiet` summary). It runs as a child process because it
//! gathers over the control socket (`history`, `timeline`, `inbox --peek`), which
//! in-process on the main thread would block the event loop on its own socket. The
//! child gets `drive --socket <this instance's socket>` ([`with_socket`]) so it reads
//! this instance's sessions; the CLI creates the file `0600`.
//!
//! A headless instance composes the plan and launches nothing.

use std::path::{Path, PathBuf};

/// The command a ledger keystroke composes and where the file goes; kept on the
/// `App` (`last_ledger_plan`) so tests can read what the key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerPlan {
    /// The focused session's stable id (`s-…`), as `@<sid>` names it.
    pub(crate) sid: String,
    /// The HTML file the CLI writes.
    pub(crate) out: PathBuf,
    /// The CLI's argv after the program: `drive ledger @<sid> --format html
    /// --out <file>`.
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

/// Output path for `sid`'s ledger: `aterm-ledger-<sid>-<unix seconds>.html` in the
/// temp dir, one per keystroke, so a ledger open in a browser is never rewritten.
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

/// The `aterm` CLI the key runs: the one beside this executable (`Contents/MacOS`
/// in the `.app`, `target/<profile>/` in a dev tree), else `aterm` on `PATH`.
pub(crate) fn aterm_cli() -> PathBuf {
    let name = format!("aterm{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// `argv` (a `drive …` command line) pointed at the instance at `sock`:
/// `drive --socket <sock> …`. The socket is chosen by flag, not environment.
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

/// Run `plan` on its own thread (gathering can take seconds), then open the file.
/// `sock` is this instance's control socket. Failures are logged, never raised.
pub(crate) fn launch(plan: LedgerPlan, sock: Option<String>) {
    let spawned = std::thread::Builder::new()
        .name("aterm-ledger-key".to_string())
        .spawn(move || {
            // Waits on a CLI child the human is not blocked on.
            crate::qos::set_self(crate::qos::Role::Background);
            let cli = aterm_cli();
            // `aterm.exe` is the CONSOLE image: started bare by the window (which
            // has no console) every key press opened a VISIBLE console window for
            // the child's whole run on Windows (measured 2026-09-30);
            // `spawnfd::command` gives it a windowless one. Not `qos::command`:
            // the child is an aterm, which must not inherit a clamp.
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
        });
    if let Err(e) = spawned {
        aterm_log::warn!("ledger key: cannot spawn its thread: {e}");
    }
}

/// Open `path` with the OS default handler for `.html`, via the same helper as the
/// terminal's link open.
fn open_file(path: &Path) {
    let url = format!("file://{}", path.display());
    crate::app_mouse::open_url_external(&url);
}

impl crate::App {
    /// The ledger plan for `wid`'s focused session, `None` with no session
    /// focused (a native tab, an empty window).
    pub(crate) fn ledger_plan(&self, wid: crate::WindowId) -> Option<LedgerPlan> {
        let session = self.focused_session_id(wid)?;
        let sid = self.pool.get(session)?.ctx.self_id.as_str().to_string();
        Some(plan_for(&sid))
    }

    /// Compose the ledger plan for the focused session, remember it, and run it when
    /// this window is a real OS window (headless stops at the plan).
    pub(crate) fn open_session_ledger(&mut self, wid: crate::WindowId) {
        let Some(plan) = self.ledger_plan(wid) else {
            return;
        };
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

    /// The argv is the documented command, `@<sid>` first, HTML to the file.
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
        // This instance's socket rides the argv as a `drive` flag.
        assert_eq!(
            with_socket(&plan.argv, Some("/run/aterm/aterm-7.sock"))[..4],
            ["drive", "--socket", "/run/aterm/aterm-7.sock", "ledger"]
        );
        assert_eq!(with_socket(&plan.argv, None), plan.argv);
        // The front door, never the GUI binary.
        assert!(!aterm_cli().to_string_lossy().contains("aterm-gui"));
    }
}
