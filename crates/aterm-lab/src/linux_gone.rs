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
mod tests {
    use super::*;

    const FOLDER_PATH: &str = "/tmp/aterm-lane-aarch64-weston-gone/home/gone-folder";

    fn orderly() -> Evidence {
        Evidence {
            status: Some("69\n".into()),
            stderr: "aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 128 bits) (Gl)\n\
                     Io error: Connection reset by peer (os error 104)\n\
                     aterm-gui: the display connection ended (Connection reset by peer (os error \
                     104)); quitting as a logout would, exit status 69\n"
                .into(),
            session: Some(format!("[[windows]]\ncwd = \"{FOLDER_PATH}\"\n")),
            journals: Some(Vec::new()),
            folder: FOLDER_PATH.into(),
            unread: None,
        }
    }

    /// A person's quit as it ends: exit 0, nothing said about the display.
    fn quit() -> Evidence {
        Evidence {
            status: Some("0\n".into()),
            stderr: "aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 128 bits) (Gl)\n".into(),
            ..orderly()
        }
    }

    #[test]
    fn an_orderly_end_passes() {
        for end in [End::AtRest, End::MidResize, End::MidCapture] {
            let detail = judge(&orderly(), end).expect("passes");
            assert!(detail.starts_with("exit 69;"), "{detail}");
        }
        let detail = judge(&quit(), End::Quit).expect("passes");
        assert!(detail.starts_with("exit 0;"), "{detail}");
    }

    /// NEGATIVE CONTROLS: what the lane saw before the fix, and each way an
    /// end can fall short of orderly, fail.
    #[test]
    fn a_panic_a_kill_a_silent_or_unsaved_end_fails() {
        let judge = |e: &Evidence| judge(e, End::AtRest);
        let panicked = Evidence {
            status: Some("101".into()),
            stderr: "thread 'main' panicked at wgpu-29.0.3/src/backend/wgpu_core.rs:\n\
                     Surface does not support the adapter's queue family\n"
                .into(),
            session: None,
            ..orderly()
        };
        assert!(judge(&panicked).unwrap_err().contains("panicked"));
        let status_only = Evidence {
            status: Some("101".into()),
            stderr: String::new(),
            ..orderly()
        };
        assert!(judge(&status_only).unwrap_err().contains("panic"));
        let xio = Evidence {
            status: Some("1".into()),
            stderr:
                "XIO:  fatal IO error 11 (Resource temporarily unavailable) on X server \":91\"\n"
                    .into(),
            session: None,
            ..orderly()
        };
        assert!(judge(&xio).unwrap_err().contains("without a line"));
        let unsaved = Evidence {
            session: None,
            ..orderly()
        };
        assert!(judge(&unsaved).unwrap_err().contains("no session.toml"));
        let other_folder = Evidence {
            session: Some("[[windows]]\ncwd = \"/tmp\"\n".into()),
            ..orderly()
        };
        assert!(judge(&other_folder).unwrap_err().contains("does not hold"));
        let journal_left = Evidence {
            journals: Some(vec!["journal-4242-1.toml".into()]),
            ..orderly()
        };
        assert!(judge(&journal_left).unwrap_err().contains("crash journal"));
        let unreadable_inventory = Evidence {
            journals: None,
            ..orderly()
        };
        assert!(judge(&unreadable_inventory).is_err());
        let killed = Evidence {
            status: Some("134".into()),
            ..orderly()
        };
        assert!(judge(&killed).unwrap_err().contains("signal 6"));
        let zero = Evidence {
            status: Some("0".into()),
            ..orderly()
        };
        assert!(judge(&zero).unwrap_err().contains("exited 0"));
        let hung = Evidence {
            status: None,
            ..orderly()
        };
        assert!(judge(&hung).unwrap_err().contains("still running"));
    }

    /// A PERSON'S QUIT is not a lost display: one that took the logout's way
    /// out (exit 69 and its line, as every `run_app` error did before GONE-2),
    /// exited with any other error, or lost the layout fails.
    #[test]
    fn a_quit_that_is_not_the_persons_fails() {
        let judge = |e: &Evidence| judge(e, End::Quit);
        let as_logout = Evidence {
            stderr: "aterm-gui: the display connection ended (os error at x11/mod.rs:350: X \
                     error: BadWindow); quitting as a logout would, exit status 69\n"
                .into(),
            ..orderly()
        };
        assert!(judge(&as_logout).unwrap_err().contains("not 0"));
        let said_only = Evidence {
            status: Some("0".into()),
            ..orderly()
        };
        assert!(judge(&said_only).unwrap_err().contains(SAID));
        let failed = Evidence {
            status: Some("70".into()),
            ..quit()
        };
        assert!(judge(&failed).unwrap_err().contains("exited 70"));
        let unsaved = Evidence {
            session: None,
            ..quit()
        };
        assert!(judge(&unsaved).unwrap_err().contains("no session.toml"));
        let hung = Evidence {
            status: None,
            ..quit()
        };
        assert!(judge(&hung).unwrap_err().contains("person's quit"));
    }

    /// The capture's row is X11's alone: a Wayland client asks the
    /// compositor nothing for its position.
    #[test]
    fn the_capture_s_row_is_x11_s_and_fails_on_its_panic() {
        let rows: Vec<Display> = Display::ALL
            .into_iter()
            .filter(|&d| End::MidCapture.applies(d))
            .collect();
        assert_eq!(rows, [Display::Xvfb, Display::Xwayland]);
        for end in [End::AtRest, End::MidResize, End::Quit] {
            assert!(Display::ALL.into_iter().all(|d| end.applies(d)), "{end:?}");
        }
        let held = Evidence {
            unread: Some("24 bytes".into()),
            ..orderly()
        };
        let detail = judge(&held, End::MidCapture).expect("passes");
        assert!(
            detail.ends_with("the server held 24 bytes unread at its kill"),
            "{detail}"
        );
        let panicked = Evidence {
            status: Some("101".into()),
            stderr: "thread 'main' panicked at crates/aterm-window/src/platform_impl/linux/x11/\
                     window.rs:1193:14:\ncalled `Result::unwrap()` on an `Err` value: \
                     X11(Connection(1))\n"
                .into(),
            session: None,
            ..held
        };
        assert!(
            judge(&panicked, End::MidCapture)
                .unwrap_err()
                .contains("panicked")
        );
    }

    /// The typed line never contains its own echo, so the echo's appearance
    /// is the shell's output.
    #[test]
    fn the_typed_line_never_contains_its_echo() {
        assert!(!TYPED.contains(ECHO));
        assert!(TYPED.contains(FOLDER));
    }
}
