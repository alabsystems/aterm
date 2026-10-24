// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client flows and admin workflows for `aterm` secrets.

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
    /// Verified handoff keys (`oldest` first) through which `signer` was reached.
    pub followed: Vec<[u8; 32]>,
    /// The name this machine asked to be called.
    pub host: String,
}

/// Whether a requested join pin admits this configured authority without fetching.
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
    /// URL (optionally `#<signer>`), path, or empty for the ALab default.
    pub source: String,
    /// The machine name to ask for (default: [`default_host_name`]).
    pub host: Option<String>,
    /// Whether a person is at a terminal for fingerprint confirmation and password prompts.
    pub interactive: bool,
    /// How long to wait for the approval after asking (zero: return at once, pending).
    pub wait: Duration,
    /// Shown a fingerprint, answers whether to trust it (interactive only).
    pub confirm: &'a dyn Fn(&str) -> bool,
    /// Returns the vault's join password when joining a URL source, or `None` for manual approval.
    pub password: &'a dyn Fn() -> Option<SecretBytes>,
    /// Called once the request is queued and before any wait.
    pub requested: &'a dyn Fn(),
    /// Progress lines for a person (never a value).
    pub say: &'a dyn Fn(&str),
}

#[derive(Clone, Copy)]
enum ExpectedConfig<'a> {
    Any,
    Read(Option<&'a Config>),
}

impl ExpectedConfig<'_> {
    fn check(self, paths: &Paths) -> crate::Result<()> {
        if let Self::Read(expected) = self
            && config(paths)?.as_ref() != expected
        {
            return Err(Error::new(
                "the configured vault changed after it was read; try again",
            ));
        }
        Ok(())
    }
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
    /// The source held an older version than this machine verified, so this machine's copy was
    /// put back.
    PutBack {
        /// The seq held, and now the source's.
        seq: u64,
        /// The seq the source held, `None` when it held none.
        served: Option<u64>,
    },
    /// No locker opens for this machine (yet, or any more).
    NotIn,
}

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

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

const MAX_STATE: usize = 4 * text::MAX_TEXT;

