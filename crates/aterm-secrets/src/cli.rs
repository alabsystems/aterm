// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets`.

use std::ffi::OsString;
use std::process::ExitCode;

#[cfg(unix)]
mod inventory;

/// The verb's help.
pub const USAGE: &str = "\
aterm secrets: one credentials file for every machine (aterm help secrets).

  aterm secrets [status [--json]]        this machine's vault, join code, membership,
                                         secrets, subscriber and server, and on an admin
                                         whether the approver runs
  aterm secrets repair [--json]         migrate outdated managed vault services to this installed
                                         release, with their arguments, health checks and rollback;
                                         explicit pins and off records remain in force
  aterm secrets join [<url>|<path>] [--name <host>] [--password-stdin]
        [--no-agent] [--no-wait]         join a vault (neither: the ALab vault; a trailing
                                         #<signer key> pins its signer, else confirm the
                                         fingerprint at a terminal), waiting up to 10 min
                                         for approval at a terminal unless --no-wait, and
                                         install the subscriber unless --no-agent
  aterm secrets sync [--diagnostic]      fetch and apply the latest version now;
                                         --diagnostic prints one value-free outcome record
  aterm secrets watch                    the subscriber (agent on installs it): apply each
                                         new version; on an admin of a <url> vault, also
                                         the approver, which lets in a machine allowed on
                                         any member or one that types the join password
  aterm secrets mirrors [add|remove <url>...]
                                         this Mac's other addresses of its vault (source.toml's
                                         mirrors): read beside its url under the same pin;
                                         no join, request, put-back or admin publish goes
                                         to a mirror
  aterm secrets ls [--describe | --json]  the names this machine holds; descriptions and
                                         file pointers with --describe, metadata JSON
                                         with --json; neither prints secret values. A
                                         Claude Code token has no file: its row says its
                                         state, where it was minted and when it ends
  aterm secrets get <name>               print one value to stdout (never a Claude Code
                                         token, which aterm hands only to Claude Code)
  aterm secrets keep <name>...           make each file the vault wrote here this Mac's own
                                         (confirmed by digest), so the removal of its entry
                                         leaves the file; prints kept, or why not, per
                                         name, never a value
  aterm secrets need ls [--all] [--json] this machine's needs (a credential it lacks, asked
                                         of the lab's Macs), --all every request in the
                                         lab's table; need <want> raises one by hand,
                                         need retire <want|id> [--note <text>] withdraws
                                         one, need pass runs this machine's pass now
                                         (aterm secrets need --help)
  aterm secrets git-credential [--dev-user <login>] [--token-file <path> [--token-key <name>]] [<op>]
                                         git's credential helper over the registry's gh:dev
                                         and gh:release entries (or, for the dev token, the
                                         file --token-file names and, in it, the <name>= line
                                         --token-key names); one this Mac lacks is filed as a
                                         need and git's request fails
  aterm secrets git-setup [--apply]    plan a local migration of known vault/native Git helpers
                                         to this protected development executable; --apply keeps
                                         exact private rollback files and refuses custom helpers
member (a Mac seated in the vault's membership log; status says vault writes: member):
  aterm secrets set <name> [--file <path>]
        [--if-absent [--with <name> | --no-with]] [--fleet-reads-anchors]
                                         sign the value from stdin or the file into the
                                         vault's log for every member, with no admin key;
                                         shared names only (release material, fleet
                                         configuration, git's config and control records
                                         are refused); with no server answering, it is
                                         signed here, and while a removal's new key is due,
                                         it waits: not yet in vault until this Mac's vault
                                         pass posts or signs it; --if-absent:
                                         only where the machine has no file of its own;
                                         --with: and only where that if-absent entry is
                                         not the machine's own (set again, an entry keeps
                                         its anchor unless --no-with); the vault's first
                                         anchored entry needs --fleet-reads-anchors
  aterm secrets unset <name>             remove a shared name from the vault's log for every
                                         member, with no admin key, under a new vault key
                                         that seals every entry left again, so no Mac let
                                         in later reads the removed value; this Mac's own
                                         write of it that still waits is taken back; it
                                         never waits, and needs a Mac that opens every
                                         entry; with no server answering, it is signed
                                         here: not yet in vault until this Mac's vault pass
                                         posts it
