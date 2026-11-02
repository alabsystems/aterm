// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A BUILD'S MEMORY CLAIM, BY ITS VERBS (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! BUILD-ADMIT). A `targo` shim's hop runs atpkg's front, `atpkg __build_admit <target> [args…]`:
//! it claims the build's memory in the capacity ledger and then execs the build in its own pid,
//! leaving behind a holder, `<aterm> __build_hold …`, that keeps the claim's lock for the build's
//! life. Both are the one aterm image; atpkg serves them. The holder is launchd's by design, for as
//! long as the build it watches (or what that build left running) lives.

use std::ffi::OsStr;
use std::path::Path;

/// The front's verb: `atpkg __build_admit <target> [args…]`.
pub const ADMIT_VERB: &str = "__build_admit";

/// The image a front runs as, by its file name: a hop execs atpkg by its path (`exec
/// '<prefix>/atpkg' __build_admit …`), and only atpkg serves the verb as its first word.
pub const FRONT_IMAGE: &str = "atpkg";

/// The holder's verb, served as an executable's first call in `main`.
pub const HOLD_VERB: &str = "__build_hold";

/// A BUILD'S FRONT, as its argument words alone name it ([`front`]): the program it execs in its
/// own pid once its claim is admitted, and the build's words. A reader names the front by its
/// target, the build it becomes (aterm-gui's `session_status::program::front_program` is that one
/// naming rule for `status` and `sys jobs`; the census calls it `atpkg build-admit`).
#[derive(Debug, PartialEq, Eq)]
pub struct Front<'a, S> {
    /// The program it becomes: the `<target>` its words name (the store's `targo`, or the root
    /// the shim's route names), exec'd on [`Self::args`].
    pub target: &'a S,
    /// The build's words, handed on unchanged.
    pub args: &'a [S],
}

