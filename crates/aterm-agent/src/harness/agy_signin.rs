// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! agy's native headless authorization handoff. The native CLI owns PKCE and its grant;
//! a successful run still requires canonical account verification before publication.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_secrets::SecretBytes;

const CALLBACK: &str = "https://antigravity.google/oauth-callback";
const LIMIT: usize = 64 * 1024;
const SCOPES: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
    "https://www.googleapis.com/auth/aicode",
];

/// Quiet failures contain no native output, account address, URL or authorization code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Profile,
    Process,
    Deadline,
    Output,
    Authorization,
    Callback,
    Native,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Profile => "Private agy login profile is unavailable",
            Self::Process => "Native agy login could not start or be controlled",
            Self::Deadline => "Native agy authorization exceeded its deadline",
            Self::Output => "Native agy authorization output exceeded its bound",
            Self::Authorization => "Native agy Google authorization request was refused",
            Self::Callback => "Native agy Google callback did not match its request",
            Self::Native => "Native agy did not complete its authorization",
        })
    }
}

/// An admitted request captured from the exact native child, never from mail or a webpage.
/// No printable representation exposes its OAuth query.
pub struct Authorization {
    url: SecretBytes,
    state: SecretBytes,
    binding: [u8; 32],
}

fn one(url: &str, key: &str) -> Result<String, Error> {
    let values = super::headless_oauth::query_values(url, key);
    if values.len() != 1
        || values[0].is_empty()
        || values[0].len() > 8192
        || values[0]
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Error::Authorization);
    }
    Ok(values[0].clone())
}

impl Authorization {
    fn admit(url: &str) -> Result<Self, Error> {
        if url.len() > 16 * 1024
            || url.contains('#')
            || url.split('?').next() != Some("https://accounts.google.com/o/oauth2/auth")
            || super::login_drive::https_host(url).as_deref() != Some("accounts.google.com")
        {
            return Err(Error::Authorization);
        }
        for (key, expected) in [
            ("redirect_uri", CALLBACK),
            ("response_type", "code"),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
        ] {
            if one(url, key)? != expected {
                return Err(Error::Authorization);
            }
        }
        let client = one(url, "client_id")?;
        if !client.ends_with(".apps.googleusercontent.com") {
            return Err(Error::Authorization);
        }
        let challenge = one(url, "code_challenge")?;
        if challenge.len() != 43
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::Authorization);
        }
        let scopes = super::headless_oauth::query_values(url, "scope");
        if scopes.len() != 1 {
            return Err(Error::Authorization);
        }
        let scopes: std::collections::BTreeSet<_> = scopes[0].split_whitespace().collect();
        if scopes != SCOPES.iter().copied().collect() {
            return Err(Error::Authorization);
        }
        let state = SecretBytes::from(one(url, "state")?);
        let mut digest = aterm_digest::Sha256::new();
        digest.update(url.as_bytes());
        Ok(Self {
            url: SecretBytes::from(url.as_bytes()),
            state,
            binding: digest.finalize(),
        })
    }

    /// In-memory request for the owned headless browser. Never log this value.
    pub fn url(&self) -> &str {
        self.url.as_str().unwrap_or_default()
    }

    pub(super) fn admits_google(&self, url: &str) -> bool {
        super::login_drive::https_host(url).as_deref() == Some("accounts.google.com")
            && super::headless_oauth::FlowBound::of(self.url())
                .ok()
                .is_some_and(|bound| {
                    bound
                        .outside(url, &["accounts.google.com", "antigravity.google"])
                        .is_none()
                })
    }

    /// Validate the original committed callback URL before the vendor page clears its query.
    /// Its code may be delivered only to the child that produced this request.
    pub fn callback(&self, url: &str) -> Result<Code, Error> {
        if url.split('?').next() != Some(CALLBACK)
            || url.contains('#')
            || super::login_drive::https_host(url).as_deref() != Some("antigravity.google")
            || !super::headless_oauth::query_values(url, "error").is_empty()
            || one(url, "state").map_err(|_| Error::Callback)?.as_bytes() != self.state.as_slice()
        {
            return Err(Error::Callback);
        }
        let code = one(url, "code").map_err(|_| Error::Callback)?;
        Ok(Code {
            value: SecretBytes::from(code),
            binding: self.binding,
        })
    }
}

/// State-checked, request-bound native handback; the code is wiped on drop.
pub struct Code {
    value: SecretBytes,
    binding: [u8; 32],
}

/// A native grant location after CLI success. This is not proof of its account identity.
pub struct UnverifiedGrant {
    path: PathBuf,
    program: PathBuf,
    _claim: super::browser_window::ProfileLock,
}

