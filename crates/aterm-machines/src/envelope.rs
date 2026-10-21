// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed envelope: the only authority the root agent acts on (design §3.3).
//!
//! Wire form: an SSHSIG armored signature (`ssh-keygen -Y sign -n aterm-machines`) followed
//! by the TOML body it signs. The controller signs with `/usr/bin/ssh-keygen`; the agent
//! verifies with Apple's `/usr/bin/ssh-keygen -Y find-principals` and `-Y verify` against its
//! root-owned `allowed_signers`. No signing or verifying cryptography lives in this crate.
//!
//! What OpenSSH 10.3 (this Mac's and the macOS 27 VM's `/usr/bin/ssh-keygen`) answers, as
//! measured with throwaway keys:
//!
//! * `-Y sign -f <key> -n aterm-machines <file>…` signs every file it is given with ONE load
//!   of the key, writing `<file>.sig` beside each and `Signing file <f>` / `Write signature
//!   to <f>.sig` on standard error. A passphrase is therefore asked once per batch. A
//!   `<file>.sig` that already exists makes it ask `Overwrite (y/n)?`, so it is removed
//!   first.
//! * `-Y find-principals -s <sig> -f <allowed_signers>` prints the matching principal on
//!   standard output and exits 0, or prints `No principal matched.` and exits 255. It does
//!   not look at `namespaces=`; `-Y verify` does.
//! * `-Y verify -f <allowed_signers> -I <principal> -n aterm-machines -s <sig>` reads the
//!   message on standard input. It prints `Good "aterm-machines" signature for <principal>
//!   with ED25519 key SHA256:<fp>` on standard output and exits 0, or prints `Could not
//!   verify signature.` (the reason, e.g. `Signature verification failed: incorrect
//!   signature`, on standard error) and exits 255.

use crate::model::{Mode, Read};
use crate::port::{Cmd, Scratch, System};
use serde::{Deserialize, Serialize};
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
const SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";

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
    /// `app-zip` (a zipped aterm.app, Developer ID checked) or `dev-zip` (a zipped dev
    /// bundle or binary, trusted on this signed hash alone).
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
    /// The signing key's fingerprint (`SHA256:…`), as `ssh-keygen -Y verify` names it;
    /// empty when it printed none.
    pub key_id: String,
}

/// Split wire bytes into the armored signature (its `BEGIN` line through its `END` line and
/// newline, exactly what `ssh-keygen -Y sign` wrote) and the body after it.
///
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
    // The armor between the lines is base64 and newlines only; anything else is not a
    // signature ssh-keygen wrote.
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
///
/// # Errors
/// The serializer's message.
pub fn body_text(envelope: &Envelope) -> Result<String, String> {
    aterm_toml::to_string(envelope).map_err(|e| format!("envelope: {e}"))
}

/// Parse a body. Only ever called on bytes a signature has been verified over.
///
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

/// How `ssh-keygen -Y sign` reaches the signing key, and so how many times the owner is
/// asked for something per batch of N envelopes (one `ssh-keygen` signs the whole batch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signing {
    /// A bounded run with no terminal ([`System::run`]): for a key with no passphrase,
    /// so automation on the controller can apply. No prompt. A FIDO key's touch is still
    /// asked for once per envelope, on the key itself.
    Unattended,
    /// On this process's terminal ([`System::run_tty`]): `ssh-keygen` asks the passphrase
    /// once for the whole batch. A FIDO key: one touch per envelope.
    Terminal,
    /// The key is loaded in ssh-agent (`-U`, naming the `.pub`): a bounded run handed this
    /// process's `SSH_AUTH_SOCK`, with no terminal and no passphrase prompt (the owner ran
    /// `ssh-add` once). A FIDO key: one touch per envelope.
    Agent,
}

/// Whether the private key at `key` is protected by a passphrase: `ssh-keygen -y -P ''`
/// prints the public key of an unprotected key, and `incorrect passphrase supplied` for a
/// protected one.
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
/// Unattended when the key has no passphrase, on the terminal otherwise; the scratch files
/// live in a fresh 0700 directory under this user's temporary directory.
///
/// # Errors
/// One line when `ssh-keygen` does not sign.
pub fn seal(
    sys: &dyn System,
    body: &Envelope,
    key_path: &std::path::Path,
) -> Result<Vec<u8>, String> {
    // Tri-state: a probe that did not answer is not "no passphrase".
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
    );
    wires?
        .pop()
        .ok_or_else(|| "envelope: ssh-keygen signed nothing".to_owned())
}

