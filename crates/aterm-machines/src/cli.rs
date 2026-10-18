// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines <verb>`: argument parsing and dispatch.
//!
//! The front door (`crates/aterm/src/main.rs`) hands every argument after `machines` here.
//! A flag this does not know is refused with the usage, the same at a terminal and through
//! a pipe (`crates/aterm/tests/front_door_verbs.rs`).

use crate::controller::commands::{self, Env};
use crate::controller::discover;
use crate::controller::enroll;
use crate::controller::inventory::{Inventory, Paths};
use crate::controller::screen::{self, Terminals};
use crate::controller::text::shown;
use crate::model::{Mode, Read};
use crate::port::{Live, System};
use std::ffi::OsString;
use std::io::IsTerminal as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

/// The usage text.
pub const USAGE: &str = "\
aterm machines — configure and manage a lab of Macs from this Mac, over SSH: no MDM and
no Apple account (docs/DESIGN-machines-2026-10-16.md).

USAGE:
    aterm machines [--controller <dir>] <verb> [args]

    --controller <dir>        the controller directory (default: machines/ in aterm's
                              config directory)

SET UP
    init                      make this Mac the controller: machines.toml, the fleet SSH
                              key, the signing and recovery keys
    scan [--wait <s>] [--subnet <cidr>]
                              Macs on the LAN: Bonjour's Remote Login and Screen Sharing
                              advertisements, and port 22 across a subnet (a /22 at most)
    enroll <address> --user <admin> [--name <n>] (--fingerprint <fp> | --trust-first-use)
                              install the agent on a Mac (at a terminal: you type the
                              admin's password to ssh and to sudo)
    unenroll <host> [--unreachable]
                              remove the agent, the Mac's GitHub keys and its entry

LOOK
    ls [--json]               the inventory
    status [<host>…] [--by-item] [--json]
                              each host's newest report, read from state/ alone
    outstanding               failures, refusals, person steps, deferred items, what the
                              newest report (an apply's) left for a login or restart,
                              hosts whose macOS changed, and unreached hosts, as one
                              to-do list

CHANGE
    plan  [<host>…|--all] [--now] [--parallel N] [--json]
    apply [<host>…|--all] [--now] [--parallel N] [--json]
                              bring hosts to their baseline (plan changes nothing); apply
                              first upgrades an agent that is not this aterm's build
    attach <host> [<run>]     follow a run whose connection dropped
    run <host>…|--all [--user <u>] -- <argv>
                              run one program (no shell) as root, or as the account <u>
    github revoke <host>      delete the host's GitHub keys from the machine user; those
                              keys are never registered again (a key it makes anew is)
    rekey <host>…|--all --signers <file> [--key <private key>]
                              replace the signers every host trusts with <file>, signed
                              with the signing key or <key> (the recovery key; its .pub
                              beside it, or in keys/); the controller's own copy follows
                              once a host takes it

PEOPLE
    person [<host>…|--all]    the switches only a person can flip (Full Disk Access,
                              Developer Tools, Screen Sharing), by step
    person <host> --here      at that Mac's keyboard: open each pane in turn and wait
                              for the switch
    screen <host> [<step>]    the same over Screen Sharing, carried by the fleet's SSH
                              connection

ON A HOST
    here plan|apply <baseline.toml> [--json]
                              run a baseline on this Mac directly (apply under sudo)
    agent <cmd>               the host agent, which the controller runs over SSH

Exit status: 0 nothing failed; 1 something failed (an unreadable inventory, a host that
refused contact or a run); 2 nothing failed but some hosts could not be reached, or the
command line was refused. `aterm machines help enroll` is the checklist for each Mac;
`aterm help machines` has the manual.
";

/// `aterm machines help enroll`: the visit to each Mac no software replaces (design §4.1).
pub const ENROLL_CHECKLIST: &str = "\
At each Mac, once (the visit no software replaces):

  1. A new Mac: Setup Assistant. Create the lab admin account, sign in to no Apple
     account, and skip the rest.
  2. System Settings ▸ General ▸ Sharing:
       - set Computer Name and Local hostname to the label on the case (lab-07);
       - turn Remote Login on, and in its ⓘ turn on \"Allow full disk access for
         remote users\";
       - turn Screen Sharing on, with access for administrators only.
  3. Read the SSH host key fingerprint the case label pairs with. In Terminal:
       ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
     Or skip this and pass --trust-first-use at enrollment, accepting the risk on a
     LAN you trust.
  4. From the controller:
       aterm machines enroll lab-07.local --user labadmin --fingerprint SHA256:…
       aterm machines apply lab-07
     enroll installs the agent; apply installs aterm.app, whose switches step 5 flips,
     and the rest of the baseline.
  5. Then, while you are still at the Mac's keyboard:
       aterm machines person lab-07 --here
     It opens the Full Disk Access pane, then the Developer Tools pane, on that Mac's
     screen: flip both switches. A Mac done this way needs no Screen Sharing.

`aterm machines person` counts these steps later, so a skipped one is never silent.
";

/// Every verb the usage names.
const VERBS: &[&str] = &[
    "init",
    "scan",
    "enroll",
    "unenroll",
    "ls",
    "status",
    "outstanding",
    "plan",
    "apply",
    "attach",
    "run",
    "github",
    "rekey",
    "person",
    "screen",
    "here",
    "agent",
];

/// `aterm machines <args>`.
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (root, args) = match args.split_first() {
        Some((first, rest)) if first == "--controller" => match rest.split_first() {
            Some((dir, rest)) => (Some(Paths::at(Path::new(dir))), rest.to_vec()),
            None => return refuse("--controller takes a directory"),
        },
        _ => (None, args),
    };
    let Some(first) = args.first() else {
        print!("{USAGE}");
        return ExitCode::from(2);
    };
    let sys = Live::new();
    match first.as_str() {
        "help" if args.get(1).is_some_and(|topic| topic == "enroll") => {
            print!("{ENROLL_CHECKLIST}");
            ExitCode::SUCCESS
        }
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "agent" => crate::agent::main(&args[1..], &sys),
        "scan" => scan(root, &args[1..]),
        "here" => here(&sys, &args[1..]),
        verb if VERBS.contains(&verb) => {
            let Some(paths) = root.or_else(Paths::default_root) else {
                eprintln!("aterm machines: no config directory to keep the controller in");
                return ExitCode::FAILURE;
            };
            controller(&sys, paths, verb, &args[1..])
        }
        flag if flag.starts_with('-') => refuse(&format!("unknown option {flag}")),
        other => refuse(&format!("unknown verb {other:?}")),
    }
}

/// What a controller verb was given: its words, its flags, and after `--` an argv.
#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    words: Vec<String>,
    all: bool,
    now: bool,
    json: bool,
    by_item: bool,
    here: bool,
    unreachable: bool,
    parallel: Option<usize>,
    user: Option<String>,
    signers: Option<String>,
    key: Option<String>,
    argv: Vec<String>,
}

