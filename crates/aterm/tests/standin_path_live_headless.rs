// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A STAND-IN ON THE INSTANCE'S PATH IS WHAT ITS LOGIN SHELL NAMES, or the
//! boot ends COULD NOT RUN before the test can type anything
//! (`support/headless_boot.rs`'s guard over `support/launch_isolation.rs`'s
//! `pin_login_path`).
//!
//! THE BUG (measured 2026-10-18): the worker tests booted a headless instance
//! with `PATH=<stand-in folder>:/usr/bin:/bin:/usr/sbin:/sbin`, but a tab runs a
//! login shell, macOS's system profile (`/etc/zprofile`, `/etc/profile`) runs
//! `path_helper`, and that put `/etc/paths.d`'s `/opt/homebrew/bin` in front: the
//! worker's `claude` was Homebrew's Claude Code, started under the scratch HOME.
//!
//! * THE PIN: under zsh and under bash, the boot session's login shell names
//!   `claude` by the stand-in's path — the system profile ran, and the scratch
//!   home's profile said the instance's PATH again after it. That `claude` is
//!   put by hand, not by `headless_boot::standin`, so the guard asks nothing
//!   of it and a broken pin reaches this file's assert: a FAIL, where the
//!   guard's refusal would be COULD NOT RUN, which `verify` files as the
//!   host's (and every guarded stand-in test with it). The guard's own pass is
//!   the records' control below, whose stand-in it asks about.
//! * THE GUARD, NEGATIVE CONTROLS, each a boot that must end COULD NOT RUN:
//!   - a decoy folder ahead of the stand-in's on the instance's own PATH,
//!     holding a program of the stand-in's name — `aterm-test-standin-x`, a
//!     name no machine has, so nothing real can answer it however the guard
//!     fares;
//!   - (macOS) the pin undone by the test's own `~/.zprofile`, which runs after
//!     it and runs `path_helper` again: `/usr/bin` (every Mac's `/etc/paths`) is
//!     then in front of a stand-in named `uptime`. The same boot stands in
//!     `claude`, and the refusal says what the shell named it — on a Mac with
//!     Homebrew's Claude Code, Homebrew's path: the bug, caught by the guard.
//!
//! NOTHING RUNS A STAND-IN HERE but [`NOBODYS`], in the records' own control:
//! the guard and the other tests type `command -v` alone, and no harness or
//! worker is on to type anything (the isolation config's harness-off table).
//! Every stand-in and the decoy record into [`MARKS`]' folder — outside the
//! scratch world, which a refused boot removes — when they run: the control
//! shows a run leaves its record, and every other test asserts the folder
//! empty.
//!
//! Every wait is a one-minute hang detector. A machine that refuses the
//! headless instance is COULD NOT RUN ([`headless_boot::boot_with`]).

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::headless_boot;

use headless_boot::Instance;

/// The variable naming the folder a stand-in or the decoy records into when
/// it runs: set on the instance, so every shell of it inherits it.
const MARKS: &str = "LIVE_STANDIN_MARKS";

/// A stand-in that records its run: `--version` answers at once (the helper
/// runs every fake once that way, `headless_boot::standin`); anything else
/// leaves `<marks>/ran-<its name>` and exits 0.
const RECORDER: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '0.0.0 (stand-in)'; exit 0 ;; esac\n\
    : > \"${LIVE_STANDIN_MARKS:?}/ran-${0##*/}\"\n";

/// The decoy: whatever it is asked, it leaves `<marks>/decoy-<its name>`.
const DECOY: &str = "#!/bin/sh\n: > \"${LIVE_STANDIN_MARKS:?}/decoy-${0##*/}\"\n";

/// The one name no machine has: a negative control's stand-in.
const NOBODYS: &str = "aterm-test-standin-x";

/// The system's folders, after the test's own: the worker tests' PATH tail.
const SYSTEM: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// A folder outside every scratch world for the runs' records, removed when it
/// drops.
fn marks() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("atsp-marks-")
        .tempdir()
        .expect("the records' folder")
}