/// Allocate owned scratch without adopting or removing any preexisting path.
///
/// # Errors
/// One line when the backend cannot exclusively allocate a directory.
pub fn fresh_scratch<'s>(sys: &'s dyn System, what: &str) -> Result<Scratch<'s>, String> {
    Scratch::new(sys, what).map_err(|e| format!("envelope scratch: {e}"))
}

/// Sign every body in `bodies` with ONE `ssh-keygen -Y sign` run, writing the scratch
/// bodies and signatures in `scratch` (a directory only this user may write). Returns the
/// wire bytes in input order.
///
/// # Errors
/// One line when the scratch directory is not private or `ssh-keygen` does not sign.
pub fn seal_batch(
    sys: &dyn System,
    bodies: &[Envelope],
    key: &Path,
    how: Signing,
    scratch: &Path,
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
                let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") else {
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
            .run_tty(&cmd)
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

/// Verify `wire` against `allowed_signers`, this host's `uuid`, the clock and the floors,
/// with the scratch files in a fresh 0700 directory under this user's temporary directory. Does not raise any floor:
/// the caller does, before it acts.
///
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

/// [`open`], with the signature and body written to `scratch` for `ssh-keygen` (the agent
/// passes its root-owned stage directory).
///
/// Nothing in the body is read before `ssh-keygen -Y verify` has accepted the signature
/// over it (design §3.2).
///
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
    if uuid.is_empty() || envelope.host != uuid {
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
    if envelope.issued > now.saturating_add(SKEW_SECS) || now > envelope.expires {
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

/// Whether `p` is a principal this crate writes and reads back: no space, quote, comma or
/// control character, so it is one word in `allowed_signers`, in an argv and in
/// `floors.toml`.
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

/// One `allowed_signers` line for `principal`, from a public key line (`<type> <base64>
/// [comment]`, as a `.pub` file holds it).
///
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
///
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

/// Check an `allowed_signers` document the way the agent writes it: every line
/// `<principal> namespaces="aterm-machines" <type> <base64>`, each key once, at least one
/// line. Returns the principals in order. Rekey and install refuse anything else, so a
/// typo can never lock the fleet out or widen who may sign.
///
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
mod tests {
    use super::*;
    use crate::port::{Fake, Live, Ran, Stat};

    fn envelope(host: &str, serial: u64, issued: u64) -> Envelope {
        Envelope {
            schema: 1,
            host: host.into(),
            host_name: "lab-07".into(),
            serial,
            issued,
            expires: issued + 86_400,
            action: Action::Apply,
            disrupt: Disrupt::WhenIdle,
            idle_secs: 900,
            baseline: Some("schema = 1\nusers = [\"admin\"]\n".into()),
            artifacts: vec![ArtifactRef {
                sha256: "ab".repeat(32),
                name: "app.Emacs".into(),
            }],
            exec: None,
            upgrade: None,
            rekey: None,
        }
    }

    /// A signature `ssh-keygen -Y sign` wrote (OpenSSH 10.3, a throwaway key).
    const SIG: &str = "-----BEGIN SSH SIGNATURE-----\n\
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgSwtZ7v55h6ZC7I/f6ieVoCUIc1\n\
UurXKiMHgI/I9bjJEAAAAOYXRlcm0tbWFjaGluZXMAAAAAAAAABnNoYTUxMgAAAFMAAAAL\n\
c3NoLWVkMjU1MTkAAABAmFAIRmmKIXpRv6CqO3Mn1cCVmjnKSGPwiGfkzT1hhEnzXPFB7F\n\
h+C/7UIZ5kDs26vTm+z6W30uFz3DtGTzPRBw==\n\
-----END SSH SIGNATURE-----\n";

    #[test]
    fn split_cuts_after_the_signature_end_line() {
        let wire = format!("{SIG}schema = 1\n");
        let (sig, body) = split(wire.as_bytes()).unwrap();
        assert_eq!(sig, SIG.as_bytes());
        assert_eq!(body, b"schema = 1\n");
    }

    #[test]
    fn split_refuses_what_is_not_a_signature_followed_by_a_body() {
        for wire in [
            "schema = 1\n".to_owned(),
            format!(" {SIG}schema = 1\n"),
            SIG.replace(END, ""),
            SIG.to_owned(),
            format!("{BEGIN}not base64 at all!\n{END}schema = 1\n"),
            format!("{BEGIN}{END}schema = 1\n"),
        ] {
            assert!(
                matches!(split(wire.as_bytes()), Err(Refusal::Malformed(_))),
                "{wire:?}"
            );
        }
    }

    #[test]
    fn a_body_round_trips_through_its_toml_and_unknown_keys_are_refused() {
        let e = envelope("U", 3, 1_800_000_000);
        let text = body_text(&e).unwrap();
        assert_eq!(parse_body(text.as_bytes()).unwrap(), e);
        let extra = format!("{text}surprise = true\n");
        assert!(matches!(
            parse_body(extra.as_bytes()),
            Err(Refusal::Malformed(_))
        ));
        let wrong = text.replace("schema = 1", "schema = 2");
        let err = parse_body(wrong.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("schema 2"), "{err}");
    }

    #[test]
    fn allowed_signers_must_be_one_namespaced_key_per_line() {
        let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";
        let good =
            format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n# a comment\n");
        assert_eq!(check_signers(&good).unwrap(), ["signing@ctl"]);
        assert_eq!(
            signers_line("signing@ctl", &format!("ssh-ed25519 {blob} me@ctl")).unwrap(),
            format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}")
        );
        for bad in [
            String::new(),
            format!("signing@ctl ssh-ed25519 {blob}\n"),
            format!("signing@ctl namespaces=\"*\" ssh-ed25519 {blob}\n"),
            format!(
                "signing@ctl cert-authority,namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
            ),
            format!("sign ing namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"),
            format!("a namespaces=\"aterm-machines\" ssh-dss {blob}\n"),
            format!(
                "a namespaces=\"aterm-machines\" ssh-ed25519 {blob}\nb namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
            ),
        ] {
            assert!(check_signers(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_key_id_is_the_fingerprint_verify_prints() {
        let out = "Good \"aterm-machines\" signature for signing@ctl with ED25519 key \
                   SHA256:n3DGXrPVwmgGR8K+oRoVIIetGQDFJsaq8hxEbDCtxLs\n";
        assert_eq!(
            key_id(out).as_deref(),
            Some("SHA256:n3DGXrPVwmgGR8K+oRoVIIetGQDFJsaq8hxEbDCtxLs")
        );
    }

    fn private(fake: &Fake, dir: &str) {
        fake.stat_of(
            dir,
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
    }

    /// The fake's answers for a good signature by `signing@ctl`, measured in the VM.
    fn script_good(fake: &Fake, scratch: &str) {
        let pid = std::process::id();
        fake.on(
            &format!(
                "/usr/bin/ssh-keygen -Y find-principals -s {scratch}/aterm-open-{pid}.sig -f /as"
            ),
            Ran::ok("signing@ctl\n"),
        );
        fake.on(
            &format!(
                "/usr/bin/ssh-keygen -Y verify -f /as -I signing@ctl -n aterm-machines -s {scratch}/aterm-open-{pid}.sig"
            ),
            Ran::ok(
                "Good \"aterm-machines\" signature for signing@ctl with ED25519 key SHA256:P6HGW+cv8zgw7CrKPm2sOl5fu7RG4dXNK0SYwxiFmiw\n", // gitleaks:allow
            ),
        );
    }

    #[test]
    fn open_asks_who_signed_then_verifies_as_that_principal_then_checks_the_body() {
        let fake = Fake::root();
        private(&fake, "/s");
        script_good(&fake, "/s");
        let body = body_text(&envelope("U", 5, fake.now)).unwrap();
        let wire = format!("{SIG}{body}");
        let opened = open_in(
            &fake,
            wire.as_bytes(),
            Path::new("/as"),
            "U",
            &|_| 4,
            Path::new("/s"),
        )
        .unwrap();
        assert_eq!(opened.principal, "signing@ctl");
        assert_eq!(
            opened.key_id,
            "SHA256:P6HGW+cv8zgw7CrKPm2sOl5fu7RG4dXNK0SYwxiFmiw"
        );
        assert_eq!(opened.envelope.serial, 5);
        let calls = fake.calls();
        assert!(calls[2].contains("find-principals"), "{calls:?}");
        assert!(calls[3].contains("-Y verify"), "{calls:?}");
        // The scratch copies are gone.
        let pid = std::process::id();
        assert!(fake.written(&format!("/s/aterm-open-{pid}.sig")).is_none());
        assert!(fake.written(&format!("/s/aterm-open-{pid}.body")).is_none());
    }

    #[test]
    fn open_refuses_an_unknown_signer_with_what_ssh_keygen_said() {
        let fake = Fake::root();
        private(&fake, "/s");
        let pid = std::process::id();
        fake.on(
            &format!("/usr/bin/ssh-keygen -Y find-principals -s /s/aterm-open-{pid}.sig -f /as"),
            Ran {
                status: Some(255),
                stdout: b"No principal matched.\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        let wire = format!("{SIG}{}", body_text(&envelope("U", 5, fake.now)).unwrap());
        let err = open_in(
            &fake,
            wire.as_bytes(),
            Path::new("/as"),
            "U",
            &|_| 0,
            Path::new("/s"),
        )
        .unwrap_err();
        assert_eq!(err, Refusal::BadSignature("No principal matched.".into()));
        assert!(!fake.calls().iter().any(|c| c.contains("-Y verify")));
    }

    #[test]
    fn open_refuses_a_scratch_directory_others_may_write() {
        let fake = Fake::root();
        fake.stat_of(
            "/tmp",
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o1777,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        let wire = format!("{SIG}{}", body_text(&envelope("U", 5, fake.now)).unwrap());
        let err = open_in(
            &fake,
            wire.as_bytes(),
            Path::new("/as"),
            "U",
            &|_| 0,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("writable by others"), "{err}");
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[test]
    fn seal_batch_hands_every_body_to_one_ssh_keygen_run() {
        let fake = Fake::root();
        private(&fake, "/s");
        let pid = std::process::id();
        let bodies = [envelope("U", 1, 10), envelope("V", 2, 10)];
        fake.on(
            &format!(
                "/usr/bin/ssh-keygen -Y sign -f /k/signing -n aterm-machines /s/aterm-envelope-{pid}-0.toml /s/aterm-envelope-{pid}-1.toml"
            ),
            Ran::exit(255, "Load key \"/k/signing\": incorrect passphrase supplied to decrypt private key"),
        );
        let err = seal_batch(
            &fake,
            &bodies,
            Path::new("/k/signing"),
            Signing::Unattended,
            Path::new("/s"),
        )
        .unwrap_err();
        assert!(err.contains("incorrect passphrase"), "{err}");
        let signs = fake
            .calls()
            .iter()
            .filter(|c| c.contains("-Y sign"))
            .count();
        assert_eq!(signs, 1);
        // The scratch bodies are removed on failure too.
        assert!(
            fake.written(&format!("/s/aterm-envelope-{pid}-0.toml"))
                .is_none()
        );
        assert!(
            fake.written(&format!("/s/aterm-envelope-{pid}-1.toml"))
                .is_none()
        );
    }

    #[test]
    fn seal_refuses_a_key_whose_passphrase_probe_does_not_answer() {
        // Nothing scripted: `ssh-keygen -y` does not run, so whether the key is protected
        // is unknown, and it is not guessed "unprotected".
        let fake = Fake::root();
        let err = seal(&fake, &envelope("U", 1, 10), Path::new("/k/signing")).unwrap_err();
        assert!(err.contains("ssh-keygen -y"), "{err}");
        assert!(
            !fake.calls().iter().any(|c| c.contains("-Y sign")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn an_agent_held_key_is_named_by_its_public_half_and_signed_without_a_terminal() {
        let fake = Fake::root();
        private(&fake, "/s");
        let pid = std::process::id();
        fake.on(
            &format!(
                "SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-keygen -Y sign -U -f /k/signing.pub -n aterm-machines /s/aterm-envelope-{pid}-0.toml"
            ),
            Ran::exit(1, ""),
        );
        let err = seal_batch(
            &fake,
            &[envelope("U", 1, 10)],
            Path::new("/k/signing"),
            Signing::Agent,
            Path::new("/s"),
        )
        .unwrap_err();
        // This process's own SSH_AUTH_SOCK decides which refusal: none means no agent to ask.
        if std::env::var_os("SSH_AUTH_SOCK").is_some() {
            assert!(err.contains("did not sign: exit 1"), "{err}");
        } else {
            assert!(err.contains("no ssh-agent"), "{err}");
        }
        assert!(!fake.calls().iter().any(|c| c.starts_with("tty ")));
    }

    // ── real cryptography: throwaway keys in a temporary directory ─────────────────────

    /// A throwaway controller: signing and recovery keys (recovery with a passphrase), an
    /// outsider's key, and the `allowed_signers` naming the first two.
    struct Keys {
        dir: aterm_tempfile::TempDir,
    }

    impl Keys {
        /// Where there is no `/usr/bin/ssh-keygen` at all the test ends COULD NOT RUN; any
        /// other failure to set up is a test failure, never a silent pass.
        fn new() -> Self {
            if !Path::new(SSH_KEYGEN).exists() {
                aterm_uds::host::could_not_run(format!("no {SSH_KEYGEN} here"));
            }
            let dir = aterm_tempfile::tempdir().expect("a temporary directory");
            let live = Live::new();
            for (name, pass) in [("signing", ""), ("recovery", "pw"), ("outsider", "")] {
                let ran = live
                    .run(
                        &Cmd::new(SSH_KEYGEN)
                            .args(["-q", "-t", "ed25519", "-N", pass, "-C"])
                            .arg(format!("{name}@test"))
                            .arg("-f")
                            .arg(dir.path().join(name)),
                    )
                    .expect("ssh-keygen runs");
                assert!(ran.success(), "ssh-keygen -t ed25519: {}", ran.describe());
            }
            let mut signers = String::new();
            for name in ["signing", "recovery"] {
                let public = std::fs::read_to_string(dir.path().join(format!("{name}.pub")))
                    .expect("the public key");
                signers.push_str(&signers_line(&format!("{name}@test"), &public).unwrap());
                signers.push('\n');
            }
            check_signers(&signers).unwrap();
            std::fs::write(dir.path().join("allowed_signers"), signers).unwrap();
            // Owner-only whatever the umask, as the agent makes its stage: a plain
            // `create_dir` is 0775 under a user-private-group login's umask 002, and
            // `private_dir` rightly refuses a group-writable scratch directory.
            live.create_dir_all(&dir.path().join("scratch"), 0o700)
                .unwrap();
            Self { dir }
        }

        fn key(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn signers(&self) -> PathBuf {
            self.dir.path().join("allowed_signers")
        }

        fn scratch(&self) -> PathBuf {
            self.dir.path().join("scratch")
        }

        fn seal(&self, e: &Envelope, key: &str) -> Vec<u8> {
            let mut w = seal_batch(
                &Live::new(),
                std::slice::from_ref(e),
                &self.key(key),
                Signing::Unattended,
                &self.scratch(),
            )
            .unwrap();
            w.pop().unwrap()
        }

        fn open(&self, wire: &[u8], uuid: &str, floor: u64) -> Result<Opened, Refusal> {
            open_in(
                &Live::new(),
                wire,
                &self.signers(),
                uuid,
                &|_| floor,
                &self.scratch(),
            )
        }
    }

    #[test]
    fn a_real_signature_opens_and_every_tampering_is_refused() {
        let keys = Keys::new();
        let now = Live::new().now();
        let e = envelope("UUID-1", 7, now);
        let wire = keys.seal(&e, "signing");
        let opened = keys.open(&wire, "UUID-1", 6).unwrap();
        assert_eq!(opened.envelope, e);
        assert_eq!(opened.principal, "signing@test");
        assert!(opened.key_id.starts_with("SHA256:"), "{}", opened.key_id);
        // Nothing is left in the scratch directory.
        assert_eq!(std::fs::read_dir(keys.scratch()).unwrap().count(), 0);

        // One byte of the body changed: the signature no longer covers it.
        let mut tampered = wire.clone();
        let at = tampered.len() - 3;
        tampered[at] ^= 1;
        assert!(
            matches!(
                keys.open(&tampered, "UUID-1", 0),
                Err(Refusal::BadSignature(_))
            ),
            "tampered"
        );
        // A body swapped under a good signature.
        let (sig, _) = split(&wire).unwrap();
        let mut swapped = sig.to_vec();
        swapped.extend_from_slice(body_text(&envelope("UUID-1", 99, now)).unwrap().as_bytes());
        assert!(matches!(
            keys.open(&swapped, "UUID-1", 0),
            Err(Refusal::BadSignature(_))
        ));
        // Another host's envelope.
        assert_eq!(
            keys.open(&wire, "UUID-2", 0),
            Err(Refusal::WrongHost {
                named: "UUID-1".into()
            })
        );
        // A replay: the floor is at the serial already.
        assert_eq!(
            keys.open(&wire, "UUID-1", 7),
            Err(Refusal::Replay {
                principal: "signing@test".into(),
                serial: 7,
                floor: 7
            })
        );
        // Expired, and issued too far in the future.
        let mut old = envelope("UUID-1", 8, now - 100_000);
        old.expires = now - 10;
        let err = keys
            .open(&keys.seal(&old, "signing"), "UUID-1", 0)
            .unwrap_err();
        assert!(matches!(err, Refusal::Expired { .. }), "{err}");
        assert!(err.to_string().contains("host-clock="), "{err}");
        let ahead = envelope("UUID-1", 9, now + SKEW_SECS + 120);
        assert!(matches!(
            keys.open(&keys.seal(&ahead, "signing"), "UUID-1", 0),
            Err(Refusal::Expired { .. })
        ));
        // A key allowed_signers does not name.
        let err = keys
            .open(&keys.seal(&e, "outsider"), "UUID-1", 0)
            .unwrap_err();
        assert!(matches!(err, Refusal::BadSignature(_)), "{err}");
    }

    #[test]
    fn a_signature_in_another_namespace_is_refused() {
        let keys = Keys::new();
        let now = Live::new().now();
        let body = body_text(&envelope("UUID-1", 1, now)).unwrap();
        let file = keys.scratch().join("other-ns");
        std::fs::write(&file, &body).unwrap();
        let ran = Live::new()
            .run(
                &Cmd::new(SSH_KEYGEN)
                    .args(["-Y", "sign", "-f"])
                    .arg(keys.key("signing"))
                    .args(["-n", "git"])
                    .arg(&file),
            )
            .unwrap();
        assert!(ran.success(), "{}", ran.describe());
        let mut wire = std::fs::read(sig_path(&file)).unwrap();
        wire.extend_from_slice(body.as_bytes());
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_file(sig_path(&file)).unwrap();
        let err = keys.open(&wire, "UUID-1", 0).unwrap_err();
        assert!(matches!(err, Refusal::BadSignature(_)), "{err}");
    }

    #[test]
    fn a_passphrase_key_is_told_apart_from_an_open_one() {
        let keys = Keys::new();
        let live = Live::new();
        assert_eq!(
            needs_passphrase(&live, &keys.key("signing")),
            Read::Value(false)
        );
        assert_eq!(
            needs_passphrase(&live, &keys.key("recovery")),
            Read::Value(true)
        );
        assert!(matches!(
            needs_passphrase(&live, &keys.key("missing")),
            Read::Unusable(_)
        ));
    }

    #[test]
    fn a_batch_signed_by_one_run_opens_envelope_by_envelope() {
        let keys = Keys::new();
        let now = Live::new().now();
        let bodies: Vec<Envelope> = (1..=3)
            .map(|i| envelope(&format!("H{i}"), i, now))
            .collect();
        let wires = seal_batch(
            &Live::new(),
            &bodies,
            &keys.key("signing"),
            Signing::Unattended,
            &keys.scratch(),
        )
        .unwrap();
        for (i, wire) in wires.iter().enumerate() {
            let opened = keys.open(wire, &format!("H{}", i + 1), 0).unwrap();
            assert_eq!(opened.envelope, bodies[i]);
        }
    }
}