/// The flags each controller verb takes.
fn flags_of(verb: &str) -> &'static [&'static str] {
    match verb {
        // `outstanding` is a to-do list for a person: `status --json` has the reports.
        "ls" | "attach" => &["--json"],
        "status" => &["--all", "--by-item", "--json"],
        "plan" | "apply" => &["--all", "--now", "--parallel", "--json"],
        "run" => &["--all", "--user", "--parallel", "--"],
        "rekey" => &["--all", "--signers", "--key", "--parallel"],
        "person" => &["--all", "--here"],
        "unenroll" => &["--unreachable"],
        _ => &[],
    }
}

/// Parse `args` for `verb`: a flag it does not take is refused, never ignored.
fn parse(verb: &str, args: &[String]) -> Result<Opts, String> {
    let allowed = flags_of(verb);
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        if !flag.starts_with('-') {
            o.words.push(arg.clone());
            continue;
        }
        if !allowed.contains(&flag) {
            return Err(format!("unknown option {flag}"));
        }
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} takes a value"))
        };
        match flag {
            "--" => {
                o.argv = it.by_ref().cloned().collect();
            }
            "--all" => o.all = true,
            "--now" => o.now = true,
            "--json" => o.json = true,
            "--by-item" => o.by_item = true,
            "--here" => o.here = true,
            "--unreachable" => o.unreachable = true,
            "--parallel" => {
                let v = value()?;
                o.parallel = Some(
                    v.parse::<usize>()
                        .ok()
                        .filter(|n| (1..=64).contains(n))
                        .ok_or_else(|| format!("--parallel takes 1 to 64, not {v:?}"))?,
                );
            }
            "--user" => o.user = Some(value()?),
            "--signers" => o.signers = Some(value()?),
            "--key" => o.key = Some(value()?),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    Ok(o)
}

