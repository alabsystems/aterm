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

pub const SOURCE_PROOF_TASK: &str = "ATERM-ISOLATED-SOURCE-PROOF/1 ";
const DOMAIN: &[u8] = b"aterm/isolated-source-proof-launch/key/v1\0";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    schema: u32,
    nonce: String,
    root: PathBuf,
    root_identity: DirectoryIdentity,
    home_identity: DirectoryIdentity,
    state: PathBuf,
    candidate: PathBuf,
    device: u64,
    inode: u64,
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
    image: Image,
    candidate: File,
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
        if body.schema != 1
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
        let image = Image::at(&body.candidate).ok_or("source proof candidate unavailable")?;
        if image.device != body.device
            || image.inode != body.inode
            || !codex_alab::isolated_source_proof_candidate(&body.candidate)
                .map_err(|_| "source proof candidate unavailable")?
        {
            return Err("source proof candidate changed".into());
        }
        let candidate =
            File::open(&body.candidate).map_err(|_| "source proof candidate unavailable")?;
        if !astream_sys::file_matches_path(&candidate, &body.candidate).unwrap_or(false) {
            return Err("source proof candidate changed".into());
        }
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
            image,
            candidate,
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
            || Image::at(&self.body.candidate).as_ref() != Some(&self.image)
            || !astream_sys::file_matches_path(&self.candidate, &self.body.candidate)
                .unwrap_or(false)
        {
            return Err("source proof launch changed".into());
        }
        Ok(())
    }

    pub fn candidate(&self) -> &Path {
        &self.body.candidate
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
        let candidate_file =
            File::open(candidate).map_err(|_| "source proof candidate unavailable")?;
        if !codex_alab::isolated_source_proof_candidate(candidate)
            .map_err(|_| "source proof candidate unavailable")?
        {
            return Err("package is not an isolated source proof candidate".into());
        }
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
        let candidate =
            std::fs::canonicalize(candidate).map_err(|_| "source proof candidate unavailable")?;
        let image = Image::at(&candidate).ok_or("source proof candidate unavailable")?;
        let body = Body {
            schema: 1,
            nonce: aterm_uds::rand::hex_token::<16>().map_err(|_| "fixture nonce unavailable")?,
            root: self.directory().to_path_buf(),
            root_identity: DirectoryIdentity::capture(self.directory())?,
            home_identity: DirectoryIdentity::capture(
                env.home.as_deref().ok_or("fixture home unavailable")?,
            )?,
            state: env.state.clone(),
            candidate: candidate.clone(),
            device: image.device,
            inode: image.inode,
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
        let bytes = aterm_json::to_vec(&Envelope { body, mac })
            .map_err(|_| "fixture ticket encoding failed")?;
        if !astream_sys::file_matches_path(&candidate_file, &candidate).unwrap_or(false) {
            return Err("source proof candidate changed during ticket creation".into());
        }
        Ok(format!(
            "{SOURCE_PROOF_TASK}{}",
            aterm_codec::hex::encode_lower(&bytes)
        ))
    }
}
