// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Argv handed to a RELAUNCH of this binary — pure list surgery, no platform
//! surface at all.
//!
//! It lives outside the macOS-only installer module because every platform
//! relaunches aterm: the GUI's cold-exec and seamless handoffs, and the
//! Windows successor spawn, all forward their own argv. Compiled here, one
//! definition serves them all.

/// Forward this process's argv to a successor MINUS the leading `--window`
/// pins an earlier boot swap prepended.
///
/// A relaunch that forwards argv verbatim re-grows the pins every time (each
/// swap adds one), so the list a long-lived session hands its successor keeps
/// getting longer. Stripping only the LEADING run keeps a user-authored
/// `--window` later in the command line intact, and makes the operation a
/// fixed point: applying it twice equals applying it once.
pub fn reexec_forwarded_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let mut forwarded = Vec::new();
    let mut leading = true;
    for arg in args {
        if leading && arg == "--window" {
            continue;
        }
        leading = false;
        forwarded.push(arg);
    }
    forwarded
}

/// Whether a forwarded argv (pins already stripped) was a front-door windowing
/// VERB's: `aterm new-window|new-tab|split-pane …`. The router hands a verb's
/// window to the window library IN THE SAME PROCESS (`window_verb` → `run_window`),
/// so the process keeps the verb as its own argv — and the verb is the only bare
/// word a running window's argv can start with: a flag-led launch that reached the
/// window parser with a bare word first was refused there ("unknown command") and
/// never ran.
fn starts_with_a_verb(forwarded: &[std::ffi::OsString]) -> bool {
    forwarded
        .first()
        .is_some_and(|first| !first.to_string_lossy().starts_with('-'))
}

/// The argv of the BOOT SWAP's re-exec — the same launch, not yet served, on the
/// new build: exactly one leading `--window` pin, then [`reexec_forwarded_args`].
///
/// The one-binary router picks window or session from argv and whether stdin is a
/// terminal, before anything else runs. A window started as `aterm --window` from a
/// shell keeps that shell's terminal on stdin, so a successor forwarded only the
/// stripped list — which drops the `--window` the person typed along with any
/// earlier swap's pins — came up as a terminal SESSION in the launching terminal:
/// the seamless handoff timed out on every attempt and the cold lane replaced the
/// window with a nested session (round-seven update audit #11). Pinning here, the
/// way the boot swap always pinned its own re-exec, keeps the lane a window, and
/// stays a fixed point: however many relaunches the process rides, one pin.
///
/// A VERB'S LAUNCH IS FORWARDED AS THE VERB, UNPINNED (round eight, #6). The router
/// dispatches a verb above the mode fork, so `new-window -d ~/proj` routes itself to
/// the window it asked for, at a terminal or not; behind a pin it fell through to the
/// window parser, which refuses the bare word and exits 2 — so `aterm new-window`
/// typed while a build was staged opened nothing at all. The launch is not served yet
/// here, so its directory rides along with it.
pub fn window_successor_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let forwarded = reexec_forwarded_args(args);
    if starts_with_a_verb(&forwarded) {
        return forwarded;
    }
    let mut successor = vec![std::ffi::OsString::from("--window")];
    successor.extend(forwarded);
    successor
}

/// The flag that names a harness's lifeline descriptor — `aterm_uds::lifeline::FLAG`,
/// spelled here because this module compiles on every platform and the lifeline is
/// unix-only (a test pins the two spellings together).
const LIFELINE_FLAG: &str = "--lifeline-fd";

/// The flag that names a harness's ready signal: the write end of a pipe a headless
/// instance writes one byte to once its control socket listens (aterm-gui's `--ready-fd`).
const READY_FLAG: &str = "--ready-fd";

/// The environment variable that marks a launch as an UPDATE'S RELAUNCH OF A
/// WINDOW ALREADY RUNNING (round eight, #7's review): set by both GUI successor
/// lanes — the cold `exec` and the seamless launch — on the image they start, and
/// inherited from there by its boot swap's re-exec and a crash-loop revert's.
/// Never present on a person's launch: the window's entry reads and clears it
/// before any session spawns, so no shell and no window opened from one inherits
/// it.
///
/// What it changes: the relaunched window's FIRST session (its bootstrap) does not
/// run the launch's `-e` command, which that window already ran; every later tab
/// and split does, as the window's did. The consumer is always this build or a
/// newer one (the successor of an update, or the build a revert restores, which
/// is the one that set it), so no build that could misread it ever sees it.
pub const RELAUNCH_OF_RUNNING_ENV: &str = "ATERM_RELAUNCH_OF_RUNNING";