/// Exactly `n` words, or the refusal naming what the verb takes.
fn words(o: &Opts, min: usize, max: usize, takes: &str) -> Result<(), String> {
    if (min..=max).contains(&o.words.len()) {
        Ok(())
    } else {
        Err(format!("takes {takes}"))
    }
}

/// A verb that runs on the controller.
fn controller(sys: &dyn System, paths: Paths, verb: &str, args: &[String]) -> ExitCode {
    let terminals = Terminals {
        stdin: std::io::stdin().is_terminal(),
        stderr: std::io::stderr().is_terminal(),
    };
    if verb == "enroll" {
        // Enrollment parses its own arguments (controller::enroll::parse_args).
        return enroll::main(sys, &paths, args, terminals.stdin && terminals.stderr);
    }
    let o = match parse(verb, args) {
        Ok(o) => o,
        Err(why) => return refuse(&format!("{verb}: {why}")),
    };
    let env = Env {
        sys,
        paths,
        json: o.json,
        tty: terminals.stdin && terminals.stderr,
        parallel: o.parallel,
        agent_sock: std::env::var_os("SSH_AUTH_SOCK"),
    };
    let checked = match verb {
        "init" | "ls" | "outstanding" => words(&o, 0, 0, "no arguments"),
        "attach" => words(&o, 1, 2, "<host> [<run>]"),
        "unenroll" => words(&o, 1, 1, "one <host>"),
        "github" => match o.words.first().map(String::as_str) {
            Some("revoke") => words(&o, 2, 2, "revoke <host>"),
            _ => Err("takes revoke <host>".to_owned()),
        },
        "screen" => words(&o, 1, 2, "<host> [<step>]"),
        "person" if o.here => words(&o, 1, 1, "one <host> with --here"),
        "rekey" if o.signers.is_none() => Err("takes --signers <file>".to_owned()),
        "run" if o.argv.is_empty() => Err("takes -- <argv>".to_owned()),
        _ => Ok(()),
    };
    if let Err(why) = checked {
        return refuse(&format!("{verb}: {why}"));
    }
    match verb {
        "init" => commands::init(&env),
        "unenroll" => commands::unenroll(&env, &o.words[0], o.unreachable),
        "ls" => commands::ls(&env),
        "status" => commands::status(&env, &o.words, o.all, o.by_item),
        "outstanding" => commands::outstanding(&env),
        "plan" => commands::plan_or_apply(&env, &o.words, o.all, Mode::Plan, o.now),
        "apply" => commands::plan_or_apply(&env, &o.words, o.all, Mode::Apply, o.now),
        "attach" => commands::attach(&env, &o.words[0], o.words.get(1).map(String::as_str)),
        "run" => commands::run(&env, &o.words, o.all, o.user.as_deref(), &o.argv),
        "github" => commands::github_revoke(&env, &o.words[1]),
        "rekey" => commands::rekey(
            &env,
            &o.words,
            o.all,
            Path::new(o.signers.as_deref().unwrap_or_default()),
            o.key.as_deref().map(Path::new),
        ),
        "person" if o.here => commands::walk(
            &env,
            &o.words[0],
            true,
            None,
            terminals,
            &screen::LiveOpener { sys },
        ),
        "person" => commands::person(&env, &o.words, o.all),
        "screen" => commands::walk(
            &env,
            &o.words[0],
            false,
            o.words.get(1).map(String::as_str),
            terminals,
            &screen::LiveOpener { sys },
        ),
        other => refuse(&format!("unknown verb {other:?}")),
    }
}