/// What ran: every record in `marks`, by name.
fn ran(marks: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(marks)
        .expect("read the records' folder")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// One headless instance under `shell`, its PATH `<front…>:<tmp>/bin:` then
/// [`SYSTEM`], a [`RECORDER`] stand-in in `<tmp>/bin` for each of `names`, a
/// [`DECOY`] of each of `decoys` in `<tmp>/decoy` (put there by hand, so no
/// stand-in: the guard asks about no decoy), recording into `marks`, and
/// `zprofile` the test's own `~/.zprofile` when given.
#[track_caller]
fn boot(
    tag: &str,
    shell: &str,
    names: &[&str],
    decoys: &[&str],
    zprofile: Option<&str>,
    marks: &Path,
) -> Instance {
    headless_boot::boot_with(
        &format!("atsp{tag}"),
        |tmp, cmd| {
            let bin = tmp.join("bin");
            for name in names {
                headless_boot::standin(&bin.join(name), RECORDER);
            }
            let decoy = tmp.join("decoy");
            for name in decoys {
                put_script(&decoy.join(name), DECOY);
            }
            if let Some(text) = zprofile {
                std::fs::write(tmp.join("home/.zprofile"), text).expect("the test's .zprofile");
            }
            let front = if decoys.is_empty() {
                String::new()
            } else {
                format!("{}:", decoy.display())
            };
            cmd.envs(environ(shell, &format!("{front}{}", bin.display()), marks));
        },
        |_| true,
    )
}

/// The instance's world, for its launch's `envs`: `shell` its shell, its PATH
/// `<head>:` then [`SYSTEM`], and [`MARKS`] naming `marks`.
fn environ(shell: &str, head: &str, marks: &Path) -> [(&'static str, OsString); 3] {
    [
        ("SHELL", shell.into()),
        ("PATH", format!("{head}:{SYSTEM}").into()),
        (MARKS, marks.into()),
    ]
}

/// Write `text` at `path`, executable, by hand: no stand-in the guard asks
/// about.
fn put_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("the script's folder");
    std::fs::write(path, text).expect("the script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// The guard's refusal of the boot `boot` makes: its message. A boot the guard
/// let through fails the test; any other panic (a machine that refuses the
/// instance, a product that cannot start) is the test's own outcome, raised
/// again.
fn refusal(boot: impl FnOnce() -> Instance) -> String {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(boot));
    let payload = match caught {
        Ok(inst) => panic!(
            "the guard let the boot through: its login shell names every stand-in by its own \
             path (scratch world {})",
            inst.tmp.display()
        ),
        Err(payload) => payload,
    };
    let said = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    if !said.contains(headless_boot::STANDIN_UNRESOLVED) {
        std::panic::resume_unwind(payload);
    }
    said
}

/// The pin: the boot session's login shell under `shell` names `claude` by its
/// stand-in's path, the system profile's `path_helper` notwithstanding, and
/// nothing ran it.
///
/// The stand-in is put by hand ([`put_script`]), so the guard asks nothing of
/// it: a pin that breaks fails the assert here, naming what the shell found,
/// instead of ending the boot COULD NOT RUN. Nothing types more than
/// `command -v`, so even then nothing runs what it found.
fn pinned(tag: &str, shell: &str) {
    let marks = marks();
    let inst = headless_boot::boot_with(
        &format!("atsp{tag}"),
        |tmp, cmd| {
            let bin = tmp.join("bin");
            put_script(&bin.join("claude"), RECORDER);
            cmd.envs(environ(shell, &bin.display().to_string(), marks.path()));
        },
        |_| true,
    );
    let standin: PathBuf = inst.tmp.join("bin/claude");
    let said = headless_boot::login_resolution(&inst, &["claude"]);
    eprintln!("{shell}: the instance's login shell names claude {said:?}");
    assert_eq!(
        said,
        [standin.to_string_lossy().into_owned()],
        "{shell}: the login shell does not name `claude` by the stand-in on the instance's \
         PATH; the scratch home's profiles:\n{}",
        profiles(&inst.tmp.join("home"))
    );
    assert_eq!(ran(marks.path()), Vec::<String>::new(), "nothing ran");
}

/// The login profiles in `home`, each by name with its text: what a failed pin
/// left.
fn profiles(home: &Path) -> String {
    [".zprofile", ".bash_profile", ".bash_login", ".profile"]
        .iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(home.join(name)).ok()?;
            Some(format!("--- {name}\n{text}"))
        })
        .collect()
}

