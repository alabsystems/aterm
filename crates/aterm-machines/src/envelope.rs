// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed envelope: the only authority the root agent acts on (design §3.3).

use crate::model::{Mode, Read};
use crate::port::{Cmd, Scratch, System};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The first line of an SSHSIG armored signature.
pub const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----\n";

/// The last line of an SSHSIG armored signature.
pub const END: &str = "-----END SSH SIGNATURE-----\n";

/// How far ahead of the host's clock `issued` may be (a controller whose clock runs fast).
pub const SKEW_SECS: u64 = 300;

/// The largest envelope the agent reads from standard input.
pub const MAX_WIRE: usize = 8 << 20;

/// The tool every signature goes through.
const SSH_KEYGEN: &str = aterm_uds::os_path!("ssh-keygen");

/// What an envelope authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Probe and plan the baseline.
    Plan,
    /// Probe, apply and verify the baseline.
    Apply,
    /// Gather the root-only facts.
    Facts,
    /// Run one argv (design §4.6).
    Exec,
    /// Replace the agent (design §4.5).
    Upgrade,
    /// Replace `allowed_signers`.
    Rekey,
    /// Remove the agent and everything enrollment wrote.
    Uninstall,
}

impl Action {
    /// The engine mode a baseline action runs in.
    #[must_use]
    pub const fn mode(self) -> Option<Mode> {
        match self {
            Self::Plan => Some(Mode::Plan),
            Self::Apply => Some(Mode::Apply),
            _ => None,
        }
    }
}

/// Whether changes may disrupt someone at the Mac (design §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Disrupt {
    /// Only when nobody is at the console or it has been idle `idle_secs`.
    #[default]
    WhenIdle,
    /// Now.
    Now,
}

/// An artifact the envelope expects in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// SHA-256, hex.
    pub sha256: String,
    /// What it is, for messages.
    pub name: String,
}

/// `exec`'s payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exec {
    /// Absolute program, then arguments.
    pub argv: Vec<String>,
    /// Run as this local account; `None` runs as root.
    #[serde(default)]
    pub user: Option<String>,
    /// Seconds before the run is killed.
    pub budget_secs: u64,
}

/// `upgrade`'s payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upgrade {
    /// The archive's SHA-256 (it is in the inbox).
    pub sha256: String,
    /// `app-zip` (a zipped aterm.app, Developer ID checked) or `dev-zip` (a zipped dev bundle or binary, trusted on this signed hash alone).
    pub kind: String,
    /// The version it carries.
    pub version: String,
}

/// An envelope's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// The IOPlatformUUID of the one host this envelope is for.
    pub host: String,
    /// The inventory name, for reports.
    pub host_name: String,
    /// Must exceed the signer's floor.
    pub serial: u64,
    /// Unix seconds.
    pub issued: u64,
    /// Unix seconds; refused after.
    pub expires: u64,
    /// What it authorizes.
    pub action: Action,
    /// Disruption policy.
    #[serde(default)]
    pub disrupt: Disrupt,
    /// Idle seconds that count as "nobody is using it".
    #[serde(default = "default_idle")]
    pub idle_secs: u64,
    /// The resolved baseline (plan, apply).
    #[serde(default)]
    pub baseline: Option<String>,
    /// Artifacts the inbox must hold.
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    /// `exec`'s payload.
    #[serde(default)]
    pub exec: Option<Exec>,
    /// `upgrade`'s payload.
    #[serde(default)]
    pub upgrade: Option<Upgrade>,
    /// `rekey`'s payload: the new `allowed_signers` text.
    #[serde(default)]
    pub rekey: Option<String>,
}

const fn default_idle() -> u64 {
    900
}

