// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines enroll` and `unenroll`: the controller's side (design §4.2).
//!
//! Enrollment needs a person once: the owner types the admin's password one time, at the
//! terminal, for every Mac the command enrolls (`enroll <address>…`, or `enroll --found`:
//! every Mac `scan` sees that is not enrolled yet), after each Mac's host key fingerprint
//! was printed. The password goes to each Mac whose key was trusted on first use, as a typed
//! one would: with `--found` that is every host on the LAN advertising a Mac's Remote Login,
//! which any machine there can advertise. The password is held in memory only
//! ([`crate::port::Secret`]) and reaches each Mac through pipes alone: its `ssh` login reads
//! it from OpenSSH's `SSH_ASKPASS` helper, this aterm itself ([`crate::askpass`]), and its
//! `sudo -S` from the encrypted channel. Macs run a few at a time (`--parallel`, else the
//! inventory's `[controller] parallel`), each one's lines prefixed by its name. For each Mac,
//! in order:
//!
//! 1. the host key, read for every Mac before the password is asked: `ssh-keyscan -t ed25519
//!    <address>`, fingerprinted by `ssh-keygen -lf -`, must equal `--fingerprint` when one is
//!    given (one Mac only), and is trusted on first use otherwise, its fingerprint printed;
//!    at the address of an enrolled Mac it must be that Mac's pinned key (which is then
//!    enrolled already), never a first use. The master trusts that key alone;
//! 2. the master (`ssh -M -S <short path> -o ControlPersist=120 … true`), its one password
//!    prompt answered by `SSH_ASKPASS=<scratch>/aterm-machines-askpass` (a link to this aterm)
//!    under `SSH_ASKPASS_REQUIRE=force`, which reads the password from `ssh`'s standard input;
//! 3. the host's facts, read as the admin (no forced command exists yet); anything but macOS
//!    (`uname -s` other than `Darwin`) stops here;
//! 4. the agent archive (`ditto -c -k --keepParent` of the running aterm.app, or of the bare
//!    dev binary, built once for every Mac) and `allowed_signers`, copied by one `scp` into the
//!    admin's temporary directory;
//! 5. one command line: one `sudo -S -p ''` takes the password from the channel and runs a
//!    root `/bin/sh` that first drops that input (`exec </dev/null`), then copies both into
//!    the root-owned staging area, checks the root-owned copies' SHA-256, expands the archive,
//!    checks a bundle's signature, and runs the STAGED agent's `machines agent install`.
//!    Nothing user-writable is executed as root;
//! 6. the host key is pinned under `HostKeyAlias=<uuid>`, the fleet key's `hello` checks the
//!    forced command answers as that UUID, and the host is recorded in `machines.toml`.
//!
//! With no `--user` the admin is the one the inventory records for the Macs enrolled
//! before ([`recorded_admin`]), else this Mac's login name. When Macs refuse the password,
//! after the others ran, and `--user` named none, the admin's name is asked once for all
//! and the password already typed is tried under it before anything else is asked; each
//! Mac that still refuses is asked for once more, by name, and a password one Mac takes is
//! tried on every other Mac still refusing ([`enroll_macs`]). A Mac enrolled already is
//! nothing to do and fails nothing; anything else that stops a Mac says so on its own line,
//! and the rest go on. The command ends with what was enrolled, one line per Mac that was
//! not, and why.
//!
//! What OpenSSH, sudo and the host printed, measured in the macOS 27.0 VM (`OpenSSH_10.3`,
//! sudo 1.9.17p2): `ssh-keyscan -t ed25519 127.0.0.1` writes `# 127.0.0.1:22
//! SSH-2.0-OpenSSH_10.3` on standard error and `127.0.0.1 ssh-ed25519 AAAA…` on standard
//! output; `ssh-keygen -lf -` answers `256 SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q
//! 127.0.0.1 (ED25519)`. A wrong password: `admin@192.168.64.5: Permission denied
//! (publickey,password,keyboard-interactive).`, exit 255, after one askpass run under
//! `PreferredAuthentications=password` (two under `keyboard-interactive,password`, the second
//! answering an empty password: the method is chosen alone so one typo costs the Mac one
//! failed attempt). The master forks to the background with its standard output and error on
//! `/dev/null`, so a run that reads them to the end returns once it logged in. `sudo -S -p ''`
//! with a wrong password: `Sorry, try again.`, then `sudo: no password was provided` and
//! `sudo: 1 incorrect password attempt`, exit 1. Under a NOPASSWD rule `sudo -S` reads nothing,
//! and a root `cat` read the password the channel still held, hence the `exec </dev/null`.

use super::discover;
use super::inventory::{self, Host, Inventory, Paths};
use super::ssh::{self, Target};
use super::text::shown;
use crate::envelope::{self, Action, Disrupt, Envelope};
use crate::model::{HostReport, Outcome, Read};
use crate::port::{Cmd, Ran, Scratch, Secret, System, shell_word};
use crate::protocol::{Hello, Line};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The client: Apple's OpenSSH.
const SSH: &str = "/usr/bin/ssh";

/// OpenSSH's password method, tried first (one prompt per login).
pub const PASSWORD_METHOD: &str = "password";

/// OpenSSH's keyboard-interactive method, for a Mac whose `sshd` offers no `password`.
pub const KBD_METHOD: &str = "keyboard-interactive";

/// How long the install command line may run on a Mac: expand aterm.app, check its
/// signature, `agent install`.
const INSTALL_BUDGET: Duration = Duration::from_secs(900);

/// How long the master's login may take (its own `ConnectTimeout` is 15 seconds).
const LOGIN_BUDGET: Duration = Duration::from_secs(90);

/// How long `enroll --found` browses Bonjour, as `scan` does by default.
const FOUND_WAIT: Duration = Duration::from_secs(3);

/// This Mac's own host key, which `--found` never enrolls.
const OWN_HOST_KEY: &str = "/etc/ssh/ssh_host_ed25519_key.pub";

/// One Mac to enroll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Where the Mac answers (`lab-07.local`, an address).
    pub address: String,
    /// Its admin account (default: the account running this controller, as `ssh` picks it).
    pub user: String,
    /// The inventory name (default: the tag its Local hostname starts with, `m10` of
    /// `m10-MacBook-M4-Max-36GB-1TB`, else the whole Local hostname).
    pub name: Option<String>,
    /// How its host key is trusted.
    pub trust: Trust,
}

/// How the host key is trusted at enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// It must have this fingerprint (`SHA256:…`), read at the Mac with
    /// `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`.
    Fingerprint(String),
    /// Whatever the Mac answers the first time (no `--fingerprint`): pinned from then on.
    FirstUse,
}

/// What the owner passed to `enroll`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The addresses named, each once.
    pub addresses: Vec<String>,
    /// `--found`: every Mac `scan` sees that is not enrolled yet, too.
    pub found: bool,
    /// The admin account on every Mac.
    pub user: String,
    /// `--user` named it (else it is the admin the inventory records for the Macs enrolled
    /// before, or this Mac's login name, and the first refusal asks for the name once).
    pub user_given: bool,
    /// It is the admin the inventory records for the Macs enrolled before
    /// ([`recorded_admin`]), not a guess from this Mac's login name.
    pub user_recorded: bool,
    /// `--name`, for one Mac.
    pub name: Option<String>,
    /// How the host key is trusted (`--fingerprint`, for one Mac).
    pub trust: Trust,
    /// `--parallel`: Macs at once.
    pub parallel: Option<usize>,
}

impl Args {
    /// One request per address named.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.addresses
            .iter()
            .map(|address| Request {
                address: address.clone(),
                user: self.user.clone(),
                name: self.name.clone(),
                trust: self.trust.clone(),
            })
            .collect()
    }
}

/// Parse `enroll <address>… [--found] [--user <admin>] [--name <n>] [--fingerprint <fp>]
/// [--parallel N]`. With no `--user` the admin is [`login_name`], and with no
/// `--fingerprint` each host key is trusted on first use. `--name` and `--fingerprint`
/// describe one Mac, so they are refused with several (or `--found`).
///
/// # Errors
/// One line.
pub fn parse_args(sys: &dyn System, args: &[String]) -> Result<Args, String> {
    let mut addresses: Vec<String> = Vec::new();
    let mut found = false;
    let mut user = None;
    let mut name = None;
    let mut fingerprint = None;
    let mut parallel = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--found" => {
                found = true;
                continue;
            }
            "--user" => &mut user,
            "--name" => &mut name,
            "--fingerprint" => &mut fingerprint,
            "--parallel" => &mut parallel,
            flag if flag.starts_with('-') => {
                return Err(format!("enroll: unknown option {flag}"));
            }
            word => {
                if !addresses.iter().any(|a| a.eq_ignore_ascii_case(word)) {
                    addresses.push(word.to_owned());
                }
                continue;
            }
        };
        // A flag left without its value is refused: `--fingerprint` alone must never read
        // as no fingerprint.
        *slot = Some(
            it.next()
                .cloned()
                .ok_or_else(|| format!("enroll: {arg} takes a value"))?,
        );
    }
    if addresses.is_empty() && !found {
        return Err(
            "enroll: name the Mac's address (several, or --found for every Mac `scan` \
                    sees that is not enrolled)"
                .into(),
        );
    }
    let several = found || addresses.len() > 1;
    if several && fingerprint.is_some() {
        return Err(
            "enroll: --fingerprint checks one Mac's host key: name that one address alone".into(),
        );
    }
    if several && name.is_some() {
        return Err("enroll: --name names one Mac: name that one address alone".into());
    }
    let parallel = match parallel {
        None => None,
        Some(v) => Some(
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=64).contains(n))
                .ok_or_else(|| format!("enroll: --parallel takes 1 to 64, not {v:?}"))?,
        ),
    };
    let user_given = user.is_some();
    let user = match user {
        Some(user) => user,
        None => login_name(sys)
            .map_err(|why| format!("enroll: no --user, and {why}: pass --user <admin>"))?,
    };
    let trust = fingerprint.map_or(Trust::FirstUse, |fp| {
        Trust::Fingerprint(normalize_fingerprint(&fp))
    });
    for address in &addresses {
        plain_destination(&Target {
            user: user.clone(),
            address: address.clone(),
            alias: None,
        })?;
    }
    if !plain_account(&user) {
        return Err(format!("{user:?} is not an account name"));
    }
    if let Some(n) = &name
        && !n
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "enroll: --name {n:?} must be letters, digits, `.`, `_` or `-`"
        ));
    }
    Ok(Args {
        addresses,
        found,
        user,
        user_given,
        user_recorded: false,
        name,
        trust,
        parallel,
    })
}

/// The admin account the inventory records for the Macs enrolled before (the one most of
/// them have), which a run with no `--user` takes before this Mac's login name: a lab's
/// Macs share one admin, and a second `enroll` must not guess again what the first was told.
#[must_use]
pub fn recorded_admin(inv: &Inventory) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for host in inv.hosts.iter().filter(|h| plain_account(&h.user)) {
        match counts.iter_mut().find(|(user, _)| *user == host.user) {
            Some((_, n)) => *n += 1,
            None => counts.push((&host.user, 1)),
        }
    }
    // The first of the most common, so a tie goes to the Mac enrolled first.
    let most = counts.iter().map(|(_, n)| *n).max()?;
    counts
        .into_iter()
        .find(|(_, n)| *n == most)
        .map(|(user, _)| user.to_owned())
}

/// `args` with no `--user` taking the inventory's [`recorded_admin`] over this Mac's login
/// name.
#[must_use]
pub fn with_recorded_admin(mut args: Args, inv: &Inventory) -> Args {
    if !args.user_given
        && let Some(admin) = recorded_admin(inv)
    {
        args.user = admin;
        args.user_recorded = true;
    }
    args
}

/// An account name `ssh` cannot read as an option or as two words.
fn plain_account(user: &str) -> bool {
    !user.is_empty()
        && !user.starts_with('-')
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The account running this controller, by name: the user `ssh` logs in as when none is
/// given (`getpwuid(getuid())`), read with `id -urn`.
///
/// # Errors
/// One line when `id` names none.
pub fn login_name(sys: &dyn System) -> Result<String, String> {
    let cmd = Cmd::new("/usr/bin/id").arg("-urn");
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    let name = ran.out().trim().to_owned();
    if !ran.success() || name.is_empty() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    Ok(name)
}

/// `SHA256:<base64>` whatever the owner typed around it.
fn normalize_fingerprint(fp: &str) -> String {
    let fp = fp.trim();
    if fp.starts_with("SHA256:") {
        fp.to_owned()
    } else {
        format!("SHA256:{fp}")
    }
}

/// Refuse a user or address `ssh` could read as an option or as two words.
fn plain_destination(t: &Target) -> Result<(), String> {
    let ok = |w: &str, extra: &[u8]| {
        !w.is_empty()
            && !w.starts_with('-')
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b) || extra.contains(&b))
    };
    if !ok(&t.user, b"") {
        return Err(format!("{:?} is not an account name", t.user));
    }
    if !ok(&t.address, b":%") {
        return Err(format!("{:?} is not a host address", t.address));
    }
    Ok(())
}

/// A host key, as `ssh-keyscan` read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// `ssh-ed25519`.
    pub kind: String,
    /// The base64 key.
    pub blob: String,
    /// `SHA256:…`.
    pub fingerprint: String,
}

impl HostKey {
    /// `<kind> <blob>`.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{} {}", self.kind, self.blob)
    }
}

/// The first `<host> ssh-ed25519 <base64>` row of `ssh-keyscan` output, as `(kind, blob)`.
#[must_use]
pub fn parse_keyscan(out: &str) -> Option<(String, String)> {
    out.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
        let mut w = l.split_whitespace();
        let (_, kind, blob) = (w.next()?, w.next()?, w.next()?);
        (kind == "ssh-ed25519").then(|| (kind.to_owned(), blob.to_owned()))
    })
}

/// The `SHA256:…` word of `ssh-keygen -lf` output.
#[must_use]
pub fn parse_fingerprint(out: &str) -> Option<String> {
    out.split_whitespace()
        .find(|w| w.starts_with("SHA256:"))
        .map(str::to_owned)
}

/// The fingerprint of a public key line, through `ssh-keygen -lf -` (the line on standard
/// input, from a file in `scratch`).
///
/// # Errors
/// One line.
pub fn fingerprint_of(sys: &dyn System, line: &str, scratch: &Path) -> Result<String, String> {
    let file = scratch.join("hostkey.pub");
    sys.write_file(&file, format!("{line}\n").as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let ran = sys.run(
        &Cmd::new("/usr/bin/ssh-keygen")
            .args(["-lf", "-"])
            .stdin_file(&file),
    );
    let _ = sys.remove(&file);
    let ran = ran.map_err(|e| format!("ssh-keygen -lf: {e}"))?;
    if !ran.success() {
        return Err(format!("ssh-keygen -lf: {}", ran.describe()));
    }
    parse_fingerprint(&ran.out()).ok_or_else(|| "ssh-keygen -lf printed no fingerprint".into())
}

/// Why `ssh-keyscan` showed no ed25519 host key, read from what it printed. Measured in the
/// VM (macOS 27.0), each exit 1: `connect (`0.0.0.1'): No route to host`; for an unused
/// address on the VM's subnet nothing at all for the 10 s, then `connect
/// (`192.168.64.252'): Host is down` once its ARP failed; for a closed port (Remote Login
/// off, the firewall off, as macOS ships) `write (192.168.64.17): Broken pipe` at once. A
/// Mac whose Remote Login is off behind the firewall's stealth mode is silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoHostKey {
    /// No route to the address.
    NoRoute,
    /// Nothing there answers (its ARP failed): asleep, off, or not on this network.
    Down,
    /// The Mac refused the connection: nothing listens on port 22, so Remote Login is off.
    Closed,
    /// Nothing at all for the whole wait: Remote Login off behind stealth mode, or nothing
    /// there.
    Silent,
    /// Something answered with no ed25519 key, as `ssh-keyscan` said.
    Answered(String),
}

/// What `ssh-keyscan` printed for a refused connection: `write (<address>): Broken pipe`
/// (measured) or `connect (`<address>'): Connection refused`.
const KEYSCAN_CLOSED: [&str; 2] = [": Broken pipe", ssh::CONNECTION_REFUSED];

impl NoHostKey {
    /// What `ran`, an `ssh-keyscan` that printed no ed25519 key, means.
    #[must_use]
    pub fn of(ran: &Ran) -> Self {
        let err = ran.err();
        if err.contains(ssh::NO_ROUTE) {
            Self::NoRoute
        } else if err.contains(ssh::HOST_DOWN) {
            Self::Down
        } else if KEYSCAN_CLOSED.iter().any(|said| err.contains(said)) {
            Self::Closed
        } else if err.trim().is_empty() {
            Self::Silent
        } else {
            Self::Answered(ran.describe())
        }
    }

    /// One line for the Mac at `address`. It does not name the address: the line goes after
    /// the Mac's label, or after the one address `enroll` was given.
    #[must_use]
    pub fn why(&self, address: &str) -> String {
        const TURN_ON: &str = "turn it on at that Mac (System Settings ▸ General ▸ Sharing ▸ \
                               Remote Login), then run the same enroll command again";
        match self {
            Self::NoRoute => ssh::Failure::NoRoute.why_alone(address),
            Self::Down => ssh::Failure::TimedOut.why(),
            Self::Closed => format!("Remote Login is off: {TURN_ON}"),
            Self::Silent => format!(
                "no answer in 10 s: Remote Login is off, or the Mac is asleep, off or not on \
                 this network; if Remote Login is off, {TURN_ON}"
            ),
            Self::Answered(said) => {
                // ssh-keyscan names the host as `(<address>)` or `` (`<address>') ``.
                let said = if address.is_empty() {
                    said.clone()
                } else {
                    said.replace(&format!(" (`{address}')"), "")
                        .replace(&format!(" ({address})"), "")
                        .replace(address, "it")
                };
                format!(
                    "something answered, but not as a Mac's Remote Login does (no ed25519 host \
                     key: {said}); if Remote Login is off, {TURN_ON}"
                )
            }
        }
    }
}

/// Read the host's ed25519 key off the network: `ssh-keyscan -t ed25519 <address>`.
///
/// # Errors
/// One line (no answer, or no ed25519 key: [`NoHostKey`]).
pub fn scan_host_key(sys: &dyn System, address: &str, scratch: &Path) -> Result<HostKey, String> {
    let ran = sys
        .run(
            &Cmd::new("/usr/bin/ssh-keyscan")
                .args(["-T", "10", "-t", "ed25519", "--", address])
                .budget(Duration::from_secs(30)),
        )
        .map_err(|e| format!("ssh-keyscan: {e}"))?;
    let (kind, blob) = parse_keyscan(&ran.out()).ok_or_else(|| NoHostKey::of(&ran).why(address))?;
    let fingerprint = fingerprint_of(sys, &format!("{kind} {blob}"), scratch)?;
    Ok(HostKey {
        kind,
        blob,
        fingerprint,
    })
}

