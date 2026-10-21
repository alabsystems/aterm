// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The display dies under a running aterm-gui (COMPOSITOR-GONE,
//! `docs/AUDIT-third-party-2026-10-15.md`, the Linux lane's findings of
//! 2026-10-16): a row of `linux proof` per arch for Xvfb and weston
//! ([`GONE`]), and `linux gone`, which runs those rows alone.
//!
//! The lane found aterm-gui panicking when weston crashed under it
//! (`docs/measured/linux-lw2-2026-10-16.md`): every client got `ECONNRESET`,
//! and aterm's next surface configure panicked in wgpu's uncaptured-error
//! handler. A dead display connection must end aterm-gui the way a logout
//! ends it: the session saved, a line saying why, a nonzero exit, never a
//! panic. The row starts aterm-gui under the display in a scratch world with
//! its stderr and exit status kept in files, has its shell report a folder of
//! its own (OSC 7, so the saved layout names it), then kills every server of
//! the display with SIGKILL, as a crash ends it, and waits (a one-minute hang
//! detector) for aterm-gui to exit. It passes only when aterm-gui exited by
//! itself with a nonzero status that is not a panic's, said that its display
//! connection ended, wrote `session.toml` holding that folder, and left no
//! crash journal behind ([`judge`]).
//!
//! Each display dies at two moments ([`End`]): with aterm-gui at rest, and
//! mid-resize, the order of the lane's weston crash: the server frozen, a
//! resize sent, the server killed once aterm-gui's requests wait unread in its
//! socket, so the replies aterm-gui waits on never come. Under the wgpu
//! oracle weston's mid-resize row also holds the fix that incident needed:
//! aterm-gui must say the resize's surface configure was rejected, the
//! scope's catch ([`SCOPED`]), or the row did not test it ([`scope_held`]).
//! An X11 display dies at a third ([`End::MidCapture`]): mid-resize on a turn
//! that owes the crash journal its capture, so the capture asks the window's
//! position (`XTranslateCoordinates`, which upstream's X11 backend unwrapped)
//! of a connection that broke before the loop looked at it, the panic the
//! proof of 2026-10-20 found (X11-GONE).
//!
//! The last row per display is their control: the display stays up and the
//! person quits (`invoke Quit`, the menu's Quit). That end is the person's
//! choice, so it must exit 0, say nothing about the display, and still save
//! the layout: only a dead display connection may take the logout's way out.

use std::path::Path;

use crate::linux_guest::{self as guest, Display};
use crate::linux_proof::{Gui, Verdict, field};

/// The displays a proof kills under aterm-gui: X11's and Wayland's, one
/// server each.
pub const GONE: [Display; 2] = [Display::Xvfb, Display::Weston];

/// The folder the shell moves to and reports (OSC 7), under the scratch
/// HOME, so the saved layout can be told from a default one.
pub const FOLDER: &str = "gone-folder";

/// What is typed: into [`FOLDER`], its path reported as OSC 7 names it, and
/// an echo whose output [`ECHO`] the typed text never contains, so its
/// appearance says the shell ran the line (and so had reported the folder
/// first).
pub const TYPED: &str =
    "cd ~/gone-folder && printf '\\033]7;file://localhost%s\\007' \"$PWD\" && echo GONE-$((6*7))";
pub const ECHO: &str = "GONE-42";

/// What aterm-gui's line says when its display connection ended
/// (`aterm-gui`'s `lib.rs`, after `run_app`).
pub const SAID: &str = "display connection ended";

/// What aterm-gui says when the wgpu arm's scoped surface configure caught a
/// rejection and latched it as a loss (aterm-gpu's
/// `configure_surface_retagging_scrgb`, read out by the frontend's downgrade
/// line): the proof that a resize reached the scope.
pub const SCOPED: &str = "configure was rejected";

/// Rust's exit status for a panic that unwound out of `main`.
const PANIC_EXIT: i32 = 101;

/// How long the mid-capture row leaves aterm-gui alone before it freezes the
/// server ([`End::MidCapture`]): three of aterm-gui's crash journal write
/// intervals (`crash_journal::MIN_WRITE_INTERVAL`, 2 s) and one more second,
/// time for the capture the row's last wake (`status`) owed and the settling
/// one after it. Then no capture is owed and none was taken within the
/// interval, so the next wake that moves anything owes one due at once.
const CAPTURE_QUIET: std::time::Duration = std::time::Duration::from_secs(7);

