// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity-bound account recovery metadata and same-account renewal selection.
//! No caller-supplied path establishes a binding, and this module supplies no
//! terminal or process-mutation authority.

use super::{accounts, accounts_sync, cli::Env, codex_app_login, self_claimed};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub account: String,
    pub user: String,
}

impl Identity {
    pub fn from_native(account: &str, user: &str) -> Self {
        fn digest(domain: &[u8], value: &str) -> String {
            let mut hash = aterm_digest::Sha256::new();
            hash.update(domain);
            hash.update(value.as_bytes());
            aterm_codec::hex::encode_lower(&hash.finalize())
        }
        Self {
            account: digest(b"aterm/codex-renewal/account/v1\0", account),
            user: digest(b"aterm/codex-renewal/user/v1\0", user),
        }
    }
}

fn hex(word: &str, size: usize) -> bool {
    word.len() == size
        && word
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    #[default]
    Login,
    WeeklyQuota,
}

impl Purpose {
    fn is_login(&self) -> bool {
        *self == Self::Login
    }

    pub fn marker(self) -> &'static str {
        match self {
            Self::Login => "renewal",
            Self::WeeklyQuota => "rotation",
        }
    }

    pub fn rule(self) -> &'static str {
        match self {
            Self::Login => "login-back@v1",
            Self::WeeklyQuota => "usage-rotate@v1",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    #[serde(default, skip_serializing_if = "Purpose::is_login")]
    pub purpose: Purpose,
    pub thread: String,
    pub wall: String,
    pub identity: Identity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Prepare(Intent),
    Commit(String),
    Status(String),
    Consume { id: String, turn: String },
}

impl Request {
    pub fn parse(line: &str) -> Option<Self> {
        Self::parse_for(line, Purpose::Login)
    }

    pub fn parse_rotation(line: &str) -> Option<Self> {
        Self::parse_for(line, Purpose::WeeklyQuota)
    }

    fn parse_for(line: &str, purpose: Purpose) -> Option<Self> {
        let mut words = line.split_ascii_whitespace();
        let verb = words.next()?;
        let mut fields = BTreeMap::new();
        for word in words {
            let (key, value) = word.split_once('=')?;
            if fields.insert(key, value).is_some() {
                return None;
            }
        }
        match verb {
            "prepare" if fields.len() == 4 => {
                let thread = *fields.get("thread")?;
                let (wall_key, account_key, user_key) = match purpose {
                    Purpose::Login => ("wall", "account", "user"),
                    Purpose::WeeklyQuota => ("turn", "source_account", "source_user"),
                };
                let wall = *fields.get(wall_key)?;
                let account = *fields.get(account_key)?;
                let user = *fields.get(user_key)?;
                (self_claimed::is_uuid(thread)
                    && self_claimed::is_uuid(wall)
                    && hex(account, 64)
                    && hex(user, 64))
                .then(|| {
                    Self::Prepare(Intent {
                        purpose,
                        thread: thread.into(),
                        wall: wall.into(),
                        identity: Identity {
                            account: account.into(),
                            user: user.into(),
                        },
                    })
                })
            }
            "commit" | "status" if fields.len() == 1 => {
                let id = *fields.get("id")?;
                hex(id, 32).then(|| {
                    if verb == "commit" {
                        Self::Commit(id.into())
                    } else {
                        Self::Status(id.into())
                    }
                })
            }
            "consume" if fields.len() == 2 => {
                let id = *fields.get("id")?;
                let turn = *fields.get("turn")?;
                (hex(id, 32) && self_claimed::is_uuid(turn)).then(|| Self::Consume {
                    id: id.into(),
                    turn: turn.into(),
                })
            }
            _ => None,
        }
    }
}

/// Captured under the account writer and native auth locks. No identity values
/// or credentials are returned; the generation and exact row detect policy changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Successor {
    pub row: accounts::Row,
    pub old_auth: PathBuf,
    pub new_auth: PathBuf,
    pub generation: u64,
    pub identity: Identity,
}

fn renewal_policy_on(config: &Path) -> bool {
    let (policy, warnings) = crate::supervise::SupervisorConfig::from_path(Some(config));
    warnings.is_empty()
        && policy.relaunch
        && policy.resume_limits
        && accounts_sync::sign_ins_off_for(config, accounts::Cli::Codex).is_none()
}

/// Read the same bound account's successor. `old_auth` comes only from a
/// kernel-validated self-claim record, never from the renewal wire request.
/// The callback executes while all account and native auth locks remain held.
pub fn with_successor<T>(
    env: &Env,
    old_auth: &Path,
    identity: &Identity,
    use_successor: impl FnOnce(&Successor) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    with_successor_writer(env, old_auth, identity, |next, _| use_successor(next))
}

/// Select the same-account successor and expose the writer held by that exact
/// transaction. Publication performs no control/network call under this guard.
pub fn with_successor_writer<T>(
    env: &Env,
    old_auth: &Path,
    identity: &Identity,
    use_successor: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    let home = env.home.as_deref().ok_or("home-unavailable")?;
    let config = env.config.as_deref().ok_or("config-unavailable")?;
    if !renewal_policy_on(config) {
        return Err("login-policy");
    }
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("store-unavailable")?;
    let _signin = store
        .signin_within()
        .map_err(|_| "signin-lock")?
        .ok_or("signin-busy")?;
    let _writer = store
        .acquire_bounded()
        .map_err(|_| "roster-lock")?
        .ok_or("roster-busy")?;
    let path = env
        .accounts_roster_for(accounts::Cli::Codex)
        .ok_or("roster-unavailable")?;
    let raw =
        accounts::Roster::load_for(accounts::Cli::Codex, &path).map_err(|_| "roster-unreadable")?;
    let (logins, _) = store
        .load_logins(env.now)
        .map_err(|_| "bindings-unreadable")?;
    let roster = accounts::bound(&raw, &logins);
    let old_home = old_auth
        .parent()
        .filter(|_| old_auth.file_name().is_some_and(|name| name == "auth.json"))
        .ok_or("auth-path")?;
    let default = home.join(".codex");
    let old_dir = (old_home != default).then(|| old_home.to_string_lossy().into_owned());
    let candidates: Vec<_> = roster
        .rows
        .iter()
        .filter(|row| row.dir == old_dir || row.was.contains(&old_dir))
        .collect();
    let [row] = candidates.as_slice() else {
        return Err("account-lineage");
    };
    let new_home = row.dir.as_deref().map(PathBuf::from).unwrap_or(default);
    let new_auth = new_home.join("auth.json");
    if new_auth == old_auth {
        return Ok(None);
    }
    if row.held_by.is_some()
        || !super::super::ai::facts::codex_file_store(old_home)
        || !super::super::ai::facts::codex_file_store(&new_home)
    {
        return Err("account-held");
    }
    let state = store.load(env.now).map_err(|_| "state-unreadable")?.0;
    if accounts::condition(row, &state, true, env.now) == accounts::Condition::SignedOut {
        return Err("account-signed-out");
    }
    let source =
        accounts_sync::codex_completion_source(env, row).map_err(|_| "source-unreadable")?;
    if source.held_by.is_some() {
        return Err("source-held");
    }
    if source.fleet.as_ref().is_some_and(|fleet| {
        fleet.roster.accounts.iter().any(|account| {
            account.label == row.label
                && (!row
                    .expected_identity_email()
                    .is_some_and(|email| accounts::email_matches(email, &account.email))
                    || account.vendor != row.vendor)
        })
    }) {
        return Err("source-conflict");
    }
    // Deterministic lock order also handles two simultaneous follow attempts.
    let mut files = [old_auth.to_path_buf(), new_auth.clone()];
    files.sort();
    let _first = codex_app_login::AuthLock::try_take(&files[0])
        .map_err(|_| "auth-lock")?
        .ok_or("auth-busy")?;
    let _second = codex_app_login::AuthLock::try_take(&files[1])
        .map_err(|_| "auth-lock")?
        .ok_or("auth-busy")?;
    if codex_app_login::renewal_identity(old_home).as_ref() != Some(identity)
        || codex_app_login::renewal_identity(&new_home).as_ref() != Some(identity)
    {
        return Err("identity-mismatch");
    }
    let binding = logins.get(&row.label).ok_or("binding-absent")?;
    if binding.last_success_generation == 0
        || binding.last_success_generation != binding.renewal_generation
    {
        return Err("renewal-uncommitted");
    }
    if !renewal_policy_on(config)
        || accounts_sync::codex_completion_source(env, row).map_err(|_| "source-unreadable")?
            != source
    {
        return Err("policy-changed");
    }
    use_successor(
        &Successor {
            row: (*row).clone(),
            old_auth: old_auth.into(),
            new_auth,
            generation: binding.last_success_generation,
            identity: identity.clone(),
        },
        &ReceiptWriter::held(env, &_writer),
    )
    .map(Some)
}

