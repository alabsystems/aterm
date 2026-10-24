// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines <verb>`: argument parsing and dispatch.

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

/// `aterm machines` usage, printed by `help`, `-h`/`--help`, a bare `aterm machines`, and
/// after the reason line of a command line this module refuses.
pub const USAGE: &str = "\
aterm machines — configure and manage a lab of Macs from this Mac over SSH, with no MDM
and no Apple account.

USAGE:
    aterm machines [--controller <dir>] <verb> [args]

    --controller <dir>        the controller directory (default: machines/ in aterm's
                              config directory)

SET UP
    init [--passphrase]       make this Mac the controller: machines.toml, the fleet SSH
                              key, the signing and recovery keys (no passphrase, so plan
                              and apply sign unattended; --passphrase, now or later, has
                              ssh-keygen ask one for the signing and recovery keys)
    scan [--wait <s>] [--subnet <cidr>]
                              Macs on the LAN: Bonjour's Remote Login, SFTP and Screen
                              Sharing advertisements, and port 22 across a subnet (a /22
                              at most)
    enroll <address>…|--found [--user <admin>] [--parallel N]
    enroll <address> [--user <admin>] [--name <n>] [--fingerprint <fp>]
                              install the agent on Macs, at a terminal: you type the
                              admin's password once for every Mac (held in memory and
                              piped to each Mac's ssh and sudo); --found adds every Mac
                              scan sees that is not enrolled (one advertising macOS's
                              SFTP or Screen Sharing beside Remote Login, never this
                              Mac); the admin is --user, else the one earlier Macs
                              enrolled with, else your login name (when the Macs refuse
                              a name --user did not give, enroll asks for the name once
                              and retries the typed password); a host key is trusted on
                              first use unless --fingerprint checks it; a Mac is named
                              by the tag its Local hostname starts with (m10 of
                              m10-MacBook-Pro-M4-Max-36GB-1TB), or by --name
    unenroll <host> [--unreachable]
                              at a terminal: remove the agent and the Mac's GitHub keys,
                              unpin its host key and retire its entry (machines.toml
                              keeps its [[host]] block as a comment); --unreachable:
                              without contacting a Mac that no longer answers, so its
                              agent stays on it
    rename <host> <new-name>  set or change a host's name in machines.toml, enrolled or
                              not (alias: name)

