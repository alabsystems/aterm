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

  aterm secrets [status]                 where this machine's vault is and what it holds
  aterm secrets join [<url>|<path>]      join a vault (Enter-equivalent: the ALab default)
        [--name <host>] [--password-stdin] [--no-agent] [--no-wait]
  aterm secrets sync                     fetch and apply the latest version now
  aterm secrets watch                    the subscriber: apply every new version (launchd runs it)
  aterm secrets ls                       the names this machine holds
  aterm secrets get <name>               print one value to stdout
admin (a machine in [admins]):
  aterm secrets create <url>|<path> [--import <dir>] [--name <host>]
  aterm secrets edit                     the vault text in $EDITOR; saving publishes it
  aterm secrets set <name> [--for <host>...] [--file <path>]
        [--if-absent [--with <name> | --no-with]] [--fleet-reads-anchors]
                                         (the value from stdin or the file; --with: filled
                                         only where that if-absent entry is not the machine's own;
                                         set again, an entry keeps its anchor unless --no-with)
  aterm secrets unset <name> [--for <host>...]
  aterm secrets password [show|set|new|open|off]   the join password a joining machine
                                         types, or open: every machine that asks is let in
  aterm secrets requests                 machines asking to join
  aterm secrets approve <code>... | --all | <age1...> --name <host>
  aterm secrets revoke <host> [--drop-sections]
  aterm secrets admins add|remove <host>   (remove also rotates the vault key)
  aterm secrets rotate-key               sign with a new vault key the current one endorses
