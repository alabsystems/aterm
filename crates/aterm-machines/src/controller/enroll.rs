// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Controller-side `aterm machines enroll` and `unenroll` implementation.
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

const SSH: &str = aterm_uds::os_path!("ssh");

/// OpenSSH's password method, tried first (one prompt per login).
pub const PASSWORD_METHOD: &str = "password";

/// OpenSSH's keyboard-interactive method, for a Mac whose `sshd` offers no `password`.
pub const KBD_METHOD: &str = "keyboard-interactive";

const INSTALL_BUDGET: Duration = Duration::from_secs(900);

const LOGIN_BUDGET: Duration = Duration::from_secs(90);

const FOUND_WAIT: Duration = Duration::from_secs(3);

const OWN_HOST_KEY: &str = "/etc/ssh/ssh_host_ed25519_key.pub";

/// One Mac to enroll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub address: String,
    pub user: String,
    pub name: Option<String>,
    pub trust: Trust,
}

/// How the host key is trusted at enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    Fingerprint(String),
    FirstUse,
}

/// What the owner passed to `enroll`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub addresses: Vec<String>,
    pub found: bool,
    pub user: String,
    pub user_given: bool,
    pub user_recorded: bool,
    pub name: Option<String>,
    pub trust: Trust,
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

/// Parse `enroll <address>…` flags and positional host arguments.
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
                .filter(|n| !v.starts_with('+') && (1..=64).contains(n))
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

/// Return the most common admin account recorded across previously enrolled Macs.
#[must_use]
pub fn recorded_admin(inv: &Inventory) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for host in inv.hosts.iter().filter(|h| plain_account(&h.user)) {
        match counts.iter_mut().find(|(user, _)| *user == host.user) {
            Some((_, n)) => *n += 1,
            None => counts.push((&host.user, 1)),
        }
    }
    let most = counts.iter().map(|(_, n)| *n).max()?;
    counts
        .into_iter()
        .find(|(_, n)| *n == most)
        .map(|(user, _)| user.to_owned())
}

/// Default `args.user` to [`recorded_admin`] over this Mac's login name when `--user` was omitted.
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

fn plain_account(user: &str) -> bool {
    !user.is_empty()
        && !user.starts_with('-')
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Return the account name running this controller via `id -urn`.
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

fn normalize_fingerprint(fp: &str) -> String {
    let fp = fp.trim();
    if fp.starts_with("SHA256:") {
        fp.to_owned()
    } else {
        format!("SHA256:{fp}")
    }
}

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

/// A host key, as the scan read it ([`scan_host_key`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub kind: String,
    pub blob: String,
    pub fingerprint: String,
}

impl HostKey {
    /// `<kind> <blob>`.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{} {}", self.kind, self.blob)
    }
}

/// The first `<host> ssh-ed25519 <base64>` row of what the scan recorded (known_hosts lines, as
/// `ssh-keyscan` prints them too), as `(kind, blob)`.
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

/// Compute the `SHA256:…` fingerprint of a public key line via `ssh-keygen -lf -`.
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

/// Reason the host key scan ([`scan_host_key`]) recorded no ed25519 host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoHostKey {
    NoRoute,
    Down,
    Closed,
    Silent,
    /// The LAN broker could not make the connection: its line ([`ssh::Failure::Broker`]).
    Broker(String),
    Answered(String),
}

/// What `ssh` says where nothing takes the connection, or the Mac closes it at once.
const SCAN_CLOSED: [&str; 2] = [ssh::CONNECTION_REFUSED, "Connection closed by remote host"];

impl NoHostKey {
    /// What `ran`, a scan that recorded no ed25519 key, means.
    #[must_use]
    pub fn of(ran: &Ran) -> Self {
        let err = ran.err();
        if let Some(line) = err
            .lines()
            .find(|line| line.contains(super::lan_proxy::BROKER_SAID))
        {
            Self::Broker(line.trim().to_owned())
        } else if err.contains(ssh::NO_ROUTE) {
            Self::NoRoute
        } else if err.contains(ssh::HOST_DOWN) {
            Self::Down
        } else if SCAN_CLOSED.iter().any(|said| err.contains(said)) {
            Self::Closed
        } else if err.trim().is_empty()
            || err.contains(ssh::CONNECT_TIMED_OUT)
            || err.contains(ssh::BANNER_TIMED_OUT)
        {
            Self::Silent
        } else {
            Self::Answered(ran.describe())
        }
    }