LOOK
    ls [--json]               the inventory
    tabs [<host>…] [--json]   aterm instances, tabs, vitals (CPU, GPU, RAM, disk, load,
                              network RTT) and AI CLI status of the hosts named, else of
                              this Mac, every enrolled Mac and each Mac Bonjour sees
    monitor [<host>…] [--open | --once | --watch] [--interval <s>] [--json]
                              tabs, live: with aterm running it opens the Fleet Monitor
                              tab, then prints tabs once (--open: the tab alone, or the
                              print when no aterm answers; --once: the print alone);
                              --watch reprints tabs every --interval seconds (default 2)
                              until Ctrl-C; top and watch are monitor --watch
    status [<host>…] [--by-item] [--json]
                              each host's newest report, read from state/ alone
                              (--by-item: each item's outcomes counted across hosts)
    outstanding               failures, refusals, person steps, deferred items, what an
                              apply left for a login or restart, hosts whose macOS
                              changed, hosts only --only runs reached, hosts never
                              planned or applied, and unreached hosts, as one to-do list

CHANGE
    plan  [<host>…|--all] [--now] [--parallel N] [--only <table>,…] [--json]
    apply <host>…|--all [--now] [--parallel N] [--only <table>,…] [--json]
                              bring hosts to their baseline (plan changes nothing, and
                              naming no host plans them all); apply first upgrades an
                              agent that is not this aterm's build, and the default
                              baseline names each Mac for what it is
                              (m10-MacBook-Pro-M4-Max-36GB-1TB); --now relaunches or
                              replaces a running app even while someone is at the Mac
                              (else that waits for the Mac to be idle); --only runs
                              just those baseline tables (apply --all --only names
                              renames the fleet), and status keeps the other tables'
                              last results
    attach <host> [<run>] [--json]
                              follow a run whose connection dropped
    run <host>…|--all [--user <u>] [--parallel N] -- <argv>
                              run one program by its absolute path (no shell, no PATH
                              lookup) as root, or as the account <u>
    github login              store the [github] machine user's token in the login
                              keychain (typed, not echoed) once GitHub signs it in as
                              that account with the admin:public_key scope
    github revoke <host>      delete the host's GitHub keys from the machine user; those
                              keys are never registered again (a key it makes anew is)
    rekey <host>…|--all --signers <file> [--key <private key>] [--parallel N]
                              replace the signers every host trusts with <file>, signed
                              with the signing key or <key> (the recovery key; its .pub
                              beside it, or in keys/); the controller's own copy follows
                              once a host takes it

PEOPLE
    person [<host>…|--all]    the switches only a person can flip (Remote Login and its
                              full disk access for remote users, Screen Sharing, Full
                              Disk Access, Developer Tools), by step
    person <host> --here      at that Mac's keyboard: open each pane in turn and wait
                              for the switch
    screen <host> [<step>]    the same over Screen Sharing, carried by the fleet's SSH
                              connection

ON A HOST
    here plan|apply <baseline.toml> [--json]
                              run a baseline on this Mac directly (apply under sudo)
    agent <cmd>               the host agent, which the controller runs over SSH

<host> is a host's name, uuid or address. --parallel N (1 to 64) reaches N hosts at once
(default: [controller] parallel in machines.toml, else 8). A host held by an IP address
that does not answer there is looked for at its Local hostname (lab-07.local), and moved
there only when ssh answers under its pinned host key.

Exit status: 0 nothing failed; 1 something failed (an unreadable inventory, a host that
refused contact or a run; for enroll and unenroll, also a Mac they could not reach); 2
nothing failed but some hosts could not be reached, or the command line was refused.
scan exits 0 even when it cannot read the inventory (each Mac then shows ?), and rename
exits 2 on any error. `aterm machines help enroll` is the checklist for each Mac;
`aterm help machines` has the manual.
";

/// `aterm machines help enroll`: the visit to each Mac no software replaces.
pub const ENROLL_CHECKLIST: &str = "\
At each Mac, once (the visit no software replaces):

  1. A new Mac: Setup Assistant. Create the lab admin account, sign in to no Apple
     account, and skip the rest.
  2. System Settings ▸ General ▸ Sharing:
       - set Computer Name and Local hostname to the label on the case (lab-07); apply
         adds what the Mac is (lab-07 MacBook Pro M4 Max 36GB 1TB);
       - turn Remote Login on, and in its ⓘ turn on \"Allow full disk access for
         remote users\";
       - turn Screen Sharing on, with access for administrators only.
  3. Optional: to check the Mac's identity, pass --fingerprint to enroll. The SSH host
     key fingerprint (the case label can carry it) reads, in Terminal:
       ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
     Without it, enroll trusts the key the Mac first answers with and prints it.
  4. From the controller:
       aterm machines enroll lab-07.local
       aterm machines apply lab-07
     enroll logs in as the admin earlier Macs enrolled with, else as your login name on
     this Mac (--user <admin> names another), and installs the agent; apply installs
     aterm.app, whose switches step 5 flips, and the rest of the baseline. Many Macs at
     once, the password typed once for all of them:
       aterm machines enroll --found        (every Mac scan sees, not yet enrolled)
       aterm machines enroll lab-07.local lab-08.local
       aterm machines apply --all
     Each Mac's ID (its host key fingerprint) is printed before the one password prompt.
     Trusted on first use, the password goes to whatever answers first (with --found,
     any machine on the LAN advertising a Mac's Remote Login): enroll on a LAN you
     control. When the Macs refuse the password and --user named none, enroll asks the
     admin account's name once and tries the same password under it; for each Mac that
     still refuses, it asks for the password once more. A Mac that cannot be enrolled
     says why on its own line and the rest go on; the same command again enrolls the
     ones left (a Mac enrolled already is skipped).
  5. Then, while you are still at the Mac's keyboard:
       aterm machines person lab-07 --here
     It opens the Full Disk Access pane, then the Developer Tools pane, on that Mac's
     screen: flip both switches. A Mac done this way needs no Screen Sharing.

`aterm machines person` counts these steps later, so a skipped one is never silent.
";

const VERBS: &[&str] = &[
    "init",
    "scan",
    "enroll",
    "unenroll",
    "rename",
    "ls",
    "tabs",
    "monitor",
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

const RETIRED_AGENT: &str = "systems.alab.aterm-machines";

const RETIRED_AGENT_DIR: &str = ".local/share/aterm-machines";

const RETIRED_FILES: &[&str] = &[
    ".metadata_never_index",
    "bin/aterm",
    "state/daemon.log",
    "state/daemon.sock",
];

const RETIRED_DIRS: &[&str] = &["bin", "state/runs", "state", "cache", "stage", "inbox"];

/// Whether `aterm pkg <args>` retires the LaunchAgent earlier builds installed: `shell-setup`
/// without `--dry-run`.
#[must_use]
pub fn pkg_retires(args: &[OsString]) -> bool {
    args.first().is_some_and(|a| a == "shell-setup") && !args.iter().any(|a| a == "--dry-run")
}

/// Take away the LaunchAgent earlier builds installed in this account ([`retire_agent`]).
pub fn retire_lan_agent() {
    let sys = Live::new();
    if sys.euid() == 0 || !crate::sessions::is_live_account() {
        return;
    }
    if let Some(home) = aterm_types::dirs::home_dir() {
        retire_agent(&sys, &home);
    }
}

fn retire_agent(sys: &dyn System, home: &Path) {
    if sys.euid() == 0 {
        return;
    }
    if let Some(parent) = physical_directory_below(sys, home, Path::new("Library/LaunchAgents")) {
        let plist = parent.join(format!("{RETIRED_AGENT}.plist"));
        if sys.stat(&plist).is_ok() {
            let _ = sys.run(
                &crate::port::Cmd::new("/bin/launchctl")
                    .args(["bootout", &format!("gui/{}/{RETIRED_AGENT}", sys.euid())])
                    .budget(Duration::from_secs(5)),
            );
            let _ = sys.remove(&plist);
        }
    }
    sweep_retired_dir(sys, home, Path::new(RETIRED_AGENT_DIR));
}

fn physical_directory_below(
    sys: &dyn System,
    home: &Path,
    below: &Path,
) -> Option<std::path::PathBuf> {
    let real = |p: &Path| sys.stat(p).is_ok_and(|st| st.dir && !st.symlink);
    if !home.is_absolute() || !real(home) {
        return None;
    }
    let mut dir = home.to_path_buf();
    for part in below.components() {
        let std::path::Component::Normal(name) = part else {
            return None;
        };
        dir.push(name);
        if !real(&dir) {
            return None;
        }
    }
    Some(dir)
}

fn sweep_retired_dir(sys: &dyn System, home: &Path, below: &Path) {
    let Some(dir) = physical_directory_below(sys, home, below) else {
        return;
    };
    let dir = dir.as_path();
    let is_dir = |p: &Path| sys.stat(p).is_ok_and(|st| st.dir);
    let is_file = |p: &Path| sys.stat(p).is_ok_and(|st| !st.dir && !st.symlink);
    let reached = |rel: &Path| {
        rel.ancestors()
            .skip(1)
            .all(|up| up.as_os_str().is_empty() || is_dir(&dir.join(up)))
    };
    if !is_dir(dir) {
        return;
    }
    for rel in RETIRED_FILES {
        let path = dir.join(rel);
        if reached(Path::new(rel)) && is_file(&path) {
            let _ = sys.remove(&path);
        }
    }
    for rel in RETIRED_DIRS {
        let path = dir.join(rel);
        if reached(Path::new(rel)) && is_dir(&path) {
            let _ = sys.remove(&path);
        }
    }
    let _ = sys.remove(dir);
}

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
        "scan" => {
            #[cfg(not(test))]
            retire_lan_agent();
            scan(root, &args[1..])
        }
        "here" => {
            #[cfg(not(test))]
            retire_lan_agent();
            here(&sys, &args[1..])
        }
        verb if VERBS.contains(&verb) || matches!(verb, "name" | "top" | "watch") => {
            #[cfg(not(test))]
            retire_lan_agent();
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

#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    words: Vec<String>,
    all: bool,
    now: bool,
    json: bool,
    open: bool,
    once: bool,
    watch: bool,
    interval: Option<u64>,
    by_item: bool,
    here: bool,
    unreachable: bool,
    passphrase: bool,
    parallel: Option<usize>,
    user: Option<String>,
    signers: Option<String>,
    key: Option<String>,
    only: Vec<String>,
    argv: Vec<String>,
}

fn flags_of(verb: &str) -> &'static [&'static str] {
    match verb {
        "ls" | "attach" => &["--json"],
        "tabs" => &["--all", "--json"],
        "monitor" | "top" | "watch" => &[
            "--all",
            "--open",
            "--once",
            "--watch",
            "--interval",
            "--json",
        ],
        "status" => &["--all", "--by-item", "--json"],
        "plan" | "apply" => &["--all", "--now", "--parallel", "--only", "--json"],
        "run" => &["--all", "--user", "--parallel", "--"],
        "rekey" => &["--all", "--signers", "--key", "--parallel"],
        "person" => &["--all", "--here"],
        "unenroll" => &["--unreachable"],
        "init" => &["--passphrase"],
        _ => &[],
    }
}

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
            "--open" => o.open = true,
            "--once" => o.once = true,
            "--watch" => o.watch = true,
            "--interval" => {
                let v = value()?;
                o.interval = Some(
                    v.parse::<u64>()
                        .ok()
                        .filter(|n| (1..=3600).contains(n))
                        .ok_or_else(|| format!("--interval takes 1 to 3600 seconds, not {v:?}"))?,
                );
            }
            "--by-item" => o.by_item = true,
            "--here" => o.here = true,
            "--unreachable" => o.unreachable = true,
            "--passphrase" => o.passphrase = true,
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
            "--only" => {
                let tables = value()?;
                for table in tables.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    if !o.only.iter().any(|t| t == table) {
                        o.only.push(table.to_owned());
                    }
                }
                crate::baseline::check_only(&o.only)?;
            }
            "--signers" => o.signers = Some(value()?),
            "--key" => o.key = Some(value()?),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    Ok(o)
}