/// The argv of a RUNNING window's update successor — the GUI's cold `exec` and its
/// seamless launch (round eight, #6, #7 and #28): exactly one `--window` pin, then the
/// launch's own argv — how this window runs (`--headless`, `--control-sock`,
/// `--columns`, `-d`, `-e <cmd>`, …) — less what its launch ASKED FOR once and a
/// successor must not be told again:
///
/// * NO WINDOWING VERB. `aterm new-window|new-tab|split-pane` keeps the verb as the
///   window's own argv; behind the pin it reached the window parser, which exits 2 on
///   the bare word, so a verb-started window's successor died on every lane and the
///   window never updated. Forwarded unpinned (as the boot swap does) it would run the
///   routing policy again, and under `attach` hand a tab to a running instance — the
///   outgoing one included — and exit. The verb and its operands go; the successor is a
///   plain window whose desk the handoff (or the quit layout) carries.
/// * NO STALE LIFELINE. `--lifeline-fd <n>` names a descriptor the first image has
///   already moved to a private close-on-exec copy (`aterm_uds::lifeline::adopt`), so
///   the successor was told to adopt `/dev/null` or a closed number and exited 2.
///   `lifeline_fd` is the descriptor the caller hands the successor instead (the cold
///   `exec` keeps one open across it); `None` drops the flag.
/// * NO STALE READY SIGNAL. `--ready-fd <n>` names the pipe the first image already
///   wrote and closed when its socket listened; the successor would be told a closed
///   number, or another descriptor that took it. The flag and its number go.
///
/// THE `-e` COMMAND STAYS (#7's review): it is not only the first tab's — the window's
/// session factory runs it for EVERY tab and split it opens, so a successor cut at
/// `-e` opened `$SHELL` where `aterm --window -e tmux`, or a harness's
/// `--headless -e claude` then `ctl new-tab`, had opened the command. What must not
/// happen again is the FIRST session running it (a deploy script re-run at every
/// update), and that is the successor's to know: the caller marks it with
/// [`RELAUNCH_OF_RUNNING_ENV`]. Everything from the payload boundary the router's own
/// scan uses (`-e`, `--command`, `--`) on is forwarded verbatim, as the payload's own.
///
/// Same token scan as the router (no flag's value is read as a flag's name), and a
/// fixed point over its own output given the same `lifeline_fd`.
pub fn running_window_successor_args(
    args: impl Iterator<Item = std::ffi::OsString>,
    lifeline_fd: Option<i32>,
) -> Vec<std::ffi::OsString> {
    let forwarded = reexec_forwarded_args(args);
    let mut successor = vec![std::ffi::OsString::from("--window")];
    if starts_with_a_verb(&forwarded) {
        return successor;
    }
    let mut tokens = forwarded.into_iter();
    while let Some(arg) = tokens.next() {
        match arg.to_str() {
            Some("-e" | "--command" | "--") => {
                successor.push(arg);
                successor.extend(tokens.by_ref());
            }
            Some(LIFELINE_FLAG) => {
                let _stale = tokens.next();
                if let Some(fd) = lifeline_fd {
                    successor.push(std::ffi::OsString::from(LIFELINE_FLAG));
                    successor.push(std::ffi::OsString::from(fd.to_string()));
                }
            }
            Some(READY_FLAG) => {
                let _said = tokens.next();
            }
            _ => successor.push(arg),
        }
    }
    successor
}