/// Why the agent refused an envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not an envelope this agent reads.
    Malformed(String),
    /// No principal in `allowed_signers` made this signature.
    BadSignature(String),
    /// For another host.
    WrongHost {
        /// The host it names.
        named: String,
    },
    /// Its serial does not exceed the signer's floor.
    Replay {
        /// The signer.
        principal: String,
        /// Its serial.
        serial: u64,
        /// The floor.
        floor: u64,
    },
    /// Past `expires` (or issued beyond the allowed skew ahead of the host's clock).
    Expired {
        /// When it was issued.
        issued: u64,
        /// Until when it held.
        expires: u64,
        /// The host's clock.
        now: u64,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed envelope: {why}"),
            Self::BadSignature(why) => write!(f, "bad-signature: {why}"),
            Self::WrongHost { named } => write!(f, "wrong-host: the envelope is for {named}"),
            Self::Replay {
                principal,
                serial,
                floor,
            } => write!(
                f,
                "replay principal={principal} serial={serial} floor={floor}"
            ),
            Self::Expired {
                issued,
                expires,
                now,
            } => {
                write!(
                    f,
                    "expired issued={issued} expires={expires} host-clock={now}"
                )
            }
        }
    }
}

/// A verified envelope and who signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The body.
    pub envelope: Envelope,
    /// The `allowed_signers` principal that signed it.
    pub principal: String,
    /// The signing key's fingerprint (`SHA256:…`), as `ssh-keygen -Y verify` names it; empty when it printed none.
    pub key_id: String,
}

/// Split wire bytes into the armored signature (its `BEGIN` line through its `END` line and newline, exactly what `ssh-keygen -Y sign` wrote) and the body after it.
/// # Errors
/// [`Refusal::Malformed`] when there is no complete signature block.
pub fn split(wire: &[u8]) -> Result<(&[u8], &[u8]), Refusal> {
    if !wire.starts_with(BEGIN.as_bytes()) {
        return Err(Refusal::Malformed(
            "it does not start with an SSH signature".into(),
        ));
    }
    let end = END.as_bytes();
    let at = wire
        .windows(end.len())
        .position(|w| w == end)
        .ok_or_else(|| Refusal::Malformed("the SSH signature has no end line".into()))?;
    let cut = at + end.len();
    let (sig, body) = wire.split_at(cut);
    let inner = &sig[BEGIN.len()..at];
    if inner.is_empty()
        || !inner
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\n'))
    {
        return Err(Refusal::Malformed(
            "the SSH signature armor is not base64".into(),
        ));
    }
    if body.is_empty() {
        return Err(Refusal::Malformed(
            "there is no body after the signature".into(),
        ));
    }
    Ok((sig, body))
}

/// The body text `seal` signs: the envelope as one TOML document.
/// # Errors
/// The serializer's message.
pub fn body_text(envelope: &Envelope) -> Result<String, String> {
    aterm_toml::to_string(envelope).map_err(|e| format!("envelope: {e}"))
}

/// Parse a body.
/// # Errors
/// [`Refusal::Malformed`] for anything but a schema-1 envelope with no unknown key.
pub fn parse_body(body: &[u8]) -> Result<Envelope, Refusal> {
    let text = std::str::from_utf8(body)
        .map_err(|_| Refusal::Malformed("the body is not UTF-8".into()))?;
    let envelope: Envelope = aterm_toml::from_str(text)
        .map_err(|e| Refusal::Malformed(format!("the body does not parse: {e}")))?;
    if envelope.schema != crate::SCHEMA {
        return Err(Refusal::Malformed(format!(
            "schema {} (this agent reads schema {})",
            envelope.schema,
            crate::SCHEMA
        )));
    }
    Ok(envelope)
}

/// How `ssh-keygen -Y sign` reaches the signing key, and so how many times the owner is asked for something per batch of N envelopes (one `ssh-keygen` signs the whole batch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signing {
    /// A bounded run with no terminal ([`System::run`]): for a key with no passphrase, so automation on the controller can apply.
    Unattended,
    /// On this process's terminal ([`System::run_tty`]): `ssh-keygen` asks the passphrase once for the whole batch.
    Terminal,
    /// The key is loaded in ssh-agent (`-U`, naming the `.pub`): a bounded run handed this process's `SSH_AUTH_SOCK`, with no terminal and no passphrase prompt (the owner ran `ssh-add` once).
    Agent,
}

