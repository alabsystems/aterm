// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flows (docs/DESIGN-secrets-2026-10-20.md §7–§8): a machine's join, sync and watch,
//! and an admin's create, edit and publish. Every function takes [`Paths`], so tests run
//! against a scratch home and never the real `~/.secrets`.
//!
//! A machine keeps four files in `~/.config/aterm/secrets` (0700): `source.toml` (where the
//! vault is and the signing key it pinned), `identity` (its age key, age-keygen's format),
//! `state.toml` (the version it holds, when it last heard the source, and what it wrote) and
//! `vault` (the last envelope it verified). Nothing here prints or returns a value.

use crate::age::{self, Identity, Recipient};
use crate::envelope::{self, Envelope, Floor, Locker, Pin, Retired, Unsigned, Verdict};
use crate::materialize::{self, Manifest, Report};
use crate::text::{self, Audience, Document, Entry};
use crate::transport::{self, Fetched, Source};
use crate::{Error, SecretBytes, fsx, join};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Where a machine keeps its vault state, under one home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The home directory (`~`).
    pub home: PathBuf,
    /// `~/.config/aterm/secrets`.
    pub dir: PathBuf,
}

impl Paths {
    /// The paths under `home`.
    pub fn for_home(home: &Path) -> Paths {
        Paths {
            home: home.to_path_buf(),
            dir: home.join(".config").join("aterm").join("secrets"),
        }
    }

    /// The paths under this user's home.
    pub fn live() -> crate::Result<Paths> {
        aterm_types::dirs::home_dir()
            .map(|home| Paths::for_home(&home))
            .ok_or_else(|| Error::new("no home directory"))
    }

    fn source_file(&self) -> PathBuf {
        self.dir.join("source.toml")
    }

    fn identity_file(&self) -> PathBuf {
        self.dir.join("identity")
    }

    fn state_file(&self) -> PathBuf {
        self.dir.join("state.toml")
    }

    fn cache_file(&self) -> PathBuf {
        self.dir.join("vault")
    }

    fn lock_file(&self) -> PathBuf {
        self.dir.join(".lock")
    }

    /// Where `aterm secrets serve` keeps its store by default.
    pub fn served(&self) -> PathBuf {
        self.dir.join("served")
    }
}

/// A machine's configured source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the vault is.
    pub source: Source,
    /// The vault's name.
    pub name: String,
    /// The pinned signing key (None for a plain text file at a path).
    pub signer: Option<[u8; 32]>,
    /// The keys the pin was reached through, oldest first: the handoff chain of the version
    /// that last moved it, empty while it is the key the join pinned. A build that retires a
    /// key judges by them whether the pin is a fork the removed admin made (§5).
    pub followed: Vec<[u8; 32]>,
    /// The name this machine asked to be called.
    pub host: String,
}

/// Whether a requested join pin admits this configured authority. An absent
/// pin retains an admitted source; an explicit pin may name its signer or a
/// verified predecessor. Retired authority gives way exactly as it does at sync.
/// This reads no files and never joins or fetches a vault.
pub fn pin_admitted(config: &Config, requested: Option<[u8; 32]>) -> bool {
    let Some(signer) = config.signer else {
        return requested.is_none() && matches!(config.source, Source::Path(_));
    };
    let retired = Retired::compiled();
    if retired.gives_way(&signer, &config.followed) {
        return false;
    }
    let Some(requested) = requested else {
        return true;
    };
    let requested = if retired.gives_way(&requested, &[]) {
        let Some(current) = retired.current else {
            return false;
        };
        current
    } else {
        requested
    };
    signer == requested || config.followed.contains(&requested)
}

/// Whether this machine is in the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    /// No source configured.
    Unconfigured,
    /// Configured; asked to join; no locker has opened yet.
    Pending,
    /// A locker opened: holding this seq, last heard from the source at this unix time.
    In {
        /// The seq held.
        seq: u64,
        /// When the source last answered.
        heard: u64,
    },
    /// It was in, and the latest envelope has no locker for it.
    Revoked {
        /// The seq that dropped it.
        seq: u64,
    },
    /// Reading a plain text file at a path.
    Plain,
}

/// What `aterm secrets status` shows. Names only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The source, if configured.
    pub config: Option<Config>,
    /// This machine's recipient and join code, if it has a key.
    pub machine: Option<(Recipient, String)>,
    /// Membership.
    pub membership: Membership,
    /// The entry names materialized here.
    pub names: Vec<String>,
}

/// How a join should behave.
pub struct JoinOptions<'a> {
    /// What the person typed: a URL (optionally `#<signer>`), a path, or empty for the ALab
    /// default.
    pub source: String,
    /// The machine name to ask for (default: [`default_host_name`]).
    pub host: Option<String>,
    /// A person is at a terminal: an unpinned source may be confirmed by fingerprint, and a
    /// join run again asks for the join password; without one, a password given came from a
    /// pipe.
    pub interactive: bool,
    /// How long to wait for the approval after asking (zero: return at once, pending).
    pub wait: Duration,
    /// Shown a fingerprint, answers whether to trust it (interactive only).
    pub confirm: &'a dyn Fn(&str) -> bool,
    /// Asked once, when this machine is not in yet and the source is a URL: the vault's join
    /// password (an admin Mac then approves within seconds), or `None` to wait for an admin
    /// to approve the code.
    pub password: &'a dyn Fn() -> Option<SecretBytes>,
    /// Called once the request is queued and before any wait: the CLI installs the subscriber
    /// here, so a Ctrl-C during the wait loses nothing.
    pub requested: &'a dyn Fn(),
    /// Progress lines for a person (never a value).
    pub say: &'a dyn Fn(&str),
}

/// How a join ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    /// In, and synced.
    Joined {
        /// The seq applied.
        seq: u64,
        /// Names written or already right.
        names: Vec<String>,
    },
    /// Asked to join; an admin must approve. `code` is what the admin compares.
    Pending {
        /// This machine's code.
        code: String,
        /// The machine name it asked for.
        host: String,
    },
    /// Configured to read a plain text file; synced.
    Plain {
        /// Names written or already right.
        names: Vec<String>,
    },
}

/// How a sync ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// A newer version, applied.
    Applied {
        /// The seq.
        seq: u64,
        /// What changed.
        report: Report,
    },
    /// Nothing newer.
    Unchanged {
        /// The seq held.
        seq: u64,
    },
    /// No locker opens for this machine (yet, or any more).
    NotIn,
}

// ── Small helpers ───────────────────────────────────────────────────────────

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

/// A TOML basic string.
fn toml_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn decode_key32(text: &str) -> Option<[u8; 32]> {
    crate::b64::decode_padded(text).and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
}

fn read_toml(path: &Path, what: &str) -> crate::Result<Option<aterm_toml::Table>> {
    let Some(bytes) = fsx::read_private(path, 4 << 20)? else {
        return Ok(None);
    };
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new(format!("{what}: not UTF-8")))?;
    match aterm_toml::from_str::<aterm_toml::Value>(text) {
        Ok(aterm_toml::Value::Table(table)) => Ok(Some(table)),
        _ => Err(Error::new(format!(
            "{}: not a valid {what}; move it aside and run aterm secrets join again",
            path.display()
        ))),
    }
}

/// Serialises state writers and coherent readiness observations. An advisory
/// lock released by `LOCK_UN` on drop (clippy.toml: the flock release rule).
struct StateLock(std::fs::File);

impl StateLock {
    fn take(paths: &Paths) -> crate::Result<StateLock> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.lock_file();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of StateLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        file.lock()
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        Ok(StateLock(file))
    }

    /// Observe only an existing regular lockfile, without changing the state
    /// directory or waiting behind a subscriber's materialization pass.
    fn observe(paths: &Paths) -> crate::Result<Option<StateLock>> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = paths.lock_file();
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::new(format!("{}: {error}", path.display()))),
        };
        let metadata = file
            .metadata()
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        if !metadata.file_type().is_file() {
            return Err(Error::new(format!(
                "{}: not a regular file",
                path.display()
            )));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the shared take of StateLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        match file.try_lock_shared() {
            Ok(()) => Ok(Some(StateLock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(Error::new(format!("{}: {error}", path.display())))
            }
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

// ── Identity, host name, code ───────────────────────────────────────────────

/// The machine name to ask for: the first word of the computer's name, lowercased, when it is
/// a valid machine name (the owner's Macs are `m22 Macbook …` → `m22`), else the local host
/// name's first label, else `machine`.
pub fn default_host_name() -> String {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        for key in ["ComputerName", "LocalHostName"] {
            let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/scutil");
            cmd.arg("--get").arg(key);
            if let Ok(out) =
                aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(5))
                && out.status.success()
            {
                candidates.push(String::from_utf8_lossy(&out.stdout).trim().to_string());
            }
        }
    }
    if let Some(name) = aterm_uds::process::lan_host_name() {
        candidates.push(name);
    }
    if let Ok(name) = std::fs::read_to_string("/etc/hostname") {
        candidates.push(name.trim().to_string());
    }
    for candidate in candidates {
        let first = candidate
            .split(|c: char| c.is_whitespace() || c == '.' || c == '-' || c == '_')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if text::valid_machine_name(&first) {
            return first;
        }
    }
    "machine".to_string()
}

/// A recipient's join code: the first 10 bytes (80 bits) of SHA-256 of its 32 bytes in
/// lowercase RFC 4648 base32, as `xxxx-xxxx-xxxx-xxxx`. Eighty bits keep a stranger from
/// minting a key whose code matches a waiting machine's.
pub fn join_code(recipient: &Recipient) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let digest = sha256(&recipient.0);
    let mut bits: u128 = 0;
    for &byte in &digest[..10] {
        bits = (bits << 8) | u128::from(byte);
    }
    let mut out = String::with_capacity(19);
    for i in 0..16 {
        let index = ((bits >> (75 - 5 * i)) & 31) as usize;
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        out.push(char::from(ALPHABET[index]));
    }
    out
}

fn read_identity(paths: &Paths) -> crate::Result<Option<Identity>> {
    let Some(bytes) = fsx::read_private(&paths.identity_file(), 16 << 10)? else {
        return Ok(None);
    };
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new("identity: not UTF-8"))?;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| Error::new("identity: no key in the file"))?;
    Identity::parse(line).map(Some).map_err(|_| {
        Error::new(format!(
            "{}: not an age identity",
            paths.identity_file().display()
        ))
    })
}

/// This machine's key, made (0600, age-keygen format) if there is none.
pub fn machine_identity(paths: &Paths) -> crate::Result<Identity> {
    if let Some(identity) = read_identity(paths)? {
        return Ok(identity);
    }
    fsx::ensure_private_dir(&paths.dir)?;
    install_identity(paths, Identity::generate()?)
}

/// Publish a generated key only into an empty name. A concurrent creator's admitted key
/// wins: every caller must return the key that remains on disk, never its losing one.
fn install_identity(paths: &Paths, identity: Identity) -> crate::Result<Identity> {
    let secret = identity.to_bech32();
    let mut file = format!(
        "# aterm secrets machine key: created {} (unix), never copied off this machine.\n# public key: {}\n",
        now(),
        identity.recipient().to_bech32()
    )
    .into_bytes();
    file.extend_from_slice(secret.as_slice());
    file.push(b'\n');
    let file = SecretBytes::new(file);
    if fsx::write_new(&paths.identity_file(), file.as_slice(), 0o600)? {
        return Ok(identity);
    }
    read_identity(paths)?
        .ok_or_else(|| Error::new("the machine key changed while it was being created; try again"))
}

// ── Config and state files ──────────────────────────────────────────────────

/// The configured source, if any.
pub fn config(paths: &Paths) -> crate::Result<Option<Config>> {
    let Some(table) = read_toml(&paths.source_file(), "source.toml")? else {
        return Ok(None);
    };
    let get = |key: &str| table.get(key).and_then(aterm_toml::Value::as_str);
    let source = match (get("url"), get("path")) {
        (Some(url), None) => Source::Url(url.to_string()),
        (None, Some(path)) => Source::Path(PathBuf::from(path)),
        _ => return Err(Error::new("source.toml: needs exactly one of url or path")),
    };
    let name = get("name")
        .ok_or_else(|| Error::new("source.toml: no name"))?
        .to_string();
    let signer = match get("signer") {
        Some(text) => {
            Some(decode_key32(text).ok_or_else(|| Error::new("source.toml: bad signer"))?)
        }
        None => None,
    };
    let mut followed = Vec::new();
    if let Some(keys) = table.get("followed") {
        let keys = keys
            .as_array()
            .ok_or_else(|| Error::new("source.toml: followed is not a list"))?;
        for key in keys {
            followed.push(
                key.as_str()
                    .and_then(decode_key32)
                    .ok_or_else(|| Error::new("source.toml: bad key in followed"))?,
            );
        }
    } else if let Some(pin) = &signer {
        // No list: none was reached (a source.toml leaves an empty one out), or an aterm
        // before `followed` wrote the file. Either way the cached version, which that pin
        // signs, carries the keys it was reached through, as a server reads them off the
        // version it holds: a machine that followed the removed admin's fork under such a
        // build is pulled back too once a build retires the key the fork came from.
        followed = cached_chain(paths, pin);
    }
    let host = get("host").unwrap_or("machine").to_string();
    Ok(Some(Config {
        source,
        name,
        signer,
        followed,
        host,
    }))
}

fn save_config(paths: &Paths, config: &Config) -> crate::Result<()> {
    fsx::ensure_private_dir(&paths.dir)?;
    let mut text =
        String::from("# aterm secrets: where this machine's vault is (aterm help secrets).\n");
    match &config.source {
        Source::Url(url) => text.push_str(&format!("url = {}\n", toml_str(url))),
        Source::Path(path) => {
            text.push_str(&format!("path = {}\n", toml_str(&path.to_string_lossy())))
        }
    }
    text.push_str(&format!("name = {}\n", toml_str(&config.name)));
    if let Some(signer) = &config.signer {
        text.push_str(&format!(
            "# the vault's signing key, pinned at join (fingerprint {})\nsigner = {}\n",
            envelope::fingerprint(signer),
            toml_str(&crate::b64::encode_padded(signer))
        ));
    }
    if !config.followed.is_empty() {
        let keys: Vec<String> = config
            .followed
            .iter()
            .map(|key| toml_str(&crate::b64::encode_padded(key)))
            .collect();
        text.push_str(&format!(
            "# the keys that pin was reached through, oldest first\nfollowed = [{}]\n",
            keys.join(", ")
        ));
    }
    text.push_str(&format!("host = {}\n", toml_str(&config.host)));
    fsx::write_atomic(&paths.source_file(), text.as_bytes(), 0o600)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stored {
    #[default]
    None,
    Pending,
    In,
    Revoked,
    Plain,
}

impl Stored {
    fn word(self) -> &'static str {
        match self {
            Stored::None => "none",
            Stored::Pending => "pending",
            Stored::In => "in",
            Stored::Revoked => "revoked",
            Stored::Plain => "plain",
        }
    }

    fn from_word(word: &str) -> Stored {
        match word {
            "pending" => Stored::Pending,
            "in" => Stored::In,
            "revoked" => Stored::Revoked,
            "plain" => Stored::Plain,
            _ => Stored::None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct State {
    seq: u64,
    digest: Option<[u8; 32]>,
    /// The source, vault name and signer this version and membership belong to.
    /// Absent in older records until a successful sync establishes the binding.
    source_identity: Option<[u8; 32]>,
    heard: u64,
    requested: u64,
    membership: Stored,
    /// The join request's proof of the password, re-sent with each hourly request.
    proof: Option<String>,
    /// The last apply left a file unwritten: the next fetch of the same version applies again.
    failed: bool,
    manifest: Manifest,
    /// Entry name → digest of each value a pass is to write, saved before it writes and
    /// cleared by its own save, and kept by the saves of passes that apply nothing: a pass cut
    /// short in between leaves those files holding what it wrote under the manifest's older
    /// digests, or none, and the next apply counts them the vault's all the same
    /// ([`materialize::apply_resuming`]).
    pending: materialize::Pending,
}

fn load_state(paths: &Paths) -> crate::Result<State> {
    let Some(table) = read_toml(&paths.state_file(), "state.toml")? else {
        return Ok(State::default());
    };
    let int = |key: &str| {
        table
            .get(key)
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0)
    };
    let digest = |key: &str| {
        table
            .get(key)
            .and_then(aterm_toml::Value::as_str)
            .and_then(|text| aterm_codec::hex::decode_vec(text.as_bytes()))
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
    };
    let digests = |key: &str| {
        let mut digests = BTreeMap::new();
        if let Some(files) = table.get(key).and_then(aterm_toml::Value::as_table) {
            for (name, digest) in files {
                if let Some(digest) = digest.as_str() {
                    digests.insert(name.clone(), digest.to_string());
                }
            }
        }
        digests
    };
    let manifest = Manifest {
        files: digests("manifest"),
    };
    Ok(State {
        seq: int("seq"),
        digest: digest("digest"),
        source_identity: digest("source_identity"),
        heard: int("heard"),
        requested: int("requested"),
        membership: Stored::from_word(
            table
                .get("membership")
                .and_then(aterm_toml::Value::as_str)
                .unwrap_or("none"),
        ),
        proof: table
            .get("proof")
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string),
        failed: table
            .get("failed")
            .and_then(aterm_toml::Value::as_bool)
            .unwrap_or(false),
        manifest,
        pending: digests("pending"),
    })
}

fn save_state(paths: &Paths, state: &State) -> crate::Result<()> {
    fsx::ensure_private_dir(&paths.dir)?;
    let mut text = String::from(
        "# aterm secrets: what this machine holds. Written by aterm; names and digests only.\n",
    );
    text.push_str(&format!(
        "membership = {}\n",
        toml_str(state.membership.word())
    ));
    text.push_str(&format!(
        "seq = {}\n",
        i64::try_from(state.seq).unwrap_or(i64::MAX)
    ));
    if let Some(digest) = &state.digest {
        text.push_str(&format!("digest = {}\n", toml_str(&hex(digest))));
    }
    if let Some(identity) = &state.source_identity {
        text.push_str(&format!("source_identity = {}\n", toml_str(&hex(identity))));
    }
    text.push_str(&format!(
        "heard = {}\n",
        i64::try_from(state.heard).unwrap_or(i64::MAX)
    ));
    text.push_str(&format!(
        "requested = {}\n",
        i64::try_from(state.requested).unwrap_or(i64::MAX)
    ));
    if let Some(proof) = &state.proof {
        text.push_str(&format!("proof = {}\n", toml_str(proof)));
    }
    if state.failed {
        text.push_str("failed = true\n");
    }
    text.push_str("\n[manifest]\n");
    for (name, digest) in &state.manifest.files {
        text.push_str(&format!("{} = {}\n", toml_str(name), toml_str(digest)));
    }
    if !state.pending.is_empty() {
        text.push_str("\n[pending]\n");
        for (name, digest) in &state.pending {
            text.push_str(&format!("{} = {}\n", toml_str(name), toml_str(digest)));
        }
    }
    fsx::write_atomic(&paths.state_file(), text.as_bytes(), 0o600)
}

fn source_identity(config: &Config) -> [u8; 32] {
    let mut identity = Vec::new();
    match &config.source {
        Source::Url(url) => {
            identity.push(0);
            identity.extend_from_slice(url.as_bytes());
        }
        Source::Path(path) => {
            identity.push(1);
            identity.extend_from_slice(path.to_string_lossy().as_bytes());
        }
    }
    identity.push(0);
    identity.extend_from_slice(config.name.as_bytes());
    identity.push(0);
    if let Some(signer) = config.signer {
        identity.push(1);
        identity.extend_from_slice(&signer);
    } else {
        identity.push(0);
    }
    sha256(&identity)
}

fn source_bound(config: &Config, state: &State) -> bool {
    state.source_identity == Some(source_identity(config))
        && (config.signer.is_none() || state.digest.is_some())
}

/// Invalidate old materialization durably before publishing a new source or
/// authority. Keep its manifest so the next sync can safely replace tracked files.
fn prepare_materialization(paths: &Paths, state: &mut State) -> crate::Result<()> {
    state.failed = true;
    state.source_identity = None;
    save_state(paths, state)
}

// ── Status ──────────────────────────────────────────────────────────────────

/// What `aterm secrets status` shows; reads only local state.
pub fn status(paths: &Paths) -> crate::Result<Status> {
    let config = config(paths)?;
    let machine = read_identity(paths)?.map(|identity| {
        let recipient = identity.recipient();
        (recipient, join_code(&recipient))
    });
    let state = load_state(paths)?;
    let membership = match (&config, state.membership) {
        (None, _) => Membership::Unconfigured,
        (Some(_), Stored::In) => Membership::In {
            seq: state.seq,
            heard: state.heard,
        },
        (Some(_), Stored::Revoked) => Membership::Revoked { seq: state.seq },
        (Some(_), Stored::Plain) => Membership::Plain,
        (Some(_), Stored::Pending | Stored::None) => Membership::Pending,
    };
    Ok(Status {
        config,
        machine,
        membership,
        names: state.manifest.files.keys().cloned().collect(),
    })
}

/// Whether an admitted vault's last sync finished and its tracked files still
/// match. Reads local state and private file digests; never fetches or writes.
/// Call explicitly when checking readiness, rather than during status polling.
/// Missing state or a concurrent writer is not ready.
pub fn materialization_ready(paths: &Paths) -> crate::Result<bool> {
    materialization_ready_with(paths, None, files_intact)
}

/// Observe readiness only while this exact source/authority is still configured.
/// A changed config is refused before reading another source's tracked files.
pub fn materialization_ready_for(paths: &Paths, expected: &Config) -> crate::Result<bool> {
    materialization_ready_with(paths, Some(expected), files_intact)
}

fn materialization_ready_with(
    paths: &Paths,
    expected: Option<&Config>,
    intact: impl FnOnce(&Paths, &State) -> bool,
) -> crate::Result<bool> {
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(false);
    };
    let Some(config) = config(paths)? else {
        return Ok(false);
    };
    if expected.is_some_and(|expected| *expected != config) || !pin_admitted(&config, None) {
        return Ok(false);
    }
    let state = load_state(paths)?;
    Ok(matches!(state.membership, Stored::In | Stored::Plain)
        && source_bound(&config, &state)
        && intact(paths, &state))
}