/// Read the host's ed25519 key over a master that is already open (its own
/// `/etc/ssh/ssh_host_ed25519_key.pub`). Production enrollment scans the key before any
/// password is typed; this is for a master someone else opened and vouched for (a VM test
/// harness's), and as a cross-check.
///
/// # Errors
/// One line.
pub fn host_key_through(
    sys: &dyn System,
    master: &Master,
    scratch: &Path,
) -> Result<HostKey, String> {
    let ran = sys
        .run(&remote_sh(
            master,
            "/bin/cat /etc/ssh/ssh_host_ed25519_key.pub",
            false,
        ))
        .map_err(|e| format!("reading the host key: {e}"))?;
    let mut words = ran
        .out()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter();
    let (Some(kind), Some(blob)) = (words.next(), words.next()) else {
        return Err(format!("the host printed no host key ({})", ran.describe()));
    };
    if kind != "ssh-ed25519" {
        return Err(format!("the host key is {kind}, not ssh-ed25519"));
    }
    let fingerprint = fingerprint_of(sys, &format!("{kind} {blob}"), scratch)?;
    Ok(HostKey {
        kind,
        blob,
        fingerprint,
    })
}

/// The line enrollment prints for the host key it goes on to pin, in the owner's words: the
/// Mac's ID (its host key's fingerprint, which `ssh-keygen -lf` prints at the Mac), and
/// whether `--fingerprint` named it or this is the first connection, after which aterm talks
/// to no other machine at this Mac's name (the key is pinned).
fn trusted(key: &HostKey, trust: &Trust) -> String {
    match trust {
        Trust::Fingerprint(_) => format!(
            "its ID is the one --fingerprint names ({})",
            key.fingerprint
        ),
        Trust::FirstUse => format!(
            "first connection to this Mac (its ID: {}); once it is enrolled, aterm knows it by this ID",
            key.fingerprint
        ),
    }
}

/// Refuse a host key the owner did not vouch for.
///
/// # Errors
/// One line naming both fingerprints.
pub fn check_host_key(key: &HostKey, trust: &Trust) -> Result<(), String> {
    match trust {
        Trust::FirstUse => Ok(()),
        Trust::Fingerprint(want) if *want == key.fingerprint => Ok(()),
        Trust::Fingerprint(want) => Err(format!(
            "the host key is {} but --fingerprint says {want}: this is not the Mac you read \
             the fingerprint at, or someone is in between. Nothing was sent.",
            key.fingerprint
        )),
    }
}

/// `known_hosts` text with `alias`'s line replaced by `alias <key>` (other lines kept).
#[must_use]
pub fn known_hosts_with(current: &str, alias: &str, key: Option<&HostKey>) -> String {
    let mut out: String = current
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(alias))
        .map(|l| format!("{l}\n"))
        .collect();
    if let Some(key) = key {
        out.push_str(&format!("{alias} {}\n", key.line()));
    }
    out
}

/// Pin `key` under `HostKeyAlias=<uuid>` in the controller's `known_hosts`.
///
/// # Errors
/// One line.
pub fn pin_host_key(
    sys: &dyn System,
    paths: &Paths,
    uuid: &str,
    key: &HostKey,
) -> Result<(), String> {
    rewrite_known_hosts(sys, paths, uuid, Some(key))
}

/// Remove `uuid`'s pinned key.
///
/// # Errors
/// One line.
pub fn unpin_host_key(sys: &dyn System, paths: &Paths, uuid: &str) -> Result<(), String> {
    rewrite_known_hosts(sys, paths, uuid, None)
}

fn rewrite_known_hosts(
    sys: &dyn System,
    paths: &Paths,
    alias: &str,
    key: Option<&HostKey>,
) -> Result<(), String> {
    let path = paths.known_hosts();
    let current = match sys.read_file(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let next = known_hosts_with(&current, alias, key);
    sys.write_file(&path, next.as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// An OpenSSH control master to the host being enrolled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Master {
    /// Its ControlPath.
    pub ctl: PathBuf,
    /// `user@address`.
    pub destination: String,
}

/// The options every enrollment connection carries: no config file (the owner's
/// `~/.ssh/config` stays out), no forwarding.
fn common() -> Vec<OsString> {
    [
        "-F",
        "/dev/null",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "ClearAllForwardings=yes",
    ]
    .map(OsString::from)
    .to_vec()
}

/// `ssh -M`: the master the admin's password logs in. It trusts only the key in
/// `known_hosts` under `alias`, takes one password prompt by `method` alone (so a key the
/// owner's agent holds, the fleet key on a re-enrolled Mac among them, cannot turn the session
/// into a forced command, and a wrong password costs the Mac one failed attempt), answers it
/// with `askpass` (this aterm's helper, which reads the password from standard input:
/// [`System::run_fed`]), and lingers 120 seconds after its last client.
#[must_use]
pub fn master_cmd(
    master: &Master,
    known_hosts: &Path,
    alias: &str,
    askpass: &Path,
    method: &str,
) -> Cmd {
    Cmd::new(SSH)
        .env("SSH_ASKPASS", askpass.to_string_lossy())
        .env("SSH_ASKPASS_REQUIRE", "force")
        .args(common())
        .args(["-M", "-S"])
        .arg(master.ctl.as_os_str())
        .args([
            "-o",
            "ControlPersist=120",
            "-o",
            "StrictHostKeyChecking=yes",
        ])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .args(["-o", "GlobalKnownHostsFile=/dev/null", "-o"])
        .arg(format!("HostKeyAlias={alias}"))
        .args(["-o", "PubkeyAuthentication=no", "-o"])
        .arg(format!("PreferredAuthentications={method}"))
        .args([
            "-o",
            "NumberOfPasswordPrompts=1",
            "-o",
            "ConnectTimeout=15",
            "--",
        ])
        .arg(&master.destination)
        .arg("true")
        .budget(LOGIN_BUDGET)
}

/// The options that ride `master`, and fail rather than open a new connection: a client
/// whose master is gone falls back to connecting by itself, and the install line's client
/// carries the password on its input, so that connection trusts no host key at all (no
/// `known_hosts`, strict checking: it stops at the key, before any login), and could log in
/// by no method if it got further. A client riding the master checks no key: the master did.
fn through(master: &Master) -> Vec<OsString> {
    let mut args = common();
    args.extend(["-S".into(), master.ctl.clone().into_os_string()]);
    args.extend(
        [
            "-o",
            "ControlMaster=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "PubkeyAuthentication=no",
        ]
        .map(OsString::from),
    );
    args
}

/// Run `script` on the host through `master`, by `/bin/sh` (the admin's login shell, zsh
/// on macOS, would expand a word such as `=x`; measured: `echo ===` fails in zsh with
/// `== not found`).
#[must_use]
pub fn remote_sh(master: &Master, script: &str, tty: bool) -> Cmd {
    let mut cmd = Cmd::new(SSH);
    if tty {
        cmd = cmd.arg("-t");
    }
    cmd.args(through(master))
        .arg("--")
        .arg(&master.destination)
        .arg(format!("/bin/sh -c {}", shell_word(script)))
}

/// End the master.
pub fn close_master(sys: &dyn System, master: &Master) {
    let _ = sys.run(
        &Cmd::new(SSH)
            .args(common())
            .arg("-S")
            .arg(master.ctl.as_os_str())
            .args(["-O", "exit", "--"])
            .arg(&master.destination)
            .budget(Duration::from_secs(10)),
    );
}

/// What enrollment reads about the host before anything is installed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// IOPlatformUUID.
    pub uuid: String,
    /// IOPlatformSerialNumber.
    pub serial: String,
    /// `hw.model`.
    pub model: String,
    /// `uname -m`.
    pub arch: String,
    /// `sw_vers -productVersion`.
    pub os_version: String,
    /// `sw_vers -buildVersion`.
    pub os_build: String,
    /// `scutil --get ComputerName`.
    pub computer_name: String,
    /// `scutil --get LocalHostName`.
    pub local_host_name: String,
    /// The admin's temporary directory (`getconf DARWIN_USER_TEMP_DIR`).
    pub tmpdir: String,
    /// The chip ([`crate::hardware::brand_cmd`]: `Apple M4 Max`), empty when unread.
    pub chip: String,
    /// The device tree's `product-name` ([`crate::hardware::product_cmd`]: `MacBook Air
    /// (15-inch, M5)`), empty on an Intel Mac, which has none.
    pub product_name: String,
}

impl Facts {
    /// The Mac as the owner knows it, for the line enrollment prints once it logged in:
    /// `"lab-07": MacBook Pro, Apple M4 Max, macOS 27.0, serial ZNYVNTRJN5`. The raw model
    /// identifier only when it names no family.
    #[must_use]
    pub fn described(&self) -> String {
        let product = (!self.product_name.is_empty()).then_some(self.product_name.as_str());
        let mut parts = vec![
            crate::hardware::model_name(&self.model, product).unwrap_or_else(|| self.model.clone()),
        ];
        if let Some((chip, _)) = crate::hardware::parse_brand(&self.chip) {
            parts.push(chip);
        }
        parts.push(format!("macOS {}", self.os_version));
        if !self.serial.is_empty() {
            parts.push(format!("serial {}", self.serial));
        }
        let parts: Vec<String> = parts
            .iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| shown(p))
            .collect();
        format!("{:?}: {}", shown(&self.computer_name), parts.join(", "))
    }
}

/// The facts, one tool per section, sections separated by a line `--aterm--`, after a check
/// that the host is macOS at all: a Linux box that took the password stops here, before
/// anything is copied to it.
pub const FACTS_SCRIPT: &str = "[ \"$(/usr/bin/uname -s)\" = Darwin ] || { echo \"not macOS: \
$(/usr/bin/uname -s)\" >&2; exit 3; }; /usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice; \
echo --aterm--; /usr/sbin/sysctl -n hw.model; echo --aterm--; /usr/bin/uname -m; \
echo --aterm--; /usr/bin/sw_vers -productVersion; echo --aterm--; /usr/bin/sw_vers -buildVersion; \
echo --aterm--; /usr/sbin/scutil --get ComputerName; echo --aterm--; \
/usr/sbin/scutil --get LocalHostName; echo --aterm--; /usr/bin/getconf DARWIN_USER_TEMP_DIR; \
echo --aterm--; /usr/sbin/sysctl -n machdep.cpu.brand_string; echo --aterm--; \
/usr/sbin/ioreg -p IODeviceTree -r -n product -d 1";

/// Parse [`FACTS_SCRIPT`]'s output.
///
/// # Errors
/// One line naming the fact that is missing (the UUID and the temporary directory are
/// required; a name may be unset).
pub fn parse_facts(out: &str) -> Result<Facts, String> {
    let sections: Vec<&str> = out.split("--aterm--\n").collect();
    let [
        ioreg,
        model,
        arch,
        version,
        build,
        computer,
        local,
        tmp,
        chip,
        product,
    ] = sections.as_slice()
    else {
        return Err(format!(
            "the host's facts came back in {} sections, not 10",
            sections.len()
        ));
    };
    let uuid = crate::agent::platform::ioreg_value(ioreg, "IOPlatformUUID")
        .filter(|u| crate::agent::platform::uuid_shaped(u))
        .ok_or("the host printed no IOPlatformUUID")?;
    let tmpdir = tmp.trim().to_owned();
    if !tmpdir.starts_with('/') || tmpdir.contains(['\'', '"', '\\', '\n']) {
        return Err(format!(
            "the admin's temporary directory {tmpdir:?} is not a plain path"
        ));
    }
    Ok(Facts {
        uuid,
        serial: crate::agent::platform::ioreg_value(ioreg, "IOPlatformSerialNumber")
            .unwrap_or_default(),
        model: model.trim().to_owned(),
        arch: arch.trim().to_owned(),
        os_version: version.trim().to_owned(),
        os_build: build.trim().to_owned(),
        computer_name: computer.trim().to_owned(),
        local_host_name: local.trim().to_owned(),
        tmpdir,
        chip: chip.trim().to_owned(),
        product_name: crate::hardware::parse_product_name(product).unwrap_or_default(),
    })
}

/// Read the host's facts as the admin, through `master`.
///
/// # Errors
/// One line.
pub fn read_facts(sys: &dyn System, master: &Master) -> Result<Facts, String> {
    let ran = sys
        .run(&remote_sh(master, FACTS_SCRIPT, false).budget(Duration::from_secs(60)))
        .map_err(|e| format!("reading the host's facts: {e}"))?;
    if !ran.success() {
        return Err(format!("reading the host's facts: {}", ran.describe()));
    }
    parse_facts(&ran.out())
}

/// The agent payload: one archive file (design §1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    /// The zip, in the controller's scratch directory.
    pub path: PathBuf,
    /// Its SHA-256.
    pub sha256: String,
    /// The agent executable inside the expanded archive (`aterm.app/Contents/MacOS/aterm`,
    /// or the dev binary's name).
    pub exe: PathBuf,
    /// A bundle (its signature is checked on the host), or a bare dev binary.
    pub bundle: bool,
}

/// What to archive for the running executable `exe`: its enclosing `.app` (the
/// Developer ID seal binds the whole bundle), else the binary itself. Returns the path to
/// archive and the executable's path inside the expanded archive.
#[must_use]
pub fn payload_of(exe: &Path) -> (PathBuf, PathBuf, bool) {
    for dir in exe.ancestors().skip(1) {
        let is_app = dir.extension().is_some_and(|e| e == "app")
            && exe.starts_with(dir.join("Contents/MacOS"));
        if is_app && let (Some(name), Ok(inner)) = (dir.file_name(), exe.strip_prefix(dir)) {
            return (dir.to_path_buf(), Path::new(name).join(inner), true);
        }
    }
    let name = exe.file_name().map(PathBuf::from).unwrap_or_default();
    (exe.to_path_buf(), name, false)
}

/// Archive the agent with `ditto -c -k` into `scratch` and hash it: a bundle with
/// `--keepParent` (the archive holds `aterm.app/…`), a bare binary without it (measured:
/// `--keepParent` on a file keeps the file's own directory too, `e2e/aterm-machines-dev`).
///
/// # Errors
/// One line.
pub fn build_archive(sys: &dyn System, exe: &Path, scratch: &Path) -> Result<Archive, String> {
    let (source, inner, bundle) = payload_of(exe);
    // The scratch directory is this enrollment's own and fresh, so nothing is there yet.
    let zip = scratch.join("aterm-agent.zip");
    let mut ditto = Cmd::new("/usr/bin/ditto").args([
        "-c",
        "-k",
        "--norsrc",
        "--noextattr",
        "--noqtn",
        "--noacl",
    ]);
    if bundle {
        ditto = ditto.arg("--keepParent");
    }
    let ran = sys
        .run(
            &ditto
                .arg(source.as_os_str())
                .arg(zip.as_os_str())
                .budget(Duration::from_secs(600)),
        )
        .map_err(|e| format!("ditto: {e}"))?;
    if !ran.success() {
        return Err(format!(
            "ditto -c -k {}: {}",
            source.display(),
            ran.describe()
        ));
    }
    let sha256 = sys
        .sha256(&zip)
        .map_err(|e| format!("{}: {e}", zip.display()))?;
    Ok(Archive {
        path: zip,
        sha256,
        exe: inner,
        bundle,
    })
}

/// The one `scp` through `master` that copies `files` into `remote_dir` on the host. With
/// the master gone it trusts no host key, as [`through`]'s clients do, so it cannot copy
/// to a machine by the owner's own keys and `known_hosts`.
#[must_use]
pub fn copy_cmd(master: &Master, files: &[&Path], remote_dir: &str) -> Cmd {
    let mut cmd = Cmd::new("/usr/bin/scp")
        .args(["-q", "-F", "/dev/null", "-o"])
        .arg(format!("ControlPath={}", master.ctl.display()))
        .args([
            "-o",
            "ControlMaster=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "BatchMode=yes",
            "--",
        ]);
    for f in files {
        cmd = cmd.arg(f.as_os_str());
    }
    cmd.arg(format!("{}:{remote_dir}", master.destination))
        .budget(Duration::from_secs(1800))
}

/// Copy `files` into `remote_dir` on the host with one `scp` through `master`.
///
/// # Errors
/// One line.
pub fn copy_payload(
    sys: &dyn System,
    master: &Master,
    files: &[&Path],
    remote_dir: &str,
) -> Result<(), String> {
    let cmd = copy_cmd(master, files, remote_dir);
    let ran = sys.run(&cmd).map_err(|e| format!("scp: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("scp: {}", ran.describe()))
    }
}

/// What the install command line needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install<'a> {
    /// The archive.
    pub archive: &'a Archive,
    /// Its copy in the admin's temporary directory.
    pub remote_archive: String,
    /// `allowed_signers`' copy there.
    pub remote_signers: String,
    /// Its SHA-256.
    pub signers_sha: String,
    /// The admin account.
    pub admin: String,
    /// The fleet key's public line.
    pub fleet_key: String,
}

/// The root half of design §4.2 step 5, for the one `/bin/sh` that `sudo` runs as root. It
/// drops its standard input first: under a NOPASSWD rule `sudo -S` reads nothing, and the
/// password would still be waiting there for whatever read it next. The archive is executed
/// only after its root-owned copy hashed right.
#[must_use]
pub fn root_script(i: &Install<'_>) -> String {
    let root = crate::AGENT_ROOT;
    let staging = format!("{root}/.staging");
    let q = |s: &str| shell_word(s);
    let exe = format!("{staging}/payload/{}", i.archive.exe.display());
    let mut s = String::new();
    s.push_str("exec </dev/null\nset -e\n");
    s.push_str(&format!(
        "/bin/mkdir -p -m 0755 /Library/Management {root}\n\
         /usr/sbin/chown root:wheel /Library/Management {root}\n\
         /bin/chmod 0755 /Library/Management {root}\n\
         /bin/rm -rf {staging}\n\
         /bin/mkdir -m 0700 {staging}\n\
         /bin/cp {} {staging}/payload.zip\n\
         /bin/cp {} {staging}/allowed_signers\n",
        q(&i.remote_archive),
        q(&i.remote_signers),
    ));
    for (file, sha) in [
        ("payload.zip", i.archive.sha256.as_str()),
        ("allowed_signers", i.signers_sha.as_str()),
    ] {
        s.push_str(&format!(
            "got=$(/usr/bin/shasum -a 256 {staging}/{file})\n\
             got=${{got%% *}}\n\
             if [ \"$got\" != {sha} ]; then echo \"aterm machines: the root-owned {file} \
             hashes to $got, not {sha}; nothing was installed\" >&2; exit 1; fi\n"
        ));
    }
    s.push_str(&format!(
        "/usr/bin/ditto -x -k {staging}/payload.zip {staging}/payload\n\
         /usr/sbin/chown -R root:wheel {staging}/payload\n\
         /bin/chmod -R go-w {staging}/payload\n\
         /bin/chmod 0755 {staging}/payload\n"
    ));
    if i.archive.bundle
        && let Some(app) = i.archive.exe.components().next()
    {
        s.push_str(&format!(
            "/usr/bin/codesign --verify --strict {}\n",
            q(&format!("{staging}/payload/{}", Path::new(&app).display()))
        ));
    }
    s.push_str(&format!(
        "{} machines agent install --admin {} --fleet-key {} --allowed-signers \
         {staging}/allowed_signers --payload-sha {} --stage {staging}/payload\n",
        q(&exe),
        q(&i.admin),
        q(&i.fleet_key),
        i.archive.sha256,
    ));
    s
}