/// `aterm machines here plan|apply <baseline.toml> [--json]`.
fn here(sys: &dyn System, args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let rest: Vec<&String> = args.iter().filter(|a| *a != "--json").collect();
    if let Some(flag) = rest.iter().find(|a| a.starts_with('-')) {
        return refuse(&format!("here: unknown option {flag}"));
    }
    let mode = match rest.first().map(|s| s.as_str()) {
        Some("plan") => Mode::Plan,
        Some("apply") => Mode::Apply,
        _ => return refuse("here: takes plan|apply <baseline.toml>"),
    };
    let [_, path] = rest.as_slice() else {
        return refuse("here: takes plan|apply <baseline.toml>");
    };
    commands::here(sys, mode, Path::new(path.as_str()), json)
}

/// One line, then the usage, on standard error; exit status 2.
fn refuse(why: &str) -> ExitCode {
    eprintln!("aterm machines: {why}");
    eprint!("{USAGE}");
    ExitCode::from(2)
}

/// `aterm machines scan [--wait <s>] [--subnet <cidr>]`.
fn scan(root: Option<Paths>, args: &[String]) -> ExitCode {
    let mut wait = Duration::from_secs(3);
    let mut cidr: Option<&str> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--wait" => match it.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(secs) if (1..=60).contains(&secs) => wait = Duration::from_secs(secs),
                _ => return refuse("scan --wait takes whole seconds from 1 to 60"),
            },
            "--subnet" => match it.next() {
                Some(c) => cidr = Some(c),
                None => return refuse("scan --subnet takes a subnet (a.b.c.d/n)"),
            },
            other => return refuse(&format!("scan: unknown argument {other}")),
        }
    }
    let sys = Live::new();
    let browse = match discover::browse(&sys, wait) {
        Ok(b) => b,
        Err(why) => {
            eprintln!("aterm machines scan: {why}");
            return ExitCode::FAILURE;
        }
    };
    let mut rows = browse.found;
    if let Some(cidr) = cidr {
        match discover::subnet(&discover::TcpKnock, cidr, Duration::from_secs(1)) {
            Ok(swept) => {
                let new: Vec<discover::Found> = swept
                    .into_iter()
                    .filter(|s| !rows.iter().any(|r| r.address == s.address))
                    .collect();
                rows.extend(new);
            }
            Err(why) => {
                eprintln!("aterm machines scan: {why}");
                return ExitCode::FAILURE;
            }
        }
    }
    // Which rows are enrolled Macs: by the host key each presents, never by name or address.
    // An inventory that is there and cannot be read leaves every row unknown (`?`), never
    // `new`: an unreadable inventory is not an empty one.
    let paths = root.or_else(Paths::default_root);
    let inventory = match &paths {
        Some(p) => match scan_inventory(&sys, p) {
            Read::Value(inv) => Some(inv),
            Read::Absent => Some(Inventory::default()),
            Read::Unusable(why) => {
                eprintln!(
                    "aterm machines scan: {why}; no Mac can be told enrolled or new, so each \
                     shows ?"
                );
                None
            }
        },
        None => None,
    };
    let marks = crate::controller::ssh::fan_out(&rows, 16, |row| match (&paths, &inventory) {
        (Some(p), Some(inv)) => match discover::pinned_host(&sys, p, inv, row) {
            Read::Value(name) => format!("enrolled {name}"),
            Read::Absent => "new".to_owned(),
            Read::Unusable(_) => "?".to_owned(),
        },
        _ => "?".to_owned(),
    });
    let width = rows
        .iter()
        .map(|r| r.address.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let mark_width = marks.iter().map(String::len).max().unwrap_or(3).max(3);
    println!(
        "{:<width$}  {:<5}  {:<18}  {:<mark_width$}  NAME",
        "ADDRESS", "PORT", "SERVICES", "MAC"
    );
    for (row, mark) in rows.iter().zip(&marks) {
        let services = match (row.ssh, row.screen_sharing) {
            (true, true) => "ssh screen-sharing",
            (true, false) => "ssh",
            (false, true) => "screen-sharing",
            (false, false) => "-",
        };
        let name = match row.source {
            discover::Source::Bonjour => shown(&row.name),
            discover::Source::Subnet => "(swept: not advertised)".to_owned(),
        };
        println!(
            "{:<width$}  {:<5}  {services:<18}  {mark:<mark_width$}  {name}",
            row.address, row.port
        );
    }
    for name in &browse.unresolved {
        println!("advertised, not located: {}", shown(name));
    }
    if rows.is_empty() && browse.unresolved.is_empty() {
        println!(
            "no Mac seen: a Mac advertises Remote Login only when it was turned on in System \
             Settings; try --subnet <cidr>"
        );
    }
    ExitCode::SUCCESS
}