// ── Reading a vault ─────────────────────────────────────────────────────────

/// The plaintext admin locker: the signing seed and the whole text.
const ADMIN_MAGIC: &str = "alab-vault-admin/1";

/// Built in one buffer sized up front, the seed's base64 wiped, so no copy of the seed is
/// freed unwiped.
fn admin_plaintext(seed: &[u8; 32], doc: &Document) -> SecretBytes {
    let rendered = doc.render();
    let mut seed_text = crate::b64::encode_padded(seed);
    let mut out = Vec::with_capacity(ADMIN_MAGIC.len() + 16 + seed_text.len() + rendered.len());
    out.extend_from_slice(ADMIN_MAGIC.as_bytes());
    out.extend_from_slice(b"\nsigner-seed: ");
    out.extend_from_slice(seed_text.as_bytes());
    out.extend_from_slice(b"\n\n");
    out.extend_from_slice(rendered.as_slice());
    zeroize::Zeroize::zeroize(&mut seed_text);
    SecretBytes::new(out)
}

/// An opened admin locker: the signing seed (wiped on drop) and the whole text.
struct AdminText {
    seed: [u8; 32],
    doc: Document,
}

impl Drop for AdminText {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.seed);
    }
}

fn parse_admin(plain: &SecretBytes) -> crate::Result<AdminText> {
    let text = plain
        .as_str()
        .ok_or_else(|| Error::new("admin locker: not UTF-8"))?;
    let rest = text
        .strip_prefix(ADMIN_MAGIC)
        .and_then(|rest| rest.strip_prefix("\nsigner-seed: "))
        .ok_or_else(|| Error::new("admin locker: not an admin locker"))?;
    let (seed_text, doc) = rest
        .split_once("\n\n")
        .ok_or_else(|| Error::new("admin locker: malformed"))?;
    let mut decoded = crate::b64::decode_padded(seed_text).unwrap_or_default();
    let seed = <[u8; 32]>::try_from(decoded.as_slice());
    zeroize::Zeroize::zeroize(&mut decoded);
    let seed = seed.map_err(|_| Error::new("admin locker: bad seed"))?;
    let doc = Document::parse(doc).map_err(|e| Error::new(format!("admin locker: {e}")))?;
    Ok(AdminText { seed, doc })
}

/// The entries this machine's identity opens, merged: `[everyone]` first, then host
/// sections over it by name. `Ok(None)` when no locker opens.
fn open_lockers(envelope: &Envelope, identity: &Identity) -> crate::Result<Option<Vec<Entry>>> {
    let identities = std::slice::from_ref(identity);
    let mut everyone: BTreeMap<String, Entry> = BTreeMap::new();
    let mut hosts: BTreeMap<String, Entry> = BTreeMap::new();
    let mut opened = false;
    for locker in envelope
        .lockers
        .iter()
        .filter(|locker| locker.label != "admin")
    {
        let plain = match age::decrypt(identities, locker.armored.as_bytes()) {
            Ok(plain) => plain,
            Err(age::AgeError::NoMatch) => continue,
            Err(error) => {
                return Err(Error::new(format!("locker {}: {error}", locker.label)));
            }
        };
        opened = true;
        let entries = text::parse_locker(plain.as_slice())
            .map_err(|e| Error::new(format!("locker {}: {e}", locker.label)))?;
        for entry in entries {
            let into = match entry.audience {
                Audience::Everyone => &mut everyone,
                Audience::Hosts(_) => &mut hosts,
            };
            if into.contains_key(&entry.name) {
                return Err(Error::new(format!(
                    "entry {} is addressed to this machine twice",
                    entry.name
                )));
            }
            into.insert(entry.name.clone(), entry);
        }
    }
    if !opened {
        return Ok(None);
    }
    everyone.extend(hosts);
    Ok(Some(everyone.into_values().collect()))
}

/// A plain file's entries for this host. The file must be this user's with no group or other
/// bits: it holds the secrets in the clear.
fn plain_entries(config: &Config, path: &Path) -> crate::Result<Vec<Entry>> {
    let bytes = fsx::read_private(path, text::MAX_TEXT)?
        .ok_or_else(|| Error::new(format!("{}: no such file", path.display())))?;
    text::parse_plain(bytes.as_slice())
        .and_then(|entries| text::merge_for(&config.host, entries))
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

fn is_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(envelope::MAGIC.as_bytes())
}

/// Write `entries`, judged against what the vault wrote, resuming the pass `state.pending`
/// names ([`materialize::apply_resuming`]). Before it writes, a pass saves what it is to
/// write (`pending`) beside the version state.toml held before it, which only its own save
/// moves on: a pass cut short is applied again, and a file that holds what it was writing is
/// the vault's, whatever version comes next (without that it would be judged changed since,
/// the machine's own for good). That save folds in what the pass resumed, so a second cut
/// still counts those files the vault's.
fn apply_entries(paths: &Paths, state: &mut State, entries: &[Entry]) -> crate::Result<Report> {
    let resumed = std::mem::take(&mut state.pending);
    let applied = materialize::apply_resuming(
        &paths.home,
        entries,
        &state.manifest,
        &resumed,
        now(),
        &mut |held, pending| {
            if pending.is_empty() {
                return Ok(());
            }
            let mut ahead = load_state(paths)?;
            ahead.manifest = held.clone();
            ahead.pending = pending.clone();
            save_state(paths, &ahead)
        },
    );
    match applied {
        Ok((manifest, report)) => {
            state.manifest = manifest;
            Ok(report)
        }
        Err(error) => {
            state.pending = resumed;
            Err(error)
        }
    }
}

fn names_of(report: &Report) -> Vec<String> {
    let mut names: Vec<String> = report
        .written
        .iter()
        .chain(report.unchanged.iter())
        .cloned()
        .collect();
    names.sort();
    names
}

// ── Sync ────────────────────────────────────────────────────────────────────

/// The rollback floor for a vault named `name` under the pin `config` holds: the version
/// state.toml holds, or, when it holds none (a reset, or a corrupt file moved aside), the
/// cached envelope's, if the cache verifies under the same pin. While the cache is the
/// version held, the floor carries its handoff chain too, which the next version must carry
/// on.
fn floor(paths: &Paths, state: &State, name: &str, config: &Config) -> Option<Floor> {
    let pin = config.signer?;
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .ok()
        .flatten();
    let verified = |bytes: SecretBytes| {
        envelope::verify_for_reader(bytes.as_slice(), name, &pin, &config.followed, None)
            .ok()
            .map(|(envelope, _, _)| envelope)
    };
    if state.membership != Stored::None {
        let Some(digest) = state.digest else {
            // No digest beside a membership: a pass whose pin gave way forgot what it held and
            // was cut short, perhaps after it cached the version that replaces it, which must
            // not floor itself as already applied, nor let an older one in.
            return Some(Floor::below(&verified(cached?)?));
        };
        let held = cached
            .and_then(|bytes| envelope::parse(bytes.as_slice()).ok())
            .filter(|cached| cached.digest() == digest);
        return Some(match held {
            Some(held) => Floor::of(&held),
            None => Floor::version(state.seq, digest),
        });
    }
    Some(Floor::of(&verified(cached?)?))
}

/// Whether the pin `config` holds gives way to the key this build names ([`Pin::Replaced`]):
/// what this machine holds under it may be the removed admin's, so its seq asks no long-poll.
fn pin_gives_way(config: &Config) -> bool {
    config
        .signer
        .is_some_and(|pin| Retired::compiled().gives_way(&pin, &config.followed))
}

/// The keys the cached version's handoff chain passes through, when `pin` signs it.
fn cached_chain(paths: &Paths, pin: &[u8; 32]) -> Vec<[u8; 32]> {
    fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .ok()
        .flatten()
        .and_then(|bytes| envelope::parse(bytes.as_slice()).ok())
        .filter(|cached| cached.signer == *pin && cached.verify_signature().is_ok())
        .map(|cached| chain_keys(&cached))
        .unwrap_or_default()
}

/// The keys an envelope's handoff chain passes through, oldest first.
fn chain_keys(envelope: &Envelope) -> Vec<[u8; 32]> {
    envelope.handoff.iter().map(|(key, _)| *key).collect()
}

/// Whether the last apply wrote everything (none failed, and none was cut short with writes
/// pending) and every file the manifest says the vault wrote still holds what it wrote. A
/// file deleted or edited by hand is rewritten at the next fetch of the same version (an `if-
/// absent` one rewritten is left the machine's own), which the subscriber makes once per
/// version ([`repair_due`]).
fn files_intact(paths: &Paths, state: &State) -> bool {
    !state.failed
        && state.pending.is_empty()
        && state.manifest.files.iter().all(|(name, digest)| {
            materialize::target_path(&paths.home, name)
                .ok()
                .and_then(|path| fsx::read_private(&path, text::MAX_VALUE).ok().flatten())
                .is_some_and(|bytes| materialize::digest_hex(bytes.as_slice()) == *digest)
        })
}

fn missing(config: &Config) -> Error {
    Error::new(format!(
        "the source holds no vault named {}; an admin's next command puts its copy back",
        config.name
    ))
}

fn quiet(report: &Report) -> bool {
    report.written.is_empty()
        && report.removed.is_empty()
        && report.moved_aside.is_empty()
        && report.failed.is_empty()
}

/// Fetch, verify, decrypt and materialize once. `wait` turns the fetch into a long-poll for a
/// version newer than the one held.
pub fn sync(paths: &Paths, wait: Option<Duration>) -> crate::Result<SyncOutcome> {
    let mut config = config(paths)?.ok_or_else(|| {
        Error::new("no vault configured: run aterm secrets join <url> (or aterm init)")
    })?;
    // A long-poll is held without the lock, so a person's command is never queued behind it.
    let before = load_state(paths)?;
    let fetched = match (&config.source, wait) {
        (Source::Url(_), Some(wait)) => {
            let after = (before.membership != Stored::None
                && !pin_gives_way(&config)
                && source_bound(&config, &before)
                && (!matches!(before.membership, Stored::In | Stored::Plain)
                    || (!before.failed && before.pending.is_empty())))
            .then_some(before.seq);
            transport::fetch(&config.source, after, wait + Duration::from_secs(10))?
        }
        _ => transport::fetch(&config.source, None, Duration::from_secs(30))?,
    };
    let _lock = StateLock::take(paths)?;
    if self::config(paths)?.as_ref() != Some(&config) {
        // A join moved this machine to another source while the fetch was out: the next pass
        // reads that one.
        return Ok(SyncOutcome::Unchanged { seq: before.seq });
    }
    let mut state = load_state(paths)?;
    let bytes = match fetched {
        Fetched::NoNewer => {
            if now().saturating_sub(state.heard) >= 60 {
                state.heard = now();
                save_state(paths, &state)?;
            }
            return Ok(SyncOutcome::Unchanged { seq: state.seq });
        }
        Fetched::Missing => return Err(missing(&config)),
        Fetched::Body(bytes) => bytes,
    };
    if config.signer.is_some()
        && state.seq > before.seq
        && envelope::parse(&bytes).is_ok_and(|parsed| parsed.seq <= state.seq)
    {
        // Another command applied a version while this fetch was out: nothing to do, and no
        // rollback to report.
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    let Some(pin) = config.signer else {
        let Source::Path(path) = &config.source else {
            return Err(Error::new("source.toml: a URL source needs a signer"));
        };
        if is_envelope(&bytes) {
            return Err(Error::new(
                "the file at the path is now a signed vault: run aterm secrets join again to pin it",
            ));
        }
        let entries = plain_entries(&config, path)?;
        let report = apply_entries(paths, &mut state, &entries)?;
        state.membership = Stored::Plain;
        state.failed = !report.failed.is_empty();
        state.source_identity = (!state.failed).then(|| source_identity(&config));
        state.heard = now();
        save_state(paths, &state)?;
        if quiet(&report) {
            return Ok(SyncOutcome::Unchanged { seq: 0 });
        }
        return Ok(SyncOutcome::Applied { seq: 0, report });
    };
    let held = floor(paths, &state, &config.name, &config);
    let (envelope, pinned, verdict) =
        envelope::verify_for_reader(&bytes, &config.name, &pin, &config.followed, held.as_ref())?;
    state.heard = now();
    if let Pin::Replaced(_) = pinned {
        // What this machine held under the pin that gave way floors nothing (the removed
        // admin may have signed it, with any seq): forgotten before the pin moves, so a pass
        // cut short after the move does not leave the new pin under that floor.
        state.seq = 0;
        state.digest = None;
    }
    if let Pin::Rotated(new) | Pin::Replaced(new) = pinned {
        prepare_materialization(paths, &mut state)?;
        config.signer = Some(new);
        config.followed = chain_keys(&envelope);
        save_config(paths, &config)?;
    }
    if verdict == Verdict::Unchanged
        && state.membership == Stored::In
        && source_bound(&config, &state)
        && files_intact(paths, &state)
    {
        save_state(paths, &state)?;
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    let identity = read_identity(paths)?
        .ok_or_else(|| Error::new("this machine has no key: run aterm secrets join"))?;
    let opened = open_lockers(&envelope, &identity)?;
    state.seq = envelope.seq;
    state.digest = Some(envelope.digest());
    fsx::write_atomic(&paths.cache_file(), envelope.raw(), 0o600)?;
    let Some(entries) = opened else {
        state.membership = match state.membership {
            Stored::In | Stored::Revoked => Stored::Revoked,
            _ => Stored::Pending,
        };
        state.source_identity = Some(source_identity(&config));
        save_state(paths, &state)?;
        return Ok(SyncOutcome::NotIn);
    };
    let report = apply_entries(paths, &mut state, &entries)?;
    state.membership = Stored::In;
    state.failed = !report.failed.is_empty();
    state.source_identity = (!state.failed).then(|| source_identity(&config));
    save_state(paths, &state)?;
    Ok(SyncOutcome::Applied {
        seq: envelope.seq,
        report,
    })
}

/// The subscriber (`aterm secrets watch`): long-poll the source and apply every new version,
/// for ever; ask again every hour while waiting for approval; and on an admin of a vault with
/// a join password, approve every request that proves it, or of an open vault, every request
/// from a machine not in it (§7). Errors back off from 5 s to 5 min and are logged once each.
/// `log` gets one line per event (names and counts only).
pub fn watch(paths: &Paths, log: &(dyn Fn(&str) + Sync)) -> crate::Result<()> {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| approver_loop(paths, log, &stop));
        let ended = subscriber_loop(paths, log);
        stop.store(true, Ordering::Relaxed);
        ended
    })
}

/// This process's executable as it is now: device, inode, size and modification time.
fn program_stamp() -> Option<(u64, u64, u64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    Some((meta.dev(), meta.ino(), meta.size(), meta.mtime()))
}

/// Sleep `total`, waking early once `stop` is set.
fn nap(stop: &AtomicBool, total: Duration) {
    let deadline = Instant::now() + total;
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
    }
}

/// The subscriber's first pause after a pass that left an entry failed ([`pause_after`]).
const FIRST_RETRY: Duration = Duration::from_secs(5);

/// The longest the subscriber waits between passes that fail, either way.
const LONGEST_RETRY: Duration = Duration::from_secs(300);

/// How long the subscriber waits after a pass that ended in `outcome` from `source`. A source
/// that answers a long-poll at once with nothing new (one that does not hold, or a stale
/// copy) is asked again only after a pause. A pass that applied a version at once asks again
/// (a long-poll), unless an entry failed: such a pass leaves the next one no long-poll (it
/// fetches and applies again, to retry the entry), so it waits `failing`, which doubles to
/// [`LONGEST_RETRY`] while entries keep failing; any other pass starts it again at
/// [`FIRST_RETRY`].
fn pause_after(source: &Source, outcome: &SyncOutcome, failing: &mut Duration) -> Duration {
    let pause = match (source, outcome) {
        (Source::Path(_), _) => Duration::from_secs(60),
        (_, SyncOutcome::NotIn) => Duration::from_secs(30),
        (_, SyncOutcome::Unchanged { .. }) => Duration::from_secs(10),
        (_, SyncOutcome::Applied { report, .. }) if report.failed.is_empty() => Duration::ZERO,
        (_, SyncOutcome::Applied { .. }) => {
            let pause = *failing;
            *failing = failing.saturating_mul(2).min(LONGEST_RETRY);
            return pause;
        }
    };
    *failing = FIRST_RETRY;
    pause
}

/// Whether the subscriber's next pass is to fetch and apply the version this machine holds
/// again, instead of long-polling for a newer one: it is in, its last apply wrote everything,
/// and a file the manifest says the vault wrote no longer holds it (deleted or edited by hand,
/// or, for an ` if-absent` entry, rewritten by a program, which the apply then leaves the
/// machine's own), so [`materialization_ready`] says not ready until an apply. Once per
/// source and version (`repaired`), so a program that keeps rewriting a file is not fought
/// on every pass, and a different vault at the same sequence still gets its repair.
fn repair_due(paths: &Paths, repaired: &mut Option<([u8; 32], u64)>) -> bool {
    let Ok(state) = load_state(paths) else {
        return false;
    };
    let Some(source) = state.source_identity else {
        return false;
    };
    let version = (source, state.seq);
    let due = state.membership == Stored::In
        && !state.failed
        && state.pending.is_empty()
        && *repaired != Some(version)
        && !files_intact(paths, &state);
    if due {
        *repaired = Some(version);
    }
    due
}

