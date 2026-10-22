// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets`: the verb (docs/DESIGN-secrets-2026-10-20.md). Arguments are parsed
//! before any probe, prompt or network call; nothing printed is ever a value except what
//! `get` was asked for.

use std::ffi::OsString;
use std::process::ExitCode;

/// The verb's help.
pub const USAGE: &str = "\
aterm secrets: one credentials file for every machine (aterm help secrets).

  aterm secrets [status]                 where this machine's vault is, what it holds, and
                                         (an admin) whether the approver runs
  aterm secrets join [<url>|<path>]      join a vault (Enter-equivalent: the ALab default)
        [--name <host>] [--password-stdin] [--no-agent] [--no-wait]
  aterm secrets sync                     fetch and apply the latest version now
  aterm secrets watch                    the subscriber: apply every new version (launchd runs it)
  aterm secrets ls                       the names this machine holds
  aterm secrets get <name>               print one value to stdout
admin (a machine in [admins]):
  aterm secrets create <url>|<path> [--import <dir>] [--name <host>]
        [--no-agent]                     (for a <url>, installs the subscriber too: its
                                         approver lets in a machine that types the join
                                         password)
  aterm secrets edit                     the vault text in $EDITOR; saving publishes it
  aterm secrets set <name> [--for <host|code>...] [--file <path>]
        [--if-absent [--with <name> | --no-with]] [--fleet-reads-anchors]
                                         (the value from stdin or the file; --with: filled
                                         only where that if-absent entry is not the machine's own;
                                         set again, an entry keeps its anchor unless --no-with)
  aterm secrets unset <name> [--for <host|code>...]
  aterm secrets password [show|set|new|open|off] [--no-agent]
                                                   the join password a joining machine
                                         types, or open: every machine that asks is let in
                                         (for a <url>, by this Mac's subscriber, which set,
                                         new and open install where it does not run; a
                                         <path> has no join queue: approve each key)
  aterm secrets requests                 machines asking to join
  aterm secrets approve <code>... | --all | <age1...> --name <host>
  aterm secrets revoke <host|code> [--drop-sections]
  aterm secrets admins add|remove <host|code>   (remove also rotates the vault key)
  aterm secrets rotate-key               sign with a new vault key the current one endorses