/// How the install command line's one `sudo` gets the admin's password.
#[derive(Debug, Clone, Copy)]
pub enum Sudo<'p> {
    /// Typed at Apple's sudo prompt, on this terminal (`ssh -t`): the dev harness's
    /// `enroll-through`, over a master someone else opened.
    Terminal,
    /// The password `enroll` asked once, read by `sudo -S` from the channel.
    Password(&'p Secret),
}

/// The one command line of design §4.2 step 5, for the admin's `/bin/sh` on the host: one
/// `sudo` running [`root_script`] (`-S -p ''` when the password comes over the channel), then
/// the admin's own copies removed, whatever the root shell's status, which it ends with.
#[must_use]
pub fn install_command(i: &Install<'_>, sudo: Sudo<'_>) -> String {
    let sudo = match sudo {
        Sudo::Terminal => "/usr/bin/sudo --",
        Sudo::Password(_) => "/usr/bin/sudo -S -p '' --",
    };
    format!(
        "{sudo} /bin/sh -c {}\nstatus=$?\n/bin/rm -f {} {}\nexit $status\n",
        shell_word(&root_script(i)),
        shell_word(&i.remote_archive),
        shell_word(&i.remote_signers)
    )
}

/// This aterm's own executable, its links resolved: run through the installer's
/// `~/.local/bin/aterm` link, `current_exe` names the link, which neither hashes (the port
/// opens with `O_NOFOLLOW`) nor sits inside the bundle `enroll` and `apply` archive.
///
/// # Errors
/// One line when it cannot be named.
pub fn own_exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("this aterm cannot name its executable: {e}"))
}

/// `aterm machines enroll …` (refuses without a terminal: a person types the password).
#[must_use]
pub fn main(sys: &dyn System, paths: &Paths, args: &[String], tty: bool) -> ExitCode {
    let args = match parse_args(sys, args) {
        Ok(a) => a,
        Err(why) => {
            eprintln!("aterm machines {why}");
            return ExitCode::from(2);
        }
    };
    if !tty {
        eprintln!(
            "aterm machines enroll: not run: you type the admin's password, once, and standard \
             input or error is not a terminal"
        );
        return ExitCode::from(2);
    }
    let exe = match own_exe() {
        Ok(e) => e,
        Err(why) => {
            eprintln!("aterm machines enroll: {why}");
            return ExitCode::FAILURE;
        }
    };
    // A controller not set up yet is set up first, as every verb that reads the inventory
    // does; read first, so one that cannot be refuses before anything is asked.
    super::commands::set_up_if_new(sys, paths);
    let inv = match inventory::load(sys, paths) {
        Ok(inv) => inv,
        Err(why) => {
            eprintln!("aterm machines enroll: {why}");
            return ExitCode::FAILURE;
        }
    };
    let args = with_recorded_admin(args, &inv);
    let mut macs = args.requests();
    if args.found {
        let browse = match discover::browse(sys, FOUND_WAIT) {
            Ok(b) => b,
            Err(why) => {
                eprintln!("aterm machines enroll --found: {why}");
                return ExitCode::FAILURE;
            }
        };
        let (found, notes) = found_macs(sys, paths, &inv, &browse);
        for note in notes {
            println!("{note}");
        }
        for address in found {
            if !macs
                .iter()
                .any(|m| m.address.eq_ignore_ascii_case(&address))
            {
                macs.push(Request {
                    address,
                    user: args.user.clone(),
                    name: None,
                    trust: Trust::FirstUse,
                });
            }
        }
        if macs.is_empty() {
            println!(
                "no Mac to enroll: each one `aterm machines scan` sees is enrolled already, or \
                 none advertises Remote Login"
            );
            return ExitCode::SUCCESS;
        }
        let list: Vec<&str> = macs.iter().map(|m| m.address.as_str()).collect();
        println!("enrolling {} Macs: {}", macs.len(), list.join(", "));
    }
    let parallel = args
        .parallel
        .unwrap_or_else(|| inv.parallel())
        .clamp(1, macs.len().max(1));
    let mut stdout = std::io::stdout();
    let sink: Sink<'_> = Mutex::new(&mut stdout as &mut (dyn Write + Send));
    let summary = match enroll_macs(sys, paths, &args, &macs, &exe, parallel, &sink) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("aterm machines enroll: {}", shown(&why));
            return ExitCode::FAILURE;
        }
    };
    drop(sink);
    // A Mac enrolled already is nothing left to do, never a failure: the same command run
    // again after a skip finishes the rest and exits 0.
    let ok = summary.skipped.is_empty();
    if macs.len() == 1 {
        match (
            summary.enrolled.first(),
            summary.already.first(),
            summary.skipped.first(),
        ) {
            (Some((_, host)), _, _) => println!(
                "enrolled {} ({}). Next: `aterm machines apply {}` (it installs aterm.app), \
                 then at its keyboard: aterm machines person {} --here",
                host.name, host.uuid, host.name, host.name
            ),
            (None, Some((_, name)), _) => {
                println!("already enrolled as {name}: nothing to do");
            }
            // The why can carry what the Mac printed (its facts script, ssh, scp).
            (None, None, Some((_, why))) => {
                eprintln!("aterm machines enroll: not enrolled: {}", shown(why));
            }
            (None, None, None) => {}
        }
    } else {
        print!("{}", summary.text());
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Standard output, shared by the Macs enrolled at once.
pub type Sink<'a> = Mutex<&'a mut (dyn Write + Send)>;

/// `m`, recovered when a thread that held it panicked (what it guards stays usable: a list of
/// claims, a sink of lines).
fn locked<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One Mac's lines on a [`Sink`]: each whole, prefixed by its label when several Macs share
/// the sink.
struct Lines<'s, 'w> {
    label: Option<String>,
    sink: &'s Sink<'w>,
    line: Vec<u8>,
}

impl<'s, 'w> Lines<'s, 'w> {
    const fn new(label: Option<String>, sink: &'s Sink<'w>) -> Self {
        Self {
            label,
            sink,
            line: Vec::new(),
        }
    }

    fn emit(&mut self) {
        let mut out = locked(self.sink);
        if let Some(label) = &self.label {
            let _ = write!(out, "{label}: ");
        }
        let _ = out.write_all(&self.line);
        let _ = out.flush();
        self.line.clear();
    }
}

impl Write for Lines<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for &b in buf {
            self.line.push(b);
            if b == b'\n' {
                self.emit();
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Lines<'_, '_> {
    fn drop(&mut self) {
        if !self.line.is_empty() {
            self.line.push(b'\n');
            self.emit();
        }
    }
}

/// What an `enroll` of several Macs did.
#[derive(Debug, Default)]
pub struct Summary {
    /// Each Mac enrolled: its label and its inventory entry.
    pub enrolled: Vec<(String, Host)>,
    /// Each Mac that was enrolled before, answering with the host key pinned for it: its
    /// label and its inventory name. Nothing to do, and no failure.
    pub already: Vec<(String, String)>,
    /// Each Mac not enrolled: its label and why.
    pub skipped: Vec<(String, String)>,
}

impl Summary {
    /// The lines the command ends with: one for what it did, then one per Mac it could not
    /// enroll (with 29 Macs, reasons run together on one line), then what to run next.
    #[must_use]
    pub fn text(&self) -> String {
        let mut head = format!("enrolled {}", self.enrolled.len());
        if !self.enrolled.is_empty() {
            let enrolled: Vec<&str> = self.enrolled.iter().map(|(_, h)| h.name.as_str()).collect();
            head.push_str(&format!(" ({})", enrolled.join(", ")));
        }
        if !self.already.is_empty() {
            let already: Vec<&str> = self.already.iter().map(|(_, n)| n.as_str()).collect();
            head.push_str(&format!(
                "; already enrolled, nothing to do: {}",
                already.join(", ")
            ));
        }
        if !self.skipped.is_empty() {
            head.push_str(&format!("; not enrolled: {}", self.skipped.len()));
        }
        let mut text = format!("{head}\n");
        for (label, why) in &self.skipped {
            text.push_str(&format!("  {label}: {}\n", shown(why)));
        }
        if !self.enrolled.is_empty() {
            text.push_str(
                "Next: `aterm machines apply --all --only names` renames them for what they are \
                 (m10 MacBook Pro M4 Max 36GB 1TB); `aterm machines apply --all` also sets up \
                 the rest of the baseline (apps, settings)\n",
            );
        }
        text
    }
}

/// The label each Mac's lines carry: the tag its address starts with (`m10` of
/// `m10-MacBook-M4-Max-36GB-1TB.local`), the whole name before `.local` when it has none, an
/// IP address as it is; the address itself for a label two Macs would share.
#[must_use]
pub fn labels(macs: &[Request]) -> Vec<String> {
    let short: Vec<String> = macs
        .iter()
        .map(|m| {
            let host = m
                .address
                .strip_suffix(".local")
                .unwrap_or(&m.address)
                .to_owned();
            if host.parse::<std::net::IpAddr>().is_ok() || host.contains('.') {
                m.address.clone()
            } else {
                crate::items::names::tag(&host).to_owned()
            }
        })
        .collect();
    short
        .iter()
        .zip(macs)
        .map(|(label, mac)| {
            let shared = short
                .iter()
                .filter(|other| other.eq_ignore_ascii_case(label))
                .count()
                > 1;
            if shared {
                mac.address.clone()
            } else {
                label.clone()
            }
        })
        .collect()
}

/// How one Mac ended.
#[derive(Debug)]
enum Done {
    /// Enrolled.
    Enrolled(Host),
    /// Enrolled before, as this inventory name: it answers with the host key pinned for it.
    Already(String),
    /// Its `ssh` refused the password (or the account), why: tried again with each password
    /// or name typed later that works, and asked about once.
    Refused(String),
    /// Stopped by anything else, in one line.
    Skipped(String),
}

/// Enroll `macs`: the agent payload built once; each Mac's host key read, checked and its
/// fingerprint printed before anything is asked (as one Mac's enrollment always did, so a key
/// `--fingerprint` does not name, or one that is not the pinned key of the enrolled Mac whose
/// address this is, stops that Mac before the password is typed); the admin's password asked
/// once; each Mac enrolled `parallel` at a time; then the Macs whose `ssh` refused it.
///
/// A refusal is a wrong password or no such account, which `ssh` cannot tell apart. With no
/// `--user`, the name was a guess (the admin recorded for the Macs enrolled before, else this
/// Mac's login name), so the first refusal asks for the name, once, and tries the password
/// already typed under it before anything else is asked: the owner whose lab admin is not
/// their login name types the password once. Each Mac that still refuses is then asked
/// for once, by name, and a password one Mac takes is tried on every other Mac still
/// refusing, those asked before included: a typo at one Mac's prompt costs it nothing when
/// the next password typed is right. Every password typed stays in memory, never on disk,
/// until the command ends.
///
/// # Errors
/// One line when nothing could be tried: the payload (a controller that is not set up), the
/// helper link, or no password typed.
pub fn enroll_macs(
    sys: &dyn System,
    paths: &Paths,
    args: &Args,
    macs: &[Request],
    exe: &Path,
    parallel: usize,
    sink: &Sink<'_>,
) -> Result<Summary, String> {
    let scratch = Scratch::new(sys, "enroll").map_err(|e| format!("enroll scratch: {e}"))?;
    let shared = Shared::new(sys, paths, exe, scratch.path());
    // Built before anything is asked: a controller without `init` refuses first.
    shared.payload()?;
    let askpass = scratch.path().join(crate::askpass::ALIAS);
    sys.symlink(exe, &askpass)
        .map_err(|e| format!("{}: {e}", askpass.display()))?;
    let labels = labels(macs);
    let several = macs.len() > 1;
    let label_of = |i: usize| several.then(|| labels.get(i).cloned().unwrap_or_default());
    let all: Vec<usize> = (0..macs.len()).collect();
    let scanned: Vec<Result<Scanned<'_>, Unscanned>> = ssh::fan_out(&all, parallel, |&i| {
        let mac = macs
            .get(i)
            .ok_or_else(|| Unscanned::Stopped("no such Mac".into()))?;
        scan_one(sys, paths, mac, &mut Lines::new(label_of(i), sink))
    });
    // A Mac whose key stopped it, that answered with no key at all (Remote Login off:
    // enrollment needs it, and the line says to turn it on), or that is enrolled already is
    // done as it stands. The others wait for the password.
    let mut done: Vec<Done> = scanned
        .iter()
        .map(|scan| match scan {
            Ok(_) => Done::Skipped(String::new()),
            Err(Unscanned::Enrolled(name)) => Done::Already(name.clone()),
            Err(Unscanned::NoKey(why) | Unscanned::Stopped(why)) => Done::Skipped(why.clone()),
        })
        .collect();
    // Of several Macs, each one its scan stopped says so on its own line now, before the
    // password is asked (a Mac with Remote Login off behind stealth mode keeps the scan
    // waiting 10 s); the summary says it again. A Mac alone gets the command's one line.
    if several {
        for (i, result) in done.iter().enumerate() {
            if scanned.get(i).is_some_and(Result::is_ok) {
                continue;
            }
            let label = labels.get(i).map_or("", String::as_str);
            match result {
                Done::Already(name) => {
                    say(
                        sink,
                        label,
                        &format!("already enrolled as {name}: nothing to do"),
                    );
                }
                Done::Skipped(why) => say(sink, label, &format!("not enrolled: {why}")),
                Done::Enrolled(_) | Done::Refused(_) => {}
            }
        }
    }
    let ready: Vec<usize> = all
        .iter()
        .copied()
        .filter(|&i| scanned.get(i).is_some_and(Result::is_ok))
        .collect();
    // One Mac's login and install with `password`, over the key its scan trusted.
    let one = |i: usize, user: &str, password: &Secret| -> Done {
        let (Some(mac), Some(Ok(scan))) = (macs.get(i), scanned.get(i)) else {
            return Done::Skipped("no such Mac".into());
        };
        let req = Request {
            user: user.to_owned(),
            ..mac.clone()
        };
        let mut out = Lines::new(label_of(i), sink);
        enroll_one(&shared, &askpass, &req, scan, password, several, &mut out)
    };
    // Each result in its Mac's slot, a refusal's newest why replacing the one before.
    let store = |done: &mut Vec<Done>, tried: &[usize], results: Vec<Done>| {
        for (&i, result) in tried.iter().zip(results) {
            if let Some(slot) = done.get_mut(i) {
                *slot = result;
            }
        }
    };
    let refusing = |done: &[Done]| -> Vec<usize> {
        all.iter()
            .copied()
            .filter(|&i| matches!(done.get(i), Some(Done::Refused(_))))
            .collect()
    };
    let mut user = args.user.clone();
    if !ready.is_empty() {
        let Some(password) = ask(sys, &first_prompt(args, macs, &ready, &user), true)? else {
            return Err("not run: no password typed".into());
        };
        let tried = ssh::fan_out(&ready, parallel, |&i| one(i, &user, &password));
        store(&mut done, &ready, tried);
        let mut name_asked = args.user_given;
        let mut asked: Vec<usize> = Vec::new();
        let mut stopped = false;
        loop {
            let refused = refusing(&done);
            if refused.is_empty() {
                break;
            }
            if !name_asked {
                name_asked = true;
                say_line(
                    sink,
                    &format!(
                        "{}: no login as {user} with that password (a wrong password, or no \
                         account called {user} there)",
                        who(&labels, &refused)
                    ),
                );
                let on = if several { "the Macs" } else { "that Mac" };
                match ask_name(sys, &user, on) {
                    Err(_) => {
                        stopped = true;
                        break;
                    }
                    // The password typed is most likely right and the name was what failed:
                    // it is tried under the new name before anything else is asked.
                    Ok(Some(name)) if name != user => {
                        user = name;
                        let retried =
                            ssh::fan_out(&refused, parallel, |&j| one(j, &user, &password));
                        store(&mut done, &refused, retried);
                        continue;
                    }
                    Ok(_) => {}
                }
            }
            let Some(i) = refused.iter().copied().find(|i| !asked.contains(i)) else {
                break;
            };
            asked.push(i);
            let label = labels.get(i).cloned().unwrap_or_default();
            let at = if several {
                label.clone()
            } else {
                macs.get(i).map_or(label.clone(), |m| m.address.clone())
            };
            let also = if refused.len() > 1 {
                "it is also tried on the other Macs that refused; Enter skips this Mac"
            } else {
                "Enter skips it"
            };
            let prompt =
                format!("{at} did not take the password for {user}. Type it again ({also}): ");
            let retyped = match ask(sys, &prompt, true) {
                Ok(retyped) => retyped,
                Err(_) => {
                    stopped = true;
                    break;
                }
            };
            let Some(retyped) = retyped else {
                if let Some(slot) = done.get_mut(i)
                    && let Done::Refused(why) = slot
                {
                    *slot = Done::Skipped(format!(
                        "{why}; skipped at its password prompt: run the same enroll command \
                         again to enroll it"
                    ));
                }
                continue;
            };
            let again = one(i, &user, &retyped);
            if matches!(again, Done::Refused(_)) {
                say(sink, &label, "did not take that password either");
            }
            let worked = matches!(again, Done::Enrolled(_));
            store(&mut done, &[i], vec![again]);
            // A retyped password that works is tried on every other Mac still refusing,
            // those asked before among them, before the next one is asked.
            let others = refusing(&done);
            if worked && !others.is_empty() {
                let retried = ssh::fan_out(&others, parallel, |&j| one(j, &user, &retyped));
                store(&mut done, &others, retried);
            }
        }
        // What still refuses was asked about once (or the prompt was stopped): not enrolled.
        for slot in &mut done {
            if let Done::Refused(why) = slot {
                let next = if stopped {
                    "not asked again: the prompt was stopped"
                } else {
                    "run the same enroll command again to retry it"
                };
                *slot = Done::Skipped(format!("{why}; {next}"));
            }
        }
    }
    let mut summary = Summary::default();
    for (label, result) in labels.into_iter().zip(done) {
        match result {
            Done::Enrolled(host) => summary.enrolled.push((label, host)),
            Done::Already(name) => summary.already.push((label, name)),
            Done::Refused(why) | Done::Skipped(why) => summary.skipped.push((label, why)),
        }
    }
    Ok(summary)
}

/// The first password prompt. It names the account when the owner named it (`--user`) or the
/// inventory records it; a guess from this Mac's login name is not named, so the owner types
/// the lab's admin password rather than this Mac's.
fn first_prompt(args: &Args, macs: &[Request], ready: &[usize], user: &str) -> String {
    let whose = if args.user_given || args.user_recorded {
        format!("Password for {user}")
    } else {
        "Admin password".to_owned()
    };
    match ready {
        [i] => format!(
            "{whose} on {} (never saved): ",
            macs.get(*i).map_or("the Mac", |m| m.address.as_str())
        ),
        _ => format!(
            "{whose} on the {} Macs (typed once, used for each, never saved): ",
            ready.len()
        ),
    }
}

/// The Macs `refused` names, for one line: a Mac by its label, several by their number and
/// labels.
fn who(labels: &[String], refused: &[usize]) -> String {
    let named: Vec<&str> = refused
        .iter()
        .filter_map(|&i| labels.get(i).map(String::as_str))
        .collect();
    match named.as_slice() {
        [one] => (*one).to_owned(),
        _ => format!("{} Macs ({})", named.len(), named.join(", ")),
    }
}

/// Ask the admin's name, once, on the Macs `on` names. `None` when Enter kept `user` or
/// what was typed is no account name (said so).
fn ask_name(sys: &dyn System, user: &str, on: &str) -> Result<Option<String>, String> {
    let prompt = format!("What is the admin account called on {on}? (Enter keeps {user}): ");
    let Some(name) = ask(sys, &prompt, false)? else {
        return Ok(None);
    };
    let name = String::from_utf8_lossy(name.bytes()).trim().to_owned();
    if name.is_empty() {
        return Ok(None);
    }
    if !plain_account(&name) {
        // Not repeated: a password typed here by mistake stays off the screen.
        say_stderr(&format!(
            "that is not an account name (letters, digits, '.', '_' or '-'); keeping {user}"
        ));
        return Ok(None);
    }
    Ok(Some(name))
}

/// [`System::ask`], its failure one line (a stopped prompt says so).
fn ask(sys: &dyn System, prompt: &str, secret: bool) -> Result<Option<Secret>, String> {
    sys.ask(prompt, secret).map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            "stopped at the prompt".to_owned()
        } else {
            format!("the terminal: {e}")
        }
    })
}