    /// Format a single-line explanation for why `address` yielded no host key.
    #[must_use]
    pub fn why(&self, address: &str) -> String {
        const TURN_ON: &str = "turn it on at that Mac (System Settings ▸ General ▸ Sharing ▸ \
                               Remote Login), then run the same enroll command again";
        match self {
            Self::NoRoute => ssh::Failure::NoRoute.why(),
            Self::Down => ssh::Failure::TimedOut.why(),
            Self::Closed => format!("Remote Login is off: {TURN_ON}"),
            Self::Silent => format!(
                "no answer in {} s: Remote Login is off, or the Mac is asleep, off or not on \
                 this network; if Remote Login is off, {TURN_ON}",
                super::lan_proxy::SCAN_WITHIN.as_secs()
            ),
            Self::Broker(line) => line.clone(),
            Self::Answered(said) => {
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

/// Scan `address` for its `ssh-ed25519` host key on port 22 ([`super::lan_proxy::scan`], its
/// record kept in `dir` only while it is read), fingerprinted in `scratch`.
pub fn scan_host_key(
    sys: &dyn System,
    dir: &Path,
    address: &str,
    scratch: &Path,
) -> Result<HostKey, String> {
    let (recorded, ran) =
        super::lan_proxy::scan(sys, dir, address, 22, super::lan_proxy::SCAN_WITHIN)?;
    let (kind, blob) = parse_keyscan(&recorded).ok_or_else(|| NoHostKey::of(&ran).why(address))?;
    let fingerprint = fingerprint_of(sys, &format!("{kind} {blob}"), scratch)?;
    Ok(HostKey {
        kind,
        blob,
        fingerprint,
    })
}

/// Read `/etc/ssh/ssh_host_ed25519_key.pub` over an already open SSH master connection.
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

/// Verify `key` against the caller's [`Trust`] policy.
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

/// `known_hosts` text with every line that pins a key under `alias` ([`inventory::pins`],
/// which [`inventory::forget_host_key`] drops too) replaced by `alias <key>` (other lines kept).
#[must_use]
pub fn known_hosts_with(current: &str, alias: &str, key: Option<&HostKey>) -> String {
    let mut out: String = current
        .lines()
        .filter(|l| !inventory::pins(l, alias))
        .map(|l| format!("{l}\n"))
        .collect();
    if let Some(key) = key {
        out.push_str(&format!("{alias} {}\n", key.line()));
    }
    out
}

/// Pin `key` under `HostKeyAlias=<uuid>` in the controller's `known_hosts`.
pub fn pin_host_key(
    sys: &dyn System,
    paths: &Paths,
    uuid: &str,
    key: &HostKey,
) -> Result<(), String> {
    rewrite_known_hosts(sys, paths, uuid, Some(key))
}

/// Remove `uuid`'s pinned key from the controller's `known_hosts`.
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
    pub ctl: PathBuf,
    pub destination: String,
}

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

/// Build the `ssh -M` master command for password login using the askpass helper.
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
        .args(["-o", "NumberOfPasswordPrompts=1", "-o", "ConnectTimeout=15"])
        .args(super::lan_proxy::options(Duration::from_secs(15)))
        .arg("--")
        .arg(&master.destination)
        .arg("true")
        .budget(LOGIN_BUDGET)
}

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
    // A rider opens no connection while the master lives; one that finds it gone connects
    // through the proxy too.
    args.extend(super::lan_proxy::options(Duration::from_secs(15)));
    args
}

/// Run `script` on the remote host via `/bin/sh` over `master`.
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
    pub uuid: String,
    pub serial: String,
    pub model: String,
    pub arch: String,
    pub os_version: String,
    pub os_build: String,
    pub computer_name: String,
    pub local_host_name: String,
    pub tmpdir: String,
    pub chip: String,
    pub product_name: String,
}