/// Whether the private key at `key` is protected by a passphrase: `ssh-keygen -y -P ''` prints the public key of an unprotected key, and `incorrect passphrase supplied` for a protected one.
#[must_use]
pub fn needs_passphrase(sys: &dyn System, key: &Path) -> Read<bool> {
    let ran = sys.run(
        &Cmd::new(SSH_KEYGEN)
            .args(["-y", "-P", "", "-f"])
            .arg(key.as_os_str()),
    );
    match ran {
        Ok(r) if r.success() => Read::Value(false),
        Ok(r) if r.err().contains("incorrect passphrase") => Read::Value(true),
        Ok(r) => Read::Unusable(format!("ssh-keygen -y: {}", r.describe())),
        Err(e) => Read::Unusable(format!("ssh-keygen -y: {e}")),
    }
}

/// Sign `body` with the controller key at `key_path` (`ssh-keygen -Y sign`): the wire bytes.
/// # Errors
/// One line when `ssh-keygen` does not sign.
pub fn seal(
    sys: &dyn System,
    body: &Envelope,
    key_path: &std::path::Path,
) -> Result<Vec<u8>, String> {
    let how = match needs_passphrase(sys, key_path) {
        Read::Value(false) => Signing::Unattended,
        Read::Value(true) => Signing::Terminal,
        Read::Absent => return Err(format!("envelope: {}: no key", key_path.display())),
        Read::Unusable(why) => return Err(format!("envelope: {}: {why}", key_path.display())),
    };
    let scratch = fresh_scratch(sys, "seal").map_err(|why| format!("envelope: {why}"))?;
    let wires = seal_batch(
        sys,
        std::slice::from_ref(body),
        key_path,
        how,
        scratch.path(),
        None,
    );
    wires?
        .pop()
        .ok_or_else(|| "envelope: ssh-keygen signed nothing".to_owned())
}

/// Allocate owned scratch without adopting or removing any preexisting path.
/// # Errors
/// One line when the backend cannot exclusively allocate a directory.
pub fn fresh_scratch<'s>(sys: &'s dyn System, what: &str) -> Result<Scratch<'s>, String> {
    Scratch::new(sys, what).map_err(|e| format!("envelope scratch: {e}"))
}

