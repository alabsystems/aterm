// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only standalone executable for `aterm-machines` (`machines`, `enroll-through`, `seal`,
//! `relay`, and `screen-headless`).

#[cfg(unix)]
use aterm_machines::controller::enroll::{self, Master, Request, Trust};
#[cfg(unix)]
use aterm_machines::controller::inventory::Paths;
#[cfg(unix)]
use aterm_machines::port::{Live, System};
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("aterm-machines-dev: unsupported here (aterm machines manages Macs, from a Mac)");
    ExitCode::from(2)
}

#[cfg(unix)]
fn main() -> ExitCode {
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if Path::new(&argv0)
        .file_name()
        .is_some_and(|n| n == aterm_machines::askpass::ALIAS)
    {
        let rest: Vec<OsString> = std::env::args_os().skip(1).collect();
        return aterm_machines::askpass::main(&rest);
    }
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let words: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.first().is_some_and(|a| a == "enroll-through") {
        return enroll_through(&words);
    }
    if args.first().is_some_and(|a| a == "seal") {
        return seal(&words);
    }
    if args.first().is_some_and(|a| a == "relay") {
        return relay(&words);
    }
    if args.first().is_some_and(|a| a == "screen-headless") {
        return screen_headless(&words);
    }
    if args.first().is_some_and(|a| a == "machines") {
        args.remove(0);
    }
    aterm_machines::cli::main_entry(args)
}