impl Facts {
    /// Format a human-readable hardware and OS summary line for an enrolled Mac.
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

/// Remote `/bin/sh` probe script that verifies macOS and collects hardware/hostname facts.
pub const FACTS_SCRIPT: &str = "[ \"$(/usr/bin/uname -s)\" = Darwin ] || { echo \"not macOS: \
$(/usr/bin/uname -s)\" >&2; exit 3; }; /usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice; \
echo --aterm--; /usr/sbin/sysctl -n hw.model; echo --aterm--; /usr/bin/uname -m; \
echo --aterm--; /usr/bin/sw_vers -productVersion; echo --aterm--; /usr/bin/sw_vers -buildVersion; \
echo --aterm--; /usr/sbin/scutil --get ComputerName; echo --aterm--; \
/usr/sbin/scutil --get LocalHostName; echo --aterm--; /usr/bin/getconf DARWIN_USER_TEMP_DIR; \
echo --aterm--; /usr/sbin/sysctl -n machdep.cpu.brand_string; echo --aterm--; \
/usr/sbin/ioreg -p IODeviceTree -r -n product -d 1";

/// Parse [`FACTS_SCRIPT`] output into [`Facts`].
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
    let uuid = crate::hardware::ioreg_value(ioreg, "IOPlatformUUID")
        .filter(|u| crate::hardware::uuid_shaped(u))
        .ok_or("the host printed no IOPlatformUUID")?;
    let tmpdir = tmp.trim().to_owned();
    if !tmpdir.starts_with('/') || tmpdir.contains(['\'', '"', '\\', '\n']) {
        return Err(format!(
            "the admin's temporary directory {tmpdir:?} is not a plain path"
        ));
    }
    Ok(Facts {
        uuid,
        serial: crate::hardware::ioreg_value(ioreg, "IOPlatformSerialNumber").unwrap_or_default(),
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

/// Read remote [`Facts`] over `master`.
pub fn read_facts(sys: &dyn System, master: &Master) -> Result<Facts, String> {
    let ran = sys
        .run(&remote_sh(master, FACTS_SCRIPT, false).budget(Duration::from_secs(60)))
        .map_err(|e| format!("reading the host's facts: {e}"))?;
    if !ran.success() {
        return Err(format!("reading the host's facts: {}", ran.describe()));
    }
    parse_facts(&ran.out())
}

/// The agent payload archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub path: PathBuf,
    pub sha256: String,
    pub exe: PathBuf,
    pub bundle: bool,
}

/// Resolve the bundle or binary path to archive for `exe` and the agent path inside it.
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

/// Archive the agent with `ditto -c -k` into `scratch` and compute its SHA-256.
pub fn build_archive(sys: &dyn System, exe: &Path, scratch: &Path) -> Result<Archive, String> {
    let (source, inner, bundle) = payload_of(exe);
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

/// Build the `scp` command that copies `files` into `remote_dir` over `master`.
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
        ])
        // `scp` opens no connection while the master lives; one that finds it gone connects
        // through the proxy too.
        .args(super::lan_proxy::options(Duration::from_secs(15)))
        .arg("--");
    for f in files {
        cmd = cmd.arg(f.as_os_str());
    }
    cmd.arg(format!("{}:{remote_dir}", master.destination))
        .budget(Duration::from_secs(1800))
}

/// Copy `files` into `remote_dir` on the host with one `scp` over `master`.
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
    pub archive: &'a Archive,
    pub remote_archive: String,
    pub remote_signers: String,
    pub signers_sha: String,
    pub admin: String,
    pub fleet_key: String,
}

/// The commands the root script reads this Mac's clocks with: the boot's id, the boot and
/// the last wake (0 before any) in `sysctl`'s `{ sec = <unix>, usec = <n> } <date>` form,
/// and the wall clock's Unix seconds.
#[derive(Debug, Clone, Copy)]
struct Clocks<'a> {
    session: &'a str,
    boot: &'a str,
    wake: &'a str,
    now: &'a str,
}

/// This Mac's [`Clocks`].
const MAC_CLOCKS: Clocks<'static> = Clocks {
    session: "/usr/sbin/sysctl -n kern.bootsessionuuid",
    boot: "/usr/sbin/sysctl -n kern.boottime",
    wake: "/usr/sbin/sysctl -n kern.waketime",
    now: "/bin/date +%s",
};

/// Shell that sets `ok` to 1 when `$word` is a boot id (`kern.bootsessionuuid`'s form),
/// and empties it when it is not.
pub(crate) fn boot_id_check(word: &str) -> String {
    let pattern = [8, 4, 4, 4, 12]
        .map(|n| "[0123456789ABCDEFabcdef]".repeat(n))
        .join("-");
    format!("ok=; case {word} in {pattern}) ok=1;; esac\n")
}