fn read_toml(path: &Path, what: &str, max: usize) -> crate::Result<Option<aterm_toml::Table>> {
    let Some(bytes) = fsx::read_private(path, max)? else {
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

/// Default machine name from the computer, LAN or host name (an `m<N>` tag first), or `"machine"`.
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
    if let Some(tag) = candidates.iter().find_map(|c| extract_machine_tag(c)) {
        return tag;
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

fn extract_machine_tag(raw: &str) -> Option<String> {
    let stem = raw
        .trim()
        .strip_suffix(".local")
        .unwrap_or_else(|| raw.trim());
    let tokens: Vec<&str> = stem
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '-' | '_' | '(' | ')'))
        .filter(|s| !s.is_empty())
        .collect();
    for (idx, &tok) in tokens.iter().enumerate() {
        let lower = tok.to_ascii_lowercase();
        let Some(digits) = lower.strip_prefix('m') else {
            continue;
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(num) = digits.parse::<u32>() else {
            continue;
        };
        if idx > 0
            && (1..=5).contains(&num)
            && tokens.get(idx + 1).is_some_and(|next| {
                let nl = next.to_ascii_lowercase();
                matches!(nl.as_str(), "max" | "pro" | "ultra" | "air")
                    || (nl.ends_with("gb")
                        && nl[..nl.len() - 2].bytes().all(|b| b.is_ascii_digit()))
            })
        {
            continue;
        }
        if text::valid_machine_name(&lower) {
            return Some(lower);
        }
    }
    None
}

/// 80-bit SHA-256 prefix of `recipient` in lowercase RFC 4648 base32 (`xxxx-xxxx-xxxx-xxxx`).
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

/// The configured source, if any.
pub fn config(paths: &Paths) -> crate::Result<Option<Config>> {
    let Some(table) = read_toml(&paths.source_file(), "source.toml", 4 << 20)? else {
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
    const fn word(self) -> &'static str {
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
    source_identity: Option<[u8; 32]>,
    heard: u64,
    requested: u64,
    membership: Stored,
    /// The join request's proof of the password, re-sent with each hourly request.
    proof: Option<String>,
    /// The last apply left a file unwritten: the next fetch of the same version applies again.
    failed: bool,
    manifest: Manifest,
    /// Entry name → digest of each value a pass is to write, saved before it writes and cleared
    /// by its own save (a save that applies nothing keeps it), so the next apply still counts
    /// the files a cut-short pass wrote as the vault's ([`materialize::apply_resuming`]).
    pending: materialize::Pending,
}

fn load_state(paths: &Paths) -> crate::Result<State> {
    let Some(table) = read_toml(&paths.state_file(), "state.toml", MAX_STATE)? else {
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
    if text.len() > MAX_STATE {
        return Err(Error::new(format!(
            "{}: would be larger than {MAX_STATE} bytes: the vault holds more entries than this machine can track",
            paths.state_file().display()
        )));
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

fn prepare_materialization(paths: &Paths, state: &mut State) -> crate::Result<()> {
    state.failed = true;
    state.source_identity = None;
    save_state(paths, state)
}

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

/// Whether this machine's join request included a join-password proof.
pub fn asked_with_password(paths: &Paths) -> bool {
    load_state(paths).is_ok_and(|state| state.proof.is_some())
}

/// Whether an admitted vault's last sync finished and its tracked files still match.
pub fn materialization_ready(paths: &Paths) -> crate::Result<bool> {
    materialization_ready_with(paths, None, files_intact)
}

/// Observe readiness only while `expected` is still configured.
pub fn materialization_ready_for(paths: &Paths, expected: &Config) -> crate::Result<bool> {
    materialization_ready_with(paths, Some(expected), files_intact)
}

/// Local materialization facts that can change a setup retry's answer.
/// This is an observation, never authority to use credentials or change a vault.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MaterializationMark([u8; 32]);

impl std::fmt::Debug for MaterializationMark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MaterializationMark")
            .finish_non_exhaustive()
    }
}

impl MaterializationMark {
    /// Encode an opaque identity in a private retry cache, not in diagnostics.
    pub fn cache_key(&self) -> String {
        hex(&self.0)
    }

    /// Read an identity from a private retry cache.
    pub fn from_cache_key(key: &str) -> Option<Self> {
        let bytes = aterm_codec::hex::decode_vec(key.as_bytes())?;
        Some(Self(<[u8; 32]>::try_from(bytes.as_slice()).ok()?))
    }
}

/// Observe the configured version and whether its local files are complete.
/// A same-version repair and a plain vault's changed values move this mark;
/// heartbeats, join proofs and unchanged syncs do not. A busy or missing lock
/// gives no reading. No state is created, and no source is fetched.
pub fn materialization_mark(paths: &Paths) -> crate::Result<Option<MaterializationMark>> {
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    let ready = pin_admitted(&config, None)
        && matches!(state.membership, Stored::In | Stored::Plain)
        && source_bound(&config, &state)
        && files_intact(paths, &state);
    let mut mark = aterm_digest::Sha256::new();
    mark.update(b"aterm materialization observation/1\0");
    mark.update(source_identity(&config));
    mark.update(state.seq.to_be_bytes());
    mark.update([u8::from(ready), u8::from(state.digest.is_some())]);
    if let Some(digest) = state.digest {
        mark.update(digest);
    }
    for (name, digest) in &state.manifest.files {
        mark.update((name.len() as u64).to_be_bytes());
        mark.update(name.as_bytes());
        mark.update((digest.len() as u64).to_be_bytes());
        mark.update(digest.as_bytes());
    }
    Ok(Some(MaterializationMark(mark.finalize())))
}

/// The source and complete materialized version whose local inputs were read.
/// Heartbeats and pending join proofs are not part of this identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializationSnapshot {
    config: Config,
    seq: u64,
    digest: Option<[u8; 32]>,
    manifest: Manifest,
}

/// Local inputs read while their materialized version was held against writers.
pub struct Materialized<T> {
    pub inputs: T,
    pub snapshot: MaterializationSnapshot,
}

impl MaterializationSnapshot {
    /// Whether the same complete version is still configured and intact.
    /// This is a point-in-time check, not a lease over later effects.
    /// # Errors
    /// The local configuration or state cannot be safely read.
    pub fn is_current(&self, paths: &Paths) -> crate::Result<bool> {
        Ok(with_materialization_ready(paths, &self.config, || Ok(()))?
            .is_some_and(|current| current.snapshot == *self))
    }
}

/// Read local inputs under the ready vault's shared state lock, releasing it on return.
/// `read` must do local reads only: no network, subprocess or other effects.
/// # Errors
/// The local configuration/state or the supplied input reader fails.
pub fn with_materialization_ready<T>(
    paths: &Paths,
    expected: &Config,
    read: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<Option<Materialized<T>>> {
    with_ready_state(paths, Some(expected), files_intact, |config, state| {
        Ok(Materialized {
            inputs: read()?,
            snapshot: MaterializationSnapshot {
                config,
                seq: state.seq,
                digest: state.digest,
                manifest: state.manifest,
            },
        })
    })
}

fn materialization_ready_with(
    paths: &Paths,
    expected: Option<&Config>,
    intact: impl FnOnce(&Paths, &State) -> bool,
) -> crate::Result<bool> {
    Ok(with_ready_state(paths, expected, intact, |_, _| Ok(()))?.is_some())
}

fn with_ready_state<T>(
    paths: &Paths,
    expected: Option<&Config>,
    intact: impl FnOnce(&Paths, &State) -> bool,
    read: impl FnOnce(Config, State) -> crate::Result<T>,
) -> crate::Result<Option<T>> {
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    if expected.is_some_and(|expected| *expected != config) || !pin_admitted(&config, None) {
        return Ok(None);
    }
    let state = load_state(paths)?;
    if !(matches!(state.membership, Stored::In | Stored::Plain)
        && source_bound(&config, &state)
        && intact(paths, &state))
    {
        return Ok(None);
    }
    read(config, state).map(Some)
}

const ADMIN_MAGIC: &str = "alab-vault-admin/1";

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

/// What the vault at a source answers now ([`look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Look {
    /// A vault answers at the URL or path.
    Answers,
    /// Transient transport failure or unreached source.
    Unreached(String),
    /// Unreached, the last failure [`transport::Unheard::held`], as macOS fails this app's LAN
    /// traffic while its Local Network question is unanswered or denied. A host that is off or a
    /// server that stalls fails alike, so this is a hint to name the setting, never a finding.
    Held(String),
    /// The endpoint or path holds no vault.
    NoVault(String),
}

const LOOK_PAUSE: Duration = Duration::from_secs(2);

trait Tries {
    fn get(&mut self, url: &str, wait: Duration) -> Result<transport::Answer, transport::Unheard>;
    fn now(&mut self) -> Instant;
    fn pause(&mut self, pause: Duration);
}

struct LiveTries;

impl Tries for LiveTries {
    fn get(&mut self, url: &str, wait: Duration) -> Result<transport::Answer, transport::Unheard> {
        transport::get_bounded(url, None, wait, Some(transport::LOOK_CONNECT))
    }

    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// Probe whether a vault answers at `source` within `wait` without modifying local state.
pub fn look(source: &Source, wait: Duration, patience: Duration, held: &dyn Fn(&str)) -> Look {
    look_with(source, wait, patience, held, &mut LiveTries)
}

fn look_with(
    source: &Source,
    wait: Duration,
    patience: Duration,
    held: &dyn Fn(&str),
    tries: &mut dyn Tries,
) -> Look {
    let shown = match source {
        Source::Url(url) => transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    };
    let fetched = match source {
        Source::Url(url) => {
            let start = tries.now();
            let mut told = false;
            loop {
                match tries.get(url, wait) {
                    Ok(transport::Answer::Fetched(fetched)) => break fetched,
                    Ok(transport::Answer::Foreign(why)) => return Look::NoVault(why),
                    Err(unheard)
                        if unheard.held
                            && tries
                                .now()
                                .saturating_duration_since(start)
                                .saturating_add(LOOK_PAUSE)
                                < patience =>
                    {
                        if !told {
                            told = true;
                            held(&unheard.why.0);
                        }
                        tries.pause(LOOK_PAUSE);
                    }
                    Err(unheard) if unheard.held => return Look::Held(unheard.why.0),
                    Err(unheard) => return Look::Unreached(unheard.why.0),
                }
            }
        }
        Source::Path(_) => match transport::fetch(source, None, wait) {
            Ok(fetched) => fetched,
            Err(error) => return Look::Unreached(error.to_string()),
        },
    };
    match fetched {
        Fetched::Body(bytes) if is_envelope(&bytes) || matches!(source, Source::Path(_)) => {
            Look::Answers
        }
        Fetched::Body(_) => Look::NoVault(format!("{shown} does not serve an aterm secrets vault")),
        Fetched::Missing => Look::NoVault(format!("{shown}: no vault there")),
        Fetched::NoNewer(_) => Look::NoVault("the source answered with no vault".into()),
    }
}

/// Copy `source.toml` and `state.toml` under the state lock into a newly created `into` (`0700`).
/// # Errors
/// The existing state cannot be locked or read, `into` already exists, or the new directory
/// or a copy cannot be written. A partial new copy is retained and is never reused.
pub fn keep_config(paths: &Paths, into: &Path) -> crate::Result<()> {
    keep_config_with(paths, into, ExpectedConfig::Any)
}

/// Keep only the configuration the caller read, refusing a different or unreadable one.
/// # Errors
/// As [`keep_config`], or the current configuration differs from `expected`.
pub fn keep_config_if_unchanged(
    paths: &Paths,
    into: &Path,
    expected: Option<&Config>,
) -> crate::Result<()> {
    keep_config_with(paths, into, ExpectedConfig::Read(expected))
}

fn keep_config_with(paths: &Paths, into: &Path, expected: ExpectedConfig<'_>) -> crate::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let files = match StateLock::observe(paths)? {
        Some(_lock) => {
            expected.check(paths)?;
            let mut files = Vec::new();
            for (from, name) in [
                (paths.source_file(), "source.toml"),
                (paths.state_file(), "state.toml"),
            ] {
                if let Some(bytes) = fsx::read_private(&from, 4 << 20)? {
                    files.push((name, bytes));
                }
            }
            files
        }
        None if fsx::kind(&paths.dir)? == fsx::Kind::Missing => {
            expected.check(paths)?;
            Vec::new()
        }
        None => {
            return Err(Error::new(
                "the vault state lock is busy or unavailable; try again",
            ));
        }
    };
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(into)
        .map_err(|error| Error::new(format!("{}: {error}", into.display())))?;
    for (name, bytes) in files {
        fsx::write_atomic(&into.join(name), bytes.as_slice(), 0o600)?;
    }
    Ok(())
}

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

fn pin_gives_way(config: &Config) -> bool {
    config
        .signer
        .is_some_and(|pin| Retired::compiled().gives_way(&pin, &config.followed))
}

fn cached_chain(paths: &Paths, pin: &[u8; 32]) -> Vec<[u8; 32]> {
    fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .ok()
        .flatten()
        .and_then(|bytes| envelope::parse(bytes.as_slice()).ok())
        .filter(|cached| cached.signer == *pin && cached.verify_signature().is_ok())
        .map(|cached| cached.reached_from())
        .unwrap_or_default()
}

fn chain_keys(envelope: &Envelope) -> Vec<[u8; 32]> {
    envelope.handoff.iter().map(|(key, _)| *key).collect()
}

fn files_intact(paths: &Paths, state: &State) -> bool {
    !state.failed
        && state.pending.is_empty()
        && state.manifest.files.iter().all(|(name, digest)| {
            materialize::behind_a_link(&paths.home, name)
                || materialize::target_path(&paths.home, name)
                    .ok()
                    .and_then(|path| fsx::read_private(&path, text::MAX_VALUE).ok().flatten())
                    .is_some_and(|bytes| materialize::digest_hex(bytes.as_slice()) == *digest)
        })
}

fn place(source: &Source) -> String {
    match source {
        Source::Url(url) => transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    }
}

fn held_copy(paths: &Paths, config: &Config, state: &State) -> Option<Vec<u8>> {
    let pin = config.signer?;
    let elsewhere = state
        .source_identity
        .is_some_and(|bound| bound != source_identity(config));
    if state.membership != Stored::In || elsewhere || pin_gives_way(config) {
        return None;
    }
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .ok()
        .flatten()?;
    let (envelope, _, _) = envelope::verify_for_reader(
        cached.as_slice(),
        &config.name,
        &pin,
        &config.followed,
        None,
    )
    .ok()?;
    (envelope.seq == state.seq && Some(envelope.digest()) == state.digest)
        .then(|| cached.as_slice().to_vec())
}

fn missing(config: &Config) -> Error {
    Error::new(format!(
        "the source holds no vault named {}; a Mac in the vault puts its copy back",
        config.name
    ))
}

const fn quiet(report: &Report) -> bool {
    report.written.is_empty()
        && report.removed.is_empty()
        && report.moved_aside.is_empty()
        && report.failed.is_empty()
}

/// Fetch, verify, decrypt, and materialize once, optionally long-polling for `wait`.
pub fn sync(paths: &Paths, wait: Option<Duration>) -> crate::Result<SyncOutcome> {
    let mut config = config(paths)?.ok_or_else(|| {
        Error::new("no vault configured: run aterm secrets join <url> (or aterm init)")
    })?;
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
    let lock = StateLock::take(paths)?;
    if self::config(paths)?.as_ref() != Some(&config) {
        return Ok(SyncOutcome::Unchanged { seq: before.seq });
    }
    let mut state = load_state(paths)?;
    let bytes = match fetched {
        Fetched::NoNewer(served) => {
            if now().saturating_sub(state.heard) >= 60 {
                state.heard = now();
                save_state(paths, &state)?;
            }
            let older = served.filter(|served| {
                *served < state.seq && (state.seq, state.digest) == (before.seq, before.digest)
            });
            let Some((served, copy)) =
                older.and_then(|served| Some((served, held_copy(paths, &config, &state)?)))
            else {
                return Ok(SyncOutcome::Unchanged { seq: state.seq });
            };
            drop(lock);
            transport::publish(&config.source, &copy).map_err(|error| {
                Error::new(format!(
                    "{} holds seq {served} of {}, older than this machine's seq {}, and putting this machine's copy back failed: {error}",
                    place(&config.source),
                    config.name,
                    state.seq
                ))
            })?;
            return Ok(SyncOutcome::PutBack {
                seq: state.seq,
                served: Some(served),
            });
        }
        Fetched::Missing => {
            let copy = match config.source {
                Source::Url(_) => held_copy(paths, &config, &state),
                Source::Path(_) => None,
            };
            let Some(copy) = copy else {
                return Err(missing(&config));
            };
            drop(lock);
            transport::publish(&config.source, &copy).map_err(|error| {
                Error::new(format!(
                    "{} holds no vault named {}, and putting this machine's copy back failed: {error}",
                    place(&config.source),
                    config.name
                ))
            })?;
            return Ok(SyncOutcome::PutBack {
                seq: state.seq,
                served: None,
            });
        }
        Fetched::Body(bytes) => bytes,
    };
    if config.signer.is_some()
        && state.seq > before.seq
        && envelope::parse(&bytes).is_ok_and(|parsed| parsed.seq <= state.seq)
    {
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
        state.seq = 0;
        state.digest = None;
    }
    if let Pin::Rotated(new) | Pin::Replaced(new) = pinned {
        prepare_materialization(paths, &mut state)?;
        config.signer = Some(new);
        config.followed = envelope.reached_from();
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

/// Long-poll the source to apply new versions and run the admin approver loop.
pub fn watch(paths: &Paths, log: &(dyn Fn(&str) + Sync)) -> crate::Result<()> {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| approver_loop(paths, log, &stop));
        let ended = subscriber_loop(paths, log);
        stop.store(true, Ordering::Relaxed);
        ended
    })
}

fn program_stamp() -> Option<(u64, u64, u64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    Some((meta.dev(), meta.ino(), meta.size(), meta.mtime()))
}

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

const FIRST_RETRY: Duration = Duration::from_secs(5);
const LONGEST_RETRY: Duration = Duration::from_secs(300);

const LONGEST_ERROR_RETRY: Duration = Duration::from_secs(60);
const MOVED_STEP: Duration = Duration::from_secs(5);

fn nap_unless_moved(
    total: Duration,
    moved: &mut dyn FnMut() -> bool,
    sleep: &mut dyn FnMut(Duration),
) -> bool {
    let mut left = total;
    while !left.is_zero() {
        let step = left.min(MOVED_STEP);
        sleep(step);
        left = left.saturating_sub(step);
        if moved() {
            return true;
        }
    }
    false
}

fn retry_after(backoff: Duration, moved: bool) -> Duration {
    if moved {
        Duration::from_secs(5)
    } else {
        backoff.saturating_mul(2).min(LONGEST_ERROR_RETRY)
    }
}

fn pause_after(source: &Source, outcome: &SyncOutcome, failing: &mut Duration) -> Duration {
    let pause = match (source, outcome) {
        (Source::Path(_), _) => Duration::from_secs(60),
        (_, SyncOutcome::NotIn) => Duration::from_secs(30),
        (_, SyncOutcome::Applied { report, .. }) if report.failed.is_empty() => Duration::ZERO,
        (_, SyncOutcome::Applied { .. } | SyncOutcome::PutBack { served: None, .. }) => {
            let pause = *failing;
            *failing = failing.saturating_mul(2).min(LONGEST_RETRY);
            return pause;
        }
        (_, SyncOutcome::Unchanged { .. } | SyncOutcome::PutBack { .. }) => Duration::from_secs(10),
    };
    *failing = FIRST_RETRY;
    pause
}

fn pass_line(
    paths: &Paths,
    config: &Config,
    outcome: &SyncOutcome,
    repeated: &mut String,
) -> Option<String> {
    let (line, again) = match outcome {
        SyncOutcome::Applied { seq, report } => (
            Some(format!(
                "applied seq {seq}: {} written, {} unchanged, {} removed, {} moved aside, {} failed",
                report.written.len(),
                report.unchanged.len(),
                report.removed.len(),
                report.moved_aside.len(),
                report.failed.len()
            )),
            !report.failed.is_empty(),
        ),
        SyncOutcome::Unchanged { .. } => (None, false),
        SyncOutcome::PutBack {
            seq,
            served: Some(served),
        } => (
            Some(format!(
                "{} held seq {served} of {}, older than this machine's seq {seq}: put this machine's copy back",
                place(&config.source),
                config.name
            )),
            false,
        ),
        SyncOutcome::PutBack { seq, served: None } => (
            Some(format!(
                "{} held no vault named {}: put this machine's copy back (seq {seq})",
                place(&config.source),
                config.name
            )),
            true,
        ),
        SyncOutcome::NotIn => (
            load_state(paths)
                .is_ok_and(|s| s.membership == Stored::Revoked)
                .then(|| "this machine is no longer in the vault".to_string()),
            false,
        ),
    };
    let last = std::mem::take(repeated);
    if let (true, Some(line)) = (again, &line) {
        repeated.clone_from(line);
    }
    line.filter(|line| *line != last)
}

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

fn subscriber_loop(paths: &Paths, log: &(dyn Fn(&str) + Sync)) -> crate::Result<()> {
    let mut backoff = Duration::from_secs(5);
    let mut failing = FIRST_RETRY;
    let mut repaired = None;
    let mut last_error = String::new();
    let mut repeated = String::new();
    let program = program_stamp();
    loop {
        if program.is_some() && program_stamp() != program {
            log("this program was replaced on disk; exiting so the service starts the new one");
            return Ok(());
        }
        let started = Instant::now();
        match subscriber_pass(paths, &mut repaired) {
            Ok((config, outcome)) => {
                backoff = Duration::from_secs(5);
                last_error.clear();
                if let Some(line) = pass_line(paths, &config, &outcome, &mut repeated) {
                    log(&line);
                }
                ask_again_if_due(paths, &config, log);
                let pause = pause_after(&config.source, &outcome, &mut failing);
                std::thread::sleep(pause.saturating_sub(started.elapsed()));
            }
            Err(error) => {
                let text = error.to_string();
                if text != last_error {
                    log(&format!(
                        "{text}; retrying with backoff up to 1 min, at once on a new network"
                    ));
                    last_error = text;
                }
                let routes = crate::server::route_sources();
                let moved = nap_unless_moved(
                    backoff,
                    &mut || crate::server::route_sources() != routes,
                    &mut std::thread::sleep,
                );
                backoff = retry_after(backoff, moved);
            }
        }
    }
}

fn subscriber_pass(
    paths: &Paths,
    repaired: &mut Option<([u8; 32], u64)>,
) -> crate::Result<(Config, SyncOutcome)> {
    let config = config(paths)?
        .ok_or_else(|| Error::new("no vault configured; waiting for aterm secrets join"))?;
    let before = *repaired;
    let wait = match config.source {
        Source::Url(_) if !repair_due(paths, repaired) => Some(Duration::from_secs(55)),
        Source::Url(_) | Source::Path(_) => None,
    };
    match sync(paths, wait) {
        Ok(outcome) => Ok((config, outcome)),
        Err(error) => {
            *repaired = before;
            let _ = change_request(paths, &config, |state| {
                let due = state.membership == Stored::Pending && state.requested != 0;
                if due {
                    state.requested = 0;
                }
                due
            });
            Err(error)
        }
    }
}

/// How often a machine still waiting asks again ([`ask_again_if_due`]).
const REASK: u64 = 60 * 60;

/// How long a join has, from saving the vault, to post its own request ([`in_flight`]).
const JOIN_GRACE: u64 = 5 * 60;

/// The `requested` a join records when it saves the vault, before it asks for the password
/// and posts: dated so the re-ask comes due [`JOIN_GRACE`] later. A subscriber asking sooner,
/// without the join's proof, would make the join's own request a change the server refuses for
/// a while; a join cut off before it posts is asked for after the grace, not an hour on.
fn in_flight() -> u64 {
    now().saturating_sub(REASK - JOIN_GRACE)
}

fn ask_again_if_due(paths: &Paths, config: &Config, log: &dyn Fn(&str)) {
    let due = || -> crate::Result<Option<(Config, Option<String>)>> {
        let _lock = StateLock::take(paths)?;
        let Some(current) = self::config(paths)?
            .filter(|current| (&current.source, &current.name) == (&config.source, &config.name))
        else {
            return Ok(None);
        };
        let state = load_state(paths)?;
        Ok(
            (state.membership == Stored::Pending && now().saturating_sub(state.requested) >= REASK)
                .then_some((current, state.proof)),
        )
    };
    let Ok(Some((current, proof))) = due() else {
        return;
    };
    match request_join(paths, &current, proof, Asker::Again) {
        Ok(code) => log(&format!("asked to join again (code {code})")),
        Err(error) => log(&format!("join request failed: {error}")),
    }
}

/// Automatic join-approval policy configured in `[join]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// `password = …`: requests whose proof matches it.
    Password,
    /// `open = yes`: requests from machines not in the vault, except `code-only` keys.
    Open,
}

impl Approval {
    /// Automatic approval mode configured in `doc`, if any.
    pub fn of(doc: &Document) -> Option<Approval> {
        if doc.open() {
            return Some(Approval::Open);
        }
        doc.join_password().map(|_| Approval::Password)
    }

    const fn reads(self, request: &Request) -> bool {
        match self {
            Approval::Password => request.proof.is_some(),
            Approval::Open => true,
        }
    }
}

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

/// Automatic approval mode active on this machine for a URL-backed vault, read from local state.
pub fn approving_here(paths: &Paths) -> crate::Result<Option<Approval>> {
    Ok(approval_here(paths, &mut None)?
        .filter(|(config, _, _)| matches!(config.source, Source::Url(_)))
        .map(|(_, _, approval)| approval))
}

fn approver_loop(paths: &Paths, log: &(dyn Fn(&str) + Sync), stop: &AtomicBool) {
    let mut approver = Approver::default();
    while !stop.load(Ordering::Relaxed) {
        let pause = approver_pass(paths, log, &mut approver);
        nap(stop, pause);
    }
}

/// What the approver keeps from one pass to the next.
#[derive(Default)]
struct Approver {
    settled: Settled,
    checked: Option<ApprovalCheck>,
}

fn approver_pass(paths: &Paths, log: &dyn Fn(&str), approver: &mut Approver) -> Duration {
    let (config, seq, approval) = match approval_here(paths, &mut approver.checked) {
        Ok(Some(held)) => held,
        Ok(None) => return Duration::from_secs(30),
        Err(_) => return Duration::from_secs(2),
    };
    let Ok(pending) = requests_for(&config, None) else {
        return Duration::from_secs(10);
    };
    let by = match approval {
        Approval::Password => "by the join password",
        Approval::Open => "as the vault is open",
    };
    if approver.settled.due(&config, seq, &pending, approval) {
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
                approver.settled.settle(done.judged);
            }
            Err(error) => {
                log(&format!("approving {by} failed: {error}"));
                return Duration::from_secs(30);
            }
        }
    }
    Duration::from_secs(2)
}