/// The subscriber's passes. Returns when this program was replaced on disk (an update), so
/// launchd or systemd starts the new build: a job keeps the image it started with.
fn subscriber_loop(paths: &Paths, log: &(dyn Fn(&str) + Sync)) -> crate::Result<()> {
    let mut backoff = Duration::from_secs(5);
    let mut failing = FIRST_RETRY;
    let mut repaired = None;
    let mut last_error = String::new();
    let mut last_applied = String::new();
    let program = program_stamp();
    loop {
        if program.is_some() && program_stamp() != program {
            log("this program was replaced on disk; exiting so the service starts the new one");
            return Ok(());
        }
        let started = Instant::now();
        let pass = config(paths).and_then(|config| {
            let config = config
                .ok_or_else(|| Error::new("no vault configured; waiting for aterm secrets join"))?;
            let wait = match config.source {
                Source::Url(_) if !repair_due(paths, &mut repaired) => {
                    Some(Duration::from_secs(55))
                }
                Source::Url(_) | Source::Path(_) => None,
            };
            sync(paths, wait).map(|outcome| (config, outcome))
        });
        match pass {
            Ok((config, outcome)) => {
                backoff = Duration::from_secs(5);
                last_error.clear();
                match &outcome {
                    SyncOutcome::Applied { seq, report } => {
                        let line = format!(
                            "applied seq {seq}: {} written, {} unchanged, {} removed, {} moved aside, {} failed",
                            report.written.len(),
                            report.unchanged.len(),
                            report.removed.len(),
                            report.moved_aside.len(),
                            report.failed.len()
                        );
                        // A retry of a failing entry that ends the same way says nothing new.
                        if line != last_applied {
                            log(&line);
                        }
                        last_applied = if report.failed.is_empty() {
                            String::new()
                        } else {
                            line
                        };
                    }
                    SyncOutcome::Unchanged { .. } => {}
                    SyncOutcome::NotIn => {
                        if load_state(paths).is_ok_and(|s| s.membership == Stored::Revoked) {
                            log("this machine is no longer in the vault");
                        }
                    }
                }
                ask_again_if_due(paths, &config, log);
                let pause = pause_after(&config.source, &outcome, &mut failing);
                std::thread::sleep(pause.saturating_sub(started.elapsed()));
            }
            Err(error) => {
                let text = error.to_string();
                if text != last_error {
                    log(&format!("{text}; retrying with backoff up to 5 min"));
                    last_error = text;
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(LONGEST_RETRY);
            }
        }
    }
}

/// A machine still waiting for approval re-queues its request every hour (the queue keeps
/// requests seven days), with the proof it made when the person typed the password.
fn ask_again_if_due(paths: &Paths, config: &Config, log: &dyn Fn(&str)) {
    let Ok(state) = load_state(paths) else {
        return;
    };
    if state.membership != Stored::Pending || now().saturating_sub(state.requested) < 3600 {
        return;
    }
    match request_join(paths, config, state.proof.clone()) {
        Ok(code) => log(&format!("asked to join again (code {code})")),
        Err(error) => log(&format!("join request failed: {error}")),
    }
}

/// What an admin's subscriber approves on a vault, by what its `[join]` says: requests that
/// prove the join password, or on an open vault every request from a machine not in it whose
/// key no `code-only` line names. A vault with neither has nothing approved but by a person,
/// by code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// `password = …`: requests whose proof matches it.
    Password,
    /// `open = yes`: requests from machines not in the vault, but for a `code-only` key.
    Open,
}

impl Approval {
    /// What `doc` has approved without a person, if anything.
    pub fn of(doc: &Document) -> Option<Approval> {
        if doc.open() {
            return Some(Approval::Open);
        }
        doc.join_password().map(|_| Approval::Password)
    }

    /// Whether the approver reads `request` at all: every one on an open vault, only one that
    /// carries a proof on a vault with a password.
    fn reads(self, request: &Request) -> bool {
        match self {
            Approval::Password => request.proof.is_some(),
            Approval::Open => true,
        }
    }
}

/// Decode policy only from the configured authority's cached envelope at the recorded
/// sequence and digest. Materialization can be incomplete while that admin locker is valid.
fn admin_here_approving(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> crate::Result<Option<Approval>> {
    let Some(pin) = config.signer else {
        return Ok(None);
    };
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)?
        .ok_or_else(|| Error::new("no cached vault to check approval"))?;
    let (envelope, _, _) = envelope::verify_for_reader(
        cached.as_slice(),
        &config.name,
        &pin,
        &config.followed,
        None,
    )?;
    if envelope.seq != state.seq || Some(envelope.digest()) != state.digest {
        return Err(Error::new("the cached vault is not the recorded version"));
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let Some(locker) = envelope.admin_locker() else {
        return Ok(None);
    };
    let plain = match age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes()) {
        Ok(plain) => plain,
        Err(age::AgeError::NoMatch) => return Ok(None),
        Err(error) => return Err(Error::new(format!("admin locker: {error}"))),
    };
    Ok(Approval::of(&parse_admin(&plain)?.doc))
}

struct ApprovalCheck {
    version: ([u8; 32], u64),
    digest: Option<[u8; 32]>,
    at: Instant,
    approval: Option<Approval>,
}

/// Read one coherent local source/version, retaining its decoded policy for at most five
/// minutes. A join can change the source without changing the sequence.
fn approval_here(
    paths: &Paths,
    checked: &mut Option<ApprovalCheck>,
) -> crate::Result<Option<(Config, u64, Approval)>> {
    let Some(_lock) = StateLock::observe(paths)? else {
        if config(paths)?.is_none() {
            return Ok(None);
        }
        return Err(Error::new(
            "vault state is being changed; check approval again",
        ));
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    let seq = state.seq;
    let version = (source_identity(&config), seq);
    if state
        .source_identity
        .is_some_and(|source| source != version.0)
    {
        *checked = None;
        return Err(Error::new("the recorded vault belongs to another source"));
    }
    if checked.as_ref().is_none_or(|held| {
        held.version != version
            || held.digest != state.digest
            || held.at.elapsed() >= Duration::from_secs(300)
    }) {
        // A transient mismatch must not become a retained negative policy: a completed
        // sync of this version can repair its cache without changing the sequence.
        *checked = None;
        let approval = admin_here_approving(paths, &config, &state)?;
        *checked = Some(ApprovalCheck {
            version,
            digest: state.digest,
            at: Instant::now(),
            approval,
        });
    }
    Ok(checked
        .as_ref()
        .and_then(|held| held.approval)
        .map(|approval| (config, seq, approval)))
}

/// The approver half of an admin's subscriber: while this machine is an admin of a vault with
/// a join password or an open one, read the join queue every 2 s and approve what proves the
/// password, or (open) what comes from a machine not in the vault. What [`auto_approve`]
/// judged stays [`Settled`] until the source or version changes, so a queue that holds only
/// requests already judged (refused, left to a person, or approved and still queued for days)
/// costs one GET a pass, never another admin open, key derivation or publish.
fn approver_loop(paths: &Paths, log: &(dyn Fn(&str) + Sync), stop: &AtomicBool) {
    let mut settled = Settled::default();
    let mut checked = None;
    while !stop.load(Ordering::Relaxed) {
        let (config, seq, approval) = match approval_here(paths, &mut checked) {
            Ok(Some(held)) => held,
            Ok(None) => {
                nap(stop, Duration::from_secs(30));
                continue;
            }
            Err(_) => {
                nap(stop, Duration::from_secs(2));
                continue;
            }
        };
        let pending = match requests_for(&config, None) {
            Ok(pending) => pending,
            Err(_) => {
                nap(stop, Duration::from_secs(10));
                continue;
            }
        };
        let by = match approval {
            Approval::Password => "by the join password",
            Approval::Open => "as the vault is open",
        };
        if settled.due(&config, seq, &pending, approval) {
            let done = admin_open(paths).and_then(|admin| {
                if admin.config != config || admin.seq != seq {
                    return Err(Error::new("the vault changed while checking approval"));
                }
                auto_approve_opened(paths, &admin)
            });
            match done {
                Ok(done) => {
                    if !done.approved.is_empty() {
                        log(&format!("approved {} {by}", done.approved.join(", ")));
                    }
                    settled.settle(done.judged);
                }
                Err(error) => {
                    log(&format!("approving {by} failed: {error}"));
                    nap(stop, Duration::from_secs(30));
                }
            }
        }
        nap(stop, Duration::from_secs(2));
    }
}

/// A join request as [`auto_approve`] read it. A machine that asks again under the same key
/// with another proof (its person mistyped the password and ran join again), or another
/// name, is another request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Judged {
    /// The machine's key.
    pub recipient: String,
    /// The name it asked for.
    pub host: String,
    /// Its proof of the join password, if it sent one.
    pub proof: Option<String>,
}

impl Judged {
    fn of(request: &Request) -> Judged {
        Judged {
            recipient: request.recipient.clone(),
            host: request.host.clone(),
            proof: request.proof.clone(),
        }
    }
}

/// What the approver judged at one version of the vault: the decision of each
/// [`approver_loop`] pass.
#[derive(Debug, Default)]
pub struct Settled {
    version: Option<([u8; 32], u64)>,
    judged: std::collections::BTreeSet<Judged>,
}

impl Settled {
    /// Whether a pass that holds vault version `seq`, whose vault approves by `approval`, and
    /// lists `listed` runs [`auto_approve`]: some listed request the approver reads (one with
    /// a proof; on an open vault, any) was not judged at that source/version. A different
    /// source or new version forgets every judgment, since it may carry a new password,
    /// open or close the vault, or have let a machine go.
    pub fn due(
        &mut self,
        config: &Config,
        seq: u64,
        listed: &[Request],
        approval: Approval,
    ) -> bool {
        let version = (source_identity(config), seq);
        if self.version != Some(version) {
            self.judged.clear();
            self.version = Some(version);
        }
        listed
            .iter()
            .filter(|request| approval.reads(request))
            .any(|request| !self.judged.contains(&Judged::of(request)))
    }

    /// Remember what an [`auto_approve`] judged, at the version the last [`Settled::due`] held.
    pub fn settle(&mut self, judged: Vec<Judged>) {
        self.judged.extend(judged);
    }
}

/// What [`auto_approve`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoApproved {
    /// The machine names added.
    pub approved: Vec<String>,
    /// The keys whose proof did not match the password.
    pub refused: Vec<String>,
    /// On an open vault, the keys its `code-only` lines name that are asking (a machine it let
    /// go, or one that asked before it opened): left to a person, who approves one by its code.
    pub code_only: Vec<String>,
    /// Every request it read and judged: approved, refused, left to a person, or already a
    /// machine of the vault. On a vault with a password it reads only proof-carrying ones.
    pub judged: Vec<Judged>,
    /// The version published, when any machine was added.
    pub seq: Option<u64>,
}

/// Approve, in one publish, every queued request whose proof matches the vault's join
/// password; on an open vault, every queued request from a key not in `[machines]`, with or
/// without a proof, but for a key a `code-only` line names; each named by [`free_name`]. Only
/// requests whose line is valid and whose code matches its key are read ([`requests`]).
/// Nothing happens on a vault with neither.
pub fn auto_approve(paths: &Paths) -> crate::Result<AutoApproved> {
    let admin = admin_open(paths)?;
    auto_approve_opened(paths, &admin)
}

fn auto_approve_opened(paths: &Paths, admin: &Admin) -> crate::Result<AutoApproved> {
    let mut done = AutoApproved::default();
    let Some(approval) = Approval::of(&admin.doc) else {
        return Ok(done);
    };
    let name = &admin.config.name;
    let key = admin
        .doc
        .join_password()
        .filter(|_| approval == Approval::Password)
        .map(|password| join::Key::derive(name, password.as_slice()));
    let members: Vec<String> = admin
        .doc
        .machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .collect();
    let code_only = admin.doc.code_only();
    let mut doc = admin.doc.clone();
    // Members' requests are read too: an approved request stays queued for days, and the
    // approver settles it only once it is judged.
    for request in requests_for(&admin.config, None)? {
        if !approval.reads(&request) {
            continue;
        }
        let judged = Judged::of(&request);
        if !members.contains(&request.recipient) {
            let admitted = match approval {
                Approval::Open => !code_only.contains(&request.recipient),
                Approval::Password => {
                    key.as_ref()
                        .zip(request.proof.as_ref())
                        .is_some_and(|(key, proof)| {
                            key.verify(name, &request.recipient, &request.host, proof)
                        })
                }
            };
            if admitted {
                let host = free_name(&doc, &request.host);
                doc.add_machine(&host, &request.recipient)
                    .map_err(|e| Error::new(e.to_string()))?;
                done.approved.push(host);
            } else if approval == Approval::Password {
                done.refused.push(request.recipient.clone());
            } else {
                done.code_only.push(request.recipient.clone());
            }
        }
        done.judged.push(judged);
    }
    if !done.approved.is_empty() {
        done.seq = Some(admin_publish(paths, admin, &doc)?.seq);
    }
    Ok(done)
}

// ── Probe ───────────────────────────────────────────────────────────────────

/// A vault that answered a [`probe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// The version it serves.
    pub seq: u64,
    /// Its signer's fingerprint.
    pub fingerprint: String,
}

/// What a [`probe`] heard back from a server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The vault asked for, signed by the pin or by a key it endorsed.
    Found(Probed),
    /// The server holds no such vault (or, for [`probe_default`], no signer is compiled in).
    Absent,
    /// Something answered with what is not that vault: another signer's vault, a vault of
    /// another name, no envelope at all, or an answer no vault server gives a GET (a
    /// redirect, a demand for credentials, bytes that are not HTTP:
    /// [`transport::Answer::Foreign`]). Asking again tells nothing new, unlike a failure to
    /// reach the server. What failed, never a value.
    NotThisVault(String),
}

/// Whether a vault answers at `url`, signed by `pin` or by a key `pin` endorsed. An error:
/// nothing answered (no route, no such name, a timeout, a connection closed unanswered) or
/// the server refused the GET as a vault server can (a `5xx`, a `408`, a `400`), either of
/// which a later try may get past. Reads and writes nothing on disk; one GET, bounded by
/// `wait` plus [`transport::FETCH_MARGIN`] (a `.local` lookup is the resolver's; see
/// [`crate::transport`]).
pub fn probe(url: &str, pin: &[u8; 32], wait: Duration) -> crate::Result<ProbeOutcome> {
    let name = transport::vault_name(&Source::Url(url.to_string()))?;
    let body = match transport::get(url, None, wait)? {
        transport::Answer::Fetched(Fetched::Body(body)) => body,
        transport::Answer::Fetched(Fetched::Missing | Fetched::NoNewer) => {
            return Ok(ProbeOutcome::Absent);
        }
        transport::Answer::Foreign(why) => return Ok(ProbeOutcome::NotThisVault(why)),
    };
    // The body arrived whole, so a failure here is what the server serves, not the way to it.
    let verified = envelope::verify_for_reader(&body, &name, pin, &[], None);
    Ok(match verified {
        Ok((envelope, _, _)) => ProbeOutcome::Found(Probed {
            seq: envelope.seq,
            fingerprint: envelope::fingerprint(&envelope.signer),
        }),
        Err(error) => ProbeOutcome::NotThisVault(error.to_string()),
    })
}

/// [`probe`] of the ALab vault ([`crate::ALAB_DEFAULT_URL`] under [`crate::ALAB_SIGNER`]):
/// what `aterm init` and aterm's first launch ask before they ask a person anything, so a
/// Mac on the ALab LAN is offered only the join password. [`ProbeOutcome::Absent`] also
/// when no signer is compiled in.
pub fn probe_default(wait: Duration) -> crate::Result<ProbeOutcome> {
    match decode_key32(crate::ALAB_SIGNER) {
        Some(pin) => probe(crate::ALAB_DEFAULT_URL, &pin, wait),
        None => Ok(ProbeOutcome::Absent),
    }
}

// ── Join ────────────────────────────────────────────────────────────────────

fn request_join(paths: &Paths, config: &Config, proof: Option<String>) -> crate::Result<String> {
    let identity = machine_identity(paths)?;
    let recipient = identity.recipient();
    let code = join_code(&recipient);
    if let Source::Url(url) = &config.source {
        let mut line = format!("{} {} {code}", recipient.to_bech32(), config.host);
        if let Some(proof) = &proof {
            line.push_str(&format!(" proof={proof}"));
        }
        transport::post_join(url, &line)?;
    }
    let _lock = StateLock::take(paths)?;
    // The state is the vault's that source.toml names now: a join to another vault since
    // this request (a person's, while the subscriber asked again) reset it for that one. The
    // same vault under a pin moved since (a rotation the join's own sync followed) is still
    // this request's.
    let current = self::config(paths)?;
    if current
        .as_ref()
        .is_none_or(|current| (&current.source, &current.name) != (&config.source, &config.name))
    {
        return Ok(code);
    }
    let mut state = load_state(paths)?;
    state.requested = now();
    state.proof = proof;
    // A revoked machine that asks again waits like a new one, so its subscriber re-queues the
    // request hourly.
    if matches!(state.membership, Stored::None | Stored::Revoked) {
        state.membership = Stored::Pending;
    }
    save_state(paths, &state)?;
    Ok(code)
}

/// The compiled-in pin for a source: the ALab key for the ALab URL.
fn default_pin(source: &Source) -> Option<[u8; 32]> {
    match source {
        Source::Url(url) if url == crate::ALAB_DEFAULT_URL => decode_key32(crate::ALAB_SIGNER),
        _ => None,
    }
}

