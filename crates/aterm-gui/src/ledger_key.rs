// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The LEDGER KEY (round 19, design §3 / SPEC19 item 6): with a session
//! focused, one keystroke — **⇧⌘L** on macOS (`cmd+shift+l`; `ctrl+shift+l`
//! off macOS; the `open_ledger` action in `[keybindings]`) — runs
//! `aterm drive ledger @<sid> --format html --out <file>` for that session and
//! opens the file. No existing binding used `L` with Cmd-Shift (the chord
//! table in `keybinding.rs` is the roster), and `l` is what the mocks' fleet
//! screen binds to the same ledger.
//!
//! The ledger is the watcher's timeline as `aterm drive ledger` renders it:
//! the human's turns, the size of each reply, the watcher's journal lines and
//! this session's mail with the worker — the long form of the presence band's
//! `◇ quiet` summary. It is a CHILD PROCESS on purpose: the ledger gathers
//! through the control socket like any driver would (`history`, `timeline`,
//! `inbox --peek`), and doing that in-process on the main thread would park the
//! event loop on its own socket. The child is pointed at THIS instance through
//! `ATERM_CONTROL_SOCK`, so a nested or hand-launched instance reads its own
//! sessions and never another's; the reply file is created `0600` by the CLI
//! (a ledger carries what the manager typed and what the worker said).
//!
//! A headless instance (no OS window) composes the plan and launches nothing:
//! there is no human to open a browser for, and the tests read the plan.

use std::path::{Path, PathBuf};

/// The command a ledger keystroke composes: what `aterm` is asked, and where
/// the file goes. Kept on the `App` (`last_ledger_plan`) so `chrome`-side
/// tests and a future menu row can read what the key did.
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

/// Where the ledger of `sid` is written: the temp dir, one file per keystroke
/// (`aterm-ledger-<sid>-<unix seconds>.html`), so an earlier ledger left open
/// in a browser is never rewritten under it.
pub(crate) fn ledger_out_path(sid: &str, unix_s: u64) -> PathBuf {
    let safe: String = sid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    std::env::temp_dir().join(format!("aterm-ledger-{safe}-{unix_s}.html"))
}

/// The plan for `sid`, now.
pub(crate) fn plan_for(sid: &str) -> LedgerPlan {
    let unix_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let out = ledger_out_path(sid, unix_s);
    LedgerPlan {
        sid: sid.to_string(),
        argv: ledger_argv(sid, &out),
        out,
    }
}

/// The `aterm` front door the key runs: the one beside this executable (the
/// shipped `.app` keeps its CLI tools in `Contents/MacOS`, a dev tree in
/// `target/<profile>/`), else `aterm` on the child `PATH`. Never `aterm-gui`
/// itself: the ledger is a CLI verb.
pub(crate) fn aterm_cli() -> PathBuf {
    let name = format!("aterm{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Run `plan` on a thread of its own — the ledger gathers over the control
/// socket and can take seconds — and open the file once the CLI wrote it.
/// `sock` is this instance's control socket, so the child reads THESE
/// sessions. Failures are logged, never raised: a key that could not open a
/// browser must not take the window with it.
pub(crate) fn launch(plan: LedgerPlan, sock: Option<String>) {
    let spawned = std::thread::Builder::new()
        .name("aterm-ledger-key".to_string())
        .spawn(move || {
            // Waits on a CLI child the human is not blocked on.
            crate::qos::set_self(crate::qos::Role::Background);
            let cli = aterm_cli();
            let mut cmd = std::process::Command::new(&cli);
            cmd.args(&plan.argv);
            if let Some(sock) = sock {
                cmd.env("ATERM_CONTROL_SOCK", sock);
            }
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

/// Open `path` with the OS default handler for `.html` — the browser. The
/// same helper the terminal's link open uses, given a `file://` URL to a file
/// this process just had written.
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

    /// THE LEDGER KEY: compose the plan for the focused session, remember it,
    /// and — when this window is a real OS window — run it. Headless, the plan
    /// is all that happens.
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
        // The front door, never the GUI binary.
        assert!(!aterm_cli().to_string_lossy().contains("aterm-gui"));
    }
}