admin (a machine in [admins]):
  aterm secrets create <url>|<path> [--import <dir>] [--name <host>]
        [--no-agent] [--serve]           a new open vault (at a <url> a machine is let in by
                                         a person's Allow on any member and its phrase;
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
  aterm secrets set <name> --for <host|code>... [set's flags]
                                         the value for those machines only (B13 removes
                                         --for); a plain set on a vault with no membership
                                         log, and of release material, fleet configuration,
                                         git's config or a control record, is an admin's too
  aterm secrets unset <name> --for <host|code>...
                                         remove it for those machines only (B13 removes
                                         --for); a plain unset on a vault with no membership
                                         log, and of release material, fleet configuration,
                                         git's config or a control record, is an admin's too
  aterm secrets password [show|set|new|open|off] [--no-agent]
                                         the join password: show (the default), set (typed
                                         or piped), new (generated), open (none: a
                                         machine allowed on any member is let in), off
                                         (close the vault or drop the password: an Allow on
                                         a member or approval by code); show and new print
                                         it only at a terminal; for a <url>, set, new and
                                         open also install the subscriber where it does not
                                         run, unless --no-agent
  aterm secrets requests [--json]        queued requests, with their codes; JSON includes
                                         the responding queue and server and whether known
                                         members could be excluded, never proof values
  aterm secrets approve <code>... | --all | <age1...> --name <host>
                                         let in machines by the code each shows (--all:
                                         every one, asked at a terminal), or a key by name
                                         (how a machine joins a <path> vault)
  aterm secrets revoke <host|code> [--drop-sections] [--stolen]
                                         remove it from the vault: where this Mac is seated in
                                         the vault's membership log, as `aterm members remove`
                                         of a name (any member, no admin key; a label is the
                                         log's, never the registry's); else, as an admin (first
                                         retiring its Commons membership, denying its swarm
                                         label/node, and releasing its pool slots); either way
                                         it rotates aterm-fleet-code; --stolen also prints a
                                         NEEDS YOU: provider rotation list; a section naming
                                         only it must be empty unless --drop-sections drops it
                                         too (an admin's edit)
  aterm secrets retire <host|code|age1...>... permanently retire Commons membership alone;
                                         a full age key can name a device already removed
  aterm secrets admins add|remove <host|code>   (remove also rotates the vault key)
  aterm secrets rotate-key               sign with a new vault key the current one endorses
  aterm secrets commons init             establish an initial Commons foundation, or finish its
                                         setup on the same root; an existing access failure
                                         is inspected with commons restore
  aterm secrets commons restore          inspect a recovery plan for the existing published root;
        [--expect <plan-digest>]          with its exact digest, restore control access for eligible
                                         existing members, preserving history and explicit denials
  aterm secrets genesis <file>           an admin signs Genesis v2 into a new <file>, seeds its
                                         membership records and encrypted entries, and attempts sync;
                                         every [machines] key, with its current Commons binding.
                                         An explicit private URL uses its pinned signer; ALab,
                                         its default URLs and lineage need a released aterm.
                                         Prints only its digest and member count
server and services:
  aterm secrets serve [--listen <addr:port>] [--advertise <name> | --no-advertise]
        [--pin <vault>=<key>] [--store <dir>] [--hold <1-60 secs>] [--no-keep-awake]
                                         serve vaults (defaults: 0.0.0.0:7484, the store
                                         ~/.config/aterm/secrets/served, long-polls held
                                         25 s; a relative --store is under ~). A vault the
                                         store lacks takes its first version only from this
                                         machine, unless --pin names the key that signs it
                                         (alab's is built in on ALab's server and an ALab
                                         member's Mac). On macOS only, it keeps the Mac
                                         from idle sleep unless --no-keep-awake, and
                                         advertises <name>.local over mDNS: by default, on
                                         every address at port 7484, an ALab member's own
                                         label, and the ALab vault's name once its store
                                         holds that vault and no other server has answered
                                         there for 90 s; on any other Mac, the ALab vault's
                                         name once its store holds the vault this Mac follows
                                         (a new ALab server: --advertise alab-secrets;
                                         --no-advertise: none)
  aterm secrets agent [on|off|status] [--program <path>] [--force]
                                         the subscriber as a launchd or systemd user job
                                         (--program: the aterm it runs, pinned so no
                                         mend moves it, else this aterm; on leaves a
                                         launchd job running as set up, unless --force)
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
    let diagnostic = matches!(command, Command::Sync { diagnostic: true });
    #[cfg(unix)]
    {
        match unix::run(command) {
            Ok(code) => code,
            Err(error) => {
                if diagnostic {
                    println!(
                        "{}",
                        crate::sync_diagnostic::SyncDiagnostic::Failed(error.kind()).line()
                    );
                } else {
                    eprintln!("aterm secrets: {error}");
                }
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        if diagnostic {
            println!(
                "{}",
                crate::sync_diagnostic::SyncDiagnostic::Failed(crate::ErrorKind::Other).line()
            );
        } else {
            eprintln!("aterm secrets: available on macOS and Linux only");
        }
        ExitCode::from(2)
    }
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Status,
    StatusJson,
    Repair {
        json: bool,
    },
    Join {
        source: String,
        name: Option<String>,
        agent: bool,
        wait: bool,
        password_stdin: bool,
    },
    Sync {
        diagnostic: bool,
    },
    Watch,
    /// `mirrors`: list them, or with `Some(add)` add or remove these.
    Mirrors {
        change: Option<bool>,
        urls: Vec<String>,
    },
    Ls {
        describe: bool,
        json: bool,
    },
    Get(String),
    Keep(Vec<String>),
    GitCredential {
        dev_user: Option<String>,
        /// The dev token's file in place of the secrets registry's gh:dev entry (absolute).
        token_file: Option<std::path::PathBuf>,
        /// The `<name>=` line of `token_file` that holds the token.
        token_key: Option<String>,
        operation: Option<String>,
    },
    GitSetup {
        apply: bool,
    },
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
    Requests {
        json: bool,
    },
    Approve {
        which: Vec<String>,
        all: bool,
        name: Option<String>,
    },
    Revoke {
        host: String,
        drop_sections: bool,
        stolen: bool,
    },
    Retire(Vec<String>),
    Admins {
        add: bool,
        host: String,
    },
    RotateKey,
    CommonsInit,
    CommonsRestore {
        expected: Option<[u8; 32]>,
    },
    Genesis(String),
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

/// Whether a `set` or `unset` of `name` for `hosts` (none: `[everyone]`) is a member's write or an
/// admin's on a vault this Mac writes by `route` (DESIGN-claude-setup-token §3.4): a plain one of
/// a shared name on a vault with a membership log is a member's, whatever the seat (one that has
/// none, or a busy state, is told so and never falls back to the admin locker); `--for`, a
/// vault with no log, and every name a member may not write, are an admin's.
#[cfg(unix)]
fn by_member(name: &str, hosts: &[String], route: crate::client::WriteRoute) -> bool {
    hosts.is_empty()
        && route != crate::client::WriteRoute::NoLog
        && crate::entry_class::of(name) == crate::entry_class::EntryClass::Shared
}

#[cfg(unix)]
impl Command {
    /// Whether this command needs the admin locker on a vault this Mac writes by `route`: what
    /// `aterm secrets status` names on its `admin key:` line ([`admin_verbs`]), read from each
    /// verb's own route. `revoke` removes through the membership log where this Mac is seated
    /// there, as `aterm members remove`.
    fn admin_route(&self, route: crate::client::WriteRoute) -> bool {
        match self {
            Self::Set { name, hosts, .. } | Self::Unset { name, hosts } => {
                !by_member(name, hosts, route)
            }
            Self::Revoke { .. } => route != crate::client::WriteRoute::Seated,
            Self::Edit
            | Self::Password { .. }
            | Self::Approve { .. }
            | Self::Retire(_)
            | Self::Admins { .. }
            | Self::RotateKey
            | Self::CommonsInit
            | Self::CommonsRestore { .. }
            | Self::Genesis(_) => true,
            _ => false,
        }
    }
}

/// Each verb that may need the admin locker, as `aterm secrets status` names it, with one command
/// line of it ([`Command::admin_route`]).
#[cfg(unix)]
fn admin_verb_lines() -> Vec<(&'static str, Command)> {
    let set = |name: &str, hosts: &[&str]| Command::Set {
        name: name.to_owned(),
        hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
        file: None,
        fill: Fill::default(),
    };
    let unset = |name: &str, hosts: &[&str]| Command::Unset {
        name: name.to_owned(),
        hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
    };
    vec![
        ("edit", Command::Edit),
        ("set", set("gh_access_token", &[])),
        (
            "set of a held name",
            set(crate::entry_class::RELEASE_GH_TOKEN, &[]),
        ),
        ("set --for", set("gh_access_token", &["m3"])),
        ("unset", unset("gh_access_token", &[])),
        (
            "unset of a held name",
            unset(crate::entry_class::RELEASE_GH_TOKEN, &[]),
        ),
        ("unset --for", unset("gh_access_token", &["m3"])),
        (
            "password",
            Command::Password {
                action: Password::Show,
                agent: false,
            },
        ),
        (
            "approve",
            Command::Approve {
                which: Vec::new(),
                all: true,
                name: None,
            },
        ),
        (
            "revoke",
            Command::Revoke {
                host: "m3".to_owned(),
                drop_sections: false,
                stolen: false,
            },
        ),
        ("retire", Command::Retire(vec!["m3".to_owned()])),
        (
            "admins",
            Command::Admins {
                add: true,
                host: "m3".to_owned(),
            },
        ),
        ("rotate-key", Command::RotateKey),
        ("commons init", Command::CommonsInit),
        (
            "commons restore",
            Command::CommonsRestore { expected: None },
        ),
        ("genesis", Command::Genesis("alab.genesis".to_owned())),
    ]
}

/// The verbs that need the admin locker on a vault this Mac writes by `route`
/// ([`Command::admin_route`]).
#[cfg(unix)]
#[must_use]
pub fn admin_verbs(route: crate::client::WriteRoute) -> Vec<&'static str> {
    admin_verb_lines()
        .into_iter()
        .filter(|(_, command)| command.admin_route(route))
        .map(|(verb, _)| verb)
        .collect()
}

/// Who writes this Mac's vault, as `aterm secrets status` says it (`vault writes:`).
#[cfg(unix)]
#[must_use]
pub fn vault_writes(route: crate::client::WriteRoute) -> &'static str {
    match route {
        crate::client::WriteRoute::Seated => "member",
        crate::client::WriteRoute::NotSeated => "none (not seated in the vault's log yet)",
        crate::client::WriteRoute::NoLog => "admin (a vault with no log)",
        crate::client::WriteRoute::Busy => "unknown (this Mac's vault state is busy)",
    }
}

/// What a member's `set` of `name` printed, by its outcome (DESIGN-claude-setup-token §3.5):
/// `Ok` a line on stdout, `Err` the refusal. A write signed here, or one that waits, is not yet
/// in vault.
#[cfg(unix)]
fn member_words(name: &str, written: &crate::client::MemberWritten) -> Result<String, String> {
    use crate::client::{MemberWritten, WaitFor};
    let pass = "this Mac's vault pass signs it";
    Ok(match written {
        MemberWritten::Posted => format!(
            "set `{name}` for every member; a per-host copy, where one exists, still wins on its \
             Mac (an admin's `set --for` changes those until B13)"
        ),
        MemberWritten::SignedHere => format!(
            "set `{name}` here; not yet in vault: no server answered, and this Mac's vault pass \
             posts it"
        ),
        MemberWritten::Waiting(why) => {
            let waits = match why {
                WaitFor::RotationDue => "waits for the vault's new key after a removal".to_owned(),
                WaitFor::NewestUnopened => {
                    "waits until this Mac holds the vault's newest key".to_owned()
                }
                WaitFor::KeysUnsettled => {
                    "waits for the vault's newest keys to settle into one".to_owned()
                }
                WaitFor::PendingOfThisMac => {
                    format!("replaces this Mac's earlier write of `{name}`, which still waited")
                }
                WaitFor::LockDeadline => "this Mac's vault state was busy".to_owned(),
            };
            format!("not yet in vault: {waits}; {pass}")
        }
        MemberWritten::Overtaken(by) => {
            let who = by.as_ref().map_or_else(
                || "another write".to_owned(),
                |by| {
                    format!(
                        "{}'s {}",
                        by.label,
                        if by.deleted { "removal" } else { "write" }
                    )
                },
            );
            return Err(format!(
                "`{name}` is not set: {who} took it first and stands (set it again to replace it)"
            ));
        }
        MemberWritten::Refused(why) => return Err(why.clone()),
        MemberWritten::Busy => return Err("the vault is busy; try again".to_owned()),
        MemberWritten::NotSeated => {
            return Err(
                "this Mac is not seated in the vault's log yet, so it writes nothing there \
                 (aterm secrets status says vault writes:)"
                    .to_owned(),
            );
        }
        MemberWritten::NoLog => {
            return Err(
                "this Mac's vault has no membership log: an admin's `aterm secrets set` writes it"
                    .to_owned(),
            );
        }
    })
}

/// What a member's `unset` of `name` printed, by its outcome (DESIGN-claude-setup-token §3.6):
/// `Ok` a line on stdout, `Err` the refusal. A removal signed here is not yet in vault.
#[cfg(unix)]
fn member_unset_words(name: &str, unset: &crate::client::MemberUnset) -> Result<String, String> {
    use crate::client::MemberUnset;
    Ok(match unset {
        MemberUnset::Posted => format!(
            "removed `{name}` for every member, under a new vault key, so no Mac let in later \
             reads the removed value; a per-host copy, where one exists, still wins on its Mac \
             (an admin's `unset --for` removes those until B13)"
        ),
        MemberUnset::SignedHere => format!(
            "removed `{name}` here; not yet in vault: no server answered, and this Mac's vault \
             pass posts it"
        ),
        MemberUnset::TookBack => format!(
            "took back this Mac's write of `{name}`, which still waited to be signed; the vault \
             never held it"
        ),
        MemberUnset::Absent => return Err(format!("no entry {name} in [everyone]")),
        MemberUnset::Overtaken(by) => {
            let who = by.as_ref().map_or_else(
                || "another write".to_owned(),
                |by| format!("{}'s write", by.label),
            );
            return Err(format!(
                "`{name}` is set again: {who} put it back after the removal and stands (unset it \
                 again to remove it)"
            ));
        }
        MemberUnset::Refused(why) => return Err(why.clone()),
        MemberUnset::Busy => return Err("the vault is busy; try again".to_owned()),
        MemberUnset::NotSeated => {
            return Err(
                "this Mac is not seated in the vault's log yet, so it removes nothing there \
                 (aterm secrets status says vault writes:)"
                    .to_owned(),
            );
        }
        MemberUnset::NoLog => {
            return Err(
                "this Mac's vault has no membership log: an admin's `aterm secrets unset` writes \
                 it"
                .to_owned(),
            );
        }
    })
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
pub(crate) enum Advertise {
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

/// Apply ALab member server-role rules (`DESIGN-fluid-swarm` §4.1 F6 & F7): every active ALab
/// member pins the ALab vault key and advertises `_alab-vault._tcp` under its own `<label>.local`
/// while [`crate::server::AlabNameWatcher`] governs `alab-secrets.local` with no lease.
#[cfg(unix)]
pub(crate) fn alab_policy_role(
    advertise: Option<String>,
    held_back: Option<String>,
    policy_role: Option<(bool, Option<String>)>,
    pins: &mut Vec<(String, [u8; 32])>,
    alab_key: Option<[u8; 32]>,
) -> (Option<String>, Option<String>) {
    let Some((pin_alab, own_label)) = policy_role else {
        return (advertise, held_back);
    };
    if pin_alab
        && !pins.iter().any(|(name, _)| name == crate::ALAB_VAULT)
        && let Some(key) = alab_key
    {
        pins.push((crate::ALAB_VAULT.to_string(), key));
    }
    if let Some(label) = own_label.filter(|l| {
        !l.is_empty() && l != crate::alab_advertised() && crate::text::valid_machine_name(l)
    }) {
        if advertise.as_deref() == Some(crate::alab_advertised()) {
            return (Some(label), None);
        }
        if advertise.is_none() && held_back.is_some() && pin_alab {
            return (Some(label), None);
        }
    }
    (advertise, held_back)
}

/// The store directory a `secrets serve …` argument list (`args` after the `aterm` executable)
/// serves when this build parses every flag cleanly, and the URL of the ALab vault that server
/// answers on this Mac (its listening address, loopback for an unspecified one)
/// (`DESIGN-fluid-swarm` §4.1 F6).
#[cfg(unix)]
pub(crate) fn served_by(
    paths: &crate::client::Paths,
    args: &[String],
) -> Option<(std::path::PathBuf, String)> {
    let tail = args.strip_prefix(["secrets".to_owned(), "serve".to_owned()].as_slice())?;
    let f = flags(tail, SERVE_VALUED, SERVE_SWITCHES).ok()?;
    no_operands(&f, "aterm secrets serve").ok()?;
    let mut settings = serve_settings(&f).ok()?;
    let store = settings
        .store
        .as_deref()
        .map_or_else(|| paths.served(), |dir| unix::store_dir(paths, dir));
    // `local_url` names the first pinned vault: this asks for the ALab vault's.
    settings.pins.clear();
    Some((store, unix::local_url(&settings)))
}

/// Starts this long-lived vault job's network-place keeper candidate (`aterm_netplace::keeper`,
/// once per process): with no window open, the subscriber or the server keeps the place, so a
/// move is read where it happens and the lanes that read it pace and catch up on time. Never in
/// a test, which must not touch the real state root.
#[cfg(unix)]
fn keep_network_place() {
    #[cfg(not(test))]
    if let Some(state) = aterm_types::dirs::state_dir() {
        let _ = aterm_netplace::keeper::start(&state);
    }
}

/// Decide the advertised mDNS name and ALab pin for one tick of `serve` (`DESIGN-fluid-swarm`
/// §4.1 F1: evaluated on each advert tick rather than once at start).
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct ServeRole {
    scoped: bool,
    last: Option<(crate::client::AlabRoleContext, String)>,
    /// Polls in a row whose denial check failed ([`ROLE_UNJUDGED_POLLS`]).
    unjudged: u32,
}

/// How many role decisions in a row a held role outlasts a denial check that fails. It counts
/// decisions, not time: the advertiser decides at its 30 s tick and again whenever it is woken (a
/// stored version, a network change), so ten take at most five minutes on a quiet server and can
/// take far less on a busy one. A count keeps the bound deterministic, and a server woken more
/// often only withdraws authority sooner. A fault of a moment costs a Mac still in nothing, and one
/// that lasts cannot keep a denied Mac serving for as long as it lasts. A poll that judges this Mac
/// before the bound starts the count again; once the bound has ended the role, only a poll that
/// grants it ([`crate::client::AlabRoleObservation::Allowed`]) gives it back, never one that only
/// keeps a held role. Applied records that are absent do not count: the cached envelope still
/// judges this Mac, and a warm role outlasts that
/// (`docs/measured/vault-cold-server-role-2026-11-04.md`); records there and unreadable do.
#[cfg(unix)]
pub(crate) const ROLE_UNJUDGED_POLLS: u32 = 10;

#[cfg(all(test, unix))]
mod role_tests;

#[cfg(unix)]
impl ServeRole {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRoleRetention",
            action = "Warm",
            reason = "The process already established a role from available authenticated evidence"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRoleRetention",
            action = "LoseCustody",
            reason = "The observer classifies missing or unavailable evidence before role retention"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRoleRetention",
            action = "ChangeContext",
            reason = "Configuration or recipient changes produce a distinct context in the observer"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRoleRetention",
            action = "Deny",
            reason = "The observer supplies authenticated denial independently of readiness"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRoleRetention",
            action = "FailDenialCheck",
            reason = "The observer reports a denial check that failed after its one retry"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRoleRetention",
            action = "Outlast",
            reason = "The polls of a server under one fault pass in its environment"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "VaultRoleRetention",
            action = "Observe",
            project = "aterm_secrets::cli::role_tests::role_transition"
        )
    )]
    fn apply(
        &mut self,
        observation: crate::client::AlabRoleObservation,
        fallback: (Option<String>, Option<String>),
        pins: &mut Vec<(String, [u8; 32])>,
        alab_key: Option<[u8; 32]>,
    ) -> (Option<String>, Option<String>) {
        use crate::client::AlabRoleObservation::{
            Allowed, Denied, Missing, NotApplicable, Unavailable, Unjudged,
        };
        self.unjudged = if matches!(observation, Unjudged(_)) {
            self.unjudged.saturating_add(1)
        } else {
            0
        };
        let held = || {
            (
                None,
                Some("ALab server role is not currently established".to_owned()),
            )
        };
        let label = match observation {
            Missing if !self.scoped => return fallback,
            NotApplicable => {
                self.last = None;
                return if self.scoped && fallback.0.as_deref() == Some(crate::alab_advertised()) {
                    held()
                } else {
                    fallback
                };
            }
            Missing | Denied => {
                self.scoped = true;
                self.last = None;
                return held();
            }
            Unavailable(context) => {
                self.scoped = true;
                self.last = self
                    .last
                    .take()
                    .filter(|(prior, _)| Some(prior) == context.as_ref());
                self.last.as_ref().map(|(_, label)| label.clone())
            }
            Unjudged(context) => {
                self.scoped = true;
                let within = self.unjudged <= ROLE_UNJUDGED_POLLS;
                self.last = self
                    .last
                    .take()
                    .filter(|(prior, _)| within && *prior == context);
                self.last.as_ref().map(|(_, label)| label.clone())
            }
            Allowed { context, own_label } => {
                self.scoped = true;
                self.last = own_label
                    .filter(|label| {
                        !label.is_empty()
                            && label != crate::alab_advertised()
                            && crate::text::valid_machine_name(label)
                    })
                    .map(|label| (context, label));
                self.last.as_ref().map(|(_, label)| label.clone())
            }
        };
        let Some(label) = label else {
            return held();
        };
        alab_policy_role(
            fallback.0,
            fallback.1,
            Some((true, Some(label))),
            pins,
            alab_key,
        )
    }

    pub(crate) fn decide(
        &mut self,
        advertise: &Advertise,
        store: &std::path::Path,
        paths: &crate::client::Paths,
        pins: &mut Vec<(String, [u8; 32])>,
        alab_key: Option<[u8; 32]>,
    ) -> (Option<String>, Option<String>) {
        self.decide_after_store(advertise, store, paths, pins, alab_key, || {})
    }

    pub(crate) fn decide_after_store(
        &mut self,
        advertise: &Advertise,
        store: &std::path::Path,
        paths: &crate::client::Paths,
        pins: &mut Vec<(String, [u8; 32])>,
        alab_key: Option<[u8; 32]>,
        before_observation: impl FnOnce(),
    ) -> (Option<String>, Option<String>) {
        let holds = crate::server::holds_alab(store);
        before_observation();
        // Fallback and eligibility must use one observation. In particular, an older ALab
        // config read cannot make a later missing-config observation a standalone server.
        let observation = crate::client::observe_alab_role(paths);
        let follows = matches!(
            &observation,
            crate::client::AlabRoleObservation::Allowed { .. }
                | crate::client::AlabRoleObservation::Denied
                | crate::client::AlabRoleObservation::Unavailable(_)
                | crate::client::AlabRoleObservation::Unjudged(_)
        );
        let here = match (holds, follows) {
            (true, true) => AlabHere::Server,
            (true, false) => AlabHere::Copy,
            (false, _) => AlabHere::Empty,
        };
        let (advertised, held_back) = alab_role(advertise.clone(), here, pins, alab_key);
        self.apply(observation, (advertised, held_back), pins, alab_key)
    }
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
                .filter(|_| !secs.is_empty() && secs.bytes().all(|b| b.is_ascii_digit()))
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
            let f = flags(rest, &[], &["--json"])?;
            no_operands(&f, "status")?;
            Ok(if f.has("--json") {
                Command::StatusJson
            } else {
                Command::Status
            })
        }
        "repair" => {
            let f = flags(rest, &[], &["--json"])?;
            no_operands(&f, "repair")?;
            Ok(Command::Repair {
                json: f.has("--json"),
            })
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
            let flags = flags(rest, &[], &["--diagnostic"])?;
            no_operands(&flags, "sync")?;
            Ok(Command::Sync {
                diagnostic: flags.has("--diagnostic"),
            })
        }
        "watch" => {
            no_operands(&flags(rest, &[], &[])?, "watch")?;
            Ok(Command::Watch)
        }
        "mirrors" => {
            let f = flags(rest, &[], &[])?;
            match f.operands.split_first() {
                None => Ok(Command::Mirrors {
                    change: None,
                    urls: Vec::new(),
                }),
                Some((verb, urls)) if (verb == "add" || verb == "remove") && !urls.is_empty() => {
                    Ok(Command::Mirrors {
                        change: Some(verb == "add"),
                        urls: urls.to_vec(),
                    })
                }
                _ => Err("mirrors takes nothing, add <url>... or remove <url>...".into()),
            }
        }
        "ls" => {
            let f = flags(rest, &[], &["--describe", "--json"])?;
            no_operands(&f, "ls")?;
            Ok(Command::Ls {
                describe: f.has("--describe"),
                json: f.has("--json"),
            })
        }
        "get" => {
            let f = flags(rest, &[], &[])?;
            match f.operands.as_slice() {
                [name] => Ok(Command::Get(name.clone())),
                _ => Err("get takes one name".into()),
            }
        }
        "keep" => {
            let f = flags(rest, &[], &[])?;
            if f.operands.is_empty() {
                return Err("keep takes one or more names".into());
            }
            Ok(Command::Keep(f.operands))
        }
        "git-setup" => {
            let f = flags(rest, &[], &["--apply"])?;
            no_operands(&f, "git-setup")?;
            Ok(Command::GitSetup {
                apply: f.has("--apply"),
            })
        }
        "git-credential" => {
            let f = flags(rest, &["--dev-user", "--token-file", "--token-key"], &[])?;
            if f.operands.len() > 1 {
                return Err("git-credential takes at most one operation".into());
            }
            for flag in ["--dev-user", "--token-file", "--token-key"] {
                if f.all(flag).len() > 1 {
                    return Err(format!("git-credential takes at most one {flag}"));
                }
            }
            let dev_user = f.one("--dev-user");
            if dev_user
                .as_deref()
                .is_some_and(|user| !is_github_login(user))
            {
                return Err("git-credential --dev-user takes a GitHub login".into());
            }
            let token_file = f.one("--token-file").map(std::path::PathBuf::from);
            if token_file
                .as_deref()
                .is_some_and(|file| !file.is_absolute())
            {
                return Err("git-credential --token-file takes an absolute path".into());
            }
            let token_key = f.one("--token-key");
            if let Some(key) = &token_key {
                if token_file.is_none() {
                    return Err("git-credential --token-key reads the --token-file it names".into());
                }
                if !crate::token_line::assignable(key) {
                    return Err("git-credential --token-key takes an assignment name".into());
                }
            }
            Ok(Command::GitCredential {
                dev_user,
                token_file,
                token_key,
                operation: f.operands.first().cloned(),
            })
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
            let f = flags(rest, &[], &["--json"])?;
            no_operands(&f, "requests")?;
            Ok(Command::Requests {
                json: f.has("--json"),
            })
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
            let f = flags(rest, &[], &["--drop-sections", "--stolen"])?;
            match f.operands.as_slice() {
                [host] => Ok(Command::Revoke {
                    host: host.clone(),
                    drop_sections: f.has("--drop-sections"),
                    stolen: f.has("--stolen"),
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
            let f = flags(rest, &["--expect"], &[])?;
            match f.operands.as_slice() {
                [verb] if verb == "init" && f.one("--expect").is_none() => Ok(Command::CommonsInit),
                [verb] if verb == "restore" && f.all("--expect").len() <= 1 => {
                    let expected = f
                        .one("--expect")
                        .map(|text| {
                            aterm_codec::hex::decode_vec(text.as_bytes())
                                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                                .ok_or_else(|| {
                                    "--expect requires a 64-digit plan digest".to_owned()
                                })
                        })
                        .transpose()?;
                    Ok(Command::CommonsRestore { expected })
                }
                _ => Err("commons takes init or restore [--expect <plan-digest>]".into()),
            }
        }
        "genesis" => {
            let f = flags(rest, &[], &[])?;
            match f.operands.as_slice() {
                [file] => Ok(Command::Genesis(file.clone())),
                _ => Err("genesis takes the new file to write".into()),
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

/// Whether `value` is a syntactically valid GitHub account login.
pub(crate) fn is_github_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(unix)]
mod unix {
    use super::{
        Action, Command, Fill, Password, SERVE_SWITCHES, SERVE_VALUED, flags, is_github_login,
        serve_settings,
    };
    use crate::client::{
        self, Approval, JoinEvent, JoinOptions, JoinOutcome, JoinPassword, Membership, Paths,
        Published, SyncOutcome,
    };
    use crate::need::{GhProfile, Want};
    use crate::service::{self, Job};
    use crate::text::Audience;
    use crate::transport::{Source, describe};
    use crate::{Error, SecretBytes, envelope, fsx, join, text};
    use std::cell::Cell;
    use std::io::{IsTerminal, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::Duration;
    use zeroize::Zeroize;

    /// What an open vault means, said where one is made or shown.
    const OPEN_VAULT: &str = "this vault is open: a machine joins with no password or code once a person presses Allow on any member and types the phrase it shows (at the ALab vault, a Mac on the home ALab LAN that proves itself to a member there joins with no Allow; one on a phone hotspot or any other network waits for an Allow), and reads [everyone]'s entries; a key you let go comes back only by its code, but a machine that makes a new key asks as a new machine (aterm secrets password set also lets in a machine that types the join password)";

    /// What a vault at a path says where one served at a URL says whom its `[join]` lets in: a
    /// path has no join queue, so nothing lets a machine in by itself there.
    const PATH_JOINS: &str = "a vault at a path has no join queue: a machine joins it only when an admin approves its key: aterm secrets approve <age1…> --name <host>";

    /// What a publish that let a machine go from an open vault says: only its key is kept out,
    /// since a new key still joins by a person's Allow on any member.
    const KEPT_OUT: &str = "the vault stays open, but not to the key you let go: it is a `code-only` line in [join], so that key is let back in only by its code (aterm secrets approve), never by a member's Allow. A new key still joins when a person presses Allow on any member (at the ALab vault, also when it proves itself to a member on the home ALab LAN), so if the machine was lost or compromised, let go any machine in [machines] (aterm secrets edit) it may have joined as since, even under its old name, and only then rotate what it could read";

    /// [`KEPT_OUT`] for a vault at a path: only an admin's approve of a key lets a machine in
    /// there, which a `code-only` line does not stop. A server may still serve the file
    /// (`server::take_from_store`), so the text gives [`KEPT_OUT`]'s steps too.
    const KEPT_OUT_AT_A_PATH: &str = "at a path no server serves, the key you let go comes back only when an admin approves it (aterm secrets approve <age1…> --name <host>): if the machine was lost or compromised, rotate what it could read. If aterm secrets serve serves this file, it serves this version from its next request, and a new key still joins it when a person presses Allow on any member: if the machine was lost or compromised, let go any machine in [machines] (aterm secrets edit) it may have joined as since, even under its old name, and only then rotate what it could read";

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
        if let Command::GitSetup { apply } = command {
            // Local developer setup must not initialize, join, or move this Mac's identity.
            let paths = Paths::locate()?;
            let program = std::env::current_exe()?;
            let report = crate::git_setup::setup(&paths.home, &program, apply)?;
            println!(
                "{} {} known helper configuration file(s) using {}",
                if apply { "updated" } else { "would update" },
                report.files.len(),
                report.program.display()
            );
            if let Some(backup) = report.backup {
                println!("private rollback files: {}", backup.display());
            }
            return Ok(ExitCode::SUCCESS);
        }
        if let Command::GitCredential {
            dev_user,
            token_file,
            token_key,
            operation,
        } = command
        {
            let dev = match &token_file {
                Some(file) => DevToken::File {
                    file,
                    key: token_key.as_deref(),
                },
                None => DevToken::Vault,
            };
            return git_credential(dev_user.as_deref(), &dev, operation.as_deref());
        }
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
        let node_state = || service::node_state(&paths.home);
        let subscriber = Subscriber {
            state: &state,
            install: &install,
            node: aterm_types::mesh_switch::MESH
                .then_some(&node_state as &dyn Fn() -> crate::Result<service::State>),
        };
        match command {
            Command::Status => status(&paths, &subscriber),
            Command::StatusJson => {
                println!("{}", crate::operational::read(&paths)?);
                Ok(ExitCode::SUCCESS)
            }
            Command::Repair { json } => repair(&paths, json),
            Command::Join {
                source,
                name,
                agent,
                wait,
                password_stdin,
            } => join(&paths, &source, name, agent, wait, password_stdin),
            Command::Sync { diagnostic } => sync(&paths, diagnostic),
            Command::Watch => {
                super::keep_network_place();
                client::watch(&paths, &|line| {
                    crate::log_line(&format!("{} {line}", stamp()));
                })?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Mirrors { change, urls } => mirrors(&paths, change, &urls),
            Command::Ls { describe, json } => {
                let names = client::registered_names(&paths)?;
                // The vault's Claude Code tokens, value-free: best effort, so a vault that cannot
                // be read just now still lists what the registry names.
                let tokens = match crate::claude_token::listed(&paths) {
                    Ok(crate::claude_token::Lookup::Found(standings)) => {
                        super::inventory::token_rows(
                            &standings,
                            aterm_time::unix_secs().unwrap_or(0),
                        )
                    }
                    _ => Vec::new(),
                };
                print!(
                    "{}",
                    super::inventory::listing(&names, &tokens, describe, json)?
                );
                Ok(ExitCode::SUCCESS)
            }
            Command::Get(name) => get(&paths, &name),
            Command::Keep(names) => keep(&paths, &names),
            Command::GitCredential { .. } => unreachable!(),
            Command::GitSetup { .. } => unreachable!(),
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
            Command::Unset { name, hosts } => unset(&paths, &name, &hosts),
            Command::Password { action, agent } => password(&paths, action, agent, &subscriber),
            Command::Requests { json } => requests(&paths, json),
            Command::Approve { which, all, name } => approve(&paths, &which, all, name),
            Command::Revoke {
                host,
                drop_sections,
                stolen,
            } => revoke(&paths, &host, drop_sections, stolen),
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
                let host = client::machine_named_in_vault(&paths, &admin, &host)?;
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
            Command::CommonsRestore { expected } => {
                let hex = aterm_codec::hex::encode_lower;
                if let Some(expected) = expected {
                    let done = client::restore_commons_control(&paths, &expected)?;
                    println!(
                        "{}",
                        aterm_json::json!({
                            "schema": 1, "kind": "commons-control-recovery",
                            "scope": "local-committed-state", "changed": done.changed,
                            "network_id": hex(&done.trust.network),
                            "authority": hex(&done.trust.authority), "genesis": hex(&done.trust.genesis),
                            "epoch": hex(&done.epoch), "recipients": done.recipients,
                            "convergence_verified": false,
                        })
                    );
                } else {
                    let plan = client::plan_commons_control_recovery(&paths)?;
                    println!(
                        "{}",
                        aterm_json::json!({
                            "schema": 1, "kind": "commons-control-recovery-plan",
                            "scope": "authenticated-local-snapshot", "plan_digest": hex(&plan.digest),
                            "network_id": hex(&plan.trust.network),
                            "authority": hex(&plan.trust.authority), "genesis": hex(&plan.trust.genesis),
                            "vault_version": plan.vault_version, "current_epoch": hex(&plan.current_epoch),
                            "needed": plan.needed, "published_descriptor": plan.published_descriptor,
                            "record_capacity": {
                                "held_records": plan.record_capacity.current.held(),
                                "additional_records": plan.record_capacity.additional.held(),
                                "required_records": plan.record_capacity.current.plus(plan.record_capacity.additional).held(),
                                "held_limit": plan.record_capacity.budgets.held,
                                "weighed_records": plan.record_capacity.current.weighed,
                                "weighed_limit": plan.record_capacity.budgets.weighed,
                                "keyed_records": plan.record_capacity.current.keyed,
                                "keyed_limit": plan.record_capacity.budgets.keyed,
                                "exceeded": plan.record_capacity.exceeded.map(|budget| budget.counts()),
                            },
                            "recipients": plan.recipients.iter().map(|target| aterm_json::json!({
                                "recipient": target.recipient, "label": target.label, "restored": target.restored,
                            })).collect::<Vec<_>>(),
                        })
                    );
                }
                Ok(ExitCode::SUCCESS)
            }
            Command::Genesis(file) => {
                let done = client::configured_genesis(&paths, Path::new(&file))?;
                println!(
                    "Genesis v2 digest {}, {} members",
                    aterm_codec::hex::encode_lower(&done.digest),
                    done.members
                );
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

    /// The machines `words` name in `doc`, each by [`client::machine_named`]: the tests' reading
    /// of a document alone, where the verbs read the vault ([`machines_named_in_vault`]).
    #[cfg(test)]
    pub(super) fn machines_named(
        doc: &text::Document,
        words: &[String],
    ) -> crate::Result<Vec<String>> {
        words
            .iter()
            .map(|word| client::machine_named(doc, word))
            .collect()
    }

    fn machines_named_in_vault(
        paths: &Paths,
        admin: &client::Admin,
        words: &[String],
    ) -> crate::Result<Vec<String>> {
        words
            .iter()
            .map(|word| client::machine_named_in_vault(paths, admin, word))
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
        if !done.waiting.is_empty() {
            println!(
                "{} {} for a removal's new key: this Mac's vault pass signs {} once it arrives (aterm secrets edit shows {} until then)",
                done.waiting.join(", "),
                if done.waiting.len() == 1 {
                    "waits"
                } else {
                    "wait"
                },
                if done.waiting.len() == 1 {
                    "it"
                } else {
                    "them"
                },
                if done.waiting.len() == 1 {
                    "it"
                } else {
                    "them"
                },
            );
        }
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
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/dev/tty")
            .ok()?;
        if !std::io::IsTerminal::is_terminal(&tty) {
            return None;
        }
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
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/dev/tty")
            .ok()?;
        if !std::io::IsTerminal::is_terminal(&tty) {
            return None;
        }
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
    /// around it dropped ([`read_piped_password`]).
    fn piped_password(what: &str) -> crate::Result<SecretBytes> {
        if std::io::stdin().is_terminal() {
            return Err(Error::new(format!("{what} reads the password from a pipe")));
        }
        read_piped_password(what, &mut std::io::stdin().lock())
    }

    /// The most a piped password's input may hold, blanks around the password included: past it
    /// the input is refused whole, never cut to a password the person did not type.
    const PIPED_PASSWORD_INPUT_MAX: usize = 4096;

    /// [`piped_password`]'s reading of `input`: one line of at most [`join::MAX_LEN`] bytes, blanks
    /// around it dropped. A longer password, an input past [`PIPED_PASSWORD_INPUT_MAX`], or one
    /// that breaks its line with a CR or LF is refused.
    pub(super) fn read_piped_password(
        what: &str,
        input: &mut dyn Read,
    ) -> crate::Result<SecretBytes> {
        let too_long = || {
            Error::new(format!(
                "{what}: a join password is at most {} bytes",
                join::MAX_LEN
            ))
        };
        let mut bytes = Vec::with_capacity(PIPED_PASSWORD_INPUT_MAX + 1);
        crate::secret::read_to_end_wiping_on_error(
            input.take(PIPED_PASSWORD_INPUT_MAX as u64 + 1),
            &mut bytes,
        )?;
        if bytes.len() > PIPED_PASSWORD_INPUT_MAX {
            bytes.zeroize();
            return Err(too_long());
        }
        let trimmed = SecretBytes::from(bytes.trim_ascii());
        bytes.zeroize();
        if trimmed.as_slice().is_empty() {
            return Err(Error::new(format!("{what}: nothing was piped in")));
        }
        if trimmed.as_slice().contains(&b'\n') || trimmed.as_slice().contains(&b'\r') {
            return Err(Error::new(format!("{what}: the password is one line")));
        }
        if trimmed.as_slice().len() > join::MAX_LEN {
            return Err(too_long());
        }
        Ok(trimmed)
    }

    /// What a view a person edits from shows of this Mac's `[everyone]` writes waiting for a
    /// removal's new key (one-mesh W3), and of the ones a later change overtook, each record once:
    /// a record, never an alert.
    fn view_notes(paths: &Paths, admin: &client::Admin) {
        let waiting = admin.waiting();
        if !waiting.is_empty() {
            eprintln!(
                "waiting for a removal's new key: {} (this Mac's writes, shown as they will be; a vault pass signs them once the key arrives, and an edit that changes one or sets it back takes it back)",
                waiting.join(", ")
            );
        }
        for unread in client::waiting_unread(paths) {
            match unread {
                client::UnreadWaiting::Waiting(file) => eprintln!(
                    "not read: {file} does not open with this Mac's key; nothing waiting is signed over it, and a save that writes [everyone] is refused until it is moved aside (which drops the writes it holds)"
                ),
                client::UnreadWaiting::Pending(file) => eprintln!(
                    "not read: {file} does not open with this Mac's key; nothing waiting is signed or landed over it, and a save whose [everyone] writes would wait for the new key, or that replaces a waiting one, is refused until it is moved aside (which drops the writes it holds)"
                ),
            }
        }
        let overtaken = admin.overtaken();
        for record in overtaken {
            eprintln!("overtaken: {}", record.line(|ms| ago(ms / 1000)));
        }
        if !overtaken.is_empty()
            && let Err(error) = client::overtaken_shown(paths, admin)
        {
            eprintln!("overtaken: not marked shown ({error})");
        }
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
        if !crate::is_alab_default_url(url) {
            join.push(' ');
            join.push_str("<vault-url>");
        }
        if let Some(name) = name {
            join.push_str(" --name ");
            join.push_str(name);
        }
        let source_note = if crate::is_alab_default_url(url) {
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
        let user_state = service::state(&paths.home, job);
        if job == Job::Server && matches!(user_state, Ok(service::State::Absent)) {
            return match service::daemon_state_for_home(&paths.home) {
                Ok(service::State::Absent) => "not installed".into(),
                Ok(service::State::Installed { pid: Some(pid), .. }) => {
                    format!(
                        "running as LaunchDaemon {} (pid {pid})",
                        service::DAEMON_LABEL
                    )
                }
                Ok(service::State::Installed { pid: None, .. }) => {
                    format!(
                        "LaunchDaemon {} installed, not running",
                        service::DAEMON_LABEL
                    )
                }
                Err(error) => format!("unknown ({error})"),
            };
        }
        match job {
            Job::Server => server_words(paths, &user_state),
            Job::Subscriber => service_words(&user_state),
        }
    }

    /// The server job's words: its state and, once installed, what the store its unit names
    /// holds ([`super::served_by`], [`crate::server::held_vaults`]), read from the store, not asked
    /// of the server, so a server that runs and serves nothing says so.
    pub(super) fn server_words(paths: &Paths, state: &crate::Result<service::State>) -> String {
        let words = service_words(state);
        if !matches!(state, Ok(service::State::Installed { .. })) {
            return words;
        }
        let Some((store, _)) = service::unit_argv(&paths.home, Job::Server)
            .and_then(|argv| super::served_by(paths, argv.get(1..)?))
        else {
            return words;
        };
        match crate::server::held_vaults(&store) {
            Ok(held) if held.is_empty() => format!("{words}, holds no vault"),
            Ok(held) => format!("{words}, holds {}", held.join(", ")),
            Err(error) => format!("{words}, its store unreadable ({error})"),
        }
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
        /// The node service's state, with the mesh on ([`service::node_state`]): its vault pass
        /// is this Mac's, and `status` reads it.
        pub(super) node: Option<&'a dyn Fn() -> crate::Result<service::State>>,
    }

    /// What the node service's vault pass did last, in `aterm secrets status`'s words.
    pub(super) fn node_pass_words(
        state: &crate::Result<service::State>,
        receipt: Option<&client::NodePassReceipt>,
        turned_off: bool,
    ) -> String {
        let job = service_words(state);
        if turned_off {
            return format!("{job}, keeps no vault work (aterm secrets agent off)");
        }
        let Some(receipt) = receipt else {
            return format!("{job}, has not looked at the vault yet");
        };
        let when = ago(receipt.ran_ms / 1000);
        match receipt.said {
            client::NodePassSaid::Done => format!("{job}, looked at the vault {when}"),
            client::NodePassSaid::Busy => {
                format!("{job}, waited for another vault write {when} and looks again")
            }
            client::NodePassSaid::Failed => {
                format!("{job}, could not finish its look at the vault {when} and looks again")
            }
        }
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

    /// Who an approver for `approval` lets in: an open vault admits nothing new by its asking
    /// alone, only a machine a person allowed on a member (docs/DESIGN-fluid-swarm-2026-10-28.md
    /// F10). A Mac on the home ALab LAN is let in by a member when it proves itself, never by an
    /// approver (docs/DESIGN-automatic-lan-admission-2026-11-02.md).
    pub(super) const fn joiner(approval: Approval) -> &'static str {
        match approval {
            Approval::Password => "a machine that types the join password",
            Approval::Open => "a machine allowed on any member",
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

    fn repair(paths: &Paths, json: bool) -> crate::Result<ExitCode> {
        let (rows, complete) = repair_plan_with(
            || {
                client::repair_legacy_frontier(
                    paths,
                    std::time::Instant::now() + std::time::Duration::from_secs(5),
                )
            },
            &mut |job| client::mend_held(paths, job),
        );
        if json {
            println!(
                "{}",
                aterm_json::json!({"schema":1,"services":rows,"complete":complete})
            );
        } else {
            for row in rows {
                println!(
                    "{}: {} — {}",
                    row["service"].as_str().unwrap_or("?"),
                    row["outcome"].as_str().unwrap_or("?"),
                    row["detail"].as_str().unwrap_or("?")
                );
            }
        }
        Ok(ExitCode::from(if complete { 0 } else { 3 }))
    }

    /// Service migration must get its chance before waiting for the vault a
    /// subscriber may hold. Each independent result remains visible even when
    /// another repair is unavailable.
    pub(super) fn repair_plan_with(
        frontier: impl FnOnce() -> crate::Result<bool>,
        repair: &mut impl FnMut(Job) -> crate::Result<client::HeldMend>,
    ) -> (Vec<aterm_json::Value>, bool) {
        let (service_rows, service_complete) = repair_with(repair);
        let (outcome, detail, frontier_complete) = match frontier() {
            Ok(true) => ("repaired", "re-acknowledged the verified local membership log; previous state retained beside state.toml".to_owned(), true),
            Ok(false) => ("unchanged", "the applied membership frontier matches the verified local log".to_owned(), true),
            Err(error) => (
                if error.kind() == crate::ErrorKind::TimedOut { "unavailable" } else { "failed" },
                error.to_string(),
                false,
            ),
        };
        let mut rows = vec![aterm_json::json!({
            "service":"membership-frontier", "outcome":outcome, "detail":detail,
        })];
        rows.extend(service_rows);
        (rows, frontier_complete && service_complete)
    }

    pub(super) fn repair_with(
        repair: &mut impl FnMut(Job) -> crate::Result<client::HeldMend>,
    ) -> (Vec<aterm_json::Value>, bool) {
        use client::{HeldKept, HeldMend};
        let mut complete = true;
        let rows = [Job::Server, Job::Subscriber]
            .into_iter()
            .map(|job| {
                let (outcome, detail) = match repair(job) {
                    Ok(HeldMend::Moved(line, why)) => {
                        ("repaired", format!("{line}; {}", why.why()))
                    }
                    Ok(HeldMend::NotHeld) => {
                        ("unchanged", "no held service needs migration".into())
                    }
                    Ok(HeldMend::Kept(why)) => (
                        "kept",
                        match why {
                            HeldKept::Pinned => "explicit service pin retained".into(),
                            HeldKept::Development => {
                                complete = false;
                                "repair requires an installed release".into()
                            }
                            HeldKept::NoAdmin => {
                                complete = false;
                                "no authorized migration applies".into()
                            }
                            HeldKept::Reads => "service already reads this vault format".into(),
                            HeldKept::Unknown(why) => {
                                complete = false;
                                why
                            }
                        },
                    ),
                    Ok(HeldMend::Unread(why)) => {
                        complete = false;
                        ("unavailable", why)
                    }
                    Err(error) => {
                        complete = false;
                        ("unavailable", error.to_string())
                    }
                };
                aterm_json::json!({"service":job.label(),"outcome":outcome,"detail":detail})
            })
            .collect();
        (rows, complete)
    }

    /// Who writes this Mac's vault, as `status` says it (DESIGN-claude-setup-token §3.9): `vault
    /// writes:` by this Mac's route ([`client::write_route`], read at once: a busy state is
    /// unknown), `admin key:` with the verbs that need the admin locker on this vault
    /// ([`super::admin_verbs`]), and on a host Mac its per-host copies that shadow `[everyone]`,
    /// counted, which only a host can see.
    pub(super) fn writes_lines(paths: &Paths) -> Vec<String> {
        let mut lines = Vec::new();
        let route = client::write_route(paths, std::time::Instant::now());
        lines.push(match &route {
            Ok(route) => format!("vault writes: {}", super::vault_writes(*route)),
            Err(error) => format!("vault writes: unknown ({error})"),
        });
        let verbs = route.as_ref().ok().map(|route| super::admin_verbs(*route));
        let opens = verbs
            .filter(|verbs| !verbs.is_empty())
            .map(|verbs| format!(" (on this vault it opens for: {})", verbs.join(", ")))
            .unwrap_or_default();
        lines.push(match client::can_administer(paths) {
            Ok(true) => format!("admin key: yes{opens}"),
            Ok(false) => format!("admin key: no{opens}"),
            Err(error) => format!("admin key: unknown ({error})"),
        });
        match client::per_host_shadows(paths) {
            Ok(0) => {}
            Ok(shadows) => lines.push(format!("per-host copies: {shadows} shadow [everyone]")),
            Err(error) => lines.push(format!("per-host copies: unknown ({error})")),
        }
        lines
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
                if !config.mirrors.is_empty() {
                    let shown: Vec<String> =
                        config.mirrors.iter().map(|url| describe(url)).collect();
                    println!("mirrors: {}", shown.join(", "));
                }
                for note in client::mirror_notes(paths) {
                    println!("mirror {note}");
                }
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
            // A locker opened, but no seat of the log binds this Mac: it is no member, and its
            // subscriber asks for a seat (one-mesh S1).
            Membership::In { seq, heard } if client::seat_asks(paths) => {
                println!(
                    "membership: {}, seq {seq}, last heard {}",
                    client::Seat::SEATLESS,
                    ago(heard)
                );
            }
            Membership::In { seq, heard } => {
                println!("membership: in, seq {seq}, last heard {}", ago(heard));
            }
            Membership::Revoked { seq } => {
                println!("membership: {}", client::out_words(paths, seq))
            }
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
            for line in writes_lines(paths) {
                println!("{line}");
            }
        }
        match client::swarm_lease_holder(paths) {
            Ok(Some(holder)) => println!("vault steward: {holder}"),
            Ok(None) => println!("vault steward: none recorded"),
            Err(error) => println!("vault steward: unknown ({error})"),
        }
        match client::release::signing_status(paths) {
            Ok(client::release::SigningStatus::Ready(now)) => {
                println!("release signing: ready ({now})")
            }
            Ok(client::release::SigningStatus::Pending(why)) => {
                println!("release signing: pending ({why})")
            }
            Ok(client::release::SigningStatus::NotApplicable(_)) => {}
            Err(_) => println!(
                "release signing: pending (local state unreadable; automatic setup will retry)"
            ),
        }
        match client::conflicts(paths) {
            Ok(conflicts) => {
                let (plain, tokens) = client::Conflict::split(&conflicts);
                if !plain.is_empty() {
                    println!(
                        "conflict: {} (saved with different values while apart; every Mac writes the same one, and {})",
                        plain.join(", "),
                        client::CONFLICT_PICK
                    );
                }
                if !tokens.is_empty() {
                    println!(
                        "conflict: {} (saved with different values while apart; every Mac reads the same one, and {})",
                        tokens.join(", "),
                        client::TOKEN_CONFLICT_PICK
                    );
                }
            }
            Err(error) => println!("conflict: unreadable ({error})"),
        }
        match client::reseal_waits(paths) {
            Ok(lines) => {
                for line in lines {
                    println!("waiting: {line}");
                }
            }
            Err(error) => println!("waiting: unreadable ({error})"),
        }
        match client::read_after_cutoff_lines(paths) {
            Ok(lines) => {
                for line in lines {
                    println!("read while out: {line}");
                }
            }
            Err(error) => println!("read while out: unreadable ({error})"),
        }
        match client::lab_set_aside(paths) {
            Ok(Some(set)) => println!("{}", set.status_line()),
            Ok(None) => {}
            Err(error) => println!("set aside: unreadable ({error})"),
        }
        match crate::need::peek(paths) {
            Ok(needs) => {
                if let Some(line) = crate::need::status_line(&needs) {
                    println!("{line}");
                }
            }
            Err(error) => println!("needs: unreadable ({error})"),
        }
        if let (Some(_), Some(node)) = (&status.config, subscriber.node) {
            println!(
                "node service: {}",
                node_pass_words(
                    &node(),
                    client::node_pass_receipt(paths).as_ref(),
                    service::subscriber_turned_off(&paths.home),
                )
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
        // Where this Mac is on the network, in the words every reporter uses: a vault that does
        // not answer from a phone hotspot is the network's doing, and this line says so.
        #[cfg(unix)]
        if let Some(state) = aterm_types::dirs::state_dir() {
            println!(
                "{}",
                aterm_netplace::words::header(
                    &aterm_netplace::read(&state),
                    &aterm_netplace::words::Words::now()
                )
            );
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
                "Join password (an admin Mac shows it: aterm secrets password; Enter joins without one and waits for Allow on any member, or for approval by code; at the ALab vault, a member on the home ALab LAN lets this Mac in by its proof): ",
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

    pub(super) fn sync_diagnostic(paths: &Paths) -> crate::sync_diagnostic::SyncDiagnostic {
        use crate::sync_diagnostic::SyncDiagnostic;
        match sync_outcome(paths) {
            Ok((SyncOutcome::Applied { seq, report }, _)) => SyncDiagnostic::Applied {
                seq,
                failed: report.failed.len() as u64,
            },
            Ok((SyncOutcome::Unchanged { seq }, _)) => SyncDiagnostic::Unchanged(seq),
            Ok((SyncOutcome::PutBack { seq, .. }, _)) => SyncDiagnostic::PutBack(seq),
            Ok((SyncOutcome::NotIn, _)) => SyncDiagnostic::NotIn,
            Err(error) => SyncDiagnostic::Failed(error.kind()),
        }
    }

    fn sync_outcome(paths: &Paths) -> crate::Result<(SyncOutcome, Option<Option<u64>>)> {
        let mut outcome = client::sync(paths, None)?;
        // Restore local files after putting the held version back on its source.
        let mut put_back = None;
        if let SyncOutcome::PutBack { served, .. } = outcome
            && !client::materialization_ready(paths)?
        {
            put_back = Some(served);
            outcome = client::sync(paths, None)?;
        }
        Ok((outcome, put_back))
    }

    /// `aterm secrets mirrors`: list this Mac's other addresses of its vault, after adding or
    /// removing (`change`) the `typed` ones. An added mirror is read once first: one serving a
    /// version this Mac's pin does not verify is refused, and nothing is saved.
    pub(super) fn mirrors(
        paths: &Paths,
        change: Option<bool>,
        typed: &[String],
    ) -> crate::Result<ExitCode> {
        let config = client::config(paths)?.ok_or_else(|| {
            Error::new("no vault configured: run aterm secrets join <url> (or aterm init)")
        })?;
        let config = match change {
            None => config,
            Some(add) => {
                let mut list = config.mirrors.clone();
                let mut heard = Vec::new();
                for typed in typed {
                    let at = |list: &[String], url: &str| {
                        list.iter()
                            .position(|kept| crate::discover::same_endpoint(kept, url))
                    };
                    if add {
                        let url = client::mirror_typed(&config, typed)?;
                        if at(&list, &url).is_some() {
                            heard.push(format!("{}: listed already", describe(&url)));
                            continue;
                        }
                        let said = match client::probe_mirror(&config, &url)? {
                            client::MirrorProbe::Serves(seq) => format!("serves seq {seq}"),
                            client::MirrorProbe::Empty => {
                                "holds no version of the vault yet; it is read once its server does"
                                    .to_owned()
                            }
                            client::MirrorProbe::Unreached(why) => {
                                format!("did not answer ({why})")
                            }
                        };
                        heard.push(format!("{}: {said}", describe(&url)));
                        list.push(url);
                    } else {
                        let base = typed.split_once('#').map_or(typed.as_str(), |(b, _)| b);
                        let Some(found) = at(&list, base.trim()) else {
                            return Err(Error::new(format!(
                                "{}: not a mirror of this vault",
                                describe(typed.trim())
                            )));
                        };
                        list.remove(found);
                    }
                }
                let saved = client::set_mirrors(paths, &config, &list)?;
                for line in heard {
                    println!("{line}");
                }
                saved
            }
        };
        match &config.source {
            Source::Url(url) => println!(
                "url: {} (the source: joins and admin publishes start here)",
                describe(url)
            ),
            Source::Path(path) => println!("path: {} (a path has no mirrors)", path.display()),
        }
        for url in &config.mirrors {
            println!("mirror: {} (read, never written to)", describe(url));
        }
        if config.mirrors.is_empty() && matches!(config.source, Source::Url(_)) {
            println!("no mirrors: aterm secrets mirrors add <url> lists one");
        }
        for note in client::mirror_notes(paths) {
            println!("mirror {note}");
        }
        Ok(ExitCode::SUCCESS)
    }

    fn sync(paths: &Paths, diagnostic: bool) -> crate::Result<ExitCode> {
        if diagnostic {
            let outcome = sync_diagnostic(paths);
            println!("{}", outcome.line());
            return Ok(if outcome.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        let held = |served: Option<u64>| {
            served.map_or("no vault".to_string(), |served| {
                format!("seq {served}, older than this Mac's")
            })
        };
        let (outcome, put_back) = sync_outcome(paths)?;
        // A put-back writes no file here: files still unsettled are applied from the copy the
        // source holds now. The first line says what the sync did, as before (a caller reads it).
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
                for (name, why) in &report.refused {
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

    fn keep(paths: &Paths, names: &[String]) -> crate::Result<ExitCode> {
        let mut all = true;
        for (name, kept) in client::keep(paths, names)? {
            match kept {
                client::Kept::Kept => println!("kept {name}"),
                client::Kept::Not(why) => {
                    all = false;
                    println!("not kept: {why}");
                }
            }
        }
        Ok(if all {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }

    fn get(paths: &Paths, name: &str) -> crate::Result<ExitCode> {
        let value = get_value(paths, name)?;
        let mut out = std::io::stdout().lock();
        out.write_all(value.as_slice())?;
        out.flush()?;
        Ok(ExitCode::SUCCESS)
    }

    pub(super) fn get_value(paths: &Paths, name: &str) -> crate::Result<crate::SecretBytes> {
        if crate::entry_class::claude_token(&name.to_ascii_lowercase()) {
            return Err(Error::new(format!("{name}: {CLAUDE_TOKEN_GET}")));
        }
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

    /// Why `secrets get` refuses a Claude Code token entry (DESIGN-claude-setup-token §4.2).
    pub(super) const CLAUDE_TOKEN_GET: &str = "a Claude Code token, which aterm hands only to Claude Code and never prints; aterm secrets ls --describe shows its state";

    const CREDENTIAL_QUIT: &[u8] = b"quit=1\n";
    const CREDENTIAL_INPUT_MAX: usize = 64 * 1024;
    const CREDENTIAL_TOKEN_MAX: usize = 4 * 1024;
    const INIT_TOML_MAX: usize = 64 * 1024;

    /// Where `aterm secrets git-credential` reads the dev token.
    #[derive(Debug, Clone, Copy)]
    pub(super) enum DevToken<'a> {
        /// The secrets registry's `gh:dev` entry ([`registry_token`]).
        Vault,
        /// `--token-file`, the whole file or (`--token-key`) its `<key>=` line.
        File {
            file: &'a Path,
            key: Option<&'a str>,
        },
    }

    /// What [`git_credential_reply`] answers: the stdout bytes git reads, and the GitHub profile
    /// the secrets registry could not give, which [`git_credential`] files in the need ledger.
    pub(super) struct CredentialAnswer {
        pub(super) reply: SecretBytes,
        pub(super) missing: Option<GhProfile>,
    }

    impl CredentialAnswer {
        fn reply(bytes: &[u8]) -> Self {
            Self {
                reply: SecretBytes::new(bytes.to_vec()),
                missing: None,
            }
        }

        fn token(username: &str, token: &SecretBytes) -> Self {
            Self {
                reply: format_credential_reply(username, token),
                missing: None,
            }
        }

        /// git stops on `quit=1`; the registry holds no usable `profile` token.
        fn missing(profile: GhProfile) -> Self {
            Self {
                reply: SecretBytes::new(CREDENTIAL_QUIT.to_vec()),
                missing: Some(profile),
            }
        }
    }

    /// `aterm secrets git-credential [--dev-user <login>] [--token-file <path> [--token-key
    /// <name>]] [<op>]`: git's credential helper over the secrets registry's GitHub tokens, the
    /// dev token read from `dev`. Answers on `get` only; `store`, `erase` and unknown operations
    /// write nothing and exit 0. A token this Mac's registry does not give is filed in the need
    /// ledger (`aterm secrets need gh:<profile>`) and git's request fails.
    fn git_credential(
        dev_user: Option<&str>,
        dev: &DevToken<'_>,
        operation: Option<&str>,
    ) -> crate::Result<ExitCode> {
        let stdin = std::io::stdin();
        let is_terminal = stdin.is_terminal();
        let paths = Paths::locate().ok();
        let answer = git_credential_reply(
            paths.as_ref(),
            dev_user,
            dev,
            operation,
            is_terminal,
            &mut stdin.lock(),
        );
        if !answer.reply.as_slice().is_empty() {
            let mut out = std::io::stdout().lock();
            out.write_all(answer.reply.as_slice())?;
            out.flush()?;
        }
        if let (Some(profile), Some(paths)) = (answer.missing, &paths) {
            file_missing(paths, profile, &mut std::io::stderr().lock());
        }
        Ok(ExitCode::SUCCESS)
    }

    /// File the GitHub token git asked for and this Mac's registry lacks in the need ledger,
    /// under the purpose of the agh identity it is (`agh.dev` or `agh.release`), so a repeat
    /// request reuses that row, and say on `err` which want was filed and its request id.
    pub(super) fn file_missing(paths: &Paths, profile: GhProfile, err: &mut dyn Write) {
        let purpose = match profile {
            GhProfile::Dev => crate::need::Purpose::AghDev,
            GhProfile::Release => crate::need::Purpose::AghRelease,
        };
        let want = Want::Gh(profile);
        let key = want.key();
        let line = match crate::need::raise(paths, want, purpose) {
            Ok(need) => format!(
                "aterm secrets git-credential: this Mac holds no {key}; asked the lab's Macs (request {})",
                aterm_codec::hex::encode_lower(&need.id)
            ),
            Err(why) => format!(
                "aterm secrets git-credential: this Mac holds no {key}; the request was not filed: {why}"
            ),
        };
        let _ = writeln!(err, "{line}");
    }

    /// Pure-input seam for [`git_credential`]: the exact stdout reply bytes, wiped on drop
    /// (`SecretBytes`), and the profile to file a need for. Reads the registry under `paths`;
    /// never writes.
    pub(super) fn git_credential_reply(
        paths: Option<&Paths>,
        dev_user: Option<&str>,
        dev: &DevToken<'_>,
        operation: Option<&str>,
        is_terminal: bool,
        input: &mut dyn Read,
    ) -> CredentialAnswer {
        if operation != Some("get") {
            return CredentialAnswer::reply(b"");
        }
        if is_terminal {
            return CredentialAnswer::reply(CREDENTIAL_QUIT);
        }
        let Some(raw) = read_credential_attributes(input) else {
            return CredentialAnswer::reply(b"");
        };
        let Ok(text) = std::str::from_utf8(raw.as_slice()) else {
            return CredentialAnswer::reply(b"");
        };
        if text.contains('\0') {
            return CredentialAnswer::reply(b"");
        }
        let (mut protocol, mut host, mut username) = (None, None, None);
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                break;
            }
            let Some((key, value)) = line.split_once('=') else {
                return CredentialAnswer::reply(b"");
            };
            match key {
                "protocol" => protocol = Some(value),
                "host" => host = Some(value),
                "username" => username = Some(value),
                _ => {}
            }
        }
        if !protocol.is_some_and(|p| p.eq_ignore_ascii_case("https"))
            || !host.is_some_and(is_github_credential_host)
        {
            return CredentialAnswer::reply(b"");
        }
        let Some(paths) = paths else {
            return CredentialAnswer::reply(CREDENTIAL_QUIT);
        };
        let public = aterm_types::channel::PUBLIC_OWNER;
        if username.is_some_and(|u| u.eq_ignore_ascii_case(public)) {
            return match registry_token(paths, GhProfile::Release) {
                Some(token) => CredentialAnswer::token(public, &token),
                None => CredentialAnswer::missing(GhProfile::Release),
            };
        }
        let unnamed =
            username.is_none_or(|u| u.is_empty() || u.eq_ignore_ascii_case("x-access-token"));
        let requested = username.filter(|u| !u.is_empty());
        let out_user = if unnamed {
            match requested {
                Some(explicit) => explicit.to_owned(),
                None => known_dev_login(&paths.home, dev_user)
                    .unwrap_or_else(|| "x-access-token".to_owned()),
            }
        } else {
            let requested = requested.unwrap_or_default();
            if !is_github_login(requested)
                || !known_dev_login(&paths.home, dev_user)
                    .is_some_and(|login| login.eq_ignore_ascii_case(requested))
            {
                return CredentialAnswer::reply(b"");
            }
            requested.to_owned()
        };
        match read_dev_token(paths, dev) {
            Some(token) => CredentialAnswer::token(&out_user, &token),
            None if matches!(dev, DevToken::Vault) => CredentialAnswer::missing(GhProfile::Dev),
            None => CredentialAnswer::reply(CREDENTIAL_QUIT),
        }
    }

    /// git's attribute lines, up to the blank line that ends them or the end of `input`. Every
    /// line git writes ends with its LF, so a last line cut off before one is refused (`None`):
    /// a `host=` value cut short could name another host.
    fn read_credential_attributes(input: &mut dyn Read) -> Option<SecretBytes> {
        let mut buf = Vec::with_capacity(128);
        let mut line_len = 0usize;
        let mut byte = [0u8; 1];
        loop {
            match input.read(&mut byte) {
                Ok(0) if line_len != 0 => {
                    buf.zeroize();
                    return None;
                }
                Ok(0) => break,
                Ok(_) => {
                    if buf.len() >= CREDENTIAL_INPUT_MAX {
                        buf.zeroize();
                        return None;
                    }
                    let b = byte[0];
                    byte.zeroize();
                    buf.push(b);
                    if b == b'\n' {
                        let content_len = if line_len > 0 && buf[buf.len() - 2] == b'\r' {
                            line_len - 1
                        } else {
                            line_len
                        };
                        if content_len == 0 {
                            break;
                        }
                        line_len = 0;
                    } else {
                        line_len += 1;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    byte.zeroize();
                    buf.zeroize();
                    return None;
                }
            }
        }
        Some(SecretBytes::new(buf))
    }

    fn is_github_credential_host(host: &str) -> bool {
        let host = match host.rsplit_once(':') {
            Some((name, port)) if !port.is_empty() && port.trim_start_matches('0') == "443" => name,
            Some(_) => return false,
            None => host,
        };
        let host = match host.strip_suffix('.') {
            Some(rest) if !rest.is_empty() => rest,
            _ => host,
        };
        host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("gist.github.com")
    }

    fn read_credential_token(path: &Path) -> Option<SecretBytes> {
        let raw = fsx::read_private(path, CREDENTIAL_TOKEN_MAX)
            .ok()
            .flatten()?;
        graphic_token(raw.as_slice())
    }

    /// The `profile` token through the one resolution path: the secrets registry this Mac
    /// applied names the entry ([`Want::entries`], [`crate::registry::read`]) and its file
    /// lands where the registry's pointer says ([`crate::materialize::target_path`]). `None`
    /// where the registry is unread, does not name it, or its file is missing, loose,
    /// symlinked or not one graphic token.
    fn registry_token(paths: &Paths, profile: GhProfile) -> Option<SecretBytes> {
        let names = Want::Gh(profile).entries();
        let registry = crate::registry::read(paths).ok()?;
        let entry = registry.iter().find(|entry| names.contains(&entry.name))?;
        let file = crate::materialize::target_path(&paths.home, &entry.name).ok()?;
        read_credential_token(&file)
    }

    /// The dev token `dev` names: the registry's `gh:dev` entry ([`registry_token`]), or the
    /// `--token-file`, whole or its `--token-key` line ([`crate::token_line::assigned`]); `None`
    /// where it is missing, loose, symlinked, not one graphic token, or (a key) assigned other
    /// than once.
    fn read_dev_token(paths: &Paths, dev: &DevToken<'_>) -> Option<SecretBytes> {
        match dev {
            DevToken::Vault => registry_token(paths, GhProfile::Dev),
            DevToken::File { file, key: None } => read_credential_token(file),
            DevToken::File {
                file,
                key: Some(key),
            } => {
                let raw = fsx::read_private(file, CREDENTIAL_INPUT_MAX)
                    .ok()
                    .flatten()?;
                let text = std::str::from_utf8(raw.as_slice()).ok()?;
                graphic_token(crate::token_line::assigned(text, key).ok()?.as_bytes())
            }
        }
    }

    /// `raw`, trimmed, when it is one token of graphic ASCII.
    fn graphic_token(raw: &[u8]) -> Option<SecretBytes> {
        let trimmed = raw.trim_ascii();
        if trimmed.is_empty() || !trimmed.iter().all(u8::is_ascii_graphic) {
            return None;
        }
        Some(SecretBytes::new(trimmed.to_vec()))
    }

    fn format_credential_reply(username: &str, token: &SecretBytes) -> SecretBytes {
        let mut out = Vec::with_capacity(
            "username=\npassword=\n".len() + username.len() + token.as_slice().len(),
        );
        out.extend_from_slice(b"username=");
        out.extend_from_slice(username.as_bytes());
        out.extend_from_slice(b"\npassword=");
        out.extend_from_slice(token.as_slice());
        out.extend_from_slice(b"\n");
        SecretBytes::new(out)
    }

    /// The dev login: `--dev-user`, else init.toml's `[github] account`.
    fn known_dev_login(home: &Path, explicit: Option<&str>) -> Option<String> {
        match explicit {
            Some(explicit) => Some(explicit.to_owned()),
            None => init_toml_github_account(home),
        }
    }

    fn init_toml_github_account(home: &Path) -> Option<String> {
        let bytes = fsx::read_owned(
            &home.join(".config").join("aterm").join("init.toml"),
            INIT_TOML_MAX,
        )
        .ok()
        .flatten()?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let Ok(aterm_toml::Value::Table(table)) = aterm_toml::from_str::<aterm_toml::Value>(text)
        else {
            return None;
        };
        table
            .get("github")
            .and_then(aterm_toml::Value::as_table)
            .and_then(|gh| gh.get("account"))
            .and_then(aterm_toml::Value::as_str)
            .filter(|account| is_github_login(account))
            .map(str::to_owned)
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
                    println!(
                        "the vault is open: a machine a person allows on any member is let in {by}"
                    );
                    println!(
                        "a join password lets a machine in with no Allow: aterm secrets password new"
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
                "machines that join it must pin this key: add #{} to the path, or confirm the fingerprint",
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
        // Query parameters may carry credentials. A template keeps those out of stdout
        // without suggesting that a stripped endpoint is a usable replacement.
        if url.contains('?') || url.contains('#') || aterm_http::Uri::parse(url).is_none() {
            return format!(
                "aterm secrets join <vault-url>#{signer} (replace <vault-url> with the full configured URL)"
            );
        }
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
                    "--serve serves the vault from this Mac: {} is not a vault URL",
                    describe(source)
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
        )
        .map(|_| ())
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

    #[cfg(test)]
    #[test]
    fn server_start_errors_omit_invalid_url_credentials() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let credential = "synthetic-server-url-credential";
        let url = format!("https://user:{credential}@vault.example/v1/team");
        let create_error = serve_here(&paths, &url).unwrap_err().to_string();
        assert!(!create_error.contains(credential));
        let timeout_error = answering(&url, Duration::ZERO).unwrap_err().to_string();
        assert!(!timeout_error.contains(credential));
    }

    /// Wait until the server `serve`'s flags set up answers a GET on this Mac, so a vault can be
    /// made or joined there the moment `server on` returns.
    fn until_answering(serve: &[String]) -> crate::Result<()> {
        let f = flags(serve, SERVE_VALUED, SERVE_SWITCHES).map_err(Error::new)?;
        let settings = serve_settings(&f).map_err(Error::new)?;
        answering(&local_url(&settings), ANSWER_HANG)
    }

    /// Wait until a GET of `url` hears any answer from a vault server, for at most `hang`.
    pub(super) fn answering(url: &str, hang: Duration) -> crate::Result<()> {
        let deadline =
            std::time::Instant::now() + hang.min(Duration::from_secs(365 * 24 * 60 * 60));
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match crate::transport::get_within(url, left.clamp(ANSWER_STEP, ANSWER_LOOK)) {
                Ok(_) => return Ok(()),
                Err(error) if left <= ANSWER_STEP => {
                    return Err(Error::new(format!(
                        "the server did not answer at {} within {}s: {error}",
                        describe(url),
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
        view_notes(paths, &admin);
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
                if let Some(why) = doc.added_refusal(&admin.doc) {
                    return Err(Error::new(why));
                }
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

    use client::FIRST_ANCHOR;

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

    pub(super) fn set(
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
        if let Some(why) = crate::entry_class::text_refusal(name) {
            return Err(Error::new(why));
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
        let route = client::write_route(paths, std::time::Instant::now() + PERSON_WAIT)?;
        if super::by_member(name, hosts, route) {
            return member_set(paths, name, value, fill);
        }
        // A seated Mac without the admin key is told why a member may not write the name, never
        // that a plain `set` needs an admin.
        if hosts.is_empty()
            && route != client::WriteRoute::NoLog
            && matches!(client::can_administer(paths), Ok(false))
            && let Some(why) = crate::entry_class::member_refusal(name)
        {
            return Err(Error::new(why));
        }
        let mut admin = client::admin_open(paths)?;
        view_notes(paths, &admin);
        let hosts = &machines_named_in_vault(paths, &admin, hosts)?;
        let (doc, anchor) = set_in(&admin.doc, &audience(hosts), name, value.as_slice(), fill)?;
        if let Some(why) = doc.added_refusal(&admin.doc) {
            return Err(Error::new(why));
        }
        // A person's save settles a conflict the log holds for this entry.
        admin.resolving(&audience(hosts), name);
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

    /// How long a person's write waits for this Mac's vault pass to let go of the state before it
    /// says the vault is busy.
    const PERSON_WAIT: Duration = Duration::from_secs(60);

    /// A member's `set` of `name` (DESIGN-claude-setup-token §3.5): signed into the vault's log
    /// with no admin key, or waiting for this Mac's vault pass, said in one line. A write a
    /// server took is then applied here ([`apply_member_write`]).
    fn member_set(
        paths: &Paths,
        name: &str,
        value: SecretBytes,
        fill: &Fill,
    ) -> crate::Result<ExitCode> {
        let anchor = match (&fill.with, fill.no_with) {
            (Some(anchor), _) => client::Anchor::To(anchor.clone()),
            (None, true) => client::Anchor::Drop,
            (None, false) => client::Anchor::Keep,
        };
        let entry = client::MemberEntry {
            name: name.to_owned(),
            value,
            if_absent: fill.if_absent,
            anchor,
            fleet_reads_anchors: fill.fleet_reads_anchors,
        };
        let by = std::time::Instant::now() + PERSON_WAIT;
        let written = client::member_set(paths, &entry, client::OnBusy::Refuse(by))?;
        let line = super::member_words(name, &written).map_err(Error::new)?;
        let gaps = if fill.if_absent {
            " (written only where the machine has no file of its own)"
        } else {
            ""
        };
        println!("{line}{gaps}");
        if written == client::MemberWritten::Posted {
            apply_member_write(paths);
        }
        Ok(ExitCode::SUCCESS)
    }

    /// `unset <name> [--for ...]`: a plain unset of a shared name on a vault with a membership log
    /// is a member's ([`member_unset`]); a seated Mac without the admin key is told why a member
    /// may not remove any other name, never that a plain `unset` needs an admin; `--for`, a vault
    /// with no log, and the names a member may not write, are an admin's.
    pub(super) fn unset(paths: &Paths, name: &str, hosts: &[String]) -> crate::Result<ExitCode> {
        if crate::entry_class::claude_token(name) {
            return Err(Error::new(crate::entry_class::claude_token_refusal(name)));
        }
        let by = std::time::Instant::now() + PERSON_WAIT;
        let route = client::write_route(paths, by)?;
        if super::by_member(name, hosts, route) {
            return member_unset(paths, name, by);
        }
        if hosts.is_empty()
            && route != client::WriteRoute::NoLog
            && matches!(client::can_administer(paths), Ok(false))
            && let Some(why) = crate::entry_class::member_refusal(name)
        {
            return Err(Error::new(why));
        }
        let admin = client::admin_open(paths)?;
        view_notes(paths, &admin);
        let mut doc = admin.doc.clone();
        let hosts = machines_named_in_vault(paths, &admin, hosts)?;
        if !doc
            .unset_entry(&audience(&hosts), name)
            .map_err(text_error)?
        {
            return Err(Error::new(format!(
                "no entry {name} in {}",
                audience(&hosts).header()
            )));
        }
        let done = client::admin_publish(paths, &admin, &doc)?;
        Ok(published(&format!("removed {name}"), &done))
    }

    /// A member's `unset` of `name` (DESIGN-claude-setup-token §3.6): its `Delete` signed into the
    /// vault's log under a fresh `[everyone]` key, with no admin key, said in one line, and a
    /// removal a server took applied here ([`apply_member_write`]).
    fn member_unset(paths: &Paths, name: &str, by: std::time::Instant) -> crate::Result<ExitCode> {
        let unset = client::member_unset(paths, name, by)?;
        let line = super::member_unset_words(name, &unset).map_err(Error::new)?;
        println!("{line}");
        if unset == client::MemberUnset::Posted {
            apply_member_write(paths);
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Apply a member write a server took to this Mac: an admin of the vault 0.145 Macs read
    /// mirrors the log into it, as a member's removal does, and any other Mac syncs. A failure is
    /// a warning: this Mac's next vault pass does it again.
    fn apply_member_write(paths: &Paths) {
        if client::admin_cached(paths).is_ok() {
            if let Err(error) = client::admin_mirror_log(paths, &[]) {
                eprintln!(
                    "warning: the vault 0.145 Macs read is not in step with it yet ({error}): this Mac's next vault pass mirrors it again"
                );
            }
        } else if let Err(error) = client::sync(paths, None) {
            eprintln!(
                "warning: this Mac's own copy is not updated yet ({error}); aterm secrets sync retries"
            );
        }
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
                        "this vault has no join password (aterm secrets password new makes one); joining machines wait for an Allow on a member or approval by code"
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
                let waiting = client::requests_before_opening(&admin, &doc)?;
                let (mut say, mut agree) = (
                    |line: String| eprintln!("{line}"),
                    |question: &str| yes(ask(question)),
                );
                let terminal = interactive().then_some((
                    &mut say as &mut dyn FnMut(String),
                    &mut agree as &mut dyn FnMut(&str) -> bool,
                ));
                let done = open_vault(paths, &admin, &mut doc, waiting, terminal)?;
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
                        "closed the vault (joining machines now wait for an Allow on a member or approval by code)",
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
                    "removed the join password (joining machines now wait for an Allow on a member or approval by code)",
                    &done,
                ))
            }
        }
    }

    /// The person at a terminal as `password open` or `approve --all` asks them: where a line
    /// is said, and what answers a yes-or-no question.
    pub(super) type Terminal<'a> = (&'a mut dyn FnMut(String), &'a mut dyn FnMut(&str) -> bool);

    /// `password open` over a closed vault: opens `doc` and publishes it, each request in
    /// `waiting` that the person at `terminal` lets in ([`admit_on_opening`]; none without one)
    /// a machine in the same publish. Each is recorded as an open admission (`Open`, never
    /// `Manual`: the prompt checks no code; through [`client::admin_publish_opened`]), so in a
    /// vault with Commons control the publish shares the control entry with it and it enrolls.
    pub(super) fn open_vault(
        paths: &Paths,
        admin: &client::Admin,
        doc: &mut text::Document,
        waiting: Vec<client::Request>,
        terminal: Option<Terminal<'_>>,
    ) -> crate::Result<Published> {
        let admitted = match terminal {
            Some((say, agree)) => admit_on_opening(doc, waiting, say, agree)?,
            None => Vec::new(),
        };
        doc.set_open(true).map_err(text_error)?;
        client::admin_publish_opened(paths, admin, doc, &admitted)
    }

    /// What `password open` at a terminal does with the requests `waiting` as it opens the vault:
    /// the keys it let in, each now a machine in `doc`.
    pub(super) fn admit_on_opening(
        doc: &mut text::Document,
        waiting: Vec<client::Request>,
        say: &mut dyn FnMut(String),
        agree: &mut dyn FnMut(&str) -> bool,
    ) -> crate::Result<Vec<String>> {
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
            return Ok(Vec::new());
        }
        let question = format!(
            "Let these {} in now? Any you do not know may be a machine you let go. Each reads every [everyone] secret. [y/N] ",
            offered.len()
        );
        let mut admitted = Vec::new();
        if agree(&question) {
            for request in offered {
                let host = client::free_name(doc, &request.host);
                doc.add_machine(&host, &request.recipient)
                    .map_err(text_error)?;
                admitted.push(request.recipient.clone());
            }
        }
        Ok(admitted)
    }

    /// Render the queue after authenticated membership filtering. Administrator
    /// metadata is needed only to describe code-only holds.
    pub(super) fn requests_json(
        inspection: &client::RequestInspection,
        doc: Option<&text::Document>,
    ) -> aterm_json::Value {
        let code_only = doc.map(text::Document::code_only);
        let requests: Vec<_> = inspection
            .requests
            .iter()
            .map(|request| {
                aterm_json::json!({
                    "recipient": request.recipient,
                    "host": request.host,
                    "code": request.code,
                    "requested_at": request.at,
                    "member_allow_present": request.allowed.is_some(),
                    "password_proof_present": request.proof.is_some(),
                    "code_only": code_only.as_ref().map(|keys| keys.contains(&request.recipient)),
                })
            })
            .collect();
        aterm_json::json!({
            "schema": 1,
            "kind": "vault-join-requests",
            "scope": "observed-vault-queue",
            "known_members_filtered": inspection.known_members_filtered,
            "queue": {
                "source": inspection.queue_source,
                "server_address": inspection.queue_server.map(|ip| ip.to_string()),
                "version": inspection.queue_version,
            },
            "requests": requests,
        })
    }

    fn requests(paths: &Paths, json: bool) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths).ok();
        let inspection = client::inspect_requests(paths, admin.as_ref().map(|a| &a.doc))?;
        if json {
            let value = requests_json(&inspection, admin.as_ref().map(|a| &a.doc));
            println!(
                "{}",
                aterm_json::to_string(&value)
                    .map_err(|error| Error::new(format!("cannot encode join requests: {error}")))?
            );
            return Ok(ExitCode::SUCCESS);
        }
        let code_only = admin
            .as_ref()
            .map(|a| a.doc.code_only())
            .unwrap_or_default();
        let mark = admin.as_ref().map_or(CODE_ONLY_MARK, |a| held_mark(&a.doc));
        if let Some(source) = inspection.queue_source.as_deref() {
            if let Some(server) = inspection.queue_server {
                println!("queue: {source} (answered by {server})");
            } else {
                println!("queue: {source} (server address unobserved)");
            }
        }
        let pending = inspection.requests;
        if pending.is_empty() {
            if inspection.queue_source.is_some() {
                println!("no pending requests in the observed queue");
            } else {
                println!("this source has no network join queue");
            }
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
        let (mut say, mut agree) = (
            |line: String| eprintln!("{line}"),
            |question: &str| yes(ask(question)),
        );
        approve_with_terminal(
            paths,
            which,
            all,
            name,
            interactive().then_some((&mut say, &mut agree)),
        )
    }

    /// The same approval and publication as [`approve`], with its terminal supplied by the
    /// caller. A bulk approval requires an affirmative answer; explicit codes need no terminal.
    pub(super) fn approve_with_terminal(
        paths: &Paths,
        which: &[String],
        all: bool,
        name: Option<String>,
        terminal: Option<Terminal<'_>>,
    ) -> crate::Result<ExitCode> {
        let admin = client::admin_open(paths)?;
        let mut doc = admin.doc.clone();
        // A named code can approve a recipient already present in the document: a plain
        // document edit grants no admission provenance. Bulk approval keeps its pending-only
        // meaning, rather than renewing every old request the server still retains.
        let observed = if all {
            client::all_requests_for_approval(&admin)?
        } else {
            let targets: Vec<_> = which
                .iter()
                .filter(|word| !word.starts_with("age1"))
                .map(|code| client::ApprovalTarget::Code(code))
                .collect();
            client::requests_for_approval(&admin, &targets)?
        };
        let mut pending = observed.requests.clone();
        if all {
            let known: std::collections::BTreeSet<_> = doc
                .machines()
                .into_iter()
                .map(|machine| machine.recipient)
                .collect();
            pending.retain(|request| !known.contains(&request.recipient));
            pending.sort_unstable_by(|left, right| {
                (&left.recipient, &left.host).cmp(&(&right.recipient, &right.host))
            });
            pending.dedup_by(|left, right| {
                left.recipient == right.recipient && left.host == right.host
            });
        }
        if all && pending.is_empty() {
            println!("no machines are asking to join");
            return Ok(ExitCode::SUCCESS);
        }
        let mut chosen = from_queue(&pending, which, all)?;
        if all {
            let Some((say, agree)) = terminal else {
                return Err(Error::new(
                    "approve --all asks for a yes at a terminal; approve each code instead",
                ));
            };
            let code_only = doc.code_only();
            let mark = held_mark(&doc);
            for request in &pending {
                let held = if code_only.contains(&request.recipient) {
                    mark
                } else {
                    ""
                };
                say(format!(
                    "  {}  code {}  {}{held}",
                    request.host, request.code, request.recipient
                ));
            }
            let question = format!(
                "Approve these {} machines? Each one reads every [everyone] secret. Compare each code with the one its machine shows. [y/N] ",
                chosen.len()
            );
            if !agree(&question) {
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
        let _ = client::record_approved(paths, &observed.requests, &recipients, observed.complete);
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
            let request = pending
                .iter()
                .find(|r| r.code.eq_ignore_ascii_case(code))
                .ok_or_else(|| {
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

    /// `revoke <host> [--drop-sections] [--stolen]`. Where this Mac removes through a trusted
    /// membership log and the log names `host` ([`client::revoke_by_log`]) it is F16's member
    /// removal, with no admin key ([`client::remove_member`]); the admin's revoke is the fallback
    /// for a vault with no log (ALab until F12b), for a Mac the log does not seat (a founder that
    /// proved no device key, or one only `[machines]` names, which the envelope 0.145 Macs read
    /// still lists), for another admin of that envelope ([`client::vault_admin_named`]: the admin
    /// path rotates the envelope's signing key it holds, or refuses while Commons authority may be
    /// pinned to it), and for `--drop-sections`, an edit of host lists that only an admin makes
    /// (it writes a log `Remove` too, and rotates the log's epochs away from the Mac).
    fn revoke(
        paths: &Paths,
        host: &str,
        drop_sections: bool,
        stolen: bool,
    ) -> crate::Result<ExitCode> {
        let by_log = client::removes_by_log(paths);
        let vault_admin = client::vault_admin_named(paths, host);
        if !drop_sections && by_log && vault_admin.is_none() && client::revoke_by_log(paths, host) {
            let done = client::remove_member(paths, host, stolen)?;
            println!(
                "removed {} through the membership log: {} records, new keys for {}.",
                done.label,
                done.signed,
                done.rotated.join(", ")
            );
            if !done.could_read.is_empty() {
                println!(
                    "{} could read {} secrets ({}); rotate them if it was lost or compromised",
                    done.label,
                    done.could_read.len(),
                    done.could_read.join(", ")
                );
            }
            if done.fleet_code_rotated {
                println!("rotated aterm-fleet-code in [everyone].");
            }
            for note in &done.envelope_notes {
                println!("{note}");
            }
            if let Some(line) = done.needs_you {
                println!("{line}");
            }
            return Ok(ExitCode::SUCCESS);
        }
        if vault_admin.is_none() && !drop_sections && by_log {
            println!(
                "{host} has no seat in this Mac's membership log (a founder that proved no device \
                 key, or a Mac only the vault's machine list names): revoking with the admin key"
            );
        } else if vault_admin.is_none() && !drop_sections {
            println!(
                "this Mac removes no Mac through a membership log here: revoking with the admin key"
            );
        }
        let admin = client::admin_open(paths)?;
        if let Some(name) = vault_admin.as_deref().filter(|_| !drop_sections) {
            // The advice names the Mac by its age recipient, which `aterm members remove` reads
            // from the log alone: a label there is the registry's, which can name another Mac.
            let recipient = admin
                .doc
                .machines()
                .iter()
                .find(|machine| machine.name == name)
                .map(|machine| machine.recipient.clone());
            match recipient {
                Some(recipient) => println!(
                    "{name} is an admin of the vault 0.145 Macs read: revoking with the admin \
                     key, which rotates the signing key it holds (`aterm members remove \
                     {recipient}` removes it from the membership log alone, leaving it that key)"
                ),
                None => println!(
                    "{name} is an admin of the vault 0.145 Macs read: revoking with the admin \
                     key, which rotates the signing key it holds"
                ),
            }
        }
        let host = &client::machine_named_in_vault(paths, &admin, host)?;
        let held = exposure(&admin.doc, host);
        if stolen {
            let done = client::admin_revoke_stolen(paths, &admin, host, drop_sections)?;
            let code = published(&format!("revoked {host}"), &done.revocation.published);
            if let Some(retirement) = done.revocation.retirement {
                println!(
                    "Commons membership permanently retired ({} recipients, {} prior device bindings).",
                    retirement.recipients, retirement.bindings
                );
            }
            if done.revocation.fleet_code_rotated {
                println!("rotated aterm-fleet-code in [everyone].");
            }
            if let Some(held) = held {
                println!("{held}");
            }
            println!("{}", done.needs_you_line);
            return Ok(code);
        }
        let done = client::admin_revoke(paths, &admin, host, drop_sections)?;
        let code = published(&format!("revoked {host}"), &done.published);
        if let Some(retirement) = done.retirement {
            println!(
                "Commons membership permanently retired ({} recipients, {} prior device bindings).",
                retirement.recipients, retirement.bindings
            );
        }
        if done.fleet_code_rotated {
            println!("rotated aterm-fleet-code in [everyone].");
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
        let advertise_mode = settings.advertise.clone();
        let mut role = super::ServeRole::default();
        let (advertise, held_back) =
            role.decide(&advertise_mode, &store, paths, &mut settings.pins, alab_key);
        crate::log_line(&format!(
            "{} serving on {}, store {}",
            stamp(),
            settings.listen,
            store.display()
        ));
        if let Some(line) = held_back {
            crate::log_line(&format!("{} {line}", stamp()));
        }
        let role_decide: Option<crate::server::RoleDecider> =
            (!matches!(advertise_mode, super::Advertise::Off)).then(|| {
                let store = store.clone();
                let paths = paths.clone();
                Box::new(move |pins: &mut Vec<(String, [u8; 32])>| {
                    role.decide(&advertise_mode, &store, &paths, pins, alab_key)
                        .0
                }) as crate::server::RoleDecider
            });
        super::keep_network_place();
        crate::server::serve_with_role(
            crate::server::Options {
                listen: settings.listen,
                store,
                advertise,
                pins: settings.pins,
                hold: settings.hold.unwrap_or(crate::server::DEFAULT_HOLD),
                keep_awake: cfg!(target_os = "macos") && settings.keep_awake,
            },
            Some(paths.clone()),
            role_decide,
        )?;
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
                if job == Job::Server
                    && matches!(
                        service::daemon_state_for_home(&paths.home)?,
                        service::State::Installed { pid: Some(_), .. }
                    )
                {
                    return Err(Error::new(format!(
                        "{} is already running as system LaunchDaemon {}; manage it with \
                         `aterm machines apply` instead of starting a second user agent on the \
                         same port",
                        service::DAEMON_LABEL,
                        service::DAEMON_PLIST
                    )));
                }
                let pinned = program.is_some();
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
                // A program a person names is their pin, which the window's mend never moves
                // (`client::mend_held`), recorded before the install so a mend that reads the job
                // meanwhile finds it; `on` with none puts the job on this aterm, unpinned.
                let (line, unrecorded) = service::install_pinned(
                    &paths.home,
                    job,
                    pinned.then_some(program.as_path()),
                    || install(&paths.home, job, &program, &args),
                )?;
                println!("{line}");
                if let Some(error) = unrecorded {
                    eprintln!(
                        "aterm secrets: {} runs, but its pin record was not changed: {error}",
                        job.label()
                    );
                }
                if job == Job::Server {
                    until_answering(args.get(2..).unwrap_or_default())?;
                }
            }
            Action::Off => {
                let removed = service::turn_off(&paths.home, job)?;
                if let Err(error) = service::clear_pin(&paths.home, job) {
                    eprintln!("aterm secrets: {}'s pin record stays: {error}", job.label());
                }
                if removed {
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
#[path = "cli_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "cli_repair_tests.rs"]
mod repair_tests;