/// Join a vault (§7): configure the source, make the key, verify, request access if no
/// locker opens (with a proof of the join password when the person gives one), sync. The
/// machine identity may be created before the fetch. The source configuration and membership
/// state change only after the vault verifies under its pin: a `#<signer>` fragment, else the
/// pin this machine already holds for the same source, else the compiled-in ALab key, else
/// (at a terminal) the fingerprint the person confirms.
pub fn join(paths: &Paths, options: &JoinOptions<'_>) -> crate::Result<JoinOutcome> {
    let typed = options.source.trim();
    let typed = if typed.is_empty() {
        crate::ALAB_DEFAULT_URL
    } else {
        typed
    };
    let (source, fragment) = transport::parse_source(typed, &paths.home)?;
    let name = transport::vault_name(&source)?;
    let source_display = match &source {
        Source::Url(url) => transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    };
    let host = match &options.host {
        Some(host) if text::valid_machine_name(host) => host.clone(),
        Some(host) => return Err(Error::new(format!("{host:?} is not a valid machine name"))),
        None => default_host_name(),
    };
    let identity = machine_identity(paths)?;
    let recipient = identity.recipient();
    let recipient_text = recipient.to_bech32();
    let code = join_code(&recipient);
    (options.say)(&format!(
        "this machine: {host}, code {code}, key {recipient_text}"
    ));
    let bytes = match transport::fetch(&source, None, Duration::from_secs(30))? {
        Fetched::Body(bytes) => bytes,
        Fetched::Missing => return Err(Error::new(format!("{source_display}: no vault there"))),
        Fetched::NoNewer => return Err(Error::new("the source answered with no vault")),
    };
    let previous = self::config(paths)?;
    if !is_envelope(&bytes) {
        let Source::Path(path) = &source else {
            return Err(Error::new(format!(
                "{source_display} does not serve an aterm secrets vault"
            )));
        };
        let config = Config {
            source: source.clone(),
            name,
            signer: None,
            followed: Vec::new(),
            host,
        };
        plain_entries(&config, path)?;
        {
            let _lock = StateLock::take(paths)?;
            // What an earlier source wrote stays tracked; its versions do not carry over.
            let mut state = load_state(paths)?;
            state.seq = 0;
            state.digest = None;
            state.membership = Stored::Plain;
            prepare_materialization(paths, &mut state)?;
            save_config(paths, &config)?;
        }
        sync(paths, None)?;
        return Ok(JoinOutcome::Plain {
            names: load_state(paths)?.manifest.files.keys().cloned().collect(),
        });
    }
    let parsed = envelope::parse(&bytes)?;
    parsed.verify_signature()?;
    let same_place = previous
        .as_ref()
        .is_some_and(|previous| previous.source == source && previous.name == name);
    let stored = previous
        .as_ref()
        .filter(|_| same_place)
        .and_then(|previous| previous.signer);
    let pin = match fragment.or(stored).or_else(|| default_pin(&source)) {
        Some(pin) => pin,
        None => {
            let shown = envelope::fingerprint(&parsed.signer);
            if !options.interactive {
                return Err(Error::new(format!(
                    "{source_display} is signed by {shown}, which nothing pins: run this in a terminal to confirm it, or add #<signer> to the URL"
                )));
            }
            if !(options.confirm)(&shown) {
                return Err(Error::new(
                    "the vault's signing key was not confirmed; nothing changed",
                ));
            }
            parsed.signer
        }
    };
    // The pin this machine already holds keeps the keys it was reached through.
    let followed = previous
        .as_ref()
        .filter(|previous| same_place && previous.signer == Some(pin))
        .map(|previous| previous.followed.clone())
        .unwrap_or_default();
    // Admit the source's expected vault name before changing this machine's config or state.
    let (parsed, checked, _) = envelope::verify_for_reader(&bytes, &name, &pin, &followed, None)?;
    let (pinned, followed) = match checked {
        Pin::Same => (pin, followed),
        Pin::Rotated(new) | Pin::Replaced(new) => (new, chain_keys(&parsed)),
    };
    let config = Config {
        source: source.clone(),
        name: name.clone(),
        signer: Some(pinned),
        followed,
        host: host.clone(),
    };
    {
        let _lock = StateLock::take(paths)?;
        let replaced = matches!(checked, Pin::Replaced(_));
        let mut state = load_state(paths)?;
        if !same_place || stored != Some(pin) || replaced {
            // Another vault, or another signing authority (a pin that gave way among them):
            // what the old one wrote stays, but its versions do not carry over. So too with no
            // source.toml at all: a state.toml left beside it holds another vault's seq, which
            // would read as a rollback.
            state.seq = 0;
            state.digest = None;
            state.membership = Stored::None;
            prepare_materialization(paths, &mut state)?;
        } else if previous.as_ref() != Some(&config) {
            prepare_materialization(paths, &mut state)?;
        }
        save_config(paths, &config)?;
    }
    if let SyncOutcome::Applied { seq, report } = sync(paths, None)? {
        return Ok(JoinOutcome::Joined {
            seq,
            names: names_of(&report),
        });
    }
    if let Some(JoinOutcome::Joined { seq, names }) = already_in(paths)? {
        return Ok(JoinOutcome::Joined { seq, names });
    }
    match &source {
        Source::Url(_) => {
            let proof = (options.password)().map(|password| {
                join::Key::derive(&name, password.as_slice()).prove(&name, &recipient_text, &host)
            });
            let by_password = proof.is_some();
            request_join(paths, &config, proof)?;
            (options.say)(&if by_password {
                format!(
                    "asked to join {name} with the join password; an admin Mac's subscriber approves it"
                )
            } else {
                format!("asked to join {name}. On an admin Mac run: aterm secrets approve {code}")
            });
            if by_password {
                (options.say)(&mistyped_hint(options, &source, &host));
            }
        }
        Source::Path(_) => {
            let _lock = StateLock::take(paths)?;
            let mut state = load_state(paths)?;
            state.membership = Stored::Pending;
            save_state(paths, &state)?;
            (options.say)(&format!(
                "not in {name} yet. On an admin Mac run: aterm secrets approve {recipient_text} --name {host}"
            ));
        }
    }
    (options.requested)();
    if !options.wait.is_zero() {
        (options.say)(
            "waiting for the approval (Ctrl-C stops waiting; this Mac joins by itself once approved)",
        );
        let deadline = Instant::now() + options.wait;
        while Instant::now() < deadline {
            let started = Instant::now();
            let left = deadline
                .saturating_duration_since(started)
                .min(Duration::from_secs(50));
            let wait = match source {
                Source::Url(_) => Some(left),
                Source::Path(_) => None,
            };
            match sync(paths, wait) {
                Ok(SyncOutcome::Applied { seq, report }) => {
                    return Ok(JoinOutcome::Joined {
                        seq,
                        names: names_of(&report),
                    });
                }
                // The subscriber `requested` installed long-polls the same vault and may apply
                // the version that lets this machine in first; this pass then finds it current.
                Ok(_) => {
                    if let Some(joined) = already_in(paths)? {
                        return Ok(joined);
                    }
                }
                Err(error) => (options.say)(&format!("still waiting ({error})")),
            }
            let pause = Duration::from_secs(5).saturating_sub(started.elapsed());
            std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
    Ok(JoinOutcome::Pending { code, host })
}

/// What a join that asked with the join password says as the request is queued. Nothing tells
/// a machine its proof was refused (the approver leaves it queued), so the person is told the
/// way out before any wait, which a Ctrl-C may end: a join again sends a new proof. Without a
/// terminal the password came from a pipe (`--password-stdin`), and a join again that is not
/// given it asks by code alone, a request that replaces the one with the proof; so the way
/// out pipes it again.
fn mistyped_hint(options: &JoinOptions<'_>, source: &Source, host: &str) -> String {
    let mut again = String::from("aterm secrets join");
    let other = !matches!(source, Source::Url(url) if url == crate::ALAB_DEFAULT_URL);
    if other {
        again.push_str(" <vault-url>");
    }
    if options.host.is_some() {
        again.push_str(&format!(" --name {host}"));
    }
    let fix = if options.interactive {
        format!("run {again} again, and it asks again")
    } else {
        format!(
            "pipe it again to {again} --password-stdin, or run {again} in a terminal, which asks for it"
        )
    };
    let source_note = if other {
        "; replace <vault-url> with this vault's full configured URL, including any query or signer pin"
    } else {
        ""
    };
    format!(
        "if no approval comes within a minute, the join password may be mistyped: {fix}{source_note}"
    )
}

/// A machine already in whose sync found nothing newer.
fn already_in(paths: &Paths) -> crate::Result<Option<JoinOutcome>> {
    let state = load_state(paths)?;
    Ok(
        (state.membership == Stored::In).then(|| JoinOutcome::Joined {
            seq: state.seq,
            names: state.manifest.files.keys().cloned().collect(),
        }),
    )
}

// ── Admin ───────────────────────────────────────────────────────────────────

/// An open vault, as an admin sees it: the text, the signing seed, and where it came from.
pub struct Admin {
    /// The vault text.
    pub doc: Document,
    /// The source config.
    pub config: Config,
    seed: [u8; 32],
    seq: u64,
    /// The handoff chain from the vault's first key to the current one, carried whole on
    /// every version, so a machine pinned to any earlier key (asleep across rotations, or a
    /// new Mac whose build pins an older key) still follows.
    handoff: Vec<([u8; 32], [u8; 64])>,
    /// How many handoff pairs the version opened carried (fewer than `handoff` when this
    /// machine's copy put back links that version dropped).
    opened_pairs: usize,
}

impl Drop for Admin {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.seed);
    }
}

impl std::fmt::Debug for Admin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Admin({} seq {})", self.config.name, self.seq)
    }
}

impl Admin {
    /// The version this was opened at.
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

fn seq_of(bytes: &[u8]) -> Option<u64> {
    envelope::parse(bytes).ok().map(|parsed| parsed.seq)
}

/// Open the vault as an admin: fetch the current envelope, verify it, and decrypt its admin
/// locker with this machine's key. A source that lost the vault, serves an older version than
/// this machine verified (a server moved to another Mac, a disk restored), or serves one this
/// machine does not verify at all (a store the removed admin fed before a build retired its
/// key) gets this machine's copy back first: same signer, higher seq, or a held key that
/// gives way, so the server takes it. A served version that dropped links of the handoff
/// chain this machine holds (a rotation by a build before chains, through a server that took
/// it) is opened with those links put back, so the next version restores them.
pub fn admin_open(paths: &Paths) -> crate::Result<Admin> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let pin = config
        .signer
        .ok_or_else(|| Error::new("a plain text file has no admins: edit the file itself"))?;
    let state = load_state(paths)?;
    let held = floor(paths, &state, &config.name, &config);
    let verifies = |bytes: &[u8]| {
        envelope::verify_for_reader(bytes, &config.name, &pin, &config.followed, None).is_ok()
    };
    let served = match transport::fetch(&config.source, None, Duration::from_secs(30))? {
        Fetched::Body(bytes) => Some(bytes),
        Fetched::Missing | Fetched::NoNewer => None,
    };
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)?
        .map(|bytes| bytes.as_slice().to_vec())
        .filter(|bytes| verifies(bytes));
    // Plainly not the vault's, whatever its seq: bytes that are no envelope, one its own
    // signature does not verify, one signed by a retired key or by a key this machine's pin
    // moved past (the old key writing on after a rotation), or one claiming a handoff from a
    // key this machine follows that the key never endorsed. A version that is merely
    // unverifiable here may be the vault this machine fell behind, and is never written over.
    let known: Vec<[u8; 32]> = std::iter::once(pin)
        .chain(config.followed.iter().copied())
        .collect();
    let foreign = |served: &[u8]| match envelope::parse(served) {
        Err(_) => true,
        Ok(served) => {
            let retired = Retired::compiled();
            served.verify_signature().is_err()
                || served.check_signer(&retired).is_err()
                || retired.gives_way(&served.signer, &chain_keys(&served))
                || config.followed.contains(&served.signer)
                || served.forges_handoff_from(&known)
        }
    };
    // The copy is put back once this machine has shown it is an admin, below: `over`, the
    // served bytes it may replace at a path whatever their seq, when those are foreign.
    let mut put_back: Option<Option<Vec<u8>>> = None;
    let bytes = match (served, cached) {
        // At a path, which judges nothing, a version that is merely unverifiable here may be
        // the vault this machine fell behind (or this machine followed a fork): its copy goes
        // over nothing, and a person settles which is the vault (§8). A server judges itself.
        (Some(served), Some(_))
            if matches!(config.source, Source::Path(_))
                && !verifies(&served)
                && !foreign(&served) =>
        {
            let place = match &config.source {
                Source::Path(path) => path.display().to_string(),
                Source::Url(url) => transport::describe(url),
            };
            // Not foreign, so it parses.
            let served = envelope::parse(&served)?;
            let aside = "move the file aside and the next admin command puts this machine's \
                         copy back";
            // The key is not offered: nothing this machine follows vouches for it, so it is
            // taken only from an admin who holds the vault.
            return Err(Error::new(if served.name == config.name {
                format!(
                    "{place} holds a version of {} signed by {}, a key this machine does not \
                     follow: a fork, or a rotation by a build before handoff chains. If it is \
                     not the vault, {aside}; if it is, `aterm secrets status` on an admin Mac \
                     that holds it names its key, and if that key's fingerprint is this one, \
                     aterm secrets join '{place}#<that key>' follows it",
                    config.name,
                    envelope::fingerprint(&served.signer)
                )
            } else {
                format!(
                    "{place} holds the vault {}, not {}: if this machine's vault belongs there, \
                     {aside}",
                    served.name, config.name
                )
            }));
        }
        (Some(served), Some(cached))
            if !verifies(&served)
                || seq_of(&cached).unwrap_or(0) > seq_of(&served).unwrap_or(0) =>
        {
            put_back = Some((!verifies(&served) && foreign(&served)).then_some(served));
            cached
        }
        (Some(served), _) => served,
        (None, Some(cached)) => {
            put_back = Some(None);
            cached
        }
        (None, None) => return Err(missing(&config)),
    };
    // The held chain is judged below, where links the served version dropped are put back
    // rather than refused.
    let unchained = held.as_ref().map(|floor| Floor {
        chain: None,
        ..floor.clone()
    });
    let (envelope, pinned, _) = envelope::verify_for_reader(
        &bytes,
        &config.name,
        &pin,
        &config.followed,
        unchained.as_ref(),
    )?;
    let handoff = match held.and_then(|floor| floor.chain) {
        Some((signer, chain)) if !matches!(pinned, Pin::Replaced(_)) => {
            envelope.chain_carried_on(&signer, &chain)?
        }
        _ => envelope.handoff.clone(),
    };
    let identity = machine_identity(paths)?;
    let locker = envelope
        .admin_locker()
        .ok_or_else(|| Error::new("this vault has no admin locker"))?;
    let plain = match age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes()) {
        Ok(plain) => plain,
        Err(age::AgeError::NoMatch) => {
            return Err(Error::new(format!(
                "this machine is not an admin of {}: an admin can run aterm secrets admins add {}",
                config.name, config.host
            )));
        }
        Err(error) => return Err(Error::new(format!("admin locker: {error}"))),
    };
    let opened = parse_admin(&plain)?;
    if envelope::signer_public(&opened.seed)? != envelope.signer {
        return Err(Error::new(
            "the admin locker's key does not match the envelope's signer",
        ));
    }
    if let Some(over) = put_back {
        let _lock = configured_admin(paths, &config)?;
        match over {
            Some(over) => transport::replace(&config.source, &bytes, &over)?,
            None => transport::publish(&config.source, &bytes)?,
        }
    }
    Ok(Admin {
        doc: opened.doc.clone(),
        config,
        seed: opened.seed,
        seq: envelope.seq,
        handoff,
        opened_pairs: envelope.handoff.len(),
    })
}

fn recipients_of(names: &[String], doc: &Document) -> crate::Result<Vec<Recipient>> {
    let machines = doc.machines();
    let mut out = Vec::new();
    for name in names {
        let machine = machines
            .iter()
            .find(|machine| &machine.name == name)
            .ok_or_else(|| Error::new(format!("{name} is not in [machines]")))?;
        out.push(
            Recipient::parse(&machine.recipient)
                .map_err(|_| Error::new(format!("{name}: bad key")))?,
        );
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Encrypt and sign `doc` as version `seq`, carrying `handoff`: the chain from the vault's
/// first key to `seed`'s public half, empty until the first rotation.
fn seal(
    name: &str,
    seq: u64,
    seed: &[u8; 32],
    doc: &Document,
    handoff: &[([u8; 32], [u8; 64])],
) -> crate::Result<Vec<u8>> {
    let admins = recipients_of(&doc.admins(), doc)?;
    if admins.is_empty() {
        return Err(Error::new("the vault needs at least one admin"));
    }
    let mut lockers = vec![Locker {
        label: "admin".to_string(),
        armored: age::armor(&age::encrypt(
            &admins,
            admin_plaintext(seed, doc).as_slice(),
        )?),
    }];
    for (index, plan) in doc.lockers().into_iter().enumerate() {
        let mut recipients = Vec::new();
        for text in &plan.recipients {
            recipients.push(Recipient::parse(text).map_err(|_| Error::new("bad machine key"))?);
        }
        if recipients.is_empty() {
            continue;
        }
        lockers.push(Locker {
            label: (index + 1).to_string(),
            armored: age::armor(&age::encrypt(&recipients, plan.text.as_slice())?),
        });
    }
    envelope::sign(
        &Unsigned {
            name: name.to_string(),
            seq,
            time: now(),
            handoff: handoff.to_vec(),
            lockers,
        },
        seed,
    )
}

/// What a publish did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The version published.
    pub seq: u64,
    /// Whether it is signed by a new vault key the old one endorsed.
    pub rotated: bool,
    /// On a rotation of the ALab vault (known by its keys: a chain from [`crate::ALAB_SIGNER`]
    /// or a key of [`crate::ALAB_RETIRED`], whatever URL this machine reaches it by): the keys
    /// aterm's next release must carry. Until a build does, a new Mac also trusts the key this
    /// rotation replaced, which every admin of the last version held (an admin removed too).
    pub alab_release: Option<AlabRelease>,
    /// Whether it is the first version of the vault to carry two or more handoff pairs, which
    /// an aterm built before handoff chains cannot read: such a machine fails every sync, and
    /// such a server refuses the version, until it is updated (§5).
    pub first_long_chain: bool,
    /// Whether it carries a new join password: this text let a machine go or dropped an
    /// admin, and kept the password either may have known.
    pub password_renewed: bool,
    /// Whether this text let a machine go from an open vault. Its key is now a `code-only`
    /// line, so the request it joined with, which the server keeps queued for days, or any it
    /// makes under that key, does not let it straight back in.
    pub kept_out: bool,
    /// When this text opened the vault: the requests already waiting, each key now on a
    /// `code-only` line, so a machine let go before (while the vault was closed, or by an
    /// aterm that wrote no such line) is not let straight back in. A person approves each
    /// they know by its code.
    pub held: Vec<Request>,
    /// Why this machine's own sync after the publish failed, if it did. The publish stands.
    pub local: Option<String>,
}

/// The compiled-in keys a rotation of the ALab vault calls for (§5), padded base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlabRelease {
    /// [`crate::ALAB_SIGNER`]'s new value: the key that signs now.
    pub signer: String,
    /// [`crate::ALAB_RETIRED`]'s new value: what it holds, then every other key the vault
    /// rotated away from, oldest first.
    pub retired: Vec<String>,
}

/// What a rotation to `signer`, whose chain is now `handoff`, calls for in aterm's next
/// release: nothing unless it is the ALab vault, which a build knows by its keys (`retired`:
/// what it carries), not by the URL, since an admin may reach the vault at any address (the
/// server's own Mac by loopback, another by its host name). The old key is the chain's last.
fn alab_release(
    retired: &Retired,
    handoff: &[([u8; 32], [u8; 64])],
    signer: &[u8; 32],
) -> Option<AlabRelease> {
    if !handoff.iter().any(|(key, _)| retired.knows(key)) {
        return None;
    }
    let mut retired: Vec<String> = retired
        .keys
        .iter()
        .map(|key| crate::b64::encode_padded(key))
        .collect();
    for (key, _) in handoff {
        let key = crate::b64::encode_padded(key);
        if !retired.contains(&key) {
            retired.push(key);
        }
    }
    Some(AlabRelease {
        signer: crate::b64::encode_padded(signer),
        retired,
    })
}