/// Failures in the explicitly configured Google-to-agy path, with no credential values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoogleLoginError {
    Configuration,
    Credentials(super::google_credentials::Error),
    Approval(super::google_direct::Error),
    Native(Error),
}

impl std::fmt::Display for GoogleLoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration => {
                f.write_str("Explicit agy Google identity binding is unavailable or changed")
            }
            Self::Credentials(error) => std::fmt::Display::fmt(error, f),
            Self::Approval(error) => std::fmt::Display::fmt(error, f),
            Self::Native(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

/// A completed native run with a configured expected account, still awaiting canonical proof.
pub struct UnverifiedGoogleGrant {
    native: UnverifiedGrant,
    expected_email: String,
    state: PathBuf,
    account: String,
    revision: Option<[u8; 32]>,
}

/// A native grant accepted by Google's canonical account endpoint. The native profile
/// remains exclusively claimed until this value is dropped.
pub struct VerifiedGoogleGrant {
    grant: UnverifiedGoogleGrant,
    profile: super::google_profile::GoogleProfile,
    digest: [u8; 32],
}

/// Worker launch prepared from a canonically verified native grant. Retains the profile
/// claim and checks the exact binding and grant again when its starter is requested.
pub struct VerifiedGoogleWorker {
    grant: VerifiedGoogleGrant,
    starter: super::worker::Starter,
}

impl VerifiedGoogleWorker {
    /// Recheck immediately before admitting/writing this worker's launch line.
    pub fn starter(&self) -> Result<&super::worker::Starter, &'static str> {
        self.grant.check_current()?;
        Ok(&self.starter)
    }
}

impl VerifiedGoogleGrant {
    /// Carry the verified profile into a worker, replacing inherited profile routing.
    /// The native executable is the same managed program that produced the grant.
    pub fn prepare_worker(
        self,
        mut starter: super::worker::Starter,
    ) -> Result<VerifiedGoogleWorker, &'static str> {
        let path = self.path()?;
        let gemini = path
            .parent()
            .and_then(Path::parent)
            .ok_or("Native agy profile path is unavailable")?;
        let home = gemini
            .parent()
            .and_then(Path::to_str)
            .ok_or("Native agy profile home is unavailable")?;
        let flags = starter
            .args
            .as_ref()
            .map_err(|_| "Inherited agy flags are unavailable")?;
        starter.args = Ok(super::upgrade_agy::bound_worker_flags(flags, gemini)?);
        starter.exe = self.grant.native.program.clone();
        starter.env.retain(|(name, _)| {
            name != "HOME" && name != "SSH_CONNECTION" && name != "AGY_CLI_DISABLE_AUTO_UPDATE"
        });
        starter.env.extend([
            ("HOME".into(), home.into()),
            ("SSH_CONNECTION".into(), "127.0.0.1 1 127.0.0.1 22".into()),
            ("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()),
        ]);
        for name in crate::ai::connector::AGY.credential_env {
            starter.env.retain(|(key, _)| key != name);
            if !starter.unset.contains(name) {
                starter.unset.push(name);
            }
        }
        Ok(VerifiedGoogleWorker {
            grant: self,
            starter,
        })
    }
    /// Provider-confirmed account identity.
    pub fn profile(&self) -> &super::google_profile::GoogleProfile {
        &self.profile
    }

    /// Recheck configuration and exact grant bytes immediately before publication/use.
    pub fn check_current(&self) -> Result<(), &'static str> {
        self.grant
            .check_binding()
            .map_err(|_| "Google binding changed")?;
        let bytes = grant_bytes(self.grant.path())?;
        if grant_digest(bytes.as_slice()) != self.digest {
            return Err("Native Google grant changed after verification");
        }
        Ok(())
    }

    /// Claimed native token path, available only while its account proof is current.
    pub fn path(&self) -> Result<&Path, &'static str> {
        self.check_current()?;
        Ok(self.grant.path())
    }
}

fn grant_bytes(path: &Path) -> Result<SecretBytes, &'static str> {
    aterm_secrets::fsx::read_private(path, LIMIT)
        .map_err(|_| "Private native Google grant is unavailable")?
        .filter(|bytes| !bytes.is_empty())
        .ok_or("Private native Google grant is empty")
}

fn grant_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(bytes);
    digest.finalize()
}

