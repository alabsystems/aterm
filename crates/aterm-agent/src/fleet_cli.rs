// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet`: this machine's membership in a fleet, the machines
//! `aterm ctl survey --fleet` reads (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
//! §5).
//!
//! Its earlier modes are gone, and each answers with what replaced it: the
//! local NDJSON bridge (`events`, `exec`), which `aterm ctl survey` and
//! `subscribe` cover, and the embedded operator's attention-queue clients
//! (`status`, `manage`, `next`, …), which left with the operator (design §7).

use std::process::ExitCode;

/// The whole fleet CLI as a callable: `argv[1..]` in, process exit code out,
/// served by `aterm fleet …` and the argv0 alias.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let mode = argv.first().map(|a| a.to_string_lossy().into_owned());
    match route(mode.as_deref()) {
        Route::Help => usage(),
        Route::Gone(instead) => {
            let mode = mode.unwrap_or_default();
            eprintln!("aterm fleet: `{mode}` is no longer an `aterm fleet` mode; {instead}");
            ExitCode::from(2)
        }
        // This machine's membership in a fleet (`crate::fleet_member`).
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

/// What `aterm fleet <mode>` is, decided apart from doing it: the dispatcher's
/// table, which a test reads without running any mode.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// No mode, `-h` or `--help`: the usage, exit 0.
    Help,
    /// A removed mode, with what took its work ([`gone_to`]).
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

/// What took a removed mode's work, for its refusal.
fn gone_to(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "events" => {
            "`aterm ctl survey` reads what every session of every instance is doing, and \
             `aterm ctl subscribe @a,@b events` streams one instance's"
        }
        "exec" => "`aterm ctl @<sid> <verb>` reaches a session of any instance on this machine",
        "status" | "inspect" | "manage" | "unmanage" | "next" | "extend" | "ack" | "reconcile"
        | "clear-fault" | "propose" => {
            "the attention queue it served is gone: a manager session takes its place, reading \
             every session with `aterm ctl survey` and starting workers with `aterm ctl @self \
             spawn agent=claude -`"
        }
        _ => return None,
    })
}

fn usage() -> ExitCode {
    #[cfg(unix)]
    let help = crate::fleet_member::USAGE;
    #[cfg(not(unix))]
    let help = "aterm fleet — this machine's membership in a fleet: the machines \
         `aterm ctl survey --fleet` reads. This platform keeps no fleet membership.\n\n\
         See `aterm help fleet`.\n";
    aterm_uds::stdout::answer_exit("aterm fleet", help, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Help, bare or asked for, is the usage; each membership mode is one; a
    /// removed mode names what replaced it; anything else is unknown.
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

    /// Help exits 0; a removed mode and an unknown word are usage errors (2).
    /// Only words whose answer is a line of text run here: a membership mode
    /// changes the machine, so its route is read above and never run.
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