/// Sign every body in `bodies` with ONE `ssh-keygen -Y sign` run, writing the scratch bodies and signatures in `scratch` (a directory only this user may write). A [`Signing::Agent`] run reaches the agent at `agent_sock` (the caller's `SSH_AUTH_SOCK`; this process's environment is not read).
/// # Errors
/// One line when the scratch directory is not private, an agent run has no `agent_sock`, or `ssh-keygen` does not sign.
pub fn seal_batch(
    sys: &dyn System,
    bodies: &[Envelope],
    key: &Path,
    how: Signing,
    scratch: &Path,
    agent_sock: Option<&OsStr>,
) -> Result<Vec<Vec<u8>>, String> {
    if bodies.is_empty() {
        return Ok(Vec::new());
    }
    private_dir(sys, scratch).map_err(|why| format!("envelope: {why}"))?;
    let tag = std::process::id();
    let mut files: Vec<(PathBuf, String)> = Vec::with_capacity(bodies.len());
    let cleanup = |files: &[(PathBuf, String)]| {
        for (f, _) in files {
            let _ = sys.remove(f);
            let _ = sys.remove(&sig_path(f));
        }
    };
    for (i, body) in bodies.iter().enumerate() {
        if body.expires < body.issued {
            cleanup(&files);
            return Err(format!(
                "envelope: expires {} is before issued {}",
                body.expires, body.issued
            ));
        }
        let text = body_text(body)?;
        let path = scratch.join(format!("aterm-envelope-{tag}-{i}.toml"));
        let _ = sys.remove(&sig_path(&path));
        if let Err(e) = sys.write_file(&path, text.as_bytes(), 0o600) {
            cleanup(&files);
            return Err(format!("envelope: {}: {e}", path.display()));
        }
        files.push((path, text));
    }
    let mut cmd = Cmd::new(SSH_KEYGEN).args(["-Y", "sign"]);
    cmd = match how {
        Signing::Agent => {
            let mut public = key.as_os_str().to_owned();
            public.push(".pub");
            cmd.arg("-U").arg("-f").arg(public)
        }
        Signing::Unattended | Signing::Terminal => cmd.arg("-f").arg(key.as_os_str()),
    };
    cmd = cmd
        .args(["-n", crate::protocol::SIG_NAMESPACE])
        .args(files.iter().map(|(f, _)| f.as_os_str().to_owned()));
    let signed = match how {
        Signing::Unattended | Signing::Agent => {
            if how == Signing::Agent {
                let Some(sock) = agent_sock.filter(|sock| !sock.is_empty()) else {
                    cleanup(&files);
                    return Err("envelope: no ssh-agent: SSH_AUTH_SOCK is not set".into());
                };
                cmd = cmd
                    .secret_env("SSH_AUTH_SOCK", sock.to_string_lossy())
                    .budget(Duration::from_secs(120));
            }
            sys.run(&cmd).map_err(|e| e.to_string()).and_then(|r| {
                if r.success() {
                    Ok(())
                } else {
                    Err(r.describe())
                }
            })
        }
        Signing::Terminal => sys
            .run_tty(&cmd, &mut |_| Ok(()))
            .map_err(|e| e.to_string())
            .and_then(|s| match s {
                Some(0) => Ok(()),
                Some(n) => Err(format!("exit {n}")),
                None => Err("killed by a signal".to_owned()),
            }),
    };
    if let Err(why) = signed {
        cleanup(&files);
        return Err(format!("envelope: ssh-keygen -Y sign did not sign: {why}"));
    }
    let mut wires = Vec::with_capacity(files.len());
    for (path, text) in &files {
        let sig = match sys.read_file(&sig_path(path)) {
            Ok(sig) => sig,
            Err(e) => {
                cleanup(&files);
                return Err(format!(
                    "envelope: ssh-keygen wrote no {}: {e}",
                    sig_path(path).display()
                ));
            }
        };
        let mut wire = sig;
        wire.extend_from_slice(text.as_bytes());
        if let Err(why) = split(&wire) {
            cleanup(&files);
            return Err(format!("envelope: ssh-keygen wrote no signature: {why}"));
        }
        wires.push(wire);
    }
    cleanup(&files);
    Ok(wires)
}

/// `<file>.sig`, where `ssh-keygen -Y sign` writes the signature of `<file>`.
fn sig_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