/// Shell that empties `ok` unless each of `words` is a count of seconds.
fn seconds_check(words: &str) -> String {
    format!("for n in {words}; do case $n in ''|*[!0-9]*|?????????????*) ok=;; esac; done\n")
}

/// Shell that sets `sess` to this boot's id, `boot` and `woke` to the Unix seconds of the boot
/// and the last wake, and `up` to the wall clock less `boot`: seconds since boot, asleep or
/// awake, which no wall-clock step moves, since XNU moves the boot time with each step. The
/// boot time is read again after the wall clock, so a step between the reads shows as a moved
/// boot time; that, or a reading of another form, ends the script.
fn clock_reading(c: &Clocks<'_>) -> String {
    let Clocks {
        session,
        boot,
        wake,
        now,
    } = c;
    format!(
        "sess=$({session})\n\
         boot=$({boot}); boot=${{boot#*sec = }}; boot=${{boot%%,*}}\n\
         woke=$({wake}); woke=${{woke#*sec = }}; woke=${{woke%%,*}}\n\
         now=$({now})\n\
         again=$({boot}); again=${{again#*sec = }}; again=${{again%%,*}}\n\
         {}{}\
         [ \"$again\" = \"$boot\" ] || ok=\n\
         if [ -z \"$ok\" ]; then echo \"aterm machines: this Mac's boot id and clocks read \
         '$sess' '$boot' '$woke' '$now' '$again'; nothing was installed\" >&2; exit 1; fi\n\
         up=$((now - boot))\n",
        boot_id_check("$sess"),
        seconds_check("\"$boot\" \"$woke\" \"$now\""),
    )
}

/// Shell that writes [`clock_reading`]'s readings to `staging`'s `exec` stamp.
fn stamp_line(staging: &str) -> String {
    format!("echo \"$sess $up $boot $woke\" > {staging}/exec\n")
}

/// The root script's refusal to clear `staging` while the agent an earlier enrollment started
/// from it has not shown it ran: [`root_script`] stamps `exec` with [`clock_reading`]'s readings
/// just before the agent's first exec, and the agent writes `ran` first thing
/// ([`crate::agent::install::main`]). A restart since releases it; otherwise it holds for
/// [`aterm_uds::deadline::FIRST_EXEC_HANG`] of `up` from the later of the stamp and the last
/// wake, so time asleep is not counted. A stamp it cannot read, or a wake it cannot place on
/// `up` (the wall clock stepped since the stamp too), makes it hold for the whole hang from now
/// and stamp `exec` again.
#[must_use]
pub fn stage_guard(staging: &str) -> String {
    guard_reading(staging, &MAC_CLOCKS)
}

/// [`stage_guard`], reading the clocks with `c`.
fn guard_reading(staging: &str, c: &Clocks<'_>) -> String {
    let hang = aterm_uds::deadline::FIRST_EXEC_HANG.as_secs();
    let reading = clock_reading(c);
    let stamp = stamp_line(staging);
    let checks =
        boot_id_check("$was_sess") + &seconds_check("\"$was_up\" \"$was_boot\" \"$was_woke\"");
    format!(
        "if [ -e {staging}/exec ] && [ ! -e {staging}/ran ]; then\n\
         {reading}\
         was_sess= was_up= was_boot= was_woke= rest=\n\
         read -r was_sess was_up was_boot was_woke rest < {staging}/exec || :\n\
         {checks}\
         left=\n\
         if [ -n \"$ok\" ] && [ -z \"$rest\" ]; then\n\
         if [ \"$was_sess\" != \"$sess\" ]; then left=0\n\
         elif [ \"$was_woke\" = \"$woke\" ]; then\n\
         [ \"$up\" -lt \"$was_up\" ] || left=$(( {hang} - up + was_up ))\n\
         elif [ \"$was_boot\" = \"$boot\" ]; then\n\
         from=$((woke - boot)); [ \"$from\" -ge \"$was_up\" ] || from=$was_up\n\
         [ \"$up\" -lt \"$from\" ] || left=$(( {hang} - up + from ))\n\
         fi\n\
         fi\n\
         if [ -z \"$left\" ]; then left={hang}; {stamp}fi\n\
         if [ \"$left\" -gt 0 ]; then echo \"aterm machines: the helper an earlier enrollment \
         started from {staging} has not shown it ran, and macOS may still be checking it; \
         enroll again in $(( (left + 59) / 60 )) minute(s), or after a restart\" >&2; exit 1; \
         fi\n\
         fi\n"
    )
}