/// THE FRONT A PROCESS'S ARGUMENT WORDS NAME: `argv[0]`'s file name is [`FRONT_IMAGE`],
/// `argv[1]` is [`ADMIT_VERB`] and the words after it name a target, `[--shim <tool> <placed>]
/// <target> [args…]`, as atpkg's front reads them (`atpkg::build_admit::run_hidden`). The one
/// question every reader that names a process by its program asks of it (a tab's `status
/// program=`, the survey's line, `sys jobs`' rows, the census), so each names a front by the build
/// it becomes and none by its image (`atpkg`). A process is a front from the shim's hop until it
/// execs its target: while its claim waits in the ledger, or, where no holder armed, while it runs
/// the build as its child; its children or its record tell those apart. `None` for every other
/// call: another verb, `__build_admit --capabilities` (which answers and exits), a `--shim` short
/// of its three words, no target, or another program whose first argument is the verb's word (`rg
/// __build_admit crates`).
#[must_use]
pub fn front<S: AsRef<OsStr>>(argv: &[S]) -> Option<Front<'_, S>> {
    let [image, verb, rest @ ..] = argv else {
        return None;
    };
    if verb.as_ref() != ADMIT_VERB
        || Path::new(image.as_ref()).file_name() != Some(OsStr::new(FRONT_IMAGE))
    {
        return None;
    }
    if let [only] = rest
        && only.as_ref() == "--capabilities"
    {
        return None;
    }
    let rest = match rest {
        [first, after @ ..] if first.as_ref() == "--shim" => after.get(2..)?,
        _ => rest,
    };
    let (target, args) = rest.split_first()?;
    Some(Front { target, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|w| (*w).to_owned()).collect()
    }

    /// The target and build words a front's argv names, read through [`front`].
    fn named(argv: &[&str]) -> Option<(String, Vec<String>)> {
        let argv = words(argv);
        let f = front(&argv)?;
        Some((f.target.clone(), f.args.to_vec()))
    }

    /// A shim's hop names the build it becomes by its target, in each placement and for each tool,
    /// and a bare front (the in-process callers' form) by its first word. The argv is the one a
    /// laid hop execs (`[ -x '<helper>' ] && exec '<helper>' __build_admit --shim <tool>
    /// "$__atb_h" "$__atb_p" "$@"`), as `atpkg::build_cache_registered`'s rendered hop hands it to
    /// a stand-in helper.
    #[test]
    fn a_front_is_named_by_the_target_its_words_name() {
        let build = words(&["--unverified", "build", "-p", "aterm-capacity"]);
        for placed in ["-", "config", "env"] {
            let argv = [
                "/Applications/aterm.app/Contents/MacOS/atpkg",
                ADMIT_VERB,
                "--shim",
                "targo",
                placed,
                "/Users//u/.local/share/aterm/store/trust/bin/targo",
                "--unverified",
                "build",
                "-p",
                "aterm-capacity",
            ];
            assert_eq!(
                named(&argv),
                Some((
                    "/Users//u/.local/share/aterm/store/trust/bin/targo".to_owned(),
                    build.clone()
                ))
            );
        }
        assert_eq!(
            named(&[
                "atpkg",
                ADMIT_VERB,
                "--shim",
                "targo-tippy",
                "-",
                "/store/bin/targo-tippy",
                "-p",
                "a",
            ]),
            Some(("/store/bin/targo-tippy".to_owned(), words(&["-p", "a"])))
        );
        assert_eq!(
            named(&["atpkg", ADMIT_VERB, "/usr/bin/true", "--unverified", "run"]),
            Some(("/usr/bin/true".to_owned(), words(&["--unverified", "run"])))
        );
        assert_eq!(
            named(&["atpkg", ADMIT_VERB, "--shim", "targo", "-", "/store/targo"]),
            Some(("/store/targo".to_owned(), Vec::new())),
            "a call of no words is still a front"
        );
    }

    /// NEGATIVE CONTROL: every other call of the image, or of anything else, is no front, so its
    /// readers name it as before: another verb (the registration call the hop replaced), the
    /// capability question, a `--shim` short of its words, a verb past its place, no verb at all,
    /// a build typed plainly, and another program searching for the verb's word (whose first
    /// argument it is), by name or by path.
    #[test]
    fn no_other_call_is_a_front() {
        for argv in [
            &["rg", ADMIT_VERB, "crates/atpkg"][..],
            &["/usr/bin/grep", ADMIT_VERB, "x.rs"],
            &[
                "/usr/bin/grep",
                ADMIT_VERB,
                "--shim",
                "targo",
                "-",
                "/store/targo",
            ],
            &["/opt/atpkg/bin/rg", ADMIT_VERB, "/store/targo"],
            &["atpkg-old", ADMIT_VERB, "/store/targo"],
            &[
                "atpkg",
                "__build_cache_register",
                "targo",
                "--unverified",
                "build",
            ][..],
            &["atpkg", ADMIT_VERB, "--capabilities"],
            &["atpkg", ADMIT_VERB],
            &["atpkg", ADMIT_VERB, "--shim"],
            &["atpkg", ADMIT_VERB, "--shim", "targo"],
            &["atpkg", ADMIT_VERB, "--shim", "targo", "-"],
            &["atpkg", "install", ADMIT_VERB, "/store/targo"],
            &["atpkg"],
            &[],
            &["/store/targo", "--unverified", "build"],
            &["atpkg", "__build_hold", "1", "2"],
        ] {
            assert_eq!(front(&words(argv)), None, "{argv:?}");
        }
    }

    /// The words are read as the kernel hands them: a target whose name is not UTF-8 is still
    /// named, by its own bytes.
    #[cfg(unix)]
    #[test]
    fn a_target_named_in_bytes_that_are_not_utf8_is_still_a_front() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        let odd = OsString::from_vec(b"/store/targ\xffo".to_vec());
        let argv = [
            OsString::from("atpkg"),
            OsString::from(ADMIT_VERB),
            odd.clone(),
            OsString::from("check"),
        ];
        let f = front(&argv).expect("a front");
        assert_eq!(f.target, &odd);
        assert_eq!(f.args, &[OsString::from("check")]);
    }
}