/// Join request key tuple evaluated by [`auto_approve`].
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

/// Join requests already judged at the current vault version.
#[derive(Debug, Default)]
pub struct Settled {
    version: Option<([u8; 32], u64)>,
    judged: std::collections::BTreeSet<Judged>,
}

impl Settled {
    /// Whether a request in `listed` relevant to `approval` is unjudged at `(config, seq)`.
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
        let still: std::collections::BTreeSet<Judged> = listed.iter().map(Judged::of).collect();
        self.judged.retain(|judged| still.contains(judged));
        listed
            .iter()
            .filter(|request| approval.reads(request))
            .any(|request| !self.judged.contains(&Judged::of(request)))
    }

    /// Record requests judged at the current version.
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
    /// Keys on an open vault's `code-only` list left for manual code approval.
    pub code_only: Vec<String>,
    /// Every request read and judged in this pass.
    pub judged: Vec<Judged>,
    /// The version published, when any machine was added.
    pub seq: Option<u64>,
}

/// Approve and publish all queued requests eligible under the vault's `[join]` policy.
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
    let mut members: Vec<String> = admin
        .doc
        .machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .collect();
    let code_only = admin.doc.code_only();
    let mut doc = admin.doc.clone();
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
                members.push(request.recipient.clone());
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
    /// The endpoint answered with something other than the requested vault.
    NotThisVault(String),
}

/// Check whether `url` serves a vault verified under `pin` within `wait`.
pub fn probe(url: &str, pin: &[u8; 32], wait: Duration) -> crate::Result<ProbeOutcome> {
    let name = transport::vault_name(&Source::Url(url.to_string()))?;
    let body = match transport::get(url, None, wait)? {
        transport::Answer::Fetched(Fetched::Body(body)) => body,
        transport::Answer::Fetched(Fetched::Missing | Fetched::NoNewer(_)) => {
            return Ok(ProbeOutcome::Absent);
        }
        transport::Answer::Foreign(why) => return Ok(ProbeOutcome::NotThisVault(why)),
    };
    let verified = envelope::verify_for_reader(&body, &name, pin, &[], None);
    Ok(match verified {
        Ok((envelope, _, _)) => ProbeOutcome::Found(Probed {
            seq: envelope.seq,
            fingerprint: envelope::fingerprint(&envelope.signer),
        }),
        Err(error) => ProbeOutcome::NotThisVault(error.to_string()),
    })
}

/// Probe the default ALab vault URL under the compiled-in signer.
pub fn probe_default(wait: Duration) -> crate::Result<ProbeOutcome> {
    match decode_key32(crate::ALAB_SIGNER) {
        Some(pin) => probe(crate::ALAB_DEFAULT_URL, &pin, wait),
        None => Ok(ProbeOutcome::Absent),
    }
}

/// Who asks to join, which says what [`request_join`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asker {
    /// A person's join: it records the proof it asked with, and when it asked.
    Join,
    /// The subscriber asking again with the proof it read ([`ask_again_if_due`]): it records
    /// only when it asked, and only while the state still holds that proof.
    Again,
}

fn request_join(
    paths: &Paths,
    config: &Config,
    proof: Option<String>,
    asker: Asker,
) -> crate::Result<String> {
    let identity = machine_identity(paths)?;
    let recipient = identity.recipient();
    let code = join_code(&recipient);
    let posted = match &config.source {
        Source::Url(url) => {
            let mut line = format!("{} {} {code}", recipient.to_bech32(), config.host);
            if let Some(proof) = &proof {
                line.push_str(&format!(" proof={proof}"));
            }
            transport::post_join(url, &line)
        }
        Source::Path(_) => Ok(()),
    };
    let queued = posted.is_ok();
    let recorded = change_request(paths, config, |state| {
        if asker == Asker::Again && state.proof != proof {
            return false;
        }
        state.requested = if queued { now() } else { 0 };
        if asker == Asker::Join {
            state.proof = proof;
            if matches!(state.membership, Stored::None | Stored::Revoked) {
                state.membership = Stored::Pending;
            }
        }
        true
    });
    posted.and(recorded).map(|()| code)
}

fn change_request(
    paths: &Paths,
    config: &Config,
    change: impl FnOnce(&mut State) -> bool,
) -> crate::Result<()> {
    let _lock = StateLock::take(paths)?;
    let current = self::config(paths)?;
    if current
        .as_ref()
        .is_none_or(|current| (&current.source, &current.name) != (&config.source, &config.name))
    {
        return Ok(());
    }
    let mut state = load_state(paths)?;
    if !change(&mut state) {
        return Ok(());
    }
    save_state(paths, &state)
}

fn default_pin(source: &Source) -> Option<[u8; 32]> {
    match source {
        Source::Url(url) if url == crate::ALAB_DEFAULT_URL => decode_key32(crate::ALAB_SIGNER),
        _ => None,
    }
}

/// Join a vault: configure the source and machine key, verify, then sync or queue a join request.
pub fn join(paths: &Paths, options: &JoinOptions<'_>) -> crate::Result<JoinOutcome> {
    join_with(paths, options, ExpectedConfig::Any, |source| {
        transport::fetch(source, None, Duration::from_secs(30))
    })
}

/// Join only over the configuration the caller read.
/// # Errors
/// As [`fn@join`], or the current configuration differs from `expected`.
pub fn join_if_unchanged(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: Option<&Config>,
) -> crate::Result<JoinOutcome> {
    join_with(paths, options, ExpectedConfig::Read(expected), |source| {
        transport::fetch(source, None, Duration::from_secs(30))
    })
}

fn join_with(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: ExpectedConfig<'_>,
    fetch: impl FnOnce(&Source) -> crate::Result<Fetched>,
) -> crate::Result<JoinOutcome> {
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
    let announce = |identity: &Identity| {
        (options.say)(&format!(
            "this machine: {host}, code {}, key {}",
            join_code(&identity.recipient()),
            identity.recipient().to_bech32()
        ));
    };
    let identity = match expected {
        ExpectedConfig::Any => {
            let identity = machine_identity(paths)?;
            announce(&identity);
            Some(identity)
        }
        ExpectedConfig::Read(_) => None,
    };
    let bytes = match fetch(&source)? {
        Fetched::Body(bytes) => bytes,
        Fetched::Missing => return Err(Error::new(format!("{source_display}: no vault there"))),
        Fetched::NoNewer(_) => return Err(Error::new("the source answered with no vault")),
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
            host: host.clone(),
        };
        plain_entries(&config, path)?;
        let identity = {
            let _lock = StateLock::take(paths)?;
            expected.check(paths)?;
            let identity = match identity {
                Some(identity) => identity,
                None => machine_identity(paths)?,
            };
            let mut state = load_state(paths)?;
            state.seq = 0;
            state.digest = None;
            state.membership = Stored::Plain;
            prepare_materialization(paths, &mut state)?;
            save_config(paths, &config)?;
            identity
        };
        if matches!(expected, ExpectedConfig::Read(_)) {
            announce(&identity);
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
    let followed = previous
        .as_ref()
        .filter(|previous| same_place && previous.signer == Some(pin))
        .map(|previous| previous.followed.clone())
        .unwrap_or_default();
    let (parsed, checked, _) = envelope::verify_for_reader(&bytes, &name, &pin, &followed, None)?;
    let (pinned, followed) = match checked {
        Pin::Same => (pin, followed),
        Pin::Rotated(new) | Pin::Replaced(new) => (new, parsed.reached_from()),
    };
    let config = Config {
        source: source.clone(),
        name: name.clone(),
        signer: Some(pinned),
        followed,
        host: host.clone(),
    };
    let identity = {
        let _lock = StateLock::take(paths)?;
        expected.check(paths)?;
        let identity = match identity {
            Some(identity) => identity,
            None => machine_identity(paths)?,
        };
        let replaced = matches!(checked, Pin::Replaced(_));
        let mut state = load_state(paths)?;
        if !same_place || stored != Some(pin) || replaced {
            state.seq = 0;
            state.digest = None;
            state.membership = Stored::None;
            state.proof = None;
            state.requested = in_flight();
            prepare_materialization(paths, &mut state)?;
        } else if previous.as_ref() != Some(&config) {
            if previous
                .as_ref()
                .is_some_and(|previous| previous.host != host)
            {
                state.proof = None;
                state.requested = in_flight();
            }
            prepare_materialization(paths, &mut state)?;
        }
        save_config(paths, &config)?;
        identity
    };
    if matches!(expected, ExpectedConfig::Read(_)) {
        announce(&identity);
    }
    let recipient_text = identity.recipient().to_bech32();
    let code = join_code(&identity.recipient());
    let synced = sync(paths, None).inspect_err(|_| {
        let _ = change_request(paths, &config, |state| {
            state.requested = 0;
            true
        });
    });
    if let SyncOutcome::Applied { seq, report } = synced? {
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
            request_join(paths, &config, proof, Asker::Join)?;
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

fn already_in(paths: &Paths) -> crate::Result<Option<JoinOutcome>> {
    let state = load_state(paths)?;
    Ok(
        (state.membership == Stored::In).then(|| JoinOutcome::Joined {
            seq: state.seq,
            names: state.manifest.files.keys().cloned().collect(),
        }),
    )
}

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
    pub const fn seq(&self) -> u64 {
        self.seq
    }
}

fn seq_of(bytes: &[u8]) -> Option<u64> {
    envelope::parse(bytes).ok().map(|parsed| parsed.seq)
}

/// Open as an admin: fetch or restore the latest verified envelope and decrypt its admin locker.
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
        Fetched::Missing | Fetched::NoNewer(_) => None,
    };
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)?
        .map(|bytes| bytes.as_slice().to_vec())
        .filter(|bytes| verifies(bytes));
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
    let mut put_back: Option<Option<Vec<u8>>> = None;
    let bytes = match (served, cached) {
        (Some(served), Some(_))
            if matches!(config.source, Source::Path(_))
                && !verifies(&served)
                && !foreign(&served) =>
        {
            let place = match &config.source {
                Source::Path(path) => path.display().to_string(),
                Source::Url(url) => transport::describe(url),
            };
            let served = envelope::parse(&served)?;
            let aside = "move the file aside and the next admin command puts this machine's \
                         copy back";
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
                config.name,
                join_code(&identity.recipient())
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
    /// Compiled-in keys required in the next release after an ALab vault key rotation.
    pub alab_release: Option<AlabRelease>,
    /// Whether this is the first version of the vault to carry two or more handoff pairs.
    pub first_long_chain: bool,
    /// Whether the join password was automatically rotated after removing a machine or admin.
    pub password_renewed: bool,
    /// Whether a machine removed from an open vault was added to `code-only`.
    pub kept_out: bool,
    /// Whether the vault is served at a URL, where an open vault's approver lets in any new
    /// key that asks (a vault at a path lets in none without an admin).
    pub queued: bool,
    /// Waiting join requests added to `code-only` when opening the vault.
    pub held: Vec<Request>,
    /// Why this machine's own sync after the publish failed, if it did. The publish stands.
    pub local: Option<String>,
}

/// Compiled-in keys required after an ALab vault rotation, in padded base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlabRelease {
    /// [`crate::ALAB_SIGNER`]'s new value: the key that signs now.
    pub signer: String,
    /// [`crate::ALAB_RETIRED`]'s new value, oldest first.
    pub retired: Vec<String>,
}

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

/// Publish `doc` as the next version, rotating keys or join credentials when members leave.
pub fn admin_publish(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<Published> {
    publish_as(paths, admin, doc, false)
}

/// Publish `doc` signed by a newly rotated vault key endorsed by the current key.
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
        queued: matches!(admin.config.source, Source::Url(_)),
        held,
        local,
    })
}

fn configured_admin(paths: &Paths, expected: &Config) -> crate::Result<StateLock> {
    let lock = StateLock::take(paths)?;
    if config(paths)?.as_ref() != Some(expected) {
        return Err(Error::new(
            "the configured vault changed while it was open; open it again before publishing",
        ));
    }
    Ok(lock)
}

fn let_go(old: &Document, doc: &Document) -> Vec<String> {
    let kept: Vec<String> = doc.machines().into_iter().map(|m| m.recipient).collect();
    old.machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .filter(|recipient| !kept.contains(recipient))
        .collect()
}

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
    /// Why this machine's own sync after the publish failed, if it did.
    pub local: Option<String>,
}

