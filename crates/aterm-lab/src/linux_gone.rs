// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `linux gone` and `linux proof` rows verifying orderly `aterm-gui` shutdown when Xvfb or Weston dies ([`GONE`]).
use std::path::Path;

use crate::linux_guest::{self as guest, Display};
use crate::linux_proof::{Gui, Verdict, field};

/// Displays tested by the display-gone proof (`Xvfb` and `Weston`).
pub const GONE: [Display; 2] = [Display::Xvfb, Display::Weston];

/// Scratch folder reported via OSC 7 to verify layout persistence.
pub const FOLDER: &str = "gone-folder";

/// Shell command typed to report [`FOLDER`] via OSC 7 and print [`ECHO`].
pub const TYPED: &str =
    "cd ~/gone-folder && printf '\\033]7;file://localhost%s\\007' \"$PWD\" && echo GONE-$((6*7))";
pub const ECHO: &str = "GONE-42";

/// Message logged by `aterm-gui` when its display connection ends.
pub const SAID: &str = "display connection ended";

/// Rust's exit status for a panic that unwound out of `main`.
const PANIC_EXIT: i32 = 101;

/// How long the mid-capture row leaves aterm-gui alone before it freezes the
/// server ([`End::MidCapture`]): three of aterm-gui's crash journal write
/// intervals (`crash_journal::MIN_WRITE_INTERVAL`, 2 s) and one more second,
/// time for the capture the row's last wake (`status`) owed and the settling
/// one after it.
const CAPTURE_QUIET: std::time::Duration = std::time::Duration::from_secs(7);

/// How a display-gone row terminates `aterm-gui`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// Display killed while `aterm-gui` is idle at the prompt.
    AtRest,
    /// Display frozen with `SIGSTOP`, sent a resize, and killed with unread requests queued.
    MidResize,
    /// Display killed mid-capture (X11 only, [`End::applies`]).
    MidCapture,
    /// Display left running while the user invokes `Quit`.
    Quit,
}

impl End {
    pub const ALL: [End; 4] = [End::AtRest, End::MidResize, End::MidCapture, End::Quit];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            End::AtRest => "display gone at rest",
            End::MidResize => "display gone mid-resize",
            End::MidCapture => "display gone mid-capture",
            End::Quit => "a person's quit",
        }
    }

    /// Returns the scratch directory slug for this [`End`].
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            End::AtRest => "rest",
            End::MidResize => "resize",
            End::MidCapture => "capture",
            End::Quit => "quit",
        }
    }

    /// What ended the run, for a line that says aterm-gui outlived it.
    const fn cause(self) -> &'static str {
        match self {
            End::AtRest | End::MidResize | End::MidCapture => "its display died",
            End::Quit => "the person's quit",
        }
    }

    /// Returns whether this [`End`] applies to `display`.
    #[must_use]
    pub fn applies(self, display: Display) -> bool {
        self != End::MidCapture || display.protocol() == "X11"
    }
}

/// Artifacts collected after `aterm-gui` exits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Exit status recorded by the shell wrapper, or `None` if still running.
    pub status: Option<String>,
    /// Captured stderr output.
    pub stderr: String,
    /// Contents of `session.toml`, if written.
    pub session: Option<String>,
    /// Crash journals (`journal-*`) found in the data directory.
    pub journals: Option<Vec<String>>,
    /// Folder reported by the shell that the saved layout must preserve.
    pub folder: String,
    /// Unread bytes in the frozen server's `Recv-Q` during [`End::MidCapture`] ([`guest::unread_bytes`]).
    pub unread: Option<String>,
}