/// How a row ends aterm-gui's run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The display killed at aterm-gui's prompt, a frame on glass, nothing
    /// asked of the server.
    AtRest,
    /// The display killed mid-resize: the server frozen (SIGSTOP), `resize 30
    /// 100` sent, and the server killed once aterm-gui's requests wait unread
    /// in its socket.
    MidResize,
    /// The display killed mid-capture (X11 only, [`End::applies`]): as
    /// mid-resize, with aterm-gui left alone first ([`CAPTURE_QUIET`]) so the
    /// resize's wake owes the crash journal a capture due on its own turn.
    /// That turn's requests fail once the server dies, and the capture asks
    /// the window's position (`XTranslateCoordinates`) before the loop's next
    /// look ends the run, as on the proof of 2026-10-20, where upstream's
    /// unwrap panicked. Mid-resize itself freezes the server right after the
    /// row's `status`, within the write interval of the capture before it, so
    /// its turn has none due and the run ends at the loop's look.
    MidCapture,
    /// The display left up, and the person quits (`invoke Quit`).
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

    /// For a scratch directory's name.
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

    /// Whether this end is a row under `display`: the capture's only where
    /// asking a window's position is a round trip, X11. A Wayland client
    /// cannot ask its position (`outer_position` is not supported there), so
    /// a capture asks the compositor nothing and the row would have no
    /// request to wait on.
    #[must_use]
    pub fn applies(self, display: Display) -> bool {
        self != End::MidCapture || display.protocol() == "X11"
    }
}

/// What the row found once aterm-gui's run was over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// aterm-gui's exit status as its shell wrapper wrote it (`$?`: 128 plus
    /// the signal when one killed it); `None` when it was still running a
    /// minute after the display died or the person quit.
    pub status: Option<String>,
    /// Its whole stderr.
    pub stderr: String,
    /// `session.toml` in its data dir, when it was written.
    pub session: Option<String>,
    /// The crash journals left in that dir (`journal-*`), or `None` when
    /// listing them failed: uncertainty is not an empty inventory.
    pub journals: Option<Vec<String>>,
    /// The folder the shell reported, which the saved layout must name.
    pub folder: String,
    /// What the frozen server held unread from aterm-gui when it was killed
    /// mid-capture (`ss`'s Recv-Q, [`guest::unread_bytes`]), for the record.
    pub unread: Option<String>,
}

/// Whether a row's dead display must show the wgpu arm's scoped surface
/// configure catching the rejection ([`SCOPED`]): aterm-gui draws on the wgpu
/// oracle, the display dies mid-resize, and it speaks Wayland, whose client
/// sizes its own surface, so the resize's configure runs at once and meets the
/// frozen, then dead, compositor (the 2026-10-16 incident's order). An X11
/// resize waits for the server's ConfigureNotify, which a frozen server never
/// sends: aterm-gui configures nothing before the connection dies (the lane,
/// 2026-10-17), so that row holds the exit alone.
#[must_use]
pub fn scope_held(wgpu_oracle: bool, display: Display, end: End) -> bool {
    wgpu_oracle && end == End::MidResize && display.protocol() == "Wayland"
}