fn native_access_token(bytes: &[u8]) -> Result<SecretBytes, &'static str> {
    let value: aterm_json::Value =
        aterm_json::from_slice(bytes).map_err(|_| "Native Google grant is unreadable")?;
    // Current native StoredToken wraps golang.org/x/oauth2.Token in `token`.
    // Cached id_token_email and legacy id_token are not account proof.
    let token = value["token"]["access_token"]
        .as_str()
        .filter(|s| {
            !s.is_empty() && s.len() <= LIMIT && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .map(|s| SecretBytes::from(s.as_bytes()))
        .ok_or("Native Google grant has no valid access token");
    fn wipe(value: aterm_json::Value) {
        match value {
            aterm_json::Value::String(s) => drop(SecretBytes::from(s)),
            aterm_json::Value::Array(values) => values.into_iter().for_each(wipe),
            aterm_json::Value::Object(values) => values.into_values().for_each(wipe),
            _ => {}
        }
    }
    wipe(value);
    token
}

impl UnverifiedGoogleGrant {
    /// Prove the exact native token's Google account before making it usable.
    /// Provider rejection, deadline expiry, or concurrent binding/grant changes fail closed.
    pub fn verify(self, deadline: Instant) -> Result<VerifiedGoogleGrant, &'static str> {
        self.check_binding().map_err(|_| "Google binding changed")?;
        let bytes = grant_bytes(self.path())?;
        let digest = grant_digest(bytes.as_slice());
        let token = native_access_token(bytes.as_slice())?;
        let wait = deadline
            .checked_duration_since(Instant::now())
            .filter(|wait| !wait.is_zero())
            .ok_or("Google verification deadline expired")?;
        let profile = super::google_profile::read(
            token.as_str().ok_or("Native Google token is not text")?,
            self.expected_email(),
            wait,
        )?;
        if Instant::now() >= deadline {
            return Err("Google verification deadline expired");
        }
        let verified = VerifiedGoogleGrant {
            grant: self,
            profile,
            digest,
        };
        verified.check_current()?;
        Ok(verified)
    }
    /// Private native grant, claimed until this guard is dropped.
    pub fn path(&self) -> &Path {
        self.native.path()
    }
    /// Exact configured Google account to verify with the provider.
    pub fn expected_email(&self) -> &str {
        &self.expected_email
    }
    /// Check again before verification/publication; a changed binding cannot publish a login.
    pub fn check_binding(&self) -> Result<(), GoogleLoginError> {
        if binding_current(&self.state, &self.account, self.revision) {
            Ok(())
        } else {
            Err(GoogleLoginError::Configuration)
        }
    }
}

fn registry(state: &Path) -> Option<super::google_identity::Registry> {
    let bytes = super::google_federation::read_registry(state).ok()??;
    super::google_identity::Registry::parse(std::str::from_utf8(&bytes).ok()?).ok()
}

fn binding_current(state: &Path, account: &str, revision: Option<[u8; 32]>) -> bool {
    revision.is_some()
        && registry(state).is_some_and(|r| r.binding_revision("agy", account) == revision)
}

fn native_profile(
    state: &Path,
    account: &str,
    identity: &super::google_identity::Identity,
) -> PathBuf {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.agy-login.v1\0");
    for part in [account, identity.id(), identity.email()] {
        digest.update(part.as_bytes());
        digest.update(b"\0");
    }
    state
        .join("agy-logins")
        .join(aterm_codec::hex::encode_lower(&digest.finalize()))
}

/// Approve a native agy login using its explicitly bound Google identity and registered
/// credentials. No account is selected implicitly and no interactive fallback is opened.
/// Native completion remains unverified until a canonical provider check accepts its grant.
pub fn run_google(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    deadline: Instant,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    if !state.is_absolute() || !home.is_absolute() {
        return Err(GoogleLoginError::Configuration);
    }
    let registry = registry(state).ok_or(GoogleLoginError::Configuration)?;
    let (identity, binding) = registry
        .resolve("agy", account)
        .ok_or(GoogleLoginError::Configuration)?;
    if !identity
        .email()
        .eq_ignore_ascii_case(binding.expected_email())
    {
        return Err(GoogleLoginError::Configuration);
    }
    let profile = native_profile(state, account, identity);
    let warm = aterm_secrets::fsx::has_private_nonempty(
        &profile.join(".gemini/antigravity-cli/antigravity-oauth-token"),
        LIMIT,
    );
    // An existing native grant owns its refresh independently of the Google web password.
    // Fresh profiles still refuse unavailable credentials before starting a native child.
    let mut credentials = if warm {
        None
    } else {
        Some(
            super::google_credentials::Credentials::load(identity, home)
                .map_err(GoogleLoginError::Credentials)?,
        )
    };
    let revision = registry.binding_revision("agy", account);
    let current = || binding_current(state, account, revision);
    let mut approval_error = None;
    let native = run(program, &profile, deadline, |request, deadline| {
        if credentials.is_none() {
            match super::google_credentials::Credentials::load(identity, home) {
                Ok(loaded) => credentials = Some(loaded),
                Err(error) => {
                    approval_error = Some(GoogleLoginError::Credentials(error));
                    return Err(Error::Authorization);
                }
            }
        }
        let credentials = credentials.as_mut().ok_or(Error::Authorization)?;
        let mailbox = super::headless_oauth::discover_mailbox_config(Some(home), Some(state));
        let context = super::google_direct::Context {
            identity,
            home,
            state,
            mailbox: mailbox.as_ref(),
            current: &current,
        };
        match super::google_direct::approve(request, credentials, &context, deadline) {
            Ok(code) => Ok(code),
            Err(error) => {
                approval_error = Some(GoogleLoginError::Approval(error));
                Err(Error::Authorization)
            }
        }
    });
    if let Some(error) = approval_error {
        return Err(error);
    }
    let result = UnverifiedGoogleGrant {
        native: native.map_err(GoogleLoginError::Native)?,
        expected_email: binding.expected_email().into(),
        state: state.into(),
        account: account.into(),
        revision,
    };
    result.check_binding()?;
    Ok(result)
}

