// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Constructor-owned native host world for the joined SDK protocol proof.
//! Synthetic issuer material qualifies custody, never a provider or real user.

use super::super::{
    account_work, accounts,
    cli::Env,
    codex_alab, codex_grant_owner as owner,
    native_act::{Image, Process},
    native_custody_platform as platform,
};
use super::DirectoryIdentity;
use super::row_fixture::NativeRowRecoveryFixture;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
#[cfg(test)]
#[path = "codex_enrollment_source_proof_tests.rs"]
mod tests;

pub const SOURCE_PROOF_TASK: &str = "ATERM-ISOLATED-SOURCE-PROOF/3 ";
const DOMAIN: &[u8] = b"aterm/isolated-source-proof-launch/key/v3\0";

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Candidate {
    path: PathBuf,
    device: u64,
    inode: u64,
}

struct CandidateLease {
    image: Image,
    file: File,
}
impl CandidateLease {
    fn qualify(path: &Path) -> Result<(Candidate, Self), String> {
        let file = File::open(path).map_err(|_| "source proof candidate unavailable")?;
        let path = std::fs::canonicalize(path).map_err(|_| "source proof candidate unavailable")?;
        let image = Image::at(&path).ok_or("source proof candidate unavailable")?;
        if !codex_alab::isolated_source_proof_candidate(&path)
            .map_err(|_| "source proof candidate unavailable")?
            || !astream_sys::file_matches_path(&file, &path).unwrap_or(false)
        {
            return Err("package is not a current isolated source proof candidate".into());
        }
        Ok((
            Candidate {
                path,
                device: image.device,
                inode: image.inode,
            },
            Self { image, file },
        ))
    }
    fn admit(candidate: &Candidate) -> Result<Self, String> {
        let (observed, lease) = Self::qualify(&candidate.path)?;
        if &observed != candidate {
            return Err("source proof candidate changed".into());
        }
        Ok(lease)
    }
    fn current(&self, candidate: &Candidate) -> bool {
        Image::at(&candidate.path).as_ref() == Some(&self.image)
            && astream_sys::file_matches_path(&self.file, &candidate.path).unwrap_or(false)
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    Source,
    Retained,
    RetainedDistinct,
    RetainedResources,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    schema: u32,
    purpose: Purpose,
    nonce: String,
    root: PathBuf,
    root_identity: DirectoryIdentity,
    home_identity: DirectoryIdentity,
    state: PathBuf,
    candidate: Candidate,
    replacement: Option<Candidate>,
    auth: PathBuf,
    binding: owner::GrantBinding,
    driver: Driver,
    expires: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Driver {
    pid: u32,
    birth: u64,
    parent: u32,
    group: u32,
    session: u32,
    image: Vec<u8>,
    device: u64,
    inode: u64,
}
impl Driver {
    fn capture(process: Process) -> Self {
        Self {
            pid: process.pid,
            birth: process.birth,
            parent: process.parent,
            group: process.group,
            session: process.session,
            image: process.image.path,
            device: process.image.device,
            inode: process.image.inode,
        }
    }
    fn current(&self, pid: u32) -> bool {
        pid == self.pid
            && Process::observe(pid).is_some_and(|process| {
                process.birth == self.birth
                    && process.parent == self.parent
                    && process.group == self.group
                    && process.session == self.session
                    && process.image.path == self.image
                    && process.image.device == self.device
                    && process.image.inode == self.inode
            })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    body: Body,
    mac: String,
}

/// An actual App launch may obtain this only after authenticating the owned
/// world's ticket. It is not cloneable or constructible from wire fields.
pub struct SourceProofLaunch {
    body: Body,
    candidate: CandidateLease,
    replacement: Option<CandidateLease>,
    scope: account_work::Scope,
}

/// An unfinished synthetic received boundary retains the actual encrypted
/// writer/Scope carrier; retry is storage-only and never creates an exchange.
pub struct SourceProofSettlement(Box<SettlementState>);
struct SettlementState {
    // Actual encrypted/plaintext/resource custody retires before the final
    // genuine counted operation capture, even on explicit fixture failure.
    error: owner::OwnerRefreshError,
    binding: owner::GrantBinding,
    expected: u64,
    root: PathBuf,
    root_identity: DirectoryIdentity,
    home_identity: DirectoryIdentity,
    state: PathBuf,
    scope: account_work::Scope,
}
pub enum SourceProofAdvance {
    Current(u64),
    Pending(SourceProofSettlement),
    Refused,
}
impl SourceProofSettlement {
    pub fn retry(mut self, env: &Env) -> SourceProofAdvance {
        if env.home.as_deref() != Some(self.0.root.join("home").as_path())
            || env.state != self.0.state
            || env.config.as_ref() != Some(&self.0.root.join("cfg/aterm/aterm.toml"))
            || DirectoryIdentity::capture(&self.0.root).as_ref().ok() != Some(&self.0.root_identity)
            || DirectoryIdentity::capture(&self.0.root.join("home"))
                .as_ref()
                .ok()
                != Some(&self.0.home_identity)
        {
            return SourceProofAdvance::Pending(self);
        }
        let error = std::mem::replace(&mut self.0.error, owner::OwnerRefreshError::Unavailable);
        let settled = match error {
            owner::OwnerRefreshError::Retaining(retention) => match retention.retry() {
                Ok(continuation) => continuation.reconcile(env, &self.0.scope),
                Err(retention) => Err(owner::OwnerRefreshError::Retaining(retention)),
            },
            owner::OwnerRefreshError::Unavailable => {
                owner::reconcile_source_proof(env, &self.0.binding, &self.0.scope)
            }
        };
        if let Err(error) = settled {
            self.0.error = error;
            return SourceProofAdvance::Pending(self);
        }
        let generation =
            owner::with_maintenance_owner(env, &self.0.binding, &self.0.scope, |mut grant| {
                Ok(grant
                    .anchor()
                    .read()?
                    .ok_or("proof witness missing")?
                    .revision
                    .generation)
            });
        match generation {
            Ok(generation) if generation >= self.0.expected => {
                SourceProofAdvance::Current(generation)
            }
            _ => SourceProofAdvance::Pending(self),
        }
    }
}

impl SourceProofLaunch {
    /// Authenticated fixture purpose only; native construction and process
    /// admission still independently check this original launch.
    pub fn retained_frontend(&self) -> bool {
        matches!(
            self.body.purpose,
            Purpose::Retained | Purpose::RetainedDistinct | Purpose::RetainedResources
        )
    }

    pub fn resources_frontend(&self) -> bool {
        matches!(self.body.purpose, Purpose::RetainedResources)
    }

    pub fn advance(&self, env: &Env) -> SourceProofAdvance {
        let Ok((binding, scope)) =
            self.with_refresh_fixture(env, |binding, scope| Ok((binding.clone(), scope.clone())))
        else {
            return SourceProofAdvance::Refused;
        };
        match owner::advance_source_proof(self, env) {
            (_, Ok(generation)) => SourceProofAdvance::Current(generation),
            (Some(expected), Err(error)) => {
                SourceProofAdvance::Pending(SourceProofSettlement(Box::new(SettlementState {
                    binding,
                    scope,
                    expected,
                    error,
                    root: self.body.root.clone(),
                    root_identity: self.body.root_identity.clone(),
                    home_identity: self.body.home_identity.clone(),
                    state: self.body.state.clone(),
                })))
            }
            (None, Err(_)) => SourceProofAdvance::Refused,
        }
    }
    pub fn admit(
        env: &Env,
        frame: &str,
        app: &account_work::Lifetime,
        scope: &account_work::Scope,
        sid: &str,
    ) -> Result<Self, String> {
        if scope.cancelled() || !scope.cancellation().owned_by(app) {
            return Err("source proof admission unavailable".into());
        }
        let encoded = frame
            .strip_prefix(SOURCE_PROOF_TASK)
            .ok_or("not a proof task")?;
        if encoded.len() > 16 * 1024 || scope.cancelled() {
            return Err("source proof admission unavailable".into());
        }
        let bytes =
            aterm_codec::hex::decode_vec(encoded.as_bytes()).ok_or("invalid proof ticket")?;
        let envelope: Envelope =
            aterm_json::from_slice(&bytes).map_err(|_| "invalid proof ticket")?;
        let home = env.home.as_deref().ok_or("source proof home unavailable")?;
        let identity =
            platform::machine_identity(home)?.ok_or("source proof identity unavailable")?;
        let expected = owner::machine_mac(
            &identity,
            DOMAIN,
            &aterm_json::to_vec(&envelope.body).map_err(|_| "invalid proof ticket")?,
        )?;
        if !aterm_digest::ct_eq(expected.as_bytes(), envelope.mac.as_bytes()) {
            return Err("source proof ticket authentication failed".into());
        }
        let body = envelope.body;
        if body.schema != 3
            || body.replacement.is_some() && !matches!(body.purpose, Purpose::Retained)
            || body.nonce.len() != 32
            || !body
                .nonce
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || home != body.root.join("home")
            || env.state != body.state
            || env.config.as_ref() != Some(&body.root.join("cfg/aterm/aterm.toml"))
            || DirectoryIdentity::capture(&body.root)? != body.root_identity
            || DirectoryIdentity::capture(home)? != body.home_identity
            || env.now >= body.expires
        {
            return Err("source proof world changed".into());
        }
        let candidate = CandidateLease::admit(&body.candidate)?;
        let replacement = body
            .replacement
            .as_ref()
            .map(CandidateLease::admit)
            .transpose()?;
        // A nonce belongs to one actual Rust host session/launch. A lost
        // outcome never permits a second child; absence of a reply is not an
        // unstarted child. The retained private world remains diagnostic custody.
        let name = format!("proof-{}.claimed", body.nonce);
        if !aterm_uds::private_file::write_private_new(
            &body.root,
            &name,
            sid.as_bytes(),
            |_| Ok(()),
        )
        .map_err(|_| "source proof launch outcome unavailable")?
        {
            return Err("source proof launch already claimed".into());
        }
        let launch = Self {
            body,
            candidate,
            replacement,
            scope: scope.clone(),
        };
        launch.current()?;
        Ok(launch)
    }

    pub fn current(&self) -> Result<(), String> {
        if self.scope.cancelled()
            || crate::supervise::limit::unix_now() >= self.body.expires
            || DirectoryIdentity::capture(&self.body.root)? != self.body.root_identity
            || DirectoryIdentity::capture(&self.body.root.join("home"))? != self.body.home_identity
            || !self.candidate.current(&self.body.candidate)
            || !match (&self.replacement, &self.body.replacement) {
                (Some(lease), Some(candidate)) => lease.current(candidate),
                (None, None) => true,
                _ => false,
            }
        {
            return Err("source proof launch changed".into());
        }
        Ok(())
    }

    pub fn candidate(&self) -> &Path {
        &self.body.candidate.path
    }
    /// Data selecting an already authenticated replacement, never an image
    /// admission derived from an SDK checkpoint or supplied executable name.
    pub fn replacement_candidate(&self) -> Option<&Path> {
        self.body
            .replacement
            .as_ref()
            .map(|candidate| candidate.path.as_path())
    }
    /// Native observations retained by this authenticated launch. This fixture
    /// policy admits only these actual files and grants no Source or provider use.
    pub fn retained_images(&self) -> Result<Vec<Image>, String> {
        self.current()?;
        if !self.retained_frontend() {
            return Err("retained image policy requires its authenticated purpose".into());
        }
        let mut images = vec![self.candidate.image.clone()];
        if let Some(replacement) = &self.replacement {
            images.push(replacement.image.clone());
        }
        Ok(images)
    }
    /// Exact process-image multiplicity selected by the authenticated driver,
    /// independently of frontend checkpoint data or child-reported processes.
    pub fn retained_backend_images(&self) -> Result<Vec<Image>, String> {
        let mut images = self.retained_images()?;
        if matches!(
            self.body.purpose,
            Purpose::RetainedDistinct | Purpose::RetainedResources
        ) {
            images.push(self.candidate.image.clone());
        }
        Ok(images)
    }
    pub fn directory(&self) -> &Path {
        &self.body.root
    }
    pub fn auth_file(&self) -> &Path {
        &self.body.auth
    }
    pub fn guardian(&self) -> PathBuf {
        self.body.root.join("proof-guardian.sock")
    }
    pub fn guardian_peer_current(&self, pid: u32) -> bool {
        self.body.driver.current(pid)
    }

    /// Failure-only kernel metadata; never exposes the signed body or image pathname.
    pub fn guardian_peer_failure(&self, pid: u32) -> String {
        let expected = &self.body.driver;
        if pid != expected.pid {
            return format!("pid expected={} observed={pid}", expected.pid);
        }
        let Some(observed) = Process::observe(pid) else {
            return format!("process observation unavailable pid={pid}");
        };
        let mut failures = Vec::new();
        for (name, wanted, actual) in [
            ("birth", expected.birth, observed.birth),
            (
                "parent",
                u64::from(expected.parent),
                u64::from(observed.parent),
            ),
            ("pgid", u64::from(expected.group), u64::from(observed.group)),
            (
                "sid",
                u64::from(expected.session),
                u64::from(observed.session),
            ),
            ("image_device", expected.device, observed.image.device),
            ("image_inode", expected.inode, observed.image.inode),
        ] {
            if wanted != actual {
                failures.push(format!("{name} expected={wanted} observed={actual}"));
            }
        }
        if observed.image.path != expected.image {
            failures.push("image_path changed".into());
        }
        if failures.is_empty() {
            failures.push("observation changed after refusal".into());
        }
        format!("pid={pid} {}", failures.join("; "))
    }

    pub(in crate::harness) fn with_refresh_fixture<R>(
        &self,
        env: &Env,
        run: impl FnOnce(&owner::GrantBinding, &account_work::Scope) -> Result<R, String>,
    ) -> Result<R, String> {
        self.current()?;
        if env.home.as_deref() != Some(self.body.root.join("home").as_path())
            || env.state != self.body.state
        {
            return Err("source proof maintenance namespace changed".into());
        }
        let store = env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("fixture store unavailable")?;
        let roster = env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("fixture roster unavailable")?;
        let records = store
            .peek_account_records(&roster, env.now)
            .map_err(|_| "fixture records unavailable")?;
        if !records.notes.is_empty() {
            return Err("fixture canonical records uncertain".into());
        }
        let generation = records
            .logins
            .labels
            .get("alpha")
            .map(|binding| binding.last_success_generation)
            .ok_or("fixture binding unavailable")?;
        let binding = records
            .logins
            .enrollment
            .published_grant("alpha", generation)
            .ok_or("fixture native publication unavailable")?;
        if binding != self.body.binding
            || records
                .roster
                .row("alpha")
                .and_then(|row| row.dir.as_ref())
                .map(|directory| Path::new(directory).join("auth.json"))
                .as_ref()
                != Some(&self.body.auth)
        {
            return Err("source proof owner changed".into());
        }
        let result = run(&binding, &self.scope)?;
        self.current()?;
        Ok(result)
    }
}

/// The fixture owns the entire fresh machine namespace until the real host and
/// child have stopped. It accepts no caller path, document, binding or image.
pub struct NativeSourceProofWorld {
    fixture: NativeRowRecoveryFixture,
}

impl NativeSourceProofWorld {
    /// Allocate and authenticate both original synthetic response occurrences.
    /// Publication still uses the real counted native recovery transaction.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            fixture: NativeRowRecoveryFixture::source_host()?,
        })
    }