/// One line on the sink, labelled.
fn say(sink: &Sink<'_>, label: &str, line: &str) {
    say_line(sink, &format!("{label}: {line}"));
}

/// One line on the sink.
fn say_line(sink: &Sink<'_>, line: &str) {
    let mut out = locked(sink);
    let _ = writeln!(out, "{}", shown(line));
    let _ = out.flush();
}

fn say_stderr(line: &str) {
    eprintln!("aterm machines enroll: {line}");
}

/// The agent payload one `enroll` copies to every Mac, built once: the archive and
/// `allowed_signers` under the names their copies in the admin's temporary directory get,
/// and the fleet key's line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// The archive (`aterm-agent-<8 hex>.zip`).
    pub archive: Archive,
    /// `allowed_signers` (`aterm-allowed-signers-<8 hex>`).
    pub signers: PathBuf,
    /// Its SHA-256.
    pub signers_sha: String,
    /// The fleet key's public line.
    pub fleet_key: String,
}

impl Payload {
    /// The first eight hex digits of the archive's SHA-256, which the copies are named by.
    fn tag(&self) -> &str {
        self.archive
            .sha256
            .get(..8)
            .unwrap_or(self.archive.sha256.as_str())
    }
}

/// Build the payload in `scratch`: the archive of `exe`, the controller's `allowed_signers`
/// (checked) and the fleet key's public line.
///
/// # Errors
/// One line (a controller with no `init` names it).
pub fn build_payload(
    sys: &dyn System,
    paths: &Paths,
    exe: &Path,
    scratch: &Path,
) -> Result<Payload, String> {
    let signers = sys.read_file(&paths.allowed_signers()).map_err(|e| {
        format!(
            "{}: {e} (run `aterm machines init`)",
            paths.allowed_signers().display()
        )
    })?;
    let signers = String::from_utf8(signers).map_err(|_| "allowed_signers is not UTF-8")?;
    envelope::check_signers(&signers).map_err(|why| format!("the controller's {why}"))?;
    let fleet_pub = paths.ssh_key().with_extension("pub");
    let fleet_key = sys
        .read_file(&fleet_pub)
        .map_err(|e| format!("{}: {e} (run `aterm machines init`)", fleet_pub.display()))?;
    let fleet_key = String::from_utf8_lossy(&fleet_key).trim().to_owned();
    crate::agent::install::fleet_line(&fleet_key)?;
    let archive = build_archive(sys, exe, scratch)?;
    let tag = archive
        .sha256
        .get(..8)
        .unwrap_or(archive.sha256.as_str())
        .to_owned();
    let signers_file = scratch.join(format!("aterm-allowed-signers-{tag}"));
    sys.write_file(&signers_file, signers.as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", signers_file.display()))?;
    let signers_sha = sys
        .sha256(&signers_file)
        .map_err(|e| format!("{}: {e}", signers_file.display()))?;
    // scp names the copies by their local names.
    let local_archive = scratch.join(format!("aterm-agent-{tag}.zip"));
    sys.rename(&archive.path, &local_archive)
        .map_err(|e| format!("{}: {e}", archive.path.display()))?;
    Ok(Payload {
        archive: Archive {
            path: local_archive,
            ..archive
        },
        signers: signers_file,
        signers_sha,
        fleet_key,
    })
}

/// What the Macs of one `enroll` share: the payload, built once by the first Mac that needs
/// it, and the hosts checked and not yet recorded, under whose lock the inventory is checked,
/// a host key pinned and a host recorded, so two Macs enrolled at once never pass each
/// other's checks or race one file.
pub struct Shared<'a> {
    sys: &'a dyn System,
    paths: &'a Paths,
    exe: &'a Path,
    scratch: &'a Path,
    payload: Mutex<Option<Payload>>,
    claims: Mutex<Vec<Host>>,
}

impl<'a> Shared<'a> {
    /// Nothing built or claimed yet; the payload goes in `scratch`.
    #[must_use]
    pub const fn new(
        sys: &'a dyn System,
        paths: &'a Paths,
        exe: &'a Path,
        scratch: &'a Path,
    ) -> Self {
        Self {
            sys,
            paths,
            exe,
            scratch,
            payload: Mutex::new(None),
            claims: Mutex::new(Vec::new()),
        }
    }

    /// The payload, built on first use.
    fn payload(&self) -> Result<Payload, String> {
        let mut slot = locked(&self.payload);
        if let Some(payload) = slot.as_ref() {
            return Ok(payload.clone());
        }
        let payload = build_payload(self.sys, self.paths, self.exe, self.scratch)?;
        *slot = Some(payload.clone());
        Ok(payload)
    }

    /// Check `host` against the inventory and the Macs in flight, and hold its place: its
    /// hardware UUID, its name and the Local hostname its baseline would give it are refused
    /// when another Mac has them.
    fn claim(&self, host: &Host) -> Result<(), String> {
        let mut claims = locked(&self.claims);
        let inv = inventory::load(self.sys, self.paths)?;
        if let Some(known) = inv
            .hosts
            .iter()
            .find(|h| h.uuid.eq_ignore_ascii_case(&host.uuid))
        {
            return Err(already_enrolled(known, &host.uuid));
        }
        if let Some(other) = claims
            .iter()
            .find(|h| h.uuid.eq_ignore_ascii_case(&host.uuid))
        {
            return Err(format!(
                "this Mac's hardware UUID {} is the one the Mac at {} answers with, which this \
                 command is enrolling too: the same Mac at two addresses, or a copy of its disk \
                 the fleet cannot tell apart",
                host.uuid, other.address
            ));
        }
        let mut with = inv;
        with.hosts.extend(claims.iter().cloned());
        if let Some(other) = with
            .hosts
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(&host.name))
        {
            return Err(format!(
                "the name {} is taken by the Mac with uuid {} at {}; pass --name <n> for this one",
                host.name, other.uuid, other.address
            ));
        }
        // And the Local hostname its baseline would give it, which no other Mac may be given.
        with.hosts.push(host.clone());
        let wanted = super::naming::wanted(self.sys, self.paths, &with);
        if let Some(why) = wanted
            .get(&host.uuid.to_ascii_lowercase())
            .and_then(|local| super::naming::clash(&with, &wanted, host, local))
        {
            return Err(format!("{why}; pass --name <n> for this one"));
        }
        claims.push(host.clone());
        Ok(())
    }

    /// Give up `uuid`'s place.
    fn release(&self, uuid: &str) {
        locked(&self.claims).retain(|h| !h.uuid.eq_ignore_ascii_case(uuid));
    }

    /// Run `f` under the claims' lock (a pin, a record).
    fn recorded<T>(&self, f: impl FnOnce() -> T) -> T {
        let _held = locked(&self.claims);
        f()
    }
}

/// A claimed place, given up when it drops (recorded or not).
struct Claimed<'s, 'a> {
    shared: &'s Shared<'a>,
    uuid: String,
}

impl Drop for Claimed<'_, '_> {
    fn drop(&mut self) {
        self.shared.release(&self.uuid);
    }
}

/// How one Mac's `ssh` login with the typed password ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    /// The master is open.
    In,
    /// The Mac refused the password, or has no such account (`ssh` cannot tell them apart).
    Refused,
    /// Anything else, in one line.
    Failed(String),
}

/// The methods of `ssh`'s `user@host: Permission denied (publickey,password,…).` line.
#[must_use]
pub fn denied_methods(err: &str) -> Option<&str> {
    const DENIED: &str = "Permission denied (";
    let at = err.find(DENIED)?;
    let rest = err.get(at + DENIED.len()..)?;
    rest.find(')').and_then(|end| rest.get(..end))
}

/// Open `master` with `password`: the password method first, keyboard-interactive for a Mac
/// whose `sshd` offers no password method.
#[must_use]
pub fn log_in(
    sys: &dyn System,
    master: &Master,
    known_hosts: &Path,
    alias: &str,
    askpass: &Path,
    password: &Secret,
    address: &str,
) -> Login {
    let mut method = PASSWORD_METHOD;
    loop {
        let cmd = master_cmd(master, known_hosts, alias, askpass, method);
        let ran = match sys.run_fed(&cmd, password) {
            Ok(ran) => ran,
            Err(e) => return Login::Failed(format!("ssh: {e}")),
        };
        if ran.success() {
            return Login::In;
        }
        let err = ran.err();
        let Some(offered) = denied_methods(&err) else {
            return Login::Failed(ssh::classify(&ran).map_or_else(
                || format!("ssh did not log in ({})", ran.describe()),
                |f| f.why_alone(address),
            ));
        };
        let offers = |m: &str| offered.split(',').any(|o| o.trim() == m);
        if method == PASSWORD_METHOD && !offers(PASSWORD_METHOD) && offers(KBD_METHOD) {
            method = KBD_METHOD;
            continue;
        }
        if offers(method) {
            return Login::Refused;
        }
        return Login::Failed(format!("it takes no password login (it offers {offered})"));
    }
}

/// The alias the one key an enrollment master trusts is filed under in its own
/// `known_hosts`.
const ENROLL_ALIAS: &str = "aterm-machines-enroll";

/// One Mac's host key, read and checked before the password is asked, and the scratch holding
/// the `known_hosts` its master (each login of it, a retry's too) trusts: that key alone.
pub struct Scanned<'s> {
    /// The key.
    pub key: HostKey,
    /// `known_hosts` with `ENROLL_ALIAS <key>`, in `_scratch`.
    pub known: PathBuf,
    _scratch: Scratch<'s>,
}

/// Why a Mac's host key did not ready it for the password.
#[derive(Debug)]
pub enum Unscanned {
    /// No key answered: Remote Login is off, or nothing is at the address.
    NoKey(String),
    /// A key answered and stopped the Mac, or its scratch could not be made.
    Stopped(String),
    /// The Mac enrolled as this inventory name answered at its address with the host key
    /// pinned for it: nothing to do.
    Enrolled(String),
}

/// Read `req`'s host key (`ssh-keyscan`), check it, print its fingerprint, and write the
/// `known_hosts` its master will trust. Nothing is asked or sent: a key `--fingerprint` does
/// not name, or one that is not the pinned key of the enrolled Mac whose address this is,
/// stops the Mac here, before the password is typed.
///
/// # Errors
/// One line, and whether any key answered.
pub fn scan_one<'s>(
    sys: &'s dyn System,
    paths: &Paths,
    req: &Request,
    out: &mut dyn Write,
) -> Result<Scanned<'s>, Unscanned> {
    let scratch = Scratch::new(sys, "enroll")
        .map_err(|e| Unscanned::Stopped(format!("enroll scratch: {e}")))?;
    let key = scan_host_key(sys, &req.address, scratch.path()).map_err(Unscanned::NoKey)?;
    if let Some(name) = check_host_key(&key, &req.trust)
        .and_then(|()| check_enrolled_address(sys, paths, &req.address, &key))
        .map_err(Unscanned::Stopped)?
    {
        return Err(Unscanned::Enrolled(name));
    }
    let _ = writeln!(out, "{}", trusted(&key, &req.trust));
    let known = scratch.path().join("known_hosts");
    sys.write_file(
        &known,
        known_hosts_with("", ENROLL_ALIAS, Some(&key)).as_bytes(),
        0o600,
    )
    .map_err(|e| Unscanned::Stopped(format!("{}: {e}", known.display())))?;
    Ok(Scanned {
        key,
        known,
        _scratch: scratch,
    })
}

/// Refuse a host that answers at the address of an enrolled Mac with a key other than the one
/// pinned for that Mac: that is no first use, so trusting it would hand the password to
/// whatever machine took the address (a rogue on the LAN answering to the Mac's name), or to
/// the Mac erased, which is retired first. The Mac itself, answering with its pinned key, is
/// enrolled already: its inventory name, and nothing left to do. Either way nothing is sent.
///
/// # Errors
/// One line.
pub fn check_enrolled_address(
    sys: &dyn System,
    paths: &Paths,
    address: &str,
    key: &HostKey,
) -> Result<Option<String>, String> {
    let inv = inventory::load(sys, paths)?;
    let plain = |a: &str| a.trim_end_matches('.').to_ascii_lowercase();
    let Some(known) = inv
        .hosts
        .iter()
        .find(|h| plain(&h.address) == plain(address))
    else {
        return Ok(None);
    };
    let name = &known.name;
    let only = Inventory {
        hosts: vec![known.clone()],
        ..Inventory::default()
    };
    match discover::pinned_name(sys, paths, &only, std::slice::from_ref(&key.blob)) {
        Read::Value(_) => Ok(Some(name.clone())),
        Read::Absent => Err(format!(
            "{address} is where {name} is enrolled, but the host key it answers with ({}) is \
             not the one pinned for {name}: another machine answers there, or {name} was erased \
             or reinstalled (then `aterm machines unenroll {name} --unreachable`, and enroll \
             it again). Nothing was sent",
            key.fingerprint
        )),
        Read::Unusable(why) => Err(format!(
            "{address} is where {name} is enrolled, and the key pinned for it cannot be read \
             ({why}). Nothing was sent"
        )),
    }
}

/// Enroll one Mac with `password`, over the host key its scan trusted: login, then
/// [`install_through`].
fn enroll_one(
    shared: &Shared<'_>,
    askpass: &Path,
    req: &Request,
    scan: &Scanned<'_>,
    password: &Secret,
    announce: bool,
    out: &mut dyn Write,
) -> Done {
    let sys = shared.sys;
    let (key, known, alias) = (&scan.key, &scan.known, ENROLL_ALIAS);
    let ctl = shared.paths.control_path(&req.address);
    if let Some(parent) = ctl.parent()
        && let Err(e) = sys.create_dir_all(parent, 0o700)
    {
        return Done::Skipped(format!("{}: {e}", parent.display()));
    }
    let master = Master {
        ctl,
        destination: format!("{}@{}", req.user, req.address),
    };
    // A master an interrupted enroll left at this path would carry this one's commands.
    close_master(sys, &master);
    let _ = writeln!(out, "logging in as {}", req.user);
    match log_in(sys, &master, known, alias, askpass, password, &req.address) {
        Login::In => {}
        Login::Refused => {
            return Done::Refused(format!(
                "no login as {} with the password typed (a wrong password, or no account \
                 called {} there)",
                req.user, req.user
            ));
        }
        Login::Failed(why) => return Done::Skipped(why),
    }
    let host = install_through(shared, req, &master, key, Sudo::Password(password), out);
    close_master(sys, &master);
    match host {
        Ok(host) => {
            // One Mac of several: its own last line (a Mac alone gets the command's).
            if announce {
                let _ = writeln!(out, "enrolled as {}", host.name);
            }
            Done::Enrolled(host)
        }
        Err(why) => Done::Skipped(why),
    }
}

/// Enrollment once a master to the host is open and its host key is known, over a master
/// someone else opened and vouched for (the dev harness's `enroll-through`): Apple's sudo
/// asks the password on this terminal. [`install_through`] does the work.
///
/// # Errors
/// One line.
#[expect(clippy::too_many_arguments, reason = "each is one input of enrollment")]
pub fn enroll_through(
    sys: &dyn System,
    paths: &Paths,
    req: &Request,
    exe: &Path,
    master: &Master,
    key: &HostKey,
    scratch: &Path,
    out: &mut dyn Write,
) -> Result<Host, String> {
    let shared = Shared::new(sys, paths, exe, scratch);
    install_through(&shared, req, master, key, Sudo::Terminal, out)
}

