// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets`.

use std::ffi::OsString;
use std::process::ExitCode;

/// The verb's help.
pub const USAGE: &str = "\
aterm secrets: one credentials file for every machine (aterm help secrets).

  aterm secrets [status]                 this machine's vault, join code, membership,
                                         secrets, subscriber and server, and on an admin
                                         whether the approver runs
  aterm secrets join [<url>|<path>] [--name <host>] [--password-stdin]
        [--no-agent] [--no-wait]         join a vault (neither: the ALab vault; a trailing
                                         #<signer key> pins its signer, else confirm the
                                         fingerprint at a terminal), waiting up to 10 min
                                         for approval at a terminal unless --no-wait, and
                                         install the subscriber unless --no-agent
  aterm secrets sync                     fetch and apply the latest version now
  aterm secrets watch                    the subscriber (agent on installs it): apply each
                                         new version; on an admin of a <url> vault, also
                                         the approver, which lets in a machine that types
                                         the join password (any that asks, when open)
  aterm secrets ls                       the names this machine holds
  aterm secrets get <name>               print one value to stdout
admin (a machine in [admins]):
  aterm secrets create <url>|<path> [--import <dir>] [--name <host>]
        [--no-agent] [--serve]           a new open vault (at a <url> each machine that asks
                                         is let in until `password new` or `off` closes it;
                                         at a <path> an admin approves each key), this
                                         machine its first admin (--import: each regular
                                         file in <dir> with a valid entry name goes into
                                         [everyone]); for a <url>, also install the
                                         subscriber where it does not run, unless
                                         --no-agent, and print the `join` line other
                                         machines paste; --serve: first set up this
                                         machine's server on the <url>'s port
  aterm secrets edit                     the vault text in $VISUAL or $EDITOR (else vi);
                                         saving publishes it
  aterm secrets set <name> [--for <host|code>...] [--file <path>]
        [--if-absent [--with <name> | --no-with]] [--fleet-reads-anchors]
                                         the value from stdin or the file; --if-absent: only
                                         where the machine has no file of its own; --with:
                                         and only where that if-absent entry is not the
                                         machine's own (set again, an entry keeps its anchor
                                         unless --no-with); the vault's first anchored entry
                                         needs --fleet-reads-anchors
  aterm secrets unset <name> [--for <host|code>...]
  aterm secrets password [show|set|new|open|off] [--no-agent]
                                         the join password: show (the default), set (typed
                                         or piped), new (generated), open (none: every
                                         machine that asks is let in), off (close the vault
                                         or drop the password: machines wait for approval
                                         by code); show and new print it only at a
                                         terminal; for a <url>, set, new and open also
                                         install the subscriber where it does not run,
                                         unless --no-agent
  aterm secrets requests                 machines asking to join, with their codes
  aterm secrets approve <code>... | --all | <age1...> --name <host>
                                         let in machines by the code each shows (--all:
                                         every one, asked at a terminal), or a key by name
                                         (how a machine joins a <path> vault)
  aterm secrets revoke <host|code> [--drop-sections]
                                         remove it from the vault (first retiring its Commons
                                         membership, where the vault has Commons control); a
                                         section naming only it must be empty unless
                                         --drop-sections drops it too
  aterm secrets retire <host|code|age1...>... permanently retire Commons membership alone;
                                         a full age key can name a device already removed
  aterm secrets admins add|remove <host|code>   (remove also rotates the vault key)
  aterm secrets rotate-key               sign with a new vault key the current one endorses
  aterm secrets commons init             initialize Commons on this existing vault, sharing its
                                         control descriptor with every current vault machine
                                         and approving each one with no admission record;
                                         retries finish the same network and never replace a root
server and services:
  aterm secrets serve [--listen <addr:port>] [--advertise <name> | --no-advertise]
        [--pin <vault>=<key>] [--store <dir>] [--hold <1-60 secs>] [--no-keep-awake]
                                         serve vaults (defaults: 0.0.0.0:7484, the store
                                         ~/.config/aterm/secrets/served, long-polls held
                                         25 s; a relative --store is under ~). A vault the
                                         store lacks takes its first version only from this
                                         machine, unless --pin names the key that signs it
                                         (alab's is built in). On macOS only, it keeps the
                                         Mac from idle sleep unless --no-keep-awake, and
                                         advertises <name>.local over mDNS: by default, on
                                         every address at port 7484, the ALab vault's name
                                         once the store holds the ALab vault (a new ALab
                                         server: --advertise alab-secrets; --no-advertise:
                                         none)
  aterm secrets agent [on|off|status] [--program <path>] [--force]
                                         the subscriber as a launchd or systemd user job
                                         (--program: the aterm it runs, else this aterm;
                                         on leaves one running as set up, unless --force)
  aterm secrets server [on|off|status] [--program <path>] [serve flags]
                                         serve as a launchd or systemd user job
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
        serve: bool,
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
    Retire(Vec<String>),
    Admins {
        add: bool,
        host: String,
    },
    RotateKey,
    CommonsInit,
    Serve(Vec<String>),
    Agent {
        action: Action,
        program: Option<String>,
        force: bool,
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
    /// `--if-absent`: written only where the machine has no file of its own (none, or one
    /// still holding what the vault wrote).
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
    advertise: Advertise,
    pins: Vec<(String, [u8; 32])>,
    store: Option<String>,
    /// `None`: the server's default.
    hold: Option<std::time::Duration>,
    keep_awake: bool,
}

/// The name `serve` registers over mDNS, as its flags give it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Advertise {
    /// `--advertise <name>`.
    Named(String),
    /// Neither flag, on macOS, answering where the ALab URL points (every address, port 7484):
    /// the ALab vault's name, once the store holds the ALab vault ([`alab_role`]).
    AlabIfHeld,
    /// `--no-advertise`, or a server answering elsewhere (a check, a test, another OS).
    Off,
}

/// What a server's store and its Mac say of the ALab vault ([`alab_role`]).
#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlabHere {
    /// The store holds the ALab vault ([`crate::server::holds_alab`]) and this Mac follows it
    /// (its pin is of the ALab vault's line): ALab's server.
    Server,
    /// The store holds a copy of the ALab vault, but this Mac does not follow it: a copy an ALab
    /// Mac put back here before servers stopped taking one they did not hold.
    Copy,
    /// The store holds no ALab vault.
    Empty,
}