/// Compare and commit the exact prepared selection while native account and
/// auth locks remain held. Concurrent commits of one receipt have one winner.
pub fn commit_receipt(env: &Env, expected: &Receipt) -> Result<Option<Receipt>, &'static str> {
    if expected.phase != Phase::Ready {
        return Err("not-ready");
    }
    with_selected(env, expected, |next| {
        if !expected.matches_successor(next)
            || read_receipt(env, &expected.id).as_ref() != Some(expected)
        {
            return Err("changed");
        }
        let mut committed = expected.clone();
        committed.phase = Phase::Committing;
        write_receipt(env, &committed)?;
        Ok(committed)
    })
}

pub fn with_selected<T>(
    env: &Env,
    receipt: &Receipt,
    use_selected: impl FnOnce(&Successor) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    with_selected_writer(env, receipt, |next, _| use_selected(next))
}

/// Retry terminal pool-record cleanup independently of selected credentials:
/// consumption has already closed native execution authority.
pub fn retire_pool_record(env: &Env, receipt: &Receipt) -> Result<Option<bool>, &'static str> {
    with_receipt_writer(env, |writer| writer.retire_pool_record(receipt))
}

pub fn with_selected_writer<T>(
    env: &Env,
    receipt: &Receipt,
    use_selected: impl FnOnce(&Successor, &ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    match receipt.intent.purpose {
        Purpose::Login => with_successor_writer(
            env,
            &receipt.old_auth,
            &receipt.intent.identity,
            use_selected,
        ),
        Purpose::WeeklyQuota => {
            super::codex_rotation::with_selected_writer(env, receipt, use_selected)
        }
    }
}

impl Intent {
    pub fn marker<'a>(&self, record: &'a aterm_json::Value) -> &'a aterm_json::Value {
        &record["owed"][self.purpose.marker()]
    }

    pub fn matches_history(&self, history: &History) -> bool {
        match self.purpose {
            Purpose::Login => history.auth_wall,
            Purpose::WeeklyQuota => history.usage_wall,
        }
    }
    /// The native record is read only after its process identity and resume
    /// command were validated by the host. No bare request establishes a wall.
    pub fn matches_record(&self, record: &aterm_json::Value, receipt: Option<&str>) -> bool {
        let owed = &record["owed"];
        let renewal = self.marker(record);
        record["sessionId"].as_str() == Some(&self.thread)
            && matches!(record["status"].as_str(), Some("idle" | "waiting"))
            && record["draftEmpty"].as_bool() == Some(true)
            && record["autopilot"].as_str() == Some("active")
            && matches!(owed["mode"].as_str(), Some("cruise" | "land"))
            && owed["thread"].as_str() == Some(&self.thread)
            && owed["nativeOrigin"]["turnId"].as_str() == Some(&self.wall)
            && owed["nativeOrigin"]["phase"].as_str() == Some("complete")
            && owed["rule"].as_str() == Some(self.purpose.rule())
            && renewal["openaiScope"].as_bool() == Some(true)
            && matches!(
                renewal.get("local"),
                None | Some(aterm_json::Value::Bool(false))
            )
            && renewal["account"].as_str() == Some(&self.identity.account)
            && renewal["user"].as_str() == Some(&self.identity.user)
            && renewal["receipt"].as_str() == receipt
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Ready,
    Committing,
    Committed,
    Consumed,
    Retired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub pid: u32,
    pub birth: u64,
    pub parent: u32,
    pub group: u32,
    pub session: u32,
    pub launcher_device: u64,
    pub launcher_inode: u64,
    pub launcher_path: Vec<u8>,
    pub forward: bool,
}

impl Candidate {
    pub fn of(process: &super::native_act::Process, forward: bool) -> Self {
        Self {
            pid: process.pid,
            birth: process.birth,
            parent: process.parent,
            group: process.group,
            session: process.session,
            launcher_device: process.image.device,
            launcher_inode: process.image.inode,
            launcher_path: process.image.path.clone(),
            forward,
        }
    }
    pub fn matches_launcher(&self, process: &super::native_act::Process, forward: bool) -> bool {
        *self == Self::of(process, forward)
    }
    pub fn matches_exec(
        &self,
        process: &super::native_act::Process,
        prepare: &super::native_act::Prepare,
    ) -> bool {
        let super::native_act::NativePlan::Restart {
            forward,
            rollback,
            launcher,
            ..
        } = &prepare.plan
        else {
            return false;
        };
        let target = if self.forward {
            forward
        } else {
            let Some(back) = rollback else { return false };
            back
        };
        target.image != *launcher
            && self.pid == process.pid
            && self.birth == process.birth
            && self.parent == process.parent
            && self.group == process.group
            && self.session == process.session
            && self.launcher_device == launcher.device
            && self.launcher_inode == launcher.inode
            && self.launcher_path == launcher.path
            && process.image == target.image
    }
}

#[path = "codex_renewal_attempts.rs"]
mod attempts;
pub use attempts::{
    Attempt, AttemptRef, Cancellation, CancellationKind, NativeCancellationProof, ReceiptWriter,
    read_active_attempt, read_attempt, read_cancellation, with_receipt_writer,
};

/// Durable metadata only. Native command and operation persistence are retained
/// separately by the existing private-restart record, never accepted from wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// A quota move must name a different account. Login renewals have no
    /// destination field and retain their existing same-identity requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Identity>,
    pub id: String,
    pub intent: Intent,
    pub sid: String,
    pub old_pid: u32,
    pub old_birth: u64,
    pub claim_revision: u64,
    pub human_seq: u64,
    pub old_auth: PathBuf,
    pub new_auth: PathBuf,
    pub generation: u64,
    pub row_stamp: String,
    pub native_prepare: String,
    pub expires_ms: u64,
    pub phase: Phase,
    pub successor_pid: Option<u32>,
    pub successor_birth: Option<u64>,
    pub accepted_turn: Option<String>,
    pub launch_attempted: bool,
    pub candidate: Option<Candidate>,
    pub current_attempt: Option<AttemptRef>,
    pub revoked: bool,
}

impl Receipt {
    pub fn target_identity(&self) -> Option<&Identity> {
        match (self.intent.purpose, &self.destination) {
            (Purpose::Login, None) => Some(&self.intent.identity),
            (Purpose::WeeklyQuota, Some(target))
                if hex(&target.account, 64)
                    && hex(&target.user, 64)
                    && target.account != self.intent.identity.account =>
            {
                Some(target)
            }
            _ => None,
        }
    }
    /// The immutable logical binding, independent of mutable attempt progress.
    pub fn same_intent(&self, other: &Self) -> bool {
        attempts::same_intent(self, other)
    }

    pub fn exec_candidate(
        &self,
        process: Option<&super::native_act::Process>,
        prepare: &super::native_act::Prepare,
    ) -> Option<bool> {
        let candidate = self.candidate.as_ref()?;
        candidate
            .matches_exec(process?, prepare)
            .then_some(candidate.forward)
    }

