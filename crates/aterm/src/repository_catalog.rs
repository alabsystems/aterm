// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admin-owned inventory upkeep. The owned worker receives no administrative key:
//! it fetches only; a later fresh admin callback conditionally publishes the result.

use agh::identity::{EnvSource, ProcessEnv, TokenKind};
use agh::page::{Consistency, End, LinkScope, ListOpts};
use agh::policy::{RouteRequest, RouteTarget, Router};
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::{Call, GithubBuilder};
use aterm_machines::init_github::{RepositoryCatalog, RepositoryCatalogVisibility};
use aterm_secrets::{
    SecretBytes,
    client::{self, Admin, Config, Paths},
    text::Audience,
};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const NAME: &str = aterm_secrets::entry_class::GITHUB_REPOSITORIES;
const TOKEN: &str = "gh_access_token";
const PROFILE: &str = "private-catalog";
const TOKEN_ENV: &str = "ATERM_ADMITTED_CATALOG_TOKEN";
const MAX: usize = 1 << 20;
const FETCH: Duration = Duration::from_secs(20);
const LOOK: Duration = Duration::from_secs(15 * 60);
const RETRY: Duration = Duration::from_secs(60);
const RESULT_AGE: Duration = Duration::from_secs(120);

#[cfg(test)]
#[path = "repository_catalog_tests.rs"]
pub(super) mod tests;

#[derive(Clone, PartialEq, Eq)]
struct Binding {
    home: std::path::PathBuf,
    state: std::path::PathBuf,
    config: Config,
    catalog: [u8; 32],
    token: [u8; 32],
}
struct Input {
    binding: Binding,
    catalog: Vec<u8>,
    token: SecretBytes,
}
#[derive(Clone)]
struct Candidate {
    binding: Binding,
    bytes: Vec<u8>,
    visibility: RepositoryCatalogVisibility,
    observed: usize,
    observed_at: Instant,
}
type Fetcher = dyn Fn(Input, Arc<AtomicBool>) -> Result<Candidate, String> + Send + Sync;

fn allowed(paths: &Paths) -> bool {
    !aterm_secrets::service::subscriber_turned_off(&paths.home)
        && aterm_secrets::swarm::enabled_in_home(&paths.home).unwrap_or(false)
}

fn entry_digest(entry: &aterm_secrets::text::Entry) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    let flags = [u8::from(entry.if_absent), u8::from(entry.anchor.is_some())];
    for part in [
        entry.value.as_slice(),
        flags.as_slice(),
        entry.anchor.as_deref().unwrap_or("").as_bytes(),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize()
}

impl Input {
    fn capture(paths: &Paths, admin: &Admin) -> Result<Self, String> {
        if !allowed(paths)
            || admin
                .waiting()
                .iter()
                .any(|name| name == NAME || name == TOKEN)
        {
            return Err("private repository catalog upkeep is held".into());
        }
        let catalog = admin
            .published_entry(&Audience::Everyone, NAME)
            .ok_or("private repository catalog is not published")?;
        let token = admin
            .published_entry(&Audience::Everyone, TOKEN)
            .ok_or("development credential is not published")?;
        // Do not overwrite a host override or publish through a pending administrative draft.
        for entry in [catalog, token] {
            if admin.doc.entry(&Audience::Everyone, &entry.name) != Some(entry)
                || admin
                    .doc
                    .entries_ref()
                    .any(|other| other.name == entry.name && other.audience != Audience::Everyone)
            {
                return Err(
                    "repository source has a separate or pending administrative policy".into(),
                );
            }
        }
        let bytes = token.value.as_slice();
        let bytes = bytes
            .strip_suffix(b"\r\n")
            .or_else(|| bytes.strip_suffix(b"\n"))
            .unwrap_or(bytes);
        TokenKind::of(bytes).map_err(|_| "repository catalog credential cannot authenticate")?;
        Ok(Self {
            binding: Binding {
                home: paths.home.clone(),
                state: paths.dir.clone(),
                config: admin.config.clone(),
                catalog: entry_digest(catalog),
                token: entry_digest(token),
            },
            catalog: catalog.value.as_slice().to_vec(),
            token: SecretBytes::from(bytes),
        })
    }
}

struct Job {
    serial: u64,
    input: Input,
    stopped: Arc<AtomicBool>,
}
#[derive(Default)]
struct Mailbox {
    closed: bool,
    serial: u64,
    pending: Option<Job>,
    active: Option<Arc<AtomicBool>>,
    result: Option<Result<Candidate, String>>,
}