#[test]
fn the_login_shell_names_the_stand_in_on_the_instances_path_under_zsh() {
    pinned("z", "/bin/zsh");
}

#[test]
fn the_login_shell_names_the_stand_in_on_the_instances_path_under_bash() {
    pinned("b", "/bin/bash");
}

/// THE RECORDS ARE LIVE (the control behind every "nothing ran" here): a
/// stand-in the guard passed, typed by its name — [`NOBODYS`], which nothing
/// real can answer — runs and leaves its record, and the decoy typed by its
/// path leaves its own, both from the instance's shell into [`MARKS`]' folder.
#[test]
fn a_stand_in_typed_by_name_runs_and_its_record_says_so() {
    let marks = marks();
    let inst = boot("r", "/bin/zsh", &[NOBODYS], &[], None, marks.path());
    let decoy = inst.tmp.join("offpath").join(NOBODYS);
    put_script(&decoy, DECOY);
    let (_, sid) = headless_boot::boot_session(&inst);
    headless_boot::type_line(&inst, &sid, NOBODYS);
    headless_boot::type_line(&inst, &sid, &format!("'{}'", decoy.display()));
    let want = [format!("decoy-{NOBODYS}"), format!("ran-{NOBODYS}")];
    let both = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        headless_boot::POLL_GAP,
        || (ran(marks.path()) == want).then_some(()),
    );
    assert!(
        both.is_some(),
        "records {:?}, want {want:?}; the screen:\n{}",
        ran(marks.path()),
        String::from_utf8_lossy(
            &headless_boot::ctl(&inst, &[&format!("@{sid}"), "text", "trim"]).stdout
        )
    );
}

/// NEGATIVE CONTROL: a decoy of the stand-in's name in a folder ahead of the
/// stand-in's on the instance's own PATH. The guard ends the boot naming the
/// decoy, and neither ran.
#[test]
fn a_decoy_ahead_of_the_stand_in_ends_the_boot_could_not_run() {
    let marks = marks();
    let said = refusal(|| boot("d", "/bin/bash", &[NOBODYS], &[NOBODYS], None, marks.path()));
    eprintln!("the guard's refusal: {said}");
    assert!(
        said.contains(aterm_spec::host::COULD_NOT_RUN),
        "COULD NOT RUN: {said}"
    );
    assert!(
        said.contains(&format!("/decoy/{NOBODYS}\"")),
        "the refusal names the decoy: {said}"
    );
    assert_eq!(ran(marks.path()), Vec::<String>::new(), "nothing ran");
}

/// NEGATIVE CONTROL (macOS): the test's own `~/.zprofile`, which runs after
/// the pin, runs `path_helper` again, so the system's folders are in front of
/// the stand-ins once more. The guard ends the boot naming `/usr/bin/uptime`
/// for the stand-in `uptime`; it says what `claude` is too (Homebrew's, where a
/// Mac has it), and nothing ran.
#[cfg(target_os = "macos")]
#[test]
fn a_login_profile_that_puts_the_system_first_again_ends_the_boot_could_not_run() {
    let marks = marks();
    let said = refusal(|| {
        boot(
            "p",
            "/bin/zsh",
            &["uptime", "claude"],
            &[],
            Some("eval \"$(/usr/libexec/path_helper -s)\"\n"),
            marks.path(),
        )
    });
    eprintln!("the guard's refusal: {said}");
    assert!(
        said.contains(aterm_spec::host::COULD_NOT_RUN),
        "COULD NOT RUN: {said}"
    );
    assert!(
        said.contains("`uptime` is \"/usr/bin/uptime\""),
        "the refusal names the system's uptime: {said}"
    );
    assert_eq!(ran(marks.path()), Vec::<String>::new(), "nothing ran");
}