    /// The completed native rollback must name this exact restored process,
    /// with no accepted recovery turn. This observes closure, not execution authority.
    fn matches_completed_rollback(
        &self,
        prepare: &super::native_act::Prepare,
        observed: &super::native_observe::SavedObservation,
    ) -> bool {
        let restored = &observed.registration.vendor;
        self.phase == Phase::Retired
            && self.launch_attempted
            && self.accepted_turn.is_none()
            && self.successor_pid == Some(restored.pid)
            && self.successor_birth == Some(restored.birth)
            && self.exec_candidate(Some(restored), prepare) == Some(false)
    }

    /// A later native restart may carry the pending action to a new process.
    /// Only a completed native operation whose exact predecessor is this
    /// receipt's successor establishes that lineage; raw relaunch records do not.
    pub fn matches_descendant(
        &self,
        completed: &super::native_act::Prepare,
        observed: &super::native_observe::SavedObservation,
        current: &super::native_act::Process,
        predecessor_gone: bool,
    ) -> bool {
        use super::native_act::{NativePlan, Request};
        let NativePlan::Restart {
            process,
            shell,
            forward,
            ..
        } = &completed.plan
        else {
            return false;
        };
        let Some(Request::Prepare(original)) = Request::parse(&self.native_prepare) else {
            return false;
        };
        let NativePlan::Restart {
            forward: previous, ..
        } = &original.plan
        else {
            return false;
        };
        predecessor_gone
            && Some(process.pid) == self.successor_pid
            && Some(process.birth) == self.successor_birth
            && process.image == previous.image
            && &observed.registration.vendor == current
            && &observed.registration.shell == shell
            && current.parent == shell.pid
            && current.session == shell.session
            && current.group == current.pid
            && observed.registration.host_epoch == completed.host_epoch
            && observed.binding.host_epoch == completed.host_epoch
            && observed.binding.id == observed.registration.id
            && current.image == forward.image
            && forward
                .env
                .iter()
                .any(|(key, value)| key == "CODEX_AUTH_PATH" && Path::new(value) == self.new_auth)
    }

    pub fn may_replace(&self) -> bool {
        self.phase == Phase::Retired && !self.launch_attempted && !self.revoked
    }

    /// Close an unlaunched Ready receipt before deriving a distinct preparation
    /// for today's claim/human epoch. The original plan and budget stay frozen.
    /// Caller holds the account writer and has revalidated its selected successor.
    pub fn retire_stale_ready(&mut self, revision: u64, human_seq: u64, now_ms: u64) -> bool {
        if self.phase != Phase::Ready
            || self.revoked
            || self.current_attempt.is_some()
            || self.launch_attempted
            || self.candidate.is_some()
            || self.accepted_turn.is_some()
            || self.successor_pid.is_some()
            || self.successor_birth.is_some()
            || (self.claim_revision == revision
                && self.human_seq == human_seq
                && now_ms < self.expires_ms)
        {
            return false;
        }
        self.phase = Phase::Retired;
        true
    }

    pub fn reset_before_signal(&mut self, now_ms: u64) -> bool {
        if self.phase != Phase::Committing || self.launch_attempted {
            return false;
        }
        self.phase = if now_ms < self.expires_ms {
            Phase::Ready
        } else {
            Phase::Retired
        };
        true
    }

    pub fn matches_successor(&self, next: &Successor) -> bool {
        self.old_auth == next.old_auth
            && self.new_auth == next.new_auth
            && self.generation == next.generation
            && self.target_identity() == Some(&next.identity)
            && self.row_stamp == row_stamp(&next.row)
    }

    pub fn reply(&self) -> String {
        let Some(target) = self.target_identity() else {
            return "ERR account recovery invalid identity transition\n".into();
        };
        let state = match self.phase {
            Phase::Ready => "ready",
            Phase::Committing => "waiting",
            Phase::Committed => "committed",
            Phase::Consumed => "consumed",
            Phase::Retired => "retired",
        };
        if self.intent.purpose == Purpose::WeeklyQuota {
            return format!(
                "OK codex-rotation state={state} id={} thread={} turn={} source_account={} source_user={} target_account={} target_user={}\n",
                self.id,
                self.intent.thread,
                self.intent.wall,
                self.intent.identity.account,
                self.intent.identity.user,
                target.account,
                target.user,
            );
        }
        format!(
            "OK codex-renewal state={state} id={} thread={} wall={} account={} user={}\n",
            self.id,
            self.intent.thread,
            self.intent.wall,
            self.intent.identity.account,
            self.intent.identity.user
        )
    }

    /// Bind acceptance once. Repeating the same durable native turn is safe;
    /// another turn or a receipt that never committed cannot consume it.
    pub fn consume(&mut self, turn: &str) -> bool {
        if self.target_identity().is_none()
            || !self_claimed::is_uuid(turn)
            || turn != self.intent.wall
        {
            return false;
        }
        match self.phase {
            Phase::Committed if self.accepted_turn.is_none() => {
                self.accepted_turn = Some(turn.into());
                self.phase = Phase::Consumed;
                true
            }
            Phase::Consumed => self.accepted_turn.as_deref() == Some(turn),
            _ => false,
        }
    }
}

pub fn row_stamp(row: &accounts::Row) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(format!("{row:?}").as_bytes()))
}

pub fn receipt_path(env: &Env, id: &str) -> Option<PathBuf> {
    hex(id, 32).then(|| {
        env.accounts_store_for(accounts::Cli::Codex)
            .map(|store| store.dir.join("renewal-follow").join(format!("{id}.json")))
    })?
}

pub fn read_receipt(env: &Env, id: &str) -> Option<Receipt> {
    let mut receipt = attempts::read_root(env, id).ok()??;
    if read_cancellation(env, &receipt).ok()?.is_some() {
        receipt.revoked = true;
        if receipt.phase != Phase::Consumed {
            receipt.phase = Phase::Retired;
        }
    }
    (receipt.id == id
        && hex(&receipt.intent.identity.account, 64)
        && hex(&receipt.intent.identity.user, 64)
        && receipt.target_identity().is_some())
    .then_some(receipt)
}

/// Tiny regular index only; unknown/corrupt custody must never mint a second
/// operation for an existing intent. O_NOFOLLOW closes the symlink read race.
pub fn read_index(path: &Path) -> Result<Option<String>, &'static str> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("index-read"),
    };
    let metadata = file.metadata().map_err(|_| "index-read")?;
    if !metadata.is_file() || metadata.len() != 32 {
        return Err("index-shape");
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(33)
        .read_to_end(&mut bytes)
        .map_err(|_| "index-read")?;
    let id = String::from_utf8(bytes).map_err(|_| "index-shape")?;
    if !aterm_codec::hex::is_lower(&id, 32) {
        return Err("index-shape");
    }
    Ok(Some(id))
}

/// Caller holds the Codex store writer guard, also serializing concurrent
/// prepare, commit and consume for this receipt.
pub fn write_receipt(env: &Env, receipt: &Receipt) -> Result<(), &'static str> {
    if receipt.target_identity().is_none()
        || !hex(&receipt.intent.identity.account, 64)
        || !hex(&receipt.intent.identity.user, 64)
    {
        return Err("receipt-identity-transition");
    }
    let path = receipt_path(env, &receipt.id).ok_or("receipt-id")?;
    let parent = path.parent().ok_or("receipt-path")?;
    aterm_types::fs_restricted::ensure_private_dir(parent).map_err(|_| "receipt-directory")?;
    if let Some(grandparent) = parent.parent() {
        aterm_durable::Flush::Durable
            .dir(grandparent)
            .map_err(|_| "receipt-parent-flush")?;
    }
    let text = aterm_json::to_string(receipt).map_err(|_| "receipt-encoding")?;
    if text.len() > 32 * 1024 {
        return Err("receipt-size");
    }
    if !receipt.revoked && read_cancellation(env, receipt)?.is_some() {
        return Err("receipt-revoked");
    }
    super::upgrade_models::write_atomic_mode(&path, &text, Some(0o600))
        .map_err(|_| "receipt-write")?;
    aterm_durable::Flush::Durable
        .dir(parent)
        .map_err(|_| "receipt-directory-flush")
}

