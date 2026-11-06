// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Leave the subscriber's launchd band at process birth, retaining its thread and child policy.

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// The replacement image must understand this entry contract before it starts any work.
/// An older image refuses this unknown secrets verb before opening state, and launchd restarts
/// its unchanged plain arguments in the original Background unit.
pub(super) const WATCH_REEXEC: &str = "__watch_runtime_v1";

/// Enter only an exact front-door watch. The replacement installs its hold before the
/// caller can start a worker, keyboard keeper, vault callback or state operation.
/// If the running image cannot replace itself, retain the functional inherited runtime.
pub(super) fn enter(argv: &mut [OsString]) {
    enter_with(
        argv,
        install,
        |next| match std::env::current_exe() {
            Ok(exe) => aterm_uds::spawnfd::command(exe)
                .arg0(&next[0])
                .args(&next[1..])
                .leave_background_band(true)
                .exec(),
            Err(error) => error,
        },
        |why| {
            eprintln!(
                "aterm secrets watch: cannot leave the Background band; keeping inherited scheduling: {why}"
            );
        },
    );
}

fn enter_with(
    argv: &mut [OsString],
    hold: impl FnOnce(),
    replace: impl FnOnce(&[OsString]) -> io::Error,
    refused: impl FnOnce(&io::Error),
) {
    let Some(argv0) = argv.first() else {
        return;
    };
    let name = Path::new(argv0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if aterm_cli::alias_route(name, "secrets") != aterm_cli::AliasRoute::FrontDoor {
        return;
    }
    let args = argv.get(1..).unwrap_or_default();
    if args.len() == 2 && args[0] == "secrets" && args[1] == "watch" {
        let mut next = argv.to_vec();
        next[2] = WATCH_REEXEC.into();
        // A successful exec never returns. Only a refusal in this still-running
        // image takes this path; an old replacement instead rejects its unknown verb.
        let why = replace(&next);
        hold();
        refused(&why);
        return;
    }
    if args.len() == 2 && args[0] == "secrets" && args[1] == WATCH_REEXEC {
        argv[2] = "watch".into();
        hold();
    }
}

pub(super) fn install() {
    aterm_gui::hold_process_as_housekeeping();
    aterm_uds::spawnfd::start_children_in_background_band();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::process::ExitCode;

    fn words(name: &str, args: &[&str]) -> Vec<OsString> {
        std::iter::once(name)
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn the_replacement_restores_plain_watch_and_holds_before_dispatch() {
        let mut argv = words("/fixture/renamed-aterm", &["secrets", WATCH_REEXEC]);
        let held = RefCell::new(false);
        enter_with(
            &mut argv,
            || *held.borrow_mut() = true,
            |_| panic!("the replacement must not exec again"),
            |_| panic!("the replacement did not attempt an exec"),
        );
        assert!(*held.borrow());
        assert_eq!(argv, words("/fixture/renamed-aterm", &["secrets", "watch"]));
    }

    #[test]
    fn a_refused_exec_holds_before_reporting_then_keeps_the_original_watch() {
        let original = words("/fixture/aterm", &["secrets", "watch"]);
        let mut argv = original.clone();
        let events = RefCell::new(Vec::new());
        enter_with(
            &mut argv,
            || events.borrow_mut().push("hold"),
            |replacement| {
                assert_eq!(
                    replacement,
                    words("/fixture/aterm", &["secrets", WATCH_REEXEC])
                );
                events.borrow_mut().push("exec");
                io::Error::from(io::ErrorKind::PermissionDenied)
            },
            |why| {
                assert_eq!(why.kind(), io::ErrorKind::PermissionDenied);
                events.borrow_mut().push("journal");
            },
        );
        events.borrow_mut().push("dispatch");
        assert_eq!(events.into_inner(), ["exec", "hold", "journal", "dispatch"]);
        assert_eq!(argv, original);
    }

    #[test]
    fn an_old_dispatcher_refuses_the_unknown_entry_verb_before_its_runtime() {
        assert_eq!(
            aterm_secrets::cli::main_entry(vec![WATCH_REEXEC.into()]),
            ExitCode::from(2)
        );
    }

    #[test]
    fn help_invalid_watch_and_other_alias_routes_do_not_change_process_policy() {
        for name in ["/fixture/aterm", "/fixture/atpkg", "/fixture/aterm-fleet"] {
            for args in [
                &["secrets", "watch", "--help"][..],
                &["secrets", "watch", "extra"],
                &["secrets", "sync"],
                &["secrets", "serve"],
                &["secrets", WATCH_REEXEC, "extra"],
                &[],
            ] {
                let mut argv = words(name, args);
                let original = argv.clone();
                enter_with(
                    &mut argv,
                    || panic!("this route must not install a hold"),
                    |_| panic!("this route must not replace itself"),
                    |_| panic!("this route did not attempt an exec"),
                );
                assert_eq!(argv, original);
            }
        }
        for name in [
            "/fixture/atpkg",
            "/fixture/aterm-fleet",
            "/fixture/aterm-gui",
        ] {
            for args in [&["secrets", "watch"][..], &["secrets", WATCH_REEXEC]] {
                let mut argv = words(name, args);
                let original = argv.clone();
                enter_with(
                    &mut argv,
                    || panic!("an alias must keep its own routing"),
                    |_| panic!("an alias must keep its own routing"),
                    |_| panic!("an alias did not attempt an exec"),
                );
                assert_eq!(argv, original);
            }
        }
    }
}