impl UnverifiedGrant {
    /// Private native grant location. Keep this guard alive through verification/publication
    /// so another cooperative login cannot replace the grant between those operations.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct Child {
    child: std::process::Child,
    _slave: OwnedFd,
    master: std::fs::File,
    exit: OwnedFd,
    group_ended: bool,
}

impl Drop for Child {
    fn drop(&mut self) {
        // Never reap before ending the group: the unreaped leader reserves its identity.
        if !self.group_ended
            && let Ok(pid) = i32::try_from(self.child.id())
        {
            let _ = aterm_uds::process::kill_group_until_empty(pid);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(Error::Deadline)
}

fn group_is_exited(
    leader: i32,
    members: &aterm_uds::process::GroupMembers,
    mut presence: impl FnMut(i32) -> aterm_uds::process::ProcessPresence,
    members_again: impl FnOnce() -> Option<aterm_uds::process::GroupMembers>,
) -> bool {
    members.complete
        && members.pids.contains(&leader)
        && members.pids.iter().all(|pid| {
            matches!(
                presence(*pid),
                aterm_uds::process::ProcessPresence::Exited
                    | aterm_uds::process::ProcessPresence::Absent
            )
        })
        // A live member that could not be signalled may fork after the first listing and exit
        // before its own presence read. A second complete listing must find no new member.
        && members_again().is_some_and(|current| {
            current.complete
                && current.pids.contains(&leader)
                && current.pids.iter().all(|pid| members.pids.contains(pid))
        })
}

/// End the child's unreaped process group. XNU refuses a signal with `EPERM` when only zombies
/// remain, so prove that case from complete kernel group listings around per-process exit reads
/// instead of treating a normal successful child exit as a process-control failure.
fn finish_exited_group(leader: i32) -> std::io::Result<()> {
    match aterm_uds::process::kill_group_until_empty(leader) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            match aterm_uds::process::process_presence(leader) {
                aterm_uds::process::ProcessPresence::Exited
                | aterm_uds::process::ProcessPresence::Absent => Ok(()),
                _ => Err(error),
            }
        }
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
            let ended = u32::try_from(leader)
                .ok()
                .and_then(aterm_uds::process::group_members)
                .is_some_and(|members| {
                    group_is_exited(
                        leader,
                        &members,
                        aterm_uds::process::process_presence,
                        || {
                            u32::try_from(leader)
                                .ok()
                                .and_then(aterm_uds::process::group_members)
                        },
                    )
                });
            if ended { Ok(()) } else { Err(error) }
        }
        Err(error) => Err(error),
    }
}