/// Prepare only the exact host-derived private plan. The caller owns the
/// matching sealed native admission; neither this API nor the wire chooses it.
pub fn prepare_restart(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
) -> Result<(), &'static str> {
    use super::{native_act, upgrade_drive};
    let key = format!("renewal-{}", prepare.id);
    let native_act::NativePlan::Restart {
        process,
        shell,
        forward,
        rollback,
        ..
    } = &prepare.plan
    else {
        return Err("restart-plan");
    };
    let mut state = upgrade_drive::load(opts, &key).unwrap_or_else(|| upgrade_drive::St {
        pid: process.pid,
        shell: shell.pid,
        tab: sid.into(),
        phase: super::upgrade::Phase::Exiting {
            at_s: upgrade_drive::now_s(),
        },
        source: upgrade_drive::SAME_PROGRAM.into(),
        to: "same".into(),
        line: forward.legacy_line().unwrap_or_default(),
        back_line: rollback
            .as_ref()
            .and_then(|back| back.legacy_line())
            .unwrap_or_default(),
        native_prepare: native_act::Request::prepare(prepare.clone())
            .encode()
            .unwrap_or_default(),
        native_coordination: true,
        ..Default::default()
    });
    upgrade_drive::save_checked(opts, &key, &state).map_err(|_| "restart-record")?;
    let mut client = upgrade_drive::connect(opts, sid).map_err(|_| "private-client")?;
    match super::native_restart::recover(&mut client, sid, &mut state) {
        Ok(
            super::native_restart::Recovery::Admitted | super::native_restart::Recovery::Completed,
        ) => {
            upgrade_drive::save_checked(opts, &key, &state).map_err(|_| "restart-record")?;
            Ok(())
        }
        _ => Err("restart-admission"),
    }
}

pub enum RestartCompletion {
    Forward(super::native_observe::SavedObservation),
    Rollback(super::native_observe::SavedObservation),
    NotStarted,
}

/// Observe the current immutable native attempt without changing the root's
/// original command or budget. Disk observations grant no native authority.
pub fn active_prepare(env: &Env, receipt: &Receipt) -> Option<super::native_act::Prepare> {
    use super::native_act::Request;
    let wire = if receipt.current_attempt.is_some() {
        read_active_attempt(env, receipt).ok()??.native_prepare
    } else {
        receipt.native_prepare.clone()
    };
    let Request::Prepare(prepare) = Request::parse(&wire)? else {
        return None;
    };
    let expected = receipt
        .current_attempt
        .as_ref()
        .map_or(receipt.id.as_str(), |attempt| attempt.id.as_str());
    (prepare.id == expected).then_some(*prepare)
}

/// Ask the native host to reconcile an exact retained attempt. The response is
/// only an observation; a successor still requires a fresh host-owned seal.
pub fn reconcile_restart(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
) {
    use super::{native_restart, upgrade_drive};
    let key = format!("renewal-{}", prepare.id);
    let Some(mut state) = upgrade_drive::load(opts, &key) else {
        return;
    };
    let Ok(mut client) = upgrade_drive::connect(opts, sid) else {
        return;
    };
    let _retired = native_restart::retire(&mut client, sid, &mut state);
    let _saved = upgrade_drive::save_checked(opts, &key, &state);
}

/// Complete an already prepared operation with the existing PrivateRestart
/// signal/resume/commit protocol. No raw terminal typing fallback is permitted.
pub fn drive_restart(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
) -> Option<RestartCompletion> {
    drive_admitted(opts, sid, prepare, true)
}

/// Drive a separately sealed continuation whose predecessor is already closed.
/// The host owns the closure proof; this transport never sends Signal.
pub fn drive_resume_only(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
) -> Option<RestartCompletion> {
    drive_admitted(opts, sid, prepare, false)
}

fn drive_admitted(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    signal_original: bool,
) -> Option<RestartCompletion> {
    use super::{native_restart, upgrade_drive};
    let key = format!("renewal-{}", prepare.id);
    let mut state = upgrade_drive::load(opts, &key)?;
    let mut client = upgrade_drive::connect(opts, sid).ok()?;
    native_restart::recover(&mut client, sid, &mut state).ok()?;
    drive_connection(opts, sid, prepare, state, client, signal_original)
}