server and services:
  aterm secrets serve [--listen <addr:port>] [--advertise <name>] [--pin <vault>=<key>]
        [--store <dir>] [--hold <1-60 secs>] [--no-keep-awake]
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
    Password(Password),
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
const SERVE_SWITCHES: &[&str] = &["--no-keep-awake"];

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
    let listen = listen
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
    Ok(ServeSettings {
        listen,
        advertise,
        pins,
        store: f.one("--store"),
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
            let f = flags(rest, &["--import", "--name"], &[])?;
            match f.operands.as_slice() {
                [source] => Ok(Command::Create {
                    source: source.clone(),
                    import: f.one("--import"),
                    name: f.one("--name"),
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
            let f = flags(rest, &[], &[])?;
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
            Ok(Command::Password(action))
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
                _ => Err("admins takes add <host> or remove <host>".into()),
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
        self, JoinOptions, JoinOutcome, Membership, Paths, Published, SyncOutcome,
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
        match command {
            Command::Status => status(&paths),
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
            } => create(&paths, &source, import, name),
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
            Command::Password(action) => password(&paths, action),
            Command::Requests => requests(&paths),
            Command::Approve { which, all, name } => approve(&paths, &which, all, name),
            Command::Revoke {
                host,
                drop_sections,
            } => revoke(&paths, &host, drop_sections),
            Command::Admins { add, host } => {
                let admin = client::admin_open(&paths)?;
                let mut doc = admin.doc.clone();
                if add {
                    doc.add_admin(&host).map_err(text_error)?;
                    let done = client::admin_publish(&paths, &admin, &doc)?;
                    Ok(published(&format!("added {host} to the admins"), &done))
                } else {
                    if !doc.remove_admin(&host).map_err(text_error)? {
                        return Err(Error::new(format!("{host} is not an admin")));
                    }
                    // Every admin holds the vault key: the one removed must not keep signing.
                    let done = client::admin_rotate(&paths, &admin, &doc)?;
                    Ok(published(&format!("removed {host} from the admins"), &done))
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
        match service::state(&paths.home, job) {
            Ok(service::State::Absent) => "not installed".into(),
            Ok(service::State::Installed { pid: Some(pid), .. }) => format!("running (pid {pid})"),
            Ok(service::State::Installed { pid: None, .. }) => "installed, not running".into(),
            Err(error) => format!("unknown ({error})"),
        }
    }

    fn status(paths: &Paths) -> crate::Result<ExitCode> {
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
            let host = status
                .config
                .as_ref()
                .map_or("this machine", |c| c.host.as_str());
            println!(
                "machine: {host}, code {code}, key {}",
                recipient.to_bech32()
            );
        }
        match status.membership {
            Membership::Unconfigured => {}
            Membership::Pending => {
                let code = status
                    .machine
                    .as_ref()
                    .map_or("?", |(_, code)| code.as_str());
                // Whether a join password went with the request is not kept, so a vault with
                // a join queue always says how a mistyped one is fixed. (A path has none, so
                // its default machine name is never looked up.)
                let hint = status
                    .config
                    .as_ref()
                    .filter(|config| matches!(config.source, Source::Url(_)))
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
            println!("subscriber: {}", service_line(paths, Job::Subscriber));
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
        )?;
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

    fn create(
        paths: &Paths,
        source: &str,
        import: Option<String>,
        name: Option<String>,
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
        println!(
            "a joining machine that types the join password is let in by this Mac's subscriber: aterm secrets password shows it"
        );
        if !created.imported.is_empty() {
            println!("imported into [everyone]: {}", created.imported.join(", "));
        }
        Ok(ExitCode::SUCCESS)
    }

    fn expand(paths: &Paths, text: &str) -> PathBuf {
        match text.strip_prefix("~/") {
            Some(rest) => paths.home.join(rest),
            None => PathBuf::from(text),
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
        let mut admin = client::admin_open(paths)?;
        // The version the text began as: a machine the vault admits after it (the approver,
        // another admin) is one the person never saw, and is put back into what they save.
        let base = admin.doc.clone();
        let scratch = client::Scratch::new(paths)?;
        let file = scratch
            .dir()
            .join(format!("{}.vault.txt", admin.config.name));
        let original = base.render();
        fsx::write_atomic(&file, original.as_slice(), 0o600)?;
        eprintln!(
            "draft: {} (kept if this edit does not publish)",
            file.display()
        );
        let editor = editor()?;
        loop {
            let mut cmd = aterm_uds::spawnfd::command(&editor[0]);
            cmd.args(&editor[1..]).arg(&file);
            let status = cmd
                .status()
                .map_err(|e| Error::new(format!("{}: {e}", editor[0])))?;
            if !status.success() {
                return Err(Error::new("the editor failed; nothing published"));
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
                    // replaces it, all but the machines admitted since, which it keeps.
                    match client::admin_open(paths) {
                        Ok(fresh) if fresh.seq() != admin.seq() => {
                            let added: Vec<String> = client::admitted_since(&base, &fresh.doc)
                                .into_iter()
                                .map(|machine| machine.name)
                                .collect();
                            let added = if added.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    " (machines added since, which saving keeps: {})",
                                    added.join(", ")
                                )
                            };
                            eprintln!(
                                "the vault moved from seq {} to seq {} while you edited; saving again publishes your text over it, so bring its changes in first{added}",
                                admin.seq(),
                                fresh.seq()
                            );
                            admin = fresh;
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
                // One line piped in (`echo token | …`) loses its newline.
                let body_end = bytes.len().saturating_sub(1);
                if bytes.last() == Some(&b'\n') && !bytes[..body_end].contains(&b'\n') {
                    bytes.pop();
                    if bytes.last() == Some(&b'\r') {
                        bytes.pop();
                    }
                }
                SecretBytes::new(bytes)
            }
        };
        if value.as_slice().is_empty() {
            return Err(Error::new(format!(
                "the value is empty; aterm secrets unset {name} removes an entry"
            )));
        }
        let admin = client::admin_open(paths)?;
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

    fn password(paths: &Paths, action: Password) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let mut doc = admin.doc.clone();
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
                    eprintln!("{OPEN_VAULT}");
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
                    println!("{OPEN_VAULT}");
                    return Ok(ExitCode::SUCCESS);
                }
                // Those already waiting are a person's to judge: one may be a machine let go
                // while the vault was closed, its approved request still queued. Not let in
                // here, each joins only by code (the publish names them).
                let waiting = client::requests(paths, Some(&doc))?;
                if !waiting.is_empty() && interactive() {
                    eprintln!("these machines asked to join before the vault opened:");
                    for request in &waiting {
                        let when = request.at.map(ago).unwrap_or_default();
                        eprintln!("  {}  code {}  {when}", request.host, request.code);
                    }
                    let question = format!(
                        "Let these {} in now? Any you do not know may be a machine you let go. Each reads every [everyone] secret. [y/N] ",
                        waiting.len()
                    );
                    if yes(ask(&question)) {
                        for request in &waiting {
                            let host = client::free_name(&doc, &request.host);
                            doc.add_machine(&host, &request.recipient)
                                .map_err(text_error)?;
                        }
                    }
                }
                doc.set_open(true).map_err(text_error)?;
                let done = client::admin_publish(paths, &admin, &doc)?;
                let code = published("opened the vault", &done);
                println!("{OPEN_VAULT}");
                eprintln!("note: {OLDER_ADMINS}");
                Ok(code)
            }
            Password::New => {
                let password = join::generate()?;
                doc.set_join_password(Some(password.as_slice()))
                    .map_err(text_error)?;
                let done = client::admin_publish(paths, &admin, &doc)?;
                let code = published("set a new join password", &done);
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
                Ok(published("set the join password", &done))
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
        let mut chosen: Vec<(String, String)> = Vec::new();
        if all {
            if pending.is_empty() {
                println!("no machines are asking to join");
                return Ok(ExitCode::SUCCESS);
            }
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
            chosen.extend(
                pending
                    .iter()
                    .map(|r| (r.recipient.clone(), r.host.clone())),
            );
        }
        for word in which {
            if word.starts_with("age1") {
                crate::age::Recipient::parse(word)
                    .map_err(|_| Error::new(format!("{word} is not a valid age key")))?;
                let host = name
                    .clone()
                    .ok_or_else(|| Error::new("approving a key needs --name <host>"))?;
                chosen.push((word.clone(), host));
                continue;
            }
            match pending.iter().find(|r| &r.code == word) {
                Some(request) => chosen.push((request.recipient.clone(), request.host.clone())),
                None => {
                    return Err(Error::new(format!(
                        "no machine waiting to join has code {word} (aterm secrets requests)"
                    )));
                }
            }
        }
        chosen.sort();
        chosen.dedup_by(|a, b| a.0 == b.0);
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

    fn revoke(paths: &Paths, host: &str, drop_sections: bool) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let mut doc = admin.doc.clone();
        let held: Vec<String> = doc.entries_for(host).into_iter().map(|e| e.name).collect();
        if !doc
            .remove_machine(host, drop_sections)
            .map_err(text_error)?
        {
            return Err(Error::new(format!("no machine {host} in the vault")));
        }
        let done = client::admin_publish(paths, &admin, &doc)?;
        let code = published(&format!("revoked {host}"), &done);
        if !held.is_empty() {
            println!(
                "{host} could read {} secrets ({}); rotate them if it was lost or compromised",
                held.len(),
                held.join(", ")
            );
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
            .map_or_else(|| paths.served(), |dir| expand(paths, dir));
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
            Ok(Command::Password(Password::Show))
        );
        assert_eq!(
            parse(&words("password new")),
            Ok(Command::Password(Password::New))
        );
        assert_eq!(
            parse(&words("password off")),
            Ok(Command::Password(Password::Off))
        );
        assert_eq!(
            parse(&words("password open")),
            Ok(Command::Password(Password::Open))
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
        ] {
            assert!(parse(&words(&refused)).is_err(), "{refused}");
        }
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
}
