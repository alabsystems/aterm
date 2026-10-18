// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines enroll` and `unenroll`: the controller's side (design §4.2).
//!
//! Enrollment needs a person: the owner types the admin's password once to `ssh` (an
//! OpenSSH control master every later step rides) and once to Apple's `sudo`. In order:
//!
//! 1. the host key: `ssh-keyscan -t ed25519 <address>`, fingerprinted by `ssh-keygen -lf -`,
//!    must equal `--fingerprint` (or `--trust-first-use` accepts it); the master trusts that
//!    key alone;
//! 2. the master (`ssh -M -S <short path> -o ControlPersist=120 … true`, on the terminal);
//! 3. the host's facts, read as the admin (no forced command exists yet);
//! 4. the agent archive (`ditto -c -k --keepParent` of the running aterm.app, or of the bare
//!    dev binary) and `allowed_signers`, copied by one `scp` into the admin's temporary
//!    directory;
//! 5. one `ssh -t` command line: `sudo` copies both into the root-owned staging area, checks
//!    the root-owned copies' SHA-256, expands the archive, checks a bundle's signature, and
//!    runs the STAGED agent's `machines agent install`. Nothing user-writable is executed as
//!    root;
//! 6. the host key is pinned under `HostKeyAlias=<uuid>`, the fleet key's `hello` checks the
//!    forced command answers as that UUID, and the host is recorded in `machines.toml`.
//!
//! What OpenSSH and the host printed, measured in the macOS 27.0 VM (`OpenSSH_10.3`):
//! `ssh-keyscan -t ed25519 127.0.0.1` writes `# 127.0.0.1:22 SSH-2.0-OpenSSH_10.3` on
//! standard error and `127.0.0.1 ssh-ed25519 AAAA…` on standard output; `ssh-keygen -lf -`
//! answers `256 SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q 127.0.0.1 (ED25519)`.

use super::inventory::{self, Host, Paths};
use super::ssh::{self, Target};
use super::text::shown;
use crate::envelope::{self, Action, Disrupt, Envelope};
use crate::model::HostReport;
use crate::port::{Cmd, Ran, System, shell_word};
use crate::protocol::{Hello, Line};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// The client: Apple's OpenSSH.
const SSH: &str = "/usr/bin/ssh";

/// What the owner passed to `enroll`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Where the Mac answers (`lab-07.local`, an address).
    pub address: String,
    /// Its admin account.
    pub user: String,
    /// The inventory name (default: the Mac's LocalHostName).
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
    /// Whatever the LAN answers (the owner accepted the risk).
    FirstUse,
}

/// Parse `enroll <address> --user <admin> [--name <n>] (--fingerprint <fp> |
/// --trust-first-use)`.
///
/// # Errors
/// One line.
pub fn parse_args(args: &[String]) -> Result<Request, String> {
    let mut address = None;
    let mut user = None;
    let mut name = None;
    let mut fingerprint = None;
    let mut tofu = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--user" => user = it.next().cloned(),
            "--name" => name = it.next().cloned(),
            "--fingerprint" => fingerprint = it.next().cloned(),
            "--trust-first-use" => tofu = true,
            flag if flag.starts_with('-') => {
                return Err(format!("enroll: unknown option {flag}"));
            }
            word if address.is_none() => address = Some(word.to_owned()),
            word => return Err(format!("enroll: one address only ({word:?} is a second)")),
        }
    }
    let address = address.ok_or("enroll: name the Mac's address")?;
    let user = user.ok_or("enroll: --user <admin> is required")?;
    let trust = match (fingerprint, tofu) {
        (Some(fp), false) => Trust::Fingerprint(normalize_fingerprint(&fp)),
        (None, true) => Trust::FirstUse,
        (Some(_), true) => {
            return Err("enroll: --fingerprint and --trust-first-use exclude each other".into());
        }
        (None, false) => {
            return Err(
                "enroll: pass --fingerprint SHA256:… (at the Mac: ssh-keygen -lf \
                 /etc/ssh/ssh_host_ed25519_key.pub), or --trust-first-use"
                    .into(),
            );
        }
    };
    let target = Target {
        user: user.clone(),
        address: address.clone(),
        alias: None,
    };
    plain_destination(&target)?;
    if let Some(n) = &name
        && !n
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "enroll: --name {n:?} must be letters, digits, `.`, `_` or `-`"
        ));
    }
    Ok(Request {
        address,
        user,
        name,
        trust,
    })
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