/// What `serve` advertises and whether it pins the ALab vault's name to its built-in key, from
/// its flags and what its store and Mac say of the ALab vault (`here`). Both belong to ALab's
/// server only: one told so (`--advertise alab-secrets`, the way a new ALab server with an empty
/// store starts) or one whose store holds the ALab vault on a Mac that follows it. Any other
/// server answering `alab-secrets.local` would take ALab Macs' copies of the vault (a subscriber
/// that finds no vault there puts its own back) and serve them as ALab's on every network it
/// goes to, where a new Mac then reads as ALab's. The second value is a line for the log when
/// the ALab name is held back.
#[cfg(any(unix, test))]
fn alab_role(
    advertise: Advertise,
    here: AlabHere,
    pins: &mut Vec<(String, [u8; 32])>,
    alab_key: Option<[u8; 32]>,
) -> (Option<String>, Option<String>) {
    let alab_name = crate::alab_advertised();
    let holds_alab = here == AlabHere::Server;
    let why = match here {
        AlabHere::Server => "",
        AlabHere::Copy => {
            "this store holds a copy of the ALab vault, but this Mac does not follow the ALab vault"
        }
        AlabHere::Empty => "that is the ALab vault's name, and this store holds no ALab vault",
    };
    let (advertised, held_back) = match advertise {
        Advertise::Named(name) => (Some(name), None),
        Advertise::AlabIfHeld if holds_alab => (Some(alab_name.to_string()), None),
        Advertise::AlabIfHeld => (
            None,
            Some(format!(
                "not advertising {alab_name}.local: {why} (ALab's server starts with --advertise {alab_name}; another vault's members reach this Mac by its own name, or by a name --advertise gives)"
            )),
        ),
        Advertise::Off => (None, None),
    };
    if (holds_alab || advertised.as_deref() == Some(alab_name))
        && !pins.iter().any(|(name, _)| name == crate::ALAB_VAULT)
        && let Some(key) = alab_key
    {
        pins.push((crate::ALAB_VAULT.to_string(), key));
    }
    (advertised, held_back)
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
            "--advertise {name}: a host label of lowercase letters, digits and -, not machines, \
             admins, everyone or join"
        ));
    }
    let answers_default_url = listen.ip().is_unspecified() && listen.port() == crate::DEFAULT_PORT;
    let advertise = match (advertise, f.has("--no-advertise")) {
        (Some(_), true) => return Err("--advertise and --no-advertise are both given".into()),
        (None, true) => Advertise::Off,
        (Some(name), false) => Advertise::Named(name),
        (None, false) if cfg!(target_os = "macos") && answers_default_url => Advertise::AlabIfHeld,
        (None, false) => Advertise::Off,
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
            let f = flags(rest, &["--import", "--name"], &["--no-agent", "--serve"])?;
            match f.operands.as_slice() {
                [source] => Ok(Command::Create {
                    source: source.clone(),
                    import: f.one("--import"),
                    name: f.one("--name"),
                    agent: !f.has("--no-agent"),
                    serve: f.has("--serve"),
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
        "retire" => {
            if !rest.is_empty() && rest.iter().all(|subject| !subject.starts_with('-')) {
                Ok(Command::Retire(rest.to_vec()))
            } else {
                Err("retire takes hosts, codes, or full age recipients".into())
            }
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
        "commons" => {
            let f = flags(rest, &[], &[])?;
            match f.operands.as_slice() {
                [verb] if verb == "init" => Ok(Command::CommonsInit),
                _ => Err("commons takes init".into()),
            }
        }
        "serve" => {
            let f = flags(rest, SERVE_VALUED, SERVE_SWITCHES)?;
            no_operands(&f, "serve")?;
            serve_settings(&f)?;
            Ok(Command::Serve(rest.to_vec()))
        }
        "agent" => {
            let f = flags(rest, &["--program"], &["--force"])?;
            if f.operands.len() > 1 {
                return Err("agent takes on, off or status".into());
            }
            Ok(Command::Agent {
                action: action(f.operands.first(), "agent")?,
                program: f.one("--program"),
                force: f.has("--force"),
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
        Action, Command, Fill, Password, SERVE_SWITCHES, SERVE_VALUED, alab_role, flags,
        serve_settings,
    };
    use crate::client::{
        self, Approval, JoinEvent, JoinOptions, JoinOutcome, JoinPassword, Membership, Paths,
        Published, SyncOutcome,
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
    const OPEN_VAULT: &str = "this vault is open: every machine that reaches its server and asks to join is let in within seconds, with no password or code, and reads [everyone]'s entries; a key you let go comes back only by its code, but a machine that makes a new key asks as a new machine (aterm secrets password set closes the vault)";

    /// What a vault at a path says where one served at a URL says whom its `[join]` lets in: a
    /// path has no join queue, so nothing lets a machine in by itself there.
    const PATH_JOINS: &str = "a vault at a path has no join queue: a machine joins it only when an admin approves its key: aterm secrets approve <age1…> --name <host>";

    /// What a publish that let a machine go from an open vault says: only its key is kept out,
    /// since an open vault lets in any new key that reaches the server.
    const KEPT_OUT: &str = "the vault stays open, but not to the key you let go: it is a `code-only` line in [join], so that key is let back in only by its code (aterm secrets approve). The vault still lets in any new key that reaches its server, so if the machine was lost or compromised, close the vault now (aterm secrets password set), let go any machine in [machines] (aterm secrets edit) it may have joined as since, even under its old name, and only then rotate what it could read";

    /// [`KEPT_OUT`] for a vault at a path: only an admin's approve of a key lets a machine in
    /// there, which a `code-only` line does not stop. A server may still serve the file
    /// (`server::take_from_store`), so the text gives [`KEPT_OUT`]'s steps too.
    const KEPT_OUT_AT_A_PATH: &str = "at a path no server serves, the key you let go comes back only when an admin approves it (aterm secrets approve <age1…> --name <host>): if the machine was lost or compromised, rotate what it could read. If aterm secrets serve serves this file, it serves this version from its next request, and the open vault still lets in any new key that reaches it: if the machine was lost or compromised, close the vault now (aterm secrets password set), let go any machine in [machines] (aterm secrets edit) it may have joined as since, even under its old name, and only then rotate what it could read";

    /// What marks a request whose key a `code-only` line names, where an open vault's requests
    /// are listed.
    const CODE_ONLY_MARK: &str =
        "  (code only: a machine let go, or one that asked before the vault opened)";

    /// [`CODE_ONLY_MARK`] on a closed vault, where the line admits and refuses nothing: a
    /// password or a code still lets that key in.
    const LET_GO_MARK: &str = "  (a machine let go, or one that asked before the vault opened)";

    /// The mark of a request from a key on a `code-only` line of `doc`.
    pub(super) fn held_mark(doc: &text::Document) -> &'static str {
        if doc.open() {
            CODE_ONLY_MARK
        } else {
            LET_GO_MARK
        }
    }

    /// What opening a vault says of admins on older builds, which cannot read its lines.
    const OLDER_ADMINS: &str =
        "an admin Mac on an aterm older than open vaults cannot open this vault until it updates";

    pub(super) fn run(command: Command) -> crate::Result<ExitCode> {
        let paths = Paths::live()?;
        let state = || service::state(&paths.home, Job::Subscriber);
        // A command that needs an approver turns the subscriber on (clearing an off record)
        // but keeps a program the job names that lasts ([`service::kept_program`]): only
        // `agent on` and a join move it to this aterm.
        let install = || {
            let program = match service::kept_program_that_starts(&paths.home, Job::Subscriber)? {
                Some(program) => program,
                None => service::program_for_service()?,
            };
            install_subscriber(&paths, Some(program))
        };
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
                client::watch(&paths, &|line| {
                    crate::log_line(&format!("{} {line}", stamp()));
                })?;
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
                serve,
            } => {
                if serve {
                    serve_here(&paths, &source)?;
                }
                create(&paths, &source, import, name, agent, &subscriber)
            }
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
            Command::Retire(subjects) => {
                let admin = client::admin_open(&paths)?;
                let retired = client::admin_retire(&paths, &admin, &subjects)?;
                println!(
                    "Commons membership permanently retired ({} recipients, {} prior device bindings).",
                    retired.recipients, retired.bindings
                );
                Ok(ExitCode::SUCCESS)
            }
            Command::Admins { add, host } => {
                let admin = client::admin_open(&paths)?;
                let mut doc = admin.doc.clone();
                let host = client::machine_named(&doc, &host)?;
                if add {
                    doc.add_admin(&host).map_err(text_error)?;
                    let done = client::admin_publish_admin(&paths, &admin, &doc, &host)?;
                    Ok(published(&format!("added {host} to the admins"), &done))
                } else {
                    let held = exposure(&doc, &host);
                    if !doc.remove_admin(&host).map_err(text_error)? {
                        return Err(Error::new(format!("{host} is not an admin")));
                    }
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
            Command::CommonsInit => {
                let done = client::admin_commons_init(&paths)?;
                println!(
                    "Commons ready: network {} authority {} genesis {}; {} vault machines eligible; {}",
                    aterm_codec::hex::encode_lower(&done.network),
                    aterm_codec::hex::encode_lower(&done.authority),
                    aterm_codec::hex::encode_lower(&done.genesis),
                    done.eligible,
                    if done.published {
                        "foundation published"
                    } else {
                        "existing foundation confirmed"
                    }
                );
                if !done.approved.is_empty() {
                    println!("approved for Commons: {}", done.approved.join(", "));
                }
                Ok(ExitCode::SUCCESS)
            }
            Command::Serve(args) => serve(&paths, &args),
            Command::Agent {
                action,
                program,
                force,
            } => services(
                &paths,
                Job::Subscriber,
                (action, force),
                program,
                vec!["secrets".into(), "watch".into()],
            ),
            Command::Server {
                action,
                program,
                serve,
            } => services(
                &paths,
                Job::Server,
                (action, false),
                program,
                serve_args(serve),
            ),
        }
    }

    fn stamp() -> String {
        format!("[{}]", aterm_time::unix_secs().unwrap_or(0))
    }

    fn text_error(error: text::TextError) -> Error {
        Error::new(error.to_string())
    }

    /// The machines `words` name in `doc`, each by [`client::machine_named`].
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

    /// How long `join` waits for the approval: ten minutes at a terminal, unless `--no-wait`.
    /// Without one (a script, an agent's tool) it returns at once with the approve command, and
    /// the subscriber finishes the join.
    pub(super) fn join_wait(wait: bool, interactive: bool) -> Duration {
        if wait && interactive {
            Duration::from_secs(600)
        } else {
            Duration::ZERO
        }
    }

    /// What `password show` refuses with where nobody reads the terminal: the join password
    /// admits a machine within seconds, so it never lands in a file or an agent's transcript.
    const SHOWN_AT_A_TERMINAL: &str = "the join password is shown only at a terminal; approve the new Mac with aterm secrets approve <code>";

    /// What `password new` says in place of the password it made where nobody reads the
    /// terminal ([`SHOWN_AT_A_TERMINAL`]).
    const NEW_SHOWN_AT_A_TERMINAL: &str =
        "a new join password is set; it is shown only at a terminal (aterm secrets password there)";

    /// `password show`: `password` on `out` when it is a terminal and no agent runs this
    /// ([`aterm_types::domain::agent_marker`]), else nothing on it and a refusal.
    pub(super) fn show_password(
        out: &mut dyn Write,
        at_terminal: bool,
        agent: Option<&str>,
        password: &SecretBytes,
    ) -> crate::Result<ExitCode> {
        if !at_terminal || agent.is_some() {
            eprintln!("{SHOWN_AT_A_TERMINAL}");
            return Ok(ExitCode::FAILURE);
        }
        out.write_all(password.as_slice())?;
        out.write_all(b"\n")?;
        out.flush()?;
        Ok(ExitCode::SUCCESS)
    }

    /// `password new`'s password on `out`, as [`show_password`] shows one: only at a terminal
    /// no agent runs, else nothing on it and a line saying where it is shown.
    pub(super) fn tell_new_password(
        out: &mut dyn Write,
        at_terminal: bool,
        agent: Option<&str>,
        password: &SecretBytes,
    ) -> std::io::Result<()> {
        if !at_terminal || agent.is_some() {
            eprintln!("{NEW_SHOWN_AT_A_TERMINAL}");
            return Ok(());
        }
        out.write_all(b"join password: ")?;
        out.write_all(password.as_slice())?;
        out.write_all(b"\n")?;
        out.flush()
    }

    /// Print what a publish did, including any key rotation, held requests, or local sync warning.
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
            println!(
                "{}",
                if done.queued {
                    KEPT_OUT
                } else {
                    KEPT_OUT_AT_A_PATH
                }
            );
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

    /// The `ALAB_SIGNER` and `ALAB_RETIRED` lines a rotation of the ALab vault asks of the next
    /// release.
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

    /// Warning printed when a vault's first version with two handoff pairs is published.
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
    /// Ctrl-C arrives as a byte and the settings are put back before the process ends.
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

    /// Read one line typed at the terminal without echo (`None` with no terminal; Ctrl-C exits
    /// 130 after restoring terminal settings).
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

    /// How a Mac waiting to join the vault at `source` fixes a mistyped join password.
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

    /// `join`'s status line for a Mac awaiting approval of `code`, with the [`retype`] hint if any.
    pub(super) fn waiting_line(
        host: &str,
        code: &str,
        approve: &str,
        retype: Option<&str>,
    ) -> String {
        let line =
            format!("waiting for approval of {host} (code {code}): on an admin Mac run {approve}");
        match retype {
            Some(retype) => format!("{line}; {retype}"),
            None => line,
        }
    }

    /// `status`'s membership line for a Mac waiting for an admin to run `approve`
    /// ([`Status::approve_command`](client::Status::approve_command)), with [`retype`] for a
    /// vault with a join queue.
    pub(super) fn pending_membership(approve: &str, retype: Option<&str>) -> String {
        let line = format!("membership: waiting for approval (on an admin Mac: {approve})");
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

    /// This Mac's subscriber, where an admin's approver runs.
    pub(super) struct Subscriber<'a> {
        /// Its state now ([`service::state`]).
        pub(super) state: &'a dyn Fn() -> crate::Result<service::State>,
        /// Install and start it ([`install_subscriber`]).
        pub(super) install: &'a dyn Fn() -> crate::Result<String>,
    }

    /// What became of this Mac's approver where a command needs one to keep what it promises.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum Approver {
        /// The subscriber was running already, so it was left be.
        Running,
        /// The subscriber was installed now.
        Installed(String),
        /// `--no-agent`.
        Skipped,
        /// The install failed.
        Failed(String),
        /// The subscriber's state could not be read.
        Unknown(String),
    }

    impl Approver {
        /// Whether an approver runs on this Mac now.
        pub(super) const fn runs(&self) -> bool {
            matches!(self, Approver::Running | Approver::Installed(_))
        }
    }

    /// Put this Mac's approver in.
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

    /// This Mac's approver for a command that needs one.
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

    /// What `password` says of whom a vault whose `[join]` is `doc`'s lets in by itself.
    pub(super) fn lets_in_words(queued: bool, doc: &text::Document) -> Option<&'static str> {
        if !queued {
            return Approval::of(doc).map(|_| PATH_JOINS);
        }
        doc.open().then_some(OPEN_VAULT)
    }

    /// Who an approver for `approval` lets in.
    const fn joiner(approval: Approval) -> &'static str {
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

    /// What a command that needs this Mac's approver says of it, for a vault that approves by
    /// `approval`.
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

    /// `status`'s approver line: `approving`, whether and how this Mac approves as `status` reads
    /// it, and `stuck`, what stops the running subscriber's approver: what it recorded
    /// ([`client::approver_stuck`]) or, for an approver from before that record, the release of
    /// its program as `status` reads it ([`client::approver_cannot_read`]). A subscriber stuck on
    /// what `status` cannot read either still says so.
    pub(super) fn approver_status(
        approving: &crate::Result<Option<Approval>>,
        subscriber: &crate::Result<service::State>,
        stuck: Option<&str>,
    ) -> Option<String> {
        match (approving, subscriber, stuck) {
            (Ok(Some(approval)), _, _) => Some(approver_line(*approval, subscriber, stuck)),
            // `status` cannot read this Mac's role either, so the line claims no approval.
            (Err(_), Ok(service::State::Installed { pid: Some(pid), .. }), Some(why)) => {
                Some(format!(
                    "approver: the subscriber (pid {pid}) cannot tell whether this Mac approves joins: {why}"
                ))
            }
            _ => None,
        }
    }

    /// `status`'s line for an admin of a vault that approves by `approval`, `stuck` what stops
    /// the running subscriber's approver ([`approver_status`]).
    pub(super) fn approver_line(
        approval: Approval,
        subscriber: &crate::Result<service::State>,
        stuck: Option<&str>,
    ) -> String {
        match subscriber {
            Ok(service::State::Installed { pid: Some(pid), .. }) => match stuck {
                Some(why) => format!(
                    "approver: running in the subscriber (pid {pid}) but not approving: {why}; until it approves again, {} waits for approval by code",
                    joiner(approval)
                ),
                None => format!(
                    "approver: running in the subscriber (pid {pid}), so {} is let in within seconds",
                    joiner(approval)
                ),
            },
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

    /// `status`'s line for this machine, `key` its `age1…` key and `code` its join code.
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
                    "no vault configured: aterm secrets join [<url>|<path>] (neither: {})",
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
                let approve = status
                    .approve_command()
                    .unwrap_or_else(|| "aterm secrets approve ?".to_owned());
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
                println!("{}", pending_membership(&approve, hint.as_deref()));
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
            let stuck = match &state {
                Ok(service::State::Installed { pid: Some(pid), .. }) => {
                    client::approver_stuck(paths, *pid)
                        .or_else(|| client::approver_cannot_read(paths, *pid))
                }
                _ => None,
            };
            let approving = client::approving_here(paths);
            if let Some(line) = approver_status(&approving, &state, stuck.as_deref()) {
                println!("{line}");
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

    /// Record `--no-agent` as the person's choice ([`service::subscriber_off_path`]) unless a
    /// subscriber runs; a record that cannot be written is a warning.
    pub(super) fn record_no_agent(
        paths: &Paths,
        state: &dyn Fn() -> crate::Result<service::State>,
    ) {
        if matches!(state(), Ok(service::State::Installed { pid: Some(_), .. })) {
            return;
        }
        if let Err(error) = service::record_subscriber_off(&paths.home) {
            eprintln!("warning: the choice to run no subscriber was not recorded ({error})");
        }
    }

    fn install_subscriber(paths: &Paths, program: Option<PathBuf>) -> crate::Result<String> {
        let program = match program {
            Some(program) => program,
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
        let from_pipe = piped.is_some();
        let given = Cell::new(from_pipe);
        let ask_password = || -> Option<SecretBytes> {
            let password = ask_hidden(
                "Join password (an admin Mac shows it: aterm secrets password; Enter joins without one: an open vault lets this Mac in, any other waits for approval by code): ",
            )
            .filter(|typed| !typed.as_slice().trim_ascii().is_empty());
            given.set(password.is_some());
            password
        };
        let password = match piped {
            Some(piped) => JoinPassword::Given(Some(piped)),
            None if interactive => JoinPassword::Ask(&ask_password),
            None => JoinPassword::Given(None),
        };
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
        let say = |event: &JoinEvent<'_>| eprintln!("{event}");
        let before = client::config(paths).ok().flatten();
        let outcome = client::join(
            paths,
            &JoinOptions {
                source: source.to_string(),
                host: name.clone(),
                interactive,
                wait: join_wait(wait, interactive),
                confirm: &confirm,
                password,
                fetched: None,
                requested: &install,
                say: &say,
            },
        )
        .inspect_err(|_| {
            // A join that fails after saving the source (the server gone before the request
            // was queued) leaves this Mac waiting with its request due at once, so the
            // subscriber that asks goes in now, as once a request is queued.
            if client::config(paths)
                .ok()
                .flatten()
                .is_some_and(|after| before.as_ref() != Some(&after))
            {
                install();
            }
        })?;
        if !agent {
            record_no_agent(paths, &|| service::state(&paths.home, Job::Subscriber));
        }
        match &outcome {
            JoinOutcome::Joined { seq, names } => {
                println!("joined: seq {seq}, {} secrets in ~/.secrets", names.len());
            }
            JoinOutcome::Pending { code, host } => {
                let hint = if given.get() {
                    client::config(paths)
                        .ok()
                        .flatten()
                        .and_then(|config| retype(&config.source, name.as_deref(), from_pipe))
                } else {
                    None
                };
                let approve = client::status(paths)
                    .ok()
                    .and_then(|status| status.approve_command())
                    .unwrap_or_else(|| format!("aterm secrets approve {code}"));
                println!("{}", waiting_line(host, code, &approve, hint.as_deref()));
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
        let held = |served: Option<u64>| {
            served.map_or("no vault".to_string(), |served| {
                format!("seq {served}, older than this Mac's")
            })
        };
        let mut outcome = client::sync(paths, None)?;
        // A put-back writes no file here: files still unsettled are applied from the copy the
        // source holds now. The first line says what the sync did, as before (a caller reads it).
        let mut put_back = None;
        if let SyncOutcome::PutBack { served, .. } = outcome
            && !client::materialization_ready(paths)?
        {
            put_back = Some(served);
            outcome = client::sync(paths, None)?;
        }
        let first_put_back = || {
            if let Some(served) = put_back {
                println!(
                    "  the source held {}: put this Mac's copy back first",
                    held(served)
                );
            }
        };
        match outcome {
            SyncOutcome::Applied { seq, report } => {
                // The harness reads this line's seq and its failed count (`Vault::of_sync`).
                println!(
                    "applied seq {seq}: {} written, {} unchanged, {} removed, {} left in place, {} moved aside, {} failed",
                    report.written.len(),
                    report.unchanged.len(),
                    report.removed.len(),
                    report.left.len(),
                    report.moved_aside.len(),
                    report.failed.len()
                );
                first_put_back();
                for (name, to) in &report.moved_aside {
                    println!("  {name}: the previous file was moved to {}", to.display());
                }
                for (name, why) in &report.failed {
                    println!("  {name}: failed: {why}");
                }
                Ok(if report.failed.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                })
            }
            SyncOutcome::Unchanged { seq } => {
                println!("up to date at seq {seq}");
                first_put_back();
                Ok(ExitCode::SUCCESS)
            }
            SyncOutcome::PutBack { seq, served } => {
                println!(
                    "the source held {}: put this Mac's copy back; up to date at seq {seq}",
                    held(served)
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
        let value = get_value(paths, name)?;
        let mut out = std::io::stdout().lock();
        out.write_all(value.as_slice())?;
        out.flush()?;
        Ok(ExitCode::SUCCESS)
    }

    pub(super) fn get_value(paths: &Paths, name: &str) -> crate::Result<crate::SecretBytes> {
        let expected = client::config(paths)?
            .ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
        if expected.signer.is_some() {
            return client::read_verified_entry(paths, &expected, name)?
                .map(|entry| entry.into_parts().0)
                .ok_or_else(|| {
                    Error::new("no verified materialized entry; run aterm secrets sync")
                });
        }
        let status = client::status(paths)?;
        if status.config.as_ref() != Some(&expected)
            || !matches!(status.membership, client::Membership::Plain)
        {
            return Err(Error::new(
                "no applied plaintext source; run aterm secrets sync",
            ));
        }
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
        Ok(value)
    }

    /// `create`, then, for a vault at a URL, this Mac's approver unless `--no-agent`
    /// ([`approver_here`]).
    pub(super) fn create(
        paths: &Paths,
        source: &str,
        import: Option<String>,
        name: Option<String>,
        agent: bool,
        subscriber: &Subscriber<'_>,
    ) -> crate::Result<ExitCode> {
        let import = import.map(|dir| expand(paths, &dir));
        let created = match client::create(paths, source, import.as_deref(), name.as_deref()) {
            Ok(created) => created,
            Err(error) => {
                // The server may have committed a PUT whose reply was lost.
                // A retained intent needs its subscriber even before the
                // first source.toml has been installed.
                if agent && client::creation_pending(paths).unwrap_or(false) {
                    let approver = approver_here(agent, subscriber);
                    if let Some(words) = approver_words(&approver, Approval::Open) {
                        eprintln!("{words}");
                    }
                    if approver.runs() {
                        eprintln!(
                            "creation is retained locally; this Mac's subscriber will retry it"
                        );
                    }
                }
                return Err(error);
            }
        };
        if !agent {
            record_no_agent(paths, subscriber.state);
        }
        println!("created vault {} (seq 1)", created.name);
        println!(
            "signer: {} (fingerprint {})",
            created.signer, created.fingerprint
        );
        let mut syncs = false;
        if let Some(Source::Url(url)) = client::config(paths)?.map(|config| config.source) {
            println!(
                "on each new Mac: {}",
                join_command(
                    &url,
                    &created.signer,
                    aterm_uds::process::lan_host_name().as_deref()
                )
            );
            // A retried create finishes the version its first try prepared, which a build before
            // open vaults made with a join password: say how the finished vault admits.
            let finished = client::admin_open(paths).ok();
            if let Some(password) = finished
                .as_ref()
                .and_then(|admin| admin.doc.join_password())
            {
                let mut out = std::io::stdout().lock();
                let at_terminal = out.is_terminal();
                let agent = aterm_types::domain::agent_marker(&|key| std::env::var(key).ok());
                tell_new_password(&mut out, at_terminal, agent, &password)?;
            }
            let approval = finished
                .as_ref()
                .map_or(Some(Approval::Open), |admin| Approval::of(&admin.doc));
            let approver = approver_here(agent, subscriber);
            if let Some(words) = approval.and_then(|approval| approver_words(&approver, approval)) {
                eprintln!("{words}");
            }
            let by = if approver.runs() {
                "by this Mac's subscriber"
            } else {
                "once this Mac's subscriber runs"
            };
            match approval {
                Some(Approval::Open) => {
                    println!("the vault is open: every machine that asks to join is let in {by}");
                    println!(
                        "to close it: aterm secrets password new (a join password) or password off (approval by code)"
                    );
                }
                Some(Approval::Password) => {
                    println!("a joining machine that types the join password is let in {by}");
                }
                None => println!(
                    "a joining machine waits for an admin to approve its key: aterm secrets approve <age1…> --name <host>"
                ),
            }
            syncs = approver.runs();
        } else {
            println!(
                "machines that join it must pin this key: add #{} to the URL, or confirm the fingerprint",
                created.signer
            );
            println!(
                "a machine joins it when an admin Mac approves its key: aterm secrets approve <age1…> --name <host>"
            );
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

    /// The command a person pastes on a new Mac to join the vault at `url` signed by `signer`:
    /// `url` as the other Macs reach it, a loopback host named by `lan` (this Mac's LAN name).
    pub(super) fn join_command(url: &str, signer: &str, lan: Option<&str>) -> String {
        let reached = match (aterm_http::Uri::parse(url), lan) {
            (Some(uri), Some(lan))
                if matches!(uri.host(), "127.0.0.1" | "localhost" | "[::1]" | "::1") =>
            {
                let (head, tail) = url.split_once(uri.host()).unwrap_or((url, ""));
                format!("{head}{lan}{tail}")
            }
            _ => url.to_owned(),
        };
        format!("aterm secrets join {reached}#{signer}")
    }

    /// `create --serve`: this Mac's server job set up and running as `server on` sets it up,
    /// answering on the port `source` names before the vault is made there.
    fn serve_here(paths: &Paths, source: &str) -> crate::Result<()> {
        let port = aterm_http::Uri::parse(source)
            .map(|uri| uri.port())
            .ok_or_else(|| {
                Error::new(format!(
                    "--serve serves the vault from this Mac: {source} is not a vault URL"
                ))
            })?;
        let mut serve = Vec::new();
        if port != crate::DEFAULT_PORT {
            serve = vec!["--listen".to_owned(), format!("0.0.0.0:{port}")];
        }
        services(
            paths,
            Job::Server,
            (Action::On, false),
            None,
            serve_args(serve),
        )?;
        Ok(())
    }

    /// `secrets serve` and the flags `serve` names: the server job's arguments.
    fn serve_args(serve: Vec<String>) -> Vec<String> {
        let mut args = vec!["secrets".to_string(), "serve".to_string()];
        args.extend(serve);
        args
    }

    /// How long `server on` waits for the job it started to answer: a hang detector.
    const ANSWER_HANG: Duration = Duration::from_secs(60);
    /// The longest one look at the starting server takes, and the pause after a refused one.
    const ANSWER_LOOK: Duration = Duration::from_secs(1);
    const ANSWER_STEP: Duration = Duration::from_millis(50);

    /// Wait until the server `serve`'s flags set up answers a GET on this Mac, so a vault can be
    /// made or joined there the moment `server on` returns.
    fn until_answering(serve: &[String]) -> crate::Result<()> {
        let f = flags(serve, SERVE_VALUED, SERVE_SWITCHES).map_err(Error::new)?;
        let settings = serve_settings(&f).map_err(Error::new)?;
        answering(&local_url(&settings), ANSWER_HANG)
    }

    /// Wait until a GET of `url` hears any answer from a vault server, for at most `hang`.
    pub(super) fn answering(url: &str, hang: Duration) -> crate::Result<()> {
        let deadline = std::time::Instant::now() + hang;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match crate::transport::get_within(url, left.clamp(ANSWER_STEP, ANSWER_LOOK)) {
                Ok(_) => return Ok(()),
                Err(error) if left <= ANSWER_STEP => {
                    return Err(Error::new(format!(
                        "the server did not answer at {url} within {}s: {error}",
                        hang.as_secs()
                    )));
                }
                Err(_) => std::thread::sleep(ANSWER_STEP),
            }
        }
    }

    /// The URL of a vault the server `settings` sets up answers on this Mac: its first pinned
    /// name, else the ALab vault's, at its listening address (loopback for all of them).
    pub(super) fn local_url(settings: &super::ServeSettings) -> String {
        let ip = match settings.listen.ip() {
            std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            }
            std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            }
            ip => ip,
        };
        let name = settings
            .pins
            .first()
            .map_or("alab", |(name, _)| name.as_str());
        format!(
            "http://{}/v1/{name}",
            std::net::SocketAddr::new(ip, settings.listen.port())
        )
    }

    fn expand(paths: &Paths, text: &str) -> PathBuf {
        match text.strip_prefix("~/") {
            Some(rest) => paths.home.join(rest),
            None => PathBuf::from(text),
        }
    }

    /// The store a `--store` names.
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

    /// [`edit`] past its terminal check.
    pub(super) fn edit_with(
        paths: &Paths,
        run: &mut dyn FnMut(&Path) -> crate::Result<bool>,
        ask: &dyn Fn(&str) -> Option<String>,
    ) -> crate::Result<ExitCode> {
        let mut admin = client::admin_open(paths)?;
        let base = admin.doc.clone();
        let scratch = client::Scratch::new(paths)?;
        let file = scratch
            .dir()
            .join(format!("{}.vault.txt", admin.config.name));
        let original = base.render();
        fsx::write_atomic(&file, original.as_slice(), 0o600)?;
        eprintln!(
            "draft: {} (kept unless the edit is published or leaves it unchanged)",
            file.display()
        );
        let mut reopened: Option<SecretBytes> = None;
        loop {
            let ran = run(&file);
            if !matches!(ran, Ok(true)) {
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

    /// `doc` with entry `name` set for `audience` as `set` sets it, and the anchor the entry got.
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

    /// A value piped in as one line of text (`echo token | …`) loses its line end.
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
            (true, None) => " (written only where the machine has no file of its own)".to_string(),
            (true, Some(anchor)) => format!(
                " (written only where the machine has no file of its own and {anchor} is not the machine's own)"
            ),
        };
        Ok(published(
            &format!("set {name} for {}{gaps}", audience(hosts).header()),
            &done,
        ))
    }

    /// `password <action>`: show the join password, set or generate one, open the vault, or
    /// turn either off; after set, new or open on a `<url>` vault, the approver goes in unless
    /// `--no-agent` ([`approver_here`]).
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
                    let at_terminal = out.is_terminal();
                    let agent = aterm_types::domain::agent_marker(&|key| std::env::var(key).ok());
                    show_password(&mut out, at_terminal, agent, &password)
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
                // Opening the vault's bulk prompt is not a pairing-code check
                // and does not create Manual Commons admission evidence.
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
                let at_terminal = out.is_terminal();
                let agent = aterm_types::domain::agent_marker(&|key| std::env::var(key).ok());
                tell_new_password(&mut out, at_terminal, agent, &password)?;
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

    /// What `password open` at a terminal does with the requests `waiting` as it opens the vault.
    pub(super) fn admit_on_opening(
        doc: &mut text::Document,
        waiting: Vec<client::Request>,
        say: &mut dyn FnMut(String),
        agree: &mut dyn FnMut(&str) -> bool,
    ) -> crate::Result<()> {
        let code_only = doc.code_only();
        let (mut offered, mut held, mut named) = (Vec::new(), Vec::new(), Vec::new());
        for request in &waiting {
            let listed = |list: &[&client::Request]| {
                list.iter()
                    .any(|r| r.recipient == request.recipient && r.host == request.host)
            };
            if listed(&offered) || listed(&held) || listed(&named) {
                continue;
            }
            if code_only.contains(&request.recipient) {
                held.push(request);
            } else if client::names_asked(&waiting, &request.recipient).len() > 1 {
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

    fn requests(paths: &Paths) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths).ok();
        let pending = client::requests(paths, admin.as_ref().map(|a| &a.doc))?;
        let code_only = admin
            .as_ref()
            .map(|a| a.doc.code_only())
            .unwrap_or_default();
        let mark = admin.as_ref().map_or(CODE_ONLY_MARK, |a| held_mark(&a.doc));
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
                mark
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

    pub(super) fn approve(
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
        let mut chosen = from_queue(&pending, which, all)?;
        if all {
            if !interactive() {
                return Err(Error::new(
                    "approve --all asks for a yes at a terminal; approve each code instead",
                ));
            }
            let code_only = doc.code_only();
            let mark = held_mark(&doc);
            for request in &pending {
                let held = if code_only.contains(&request.recipient) {
                    mark
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
        let mut recipients = Vec::with_capacity(chosen.len());
        for (recipient, host) in chosen {
            let host = if let Some(existing) = doc
                .machines()
                .into_iter()
                .find(|m| m.recipient == recipient)
            {
                if host != existing.name {
                    return Err(Error::new(format!(
                        "that key already belongs to {}; approve it with --name {}",
                        existing.name, existing.name
                    )));
                }
                existing.name
            } else {
                let host = client::free_name(&doc, &host);
                doc.add_machine(&host, &recipient).map_err(text_error)?;
                host
            };
            let code = crate::age::Recipient::parse(&recipient)
                .map(|r| client::join_code(&r))
                .unwrap_or_default();
            approved.push(format!("{host} (code {code})"));
            recipients.push(recipient);
        }
        let done = client::admin_publish_approved(paths, &admin, &doc, &recipients)?;
        // Bookkeeping only: the approver records the requests too once it reads this version.
        let _ = client::record_approved(paths, &pending, &recipients);
        Ok(published(
            &format!("approved {}", approved.join(", ")),
            &done,
        ))
    }

    /// What `host` could read in `doc`, said where `revoke` or `admins remove` lets it go.
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

    /// The `(key, host)` pairs `approve` takes from the queue, one per key.
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
            client::one_name_asked(pending, request)?;
            chosen.push((request.recipient.clone(), request.host.clone()));
        }
        chosen.sort();
        chosen.dedup();
        Ok(chosen)
    }

    fn revoke(paths: &Paths, host: &str, drop_sections: bool) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let host = &client::machine_named(&admin.doc, host)?;
        let held = exposure(&admin.doc, host);
        let done = client::admin_revoke(paths, &admin, host, drop_sections)?;
        let code = published(&format!("revoked {host}"), &done.published);
        if let Some(retirement) = done.retirement {
            println!(
                "Commons membership permanently retired ({} recipients, {} prior device bindings).",
                retirement.recipients, retirement.bindings
            );
        }
        if let Some(held) = held {
            println!("{held}");
        }
        Ok(code)
    }

    fn serve(paths: &Paths, args: &[String]) -> crate::Result<ExitCode> {
        let f = flags(args, SERVE_VALUED, SERVE_SWITCHES).map_err(Error::new)?;
        let mut settings = serve_settings(&f).map_err(Error::new)?;
        let store = settings
            .store
            .as_deref()
            .map_or_else(|| paths.served(), |dir| store_dir(paths, dir));
        let alab_key = crate::b64::decode_padded(crate::ALAB_SIGNER)
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
        // A copy alone does not make this ALab's server: its Mac must follow the ALab vault
        // too, by the vault's line, so a release that retires its key changes nothing here.
        let follows = client::config(paths).ok().flatten().is_some_and(|config| {
            config.signer.is_some_and(|signer| {
                crate::envelope::Retired::compiled().lineage(&signer, &config.followed)
            })
        });
        let here = match (crate::server::holds_alab(&store), follows) {
            (true, true) => super::AlabHere::Server,
            (true, false) => super::AlabHere::Copy,
            (false, _) => super::AlabHere::Empty,
        };
        let (advertise, held_back) = alab_role(
            settings.advertise.clone(),
            here,
            &mut settings.pins,
            alab_key,
        );
        crate::log_line(&format!(
            "{} serving on {}, store {}",
            stamp(),
            settings.listen,
            store.display()
        ));
        if let Some(line) = held_back {
            crate::log_line(&format!("{} {line}", stamp()));
        }
        crate::server::serve(crate::server::Options {
            listen: settings.listen,
            store,
            advertise,
            pins: settings.pins,
            hold: settings.hold.unwrap_or(crate::server::DEFAULT_HOLD),
            keep_awake: cfg!(target_os = "macos") && settings.keep_awake,
        })?;
        Ok(ExitCode::SUCCESS)
    }

    /// `agent` or `server` `on|off|status`; `on` with `force` restarts a job that runs as it
    /// would be installed ([`service::reinstall`]).
    fn services(
        paths: &Paths,
        job: Job,
        (action, force): (Action, bool),
        program: Option<String>,
        args: Vec<String>,
    ) -> crate::Result<ExitCode> {
        match action {
            Action::On => {
                let program = match program {
                    Some(program) => {
                        let program = PathBuf::from(program);
                        if !program.is_absolute() {
                            return Err(Error::new("--program must be an absolute path"));
                        }
                        // A program a person names may not start (a copy of the app's
                        // executable outside its bundle): this aterm does.
                        service::check_program_starts(&program).map_err(|error| {
                            Error::new(format!("{error}; pass --program <an aterm that runs>"))
                        })?;
                        program
                    }
                    None => service::program_for_service()?,
                };
                let install = if force {
                    service::reinstall
                } else {
                    service::install
                };
                println!("{}", install(&paths.home, job, &program, &args)?);
                if job == Job::Server {
                    until_answering(args.get(2..).unwrap_or_default())?;
                }
            }
            Action::Off => {
                if service::turn_off(&paths.home, job)? {
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

    #[cfg(unix)]
    #[test]
    fn signed_get_refuses_materialized_bytes_changed_after_sync() {
        use crate::{client, fsx, materialize, text::Audience};
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = client::Paths::for_home(&root.path().join("admin"));
        let source = root.path().join("test.vault");
        client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"authentic-value")
            .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        assert_eq!(
            unix::get_value(&paths, "token").unwrap().as_slice(),
            b"authentic-value"
        );
        let path = materialize::target_path(&paths.home, "token").unwrap();
        fsx::write_atomic(&path, b"changed-after-sync", 0o600).unwrap();
        assert!(unix::get_value(&paths, "token").is_err());
        let config_file = paths.dir.join("source.toml");
        let source_text = fsx::read_private(&config_file, 64 * 1024).unwrap().unwrap();
        let unsigned = source_text
            .as_str()
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("signer = "))
            .collect::<Vec<_>>()
            .join("\n");
        fsx::write_atomic(&config_file, unsigned.as_bytes(), 0o600).unwrap();
        assert!(client::config(&paths).unwrap().unwrap().signer.is_none());
        assert!(
            unix::get_value(&paths, "token").is_err(),
            "removing the pin cannot make signed state plaintext"
        );
        std::fs::rename(config_file, root.path().join("kept-config")).unwrap();
        assert!(
            unix::get_value(&paths, "token").is_err(),
            "unconfigured state cannot read a previous vault's file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plaintext_get_retains_local_reads_without_authenticated_evidence() {
        use crate::{client, fsx, materialize};
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = client::Paths::for_home(&root.path().join("reader"));
        let source = root.path().join("plain.secrets");
        fsx::write_atomic(&source, b"token=plain-value\n", 0o600).unwrap();
        client::join(
            &paths,
            &client::JoinOptions {
                source: source.to_str().unwrap().into(),
                host: Some("reader".into()),
                interactive: false,
                wait: std::time::Duration::ZERO,
                confirm: &|_| false,
                password: client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert_eq!(
            unix::get_value(&paths, "token").unwrap().as_slice(),
            b"plain-value"
        );
        let expected = client::config(&paths).unwrap().unwrap();
        assert!(
            client::read_verified_entry(&paths, &expected, "token")
                .unwrap()
                .is_none()
        );
        fsx::write_atomic(
            &materialize::target_path(&paths.home, "token").unwrap(),
            b"local-edit",
            0o600,
        )
        .unwrap();
        assert_eq!(
            unix::get_value(&paths, "token").unwrap().as_slice(),
            b"local-edit"
        );
    }

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
                serve: false,
            })
        );
        assert_eq!(
            parse(&words("create /v/team.vault")),
            Ok(Command::Create {
                source: "/v/team.vault".into(),
                import: None,
                name: None,
                agent: true,
                serve: false,
            })
        );
        assert!(matches!(
            parse(&words("create http://127.0.0.1:7484/v1/alab --serve")),
            Ok(Command::Create { serve: true, .. })
        ));
        for refused in [
            "create",
            "create a --serve b",
            "create a b",
            "create a --no-wait",
            "create a --name",
        ] {
            assert!(parse(&words(refused)).is_err(), "{refused}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_join_line_names_this_mac_as_the_others_reach_it() {
        let signer = "age1signer";
        assert_eq!(
            unix::join_command("http://127.0.0.1:7484/v1/alab", signer, Some("m9.local")),
            "aterm secrets join http://m9.local:7484/v1/alab#age1signer"
        );
        assert_eq!(
            unix::join_command("http://localhost:7484/v1/alab", signer, Some("m9.local")),
            "aterm secrets join http://m9.local:7484/v1/alab#age1signer"
        );
        // Control: a host the other Macs already reach, and a loopback one with no LAN name,
        // stay as they are.
        assert_eq!(
            unix::join_command("http://vault.lan:7484/v1/alab", signer, Some("m9.local")),
            "aterm secrets join http://vault.lan:7484/v1/alab#age1signer"
        );
        assert_eq!(
            unix::join_command("http://127.0.0.1:7484/v1/alab", signer, None),
            "aterm secrets join http://127.0.0.1:7484/v1/alab#age1signer"
        );
    }

    #[cfg(unix)]
    #[test]
    fn server_on_waits_for_the_vault_it_serves_on_this_mac() {
        let settings = |line: &str| {
            let f = flags(&words(line), SERVE_VALUED, SERVE_SWITCHES).expect("flags");
            serve_settings(&f).expect("settings")
        };
        assert_eq!(
            unix::local_url(&settings("")),
            format!("http://127.0.0.1:{}/v1/alab", crate::DEFAULT_PORT)
        );
        let pin = format!("team={}=", "A".repeat(43));
        assert_eq!(
            unix::local_url(&settings(&format!("--listen 0.0.0.0:9001 --pin {pin}"))),
            "http://127.0.0.1:9001/v1/team"
        );
        assert_eq!(
            unix::local_url(&settings("--listen 10.0.0.7:9001")),
            "http://10.0.0.7:9001/v1/alab"
        );
    }

    /// `server on` returns once the job it started answers: a listener whose server starts
    /// late is waited for. Negative control: an address nothing answers on ends at the hang.
    #[cfg(unix)]
    #[test]
    fn server_on_returns_once_the_server_answers() {
        let store = aterm_tempfile::tempdir().expect("tempdir");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
        let options = crate::server::Options {
            listen: listener.local_addr().expect("addr"),
            store: store.path().to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_secs(1),
            keep_awake: false,
        };
        let late = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            crate::server::start(listener, &options).expect("server starts")
        });
        unix::answering(&url, std::time::Duration::from_secs(60)).expect("answered");
        late.join().expect("started").stop();

        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let nobody = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
        drop(silent);
        let error = unix::answering(&nobody, std::time::Duration::from_millis(300))
            .expect_err("unanswered");
        assert!(error.to_string().contains("did not answer"), "{error}");
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

    /// A key let go from a closed vault is marked as one, never as code only: a password or a
    /// code still lets it in there.
    #[cfg(unix)]
    #[test]
    fn a_closed_vault_marks_a_key_let_go_but_never_code_only() {
        let key = crate::age::Identity::generate()
            .expect("a key")
            .recipient()
            .to_bech32();
        let mut doc = crate::text::Document::new_vault("m22", &key);
        assert!(!doc.open(), "CONTROL: a new vault text is closed");
        assert!(!unix::held_mark(&doc).contains("code only"));
        assert!(unix::held_mark(&doc).contains("a machine let go"));
        doc.set_open(true).expect("open");
        assert!(unix::held_mark(&doc).contains("code only"));
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

        let (doc, anchor) =
            unix::set_in(&doc, &everyone, PROFILE, b"w\n", &gaps(None, false)).expect("rotated");
        assert_eq!(anchor.as_deref(), Some(KEY));
        assert_eq!(anchor_of(&doc, PROFILE), (true, Some(KEY.to_string())));
        assert_eq!(
            doc.entry(&everyone, PROFILE).expect("set").value.as_slice(),
            b"w\n"
        );

        let error =
            unix::set_in(&doc, &everyone, PROFILE, b"x\n", &Fill::default()).expect_err("refused");
        assert!(error.0.contains(&format!("if-absent={KEY}")), "{error}");
        let error =
            unix::set_in(&doc, &everyone, KEY, b"x\n", &Fill::default()).expect_err("refused");
        assert!(error.0.contains("--if-absent"), "{error}");

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
    /// its own address under a name of its choosing, beside the machine's line.
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

    #[cfg(unix)]
    #[test]
    fn explicit_approval_can_record_existing_recipient_without_duplicating_machine() {
        use crate::client::{self, AdmissionMethod, Paths};
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("manual.vault");
        client::create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let recipient = crate::age::Identity::from_secret_bytes([38; 32])
            .recipient()
            .to_bech32();
        for expected_seq in [2, 3] {
            assert_eq!(
                unix::approve(
                    &paths,
                    std::slice::from_ref(&recipient),
                    false,
                    Some("m2".into())
                )
                .unwrap(),
                std::process::ExitCode::SUCCESS
            );
            let admin = client::admin_open(&paths).unwrap();
            assert_eq!(admin.doc.machines().len(), 2);
            let evidence = admin.commons_admission("m2").unwrap();
            assert_eq!(evidence.sequence, expected_seq);
            assert_eq!(evidence.method, AdmissionMethod::Manual);
        }
        assert!(unix::approve(&paths, &[recipient], false, Some("wrong-host".into())).is_err());
        assert_eq!(client::admin_open(&paths).unwrap().seq(), 3);
    }

    /// `approve <code>` records the request it lets in for this Mac's approver, as the notice's
    /// Approve does, so an approver away while that Mac is in shows no ghost once it is let go.
    #[cfg(unix)]
    #[test]
    fn approving_by_code_records_the_request_for_this_macs_approver() {
        use crate::client::{self, Paths};
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
        let paths = Paths::for_home(&root.path().join("m1"));
        let url = format!("http://{}/v1/team", server.addr);
        client::create(&paths, &url, None, Some("m1")).expect("create");
        let recipient = crate::age::Identity::from_secret_bytes([38; 32]).recipient();
        let code = client::join_code(&recipient);
        let key = recipient.to_bech32();
        crate::transport::post_join(&url, &format!("{key} m2 {code}")).expect("queued");
        assert!(
            client::approved_recipients(&paths).is_empty(),
            "CONTROL: nothing recorded yet"
        );
        assert_eq!(
            unix::approve(&paths, &[code], false, None).expect("approved"),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(client::approved_recipients(&paths), [key]);
        server.stop();
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
    fn commons_init_action() {
        assert_eq!(parse(&words("commons init")), Ok(Command::CommonsInit));
        assert!(parse(&words("commons")).is_err());
        assert!(parse(&words("commons replace")).is_err());
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

    /// On macOS, `serve` may advertise the ALab vault's name only when listening on all
    /// addresses at the default port and not told otherwise ([`alab_role`] decides whether it
    /// does).
    #[test]
    fn serve_advertises_the_default_vaults_name_unless_told() {
        let advertised = |line: &str| {
            let f = flags(&words(line), SERVE_VALUED, SERVE_SWITCHES).expect("flags");
            serve_settings(&f).expect("settings").advertise
        };
        let default = if cfg!(target_os = "macos") {
            Advertise::AlabIfHeld
        } else {
            Advertise::Off
        };
        assert_eq!(advertised(""), default);
        assert_eq!(advertised("--listen 0.0.0.0:7484"), default);
        assert_eq!(advertised("--listen [::]:7484"), default);
        for elsewhere in [
            "--listen 127.0.0.1:7485",
            "--listen 127.0.0.1:7484",
            "--listen 0.0.0.0:7485",
            "--listen [::1]:7484",
        ] {
            assert_eq!(advertised(elsewhere), Advertise::Off, "{elsewhere}");
        }
        assert_eq!(
            advertised("--listen 127.0.0.1:7485 --advertise lab-vault"),
            Advertise::Named("lab-vault".into())
        );
        assert_eq!(
            advertised("--advertise lab-vault"),
            Advertise::Named("lab-vault".into())
        );
        assert_eq!(advertised("--no-advertise"), Advertise::Off);
        assert_eq!(
            parse(&words("server on --no-advertise")),
            Ok(Command::Server {
                action: Action::On,
                program: None,
                serve: vec!["--no-advertise".into()],
            })
        );
    }

    /// Only ALab's server answers the ALab name or takes another Mac's first version of the ALab
    /// vault: one told so, or one whose store holds it on a Mac that follows it.
    #[test]
    fn only_alabs_server_advertises_the_alab_name_or_pins_the_alab_vault() {
        let key = [7; 32];
        let alab = crate::alab_advertised().to_string();
        let role = |advertise: Advertise, holds: bool, given: &[(&str, [u8; 32])]| {
            let mut pins: Vec<(String, [u8; 32])> = given
                .iter()
                .map(|(name, key)| ((*name).into(), *key))
                .collect();
            let here = if holds {
                AlabHere::Server
            } else {
                AlabHere::Empty
            };
            let (advertised, held_back) = alab_role(advertise, here, &mut pins, Some(key));
            let pinned: Vec<&str> = pins.iter().map(|(name, _)| name.as_str()).collect();
            (advertised, held_back.is_some(), pinned.join(","))
        };
        // A personal server: no ALab name, no ALab pin, and a line that says why.
        assert_eq!(
            role(Advertise::AlabIfHeld, false, &[]),
            (None, true, String::new())
        );
        assert_eq!(
            role(Advertise::Named("home".into()), false, &[]),
            (Some("home".into()), false, String::new())
        );
        assert_eq!(
            role(Advertise::Off, false, &[]),
            (None, false, String::new())
        );
        // ALab's server, by its store or by being told (a new one starts with an empty store).
        assert_eq!(
            role(Advertise::AlabIfHeld, true, &[]),
            (Some(alab.clone()), false, "alab".into())
        );
        assert_eq!(
            role(Advertise::Named(alab.clone()), false, &[]),
            (Some(alab.clone()), false, "alab".into())
        );
        assert_eq!(
            role(Advertise::Off, true, &[]),
            (None, false, "alab".into())
        );
        // A pin given by hand stands as given.
        assert_eq!(
            role(
                Advertise::AlabIfHeld,
                true,
                &[("alab", [9; 32]), ("home", [8; 32])]
            ),
            (Some(alab.clone()), false, "alab,home".into())
        );
        // A copy an ALab Mac put back on a server whose Mac does not follow the ALab vault (one
        // that took it before servers stopped) neither names nor pins, and says why.
        let mut pins = Vec::new();
        let (advertised, held_back) =
            alab_role(Advertise::AlabIfHeld, AlabHere::Copy, &mut pins, Some(key));
        assert_eq!(advertised, None);
        assert!(pins.is_empty());
        assert!(
            held_back.is_some_and(|line| line.contains("holds a copy of the ALab vault")),
            "says why"
        );
        // Told so, it is ALab's server whatever its store holds.
        let mut pins = Vec::new();
        let (advertised, _) = alab_role(
            Advertise::Named(alab.clone()),
            AlabHere::Copy,
            &mut pins,
            Some(key),
        );
        assert_eq!(advertised, Some(alab));
        assert_eq!(pins, [("alab".to_string(), key)]);
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
    fn agent_on_force_restarts_and_plain_on_does_not() {
        assert_eq!(
            parse(&words("agent on --force")),
            Ok(Command::Agent {
                action: Action::On,
                program: None,
                force: true,
            })
        );
        assert_eq!(
            parse(&words("agent on")),
            Ok(Command::Agent {
                action: Action::On,
                program: None,
                force: false,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_join_without_a_terminal_does_not_wait_for_its_approval() {
        assert_eq!(unix::join_wait(true, false), std::time::Duration::ZERO);
        assert_eq!(unix::join_wait(false, true), std::time::Duration::ZERO);
        // CONTROL: at a terminal, a join waits ten minutes for the approval.
        assert_eq!(
            unix::join_wait(true, true),
            std::time::Duration::from_secs(600)
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_join_password_is_shown_only_at_a_terminal_no_agent_runs() {
        let password = crate::SecretBytes::from("pear-otter-quilt".as_bytes());
        for (at_terminal, agent) in [
            (false, None),
            (true, Some("CLAUDECODE")),
            (false, Some("CODEX")),
        ] {
            let mut out = Vec::new();
            let code = unix::show_password(&mut out, at_terminal, agent, &password).expect("runs");
            assert_eq!(code, ExitCode::FAILURE, "{at_terminal} {agent:?}");
            assert!(
                out.is_empty(),
                "{at_terminal} {agent:?}: {} bytes",
                out.len()
            );
        }
        // CONTROL: a person at a terminal sees it.
        let mut out = Vec::new();
        let code = unix::show_password(&mut out, true, None, &password).expect("runs");
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"pear-otter-quilt\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_join_password_is_told_only_at_a_terminal_no_agent_runs() {
        let password = crate::SecretBytes::from("pear-otter-quilt".as_bytes());
        for (at_terminal, agent) in [(false, None), (true, Some("CLAUDECODE"))] {
            let mut out = Vec::new();
            unix::tell_new_password(&mut out, at_terminal, agent, &password).expect("runs");
            assert!(
                out.is_empty(),
                "{at_terminal} {agent:?}: {} bytes",
                out.len()
            );
        }
        // CONTROL: a person at a terminal reads the password the vault now takes.
        let mut out = Vec::new();
        unix::tell_new_password(&mut out, true, None, &password).expect("runs");
        assert_eq!(out, b"join password: pear-otter-quilt\n");
    }

    #[test]
    fn unknown_commands_and_options_are_usage_errors() {
        assert!(parse(&words("frobnicate")).is_err());
        assert!(parse(&words("--aterm-front-door-routing-probe")).is_err());
        assert!(parse(&words("sync --fast")).is_err());
        assert!(parse(&words("approve")).is_err());
        assert!(parse(&words("admins promote m3")).is_err());
    }

    /// What a waiting Mac names for an admin Mac is an `approve` that lets it in: its code for a
    /// vault at a URL, whose server queues the request, and its key and name for one at a path,
    /// which has no queue, so no code there is ever approved.
    #[cfg(unix)]
    #[test]
    fn a_waiting_machine_names_the_approve_its_vault_takes() {
        use crate::client::{Config, Membership, Status};
        use crate::transport::Source;
        let key = crate::age::Recipient([7; 32]);
        let waiting = |source| Status {
            config: Some(Config {
                source,
                name: "home".into(),
                signer: Some([3; 32]),
                followed: Vec::new(),
                host: "mb".into(),
            }),
            machine: Some((key, "abcd-efgh-ijkm-npqr".into())),
            membership: Membership::Pending,
            names: Vec::new(),
        };
        let by_code = waiting(Source::Url("http://h:7484/v1/home".into()))
            .approve_command()
            .expect("a key");
        assert_eq!(by_code, "aterm secrets approve abcd-efgh-ijkm-npqr");
        let by_key = waiting(Source::Path("/Users//someone/home.vault".into()))
            .approve_command()
            .expect("a key");
        assert_eq!(
            by_key,
            format!("aterm secrets approve {} --name mb", key.to_bech32())
        );
        for command in [by_code, by_key] {
            let args = command
                .strip_prefix("aterm secrets ")
                .expect("a secrets verb");
            assert!(
                matches!(parse(&words(args)), Ok(Command::Approve { .. })),
                "{command}"
            );
        }
        let mut keyless = waiting(Source::Url("http://h:7484/v1/home".into()));
        keyless.machine = None;
        assert_eq!(keyless.approve_command(), None);
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
        let approve = format!("aterm secrets approve {code}");
        for line in [
            unix::waiting_line("m9", code, &approve, Some(&hint)),
            unix::pending_membership(&approve, Some(&hint)),
        ] {
            assert!(
                line.contains(&format!("aterm secrets approve {code}")),
                "{line}"
            );
            assert!(line.contains(&hint), "{line}");
        }
        assert!(!unix::waiting_line("m9", code, &approve, None).contains("mistyped"));
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
        let piped = unix::retype(&alab, None, true).expect("a URL has a join queue");
        assert!(
            piped.contains("piping it again to aterm secrets join --password-stdin"),
            "{piped}"
        );
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

    /// Run an edit where a concurrent publish forces a reopen, then quit unchanged and answer
    /// `publish`.
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
        let (admin, began, code, asked) = edit_reopened_unchanged(&root.path().join("a"), "");
        assert_eq!(code, ExitCode::FAILURE);
        assert!(asked.iter().any(|p| p.contains("as it is")), "{asked:?}");
        let after = crate::client::admin_open(&admin).expect("open");
        assert_eq!(after.seq(), began + 1, "only the other admin's version");
        assert!(after.doc.entry(&everyone, "other").is_some());
        assert!(after.doc.entry(&everyone, "note").is_none());
        assert_eq!(edit_dirs(&admin).len(), 1, "the draft is kept");
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

    /// `revoke` and `admins remove` list every secret the machine could read (everything for an
    /// admin, `[everyone]` plus its own host sections for a member).
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

    /// A join code typed for a machine resolves to the machine whose key has that code, and a
    /// code no key has is refused even if a machine bears it as its name.
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
    /// may be another machine's in the vault.
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

    /// `put_in_approver` installs a stopped or absent subscriber, leaves a running one alone, and
    /// skips installation when the state cannot be read.
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

    /// When no approver runs locally, commands that need one report why and name
    /// `aterm secrets agent on`.
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

    /// A vault at a path has no join queue, so `lets_in_words` reports that machines join only by
    /// admin approval of their key.
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

    /// `create` installs the approver unless `--no-agent` is set or the vault is at a path, and a
    /// failed install still succeeds with a warning.
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
        let off =
            |dir: &str| crate::service::subscriber_turned_off(&root.path().join(dir).join("ma"));
        create("a", true, &played(&absent, &install));
        assert_eq!(installs.get(), 1, "create installs the subscriber");
        assert!(!off("a"), "no choice to record");
        create("b", false, &played(&absent, &install));
        assert_eq!(installs.get(), 1, "--no-agent installs nothing");
        assert!(off("b"), "--no-agent is the person's choice, recorded");
        create("d", false, &played(&running, &install));
        assert_eq!(
            reads.get(),
            2,
            "--no-agent reads whether the approver runs, for the record and for the approver"
        );
        assert_eq!(installs.get(), 1, "a running subscriber is left be");
        assert!(
            !off("d"),
            "a subscriber that runs is left be, its choice not recorded"
        );
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

    /// Test server that accepts the first `PUT` (204) and returns 503 on subsequent `GET`s.
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
            Self::start_with_dropped_reply(false)
        }

        fn start_with_dropped_reply(drop_reply: bool) -> LostAfterThePut {
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
                        if drop_reply {
                            continue;
                        }
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

    /// A server becoming unavailable after publication does not prevent the
    /// retained initial envelope from installing locally with its approver.
    #[cfg(unix)]
    #[test]
    fn a_create_installs_locally_when_the_server_stops_answering_after_put() {
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
        assert!(matches!(
            status.membership,
            crate::client::Membership::In { seq: 1, .. }
        ));
        lost.end();

        let lost = LostAfterThePut::start();
        let other = crate::client::Paths::for_home(&root.path().join("mb"));
        let created = crate::client::create(&other, &lost.url, None, Some("mb")).expect("created");
        assert_eq!(
            created.local, None,
            "installs from retained authenticated bytes"
        );
        lost.end();
    }

    #[cfg(unix)]
    #[test]
    fn a_lost_creation_reply_still_starts_its_recovery_subscriber() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = crate::client::Paths::for_home(&root.path().join("m1"));
        let installs = std::cell::Cell::new(0);
        let install = || {
            installs.set(installs.get() + 1);
            Ok("played: installed".to_string())
        };
        let absent = || Ok(crate::service::State::Absent);
        let lost = LostAfterThePut::start_with_dropped_reply(true);
        assert!(
            unix::create(
                &paths,
                &lost.url,
                None,
                Some("m1".into()),
                true,
                &played(&absent, &install)
            )
            .is_err()
        );
        assert_eq!(installs.get(), 1);
        assert!(crate::client::creation_pending(&paths).unwrap());
        assert!(crate::client::config(&paths).unwrap().is_none());
        lost.end();
    }

    /// Opening a vault or setting a join password installs the approver when none runs; closing it
    /// (`off`) reads no subscriber.
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
        assert_eq!(
            approving_here(&admin),
            Ok(Some(Approval::Open)),
            "a new vault is open"
        );
        let password = |action, subscriber: &unix::Subscriber<'_>| {
            unix::password(&admin, action, true, subscriber).expect("published")
        };
        password(Password::Off, &played(&unread, &install));
        assert_eq!(approving_here(&admin), Ok(None), "closed, nothing approves");
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
        unix::password(&admin, Password::Open, false, &played(&absent, &install))
            .expect("published");
        assert_eq!(installs.get(), 2, "--no-agent installs nothing");
        password(Password::Off, &played(&unread, &install));
        assert_eq!(approving_here(&admin), Ok(None), "closed, nothing approves");
        password(Password::Off, &played(&unread, &install));
        assert_eq!(installs.get(), 2);
        server.stop();
        let stranger = crate::client::Paths::for_home(&root.path().join("stranger"));
        assert_eq!(approving_here(&stranger), Ok(None));
        let at_a_path = admin_of_a_path_vault(&root.path().join("path"));
        assert_eq!(approving_here(&at_a_path), Ok(None));
        let no_queue =
            || -> crate::Result<crate::service::State> { panic!("a path has no queue to approve") };
        // A new path vault is open too: close it so opening runs the arm that publishes.
        unix::password(
            &at_a_path,
            Password::Off,
            true,
            &played(&no_queue, &install),
        )
        .expect("closed");
        unix::password(
            &at_a_path,
            Password::Open,
            true,
            &played(&no_queue, &install),
        )
        .expect("published");
        assert_eq!(installs.get(), 2, "nor is one put in there");

        let line = |approval, state| unix::approver_line(approval, &state, None);
        let ok = line(Approval::Password, Ok(installed(Some(42))));
        assert!(ok.contains("running in the subscriber (pid 42)"), "{ok}");
        assert!(ok.contains("types the join password is let in"), "{ok}");
        let why = "admin locker: alab-vault-admin/4 is newer";
        let said = unix::approver_line(Approval::Open, &Ok(installed(Some(42))), Some(why));
        assert!(
            said.contains("(pid 42) but not approving: admin locker: alab-vault-admin/4 is newer;"),
            "{said}"
        );
        assert!(
            said.contains("a machine that asks to join waits for approval by code"),
            "{said}"
        );
        // A status that cannot read what stopped the approver still says it is stuck, and
        // says nothing where no record is, or no subscriber runs.
        let status = |approving, state, stuck| unix::approver_status(&approving, &state, stuck);
        let unreadable = || Err(crate::Error::new(why));
        let stuck = status(unreadable(), Ok(installed(Some(42))), Some(why)).expect("a line");
        assert!(
            stuck.contains("(pid 42) cannot tell whether this Mac approves joins: admin locker"),
            "{stuck}"
        );
        assert_eq!(status(unreadable(), Ok(installed(Some(42))), None), None);
        assert_eq!(status(unreadable(), Ok(installed(None)), Some(why)), None);
        assert_eq!(status(Ok(None), Ok(installed(Some(42))), Some(why)), None);
        assert_eq!(
            status(Ok(Some(Approval::Password)), Ok(installed(Some(42))), None),
            Some(ok.clone())
        );
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
