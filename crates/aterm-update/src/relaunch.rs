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

#[cfg(test)]
mod tests {
    use super::reexec_forwarded_args;

    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn stripping_the_leading_pins_is_a_fixed_point() {
        let once = reexec_forwarded_args(args(&["--window", "--window", "-e", "bash"]).into_iter());
        assert_eq!(once, args(&["-e", "bash"]));
        let twice = reexec_forwarded_args(once.clone().into_iter());
        assert_eq!(twice, once, "applying it twice equals applying it once");
    }

    #[test]
    fn a_later_window_flag_is_the_users_and_survives() {
        assert_eq!(
            reexec_forwarded_args(args(&["--window", "-e", "bash", "--window"]).into_iter()),
            args(&["-e", "bash", "--window"]),
            "only the LEADING run is boot-swap residue"
        );
    }
}