fn words(o: &Opts, min: usize, max: usize, takes: &str) -> Result<(), String> {
    if (min..=max).contains(&o.words.len()) {
        Ok(())
    } else {
        Err(format!("takes {takes}"))
    }
}

fn controller(sys: &dyn System, paths: Paths, verb: &str, args: &[String]) -> ExitCode {
    let terminals = Terminals {
        stdin: std::io::stdin().is_terminal(),
        stderr: std::io::stderr().is_terminal(),
    };
    if verb == "enroll" {
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
        "rename" | "name" => words(&o, 2, 2, "<host> <new-name>"),
        "github" => match o.words.first().map(String::as_str) {
            Some("login") => words(&o, 1, 1, "login"),
            Some("revoke") => words(&o, 2, 2, "revoke <host>"),
            _ => Err("takes login or revoke <host>".to_owned()),
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
        "init" => commands::init(&env, o.passphrase, &mut std::io::stdout().lock()),
        "unenroll" => commands::unenroll(&env, &o.words[0], o.unreachable),
        "rename" | "name" => commands::rename(&env, &o.words[0], &o.words[1]),
        "ls" => commands::ls(&env),
        "tabs" => commands::tabs(&env, &o.words, o.all),
        "monitor" | "top" | "watch" => commands::monitor(
            &env,
            &o.words,
            o.all,
            o.open,
            o.once,
            o.watch || matches!(verb, "top" | "watch"),
            o.interval.unwrap_or(2),
        ),
        "status" => commands::status(&env, &o.words, o.all, o.by_item),
        "outstanding" => commands::outstanding(&env),
        "plan" => commands::plan_or_apply_only(&env, &o.words, o.all, Mode::Plan, o.now, &o.only),
        "apply" => commands::plan_or_apply_only(&env, &o.words, o.all, Mode::Apply, o.now, &o.only),
        "attach" => commands::attach(&env, &o.words[0], o.words.get(1).map(String::as_str)),
        "run" => commands::run(&env, &o.words, o.all, o.user.as_deref(), &o.argv),
        "github" if o.words[0] == "login" => commands::github_login(&env),
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

fn refuse(why: &str) -> ExitCode {
    eprintln!("aterm machines: {why}");
    eprint!("{USAGE}");
    ExitCode::from(2)
}

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
        (Some(p), Some(inv)) => mark(&sys, p, inv, row),
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
    if let Some(why) = &browse.sftp_failed {
        println!(
            "macOS's SFTP advertisements could not be browsed ({}): `enroll --found` takes only \
             the Macs Screen Sharing marks",
            shown(why)
        );
    }
    if rows.is_empty() && browse.unresolved.is_empty() {
        println!(
            "no Mac seen: a Mac advertises Remote Login only when it was turned on in System \
             Settings; try --subnet <cidr>"
        );
    }
    ExitCode::SUCCESS
}

fn mark(sys: &dyn System, paths: &Paths, inv: &Inventory, row: &discover::Found) -> String {
    match discover::pinned_host(sys, paths, inv, row) {
        Read::Value(name) => format!("enrolled {name}"),
        Read::Absent => "new".to_owned(),
        Read::Unusable(_) => "?".to_owned(),
    }
}

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
    use crate::port::{Fake, Ran};

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
    fn the_lan_agent_earlier_builds_installed_is_unloaded_and_what_it_made_taken_away() {
        let home = Path::new("/Users//alice");
        let plist = "/Users//alice/Library/LaunchAgents/systems.alab.aterm-machines.plist";
        let dir = "/Users//alice/.local/share/aterm-machines";
        let bootout = "/bin/launchctl bootout gui/501/systems.alab.aterm-machines";
        let alice = || {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys
        };
        let real_dir = crate::port::Stat {
            uid: 501,
            gid: 20,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        };
        let installed = || {
            let sys = alice();
            sys.file(plist, b"<plist/>")
                .file(&format!("{dir}/.metadata_never_index"), b"")
                .file(&format!("{dir}/bin/aterm"), b"a copy of aterm")
                .file(&format!("{dir}/state/daemon.log"), b"refused: \n")
                .file(&format!("{dir}/state/daemon.sock"), b"")
                .file(&format!("{dir}/state/notes.txt"), b"not the job's")
                .on(bootout, Ran::ok(""));
            for parent in [
                home.to_path_buf(),
                home.join(".local"),
                home.join(".local/share"),
                home.join("Library"),
                home.join("Library/LaunchAgents"),
            ] {
                sys.stat_of(&parent.to_string_lossy(), real_dir);
            }
            for sub in ["", "/bin", "/state", "/state/runs"] {
                sys.stat_of(&format!("{dir}{sub}"), real_dir);
            }
            sys
        };
        let sys = installed();
        retire_agent(&sys, home);
        let calls = sys.calls();
        let at = |c: &str| calls.iter().position(|x| x == c);
        assert!(at(bootout).is_some(), "{calls:?}");
        assert!(at(bootout) < at(&format!("remove {plist}")), "{calls:?}");
        for gone in [
            plist.to_owned(),
            format!("{dir}/.metadata_never_index"),
            format!("{dir}/bin/aterm"),
            format!("{dir}/state/daemon.log"),
            format!("{dir}/state/daemon.sock"),
        ] {
            assert_eq!(sys.written(&gone), None, "{gone}");
        }
        let removed: Vec<&str> = calls
            .iter()
            .filter_map(|c| c.strip_prefix("remove "))
            .collect();
        assert_eq!(
            removed,
            [
                plist.to_owned(),
                format!("{dir}/.metadata_never_index"),
                format!("{dir}/bin/aterm"),
                format!("{dir}/state/daemon.log"),
                format!("{dir}/state/daemon.sock"),
                format!("{dir}/bin"),
                format!("{dir}/state/runs"),
                format!("{dir}/state"),
                dir.to_owned(),
            ]
        );
        assert!(sys.written(&format!("{dir}/state/notes.txt")).is_some());

        let sys = installed();
        let link = crate::port::Stat {
            dir: false,
            symlink: true,
            ..real_dir
        };
        sys.stat_of(&format!("{dir}/state"), link)
            .stat_of(&format!("{dir}/bin"), link)
            .file(&format!("{dir}/cache"), b"a file of mine");
        retire_agent(&sys, home);
        let calls = sys.calls();
        for kept in [
            "/state",
            "/state/runs",
            "/state/daemon.log",
            "/bin",
            "/bin/aterm",
            "/cache",
        ] {
            let kept = format!("remove {dir}{kept}");
            assert!(!calls.contains(&kept), "{kept}: {calls:?}");
        }
        assert!(calls.contains(&format!("remove {dir}/.metadata_never_index")));

        let sys = alice();
        retire_agent(&sys, home);
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        let sys = installed();
        sys.stat_of(
            dir,
            crate::port::Stat {
                dir: false,
                symlink: true,
                ..real_dir
            },
        );
        retire_agent(&sys, home);
        assert!(sys.written(&format!("{dir}/bin/aterm")).is_some());
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with(&format!("remove {dir}")))
        );
        let sys = Fake::root();
        sys.file(plist, b"<plist/>");
        retire_agent(&sys, home);
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn only_a_shell_setup_that_is_not_a_dry_run_retires_the_job() {
        let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        assert!(pkg_retires(&args(&["shell-setup"])));
        assert!(pkg_retires(&args(&["shell-setup", "--path"])));
        assert!(!pkg_retires(&args(&["shell-setup", "--dry-run"])));
        assert!(!pkg_retires(&args(&["shell-setup", "--path", "--dry-run"])));
        assert!(!pkg_retires(&args(&["install", "shell-setup"])));
        assert!(!pkg_retires(&[]));
    }

    #[test]
    fn a_retired_directory_is_swept_only_where_the_job_made_it() {
        use std::os::unix::fs::symlink;
        let tmp = aterm_tempfile::Builder::new()
            .prefix("at-retire-")
            .tempdir_in("/tmp")
            .unwrap();
        let t = tmp.path();
        let live = Live::new();
        let write = |p: &Path| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        };
        let there = |p: &Path| p.symlink_metadata().is_ok();

        let job = t.join("job");
        for d in ["bin", "state/runs", "cache", "stage"] {
            std::fs::create_dir_all(job.join(d)).unwrap();
        }
        for f in [
            ".metadata_never_index",
            "bin/aterm",
            "state/daemon.log",
            "stage/notes",
        ] {
            write(&job.join(f));
        }
        let _sock = std::os::unix::net::UnixListener::bind(job.join("state/daemon.sock")).unwrap();
        write(&t.join("mine/inbox/keep"));
        symlink(t.join("mine/inbox"), job.join("inbox")).unwrap();
        sweep_retired_dir(&live, t, Path::new("job"));
        for gone in [".metadata_never_index", "bin", "state", "cache"] {
            assert!(!there(&job.join(gone)), "{gone}");
        }
        for kept in ["stage/notes", "inbox", "inbox/keep"] {
            assert!(there(&job.join(kept)), "{kept}");
        }
        std::fs::remove_file(job.join("stage/notes")).unwrap();
        std::fs::remove_file(job.join("inbox")).unwrap();
        sweep_retired_dir(&live, t, Path::new("job"));
        assert!(!there(&job));
        assert!(there(&t.join("mine/inbox/keep")));

        let job = t.join("job2");
        std::fs::create_dir_all(t.join("elsewhere/runs")).unwrap();
        write(&t.join("elsewhere/daemon.log"));
        write(&t.join("mine/bin/aterm"));
        write(&t.join("marker"));
        write(&job.join("cache"));
        symlink(t.join("elsewhere"), job.join("state")).unwrap();
        symlink(t.join("mine/bin"), job.join("bin")).unwrap();
        symlink(t.join("marker"), job.join(".metadata_never_index")).unwrap();
        sweep_retired_dir(&live, t, Path::new("job2"));
        for kept in [
            "job2/state",
            "job2/bin",
            "job2/cache",
            "job2/.metadata_never_index",
            "elsewhere/runs",
            "elsewhere/daemon.log",
            "mine/bin/aterm",
            "marker",
        ] {
            assert!(there(&t.join(kept)), "{kept}");
        }

        for (i, prefix, tail) in [
            (0, ".local", "share/aterm-machines"),
            (1, ".local/share", "aterm-machines"),
            (2, RETIRED_AGENT_DIR, ""),
        ] {
            let home = t.join(format!("home-{i}"));
            let outside = t.join(format!("outside-{i}"));
            let alias = home.join(prefix);
            std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
            let marker = outside.join(tail).join("bin/aterm");
            write(&marker);
            symlink(&outside, &alias).unwrap();
            sweep_retired_dir(&live, &home, Path::new(RETIRED_AGENT_DIR));
            assert!(there(&alias), "the ancestor alias stays");
            assert_eq!(std::fs::read(&marker).unwrap(), b"x", "external data stays");
        }
        for (i, prefix, tail) in [
            (0, "Library", "LaunchAgents"),
            (1, "Library/LaunchAgents", ""),
        ] {
            let home = t.join(format!("plist-home-{i}"));
            let outside = t.join(format!("plist-outside-{i}"));
            let alias = home.join(prefix);
            std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
            let plist = outside.join(tail).join(format!("{RETIRED_AGENT}.plist"));
            write(&plist);
            symlink(&outside, &alias).unwrap();
            assert!(
                physical_directory_below(&live, &home, Path::new("Library/LaunchAgents")).is_none()
            );
            assert_eq!(std::fs::read(&plist).unwrap(), b"x");
        }
        let home = t.join("physical-home");
        let parent = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&parent).unwrap();
        assert_eq!(
            physical_directory_below(&live, &home, Path::new("Library/LaunchAgents")),
            Some(parent)
        );
        assert!(physical_directory_below(&live, &home, Path::new("missing")).is_none());
        write(&home.join("not-a-directory"));
        assert!(physical_directory_below(&live, &home, Path::new("not-a-directory")).is_none());
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
    fn only_takes_known_table_names_and_refuses_the_rest_with_the_list() {
        let o = parse(
            "apply",
            &[
                "--all".into(),
                "--only".into(),
                "names, power".into(),
                "--only".into(),
                "names".into(),
            ],
        )
        .unwrap();
        assert_eq!(o.only, ["names", "power"]);
        let err = parse("plan", &["--only".into(), "nmaes".into()]).unwrap_err();
        assert!(
            err.contains("\"nmaes\"") && err.contains("names, sharing"),
            "{err}"
        );
        assert!(parse("status", &["--only".into(), "names".into()]).is_err());
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