pub(super) struct Upkeep {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
    fetch: Arc<Fetcher>,
    basis: Option<Binding>,
    due: Option<Instant>,
    reported: Option<String>,
    members: Option<[u8; 32]>,
    followup: bool,
}
impl Default for Upkeep {
    fn default() -> Self {
        Self {
            mailbox: Arc::new((Mutex::new(Mailbox::default()), Condvar::new())),
            worker: None,
            fetch: Arc::new(fetch),
            basis: None,
            due: None,
            reported: None,
            members: None,
            followup: false,
        }
    }
}
impl Upkeep {
    pub(super) fn pause(&mut self) {
        self.basis = None;
        self.due = None;
        self.members = None;
        self.followup = false;
        let mut mail = self
            .mailbox
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        mail.serial = mail.serial.wrapping_add(1);
        if let Some(stop) = &mail.active {
            stop.store(true, Ordering::Release);
        }
        mail.pending = None;
        mail.result = None;
    }

    fn start(&mut self) -> Result<(), String> {
        if self.worker.is_some() {
            return Ok(());
        }
        let mailbox = Arc::clone(&self.mailbox);
        let fetch = Arc::clone(&self.fetch);
        self.worker = Some(
            std::thread::Builder::new()
                .name("repository-catalog".into())
                .spawn(move || {
                    loop {
                        let job = {
                            let mut mail = mailbox
                                .0
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            while !mail.closed && mail.pending.is_none() {
                                mail = mailbox
                                    .1
                                    .wait(mail)
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                            }
                            if mail.closed {
                                return;
                            }
                            let job = mail.pending.take().expect("pending job");
                            mail.active = Some(Arc::clone(&job.stopped));
                            job
                        };
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            fetch(job.input, Arc::clone(&job.stopped))
                        }))
                        .unwrap_or_else(|_| {
                            Err("private catalog provider read ended unexpectedly".into())
                        });
                        let mut mail = mailbox
                            .0
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        mail.active = None;
                        if !mail.closed
                            && mail.serial == job.serial
                            && !job.stopped.load(Ordering::Acquire)
                        {
                            mail.result = Some(result);
                        }
                    }
                })
                .map_err(|_| "private catalog worker could not start")?,
        );
        Ok(())
    }

    /// Offering, replacing and pausing never joins HTTP. Drop retires pending dispatch and joins.
    pub(super) fn step(
        &mut self,
        paths: &Paths,
        admin: &Admin,
        deadline: Instant,
    ) -> Result<usize, String> {
        let input = match Input::capture(paths, admin) {
            Ok(input) => input,
            Err(error) => {
                self.pause();
                let repeated = self.reported.as_ref() == Some(&error);
                self.reported = Some(error.clone());
                return if repeated { Ok(0) } else { Err(error) };
            }
        };
        self.reported = None;
        let changed = self.basis.as_ref() != Some(&input.binding);
        if changed {
            self.pause();
            self.basis = Some(input.binding.clone());
        }
        let members = membership(admin);
        if self
            .members
            .replace(members)
            .is_some_and(|before| before != members)
        {
            self.followup = true;
            if self
                .mailbox
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                .is_none()
            {
                self.due = None;
            }
        }
        let result = self
            .mailbox
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .result
            .take();
        let mut published = 0;
        if let Some(result) = result {
            match result {
                Ok(candidate) => {
                    self.due = None;
                    if candidate.observed_at.elapsed() > RESULT_AGE {
                        // A retained store retry cannot turn into an indefinitely cached provider
                        // observation. Retire it and offer a fresh bounded read below.
                    } else {
                        if Instant::now() >= deadline {
                            self.mailbox
                                .0
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .result = Some(Ok(candidate));
                            return Ok(0);
                        }
                        if candidate.binding == input.binding {
                            let mut next_basis = input.binding.clone();
                            let mut next_entry = admin
                                .published_entry(&Audience::Everyone, NAME)
                                .expect("captured catalog")
                                .clone();
                            next_entry.value = SecretBytes::from(candidate.bytes.as_slice());
                            next_basis.catalog = entry_digest(&next_entry);
                            published = match publish(paths, admin, candidate.clone(), deadline) {
                                Ok(done) => done,
                                Err(error) => {
                                    self.mailbox
                                        .0
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .result = Some(Ok(candidate));
                                    return Err(error);
                                }
                            };
                            self.due = (!self.followup).then(|| Instant::now() + LOOK);
                            if published != 0 {
                                self.basis = Some(next_basis);
                                return Ok(published);
                            }
                        }
                    }
                }
                Err(error) => {
                    self.followup = false;
                    self.due = Some(Instant::now() + RETRY);
                    return Err(error);
                }
            }
        }
        if self.due.is_none_or(|due| Instant::now() >= due) {
            if let Err(error) = self.start() {
                self.due = Some(Instant::now() + RETRY);
                return Err(error);
            }
            let mut mail = self
                .mailbox
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !changed && (mail.active.is_some() || mail.pending.is_some()) {
                return Ok(published);
            }
            if let Some(stop) = &mail.active {
                stop.store(true, Ordering::Release);
            }
            mail.serial = mail.serial.wrapping_add(1);
            mail.pending = Some(Job {
                serial: mail.serial,
                input,
                stopped: Arc::new(AtomicBool::new(false)),
            });
            self.due = None;
            self.followup = false;
            self.mailbox.1.notify_one();
        }
        Ok(published)
    }
}