/// Refuse a scratch directory someone else could write into between our write and
/// `ssh-keygen`'s read: it must be a real directory owned by this user (or root) and not
/// writable by group or others.
fn private_dir(sys: &dyn System, dir: &Path) -> Result<(), String> {
    let st = sys
        .stat(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let euid = sys.euid();
    if !st.dir || st.symlink {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if st.uid != euid && st.uid != 0 {
        return Err(format!(
            "{} belongs to uid {}, not this user",
            dir.display(),
            st.uid
        ));
    }
    if st.mode & 0o022 != 0 {
        return Err(format!(
            "{} is writable by others (mode {:o})",
            dir.display(),
            st.mode
        ));
    }
    Ok(())
}

/// Verify `wire` against `allowed_signers`, this host's `uuid`, the clock and the floors, with the scratch files in a fresh 0700 directory under this user's temporary directory.
/// # Errors
/// The [`Refusal`].
pub fn open(
    sys: &dyn System,
    wire: &[u8],
    allowed_signers: &std::path::Path,
    uuid: &str,
    floor_of: &dyn Fn(&str) -> u64,
) -> Result<Opened, Refusal> {
    let scratch = fresh_scratch(sys, "open").map_err(Refusal::Malformed)?;
    open_in(sys, wire, allowed_signers, uuid, floor_of, scratch.path())
}

/// [`open`], with the signature and body written to `scratch` for `ssh-keygen` (the agent passes its root-owned stage directory).
/// # Errors
/// The [`Refusal`].
pub fn open_in(
    sys: &dyn System,
    wire: &[u8],
    allowed_signers: &Path,
    uuid: &str,
    floor_of: &dyn Fn(&str) -> u64,
    scratch: &Path,
) -> Result<Opened, Refusal> {
    if wire.len() > MAX_WIRE {
        return Err(Refusal::Malformed(format!(
            "{} bytes is more than the {MAX_WIRE} an envelope may hold",
            wire.len()
        )));
    }
    let (sig, body) = split(wire)?;
    private_dir(sys, scratch).map_err(|why| Refusal::Malformed(format!("scratch: {why}")))?;
    let tag = std::process::id();
    let sig_file = scratch.join(format!("aterm-open-{tag}.sig"));
    let body_file = scratch.join(format!("aterm-open-{tag}.body"));
    let verified = (|| {
        sys.write_file(&sig_file, sig, 0o600)
            .map_err(|e| Refusal::Malformed(format!("scratch: {e}")))?;
        sys.write_file(&body_file, body, 0o600)
            .map_err(|e| Refusal::Malformed(format!("scratch: {e}")))?;
        verify_files(sys, &sig_file, &body_file, allowed_signers)
    })();
    let _ = sys.remove(&sig_file);
    let _ = sys.remove(&body_file);
    let (principal, key_id) = verified?;
    let envelope = parse_body(body)?;
    if uuid.is_empty() || !envelope.host.eq_ignore_ascii_case(uuid) {
        return Err(Refusal::WrongHost {
            named: envelope.host,
        });
    }
    let floor = floor_of(&principal);
    if envelope.serial <= floor {
        return Err(Refusal::Replay {
            principal,
            serial: envelope.serial,
            floor,
        });
    }
    let now = sys.now();
    if envelope.expires < envelope.issued
        || envelope.issued > now.saturating_add(SKEW_SECS)
        || now > envelope.expires
    {
        return Err(Refusal::Expired {
            issued: envelope.issued,
            expires: envelope.expires,
            now,
        });
    }
    Ok(Opened {
        envelope,
        principal,
        key_id,
    })
}

/// `find-principals`, then `verify -I <principal>`: who signed, and the key's fingerprint.
fn verify_files(
    sys: &dyn System,
    sig: &Path,
    body: &Path,
    allowed_signers: &Path,
) -> Result<(String, String), Refusal> {
    let found = sys
        .run(
            &Cmd::new(SSH_KEYGEN)
                .args(["-Y", "find-principals", "-s"])
                .arg(sig.as_os_str())
                .arg("-f")
                .arg(allowed_signers.as_os_str()),
        )
        .map_err(|e| Refusal::BadSignature(format!("ssh-keygen did not run: {e}")))?;
    if !found.success() {
        return Err(Refusal::BadSignature(first_line(&found)));
    }
    let out = found.out();
    let principals: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let principal = match principals.as_slice() {
        [one] => (*one).to_owned(),
        [] => return Err(Refusal::BadSignature("no principal matched".into())),
        many => {
            return Err(Refusal::BadSignature(format!(
                "the key is listed under several principals ({}); allowed_signers must \
                 name each key once",
                many.join(", ")
            )));
        }
    };
    if !principal_ok(&principal) {
        return Err(Refusal::BadSignature(format!(
            "principal {principal:?} is not a plain name"
        )));
    }
    let verified = sys
        .run(
            &Cmd::new(SSH_KEYGEN)
                .args(["-Y", "verify", "-f"])
                .arg(allowed_signers.as_os_str())
                .args(["-I", principal.as_str()])
                .args(["-n", crate::protocol::SIG_NAMESPACE, "-s"])
                .arg(sig.as_os_str())
                .stdin_file(body),
        )
        .map_err(|e| Refusal::BadSignature(format!("ssh-keygen did not run: {e}")))?;
    let good = format!(
        "Good \"{}\" signature for {principal} ",
        crate::protocol::SIG_NAMESPACE
    );
    let out = verified.out();
    if !verified.success() || !out.starts_with(&good) {
        return Err(Refusal::BadSignature(first_line(&verified)));
    }
    let key_id = key_id(&out).unwrap_or_default();
    Ok((principal, key_id))
}

/// The most telling line `ssh-keygen` printed: standard error's first line (the reason),
/// else standard output's.
fn first_line(ran: &crate::port::Ran) -> String {
    let err = ran.err();
    let line = err
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            ran.out()
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(str::to_owned)
        });
    line.unwrap_or_else(|| ran.describe())
}