pub(super) fn drive_connection(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    mut state: super::upgrade_drive::St,
    mut client: super::upgrade_drive::Client,
    signal_original: bool,
) -> Option<RestartCompletion> {
    use super::{native_act, native_restart, upgrade_drive};
    let key = format!("renewal-{}", prepare.id);
    let binding = state.native.clone()?;
    let native_act::NativePlan::Restart { process, .. } = &prepare.plan else {
        return None;
    };
    if signal_original {
        let signal = native_restart::signal(&mut client, sid, &binding);
        // An unread/truncated response makes this connection unusable, not evidence
        // that the effect was absent. Reconcile the same retained operation anew.
        if client.reply_unread() {
            return None;
        }
        if !matches!(signal, upgrade_drive::Terminated::Sent)
            && native_restart::abort(&mut client, sid, &mut state)
        {
            upgrade_drive::save_checked(opts, &key, &state).ok()?;
            return Some(RestartCompletion::NotStarted);
        }
    }
    let started = std::time::Instant::now();
    while native_act::monotonic_us().is_some_and(|now| now < prepare.deadline_us) {
        if native_restart::complete(opts, &key, &mut client, sid, &mut state) {
            upgrade_drive::save_checked(opts, &key, &state).ok()?;
            return state.native_observation.map(|observation| {
                if state.native_rollback {
                    RestartCompletion::Rollback(observation)
                } else {
                    RestartCompletion::Forward(observation)
                }
            });
        }
        if client.reply_unread() {
            return None;
        }
        if native_act::Process::observe(process.pid)
            .is_none_or(|current| current.birth != process.birth)
            && let Some(screen) = upgrade_drive::screen(&mut client, sid)
            && let Some(generation) = screen.generation
        {
            let phase = if state.native_rollback
                || started.elapsed() >= std::time::Duration::from_secs(60)
            {
                native_act::ImagePhase::Rollback
            } else {
                native_act::ImagePhase::Forward
            };
            if native_restart::resume(
                &mut client,
                sid,
                &binding,
                phase,
                &generation,
                prepare.expected_human_seq?,
            )
            .is_ok()
                && phase == native_act::ImagePhase::Rollback
            {
                state.native_rollback = true;
                let _saved = upgrade_drive::save_checked(opts, &key, &state);
            }
        }
        if client.reply_unread() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // The existing record retains the exact original command and operation for
    // recovery. An expired or ambiguous operation never becomes a fresh signal.
    let _saved = upgrade_drive::save_checked(opts, &key, &state);
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct History {
    pub starts: u64,
    pub auth_wall: bool,
    /// A native usage-limit failure. The host must additionally establish a
    /// weekly included-usage wall before preparing an account rotation.
    pub usage_wall: bool,
    pub latest_is_wall: bool,
}

/// Inspect the native thread's own complete rollout, never UI text or subagent
/// history. Bounds are refusal limits, not a tail that could miss an earlier start.
pub fn history(home: &Path, intent: &Intent) -> Option<History> {
    use std::io::Read;
    let path = super::codex_rollout::conversation_rollouts(home, &intent.thread).own?;
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() || file.metadata().ok()?.len() > 512 * 1024 * 1024 {
        return None;
    }
    history_lines(
        std::io::BufReader::new(file).take(512 * 1024 * 1024),
        intent,
    )
}

fn native_login_error(error: &aterm_json::Value) -> bool {
    let info = &error["codex_error_info"];
    if info.as_str() == Some("unauthorized") {
        return true;
    }
    let Some(variants) = info.as_object().filter(|value| value.len() == 1) else {
        return false;
    };
    let Some((kind, value)) = variants.iter().next() else {
        return false;
    };
    if !matches!(
        kind.as_str(),
        "http_connection_failed"
            | "response_stream_connection_failed"
            | "response_too_many_failed_attempts"
            | "response_stream_disconnected"
    ) {
        return false;
    }
    let Some(fields) = value.as_object().filter(|value| value.len() == 1) else {
        return false;
    };
    fields
        .get("http_status_code")
        .and_then(aterm_json::Value::as_u64)
        == Some(401)
}

fn history_lines(reader: impl std::io::BufRead, intent: &Intent) -> Option<History> {
    let mut starts = 0;
    let mut current = String::new();
    let mut auth = false;
    let mut usage = false;
    let mut ended = false;
    for line in reader.lines() {
        let line = line.ok()?;
        if line.len() > 4 * 1024 * 1024 {
            return None;
        }
        let value: aterm_json::Value = aterm_json::from_str(&line).ok()?;
        if value["type"].as_str() != Some("event_msg") {
            continue;
        }
        let event = &value["payload"];
        match event["type"].as_str()? {
            "task_started" | "turn_started" => {
                current = event["turn_id"].as_str()?.into();
                if current == intent.wall {
                    starts += 1
                }
                auth = false;
                usage = false;
                ended = false;
            }
            "error" if current == intent.wall => {
                auth = native_login_error(event);
                usage = event["codex_error_info"].as_str() == Some("usage_limit_exceeded");
            }
            "task_complete" | "turn_complete" if event["turn_id"].as_str() == Some(&current) => {
                if let Some(error) = event.get("error") {
                    auth = native_login_error(error);
                    usage = error["codex_error_info"].as_str() == Some("usage_limit_exceeded");
                }
                ended = true;
            }
            _ => {}
        }
    }
    Some(History {
        starts,
        auth_wall: auth && ended && current == intent.wall,
        usage_wall: usage && ended && current == intent.wall,
        latest_is_wall: current == intent.wall,
    })
}

/// Positive completion retained by the private restart record. Reading this
/// does not prepare, signal or launch anything, including after a host crash.
pub fn completed_restart(
    opts: &super::upgrade_drive::Opts,
    id: &str,
) -> Option<(bool, super::native_observe::SavedObservation)> {
    let state = super::upgrade_drive::load(opts, &format!("renewal-{id}"))?;
    let prepare = state.native_completed_prepare.as_ref()?;
    if prepare.id != id {
        return None;
    }
    let observed = state.native_observation?;
    let super::native_act::NativePlan::Restart {
        forward, rollback, ..
    } = &prepare.plan
    else {
        return None;
    };
    let command = if state.native_rollback {
        rollback.as_ref()?
    } else {
        forward
    };
    (observed.registration.vendor.image == command.image)
        .then_some((!state.native_rollback, observed))
}

/// Read only the existing Codex per-tab native completion ledger. A raw line,
/// missing completion, rollback or still-live predecessor cannot rebind a receipt.
pub fn authorized_descendant(
    opts: &super::upgrade_drive::Opts,
    receipt: &Receipt,
    current: &super::native_act::Process,
) -> bool {
    let Some(key) = super::upgrade::Agent::Codex.record_key(&receipt.sid) else {
        return false;
    };
    let Some(state) = super::upgrade_drive::load(opts, &key) else {
        return false;
    };
    if !state.native_coordination
        || state.native_rollback
        || state.agent != super::upgrade::Agent::Codex
        || state.tab != receipt.sid
        || state.thread != receipt.intent.thread
    {
        return false;
    }
    let (Some(completed), Some(observed)) = (
        state.native_completed_prepare.as_deref(),
        state.native_observation.as_ref(),
    ) else {
        return false;
    };
    let super::native_act::NativePlan::Restart { process, .. } = &completed.plan else {
        return false;
    };
    let predecessor_gone = super::native_act::Process::observe(process.pid)
        .is_none_or(|live| live.birth != process.birth);
    receipt.matches_descendant(completed, observed, current, predecessor_gone)
}

/// Read the validated native process record with the same bounded regular-file
/// reader used by native account/state management.
pub fn native_record(copy: &super::self_claimed::Copy) -> Option<aterm_json::Value> {
    retained_native_record(&copy.home, copy.pid)
}

/// Observe the exact record path captured while its source was validated.
/// This observation grants no authority for an exited process.
pub fn retained_native_record(home: &Path, pid: u32) -> Option<aterm_json::Value> {
    let text = accounts::read_bounded(
        &super::self_claimed::record_path(home, pid),
        2 * 1024 * 1024,
    )
    .ok()??;
    aterm_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    const THREAD: &str = "019a4b54-5fa1-7123-8123-123456789abc";
    const WALL: &str = "019a4b54-5fa2-7123-8123-123456789abc";
    const OTHER: &str = "019a4b54-5fa3-7123-8123-123456789abc";

    fn intent() -> Intent {
        Intent {
            purpose: Default::default(),
            thread: THREAD.into(),
            wall: WALL.into(),
            identity: Identity::from_native("workspace", "user"),
        }
    }

    #[test]
    fn protocol_admits_only_exact_bounded_fields() {
        let intent = intent();
        let line = format!(
            "prepare thread={THREAD} wall={WALL} account={} user={}",
            intent.identity.account, intent.identity.user
        );
        assert_eq!(Request::parse(&line), Some(Request::Prepare(intent)));
        for extra in [" path=/tmp/auth", " thread=x", " extra=x"] {
            assert!(Request::parse(&(line.clone() + extra)).is_none());
        }
        assert!(Request::parse("commit id=../file").is_none());
        assert!(Request::parse("status id=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_none());
    }

    #[test]
    fn identity_proofs_are_domain_separated_and_disclose_no_identifiers() {
        let identity = Identity::from_native("same", "same");
        assert_ne!(identity.account, identity.user);
        assert_eq!(identity.account.len(), 64);
        assert!(!format!("{identity:?}").contains("same"));
        assert_ne!(identity, Identity::from_native("other", "same"));
    }

    fn event(value: aterm_json::Value) -> String {
        format!(
            "{}\n",
            aterm_json::json!({"type":"event_msg","payload":value})
        )
    }

    #[test]
    fn native_history_requires_latest_completed_auth_failure() {
        let start = event(aterm_json::json!({"type":"task_started","turn_id":WALL}));
        let error = event(aterm_json::json!({"type":"error","codex_error_info":"unauthorized"}));
        let done = event(aterm_json::json!({"type":"task_complete","turn_id":WALL}));
        let text = format!("{start}{error}{done}");
        assert_eq!(
            history_lines(text.as_bytes(), &intent()).unwrap(),
            History {
                starts: 1,
                auth_wall: true,
                usage_wall: false,
                latest_is_wall: true
            }
        );
        let later = text + &event(aterm_json::json!({"type":"task_started","turn_id":OTHER}));
        assert!(
            !history_lines(later.as_bytes(), &intent())
                .unwrap()
                .auth_wall
        );
        assert!(
            !history_lines(format!("{start}{error}").as_bytes(), &intent())
                .unwrap()
                .auth_wall
        );
    }

    #[test]
    fn terminal_typed_401_is_login_but_other_http_failures_are_not() {
        let start = event(aterm_json::json!({"type":"task_started", "turn_id":WALL}));
        for kind in [
            "http_connection_failed",
            "response_stream_connection_failed",
            "response_too_many_failed_attempts",
            "response_stream_disconnected",
        ] {
            for (status, expected) in [
                ("401", true),
                ("403", false),
                ("null", false),
                ("\"401\"", false),
            ] {
                let error: aterm_json::Value = aterm_json::from_str(&format!(
                    "{{\"codex_error_info\":{{\"{kind}\":{{\"http_status_code\":{status}}}}}}}"
                ))
                .unwrap();
                let done = event(
                    aterm_json::json!({"type":"task_complete", "turn_id":WALL, "error":error}),
                );
                assert_eq!(
                    history_lines(format!("{start}{done}").as_bytes(), &intent())
                        .unwrap()
                        .auth_wall,
                    expected
                );
            }
        }
        for bad in [
            aterm_json::json!({"codex_error_info":{"other":{"http_status_code":401}}}),
            aterm_json::json!({"codex_error_info":{"http_connection_failed":{"http_status_code":401},"unauthorized":null}}),
            aterm_json::json!({"codex_error_info":{"http_connection_failed":{"http_status_code":401,"extra":true}}}),
            aterm_json::json!({"codex_error_info":"other","message":"401 unauthorized"}),
        ] {
            assert!(!native_login_error(&bad));
        }
        let previous =
            event(aterm_json::json!({"type":"error", "codex_error_info":"unauthorized"}));
        let success =
            event(aterm_json::json!({"type":"task_complete", "turn_id":WALL, "error":null}));
        assert!(
            !history_lines(format!("{start}{previous}{success}").as_bytes(), &intent())
                .unwrap()
                .auth_wall
        );
    }

    #[test]
    fn repeated_native_turn_is_counted_before_receipt_consumption() {
        let text = event(aterm_json::json!({"type":"task_started","turn_id":WALL}))
            + &event(aterm_json::json!({"type":"turn_started","turn_id":WALL}));
        assert_eq!(history_lines(text.as_bytes(), &intent()).unwrap().starts, 2);
    }

    #[test]
    fn quota_requests_and_markers_cannot_be_used_as_same_account_renewals() {
        let mut quota = intent();
        quota.purpose = Purpose::WeeklyQuota;
        let request = format!(
            "prepare thread={THREAD} turn={WALL} source_account={} source_user={}",
            quota.identity.account, quota.identity.user,
        );
        assert_eq!(
            Request::parse_rotation(&request),
            Some(Request::Prepare(quota.clone()))
        );
        assert!(Request::parse(&request).is_none());
        assert!(Request::parse_rotation(&(request + " target=/arbitrary/login")).is_none());
        let record = aterm_json::json!({
            "sessionId": THREAD, "status": "waiting", "draftEmpty": true,
            "autopilot": "active", "owed": {
                "thread": THREAD, "mode": "cruise", "rule": "usage-rotate@v1",
                "nativeOrigin": {"turnId": WALL, "phase": "complete"},
                "rotation": {"openaiScope": true, "account": quota.identity.account,
                    "user": quota.identity.user, "receipt": null}
            }
        });
        assert!(quota.matches_record(&record, None));
        assert!(!intent().matches_record(&record, None));
    }

    #[test]
    fn quota_history_is_typed_and_separate_from_authentication_failure() {
        let mut quota = intent();
        quota.purpose = Purpose::WeeklyQuota;
        let start = event(aterm_json::json!({"type":"task_started", "turn_id": WALL}));
        let error =
            event(aterm_json::json!({"type":"error", "codex_error_info":"usage_limit_exceeded"}));
        let done = event(aterm_json::json!({"type":"task_complete", "turn_id": WALL}));
        let history = history_lines(format!("{start}{error}{done}").as_bytes(), &quota).unwrap();
        assert!(quota.matches_history(&history));
        assert!(!intent().matches_history(&history));
        let unfinished = history_lines(format!("{start}{error}").as_bytes(), &quota).unwrap();
        assert!(!quota.matches_history(&unfinished));
        let prose = event(aterm_json::json!({"type":"error", "message":"usage_limit_exceeded"}));
        let untyped = history_lines(format!("{start}{prose}{done}").as_bytes(), &quota).unwrap();
        assert!(!quota.matches_history(&untyped));
    }

    #[test]
    fn intent_index_refuses_oversized_malformed_and_symlink_files() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let path = tmp.path().join("index");
        assert_eq!(read_index(&path), Ok(None));
        std::fs::write(&path, "ab".repeat(16)).unwrap();
        assert_eq!(read_index(&path), Ok(Some("ab".repeat(16))));
        for bad in ["AB".repeat(16), "ab".repeat(17), "z".repeat(32)] {
            std::fs::write(&path, bad).unwrap();
            assert!(read_index(&path).is_err());
        }
        let directory = tmp.path().join("directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(read_index(&directory).is_err());
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_index(&link).is_err());
        }
    }

    fn auth(home: &Path, workspace: &str, user: Option<&str>, explicit: Option<&str>) {
        std::fs::create_dir_all(home).unwrap();
        let claims = aterm_json::json!({"https://api.openai.com/auth": {
            "chatgpt_account_id": workspace, "chatgpt_user_id": user
        }});
        let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
            .unwrap()
            .replace('+', "-")
            .replace('/', "_")
            .trim_end_matches('=')
            .to_owned();
        let token = format!("e30.{encoded}.fixture");
        std::fs::write(home.join("auth.json"), aterm_json::json!({"auth_mode":"chatgpt", "tokens": {
            "id_token":token, "access_token":token, "refresh_token":"fixture", "account_id":explicit
        }}).to_string()).unwrap();
    }

    #[test]
    fn native_workspace_selection_matches_codex_token_precedence() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        auth(
            tmp.path(),
            "default-workspace",
            Some("user"),
            Some("selected-workspace"),
        );
        assert_eq!(
            codex_app_login::renewal_identity(tmp.path()),
            Some(Identity::from_native("selected-workspace", "user"))
        );
        auth(tmp.path(), "default-workspace", None, None);
        assert!(codex_app_login::renewal_identity(tmp.path()).is_none());
    }

    #[test]
    fn successor_requires_same_native_identity_current_generation_and_no_hold() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let mut env = Env::from_process().unwrap();
        env.home = Some(tmp.path().join("home"));
        env.state = tmp.path().join("state");
        env.config = Some(tmp.path().join("aterm.toml"));
        let old = tmp.path().join("old");
        let new = tmp.path().join("new");
        auth(&old, "workspace", Some("user"), None);
        auth(&new, "workspace", Some("user"), None);
        let raw = format!(
            "[[account]]\nlabel='main'\nclass='dedicated'\ndir={}\nemail='owner@example'\n",
            aterm_toml::Value::from(old.to_str().unwrap())
        );
        std::fs::write(env.accounts_roster_for(accounts::Cli::Codex).unwrap(), &raw).unwrap();
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        std::fs::create_dir_all(&store.dir).unwrap();
        let mut logins = accounts::Logins {
            generation_clock: 2,
            labels: std::collections::BTreeMap::from([(
                "main".into(),
                accounts::Binding {
                    dir: Some(new.to_string_lossy().into_owned()),
                    was: vec![Some(old.to_string_lossy().into_owned())],
                    renewal_generation: 2,
                    last_success_generation: 2,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        std::fs::write(store.logins_file(), logins.to_json()).unwrap();
        let identity = Identity::from_native("workspace", "user");
        assert_eq!(
            with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)),
            Ok(Some(true))
        );
        let selected = with_successor(&env, &old.join("auth.json"), &identity, |next| {
            Ok(next.clone())
        })
        .unwrap()
        .unwrap();
        let mut prepared = receipt();
        prepared.old_auth = selected.old_auth.clone();
        prepared.new_auth = selected.new_auth.clone();
        prepared.intent.identity = selected.identity.clone();
        prepared.row_stamp = row_stamp(&selected.row);
        write_receipt(&env, &prepared).unwrap();
        let start = std::sync::Barrier::new(3);
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                start.wait();
                commit_receipt(&env, &prepared)
            });
            let second = scope.spawn(|| {
                start.wait();
                commit_receipt(&env, &prepared)
            });
            start.wait();
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok(Some(_))))
                .count(),
            1
        );
        assert_eq!(
            read_receipt(&env, &prepared.id).unwrap().phase,
            Phase::Committing
        );
        // The actual account writer serializes stale-Ready retirement against
        // late Commit and another fresh preparation, without renewing a login.
        let mut stale = prepared.clone();
        stale.id = "cd".repeat(16);
        stale.native_prepare = "immutable-original-prepare".into();
        with_successor(&env, &stale.old_auth, &identity, |_| {
            write_receipt(&env, &stale)
        })
        .unwrap()
        .unwrap();
        let start = std::sync::Barrier::new(3);
        let retired = std::thread::scope(|scope| {
            let retire = || {
                start.wait();
                with_successor(&env, &stale.old_auth, &identity, |next| {
                    let mut current = read_receipt(&env, &stale.id).ok_or("stale-receipt")?;
                    if current != stale
                        || !current.matches_successor(next)
                        || !current.retire_stale_ready(4, 4, 50)
                    {
                        return Ok(false);
                    }
                    write_receipt(&env, &current)?;
                    Ok(true)
                })
            };
            let first = scope.spawn(retire);
            let second = scope.spawn(retire);
            start.wait();
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(
            retired
                .iter()
                .filter(|outcome| matches!(outcome, Ok(Some(true))))
                .count(),
            1
        );
        assert!(
            commit_receipt(&env, &stale).is_err(),
            "late Commit cannot revive the retired preparation"
        );
        let retired = read_receipt(&env, &stale.id).unwrap();
        assert_eq!(retired.phase, Phase::Retired);
        assert_eq!(retired.native_prepare, stale.native_prepare);
        assert_eq!(retired.expires_ms, stale.expires_ms);
        assert!(!retired.launch_attempted);
        let mut fresh = stale.clone();
        fresh.id = "ef".repeat(16);
        fresh.claim_revision = 4;
        fresh.expires_ms = 200;
        fresh.native_prepare = "distinct-current-prepare".into();
        with_successor(&env, &fresh.old_auth, &identity, |next| {
            assert!(fresh.matches_successor(next));
            write_receipt(&env, &fresh)
        })
        .unwrap()
        .unwrap();
        assert!(commit_receipt(&env, &fresh).unwrap().is_some());
        assert_eq!(read_receipt(&env, &stale.id).unwrap(), retired);
        assert_eq!(
            store.load_logins(env.now).unwrap().0.generation_clock,
            2,
            "resealing reuses the same valid renewed credential generation"
        );
        logins
            .labels
            .get_mut("main")
            .unwrap()
            .last_success_generation = 1;
        std::fs::write(store.logins_file(), logins.to_json()).unwrap();
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        logins
            .labels
            .get_mut("main")
            .unwrap()
            .last_success_generation = 2;
        std::fs::write(store.logins_file(), logins.to_json()).unwrap();
        auth(&new, "workspace", Some("other-user"), None);
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        auth(&new, "other-workspace", Some("user"), None);
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        auth(&new, "workspace", Some("user"), None);
        std::fs::write(env.config.as_ref().unwrap(), "[harness]\nlogins='off'\n").unwrap();
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        for setting in ["relaunch", "resume_limits"] {
            std::fs::write(
                env.config.as_ref().unwrap(),
                format!("[harness]\n{setting}=false\n"),
            )
            .unwrap();
            assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        }
        std::fs::write(
            env.config.as_ref().unwrap(),
            "[harness.codex]\nenabled=false\n",
        )
        .unwrap();
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        std::fs::write(env.config.as_ref().unwrap(), "[harness").unwrap();
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
        std::fs::write(env.config.as_ref().unwrap(), "").unwrap();
        std::fs::write(
            env.accounts_roster_for(accounts::Cli::Codex).unwrap(),
            raw + "held_by='m2'\n",
        )
        .unwrap();
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    }
    fn receipt() -> Receipt {
        Receipt {
            destination: None,
            id: "ab".repeat(16),
            intent: intent(),
            sid: "s-fixture".into(),
            old_pid: 10,
            old_birth: 20,
            claim_revision: 3,
            human_seq: 4,
            old_auth: "/old/auth.json".into(),
            new_auth: "/new/auth.json".into(),
            generation: 2,
            row_stamp: String::new(),
            native_prepare: String::new(),
            expires_ms: 100,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        }
    }

    #[test]
    fn receipt_requires_an_explicit_distinct_quota_destination() {
        let original = receipt();
        assert_eq!(original.target_identity(), Some(&original.intent.identity));
        let value = aterm_json::to_value(&original).unwrap();
        assert!(value.get("destination").is_none());
        assert!(value["intent"].get("purpose").is_none());
        assert_eq!(aterm_json::from_value::<Receipt>(value).unwrap(), original);

        let target = Identity::from_native("next-workspace", "next-user");
        let mut quota = original.clone();
        quota.intent.purpose = Purpose::WeeklyQuota;
        assert!(quota.target_identity().is_none());
        quota.destination = Some(quota.intent.identity.clone());
        assert!(quota.target_identity().is_none());
        quota.destination = Some(target.clone());
        assert_eq!(quota.target_identity(), Some(&target));
        let reply = quota.reply();
        assert!(reply.starts_with("OK codex-rotation state=ready "));
        assert_eq!(reply.split_whitespace().count(), 10);
        assert!(reply.contains(&format!("target_account={}", target.account)));
        assert!(!reply.contains("/old/") && !reply.contains("/new/"));
        let mut changed = quota.clone();
        changed.destination = Some(Identity::from_native("third", "user"));
        assert!(!quota.same_intent(&changed));
        let mut renewal = original;
        renewal.destination = Some(target);
        assert!(renewal.target_identity().is_none());
    }

    #[test]
    fn stale_ready_retires_only_unlaunched_preparations_and_keeps_the_original_recipe() {
        let original = receipt();
        assert!(!original.clone().retire_stale_ready(3, 4, 99));
        for (revision, human, now) in [(4, 4, 50), (3, 5, 50), (3, 4, 100)] {
            let mut stale = original.clone();
            assert!(stale.retire_stale_ready(revision, human, now));
            let mut expected = original.clone();
            expected.phase = Phase::Retired;
            assert_eq!(stale, expected);
        }
        for phase in [
            Phase::Committing,
            Phase::Committed,
            Phase::Consumed,
            Phase::Retired,
        ] {
            let mut active = original.clone();
            active.phase = phase;
            let before = active.clone();
            assert!(!active.retire_stale_ready(4, 5, 200));
            assert_eq!(active, before);
        }
        let mut attempted = original.clone();
        attempted.launch_attempted = true;
        assert!(!attempted.retire_stale_ready(4, 5, 200));
        let mut accepted = original;
        accepted.accepted_turn = Some(WALL.into());
        assert!(!accepted.retire_stale_ready(4, 5, 200));
        let mut revoked = receipt();
        revoked.revoked = true;
        assert!(!revoked.retire_stale_ready(4, 5, 200));
        let mut attempted = receipt();
        attempted.current_attempt = Some(AttemptRef {
            id: "12".repeat(16),
            generation: 1,
        });
        assert!(!attempted.retire_stale_ready(4, 5, 200));
    }

    #[test]
    fn receipt_consumption_is_one_native_turn_and_only_after_commit() {
        let mut receipt = receipt();
        assert!(!receipt.consume(WALL));
        receipt.phase = Phase::Committing;
        assert!(!receipt.consume(WALL));
        receipt.phase = Phase::Committed;
        assert!(!receipt.consume(OTHER));
        assert!(receipt.consume(WALL));
        assert!(receipt.consume(WALL));
        assert!(!receipt.consume(OTHER));
    }

    #[test]
    fn proven_no_signal_recovers_but_unknown_launch_cannot_restart() {
        let mut receipt = receipt();
        receipt.phase = Phase::Committing;
        assert!(receipt.reset_before_signal(90));
        assert_eq!(receipt.phase, Phase::Ready);
        receipt.phase = Phase::Committing;
        assert!(receipt.reset_before_signal(100));
        assert!(receipt.may_replace());
        receipt.phase = Phase::Committing;
        receipt.launch_attempted = true;
        assert!(!receipt.reset_before_signal(100));
        assert!(!receipt.may_replace());
    }

    #[test]
    fn every_known_reply_is_identity_bound_without_native_paths() {
        for phase in [
            Phase::Ready,
            Phase::Committing,
            Phase::Committed,
            Phase::Consumed,
            Phase::Retired,
        ] {
            let mut receipt = receipt();
            receipt.phase = phase;
            let reply = receipt.reply();
            assert_eq!(reply.split_whitespace().count(), 8);
            assert!(reply.contains(&receipt.intent.identity.account));
            assert!(!reply.contains("/old/") && !reply.contains("/new/"));
        }
        assert_eq!(
            Identity::from_native("same", "same").account,
            "4d9a5fa2af08763c11df9b82406d3b53813e1d0e957f9f682bb451234e54ce46"
        );
        assert_eq!(
            Identity::from_native("same", "same").user,
            "002317753bb35b3537fca8bdfaa8c0912d9c3912d607e1ead5afca2482c08814"
        );
    }

    #[test]
    fn persisted_intent_cannot_override_pause_draft_or_other_native_point() {
        let intent = intent();
        let record = |autopilot: &str, draft_empty: bool, wall: &str| {
            aterm_json::json!({"sessionId":THREAD,"status":"waiting","draftEmpty":draft_empty,"autopilot":autopilot,
                "owed":{"thread":THREAD,"mode":"land","rule":"login-back@v1","nativeOrigin":{"turnId":wall,"phase":"complete"},
                "renewal":{"account":intent.identity.account,"user":intent.identity.user,"receipt":null,"openaiScope":true}}})
        };
        assert!(intent.matches_record(&record("active", true, WALL), None));
        assert!(!intent.matches_record(&record("off", true, WALL), None));
        assert!(!intent.matches_record(&record("active", false, WALL), None));
        assert!(!intent.matches_record(&record("active", true, OTHER), None));
        for scope in [
            aterm_json::Value::Null,
            aterm_json::json!(false),
            aterm_json::json!("true"),
        ] {
            let mut unscoped = record("active", true, WALL);
            unscoped
                .as_object_mut()
                .unwrap()
                .get_mut("owed")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .get_mut("renewal")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("openaiScope".into(), scope);
            assert!(!intent.matches_record(&unscoped, None));
        }
        let mut absent = record("active", true, WALL);
        absent
            .as_object_mut()
            .unwrap()
            .get_mut("owed")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("renewal")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("openaiScope");
        assert!(!intent.matches_record(&absent, None));
        let local = aterm_json::json!({"sessionId":THREAD,"status":"waiting","draftEmpty":true,"autopilot":"active",
            "owed":{"thread":THREAD,"mode":"land","rule":"login-back@v1","nativeOrigin":{"turnId":WALL,"phase":"complete"},
            "renewal":{"account":intent.identity.account,"user":intent.identity.user,"receipt":null,"local":true,"openaiScope":true}}});
        assert!(!intent.matches_record(&local, None));
    }
    #[test]
    fn attached_launcher_requires_positive_exact_exec_image_before_recovery() {
        use super::super::native_act::{Image, NativePlan, Prepare, Process, RestartCommand};
        let launcher = Image {
            path: b"/host".to_vec(),
            device: 1,
            inode: 1,
        };
        let target = Image {
            path: b"/codex".to_vec(),
            device: 1,
            inode: 2,
        };
        let attached = Process {
            pid: 50,
            birth: 70,
            parent: 20,
            group: 50,
            session: 20,
            image: launcher.clone(),
        };
        let shell = Process {
            pid: 20,
            birth: 60,
            parent: 1,
            group: 20,
            session: 20,
            image: launcher.clone(),
        };
        let command = RestartCommand {
            image: target.clone(),
            program: "/codex".into(),
            argv: vec![],
            env: vec![],
            unset: None,
            cwd: None,
            heal: None,
            dialect: super::super::upgrade::Dialect::Zsh,
        };
        let mut prepare = Prepare {
            id: "ab".repeat(16),
            host_epoch: "cd".repeat(16),
            holder: "fixture".into(),
            revision: 1,
            expected_human_seq: Some(0),
            deadline_us: 100,
            duty: super::super::contract::Scope::Relaunch,
            plan: NativePlan::Restart {
                process: attached.clone(),
                observation: None,
                shell: shell.clone(),
                launcher: launcher.clone(),
                socket: "/socket".into(),
                forward: Box::new(command),
                rollback: None,
            },
        };
        let mut receipt = receipt();
        receipt.candidate = Some(Candidate::of(&attached, true));
        assert_eq!(receipt.exec_candidate(None, &prepare), None);
        assert_eq!(receipt.exec_candidate(Some(&attached), &prepare), None);
        let mut executed = Process {
            image: target,
            ..attached
        };
        assert_eq!(
            receipt.exec_candidate(Some(&executed), &prepare),
            Some(true)
        );
        // The next restart needs an independently completed native lineage,
        // not merely a fresh PID using this same thread and auth path.
        let mut original = prepare.clone();
        if let NativePlan::Restart { forward, .. } = &mut original.plan {
            forward.env = vec![("CODEX_AUTH_PATH".into(), "/new/auth.json".into())];
        }
        receipt.native_prepare = super::super::native_act::Request::prepare(original.clone())
            .encode()
            .unwrap();
        receipt.successor_pid = Some(executed.pid);
        receipt.successor_birth = Some(executed.birth);
        let mut completed = original;
        if let NativePlan::Restart { process, .. } = &mut completed.plan {
            *process = executed.clone();
        }
        let mut descendant = executed.clone();
        descendant.pid += 1;
        descendant.birth += 1;
        descendant.group = descendant.pid;
        let registration = super::super::native_observe::Registration {
            id: "ef".repeat(16),
            host_epoch: completed.host_epoch.clone(),
            vendor: descendant.clone(),
            shell: shell.clone(),
        };
        let observed = super::super::native_observe::SavedObservation {
            binding: super::super::native_observe::ObservationBinding {
                id: registration.id.clone(),
                host_epoch: registration.host_epoch.clone(),
                capability: "ab".repeat(32),
            },
            registration,
        };
        assert!(receipt.matches_descendant(&completed, &observed, &descendant, true));
        assert!(!receipt.matches_descendant(&completed, &observed, &descendant, false));
        let mut back = completed.clone();
        if let NativePlan::Restart {
            forward, rollback, ..
        } = &mut back.plan
        {
            *rollback = Some(forward.clone());
        }
        let mut restored_prefix = descendant.clone();
        restored_prefix.image = launcher.clone();
        let mut retired = receipt.clone();
        retired.phase = Phase::Retired;
        retired.launch_attempted = true;
        retired.accepted_turn = None;
        retired.successor_pid = Some(descendant.pid);
        retired.successor_birth = Some(descendant.birth);
        retired.candidate = Some(Candidate::of(&restored_prefix, false));
        assert!(retired.matches_completed_rollback(&back, &observed));
        for change in 0..4 {
            let mut invalid = retired.clone();
            match change {
                0 => invalid.successor_birth = Some(descendant.birth + 1),
                1 => invalid.candidate.as_mut().unwrap().forward = true,
                2 => invalid.accepted_turn = Some(invalid.intent.wall.clone()),
                _ => invalid.phase = Phase::Committing,
            }
            assert!(!invalid.matches_completed_rollback(&back, &observed));
        }
        let mut forged = descendant.clone();
        forged.birth += 1;
        assert!(!receipt.matches_descendant(&completed, &observed, &forged, true));
        let mut wrong = completed.clone();
        if let NativePlan::Restart { process, .. } = &mut wrong.plan {
            process.birth += 1;
        }
        assert!(!receipt.matches_descendant(&wrong, &observed, &descendant, true));
        let mut wrong = completed.clone();
        if let NativePlan::Restart { forward, .. } = &mut wrong.plan {
            forward.env.clear();
        }
        assert!(!receipt.matches_descendant(&wrong, &observed, &descendant, true));
        executed.birth += 1;
        assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
        executed.birth -= 1;
        executed.image.inode += 1;
        assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
        executed.image.inode -= 1;
        receipt.candidate.as_mut().unwrap().launcher_inode += 1;
        assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
        receipt.candidate.as_mut().unwrap().launcher_inode -= 1;
        if let NativePlan::Restart { forward, .. } = &mut prepare.plan {
            forward.image = launcher.clone();
        }
        executed.image = launcher;
        assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
    }
}
