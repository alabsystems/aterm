// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet`: the lab. Bare (or `--json`) it is the lab's status line and one row per seat
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.12, [`crate::fleet_view`]); `remove`, `admit`,
//! `invite`, `requests` and `ignore` are the lab verbs `aterm members` runs, by the same code
//! (§4.16, [`crate::members_cli::parse_lab`]); the rest are this machine's membership in a fleet,
//! the machines `aterm ctl survey --fleet` reads
//! (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5). A removed mode word exits 2 naming the
//! `aterm ctl` verb that does its work. `activity` reads this Mac's fleet activity record
//! ([`crate::fleet_activity`]). A new verb is one word in [`route`] and its arm here.

use std::process::ExitCode;

/// `aterm fleet …` and the `aterm-fleet` argv0 alias: `argv[1..]` in, exit
/// code out.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let mode = argv.first().map(|a| a.to_string_lossy().into_owned());
    match route(mode.as_deref()) {
        Route::Help => usage(),
        #[cfg(unix)]
        Route::View => crate::fleet_view::main_entry(&argv),
        #[cfg(unix)]
        Route::Lab => {
            let args: Vec<String> = argv
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            crate::members_cli::lab_entry(crate::members_cli::Front::Fleet, &args)
        }
        #[cfg(unix)]
        Route::Activity => crate::fleet_activity::main_entry(&argv[1..]),
        #[cfg(not(unix))]
        Route::View | Route::Lab | Route::Activity => {
            eprintln!("aterm fleet: this platform keeps no fleet membership");
            ExitCode::from(2)
        }
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
    /// `-h` or `--help`: the usage, exit 0.
    Help,
    /// No mode, or `--json`: the lab's status line and seats ([`crate::fleet_view`]).
    View,
    /// A lab verb `aterm members` runs too (`remove`, `admit`, `invite`, `requests`, `ignore`).
    Lab,
    /// `activity`: the agent requests that crossed between this Mac and its fleet's others
    /// ([`crate::fleet_activity`]), a local read that changes nothing.
    Activity,
    /// A removed mode, with what does its work ([`gone_to`]).
    Gone(&'static str),
    /// A membership mode the usage names (`init`, `join`, `peers`, `allow`, `access`,
    /// `publish`, `sync`, `census`, `name`, `leave`).
    Membership,
    /// Any other word.
    Unknown,
}

fn route(mode: Option<&str>) -> Route {
    match mode {
        Some("-h" | "--help") => Route::Help,
        None | Some("--json") => Route::View,
        Some("remove" | "admit" | "invite" | "requests" | "ignore") => Route::Lab,
        Some("activity") => Route::Activity,
        Some(
            "init" | "join" | "peers" | "allow" | "access" | "publish" | "sync" | "census"
            | "leave" | "name",
        ) => Route::Membership,
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
#[path = "fleet_cli_tests.rs"]
mod tests;