/// Create a new vault at `source` with this machine as its initial admin.
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
    let local = sync(paths, None).err().map(|error| error.to_string());
    Ok(Created {
        name,
        signer: crate::b64::encode_padded(&signer),
        fingerprint: envelope::fingerprint(&signer),
        imported,
        local,
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

/// Pending join requests at the configured URL, excluding machines already in `doc`.
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

/// Return an unused valid machine name in `doc` based on `wanted`, avoiding join-code shapes.
pub fn free_name(doc: &Document, wanted: &str) -> String {
    let taken: Vec<String> = doc.machines().into_iter().map(|m| m.name).collect();
    let takeable = |name: &str| text::valid_machine_name(name) && !transport::valid_code(name);
    let base = if takeable(wanted) { wanted } else { "machine" };
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2u64..)
        .map(|n| {
            let suffix = format!("-{n}");
            let room = 63usize.saturating_sub(suffix.len());
            let stem = match base.get(..room) {
                Some(cut) if base.len() > room => cut.trim_end_matches('-'),
                _ => base,
            };
            format!("{stem}{suffix}")
        })
        .find(|name| !taken.contains(name) && takeable(name))
        .unwrap_or_else(|| "machine".to_string())
}

/// Resolve `word` (a join code or machine name) to a machine name in `doc`.
pub fn machine_named(doc: &Document, word: &str) -> crate::Result<String> {
    let code = word.to_ascii_lowercase();
    if !transport::valid_code(&code) {
        return Ok(word.to_string());
    }
    let machines = doc.machines();
    let by_code = machines.iter().find(|machine| {
        Recipient::parse(&machine.recipient).is_ok_and(|key| join_code(&key) == code)
    });
    let by_name = machines.iter().find(|machine| machine.name == code);
    match (by_code, by_name) {
        (Some(keyed), Some(named)) if keyed.name != named.name => Err(Error::new(format!(
            "{code} is the code of machine {} and the name of another machine (key {}): name the one you mean by its name, or rename the other in aterm secrets edit",
            keyed.name, named.recipient
        ))),
        (Some(machine), _) => Ok(machine.name.clone()),
        (None, Some(named)) => Err(Error::new(format!(
            "no machine of the vault has code {code}; a machine is named {code} (key {}{}), and its own code names it",
            named.recipient,
            Recipient::parse(&named.recipient)
                .map(|key| format!(", code {}", join_code(&key)))
                .unwrap_or_default()
        ))),
        (None, None) => Err(Error::new(format!(
            "no machine of the vault has code {code}"
        ))),
    }
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

/// Merge membership, admin, and `[join]` changes made between `base` and `current` into `edited`.
pub fn keep_admitted(
    base: &Document,
    current: &Document,
    edited: &mut Document,
) -> crate::Result<Vec<String>> {
    let refused = |error: text::TextError| Error::new(error.to_string());
    let known: Vec<String> = base.machines().into_iter().map(|m| m.recipient).collect();
    let now = current.machines();
    for gone in base.machines() {
        if now
            .iter()
            .any(|machine| machine.recipient == gone.recipient)
        {
            continue;
        }
        let Some(held) = edited
            .machines()
            .into_iter()
            .find(|machine| machine.recipient == gone.recipient)
        else {
            continue;
        };
        if let Some(other) = now
            .iter()
            .find(|machine| machine.name == gone.name && !known.contains(&machine.recipient))
        {
            return Err(Error::new(format!(
                "{} has another key since your edit began ({}): put it on {}'s line to give it what your text gives {}, or delete the machine, then save again",
                gone.name, other.recipient, held.name, held.name
            )));
        }
        edited.remove_machine(&held.name, true).map_err(refused)?;
    }
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
            .map_err(refused)?;
        kept.push(name);
    }
    let admins = |doc: &Document| -> Vec<String> {
        let names = doc.admins();
        doc.machines()
            .into_iter()
            .filter(|machine| names.contains(&machine.name))
            .map(|machine| machine.recipient)
            .collect()
    };
    let (were, are) = (admins(base), admins(current));
    for machine in edited.machines() {
        if are.contains(&machine.recipient) && !were.contains(&machine.recipient) {
            edited.add_admin(&machine.name).map_err(refused)?;
        }
    }
    for machine in edited.machines() {
        if were.contains(&machine.recipient) && !are.contains(&machine.recipient) {
            edited.remove_admin(&machine.name).map_err(refused)?;
        }
    }
    if !same_join(base, current) && same_join(base, edited) {
        let password = current.join_password();
        edited
            .set_join_password(password.as_ref().map(|password| password.as_slice()))
            .map_err(refused)?;
        edited.set_open(current.open()).map_err(refused)?;
    }
    let before = base.code_only();
    let members: Vec<String> = edited.machines().into_iter().map(|m| m.recipient).collect();
    for key in current.code_only() {
        if !before.contains(&key) && !members.contains(&key) {
            edited.add_code_only(&key).map_err(refused)?;
        }
    }
    Ok(kept)
}

fn same_join(a: &Document, b: &Document) -> bool {
    a.open() == b.open()
        && match (a.join_password(), b.join_password()) {
            (Some(x), Some(y)) => x.as_slice() == y.as_slice(),
            (x, y) => x.is_none() && y.is_none(),
        }
}

struct HeldLock(std::fs::File);

impl HeldLock {
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

/// An exclusively created `0700` edit directory, locked while it lives.
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

    /// Discard `file` if it still holds `expected`, then remove the lock and empty directory.
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

    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    #[test]
    fn a_wait_after_an_error_ends_once_the_network_changes() {
        let mut slept = Duration::ZERO;
        let mut looks = 0;
        let moved = nap_unless_moved(
            LONGEST_ERROR_RETRY,
            &mut || {
                looks += 1;
                looks == 3
            },
            &mut |step| slept += step,
        );
        assert!(moved);
        assert_eq!(slept, MOVED_STEP * 3);

        let mut slept = Duration::ZERO;
        assert!(!nap_unless_moved(
            LONGEST_ERROR_RETRY,
            &mut || false,
            &mut |step| slept += step
        ));
        assert_eq!(slept, LONGEST_ERROR_RETRY);
        let mut steps = Vec::new();
        nap_unless_moved(Duration::from_secs(2), &mut || false, &mut |step| {
            steps.push(step)
        });
        assert_eq!(steps, [Duration::from_secs(2)]);
    }

    #[test]
    fn a_pass_that_errs_is_tried_again_within_a_minute() {
        let mut backoff = Duration::from_secs(5);
        let waits: Vec<u64> = (0..8)
            .map(|_| {
                let wait = backoff;
                backoff = retry_after(backoff, false);
                wait.as_secs()
            })
            .collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60, 60]);
        assert_eq!(retry_after(backoff, true), Duration::from_secs(5));
    }

    #[test]
    fn a_look_tells_an_answering_vault_from_none_and_makes_nothing() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let reader = Paths::for_home(&root.path().join("reader"));

        let plain = root.path().join("team.secrets");
        let missing = Source::Path(root.path().join("unplugged").join("team.vault"));
        assert!(
            matches!(look(&missing, HANG_DETECTOR, Duration::ZERO, &|_| {}), Look::NoVault(why) if why.ends_with("team.vault: no vault there"))
        );
        fsx::write_atomic(&plain, b"token=inert-token\n", 0o600).expect("plain vault");
        assert_eq!(
            look(&Source::Path(plain), HANG_DETECTOR, Duration::ZERO, &|_| {}),
            Look::Answers
        );

        let running = serve(&root.path().join("store"), &bound());
        let url = format!("http://{}/v1/alab", running.addr);
        let source = Source::Url(url.clone());
        assert_eq!(
            look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {}),
            Look::NoVault(format!("{url}: no vault there"))
        );
        let admin = Paths::for_home(&root.path().join("ma"));
        create(&admin, &url, None, Some("ma")).expect("create");
        assert_eq!(
            look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {}),
            Look::Answers
        );
        running.stop();
        assert!(
            matches!(look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {}), Look::Unreached(why) if why.starts_with(&format!("fetching {url}"))),
            "{:?}",
            look(&source, HANG_DETECTOR, Duration::ZERO, &|_| {})
        );
        assert!(!reader.dir.exists(), "a look makes nothing");
    }

    struct ScriptedTries {
        answers: std::collections::VecDeque<Result<transport::Answer, transport::Unheard>>,
        now: Instant,
        took: Duration,
        gets: usize,
        pauses: Vec<Duration>,
    }

    impl ScriptedTries {
        fn new(
            took: Duration,
            answers: impl IntoIterator<Item = Result<transport::Answer, transport::Unheard>>,
        ) -> Self {
            Self {
                answers: answers.into_iter().collect(),
                now: Instant::now(),
                took,
                gets: 0,
                pauses: Vec::new(),
            }
        }
    }

    impl Tries for ScriptedTries {
        fn get(
            &mut self,
            url: &str,
            wait: Duration,
        ) -> Result<transport::Answer, transport::Unheard> {
            assert_eq!(url, "http://alab-secrets.local:7484/v1/alab");
            assert_eq!(
                wait, HANG_DETECTOR,
                "every try waits as long as the look's wait"
            );
            self.gets += 1;
            self.now += self.took;
            self.answers
                .pop_front()
                .expect("a GET the script has no answer for")
        }

        fn now(&mut self) -> Instant {
            self.now
        }

        fn pause(&mut self, pause: Duration) {
            self.pauses.push(pause);
            self.now += pause;
        }
    }

    fn unheard(why: &str, held: bool) -> Result<transport::Answer, transport::Unheard> {
        Err(transport::Unheard {
            why: Error::new(why),
            held,
        })
    }

    fn served_vault() -> Result<transport::Answer, transport::Unheard> {
        Ok(transport::Answer::Fetched(Fetched::Body(
            envelope::MAGIC.as_bytes().to_vec(),
        )))
    }

    fn alab() -> Source {
        Source::Url("http://alab-secrets.local:7484/v1/alab".into())
    }

    #[test]
    fn a_look_held_for_the_local_network_answer_hears_the_vault_after_it() {
        let said = std::cell::RefCell::new(Vec::new());
        let say = |why: &str| said.borrow_mut().push(why.to_owned());
        let patience = Duration::from_secs(180);
        let mut tries = ScriptedTries::new(
            transport::LOOK_CONNECT,
            [
                unheard("dns: no answer within 8000ms", true),
                unheard("dns: no answer within 7999ms", true),
                served_vault(),
            ],
        );
        assert_eq!(
            look_with(&alab(), HANG_DETECTOR, patience, &say, &mut tries),
            Look::Answers
        );
        assert_eq!(tries.gets, 3);
        assert_eq!(tries.pauses, [LOOK_PAUSE, LOOK_PAUSE]);
        assert_eq!(*said.borrow(), ["dns: no answer within 8000ms"]);

        said.borrow_mut().clear();
        let mut tries = ScriptedTries::new(
            transport::LOOK_CONNECT,
            [
                unheard("dns: no answer within 8000ms", true),
                served_vault(),
            ],
        );
        assert_eq!(
            look_with(&alab(), HANG_DETECTOR, Duration::ZERO, &say, &mut tries),
            Look::Held("dns: no answer within 8000ms".into())
        );
        assert_eq!((tries.gets, tries.pauses.len()), (1, 0));
        assert!(
            said.borrow().is_empty(),
            "a look that gives up says nothing held"
        );

        let mut tries = ScriptedTries::new(
            transport::LOOK_CONNECT,
            [
                unheard("dns: no answer within 8000ms", true),
                unheard("fetching …: Connection refused", false),
                served_vault(),
            ],
        );
        assert_eq!(
            look_with(&alab(), HANG_DETECTOR, patience, &say, &mut tries),
            Look::Unreached("fetching …: Connection refused".into()),
            "a held look ends at the first failure that is not held"
        );
        assert_eq!((tries.gets, tries.pauses.len()), (2, 1));
        assert_eq!(*said.borrow(), ["dns: no answer within 8000ms"]);

        said.borrow_mut().clear();
        let mut tries = ScriptedTries::new(
            Duration::from_secs(5),
            [
                unheard(
                    "dns: alab-secrets.local: nodename nor servname provided",
                    false,
                ),
                served_vault(),
            ],
        );
        assert_eq!(
            look_with(&alab(), HANG_DETECTOR, patience, &say, &mut tries),
            Look::Unreached("dns: alab-secrets.local: nodename nor servname provided".into())
        );
        assert_eq!((tries.gets, tries.pauses.len()), (1, 0));
        assert!(said.borrow().is_empty());
    }

    #[test]
    fn a_held_look_ends_with_its_patience_or_at_a_foreign_answer() {
        let patience = Duration::from_secs(60);
        let reasons: Vec<String> = (1..=6).map(|n| format!("try {n}")).collect();
        let mut tries = ScriptedTries::new(
            transport::LOOK_CONNECT,
            reasons.iter().map(|why| unheard(why, true)),
        );
        assert_eq!(
            look_with(&alab(), HANG_DETECTOR, patience, &|_| {}, &mut tries),
            Look::Held("try 6".into())
        );
        assert_eq!(tries.gets, 6, "tries at 0, 10, 20, 30, 40 and 50 s");
        assert_eq!(tries.pauses, [LOOK_PAUSE; 5]);

        let mut tries = ScriptedTries::new(
            transport::LOOK_CONNECT,
            [
                unheard("dns: no answer within 8000ms", true),
                Ok(transport::Answer::Foreign("a login page".into())),
            ],
        );
        assert_eq!(
            look_with(&alab(), HANG_DETECTOR, patience, &|_| {}, &mut tries),
            Look::NoVault("a login page".into())
        );
        assert_eq!(tries.gets, 2);
    }

    #[test]
    fn a_kept_config_copied_back_reads_the_vault_this_machine_read_before() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let first = root.path().join("first.secrets");
        let second = root.path().join("second.secrets");
        fsx::write_atomic(&first, b"token=first-token\n", 0o600).expect("first vault");
        fsx::write_atomic(&second, b"token=second-token\n", 0o600).expect("second vault");
        let reader = Paths::for_home(&root.path().join("reader"));
        join_pinned(&reader, first.to_str().unwrap(), "reader");
        let kept = reader.dir.join("before-join-1800000000");
        keep_config(&reader, &kept).expect("kept");
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&kept), 0o700);
        assert_eq!(mode(&kept.join("source.toml")), 0o600);
        assert_eq!(mode(&kept.join("state.toml")), 0o600);
        let kept_source = std::fs::read(kept.join("source.toml")).unwrap();
        let kept_state = std::fs::read(kept.join("state.toml")).unwrap();

        join_pinned(&reader, second.to_str().unwrap(), "reader");
        let token = materialize::target_path(&reader.home, "token").unwrap();
        assert_eq!(std::fs::read(&token).unwrap(), b"second-token");
        assert_eq!(
            pinned(&reader).name,
            "second",
            "the control: still the other"
        );

        keep_config(&reader, &kept).expect_err("the earlier snapshot owns this name");
        assert_eq!(
            std::fs::read(kept.join("source.toml")).unwrap(),
            kept_source
        );
        assert_eq!(std::fs::read(kept.join("state.toml")).unwrap(), kept_state);

        let partial = root.path().join("partial-kept");
        fsx::ensure_private_dir(&partial).unwrap();
        fsx::write_atomic(&partial.join("source.toml"), b"partial copy", 0o600).unwrap();
        keep_config(&reader, &partial).expect_err("a partial snapshot stays as it is");
        assert_eq!(
            std::fs::read(partial.join("source.toml")).unwrap(),
            b"partial copy"
        );
        assert!(!partial.join("state.toml").exists());

        let busy = root.path().join("busy-kept");
        {
            let _writer = StateLock::take(&reader).expect("the writer owns the state");
            keep_config(&reader, &busy).expect_err("the state is being changed");
            assert!(!busy.exists(), "no snapshot was admitted");
        }
        keep_config(&reader, &busy).expect("the coherent current state can be kept");
        assert_eq!(
            std::fs::read(busy.join("source.toml")).unwrap(),
            std::fs::read(reader.source_file()).unwrap()
        );
        assert_eq!(
            std::fs::read(busy.join("state.toml")).unwrap(),
            std::fs::read(reader.state_file()).unwrap()
        );

        let unlocked = Paths::for_home(&root.path().join("unlocked"));
        fsx::ensure_private_dir(&unlocked.dir).unwrap();
        fsx::write_atomic(&unlocked.source_file(), &kept_source, 0o600).unwrap();
        fsx::write_atomic(&unlocked.state_file(), &kept_state, 0o600).unwrap();
        let unavailable = root.path().join("unavailable-kept");
        keep_config(&unlocked, &unavailable).expect_err("existing state needs its lock");
        assert!(!unavailable.exists());
        assert_eq!(std::fs::read(unlocked.source_file()).unwrap(), kept_source);
        assert_eq!(std::fs::read(unlocked.state_file()).unwrap(), kept_state);

        for name in ["source.toml", "state.toml"] {
            std::fs::copy(kept.join(name), reader.dir.join(name)).expect("copied back");
        }
        sync(&reader, None).expect("sync");
        assert_eq!(pinned(&reader).name, "first");
        assert_eq!(std::fs::read(&token).unwrap(), b"first-token");
        assert!(materialization_ready(&reader).unwrap());

        let fresh = Paths::for_home(&root.path().join("fresh"));
        let into = root.path().join("fresh-kept");
        keep_config(&fresh, &into).expect("nothing to keep");
        assert_eq!(std::fs::read_dir(&into).unwrap().count(), 0);
        assert!(!fresh.dir.exists());

        let signed = root.path().join("signed.vault");
        let admin = Paths::for_home(&root.path().join("admin"));
        let created = create(&admin, signed.to_str().unwrap(), None, Some("admin")).unwrap();
        let targets = [
            first.to_string_lossy().into_owned(),
            format!("{}#{}", signed.display(), created.signer),
        ];
        let config_bytes = |dir: &Path| {
            ["source.toml", "state.toml"].map(|name| std::fs::read(dir.join(name)).unwrap())
        };
        for (kind, target) in targets.into_iter().enumerate() {
            for had_config in [false, true] {
                let reader =
                    Paths::for_home(&root.path().join(format!("raced-{kind}-{had_config}")));
                if had_config {
                    join_pinned(&reader, first.to_str().unwrap(), "reader");
                }
                let expected = config(&reader).unwrap();
                let kept = root.path().join(format!("raced-kept-{kind}-{had_config}"));
                if had_config {
                    keep_config_if_unchanged(&reader, &kept, expected.as_ref()).unwrap();
                }
                let kept_before = had_config.then(|| config_bytes(&kept));
                let options = JoinOptions {
                    source: target.clone(),
                    host: Some("reader".into()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: &|| None,
                    requested: &|| {},
                    say: &|_| {},
                };
                if !had_config {
                    let pristine = Paths::for_home(&root.path().join(format!("pristine-{kind}")));
                    join_if_unchanged(&pristine, &options, None).unwrap();
                    assert!(pristine.identity_file().is_file());
                    assert_eq!(
                        config(&pristine).unwrap().unwrap().source,
                        transport::parse_source(&target, &pristine.home).unwrap().0
                    );
                }
                let bytes = || {
                    [
                        reader.source_file(),
                        reader.state_file(),
                        reader.identity_file(),
                        materialize::target_path(&reader.home, "token").unwrap(),
                    ]
                    .map(|path| std::fs::read(path).unwrap())
                };
                let terminal = std::cell::RefCell::new(None);
                let refused = join_with(
                    &reader,
                    &options,
                    ExpectedConfig::Read(expected.as_ref()),
                    |source| {
                        let fetched = transport::fetch(source, None, Duration::from_secs(30))?;
                        join_pinned(&reader, second.to_str().unwrap(), "reader");
                        *terminal.borrow_mut() = Some(bytes());
                        Ok(fetched)
                    },
                );
                assert!(
                    refused.is_err(),
                    "the terminal's newer vault owns the state"
                );
                assert_eq!(bytes(), terminal.into_inner().unwrap());
                assert_eq!(config(&reader).unwrap().unwrap().name, "second");

                let missed = root.path().join(format!("missed-{kind}-{had_config}"));
                keep_config_if_unchanged(&reader, &missed, expected.as_ref())
                    .expect_err("the admitted vault cannot name a snapshot of the newer one");
                assert!(!missed.exists());
                if let Some(before) = kept_before {
                    assert_eq!(config_bytes(&kept), before);
                }

                let current = config(&reader).unwrap();
                let admitted = root.path().join(format!("admitted-{kind}-{had_config}"));
                keep_config_if_unchanged(&reader, &admitted, current.as_ref()).unwrap();
                assert_eq!(config_bytes(&admitted), config_bytes(&reader.dir));
                join_if_unchanged(&reader, &options, current.as_ref()).unwrap();
                let wanted = config(&reader).unwrap().unwrap();
                assert_eq!(
                    wanted.source,
                    transport::parse_source(&target, &reader.home).unwrap().0
                );

                join_with(&reader, &options, ExpectedConfig::Any, |source| {
                    let fetched = transport::fetch(source, None, Duration::from_secs(30))?;
                    join_pinned(&reader, second.to_str().unwrap(), "reader");
                    Ok(fetched)
                })
                .unwrap();
                assert_eq!(config(&reader).unwrap().unwrap(), wanted);
            }
        }
    }

    #[test]
    fn a_vault_too_large_to_seal_is_refused_saying_why() {
        let key = Identity::generate().expect("a key").recipient().to_bech32();
        let mut doc = Document::new_vault("m1", &key);
        let value = vec![b'x'; text::MAX_VALUE];
        for n in 1..=5 {
            doc.set_entry(&Audience::Everyone, &format!("a{n}"), &value)
                .expect("set");
        }
        seal("alab", 1, &[7u8; 32], &doc, &[]).expect("five values of 1 MiB fit");
        doc.set_entry(&Audience::Everyone, "a6", &value)
            .expect("the text holds a sixth");
        let error = seal("alab", 1, &[7u8; 32], &doc, &[]).expect_err("too large");
        assert!(
            error.0.contains("too large to publish") && error.0.contains("entries fit"),
            "{error}"
        );
    }

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
        assert_eq!(pause_after(&url, &applied(0), &mut failing), Duration::ZERO);
        assert_eq!(pause_after(&url, &applied(2), &mut failing), FIRST_RETRY);
        pause_after(&url, &applied(2), &mut failing);
        let unchanged = SyncOutcome::Unchanged { seq: 2 };
        assert_eq!(pause_after(&url, &unchanged, &mut failing).as_secs(), 10);
        assert_eq!(pause_after(&url, &applied(1), &mut failing), FIRST_RETRY);
        let path = Source::Path(PathBuf::from("/tmp/vault"));
        assert_eq!(pause_after(&path, &applied(1), &mut failing).as_secs(), 60);
    }

    fn bound() -> std::net::TcpListener {
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind")
    }

    fn serve(store: &Path, socket: &std::net::TcpListener) -> crate::server::Running {
        crate::server::start(
            socket.try_clone().expect("a copy of the socket"),
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

    struct Unserved {
        stop: std::sync::Arc<AtomicBool>,
        addr: std::net::SocketAddr,
        closer: std::thread::JoinHandle<()>,
    }

    impl Unserved {
        fn at(socket: &std::net::TcpListener) -> Unserved {
            let listener = socket.try_clone().expect("a copy of the socket");
            let addr = listener.local_addr().expect("its address");
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let seen = std::sync::Arc::clone(&stop);
            let closer = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    drop(stream);
                    if seen.load(Ordering::SeqCst) {
                        return;
                    }
                }
            });
            Unserved { stop, addr, closer }
        }

        fn end(self) {
            self.stop.store(true, Ordering::SeqCst);
            std::net::TcpStream::connect(self.addr).expect("the closer is woken");
            self.closer.join().expect("the closer ends");
        }
    }

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

    /// A file the vault wrote whose directory then became a symlink (a dotfiles setup's
    /// `~/.config/gh`) is held without a look, so readiness does not judge it through the link,
    /// while the same file missing from a real directory is incomplete.
    #[test]
    fn readiness_does_not_judge_a_held_file_behind_a_symlinked_directory() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.secrets");
        let text = b"~/.config/gh/hosts.yml<<END if-absent\ngithub.com: {}\nEND\n";
        fsx::write_atomic(&source, text, 0o600).unwrap();
        let reader = Paths::for_home(&root.path().join("reader"));
        join_pinned(&reader, source.to_str().unwrap(), "reader");
        assert!(materialization_ready(&reader).unwrap(), "filled");
        let gh = reader.home.join(".config/gh");
        let dotfiles = root.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        std::fs::remove_dir_all(&gh).unwrap();
        std::os::unix::fs::symlink(&dotfiles, &gh).unwrap();
        sync(&reader, None).expect("synced");
        assert!(
            materialization_ready(&reader).unwrap(),
            "held behind the link, not judged through it"
        );
        std::fs::remove_file(&gh).unwrap();
        std::fs::create_dir(&gh).unwrap();
        assert!(
            !materialization_ready(&reader).unwrap(),
            "missing from a real directory"
        );
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
    fn a_key_the_chain_names_before_a_link_that_does_not_verify_is_no_predecessor() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let source = root.path().join("team.vault");
        let admin = Paths::for_home(&root.path().join("admin"));
        create(&admin, source.to_str().expect("utf-8"), None, Some("admin"))
            .expect("create scratch vault");
        let opened = admin_open(&admin).expect("open");
        let signer = envelope::signer_public(&opened.seed).expect("public");
        let named = [239; 32];
        let key = envelope::signer_public(&named).expect("public");
        let signed = |seq: u64, handoff: Vec<envelope::Link>, seed: &[u8; 32]| {
            let unsigned = Unsigned {
                name: "team".to_string(),
                seq,
                time: 0,
                handoff,
                lockers: Vec::new(),
            };
            envelope::sign_unchecked(&unsigned, seed).expect("signed")
        };
        let cached = |endorsement: [u8; 64]| {
            let bytes = signed(opened.seq() + 1, vec![(key, endorsement)], &opened.seed);
            std::fs::write(admin.cache_file(), bytes).expect("cached");
            pinned(&admin)
        };
        let config = cached([7; 64]);
        assert_eq!((config.signer, config.followed.len()), (Some(signer), 0));
        assert!(!pin_admitted(&config, Some(key)));
        let config = cached(envelope::endorse(&named, "team", &signer).expect("endorsed"));
        assert_eq!(config.followed, vec![key]);
        assert!(pin_admitted(&config, Some(key)));

        let next_seed = [241; 32];
        let next = envelope::signer_public(&next_seed).expect("public");
        let handoff = vec![
            (key, [7; 64]),
            (
                signer,
                envelope::endorse(&opened.seed, "team", &next).expect("endorsed"),
            ),
        ];
        std::fs::write(&source, signed(opened.seq() + 2, handoff, &next_seed)).expect("written");
        sync(&admin, None).expect("synced");
        let config = pinned(&admin);
        assert_eq!(
            (config.signer, config.followed.clone()),
            (Some(next), vec![signer])
        );
        assert!(!pin_admitted(&config, Some(key)));
        assert!(pin_admitted(&config, Some(signer)));
        let reader = Paths::for_home(&root.path().join("reader"));
        let pin = crate::b64::encode_padded(&signer);
        join_pinned(&reader, &format!("{}#{pin}", source.display()), "reader");
        let config = pinned(&reader);
        assert_eq!(
            (config.signer, config.followed.clone()),
            (Some(next), vec![signer])
        );
        assert!(!pin_admitted(&config, Some(key)));
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

        std::fs::remove_file(&token).unwrap();
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "note", b"inert-note")
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
        std::fs::remove_file(&token).unwrap();
        assert!(repair_due(&admin, &mut repaired));

        let store = root.path().join("store");
        let socket = bound();
        let server = serve(&store, &socket);
        let url = format!("http://{}/v1/served", server.addr);
        create(&admin, &url, None, Some("admin")).unwrap();
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-served")
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
        assert!(materialization_ready(&admin).unwrap());
        std::fs::remove_file(&token).unwrap();
        server.stop();
        let gone = Unserved::at(&socket);
        assert!(subscriber_pass(&admin, &mut repaired).is_err());
        gone.end();
        let server = serve(&store, &socket);
        let (_, outcome) = subscriber_pass(&admin, &mut repaired).expect("the server is back");
        assert!(
            matches!(outcome, SyncOutcome::Applied { .. }),
            "{outcome:?}"
        );
        assert!(materialization_ready(&admin).unwrap());
        assert_eq!(std::fs::read(&token).unwrap(), b"inert-served");
        server.stop();
    }

    #[test]
    fn materialization_capture_holds_local_reads_and_fences_the_version() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("team.secrets");
        fsx::write_atomic(&source, b"token=inert-first-token\n", 0o600).unwrap();
        let paths = Paths::for_home(&root.path().join("reader"));
        join_pinned(&paths, source.to_str().unwrap(), "reader");
        let configured = config(&paths).unwrap().unwrap();
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.lock_file())
            .unwrap();
        let try_writer = || {
            #[expect(
                clippy::disallowed_methods,
                reason = "the test observes the materialization reader lease without blocking"
            )]
            writer.try_lock()
        };
        let captured = with_materialization_ready(&paths, &configured, || {
            assert!(matches!(
                try_writer(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            Ok(fsx::read_private(&paths.home.join(".secrets/token"), 1024)?.unwrap())
        })
        .unwrap()
        .unwrap();
        assert_eq!(captured.inputs.as_slice(), b"inert-first-token");
        assert!(captured.snapshot.is_current(&paths).unwrap());
        try_writer().unwrap();
        writer.unlock().unwrap();

        let mut state = load_state(&paths).unwrap();
        state.heard += 1;
        state.requested += 1;
        state.proof = Some("inert-proof".into());
        save_state(&paths, &state).unwrap();
        assert!(
            captured.snapshot.is_current(&paths).unwrap(),
            "not a new version"
        );

        fsx::write_atomic(&source, b"token=inert-next-token\n", 0o600).unwrap();
        sync(&paths, None).unwrap();
        assert!(materialization_ready(&paths).unwrap());
        assert!(!captured.snapshot.is_current(&paths).unwrap());
        let current = config(&paths).unwrap().unwrap();
        let mut mismatch = current.clone();
        mismatch.name = "another".into();
        assert!(
            with_materialization_ready::<()>(&paths, &mismatch, || {
                panic!("another source must not read inputs")
            })
            .unwrap()
            .is_none()
        );
        let writer = StateLock::take(&paths).unwrap();
        assert!(
            with_materialization_ready::<()>(&paths, &current, || {
                panic!("an active sync must not read inputs")
            })
            .unwrap()
            .is_none()
        );
        drop(writer);
        assert!(
            with_materialization_ready(&paths, &current, || {
                Err::<(), _>(Error::new("injected local read failure"))
            })
            .is_err()
        );
        assert!(
            StateLock::observe(&paths).unwrap().is_some(),
            "failure releases the lease"
        );
    }

    #[test]
    fn materialization_marks_are_local_redacted_and_unknown_while_a_writer_runs() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&scratch.path().join("home"));
        assert!(materialization_mark(&paths).unwrap().is_none());
        assert!(
            !paths.home.exists(),
            "an observation creates no local state"
        );
        let source = scratch.path().join("scratch.secrets");
        fsx::write_atomic(&source, b"token=inert-mark-test-value\n", 0o600).unwrap();
        join_pinned(&paths, source.to_str().unwrap(), "scratch");
        let known = materialization_mark(&paths).unwrap().unwrap();
        assert_eq!(
            MaterializationMark::from_cache_key(&known.cache_key()),
            Some(known)
        );
        let debug = format!("{known:?}");
        assert!(!debug.contains(&known.cache_key()));
        assert!(!debug.contains("inert-mark-test-value"));
        let mut heartbeat = load_state(&paths).unwrap();
        heartbeat.heard += 1;
        heartbeat.requested += 1;
        heartbeat.proof = Some("inert-join-proof".into());
        save_state(&paths, &heartbeat).unwrap();
        let state = std::fs::read(paths.state_file()).unwrap();
        assert_eq!(materialization_mark(&paths).unwrap(), Some(known));
        {
            let _writer = StateLock::take(&paths).unwrap();
            assert!(materialization_mark(&paths).unwrap().is_none());
        }
        assert_eq!(materialization_mark(&paths).unwrap(), Some(known));
        assert_eq!(std::fs::read(paths.state_file()).unwrap(), state);

        fsx::write_atomic(&paths.state_file(), b"invalid = [", 0o600).unwrap();
        assert!(materialization_mark(&paths).is_err());
        assert_eq!(std::fs::read(paths.state_file()).unwrap(), b"invalid = [");
        fsx::write_atomic(&paths.state_file(), &state, 0o600).unwrap();
        assert_eq!(materialization_mark(&paths).unwrap(), Some(known));
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
        let chain = vec![(key(1), [0u8; 64]), (key(2), [0u8; 64])];
        let knows = |keys: Vec<[u8; 32]>, current: [u8; 32]| Retired {
            keys,
            current: Some(current),
        };
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
        assert_eq!(
            alab_release(&knows(vec![key(5)], key(6)), &chain, &key(3)),
            None
        );
        assert_eq!(alab_release(&Retired::default(), &chain, &key(3)), None);

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = serve(&root.path().join("store"), &bound());
        let admin = Paths::for_home(&root.path().join("ma"));
        let url = format!("http://{}/v1/alab", running.addr);
        create(&admin, &url, None, Some("ma")).expect("create");
        let first = pinned(&admin).signer.expect("pinned");
        let done = rotate(&admin);
        assert!(done.rotated);
        assert_eq!(done.alab_release, None);
        let second = pinned(&admin).signer.expect("pinned");
        let done = retiring(Vec::new(), first, || rotate(&admin));
        let third = pinned(&admin).signer.expect("pinned");
        let release = done.alab_release.expect("the ALab vault, by loopback");
        assert_eq!(release.signer, b64(&third));
        assert_eq!(release.retired, vec![b64(&first), b64(&second)]);
        running.stop();
    }

    #[test]
    fn a_reader_the_removed_admin_led_off_follows_the_vault_once_its_build_retires_the_key() {
        std::thread::scope(|scope| {
            for (fork, rejoin, bare) in [
                (true, false, false),
                (false, false, false),
                (true, true, false),
                (true, false, true),
            ] {
                scope.spawn(move || {
                    let root = aterm_tempfile::tempdir().expect("tempdir");
                    let store = root.path().join("store");
                    let socket = bound();
                    let running = serve(&store, &socket);
                    let url = format!("http://{}/v1/alab", running.addr);
                    let admin = Paths::for_home(&root.path().join("ma"));
                    let created = create(&admin, &url, None, Some("ma")).expect("create");
                    let reader = Paths::for_home(&root.path().join("mb"));
                    join_pinned(&reader, &format!("{url}#{}", created.signer), "mb");
                    approve(&admin);
                    assert!(matches!(
                        sync(&reader, None).expect("sync"),
                        SyncOutcome::Applied { seq: 2, .. }
                    ));
                    let stale = std::fs::read(store.join("alab.vault")).expect("the store's copy");

                    let old = admin_open(&admin).expect("open").seed;
                    let old_key = envelope::signer_public(&old).expect("public");
                    rotate(&admin);
                    let new_key = pinned(&admin).signer.expect("pinned");

                    running.stop();
                    let spoof = serve(&root.path().join("spoof"), &socket);
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
                    transport::publish(&Source::Url(url.clone()), &fed)
                        .expect("the spoof takes it");
                    assert_eq!(sync(&reader, None).expect("taken"), SyncOutcome::NotIn);
                    assert_eq!(
                        pinned(&reader).signer,
                        Some(envelope::signer_public(&signer).expect("public"))
                    );
                    if bare {
                        let text =
                            std::fs::read_to_string(reader.source_file()).expect("source.toml");
                        let kept: String = text
                            .lines()
                            .filter(|line| !line.contains("followed"))
                            .map(|line| format!("{line}\n"))
                            .collect();
                        assert_ne!(kept, text, "the fork's lineage was written");
                        std::fs::write(reader.source_file(), kept).expect("written");
                    }
                    spoof.stop();

                    let lost = serve(&root.path().join("lost"), &socket);
                    transport::publish(&Source::Url(url.clone()), &stale).expect("the stale copy");
                    let poll = Some(Duration::from_millis(100));
                    assert_eq!(
                        sync(&reader, poll).expect("polled"),
                        SyncOutcome::Unchanged { seq: 1 << 40 }
                    );
                    assert_eq!(
                        std::fs::read(root.path().join("lost").join("alab.vault")).ok(),
                        Some(stale),
                        "nothing put back"
                    );
                    lost.stop();

                    let running = serve(&store, &socket);
                    let error = sync(&reader, None).expect_err("kept off");
                    let why = if fork {
                        "endorsed no handoff"
                    } else {
                        "rollback"
                    };
                    assert!(error.0.contains(why), "{error}");
                    assert_eq!(
                        sync(&reader, poll).expect("polled"),
                        SyncOutcome::Unchanged { seq: 1 << 40 }
                    );
                    assert_eq!(load_state(&reader).expect("state").seq, 1 << 40);
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
                });
            }
        });
    }

    #[test]
    fn an_admin_gives_the_vault_back_to_a_store_the_removed_admin_fed() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let socket = bound();
        let running = serve(&store, &socket);
        let url = format!("http://{}/v1/alab", running.addr);
        let admin = Paths::for_home(&root.path().join("ma"));
        create(&admin, &url, None, Some("ma")).expect("create");
        let stale = root.path().join("stale");
        std::fs::create_dir_all(&stale).expect("dir");
        std::fs::copy(store.join("alab.vault"), stale.join("alab.vault")).expect("copied");
        let old = admin_open(&admin).expect("open").seed;
        let old_key = envelope::signer_public(&old).expect("public");
        rotate(&admin);
        let new_key = pinned(&admin).signer.expect("pinned");
        running.stop();

        let fed = Unsigned {
            name: "alab".to_string(),
            seq: 1 << 40,
            time: 0,
            handoff: Vec::new(),
            lockers: Vec::new(),
        };
        let fed = envelope::sign(&fed, &old).expect("signed");
        let moved = serve(&stale, &socket);
        transport::publish(&Source::Url(url.clone()), &fed).expect("taken");
        moved.stop();
        let opened = retiring(vec![old_key], new_key, || {
            let moved = serve(&stale, &socket);
            let opened = admin_open(&admin);
            moved.stop();
            opened
        });
        assert_eq!(opened.expect("opened, its copy put back").seq(), 2);
        let cached = std::fs::read(admin.cache_file()).expect("cache");
        assert_eq!(std::fs::read(stale.join("alab.vault")).ok(), Some(cached));
    }

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
        let at = materialize::target_path(&reader.home, gap).expect("valid");
        std::fs::write(&at, b"the machine's own\n").expect("written");
        let first = sync(&reader, None).expect("sync");
        assert!(matches!(first, SyncOutcome::Applied { .. }), "{first:?}");
        assert_eq!(std::fs::read(&at).expect("kept"), b"the machine's own\n");
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
        std::fs::write(admin.cache_file(), b"alab-vault/1\ngarbage\n").expect("written");
        assert!(floor(&admin, &state, &config.name, &config).is_none());
        std::fs::write(admin.cache_file(), &cached).expect("written");
        state.membership = Stored::None;
        assert!(floor(&admin, &state, &config.name, &config).is_some());
    }

    #[test]
    fn a_large_manifest_reads_back_and_one_past_the_bound_is_never_written() {
        let home = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(home.path());
        let digest = materialize::digest_hex(b"v");
        let names = |count: usize, len: usize| {
            (0..count)
                .map(|i| (format!("~/{i:0>len$}"), digest.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let mut state = State::default();
        state.manifest.files = names(25_000, 120);
        state.pending = names(25_000, 120);
        save_state(&paths, &state).expect("saved");
        let loaded = load_state(&paths).expect("read back");
        assert_eq!(loaded.manifest, state.manifest);
        assert_eq!(loaded.pending, state.pending);

        let mut past = State::default();
        past.manifest.files = names(MAX_STATE / 1000 + 1, 1000);
        let error = save_state(&paths, &past).expect_err("past the bound");
        assert!(error.0.contains("more entries"), "{error}");
        assert_eq!(
            load_state(&paths).expect("still reads").manifest,
            state.manifest
        );
    }

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
        request_join(
            &reader,
            &left,
            Some("a-proof-for-the-vault-it-left".into()),
            Asker::Join,
        )
        .expect("asked");
        assert_eq!(held(), before);
        let current = pinned(&reader);
        request_join(&reader, &current, Some("its-proof".into()), Asker::Join).expect("asked");
        assert_eq!(
            load_state(&reader).expect("state").proof.as_deref(),
            Some("its-proof")
        );

        struct Stop(Option<crate::server::Running>);
        impl Drop for Stop {
            fn drop(&mut self) {
                if let Some(server) = self.0.take() {
                    server.stop();
                }
            }
        }
        let server = Stop(Some(serve(&root.path().join("store"), &bound())));
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

    #[test]
    fn a_join_to_another_vault_forgets_the_proof_made_for_the_last() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let server = serve(&root.path().join("store"), &bound());
        let vault = |admin: &str, name: &str| {
            let paths = Paths::for_home(&root.path().join(admin));
            let url = format!("http://{}/v1/{name}", server.addr);
            let created = create(&paths, &url, None, Some(admin)).expect("create");
            (paths, format!("{url}#{}", created.signer))
        };
        let (team_admin, team) = vault("ma", "team");
        let (other_admin, other) = vault("mz", "other");
        let password = admin_open(&team_admin)
            .expect("admin opens")
            .doc
            .join_password()
            .expect("create makes a join password");
        let reader = Paths::for_home(&root.path().join("mb"));
        let joined = |source: &str, password: &dyn Fn() -> Option<SecretBytes>| {
            join(
                &reader,
                &JoinOptions {
                    source: source.to_string(),
                    host: Some("mb".to_string()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
        };
        let asked = joined(&team, &|| Some(password.clone()));
        assert!(
            matches!(asked, Ok(JoinOutcome::Pending { .. })),
            "{asked:?}"
        );
        let mut state = load_state(&reader).expect("state");
        assert!(state.proof.is_some(), "the request carried a proof");
        state.requested = state.requested.saturating_sub(90 * 24 * 60 * 60);
        save_state(&reader, &state).expect("state saved");

        let seen = std::cell::RefCell::new(None);
        let moved = joined(&other, &|| {
            ask_again_if_due(&reader, &pinned(&reader), &|_| {});
            let held = load_state(&reader).expect("state").proof.is_some();
            let queued = requests(&other_admin, None).expect("requests").len();
            *seen.borrow_mut() = Some((held, queued));
            None
        });
        let (held, queued) = seen.take().expect("asked for the password");
        assert!(!held, "the proof made for the vault it left is gone");
        assert_eq!(
            queued, 0,
            "nothing reached the other vault before its own request"
        );
        assert!(
            matches!(moved, Ok(JoinOutcome::Pending { .. })),
            "{moved:?}"
        );
        server.stop();
    }

    #[test]
    fn a_join_cut_off_before_its_request_is_queued_is_asked_for_by_the_next_pass() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let socket = bound();
        let server = std::cell::RefCell::new(Some(serve(&store, &socket)));
        let url = format!("http://{}/v1/team", socket.local_addr().expect("address"));
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let gone = std::cell::RefCell::new(None);
        let stop = || {
            server.borrow_mut().take().expect("running").stop();
            *gone.borrow_mut() = Some(Unserved::at(&socket));
        };
        let cut_off = |host: &str, source: String, interactive: bool| {
            let paths = Paths::for_home(&root.path().join(host));
            let joined = join(
                &paths,
                &JoinOptions {
                    source,
                    host: Some(host.to_string()),
                    interactive,
                    wait: Duration::ZERO,
                    confirm: &|_| {
                        stop();
                        true
                    },
                    password: &|| {
                        stop();
                        None
                    },
                    requested: &|| {},
                    say: &|_| {},
                },
            );
            assert!(joined.is_err(), "{host}: {joined:?}");
            gone.borrow_mut().take().expect("stopped").end();
            *server.borrow_mut() = Some(serve(&store, &socket));
            paths
        };
        let at_sync = cut_off("mb", url.clone(), true);
        let at_request = cut_off("mc", format!("{url}#{}", created.signer), false);
        for (paths, host) in [(&at_sync, "mb"), (&at_request, "mc")] {
            assert_eq!(
                status(paths).expect("status").membership,
                Membership::Pending
            );
            let (config, _) = subscriber_pass(paths, &mut None).expect("a pass");
            ask_again_if_due(paths, &config, &|_| {});
            let queued: Vec<String> = requests(&admin, None)
                .expect("requests")
                .into_iter()
                .map(|request| request.host)
                .collect();
            assert!(queued.iter().any(|queued| queued == host), "{queued:?}");
        }
        server.borrow_mut().take().expect("running").stop();
    }

    #[test]
    fn a_reask_begun_on_the_vault_left_never_sends_the_new_ones_proof_there() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let team_server = serve(&root.path().join("team-store"), &bound());
        let team_url = format!("http://{}/v1/team", team_server.addr);
        let other_socket = bound();
        let other_server =
            std::cell::RefCell::new(Some(serve(&root.path().join("other-store"), &other_socket)));
        let other_url = format!(
            "http://{}/v1/other",
            other_socket.local_addr().expect("address")
        );
        let other_admin = Paths::for_home(&root.path().join("mz"));
        let other = create(&other_admin, &other_url, None, Some("mz")).expect("create");
        let password = admin_open(&other_admin)
            .expect("admin opens")
            .doc
            .join_password()
            .expect("create makes a join password");
        let mac = Paths::for_home(&root.path().join("ma"));
        create(&mac, &team_url, None, Some("ma")).expect("create");
        let begun = pinned(&mac);
        let gone = std::cell::RefCell::new(None);
        let moved = join(
            &mac,
            &JoinOptions {
                source: format!("{other_url}#{}", other.signer),
                host: Some("ma".to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| {
                    other_server.borrow_mut().take().expect("running").stop();
                    *gone.borrow_mut() = Some(Unserved::at(&other_socket));
                    Some(password.clone())
                },
                requested: &|| {},
                say: &|_| {},
            },
        );
        assert!(moved.is_err(), "{moved:?}");
        let state = load_state(&mac).expect("state");
        assert!(
            state.proof.is_some() && state.requested == 0,
            "due at once, with the other vault's proof"
        );
        ask_again_if_due(&mac, &begun, &|_| {});
        let queued = transport::list_joins(&team_url).expect("listed");
        assert!(queued.is_empty(), "{queued:?}");
        gone.borrow_mut().take().expect("stopped").end();
        team_server.stop();
    }

    #[test]
    fn a_reask_asks_under_the_name_source_toml_holds_now() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let socket = bound();
        let server = std::cell::RefCell::new(Some(serve(&store, &socket)));
        let url = format!("http://{}/v1/team", socket.local_addr().expect("address"));
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let mac = Paths::for_home(&root.path().join("mb"));
        let cut_off = |host: &str| {
            let gone = std::cell::RefCell::new(None);
            let joined = join(
                &mac,
                &JoinOptions {
                    source: format!("{url}#{}", created.signer),
                    host: Some(host.to_string()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: &|| {
                        server.borrow_mut().take().expect("running").stop();
                        *gone.borrow_mut() = Some(Unserved::at(&socket));
                        None
                    },
                    requested: &|| {},
                    say: &|_| {},
                },
            );
            assert!(joined.is_err(), "{host}: {joined:?}");
            gone.borrow_mut().take().expect("stopped").end();
            *server.borrow_mut() = Some(serve(&store, &socket));
        };
        cut_off("mb");
        let begun = pinned(&mac);
        cut_off("renamed");
        ask_again_if_due(&mac, &begun, &|_| {});
        let queued: Vec<String> = requests(&admin, None)
            .expect("requests")
            .into_iter()
            .map(|request| request.host)
            .collect();
        assert_eq!(queued, ["renamed"]);
        server.borrow_mut().take().expect("running").stop();
    }

    /// A join cut off before posting its request is asked for by a running subscriber
    /// [`JOIN_GRACE`] after it saved the vault: not sooner, and not an hour on.
    #[test]
    fn a_join_cut_off_before_its_request_is_asked_for_after_the_grace() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let server = serve(&root.path().join("store"), &bound());
        let url = format!("http://{}/v1/team", server.addr);
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let mac = Paths::for_home(&root.path().join("mb"));
        let cut = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            join(
                &mac,
                &JoinOptions {
                    source: format!("{url}#{}", created.signer),
                    host: Some("mb".to_string()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: &|| panic!("the process ends at the password prompt"),
                    requested: &|| {},
                    say: &|_| {},
                },
            )
        }));
        assert!(cut.is_err(), "the join ends at its prompt");
        let state = load_state(&mac).expect("state");
        assert!(
            state.membership == Stored::Pending && state.requested != 0,
            "waiting, with a request recorded that was never posted"
        );
        let hosts = || -> Vec<String> {
            requests(&admin, None)
                .expect("requests")
                .into_iter()
                .map(|request| request.host)
                .collect()
        };
        let config = pinned(&mac);
        ask_again_if_due(&mac, &config, &|_| {});
        assert!(
            hosts().is_empty(),
            "a pass leaves a join in flight to post its own request"
        );
        let mut state = load_state(&mac).expect("state");
        state.requested = state.requested.saturating_sub(JOIN_GRACE);
        save_state(&mac, &state).expect("state saved");
        ask_again_if_due(&mac, &config, &|_| {});
        assert_eq!(hosts(), ["mb"], "a pass after the grace asks");
        server.stop();
    }

    #[test]
    fn a_join_under_another_name_drops_the_old_names_proof() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("store");
        let socket = bound();
        let server = std::cell::RefCell::new(Some(serve(&store, &socket)));
        let url = format!("http://{}/v1/team", socket.local_addr().expect("address"));
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let password = admin_open(&admin)
            .expect("admin opens")
            .doc
            .join_password()
            .expect("create makes a join password");
        let key = join::Key::derive("team", password.as_slice());
        let mac = Paths::for_home(&root.path().join("mb"));
        let asked = |host: &str, password: &dyn Fn() -> Option<SecretBytes>| {
            join(
                &mac,
                &JoinOptions {
                    source: format!("{url}#{}", created.signer),
                    host: Some(host.to_string()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
        };
        let gone = std::cell::RefCell::new(None);
        let first = asked("mb", &|| {
            server.borrow_mut().take().expect("running").stop();
            *gone.borrow_mut() = Some(Unserved::at(&socket));
            Some(password.clone())
        });
        assert!(first.is_err(), "{first:?}");
        gone.borrow_mut().take().expect("stopped").end();
        *server.borrow_mut() = Some(serve(&store, &socket));
        let held = load_state(&mac).expect("state");
        assert!(
            held.proof.is_some() && held.requested == 0,
            "due, with mb's proof"
        );

        let misproved = |queued: &[Request]| -> Vec<String> {
            queued
                .iter()
                .filter(|request| {
                    request.proof.as_ref().is_some_and(|proof| {
                        !key.verify("team", &request.recipient, &request.host, proof)
                    })
                })
                .map(|request| request.host.clone())
                .collect()
        };
        let seen = std::cell::RefCell::new(None);
        let renamed = asked("renamed", &|| {
            ask_again_if_due(&mac, &pinned(&mac), &|_| {});
            *seen.borrow_mut() = Some((
                load_state(&mac).expect("state").proof,
                requests(&admin, None).expect("requests"),
            ));
            Some(password.clone())
        });
        let (proof, queued) = seen.take().expect("asked for the password");
        assert!(misproved(&queued).is_empty(), "{:?}", misproved(&queued));
        assert!(
            proof.is_none(),
            "no proof made for mb beside the name renamed"
        );
        assert!(
            matches!(renamed, Ok(JoinOutcome::Pending { ref host, .. }) if host == "renamed"),
            "{renamed:?}"
        );
        let queued = requests(&admin, None).expect("requests");
        assert!(misproved(&queued).is_empty(), "{:?}", misproved(&queued));
        let held: Vec<(&str, bool)> = queued
            .iter()
            .map(|request| (request.host.as_str(), request.proof.is_some()))
            .collect();
        assert_eq!(held, [("renamed", true)], "with renamed's proof");
        server.borrow_mut().take().expect("running").stop();
    }

    #[test]
    fn a_reask_never_puts_back_a_proof_a_join_replaced() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let gone = Unserved::at(&socket);
        let mac = Paths::for_home(&root.path().join("mb"));
        let config = Config {
            source: Source::Url(format!(
                "http://{}/v1/team",
                socket.local_addr().expect("address")
            )),
            name: "team".to_string(),
            signer: Some([7; 32]),
            followed: Vec::new(),
            host: "mb".to_string(),
        };
        save_config(&mac, &config).expect("source.toml");
        let read = Some(crate::b64::encode_padded(&[1; 32]));
        let typed = Some(crate::b64::encode_padded(&[2; 32]));
        let state = |proof: &Option<String>, requested: u64| State {
            membership: Stored::Pending,
            requested,
            proof: proof.clone(),
            ..State::default()
        };
        let held = || {
            let state = load_state(&mac).expect("state");
            (state.proof, state.requested, state.membership)
        };
        save_state(&mac, &state(&typed, 1_000)).expect("state.toml");
        request_join(&mac, &config, read.clone(), Asker::Again).expect_err("unanswered");
        assert_eq!(
            held(),
            (typed.clone(), 1_000, Stored::Pending),
            "left alone"
        );
        request_join(&mac, &config, typed.clone(), Asker::Again).expect_err("unanswered");
        assert_eq!(held(), (typed.clone(), 0, Stored::Pending));
        request_join(&mac, &config, read.clone(), Asker::Join).expect_err("unanswered");
        assert_eq!(held(), (read, 0, Stored::Pending));
        gone.end();
    }

    #[test]
    fn a_request_the_server_lost_with_its_store_is_asked_again_once_the_vault_is_back() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let server = serve(&root.path().join("store"), &socket);
        let url = format!("http://{}/v1/team", server.addr);
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let mac = Paths::for_home(&root.path().join("mb"));
        let asked = join(
            &mac,
            &JoinOptions {
                source: format!("{url}#{}", created.signer),
                host: Some("mb".to_string()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| None,
                requested: &|| {},
                say: &|_| {},
            },
        );
        assert!(
            matches!(asked, Ok(JoinOutcome::Pending { .. })),
            "{asked:?}"
        );
        server.stop();
        let gone = Unserved::at(&socket);
        assert!(
            subscriber_pass(&mac, &mut None).is_err(),
            "the server is gone"
        );
        gone.end();
        let server = serve(&root.path().join("moved"), &socket);
        let (_, restored) = subscriber_pass(&admin, &mut None).expect("the admin's pass");
        assert!(
            matches!(restored, SyncOutcome::PutBack { served: None, .. }),
            "the admin's subscriber put the vault back: {restored:?}"
        );
        let (config, outcome) = subscriber_pass(&mac, &mut None).expect("a pass");
        assert!(
            matches!(outcome, SyncOutcome::Unchanged { .. }),
            "{outcome:?}"
        );
        ask_again_if_due(&mac, &config, &|_| {});
        let queued: Vec<String> = requests(&admin, None)
            .expect("requests")
            .into_iter()
            .map(|request| request.host)
            .collect();
        assert_eq!(queued, ["mb"]);
        server.stop();
    }

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
        assert!(!rotate(&admin).first_long_chain);
        assert!(matches!(
            sync(&reader, None).expect("sync"),
            SyncOutcome::Applied { seq: 3, .. }
        ));

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

        let error = sync(&reader, None).expect_err("refused");
        assert!(
            error.0.contains("drops or changes handoff 1 of 1"),
            "{error}"
        );
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
        let late = Paths::for_home(&root.path().join("mc"));
        join_pinned(&late, &first, "mc");
        assert_eq!(pinned(&late).signer, Some(next));
    }

    #[test]
    fn only_a_request_with_a_proof_was_asked_with_a_password() {
        let home = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(home.path());
        assert!(!asked_with_password(&paths), "no state");
        let mut state = State {
            membership: Stored::Pending,
            ..State::default()
        };
        save_state(&paths, &state).expect("saved");
        assert!(!asked_with_password(&paths), "asked by code alone");
        state.proof = Some("proof".to_string());
        save_state(&paths, &state).expect("saved");
        assert!(asked_with_password(&paths));
    }

    #[test]
    fn the_approver_remembers_only_what_is_still_listed() {
        let key = crate::transport::test_recipient(1);
        let request = |n: u32| Request {
            recipient: key.clone(),
            host: "m9".to_string(),
            code: "abcd-efgh-ijkm-npqr".to_string(),
            at: None,
            proof: Some(format!("proof-{n}")),
        };
        let waiting = request(0);
        let config = Config {
            source: Source::Url("http://vault.invalid:7484/v1/team".to_string()),
            name: "team".to_string(),
            signer: Some([7; 32]),
            followed: Vec::new(),
            host: "ma".to_string(),
        };
        let mut settled = Settled::default();
        for n in 1..=100 {
            let listed = [waiting.clone(), request(n)];
            assert!(
                settled.due(&config, 1, &listed, Approval::Password),
                "proof {n}"
            );
            settled.settle(listed.iter().map(Judged::of).collect());
            assert!(
                !settled.due(&config, 1, &listed, Approval::Password),
                "proof {n}"
            );
        }
        assert_eq!(settled.judged.len(), 2);
        assert!(!settled.due(
            &config,
            1,
            std::slice::from_ref(&waiting),
            Approval::Password
        ));
        assert_eq!(settled.judged.len(), 1);
    }

    fn serve_apart(
        store: &Path,
        socket: &std::net::TcpListener,
        pins: &[(String, [u8; 32])],
    ) -> crate::server::Running {
        crate::server::start_apart(
            socket.try_clone().expect("a copy of the socket"),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().expect("addr"),
                store: store.to_path_buf(),
                advertise: None,
                pins: pins.to_vec(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .expect("server starts")
    }

    fn a_vault_with_a_member(
        root: &Path,
        socket: &std::net::TcpListener,
    ) -> (crate::server::Running, String, Paths, Paths) {
        let server = serve(&root.join("store"), socket);
        let url = format!("http://{}/v1/team", server.addr);
        let admin = Paths::for_home(&root.join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let member = Paths::for_home(&root.join("mb"));
        join_pinned(&member, &format!("{url}#{}", created.signer), "mb");
        let key = status(&member)
            .expect("status")
            .machine
            .expect("a key")
            .0
            .to_bech32();
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.add_machine("mb", &key).expect("add");
        doc.set_entry(&Audience::Everyone, "token", b"inert-token")
            .expect("set");
        admin_publish(&admin, &opened, &doc).expect("publish");
        assert!(matches!(
            sync(&member, None).expect("sync"),
            SyncOutcome::Applied { seq: 2, .. }
        ));
        (server, url, admin, member)
    }

    fn served_at(source: &Source) -> Option<Vec<u8>> {
        match transport::fetch(source, None, Duration::from_secs(30)) {
            Ok(Fetched::Body(bytes)) => Some(bytes),
            Ok(Fetched::Missing) => None,
            other => panic!("{other:?}"),
        }
    }

    fn pin_of(paths: &Paths) -> Vec<(String, [u8; 32])> {
        let config = pinned(paths);
        vec![(config.name, config.signer.expect("signed"))]
    }

    #[test]
    fn a_machine_in_the_vault_puts_back_a_vault_its_server_lost() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let (server, url, admin, member) = a_vault_with_a_member(root.path(), &socket);
        let source = Source::Url(url);
        let pin = pin_of(&admin);
        let lose = |server: crate::server::Running, store: &str| {
            server.stop();
            let empty = serve_apart(&root.path().join(store), &socket, &pin);
            assert!(served_at(&source).is_none(), "the server holds no vault");
            empty
        };
        let copy = std::fs::read(member.cache_file()).expect("the member's copy");

        let server = lose(server, "moved");
        let (config, outcome) = subscriber_pass(&member, &mut None).expect("a pass");
        assert_eq!(
            outcome,
            SyncOutcome::PutBack {
                seq: 2,
                served: None
            }
        );
        assert!(
            served_at(&source) == Some(copy.clone()),
            "the member's copy is served"
        );
        let mut failing = FIRST_RETRY;
        assert_eq!(
            pause_after(&config.source, &outcome, &mut failing),
            FIRST_RETRY
        );

        let token = materialize::target_path(&member.home, "token").expect("target");
        std::fs::remove_file(&token).expect("the token removed");
        std::fs::create_dir(&token).expect("the entry blocked");
        let SyncOutcome::Applied { report, .. } = sync(&member, None).expect("a partial apply")
        else {
            panic!("the blocked entry is applied again");
        };
        assert!(!report.failed.is_empty());
        let server = lose(server, "again");
        let (_, outcome) = subscriber_pass(&member, &mut None).expect("a pass");
        assert_eq!(
            outcome,
            SyncOutcome::PutBack {
                seq: 2,
                served: None
            }
        );
        assert!(served_at(&source) == Some(copy), "the member's copy again");
        server.stop();
    }

    #[test]
    fn a_put_back_the_source_keeps_no_more_backs_off_and_is_logged_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(root.path());
        let config = Config {
            source: Source::Url("http://127.0.0.1:7484/v1/team".to_string()),
            name: "team".into(),
            signer: Some([1u8; 32]),
            followed: Vec::new(),
            host: "mb".into(),
        };
        let put_back = SyncOutcome::PutBack {
            seq: 2,
            served: None,
        };
        let (mut failing, mut repeated) = (FIRST_RETRY, String::new());
        let mut pass = |outcome: &SyncOutcome| {
            (
                pause_after(&config.source, outcome, &mut failing).as_secs(),
                pass_line(&paths, &config, outcome, &mut repeated),
            )
        };
        let passes: Vec<(u64, bool)> = (0..8)
            .map(|_| {
                let (pause, line) = pass(&put_back);
                (pause, line.is_some())
            })
            .collect();
        assert_eq!(
            passes,
            [
                (5, true),
                (10, false),
                (20, false),
                (40, false),
                (80, false),
                (160, false),
                (300, false),
                (300, false)
            ]
        );
        assert_eq!(pass(&SyncOutcome::Unchanged { seq: 2 }), (10, None));
        let (pause, line) = pass(&put_back);
        assert_eq!(pause, 5);
        let line = line.expect("logged again");
        assert!(
            line.contains("held no vault named team: put this machine's copy back (seq 2)"),
            "{line}"
        );
        let failed = SyncOutcome::Applied {
            seq: 2,
            report: crate::materialize::Report {
                failed: vec![("token".to_string(), "is a symlink".to_string())],
                ..Default::default()
            },
        };
        assert!(pass(&failed).1.is_some());
        assert_eq!(pass(&failed).1, None);
    }

    #[test]
    fn a_server_that_does_not_pin_the_vault_refuses_its_copy_and_the_pass_errs() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let (server, url, _, member) = a_vault_with_a_member(root.path(), &socket);
        let source = Source::Url(url);
        server.stop();
        let server = serve_apart(&root.path().join("moved"), &socket, &[]);
        let first = subscriber_pass(&member, &mut None)
            .expect_err("refused")
            .to_string();
        assert!(
            first
                .contains("holds no vault named team, and putting this machine's copy back failed")
                && first.contains("403")
                && first.contains(
                    "only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>"
                ),
            "{first}"
        );
        assert!(served_at(&source).is_none(), "nothing stored");
        let second = subscriber_pass(&member, &mut None)
            .expect_err("refused again")
            .to_string();
        assert_eq!(second, first, "one line in the log, then the backoff");
        assert!(served_at(&source).is_none(), "nothing stored");
        server.stop();
    }

    #[test]
    fn a_machine_kept_out_or_without_a_verified_copy_puts_nothing_back() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let (server, url, admin, member) = a_vault_with_a_member(root.path(), &socket);
        let source = Source::Url(url.clone());
        let signer = crate::b64::encode_padded(&pinned(&admin).signer.expect("signed"));
        let waiting = Paths::for_home(&root.path().join("mc"));
        assert!(matches!(
            join_pinned(&waiting, &format!("{url}#{signer}"), "mc"),
            JoinOutcome::Pending { .. }
        ));
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "later", b"inert-later")
            .expect("set");
        admin_publish(&admin, &opened, &doc).expect("publish");
        let newer = served_at(&source).expect("seq 3");
        let copy = std::fs::read(member.cache_file()).expect("the member's copy");
        server.stop();
        let server = serve_apart(&root.path().join("moved"), &socket, &pin_of(&admin));
        let missing_here = |paths: &Paths| missing(&pinned(paths)).to_string();

        let error = subscriber_pass(&waiting, &mut None).expect_err("kept out");
        assert_eq!(error.to_string(), missing_here(&waiting));
        assert!(
            served_at(&source).is_none(),
            "a waiting machine puts nothing back"
        );
        assert_eq!(
            load_state(&waiting).expect("state").requested,
            0,
            "asked again at once"
        );

        let mut forged = copy.clone();
        let at = forged.len() - 10;
        forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
        let held = load_state(&member).expect("state").digest;
        let record = |digest: Option<[u8; 32]>| {
            let mut state = load_state(&member).expect("state");
            state.digest = digest;
            save_state(&member, &state).expect("state.toml");
        };
        for (what, cached, recorded) in [
            ("a cache that is not the version held", &newer, held),
            (
                "a cache that does not verify, though state.toml holds it",
                &forged,
                Some(sha256(&forged)),
            ),
        ] {
            record(recorded);
            fsx::write_atomic(&member.cache_file(), cached, 0o600).expect("cache");
            let error = subscriber_pass(&member, &mut None).expect_err(what);
            assert_eq!(error.to_string(), missing_here(&member), "{what}");
            assert!(served_at(&source).is_none(), "{what}: nothing put back");
        }
        record(held);
        fsx::write_atomic(&member.cache_file(), &copy, 0o600).expect("cache");
        let (_, outcome) = subscriber_pass(&member, &mut None).expect("a pass");
        assert_eq!(
            outcome,
            SyncOutcome::PutBack {
                seq: 2,
                served: None
            }
        );
        assert!(
            served_at(&source) == Some(copy),
            "the member's copy is served"
        );
        server.stop();
    }

    #[test]
    fn a_machine_in_the_vault_writes_nothing_to_a_path_that_holds_no_vault() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("shared").join("team.vault");
        std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
        let path = shared.to_str().expect("utf-8");
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, path, None, Some("ma")).expect("create");
        let member = Paths::for_home(&root.path().join("mb"));
        join_pinned(&member, &format!("{path}#{}", created.signer), "mb");
        let key = status(&member)
            .expect("status")
            .machine
            .expect("a key")
            .0
            .to_bech32();
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.add_machine("mb", &key).expect("add");
        admin_publish(&admin, &opened, &doc).expect("publish");
        assert!(matches!(
            sync(&member, None).expect("sync"),
            SyncOutcome::Applied { seq: 2, .. }
        ));
        std::fs::remove_file(&shared).expect("the file gone");

        let error = subscriber_pass(&member, &mut None).expect_err("missing");
        assert_eq!(error.to_string(), missing(&pinned(&member)).to_string());
        assert!(!shared.exists(), "nothing written at the path");
        admin_open(&admin).expect("open");
        assert_eq!(
            std::fs::read(&shared).ok(),
            std::fs::read(admin.cache_file()).ok(),
            "the admin's copy is back"
        );
    }

    #[test]
    fn an_admin_of_a_vault_closed_to_joins_puts_back_a_vault_its_server_lost() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let server = serve(&root.path().join("store"), &socket);
        let url = format!("http://{}/v1/team", server.addr);
        let source = Source::Url(url.clone());
        let admin = Paths::for_home(&root.path().join("ma"));
        create(&admin, &url, None, Some("ma")).expect("create");
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.set_join_password(None).expect("closed");
        admin_publish(&admin, &opened, &doc).expect("publish");
        assert_eq!(approving_here(&admin).expect("read"), None, "no approver");
        server.stop();
        let server = serve_apart(&root.path().join("moved"), &socket, &pin_of(&admin));
        let (_, outcome) = subscriber_pass(&admin, &mut None).expect("a pass");
        assert_eq!(
            outcome,
            SyncOutcome::PutBack {
                seq: 2,
                served: None
            }
        );
        let copy = std::fs::read(admin.cache_file()).expect("the admin's copy");
        assert!(
            served_at(&source) == Some(copy),
            "the admin's copy is served"
        );
        server.stop();
    }

    #[test]
    fn a_machine_puts_its_newer_copy_back_over_an_older_one_a_server_took() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = bound();
        let server = serve(&root.path().join("store"), &socket);
        let url = format!("http://{}/v1/team", server.addr);
        let source = Source::Url(url.clone());
        let served = || match transport::fetch(&source, None, Duration::from_secs(30)) {
            Ok(Fetched::Body(bytes)) => bytes,
            other => panic!("{other:?}"),
        };
        let admin = Paths::for_home(&root.path().join("ma"));
        let created = create(&admin, &url, None, Some("ma")).expect("create");
        let member = Paths::for_home(&root.path().join("mb"));
        join_pinned(&member, &format!("{url}#{}", created.signer), "mb");
        let key = status(&member)
            .expect("status")
            .machine
            .expect("a key")
            .0
            .to_bech32();
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.add_machine("mb", &key).expect("add");
        admin_publish(&admin, &opened, &doc).expect("publish");
        let stale = served();
        let opened = admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-token")
            .expect("set");
        admin_publish(&admin, &opened, &doc).expect("publish");
        assert!(matches!(
            sync(&member, None).expect("sync"),
            SyncOutcome::Applied { seq: 3, .. }
        ));
        let newest = std::fs::read(member.cache_file()).expect("the member's copy");

        server.stop();
        let server = serve(&root.path().join("moved"), &socket);
        transport::publish(&source, &stale).expect("the stale copy is taken");

        let mut forged = newest.clone();
        let at = forged.len() - 10;
        forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
        for (what, cached) in [
            ("a cache that is not the version held", &stale),
            ("a cache that does not verify", &forged),
        ] {
            fsx::write_atomic(&member.cache_file(), cached, 0o600).expect("cache");
            let (_, outcome) = subscriber_pass(&member, &mut None).expect(what);
            assert_eq!(outcome, SyncOutcome::Unchanged { seq: 3 }, "{what}");
            assert!(served() == stale, "{what}: nothing published");
        }
        fsx::write_atomic(&member.cache_file(), &newest, 0o600).expect("cache");
        let (_, outcome) = subscriber_pass(&member, &mut None).expect("a pass");
        assert_eq!(
            outcome,
            SyncOutcome::PutBack {
                seq: 3,
                served: Some(2)
            }
        );
        assert!(served() == newest, "the newest copy is served");
        server.stop();
    }
}

#[cfg(test)]
mod edit_scratch_tests {
    use super::{Paths, Scratch, extract_machine_tag};
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

    #[test]
    fn extract_machine_tag_finds_mid_string_machine_id_and_skips_apple_silicon_chip_names() {
        assert_eq!(
            extract_machine_tag("MacbookAir-m27-M5-32GB-4TB.local"),
            Some("m27".to_string())
        );
        assert_eq!(
            extract_machine_tag("m22-Macbook-M5-Max-128GB-8TB.local"),
            Some("m22".to_string())
        );
        assert_eq!(
            extract_machine_tag("Andrew's MacBook Pro (m15)"),
            Some("m15".to_string())
        );
        assert_eq!(extract_machine_tag("MacBook-Pro-M4-Max.local"), None);
    }
}