/// Enrollment over an open master: facts, the checks, the payload, the install command line,
/// the pin, the first `hello`, the inventory.
///
/// # Errors
/// One line; nothing is installed before the checks pass.
fn install_through(
    shared: &Shared<'_>,
    req: &Request,
    master: &Master,
    key: &HostKey,
    sudo: Sudo<'_>,
    out: &mut dyn Write,
) -> Result<Host, String> {
    let (sys, paths) = (shared.sys, shared.paths);
    // Read before anything is installed: an inventory that cannot be read would refuse the
    // host only after the agent is on it.
    inventory::load(sys, paths)?;
    let facts = read_facts(sys, master)?;
    // The inventory name, checked here for the same reason: a name another Mac holds would
    // refuse the host only after the agent is on it.
    let host = Host {
        name: req
            .name
            .clone()
            .unwrap_or_else(|| crate::items::names::tag(&facts.local_host_name).to_owned()),
        uuid: facts.uuid.clone(),
        address: req.address.clone(),
        user: req.user.clone(),
        baseline: None,
        users: Vec::new(),
    };
    inventory::check_host(&host).map_err(|why| format!("{why}; pass --name <n>"))?;
    shared.claim(&host)?;
    let _claimed = Claimed {
        shared,
        uuid: host.uuid.clone(),
    };
    let _ = writeln!(out, "found {}", facts.described());
    let payload = shared.payload()?;
    let remote_dir = facts.tmpdir.trim_end_matches('/').to_owned();
    let tag = payload.tag();
    let install = Install {
        archive: &payload.archive,
        remote_archive: format!("{remote_dir}/aterm-agent-{tag}.zip"),
        remote_signers: format!("{remote_dir}/aterm-allowed-signers-{tag}"),
        signers_sha: payload.signers_sha.clone(),
        admin: req.user.clone(),
        fleet_key: payload.fleet_key.clone(),
    };
    let _ = writeln!(out, "copying aterm's helper to this Mac");
    copy_payload(
        sys,
        master,
        &[&payload.archive.path, &payload.signers],
        &format!("{remote_dir}/"),
    )?;
    run_install(sys, master, &install, sudo, req, out)?;
    shared.recorded(|| pin_host_key(sys, paths, &facts.uuid, key))?;
    match first_hello(sys, paths, &host) {
        Ok(hello) => {
            // The fleet key's forced command answered as this Mac: from here on no password.
            let _ = writeln!(out, "checked: the controller reaches it without a password");
            // A state file that is there and cannot be read is left as it is, never replaced
            // by a fresh one: it may hold GitHub key ids.
            match inventory::load_state(sys, paths, &host.uuid) {
                Ok(mut state) => {
                    state.last_seen = sys.now();
                    state.hello = aterm_json::to_string(&hello).ok();
                    state.signers_sha256 = Some(payload.signers_sha.clone());
                    let _ = inventory::save_state(sys, paths, &host.uuid, &state);
                }
                Err(why) => {
                    let _ = writeln!(out, "warning: its state is not updated: {why}");
                }
            }
        }
        Err(why) => {
            let _ = writeln!(
                out,
                "warning: aterm's helper is installed, but the controller's first contact \
                 without a password did not answer: {}",
                shown(&why)
            );
        }
    }
    shared.recorded(|| inventory::upsert_host(sys, paths, &host))?;
    Ok(host)
}

/// The install command line, its one `sudo` fed the password or asking on this terminal.
fn run_install(
    sys: &dyn System,
    master: &Master,
    install: &Install<'_>,
    sudo: Sudo<'_>,
    req: &Request,
    out: &mut dyn Write,
) -> Result<(), String> {
    let left = format!(
        "nothing outside {}/.staging is changed unless `agent install` said so",
        crate::AGENT_ROOT
    );
    let password = match sudo {
        Sudo::Password(password) => password,
        Sudo::Terminal => {
            let _ = writeln!(
                out,
                "sudo asks for {}'s password on {}:",
                req.user, req.address
            );
            return match sys.run_tty(&remote_sh(master, &install_command(install, sudo), true)) {
                Ok(Some(0)) => Ok(()),
                Ok(code) => Err(format!(
                    "the install command line failed (status {code:?}); {left}"
                )),
                Err(e) => Err(format!("ssh -t: {e}")),
            };
        }
    };
    let _ = writeln!(
        out,
        "installing aterm's helper: installing system software takes the admin password, \
         this once; from now on the helper renames and sets up this Mac without asking"
    );
    let ran = sys
        .run_fed(
            &remote_sh(master, &install_command(install, sudo), false).budget(INSTALL_BUDGET),
            password,
        )
        .map_err(|e| format!("the install command line: {e}; {left}"))?;
    for line in ran.out().lines().filter(|l| !l.trim().is_empty()) {
        let _ = writeln!(out, "{}", shown(line));
    }
    if ran.success() {
        return Ok(());
    }
    let err = ran.err();
    // What the root shell said last: on standard error (sudo, a hash that did not match),
    // else on standard output (`agent install`'s refusal).
    let said = |text: &str| {
        text.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned)
    };
    let last = said(&err).or_else(|| said(&ran.out())).unwrap_or_default();
    Err(if err.contains("incorrect password attempt") {
        format!(
            "sudo refused {}'s password, which ssh took: nothing was installed",
            req.user
        )
    } else if err.contains("is not in the sudoers file") || err.contains("not allowed to run") {
        format!(
            "{} is not an administrator on it (sudo: {last}); pass --user <admin>: nothing was \
             installed",
            req.user
        )
    } else {
        let status = ran
            .status
            .map_or_else(|| "killed".to_owned(), |s| format!("exit {s}"));
        let said = if last.is_empty() {
            String::new()
        } else {
            format!(": {last}")
        };
        format!("the install command line failed ({status}{said}); {left}")
    })
}

/// The Macs `scan` sees that `enroll --found` takes, and one line for each row it leaves.
///
/// A row is taken when it is a Mac with Remote Login on that is neither enrolled nor this
/// Mac. A Mac is told from another `_ssh._tcp` host (a Linux box) by what Bonjour shows of it,
/// before any password is sent: macOS's Remote Login advertises `_sftp-ssh._tcp` beside
/// `_ssh._tcp` (`/System/Library/LaunchDaemons/ssh.plist`'s Bonjour list, measured in the
/// VM), and Screen Sharing advertises `_rfb._tcp`; a host advertising Remote Login alone is
/// left. The facts read after login refuse anything but macOS as well. Enrolled rows and this
/// Mac are told by the host key each presents (this Mac's is
/// `/etc/ssh/ssh_host_ed25519_key.pub`), and this Mac by its Local hostname too.
#[must_use]
pub fn found_macs(
    sys: &dyn System,
    paths: &Paths,
    inv: &Inventory,
    browse: &discover::Browse,
) -> (Vec<String>, Vec<String>) {
    let own_key = sys.read_file(Path::new(OWN_HOST_KEY)).ok().and_then(|b| {
        String::from_utf8_lossy(&b)
            .split_whitespace()
            .nth(1)
            .map(str::to_owned)
    });
    let own_local = sys
        .run(&Cmd::new("/usr/sbin/scutil").args(["--get", "LocalHostName"]))
        .ok()
        .filter(Ran::success)
        .map(|ran| format!("{}.local", ran.out().trim()));
    let marks = ssh::fan_out(&browse.found, 16, |row| {
        if !row.ssh {
            return Err("Remote Login is off (it advertises Screen Sharing only)".to_owned());
        }
        if !(row.sftp || row.screen_sharing) {
            return Err(match &browse.sftp_failed {
                Some(why) => format!(
                    "not known to be a Mac: it advertises no Screen Sharing, and the SFTP \
                     browse failed ({why}); name its address to enroll it"
                ),
                None => "not a Mac: it advertises Remote Login alone, not macOS's SFTP or \
                         Screen Sharing"
                    .to_owned(),
            });
        }
        if own_local
            .as_ref()
            .is_some_and(|own| own.eq_ignore_ascii_case(&row.address))
        {
            return Err("this Mac, the controller".to_owned());
        }
        if row.port != 22 {
            return Err(format!(
                "Remote Login on port {}, which enroll does not take",
                row.port
            ));
        }
        let offered = discover::offered_keys(sys, row)?;
        if own_key.as_ref().is_some_and(|own| offered.contains(own)) {
            return Err("this Mac, the controller".to_owned());
        }
        match discover::pinned_name(sys, paths, inv, &offered) {
            Read::Value(name) => Err(format!("enrolled already, as {name}")),
            Read::Absent => Ok(()),
            Read::Unusable(why) => Err(why),
        }
    });
    let mut take = Vec::new();
    let mut notes = Vec::new();
    for (row, mark) in browse.found.iter().zip(marks) {
        match mark {
            Ok(()) => take.push(row.address.clone()),
            Err(why) => notes.push(format!("{}: left: {why}", shown(&row.address))),
        }
    }
    for name in &browse.unresolved {
        notes.push(format!(
            "{}: left: advertised, not located (try again, or name its address)",
            shown(name)
        ));
    }
    (take, notes)
}

/// The refusal for a Mac whose hardware UUID the inventory already holds: the same Mac
/// enrolled again, or a copy of its disk (VM, tart: every clone of an image answers with
/// the image's IOPlatformUUID and serial number), which the fleet could not tell apart.
/// The way on differs: an erased or reinstalled Mac (its UUID survives, its host key and
/// agent do not) can no longer answer as enrolled, so it is retired without contact first; a
/// copy must not take the original's place.
fn already_enrolled(known: &Host, uuid: &str) -> String {
    format!(
        "this Mac's hardware UUID {uuid} is already enrolled as {name} at {address}: the same \
         Mac, or a copy of its disk the fleet cannot tell apart. If it is {name}, erased or \
         reinstalled: `aterm machines unenroll {name} --unreachable`, then enroll it again \
         (its retired block in machines.toml keeps the baseline and users to copy back). If \
         it is a copy, leave {name} enrolled and give the copy its own hardware UUID and \
         serial number",
        name = known.name,
        address = known.address
    )
}

/// The fleet key's first `hello` to a freshly enrolled host: it must answer as `host.uuid`.
fn first_hello(sys: &dyn System, paths: &Paths, host: &Host) -> Result<Hello, String> {
    let ran = ssh::request(
        sys,
        paths,
        &Target::of(host),
        "hello",
        None,
        Duration::from_secs(30),
    )
    .map_err(|e| e.to_string())?;
    if !ran.success() {
        return Err(
            ssh::classify(&ran).map_or_else(|| ran.describe(), |f| f.why_alone(&host.address))
        );
    }
    let hello: Hello = aterm_json::from_str(ran.out().trim()).map_err(|e| format!("hello: {e}"))?;
    if hello.uuid != host.uuid {
        return Err(format!(
            "the forced command answers as {:?}, not {}",
            hello.uuid, host.uuid
        ));
    }
    Ok(hello)
}

/// What `unenroll --unreachable` leaves on a Mac it could not reach.
pub const LEFT_ON_HOST: &[&str] = &[
    "/Library/Management/aterm (the agent and its state)",
    "/etc/sudoers.d/aterm-machines (the one sudoers rule)",
    "the fleet key's line in the admin's ~/.ssh/authorized_keys",
    "each user's ~/.ssh/aterm_github_ed25519 and its ~/.ssh/config block",
];

/// An `uninstall` envelope body for `host`.
#[must_use]
pub fn uninstall_body(host: &Host, serial: u64, now: u64) -> Envelope {
    Envelope {
        schema: crate::SCHEMA,
        host: host.uuid.clone(),
        host_name: host.name.clone(),
        serial,
        issued: now,
        expires: now + 24 * 3600,
        action: Action::Uninstall,
        disrupt: Disrupt::Now,
        idle_secs: 0,
        baseline: None,
        artifacts: Vec::new(),
        exec: None,
        upgrade: None,
        rekey: None,
    }
}

/// The report at the end of an `apply` stream; a refusal becomes the error.
///
/// # Errors
/// The refusal, or that the stream ended without a report.
pub fn report_of(stream: &str) -> Result<HostReport, String> {
    for line in stream.lines().filter(|l| !l.trim().is_empty()) {
        match aterm_json::from_str::<Line>(line) {
            Ok(Line::Report(r)) => return Ok(*r),
            Ok(Line::Refused { why }) => return Err(format!("refused: {why}")),
            Ok(_) | Err(_) => {}
        }
    }
    Err("the stream ended without a report".into())
}