server and services:
  aterm secrets serve [--listen <addr:port>] [--advertise <name> | --no-advertise]
        [--pin <vault>=<key>] [--store <dir>] [--hold <1-60 secs>] [--no-keep-awake]
                                         (on macOS, listening on every address at port 7484
                                         as it does by default, it advertises the default
                                         vault's name)
  aterm secrets agent on|off|status [--program <path>]
  aterm secrets server on|off|status [--program <path>] [serve flags]
";

/// Run `aterm secrets`.
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let mut words = Vec::with_capacity(args.len());
    for arg in args {
        match arg.into_string() {
            Ok(word) => words.push(word),
            Err(_) => {
                eprintln!("aterm secrets: arguments must be UTF-8\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    if words.iter().any(|w| w == "--help" || w == "-h")
        || words.first().is_some_and(|w| w == "help")
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let command = match parse(&words) {
        Ok(command) => command,
        Err(why) => {
            eprintln!("aterm secrets: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    #[cfg(unix)]
    {
        match unix::run(command) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("aterm secrets: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        eprintln!("aterm secrets: available on macOS and Linux only");
        ExitCode::from(2)
    }
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Status,
    Join {
        source: String,
        name: Option<String>,
        agent: bool,
        wait: bool,
        password_stdin: bool,
    },
    Sync,
    Watch,
    Ls,
    Get(String),
    Create {
        source: String,
        import: Option<String>,
        name: Option<String>,
        agent: bool,
    },
    Edit,
    Set {
        name: String,
        hosts: Vec<String>,
        file: Option<String>,
        fill: Fill,
    },
    Unset {
        name: String,
        hosts: Vec<String>,
    },
    Password {
        action: Password,
        agent: bool,
    },
    Requests,
    Approve {
        which: Vec<String>,
        all: bool,
        name: Option<String>,
    },
    Revoke {
        host: String,
        drop_sections: bool,
    },
    Admins {
        add: bool,
        host: String,
    },
    RotateKey,
    Serve(Vec<String>),
    Agent {
        action: Action,
        program: Option<String>,
    },
    Server {
        action: Action,
        program: Option<String>,
        serve: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    On,
    Off,
    Status,
}

/// What `password` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Password {
    Show,
    Set,
    New,
    Open,
    Off,
}

/// How `set` fills an entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Fill {
    /// `--if-absent`: written only where no such file exists.
    if_absent: bool,
    /// `--with <anchor>`: filled only where that entry is not the machine's own.
    with: Option<String>,
    /// `--no-with`: drop the anchor a re-set would otherwise keep.
    no_with: bool,
    /// `--fleet-reads-anchors`: every machine and admin runs an aterm that reads anchors, so
    /// the vault's first anchored entry may be published.
    fleet_reads_anchors: bool,
}

/// Pull `--flag <value>` pairs and bare switches out of `args`; the rest are operands.
struct Flags {
    operands: Vec<String>,
    values: Vec<(String, String)>,
    switches: Vec<String>,
}

fn flags(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Flags, String> {
    let mut out = Flags {
        operands: Vec::new(),
        values: Vec::new(),
        switches: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if valued.contains(&arg.as_str()) {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("{arg} needs a value"))?;
            out.values.push((arg.clone(), value.clone()));
            i += 2;
            continue;
        }
        if switches.contains(&arg.as_str()) {
            out.switches.push(arg.clone());
        } else if arg.starts_with("--") {
            return Err(format!("unknown option {arg}"));
        } else {
            out.operands.push(arg.clone());
        }
        i += 1;
    }
    Ok(out)
}

impl Flags {
    fn one(&self, flag: &str) -> Option<String> {
        self.values
            .iter()
            .rev()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
    }

    fn all(&self, flag: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(f, _)| f == flag)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn has(&self, switch: &str) -> bool {
        self.switches.iter().any(|s| s == switch)
    }
}

/// The flags `serve` takes (also passed through by `server on`).
const SERVE_VALUED: &[&str] = &["--listen", "--advertise", "--pin", "--store", "--hold"];
const SERVE_SWITCHES: &[&str] = &["--no-keep-awake", "--no-advertise"];

/// The longest `--hold`: a watching machine's request gives up a little after a minute.
const MAX_HOLD_SECS: u64 = 60;

/// What `serve` was asked for, checked when the command line is parsed, so `server on`
/// installs only a command line `serve` accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeSettings {
    listen: std::net::SocketAddr,
    advertise: Option<String>,
    pins: Vec<(String, [u8; 32])>,
    store: Option<String>,
    /// `None`: the server's default.
    hold: Option<std::time::Duration>,
    keep_awake: bool,
}

fn serve_settings(f: &Flags) -> Result<ServeSettings, String> {
    let listen = f
        .one("--listen")
        .unwrap_or_else(|| format!("0.0.0.0:{}", crate::DEFAULT_PORT));
    let listen: std::net::SocketAddr = listen
        .parse()
        .map_err(|_| format!("--listen {listen}: not an address:port"))?;
    let mut pins: Vec<(String, [u8; 32])> = Vec::new();
    for pin in f.all("--pin") {
        let (name, key) = pin
            .split_once('=')
            .ok_or("--pin takes <vault>=<base64 key>")?;
        if !crate::envelope::valid_name(name) {
            return Err(format!("--pin {name}: not a vault name"));
        }
        let key = crate::b64::decode_padded(key)
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
            .ok_or_else(|| format!("--pin {name}: not a 32-byte base64 key"))?;
        if pins.iter().any(|(pinned, _)| pinned == name) {
            return Err(format!("--pin {name} is given twice"));
        }
        pins.push((name.to_string(), key));
    }
    let hold = match f.one("--hold") {
        Some(secs) => Some(std::time::Duration::from_secs(
            secs.parse::<u64>()
                .ok()
                .filter(|n| (1..=MAX_HOLD_SECS).contains(n))
                .ok_or_else(|| format!("--hold {secs}: seconds from 1 to {MAX_HOLD_SECS}"))?,
        )),
        None => None,
    };
    let advertise = f.one("--advertise");
    if let Some(name) = &advertise
        && !crate::text::valid_machine_name(name)
    {
        return Err(format!(
            "--advertise {name}: a host label of lowercase letters, digits and -"
        ));
    }
    // On macOS the default vault's name, so a server set up with no flags answers the URL
    // every new Mac joins, but only where it can: on every address at the default URL's port.
    // A server hand-run on loopback or another port (a check, a test) claims no name. Two
    // Macs advertising one name contend for it: one serving another vault names its own, or
    // none.
    let answers_default_url = listen.ip().is_unspecified() && listen.port() == crate::DEFAULT_PORT;
    let advertise = match (advertise, f.has("--no-advertise")) {
        (Some(_), true) => return Err("--advertise and --no-advertise are both given".into()),
        (None, true) => None,
        (Some(name), false) => Some(name),
        (None, false) => (cfg!(target_os = "macos") && answers_default_url)
            .then(|| crate::alab_advertised().to_string()),
    };
    let store = f.one("--store");
    Ok(ServeSettings {
        listen,
        advertise,
        pins,
        store,
        hold,
        keep_awake: !f.has("--no-keep-awake"),
    })
}

fn no_operands(f: &Flags, what: &str) -> Result<(), String> {
    if f.operands.is_empty() {
        Ok(())
    } else {
        Err(format!("{what} takes no operand {:?}", f.operands[0]))
    }
}

fn action(word: Option<&String>, what: &str) -> Result<Action, String> {
    match word.map(String::as_str) {
        Some("on") => Ok(Action::On),
        Some("off") => Ok(Action::Off),
        Some("status") | None => Ok(Action::Status),
        Some(other) => Err(format!("{what} takes on, off or status, not {other:?}")),
    }
}

fn parse(words: &[String]) -> Result<Command, String> {
    let Some((verb, rest)) = words.split_first() else {
        return Ok(Command::Status);
    };
    match verb.as_str() {
        "status" => {
            no_operands(&flags(rest, &[], &[])?, "status")?;
            Ok(Command::Status)
        }
        "join" => {
            let f = flags(
                rest,
                &["--name"],
                &["--no-agent", "--no-wait", "--password-stdin"],
            )?;
            if f.operands.len() > 1 {
                return Err("join takes one URL or path".into());
            }
            Ok(Command::Join {
                source: f.operands.first().cloned().unwrap_or_default(),
                name: f.one("--name"),
                agent: !f.has("--no-agent"),
                wait: !f.has("--no-wait"),
                password_stdin: f.has("--password-stdin"),
            })
        }
        "sync" => {
            no_operands(&flags(rest, &[], &[])?, "sync")?;
            Ok(Command::Sync)
        }
        "watch" => {
            no_operands(&flags(rest, &[], &[])?, "watch")?;
            Ok(Command::Watch)
        }
        "ls" => {
            no_operands(&flags(rest, &[], &[])?, "ls")?;
            Ok(Command::Ls)
        }
        "get" => {
            let f = flags(rest, &[], &[])?;
            match f.operands.as_slice() {
                [name] => Ok(Command::Get(name.clone())),
                _ => Err("get takes one name".into()),
            }
        }
        "create" => {
            let f = flags(rest, &["--import", "--name"], &["--no-agent"])?;
            match f.operands.as_slice() {
                [source] => Ok(Command::Create {
                    source: source.clone(),
                    import: f.one("--import"),
                    name: f.one("--name"),
                    agent: !f.has("--no-agent"),
                }),
                _ => Err("create takes one URL or path".into()),
            }
        }
        "edit" => {
            no_operands(&flags(rest, &[], &[])?, "edit")?;
            Ok(Command::Edit)
        }
        "set" => {
            let f = flags(
                rest,
                &["--for", "--file", "--with"],
                &["--if-absent", "--no-with", "--fleet-reads-anchors"],
            )?;
            match f.operands.as_slice() {
                [name, more @ ..] => {
                    let mut hosts = f.all("--for");
                    if !more.is_empty() {
                        return Err(format!(
                            "set takes the value from stdin or --file, never the command line (got {} extra operand(s))",
                            more.len()
                        ));
                    }
                    hosts.sort();
                    hosts.dedup();
                    let if_absent = f.has("--if-absent");
                    let with = f.one("--with");
                    let no_with = f.has("--no-with");
                    if let Some(anchor) = &with {
                        if !if_absent {
                            return Err(
                                "--with <name> goes with --if-absent: it names the entry this one fills a gap beside".into(),
                            );
                        }
                        if !crate::text::valid_entry_name(anchor) || anchor == name {
                            return Err(
                                "--with takes another entry's name (letters, digits, . _ -; or ~/a/b)".into(),
                            );
                        }
                        if no_with {
                            return Err("--with and --no-with: pick one".into());
                        }
                    }
                    if no_with && !if_absent {
                        return Err(
                            "--no-with goes with --if-absent: it drops the anchor an if-absent entry has".into(),
                        );
                    }
                    Ok(Command::Set {
                        name: name.clone(),
                        hosts,
                        file: f.one("--file"),
                        fill: Fill {
                            if_absent,
                            with,
                            no_with,
                            fleet_reads_anchors: f.has("--fleet-reads-anchors"),
                        },
                    })
                }
                [] => Err("set takes a name".into()),
            }
        }
        "unset" => {
            let f = flags(rest, &["--for"], &[])?;
            match f.operands.as_slice() {
                [name] => {
                    let mut hosts = f.all("--for");
                    hosts.sort();
                    hosts.dedup();
                    Ok(Command::Unset {
                        name: name.clone(),
                        hosts,
                    })
                }
                _ => Err("unset takes one name".into()),
            }
        }
        "password" => {
            let f = flags(rest, &[], &["--no-agent"])?;
            let action = match f.operands.as_slice() {
                [] => Password::Show,
                [word] => match word.as_str() {
                    "show" => Password::Show,
                    "set" => Password::Set,
                    "new" => Password::New,
                    "open" => Password::Open,
                    "off" => Password::Off,
                    _ => return Err("password takes show, set, new, open or off".into()),
                },
                _ => return Err("password takes show, set, new, open or off".into()),
            };
            Ok(Command::Password {
                action,
                agent: !f.has("--no-agent"),
            })
        }
        "requests" => {
            no_operands(&flags(rest, &[], &[])?, "requests")?;
            Ok(Command::Requests)
        }
        "approve" => {
            let f = flags(rest, &["--name"], &["--all"])?;
            let all = f.has("--all");
            let name = f.one("--name");
            match (f.operands.as_slice(), all, &name) {
                ([], false, _) => {
                    return Err("approve takes join codes (aterm secrets requests), --all, or an age1… key with --name".into());
                }
                ([_, ..], true, _) => return Err("approve --all takes no codes".into()),
                ([key], false, Some(_)) if key.starts_with("age1") => {}
                (_, _, Some(_)) => return Err("--name goes with one age1… key".into()),
                ([key], false, None) if key.starts_with("age1") => {
                    return Err("approving a key needs --name <host>".into());
                }
                _ => {}
            }
            // Never admitted under a generic name in its place: the machine was told this one.
            // Nor one shaped like a join code, which names the machine whose key has it.
            if let Some(name) = &name
                && (!crate::text::valid_machine_name(name) || crate::transport::valid_code(name))
            {
                return Err(format!(
                    "--name {name:?}: a machine name is lowercase letters, digits and - (at most 63), not machines, admins, everyone or join, and not shaped like a join code"
                ));
            }
            let mut which = Vec::with_capacity(f.operands.len());
            for word in &f.operands {
                if word.starts_with("age1") {
                    which.push(word.clone());
                    continue;
                }
                let code = word.to_ascii_lowercase();
                if !crate::transport::valid_code(&code) {
                    return Err(format!(
                        "{word:?} is not a join code (xxxx-xxxx-xxxx-xxxx): approve by the code the machine shows, never by a host name anyone can ask for"
                    ));
                }
                which.push(code);
            }
            Ok(Command::Approve { which, all, name })
        }
        "revoke" => {
            let f = flags(rest, &[], &["--drop-sections"])?;
            match f.operands.as_slice() {
                [host] => Ok(Command::Revoke {
                    host: host.clone(),
                    drop_sections: f.has("--drop-sections"),
                }),
                _ => Err("revoke takes one host".into()),
            }
        }
        "admins" => {
            let f = flags(rest, &[], &[])?;
            match f.operands.as_slice() {
                [verb, host] if verb == "add" || verb == "remove" => Ok(Command::Admins {
                    add: verb == "add",
                    host: host.clone(),
                }),
                _ => Err("admins takes add <host|code> or remove <host|code>".into()),
            }
        }
        "rotate-key" => {
            no_operands(&flags(rest, &[], &[])?, "rotate-key")?;
            Ok(Command::RotateKey)
        }
        "serve" => {
            let f = flags(rest, SERVE_VALUED, SERVE_SWITCHES)?;
            no_operands(&f, "serve")?;
            serve_settings(&f)?;
            Ok(Command::Serve(rest.to_vec()))
        }
        "agent" => {
            let f = flags(rest, &["--program"], &[])?;
            if f.operands.len() > 1 {
                return Err("agent takes on, off or status".into());
            }
            Ok(Command::Agent {
                action: action(f.operands.first(), "agent")?,
                program: f.one("--program"),
            })
        }
        "server" => {
            let (word, rest) = match rest.split_first() {
                Some((word, rest)) if !word.starts_with("--") => (Some(word), rest),
                _ => (None, rest),
            };
            let mut valued = SERVE_VALUED.to_vec();
            valued.push("--program");
            let f = flags(rest, &valued, SERVE_SWITCHES)?;
            no_operands(&f, "server")?;
            serve_settings(&f)?;
            let mut serve = Vec::new();
            for (flag, value) in &f.values {
                if flag != "--program" {
                    serve.push(flag.clone());
                    serve.push(value.clone());
                }
            }
            serve.extend(f.switches.iter().cloned());
            Ok(Command::Server {
                action: action(word, "server")?,
                program: f.one("--program"),
                serve,
            })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

#[cfg(unix)]
mod unix {
    use super::{
        Action, Command, Fill, Password, SERVE_SWITCHES, SERVE_VALUED, flags, serve_settings,
    };
    use crate::client::{
        self, Approval, JoinOptions, JoinOutcome, Membership, Paths, Published, SyncOutcome,
    };
    use crate::service::{self, Job};
    use crate::text::Audience;
    use crate::transport::Source;
    use crate::{Error, SecretBytes, envelope, fsx, join, text};
    use std::cell::Cell;
    use std::io::{IsTerminal, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::Duration;
    use zeroize::Zeroize;

    /// What an open vault means, said where one is made or shown.
    const OPEN_VAULT: &str = "this vault is open: every machine that reaches its server and asks to join is let in within seconds, with no password and no code, and reads [everyone]'s entries, except a machine you let go (aterm secrets password set closes it)";

    /// What a vault at a path says where one served at a URL says whom its `[join]` lets in: a
    /// path has no join queue, so nothing lets a machine in by itself there.
    const PATH_JOINS: &str = "a vault at a path has no join queue: a machine joins it only when an admin approves its key: aterm secrets approve <age1…> --name <host>";

    /// What a publish that let a machine go from an open vault says.
    const KEPT_OUT: &str = "the vault stays open, but not to a machine you let go: its key is a `code-only` line in [join], so whatever it asks, it is let back in only by its code (aterm secrets approve)";

    /// What marks a request whose key a `code-only` line names, where requests are listed.
    const CODE_ONLY_MARK: &str =
        "  (code only: a machine let go, or one that asked before the vault opened)";

    /// What opening a vault says of admins on older builds, which cannot read its lines.
    const OLDER_ADMINS: &str =
        "an admin Mac on an aterm older than open vaults cannot open this vault until it updates";

    pub(super) fn run(command: Command) -> crate::Result<ExitCode> {
        let paths = Paths::live()?;
        let state = || service::state(&paths.home, Job::Subscriber);
        let install = || install_subscriber(&paths, None);
        let subscriber = Subscriber {
            state: &state,
            install: &install,
        };
        match command {
            Command::Status => status(&paths, &subscriber),
            Command::Join {
                source,
                name,
                agent,
                wait,
                password_stdin,
            } => join(&paths, &source, name, agent, wait, password_stdin),
            Command::Sync => sync(&paths),
            Command::Watch => {
                client::watch(&paths, &|line| eprintln!("{} {line}", stamp()))?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Ls => {
                for name in client::status(&paths)?.names {
                    println!("{name}");
                }
                Ok(ExitCode::SUCCESS)
            }
            Command::Get(name) => get(&paths, &name),
            Command::Create {
                source,
                import,
                name,
                agent,
            } => create(&paths, &source, import, name, agent, &subscriber),
            Command::Edit => edit(&paths),
            Command::Set {
                name,
                hosts,
                file,
                fill,
            } => set(&paths, &name, &hosts, file, &fill),
            Command::Unset { name, hosts } => {
                let admin = client::admin_open(&paths)?;
                let mut doc = admin.doc.clone();
                let hosts = machines_named(&doc, &hosts)?;
                if !doc
                    .unset_entry(&audience(&hosts), &name)
                    .map_err(text_error)?
                {
                    return Err(Error::new(format!(
                        "no entry {name} in {}",
                        audience(&hosts).header()
                    )));
                }
                let done = client::admin_publish(&paths, &admin, &doc)?;
                Ok(published(&format!("removed {name}"), &done))
            }
            Command::Password { action, agent } => password(&paths, action, agent, &subscriber),
            Command::Requests => requests(&paths),
            Command::Approve { which, all, name } => approve(&paths, &which, all, name),
            Command::Revoke {
                host,
                drop_sections,
            } => revoke(&paths, &host, drop_sections),
            Command::Admins { add, host } => {
                let admin = client::admin_open(&paths)?;
                let mut doc = admin.doc.clone();
                let host = client::machine_named(&doc, &host)?;
                if add {
                    doc.add_admin(&host).map_err(text_error)?;
                    let done = client::admin_publish(&paths, &admin, &doc)?;
                    Ok(published(&format!("added {host} to the admins"), &done))
                } else {
                    let held = exposure(&doc, &host);
                    if !doc.remove_admin(&host).map_err(text_error)? {
                        return Err(Error::new(format!("{host} is not an admin")));
                    }
                    // Every admin holds the vault key: the one removed must not keep signing.
                    let done = client::admin_rotate(&paths, &admin, &doc)?;
                    let code = published(&format!("removed {host} from the admins"), &done);
                    if let Some(held) = held {
                        println!("{held}");
                    }
                    Ok(code)
                }
            }
            Command::RotateKey => {
                let admin = client::admin_open(&paths)?;
                let doc = admin.doc.clone();
                let done = client::admin_rotate(&paths, &admin, &doc)?;
                Ok(published(
                    "rotated the vault key (aterm secrets status shows the new fingerprint)",
                    &done,
                ))
            }
            Command::Serve(args) => serve(&paths, &args),
            Command::Agent { action, program } => services(
                &paths,
                Job::Subscriber,
                action,
                program,
                vec!["secrets".into(), "watch".into()],
            ),
            Command::Server {
                action,
                program,
                serve,
            } => {
                let mut args = vec!["secrets".to_string(), "serve".to_string()];
                args.extend(serve);
                services(&paths, Job::Server, action, program, args)
            }
        }
    }

    fn stamp() -> String {
        format!("[{}]", aterm_time::unix_secs().unwrap_or(0))
    }

    fn text_error(error: text::TextError) -> Error {
        Error::new(error.to_string())
    }

    /// The machines `words` name in `doc`, each by [`client::machine_named`]: a join code
    /// reaches the machine whose key has it, never one named like it; a name passes as typed.
    pub(super) fn machines_named(
        doc: &text::Document,
        words: &[String],
    ) -> crate::Result<Vec<String>> {
        words
            .iter()
            .map(|word| client::machine_named(doc, word))
            .collect()
    }

    fn audience(hosts: &[String]) -> Audience {
        if hosts.is_empty() {
            Audience::Everyone
        } else {
            Audience::Hosts(hosts.to_vec())
        }
    }

    fn interactive() -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    /// Say what a publish did: the version, a key rotation (for the ALab vault, what the next
    /// release must carry; for any vault, that its second key change leaves out the builds
    /// before handoff chains), and this machine's own sync failing (the publish stands either
    /// way). Exits 0: the change is out.
    fn published(what: &str, done: &Published) -> ExitCode {
        let rotated = if done.rotated {
            ", signed by a new vault key the old one endorsed"
        } else {
            ""
        };
        println!("{what}; published seq {}{rotated}", done.seq);
        if let Some(release) = &done.alab_release {
            print!("{}", release_note(release));
        }
        if done.first_long_chain {
            print!("{}", LONG_CHAIN_NOTE);
        }
        if done.kept_out {
            println!("{KEPT_OUT}");
        }
        if !done.held.is_empty() {
            println!(
                "{} machines asked before the vault opened, and join only by code (one may be a machine let go before); approve each you know with aterm secrets approve <code>:",
                done.held.len()
            );
            for request in &done.held {
                let when = request.at.map(ago).unwrap_or_default();
                println!("  {}  code {}  {when}", request.host, request.code);
            }
        }
        if done.password_renewed {
            println!(
                "the join password changed (a machine or admin that left may have known it): aterm secrets password shows the new one"
            );
        }
        if let Some(why) = &done.local {
            eprintln!(
                "warning: this Mac's own copy is not updated yet ({why}); aterm secrets sync retries"
            );
        }
        ExitCode::SUCCESS
    }

    /// The change a rotation of the ALab vault asks of aterm's next release, exactly as the
    /// two lines of crates/aterm-secrets/src/lib.rs read after it. The handoff chain lets a
    /// build that pins the old key follow the vault, but every admin held that key, so until
    /// a build pins the new one and retires the old, a new Mac also trusts what the admin
    /// removed signs with its copy.
    pub(super) fn release_note(release: &client::AlabRelease) -> String {
        let retired: Vec<String> = release
            .retired
            .iter()
            .map(|key| format!("\"{key}\""))
            .collect();
        format!(
            "the ALab vault has a new key. Until an aterm release carries it, a new Mac also trusts the old key, which every admin of the last version held (an admin removed too). For the next release, in crates/aterm-secrets/src/lib.rs:\n  pub const ALAB_SIGNER: &str = \"{}\";\n  pub const ALAB_RETIRED: &[&str] = &[{}];\n",
            release.signer,
            retired.join(", ")
        )
    }

    /// What a vault's first version with two handoff pairs costs: an aterm built before
    /// chains parses at most one pair, so it fails every sync of this vault (`expected
    /// locker: or signature:`) and, serving it, refuses the version (400), until it updates.
    const LONG_CHAIN_NOTE: &str = "this version carries a second key handoff: an aterm built before handoff chains reads at most one, so every Mac running one fails each sync of this vault, and a server running one refuses it, until it is updated\n";

    fn yes(answer: Option<String>) -> bool {
        answer.is_some_and(|a| a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes"))
    }

    /// One line from the terminal itself (never stdin), echo on.
    fn ask(prompt: &str) -> Option<String> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        tty.write_all(prompt.as_bytes()).ok()?;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while tty.read(&mut byte).ok()? == 1 {
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
            if line.len() > 4096 {
                return None;
            }
        }
        Some(String::from_utf8_lossy(&line).trim().to_string())
    }

    /// `stty <args>` on the terminal `tty`; its output, when it succeeds.
    fn stty(tty: &std::fs::File, args: &[&str]) -> Option<String> {
        let input = tty.try_clone().ok()?;
        let output = aterm_uds::spawnfd::command("/bin/stty")
            .args(args)
            .stdin(input)
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The terminal while a password is typed: no echo, no line editing, and no signals, so
    /// Ctrl-C arrives as a byte and the settings are put back before the process ends. The
    /// settings `stty -g` saved are restored on drop.
    struct Quiet {
        tty: std::fs::File,
        saved: String,
    }

    impl Quiet {
        fn new(tty: &std::fs::File) -> Option<Quiet> {
            let saved = stty(tty, &["-g"])?;
            if saved.is_empty() || !saved.bytes().all(|b| b.is_ascii_graphic()) {
                return None;
            }
            let quiet = Quiet {
                tty: tty.try_clone().ok()?,
                saved,
            };
            stty(tty, &["-echo", "-icanon", "-isig", "min", "1", "time", "0"])?;
            Some(quiet)
        }
    }

    impl Drop for Quiet {
        fn drop(&mut self) {
            let _ = stty(&self.tty, &[self.saved.as_str()]);
        }
    }

    /// The longest line a hidden prompt keeps (its buffer never grows, so no copy is left).
    const HIDDEN_MAX: usize = 1024;

    /// One line typed at the terminal without echo. `None` with no terminal; Ctrl-C ends
    /// the process (exit 130) once the terminal is put back; Ctrl-D on an empty line and
    /// an empty line answer an empty value.
    fn ask_hidden(prompt: &str) -> Option<SecretBytes> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        let quiet = Quiet::new(&tty)?;
        tty.write_all(prompt.as_bytes()).ok()?;
        let mut line: Vec<u8> = Vec::with_capacity(HIDDEN_MAX);
        let mut byte = [0u8; 1];
        let mut cancelled = false;
        while let Ok(1) = tty.read(&mut byte) {
            match byte[0] {
                b'\r' | b'\n' => break,
                0x03 => {
                    cancelled = true;
                    break;
                }
                0x04 if line.is_empty() => break,
                0x7f | 0x08 => {
                    if let Some(last) = line.last_mut() {
                        *last = 0;
                    }
                    line.pop();
                }
                0x15 => line.zeroize(),
                b if b < 0x20 => {}
                b if line.len() < HIDDEN_MAX => line.push(b),
                _ => {}
            }
        }
        byte.zeroize();
        drop(quiet);
        let _ = tty.write_all(b"\n");
        if cancelled {
            line.zeroize();
            std::process::exit(130);
        }
        Some(SecretBytes::new(line))
    }

    /// A password piped in for `--password-stdin` or `password set`: one line, blanks
    /// around it dropped.
    fn piped_password(what: &str) -> crate::Result<SecretBytes> {
        if std::io::stdin().is_terminal() {
            return Err(Error::new(format!("{what} reads the password from a pipe")));
        }
        let mut bytes = Vec::with_capacity(join::MAX_LEN + 2);
        crate::secret::read_to_end_wiping_on_error(
            std::io::stdin().take(join::MAX_LEN as u64 + 2),
            &mut bytes,
        )?;
        let trimmed = SecretBytes::from(bytes.trim_ascii());
        bytes.zeroize();
        if trimmed.as_slice().is_empty() {
            return Err(Error::new(format!("{what}: nothing was piped in")));
        }
        if trimmed.as_slice().contains(&b'\n') {
            return Err(Error::new(format!("{what}: the password is one line")));
        }
        Ok(trimmed)
    }

    fn ago(then: u64) -> String {
        let now = aterm_time::unix_secs().unwrap_or(then);
        let secs = now.saturating_sub(then);
        match secs {
            0..=119 => format!("{secs} s ago"),
            120..=7199 => format!("{} min ago", secs / 60),
            7200..=172_799 => format!("{} h ago", secs / 3600),
            _ => format!("{} days ago", secs / 86400),
        }
    }

    pub(super) fn source_text(source: &Source) -> String {
        match source {
            Source::Url(url) => crate::transport::describe(url),
            Source::Path(path) => path.display().to_string(),
        }
    }

    /// How a Mac waiting to join the vault at `source` fixes a mistyped join password: the
    /// join again, which asks for the password again and queues a request with the new proof.
    /// Without it a Mac whose proof matched nothing just waits, told only what an admin runs.
    /// `None` for a vault at a path, which has no join queue and so no password. The command
    /// names what a bare join would not ask for: a vault other than the ALab default, and the
    /// machine name `name` (`--name`), since the join again would otherwise ask for the ALab
    /// vault, or ask under this Mac's default name. A password that came `piped` is piped
    /// again: a join without a terminal that is not given it asks by code alone, a request
    /// that replaces the one with the proof.
    pub(super) fn retype(source: &Source, name: Option<&str>, piped: bool) -> Option<String> {
        let Source::Url(url) = source else {
            return None;
        };
        let mut join = String::from("aterm secrets join");
        if url != crate::ALAB_DEFAULT_URL {
            join.push(' ');
            join.push_str("<vault-url>");
        }
        if let Some(name) = name {
            join.push_str(" --name ");
            join.push_str(name);
        }
        let source_note = if url == crate::ALAB_DEFAULT_URL {
            ""
        } else {
            "; replace <vault-url> with this vault's full configured URL, including any query or signer pin"
        };
        Some(if piped {
            format!(
                "a mistyped join password is fixed by piping it again to {join} --password-stdin, or by running {join} in a terminal, which asks for it{source_note}"
            )
        } else {
            format!(
                "a mistyped join password is fixed by running {join} again (it asks for the password again){source_note}"
            )
        })
    }

    /// `join`'s last line for a Mac left waiting for an admin to approve `code`, with how a
    /// mistyped join password is fixed ([`retype`]) when one went with the request.
    pub(super) fn waiting_line(host: &str, code: &str, retype: Option<&str>) -> String {
        let line = format!(
            "waiting for approval of {host} (code {code}): on an admin Mac run aterm secrets approve {code}"
        );
        match retype {
            Some(retype) => format!("{line}; {retype}"),
            None => line,
        }
    }

    /// `status`'s membership line for a Mac waiting for an admin to approve `code`, with
    /// [`retype`] for a vault with a join queue.
    pub(super) fn pending_membership(code: &str, retype: Option<&str>) -> String {
        let line = format!(
            "membership: waiting for approval (on an admin Mac: aterm secrets approve {code})"
        );
        match retype {
            Some(retype) => format!("{line}; {retype}"),
            None => line,
        }
    }

    fn service_line(paths: &Paths, job: Job) -> String {
        service_words(&service::state(&paths.home, job))
    }

    fn service_words(state: &crate::Result<service::State>) -> String {
        match state {
            Ok(service::State::Absent) => "not installed".into(),
            Ok(service::State::Installed { pid: Some(pid), .. }) => format!("running (pid {pid})"),
            Ok(service::State::Installed { pid: None, .. }) => "installed, not running".into(),
            Err(error) => format!("unknown ({error})"),
        }
    }

    /// This Mac's subscriber, where an admin's approver runs (§9): read and installed through
    /// here, so a test plays it with an inert arm and never installs a launchd job or a
    /// systemd unit.
    pub(super) struct Subscriber<'a> {
        /// Its state now ([`service::state`]).
        pub(super) state: &'a dyn Fn() -> crate::Result<service::State>,
        /// Install and start it ([`install_subscriber`]): the line saying what was done.
        pub(super) install: &'a dyn Fn() -> crate::Result<String>,
    }

    /// What became of this Mac's approver where a command needs one to keep what it promises.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum Approver {
        /// The subscriber was running already, so it was left be.
        Running,
        /// The subscriber was installed now: the line saying so.
        Installed(String),
        /// `--no-agent`: nothing was installed.
        Skipped,
        /// The install failed: why.
        Failed(String),
        /// The subscriber's state could not be read: why. Nothing was installed.
        Unknown(String),
    }

    impl Approver {
        /// Whether an approver runs on this Mac now.
        pub(super) fn runs(&self) -> bool {
            matches!(self, Approver::Running | Approver::Installed(_))
        }
    }

    /// Put this Mac's approver in: install the subscriber unless it runs already. One that is
    /// installed with no process (a plist launchd never loaded, or a job that keeps ending)
    /// runs no approver, so it is installed again, as `aterm init`'s row judges it. A state
    /// that cannot be read installs nothing, since an install asks the same service manager.
    pub(super) fn put_in_approver(subscriber: &Subscriber<'_>) -> Approver {
        match (subscriber.state)() {
            Ok(service::State::Installed { pid: Some(_), .. }) => Approver::Running,
            Ok(_) => match (subscriber.install)() {
                Ok(line) => Approver::Installed(line),
                Err(error) => Approver::Failed(error.to_string()),
            },
            Err(error) => Approver::Unknown(error.to_string()),
        }
    }

    /// This Mac's approver for a command that needs one: put in where it does not run
    /// ([`put_in_approver`]), or with `--no-agent` (`agent` false) only read, so a subscriber
    /// already running (installed by an earlier join or command) is said to run, and nothing
    /// is installed.
    pub(super) fn approver_here(agent: bool, subscriber: &Subscriber<'_>) -> Approver {
        if agent {
            put_in_approver(subscriber)
        } else if matches!(
            (subscriber.state)(),
            Ok(service::State::Installed { pid: Some(_), .. })
        ) {
            Approver::Running
        } else {
            Approver::Skipped
        }
    }

    /// What `password` says of whom a vault whose `[join]` is `doc`'s lets in by itself: for
    /// one with a join queue (`queued`, a URL's), that an open one lets in every machine that
    /// asks (a password is the approver's line to say); for one at a path, which has none,
    /// that a machine joins it only by an admin's approval of its key, whatever `[join]` says.
    pub(super) fn lets_in_words(queued: bool, doc: &text::Document) -> Option<&'static str> {
        if !queued {
            return Approval::of(doc).map(|_| PATH_JOINS);
        }
        doc.open().then_some(OPEN_VAULT)
    }

    /// Who an approver for `approval` lets in.
    fn joiner(approval: Approval) -> &'static str {
        match approval {
            Approval::Password => "a machine that types the join password",
            Approval::Open => "a machine that asks to join",
        }
    }

    /// What a vault that approves by `approval` does where no approver runs on this Mac.
    fn waits(approval: Approval) -> String {
        format!(
            "{} waits for approval by code unless another admin Mac runs one: aterm secrets agent on starts one here",
            joiner(approval)
        )
    }

    /// What a command that needs this Mac's approver says of it, for a vault that approves
    /// by `approval`: the install line, nothing for one that ran already, else that none runs
    /// here and that `aterm secrets agent on` starts one.
    pub(super) fn approver_words(approver: &Approver, approval: Approval) -> Option<String> {
        let none = |why: &str| {
            format!(
                "no approver runs on this Mac ({why}), so {}",
                waits(approval)
            )
        };
        match approver {
            Approver::Running => None,
            Approver::Installed(line) => Some(line.clone()),
            Approver::Skipped => Some(none("--no-agent")),
            Approver::Failed(error) => Some(format!(
                "warning: {}",
                none(&format!("the subscriber was not installed: {error}"))
            )),
            Approver::Unknown(error) => Some(format!(
                "warning: whether this Mac's subscriber runs is unknown ({error}); if it does not, {}",
                waits(approval)
            )),
        }
    }

    /// `status`'s line for an admin of a vault that approves by `approval`: whether the
    /// approver runs, by its subscriber's state (`subscriber`).
    pub(super) fn approver_line(
        approval: Approval,
        subscriber: &crate::Result<service::State>,
    ) -> String {
        match subscriber {
            Ok(service::State::Installed { pid: Some(pid), .. }) => format!(
                "approver: running in the subscriber (pid {pid}), so {} is let in within seconds",
                joiner(approval)
            ),
            Ok(service::State::Installed { pid: None, .. }) => format!(
                "approver: not running (the subscriber is installed but not running), so {}",
                waits(approval)
            ),
            Ok(service::State::Absent) => format!(
                "approver: not running (no subscriber is installed), so {}",
                waits(approval)
            ),
            Err(error) => format!(
                "approver: unknown ({error}); if it does not run, {}",
                waits(approval)
            ),
        }
    }

    /// `status`'s line for this machine, `key` its `age1…` key and `code` its join code. A
    /// plain text file is read under the name `source.toml` holds. A signed vault calls a
    /// machine what its admission did, which is not the name it asked for when that was taken
    /// (it got a free one, which never reaches the machine): that name is said as asked, and
    /// the machine is named by its code, which `admins add` takes.
    pub(super) fn machine_line(config: Option<&client::Config>, key: &str, code: &str) -> String {
        let line = format!("machine: code {code}, key {key}");
        match config {
            Some(config) if config.signer.is_some() => {
                format!("{line}, asked to join as {}", config.host)
            }
            Some(config) => format!("machine: {}, code {code}, key {key}", config.host),
            None => line,
        }
    }

    fn status(paths: &Paths, subscriber: &Subscriber<'_>) -> crate::Result<ExitCode> {
        let status = client::status(paths)?;
        match &status.config {
            None => {
                println!(
                    "no vault configured: aterm secrets join [<url>|<path>] (Enter-equivalent: {})",
                    crate::ALAB_DEFAULT_URL
                );
            }
            Some(config) => {
                println!("vault: {} at {}", config.name, source_text(&config.source));
                match &config.signer {
                    Some(signer) => println!(
                        "signer: {} (pinned), key {}",
                        envelope::fingerprint(signer),
                        crate::b64::encode_padded(signer)
                    ),
                    None => println!("signer: none (a plain text file)"),
                }
            }
        }
        if let Some((recipient, code)) = &status.machine {
            println!(
                "{}",
                machine_line(status.config.as_ref(), &recipient.to_bech32(), code)
            );
        }
        match status.membership {
            Membership::Unconfigured => {}
            Membership::Pending => {
                let code = status
                    .machine
                    .as_ref()
                    .map_or("?", |(_, code)| code.as_str());
                // How a mistyped join password is fixed, only when one went with the request.
                // (A path has no join queue, so its default machine name is never looked up.)
                let hint = status
                    .config
                    .as_ref()
                    .filter(|config| matches!(config.source, Source::Url(_)))
                    .filter(|_| client::asked_with_password(paths))
                    .and_then(|config| {
                        let named = (config.host != client::default_host_name())
                            .then_some(config.host.as_str());
                        retype(&config.source, named, false)
                    });
                println!("{}", pending_membership(code, hint.as_deref()));
            }
            Membership::In { seq, heard } => {
                println!("membership: in, seq {seq}, last heard {}", ago(heard));
            }
            Membership::Revoked { seq } => println!("membership: revoked at seq {seq}"),
            Membership::Plain => println!("membership: reading a plain text file"),
        }
        if !status.names.is_empty() {
            println!(
                "secrets: {} ({})",
                status.names.len(),
                status.names.join(", ")
            );
        }
        if status.config.is_some() {
            let state = (subscriber.state)();
            println!("subscriber: {}", service_words(&state));
            // An admin of a vault that lets machines in by itself: they are let in only while
            // an approver runs (on this Mac, or another admin's).
            if let Ok(Some(approval)) = client::approving_here(paths) {
                println!("{}", approver_line(approval, &state));
            }
        }
        if !matches!(
            service::state(&paths.home, Job::Server),
            Ok(service::State::Absent)
        ) {
            println!("server: {}", service_line(paths, Job::Server));
        }
        Ok(ExitCode::SUCCESS)
    }

    fn install_subscriber(paths: &Paths, program: Option<String>) -> crate::Result<String> {
        let program = match program {
            Some(program) => PathBuf::from(program),
            None => service::program_for_service()?,
        };
        service::install(
            &paths.home,
            Job::Subscriber,
            &program,
            &["secrets".to_string(), "watch".to_string()],
        )
    }

    fn join(
        paths: &Paths,
        source: &str,
        name: Option<String>,
        agent: bool,
        wait: bool,
        password_stdin: bool,
    ) -> crate::Result<ExitCode> {
        let interactive = interactive();
        // Read before any network call, so a pipe that holds nothing fails at once.
        let piped = if password_stdin {
            Some(piped_password("--password-stdin")?)
        } else {
            None
        };
        let confirm = |fingerprint: &str| {
            yes(ask(&format!(
                "The vault is signed by {fingerprint}. Compare it with `aterm secrets status` on an admin Mac. Trust it? [y/N] "
            )))
        };
        // Whether a join password went with the request: only then can it have been mistyped.
        let given = Cell::new(false);
        let from_pipe = piped.is_some();
        let password = || -> Option<SecretBytes> {
            let password = match &piped {
                Some(piped) => Some(piped.clone()),
                None if !interactive => None,
                None => ask_hidden(
                    "Join password (an admin Mac shows it: aterm secrets password; Enter joins without one: an open vault lets this Mac in, another approves it by code): ",
                )
                .filter(|typed| !typed.as_slice().trim_ascii().is_empty()),
            };
            given.set(password.is_some());
            password
        };
        // The subscriber goes in as soon as the request is queued, so a Ctrl-C during the
        // wait still leaves a machine that joins by itself once approved.
        let installed = Cell::new(false);
        let install_failed = Cell::new(false);
        let install = || {
            if !agent || installed.replace(true) {
                return;
            }
            match install_subscriber(paths, None) {
                Ok(line) => eprintln!("{line}"),
                Err(error) => {
                    install_failed.set(true);
                    eprintln!(
                        "the subscriber was not installed ({error}); run aterm secrets agent on --program <aterm> to keep this machine in sync"
                    );
                }
            }
        };
        let say = |line: &str| eprintln!("{line}");
        let before = client::config(paths).ok().flatten();
        let outcome = client::join(
            paths,
            &JoinOptions {
                source: source.to_string(),
                host: name.clone(),
                interactive,
                wait: if wait {
                    Duration::from_secs(600)
                } else {
                    Duration::ZERO
                },
                confirm: &confirm,
                password: &password,
                requested: &install,
                say: &say,
            },
        )
        .inspect_err(|_| {
            // Cut off after it saved the source (the server gone before the request was
            // queued), the join leaves this Mac waiting with its request due at once: the
            // subscriber that makes it goes in now, as once a request is queued.
            if client::config(paths)
                .ok()
                .flatten()
                .is_some_and(|after| before.as_ref() != Some(&after))
            {
                install();
            }
        })?;
        match &outcome {
            JoinOutcome::Joined { seq, names } => {
                println!("joined: seq {seq}, {} secrets in ~/.secrets", names.len());
            }
            JoinOutcome::Pending { code, host } => {
                // The source the join saved, not what was typed: a URL without its `#<signer>`
                // (this Mac holds that pin now), and the ALab default when nothing was typed.
                let hint = if given.get() {
                    client::config(paths)
                        .ok()
                        .flatten()
                        .and_then(|config| retype(&config.source, name.as_deref(), from_pipe))
                } else {
                    None
                };
                println!("{}", waiting_line(host, code, hint.as_deref()));
            }
            JoinOutcome::Plain { names } => {
                println!(
                    "reading a plain text file: {} secrets in ~/.secrets",
                    names.len()
                );
            }
        }
        install();
        Ok(if install_failed.get() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }

    fn sync(paths: &Paths) -> crate::Result<ExitCode> {
        match client::sync(paths, None)? {
            SyncOutcome::Applied { seq, report } => {
                println!(
                    "applied seq {seq}: {} written, {} unchanged, {} removed, {} left in place, {} moved aside",
                    report.written.len(),
                    report.unchanged.len(),
                    report.removed.len(),
                    report.left.len(),
                    report.moved_aside.len()
                );
                for (name, to) in &report.moved_aside {
                    println!("  {name}: the previous file was moved to {}", to.display());
                }
                for (name, why) in &report.failed {
                    println!("  {name}: not written: {why}");
                }
                Ok(if report.failed.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                })
            }
            SyncOutcome::Unchanged { seq } => {
                println!("up to date at seq {seq}");
                Ok(ExitCode::SUCCESS)
            }
            SyncOutcome::PutBack { seq, served } => {
                let held = served.map_or("no vault".to_string(), |served| {
                    format!("seq {served}, older than this Mac's")
                });
                println!(
                    "the source held {held}: put this Mac's copy back; up to date at seq {seq}"
                );
                Ok(ExitCode::SUCCESS)
            }
            SyncOutcome::NotIn => {
                println!("this machine is not in the vault (yet): aterm secrets status");
                Ok(ExitCode::FAILURE)
            }
        }
    }

    fn get(paths: &Paths, name: &str) -> crate::Result<ExitCode> {
        let status = client::status(paths)?;
        if !status.names.iter().any(|n| n == name) {
            return Err(Error::new(format!(
                "{name} is not among this machine's secrets (aterm secrets ls)"
            )));
        }
        let path = crate::materialize::target_path(&paths.home, name)?;
        let value = fsx::read_private(&path, text::MAX_VALUE)?.ok_or_else(|| {
            Error::new(format!(
                "{}: missing; run aterm secrets sync",
                path.display()
            ))
        })?;
        let mut out = std::io::stdout().lock();
        out.write_all(value.as_slice())?;
        out.flush()?;
        Ok(ExitCode::SUCCESS)
    }

    /// `create`, then this Mac's subscriber put in as `join` puts it in (unless `agent` is
    /// false, `--no-agent`): a new vault has a join password, and only an admin's subscriber
    /// approves a machine that types it. A failed install is a warning, not a failure: the
    /// vault is out, and `aterm secrets agent on` starts the approver later. So is a failure
    /// of this Mac's own sync after the publish, said after the approver step, which still
    /// runs: the subscriber it puts in syncs the vault.
    pub(super) fn create(
        paths: &Paths,
        source: &str,
        import: Option<String>,
        name: Option<String>,
        agent: bool,
        subscriber: &Subscriber<'_>,
    ) -> crate::Result<ExitCode> {
        let import = import.map(|dir| expand(paths, &dir));
        let created = client::create(paths, source, import.as_deref(), name.as_deref())?;
        println!("created vault {} (seq 1)", created.name);
        println!(
            "signer: {} (fingerprint {})",
            created.signer, created.fingerprint
        );
        println!(
            "machines that join it must pin this key: add #{} to the URL, or confirm the fingerprint",
            created.signer
        );
        // A vault at a path has no join queue: a machine joins it by key (`approve <age1…>
        // --name`), so no approver has anything to do there.
        let mut syncs = false;
        if !matches!(
            client::config(paths)?.map(|config| config.source),
            Some(Source::Url(_))
        ) {
            println!(
                "a machine joins it when an admin Mac approves its key: aterm secrets approve <age1…> --name <host>"
            );
        } else {
            let approver = approver_here(agent, subscriber);
            if let Some(words) = approver_words(&approver, Approval::Password) {
                eprintln!("{words}");
            }
            if approver.runs() {
                println!(
                    "a joining machine that types the join password is let in by this Mac's subscriber: aterm secrets password shows it"
                );
            } else {
                println!("aterm secrets password shows the join password a joining machine types");
            }
            syncs = approver.runs();
        }
        if !created.imported.is_empty() {
            println!("imported into [everyone]: {}", created.imported.join(", "));
        }
        if let Some(why) = &created.local {
            let then = if syncs {
                "this Mac's subscriber syncs it"
            } else {
                "aterm secrets sync retries"
            };
            eprintln!(
                "warning: the vault is published, but this Mac's copy is not synced yet ({why}): {then}"
            );
        }
        Ok(ExitCode::SUCCESS)
    }

    fn expand(paths: &Paths, text: &str) -> PathBuf {
        match text.strip_prefix("~/") {
            Some(rest) => paths.home.join(rest),
            None => PathBuf::from(text),
        }
    }

    /// The store a `--store` names: a relative one is under the home directory, where a
    /// systemd unit runs (a launchd job runs from `/`, a hand-run serve from anywhere), so
    /// one line means one store however it is run.
    pub(super) fn store_dir(paths: &Paths, text: &str) -> PathBuf {
        let dir = expand(paths, text);
        if dir.is_absolute() {
            dir
        } else {
            paths.home.join(dir)
        }
    }

    /// `$VISUAL` / `$EDITOR` (words split on spaces), else vi, resolved on PATH.
    fn editor() -> crate::Result<Vec<String>> {
        let line = std::env::var("VISUAL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| "vi".to_string());
        let mut words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        if words.is_empty() {
            return Err(Error::new("no editor: set EDITOR"));
        }
        if !words[0].contains('/') {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let found = std::env::split_paths(&path)
                .map(|dir| dir.join(&words[0]))
                .find(|candidate| {
                    fsx::kind(candidate)
                        .is_ok_and(|k| matches!(k, fsx::Kind::File | fsx::Kind::Symlink))
                })
                .ok_or_else(|| Error::new(format!("editor {} is not on PATH", words[0])))?;
            words[0] = found.to_string_lossy().into_owned();
        }
        Ok(words)
    }

    fn edit(paths: &Paths) -> crate::Result<ExitCode> {
        if !interactive() {
            return Err(Error::new(
                "edit needs a terminal; use set/unset to change one entry",
            ));
        }
        // Found first, so an editor that is not there leaves no draft behind.
        let editor = editor()?;
        edit_with(
            paths,
            &mut |file| {
                let mut cmd = aterm_uds::spawnfd::command(&editor[0]);
                cmd.args(&editor[1..]).arg(file);
                cmd.status()
                    .map(|status| status.success())
                    .map_err(|e| Error::new(format!("{}: {e}", editor[0])))
            },
            &ask,
        )
    }

    /// [`edit`] past its terminal check: `run` opens the draft in the editor (false when the
    /// editor fails), and `ask` asks the person.
    pub(super) fn edit_with(
        paths: &Paths,
        run: &mut dyn FnMut(&Path) -> crate::Result<bool>,
        ask: &dyn Fn(&str) -> Option<String>,
    ) -> crate::Result<ExitCode> {
        let mut admin = client::admin_open(paths)?;
        // The version the text began as: what another admin, or the approver, changed after
        // it is brought into what the person saves ([`client::keep_admitted`]).
        let base = admin.doc.clone();
        let scratch = client::Scratch::new(paths)?;
        let file = scratch
            .dir()
            .join(format!("{}.vault.txt", admin.config.name));
        let original = base.render();
        fsx::write_atomic(&file, original.as_slice(), 0o600)?;
        eprintln!(
            "draft: {} (kept unless edit ends with it published or unchanged, so an interrupted edit leaves it)",
            file.display()
        );
        // The draft as the editor reopened it on a version that moved: quitting on it unchanged
        // is no answer to the warning, so it publishes only when the person says so.
        let mut reopened: Option<SecretBytes> = None;
        loop {
            let ran = run(&file);
            if !matches!(ran, Ok(true)) {
                // A draft that still holds the published text holds no work, so it goes;
                // one that changed is kept.
                let kept = scratch.discard(&file, original.as_slice()).is_err();
                let why = match ran {
                    Err(error) => error.to_string(),
                    Ok(_) => "the editor failed".to_string(),
                };
                let kept = if kept { " (the draft is kept)" } else { "" };
                return Err(Error::new(format!("{why}; nothing published{kept}")));
            }
            let edited = fsx::read_private(&file, text::MAX_TEXT)?
                .ok_or_else(|| Error::new("the edited file is gone; nothing published"))?;
            if edited.as_slice() == original.as_slice() {
                if let Err(error) = scratch.discard(&file, original.as_slice()) {
                    eprintln!("no change; draft cleanup failed: {error}");
                }
                println!("no change; nothing published");
                return Ok(ExitCode::SUCCESS);
            }
            if reopened
                .as_ref()
                .is_some_and(|text| text.as_slice() == edited.as_slice())
                && !yes(ask(&format!(
                    "No change since the vault moved. Publish it over seq {} as it is? [y/N] ",
                    admin.seq()
                )))
            {
                eprintln!("nothing published (the draft is kept)");
                return Ok(ExitCode::FAILURE);
            }
            let parsed = match edited.as_str() {
                Some(text) => text::Document::parse(text).map_err(text_error),
                None => Err(Error::new("the file is not UTF-8")),
            };
            let confirmed = parsed.and_then(|mut doc| {
                let kept = client::keep_admitted(&base, &admin.doc, &mut doc)?;
                if !kept.is_empty() {
                    eprintln!("keeping {}: admitted while you edited", kept.join(", "));
                }
                if doc.anchored()
                    && !admin.doc.anchored()
                    && !yes(ask(&format!(
                        "{FIRST_ANCHOR}. Does every one read anchors now? [y/N] "
                    )))
                {
                    return Err(Error::new(
                        "the vault's first anchored entry waits until every machine and admin reads anchors",
                    ));
                }
                Ok(doc)
            });
            match confirmed.and_then(|doc| client::admin_publish(paths, &admin, &doc)) {
                Ok(done) => {
                    if let Err(error) = scratch.discard(&file, edited.as_slice()) {
                        eprintln!("published; draft cleanup failed: {error}");
                    }
                    return Ok(published("published the edit", &done));
                }
                Err(error) => {
                    eprintln!("not published: {error}");
                    // Another admin, or the approver, may have published meanwhile (the server
                    // refuses a version that is not newer, and so does a path): the next try
                    // goes on top of the newest version, and the person is told their text
                    // replaces it, all but who that version let in or out and its [join] line.
                    match client::admin_open(paths) {
                        Ok(fresh) if fresh.seq() != admin.seq() => {
                            eprintln!(
                                "the vault moved from seq {} to seq {} while you edited; a save publishes your text over it, keeping the machines and admins it let in or out and its [join] line, so bring in its other changes first (a file left as it is publishes only if you say so)",
                                admin.seq(),
                                fresh.seq()
                            );
                            admin = fresh;
                            reopened = Some(edited.clone());
                        }
                        Ok(_) => {}
                        Err(reopen) => eprintln!("the vault could not be reopened: {reopen}"),
                    }
                    let again = ask("Edit again? [Y/n] ").unwrap_or_default();
                    if again.eq_ignore_ascii_case("n") || again.eq_ignore_ascii_case("no") {
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        }
    }

    /// Why the vault's first anchored entry waits for the fleet: no machine tells the vault
    /// which aterm it runs, so the admin says it.
    const FIRST_ANCHOR: &str = "this is the vault's first anchored entry (if-absent=<anchor>), and an aterm older than anchors refuses every version that holds one: a machine on it applies nothing until it updates, and an admin on it cannot open the vault. Update every machine and admin first (there `aterm secrets --help` names --with)";

    /// `doc` with entry `name` set for `audience` as `set` sets it, and the anchor the entry
    /// got. Leaving a flag out never changes how an entry fills: an ` if-absent` entry set
    /// again without `fill.if_absent` is refused (unsetting it first makes it a plain one),
    /// and one set again with it but neither `--with` nor `--no-with` keeps the anchor it
    /// has. The vault's first anchored entry is refused without `--fleet-reads-anchors`
    /// ([`FIRST_ANCHOR`]).
    pub(super) fn set_in(
        doc: &text::Document,
        audience: &Audience,
        name: &str,
        value: &[u8],
        fill: &Fill,
    ) -> crate::Result<(text::Document, Option<String>)> {
        let was = doc.entry(audience, name);
        if !fill.if_absent
            && let Some(was) = was.filter(|was| was.if_absent)
        {
            let (spelled, kept) = match &was.anchor {
                Some(anchor) => (format!("if-absent={anchor}"), " (its anchor stays)"),
                None => ("if-absent".to_string(), ""),
            };
            return Err(Error::new(format!(
                "{name} fills gaps ({spelled}): pass --if-absent to keep it so{kept}, or unset it first to make it a plain entry"
            )));
        }
        let anchor = match &fill.with {
            Some(anchor) => Some(anchor.clone()),
            None if fill.no_with => None,
            None => was.and_then(|was| was.anchor.clone()),
        };
        let mut next = doc.clone();
        next.set_entry_with(audience, name, value, fill.if_absent, anchor.as_deref())
            .map_err(text_error)?;
        if next.anchored() && !doc.anchored() && !fill.fleet_reads_anchors {
            return Err(Error::new(format!(
                "{FIRST_ANCHOR}, then pass --fleet-reads-anchors"
            )));
        }
        Ok((next, anchor))
    }

    /// A value piped in as one line of text (`echo token | …`) loses its line end: the LF and
    /// a CR before it, when what is left is one line `name=value` spells (UTF-8 without CR, LF
    /// or NUL). Anything else, a binary value above all, is kept byte for byte, as `--file`
    /// keeps it.
    pub(super) fn trim_piped_line(bytes: &mut Vec<u8>) {
        let Some(body) = bytes.strip_suffix(b"\n") else {
            return;
        };
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        let one_line = std::str::from_utf8(body).is_ok()
            && !body.iter().any(|b| matches!(b, b'\r' | b'\n' | 0));
        let keep = body.len();
        if one_line {
            bytes.truncate(keep);
        }
    }

    fn set(
        paths: &Paths,
        name: &str,
        hosts: &[String],
        file: Option<String>,
        fill: &Fill,
    ) -> crate::Result<ExitCode> {
        if !text::valid_entry_name(name) {
            return Err(Error::new(format!(
                "{name:?} is not a valid name (letters, digits, . _ -; or ~/a/b)"
            )));
        }
        let value = match file {
            Some(file) => {
                let path = expand(paths, &file);
                SecretBytes::new(
                    fsx::read_public(&path, text::MAX_VALUE)?
                        .ok_or_else(|| Error::new(format!("{}: no such file", path.display())))?,
                )
            }
            None => {
                if std::io::stdin().is_terminal() {
                    return Err(Error::new(format!(
                        "pipe the value in (e.g. pbpaste | aterm secrets set {name}) or pass --file <path>; it is never read from the command line"
                    )));
                }
                let mut bytes = Vec::new();
                crate::secret::read_to_end_wiping_on_error(
                    std::io::stdin().take(text::MAX_VALUE as u64 + 1),
                    &mut bytes,
                )?;
                if bytes.len() > text::MAX_VALUE {
                    bytes.zeroize();
                    return Err(Error::new("the value is larger than 1 MiB"));
                }
                trim_piped_line(&mut bytes);
                SecretBytes::new(bytes)
            }
        };
        if value.as_slice().is_empty() {
            return Err(Error::new(format!(
                "the value is empty; aterm secrets unset {name} removes an entry"
            )));
        }
        let admin = client::admin_open(paths)?;
        let hosts = &machines_named(&admin.doc, hosts)?;
        let (doc, anchor) = set_in(&admin.doc, &audience(hosts), name, value.as_slice(), fill)?;
        let done = client::admin_publish(paths, &admin, &doc)?;
        let gaps = match (fill.if_absent, anchor) {
            (false, _) => String::new(),
            (true, None) => " (written only where no such file exists)".to_string(),
            (true, Some(anchor)) => format!(
                " (written only where no such file exists and {anchor} is not the machine's own)"
            ),
        };
        Ok(published(
            &format!("set {name} for {}{gaps}", audience(hosts).header()),
            &done,
        ))
    }

    /// `password`. What leaves a vault with a join queue (a URL's) letting machines in by
    /// itself (`open`, `set`, `new`) also puts this Mac's approver in where none runs
    /// ([`put_in_approver`]), so the promise it prints holds: only an admin's subscriber lets
    /// a machine in without a person. `--no-agent` (`agent` false) installs nothing, for an
    /// admin whose subscriber was turned off on purpose, and says so where none runs. A vault
    /// at a path has no queue: nothing is put in, and what it prints says a machine joins it
    /// by key ([`lets_in_words`]).
    pub(super) fn password(
        paths: &Paths,
        action: Password,
        agent: bool,
        subscriber: &Subscriber<'_>,
    ) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let mut doc = admin.doc.clone();
        let queued = matches!(admin.config.source, Source::Url(_));
        let approver = |doc: &text::Document| {
            let Some(approval) = Approval::of(doc).filter(|_| queued) else {
                return;
            };
            let approver = approver_here(agent, subscriber);
            if let Some(words) = approver_words(&approver, approval) {
                eprintln!("{words}");
            }
        };
        match action {
            Password::Show => match admin.doc.join_password() {
                Some(password) => {
                    let mut out = std::io::stdout().lock();
                    out.write_all(password.as_slice())?;
                    out.write_all(b"\n")?;
                    out.flush()?;
                    Ok(ExitCode::SUCCESS)
                }
                None if admin.doc.open() => {
                    if let Some(words) = lets_in_words(queued, &admin.doc) {
                        eprintln!("{words}");
                    }
                    Ok(ExitCode::FAILURE)
                }
                None => {
                    eprintln!(
                        "this vault has no join password (aterm secrets password new makes one); joining machines wait for approval by code"
                    );
                    Ok(ExitCode::FAILURE)
                }
            },
            Password::Open => {
                if admin.doc.open() {
                    if let Some(words) = lets_in_words(queued, &admin.doc) {
                        println!("{words}");
                    }
                    approver(&admin.doc);
                    return Ok(ExitCode::SUCCESS);
                }
                // Those already waiting are a person's to judge: one may be a machine let go
                // while the vault was closed, its approved request still queued. Not let in
                // here, each joins only by code (the publish names them).
                let waiting = client::requests(paths, Some(&doc))?;
                if interactive() {
                    admit_on_opening(
                        &mut doc,
                        waiting,
                        &mut |line| eprintln!("{line}"),
                        &mut |question| yes(ask(question)),
                    )?;
                }
                doc.set_open(true).map_err(text_error)?;
                let done = client::admin_publish(paths, &admin, &doc)?;
                let code = published("opened the vault", &done);
                if let Some(words) = lets_in_words(queued, &doc) {
                    println!("{words}");
                }
                approver(&doc);
                eprintln!("note: {OLDER_ADMINS}");
                Ok(code)
            }
            Password::New => {
                let password = join::generate()?;
                doc.set_join_password(Some(password.as_slice()))
                    .map_err(text_error)?;
                let done = client::admin_publish(paths, &admin, &doc)?;
                let code = published("set a new join password", &done);
                if let Some(words) = lets_in_words(queued, &doc) {
                    println!("{words}");
                }
                approver(&doc);
                let mut out = std::io::stdout().lock();
                out.write_all(b"join password: ")?;
                out.write_all(password.as_slice())?;
                out.write_all(b"\n")?;
                out.flush()?;
                Ok(code)
            }
            Password::Set => {
                let password = if interactive() {
                    let first = ask_hidden("New join password: ")
                        .ok_or_else(|| Error::new("no terminal to type the password at"))?;
                    join::strength(first.as_slice()).map_err(Error::new)?;
                    let second = ask_hidden("The same again: ")
                        .ok_or_else(|| Error::new("no terminal to type the password at"))?;
                    if first.as_slice().trim_ascii() != second.as_slice().trim_ascii() {
                        return Err(Error::new("the two passwords differ; nothing changed"));
                    }
                    first
                } else {
                    piped_password("password set")?
                };
                doc.set_join_password(Some(password.as_slice()))
                    .map_err(text_error)?;
                let done = client::admin_publish(paths, &admin, &doc)?;
                let code = published("set the join password", &done);
                if let Some(words) = lets_in_words(queued, &doc) {
                    println!("{words}");
                }
                approver(&doc);
                Ok(code)
            }
            Password::Off => {
                if admin.doc.open() {
                    doc.set_open(false).map_err(text_error)?;
                    let done = client::admin_publish(paths, &admin, &doc)?;
                    return Ok(published(
                        "closed the vault (joining machines now wait for approval by code)",
                        &done,
                    ));
                }
                if admin.doc.join_password().is_none() {
                    println!("this vault has no join password");
                    return Ok(ExitCode::SUCCESS);
                }
                doc.set_join_password(None).map_err(text_error)?;
                let done = client::admin_publish(paths, &admin, &doc)?;
                Ok(published(
                    "removed the join password (joining machines now wait for approval by code)",
                    &done,
                ))
            }
        }
    }

    /// What `password open` at a terminal does with the requests `waiting` as it opens the
    /// vault: names each machine once on `say`, and on one yes from `agree` lets in every one
    /// but a key a `code-only` line names, or one asking under more than one name. The first
    /// is a machine let go (or one held when the vault last opened): it is named, marked, and
    /// joins only by its code. The second may be a stranger that read the key off the listing
    /// and queued it under a name of its choosing: it joins only as the person names it.
    pub(super) fn admit_on_opening(
        doc: &mut text::Document,
        waiting: Vec<client::Request>,
        say: &mut dyn FnMut(String),
        agree: &mut dyn FnMut(&str) -> bool,
    ) -> crate::Result<()> {
        let code_only = doc.code_only();
        let (mut offered, mut held, mut named) = (Vec::new(), Vec::new(), Vec::new());
        for request in &waiting {
            // One host asked from two addresses is one machine.
            let listed = |list: &[&client::Request]| {
                list.iter()
                    .any(|r| r.recipient == request.recipient && r.host == request.host)
            };
            if listed(&offered) || listed(&held) || listed(&named) {
                continue;
            }
            if code_only.contains(&request.recipient) {
                held.push(request);
            } else if names_asked(&waiting, &request.recipient).len() > 1 {
                named.push(request);
            } else {
                offered.push(request);
            }
        }
        let line = |request: &client::Request, mark: &str| {
            let when = request.at.map(ago).unwrap_or_default();
            format!("  {}  code {}  {when}{mark}", request.host, request.code)
        };
        if !offered.is_empty() {
            say("these machines asked to join before the vault opened:".into());
            for request in &offered {
                say(line(request, ""));
            }
        }
        if !held.is_empty() {
            say(
                "these are asking too, and are let in only by code (aterm secrets approve):".into(),
            );
            for request in &held {
                say(line(request, CODE_ONLY_MARK));
            }
        }
        if !named.is_empty() {
            say("these keys ask under more than one name, and anyone on the LAN can queue a key under a name of its choosing: let one in under the name its machine asked for with aterm secrets approve <age1…> --name <host>".into());
            for request in &named {
                say(format!("{}  {}", line(request, ""), request.recipient));
            }
        }
        if offered.is_empty() {
            return Ok(());
        }
        let question = format!(
            "Let these {} in now? Any you do not know may be a machine you let go. Each reads every [everyone] secret. [y/N] ",
            offered.len()
        );
        if agree(&question) {
            for request in offered {
                let host = client::free_name(doc, &request.host);
                doc.add_machine(&host, &request.recipient)
                    .map_err(text_error)?;
            }
        }
        Ok(())
    }

    /// The names the queue lists `key` under, sorted, each once.
    fn names_asked<'a>(pending: &'a [client::Request], key: &str) -> Vec<&'a str> {
        let mut hosts: Vec<&str> = pending
            .iter()
            .filter(|r| r.recipient == key)
            .map(|r| r.host.as_str())
            .collect();
        hosts.sort_unstable();
        hosts.dedup();
        hosts
    }

    fn requests(paths: &Paths) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths).ok();
        let pending = client::requests(paths, admin.as_ref().map(|a| &a.doc))?;
        let code_only = admin
            .as_ref()
            .map(|a| a.doc.code_only())
            .unwrap_or_default();
        if pending.is_empty() {
            println!("no machines are asking to join");
        }
        for request in pending {
            let when = request.at.map(ago).unwrap_or_default();
            let proof = if request.proof.is_some() {
                "  (sent a join-password proof)"
            } else {
                ""
            };
            let held = if code_only.contains(&request.recipient) {
                CODE_ONLY_MARK
            } else {
                ""
            };
            println!(
                "{}  code {}  {}  {when}{proof}{held}",
                request.host, request.code, request.recipient
            );
        }
        Ok(ExitCode::SUCCESS)
    }

    fn approve(
        paths: &Paths,
        which: &[String],
        all: bool,
        name: Option<String>,
    ) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let mut doc = admin.doc.clone();
        let pending = client::requests(paths, Some(&doc))?;
        if all && pending.is_empty() {
            println!("no machines are asking to join");
            return Ok(ExitCode::SUCCESS);
        }
        // Before any prompt: a code no machine asks with, or a key listed under two names.
        let mut chosen = from_queue(&pending, which, all)?;
        if all {
            if !interactive() {
                return Err(Error::new(
                    "approve --all asks for a yes at a terminal; approve each code instead",
                ));
            }
            let code_only = doc.code_only();
            for request in &pending {
                let held = if code_only.contains(&request.recipient) {
                    CODE_ONLY_MARK
                } else {
                    ""
                };
                eprintln!(
                    "  {}  code {}  {}{held}",
                    request.host, request.code, request.recipient
                );
            }
            let question = format!(
                "Approve these {} machines? Each one reads every [everyone] secret. Compare each code with the one its machine shows. [y/N] ",
                pending.len()
            );
            if !yes(ask(&question)) {
                println!("nothing approved");
                return Ok(ExitCode::FAILURE);
            }
        }
        // A key named here is not in the queue's pairs: `--name` goes with one key alone, and
        // `add_machine` refuses a key twice rather than pick one of its names.
        for word in which.iter().filter(|word| word.starts_with("age1")) {
            crate::age::Recipient::parse(word)
                .map_err(|_| Error::new(format!("{word} is not a valid age key")))?;
            let host = name
                .clone()
                .ok_or_else(|| Error::new("approving a key needs --name <host>"))?;
            chosen.push((word.clone(), host));
        }
        if chosen.is_empty() {
            println!("nothing to approve");
            return Ok(ExitCode::SUCCESS);
        }
        let mut approved = Vec::with_capacity(chosen.len());
        for (recipient, host) in chosen {
            let host = client::free_name(&doc, &host);
            doc.add_machine(&host, &recipient).map_err(text_error)?;
            let code = crate::age::Recipient::parse(&recipient)
                .map(|r| client::join_code(&r))
                .unwrap_or_default();
            approved.push(format!("{host} (code {code})"));
        }
        let done = client::admin_publish(paths, &admin, &doc)?;
        Ok(published(
            &format!("approved {}", approved.join(", ")),
            &done,
        ))
    }

    /// What `host` could read in `doc`, said where it is let go (`revoke`, `admins remove`),
    /// so a person rotates all of it: as an admin, every entry, since the admin locker holds
    /// the whole text; else every entry of `[everyone]` and of each section naming it, an
    /// `[everyone]` value its own section overrides too, since it opened both lockers. A host
    /// section's entry is named with its header. `None` when it could read none.
    pub(super) fn exposure(doc: &text::Document, host: &str) -> Option<String> {
        let admin = doc.admins().iter().any(|name| name == host);
        let held: Vec<String> = doc
            .entries()
            .into_iter()
            .filter(|entry| admin || entry.audience.includes(host))
            .map(|entry| match &entry.audience {
                Audience::Everyone => entry.name.clone(),
                hosts => format!("{} {}", entry.name, hosts.header()),
            })
            .collect();
        if held.is_empty() {
            return None;
        }
        let (n, list) = (held.len(), held.join(", "));
        Some(if admin {
            format!(
                "{host} was an admin, so it could read every secret ({n}: {list}); rotate them if it was lost or compromised"
            )
        } else {
            format!(
                "{host} could read {n} secrets ({list}); rotate them if it was lost or compromised"
            )
        })
    }

    /// The `(key, host)` pairs `approve` takes from the queue, one per key: the request each
    /// code in `which` names (an `age1…` word is not a code), or with `all` every request. A
    /// key listed under more than one host is refused: anyone on the LAN can read a key from
    /// the listing and queue it beside its machine's own line under a name of its choosing,
    /// so which name is the machine's is the person's call (`approve <age1…> --name`).
    pub(super) fn from_queue(
        pending: &[client::Request],
        which: &[String],
        all: bool,
    ) -> crate::Result<Vec<(String, String)>> {
        let mut picked: Vec<&client::Request> = Vec::new();
        if all {
            picked.extend(pending);
        }
        for code in which.iter().filter(|word| !word.starts_with("age1")) {
            let request = pending.iter().find(|r| &r.code == code).ok_or_else(|| {
                Error::new(format!(
                    "no machine waiting to join has code {code} (aterm secrets requests)"
                ))
            })?;
            picked.push(request);
        }
        let mut chosen = Vec::with_capacity(picked.len());
        for request in picked {
            let hosts = names_asked(pending, &request.recipient);
            if hosts.len() > 1 {
                return Err(Error::new(format!(
                    "code {} asks to join under {} names ({}), and anyone on the LAN can queue a key under a name of its choosing: approve it under the name its machine asked for with aterm secrets approve {} --name <host>",
                    request.code,
                    hosts.len(),
                    hosts.join(", "),
                    request.recipient
                )));
            }
            chosen.push((request.recipient.clone(), request.host.clone()));
        }
        chosen.sort();
        chosen.dedup();
        Ok(chosen)
    }

    fn revoke(paths: &Paths, host: &str, drop_sections: bool) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let mut doc = admin.doc.clone();
        let host = &client::machine_named(&doc, host)?;
        let held = exposure(&doc, host);
        if !doc
            .remove_machine(host, drop_sections)
            .map_err(text_error)?
        {
            return Err(Error::new(format!("no machine {host} in the vault")));
        }
        let done = client::admin_publish(paths, &admin, &doc)?;
        let code = published(&format!("revoked {host}"), &done);
        if let Some(held) = held {
            println!("{held}");
        }
        Ok(code)
    }

    fn serve(paths: &Paths, args: &[String]) -> crate::Result<ExitCode> {
        let f = flags(args, SERVE_VALUED, SERVE_SWITCHES).map_err(Error::new)?;
        let mut settings = serve_settings(&f).map_err(Error::new)?;
        if !settings.pins.iter().any(|(name, _)| name == "alab")
            && let Some(key) = crate::b64::decode_padded(crate::ALAB_SIGNER)
                .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        {
            settings.pins.push(("alab".to_string(), key));
        }
        let store = settings
            .store
            .as_deref()
            .map_or_else(|| paths.served(), |dir| store_dir(paths, dir));
        eprintln!(
            "{} serving on {}, store {}",
            stamp(),
            settings.listen,
            store.display()
        );
        crate::server::serve(crate::server::Options {
            listen: settings.listen,
            store,
            advertise: settings.advertise,
            pins: settings.pins,
            hold: settings.hold.unwrap_or(crate::server::DEFAULT_HOLD),
            keep_awake: cfg!(target_os = "macos") && settings.keep_awake,
        })?;
        Ok(ExitCode::SUCCESS)
    }

    fn services(
        paths: &Paths,
        job: Job,
        action: Action,
        program: Option<String>,
        args: Vec<String>,
    ) -> crate::Result<ExitCode> {
        match action {
            Action::On => {
                let program = match program {
                    Some(program) => PathBuf::from(program),
                    None => service::program_for_service()?,
                };
                if !Path::new(&program).is_absolute() {
                    return Err(Error::new("--program must be an absolute path"));
                }
                println!("{}", service::install(&paths.home, job, &program, &args)?);
            }
            Action::Off => {
                if service::uninstall(&paths.home, job)? {
                    println!("{} stopped and removed", job.label());
                } else {
                    println!("{} was not installed", job.label());
                }
            }
            Action::Status => println!("{}: {}", job.label(), service_line(paths, job)),
        }
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn bare_verb_is_status() {
        assert_eq!(parse(&[]), Ok(Command::Status));
        assert_eq!(parse(&words("status")), Ok(Command::Status));
    }

    #[test]
    fn join_takes_one_source_and_its_switches() {
        assert_eq!(
            parse(&words("join http://h:1/v1/x --name m9 --no-agent")),
            Ok(Command::Join {
                source: "http://h:1/v1/x".into(),
                name: Some("m9".into()),
                agent: false,
                wait: true,
                password_stdin: false,
            })
        );
        assert_eq!(
            parse(&words("join")),
            Ok(Command::Join {
                source: String::new(),
                name: None,
                agent: true,
                wait: true,
                password_stdin: false,
            })
        );
        assert!(matches!(
            parse(&words("join --password-stdin")),
            Ok(Command::Join {
                password_stdin: true,
                ..
            })
        ));
        assert!(parse(&words("join a b")).is_err());
        assert!(parse(&words("join --name")).is_err());
    }

    #[test]
    fn create_takes_one_source_and_its_switches() {
        assert_eq!(
            parse(&words(
                "create http://h:1/v1/x --import ~/.secrets --name m9 --no-agent"
            )),
            Ok(Command::Create {
                source: "http://h:1/v1/x".into(),
                import: Some("~/.secrets".into()),
                name: Some("m9".into()),
                agent: false,
            })
        );
        assert_eq!(
            parse(&words("create /v/team.vault")),
            Ok(Command::Create {
                source: "/v/team.vault".into(),
                import: None,
                name: None,
                agent: true,
            })
        );
        for refused in [
            "create",
            "create a b",
            "create a --no-wait",
            "create a --name",
        ] {
            assert!(parse(&words(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn set_never_takes_a_value_on_the_command_line() {
        let error = parse(&words("set token hunter2")).expect_err("refused");
        assert!(!error.contains("hunter2"), "{error}");
        assert_eq!(
            parse(&words("set token --for m3 --for m22 --for m3")),
            Ok(Command::Set {
                name: "token".into(),
                hosts: vec!["m22".into(), "m3".into()],
                file: None,
                fill: Fill::default(),
            })
        );
        assert_eq!(
            parse(&words("set ~/.aterm/machine.key --file /k --if-absent")),
            Ok(Command::Set {
                name: "~/.aterm/machine.key".into(),
                hosts: vec![],
                file: Some("/k".into()),
                fill: Fill {
                    if_absent: true,
                    ..Fill::default()
                },
            })
        );
    }

    #[test]
    fn set_with_names_the_anchor_of_an_if_absent_entry() {
        assert_eq!(
            parse(&words(
                "set ~/.aterm/release-credentials.toml --if-absent --with ~/.aterm/machine.key --file /p"
            )),
            Ok(Command::Set {
                name: "~/.aterm/release-credentials.toml".into(),
                hosts: vec![],
                file: Some("/p".into()),
                fill: Fill {
                    if_absent: true,
                    with: Some("~/.aterm/machine.key".into()),
                    ..Fill::default()
                },
            })
        );
        assert!(matches!(
            parse(&words("set a --if-absent --no-with --fleet-reads-anchors")),
            Ok(Command::Set {
                fill: Fill {
                    if_absent: true,
                    with: None,
                    no_with: true,
                    fleet_reads_anchors: true,
                },
                ..
            })
        ));
        for refused in [
            "set a --with b",
            "set a --if-absent --with",
            "set a --if-absent --with a",
            "set a --if-absent --with ../b",
            "set a --no-with",
            "set a --if-absent --with b --no-with",
        ] {
            assert!(parse(&words(refused)).is_err(), "{refused}");
        }
        let error = parse(&words("set a --if-absent --with hunter2!")).expect_err("refused");
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[cfg(unix)]
    const KEY: &str = "~/.aterm/machine.key";
    #[cfg(unix)]
    const PROFILE: &str = "~/.aterm/release-credentials.toml";

    /// A vault of one machine holding the release key and profile as if-absent entries, with
    /// no anchor yet, as the live one does.
    #[cfg(unix)]
    fn unanchored_vault() -> crate::text::Document {
        let recipient = crate::age::Identity::generate()
            .expect("a key")
            .recipient()
            .to_bech32();
        let mut doc = crate::text::Document::new_vault("m22", &recipient);
        for name in [KEY, PROFILE] {
            doc.set_entry_with(&crate::text::Audience::Everyone, name, b"v\n", true, None)
                .expect("if-absent");
        }
        doc
    }

    #[cfg(unix)]
    #[test]
    fn opening_never_offers_a_machine_let_go() {
        let key = || {
            crate::age::Identity::generate()
                .expect("a key")
                .recipient()
                .to_bech32()
        };
        let (gone, asking) = (key(), key());
        let mut doc = crate::text::Document::new_vault("m22", &key());
        doc.add_code_only(&gone).expect("let go");
        let request = |recipient: &str, host: &str| crate::client::Request {
            recipient: recipient.to_owned(),
            host: host.to_owned(),
            code: String::new(),
            at: None,
            proof: None,
        };
        // Only the machine let go is asking: it is named and marked, and nothing is asked.
        let mut said = Vec::new();
        unix::admit_on_opening(
            &mut doc,
            vec![request(&gone, "laptop")],
            &mut |line| said.push(line),
            &mut |_| panic!("asked to let in a machine let go"),
        )
        .expect("named");
        assert!(
            said.iter()
                .any(|line| line.contains("laptop") && line.contains("code only")),
            "{said:?}"
        );
        // Beside one that may be let in, one yes lets in that one only.
        unix::admit_on_opening(
            &mut doc,
            vec![request(&gone, "laptop"), request(&asking, "m7")],
            &mut |_| {},
            &mut |_| true,
        )
        .expect("let in");
        let members: Vec<String> = doc.machines().into_iter().map(|m| m.recipient).collect();
        assert!(
            members.contains(&asking) && !members.contains(&gone),
            "{members:?}"
        );
        assert_eq!(doc.code_only(), [gone]);

        // The queue lists a key once per address. One machine that asked from two is offered
        // and let in once; a key listed under two names is left to the person, who names it.
        let (moved, doubled) = (key(), key());
        let mut asked = Vec::new();
        unix::admit_on_opening(
            &mut doc,
            vec![
                request(&moved, "m8"),
                request(&doubled, "m9"),
                request(&moved, "m8"),
                request(&doubled, "evil"),
            ],
            &mut |line| said.push(line),
            &mut |question| {
                asked.push(question.to_owned());
                true
            },
        )
        .expect("one yes lets in the machine that asked from two addresses");
        assert!(asked[0].starts_with("Let these 1 in now?"), "{asked:?}");
        let machines = doc.machines();
        assert_eq!(
            machines.iter().filter(|m| m.recipient == moved).count(),
            1,
            "{machines:?}"
        );
        assert!(!machines.iter().any(|m| m.recipient == doubled));
        assert!(
            said.iter()
                .any(|line| line.contains("evil") && line.contains(&doubled)),
            "{said:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn setting_an_entry_again_keeps_how_it_fills_unless_told() {
        let everyone = crate::text::Audience::Everyone;
        let anchor_of = |doc: &crate::text::Document, name: &str| {
            let entry = doc.entry(&everyone, name).expect("set");
            (entry.if_absent, entry.anchor.clone())
        };
        let gaps = |with: Option<&str>, no_with: bool| Fill {
            if_absent: true,
            with: with.map(str::to_string),
            no_with,
            fleet_reads_anchors: true,
        };
        let (doc, anchor) = unix::set_in(
            &unanchored_vault(),
            &everyone,
            PROFILE,
            b"v\n",
            &gaps(Some(KEY), false),
        )
        .expect("anchored");
        assert_eq!(anchor.as_deref(), Some(KEY));

        // Rotated with --if-absent alone, the profile keeps its anchor.
        let (doc, anchor) =
            unix::set_in(&doc, &everyone, PROFILE, b"w\n", &gaps(None, false)).expect("rotated");
        assert_eq!(anchor.as_deref(), Some(KEY));
        assert_eq!(anchor_of(&doc, PROFILE), (true, Some(KEY.to_string())));
        assert_eq!(
            doc.entry(&everyone, PROFILE).expect("set").value.as_slice(),
            b"w\n"
        );

        // Without --if-absent, an if-absent entry (anchored or not) is refused by name.
        let error =
            unix::set_in(&doc, &everyone, PROFILE, b"x\n", &Fill::default()).expect_err("refused");
        assert!(error.0.contains(&format!("if-absent={KEY}")), "{error}");
        let error =
            unix::set_in(&doc, &everyone, KEY, b"x\n", &Fill::default()).expect_err("refused");
        assert!(error.0.contains("--if-absent"), "{error}");

        // --no-with drops the anchor; an entry new to its audience has none to keep.
        let (dropped, anchor) =
            unix::set_in(&doc, &everyone, PROFILE, b"w\n", &gaps(None, true)).expect("dropped");
        assert_eq!(anchor, None);
        assert_eq!(anchor_of(&dropped, PROFILE), (true, None));
        let (doc, anchor) = unix::set_in(
            &doc,
            &everyone,
            "~/.aterm/other",
            b"o\n",
            &gaps(None, false),
        )
        .expect("new");
        assert_eq!(anchor, None);
        assert_eq!(anchor_of(&doc, "~/.aterm/other"), (true, None));
    }

    #[cfg(unix)]
    #[test]
    fn the_vaults_first_anchor_waits_for_the_fleet() {
        let everyone = crate::text::Audience::Everyone;
        let doc = unanchored_vault();
        let with_key = |fleet_reads_anchors| Fill {
            if_absent: true,
            with: Some(KEY.to_string()),
            no_with: false,
            fleet_reads_anchors,
        };
        let error =
            unix::set_in(&doc, &everyone, PROFILE, b"v\n", &with_key(false)).expect_err("refused");
        assert!(error.0.contains("--fleet-reads-anchors"), "{error}");
        let (doc, _) = unix::set_in(&doc, &everyone, PROFILE, b"v\n", &with_key(true))
            .expect("the fleet reads anchors");
        assert!(doc.anchored());
        // Once the vault holds one, an older aterm already refuses it: no need to say so again.
        let toml = Fill {
            if_absent: true,
            with: Some(KEY.to_string()),
            ..Fill::default()
        };
        unix::set_in(&doc, &everyone, "~/.aterm/machine.toml", b"v\n", &toml)
            .expect("a later anchor");
        unix::set_in(&doc, &everyone, "plain", b"v", &Fill::default()).expect("a plain entry");
    }

    /// Anyone on the LAN can read a waiting machine's key from the listing and queue it from
    /// its own address under a name of its choosing, beside the machine's line. Such a key is
    /// approved neither by its code nor by `--all`, whichever line sorts or was queued first:
    /// the person names the machine. One name asked from two addresses is one machine.
    #[cfg(unix)]
    #[test]
    fn approve_takes_no_key_listed_under_two_names() {
        let (code_k, code_j) = ("aaaa-bbbb-cccc-dddd", "eeee-ffff-gggg-hhhh");
        let request = |recipient: &str, host: &str, code: &str| crate::client::Request {
            recipient: recipient.into(),
            host: host.into(),
            code: code.into(),
            at: None,
            proof: None,
        };
        let pair = |k: &str, h: &str| (k.to_string(), h.to_string());
        let pending = [
            request("age1k", "m30", code_k),
            request("age1j", "m31", code_j),
            request("age1k", "a", code_k),
        ];
        assert_eq!(
            unix::from_queue(&pending, &[code_j.into()], false),
            Ok(vec![pair("age1j", "m31")])
        );
        for (which, all) in [(vec![code_k.to_string()], false), (Vec::new(), true)] {
            let refused = unix::from_queue(&pending, &which, all).expect_err("two names");
            assert!(
                refused.0.contains("(a, m30)") && refused.0.contains("approve age1k --name"),
                "{refused:?}"
            );
        }
        let moved = [
            request("age1k", "m30", code_k),
            request("age1k", "m30", code_k),
        ];
        assert_eq!(
            unix::from_queue(&moved, &[], true),
            Ok(vec![pair("age1k", "m30")])
        );
        assert_eq!(
            unix::from_queue(&moved, &[code_k.into(), code_k.into()], false),
            Ok(vec![pair("age1k", "m30")])
        );
        assert!(unix::from_queue(&moved, &[code_j.into()], false).is_err());
    }

    #[test]
    fn approve_takes_codes_never_host_names() {
        let code = "abcd-ef23-ghij-k234";
        assert_eq!(
            parse(&words(&format!("approve {} {code}", code.to_uppercase()))),
            Ok(Command::Approve {
                which: vec![code.into(), code.into()],
                all: false,
                name: None,
            })
        );
        for refused in [
            "approve m22".to_string(),
            "approve abcd-ef23".to_string(),
            format!("approve {code} --all"),
            format!("approve {code} --name m9"),
            "approve age1xyz".to_string(),
            "approve --all --name m9".to_string(),
        ] {
            assert!(parse(&words(&refused)).is_err(), "{refused}");
        }
        assert_eq!(
            parse(&words("approve --all")),
            Ok(Command::Approve {
                which: vec![],
                all: true,
                name: None,
            })
        );
        assert_eq!(
            parse(&words("approve age1xyz --name m9")),
            Ok(Command::Approve {
                which: vec!["age1xyz".into()],
                all: false,
                name: Some("m9".into()),
            })
        );
    }

    #[test]
    fn password_actions() {
        assert_eq!(
            parse(&words("password")),
            Ok(Command::Password {
                action: Password::Show,
                agent: true
            })
        );
        assert_eq!(
            parse(&words("password new")),
            Ok(Command::Password {
                action: Password::New,
                agent: true
            })
        );
        assert_eq!(
            parse(&words("password off")),
            Ok(Command::Password {
                action: Password::Off,
                agent: true
            })
        );
        assert_eq!(
            parse(&words("password open")),
            Ok(Command::Password {
                action: Password::Open,
                agent: true
            })
        );
        assert_eq!(
            parse(&words("password open --no-agent")),
            Ok(Command::Password {
                action: Password::Open,
                agent: false
            })
        );
        assert!(parse(&words("password hunter2-hunter2")).is_err());
        let error = parse(&words("password set hunter2-hunter2")).expect_err("refused");
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn serve_flags_are_checked_before_anything_runs() {
        let key = crate::b64::encode_padded(&[9u8; 32]);
        assert!(parse(&words(&format!("serve --pin alab={key} --hold 60"))).is_ok());
        for refused in [
            "serve --hold 0".to_string(),
            "serve --hold 61".to_string(),
            "serve --listen nowhere".to_string(),
            format!("serve --pin {key}"),
            "serve --pin alab=short".to_string(),
            format!("serve --pin alab={key} --pin alab={key}"),
            "serve --advertise Bad_Name".to_string(),
            "server on --hold 0".to_string(),
            "serve --advertise lab-vault --no-advertise".to_string(),
        ] {
            assert!(parse(&words(&refused)).is_err(), "{refused}");
        }
        for accepted in [
            "serve --store /srv/vaults",
            "server on --store ~/vaults",
            "server on --store vault-store",
        ] {
            assert!(parse(&words(accepted)).is_ok(), "{accepted}");
        }
    }

    /// A relative `--store` is under the home directory whether launchd (from `/`), systemd
    /// (from the home directory) or a person in another directory runs the server.
    #[cfg(unix)]
    #[test]
    fn a_relative_store_is_under_the_home_directory() {
        let paths = crate::client::Paths::for_home(std::path::Path::new("/Users//someone"));
        for (store, dir) in [
            ("vault-store", "/Users//someone/vault-store"),
            ("~/vaults", "/Users//someone/vaults"),
            ("/srv/vaults", "/srv/vaults"),
        ] {
            assert_eq!(unix::store_dir(&paths, store), std::path::Path::new(dir));
        }
    }

    /// A server set up with no flags answers the default vault's URL: on macOS it advertises
    /// that name unless told another, or none, and only on every address at the URL's port,
    /// so a server hand-run on loopback or another port takes no Mac's name.
    #[test]
    fn serve_advertises_the_default_vaults_name_unless_told() {
        let advertised = |line: &str| {
            let f = flags(&words(line), SERVE_VALUED, SERVE_SWITCHES).expect("flags");
            serve_settings(&f).expect("settings").advertise
        };
        let default = cfg!(target_os = "macos").then(|| crate::alab_advertised().to_string());
        assert_eq!(advertised(""), default);
        assert_eq!(advertised("--listen 0.0.0.0:7484"), default);
        assert_eq!(advertised("--listen [::]:7484"), default);
        for elsewhere in [
            "--listen 127.0.0.1:7485",
            "--listen 127.0.0.1:7484",
            "--listen 0.0.0.0:7485",
            "--listen [::1]:7484",
        ] {
            assert_eq!(advertised(elsewhere), None, "{elsewhere}");
        }
        assert_eq!(
            advertised("--listen 127.0.0.1:7485 --advertise lab-vault"),
            Some("lab-vault".into())
        );
        assert_eq!(
            advertised("--advertise lab-vault"),
            Some("lab-vault".into())
        );
        assert_eq!(advertised("--no-advertise"), None);
        assert_eq!(
            parse(&words("server on --no-advertise")),
            Ok(Command::Server {
                action: Action::On,
                program: None,
                serve: vec!["--no-advertise".into()],
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_rotation_of_the_alab_vault_names_both_lines_the_next_release_changes() {
        let key = |n: u8| crate::b64::encode_padded(&[n; 32]);
        let note = unix::release_note(&crate::client::AlabRelease {
            signer: key(3),
            retired: vec![key(1), key(2)],
        });
        assert!(
            note.contains(&format!(
                "\n  pub const ALAB_SIGNER: &str = \"{}\";\n",
                key(3)
            )),
            "{note}"
        );
        assert!(
            note.ends_with(&format!(
                "\n  pub const ALAB_RETIRED: &[&str] = &[\"{}\", \"{}\"];\n",
                key(1),
                key(2)
            )),
            "{note}"
        );
    }

    #[test]
    fn server_on_passes_serve_flags_through() {
        assert_eq!(
            parse(&words(
                "server on --advertise alab-secrets --program /x/aterm --no-keep-awake"
            )),
            Ok(Command::Server {
                action: Action::On,
                program: Some("/x/aterm".into()),
                serve: vec![
                    "--advertise".into(),
                    "alab-secrets".into(),
                    "--no-keep-awake".into()
                ],
            })
        );
        assert!(parse(&words("server on --bogus")).is_err());
    }

    #[test]
    fn unknown_commands_and_options_are_usage_errors() {
        assert!(parse(&words("frobnicate")).is_err());
        assert!(parse(&words("--aterm-front-door-routing-probe")).is_err());
        assert!(parse(&words("sync --fast")).is_err());
        assert!(parse(&words("approve")).is_err());
        assert!(parse(&words("admins promote m3")).is_err());
    }

    /// `join`'s last line and `status`'s membership line for a waiting Mac say how a
    /// mistyped join password is fixed, beside what an admin runs.
    #[cfg(unix)]
    #[test]
    fn a_waiting_machine_is_told_how_a_mistyped_join_password_is_fixed() {
        use crate::transport::Source;
        let code = "abcd-efgh-ijkm-npqr";
        let alab = Source::Url(crate::ALAB_DEFAULT_URL.into());
        let hint = unix::retype(&alab, None, false).expect("a URL has a join queue");
        assert!(
            hint.contains("mistyped join password is fixed by running aterm secrets join again"),
            "{hint}"
        );
        for line in [
            unix::waiting_line("m9", code, Some(&hint)),
            unix::pending_membership(code, Some(&hint)),
        ] {
            assert!(
                line.contains(&format!("aterm secrets approve {code}")),
                "{line}"
            );
            assert!(line.contains(&hint), "{line}");
        }
        assert!(!unix::waiting_line("m9", code, None).contains("mistyped"));
        // A bare join asks for the ALab vault under this Mac's default name. Another
        // vault needs its full configured URL; the hint never substitutes a redacted URL.
        let other = unix::retype(&Source::Url("http://h:1/v1/x".into()), Some("m9"), false)
            .expect("a URL has a join queue");
        assert!(
            other.contains("running aterm secrets join <vault-url> --name m9 again"),
            "{other}"
        );
        let private = Source::Url("https://h/v1/x?token=private-query#private-pin".into());
        assert_eq!(unix::source_text(&private), "https://h/v1/x");
        for piped in [false, true] {
            let hint = unix::retype(&private, Some("m9"), piped).expect("a URL");
            assert!(hint.contains("<vault-url>"), "{hint}");
            assert!(hint.contains("full configured URL"), "{hint}");
            assert!(
                !hint.contains("private-query") && !hint.contains("private-pin"),
                "{hint}"
            );
        }
        // A password that came piped is piped again: without a terminal, a join not given it
        // asks by code alone.
        let piped = unix::retype(&alab, None, true).expect("a URL has a join queue");
        assert!(
            piped.contains("piping it again to aterm secrets join --password-stdin"),
            "{piped}"
        );
        // A vault at a path has no join queue, so no password to mistype.
        assert_eq!(
            unix::retype(&Source::Path("/v/x.vault".into()), None, false),
            None
        );
    }

    /// An admin of a new vault at a path under `root`.
    #[cfg(unix)]
    fn admin_of_a_path_vault(root: &std::path::Path) -> crate::client::Paths {
        let shared = root.join("shared").join("team.vault");
        std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
        let admin = crate::client::Paths::for_home(&root.join("ma"));
        crate::client::create(&admin, shared.to_str().expect("utf-8"), None, Some("ma"))
            .expect("create");
        admin
    }

    /// The edit directories left under `paths`.
    #[cfg(unix)]
    fn edit_dirs(paths: &crate::client::Paths) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&paths.dir)
            .expect("state directory")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("edit-"))
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn an_editor_that_fails_on_the_published_text_leaves_no_draft() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let admin = admin_of_a_path_vault(root.path());
        let nothing_asked = |_: &str| -> Option<String> { panic!("nothing to ask") };
        let error = unix::edit_with(&admin, &mut |_: &std::path::Path| Ok(false), &nothing_asked)
            .expect_err("the editor failed");
        assert!(error.0.contains("nothing published"), "{error}");
        let error = unix::edit_with(
            &admin,
            &mut |_: &std::path::Path| Err(crate::Error::new("synthetic-editor: not found")),
            &nothing_asked,
        )
        .expect_err("the editor did not start");
        assert!(error.0.contains("synthetic-editor"), "{error}");
        assert_eq!(edit_dirs(&admin), Vec::<std::path::PathBuf>::new());
        // A draft the failed editor changed may be the only copy of new work: it stays.
        let error = unix::edit_with(
            &admin,
            &mut |file: &std::path::Path| {
                let mut draft = std::fs::OpenOptions::new()
                    .append(true)
                    .open(file)
                    .expect("draft");
                std::io::Write::write_all(&mut draft, b"# new work\n").expect("write");
                Ok(false)
            },
            &nothing_asked,
        )
        .expect_err("the editor failed");
        assert!(error.0.contains("the draft is kept"), "{error}");
        assert_eq!(edit_dirs(&admin).len(), 1);
    }

    /// An edit on a new vault at a path under `root`: the person adds `note` while another
    /// admin lets `mb` in and sets `other`, so the save is refused; reopened, the person quits
    /// without saving and answers `publish` when asked to publish the draft as it is. The
    /// admin, the seq the edit began on, the exit code and the prompts asked.
    #[cfg(unix)]
    fn edit_reopened_unchanged(
        root: &std::path::Path,
        publish: &str,
    ) -> (crate::client::Paths, u64, ExitCode, Vec<String>) {
        let admin = admin_of_a_path_vault(root);
        let began = crate::client::admin_open(&admin).expect("open").seq();
        let mut opened = 0;
        let asked = std::cell::RefCell::new(Vec::new());
        let code = unix::edit_with(
            &admin,
            &mut |file: &std::path::Path| {
                opened += 1;
                if opened == 1 {
                    let mut draft = std::fs::read_to_string(file).expect("draft");
                    draft.push_str("[everyone]\nnote=synthetic-note\n");
                    std::fs::write(file, draft).expect("save");
                    let other = crate::client::admin_open(&admin).expect("open");
                    let mut doc = other.doc.clone();
                    let mb = crate::age::Identity::generate().expect("identity");
                    doc.add_machine("mb", &mb.recipient().to_bech32())
                        .expect("add");
                    doc.set_entry(
                        &crate::text::Audience::Everyone,
                        "other",
                        b"synthetic-other",
                    )
                    .expect("set");
                    crate::client::admin_publish(&admin, &other, &doc).expect("publish");
                }
                // Reopened after the warning, the person quits without saving.
                Ok(true)
            },
            &|prompt: &str| {
                asked.borrow_mut().push(prompt.to_string());
                Some(
                    if prompt.contains("as it is") {
                        publish
                    } else {
                        ""
                    }
                    .to_string(),
                )
            },
        )
        .expect("edit");
        assert_eq!(opened, 2, "the editor reopened once");
        (admin, began, code, asked.into_inner())
    }

    #[cfg(unix)]
    #[test]
    fn a_draft_reopened_unchanged_on_a_vault_that_moved_publishes_only_when_the_person_says_so() {
        let everyone = crate::text::Audience::Everyone;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        // Quitting the reopened editor and pressing return publishes nothing: the draft is
        // kept, and only the other admin's version is the vault's.
        let (admin, began, code, asked) = edit_reopened_unchanged(&root.path().join("a"), "");
        assert_eq!(code, ExitCode::FAILURE);
        assert!(asked.iter().any(|p| p.contains("as it is")), "{asked:?}");
        let after = crate::client::admin_open(&admin).expect("open");
        assert_eq!(after.seq(), began + 1, "only the other admin's version");
        assert!(after.doc.entry(&everyone, "other").is_some());
        assert!(after.doc.entry(&everyone, "note").is_none());
        assert_eq!(edit_dirs(&admin).len(), 1, "the draft is kept");
        // Told to, the same draft publishes over that version with no change to make up: its
        // text replaces the version's entries, and the machine it let in is kept.
        let (admin, began, code, _) = edit_reopened_unchanged(&root.path().join("b"), "y");
        assert_eq!(code, ExitCode::SUCCESS);
        let after = crate::client::admin_open(&admin).expect("open");
        assert_eq!(after.seq(), began + 2, "the edit went on top");
        assert!(after.doc.entry(&everyone, "note").is_some());
        assert!(after.doc.entry(&everyone, "other").is_none());
        assert!(after.doc.machines().iter().any(|m| m.name == "mb"));
        assert_eq!(edit_dirs(&admin), Vec::<std::path::PathBuf>::new());
    }

    /// A key approved with `--name` gets that name or none: a name that is not a machine name
    /// is a usage error, never a generic `machine` in its place.
    #[test]
    fn a_key_is_never_approved_under_a_name_it_was_not_given() {
        // A code-shaped name would be a code for `admins`, `revoke` and `set --for`.
        for refused in [
            "M9",
            "join",
            "everyone",
            "-m9",
            "m9 extra",
            "abcd-efgh-ijkm-npqr",
        ] {
            let line = vec![
                "approve".to_string(),
                "age1xyz".to_string(),
                "--name".to_string(),
                refused.to_string(),
            ];
            let why = parse(&line).expect_err(refused);
            assert!(why.contains("--name"), "{refused}: {why}");
        }
        assert!(parse(&words("approve age1xyz --name m9")).is_ok());
    }

    /// A value piped to `set` loses its line end only when it is one line of text; a binary
    /// value that happens to end in LF (or CR LF) keeps every byte, as `--file` keeps it.
    #[cfg(unix)]
    #[test]
    fn a_piped_value_loses_its_line_end_only_when_it_is_one_line_of_text() {
        let trimmed = |bytes: &[u8]| {
            let mut bytes = bytes.to_vec();
            unix::trim_piped_line(&mut bytes);
            bytes
        };
        assert_eq!(trimmed(b"token\n"), b"token");
        assert_eq!(trimmed(b"token\r\n"), b"token");
        assert_eq!(trimmed("t\u{f6}ken\n".as_bytes()), "t\u{f6}ken".as_bytes());
        for kept in [
            &b"two\nlines\n"[..],
            b"no line end",
            b"\xff\x00\x41\n",
            b"\x80\x0d\x0a",
            b"\x9f\x13\xc0\xfe\x0a",
            b"a\x00b\n",
            b"a\rb\n",
        ] {
            assert_eq!(trimmed(kept), kept, "{kept:?}");
        }
    }

    /// `revoke` and `admins remove` name everything the machine could read: an admin read the
    /// whole text in the admin locker, every host section's entries too; any machine read
    /// `[everyone]`'s locker beside its own sections', so an `[everyone]` value its section
    /// overrides is named as well.
    #[cfg(unix)]
    #[test]
    fn letting_a_machine_go_names_everything_it_could_read() {
        use crate::text::{Audience, Document};
        let key = || {
            crate::age::Identity::generate()
                .expect("a key")
                .recipient()
                .to_bech32()
        };
        let mut doc = Document::new_vault("m22", &key());
        for name in ["m5", "m3", "build"] {
            doc.add_machine(name, &key()).expect("machine");
        }
        doc.add_admin("m5").expect("admin");
        let hosts = |name: &str| Audience::Hosts(vec![name.to_string()]);
        for (audience, name) in [
            (Audience::Everyone, "gh_token"),
            (Audience::Everyone, "npm_token"),
            (hosts("build"), "deploy_key"),
            (hosts("m3"), "signing_key"),
            (hosts("m3"), "npm_token"),
        ] {
            doc.set_entry(&audience, name, b"synthetic").expect("set");
        }
        let admin = unix::exposure(&doc, "m5").expect("an admin read everything");
        assert!(
            admin.contains(
                "m5 was an admin, so it could read every secret (5: gh_token, npm_token, deploy_key [build], signing_key [m3], npm_token [m3])"
            ),
            "{admin}"
        );
        let m3 = unix::exposure(&doc, "m3").expect("m3 read its own and everyone's");
        assert!(
            m3.contains(
                "m3 could read 4 secrets (gh_token, npm_token, signing_key [m3], npm_token [m3])"
            ),
            "{m3}"
        );
        assert!(!m3.contains("deploy_key"), "{m3}");
        assert_eq!(
            unix::exposure(&Document::new_vault("m22", &key()), "m22"),
            None,
            "nothing to read, nothing to rotate"
        );
    }

    /// A JOIN CODE TYPED FOR A MACHINE (`revoke`, `set|unset --for`, as `admins`) reaches the
    /// machine whose key has it, never one an older approver let in under that code as its
    /// name; a name passes as typed, and a code no key has is refused, even when a machine is
    /// so named.
    #[cfg(unix)]
    #[test]
    fn a_code_typed_for_a_machine_reaches_the_key_that_has_it() {
        use crate::text::Document;
        let key = || crate::age::Identity::generate().expect("a key").recipient();
        let mac = key();
        let code = crate::client::join_code(&mac);
        let mut doc = Document::new_vault("m22", &key().to_bech32());
        doc.add_machine("mb", &mac.to_bech32()).expect("machine");
        let typed = [code.clone(), code.to_uppercase(), "m3".to_string()];
        assert_eq!(
            unix::machines_named(&doc, &typed).expect("named"),
            ["mb", "mb", "m3"]
        );
        assert!(unix::machines_named(&doc, &["aaaa-bbbb-cccc-dddd".to_string()]).is_err());

        // A machine an older approver let in under another's code as its name: the code is
        // refused, naming both, since either may be meant.
        let mut both = doc.clone();
        let stranger = key();
        both.add_machine(&code, &stranger.to_bech32())
            .expect("a code is a valid machine name in a text");
        let error = unix::machines_named(&both, std::slice::from_ref(&code))
            .expect_err("either machine may be meant");
        assert!(
            error.0.contains("mb") && error.0.contains(&stranger.to_bech32()),
            "{error}"
        );
        // Named with a code no key has, a machine is not reached by it (meant for a machine
        // not yet in, it would reach a stranger): the refusal gives its own code, which is.
        let unkeyed = "aaaa-bbbb-cccc-dddd".to_string();
        let named = key();
        doc.add_machine(&unkeyed, &named.to_bech32())
            .expect("machine");
        let error = unix::machines_named(&doc, std::slice::from_ref(&unkeyed))
            .expect_err("a code no key has");
        let own = crate::client::join_code(&named);
        assert!(error.0.contains(&own), "{error}");
        assert_eq!(
            unix::machines_named(&doc, &[own]).expect("its own code"),
            [unkeyed]
        );
    }

    /// `status` of a machine of a signed vault names it by its code: the name it asked for
    /// may be another machine's in the vault. A plain text file is read under that name.
    #[cfg(unix)]
    #[test]
    fn status_names_a_machine_of_a_signed_vault_by_its_code() {
        use crate::transport::Source;
        let config = |signer: Option<[u8; 32]>| crate::client::Config {
            source: Source::Path("/v/team.vault".into()),
            name: "team".to_string(),
            signer,
            followed: Vec::new(),
            host: "m5".to_string(),
        };
        let code = "abcd-efgh-ijkm-npqr";
        let signed = unix::machine_line(Some(&config(Some([7; 32]))), "age1k", code);
        assert_eq!(
            signed,
            format!("machine: code {code}, key age1k, asked to join as m5")
        );
        let plain = unix::machine_line(Some(&config(None)), "age1k", code);
        assert_eq!(plain, format!("machine: m5, code {code}, key age1k"));
        assert_eq!(
            unix::machine_line(None, "age1k", code),
            format!("machine: code {code}, key age1k")
        );
    }

    /// A subscriber played with an inert arm: `state` says its state and `install` stands in
    /// for an install (each test counts them), never a launchd job or a systemd unit.
    #[cfg(unix)]
    fn played<'a>(
        state: &'a dyn Fn() -> crate::Result<crate::service::State>,
        install: &'a dyn Fn() -> crate::Result<String>,
    ) -> unix::Subscriber<'a> {
        unix::Subscriber { state, install }
    }

    #[cfg(unix)]
    fn installed(pid: Option<u32>) -> crate::service::State {
        crate::service::State::Installed {
            pid,
            unit: std::path::PathBuf::from("/played/systems.alab.aterm-secrets.plist"),
        }
    }

    /// The approver goes in where it does not run: a subscriber that is not installed, or one
    /// installed with no process, is installed; a running one is left be; a state that cannot
    /// be read installs nothing (an install asks the same service manager).
    #[cfg(unix)]
    #[test]
    fn the_approver_is_put_in_only_where_it_does_not_run() {
        use crate::service::State;
        use unix::Approver;
        let installs = std::cell::Cell::new(0);
        let install = || {
            installs.set(installs.get() + 1);
            Ok("played: installed".to_string())
        };
        let refused = || -> crate::Result<String> {
            installs.set(installs.get() + 1);
            Err(crate::Error::new("played: no launchd"))
        };
        let running = || Ok(installed(Some(42)));
        assert_eq!(
            unix::put_in_approver(&played(&running, &install)),
            Approver::Running
        );
        assert_eq!(installs.get(), 0, "a running subscriber is left be");
        let stopped = || Ok(installed(None));
        let absent = || Ok(State::Absent);
        for state in [&stopped as &dyn Fn() -> crate::Result<State>, &absent] {
            assert_eq!(
                unix::put_in_approver(&played(state, &install)),
                Approver::Installed("played: installed".into())
            );
        }
        assert_eq!(installs.get(), 2);
        assert_eq!(
            unix::put_in_approver(&played(&absent, &refused)),
            Approver::Failed("played: no launchd".into())
        );
        let unreadable = || Err(crate::Error::new("played: launchctl hung"));
        assert_eq!(
            unix::put_in_approver(&played(&unreadable, &install)),
            Approver::Unknown("played: launchctl hung".into())
        );
        assert_eq!(installs.get(), 3, "an unreadable state installs nothing");
        // `--no-agent` installs nothing, but a subscriber already running still approves.
        assert_eq!(
            unix::approver_here(false, &played(&running, &install)),
            Approver::Running
        );
        for state in [
            &stopped as &dyn Fn() -> crate::Result<State>,
            &absent,
            &unreadable,
        ] {
            assert_eq!(
                unix::approver_here(false, &played(state, &install)),
                Approver::Skipped
            );
        }
        assert_eq!(
            unix::approver_here(true, &played(&absent, &install)),
            Approver::Installed("played: installed".into())
        );
        assert_eq!(
            installs.get(),
            4,
            "only the one asked to put it in installs"
        );
    }

    /// Where no approver runs on this Mac, a command that needs one says so, and that `aterm
    /// secrets agent on` starts one; one it installed shows its install line; one that ran
    /// already says nothing.
    #[cfg(unix)]
    #[test]
    fn a_command_whose_approver_does_not_run_says_what_starts_one() {
        use crate::client::Approval;
        use unix::Approver;
        assert_eq!(
            unix::approver_words(&Approver::Running, Approval::Password),
            None
        );
        assert_eq!(
            unix::approver_words(&Approver::Installed("line".into()), Approval::Open),
            Some("line".into())
        );
        for (approver, why) in [
            (Approver::Skipped, "(--no-agent)"),
            (Approver::Failed("played".into()), "not installed: played"),
            (Approver::Unknown("played".into()), "unknown (played)"),
        ] {
            for (approval, who) in [
                (
                    Approval::Password,
                    "a machine that types the join password waits",
                ),
                (Approval::Open, "a machine that asks to join waits"),
            ] {
                let words = unix::approver_words(&approver, approval).expect("said");
                assert!(words.contains(why), "{words}");
                assert!(words.contains(who), "{words}");
                assert!(
                    words.contains("aterm secrets agent on starts one"),
                    "{words}"
                );
                assert_eq!(
                    words.starts_with("warning: "),
                    approver != Approver::Skipped,
                    "{words}"
                );
            }
        }
        assert!(Approver::Running.runs() && Approver::Installed(String::new()).runs());
        assert!(!Approver::Skipped.runs() && !Approver::Failed(String::new()).runs());
    }

    /// A vault at a path has no join queue, so nothing lets a machine in there by itself:
    /// where `password` would say an open vault lets in every machine that asks, or a join
    /// password lets in whoever types it, it says a machine joins only by an admin's approval
    /// of its key. A vault served at a URL says what an open one lets in, and leaves a
    /// password to the approver's line; a closed one says nothing either way.
    #[cfg(unix)]
    #[test]
    fn a_vault_at_a_path_says_a_machine_joins_it_by_key() {
        let mut open = crate::text::Document::new_vault("ma", &crate::transport::test_recipient(1));
        let mut password = open.clone();
        let closed = open.clone();
        open.set_open(true).expect("open");
        password
            .set_join_password(Some(b"a-long-enough-join-password"))
            .expect("password");
        for doc in [&open, &password] {
            let words = unix::lets_in_words(false, doc).expect("said");
            assert!(words.contains("no join queue"), "{words}");
            assert!(
                words.contains("aterm secrets approve <age1…> --name <host>"),
                "{words}"
            );
            assert!(!words.contains("let in"), "{words}");
        }
        assert_eq!(unix::lets_in_words(false, &closed), None);
        let words = unix::lets_in_words(true, &open).expect("said");
        assert!(
            words.contains("every machine that reaches its server"),
            "{words}"
        );
        assert_eq!(unix::lets_in_words(true, &password), None);
        assert_eq!(unix::lets_in_words(true, &closed), None);
    }

    /// `create` puts this Mac's approver in, as `join` puts the subscriber in, so a machine
    /// that types the new vault's join password is let in; `--no-agent` installs nothing, but
    /// reads the subscriber, which, already running, approves for the new vault too; a failed
    /// install leaves the vault made (exit 0) with a warning. A vault at a path has no join
    /// queue, so nothing to approve: no subscriber is read or installed for one.
    #[cfg(unix)]
    #[test]
    fn create_puts_in_the_approver_unless_told_not_to() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let server = crate::server::start(
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().expect("addr"),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .expect("a loopback server");
        let installs = std::cell::Cell::new(0);
        let install = || {
            installs.set(installs.get() + 1);
            Ok("played: installed".to_string())
        };
        let refused = || -> crate::Result<String> {
            installs.set(installs.get() + 1);
            Err(crate::Error::new("played: no launchd"))
        };
        let absent = || Ok(crate::service::State::Absent);
        let unread = || -> crate::Result<crate::service::State> {
            panic!("this create reads no subscriber")
        };
        let reads = std::cell::Cell::new(0);
        let running = || {
            reads.set(reads.get() + 1);
            Ok(installed(Some(42)))
        };
        let create = |dir: &str, agent: bool, subscriber: &unix::Subscriber<'_>| {
            let source = if dir == "path" {
                let shared = root.path().join(dir).join("team.vault");
                std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
                shared.to_str().expect("utf-8").to_string()
            } else {
                format!("http://{}/v1/{dir}", server.addr)
            };
            let admin = crate::client::Paths::for_home(&root.path().join(dir).join("ma"));
            let code = unix::create(&admin, &source, None, Some("ma".into()), agent, subscriber)
                .expect("created");
            assert_eq!(code, ExitCode::SUCCESS);
            crate::client::admin_open(&admin).expect("an admin of it");
        };
        create("a", true, &played(&absent, &install));
        assert_eq!(installs.get(), 1, "create installs the subscriber");
        create("b", false, &played(&absent, &install));
        assert_eq!(installs.get(), 1, "--no-agent installs nothing");
        create("d", false, &played(&running, &install));
        assert_eq!(reads.get(), 1, "--no-agent reads whether the approver runs");
        assert_eq!(installs.get(), 1, "a running subscriber is left be");
        create("c", true, &played(&absent, &refused));
        assert_eq!(installs.get(), 2, "a failed install still makes the vault");
        create("path", true, &played(&unread, &install));
        assert_eq!(
            installs.get(),
            2,
            "a vault at a path has nothing to approve"
        );
        server.stop();
    }

    /// A vault server that takes a vault's first version and then fails every read, as one that
    /// goes away between `create`'s publish and its own sync: a GET before the PUT is 404, the
    /// PUT 204, a GET after it 503. It answers until [`LostAfterThePut::end`].
    #[cfg(unix)]
    struct LostAfterThePut {
        url: String,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        addr: std::net::SocketAddr,
        server: std::thread::JoinHandle<()>,
    }

    #[cfg(unix)]
    impl LostAfterThePut {
        fn start() -> LostAfterThePut {
            use std::io::{Read, Write};
            use std::sync::atomic::Ordering;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("address");
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let seen = std::sync::Arc::clone(&stop);
            let server = std::thread::spawn(move || {
                let mut put = false;
                for stream in listener.incoming() {
                    if seen.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut stream) = stream else {
                        continue;
                    };
                    // A hang detector, not a latency budget.
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(60)));
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => break,
                        }
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    let _ = stream.read_exact(&mut body);
                    let status = if head.starts_with("PUT ") {
                        put = true;
                        "204 No Content"
                    } else if put {
                        "503 Service Unavailable"
                    } else {
                        "404 Not Found"
                    };
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    );
                }
            });
            LostAfterThePut {
                url: format!("http://{addr}/v1/team"),
                stop,
                addr,
                server,
            }
        }

        /// Answer no more: the connection this makes wakes the server to see it.
        fn end(self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            std::net::TcpStream::connect(self.addr).expect("the server is woken");
            self.server.join().expect("the server ends");
        }
    }

    /// A create whose own sync fails once the vault is published (its server gone between the
    /// two) is no failure: the vault is out and source.toml names it, and a create run again
    /// would be refused ("already exists there"). The approver still goes in, and its
    /// subscriber syncs the vault; client::create says why this Mac's copy is not synced.
    #[cfg(unix)]
    #[test]
    fn a_create_whose_own_sync_fails_still_puts_the_approver_in() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let installs = std::cell::Cell::new(0);
        let install = || {
            installs.set(installs.get() + 1);
            Ok("played: installed".to_string())
        };
        let absent = || Ok(crate::service::State::Absent);
        let lost = LostAfterThePut::start();
        let admin = crate::client::Paths::for_home(&root.path().join("ma"));
        let code = unix::create(
            &admin,
            &lost.url,
            None,
            Some("ma".into()),
            true,
            &played(&absent, &install),
        )
        .expect("created");
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(installs.get(), 1, "the approver goes in");
        let status = crate::client::status(&admin).expect("status");
        assert_eq!(
            status.config.map(|config| config.source),
            Some(crate::transport::Source::Url(lost.url.clone()))
        );
        assert_eq!(
            status.membership,
            crate::client::Membership::Pending,
            "not synced yet"
        );
        lost.end();

        let lost = LostAfterThePut::start();
        let other = crate::client::Paths::for_home(&root.path().join("mb"));
        let created = crate::client::create(&other, &lost.url, None, Some("mb")).expect("created");
        assert!(
            created
                .local
                .as_ref()
                .is_some_and(|why| why.contains("503")),
            "{:?}",
            created.local
        );
        lost.end();
    }

    /// What leaves a vault letting machines in by itself (`password open`, as `set` and `new`)
    /// puts this Mac's approver in where none runs, so what it prints holds; closing it
    /// (`off`) reads no subscriber. `status` says whether the approver runs only on an admin
    /// of a vault that approves by itself, read off the version this Mac holds, and only for
    /// a vault with a join queue (a URL's; a path has none, so nothing to approve).
    #[cfg(unix)]
    #[test]
    fn a_vault_that_lets_machines_in_gets_an_approver_and_status_says_whether_it_runs() {
        use crate::client::{Approval, approving_here};
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let server = crate::server::start(
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().expect("addr"),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .expect("a loopback server");
        let installs = std::cell::Cell::new(0);
        let install = || {
            installs.set(installs.get() + 1);
            Ok("played: installed".to_string())
        };
        let absent = || Ok(crate::service::State::Absent);
        let running = || Ok(installed(Some(42)));
        let unread = || -> crate::Result<crate::service::State> {
            panic!("closing the vault reads no subscriber")
        };
        let admin = crate::client::Paths::for_home(&root.path().join("ma"));
        let url = format!("http://{}/v1/team", server.addr);
        unix::create(
            &admin,
            &url,
            None,
            Some("ma".into()),
            true,
            &played(&running, &install),
        )
        .expect("created");
        assert_eq!(installs.get(), 0, "a running subscriber is left be");
        assert_eq!(approving_here(&admin), Ok(Some(Approval::Password)));
        let password = |action, subscriber: &unix::Subscriber<'_>| {
            unix::password(&admin, action, true, subscriber).expect("published")
        };
        password(Password::Open, &played(&absent, &install));
        assert_eq!(installs.get(), 1, "opening puts the approver in");
        assert_eq!(approving_here(&admin), Ok(Some(Approval::Open)));
        password(Password::Open, &played(&absent, &install));
        assert_eq!(
            installs.get(),
            2,
            "open again, it still says the vault lets in"
        );
        password(Password::Open, &played(&running, &install));
        assert_eq!(installs.get(), 2, "a running approver is left be");
        // `--no-agent`: an admin whose subscriber was turned off on purpose keeps it off.
        unix::password(&admin, Password::Open, false, &played(&absent, &install))
            .expect("published");
        assert_eq!(installs.get(), 2, "--no-agent installs nothing");
        password(Password::Off, &played(&unread, &install));
        assert_eq!(approving_here(&admin), Ok(None), "closed, nothing approves");
        password(Password::Off, &played(&unread, &install));
        assert_eq!(installs.get(), 2);
        server.stop();
        // A machine that is no admin, or has no vault, has no approver to report; nor does
        // an admin of a vault at a path, whatever its [join] says.
        let stranger = crate::client::Paths::for_home(&root.path().join("stranger"));
        assert_eq!(approving_here(&stranger), Ok(None));
        let at_a_path = admin_of_a_path_vault(&root.path().join("path"));
        assert_eq!(approving_here(&at_a_path), Ok(None));
        let no_queue =
            || -> crate::Result<crate::service::State> { panic!("a path has no queue to approve") };
        unix::password(
            &at_a_path,
            Password::Open,
            true,
            &played(&no_queue, &install),
        )
        .expect("published");
        assert_eq!(installs.get(), 2, "nor is one put in there");

        let line = |approval, state| unix::approver_line(approval, &state);
        let ok = line(Approval::Password, Ok(installed(Some(42))));
        assert!(ok.contains("running in the subscriber (pid 42)"), "{ok}");
        assert!(ok.contains("types the join password is let in"), "{ok}");
        for (state, why) in [
            (
                Ok(crate::service::State::Absent),
                "no subscriber is installed",
            ),
            (Ok(installed(None)), "installed but not running"),
            (Err(crate::Error::new("played")), "unknown (played)"),
        ] {
            let said = line(Approval::Open, state);
            assert!(said.contains(why), "{said}");
            assert!(said.contains("a machine that asks to join waits"), "{said}");
            assert!(said.contains("aterm secrets agent on starts one"), "{said}");
        }
    }
}