#[cfg(test)]
mod tests {
    use super::{reexec_forwarded_args, running_window_successor_args, window_successor_args};

    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    }

    /// The field case: accumulated pins collapse to none forwarded, the strip is a
    /// fixed point, and it never reaches past the first non-pin token, so a later
    /// `--window` (the person's, or inside an `-e` payload) survives verbatim.
    #[test]
    fn stripping_the_leading_pins_is_a_fixed_point() {
        assert_eq!(
            reexec_forwarded_args(args(&["--window"; 6]).into_iter()),
            args(&[])
        );
        let once = reexec_forwarded_args(
            args(&["--window", "--window", "-e", "sh", "-c", "--window"]).into_iter(),
        );
        assert_eq!(once, args(&["-e", "sh", "-c", "--window"]));
        let twice = reexec_forwarded_args(once.clone().into_iter());
        assert_eq!(twice, once, "applying it twice equals applying it once");
    }

    /// Round eight, #6: the boot swap's re-exec of a verb's launch is the verb,
    /// unpinned — the router routes it to the window it asked for, where a pin sent
    /// the bare word to the window parser's exit 2. Every other launch keeps its pin.
    #[test]
    fn the_boot_swap_forwards_a_verbs_launch_as_the_verb() {
        for verb in [
            args(&["new-window", "-d", "/tmp"]),
            args(&["new-tab"]),
            args(&["split-pane", "-H"]),
        ] {
            assert_eq!(window_successor_args(verb.clone().into_iter()), verb);
            let pinned = [args(&["--window"]), verb.clone()].concat();
            assert_eq!(
                window_successor_args(pinned.into_iter()),
                verb,
                "a pin an earlier build put in front of the verb is stripped"
            );
        }
        let pinned = window_successor_args(args(&["-d", "/tmp"]).into_iter());
        assert_eq!(pinned, args(&["--window", "-d", "/tmp"]));
        assert_eq!(
            window_successor_args(pinned.clone().into_iter()),
            pinned,
            "a fixed point"
        );
    }

    /// Round eight, #6, #7 and #28: a RUNNING window's successor carries how the
    /// window runs — its `-e` command included (#7's review) — and neither a
    /// windowing verb nor a stale lifeline number.
    #[test]
    fn a_running_windows_successor_carries_its_flags_and_not_its_launch() {
        // #6: a verb-started window is a plain window's successor.
        for verb in [
            args(&["new-window"]),
            args(&["new-window", "-d", "/tmp"]),
            args(&["new-tab"]),
            args(&["--window", "split-pane", "-H"]),
        ] {
            assert_eq!(
                running_window_successor_args(verb.clone().into_iter(), None),
                args(&["--window"]),
                "{verb:?}"
            );
        }
        // #7's review: the `-e` payload rides along verbatim — every later tab of
        // the window runs it — and nothing inside it is read as the window's flag
        // (the successor is marked instead, `RELAUNCH_OF_RUNNING_ENV`, so its
        // first session does not run it again).
        let payload = args(&[
            "--window",
            "--hold",
            "-d",
            "/tmp",
            "-e",
            "./deploy.sh",
            "--lifeline-fd",
            "0",
            "--now",
        ]);
        assert_eq!(
            running_window_successor_args(payload.clone().into_iter(), Some(7)),
            payload,
            "the payload's own `--lifeline-fd 0` is its argument, not the window's"
        );
        assert_eq!(
            running_window_successor_args(args(&["--command", "vim"]).into_iter(), None),
            args(&["--window", "--command", "vim"])
        );
        assert_eq!(
            running_window_successor_args(
                args(&["--lifeline-fd", "0", "--", "sh", "-c", "true"]).into_iter(),
                Some(9)
            ),
            args(&["--window", "--lifeline-fd", "9", "--", "sh", "-c", "true"])
        );
        // #28: the stale lifeline number goes; the caller's descriptor replaces it.
        let lifelined = args(&[
            "--headless",
            "--lifeline-fd",
            "0",
            "--control-sock",
            "/tmp/a.sock",
        ]);
        assert_eq!(
            running_window_successor_args(lifelined.clone().into_iter(), None),
            args(&["--window", "--headless", "--control-sock", "/tmp/a.sock"])
        );
        let handed = running_window_successor_args(lifelined.into_iter(), Some(7));
        assert_eq!(
            handed,
            args(&[
                "--window",
                "--headless",
                "--lifeline-fd",
                "7",
                "--control-sock",
                "/tmp/a.sock"
            ])
        );
        assert_eq!(
            running_window_successor_args(handed.clone().into_iter(), Some(7)),
            handed,
            "a fixed point"
        );
        // The ready signal was the first image's to give: it goes, with its number.
        let readied = args(&["--headless", "--ready-fd", "5", "--columns", "80"]);
        let told = running_window_successor_args(readied.into_iter(), None);
        assert_eq!(told, args(&["--window", "--headless", "--columns", "80"]));
        assert_eq!(
            running_window_successor_args(told.clone().into_iter(), None),
            told,
            "a fixed point"
        );
        // Everything a window runs with rides along.
        let flags = args(&["--headless", "--columns", "80", "--cpu", "--font", "Menlo"]);
        assert_eq!(
            running_window_successor_args(flags.clone().into_iter(), None),
            [args(&["--window"]), flags].concat()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_lifeline_flag_is_the_one_the_instance_parses() {
        assert_eq!(super::LIFELINE_FLAG, aterm_uds::lifeline::FLAG);
    }
}