/// Read the host's ed25519 key off the network: `ssh-keyscan -t ed25519 <address>`.
///
/// # Errors
/// One line (no answer, or no ed25519 key).
pub fn scan_host_key(sys: &dyn System, address: &str, scratch: &Path) -> Result<HostKey, String> {
    let ran = sys
        .run(
            &Cmd::new("/usr/bin/ssh-keyscan")
                .args(["-T", "10", "-t", "ed25519", "--", address])
                .budget(Duration::from_secs(30)),
        )
        .map_err(|e| format!("ssh-keyscan: {e}"))?;
    let (kind, blob) = parse_keyscan(&ran.out()).ok_or_else(|| {
        format!(
            "{address} answered no ed25519 host key ({}); is Remote Login on?",
            ran.describe()
        )
    })?;
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

/// `ssh -M`: the master the owner types the password to. It trusts only the key in
/// `known_hosts` under `alias`, uses passwords alone (so a key the owner's agent holds,
/// the fleet key on a re-enrolled Mac among them, cannot turn the session into a forced
/// command), and lingers 120 seconds after its last client.
#[must_use]
pub fn master_cmd(master: &Master, known_hosts: &Path, alias: &str) -> Cmd {
    Cmd::new(SSH)
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
        .args([
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "PreferredAuthentications=keyboard-interactive,password",
            "-o",
            "ConnectTimeout=15",
            "--",
        ])
        .arg(&master.destination)
        .arg("true")
}

/// The options that ride `master` (and fail rather than open a new connection).
fn through(master: &Master) -> Vec<OsString> {
    let mut args = common();
    args.extend(["-S".into(), master.ctl.clone().into_os_string()]);
    args.extend(
        [
            "-o",
            "ControlMaster=no",
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
}

/// The facts, one tool per section, sections separated by a line `--aterm--`.
pub const FACTS_SCRIPT: &str = "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice; echo --aterm--; \
/usr/sbin/sysctl -n hw.model; echo --aterm--; /usr/bin/uname -m; echo --aterm--; \
/usr/bin/sw_vers -productVersion; echo --aterm--; /usr/bin/sw_vers -buildVersion; echo --aterm--; \
/usr/sbin/scutil --get ComputerName; echo --aterm--; /usr/sbin/scutil --get LocalHostName; \
echo --aterm--; /usr/bin/getconf DARWIN_USER_TEMP_DIR";

/// Parse [`FACTS_SCRIPT`]'s output.
///
/// # Errors
/// One line naming the fact that is missing (the UUID and the temporary directory are
/// required; a name may be unset).
pub fn parse_facts(out: &str) -> Result<Facts, String> {
    let sections: Vec<&str> = out.split("--aterm--\n").collect();
    let [ioreg, model, arch, version, build, computer, local, tmp] = sections.as_slice() else {
        return Err(format!(
            "the host's facts came back in {} sections, not 8",
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
    let mut cmd = Cmd::new("/usr/bin/scp")
        .args(["-q", "-F", "/dev/null", "-o"])
        .arg(format!("ControlPath={}", master.ctl.display()))
        .args(["-o", "ControlMaster=no", "-o", "BatchMode=yes", "--"]);
    for f in files {
        cmd = cmd.arg(f.as_os_str());
    }
    let cmd = cmd
        .arg(format!("{}:{remote_dir}", master.destination))
        .budget(Duration::from_secs(1800));
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

/// The one command line of design §4.2 step 4, for `/bin/sh` on the host. Each `sudo` asks
/// for the password at most once (Apple's sudo keeps a per-terminal timestamp); the
/// archive is executed only after its root-owned copy hashed right.
#[must_use]
pub fn install_script(i: &Install<'_>) -> String {
    let root = crate::AGENT_ROOT;
    let staging = format!("{root}/.staging");
    let q = |s: &str| shell_word(s);
    let exe = format!("{staging}/payload/{}", i.archive.exe.display());
    let mut s = String::new();
    s.push_str("set -e\n");
    s.push_str(&format!(
        "/usr/bin/sudo /bin/mkdir -p -m 0755 /Library/Management {root}\n\
         /usr/bin/sudo /usr/sbin/chown root:wheel /Library/Management {root}\n\
         /usr/bin/sudo /bin/chmod 0755 /Library/Management {root}\n\
         /usr/bin/sudo /bin/rm -rf {staging}\n\
         /usr/bin/sudo /bin/mkdir -m 0700 {staging}\n\
         /usr/bin/sudo /bin/cp {} {staging}/payload.zip\n\
         /usr/bin/sudo /bin/cp {} {staging}/allowed_signers\n",
        q(&i.remote_archive),
        q(&i.remote_signers),
    ));
    for (file, sha) in [
        ("payload.zip", i.archive.sha256.as_str()),
        ("allowed_signers", i.signers_sha.as_str()),
    ] {
        s.push_str(&format!(
            "got=$(/usr/bin/sudo /usr/bin/shasum -a 256 {staging}/{file})\n\
             got=${{got%% *}}\n\
             if [ \"$got\" != {sha} ]; then echo \"aterm machines: the root-owned {file} \
             hashes to $got, not {sha}; nothing was installed\" >&2; exit 1; fi\n"
        ));
    }
    s.push_str(&format!(
        "/usr/bin/sudo /usr/bin/ditto -x -k {staging}/payload.zip {staging}/payload\n\
         /usr/bin/sudo /usr/sbin/chown -R root:wheel {staging}/payload\n\
         /usr/bin/sudo /bin/chmod -R go-w {staging}/payload\n\
         /usr/bin/sudo /bin/chmod 0755 {staging}/payload\n"
    ));
    if i.archive.bundle
        && let Some(app) = i.archive.exe.components().next()
    {
        s.push_str(&format!(
            "/usr/bin/sudo /usr/bin/codesign --verify --strict {}\n",
            q(&format!("{staging}/payload/{}", Path::new(&app).display()))
        ));
    }
    s.push_str(&format!(
        "/usr/bin/sudo {} machines agent install --admin {} --fleet-key {} --allowed-signers \
         {staging}/allowed_signers --payload-sha {} --stage {staging}/payload\n",
        q(&exe),
        q(&i.admin),
        q(&i.fleet_key),
        i.archive.sha256,
    ));
    s.push_str(&format!(
        "/bin/rm -f {} {}\n",
        q(&i.remote_archive),
        q(&i.remote_signers)
    ));
    s
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

/// `aterm machines enroll …` (refuses without a terminal: a person types two passwords).
#[must_use]
pub fn main(sys: &dyn System, paths: &Paths, args: &[String], tty: bool) -> ExitCode {
    let req = match parse_args(args) {
        Ok(r) => r,
        Err(why) => {
            eprintln!("aterm machines {why}");
            return ExitCode::from(2);
        }
    };
    if !tty {
        eprintln!(
            "aterm machines enroll: not run: you type the admin's password to ssh and to \
             sudo, and standard input or error is not a terminal"
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
    let mut out = std::io::stdout().lock();
    match enroll(sys, paths, &req, &exe, &mut out) {
        Ok(host) => {
            let _ = writeln!(
                out,
                "enrolled {} ({}). Next: `aterm machines apply {}` (it installs aterm.app), \
                 then at its keyboard: aterm machines person {} --here",
                host.name, host.uuid, host.name, host.name
            );
            ExitCode::SUCCESS
        }
        // Why can carry what the Mac printed (its facts script, ssh, scp).
        Err(why) => {
            eprintln!("aterm machines enroll: {}", shown(&why));
            ExitCode::FAILURE
        }
    }
}

/// Enroll the Mac `req` names, installing the agent built from `exe`.
///
/// # Errors
/// One line; the step that failed says what it left.
pub fn enroll(
    sys: &dyn System,
    paths: &Paths,
    req: &Request,
    exe: &Path,
    out: &mut dyn Write,
) -> Result<Host, String> {
    let scratch =
        crate::port::Scratch::new(sys, "enroll").map_err(|e| format!("enroll scratch: {e}"))?;
    let dir = scratch.path();
    (|| {
        let key = scan_host_key(sys, &req.address, dir)?;
        check_host_key(&key, &req.trust)?;
        let _ = writeln!(out, "host key {} (ed25519)", key.fingerprint);
        let alias = "aterm-machines-enroll";
        let known = dir.join("known_hosts");
        sys.write_file(
            &known,
            known_hosts_with("", alias, Some(&key)).as_bytes(),
            0o600,
        )
        .map_err(|e| format!("{}: {e}", known.display()))?;
        let ctl = paths.control_path(&req.address);
        if let Some(parent) = ctl.parent() {
            sys.create_dir_all(parent, 0o700)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let master = Master {
            ctl,
            destination: format!("{}@{}", req.user, req.address),
        };
        let _ = writeln!(
            out,
            "ssh asks for {}'s password on {}:",
            req.user, req.address
        );
        match sys.run_tty(&master_cmd(&master, &known, alias)) {
            Ok(Some(0)) => {}
            Ok(code) => return Err(format!("ssh did not log in (status {code:?})")),
            Err(e) => return Err(format!("ssh: {e}")),
        }
        let host = enroll_through(sys, paths, req, exe, &master, &key, dir, out);
        close_master(sys, &master);
        host
    })()
}

/// Enrollment once a master to the host is open and its host key is known: facts, the
/// payload, the install command line, the pin, the first `hello`, the inventory.
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
    // Read before anything is installed: an inventory that cannot be read would refuse the
    // host only after the agent is on it.
    let inv = inventory::load(sys, paths)?;
    let facts = read_facts(sys, master)?;
    if let Some(known) = inv
        .hosts
        .iter()
        .find(|h| h.uuid.eq_ignore_ascii_case(&facts.uuid))
    {
        return Err(already_enrolled(known, &facts.uuid));
    }
    // The inventory name, checked here for the same reason: a name another Mac holds would
    // refuse the host only after the agent is on it.
    let host = Host {
        name: req
            .name
            .clone()
            .unwrap_or_else(|| facts.local_host_name.clone()),
        uuid: facts.uuid.clone(),
        address: req.address.clone(),
        user: req.user.clone(),
        baseline: None,
        users: Vec::new(),
    };
    inventory::check_host(&host).map_err(|why| format!("{why}; pass --name <n>"))?;
    if let Some(other) = inv
        .hosts
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(&host.name))
    {
        return Err(format!(
            "the name {} is taken by the Mac with uuid {} at {}; pass --name <n> for this one",
            host.name, other.uuid, other.address
        ));
    }
    let _ = writeln!(
        out,
        "{}: serial {} (check it against the case), {} {}, macOS {} ({}), {:?}",
        req.address,
        shown(&facts.serial),
        shown(&facts.model),
        shown(&facts.arch),
        shown(&facts.os_version),
        shown(&facts.os_build),
        facts.computer_name
    );
    let archive = build_archive(sys, exe, scratch)?;
    let signers = sys.read_file(&paths.allowed_signers()).map_err(|e| {
        format!(
            "{}: {e} (run `aterm machines init`)",
            paths.allowed_signers().display()
        )
    })?;
    let signers = String::from_utf8(signers).map_err(|_| "allowed_signers is not UTF-8")?;
    envelope::check_signers(&signers).map_err(|why| format!("the controller's {why}"))?;
    let signers_file = scratch.join("aterm-allowed-signers");
    sys.write_file(&signers_file, signers.as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", signers_file.display()))?;
    let signers_sha = sys
        .sha256(&signers_file)
        .map_err(|e| format!("{}: {e}", signers_file.display()))?;
    let fleet_pub = paths.ssh_key().with_extension("pub");
    let fleet_key = sys
        .read_file(&fleet_pub)
        .map_err(|e| format!("{}: {e} (run `aterm machines init`)", fleet_pub.display()))?;
    let fleet_key = String::from_utf8_lossy(&fleet_key).trim().to_owned();
    crate::agent::install::fleet_line(&fleet_key)?;
    let tag = &archive.sha256[..8];
    let remote_dir = facts.tmpdir.trim_end_matches('/').to_owned();
    let remote_archive = format!("{remote_dir}/aterm-agent-{tag}.zip");
    let remote_signers = format!("{remote_dir}/aterm-allowed-signers-{tag}");
    // scp names the copies by their local names; stage them under the remote names.
    let local_archive = scratch.join(format!("aterm-agent-{tag}.zip"));
    let local_signers = scratch.join(format!("aterm-allowed-signers-{tag}"));
    sys.rename(&archive.path, &local_archive)
        .map_err(|e| format!("{}: {e}", archive.path.display()))?;
    sys.rename(&signers_file, &local_signers)
        .map_err(|e| format!("{}: {e}", signers_file.display()))?;
    let _ = writeln!(
        out,
        "copying the agent ({}, sha256 {}) to {}",
        archive.exe.display(),
        archive.sha256,
        req.address
    );
    copy_payload(
        sys,
        master,
        &[&local_archive, &local_signers],
        &format!("{remote_dir}/"),
    )?;
    let script = install_script(&Install {
        archive: &archive,
        remote_archive,
        remote_signers,
        signers_sha: signers_sha.clone(),
        admin: req.user.clone(),
        fleet_key,
    });
    let _ = writeln!(
        out,
        "sudo asks for {}'s password on {}:",
        req.user, req.address
    );
    match sys.run_tty(&remote_sh(master, &script, true)) {
        Ok(Some(0)) => {}
        Ok(code) => {
            return Err(format!(
                "the install command line failed (status {code:?}); nothing outside \
                 /Library/Management/aterm/.staging is changed unless `agent install` said so"
            ));
        }
        Err(e) => return Err(format!("ssh -t: {e}")),
    }
    pin_host_key(sys, paths, &facts.uuid, key)?;
    match first_hello(sys, paths, &host) {
        Ok(hello) => {
            let _ = writeln!(
                out,
                "the fleet key's forced command answers: agent {} on {}",
                shown(&hello.agent_version),
                hello.uuid
            );
            // A state file that is there and cannot be read is left as it is, never replaced
            // by a fresh one: it may hold GitHub key ids.
            match inventory::load_state(sys, paths, &host.uuid) {
                Ok(mut state) => {
                    state.last_seen = sys.now();
                    state.hello = aterm_json::to_string(&hello).ok();
                    state.signers_sha256 = Some(signers_sha.clone());
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
                "warning: the agent is installed, but the fleet key's first hello did not \
                 answer: {}",
                shown(&why)
            );
        }
    }
    inventory::upsert_host(sys, paths, &host)?;
    Ok(host)
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
        return Err(ssh::classify(&ran).map_or_else(|| ran.describe(), |f| f.why()));
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
        for row in &report.items {
            let _ = writeln!(
                out,
                "  {:?} {}{}",
                row.outcome,
                row.id,
                row.why
                    .as_ref()
                    .map(|w| format!(": {w}"))
                    .unwrap_or_default()
            );
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
    fn enroll_takes_one_address_an_admin_and_exactly_one_way_to_trust_the_key() {
        let r = parse_args(&args(&[
            "lab-07.local",
            "--user",
            "labadmin",
            "--fingerprint",
            "0Tdy7Goos0XG",
        ]))
        .unwrap();
        assert_eq!(r.trust, Trust::Fingerprint("SHA256:0Tdy7Goos0XG".into()));
        assert!(parse_args(&args(&["lab-07.local", "--user", "labadmin"])).is_err());
        assert!(
            parse_args(&args(&[
                "lab-07.local",
                "--user",
                "a",
                "--trust-first-use",
                "--fingerprint",
                "x"
            ]))
            .is_err()
        );
        assert!(
            parse_args(&args(&[
                "-oProxyCommand=x",
                "--user",
                "a",
                "--trust-first-use"
            ]))
            .is_err()
        );
        assert!(parse_args(&args(&["h", "--user", "a b", "--trust-first-use"])).is_err());
        assert!(parse_args(&args(&["h", "i", "--user", "a", "--trust-first-use"])).is_err());
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

    #[test]
    fn the_install_line_hashes_the_root_owned_copies_before_it_executes_anything() {
        let a = archive(true);
        let s = install_script(&Install {
            archive: &a,
            remote_archive: "/var/folders/yt/x/T/aterm-agent-abababab.zip".into(),
            remote_signers: "/var/folders/yt/x/T/aterm-allowed-signers-abababab".into(),
            signers_sha: "cd".repeat(32),
            admin: "admin".into(),
            fleet_key: "ssh-ed25519 AAAA fleet@ctl".into(),
        });
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
            "/usr/bin/sudo /Library/Management/aterm/.staging/payload/aterm.app/Contents/MacOS/aterm machines agent install --admin admin --fleet-key 'ssh-ed25519 AAAA fleet@ctl'",
        );
        assert!(
            copy < hash && hash < expand && expand < sign && sign < run,
            "{s}"
        );
        // The user-writable copy is never what runs.
        assert!(!s.contains("/usr/bin/sudo /var/folders"), "{s}");
        assert!(s.starts_with("set -e\n"));
        let dev = install_script(&Install {
            archive: &archive(false),
            remote_archive: "/t/a.zip".into(),
            remote_signers: "/t/s".into(),
            signers_sha: "cd".repeat(32),
            admin: "admin".into(),
            fleet_key: "ssh-ed25519 AAAA fleet@ctl".into(),
        });
        assert!(!dev.contains("codesign"), "{dev}");
        assert!(dev.contains("/usr/bin/sudo /Library/Management/aterm/.staging/payload/aterm-machines-dev machines agent install"));
    }

    #[test]
    fn the_master_trusts_only_the_vouched_key_and_takes_passwords_alone() {
        let m = Master {
            ctl: "/c/ctl/0123abcd".into(),
            destination: "admin@192.168.64.2".into(),
        };
        let line = master_cmd(&m, Path::new("/s/known_hosts"), "aterm-machines-enroll").display();
        for want in [
            "-F /dev/null",
            "-M -S /c/ctl/0123abcd",
            "ControlPersist=120",
            "StrictHostKeyChecking=yes",
            "UserKnownHostsFile=/s/known_hosts",
            "HostKeyAlias=aterm-machines-enroll",
            "PubkeyAuthentication=no",
            "-- admin@192.168.64.2 true",
        ] {
            assert!(line.contains(want), "{want}: {line}");
        }
        let remote = remote_sh(&m, "echo ===", false).display();
        assert!(
            remote.ends_with(r"-- admin@192.168.64.2 '/bin/sh -c '\''echo ==='\'''"),
            "{remote}"
        );
        assert!(remote.contains("ControlMaster=no") && remote.contains("BatchMode=yes"));
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
        sys.calls()
            .iter()
            .any(|c| c.contains("ditto") || c.contains("scp") || c.starts_with("tty "))
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