fn membership(admin: &Admin) -> [u8; 32] {
    let doc = admin.published_document();
    let admins = doc.admins();
    let held = doc.code_only();
    let mut members: Vec<_> = doc
        .machines()
        .into_iter()
        .map(|member| {
            (
                member.recipient.clone(),
                admins.contains(&member.name),
                held.contains(&member.recipient),
            )
        })
        .collect();
    members.sort();
    let mut hash = aterm_digest::Sha256::new();
    for (recipient, admin, held) in members {
        hash.update((recipient.len() as u64).to_le_bytes());
        hash.update(recipient.as_bytes());
        hash.update([u8::from(admin), u8::from(held)]);
    }
    hash.finalize()
}
impl Drop for Upkeep {
    fn drop(&mut self) {
        self.pause();
        self.mailbox
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.mailbox.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn publish(
    paths: &Paths,
    supplied: &Admin,
    candidate: Candidate,
    deadline: Instant,
) -> Result<usize, String> {
    // Reopen locally: unrelated versions do not invalidate a read, but changed relevant entries
    // or authority do. Exact receiving-parent CAS closes concurrent publication at the store.
    let fresh = client::admin_cached_until(paths, deadline).map_err(|e| e.to_string())?;
    let current = Input::capture(paths, &fresh)?;
    if fresh.config != supplied.config
        || current.binding != candidate.binding
        || Instant::now() >= deadline
    {
        return Err("private repository catalog source changed before publication".into());
    }
    let old = fresh
        .published_entry(&Audience::Everyone, NAME)
        .expect("captured catalog");
    if aterm_json::from_slice::<aterm_json::Value>(old.value.as_slice()).ok()
        == aterm_json::from_slice::<aterm_json::Value>(&candidate.bytes).ok()
    {
        return Ok(0);
    }
    let mut doc = fresh.doc.clone();
    doc.set_entry_with(
        &Audience::Everyone,
        NAME,
        &candidate.bytes,
        old.if_absent,
        old.anchor.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    if !allowed(paths) {
        return Err("private repository catalog upkeep was stopped".into());
    }
    client::admin_publish_conditional_until(paths, &fresh, &doc, deadline).map_err(|_| {
        "private catalog publication awaits current authority or readback".to_string()
    })?;
    aterm_log::info!(
        "private repository catalog refreshed: mode={:?}, observed_owned={}",
        candidate.visibility,
        candidate.observed
    );
    Ok(1)
}

struct AdmittedToken(SecretBytes);
impl EnvSource for AdmittedToken {
    fn var_os(&self, name: &str) -> Option<OsString> {
        (name == TOKEN_ENV).then(|| OsString::from_vec(self.0.as_slice().to_vec()))
    }
}
fn call(path: &str) -> Call {
    Call::get(
        path,
        RouteRequest::read(RouteTarget::Identity).with_as(Some(PROFILE.into())),
    )
    .limit(MAX as u64)
    .probe_404(false)
    .follow_moved(false)
}
#[derive(PartialEq, Eq)]
struct Identity {
    login: String,
    id: u64,
    public: Option<u64>,
    private: Option<u64>,
    repo_scope: bool,
}
fn identity(answer: &agh::Answered) -> Result<Identity, String> {
    let repo_scope = answer
        .header("x-oauth-scopes")
        .is_some_and(|scopes| scopes.split(',').any(|scope| scope.trim() == "repo"));
    let value = answer
        .json()
        .map_err(|_| "provider identity cannot be decoded")?;
    if value["type"].as_str() != Some("User") {
        return Err("catalog owner is not a personal account".into());
    }
    Ok(Identity {
        login: value["login"]
            .as_str()
            .ok_or("provider login is absent")?
            .to_owned(),
        id: value["id"]
            .as_u64()
            .filter(|id| *id != 0)
            .ok_or("provider id is absent")?,
        public: value["public_repos"].as_u64(),
        private: value["owned_private_repos"].as_u64(),
        repo_scope,
    })
}
fn fetch(input: Input, stopped: Arc<AtomicBool>) -> Result<Candidate, String> {
    let until = Instant::now() + FETCH;
    let credential = CredentialBasis::of(&input.token);
    let gh = github(
        input.token,
        Arc::new(HttpTransport::new(Arc::new(ProcessEnv)).bounded(until, Arc::clone(&stopped))),
    )?;
    observe(
        &gh,
        input.binding,
        &input.catalog,
        &stopped,
        until,
        &credential,
    )
}

fn github(
    token: SecretBytes,
    transport: Arc<dyn agh::wire::Transport>,
) -> Result<agh::Github, String> {
    let config = agh::config::parse("[profiles.private-catalog]\nenv='ATERM_ADMITTED_CATALOG_TOKEN'\ntier='dev'\n[default]\nread=['private-catalog']\n")
        .map_err(|_| "catalog routing configuration is invalid")?;
    let validated = agh::config::validate(&config, std::path::Path::new("/"), &|_| None)
        .map_err(|_| "catalog routing configuration is invalid")?;
    Ok(GithubBuilder::new(Router::from_validated(validated))
        .env(Arc::new(AdmittedToken(token)))
        .transport(transport)
        .refuse_anonymous_api(true)
        .retry(RetryPolicy::NONE)
        .caller("aterm-private-catalog")
        .rate_book(
            agh::config::Dirs::from_env().map_or_else(agh::rate::RateBook::none, |dirs| {
                agh::rate::RateBook::at(dirs.rate())
            }),
        )
        .build())
}

#[cfg(test)]
#[path = "repository_catalog_protocol_tests.rs"]
mod protocol_tests;

struct CredentialBasis {
    identity: agh::error::IdentityRef,
    complete_token: bool,
}
impl CredentialBasis {
    fn of(token: &SecretBytes) -> Self {
        Self {
            identity: agh::error::IdentityRef::Profile {
                name: PROFILE.into(),
                fingerprint: agh::identity::fingerprint(token.as_slice()),
            },
            complete_token: matches!(
                TokenKind::of(token.as_slice()),
                Ok(TokenKind::Classic | TokenKind::OAuth)
            ),
        }
    }
}

fn observe(
    gh: &agh::Github,
    binding: Binding,
    bytes: &[u8],
    stopped: &AtomicBool,
    until: Instant,
    credential: &CredentialBasis,
) -> Result<Candidate, String> {
    let catalog = RepositoryCatalog::parse(bytes)?;
    // No shared rate-wait book or SDK retries: only the owned scheduler retains retry debt.
    let before = gh
        .call(&call("/user"))
        .map_err(|_| "catalog provider identity is unavailable")?;
    if before.as_ != credential.identity {
        return Err("catalog provider did not use the admitted credential".into());
    }
    let user = identity(&before)?;
    if catalog.owner_id().is_some_and(|id| id != user.id)
        || (catalog.owner_id().is_none() && !catalog.account().eq_ignore_ascii_case(&user.login))
    {
        return Err("catalog provider identity differs from the published account".into());
    }
    let listing = agh::page::walk(
        gh,
        call("/user/repos").query("affiliation", "owner"),
        &ListOpts::CAS,
        &LinkScope::Exact,
    )
    .map_err(|_| "catalog provider listing is incomplete or unavailable")?;
    if listing.end != End::Complete
        || listing.consistency != Consistency::Snapshot
        || listing.as_ != before.as_
    {
        return Err("catalog provider listing is not a complete current identity snapshot".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    let (mut public, mut private) = (0u64, 0u64);
    for row in &listing.items {
        if row["owner"]["id"].as_u64() != Some(user.id)
            || !row["owner"]["login"]
                .as_str()
                .is_some_and(|login| login.eq_ignore_ascii_case(&user.login))
            || !row["id"]
                .as_u64()
                .is_some_and(|id| id != 0 && ids.insert(id))
        {
            return Err("catalog provider listing contains ambiguous ownership".into());
        }
        match row["private"].as_bool() {
            Some(true) => private += 1,
            Some(false) => public += 1,
            None => return Err("catalog provider listing has unknown visibility".into()),
        }
    }
    let after = gh
        .call(&call("/user"))
        .map_err(|_| "catalog provider final identity is unavailable")?;
    if identity(&after)? != user
        || after.as_ != before.as_
        || stopped.load(Ordering::Acquire)
        || Instant::now() >= until
    {
        return Err(
            "catalog provider visibility or identity changed during its complete read".into(),
        );
    }
    let visibility = if credential.complete_token
        && user.repo_scope
        && let (Some(expected_public), Some(expected_private)) = (user.public, user.private)
    {
        if (public, private) != (expected_public, expected_private) {
            return Err("provider collection disagrees with whole-account counters".into());
        }
        RepositoryCatalogVisibility::AccountComplete
    } else {
        RepositoryCatalogVisibility::TokenVisible
    };
    let bytes = catalog.refresh(&user.login, user.id, &listing.items, visibility)?;
    let observed_at = Instant::now();
    if stopped.load(Ordering::Acquire) || observed_at >= until {
        return Err("catalog provider read expired before its final result".into());
    }
    Ok(Candidate {
        binding,
        bytes,
        visibility,
        observed: listing.items.len(),
        observed_at,
    })
}