/// Evaluates `e` for a run ending with `end`.
///
/// # Errors
/// aterm-gui did not exit, panicked or was killed; for a dead display, it
/// exited 0 or never said why; for a person's quit, it exited nonzero or said its display
/// connection ended; either way, it left no saved layout naming the folder,
/// or left a crash journal.
pub fn judge(e: &Evidence, end: End) -> Result<String, String> {
    let Some(status) = e.status.as_deref().map(str::trim) else {
        return Err(format!(
            "aterm-gui was still running a minute after {}",
            end.cause()
        ));
    };
    if let Some(line) = e.stderr.lines().find(|l| l.contains("panicked at")) {
        return Err(format!(
            "aterm-gui panicked (exit {status}): {}",
            crate::util::one_line(line, 300)
        ));
    }
    let code: i32 = status
        .parse()
        .map_err(|_| format!("aterm-gui's exit status {status:?} is not a number"))?;
    match code {
        PANIC_EXIT => return Err(format!("aterm-gui exited {PANIC_EXIT}, a panic's status")),
        c if c > 128 => return Err(format!("aterm-gui was killed by signal {}", c - 128)),
        _ => {}
    }
    let said = e.stderr.lines().find(|l| l.contains(SAID));
    let why = if end == End::Quit {
        if code != 0 {
            return Err(format!(
                "aterm-gui exited {code} after a person's quit, not 0; its stderr ends: {}",
                crate::util::one_line(&tail(&e.stderr, 3), 300)
            ));
        }
        if let Some(said) = said {
            return Err(format!(
                "a person's quit said its {SAID}: {}",
                crate::util::one_line(said.trim(), 200)
            ));
        }
        "nothing about the display".to_string()
    } else {
        if code == 0 {
            return Err("aterm-gui exited 0: a lost display is not a person's quit".into());
        }
        let Some(said) = said else {
            return Err(format!(
                "aterm-gui exited {code} without a line saying its {SAID}; its stderr ends: {}",
                crate::util::one_line(&tail(&e.stderr, 3), 300)
            ));
        };
        format!("\"{}\"", crate::util::one_line(said.trim(), 200))
    };
    let Some(session) = e.session.as_deref() else {
        return Err(format!(
            "aterm-gui exited {code} and saved no session.toml: the layout is lost"
        ));
    };
    if !session.contains(&e.folder) {
        return Err(format!(
            "the saved session.toml does not hold the tab's folder {}",
            e.folder
        ));
    }
    let Some(journals) = e.journals.as_deref() else {
        return Err("the crash journal inventory could not be read".into());
    };
    if !journals.is_empty() {
        return Err(format!(
            "a crash journal was left beside session.toml: {}",
            journals.join(", ")
        ));
    }
    let unread = e.unread.as_deref().map_or(String::new(), |u| {
        format!("; the server held {u} unread at its kill")
    });
    Ok(format!(
        "exit {code}; said {why}; session.toml holds {}; no crash journal left{unread}",
        e.folder
    ))
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

/// Runs `aterm-gui` under `display`, terminates it via `end`, and evaluates the resulting [`Evidence`].
pub(crate) fn drive(
    gui: &Gui<'_>,
    display: Display,
    end: End,
    results: &Path,
) -> (Verdict, String) {
    if let Err(e) = gui.prepare() {
        return (Verdict::NotRun, format!("scratch world: {e}"));
    }
    let folder = format!("{}/home/{FOLDER}", gui.dir);
    if !gui
        .up
        .sh(&[
            "install".into(),
            "-d".into(),
            "-m".into(),
            "700".into(),
            folder.clone(),
        ])
        .ok()
    {
        return (Verdict::NotRun, format!("could not make {folder}"));
    }
    let stderr = format!("{}/gui.stderr", gui.dir);
    let status = format!("{}/gui.status", gui.dir);
    if let Err(why) = gui.start_watched(display, &stderr, &status) {
        return (Verdict::Fail, why);
    }
    let result = run(gui, display, end, &folder, &stderr, &status);
    gui.stop();
    let why = match result.and_then(|evidence| judge(&evidence, end)) {
        Ok(detail) => return (Verdict::Pass, detail),
        Err(why) => why,
    };
    let said = gui.up.sh(&["cat".into(), stderr]);
    (
        Verdict::Fail,
        format!(
            "{why}\n    aterm-gui's stderr ends:\n    {}{}",
            said.tail(12).replace('\n', "\n    "),
            gui.keep_crash_reports(results)
        ),
    )
}

/// Drive it to the echo, end the run as `end` says (the display killed, or
/// the person's quit), wait for aterm-gui to end, and collect what it left.
fn run(
    gui: &Gui<'_>,
    display: Display,
    end: End,
    folder: &str,
    stderr: &str,
    status: &str,
) -> Result<Evidence, String> {
    if !gui.up.sh(&guest::wait_socket(&gui.sock(), 60)).ok() {
        return Err("the control socket never listened (60 s)".into());
    }
    gui.expect(&["ready", "60000"], &|r| r.starts_with("OK ready"))?;
    gui.expect(&["send", TYPED], &|r| r.starts_with("OK"))?;
    gui.expect(&["key", "enter"], &|r| r.starts_with("OK"))?;
    gui.expect(&["await", "match", ECHO, "timeout=60000"], &|r| {
        r.starts_with("OK match")
    })?;
    gui.expect(&["status"], &|r| field(r, "render") == Some("ok"))?;
    let units = display.units(&gui.up.runtime, gui.up.config()?);
    let signal = |signal: &str| -> Result<(), String> {
        for unit in units.iter().rev() {
            let sent = gui.up.sh(&guest::signal_unit(&unit.name, signal));
            if !sent.ok() {
                return Err(format!(
                    "could not {signal} {}: {}",
                    unit.name,
                    sent.tail(3)
                ));
            }
        }
        Ok(())
    };
    if end == End::Quit {
        // The menu's Quit, as a person chooses it; the display stays up.
        gui.expect(&["invoke", "Quit"], &|r| r.starts_with("OK"))?;
    }
    let server = units.last().map_or("", |u| u.name.as_str());
    let mut unread = String::new();
    if matches!(end, End::MidResize | End::MidCapture) {
        if end == End::MidCapture {
            // Quiet until the captures the row's own wakes owed are taken and
            // the write interval has run out, so the resize's wake owes one
            // due on its own turn.
            std::thread::sleep(CAPTURE_QUIET);
        }
        signal("SIGSTOP")?;
        let resize = guest::Unit {
            name: format!("{}resize", guest::UNIT_PREFIX),
            env: Vec::new(),
            argv: gui.ctl_argv(&["resize", "30", "100"]),
            socket: String::new(),
        };
        let started = gui.up.sh(&guest::run_unit(&resize, None));
        if !started.ok() {
            return Err(format!("could not send the resize: {}", started.tail(3)));
        }
        if !gui.up.sh(&guest::wait_unread(server, 60)).ok() {
            return Err(format!(
                "aterm-gui sent the frozen {server} nothing for the resize (60 s)"
            ));
        }
        if end == End::MidCapture {
            unread = gui
                .up
                .sh(&guest::unread_bytes(server))
                .text
                .trim()
                .to_string();
        }
    }
    if end != End::Quit {
        signal("SIGKILL")?;
    }
    let ended = gui.up.sh(&guest::wait_file(status, 60)).ok();
    let read = |path: &str| {
        let out = gui.up.sh(&["cat".into(), path.into()]);
        out.ok().then_some(out.text)
    };
    let data = format!("{}/home/.local/share/aterm", gui.dir);
    let journals = gui.up.sh(&[
        "find".into(),
        data.clone(),
        "-maxdepth".into(),
        "1".into(),
        "-name".into(),
        "journal-*".into(),
    ]);
    Ok(Evidence {
        status: if ended { read(status) } else { None },
        stderr: read(stderr).unwrap_or_default(),
        session: read(&format!("{data}/session.toml")),
        journals: journals.ok().then(|| {
            journals
                .text
                .lines()
                .filter(|l| l.starts_with('/'))
                .map(|l| l.rsplit('/').next().unwrap_or(l).to_string())
                .collect()
        }),
        folder: folder.to_string(),
        unread: (!unread.is_empty()).then_some(unread),
    })
}

#[cfg(test)]
#[path = "linux_gone_tests.rs"]
mod tests;