/// The key fingerprint in `ssh-keygen -Y verify`'s `Good … with ED25519 key SHA256:…` line.
#[must_use]
pub fn key_id(verify_out: &str) -> Option<String> {
    verify_out
        .split_whitespace()
        .find(|w| w.starts_with("SHA256:"))
        .map(str::to_owned)
}

/// Whether `p` is a principal this crate writes and reads back: no space, quote, comma or control character, so it is one word in `allowed_signers`, in an argv and in `floors.toml`.
#[must_use]
pub fn principal_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 128
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'-' | b'+'))
}

/// The key types `allowed_signers` may name.
const KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "sk-ssh-ed25519@openssh.com",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "ssh-rsa",
];

/// One `allowed_signers` line for `principal`, from a public key line (`<type> <base64> [comment]`, as a `.pub` file holds it).
/// # Errors
/// One line when the principal or the key is not one this crate accepts.
pub fn signers_line(principal: &str, public_key: &str) -> Result<String, String> {
    if !principal_ok(principal) {
        return Err(format!("principal {principal:?} is not a plain name"));
    }
    let (kind, blob) = public_key_parts(public_key)?;
    Ok(format!(
        "{principal} namespaces=\"{}\" {kind} {blob}",
        crate::protocol::SIG_NAMESPACE
    ))
}

/// The type and base64 blob of a public key line.
/// # Errors
/// One line when it is not `<known type> <base64>`.
pub fn public_key_parts(line: &str) -> Result<(&str, &str), String> {
    let mut words = line.split_whitespace();
    let (Some(kind), Some(blob)) = (words.next(), words.next()) else {
        return Err("not a public key line (`<type> <base64> [comment]`)".into());
    };
    if !KEY_TYPES.contains(&kind) {
        return Err(format!(
            "key type {kind:?} is not one allowed_signers takes"
        ));
    }
    if blob.len() < 16
        || !blob
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err("the key's base64 is not base64".into());
    }
    Ok((kind, blob))
}

/// Check an `allowed_signers` document the way the agent writes it: every line `<principal> namespaces="aterm-machines" <type> <base64>`, each key once, at least one line.
/// # Errors
/// One line naming the first bad line.
pub fn check_signers(text: &str) -> Result<Vec<String>, String> {
    let options = format!("namespaces=\"{}\"", crate::protocol::SIG_NAMESPACE);
    let mut principals = Vec::new();
    let mut blobs: Vec<String> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = line.split(' ').filter(|w| !w.is_empty()).collect();
        let [principal, opts, kind, blob] = words.as_slice() else {
            return Err(format!(
                "allowed_signers line {}: want `<principal> {options} <type> <base64>`",
                n + 1
            ));
        };
        if !principal_ok(principal) {
            return Err(format!(
                "allowed_signers line {}: principal {principal:?} is not a plain name",
                n + 1
            ));
        }
        if *opts != options {
            return Err(format!(
                "allowed_signers line {}: the options must be exactly {options}",
                n + 1
            ));
        }
        public_key_parts(&format!("{kind} {blob}"))
            .map_err(|why| format!("allowed_signers line {}: {why}", n + 1))?;
        if blobs.iter().any(|b| b == blob) {
            return Err(format!(
                "allowed_signers line {}: this key is already listed",
                n + 1
            ));
        }
        blobs.push((*blob).to_owned());
        principals.push((*principal).to_owned());
    }
    if principals.is_empty() {
        return Err("allowed_signers names no key: nobody could sign for this host".into());
    }
    Ok(principals)
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