/// `aterm machines unenroll <host> [--unreachable]`: the host's GitHub keys deleted, the
/// `uninstall` envelope (when reachable), then the host retired on the controller.
///
/// # Errors
/// One line; the steps before it stay done.
pub fn unenroll(
    sys: &dyn System,
    paths: &Paths,
    principals: &[String],
    host: &Host,
    unreachable: bool,
    out: &mut dyn Write,
) -> Result<(), String> {
    let inv = inventory::load(sys, paths)?;
    // Refused before anything is touched: an unreadable state file is not one with no
    // GitHub keys, and retiring the host would leave its keys registered for good.
    let mut state = inventory::load_state(sys, paths, &host.uuid).map_err(|why| {
        format!(
            "{}: not unenrolled: its state file holds its GitHub key ids and cannot be read \
             ({why}); fix or remove that file, or delete the keys titled \
             aterm-machines:{}:* on GitHub, then unenroll again",
            host.name, host.name
        )
    })?;
    let ids: Vec<u64> = state.github_keys.iter().map(|(_, id)| *id).collect();
    if let (Some(github), false) = (&inv.github, ids.is_empty()) {
        super::github::revoke(sys, &github.account, &ids)?;
        let _ = writeln!(
            out,
            "deleted {} GitHub key(s) from {}",
            ids.len(),
            github.account
        );
        // Recorded before the host is contacted: an uninstall that fails leaves the host
        // enrolled with no dead ids standing in for its keys.
        state.github_keys.clear();
        state.github_key_text.clear();
        inventory::save_state(sys, paths, &host.uuid, &state)?;
    }
    if unreachable {
        let _ = writeln!(out, "{} was not contacted; left on it:", host.name);
        for left in LEFT_ON_HOST {
            let _ = writeln!(out, "  {left}");
        }
    } else {
        let hello = first_hello(sys, paths, host).map_err(|why| {
            format!(
                "{}: {why}; a Mac that cannot answer any more (erased, reinstalled, lost) is \
                 retired with `aterm machines unenroll {} --unreachable`",
                host.name, host.name
            )
        })?;
        let floor = hello
            .floors
            .iter()
            .filter(|f| principals.contains(&f.principal))
            .map(|f| f.serial)
            .max()
            .unwrap_or(0);
        let serial = inventory::next_serial(sys, paths, floor)?;
        let wire = envelope::seal(
            sys,
            &uninstall_body(host, serial, sys.now()),
            &paths.signing_key(),
        )?;
        let staged = crate::port::staging_file(sys, &wire).map_err(|e| e.to_string())?;
        let ran: Ran = ssh::request(
            sys,
            paths,
            &Target::of(host),
            "apply",
            Some(staged.path()),
            Duration::from_secs(600),
        )
        .map_err(|e| e.to_string())?;
        let report = report_of(&ran.out())?;
        // What was removed, and what could not be; a step with nothing to remove is `ok`
        // and says nothing.
        for row in report.items.iter().filter(|r| r.outcome != Outcome::Ok) {
            let _ = writeln!(out, "  {}", super::commands::row_line(row));
        }
        if report.failed() {
            return Err(format!(
                "{}: the agent could not remove everything; the host stays enrolled",
                host.name
            ));
        }
    }
    unpin_host_key(sys, paths, &host.uuid)?;
    inventory::retire_host(sys, paths, &host.uuid)?;
    let _ = writeln!(out, "retired {} ({})", host.name, host.uuid);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;

    const VM_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
    const VM_FP: &str = "SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q";

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn enroll_takes_one_address_and_defaults_the_admin_and_how_the_key_is_trusted() {
        let sys = Fake::root();
        sys.on("/usr/bin/id -urn", Ran::ok("owner\n"));
        let r = parse_args(
            &sys,
            &args(&[
                "lab-07.local",
                "--user",
                "labadmin",
                "--fingerprint",
                "0Tdy7Goos0XG",
            ]),
        )
        .unwrap();
        assert_eq!((r.user.as_str(), r.user_given), ("labadmin", true));
        assert_eq!(r.trust, Trust::Fingerprint("SHA256:0Tdy7Goos0XG".into()));
        assert!(sys.calls().is_empty(), "a --user is never looked up");
        // The plain form: this Mac's login name, and the host key trusted on first use.
        let r = parse_args(&sys, &args(&["lab-07.local"])).unwrap();
        assert_eq!((r.user.as_str(), r.user_given), ("owner", false));
        assert_eq!(r.trust, Trust::FirstUse);
        assert_eq!(r.requests().len(), 1);
        // A flag without its value is refused, never read as left out.
        for flag in ["--fingerprint", "--user", "--name", "--parallel"] {
            let err = parse_args(&sys, &args(&["lab-07.local", flag])).unwrap_err();
            assert!(err.contains("takes a value"), "{err}");
        }
        assert!(parse_args(&sys, &args(&["-oProxyCommand=x", "--user", "a"])).is_err());
        assert!(parse_args(&sys, &args(&["h", "--user", "a b"])).is_err());
        assert!(parse_args(&sys, &args(&["--found", "--user", "-x"])).is_err());
        // No login name to fall back on: refused, naming the flag that sets one.
        let nameless = Fake::root();
        nameless.on("/usr/bin/id -urn", Ran::exit(1, "id: 501: no such user\n"));
        let err = parse_args(&nameless, &args(&["lab-07.local"])).unwrap_err();
        assert!(err.contains("--user <admin>"), "{err}");
    }

    #[test]
    fn several_macs_share_the_admin_and_refuse_what_describes_one_mac() {
        let sys = Fake::root();
        sys.on("/usr/bin/id -urn", Ran::ok("owner\n"));
        let r = parse_args(
            &sys,
            &args(&["m1.local", "m2.local", "M1.LOCAL", "--parallel", "4"]),
        )
        .unwrap();
        assert_eq!(r.addresses, ["m1.local", "m2.local"], "each address once");
        assert_eq!(r.parallel, Some(4));
        assert!(r.requests().iter().all(|q| q.user == "owner"));
        let found = parse_args(&sys, &args(&["--found"])).unwrap();
        assert!(found.found && found.addresses.is_empty());
        for words in [
            &["m1.local", "m2.local", "--fingerprint", "SHA256:x"][..],
            &["--found", "--fingerprint", "SHA256:x"],
            &["m1.local", "m2.local", "--name", "lab-01"],
            &["--found", "--name", "lab-01"],
        ] {
            let err = parse_args(&sys, &args(words)).unwrap_err();
            assert!(err.contains("one Mac"), "{words:?}: {err}");
        }
        assert!(
            parse_args(&sys, &args(&[]))
                .unwrap_err()
                .contains("--found")
        );
        assert!(parse_args(&sys, &args(&["m1.local", "--parallel", "0"])).is_err());
    }

    #[test]
    fn the_scanned_key_must_match_the_fingerprint_read_at_the_mac() {
        // `ssh-keyscan -t ed25519 127.0.0.1` in the VM (the comment line is stderr's).
        let out = format!("127.0.0.1 ssh-ed25519 {VM_KEY}\n");
        let (kind, blob) = parse_keyscan(&out).unwrap();
        assert_eq!((kind.as_str(), blob.as_str()), ("ssh-ed25519", VM_KEY));
        assert_eq!(parse_keyscan("# 127.0.0.1:22 SSH-2.0-OpenSSH_10.3\n"), None);
        // `ssh-keygen -lf -` on it, in the VM.
        assert_eq!(
            parse_fingerprint(
                "256 SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q 127.0.0.1 (ED25519)\n"
            )
            .as_deref(),
            Some(VM_FP)
        );
        let key = HostKey {
            kind,
            blob,
            fingerprint: VM_FP.into(),
        };
        check_host_key(&key, &Trust::Fingerprint(VM_FP.into())).unwrap();
        check_host_key(&key, &Trust::FirstUse).unwrap();
        let err = check_host_key(&key, &Trust::Fingerprint("SHA256:other".into())).unwrap_err();
        assert!(err.contains("Nothing was sent"), "{err}");
    }

    #[test]
    fn scanning_reads_the_key_and_fingerprints_it_from_a_file() {
        let fake = Fake::root();
        fake.on(
            "/usr/bin/ssh-keyscan -T 10 -t ed25519 -- 192.168.64.2",
            Ran::ok(&format!("192.168.64.2 ssh-ed25519 {VM_KEY}\n")),
        );
        fake.on(
            "/usr/bin/ssh-keygen -lf -",
            Ran::ok(&format!("256 {VM_FP} 192.168.64.2 (ED25519)\n")),
        );
        let key = scan_host_key(&fake, "192.168.64.2", Path::new("/s")).unwrap();
        assert_eq!(key.fingerprint, VM_FP);
        assert!(fake.written("/s/hostkey.pub").is_none());
    }

    #[test]
    fn what_ssh_keyscan_printed_tells_no_route_nothing_there_and_no_remote_login_apart() {
        // What ssh-keyscan printed in the VM, each exit 1.
        let said = |err: &str| NoHostKey::of(&Ran::exit(1, err));
        assert_eq!(
            said("connect (`0.0.0.1'): No route to host\n"),
            NoHostKey::NoRoute
        );
        // An unused address on the subnet, once its ARP failed: no Mac to turn Remote
        // Login on at.
        assert_eq!(
            said("connect (`192.168.64.252'): Host is down\n"),
            NoHostKey::Down
        );
        assert_eq!(said(""), NoHostKey::Silent);
        // Remote Login off with the firewall off, as macOS ships: refused at once.
        assert_eq!(
            said("write (192.168.64.17): Broken pipe\n"),
            NoHostKey::Closed
        );
        assert_eq!(
            said("connect (`192.168.64.17'): Connection refused\n"),
            NoHostKey::Closed
        );
        assert!(matches!(
            said("read (192.168.64.17): Connection reset by peer\n"),
            NoHostKey::Answered(_)
        ));
        // No route to a LAN address names Local Network privacy; the others do not, and
        // only a Mac that may be there is asked about Remote Login.
        assert!(
            NoHostKey::NoRoute
                .why("192.168.64.2")
                .contains(ssh::LOCAL_NETWORK_ROW)
        );
        for (class, remote_login) in [
            (NoHostKey::Down, false),
            (NoHostKey::Closed, true),
            (NoHostKey::Silent, true),
            (NoHostKey::Answered("exit 1".into()), true),
        ] {
            let why = class.why("192.168.64.3");
            assert!(!why.contains("Local Network"), "{why}");
            assert_eq!(why.contains("Remote Login"), remote_login, "{why}");
        }
        // The line follows the Mac's label, so it does not name the address itself, even
        // where ssh-keyscan did.
        for err in [
            "connect (`192.168.64.3'): No route to host\n",
            "connect (`192.168.64.3'): Host is down\n",
            "write (192.168.64.3): Broken pipe\n",
            "",
            "read (192.168.64.3): Connection reset by peer\n",
            "connect (`192.168.64.3'): Operation timed out\n",
        ] {
            let why = said(err).why("192.168.64.3");
            assert!(!why.contains("192.168.64.3"), "{err:?}: {why}");
        }
        // scan_host_key answers with that line.
        let fake = Fake::root();
        fake.on(
            "/usr/bin/ssh-keyscan -T 10 -t ed25519 -- 192.168.64.4",
            Ran::exit(1, "connect (`192.168.64.4'): Host is down\n"),
        );
        let err = scan_host_key(&fake, "192.168.64.4", Path::new("/s")).unwrap_err();
        assert_eq!(err, NoHostKey::Down.why("192.168.64.4"));
    }

    #[test]
    fn pinning_replaces_only_the_aliases_own_line() {
        let key = HostKey {
            kind: "ssh-ed25519".into(),
            blob: VM_KEY.into(),
            fingerprint: VM_FP.into(),
        };
        let uuid = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";
        let text = known_hosts_with(
            "OTHER ssh-ed25519 AAAA\nC3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 ssh-ed25519 OLD\n",
            uuid,
            Some(&key),
        );
        assert_eq!(
            text,
            format!("OTHER ssh-ed25519 AAAA\n{uuid} ssh-ed25519 {VM_KEY}\n")
        );
        assert_eq!(
            known_hosts_with(&text, uuid, None),
            "OTHER ssh-ed25519 AAAA\n"
        );
    }

    /// FACTS_SCRIPT's output in the macOS 27.0 VM (ioreg cut to its identity rows).
    fn vm_facts() -> String {
        [
            "+-o VMA2MACOSAP  <class IOPlatformExpertDevice>\n    {\n      \"IOPlatformSerialNumber\" = \"ZNYVNTRJN5\"\n      \"IOPlatformUUID\" = \"C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43\"\n    }\n",
            "VirtualMac2,1\n",
            "arm64\n",
            "27.0\n",
            "26A428\n",
            "Managed’s Virtual Machine\n",
            "Manageds-Virtual-Machine\n",
            "/var/folders/yt/jkgjjlb55fvbjsx8t5xw72280000gn/T/\n",
            "Apple M5 (Virtual)\n",
            "+-o product  <class IOPlatformDevice>\n    {\n      \"product-name\" = <\"Apple Virtual Machine 1\">\n    }\n",
        ]
        .join("--aterm--\n")
    }

    #[test]
    fn the_hosts_facts_are_read_section_by_section() {
        let f = parse_facts(&vm_facts()).unwrap();
        assert_eq!(f.uuid, "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43");
        assert_eq!(f.serial, "ZNYVNTRJN5");
        assert_eq!(f.model, "VirtualMac2,1");
        assert_eq!(f.computer_name, "Managed’s Virtual Machine");
        assert_eq!(
            f.tmpdir,
            "/var/folders/yt/jkgjjlb55fvbjsx8t5xw72280000gn/T/"
        );
        assert!(parse_facts("nothing").is_err());
        assert!(parse_facts(&vm_facts().replace("IOPlatformUUID", "Other")).is_err());
        // The Mac as the owner knows it, never `VirtualMac2,1 arm64` or a build number.
        assert_eq!(
            f.described(),
            "\"Managed’s Virtual Machine\": Virtual Machine, Apple M5 (Virtual), macOS 27.0, \
             serial ZNYVNTRJN5"
        );
        // An Apple silicon Mac by its product name; an Intel Mac, which has none, by its
        // model identifier's family.
        let air = Facts {
            model: "Mac16,12".into(),
            product_name: "MacBook Air (15-inch, M4, 2025)".into(),
            chip: "Apple M4\n".into(),
            ..f.clone()
        };
        assert!(
            air.described()
                .contains(": MacBook Air, Apple M4, macOS 27.0"),
            "{}",
            air.described()
        );
        let intel = Facts {
            model: "MacBookPro14,3".into(),
            product_name: String::new(),
            chip: "Intel(R) Core(TM) i7-7920HQ CPU @ 3.10GHz\n".into(),
            ..f
        };
        assert!(
            intel.described().contains(": MacBook Pro, Intel Core i7,"),
            "{}",
            intel.described()
        );
    }

    #[test]
    fn a_bundle_travels_whole_and_a_dev_binary_alone() {
        let (source, inner, bundle) =
            payload_of(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
        assert_eq!(source, Path::new("/Applications/aterm.app"));
        assert_eq!(inner, Path::new("aterm.app/Contents/MacOS/aterm"));
        assert!(bundle);
        let (source, inner, bundle) = payload_of(Path::new("/w/target/debug/aterm-machines-dev"));
        assert_eq!(source, Path::new("/w/target/debug/aterm-machines-dev"));
        assert_eq!(inner, Path::new("aterm-machines-dev"));
        assert!(!bundle);
    }

    #[test]
    fn only_a_bundle_is_archived_with_its_parent_kept() {
        let fake = Fake::root();
        fake.on(
            "/usr/bin/ditto -c -k --norsrc --noextattr --noqtn --noacl /w/aterm-machines-dev /s/aterm-agent.zip",
            Ran::ok(""),
        );
        fake.file("/s/aterm-agent.zip", b"zip");
        let a = build_archive(&fake, Path::new("/w/aterm-machines-dev"), Path::new("/s")).unwrap();
        assert_eq!(a.exe, Path::new("aterm-machines-dev"));
        let fake = Fake::root();
        fake.on(
            "/usr/bin/ditto -c -k --norsrc --noextattr --noqtn --noacl --keepParent /Applications/aterm.app /s/aterm-agent.zip",
            Ran::ok(""),
        );
        fake.file("/s/aterm-agent.zip", b"zip");
        let a = build_archive(
            &fake,
            Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
            Path::new("/s"),
        )
        .unwrap();
        assert!(a.bundle);
    }

    fn archive(bundle: bool) -> Archive {
        Archive {
            path: "/s/aterm-agent.zip".into(),
            sha256: "ab".repeat(32),
            exe: if bundle {
                "aterm.app/Contents/MacOS/aterm".into()
            } else {
                "aterm-machines-dev".into()
            },
            bundle,
        }
    }

    fn install_of(a: &Archive) -> Install<'_> {
        Install {
            archive: a,
            remote_archive: "/var/folders/yt/x/T/aterm-agent-abababab.zip".into(),
            remote_signers: "/var/folders/yt/x/T/aterm-allowed-signers-abababab".into(),
            signers_sha: "cd".repeat(32),
            admin: "admin".into(),
            fleet_key: "ssh-ed25519 AAAA fleet@ctl".into(),
        }
    }

    #[test]
    fn the_install_line_hashes_the_root_owned_copies_before_it_executes_anything() {
        let a = archive(true);
        let s = root_script(&install_of(&a));
        let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("{needle}\n{s}"));
        let copy = at(
            "/bin/cp /var/folders/yt/x/T/aterm-agent-abababab.zip /Library/Management/aterm/.staging/payload.zip",
        );
        let hash = at("/usr/bin/shasum -a 256 /Library/Management/aterm/.staging/payload.zip");
        let expand = at("/usr/bin/ditto -x -k");
        let sign = at(
            "/usr/bin/codesign --verify --strict /Library/Management/aterm/.staging/payload/aterm.app",
        );
        let run = at(
            "/Library/Management/aterm/.staging/payload/aterm.app/Contents/MacOS/aterm machines agent install --admin admin --fleet-key 'ssh-ed25519 AAAA fleet@ctl'",
        );
        assert!(
            copy < hash && hash < expand && expand < sign && sign < run,
            "{s}"
        );
        // The user-writable copy is never what runs.
        assert!(
            !s.lines()
                .any(|l| l.starts_with("/var/folders") || l.starts_with("'/var")),
            "{s}"
        );
        let dev = root_script(&Install {
            remote_archive: "/t/a.zip".into(),
            remote_signers: "/t/s".into(),
            ..install_of(&archive(false))
        });
        assert!(!dev.contains("codesign"), "{dev}");
        assert!(dev.contains(
            "\n/Library/Management/aterm/.staging/payload/aterm-machines-dev machines agent install"
        ));
    }

    #[test]
    fn the_root_shell_drops_the_channel_before_it_reads_anything() {
        let a = archive(true);
        let s = root_script(&install_of(&a));
        // Under a NOPASSWD rule `sudo -S` reads nothing, and a root `cat` read the password
        // the channel still held (VM): the root shell's first act is to let go of it.
        assert!(s.starts_with("exec </dev/null\nset -e\n"), "{s}");
        let secret = Secret::new(b"x").unwrap();
        let fed = install_command(&install_of(&a), Sudo::Password(&secret));
        assert!(
            fed.starts_with("/usr/bin/sudo -S -p '' -- /bin/sh -c 'exec </dev/null"),
            "{fed}"
        );
        // One sudo for the whole root half; the admin's copies go whatever it answered.
        assert_eq!(fed.matches("/usr/bin/sudo").count(), 1, "{fed}");
        assert!(fed.ends_with(
            "status=$?\n/bin/rm -f /var/folders/yt/x/T/aterm-agent-abababab.zip \
             /var/folders/yt/x/T/aterm-allowed-signers-abababab\nexit $status\n"
        ));
        let tty = install_command(&install_of(&a), Sudo::Terminal);
        assert!(tty.starts_with("/usr/bin/sudo -- /bin/sh -c "), "{tty}");
    }

    #[test]
    fn the_master_trusts_only_the_vouched_key_and_takes_one_password_by_askpass() {
        let m = Master {
            ctl: "/c/ctl/0123abcd".into(),
            destination: "admin@192.168.64.2".into(),
        };
        let cmd = master_cmd(
            &m,
            Path::new("/s/known_hosts"),
            "aterm-machines-enroll",
            Path::new("/b/aterm-machines-askpass"),
            PASSWORD_METHOD,
        );
        let line = cmd.display();
        for want in [
            "SSH_ASKPASS=/b/aterm-machines-askpass SSH_ASKPASS_REQUIRE=force /usr/bin/ssh",
            "-F /dev/null",
            "-M -S /c/ctl/0123abcd",
            "ControlPersist=120",
            "StrictHostKeyChecking=yes",
            "UserKnownHostsFile=/s/known_hosts",
            "HostKeyAlias=aterm-machines-enroll",
            "PubkeyAuthentication=no",
            "PreferredAuthentications=password -o NumberOfPasswordPrompts=1",
            "-- admin@192.168.64.2 true",
        ] {
            assert!(line.contains(want), "{want}: {line}");
        }
        // The helper's path and the mode are all the environment carries: never a password.
        assert_eq!(cmd.env.len(), 2);
        let remote = remote_sh(&m, "echo ===", false).display();
        assert!(
            remote.ends_with(r"-- admin@192.168.64.2 '/bin/sh -c '\''echo ==='\'''"),
            "{remote}"
        );
        assert!(remote.contains("ControlMaster=no") && remote.contains("BatchMode=yes"));
        // A client whose master is gone connects by itself: the install line's carries the
        // password, so that connection trusts no host key and stops before any login. So
        // does the copy's.
        let copy = copy_cmd(&m, &[Path::new("/s/a.zip")], "/t/").display();
        for line in [&remote, &copy] {
            for want in [
                "-o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null -o \
                 StrictHostKeyChecking=yes",
                "PubkeyAuthentication=no",
            ] {
                assert!(line.contains(want), "{want}: {line}");
            }
        }
    }

    #[test]
    fn a_refusal_names_the_methods_the_mac_offers() {
        assert_eq!(
            denied_methods(DENIED),
            Some("publickey,password,keyboard-interactive")
        );
        assert_eq!(
            denied_methods("ssh: connect to host x port 22: No route to host"),
            None
        );
    }

    const ROOT: &str = "/ctl/machines";
    const VM_UUID: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";

    fn ctl() -> Paths {
        Paths {
            root: PathBuf::from(ROOT),
        }
    }

    fn master() -> Master {
        Master {
            ctl: "/c/ctl/0123abcd".into(),
            destination: "admin@192.168.64.4".into(),
        }
    }

    fn request(name: Option<&str>) -> Request {
        Request {
            address: "192.168.64.4".into(),
            user: "admin".into(),
            name: name.map(str::to_owned),
            trust: Trust::FirstUse,
        }
    }

    fn vm_key() -> HostKey {
        HostKey {
            kind: "ssh-ed25519".into(),
            blob: VM_KEY.into(),
            fingerprint: VM_FP.into(),
        }
    }

    /// A controller whose inventory holds `lab-03` with `uuid`, and a host whose facts are
    /// the VM's (LocalHostName `Manageds-Virtual-Machine`).
    fn enrolling(holder: &str, uuid: &str) -> Fake {
        let sys = Fake::root();
        sys.file(
            &format!("{ROOT}/machines.toml"),
            format!(
                "schema = 1\n[[host]]\nname = \"{holder}\"\nuuid = \"{uuid}\"\naddress = \
                 \"192.168.64.3\"\nuser = \"admin\"\n"
            )
            .as_bytes(),
        );
        sys.on(
            &remote_sh(&master(), FACTS_SCRIPT, false).display(),
            Ran::ok(&vm_facts()),
        );
        sys
    }

    /// Whether anything was copied to or installed on the host.
    fn installed_anything(sys: &Fake) -> bool {
        sys.calls().iter().any(|c| {
            c.contains("ditto")
                || c.contains("scp")
                || c.starts_with("tty ")
                || (c.starts_with("fed ") && c.contains("agent install"))
        })
    }

    // ── one password for many Macs ──────────────────────────────────────────────────────

    /// What `ssh` printed for a wrong password in the VM (exit 255).
    const DENIED: &str =
        "admin@192.168.64.5: Permission denied (publickey,password,keyboard-interactive).\n";
    /// What `sudo -S -p ''` printed for a wrong password in the VM (exit 1).
    const SUDO_WRONG: &str =
        "Sorry, try again.\n\nsudo: no password was provided\nsudo: 1 incorrect password attempt\n";
    const RIGHT: &str = "hunter2-right";
    const TYPO: &str = "hunter2-typo";
    const EXE: &str = "/w/aterm-machines-dev";
    const FLEET: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao aterm-machines-fleet@ctl";
    const TMP: &str = "/var/folders/yt/jkgjjlb55fvbjsx8t5xw72280000gn/T";
    const UUID_A: &str = "0A1B2C3D-0000-4000-8000-00000000000A";
    const UUID_B: &str = "0A1B2C3D-0000-4000-8000-00000000000B";

    /// A controller `init` set up, nothing enrolled: its inventory, signers and fleet key,
    /// and the archive `ditto` makes in the command's scratch.
    fn set_up() -> Fake {
        let sys = Fake::root();
        sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
        sys.file(
            &format!("{ROOT}/allowed_signers"),
            format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {VM_KEY}\n").as_bytes(),
        );
        sys.file(
            &format!("{ROOT}/keys/ssh_ed25519.pub"),
            format!("{FLEET}\n").as_bytes(),
        );
        let zip = Fake::scratch_path(None, "enroll", 0).join("aterm-agent.zip");
        let ditto = format!(
            "/usr/bin/ditto -c -k --norsrc --noextattr --noqtn --noacl {EXE} {}",
            zip.display()
        );
        sys.on(&ditto, Ran::ok(""));
        sys.on_write(&ditto, &zip, b"the agent");
        sys
    }

    /// The payload the command builds, as a controller set up alike builds it.
    fn payload() -> Payload {
        let sys = set_up();
        build_payload(
            &sys,
            &ctl(),
            Path::new(EXE),
            &Fake::scratch_path(None, "enroll", 0),
        )
        .unwrap()
    }

    fn master_of(address: &str) -> Master {
        master_as("admin", address)
    }

    fn master_as(user: &str, address: &str) -> Master {
        Master {
            ctl: ctl().control_path(address),
            destination: format!("{user}@{address}"),
        }
    }

    /// The key a login to `address` through the scratch numbered `seq` is scripted under.
    fn login_key(address: &str, seq: u64, method: &str) -> String {
        login_key_as("admin", address, seq, method)
    }

    /// [`login_key`] for a login as `user`.
    fn login_key_as(user: &str, address: &str, seq: u64, method: &str) -> String {
        let known = Fake::scratch_path(None, "enroll", seq).join("known_hosts");
        let askpass = Fake::scratch_path(None, "enroll", 0).join(crate::askpass::ALIAS);
        format!(
            "fed {}",
            master_cmd(
                &master_as(user, address),
                &known,
                "aterm-machines-enroll",
                &askpass,
                method
            )
            .display()
        )
    }

    /// The key the install command line on `address` is scripted under.
    fn install_key(address: &str) -> String {
        let p = payload();
        let tag = p.tag().to_owned();
        let install = Install {
            archive: &p.archive,
            remote_archive: format!("{TMP}/aterm-agent-{tag}.zip"),
            remote_signers: format!("{TMP}/aterm-allowed-signers-{tag}"),
            signers_sha: p.signers_sha.clone(),
            admin: "admin".into(),
            fleet_key: p.fleet_key.clone(),
        };
        let secret = Secret::new(b"x").unwrap();
        format!(
            "fed {}",
            remote_sh(
                &master_of(address),
                &install_command(&install, Sudo::Password(&secret)),
                false
            )
            .display()
        )
    }

    /// The Mac at `address`: its key scan, its facts (hardware UUID `uuid`, Local hostname
    /// `local`), the copy and the install line answering `install`. Its logins are scripted
    /// by the test, one per attempt ([`login_key`]).
    fn a_mac(sys: &Fake, address: &str, uuid: &str, local: &str, install: Ran) {
        sys.on(
            &format!("/usr/bin/ssh-keyscan -T 10 -t ed25519 -- {address}"),
            Ran::ok(&format!("{address} ssh-ed25519 {VM_KEY}\n")),
        );
        sys.on(
            "/usr/bin/ssh-keygen -lf -",
            Ran::ok(&format!("256 {VM_FP} {address} (ED25519)\n")),
        );
        let master = master_of(address);
        sys.on(
            &remote_sh(&master, FACTS_SCRIPT, false).display(),
            Ran::ok(
                &vm_facts()
                    .replace(VM_UUID, uuid)
                    .replace("Manageds-Virtual-Machine\n", &format!("{local}\n")),
            ),
        );
        let p = payload();
        sys.on(
            &copy_cmd(&master, &[&p.archive.path, &p.signers], &format!("{TMP}/")).display(),
            Ran::ok(""),
        );
        sys.on(&install_key(address), install);
    }

    fn batch(addresses: &[&str], user_given: bool) -> Args {
        Args {
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
            found: false,
            user: "admin".into(),
            user_given,
            user_recorded: false,
            name: None,
            trust: Trust::FirstUse,
            parallel: Some(1),
        }
    }

    /// Run `enroll` of `args` on `sys`, one Mac at a time: the summary and what it printed.
    fn run_batch(sys: &Fake, args: &Args) -> (Result<Summary, String>, String) {
        let mut out: Vec<u8> = Vec::new();
        let sink: Sink<'_> = Mutex::new(&mut out as &mut (dyn Write + Send));
        let done = enroll_macs(
            sys,
            &ctl(),
            args,
            &args.requests(),
            Path::new(EXE),
            1,
            &sink,
        );
        drop(sink);
        (done, String::from_utf8(out).unwrap())
    }

    /// The password reached each Mac only as the input of a fed run: never an argument, a
    /// variable, a line printed or a file written.
    fn never_shown(sys: &Fake, out: &str, password: &str) {
        for call in sys.calls() {
            assert!(!call.contains(password), "{call}");
        }
        assert!(!out.contains(password), "{out}");
        for path in [
            format!("{ROOT}/machines.toml"),
            format!("{ROOT}/known_hosts"),
            format!("{ROOT}/state/{UUID_A}.json"),
        ] {
            let bytes = sys.written(&path).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(password),
                "{path}"
            );
        }
    }

    #[test]
    fn one_password_typed_once_logs_in_and_sudoes_on_every_mac() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(
            &sys,
            "lab-02.local",
            UUID_A,
            "lab-02",
            Ran::ok("installed\n"),
        );
        a_mac(
            &sys,
            "lab-04.local",
            UUID_B,
            "lab-04",
            Ran::ok("installed\n"),
        );
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
        let summary = done.unwrap();
        let names: Vec<&str> = summary
            .enrolled
            .iter()
            .map(|(_, h)| h.name.as_str())
            .collect();
        assert_eq!(names, ["lab-02", "lab-04"], "{out}");
        assert!(summary.skipped.is_empty());
        assert!(
            summary
                .text()
                .starts_with("enrolled 2 (lab-02, lab-04)\nNext:"),
            "{}",
            summary.text()
        );
        // Asked once, for both, after each Mac's host key was read (its fingerprint shown)
        // and before anything was sent to any.
        let calls = sys.calls();
        let asks: Vec<&String> = calls.iter().filter(|c| c.starts_with("ask")).collect();
        assert_eq!(
            asks,
            [
                "ask secret Password for admin on the 2 Macs (typed once, used for each, never \
                 saved): "
            ]
        );
        let asked = calls.iter().position(|c| c.starts_with("ask")).unwrap();
        let last_scan = calls
            .iter()
            .rposition(|c| c.contains("ssh-keyscan"))
            .unwrap();
        let first_fed = calls.iter().position(|c| c.starts_with("fed ")).unwrap();
        assert!(last_scan < asked && asked < first_fed, "{calls:?}");
        // Each Mac's login and its sudo were fed that password, and nothing else was.
        let fed = sys.fed();
        assert_eq!(fed.len(), 4, "{fed:?}");
        assert!(fed.iter().all(|(_, bytes)| bytes == RIGHT.as_bytes()));
        never_shown(&sys, &out, RIGHT);
        // Each Mac's lines carry its name, and the helper ssh runs is this aterm.
        assert!(
            out.contains("lab-02: first connection to this Mac (its ID: SHA256:"),
            "{out}"
        );
        assert!(out.contains("lab-04: enrolled as lab-04"), "{out}");
        assert!(calls.contains(&format!(
            "symlink {} -> {EXE}",
            Fake::scratch_path(None, "enroll", 0)
                .join(crate::askpass::ALIAS)
                .display()
        )));
        let inv = inventory::load(&sys, &ctl()).unwrap();
        assert_eq!(inv.hosts.len(), 2);
    }

    #[test]
    fn a_mac_that_refuses_the_password_is_asked_for_once_more_by_name() {
        let sys = set_up();
        // The first password, then (lab-04 refused it) the admin's name kept, and the right
        // password for lab-04 alone.
        sys.answer(Some(TYPO)).answer(None).answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        // lab-04's two logins, over the one key its scan trusted.
        sys.on(
            &login_key("lab-04.local", 2, PASSWORD_METHOD),
            Ran::exit(255, DENIED),
        );
        sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], false));
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 2, "{out}");
        let asks: Vec<String> = sys
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("ask"))
            .collect();
        assert_eq!(
            asks,
            [
                "ask secret Admin password on the 2 Macs (typed once, used for each, never \
                 saved): "
                    .to_owned(),
                "ask What is the admin account called on the Macs? (Enter keeps admin): "
                    .to_owned(),
                "ask secret lab-04 did not take the password for admin. Type it again (Enter \
                 skips it): "
                    .to_owned(),
            ]
        );
        assert!(
            out.contains(
                "lab-04: no login as admin with that password (a wrong password, or no account \
                 called admin there)"
            ),
            "{out}"
        );
        let fed: Vec<(String, Vec<u8>)> = sys.fed();
        let lab04: Vec<&[u8]> = fed
            .iter()
            .filter(|(k, _)| k.contains("lab-04.local"))
            .map(|(_, b)| b.as_slice())
            .collect();
        assert_eq!(
            lab04,
            [TYPO.as_bytes(), RIGHT.as_bytes(), RIGHT.as_bytes()],
            "the typo, then the retyped password to its login and its sudo"
        );
        never_shown(&sys, &out, TYPO);
        never_shown(&sys, &out, RIGHT);
    }

    #[test]
    fn a_retyped_password_that_works_is_tried_on_the_others_before_each_is_asked() {
        let sys = set_up();
        sys.answer(Some(TYPO)).answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        for (address, seq) in [("lab-02.local", 1), ("lab-04.local", 2)] {
            sys.on(
                &login_key(address, seq, PASSWORD_METHOD),
                Ran::exit(255, DENIED),
            );
            sys.on(&login_key(address, seq, PASSWORD_METHOD), Ran::ok(""));
        }
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
        assert_eq!(done.unwrap().enrolled.len(), 2, "{out}");
        let asks = sys.calls().iter().filter(|c| c.starts_with("ask")).count();
        assert_eq!(asks, 2, "a typo costs one more prompt, not one per Mac");
    }

    #[test]
    fn a_mac_that_refuses_every_password_typed_is_skipped_with_one_line_and_the_rest_go_on() {
        let sys = set_up();
        sys.answer(Some(RIGHT)).answer(Some(TYPO));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        // Both of lab-02's logins are refused.
        sys.on(
            &login_key("lab-02.local", 1, PASSWORD_METHOD),
            Ran::exit(255, DENIED),
        );
        sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 1, "{out}");
        assert_eq!(summary.skipped.len(), 1);
        let text = summary.text();
        assert!(
            text.starts_with(
                "enrolled 1 (lab-04); not enrolled: 1\n  lab-02: no login as admin with the \
                 password typed"
            ) && text.contains("; run the same enroll command again to retry it\nNext:"),
            "{text}"
        );
        // Said when it happened, not only at the end.
        assert!(
            out.contains("lab-02: did not take that password either"),
            "{out}"
        );
        // Nothing was copied to it.
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("scp") && c.contains("lab-02.local"))
        );
    }

    /// The Macs' logins as `user` in order: one reply per attempt, each Mac through the
    /// scratch its scan made (`lab-02.local` 1, `lab-04.local` 2).
    fn logins(sys: &Fake, user: &str, address: &str, replies: &[bool]) {
        let seq = if address == "lab-02.local" { 1 } else { 2 };
        for &ok in replies {
            let reply = if ok {
                Ran::ok("")
            } else {
                Ran::exit(255, DENIED)
            };
            sys.on(&login_key_as(user, address, seq, PASSWORD_METHOD), reply);
        }
    }

    fn asks(sys: &Fake) -> Vec<String> {
        sys.calls()
            .into_iter()
            .filter(|c| c.starts_with("ask"))
            .collect()
    }

    #[test]
    fn a_lab_admin_who_is_not_this_macs_login_types_the_password_once_and_the_name_once() {
        // The rehearsal (VMs, macOS 27.0): the controller's login is `owner`, every lab Mac's
        // admin is `admin`, no --user. Both Macs refuse `owner`; the name typed, the password
        // already typed is tried under it before anything else is asked.
        let sys = set_up();
        sys.answer(Some(RIGHT)).answer(Some("admin"));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        for address in ["lab-02.local", "lab-04.local"] {
            logins(&sys, "owner", address, &[false]);
            logins(&sys, "admin", address, &[true]);
        }
        let mut args = batch(&["lab-02.local", "lab-04.local"], false);
        args.user = "owner".into();
        let (done, out) = run_batch(&sys, &args);
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 2, "{out}");
        assert!(summary.enrolled.iter().all(|(_, h)| h.user == "admin"));
        assert_eq!(
            asks(&sys),
            [
                "ask secret Admin password on the 2 Macs (typed once, used for each, never \
                 saved): ",
                "ask What is the admin account called on the Macs? (Enter keeps owner): ",
            ],
            "{out}"
        );
        assert!(
            out.contains("2 Macs (lab-02, lab-04): no login as owner with that password"),
            "{out}"
        );
        // The one password went to every login and both sudos.
        assert!(sys.fed().iter().all(|(_, bytes)| bytes == RIGHT.as_bytes()));
        never_shown(&sys, &out, RIGHT);
    }

    #[test]
    fn a_typo_at_one_macs_prompt_costs_it_nothing_when_the_next_password_typed_is_right() {
        // The rehearsal: the first password and lab-02's retype mistyped, lab-04's retype
        // right. lab-02 takes lab-04's password, so it is enrolled too.
        let sys = set_up();
        sys.answer(Some(TYPO))
            .answer(Some(TYPO))
            .answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        logins(&sys, "admin", "lab-02.local", &[false, false, true]);
        logins(&sys, "admin", "lab-04.local", &[false, true]);
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 2, "{out}\n{summary:?}");
        assert_eq!(asks(&sys).len(), 3, "{:?}", asks(&sys));
        assert!(
            out.contains("lab-02: did not take that password either"),
            "{out}"
        );
        let lab02: Vec<Vec<u8>> = sys
            .fed()
            .into_iter()
            .filter(|(k, _)| k.contains("lab-02.local"))
            .map(|(_, b)| b)
            .collect();
        assert_eq!(
            lab02,
            [
                TYPO.as_bytes(),
                TYPO.as_bytes(),
                RIGHT.as_bytes(),
                RIGHT.as_bytes()
            ],
            "its two typos, then lab-04's password to its login and its sudo"
        );
    }

    #[test]
    fn a_mac_that_refuses_under_the_new_name_too_is_told_by_that_name() {
        // lab-04's admin password differs. Both refuse `owner`; under `admin` lab-02 takes the
        // password and lab-04 refuses it; then the prompt is stopped. What lab-04 says is
        // its refusal of `admin`, not the first round's of `owner`.
        let sys = set_up();
        sys.answer(Some(RIGHT)).answer(Some("admin"));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        for address in ["lab-02.local", "lab-04.local"] {
            logins(&sys, "owner", address, &[false]);
        }
        logins(&sys, "admin", "lab-02.local", &[true]);
        logins(&sys, "admin", "lab-04.local", &[false]);
        let mut args = batch(&["lab-02.local", "lab-04.local"], false);
        args.user = "owner".into();
        let (done, out) = run_batch(&sys, &args);
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 1, "{out}");
        let (label, why) = &summary.skipped[0];
        assert_eq!(label, "lab-04");
        assert!(
            why.starts_with("no login as admin with the password typed")
                && why.ends_with("not asked again: the prompt was stopped"),
            "{why}"
        );
        assert!(
            asks(&sys)
                .last()
                .is_some_and(|a| a.contains("lab-04 did not take the password for admin.")),
            "{:?}",
            asks(&sys)
        );
    }

    #[test]
    fn of_several_macs_one_with_remote_login_off_says_so_before_the_password_is_asked() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        sys.on(
            "/usr/bin/ssh-keyscan -T 10 -t ed25519 -- 192.168.64.9",
            Ran::exit(1, "write (192.168.64.9): Broken pipe\n"),
        );
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "192.168.64.9"], true));
        let summary = done.unwrap();
        assert!(
            out.lines()
                .next()
                .is_some_and(|l| l.starts_with("lab-02: first connection")),
            "{out}"
        );
        assert!(
            out.contains(
                "192.168.64.9: not enrolled: Remote Login is off: turn it on at that Mac \
                 (System Settings ▸ General ▸ Sharing ▸ Remote Login), then run the same \
                 enroll command again\n"
            ),
            "{out}"
        );
        // One Mac left to ask for: `on <address>`, never `for the 1 Macs`.
        assert_eq!(
            asks(&sys),
            ["ask secret Password for admin on lab-02.local (never saved): "]
        );
        // The summary gives each Mac not enrolled its own line.
        assert!(
            summary.text().starts_with(
                "enrolled 1 (lab-02); not enrolled: 1\n  192.168.64.9: Remote Login is off"
            ),
            "{}",
            summary.text()
        );
    }

    #[test]
    fn a_second_enroll_takes_the_recorded_admin_and_an_enrolled_mac_is_nothing_to_do() {
        let inv: Inventory = aterm_toml::from_str(&format!(
            "schema = 1\n[[host]]\nname = \"m201\"\nuuid = \"{UUID_A}\"\naddress = \
             \"192.168.64.20\"\nuser = \"admin\"\n[[host]]\nname = \"m203\"\nuuid = \
             \"{UUID_B}\"\naddress = \"192.168.64.23\"\nuser = \"labadmin\"\n[[host]]\nname = \
             \"m204\"\nuuid = \"{VM_UUID}\"\naddress = \"192.168.64.24\"\nuser = \"admin\"\n"
        ))
        .unwrap();
        assert_eq!(recorded_admin(&inv).as_deref(), Some("admin"));
        assert_eq!(recorded_admin(&Inventory::default()), None);
        let mut args = batch(&["192.168.64.21"], false);
        args.user = "owner".into();
        let args = with_recorded_admin(args, &inv);
        assert_eq!((args.user.as_str(), args.user_recorded), ("admin", true));
        // --user wins over the record.
        let mut given = batch(&["192.168.64.21"], true);
        given.user = "root2".into();
        assert_eq!(with_recorded_admin(given, &inv).user, "root2");
        // An enrolled Mac among the addresses is no failure: not counted as not enrolled.
        let summary = Summary {
            already: vec![("192.168.64.21".into(), "m202".into())],
            ..Summary::default()
        };
        assert!(summary.skipped.is_empty());
        assert_eq!(
            summary.text(),
            "enrolled 0; already enrolled, nothing to do: m202\n"
        );
    }

    #[test]
    fn the_retry_prompt_says_why_it_asks_again_and_that_others_get_the_password_too() {
        let sys = set_up();
        sys.answer(Some(TYPO)).answer(None).answer(None);
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        logins(&sys, "admin", "lab-02.local", &[false]);
        logins(&sys, "admin", "lab-04.local", &[false]);
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
        assert_eq!(
            asks(&sys)[1],
            "ask secret lab-02 did not take the password for admin. Type it again (it is also \
             tried on the other Macs that refused; Enter skips this Mac): "
        );
        let summary = done.unwrap();
        assert_eq!(summary.skipped.len(), 2, "{out}");
        assert!(
            summary.skipped[0].1.ends_with(
                "skipped at its password prompt: run the same enroll command again to enroll it"
            ),
            "{:?}",
            summary.skipped
        );
        // Nothing was copied to it.
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("scp") && c.contains("lab-02.local"))
        );
    }

    #[test]
    fn a_mac_whose_sudo_refuses_is_skipped_naming_sudo() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(
            &sys,
            "lab-02.local",
            UUID_A,
            "lab-02",
            Ran::exit(1, SUDO_WRONG),
        );
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
        let summary = done.unwrap();
        let (_, why) = &summary.skipped[0];
        assert!(
            why.contains("sudo refused admin's password"),
            "{why}\n{out}"
        );
        assert!(inventory::load(&sys, &ctl()).unwrap().hosts.is_empty());
    }

    #[test]
    fn a_mac_another_controller_enrolled_is_skipped_with_what_its_agent_said() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        // What the install line answered in the VM on a Mac an earlier controller enrolled
        // (exit 2, the refusal on standard output, nothing on standard error).
        let refused = Ran {
            status: Some(2),
            stdout: b"refused: install: this Mac is already enrolled \
                      (/Library/Management/aterm/allowed_signers exists) and only a signed rekey \
                      changes who it trusts\n"
                .to_vec(),
            stderr: Vec::new(),
        };
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", refused);
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
        let (_, why) = &done.unwrap().skipped[0];
        assert!(
            why.starts_with("the install command line failed (exit 2: refused: install: this Mac is already enrolled"),
            "{why}"
        );
        assert!(out.contains("refused: install:"), "{out}");
    }

    #[test]
    fn a_mac_offering_no_password_method_gets_keyboard_interactive() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        sys.on(
            &login_key("lab-02.local", 1, PASSWORD_METHOD),
            Ran::exit(
                255,
                "admin@lab-02.local: Permission denied (publickey,keyboard-interactive).\n",
            ),
        );
        sys.on(&login_key("lab-02.local", 1, KBD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
        assert_eq!(done.unwrap().enrolled.len(), 1, "{out}");
        assert_eq!(
            sys.calls().iter().filter(|c| c.starts_with("ask")).count(),
            1,
            "the same password, no second prompt"
        );
    }

    #[test]
    fn a_controller_that_is_not_set_up_refuses_before_the_password_is_asked() {
        let sys = Fake::root();
        sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
        let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
        assert!(done.unwrap_err().contains("aterm machines init"));
        assert!(!sys.calls().iter().any(|c| c.starts_with("ask")));
        // And nothing typed is nothing tried.
        let sys = set_up();
        sys.answer(None);
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
        assert!(done.unwrap_err().contains("no password typed"));
        assert!(sys.fed().is_empty(), "{:?}", sys.calls());
        // A Mac no key was read from is skipped, and nothing is asked for none.
        let sys = set_up();
        sys.on(
            "/usr/bin/ssh-keyscan -T 10 -t ed25519 -- lab-02.local",
            Ran::exit(1, ""),
        );
        let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
        let summary = done.unwrap();
        assert!(
            summary.skipped[0].1.contains("Remote Login"),
            "{:?}",
            summary.skipped
        );
        assert!(!sys.calls().iter().any(|c| c.starts_with("ask")));
    }

    #[test]
    fn a_mac_with_remote_login_off_is_skipped_with_one_line_and_nothing_asked() {
        // Remote Login off behind the firewall's stealth mode, where ssh-keyscan hears
        // nothing, and with the firewall off, as macOS ships (measured in the VM).
        for (said, class) in [
            ("", NoHostKey::Silent),
            ("write (192.168.64.9): Broken pipe\n", NoHostKey::Closed),
        ] {
            let sys = set_up();
            sys.on(
                "/usr/bin/ssh-keyscan -T 10 -t ed25519 -- 192.168.64.9",
                Ran::exit(1, said),
            );
            let inventory = format!("{ROOT}/machines.toml");
            let before = sys.written(&inventory);
            let (done, out) = run_batch(&sys, &batch(&["192.168.64.9"], true));
            let summary = done.unwrap();
            assert!(summary.enrolled.is_empty(), "{summary:?}");
            assert_eq!(
                summary.skipped,
                [("192.168.64.9".to_owned(), class.why("192.168.64.9"))]
            );
            assert!(out.is_empty(), "{out}");
            // The summary names the Mac once, by its label.
            assert_eq!(
                summary.text().matches("192.168.64.9").count(),
                1,
                "{}",
                summary.text()
            );
            // Nothing asked, nothing recorded.
            assert!(
                !sys.calls().iter().any(|c| c.starts_with("ask")),
                "{:?}",
                sys.calls()
            );
            assert_eq!(sys.written(&inventory), before);
        }
    }

    #[test]
    fn a_host_at_an_enrolled_macs_address_is_never_trusted_on_first_use() {
        // lab-02 is enrolled at lab-02.local, its pinned key not the one this host shows.
        let sys = set_up();
        sys.answer(Some(RIGHT));
        sys.file(
            &format!("{ROOT}/machines.toml"),
            format!(
                "schema = 1\n[[host]]\nname = \"lab-02\"\nuuid = \"{VM_UUID}\"\naddress = \
                 \"lab-02.local\"\nuser = \"admin\"\n"
            )
            .as_bytes(),
        );
        sys.file(
            &format!("{ROOT}/known_hosts"),
            format!("{VM_UUID} ssh-ed25519 {OWN_KEY}\n").as_bytes(),
        );
        a_mac(&sys, "LAB-02.local.", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
        sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["LAB-02.local.", "lab-04.local"], true));
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 1, "{out}");
        let (label, why) = &summary.skipped[0];
        assert_eq!(label, "LAB-02.local.");
        assert!(
            why.contains("is where lab-02 is enrolled")
                && why.contains(VM_FP)
                && why.contains("unenroll lab-02 --unreachable")
                && why.contains("Nothing was sent"),
            "{why}"
        );
        // The password went to lab-04 alone.
        assert!(
            sys.fed().iter().all(|(k, _)| !k.contains("LAB-02")),
            "{:?}",
            sys.fed()
        );
        // The enrolled Mac itself, answering with its pinned key: enrolled already, and with
        // no other Mac to enroll nothing is asked.
        let sys = set_up();
        sys.file(
            &format!("{ROOT}/machines.toml"),
            format!(
                "schema = 1\n[[host]]\nname = \"lab-02\"\nuuid = \"{VM_UUID}\"\naddress = \
                 \"lab-02.local\"\nuser = \"admin\"\n"
            )
            .as_bytes(),
        );
        sys.file(
            &format!("{ROOT}/known_hosts"),
            format!("{VM_UUID} ssh-ed25519 {VM_KEY}\n").as_bytes(),
        );
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
        let summary = done.unwrap();
        assert_eq!(
            summary.already,
            [("lab-02".to_owned(), "lab-02".to_owned())]
        );
        assert!(summary.skipped.is_empty(), "{summary:?}");
        assert!(!sys.calls().iter().any(|c| c.starts_with("ask")));
    }

    #[test]
    fn with_no_fingerprint_the_first_key_is_the_one_trusted_and_its_fingerprint_printed() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        let kept = Fake::scratch_path(None, "enroll", 1);
        sys.scratch_close_fails(&kept, Some(std::io::ErrorKind::PermissionDenied));
        let (_, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
        let line = out.lines().find(|l| l.contains(VM_FP)).unwrap_or_default();
        assert!(line.contains("first connection to this Mac"), "{out}");
        // The master trusted the scanned key and no other.
        assert_eq!(
            sys.written(&kept.join("known_hosts").to_string_lossy())
                .unwrap(),
            format!("aterm-machines-enroll ssh-ed25519 {VM_KEY}\n").as_bytes()
        );
    }

    #[test]
    fn a_fingerprint_that_does_not_match_is_refused_before_any_password_is_sent() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        let mut args = batch(&["lab-02.local"], true);
        args.trust = Trust::Fingerprint("SHA256:other".into());
        let (done, _) = run_batch(&sys, &args);
        let (_, why) = &done.unwrap().skipped[0];
        assert!(why.contains("Nothing was sent"), "{why}");
        assert!(sys.fed().is_empty(), "{:?}", sys.calls());
        // Refused before any password is typed, as one Mac's enrollment always was.
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("ask")),
            "{:?}",
            sys.calls()
        );
        // The matching one goes on to the login, saying --fingerprint named it.
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        args.trust = Trust::Fingerprint(VM_FP.into());
        let (done, out) = run_batch(&sys, &args);
        assert_eq!(done.unwrap().enrolled.len(), 1);
        assert!(out.contains(&format!("its ID is the one --fingerprint names ({VM_FP})")));
    }

    #[test]
    fn two_addresses_of_one_mac_enroll_it_once() {
        let sys = set_up();
        sys.answer(Some(RIGHT));
        a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
        a_mac(&sys, "192.168.64.5", UUID_A, "lab-02", Ran::ok(""));
        sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
        sys.on(&login_key("192.168.64.5", 2, PASSWORD_METHOD), Ran::ok(""));
        let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "192.168.64.5"], true));
        let summary = done.unwrap();
        assert_eq!(summary.enrolled.len(), 1, "{out}");
        assert!(
            summary.skipped[0].1.contains("already enrolled as lab-02"),
            "{out}"
        );
    }

    #[test]
    fn macs_in_flight_hold_their_uuid_and_name_against_each_other() {
        let sys = set_up();
        let paths = ctl();
        let scratch = Fake::scratch_path(None, "enroll", 0);
        let shared = Shared::new(&sys, &paths, Path::new(EXE), &scratch);
        let host = |name: &str, uuid: &str, address: &str| Host {
            name: name.into(),
            uuid: uuid.into(),
            address: address.into(),
            user: "admin".into(),
            baseline: None,
            users: Vec::new(),
        };
        shared
            .claim(&host("lab-02", UUID_A, "lab-02.local"))
            .unwrap();
        // The same Mac at another address, enrolled at the same time.
        let err = shared
            .claim(&host("lab-02-again", UUID_A, "192.168.64.5"))
            .unwrap_err();
        assert!(err.contains("enrolling too"), "{err}");
        // Another Mac answering with a name one in flight has.
        let err = shared
            .claim(&host("lab-02", UUID_B, "lab-04.local"))
            .unwrap_err();
        assert!(err.contains("the name lab-02 is taken"), "{err}");
        // Given up (enrolled or not), the place is free again.
        shared.release(UUID_A);
        shared
            .claim(&host("lab-02", UUID_B, "lab-04.local"))
            .unwrap();
    }

    #[test]
    fn a_mac_is_labelled_by_its_tag_and_a_shared_label_by_its_address() {
        let reqs = |addresses: &[&str]| -> Vec<Request> { batch(addresses, true).requests() };
        assert_eq!(
            labels(&reqs(&[
                "m10-MacBook-M4-Max-36GB-1TB.local",
                "lab-02.local",
                "192.168.64.5"
            ])),
            ["m10", "lab-02", "192.168.64.5"]
        );
        assert_eq!(
            labels(&reqs(&["m1-Air.local", "m1-Pro.local"])),
            ["m1-Air.local", "m1-Pro.local"]
        );
    }

    // ── --found ─────────────────────────────────────────────────────────────────────────

    /// This Mac's own host key, as the controller (m14) would read it.
    const OWN_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIPfo6cCmlK4W9QpUZ7Jvt5f9a0r2n8eXaAZ0aqNjSm2r";

    #[test]
    fn found_takes_the_macs_it_sees_and_leaves_linux_boxes_this_mac_and_enrolled_ones() {
        // The lab-02 session's Bonjour (crate::controller::discover's captures): lab-02,
        // this Mac (m14) and spark-1, which advertises Remote Login alone.
        let sys = discover::tests::lab_session();
        let browse = discover::browse(&sys, Duration::from_secs(3)).unwrap();
        sys.file(
            OWN_HOST_KEY,
            format!("ssh-ed25519 {OWN_KEY} root@m14\n").as_bytes(),
        );
        sys.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("m14-Air-M5-32GB-4TB-prime\n"),
        );
        sys.on(
            "/usr/bin/ssh-keyscan -T 3 -t ed25519 -p 22 -- lab-02.local",
            Ran::ok(&format!("lab-02.local ssh-ed25519 {VM_KEY}\n")),
        );
        let inv = Inventory::default();
        let (take, notes) = found_macs(&sys, &ctl(), &inv, &browse);
        assert_eq!(take, ["lab-02.local"]);
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("spark-1.local: left: not a Mac")),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n == "m14-Air-M5-32GB-4TB-prime.local: left: this Mac, the controller"),
            "{notes:?}"
        );
        // Neither was asked for a host key: what Bonjour showed was enough.
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("keyscan") && c.contains("spark-1"))
        );
        // Enrolled already: left, by the key it presents.
        sys.file(
            &format!("{ROOT}/known_hosts"),
            format!("{VM_UUID} ssh-ed25519 {VM_KEY}\n").as_bytes(),
        );
        let inv: Inventory = aterm_toml::from_str(&format!(
            "schema = 1\n[[host]]\nname = \"lab-02\"\nuuid = \"{VM_UUID}\"\naddress = \
             \"lab-02.local\"\nuser = \"admin\"\n"
        ))
        .unwrap();
        let (take, notes) = found_macs(&sys, &ctl(), &inv, &browse);
        assert!(take.is_empty(), "{take:?}");
        assert!(notes.contains(&"lab-02.local: left: enrolled already, as lab-02".to_owned()));
    }

    #[test]
    fn found_says_so_when_the_sftp_browse_failed() {
        let sys =
            discover::tests::lab_session_with(Ran::exit(255, "DNSService call failed -65570"));
        let browse = discover::browse(&sys, Duration::from_secs(3)).unwrap();
        sys.on("/usr/sbin/scutil --get LocalHostName", Ran::exit(1, ""));
        sys.on(
            "/usr/bin/ssh-keyscan -T 3 -t ed25519 -p 22 -- lab-02.local",
            Ran::ok(&format!("lab-02.local ssh-ed25519 {VM_KEY}\n")),
        );
        let (take, notes) = found_macs(&sys, &ctl(), &Inventory::default(), &browse);
        // lab-02 advertises Screen Sharing; spark-1 and m14 are not known to be Macs.
        assert_eq!(take, ["lab-02.local"]);
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("spark-1.local: left: not known to be a Mac")),
            "{notes:?}"
        );
    }

    #[test]
    fn found_knows_this_mac_by_its_host_key_when_its_name_says_nothing() {
        let sys = discover::tests::lab_session();
        let browse = discover::browse(&sys, Duration::from_secs(3)).unwrap();
        sys.file(
            OWN_HOST_KEY,
            format!("ssh-ed25519 {OWN_KEY} root@m14\n").as_bytes(),
        );
        sys.on("/usr/sbin/scutil --get LocalHostName", Ran::exit(1, ""));
        sys.on(
            "/usr/bin/ssh-keyscan -T 3 -t ed25519 -p 22 -- lab-02.local",
            Ran::ok(&format!("lab-02.local ssh-ed25519 {VM_KEY}\n")),
        );
        sys.on(
            "/usr/bin/ssh-keyscan -T 3 -t ed25519 -p 22 -- m14-Air-M5-32GB-4TB-prime.local",
            Ran::ok(&format!(
                "m14-Air-M5-32GB-4TB-prime.local ssh-ed25519 {OWN_KEY}\n"
            )),
        );
        let (take, _) = found_macs(&sys, &ctl(), &Inventory::default(), &browse);
        assert_eq!(take, ["lab-02.local"]);
    }

    #[test]
    fn a_name_another_mac_holds_is_refused_before_anything_is_installed() {
        let sys = enrolling(
            "Manageds-Virtual-Machine",
            "0A1B2C3D-0000-4000-8000-000000000003",
        );
        let mut out = Vec::new();
        let err = enroll_through(
            &sys,
            &ctl(),
            &request(None),
            Path::new("/w/aterm-machines-dev"),
            &master(),
            &vm_key(),
            Path::new("/s"),
            &mut out,
        )
        .unwrap_err();
        assert!(err.contains("taken") && err.contains("--name"), "{err}");
        assert!(!installed_anything(&sys), "{:?}", sys.calls());
    }

    #[test]
    fn a_mac_is_named_by_the_tag_its_local_hostname_starts_with() {
        // The holder is m10; the Mac answers m10's own hand-made Local hostname, so the
        // default name is its tag, m10, which is taken: refused, naming --name.
        let sys = Fake::root();
        sys.file(
            &format!("{ROOT}/machines.toml"),
            b"schema = 1\n[[host]]\nname = \"m10\"\nuuid = \"0A1B2C3D-0000-4000-8000-000000000003\"\n\
              address = \"192.168.64.3\"\nuser = \"admin\"\n",
        );
        sys.on(
            &remote_sh(&master(), FACTS_SCRIPT, false).display(),
            Ran::ok(&vm_facts().replace(
                "Manageds-Virtual-Machine\n",
                "m10-MacBook-M4-Max-36GB-1TB\n",
            )),
        );
        let err = enroll_through(
            &sys,
            &ctl(),
            &request(None),
            Path::new("/w/aterm-machines-dev"),
            &master(),
            &vm_key(),
            Path::new("/s"),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(err.contains("the name m10 is taken"), "{err}");
        assert!(!installed_anything(&sys));
    }

    #[test]
    fn a_name_whose_local_hostname_another_mac_would_get_is_refused_before_anything_is_installed() {
        let sys = enrolling("m1", "0A1B2C3D-0000-4000-8000-000000000003");
        sys.file(
            &format!("{ROOT}/baselines/default.toml"),
            crate::controller::templates::DEFAULT_BASELINE.as_bytes(),
        );
        let err = enroll_through(
            &sys,
            &ctl(),
            &request(Some("m1-spare")),
            Path::new("/w/aterm-machines-dev"),
            &master(),
            &vm_key(),
            Path::new("/s"),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            err.contains("m1-spare and m1 would both get the Local hostname m1-<hardware>")
                && err.contains("--name"),
            "{err}"
        );
        assert!(!installed_anything(&sys));
    }

    #[test]
    fn a_known_hardware_uuid_says_how_to_retire_an_erased_mac_and_not_to_unenroll_a_copy() {
        let sys = enrolling("lab-03", VM_UUID);
        let mut out = Vec::new();
        let err = enroll_through(
            &sys,
            &ctl(),
            &request(Some("lab-09")),
            Path::new("/w/aterm-machines-dev"),
            &master(),
            &vm_key(),
            Path::new("/s"),
            &mut out,
        )
        .unwrap_err();
        assert!(
            err.contains("`aterm machines unenroll lab-03 --unreachable`"),
            "{err}"
        );
        assert!(err.contains("copy, leave lab-03 enrolled"), "{err}");
        assert!(!installed_anything(&sys));
    }

    #[test]
    fn unenroll_refuses_a_host_whose_state_cannot_be_read_and_leaves_it_enrolled() {
        let sys = Fake::root();
        let inventory = format!(
            "schema = 1\n[github]\naccount = \"fleetbot\"\n[[host]]\nname = \"lab-03\"\nuuid = \
             \"{VM_UUID}\"\naddress = \"192.168.64.4\"\nuser = \"admin\"\n"
        );
        sys.file(&format!("{ROOT}/machines.toml"), inventory.as_bytes());
        // Cut off mid-write: it held the host's GitHub key ids.
        sys.file(
            &format!("{ROOT}/state/{VM_UUID}.json"),
            b"{\"github_keys\":[[\"alice\",5",
        );
        let host = inventory::load(&sys, &ctl()).unwrap().hosts[0].clone();
        let mut out = Vec::new();
        let err = unenroll(&sys, &ctl(), &[], &host, true, &mut out).unwrap_err();
        assert!(
            err.contains("not unenrolled") && err.contains("aterm-machines:lab-03:"),
            "{err}"
        );
        assert_eq!(
            sys.written(&format!("{ROOT}/machines.toml")).unwrap(),
            inventory.as_bytes(),
            "still enrolled"
        );
        assert!(
            sys.calls().is_empty(),
            "nothing asked of GitHub or the host: {:?}",
            sys.calls()
        );
    }

    #[test]
    fn an_uninstall_stream_ends_in_its_report_or_its_refusal() {
        let refused = "{\"refused\":{\"why\":\"bad-signature: No principal matched.\"}}\n";
        assert_eq!(
            report_of(refused).unwrap_err(),
            "refused: bad-signature: No principal matched."
        );
        assert!(report_of("").is_err());
    }
}