/// Publish `doc` as the next version: refuses a text that would leave this machine without
/// admin rights, then encrypts, signs, uploads and syncs this machine. When an admin of the
/// version it replaces is not one of this text (removed, revoked, or given another key), the
/// vault key rotates: every admin holds the old one. When a machine of that version is not
/// one of this text and the join password is the same, the published text carries a new
/// one: the machine let go may have joined with it, and its queued request, or one it makes
/// under a new key, would otherwise be approved again. So too when an admin of that version
/// is not an admin of this text: it read the password in the admin locker. On an open vault,
/// a machine let go leaves its key on a `code-only` line, and a text that opens the vault
/// puts every request already waiting on one ([`code_only_for`]).
pub fn admin_publish(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<Published> {
    publish_as(paths, admin, doc, false)
}

/// Publish `doc` signed by a new vault key that the current key endorses (§5): the
/// endorsement joins the handoff chain every later version carries, so every machine
/// re-pins to the new key as it reads any version that key signs, however many rotations it
/// slept through.
pub fn admin_rotate(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<Published> {
    publish_as(paths, admin, doc, true)
}

fn publish_as(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    rotate: bool,
) -> crate::Result<Published> {
    let seq = envelope::next_seq(admin.seq)?;
    let identity = machine_identity(paths)?;
    let mine = identity.recipient().to_bech32();
    let me = doc
        .machines()
        .into_iter()
        .find(|machine| machine.recipient == mine)
        .ok_or_else(|| {
            Error::new("this machine would no longer be in the vault; refusing to publish")
        })?;
    if !doc.admins().contains(&me.name) {
        return Err(Error::new(format!(
            "{} would no longer be an admin; refusing to publish (make another admin first)",
            me.name
        )));
    }
    let before = recipients_of(&admin.doc.admins(), &admin.doc)?;
    let after = recipients_of(&doc.admins(), doc)?;
    let rotate = rotate || before.iter().any(|old| !after.contains(old));
    let renewed = renew_join_password(&admin.doc, doc)?;
    let password_renewed = renewed.is_some();
    let doc = renewed.as_ref().unwrap_or(doc);
    let (recorded, kept_out, held) = code_only_for(&admin.config, &admin.doc, doc)?;
    let doc = recorded.as_ref().unwrap_or(doc);
    let (bytes, release, pairs) = if rotate {
        // The old key's endorsement of the new one joins the chain; every link before it
        // stays, so a machine pinned to any key the vault had still follows.
        let mut seed = [0u8; 32];
        aterm_uds::rand::fill(&mut seed)?;
        let sealed = envelope::signer_public(&seed).and_then(|new| {
            let old = envelope::signer_public(&admin.seed)?;
            let endorsement = envelope::endorse(&admin.seed, &admin.config.name, &new)?;
            let mut handoff = admin.handoff.clone();
            handoff.push((old, endorsement));
            let bytes = seal(&admin.config.name, seq, &seed, doc, &handoff)?;
            let release = alab_release(&Retired::compiled(), &handoff, &new);
            Ok((bytes, release, handoff.len()))
        });
        zeroize::Zeroize::zeroize(&mut seed);
        sealed?
    } else {
        let bytes = seal(&admin.config.name, seq, &admin.seed, doc, &admin.handoff)?;
        (bytes, None, admin.handoff.len())
    };
    {
        let _lock = configured_admin(paths, &admin.config)?;
        transport::publish(&admin.config.source, &bytes)?;
    }
    let local = sync(paths, None).err().map(|error| error.to_string());
    Ok(Published {
        seq,
        rotated: rotate,
        alab_release: release,
        first_long_chain: pairs >= 2 && admin.opened_pairs < 2,
        password_renewed,
        kept_out,
        held,
        local,
    })
}

/// Keep a local source change from passing an admin publication admitted for the source it
/// left. Hold the same state-writer lock through the source effect, then release it before
/// syncing the published version locally.
fn configured_admin(paths: &Paths, expected: &Config) -> crate::Result<StateLock> {
    let lock = StateLock::take(paths)?;
    if config(paths)?.as_ref() != Some(expected) {
        return Err(Error::new(
            "the configured vault changed while it was open; open it again before publishing",
        ));
    }
    Ok(lock)
}

/// The keys of `old`'s machines that `doc` lets go: their lines deleted, or their keys
/// replaced.
fn let_go(old: &Document, doc: &Document) -> Vec<String> {
    let kept: Vec<String> = doc.machines().into_iter().map(|m| m.recipient).collect();
    old.machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .filter(|recipient| !kept.contains(recipient))
        .collect()
}

/// What `doc`, published over `old`, leaves to approval by code when it is open: a
/// `code-only` line for each key of `old`'s machines it lets go, and, when it opens the vault,
/// for each key whose request is already waiting and is not one of its machines. The server
/// keeps an approved request queued for days, so either would otherwise be let straight
/// (back) in: the machine let go now, and one let go before, while the vault was closed or by
/// an aterm that wrote no such line, which the waiting requests cannot be told apart from
/// (`aterm secrets password open` asks a person about them first). Nothing on a vault `doc`
/// leaves closed, so a fleet that never opens one keeps a text every aterm reads. Returns the
/// text, when it changes; whether it let a machine go; and the waiting requests it held.
fn code_only_for(
    config: &Config,
    old: &Document,
    doc: &Document,
) -> crate::Result<(Option<Document>, bool, Vec<Request>)> {
    if !doc.open() {
        return Ok((None, false, Vec::new()));
    }
    let named = doc.code_only();
    let gone: Vec<String> = let_go(old, doc)
        .into_iter()
        .filter(|key| !named.contains(key))
        .collect();
    let waiting: Vec<Request> = if old.open() {
        Vec::new()
    } else {
        requests_for(config, Some(doc))?
            .into_iter()
            .filter(|request| {
                !named.contains(&request.recipient) && !gone.contains(&request.recipient)
            })
            .collect()
    };
    if gone.is_empty() && waiting.is_empty() {
        return Ok((None, false, Vec::new()));
    }
    let mut held = doc.clone();
    for key in gone
        .iter()
        .chain(waiting.iter().map(|request| &request.recipient))
    {
        held.add_code_only(key)
            .map_err(|error| Error::new(error.to_string()))?;
    }
    Ok((Some(held), !gone.is_empty(), waiting))
}

/// `doc` with a new join password, when it lets a machine of `old` go (revoked, or its key
/// replaced) or drops an admin of `old` (the diff that rotates the vault key in
/// [`admin_publish`]), and keeps `old`'s password unchanged; `None` when nothing calls for
/// one. An admin read the password in the admin locker: one removed from `[admins]` stays a
/// machine, and could mint keys whose proofs the approver admits.
fn renew_join_password(old: &Document, doc: &Document) -> crate::Result<Option<Document>> {
    let let_go = !let_go(old, doc).is_empty();
    let admins = recipients_of(&doc.admins(), doc)?;
    let demoted = recipients_of(&old.admins(), old)?
        .iter()
        .any(|admin| !admins.contains(admin));
    let unchanged = match (old.join_password(), doc.join_password()) {
        (Some(before), Some(now)) => before.as_slice() == now.as_slice(),
        _ => false,
    };
    if !((let_go || demoted) && unchanged) {
        return Ok(None);
    }
    let mut renewed = doc.clone();
    renewed
        .set_join_password(Some(join::generate()?.as_slice()))
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(Some(renewed))
}

/// What [`create`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The vault's name.
    pub name: String,
    /// The signing key's public half (the pin), padded base64.
    pub signer: String,
    /// Its fingerprint.
    pub fingerprint: String,
    /// Names imported into `[everyone]`.
    pub imported: Vec<String>,
}

/// Create a vault at `source` (§8): a new signing key, this machine its only machine and
/// admin, a generated join password, the regular files of `import` (non-recursive; dot files
/// skipped) in `[everyone]`. Refuses a path where anything already exists, and a URL that
/// answers with anything but "no vault here".
pub fn create(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
) -> crate::Result<Created> {
    let (source, _) = transport::parse_source(source, &paths.home)?;
    let name = transport::vault_name(&source)?;
    match &source {
        Source::Path(path) => {
            if fsx::kind(path)? != fsx::Kind::Missing {
                return Err(Error::new(format!(
                    "{} already exists; create the vault at a new path",
                    path.display()
                )));
            }
        }
        Source::Url(_) => match transport::fetch(&source, None, Duration::from_secs(30))? {
            Fetched::Missing => {}
            Fetched::Body(bytes) if is_envelope(&bytes) => {
                return Err(Error::new(format!(
                    "a vault named {name} already exists there; join it instead"
                )));
            }
            _ => {
                return Err(Error::new(
                    "something other than a vault server answers at that URL",
                ));
            }
        },
    }
    let host = match host {
        Some(host) if text::valid_machine_name(host) => host.to_string(),
        Some(host) => return Err(Error::new(format!("{host:?} is not a valid machine name"))),
        None => default_host_name(),
    };
    let identity = machine_identity(paths)?;
    let mut doc = Document::new_vault(&host, &identity.recipient().to_bech32());
    let password = join::generate()?;
    doc.set_join_password(Some(password.as_slice()))
        .map_err(|e| Error::new(e.to_string()))?;
    let mut imported = Vec::new();
    if let Some(dir) = import {
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                (!name.starts_with('.') && text::valid_entry_name(&name))
                    .then(|| (name, entry.path()))
            })
            .collect();
        names.sort();
        for (name, path) in names {
            if fsx::kind(&path)? != fsx::Kind::File {
                continue;
            }
            let Some(value) = fsx::read_owned(&path, text::MAX_VALUE)? else {
                continue;
            };
            let value = SecretBytes::new(value);
            doc.set_entry(&Audience::Everyone, &name, value.as_slice())
                .map_err(|e| Error::new(format!("{name}: {e}")))?;
            imported.push(name);
        }
    }
    let mut seed = [0u8; 32];
    aterm_uds::rand::fill(&mut seed)?;
    let signer = envelope::signer_public(&seed)?;
    let bytes = seal(&name, 1, &seed, &doc, &[]);
    zeroize::Zeroize::zeroize(&mut seed);
    transport::publish(&source, &bytes?)?;
    let config = Config {
        source,
        name: name.clone(),
        signer: Some(signer),
        followed: Vec::new(),
        host,
    };
    {
        let _lock = StateLock::take(paths)?;
        let mut state = load_state(paths)?;
        state.seq = 0;
        state.digest = None;
        state.membership = Stored::None;
        prepare_materialization(paths, &mut state)?;
        save_config(paths, &config)?;
    }
    sync(paths, None)?;
    Ok(Created {
        name,
        signer: crate::b64::encode_padded(&signer),
        fingerprint: envelope::fingerprint(&signer),
        imported,
    })
}

/// A pending join request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The machine's key.
    pub recipient: String,
    /// The name it asked for.
    pub host: String,
    /// Its code.
    pub code: String,
    /// When it asked (unix), when the server says.
    pub at: Option<u64>,
    /// Its proof of the join password, if it sent one.
    pub proof: Option<String>,
}

/// The pending join requests at the configured URL, without those already in `[machines]`
/// when `doc` is given. Each code is recomputed from the key, never taken from the wire.
pub fn requests(paths: &Paths, doc: Option<&Document>) -> crate::Result<Vec<Request>> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    requests_for(&config, doc)
}

fn requests_for(config: &Config, doc: Option<&Document>) -> crate::Result<Vec<Request>> {
    let Source::Url(url) = &config.source else {
        return Ok(Vec::new());
    };
    let known: Vec<String> = doc
        .map(|doc| doc.machines().into_iter().map(|m| m.recipient).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for line in transport::list_joins(url)? {
        let Ok(request) = transport::parse_join_line(&line) else {
            continue;
        };
        let Ok(parsed) = Recipient::parse(&request.recipient) else {
            continue;
        };
        if join_code(&parsed) != request.code || known.contains(&request.recipient) {
            continue;
        }
        out.push(Request {
            recipient: request.recipient,
            host: request.host,
            code: request.code,
            at: request.time,
            proof: request.proof,
        });
    }
    Ok(out)
}

/// A machine name not yet in `doc`: `wanted`, else `wanted-2`, `wanted-3`, …, `wanted` cut
/// short (and never left ending in `-`) where the suffix would take the name past 63
/// characters. Every candidate is a valid name, so the search ends.
pub fn free_name(doc: &Document, wanted: &str) -> String {
    let taken: Vec<String> = doc.machines().into_iter().map(|m| m.name).collect();
    let base = if text::valid_machine_name(wanted) {
        wanted
    } else {
        "machine"
    };
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2u64..)
        .map(|n| {
            let suffix = format!("-{n}");
            let room = 63usize.saturating_sub(suffix.len());
            let stem = match base.get(..room) {
                // A valid name is ASCII, so the cut is a char boundary.
                Some(cut) if base.len() > room => cut.trim_end_matches('-'),
                _ => base,
            };
            format!("{stem}{suffix}")
        })
        .find(|name| !taken.contains(name) && text::valid_machine_name(name))
        .unwrap_or_else(|| "machine".to_string())
}

/// The machines of `current` whose key `base` does not hold: those admitted since `base`.
pub fn admitted_since(base: &Document, current: &Document) -> Vec<text::Machine> {
    let known: Vec<String> = base.machines().into_iter().map(|m| m.recipient).collect();
    current
        .machines()
        .into_iter()
        .filter(|machine| !known.contains(&machine.recipient))
        .collect()
}

/// Put back into `edited` (the text a person saved in `aterm secrets edit`, which began as
/// `base`) every machine of `current` (the version the save goes on top of) whose key neither
/// holds: one the approver or another admin admitted while the person edited. Published over
/// `current` without it, the text would let that machine go and renew the join password it
/// was let in with. A machine the person deleted was in `base`, so it stays deleted. So too
/// every `code-only` line `current` added that `edited` lacks, unless `edited` makes that key
/// a machine: without it the approver would let in the machine another admin let go. Returns
/// the names put back.
pub fn keep_admitted(
    base: &Document,
    current: &Document,
    edited: &mut Document,
) -> crate::Result<Vec<String>> {
    let mut kept = Vec::new();
    for machine in admitted_since(base, current) {
        if edited
            .machines()
            .iter()
            .any(|known| known.recipient == machine.recipient)
        {
            continue;
        }
        let name = free_name(edited, &machine.name);
        edited
            .add_machine(&name, &machine.recipient)
            .map_err(|e| Error::new(e.to_string()))?;
        kept.push(name);
    }
    let before = base.code_only();
    let members: Vec<String> = edited.machines().into_iter().map(|m| m.recipient).collect();
    for key in current.code_only() {
        if !before.contains(&key) && !members.contains(&key) {
            edited
                .add_code_only(&key)
                .map_err(|e| Error::new(e.to_string()))?;
        }
    }
    Ok(kept)
}

// ── Editing ─────────────────────────────────────────────────────────────────

/// An advisory lock on a file, released by `LOCK_UN` on drop (clippy.toml: the flock release
/// rule): what marks an edit directory as live.
struct HeldLock(std::fs::File);

impl HeldLock {
    /// Take the lock on `path` without waiting; `None` when someone holds it.
    fn try_take(path: &Path) -> crate::Result<Option<HeldLock>> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of HeldLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Some(HeldLock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(Error::new(format!("{}: {e}", path.display())))
            }
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// An exclusively created 0700 edit directory, locked while it lives. Dropping it keeps
/// its draft: an interrupted or unpublished edit may be the only copy of new work.
/// Later commands never sweep directories merely named `edit-*`.
pub struct Scratch {
    dir: PathBuf,
    _lock: HeldLock,
}

impl Scratch {
    /// Create a new edit directory. Existing directories are never adopted or swept.
    pub fn new(paths: &Paths) -> crate::Result<Scratch> {
        fsx::ensure_private_dir(&paths.dir)?;
        let dir = aterm_tempfile::Builder::new()
            .prefix("edit-")
            .tempdir_in(&paths.dir)
            .map_err(|error| Error::new(format!("{}: {error}", paths.dir.display())))?
            .keep();
        let lock = HeldLock::try_take(&dir.join(".lock"))?
            .ok_or_else(|| Error::new("a fresh edit directory is already locked"))?;
        Ok(Scratch { dir, _lock: lock })
    }