/// The inventory `scan` compares host keys against: `Value` when it loads, `Absent` when
/// there is none yet (nothing is enrolled, so a Mac whose key shows is new), `Unusable` when
/// it is there and cannot be read or parsed.
fn scan_inventory(sys: &dyn System, paths: &Paths) -> Read<Inventory> {
    match sys.stat(&paths.inventory()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read::Absent,
        Err(e) => return Read::Unusable(format!("{}: {e}", paths.inventory().display())),
        Ok(_) => {}
    }
    match crate::controller::inventory::load(sys, paths) {
        Ok(inv) => Read::Value(inv),
        Err(why) => Read::Unusable(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;

    #[test]
    fn an_inventory_that_cannot_be_read_is_unknown_and_a_missing_one_is_empty() {
        let paths = Paths {
            root: std::path::PathBuf::from("/ctl/machines"),
        };
        assert!(matches!(
            scan_inventory(&Fake::root(), &paths),
            Read::Absent
        ));
        let corrupt = Fake::root();
        corrupt.file(
            "/ctl/machines/machines.toml",
            b"schema = 1\n[[host]]\nname = 3\n",
        );
        assert!(matches!(
            scan_inventory(&corrupt, &paths),
            Read::Unusable(_)
        ));
        let fine = Fake::root();
        fine.file("/ctl/machines/machines.toml", b"schema = 1\n");
        assert!(matches!(scan_inventory(&fine, &paths), Read::Value(_)));
    }

    #[test]
    fn the_usage_names_every_verb_main_entry_routes() {
        for verb in VERBS {
            assert!(
                USAGE
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(verb)),
                "{verb}"
            );
        }
        assert_eq!(
            main_entry(vec![OsString::from("help"), OsString::from("enroll")]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn an_unknown_flag_or_verb_is_refused_with_status_2() {
        for args in [
            &["--aterm-front-door-routing-probe"][..],
            &["frobnicate"],
            &["scan", "--bogus"],
        ] {
            let code = main_entry(args.iter().map(OsString::from).collect());
            assert_eq!(code, ExitCode::from(2), "{args:?}");
        }
        assert_eq!(
            main_entry(vec![OsString::from("--help")]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn outstanding_refuses_json_rather_than_print_text_a_script_cannot_parse() {
        let sys = Fake::root();
        sys.file("/ctl/machines/machines.toml", b"schema = 1\n");
        let paths = || Paths {
            root: std::path::PathBuf::from("/ctl/machines"),
        };
        assert_eq!(
            controller(&sys, paths(), "outstanding", &["--json".to_owned()]),
            ExitCode::from(2)
        );
        assert_eq!(
            controller(&sys, paths(), "outstanding", &[]),
            ExitCode::SUCCESS
        );
    }
}