/// Sign and write an envelope to standard output.
#[cfg(unix)]
fn seal(words: &[String]) -> ExitCode {
    use aterm_machines::envelope::{self, Action, ArtifactRef, Disrupt, Envelope, Exec, Upgrade};
    let mut get = std::collections::BTreeMap::new();
    let mut it = words.iter();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        get.insert(k.trim_start_matches('-').to_owned(), v.clone());
    }
    let sys = Live::new();
    let now = sys.now();
    let num = |k: &str, d: u64| get.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let action = match get.get("action").map(String::as_str) {
        Some("plan") => Action::Plan,
        Some("apply") => Action::Apply,
        Some("facts") => Action::Facts,
        Some("exec") => Action::Exec,
        Some("uninstall") => Action::Uninstall,
        Some("upgrade") => Action::Upgrade,
        other => {
            eprintln!("seal: --action plan|apply|facts|exec|upgrade|uninstall, not {other:?}");
            return ExitCode::from(2);
        }
    };
    let baseline = match get.get("baseline") {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("seal: {p}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let e = Envelope {
        schema: aterm_machines::SCHEMA,
        host: get.get("host").cloned().unwrap_or_default(),
        host_name: "dev".into(),
        serial: num("serial", 1),
        issued: num("issued", now),
        expires: num("expires", now + 86_400),
        action,
        disrupt: Disrupt::Now,
        idle_secs: 900,
        baseline,
        artifacts: get
            .get("artifact")
            .map(|sha| {
                vec![ArtifactRef {
                    sha256: sha.clone(),
                    name: "dev artifact".into(),
                }]
            })
            .unwrap_or_default(),
        exec: get.get("exec").map(|p| Exec {
            argv: p.split(' ').map(str::to_owned).collect(),
            user: None,
            budget_secs: 60,
        }),
        upgrade: get.get("upgrade").map(|sha| Upgrade {
            sha256: sha.clone(),
            kind: "dev-zip".into(),
            version: get.get("version").cloned().unwrap_or_default(),
        }),
        rekey: None,
    };
    let Some(key) = get.get("key") else {
        eprintln!("seal: --key <signing key> is required");
        return ExitCode::from(2);
    };
    match envelope::seal(&sys, &e, std::path::Path::new(key)) {
        Ok(wire) => {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(&wire);
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("seal: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn relay(words: &[String]) -> ExitCode {
    use aterm_machines::controller::screen;
    use aterm_machines::controller::ssh::{self, Target};
    use std::io::{Read as _, Write as _};
    let mut get = std::collections::BTreeMap::new();
    let mut it = words.iter();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        get.insert(k.trim_start_matches('-').to_owned(), v.clone());
    }
    let cmd = if let Some(master) = get.get("master") {
        let (Some(destination), Some(remote)) = (get.get("destination"), get.get("remote")) else {
            eprintln!("relay --master <socket> --destination <user@address> --remote '<command>'");
            return ExitCode::from(2);
        };
        ssh::ssh_cmd(
            [
                "-T",
                "-F",
                "/dev/null",
                "-o",
                "ControlMaster=no",
                "-o",
                &format!("ControlPath={master}"),
                "-o",
                "BatchMode=yes",
                "-o",
                "LogLevel=ERROR",
                "--",
                destination,
                remote,
            ]
            .map(OsString::from)
            .into(),
        )
    } else {
        let (Some(controller), Some(address), Some(user), Some(alias)) = (
            get.get("controller"),
            get.get("address"),
            get.get("user"),
            get.get("alias"),
        ) else {
            eprintln!(
                "relay --controller <dir> --address <a> --user <u> --alias <uuid> | relay \
                 --master <socket> --destination <user@address> --remote '<command>'"
            );
            return ExitCode::from(2);
        };
        let paths = Paths::at(Path::new(controller));
        let target = Target {
            user: user.clone(),
            address: address.clone(),
            alias: Some(alias.clone()),
        };
        match screen::relay_cmd(&paths, &target) {
            Ok(cmd) => cmd,
            Err(why) => {
                eprintln!("relay: {why}");
                return ExitCode::from(2);
            }
        }
    };
    let sys = Live::new();
    let mut relay = match sys.relay(&cmd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("relay: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("relay 127.0.0.1:{}", relay.port());
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    match relay.close() {
        Ok(()) => {
            println!("closed");
            ExitCode::SUCCESS
        }
        Err(why) => {
            println!("closed: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Headless VNC opener that prints the `vnc://` URL to standard output.
#[cfg(unix)]
struct PrintOpener;

#[cfg(unix)]
impl aterm_machines::controller::screen::Opener for PrintOpener {
    fn open(&self, url: &str) -> Result<(), String> {
        use std::io::Write as _;
        println!("screen-headless: open {url}");
        let _ = std::io::stdout().flush();
        Ok(())
    }
}

/// `screen-headless <host> [<step>] --controller <dir>`: `aterm machines screen` with
/// [`PrintOpener`] in place of the live opener.
#[cfg(unix)]
fn screen_headless(words: &[String]) -> ExitCode {
    use aterm_machines::controller::commands::{self, Env};
    use aterm_machines::controller::screen::Terminals;
    use std::io::IsTerminal as _;
    let mut controller = None;
    let mut positional: Vec<String> = Vec::new();
    let mut it = words.iter();
    while let Some(w) = it.next() {
        match w.as_str() {
            "--controller" => controller = it.next().cloned(),
            other if other.starts_with("--") => {
                eprintln!("screen-headless: unknown argument {other:?}");
                return ExitCode::from(2);
            }
            other => positional.push(other.to_owned()),
        }
    }
    let (Some(controller), Some(host)) = (controller, positional.first()) else {
        eprintln!("screen-headless <host> [<step>] --controller <dir>");
        return ExitCode::from(2);
    };
    let step = positional.get(1).map(String::as_str);
    let sys = Live::new();
    let sysref: &dyn System = &sys;
    let terminals = Terminals {
        stdin: std::io::stdin().is_terminal(),
        stderr: std::io::stderr().is_terminal(),
    };
    let env = Env {
        sys: sysref,
        paths: Paths::at(Path::new(&controller)),
        json: false,
        tty: terminals.stdin && terminals.stderr,
        parallel: None,
        agent_sock: std::env::var_os("SSH_AUTH_SOCK"),
    };
    commands::walk(&env, host, false, step, terminals, &PrintOpener)
}

#[cfg(unix)]
fn enroll_through(words: &[String]) -> ExitCode {
    let mut master = None;
    let mut address = None;
    let mut user = None;
    let mut fingerprint = None;
    let mut controller = None;
    let mut name = None;
    let mut it = words.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--master" => &mut master,
            "--address" => &mut address,
            "--user" => &mut user,
            "--fingerprint" => &mut fingerprint,
            "--controller" => &mut controller,
            "--name" => &mut name,
            other => {
                eprintln!("enroll-through: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        };
        *slot = it.next().cloned();
    }
    let (Some(master), Some(address), Some(user), Some(fingerprint), Some(controller)) =
        (master, address, user, fingerprint, controller)
    else {
        eprintln!(
            "enroll-through --master <socket> --address <a> --user <u> --fingerprint <fp> \
             --controller <dir> [--name <n>]"
        );
        return ExitCode::from(2);
    };
    let sys = Live::new();
    let paths = Paths::at(Path::new(&controller));
    let req = Request {
        address: address.clone(),
        user: user.clone(),
        name,
        trust: Trust::Fingerprint(fingerprint),
    };
    let master = Master {
        ctl: PathBuf::from(master),
        destination: format!("{user}@{address}"),
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("enroll-through: {e}");
            return ExitCode::FAILURE;
        }
    };
    let scratch = match aterm_machines::port::Scratch::new(&sys, "dev-enroll") {
        Ok(scratch) => scratch,
        Err(e) => {
            eprintln!("enroll-through scratch: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut out = std::io::stdout().lock();
    let done = enroll::host_key_through(&sys, &master, scratch.path()).and_then(|key| {
        enroll::check_host_key(&key, &req.trust)?;
        enroll::enroll_through(
            &sys,
            &paths,
            &req,
            &exe,
            &master,
            &key,
            scratch.path(),
            &mut out,
        )
    });
    match done {
        Ok(host) => {
            println!("enrolled {} ({})", host.name, host.uuid);
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("enroll-through: {why}");
            ExitCode::FAILURE
        }
    }
}