/// Run native agy in a private home and terminal. `approve` must use the admitted request's
/// original callback to construct its handback. It shares the same authorization deadline.
/// The caller must verify the resulting grant's canonical account before publishing it.
pub fn run(
    program: &Path,
    profile: &Path,
    deadline: Instant,
    mut approve: impl FnMut(&Authorization, Instant) -> Result<Code, Error>,
) -> Result<UnverifiedGrant, Error> {
    remaining(deadline)?;
    if !program.is_absolute() || !program.is_file() || !profile.is_absolute() {
        return Err(Error::Profile);
    }
    aterm_types::fs_restricted::ensure_private_dir(profile).map_err(|_| Error::Profile)?;
    let claim = super::browser_window::ProfileLock::claim(profile).map_err(|_| Error::Profile)?;
    let gemini = profile.join(".gemini");
    let (master, slave, mut child) = aterm_pty::spawn_session_leader_on_pty(|_| {
        #[allow(clippy::disallowed_types, reason = "the controlling-terminal spawn seam takes StdCommand and applies inherit_nothing before spawning")]
        let mut cmd = std::process::Command::new(program);
        cmd.env_clear();
        for name in super::login_drive::PASSED {
            if *name != "HOME"
                && let Some(value) = std::env::var_os(name)
            {
                cmd.env(name, value);
            }
        }
        cmd.env("HOME", profile)
            .env("SSH_CONNECTION", "127.0.0.1 1 127.0.0.1 22")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .env("TERM", "dumb")
            .current_dir(profile)
            .arg("--gemini_dir")
            .arg(&gemini)
            .args([
                "--app_data_dir=antigravity-cli",
                "-p",
                "/usage",
                "--output-format",
                "json",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        Ok(cmd)
    })
    .map_err(|_| Error::Process)?;
    // Install cleanup before acquiring any other fallible resource.
    let exit = match aterm_uds::exitwatch::exit_fd(child.id()) {
        Some(exit) => exit,
        None => {
            if let Ok(pid) = i32::try_from(child.id()) {
                let _ = aterm_uds::process::kill_group_until_empty(pid);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Process);
        }
    };
    let mut child = Child {
        child,
        _slave: slave,
        master: std::fs::File::from(master),
        exit,
        group_ended: false,
    };
    let stdout = child.child.stdout.take().ok_or(Error::Process)?;
    let stderr = child.child.stderr.take().ok_or(Error::Process)?;
    aterm_pty::set_nonblocking(child.master.as_raw_fd(), true).map_err(|_| Error::Process)?;
    let (sent, received) = mpsc::sync_channel(16);
    for (index, mut stream) in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)]
        .into_iter()
        .enumerate()
    {
        let sent = sent.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 2048];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                    Ok(n) => {
                        if sent.send((index, SecretBytes::from(&chunk[..n]))).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    drop(sent);
    let mut output = [SecretBytes::default(), SecretBytes::default()];
    let mut approved = false;
    while !approved {
        let (index, chunk) =
            match received.recv_timeout(remaining(deadline)?.min(Duration::from_millis(100))) {
                Ok(chunk) => chunk,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
        if output.iter().map(SecretBytes::len).sum::<usize>() + chunk.len() > LIMIT {
            return Err(Error::Output);
        }
        let mut bytes = output[index].as_slice().to_vec();
        bytes.extend_from_slice(chunk.as_slice());
        output[index] = SecretBytes::from(bytes);
        let text = String::from_utf8_lossy(output[index].as_slice());
        let Some(start) = text.find("https://accounts.google.com/") else {
            continue;
        };
        let tail = &text[start..];
        let Some(end) = tail.find(|c: char| {
            c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\'')
        }) else {
            continue;
        };
        let request = Authorization::admit(&tail[..end])?;
        let code = approve(&request, deadline)?;
        if code.binding != request.binding {
            return Err(Error::Callback);
        }
        let mut line = code.value.as_slice().to_vec();
        line.push(b'\n');
        let line = SecretBytes::from(line);
        let mut bytes = line.as_slice();
        while !bytes.is_empty() {
            remaining(deadline)?;
            match child.master.write(bytes) {
                Ok(0) => return Err(Error::Process),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    aterm_uds::ready::until(
                        child.master.as_fd(),
                        aterm_uds::ready::Interest::Write,
                        deadline,
                    )
                    .map_err(|_| Error::Deadline)?;
                }
                Err(_) => return Err(Error::Process),
            }
        }
        approved = true;
    }
    // Keep draining while awaiting native completion; auth output is never returned or logged.
    loop {
        let slice = Instant::now() + remaining(deadline)?.min(Duration::from_millis(20));
        match aterm_uds::ready::until(child.exit.as_fd(), aterm_uds::ready::Interest::Read, slice) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return Err(Error::Process),
        }
        for _ in 0..16 {
            if received.try_recv().is_err() {
                break;
            }
        }
    }
    let pid = i32::try_from(child.child.id()).map_err(|_| Error::Process)?;
    finish_exited_group(pid).map_err(|_| Error::Process)?;
    child.group_ended = true;
    let status = child.child.wait().map_err(|_| Error::Process)?;
    // No live group member remains before wait reaps its leader; Drop must not signal a reused
    // pid after wait releases that leader's reserved group id.
    let path = gemini.join("antigravity-cli/antigravity-oauth-token");
    let grant = aterm_secrets::fsx::read_private(&path, LIMIT).map_err(|_| Error::Native)?;
    if !status.success() || grant.is_none_or(|bytes| bytes.is_empty()) {
        return Err(Error::Native);
    }
    Ok(UnverifiedGrant {
        path,
        program: program.to_owned(),
        _claim: claim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_token_requires_current_nested_schema_not_cached_identity() {
        let valid =
            br#"{"token":{"access_token":"synthetic-token"},"id_token_email":"wrong@example.com"}"#;
        assert_eq!(
            native_access_token(valid).unwrap().as_str(),
            Some("synthetic-token")
        );
        for invalid in [
            br#"{"access_token":"synthetic-token"}"#.as_slice(),
            br#"{"id_token_email":"person@example.com","id_token":"synthetic-jwt"}"#,
            br#"{"token":{"access_token":""}}"#,
            br#"{"token":{"access_token":"bad\r\nheader"}}"#,
        ] {
            assert!(native_access_token(invalid).is_err());
        }
    }

    #[test]
    fn account_proof_refuses_replaced_grant_and_changed_binding() {
        let state = aterm_tempfile::tempdir().unwrap();
        let profile = aterm_tempfile::tempdir().unwrap();
        let text = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
        let config = state.path().join("google-identities.toml");
        aterm_secrets::fsx::write_atomic(&config, text.as_bytes(), 0o600).unwrap();
        let revision = registry(state.path())
            .unwrap()
            .binding_revision("agy", "one");
        let gemini = profile.path().join(".gemini");
        let path = gemini.join("antigravity-cli/antigravity-oauth-token");
        aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
        let bytes = br#"{"token":{"access_token":"synthetic-token"}}"#;
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        // Synthetic proof exercises the post-verification publication guard, not Google login.
        let verified = VerifiedGoogleGrant {
            grant: UnverifiedGoogleGrant {
                native: UnverifiedGrant {
                    path: path.clone(),
                    program: PathBuf::from("/synthetic/agy"),
                    _claim: super::super::browser_window::ProfileLock::claim(profile.path())
                        .unwrap(),
                },
                expected_email: "person@example.com".into(),
                state: state.path().to_owned(),
                account: "one".into(),
                revision,
            },
            profile: super::super::google_profile::GoogleProfile {
                email: "person@example.com".into(),
                principal: [1; 32],
            },
            digest: grant_digest(bytes),
        };
        assert!(verified.path().is_ok());
        aterm_secrets::fsx::write_atomic(&path, b"replacement", 0o600).unwrap();
        assert!(verified.path().is_err());
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        assert!(verified.path().is_ok());
        let prepared = verified
            .prepare_worker(super::super::worker::Starter {
                exe: PathBuf::from("/inherited/agy"),
                env: vec![
                    ("HOME".into(), "/inherited/home".into()),
                    ("GEMINI_API_KEY".into(), "synthetic-key".into()),
                ],
                args: Ok(vec![
                    "-gemini_dir=/inherited".into(),
                    "--app_data_dir".into(),
                    "old".into(),
                    "--mode".into(),
                    "plan".into(),
                ]),
                unset: Vec::new(),
            })
            .unwrap();
        let starter = prepared.starter().unwrap();
        assert_eq!(starter.exe, Path::new("/synthetic/agy"));
        assert_eq!(
            starter.args.as_ref().unwrap(),
            &vec![
                "--mode".to_owned(),
                "plan".into(),
                "--gemini_dir".into(),
                gemini.to_str().unwrap().into(),
                "--app_data_dir=antigravity-cli".into()
            ]
        );
        assert!(
            starter
                .env
                .iter()
                .any(|(k, v)| k == "HOME" && v == profile.path().to_str().unwrap())
        );
        assert!(!starter.env.iter().any(|(k, _)| k == "GEMINI_API_KEY"));
        assert!(starter.unset.contains(&"GEMINI_API_KEY"));
        assert!(super::super::browser_window::ProfileLock::claim(profile.path()).is_err());
        aterm_secrets::fsx::write_atomic(&path, b"replacement", 0o600).unwrap();
        assert!(prepared.starter().is_err());
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(
            &config,
            text.replace("person@", "other@").as_bytes(),
            0o600,
        )
        .unwrap();
        assert!(prepared.starter().is_err());
    }

    #[test]
    fn worker_profile_rebinding_respects_flag_looking_values_and_refuses_unknown_flags() {
        let bind = |flags: &[&str]| {
            super::super::upgrade_agy::bound_worker_flags(
                &flags.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                Path::new("/private/.gemini"),
            )
        };
        let flags = bind(&[
            "--model",
            "--gemini_dir=not-a-profile",
            "--gemini_dir",
            "--mode=not-a-permission",
            "--mode=plan",
        ])
        .unwrap();
        assert_eq!(
            &flags[..3],
            ["--model", "--gemini_dir=not-a-profile", "--mode=plan"]
        );
        for flags in [
            &["--unknown"][..],
            &["--gemini_dir"],
            &["--settings", "{}"],
            &["--", "operand"],
        ] {
            assert!(bind(flags).is_err());
        }
    }

    fn request(state: &str) -> String {
        let scope = SCOPES.join("%20");
        format!(
            "https://accounts.google.com/o/oauth2/auth?client_id=native-fixture.apps.googleusercontent.com&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&access_type=offline&code_challenge_method=S256&code_challenge=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&state={state}&scope={scope}"
        )
    }

    #[test]
    fn google_callback_cannot_cross_native_requests_or_change_its_destination() {
        let first = Authorization::admit(&request("first-state")).unwrap();
        let second = Authorization::admit(&request("second-state")).unwrap();
        let code = first
            .callback(
                "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state",
            )
            .unwrap();
        assert_eq!(code.value.as_str(), Some("fixture-code"));
        assert_eq!(code.binding, first.binding);
        assert_ne!(code.binding, second.binding);
        for callback in [
            "https://antigravity.google/oauth-callback?code=fixture-code&state=second-state",
            "https://antigravity.google/oauth-callback?code=fixture-code",
            "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state&state=first-state",
            "https://antigravity.google/oauth-callback?code=fixture-code%0Anext&state=first-state",
            "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state&error=denied",
            "https://antigravity.google.evil.test/oauth-callback?code=fixture-code&state=first-state",
            "https://antigravity.google/elsewhere?code=fixture-code&state=first-state",
        ] {
            assert!(first.callback(callback).is_err());
        }
        for url in [
            request("first-state") + "&client_id=other.apps.googleusercontent.com",
            request("first-state").replace("S256", "plain"),
            request("first-state").replace("antigravity.google", "attacker.test"),
            request("first-state").replace("auth/aicode", "auth/gmail.readonly"),
            request("first-state").replace("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "short"),
        ] {
            assert!(Authorization::admit(&url).is_err());
        }
    }

    #[test]
    fn expired_native_attempt_starts_no_child_and_creates_no_profile() {
        let root = aterm_tempfile::tempdir().unwrap();
        let profile = root.path().join("not-created");
        let result = run(
            Path::new("/usr/bin/true"),
            &profile,
            Instant::now(),
            |_, _| panic!("expired attempt invoked approval"),
        );
        assert!(matches!(result, Err(Error::Deadline)));
        assert!(!profile.exists());
    }

    #[test]
    fn explicit_google_path_refuses_missing_credentials_before_native_launch() {
        let state = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let registry = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
        aterm_uds::private_file::write_private(
            state.path(),
            "google-identities.toml",
            registry.as_bytes(),
        )
        .unwrap();
        let result = run_google(
            Path::new("/not-a-native-program"),
            state.path(),
            home.path(),
            "one",
            Instant::now() + Duration::from_secs(30),
        );
        assert!(matches!(
            result,
            Err(GoogleLoginError::Credentials(
                super::super::google_credentials::Error::MissingReference
            ))
        ));
        assert!(!state.path().join("agy-logins").exists());
        assert!(!state.path().join("google-identities").exists());
        let result = run_google(
            Path::new("/not-a-native-program"),
            state.path(),
            home.path(),
            "other",
            Instant::now() + Duration::from_secs(30),
        );
        assert!(matches!(result, Err(GoogleLoginError::Configuration)));
    }

    #[test]
    fn existing_private_native_grant_reaches_native_route_without_a_google_password() {
        let state = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let text = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
        aterm_uds::private_file::write_private(
            state.path(),
            "google-identities.toml",
            text.as_bytes(),
        )
        .unwrap();
        let registry = super::registry(state.path()).unwrap();
        let identity = registry.resolve("agy", "one").unwrap().0;
        let path = native_profile(state.path(), "one", identity)
            .join(".gemini/antigravity-cli/antigravity-oauth-token");
        aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
        aterm_secrets::fsx::write_atomic(&path, b"synthetic-native-grant", 0o600).unwrap();
        // Deliberately stop at native program admission: this proves password independence,
        // not that the synthetic grant is valid or accepted by the provider.
        let result = run_google(
            Path::new("/not-a-native-program"),
            state.path(),
            home.path(),
            "one",
            Instant::now() + Duration::from_secs(30),
        );
        assert!(matches!(
            result,
            Err(GoogleLoginError::Native(Error::Profile))
        ));
    }

    #[test]
    fn only_a_stable_complete_group_of_exited_members_settles_permission_denial() {
        use aterm_uds::process::{GroupMembers, ProcessPresence};

        let mut group = GroupMembers {
            pids: vec![41, 42],
            complete: true,
        };
        let exited = |pid| {
            if pid == 41 {
                ProcessPresence::Exited
            } else {
                ProcessPresence::Absent
            }
        };
        let snapshot = || Some(group.clone());
        assert!(group_is_exited(41, &group, exited, snapshot));
        assert!(group_is_exited(41, &group, exited, || {
            Some(GroupMembers {
                pids: vec![41],
                complete: true,
            })
        }));
        assert!(!group_is_exited(41, &group, exited, || {
            Some(GroupMembers {
                pids: vec![41, 42, 43],
                complete: true,
            })
        }));
        assert!(!group_is_exited(41, &group, exited, || {
            Some(GroupMembers {
                pids: vec![41, 42],
                complete: false,
            })
        }));
        assert!(!group_is_exited(41, &group, exited, || {
            Some(GroupMembers {
                pids: vec![42],
                complete: true,
            })
        }));
        group.complete = false;
        assert!(!group_is_exited(41, &group, exited, || None));
        group.complete = true;
        group.pids.remove(0);
        assert!(!group_is_exited(41, &group, exited, || None));
        group.pids.insert(0, 41);
        assert!(!group_is_exited(
            41,
            &group,
            |pid| {
                if pid == 42 {
                    ProcessPresence::Present
                } else {
                    ProcessPresence::Exited
                }
            },
            || None,
        ));
        assert!(!group_is_exited(
            41,
            &group,
            |pid| {
                if pid == 42 {
                    ProcessPresence::Unknown
                } else {
                    ProcessPresence::Exited
                }
            },
            || None,
        ));
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn xnu_zombie_only_group_is_a_completed_cleanup() {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;

        struct Forked(i32);
        impl Drop for Forked {
            fn drop(&mut self) {
                if self.0 <= 1 {
                    return;
                }
                let mut status = 0;
                // SAFETY: this test's child leads its own group and remains unreaped here.
                unsafe {
                    libc::kill(-self.0, libc::SIGKILL);
                    libc::waitpid(self.0, &raw mut status, 0);
                }
            }
        }

        let (mut proof_r, proof_w) = std::io::pipe().unwrap();
        let (proof_r_fd, proof_w_fd) = (proof_r.as_raw_fd(), proof_w.as_raw_fd());
        // SAFETY: the child calls only async-signal-safe functions (`close`, `setpgid`, `write`,
        // `_exit`) on descriptors prepared before the fork, then exits without unwinding.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            // SAFETY: the forked child follows the contract above.
            unsafe {
                libc::close(proof_r_fd);
                let ok = libc::setpgid(0, 0) == 0;
                let byte = u8::from(ok);
                libc::write(proof_w_fd, (&raw const byte).cast(), 1);
                libc::close(proof_w_fd);
                libc::_exit(i32::from(!ok));
            }
        }
        assert!(pid > 1, "a forked child has a positive process ID");
        let mut child = Forked(pid);
        drop(proof_w);
        let mut proof = [0u8; 1];
        proof_r.read_exact(&mut proof).unwrap();
        assert_eq!(proof, [1], "the child made itself its group leader");
        drop(proof_r);

        let until = Instant::now() + Duration::from_secs(60);
        loop {
            // SAFETY: signal zero observes this child's positive, uniquely held group.
            let found = unsafe { libc::kill(-pid, 0) };
            let error = std::io::Error::last_os_error();
            if found < 0 && error.raw_os_error() == Some(libc::EPERM) {
                break;
            }
            assert!(
                Instant::now() < until,
                "the child did not reach XNU's zombie-only group state: {error}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let refused = aterm_uds::process::kill_group_until_empty(pid).unwrap_err();
        assert_eq!(
            refused.raw_os_error(),
            Some(libc::EPERM),
            "the former direct cleanup rejects an ordinary exited group"
        );
        finish_exited_group(pid).expect("a complete zombie-only group is already ended");

        let mut status = 0;
        // SAFETY: this test owns and reaps this child exactly once.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        child.0 = 0;
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }

    #[test]
    #[ignore = "contacts the installed native agy anonymously; ATERM_TEST_AGY names its pinned binary"]
    fn native_headless_handoff_is_admitted_and_cancellation_releases_its_profile() {
        let program = std::env::var_os("ATERM_TEST_AGY")
            .map(PathBuf::from)
            .expect("ATERM_TEST_AGY is required");
        let profile = aterm_tempfile::tempdir().unwrap();
        let mut observed = false;
        let result = run(
            &program,
            profile.path(),
            Instant::now() + Duration::from_secs(30),
            |request, _| {
                assert!(
                    request
                        .url()
                        .starts_with("https://accounts.google.com/o/oauth2/auth?")
                );
                observed = true;
                Err(Error::Callback)
            },
        );
        assert!(observed, "native handoff was not observed");
        assert!(matches!(result, Err(Error::Callback)));
        let _claim = super::super::browser_window::ProfileLock::claim(profile.path()).unwrap();
    }
}
