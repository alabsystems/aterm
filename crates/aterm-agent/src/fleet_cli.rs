// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet`: this machine's membership in a fleet, the machines
//! `aterm ctl survey --fleet` reads (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
//! §5). A removed mode word exits 2 naming the `aterm ctl` verb that does its
//! work.

use std::process::ExitCode;

/// `aterm fleet …` and the `aterm-fleet` argv0 alias: `argv[1..]` in, exit
/// code out.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let mode = argv.first().map(|a| a.to_string_lossy().into_owned());
    match route(mode.as_deref()) {
        Route::Help => usage(),
        Route::Gone(instead) => {
            let mode = mode.unwrap_or_default();
            eprintln!("aterm fleet: `{mode}` is no longer an `aterm fleet` mode; {instead}");
            ExitCode::from(2)
        }
        #[cfg(unix)]
        Route::Membership => crate::fleet_member::main_entry(&mode.unwrap_or_default(), &argv[1..]),
        #[cfg(not(unix))]
        Route::Membership => {
            eprintln!("aterm fleet: this platform keeps no fleet membership");
            ExitCode::from(2)
        }
        Route::Unknown => {
            eprintln!("aterm fleet: no such mode; run `aterm fleet --help`");
            ExitCode::from(2)
        }
    }
}

/// Where `aterm fleet <mode>` goes, decided without running it so a test can
/// read it.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// No mode, `-h` or `--help`: the usage, exit 0.
    Help,
    /// A removed mode, with what does its work ([`gone_to`]).
    Gone(&'static str),
    /// A membership mode the usage names (`init`, `join`, `peers`, `allow`,
    /// `leave`).
    Membership,
    /// Any other word.
    Unknown,
}

fn route(mode: Option<&str>) -> Route {
    match mode {
        Some("-h" | "--help") | None => Route::Help,
        Some("init" | "join" | "peers" | "allow" | "leave") => Route::Membership,
        Some(mode) => gone_to(mode).map_or(Route::Unknown, Route::Gone),
    }
}

/// What does a removed mode's work, named in its refusal.
fn gone_to(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "events" => "use `aterm ctl survey` or `aterm ctl subscribe @a,@b events`",
        "exec" => "use `aterm ctl @<sid> <verb>`",
        "status" | "inspect" | "manage" | "unmanage" | "next" | "extend" | "ack" | "reconcile"
        | "clear-fault" | "propose" => {
            "use `aterm ctl survey` and `aterm ctl @self spawn agent=claude -`"
        }
        _ => return None,
    })
}

fn usage() -> ExitCode {
    #[cfg(unix)]
    let help = crate::fleet_member::USAGE;
    #[cfg(not(unix))]
    let help = "aterm fleet — fleet membership (unsupported on this platform; \
         see `aterm help fleet`).\n";
    aterm_uds::stdout::answer_exit("aterm fleet", help, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No mode, `-h` and `--help` are help; the five membership words are
    /// membership; each removed mode is refused; any other word is unknown.
    #[test]
    fn every_mode_word_routes_to_its_one_answer() {
        for help in [None, Some("--help"), Some("-h")] {
            assert_eq!(route(help), Route::Help, "{help:?}");
        }
        for word in ["init", "join", "peers", "allow", "leave"] {
            assert_eq!(route(Some(word)), Route::Membership, "{word}");
        }
        for mode in [
            "events",
            "exec",
            "status",
            "inspect",
            "manage",
            "unmanage",
            "next",
            "extend",
            "ack",
            "reconcile",
            "clear-fault",
            "propose",
        ] {
            assert!(matches!(route(Some(mode)), Route::Gone(_)), "{mode}");
        }
        for word in ["bogus", "", "INIT", "--init"] {
            assert_eq!(route(Some(word)), Route::Unknown, "{word:?}");
        }
    }

    /// Help exits 0; a removed mode or an unknown word exits 2. A membership
    /// mode changes the machine, so only its route is tested, never a run.
    #[test]
    fn help_succeeds_and_a_refused_word_is_a_usage_error() {
        let exit = |args: &[&str]| main_entry(args.iter().map(std::ffi::OsString::from).collect());
        for args in [&[][..], &["--help"], &["-h"]] {
            assert_eq!(exit(args), ExitCode::SUCCESS, "{args:?}");
        }
        for word in ["events", "exec", "status", "propose", "bogus"] {
            assert_eq!(exit(&[word]), ExitCode::from(2), "{word}");
        }
    }
}
