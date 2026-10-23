// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Argv construction for relaunching the binary across platforms.

/// Forward this process's argv to a successor minus leading `--window` pins.
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

/// Whether a forwarded argv (pins stripped) is a windowing verb's
/// (`aterm new-window|new-tab|split-pane …`).
fn starts_with_a_verb(forwarded: &[std::ffi::OsString]) -> bool {
    forwarded
        .first()
        .is_some_and(|first| !first.to_string_lossy().starts_with('-'))
}

/// The argv of the boot swap's re-exec: one leading `--window` pin unless launched with a verb.
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

/// The flag that names a harness's lifeline descriptor: the instance's own spelling
/// where the lifeline exists (unix), and that same word elsewhere, where this module
/// still compiles but no instance takes one.
#[cfg(unix)]
const LIFELINE_FLAG: &str = aterm_uds::lifeline::FLAG;
#[cfg(not(unix))]
const LIFELINE_FLAG: &str = "--lifeline-fd";

/// The flag that names a harness's ready signal: the write end of a pipe a headless
/// instance writes one byte to once its control socket listens (aterm-gui's `--ready-fd`).
const READY_FLAG: &str = "--ready-fd";

/// Environment variable marking an update's relaunch of an already-running window.
pub const RELAUNCH_OF_RUNNING_ENV: &str = "ATERM_RELAUNCH_OF_RUNNING";

/// The argv of a running window's update successor.
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

    /// The boot swap's re-exec of a verb's launch is the verb,
    /// unpinned — the router routes it to the window it asked for, where a pin sends
    /// the bare word to the window parser's exit 2.
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

    /// A running window's successor carries how the
    /// window runs — its `-e` command included — and neither a
    /// windowing verb nor a stale lifeline number.
    #[test]
    fn a_running_windows_successor_carries_its_flags_and_not_its_launch() {
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
        let readied = args(&["--headless", "--ready-fd", "5", "--columns", "80"]);
        let told = running_window_successor_args(readied.into_iter(), None);
        assert_eq!(told, args(&["--window", "--headless", "--columns", "80"]));
        assert_eq!(
            running_window_successor_args(told.clone().into_iter(), None),
            told,
            "a fixed point"
        );
        let flags = args(&["--headless", "--columns", "80", "--cpu", "--font", "Menlo"]);
        assert_eq!(
            running_window_successor_args(flags.clone().into_iter(), None),
            [args(&["--window"]), flags].concat()
        );
    }
}