    pub fn directory(&self) -> &Path {
        self.fixture.directory()
    }
    pub fn keep(self) -> PathBuf {
        self.fixture.keep()
    }

    pub fn env(&self) -> &super::super::cli::Env {
        self.fixture.env()
    }

    /// Publish the two original occurrences without a vendor request. No raw
    /// data constructor or arbitrary retained-response adoption is exposed.
    pub fn publish(&self) -> Result<(), String> {
        self.fixture.publish_source_rows()
    }

    /// Sign only this constructor's actual published source and exact opened
    /// qualification image. The private source paths are not wire authority.
    pub fn launch_task(&self, candidate: &Path) -> Result<String, String> {
        self.task(candidate, Purpose::Source, None)
    }
    pub fn retained_task(&self, candidate: &Path) -> Result<String, String> {
        self.task(candidate, Purpose::Retained, None)
    }
    pub fn retained_distinct_task(&self, candidate: &Path) -> Result<String, String> {
        self.task(candidate, Purpose::RetainedDistinct, None)
    }
    pub fn retained_resources_task(&self, candidate: &Path) -> Result<String, String> {
        self.task(candidate, Purpose::RetainedResources, None)
    }
    pub fn retained_upgrade_task(
        &self,
        candidate: &Path,
        replacement: &Path,
    ) -> Result<String, String> {
        self.task(candidate, Purpose::Retained, Some(replacement))
    }
    fn task(
        &self,
        candidate: &Path,
        purpose: Purpose,
        replacement: Option<&Path>,
    ) -> Result<String, String> {
        let (candidate, candidate_lease) = CandidateLease::qualify(candidate)?;
        let replacement = replacement.map(CandidateLease::qualify).transpose()?;
        let (replacement, replacement_lease) = match replacement {
            Some((candidate, lease)) => (Some(candidate), Some(lease)),
            None => (None, None),
        };
        let env = self.env();
        let store = env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("fixture store unavailable")?;
        let roster = env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("fixture roster unavailable")?;
        let records = store
            .peek_account_records(&roster, env.now)
            .map_err(|_| "fixture records unavailable")?;
        let directory = records
            .roster
            .row("alpha")
            .and_then(|row| row.dir.as_ref())
            .ok_or("fixture source is not published")?;
        let generation = records
            .logins
            .labels
            .get("alpha")
            .map(|binding| binding.last_success_generation)
            .ok_or("fixture binding unavailable")?;
        let binding = records
            .logins
            .enrollment
            .published_grant("alpha", generation)
            .ok_or("fixture native publication unavailable")?;
        let body = Body {
            schema: 3,
            purpose,
            nonce: aterm_uds::rand::hex_token::<16>().map_err(|_| "fixture nonce unavailable")?,
            root: self.directory().to_path_buf(),
            root_identity: DirectoryIdentity::capture(self.directory())?,
            home_identity: DirectoryIdentity::capture(
                env.home.as_deref().ok_or("fixture home unavailable")?,
            )?,
            state: env.state.clone(),
            candidate,
            replacement,
            auth: Path::new(directory).join("auth.json"),
            binding,
            driver: Driver::capture(
                Process::observe(std::process::id()).ok_or("fixture driver unavailable")?,
            ),
            expires: crate::supervise::limit::unix_now().saturating_add(30 * 60),
        };
        let identity =
            platform::machine_identity(env.home.as_deref().ok_or("fixture home unavailable")?)?
                .ok_or("fixture identity unavailable")?;
        let mac = owner::machine_mac(
            &identity,
            DOMAIN,
            &aterm_json::to_vec(&body).map_err(|_| "fixture ticket encoding failed")?,
        )?;
        let envelope = Envelope { body, mac };
        let bytes = aterm_json::to_vec(&envelope).map_err(|_| "fixture ticket encoding failed")?;
        if bytes.len() > 8 * 1024
            || !candidate_lease.current(&envelope.body.candidate)
            || !match (&replacement_lease, &envelope.body.replacement) {
                (Some(lease), Some(candidate)) => lease.current(candidate),
                (None, None) => true,
                _ => false,
            }
        {
            return Err("source proof candidate changed during ticket creation".into());
        }
        Ok(format!(
            "{SOURCE_PROOF_TASK}{}",
            aterm_codec::hex::encode_lower(&bytes)
        ))
    }
}