/// Generate the root `/bin/sh` installation script executed under `sudo`.
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
         /bin/chmod 0755 /Library/Management {root}\n"
    ));
    s.push_str(&stage_guard(&staging));
    s.push_str(&format!(
        "/bin/rm -rf {staging}\n\
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
        "{}{}{} machines agent install --admin {} --fleet-key {} --allowed-signers \
         {staging}/allowed_signers --payload-sha {} --stage {staging}/payload\n",
        clock_reading(&MAC_CLOCKS),
        stamp_line(&staging),
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
    Terminal,
    Password(&'p Secret),
}

/// Generate the admin `/bin/sh` command line that runs [`root_script`] under `sudo` and cleans up.
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

/// Resolve this `aterm` executable path with symlinks canonicalized.
pub fn own_exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("this aterm cannot name its executable: {e}"))
}

/// Runs `aterm machines enroll`.
#[must_use = "handle or propagate the command's exit status"]
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
    let done = {
        let mut stdout = std::io::stdout();
        let sink: Sink<'_> = Mutex::new(&mut stdout as &mut (dyn Write + Send));
        enroll_macs(sys, paths, &args, &macs, &exe, parallel, &sink)
    };
    let summary = match done {
        Ok(s) => s,
        Err(why) => {
            eprintln!("aterm machines enroll: {}", shown(&why));
            return ExitCode::FAILURE;
        }
    };
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

fn locked<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

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
    pub enrolled: Vec<(String, Host)>,
    pub already: Vec<(String, String)>,
    pub skipped: Vec<(String, String)>,
}

impl Summary {
    /// Format the final enrollment summary and per-host failure lines.
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

/// Assign unique display labels for each enrollment request.
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

#[derive(Debug)]
enum Done {
    Enrolled(Host),
    Already(String),
    Refused(String),
    Skipped(String),
}

/// Enroll `macs` in parallel after scanning host keys and prompting for the admin password.
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
    let mut done: Vec<Done> = scanned
        .iter()
        .map(|scan| match scan {
            Ok(_) => Done::Skipped(String::new()),
            Err(Unscanned::Enrolled(name)) => Done::Already(name.clone()),
            Err(Unscanned::NoKey(why) | Unscanned::Stopped(why)) => Done::Skipped(why.clone()),
        })
        .collect();
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
            let others = refusing(&done);
            if worked && !others.is_empty() {
                let retried = ssh::fan_out(&others, parallel, |&j| one(j, &user, &retyped));
                store(&mut done, &others, retried);
            }
        }
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
        say_stderr(&format!(
            "that is not an account name (letters, digits, '.', '_' or '-'); keeping {user}"
        ));
        return Ok(None);
    }
    Ok(Some(name))
}

fn ask(sys: &dyn System, prompt: &str, secret: bool) -> Result<Option<Secret>, String> {
    sys.ask(prompt, secret).map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            "stopped at the prompt".to_owned()
        } else {
            format!("the terminal: {e}")
        }
    })
}

fn say(sink: &Sink<'_>, label: &str, line: &str) {
    say_line(sink, &format!("{label}: {line}"));
}

fn say_line(sink: &Sink<'_>, line: &str) {
    let mut out = locked(sink);
    let _ = writeln!(out, "{}", shown(line));
    let _ = out.flush();
}

fn say_stderr(line: &str) {
    eprintln!("aterm machines enroll: {line}");
}

/// Shared agent archive, `allowed_signers`, and fleet public key payload for an `enroll` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub archive: Archive,
    pub signers: PathBuf,
    pub signers_sha: String,
    pub fleet_key: String,
}

impl Payload {
    fn tag(&self) -> &str {
        self.archive
            .sha256
            .get(..8)
            .unwrap_or(self.archive.sha256.as_str())
    }
}

/// Build the [`Payload`] in `scratch` from `exe` and the controller's keys.
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