    /// The directory. Its contents survive this guard unless explicitly discarded.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Discard the direct draft file after it was published or found unchanged, provided
    /// it still holds `expected`, then its lock and empty directory. Other editor-created
    /// files are kept, never swept. The read and unlink are not one namespace transaction.
    pub fn discard(self, file: &Path, expected: &[u8]) -> crate::Result<()> {
        if file.parent() != Some(self.dir.as_path())
            || file.file_name().is_none_or(|name| name == ".lock")
        {
            return Err(Error::new(
                "an edit draft must be one file directly in its scratch directory",
            ));
        }
        if let Some(bytes) = fsx::read_private(file, text::MAX_TEXT)?
            && !aterm_digest::ct_eq(bytes.as_slice(), expected)
        {
            return Err(Error::new(format!(
                "{}: the draft changed before cleanup; kept",
                file.display()
            )));
        }
        fsx::wipe_and_remove(file)?;
        let lock = self.dir.join(".lock");
        match std::fs::remove_file(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::new(format!("{}: {error}", lock.display()))),
        }
        match std::fs::remove_dir(&self.dir) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(Error::new(format!("{}: {error}", self.dir.display()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_that_leaves_an_entry_failed_backs_off_and_a_clean_one_starts_again() {
        let url = Source::Url("http://127.0.0.1:7484/v1/alab".to_string());
        let applied = |failed: usize| SyncOutcome::Applied {
            seq: 2,
            report: crate::materialize::Report {
                failed: (0..failed)
                    .map(|n| (format!("entry{n}"), "is a symlink".to_string()))
                    .collect(),
                ..Default::default()
            },
        };
        let mut failing = FIRST_RETRY;
        let pauses: Vec<u64> = (0..8)
            .map(|_| pause_after(&url, &applied(1), &mut failing).as_secs())
            .collect();
        assert_eq!(pauses, [5, 10, 20, 40, 80, 160, 300, 300]);
        // A clean pass long-polls at once and starts the retries again; so does any other.
        assert_eq!(pause_after(&url, &applied(0), &mut failing), Duration::ZERO);
        assert_eq!(pause_after(&url, &applied(2), &mut failing), FIRST_RETRY);
        pause_after(&url, &applied(2), &mut failing);
        let unchanged = SyncOutcome::Unchanged { seq: 2 };
        assert_eq!(pause_after(&url, &unchanged, &mut failing).as_secs(), 10);
        assert_eq!(pause_after(&url, &applied(1), &mut failing), FIRST_RETRY);
        // A path source is read once a minute whatever the pass did.
        let path = Source::Path(PathBuf::from("/tmp/vault"));
        assert_eq!(pause_after(&path, &applied(1), &mut failing).as_secs(), 60);
    }

    /// A server for `store` at `addr` (port 0: any free one).
    fn serve(store: &Path, addr: &str) -> crate::server::Running {
        let listener = std::net::TcpListener::bind(addr).expect("bind");
        crate::server::start(
            listener,
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().expect("addr"),
                store: store.to_path_buf(),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .expect("server starts")
    }

    /// Run `flow` on this thread as a build that retired `keys` for `current` would.
    fn retiring<T>(keys: Vec<[u8; 32]>, current: [u8; 32], flow: impl FnOnce() -> T) -> T {
        let set = |retired: Option<Retired>| {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = retired);
        };
        set(Some(Retired {
            keys,
            current: Some(current),
        }));
        let out = flow();
        set(None);
        out
    }

    /// An unattended join that asks nobody anything.
    fn join_pinned(paths: &Paths, source: &str, host: &str) -> JoinOutcome {
        join(
            paths,
            &JoinOptions {
                source: source.to_string(),
                host: Some(host.to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .expect("join")
    }

    #[test]
    fn readiness_observation_creates_no_state_or_lock_and_never_changes_directory_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let reader = Paths::for_home(&root.path().join("absent"));
        let expected = Config {
            source: Source::Path(root.path().join("team.secrets")),
            name: "team".into(),
            signer: None,
            followed: Vec::new(),
            host: "reader".into(),
        };
        assert!(!materialization_ready(&reader).unwrap());
        assert!(!materialization_ready_for(&reader, &expected).unwrap());
        assert!(!reader.home.exists());

        std::fs::create_dir_all(&reader.dir).unwrap();
        std::fs::set_permissions(&reader.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!materialization_ready(&reader).unwrap());
        assert!(!reader.lock_file().exists());
        assert_eq!(
            std::fs::metadata(&reader.dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn readiness_observation_yields_to_writers_and_coexists_with_shared_readers() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.secrets");
        fsx::write_atomic(&source, b"token=inert-token\n", 0o600).unwrap();
        let reader = Paths::for_home(&root.path().join("reader"));
        join_pinned(&reader, source.to_str().unwrap(), "reader");
        let writer = StateLock::take(&reader).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let observing = reader.clone();
        let worker = std::thread::spawn(move || {
            send.send(materialization_ready(&observing)).unwrap();
        });
        // A hang detector, not a latency requirement: the writer stays locked
        // until this nonblocking observation has answered.
        let result = receive.recv_timeout(Duration::from_secs(60));
        drop(writer);
        worker.join().unwrap();
        assert!(
            !result
                .expect("readiness must not wait for the writer")
                .unwrap()
        );
        assert!(materialization_ready(&reader).unwrap());

        let shared = StateLock::observe(&reader).unwrap().expect("first reader");
        assert!(materialization_ready(&reader).unwrap());
        drop(shared);
        let writer = StateLock::take(&reader).unwrap();
        drop(writer);
        assert!(materialization_ready(&reader).unwrap());
    }

    #[test]
    fn readiness_refuses_a_changed_source_before_its_file_digests() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let first = root.path().join("first.secrets");
        let second = root.path().join("second.secrets");
        fsx::write_atomic(&first, b"token=first-token\n", 0o600).unwrap();
        fsx::write_atomic(&second, b"token=second-token\n", 0o600).unwrap();
        let reader = Paths::for_home(&root.path().join("reader"));
        join_pinned(&reader, first.to_str().unwrap(), "reader");
        let expected = status(&reader).unwrap().config.unwrap();
        assert!(materialization_ready_for(&reader, &expected).unwrap());
        join_pinned(&reader, second.to_str().unwrap(), "reader");
        let digests_read = std::cell::Cell::new(false);
        assert!(
            !materialization_ready_with(&reader, Some(&expected), |paths, state| {
                digests_read.set(true);
                files_intact(paths, state)
            })
            .unwrap()
        );
        assert!(!digests_read.get());
        assert!(!materialization_ready_for(&reader, &expected).unwrap());
        assert!(materialization_ready(&reader).unwrap());
        assert!(materialization_ready_for(&reader, &pinned(&reader)).unwrap());

        join_pinned(&reader, first.to_str().unwrap(), "reader");
        assert!(materialization_ready_for(&reader, &expected).unwrap());
    }

    #[test]
    fn readiness_lock_admission_refuses_symlinks_and_nonregular_files() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let reader = Paths::for_home(&root.path().join("reader"));
        std::fs::create_dir_all(&reader.dir).unwrap();
        let target = root.path().join("owned-target");
        fsx::write_atomic(&target, b"inert target", 0o600).unwrap();
        std::os::unix::fs::symlink(&target, reader.lock_file()).unwrap();
        assert!(StateLock::observe(&reader).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"inert target");
        std::fs::remove_file(reader.lock_file()).unwrap();
        std::fs::create_dir(reader.lock_file()).unwrap();
        assert!(StateLock::observe(&reader).is_err());
    }

    #[test]
    fn pin_admission_follows_verified_rotation_and_compiled_replacement() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.vault");
        let admin = Paths::for_home(&root.path().join("admin"));
        create(&admin, source.to_str().expect("utf-8"), None, Some("admin"))
            .expect("create scratch vault");
        let original = pinned(&admin).signer.expect("original pin");
        rotate(&admin);
        let successor = pinned(&admin).signer.expect("successor pin");
        rotate(&admin);
        let config = pinned(&admin);
        let current = config.signer.expect("current pin");
        let unrelated = envelope::signer_public(&[239; 32]).expect("unrelated public key");
        for pin in [original, successor, current] {
            assert!(pin_admitted(&config, Some(pin)));
        }
        assert!(!pin_admitted(&config, Some(unrelated)));

        // The original seed now gives way to the compiled successor, whose
        // legitimate later rotation is still the configured authority.
        let mut fork = config.clone();
        fork.signer = Some(unrelated);
        fork.followed = vec![original];
        let admitted = retiring(vec![original], successor, || {
            [
                pin_admitted(&config, Some(original)),
                pin_admitted(&config, Some(successor)),
                pin_admitted(&config, Some(current)),
                pin_admitted(&config, Some(unrelated)),
                pin_admitted(&fork, None),
                pin_admitted(&fork, Some(unrelated)),
            ]
        });
        assert_eq!(admitted, [true, true, true, false, false, false]);

        let mut unsigned = config;
        unsigned.signer = None;
        unsigned.followed.clear();
        assert!(pin_admitted(&unsigned, None));
        assert!(!pin_admitted(&unsigned, Some(current)));
        unsigned.source = Source::Url("https://vault.example/v1/team".into());
        assert!(!pin_admitted(&unsigned, None));
    }

    #[test]
    fn materialization_readiness_rejects_intact_files_under_retired_authority() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.vault");
        let admin = Paths::for_home(&root.path().join("admin"));
        create(&admin, source.to_str().expect("utf-8"), None, Some("admin"))
            .expect("create scratch vault");
        let opened = admin_open(&admin).expect("open scratch vault");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-token")
            .expect("add test credential");
        admin_publish(&admin, &opened, &doc).expect("publish test credential");
        sync(&admin, None).expect("materialize test credential");
        assert!(materialization_ready(&admin).expect("ready before retirement"));
        let old = pinned(&admin).signer.expect("held pin");
        let current = envelope::signer_public(&[238; 32]).expect("replacement public key");
        let state_before = std::fs::read(admin.state_file()).expect("state before check");
        let source_before = std::fs::read(admin.source_file()).expect("source before check");
        let ready = retiring(vec![old], current, || materialization_ready(&admin));
        assert!(!ready.expect("retired authority refused"));
        assert_eq!(std::fs::read(admin.state_file()).unwrap(), state_before);
        assert_eq!(std::fs::read(admin.source_file()).unwrap(), source_before);
        assert!(materialization_ready(&admin).expect("test authority restored"));
    }

    #[test]
    fn plain_source_changes_never_admit_the_previous_sources_intact_files() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let first = root.path().join("first.secrets");
        let second = root.path().join("second.secrets");
        fsx::write_atomic(&first, b"token=first-token\n", 0o600).expect("first vault");
        fsx::write_atomic(&second, b"token=second-token\n", 0o600).expect("second vault");
        let reader = Paths::for_home(&root.path().join("reader"));
        join_pinned(&reader, first.to_str().unwrap(), "reader");
        assert!(materialization_ready(&reader).unwrap());
        let original = pinned(&reader);
        let mut changed = original.clone();
        changed.source = Source::Path(second.clone());
        changed.name = "second".into();

        // Old clients could publish another source while leaving an intact old
        // manifest. Neither a mismatched nor an absent binding certifies it.
        save_config(&reader, &changed).unwrap();
        assert!(!materialization_ready(&reader).unwrap());
        let mut legacy = load_state(&reader).unwrap();
        legacy.source_identity = None;
        legacy.failed = false;
        save_state(&reader, &legacy).unwrap();
        assert!(!materialization_ready(&reader).unwrap());
        sync(&reader, None).expect("migrate the old record");
        assert!(materialization_ready(&reader).unwrap());
        let token = materialize::target_path(&reader.home, "token").unwrap();
        assert_eq!(std::fs::read(&token).unwrap(), b"second-token");

        // Stop at the same preparation/config boundary used by plain join,
        // then make its second fetch fail. The old files are still intact.
        let mut state = load_state(&reader).unwrap();
        state.seq = 0;
        state.digest = None;
        state.membership = Stored::Plain;
        prepare_materialization(&reader, &mut state).unwrap();
        assert!(!materialization_ready(&reader).unwrap());
        save_config(&reader, &original).unwrap();
        std::fs::remove_file(&first).expect("remove only scratch vault");
        assert!(sync(&reader, None).is_err());
        assert!(!materialization_ready(&reader).unwrap());
        assert_eq!(std::fs::read(&token).unwrap(), b"second-token");
        fsx::write_atomic(&first, b"token=first-token\n", 0o600).expect("restore scratch vault");
        sync(&reader, None).expect("finish the interrupted join");
        assert!(materialization_ready(&reader).unwrap());
        assert_eq!(std::fs::read(token).unwrap(), b"first-token");
    }

    #[test]
    fn signed_source_binding_and_applied_digest_are_required_until_sync_repairs_them() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.vault");
        let admin = Paths::for_home(&root.path().join("admin"));
        create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"intact-token")
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
        sync(&admin, None).unwrap();
        assert!(materialization_ready(&admin).unwrap());
        for missing_digest in [false, true] {
            let mut legacy = load_state(&admin).unwrap();
            if missing_digest {
                legacy.digest = None;
            } else {
                legacy.source_identity = None;
            }
            legacy.failed = false;
            save_state(&admin, &legacy).unwrap();
            assert!(!materialization_ready(&admin).unwrap());
            assert!(matches!(
                sync(&admin, None).unwrap(),
                SyncOutcome::Applied { .. }
            ));
            assert!(materialization_ready(&admin).unwrap());
        }

        let old_config = pinned(&admin);
        let old_state = load_state(&admin).unwrap();
        rotate(&admin);
        let new_config = pinned(&admin);
        assert_ne!(old_config.signer, new_config.signer);
        assert!(
            materialization_ready(&admin).unwrap(),
            "one sync binds the new signer"
        );
        save_state(&admin, &old_state).unwrap();
        assert!(
            !materialization_ready(&admin).unwrap(),
            "old signer binding cannot admit new config"
        );

        for replaced in [false, true] {
            save_config(&admin, &old_config).unwrap();
            let mut interrupted = old_state.clone();
            if replaced {
                interrupted.seq = 0;
                interrupted.digest = None;
            }
            prepare_materialization(&admin, &mut interrupted).unwrap();
            save_config(&admin, &new_config).unwrap();
            assert!(!materialization_ready(&admin).unwrap());
            sync(&admin, None).expect("resume after rotated or replaced config publication");
            assert!(materialization_ready(&admin).unwrap());
            assert_eq!(
                load_state(&admin).unwrap().source_identity,
                Some(source_identity(&new_config))
            );
        }
    }

    #[test]
    fn the_subscriber_repairs_a_changed_file_once_per_version_so_readiness_returns() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.vault");
        let admin = Paths::for_home(&root.path().join("admin"));
        create(&admin, source.to_str().unwrap(), None, Some("admin")).unwrap();
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-token")
            .unwrap();
        doc.set_entry_with(
            &Audience::Everyone,
            "~/.config/tool/config",
            b"[tool]\n",
            true,
            None,
        )
        .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
        sync(&admin, None).unwrap();
        assert!(materialization_ready(&admin).unwrap());
        let mut repaired = None;
        assert!(!repair_due(&admin, &mut repaired), "nothing to repair");

        // A program rewrites the if-absent file the vault filled, and a person deletes the
        // token: not ready, and the next pass applies the same version again, once.
        let config = materialize::target_path(&admin.home, "~/.config/tool/config").unwrap();
        fsx::write_atomic(&config, b"[tool]\nhelper = local\n", 0o600).unwrap();
        let token = materialize::target_path(&admin.home, "token").unwrap();
        std::fs::remove_file(&token).unwrap();
        assert!(!materialization_ready(&admin).unwrap());
        assert!(repair_due(&admin, &mut repaired));
        assert!(
            !repair_due(&admin, &mut repaired),
            "once per version, whatever the files hold"
        );
        sync(&admin, None).unwrap();
        assert!(materialization_ready(&admin).unwrap());
        assert_eq!(std::fs::read(&token).unwrap(), b"inert-token");
        assert_eq!(
            std::fs::read(&config).unwrap(),
            b"[tool]\nhelper = local\n",
            "the rewritten if-absent file is the machine's own"
        );

        // This same subscriber moves to another signed source at the same sequence. Its
        // first repair must not inherit the first vault's once-per-version suppression.
        let first = load_state(&admin).unwrap();
        let other = root.path().join("other.vault");
        create(&admin, other.to_str().unwrap(), None, Some("admin")).unwrap();
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-other")
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
        let second = load_state(&admin).unwrap();
        assert_eq!(first.seq, 2);
        assert_eq!(second.seq, first.seq);
        assert_ne!(second.source_identity, first.source_identity);
        assert!(materialization_ready(&admin).unwrap());
        assert!(!repair_due(&admin, &mut repaired));
        std::fs::remove_file(&token).unwrap();
        assert!(repair_due(&admin, &mut repaired));
        assert!(
            !repair_due(&admin, &mut repaired),
            "once for this source too"
        );
        sync(&admin, None).unwrap();
        assert!(materialization_ready(&admin).unwrap());
        assert_eq!(std::fs::read(&token).unwrap(), b"inert-other");