/// The row's verdict on `e`, a run that ended as `end` says: `Ok` with the
/// detail, or `Err` naming the first thing that was not an orderly end.
/// `scoped` ([`scope_held`]): the dead display must also show the resize's
/// configure rejected inside its error scope.
///
/// # Errors
/// aterm-gui did not exit, panicked or was killed; for a dead display, it
/// exited 0, never said why, or (`scoped`) never reached the scoped
/// configure; for a person's quit, it exited nonzero or said its display
/// connection ended; either way, it left no saved layout naming the folder,
/// or left a crash journal.
pub fn judge(e: &Evidence, end: End, scoped: bool) -> Result<String, String> {
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
        let said = format!("\"{}\"", crate::util::one_line(said.trim(), 200));
        if !scoped {
            said
        } else if let Some(caught) = e.stderr.lines().find(|l| l.contains(SCOPED)) {
            format!(
                "{said} after \"{}\"",
                crate::util::one_line(caught.trim(), 200)
            )
        } else {
            return Err(format!(
                "the resize never reached the scoped surface configure: no line says a \
                 configure was rejected ({SCOPED:?}), so this row did not test the scope"
            ));
        }
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

/// Start aterm-gui under `display` in `gui`'s prepared world, end its run as
/// `end` says, and judge how aterm-gui ended (`wgpu_oracle`: it draws on the
/// wgpu oracle, [`scope_held`]): the row's verdict and detail. A failure
/// keeps the crash reports aterm-gui wrote in `results`.
pub(crate) fn drive(
    gui: &Gui<'_>,
    display: Display,
    end: End,
    wgpu_oracle: bool,
    results: &Path,
) -> (Verdict, String) {
    let scoped = scope_held(wgpu_oracle, display, end);
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
    let why = match result.and_then(|evidence| judge(&evidence, end, scoped)) {
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
    // A frame on glass before the kill, so the dead connection meets a live
    // swapchain (the case the lane panicked in), not a window never drawn.
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
    // The client talks to the display's own server, the last started.
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
        // Its reply waits on the frozen server, so it runs as a unit of its own.
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
            // As weston killed mid-resize left it (the Linux lane, 2026-10-17).
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
            let detail = judge(&orderly(), end, false).expect("passes");
            assert!(detail.starts_with("exit 69;"), "{detail}");
        }
        let detail = judge(&quit(), End::Quit, false).expect("passes");
        assert!(detail.starts_with("exit 0;"), "{detail}");
    }

    /// NEGATIVE CONTROLS: what the lane saw before the fix, and each way an
    /// end can fall short of orderly, fail.
    #[test]
    fn a_panic_a_kill_a_silent_or_unsaved_end_fails() {
        let judge = |e: &Evidence| judge(e, End::AtRest, false);
        // The weston crash of 2026-10-16: wgpu's default handler panicked.
        let panicked = Evidence {
            status: Some("101".into()),
            stderr: "thread 'main' panicked at wgpu-29.0.3/src/backend/wgpu_core.rs:\n\
                     Surface does not support the adapter's queue family\n"
                .into(),
            session: None,
            ..orderly()
        };
        assert!(judge(&panicked).unwrap_err().contains("panicked"));
        // A panic whose line never reached stderr still has a panic's status.
        let status_only = Evidence {
            status: Some("101".into()),
            stderr: String::new(),
            ..orderly()
        };
        assert!(judge(&status_only).unwrap_err().contains("panic"));
        // Xlib's default IO error handler: `exit(1)`, nothing saved.
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
        let judge = |e: &Evidence| judge(e, End::Quit, false);
        // An X11 error at the quit's last sync, reported as a lost display.
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

    /// Under the wgpu oracle weston's mid-resize death passes only once the
    /// resize reached the scoped configure and its rejection was said; the
    /// rows at rest, X11's and the GL arm's ask nothing of it.
    #[test]
    fn the_oracle_s_mid_resize_row_needs_the_scoped_configure() {
        assert!(scope_held(true, Display::Weston, End::MidResize));
        assert!(scope_held(true, Display::Mutter, End::MidResize));
        assert!(!scope_held(true, Display::Xvfb, End::MidResize));
        assert!(!scope_held(true, Display::Weston, End::AtRest));
        assert!(!scope_held(true, Display::Weston, End::Quit));
        assert!(!scope_held(false, Display::Weston, End::MidResize));
        let missed = judge(&orderly(), End::MidResize, true).unwrap_err();
        assert!(missed.contains("never reached the scoped"), "{missed}");
        let caught = Evidence {
            stderr: format!(
                "aterm-gui: GPU device lost (the resize configure was rejected: Validation \
                 Error In Surface::configure Surface does not support the adapter's queue \
                 family) — downgrading to the CPU renderer so windows keep rendering\n{}",
                orderly().stderr
            ),
            ..orderly()
        };
        let detail = judge(&caught, End::MidResize, true).expect("passes");
        assert!(detail.contains("downgrading"), "{detail}");
        // A row `scope_held` leaves out passes on the exit and the line alone.
        assert!(judge(&orderly(), End::MidResize, false).is_ok());
        assert!(judge(&orderly(), End::AtRest, false).is_ok());
    }

    /// The capture's row is X11's alone: a Wayland client asks the
    /// compositor nothing for its position. Its record says what the frozen
    /// server held, and a panic there (the proof of 2026-10-20, upstream's
    /// unwrap of `XTranslateCoordinates`) fails it.
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
        let detail = judge(&held, End::MidCapture, false).expect("passes");
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
            judge(&panicked, End::MidCapture, false)
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