/// Shared enrollment state synchronizing payload creation and inventory updates across threads.
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

    fn payload(&self) -> Result<Payload, String> {
        let mut slot = locked(&self.payload);
        if let Some(payload) = slot.as_ref() {
            return Ok(payload.clone());
        }
        let payload = build_payload(self.sys, self.paths, self.exe, self.scratch)?;
        *slot = Some(payload.clone());
        Ok(payload)
    }

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

    fn release(&self, uuid: &str) {
        locked(&self.claims).retain(|h| !h.uuid.eq_ignore_ascii_case(uuid));
    }

    fn recorded<T>(&self, f: impl FnOnce() -> T) -> T {
        let _held = locked(&self.claims);
        f()
    }
}

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
    In,
    Refused,
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

/// Authenticate and open `master` using `password`.
#[must_use]
pub fn log_in(
    sys: &dyn System,
    master: &Master,
    known_hosts: &Path,
    alias: &str,
    askpass: &Path,
    password: &Secret,
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
                |f| f.why(),
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

const ENROLL_ALIAS: &str = "aterm-machines-enroll";

/// Scanned host key and temporary `known_hosts` file for one Mac prior to login.
pub struct Scanned<'s> {
    pub key: HostKey,
    pub known: PathBuf,
    _scratch: Scratch<'s>,
}

/// Why a Mac's host key did not ready it for the password.
#[derive(Debug)]
pub enum Unscanned {
    NoKey(String),
    Stopped(String),
    Enrolled(String),
}

/// Scan `req`'s host key, verify trust, and write its temporary `known_hosts`.
pub fn scan_one<'s>(
    sys: &'s dyn System,
    paths: &Paths,
    req: &Request,
    out: &mut dyn Write,
) -> Result<Scanned<'s>, Unscanned> {
    let scratch = Scratch::new(sys, "enroll")
        .map_err(|e| Unscanned::Stopped(format!("enroll scratch: {e}")))?;
    let dir = super::lan_proxy::scan_dir(sys, paths).map_err(Unscanned::Stopped)?;
    let key = scan_host_key(sys, &dir, &req.address, scratch.path()).map_err(Unscanned::NoKey)?;
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

/// Reject a host answering at an enrolled Mac's address with a different host key.
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
    close_master(sys, &master);
    let _ = writeln!(out, "logging in as {}", req.user);
    match log_in(sys, &master, known, alias, askpass, password) {
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
            if announce {
                let _ = writeln!(out, "enrolled as {}", host.name);
            }
            Done::Enrolled(host)
        }
        Err(why) => Done::Skipped(why),
    }
}

/// Complete enrollment over an already authenticated `master` connection.
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

fn install_through(
    shared: &Shared<'_>,
    req: &Request,
    master: &Master,
    key: &HostKey,
    sudo: Sudo<'_>,
    out: &mut dyn Write,
) -> Result<Host, String> {
    let (sys, paths) = (shared.sys, shared.paths);
    inventory::load(sys, paths)?;
    let facts = read_facts(sys, master)?;
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
            let _ = writeln!(out, "checked: the controller reaches it without a password");
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
            return match sys.run_tty(
                &remote_sh(master, &install_command(install, sudo), true),
                &mut |_| Ok(()),
            ) {
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

/// Filter Bonjour-discovered hosts to un-enrolled Macs with Remote Login enabled.
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
        let offered = discover::offered_keys(sys, paths, row)?;
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
        return Err(ssh::classify(&ran).map_or_else(|| ran.describe(), |f| f.why()));
    }
    let hello: Hello = aterm_json::from_str(ran.out().trim()).map_err(|e| format!("hello: {e}"))?;
    if !hello.uuid.eq_ignore_ascii_case(&host.uuid) {
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

/// Extract the final [`HostReport`] from an `apply` output stream.
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

/// `aterm machines unenroll <host> [--unreachable]`.
pub fn unenroll(
    sys: &dyn System,
    paths: &Paths,
    principals: &[String],
    host: &Host,
    unreachable: bool,
    out: &mut dyn Write,
) -> Result<(), String> {
    let inv = inventory::load(sys, paths)?;
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
        super::github::revoke_via(sys, paths, github, &ids)?;
        let _ = writeln!(
            out,
            "deleted {} GitHub key(s) from {}",
            ids.len(),
            github.account
        );
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
#[path = "enroll_tests.rs"]
mod tests;
