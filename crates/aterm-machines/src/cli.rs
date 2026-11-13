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
    storage plan --git-common-dir ABS [--json] [--verify-io-bytes N]
                              bounded local Git pack storage report; content verification
                              reads at most N bytes across both files (default 0)
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
    here apply --plain [--root-steps-only] [--leave-developer-mode]
               [--name m<N> [--nickname <word>]]
                              install Apple's Command Line Tools from aterm's own
                              baseline, reading no file, and with --name give the Mac
                              its generated computer and network names;
                              --root-steps-only preserves developer-tool installation
                              and selection while applying independent root steps;
                              --leave-developer-mode leaves developer mode as it reads;
                              prints no report and exits with why the tools did not
                              finish (`aterm init`'s root half)
    here apply --plain --name-rule-only
                              refresh only the console account's naming permission;
                              accepts no machine name and changes no OS name
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
    "storage",
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
#[must_use = "handle or propagate the command's exit status"]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    // Local read-only storage has no controller or legacy-agent setup. Preserve
    // pathname bytes and dispatch before any global live/controller actions.
    if let Some(route) = storage_route(&args) {
        return match route {
            StorageRoute::Run(words) => storage_exit(aterm_repo_storage::cli::main_entry(words)),
            StorageRoute::Help => {
                storage_exit(aterm_repo_storage::cli::main_entry(&["--help".into()]))
            }
            StorageRoute::Controller => refuse("storage is local and does not take --controller"),
        };
    }
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
        "storage" => refuse("storage is local and does not take --controller"),
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

#[derive(Debug, PartialEq, Eq)]
enum StorageRoute<'a> {
    Run(&'a [OsString]),
    Help,
    Controller,
}

fn storage_route(args: &[OsString]) -> Option<StorageRoute<'_>> {
    if args.first().is_some_and(|word| word == "storage") {
        Some(StorageRoute::Run(&args[1..]))
    } else if args.len() == 2 && args[0] == "help" && args[1] == "storage" {
        Some(StorageRoute::Help)
    } else if args.first().is_some_and(|word| word == "--controller")
        && args.get(2).is_some_and(|word| word == "storage")
    {
        Some(StorageRoute::Controller)
    } else {
        None
    }
}

fn storage_exit(code: i32) -> ExitCode {
    match code {
        0 => ExitCode::SUCCESS,
        2 => ExitCode::from(2),
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
#[path = "cli/storage_tests.rs"]
mod storage_tests;

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
                        .filter(|n| !v.starts_with('+') && (1..=3600).contains(n))
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
                        .filter(|n| !v.starts_with('+') && (1..=64).contains(n))
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
    let plain = args.iter().any(|a| a == "--plain");
    let steps_only = args.iter().any(|a| a == "--root-steps-only");
    let name_rule_only = args.iter().any(|a| a == "--name-rule-only");
    if steps_only && !plain {
        return refuse("here: --root-steps-only requires --plain");
    }
    if name_rule_only && (!plain || steps_only) {
        return refuse("here: --name-rule-only requires --plain and no --root-steps-only");
    }
    if json && plain {
        return refuse("here: choose either --json or --plain");
    }
    let output = if json {
        commands::HereOutput::Json
    } else if plain {
        commands::HereOutput::Plain
    } else {
        commands::HereOutput::Report
    };
    // Typed number and optional nickname ride the root half alone. Root derives the
    // targets without reading any account-writable configuration. So does
    // `--leave-developer-mode`, which only narrows what it does: developer mode is left as it
    // reads.
    let mut name = None;
    let mut nickname = None;
    let mut leave_developer_mode = false;
    let mut rest: Vec<&String> = Vec::new();
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        match arg.as_str() {
            "--json" | "--plain" | "--root-steps-only" | "--name-rule-only" => {}
            crate::init_clt::LEAVE_DEVELOPER_MODE => leave_developer_mode = true,
            "--name" => match words
                .next()
                .map(String::as_str)
                .and_then(crate::init_clt::plain_name)
            {
                Some(label) if name.is_none() => name = Some(label),
                Some(_) => return refuse("here: --name is given once"),
                None => {
                    return refuse("here: --name takes a machine label: m and a number (m42)");
                }
            },
            "--nickname" => {
                if nickname.is_some() {
                    return refuse("here: --nickname is given once");
                }
                let Some(value) = words.next() else {
                    return refuse("here: --nickname needs a value");
                };
                nickname = match aterm_types::machine_name::Nickname::parse(value) {
                    Ok(nickname) => Some(nickname),
                    Err(_) => return refuse("here: --nickname takes a valid machine nickname"),
                };
            }
            _ => rest.push(arg),
        }
    }
    if name.is_some() && !plain {
        return refuse("here: --name rides --plain alone");
    }
    if leave_developer_mode && !plain {
        return refuse("here: --leave-developer-mode rides --plain alone");
    }
    if name_rule_only && (name.is_some() || nickname.is_some()) {
        return refuse("here: --name-rule-only accepts no name or nickname");
    }
    if nickname.is_some() && name.is_none() {
        return refuse("here: --nickname requires --name");
    }
    let name = name.map(|number| {
        let number = aterm_types::machine_name::Number::parse(&number)
            .expect("a validated canonical machine number");
        nickname.as_ref().map_or_else(
            || aterm_types::machine_name::GeneratedNames::number_only(number),
            |nickname| aterm_types::machine_name::GeneratedNames::new(number, nickname),
        )
    });
    if let Some(flag) = rest.iter().find(|a| a.starts_with('-')) {
        return refuse(&format!("here: unknown option {flag}"));
    }
    let mode = match rest.first().map(|s| s.as_str()) {
        Some("plan") => Mode::Plan,
        Some("apply") => Mode::Apply,
        _ => return refuse("here: takes plan|apply <baseline.toml>"),
    };
    // `--plain` applies only compiled root work. The explicit naming tab first waits
    // for its admitted landing, then invokes the same fixed endpoint as root.
    if plain {
        if name_rule_only
            && mode == Mode::Apply
            && rest.len() == 1
            && sys.euid() != 0
            && std::env::var_os(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB)
                .is_some_and(|value| !value.is_empty())
        {
            return ExitCode::from(crate::init_clt::name_rule_tab_entry(sys));
        }
        return match rest.as_slice() {
            [_] if mode == Mode::Apply => commands::here(
                sys,
                mode,
                commands::HereBaseline::BuiltIn(if name_rule_only {
                    crate::init_clt::RootPlan::NameRuleOnly
                } else if steps_only {
                    crate::init_clt::RootPlan::StepsOnly
                } else {
                    crate::init_clt::RootPlan::WithTools
                }),
                commands::Plain {
                    name: name.as_ref(),
                    leave_developer_mode,
                },
                output,
            ),
            _ => refuse("here: --plain takes apply alone, with no baseline file"),
        };
    }
    let [_, path] = rest.as_slice() else {
        return refuse("here: takes plan|apply <baseline.toml>");
    };
    commands::here(
        sys,
        mode,
        commands::HereBaseline::File(Path::new(path.as_str())),
        commands::Plain::default(),
        output,
    )
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
            "--wait" => match it.next().and_then(|s| crate::parse_uint::<u64>(s)) {
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
#[path = "cli_tests.rs"]
mod tests;