        // A newer version may be repaired again.
        std::fs::remove_file(&token).unwrap();
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "note", b"inert-note")
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
        std::fs::remove_file(&token).unwrap();
        assert!(repair_due(&admin, &mut repaired));
    }

    #[test]
    fn materialization_readiness_tracks_failed_resumed_and_missing_files() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.secrets");
        fsx::write_atomic(
            &source,
            b"token=inert-token\n~/.tool/token=inert-value\n",
            0o600,
        )
        .expect("plain vault");
        let reader = Paths::for_home(&root.path().join("reader"));
        assert!(!materialization_ready(&reader).expect("unconfigured"));
        assert!(!reader.dir.exists(), "a readiness check creates no state");
        let blocked = materialize::target_path(&reader.home, "~/.tool/token").expect("target");
        std::fs::create_dir_all(&blocked).expect("directory blocks the file");
        assert!(matches!(
            join_pinned(&reader, source.to_str().expect("utf-8"), "reader"),
            JoinOutcome::Plain { .. }
        ));
        assert_eq!(
            status(&reader).expect("status").membership,
            Membership::Plain
        );
        assert!(load_state(&reader).expect("state").failed);
        assert!(!materialization_ready(&reader).expect("partial sync"));

        std::fs::remove_dir(&blocked).expect("remove only the test's empty directory");
        sync(&reader, None).expect("retry same source");
        assert!(materialization_ready(&reader).expect("complete sync"));

        let token = materialize::target_path(&reader.home, "token").expect("token path");
        std::fs::remove_file(&token).expect("remove test token");
        let state_before = std::fs::read(reader.state_file()).expect("state before check");
        assert!(!materialization_ready(&reader).expect("missing file"));
        assert!(!token.exists(), "readiness must not repair the file");
        assert_eq!(std::fs::read(reader.state_file()).unwrap(), state_before);
        sync(&reader, None).expect("repair missing file");
        assert!(materialization_ready(&reader).expect("repaired"));

        fsx::write_atomic(&token, b"inert-local-edit", 0o600).expect("edit test token");
        assert!(!materialization_ready(&reader).expect("changed file"));
    }

    /// Admit every machine that asked to join the vault `admin` opens.
    fn approve(admin: &Paths) {
        let opened = admin_open(admin).expect("admin opens");
        let mut doc = opened.doc.clone();
        for request in requests(admin, Some(&doc)).expect("requests") {
            let host = free_name(&doc, &request.host);
            doc.add_machine(&host, &request.recipient).expect("add");
        }
        admin_publish(admin, &opened, &doc).expect("publish");
    }

    fn rotate(admin: &Paths) -> Published {
        let opened = admin_open(admin).expect("admin opens");
        admin_rotate(admin, &opened, &opened.doc.clone()).expect("rotate")
    }

    fn pinned(paths: &Paths) -> Config {
        config(paths).expect("config").expect("configured")
    }

    #[test]
    fn a_concurrent_machine_key_wins_without_replacing_any_existing_bytes() {
        let home = aterm_tempfile::tempdir().expect("owned home");
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).expect("state directory");
        // Both callers already observed no key; their publication steps are ordered here.
        let first = Identity::from_secret_bytes([7; 32]);
        let expected = first.recipient();
        let first = install_identity(&paths, first).expect("first key wins");
        assert_eq!(first.recipient(), expected);
        let held = fsx::read_private(&paths.identity_file(), 16 << 10)
            .expect("read")
            .expect("key");
        let later = install_identity(&paths, Identity::from_secret_bytes([9; 32]))
            .expect("return the winning key");
        assert_eq!(later.recipient(), expected);
        assert_eq!(
            fsx::read_private(&paths.identity_file(), 16 << 10)
                .expect("read")
                .expect("key"),
            held,
            "a creator never replaces the first private key"
        );
        assert_eq!(
            machine_identity(&paths).expect("existing key").recipient(),
            expected
        );

        // A taken name is not permission to replace bytes that are not a valid key.
        fsx::write_atomic(&paths.identity_file(), b"unrecognized existing work", 0o600)
            .expect("owned sentinel");
        assert!(install_identity(&paths, Identity::from_secret_bytes([11; 32])).is_err());
        assert_eq!(
            fsx::read_private(&paths.identity_file(), 16 << 10)
                .expect("read")
                .expect("sentinel")
                .as_slice(),
            b"unrecognized existing work"
        );
    }

    #[test]
    fn the_alab_vault_is_known_by_its_keys_whatever_url_reaches_it() {
        let key = |n: u8| envelope::signer_public(&[n; 32]).expect("public");
        let b64 = |key: &[u8; 32]| crate::b64::encode_padded(key);
        // Endorsements are not this function's to judge.
        let chain = vec![(key(1), [0u8; 64]), (key(2), [0u8; 64])];
        let knows = |keys: Vec<[u8; 32]>, current: [u8; 32]| Retired {
            keys,
            current: Some(current),
        };
        // A chain through the key a build names or one it retired: what it retired, then the
        // chain's other keys.
        for retired in [
            knows(Vec::new(), key(1)),
            knows(vec![key(1)], key(2)),
            knows(vec![key(0)], key(2)),
        ] {
            let release = alab_release(&retired, &chain, &key(3)).expect("the ALab vault");
            assert_eq!(release.signer, b64(&key(3)));
            let mut expected: Vec<String> = retired.keys.iter().map(b64).collect();
            for (link, _) in &chain {
                if !expected.contains(&b64(link)) {
                    expected.push(b64(link));
                }
            }
            assert_eq!(release.retired, expected);
        }
        // Another vault's keys, or a build that names none.
        assert_eq!(
            alab_release(&knows(vec![key(5)], key(6)), &chain, &key(3)),
            None
        );
        assert_eq!(alab_release(&Retired::default(), &chain, &key(3)), None);

        // An admin that reaches the vault by loopback, as the server's own Mac does.
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = serve(&root.path().join("store"), "127.0.0.1:0");
        let admin = Paths::for_home(&root.path().join("ma"));
        let url = format!("http://{}/v1/alab", running.addr);
        create(&admin, &url, None, Some("ma")).expect("create");
        let first = pinned(&admin).signer.expect("pinned");
        // The control: to a build whose keys are not this vault's, it is any other vault.
        let done = rotate(&admin);
        assert!(done.rotated);
        assert_eq!(done.alab_release, None);
        let second = pinned(&admin).signer.expect("pinned");
        // To one that names its first key, it is the ALab vault.
        let done = retiring(Vec::new(), first, || rotate(&admin));
        let third = pinned(&admin).signer.expect("pinned");
        let release = done.alab_release.expect("the ALab vault, by loopback");
        assert_eq!(release.signer, b64(&third));
        assert_eq!(release.retired, vec![b64(&first), b64(&second)]);
        running.stop();
    }

    #[test]
    fn a_reader_the_removed_admin_led_off_follows_the_vault_once_its_build_retires_the_key() {
        // `bare`: the reader followed the fork under an aterm before `followed`, whose
        // source.toml names the pin alone.
        for (fork, rejoin, bare) in [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let root = aterm_tempfile::tempdir().expect("tempdir");
            let store = root.path().join("store");
            let running = serve(&store, "127.0.0.1:0");
            let addr = running.addr.to_string();
            let url = format!("http://{addr}/v1/alab");
            let admin = Paths::for_home(&root.path().join("ma"));
            let created = create(&admin, &url, None, Some("ma")).expect("create");
            let reader = Paths::for_home(&root.path().join("mb"));
            join_pinned(&reader, &format!("{url}#{}", created.signer), "mb");
            approve(&admin);
            assert!(matches!(
                sync(&reader, None).expect("sync"),
                SyncOutcome::Applied { seq: 2, .. }
            ));

            // The vault's key rotates, and an admin the rotation removed kept the old one.
            let old = admin_open(&admin).expect("open").seed;
            let old_key = envelope::signer_public(&old).expect("public");
            rotate(&admin);
            let new_key = pinned(&admin).signer.expect("pinned");

            // Before a build retires the old key, that admin answers at the vault's address,
            // and the reader, still pinned to the old key, takes what it signs: (a) a fork, a
            // key of its own the old one endorsed, or (b) a version the old key signed, each
            // with a seq no real version reaches.
            running.stop();
            let spoof = serve(&root.path().join("spoof"), &addr);
            let mut signer = old;
            let mut fed = Unsigned {
                name: "alab".to_string(),
                seq: 1 << 40,
                time: 0,
                handoff: Vec::new(),
                lockers: Vec::new(),
            };
            if fork {
                aterm_uds::rand::fill(&mut signer).expect("random");
                let key = envelope::signer_public(&signer).expect("public");
                let endorsement = envelope::endorse(&old, "alab", &key).expect("endorsed");
                fed.handoff = vec![(old_key, endorsement)];
            }
            let fed = envelope::sign(&fed, &signer).expect("signed");
            transport::publish(&Source::Url(url.clone()), &fed).expect("the spoof takes it");
            assert_eq!(sync(&reader, None).expect("taken"), SyncOutcome::NotIn);
            assert_eq!(
                pinned(&reader).signer,
                Some(envelope::signer_public(&signer).expect("public"))
            );
            if bare {
                let text = std::fs::read_to_string(reader.source_file()).expect("source.toml");
                let kept: String = text
                    .lines()
                    .filter(|line| !line.contains("followed"))
                    .map(|line| format!("{line}\n"))
                    .collect();
                assert_ne!(kept, text, "the fork's lineage was written");
                std::fs::write(reader.source_file(), kept).expect("written");
            }
            spoof.stop();

            // The vault's own server again. A build that does not know the old key is retired
            // keeps the reader off it (the control): (a) pinned to the fork, or (b) under a
            // floor no version passes, which a long-poll waits above for ever.
            let running = serve(&store, &addr);
            let error = sync(&reader, None).expect_err("kept off");
            let why = if fork {
                "endorsed no handoff"
            } else {
                "rollback"
            };
            assert!(error.0.contains(why), "{error}");
            let poll = Some(Duration::from_millis(100));
            assert_eq!(
                sync(&reader, poll).expect("polled"),
                SyncOutcome::Unchanged { seq: 1 << 40 }
            );
            // The build that retires it: the pin gives way, what it held floors nothing, and
            // the reader is in the vault again, pinned to the key that signs it, whether its
            // subscriber reads the vault or a person joins it again at the same address.
            retiring(vec![old_key], new_key, || {
                if rejoin {
                    assert!(matches!(
                        join_pinned(&reader, &url, "mb"),
                        JoinOutcome::Joined { seq: 3, .. }
                    ));
                } else {
                    assert!(matches!(
                        sync(&reader, poll).expect("sync"),
                        SyncOutcome::Applied { seq: 3, .. }
                    ));
                }
            });
            let config = pinned(&reader);
            assert_eq!(config.signer, Some(new_key));
            assert_eq!(config.followed, vec![old_key]);
            running.stop();
        }
    }

    #[test]
    fn an_admin_gives_the_vault_back_to_a_store_the_removed_admin_fed() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let running = serve(&store, "127.0.0.1:0");
        let addr = running.addr.to_string();
        let url = format!("http://{addr}/v1/alab");
        let admin = Paths::for_home(&root.path().join("ma"));
        create(&admin, &url, None, Some("ma")).expect("create");
        // Another Mac's store, which still holds the vault's first version.
        let stale = root.path().join("stale");
        std::fs::create_dir_all(&stale).expect("dir");
        std::fs::copy(store.join("alab.vault"), stale.join("alab.vault")).expect("copied");
        let old = admin_open(&admin).expect("open").seed;
        let old_key = envelope::signer_public(&old).expect("public");
        rotate(&admin);
        let new_key = pinned(&admin).signer.expect("pinned");
        running.stop();

        // The vault's server moves to that Mac, whose store the admin the rotation removed fed
        // first: a version of the old key's with a seq no real one reaches.
        let fed = Unsigned {
            name: "alab".to_string(),
            seq: 1 << 40,
            time: 0,
            handoff: Vec::new(),
            lockers: Vec::new(),
        };
        let fed = envelope::sign(&fed, &old).expect("signed");
        let moved = serve(&stale, &addr);
        transport::publish(&Source::Url(url.clone()), &fed).expect("taken");
        moved.stop();
        // Once the admin's build and the store's retire the old key, the admin's next command
        // puts its copy back (the store's held key gives way), and goes on from it.
        let opened = retiring(vec![old_key], new_key, || {
            let moved = serve(&stale, &addr);
            let opened = admin_open(&admin);
            moved.stop();
            opened
        });
        assert_eq!(opened.expect("opened, its copy put back").seq(), 2);
        let cached = std::fs::read(admin.cache_file()).expect("cache");
        assert_eq!(std::fs::read(stale.join("alab.vault")).ok(), Some(cached));
    }

    /// A pass is cut short after it wrote the release identity's next value and before it
    /// saved what it wrote; the version after that still finds the set the vault's, and
    /// updates it, rather than leaving it the machine's own for good.
    #[test]
    fn a_pass_cut_short_after_its_writes_leaves_its_files_the_vaults() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(root.path());
        let (key, toml, profile) = (
            "~/.aterm/machine.key",
            "~/.aterm/machine.toml",
            "~/.aterm/release-credentials.toml",
        );
        let ring = |value: &[u8]| {
            [(key, toml), (toml, profile), (profile, key)].map(|(name, anchor)| Entry {
                name: name.to_string(),
                value: SecretBytes::from(value),
                audience: Audience::Everyone,
                if_absent: true,
                anchor: Some(anchor.to_string()),
            })
        };
        let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
        // A first pass fills the gaps and is cut short too: what it filled is the vault's.
        let mut first = State {
            seq: 1,
            ..State::default()
        };
        let report = apply_entries(&paths, &mut first, &ring(b"v1")).expect("v1");
        assert_eq!(report.written.len(), 3);
        drop(first);
        let mut state = load_state(&paths).expect("state");
        assert_eq!(
            state.seq, 0,
            "the version moves on only with the pass's own save"
        );
        let report = apply_entries(&paths, &mut state, &ring(b"v1")).expect("v1 again");
        assert_eq!(report.unchanged.len(), 3, "{report:?}");
        state.seq = 1;
        save_state(&paths, &state).expect("saved");
        // v2's pass (sync has moved the version it holds in memory) writes and ends before its
        // own save: state.toml still holds v1, so the pass is made again.
        let mut cut = load_state(&paths).expect("state");
        cut.seq = 2;
        apply_entries(&paths, &mut cut, &ring(b"v2")).expect("v2");
        drop(cut);
        assert_eq!(load_state(&paths).expect("state").seq, 1);
        let mut next = load_state(&paths).expect("state");
        let report = apply_entries(&paths, &mut next, &ring(b"v3")).expect("v3");
        assert_eq!(report.written.len(), 3, "{report:?}");
        assert!(
            report.left.is_empty() && report.removed.is_empty(),
            "{report:?}"
        );
        for name in [key, toml, profile] {
            assert_eq!(std::fs::read(at(name)).expect("read"), b"v3", "{name}");
            assert_eq!(
                next.manifest.files[name],
                materialize::digest_hex(b"v3"),
                "{name}"
            );
        }
        assert!(next.pending.is_empty());
    }

    /// A pass cut short after its early save, whose anchored entries stay away from a machine
    /// with its anchor of its own: neither the gap it left nor a file the vault once wrote and
    /// the person changed since is pending, so what a person later puts there, even the
    /// vault's very value, stays theirs and is never taken back.
    #[test]
    fn a_pass_cut_short_leaves_no_entry_it_keeps_away_pending() {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(root.path());
        let (anchor, gap, changed) = ("~/.config/gh/hosts.yml", "~/.gitconfig", "~/.gitnotes");
        let entry = |name: &str, anchor: Option<&str>| Entry {
            name: name.to_string(),
            value: SecretBytes::from(&b"vault-value"[..]),
            audience: Audience::Everyone,
            if_absent: true,
            anchor: anchor.map(str::to_string),
        };
        let entries = [
            entry(anchor, None),
            entry(gap, Some(anchor)),
            entry(changed, Some(anchor)),
        ];
        let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
        for (name, value) in [
            (anchor, &b"the machine's own"[..]),
            (changed, b"vault-value"),
        ] {
            std::fs::create_dir_all(at(name).parent().expect("parent")).expect("dir");
            std::fs::write(at(name), value).expect("written");
        }
        // The vault wrote `changed` once, before the person made it hold the new value.
        let mut state = State {
            seq: 1,
            ..State::default()
        };
        state.manifest.files.insert(
            changed.to_string(),
            materialize::digest_hex(b"vault-before"),
        );
        save_state(&paths, &state).expect("saved");
        let report = apply_entries(&paths, &mut state, &entries).expect("applied");
        assert!(report.written.is_empty(), "{report:?}");
        drop(state);
        let pending = load_state(&paths).expect("state").pending;
        assert!(pending.is_empty(), "{pending:?}");
        // Later the person puts the vault's value at the gap themselves, private as a copy
        // from a Mac the vault wrote it on is.
        std::fs::write(at(gap), b"vault-value").expect("written");
        for name in [gap, changed] {
            std::fs::set_permissions(at(name), std::fs::Permissions::from_mode(0o600))
                .expect("private");
        }
        let mut next = load_state(&paths).expect("state");
        let report = apply_entries(&paths, &mut next, &entries).expect("applied again");
        assert!(report.removed.is_empty(), "{report:?}");
        for name in [anchor, gap, changed] {
            assert!(at(name).exists(), "{name} is kept");
        }
    }

    /// What a pass cut short was writing, found in place, is the vault's: taken back with its
    /// anchor, and removed when the vault drops it, as if the pass had saved it. Nothing
    /// behind a symlinked directory is pended or taken for what the pass wrote, and a pass
    /// writes nothing it did not pend first.
    #[test]
    fn a_cut_pass_resumes_as_the_vaults_and_writes_only_what_it_pended() {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(root.path());
        let at = |name: &str| materialize::target_path(root.path(), name).expect("valid");
        let private = |name: &str, value: &[u8]| {
            std::fs::create_dir_all(at(name).parent().expect("parent")).expect("dir");
            std::fs::write(at(name), value).expect("written");
            std::fs::set_permissions(at(name), std::fs::Permissions::from_mode(0o600))
                .expect("private");
        };
        let entry = |name: &str, value: &[u8], if_absent: bool, anchor: Option<&str>| Entry {
            name: name.to_string(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent,
            anchor: anchor.map(str::to_string),
        };
        let resume = |pending: &[(&str, &[u8])], manifest: &[(&str, &[u8])]| {
            let mut state = State {
                seq: 1,
                ..State::default()
            };
            for (name, value) in pending {
                state
                    .pending
                    .insert(name.to_string(), materialize::digest_hex(value));
            }
            for (name, value) in manifest {
                state
                    .manifest
                    .files
                    .insert(name.to_string(), materialize::digest_hex(value));
            }
            save_state(&paths, &state).expect("saved");
            load_state(&paths).expect("state")
        };
        // A pass filled a gap its anchor (a gap too) let it fill, and was cut before its save;
        // since, the anchor became the machine's own: the gap's file is taken back.
        let (anchor, gap) = ("~/.config/gh/hosts.yml", "~/.gitconfig");
        private(gap, b"vault-value");
        private(anchor, b"the machine's own");
        let mut state = resume(&[(gap, b"vault-value")], &[]);
        let entries = [
            entry(anchor, b"vault-value", true, None),
            entry(gap, b"vault-value", true, Some(anchor)),
        ];
        let report = apply_entries(&paths, &mut state, &entries).expect("applied");
        assert_eq!(report.removed, vec![gap.to_string()], "{report:?}");
        assert!(!at(gap).exists());
        assert!(at(anchor).exists(), "the machine's own anchor");
        // The ring the vault wrote at v1 was rewritten at v2 by a pass cut short; the version
        // after drops it: all three files are removed, as v2's save would have let them be.
        let ring = [
            "~/.aterm/machine.key",
            "~/.aterm/machine.toml",
            "~/.aterm/release-credentials.toml",
        ];
        for name in ring {
            private(name, b"v2");
        }
        let v1: Vec<(&str, &[u8])> = ring.iter().map(|name| (*name, &b"v1"[..])).collect();
        let v2: Vec<(&str, &[u8])> = ring.iter().map(|name| (*name, &b"v2"[..])).collect();
        let mut state = resume(&v2, &v1);
        let report = apply_entries(&paths, &mut state, &[]).expect("applied");
        assert_eq!(report.removed.len(), 3, "{report:?}");
        for name in ring {
            assert!(!at(name).exists(), "{name}");
        }
        assert!(state.manifest.files.is_empty(), "{:?}", state.manifest);
        // A plain entry behind a symlinked directory is never written, so never pended, and a
        // file there holding a pending value is not taken for the pass's.
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("dir");
        std::os::unix::fs::symlink(&elsewhere, root.path().join(".tool")).expect("linked");
        let linked = "~/.tool/token";
        private(linked, b"vault-value");
        let resumed: materialize::Pending =
            [(linked.to_string(), materialize::digest_hex(b"vault-value"))].into();
        let mut ahead = None;
        let (manifest, _) = materialize::apply_resuming(
            root.path(),
            &[entry(linked, b"vault-value", false, None)],
            &materialize::Manifest::default(),
            &resumed,
            now(),
            &mut |held, pending| {
                ahead = Some((held.clone(), pending.clone()));
                Ok(())
            },
        )
        .expect("applied");
        let (resumed_held, pending) = ahead.expect("told ahead");
        assert!(pending.is_empty(), "{pending:?}");
        assert!(resumed_held.files.is_empty(), "{resumed_held:?}");
        assert!(manifest.files.is_empty(), "{manifest:?}");
        // Whatever a pass writes, it pended first: a plain gap, a plain entry over a file of
        // the machine's own (moved aside), a filled if-absent gap, one its own anchor keeps
        // away, and the entry behind the link, which fails.
        private("~/.netrc", b"the machine's own");
        let own_anchor = "~/.config/tool/own";
        private(own_anchor, b"the machine's own");
        let entries = [
            entry("plain_gap", b"vault-value", false, None),
            entry("~/.netrc", b"vault-value", false, None),
            entry("~/.config/tool/filled", b"vault-value", true, None),
            entry(own_anchor, b"vault-value", true, None),
            entry(
                "~/.config/tool/kept",
                b"vault-value",
                true,
                Some(own_anchor),
            ),
            entry(linked, b"vault-value", false, None),
        ];
        let mut told = materialize::Pending::new();
        let (_, report) = materialize::apply_resuming(
            root.path(),
            &entries,
            &materialize::Manifest::default(),
            &materialize::Pending::new(),
            now(),
            &mut |_, pending| {
                told = pending.clone();
                Ok(())
            },
        )
        .expect("applied");
        assert_eq!(report.written.len(), 3, "{report:?}");
        for name in &report.written {
            assert!(told.contains_key(name), "{name} written unpended: {told:?}");
        }
        assert!(!told.contains_key("~/.config/tool/kept"), "{told:?}");
        assert!(!told.contains_key(linked), "{told:?}");
    }

    /// A pass cut short with writes pending is not done though the version is the same and
    /// every file the manifest names is intact: the next fetch of that version applies again.
    #[test]
    fn a_pass_cut_short_is_applied_again_at_the_same_version() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("shared").join("team.vault");
        std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
        let path = shared.to_str().expect("utf-8");
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, path, None, Some("ma")).expect("create");
        let reader = Paths::for_home(&root.path().join("mb"));
        join_pinned(&reader, &format!("{path}#{}", created.signer), "mb");
        let recipient = status(&reader)
            .expect("status")
            .machine
            .expect("a key")
            .0
            .to_bech32();
        let (token, gap) = ("token", "~/.gitconfig");
        {
            let opened = admin_open(&admin).expect("open");
            let mut doc = opened.doc.clone();
            doc.add_machine("mb", &recipient).expect("add");
            doc.set_entry(&Audience::Everyone, token, b"vault-token")
                .expect("set");
            doc.set_entry_with(&Audience::Everyone, gap, b"vault-config\n", true, None)
                .expect("set");
            admin_publish(&admin, &opened, &doc).expect("publish");
        }
        // The reader has a file of its own at the gap, which the vault leaves.
        let at = materialize::target_path(&reader.home, gap).expect("valid");
        std::fs::write(&at, b"the machine's own\n").expect("written");
        let first = sync(&reader, None).expect("sync");
        assert!(matches!(first, SyncOutcome::Applied { .. }), "{first:?}");
        assert_eq!(std::fs::read(&at).expect("kept"), b"the machine's own\n");
        // Its own file goes, and a pass that pended the gap is cut before it fills it.
        std::fs::remove_file(&at).expect("removed");
        let mut state = load_state(&reader).expect("state");
        state
            .pending
            .insert(gap.to_string(), materialize::digest_hex(b"vault-config\n"));
        save_state(&reader, &state).expect("saved");
        assert!(!materialization_ready(&reader).expect("interrupted write"));
        assert!(matches!(
            sync(&reader, None).expect("sync again"),
            SyncOutcome::Applied { .. }
        ));
        assert_eq!(std::fs::read(&at).expect("filled"), b"vault-config\n");
        assert!(load_state(&reader).expect("state").pending.is_empty());
        assert!(materialization_ready(&reader).expect("resumed write completed"));
    }

    /// A plain entry whose target was a directory when the pass looked, and is a gap by the
    /// time it would write: written, it would be nothing `ahead` was told of, so it fails and
    /// the next pass places it.
    #[test]
    fn a_target_that_changed_since_the_look_is_not_written_unannounced() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let name = "~/.tool/token";
        let at = materialize::target_path(root.path(), name).expect("valid");
        std::fs::create_dir_all(&at).expect("a directory in the way");
        let entries = [Entry {
            name: name.to_string(),
            value: SecretBytes::from(&b"vault-value"[..]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }];
        let (manifest, report) = materialize::apply_resuming(
            root.path(),
            &entries,
            &materialize::Manifest::default(),
            &materialize::Pending::new(),
            now(),
            &mut |_, pending| {
                assert!(pending.is_empty(), "{pending:?}");
                std::fs::remove_dir(&at).expect("the directory goes");
                Ok(())
            },
        )
        .expect("applied");
        assert!(report.written.is_empty(), "{report:?}");
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(!at.exists());
        assert!(manifest.files.is_empty(), "{manifest:?}");
    }

    /// A pass whose pin gave way forgot what it held, cached the version that replaces it,
    /// and was cut short: the floor is that version unapplied, so the next pass applies it,
    /// and refuses every version older than it, another at its seq, and a next one that
    /// drops its handoff chain.
    #[test]
    fn a_membership_with_no_digest_floors_on_its_cache_unapplied() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("shared").join("team.vault");
        std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
        let admin = Paths::for_home(&root.path().join("ma"));
        create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
        let first = std::fs::read(admin.cache_file()).expect("cache");
        approve(&admin);
        let second = std::fs::read(admin.cache_file()).expect("cache");
        rotate(&admin);
        let config = pinned(&admin);
        let mut state = load_state(&admin).expect("state");
        assert!(floor(&admin, &state, &config.name, &config).is_some());
        state.seq = 0;
        state.digest = None;
        let below = floor(&admin, &state, &config.name, &config).expect("a floor below the cache");
        let cached = std::fs::read(admin.cache_file()).expect("cache");
        let judged = |bytes: &[u8]| {
            envelope::verify_for_reader(
                bytes,
                &config.name,
                &config.signer.expect("pin"),
                &config.followed,
                Some(&below),
            )
            .map(|(_, _, verdict)| verdict)
        };
        assert_eq!(judged(&cached).expect("the cache"), Verdict::New);
        for older in [&first, &second] {
            assert!(judged(older).is_err(), "an older version is refused");
        }
        let held = envelope::parse(&cached).expect("parsed");
        assert!(!held.handoff.is_empty(), "the rotation's link");
        let seed = admin_open(&admin).expect("open").seed;
        let signed = |seq: u64, handoff: Vec<envelope::Link>| {
            let unsigned = Unsigned {
                name: "team".to_string(),
                seq,
                time: 0,
                handoff,
                lockers: Vec::new(),
            };
            envelope::sign(&unsigned, &seed).expect("signed")
        };
        assert!(
            judged(&signed(held.seq, held.handoff.clone())).is_err(),
            "another at its seq"
        );
        assert_eq!(
            judged(&signed(held.seq + 1, held.handoff.clone())).expect("next"),
            Verdict::New
        );
        assert!(
            judged(&signed(held.seq + 1, Vec::new())).is_err(),
            "a next one dropping the chain"
        );
        // No cache that verifies: nothing to floor on, as before any version was held.
        std::fs::write(admin.cache_file(), b"alab-vault/1\ngarbage\n").expect("written");
        assert!(floor(&admin, &state, &config.name, &config).is_none());
        std::fs::write(admin.cache_file(), &cached).expect("written");
        // A reset (another vault joined, a corrupt state moved aside) still floors on the
        // cache, when it verifies under the pin.
        state.membership = Stored::None;
        assert!(floor(&admin, &state, &config.name, &config).is_some());
    }

    /// The subscriber asks again for the vault it read, while a person's join moves the
    /// machine to another: the request for the vault it left changes nothing of the state the
    /// join made for the one it went to.
    #[test]
    fn a_request_for_a_vault_this_machine_left_leaves_its_state_alone() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("shared");
        std::fs::create_dir_all(&shared).expect("dir");
        let vault = |admin: &str, name: &str| {
            let path = shared.join(format!("{name}.vault"));
            let paths = Paths::for_home(&root.path().join(admin));
            let created =
                create(&paths, path.to_str().expect("utf-8"), None, Some(admin)).expect("create");
            format!("{}#{}", path.display(), created.signer)
        };
        let (first, second) = (vault("ma", "team"), vault("mz", "other"));
        let reader = Paths::for_home(&root.path().join("mb"));
        join_pinned(&reader, &first, "mb");
        let left = pinned(&reader);
        join_pinned(&reader, &second, "mb");
        let held = || {
            let state = load_state(&reader).expect("state");
            (state.seq, state.membership, state.proof, state.requested)
        };
        let before = held();
        request_join(&reader, &left, Some("a-proof-for-the-vault-it-left".into())).expect("asked");
        assert_eq!(held(), before);
        // The request for the vault it is in is kept.
        let current = pinned(&reader);
        request_join(&reader, &current, Some("its-proof".into())).expect("asked");
        assert_eq!(
            load_state(&reader).expect("state").proof.as_deref(),
            Some("its-proof")
        );

        // An open admin's approval pass must keep reading that vault's queue even when a
        // join changes its local source, and cannot publish after the source change.
        struct Stop(Option<crate::server::Running>);
        impl Drop for Stop {
            fn drop(&mut self) {
                if let Some(server) = self.0.take() {
                    server.stop();
                }
            }
        }
        let server = Stop(Some(serve(&root.path().join("store"), "127.0.0.1:0")));
        let addr = server.0.as_ref().expect("running").addr;
        let admin = Paths::for_home(&root.path().join("admin"));
        let mut sources = Vec::new();
        for (name, value) in [("first", b"inert-first"), ("second", b"inert-other")] {
            let url = format!("http://{addr}/v1/{name}");
            let created = create(&admin, &url, None, Some("admin")).expect("create");
            let opened = admin_open(&admin).expect("open");
            let mut doc = opened.doc.clone();
            if name == "first" {
                doc.set_open(true).expect("open first vault");
            }
            doc.set_entry(&Audience::Everyone, "token", value)
                .expect("set token");
            admin_publish(&admin, &opened, &doc).expect("publish open vault");
            sources.push(format!("{url}#{}", created.signer));
        }
        let second_config = pinned(&admin);
        let from_a = Paths::for_home(&root.path().join("from-a"));
        let from_b = Paths::for_home(&root.path().join("from-b"));
        join_pinned(&from_a, &sources[0], "from-a");
        join_pinned(&from_b, &sources[1], "from-b");
        join_pinned(&admin, &sources[0], "admin");
        let opened = admin_open(&admin).expect("capture first vault");
        let mut checked = None;
        let (_, first_seq, first_approval) = approval_here(&admin, &mut checked)
            .expect("first policy")
            .expect("first approves");
        assert_eq!(first_approval, Approval::Open);
        // The durable boundary in a different-source join: state is reset and invalidated,
        // then config is published, before its first sync replaces the old cached locker.
        {
            let _lock = StateLock::take(&admin).expect("join writer");
            let mut state = load_state(&admin).expect("first state");
            state.seq = 0;
            state.digest = None;
            state.membership = Stored::None;
            prepare_materialization(&admin, &mut state).expect("invalidate first source");
            save_config(&admin, &second_config).expect("publish second source");
        }
        assert!(approval_here(&admin, &mut checked).is_err());
        assert!(
            checked.is_none(),
            "an incomplete cache is not retained policy"
        );
        sync(&admin, None).expect("finish second-source join");
        let current = pinned(&admin);
        let (_, second_seq, second_approval) = approval_here(&admin, &mut checked)
            .expect("second policy")
            .expect("second approves");
        assert_eq!(second_seq, first_seq);
        assert_eq!(second_approval, Approval::Password);

        // An unrelated materialized file can fail while the current verified admin locker
        // remains usable. Approval is not gated by whole-machine materialization readiness.
        let token = materialize::target_path(&admin.home, "token").unwrap();
        std::fs::remove_file(&token).expect("remove owned token");
        std::fs::create_dir(&token).expect("block only the owned entry");
        let SyncOutcome::Applied { report, .. } = sync(&admin, None).expect("partial apply") else {
            panic!("the blocked entry must be retried");
        };
        assert!(!report.failed.is_empty());
        let partial = load_state(&admin).expect("failed materialization state");
        assert!(partial.failed && partial.source_identity.is_none());
        let mut cold = None;
        let (_, seq, approval) = approval_here(&admin, &mut cold)
            .expect("verified cache despite failed file")
            .expect("still an admin");
        assert_eq!((seq, approval), (second_seq, Approval::Password));
        std::fs::remove_dir(&token).expect("remove only the empty owned blocker");
        sync(&admin, None).expect("repair materialization");
        let queued = |config: &Config| {
            requests_for(config, None)
                .expect("queue")
                .into_iter()
                .map(|request| request.host)
                .collect::<Vec<_>>()
        };
        assert_eq!(queued(&opened.config), ["from-a"]);
        assert_eq!(queued(&current), ["from-b"]);
        let mut closed = opened.doc.clone();
        closed.set_open(false).expect("close clone");
        let (_, _, held) = code_only_for(&opened.config, &closed, &opened.doc)
            .expect("hold only the opened vault's waiting keys");
        assert_eq!(
            held.iter()
                .map(|request| request.host.as_str())
                .collect::<Vec<_>>(),
            ["from-a"]
        );
        let body = |source: &Source| match transport::fetch(source, None, Duration::from_secs(30))
            .expect("fetch")
        {
            Fetched::Body(bytes) => bytes,
            other => panic!("expected a vault, got {other:?}"),
        };
        let before_a = body(&opened.config.source);
        let before_b = body(&current.source);
        let state = std::fs::read(admin.state_file()).expect("current state");
        assert!(auto_approve_opened(&admin, &opened).is_err());
        assert_eq!(body(&opened.config.source), before_a);
        assert_eq!(body(&current.source), before_b);
        assert_eq!(std::fs::read(admin.state_file()).unwrap(), state);
        for machine in [&from_a, &from_b] {
            assert_eq!(
                sync(machine, None).expect("not admitted"),
                SyncOutcome::NotIn
            );
            assert!(
                !materialize::target_path(&machine.home, "token")
                    .unwrap()
                    .exists()
            );
        }

        // A fresh pass on the same source admits only its own request and value.
        join_pinned(&admin, &sources[0], "admin");
        assert_eq!(
            auto_approve(&admin).expect("approve first").approved,
            ["from-a"]
        );
        sync(&from_a, None).expect("first machine syncs");
        assert_eq!(
            std::fs::read(materialize::target_path(&from_a.home, "token").unwrap()).unwrap(),
            b"inert-first"
        );
        assert_eq!(
            sync(&from_b, None).expect("still pending"),
            SyncOutcome::NotIn
        );
        assert!(
            !materialize::target_path(&from_b.home, "token")
                .unwrap()
                .exists()
        );
        assert_eq!(body(&current.source), before_b);

        // Only A's already-member request is judged: fetching B's unadmitted request here
        // would try to publish and fail the source guard instead of resting successfully.
        let fresh = admin_open(&admin).expect("capture admitted first vault");
        join_pinned(&admin, &sources[1], "admin");
        let rested = auto_approve_opened(&admin, &fresh).expect("no publication needed");
        assert!(rested.approved.is_empty());
        assert_eq!(rested.seq, None);
        assert_eq!(
            rested
                .judged
                .iter()
                .map(|request| request.host.as_str())
                .collect::<Vec<_>>(),
            ["from-a"]
        );
        assert_eq!(body(&current.source), before_b);
    }

    /// A path checks no signature, so a version of the vault's name there that the admin does
    /// not verify, however high its seq, must not floor the admin's copy: the removed admin's
    /// old key writing on after a rotation, or a version signed by no key of the vault's.
    #[test]
    fn an_admin_gives_a_path_its_copy_back_over_a_version_it_does_not_verify() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("shared").join("team.vault");
        std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
        let admin = Paths::for_home(&root.path().join("ma"));
        create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
        let old = admin_open(&admin).expect("open").seed;
        rotate(&admin);
        let seq = admin_open(&admin).expect("open").seq();
        let written = |seq: u64, seed: &[u8; 32]| {
            let unsigned = Unsigned {
                name: "team".to_string(),
                seq,
                time: 0,
                handoff: Vec::new(),
                lockers: Vec::new(),
            };
            envelope::sign(&unsigned, seed).expect("signed")
        };
        // The signature line ends `…xx==` and a LF: a character well inside it changed.
        let mut forged = written(seq, &old);
        let at = forged.len() - 10;
        forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
        for (what, bytes) in [
            ("the old key, far ahead", written(1 << 40, &old)),
            ("a signature that does not verify, at its seq", forged),
            ("no envelope at all", b"alab-vault/1\ngarbage\n".to_vec()),
        ] {
            std::fs::write(&shared, &bytes).expect("written");
            let opened = admin_open(&admin).unwrap_or_else(|error| panic!("{what}: {error}"));
            assert_eq!(opened.seq(), seq, "{what}");
            let cached = std::fs::read(admin.cache_file()).expect("cache");
            assert_eq!(std::fs::read(&shared).ok(), Some(cached), "{what}");
        }
        // A version signed by a key this machine never followed may be the vault (a fork it
        // did not follow, a rotation by a build before chains), whatever its seq: never
        // written over. The refusal names the key's fingerprint, never the key: nothing this
        // machine follows vouches for it, so a join to it is not offered ready to run.
        let stranger = envelope::signer_public(&[9u8; 32]).expect("key");
        for seq in [1, 1 << 40] {
            let other = written(seq, &[9u8; 32]);
            std::fs::write(&shared, &other).expect("written");
            let error = admin_open(&admin).expect_err("refused");
            assert!(error.0.contains("move the file aside"), "{error}");
            assert!(
                error.0.contains(&envelope::fingerprint(&stranger)),
                "{error}"
            );
            assert!(
                !error.0.contains(&crate::b64::encode_padded(&stranger)),
                "{error}"
            );
            assert_eq!(std::fs::read(&shared).ok(), Some(other), "seq {seq}");
        }
        // One claiming a handoff from the key this machine follows that the key never endorsed
        // is forged: plainly not the vault's, and the copy goes back over it.
        let pin = pinned(&admin).signer.expect("pinned");
        let forged_chain = Unsigned {
            name: "team".to_string(),
            seq: 1 << 40,
            time: 0,
            handoff: vec![(pin, [7u8; 64])],
            lockers: Vec::new(),
        };
        std::fs::write(
            &shared,
            envelope::sign_unchecked(&forged_chain, &[9u8; 32]).expect("signed"),
        )
        .expect("written");
        let opened = admin_open(&admin).expect("put back over a forged handoff");
        assert_eq!(opened.seq(), seq);
        let cached = std::fs::read(admin.cache_file()).expect("cache");
        assert_eq!(std::fs::read(&shared).ok(), Some(cached));
        // Another vault's version is no version of this one: the refusal says so, and offers
        // no join.
        let elsewhere = Unsigned {
            name: "other".to_string(),
            seq: 1,
            time: 0,
            handoff: Vec::new(),
            lockers: Vec::new(),
        };
        let elsewhere = envelope::sign(&elsewhere, &[9u8; 32]).expect("signed");
        std::fs::write(&shared, &elsewhere).expect("written");
        let error = admin_open(&admin).expect_err("refused");
        assert!(
            error.0.contains("holds the vault other, not team"),
            "{error}"
        );
        assert!(!error.0.contains("secrets join"), "{error}");
        assert_eq!(std::fs::read(&shared).ok(), Some(elsewhere));
        // A fork the old key endorsed is one only a build that retires that key names: the
        // ALab vault's build does, and the copy goes back over it.
        let old_key = envelope::signer_public(&old).expect("public");
        let fresh = [9u8; 32];
        let fork = Unsigned {
            name: "team".to_string(),
            seq: 1 << 40,
            time: 0,
            handoff: vec![(
                old_key,
                envelope::endorse(&old, "team", &envelope::signer_public(&fresh).expect("key"))
                    .expect("endorsed"),
            )],
            lockers: Vec::new(),
        };
        std::fs::write(&shared, envelope::sign(&fork, &fresh).expect("signed")).expect("written");
        assert!(admin_open(&admin).is_err(), "a fork no build names");
        let new_key = pinned(&admin).signer.expect("pinned");
        let opened = retiring(vec![old_key], new_key, || admin_open(&admin));
        assert_eq!(opened.expect("put back over the fork").seq(), seq);
        let cached = std::fs::read(admin.cache_file()).expect("cache");
        assert_eq!(std::fs::read(&shared).ok(), Some(cached));
    }

    #[test]
    fn an_admin_puts_back_the_link_a_rotation_by_a_build_before_chains_dropped() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("shared").join("team.vault");
        std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
        let path = shared.to_str().expect("utf-8");
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, path, None, Some("ma")).expect("create");
        let first = format!("{path}#{}", created.signer);
        let reader = Paths::for_home(&root.path().join("mb"));
        join_pinned(&reader, &first, "mb");
        let recipient = status(&reader)
            .expect("status")
            .machine
            .expect("a key")
            .0
            .to_bech32();
        {
            let opened = admin_open(&admin).expect("open");
            let mut doc = opened.doc.clone();
            doc.add_machine("mb", &recipient).expect("add");
            admin_publish(&admin, &opened, &doc).expect("publish");
        }
        // One rotation: one pair, which a build before chains reads too.
        assert!(!rotate(&admin).first_long_chain);
        assert!(matches!(
            sync(&reader, None).expect("sync"),
            SyncOutcome::Applied { seq: 3, .. }
        ));

        // An admin on a build before chains rotates again, carrying its own link alone, and
        // the file takes it as a server before chains would.
        let opened = admin_open(&admin).expect("open");
        let mut seed = [0u8; 32];
        aterm_uds::rand::fill(&mut seed).expect("random");
        let next = envelope::signer_public(&seed).expect("public");
        let name = &opened.config.name;
        let link = (
            envelope::signer_public(&opened.seed).expect("public"),
            envelope::endorse(&opened.seed, name, &next).expect("endorsed"),
        );
        let cut = seal(name, opened.seq + 1, &seed, &opened.doc, &[link]).expect("sealed");
        transport::publish(&opened.config.source, &cut).expect("written");

        // A reader that holds the chain refuses it: taking it would leave no version that
        // leads from the first key.
        let error = sync(&reader, None).expect_err("refused");
        assert!(
            error.0.contains("drops or changes handoff 1 of 1"),
            "{error}"
        );
        // The admin's copy still carries the dropped link, and its next version restores it.
        let healed = admin_open(&admin).expect("opened with the link put back");
        assert_eq!(healed.handoff.len(), 2);
        let done = admin_publish(&admin, &healed, &healed.doc.clone()).expect("publish");
        assert!(
            done.first_long_chain,
            "two pairs, which builds before chains cannot read"
        );
        assert!(matches!(
            sync(&reader, None).expect("sync"),
            SyncOutcome::Applied { seq: 5, .. }
        ));
        // And a new Mac whose build pins the first key still follows the vault.
        let late = Paths::for_home(&root.path().join("mc"));
        join_pinned(&late, &first, "mc");
        assert_eq!(pinned(&late).signer, Some(next));
    }
}

#[cfg(test)]
mod edit_scratch_tests {
    use super::{Paths, Scratch};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn an_unpublished_edit_and_unclaimed_files_survive_later_edits() {
        let home = aterm_tempfile::tempdir().expect("owned home");
        let paths = Paths::for_home(home.path());
        crate::fsx::ensure_private_dir(&paths.dir).expect("state directory");
        let unclaimed = paths.dir.join("edit-unclaimed");
        std::fs::create_dir(&unclaimed).expect("unclaimed directory");
        std::fs::write(unclaimed.join("draft"), b"unclaimed text").expect("unclaimed draft");

        let edit = Scratch::new(&paths).expect("exclusive edit");
        let kept = edit.dir().to_path_buf();
        assert_ne!(kept, unclaimed);
        assert_eq!(
            std::fs::symlink_metadata(&kept)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        assert!(
            crate::fsx::write_new(&kept.join("draft"), b"unpublished changes", 0o600)
                .expect("private draft")
        );
        drop(edit);
        assert_eq!(
            std::fs::read(kept.join("draft")).expect("kept"),
            b"unpublished changes"
        );

        let changed = Scratch::new(&paths).expect("changed cleanup control");
        let changed_dir = changed.dir().to_path_buf();
        let changed_file = changed_dir.join("draft");
        // Use the private writer used by the real editor path.
        crate::fsx::write_atomic(&changed_file, b"later unpublished text", 0o600)
            .expect("private later edit");
        assert!(changed.discard(&changed_file, b"published text").is_err());
        assert_eq!(
            std::fs::read(&changed_file).expect("changed draft kept"),
            b"later unpublished text"
        );

        let completed = Scratch::new(&paths).expect("another exclusive edit");
        let completed_dir = completed.dir().to_path_buf();
        assert_ne!(completed_dir, kept);
        assert_eq!(
            std::fs::read(unclaimed.join("draft")).expect("unclaimed kept"),
            b"unclaimed text"
        );
        assert_eq!(
            std::fs::read(kept.join("draft")).expect("interrupted edit kept"),
            b"unpublished changes"
        );
        let published = completed_dir.join("draft");
        crate::fsx::write_atomic(&published, b"published text", 0o600)
            .expect("private published copy");
        std::fs::write(completed_dir.join("editor-note"), b"other unpublished text")
            .expect("editor file");
        completed
            .discard(&published, b"published text")
            .expect("discard only the completed draft");
        assert!(!published.exists());
        assert!(!completed_dir.join(".lock").exists());
        assert_eq!(
            std::fs::read(completed_dir.join("editor-note")).expect("editor file kept"),
            b"other unpublished text"
        );

        let completed = Scratch::new(&paths).expect("empty cleanup control");
        let dir = completed.dir().to_path_buf();
        let published = dir.join("draft");
        crate::fsx::write_atomic(&published, b"published text", 0o600)
            .expect("private published copy");
        completed
            .discard(&published, b"published text")
            .expect("known completed draft removed");
        assert!(!dir.exists(), "an empty owned edit directory is retired");
        assert_eq!(
            std::fs::read(kept.join("draft")).expect("still kept"),
            b"unpublished changes"
        );
    }
    #[test]
    fn a_broadly_readable_completed_draft_is_refused_and_preserved() {
        let home = aterm_tempfile::tempdir().expect("owned home");
        let edit = Scratch::new(&Paths::for_home(home.path())).expect("edit");
        let file = edit.dir().join("draft");
        assert!(crate::fsx::write_new(&file, b"published text", 0o600).expect("private draft"));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
            .expect("exposed draft control");
        assert!(edit.discard(&file, b"published text").is_err());
        assert_eq!(
            std::fs::read(&file).expect("refused draft kept"),
            b"published text"
        );
    }
}
