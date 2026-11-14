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
        && accounts_sync::sign_ins_on(config, accounts::Cli::Codex)
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
    let candidates = roster.rows_of_login(old_dir.as_deref());
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
    let mut state = store.load(env.now).map_err(|_| "state-unreadable")?.0;
    store.hydrate_auth_contexts(&mut state, &roster);
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
    if expected.phase != Phase::Ready || expected.revoked || !expected.has_no_execution_evidence() {
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

/// Retire a native reservation that never issued Go. The caller must prove
/// that its execution gate is closed and no successor ran. The retained
/// metadata must also contain no execution evidence.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeTerminalEvidence",
        action = "NextCase",
        reason = "The fixture supplies every combination of retained execution witnesses"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeTerminalEvidence",
        action = "Apply",
        project = "aterm_agent::harness::accounts_pool::tests::quota_proposals::canary_ownership::terminal_evidence::transition"
    )
)]
pub fn cancel_unexecuted_rotation(
    env: &Env,
    receipt: &Receipt,
    target: &str,
) -> Result<Option<Receipt>, &'static str> {
    if !receipt.has_no_execution_evidence()
        || !matches!(receipt.phase, Phase::Ready | Phase::Retired)
    {
        return Err("rotation-already-executed");
    }
    let mut retired = receipt.clone();
    retired.phase = Phase::Retired;
    retired.revoked = true;
    with_receipt_writer(env, |writer| {
        let stored = read_receipt(env, &receipt.id);
        if stored.as_ref() != Some(receipt) && stored.as_ref() != Some(&retired) {
            return Err("rotation-receipt-changed");
        }
        write_receipt(env, &retired)?;
        writer.release_unexecuted_rotation(&retired, target)?;
        Ok(retired)
    })
}

/// Record native evidence that the consumed original turn served a successful
/// response. Only the native host may supply that evidence; a receipt alone
/// never proves process execution or completion.
pub fn record_completed_rotation(env: &Env, receipt: &Receipt) -> Result<Option<()>, &'static str> {
    super::codex_rotation::with_completed_writer(env, receipt, |next, writer| {
        writer.prove_completed_rotation(receipt, &next.row.label)
    })
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
        let settled = target.settled();
        *settled != *launcher
            && self.pid == process.pid
            && self.birth == process.birth
            && self.parent == process.parent
            && self.group == process.group
            && self.session == process.session
            && self.launcher_device == launcher.device
            && self.launcher_inode == launcher.inode
            && self.launcher_path == launcher.path
            && process.image == *settled
    }
}

#[path = "codex_renewal_attempts.rs"]
mod attempts;
pub use attempts::{
    Attempt, AttemptRef, Cancellation, CancellationKind, CompletionProgress, IndexKey,
    NativeCancellationProof, NativeUnattachedProof, ReceiptWriter, RootDriver, advance_root,
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
    /// Whether every execution witness is absent from this receipt.
    /// This is a metadata check, not proof that native execution never ran;
    /// callers still need the execution gate's closure and provenance.
    #[must_use]
    pub fn has_no_execution_evidence(&self) -> bool {
        !self.launch_attempted
            && self.accepted_turn.is_none()
            && self.current_attempt.is_none()
            && self.candidate.is_none()
            && self.successor_pid.is_none()
            && self.successor_birth.is_none()
    }

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

    fn has_rollback_candidate(&self) -> bool {
        self.phase == Phase::Retired
            && self.launch_attempted
            && self.accepted_turn.is_none()
            && self.successor_pid.is_some()
            && self.successor_birth.is_some()
            && self
                .candidate
                .as_ref()
                .is_some_and(|candidate| !candidate.forward)
    }

    /// The completed native rollback must name this exact restored process,
    /// with no accepted recovery turn. This observes closure, not execution authority.
    fn matches_completed_rollback(
        &self,
        prepare: &super::native_act::Prepare,
        observed: &super::native_observe::SavedObservation,
    ) -> bool {
        let restored = &observed.registration.vendor;
        self.has_rollback_candidate()
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
            && process.image == *previous.settled()
            && &observed.registration.vendor == current
            && &observed.registration.shell == shell
            && current.parent == shell.pid
            && current.session == shell.session
            && current.group == current.pid
            && observed.registration.host_epoch == completed.host_epoch
            && observed.binding.host_epoch == completed.host_epoch
            && observed.binding.id == observed.registration.id
            && current.image == *forward.settled()
            && forward
                .env
                .iter()
                .any(|(key, value)| key == "CODEX_AUTH_PATH" && Path::new(value) == self.new_auth)
    }

    pub fn may_replace(&self) -> bool {
        self.phase == Phase::Retired && self.has_no_execution_evidence() && !self.revoked
    }

    /// Close an unlaunched Ready receipt before deriving a distinct preparation
    /// for today's claim/human epoch. The original plan and budget stay frozen.
    /// Caller holds the account writer and has revalidated its selected successor.
    pub fn retire_stale_ready(&mut self, revision: u64, human_seq: u64, now_ms: u64) -> bool {
        if self.phase != Phase::Ready
            || self.revoked
            || !self.has_no_execution_evidence()
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
        if self.phase != Phase::Committing || self.revoked || !self.has_no_execution_evidence() {
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

/// Caller holds the Codex store writer guard, also serializing concurrent
/// prepare, commit and consume for this receipt.
/// Accepted history cannot regress even when a caller retained an older mirror.
/// An authorized descendant may still update the successor process binding.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ConsumedReceiptPublication",
        action = "NextCase",
        reason = "The caller supplies a retained receipt and a proposed replacement"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ConsumedReceiptPublication",
        action = "PriorRevocation",
        reason = "The retained root may already carry revocation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ConsumedReceiptPublication",
        action = "NextFault",
        reason = "The filesystem can refuse a directory durability barrier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ConsumedReceiptPublication",
        action = "Publish",
        project = "aterm_agent::harness::codex_renewal::attempts::tests::consumed::transition"
    )
)]
pub fn write_receipt(env: &Env, receipt: &Receipt) -> Result<(), &'static str> {
    if receipt.target_identity().is_none()
        || !hex(&receipt.intent.identity.account, 64)
        || !hex(&receipt.intent.identity.user, 64)
    {
        return Err("receipt-identity-transition");
    }
    // Read the raw root, not its cancellation projection. A failed consume
    // acknowledgement may have left Consumed on disk while the caller still
    // holds Committing; neither that caller nor a stale cancellation may erase
    // the accepted turn. An unreadable root is not an absent root.
    if let Some(stored) = attempts::read_root(env, &receipt.id)?
        && stored.phase == Phase::Consumed
        && (receipt.phase != Phase::Consumed
            || receipt.accepted_turn != stored.accepted_turn
            || !receipt.same_intent(&stored)
            || receipt.current_attempt != stored.current_attempt
            || receipt.candidate != stored.candidate
            || (stored.successor_pid.is_some() && receipt.successor_pid.is_none())
            || (stored.successor_birth.is_some() && receipt.successor_birth.is_none())
            || (stored.launch_attempted && !receipt.launch_attempted)
            || (stored.revoked && !receipt.revoked))
    {
        return Err("receipt-consumed-changed");
    }
    if receipt.phase == Phase::Consumed
        && (receipt.accepted_turn.as_deref() != Some(receipt.intent.wall.as_str())
            || receipt.successor_pid.is_some() != receipt.successor_birth.is_some())
    {
        return Err("receipt-consumed-shape");
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
    super::atomic_write::write_atomic_mode(&path, &text, Some(0o600))
        .map_err(|_| "receipt-write")?;
    aterm_durable::Flush::Durable
        .dir(parent)
        .map_err(|_| "receipt-directory-flush")
}

/// Validate the caller's request before its ID becomes a record path.
fn restart_wire(prepare: &super::native_act::Prepare) -> Option<String> {
    use super::native_act;
    if !matches!(prepare.plan, native_act::NativePlan::Restart { .. }) {
        return None;
    }
    native_act::Request::prepare(prepare.clone()).encode()
}

/// A record's filename is not evidence that it belongs to this restart.
/// Compare the complete request while retaining its supported wire spelling.
fn matching_restart(
    state: &super::upgrade_drive::St,
    prepare: &super::native_act::Prepare,
) -> bool {
    restart_wire(prepare).is_some()
        && super::native_restart::retained_prepare(state)
            .is_ok_and(|retained| retained.as_ref() == prepare)
}

/// Prepare only the exact host-derived private plan. The caller owns the
/// matching sealed native admission; neither this API nor the wire chooses it.
pub fn prepare_restart(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
) -> Result<(), &'static str> {
    use super::{native_act, upgrade_drive};
    let encoded = restart_wire(prepare).ok_or("restart-plan")?;
    if opts.dry_run {
        return Err("restart-dry-run");
    }
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
    let mut state = upgrade_drive::load_record(opts, &key)
        .map_err(|_| "restart-record-unreadable")?
        .unwrap_or_else(|| upgrade_drive::St {
            pid: process.pid,
            pid_start: super::native_restart::vendor_start(process),
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
            native_prepare: encoded,
            native_coordination: true,
            ..Default::default()
        });
    if state.native_completed_prepare.is_some() || state.native_noexec_prepare.is_some() {
        return recover_completion(opts, sid, prepare, &state)
            .map(|_| ())
            .ok_or("restart-completion");
    }
    if !matching_restart(&state, prepare) {
        return Err("restart-plan");
    }
    upgrade_drive::save_checked(opts, &key, &state).map_err(|_| "restart-record")?;
    let mut client = upgrade_drive::connect(opts, sid).map_err(|_| "private-client")?;
    let recovery = super::native_restart::recover(opts, &key, &mut client, sid, &mut state);
    upgrade_drive::save_checked(opts, &key, &state).map_err(|_| "restart-record")?;
    match recovery {
        Ok(
            super::native_restart::Recovery::Admitted | super::native_restart::Recovery::Completed,
        ) => Ok(()),
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
    if restart_wire(prepare).is_none() || opts.dry_run {
        return;
    }
    let key = format!("renewal-{}", prepare.id);
    let Ok(Some(mut state)) = upgrade_drive::load_record(opts, &key) else {
        return;
    };
    if state.native_completed_prepare.is_some() || state.native_noexec_prepare.is_some() {
        let _completion = recover_completion(opts, sid, prepare, &state);
        return;
    }
    if !matching_restart(&state, prepare) {
        return;
    }
    let Ok(mut client) = upgrade_drive::connect(opts, sid) else {
        return;
    };
    let _retired = native_restart::retire(opts, &key, &mut client, sid, &mut state);
    let _saved = upgrade_drive::save_checked(opts, &key, &state);
}

/// Complete an already prepared operation with the existing PrivateRestart
/// signal/resume/commit protocol. No raw terminal typing fallback is permitted.
pub fn drive_restart(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
) -> Option<RestartCompletion> {
    drive_admitted(opts, sid, prepare, true, None)
}

/// Drive a separately sealed continuation whose predecessor is already closed.
/// The host owns the closure proof; this transport never sends Signal. `latched` is the
/// phase the host admitted it in: Rollback when its predecessor closed in Rollback, whose
/// Forward the host refuses, so this drive never asks for it.
pub fn drive_resume_only(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    latched: super::native_act::ImagePhase,
) -> Option<RestartCompletion> {
    drive_admitted(opts, sid, prepare, false, Some(latched))
}

fn drive_admitted(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    signal_original: bool,
    latched: Option<super::native_act::ImagePhase>,
) -> Option<RestartCompletion> {
    use super::upgrade_drive;
    restart_wire(prepare)?;
    if opts.dry_run {
        return None;
    }
    let key = format!("renewal-{}", prepare.id);
    let state = upgrade_drive::load_record(opts, &key).ok()??;
    if state.native_completed_prepare.is_some() || state.native_noexec_prepare.is_some() {
        return recover_completion(opts, sid, prepare, &state);
    }
    if !matching_restart(&state, prepare) {
        return None;
    }
    let client = upgrade_drive::connect(opts, sid).ok()?;
    drive_recovered(
        opts,
        sid,
        prepare,
        state,
        client,
        signal_original,
        latched,
        &HostPace,
    )
}

/// Replay the exact retained preparation on `client`, then drive it on `pace`'s clock from the
/// phase the host latched for it: a record mirroring a Rollback latch never asks for Forward.
#[allow(clippy::too_many_arguments)]
pub(super) fn drive_recovered(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    mut state: super::upgrade_drive::St,
    mut client: super::upgrade_drive::Client,
    signal_original: bool,
    latched: Option<super::native_act::ImagePhase>,
    pace: &impl Pace,
) -> Option<RestartCompletion> {
    use super::{native_restart, upgrade_drive};
    let key = format!("renewal-{}", prepare.id);
    native_restart::recover(opts, &key, &mut client, sid, &mut state).ok()?;
    if state.native_completed_prepare.is_some() || state.native_noexec_prepare.is_some() {
        // Even a terminal receipt without an observation must survive reload.
        upgrade_drive::save_checked(opts, &key, &state).ok()?;
        return completion_value(&state, sid, prepare);
    }
    // The record mirrors the host's frozen phase; the drive saves it before any Resume.
    if latched == Some(super::native_act::ImagePhase::Rollback) {
        state.native_rollback = true;
    }
    drive_connection_paced(opts, sid, prepare, state, client, signal_original, pace)
}

/// The clock and the sleep a connection drive paces its host polls by ([`drive_connection_paced`]).
pub(super) trait Pace {
    /// This host's transaction clock ([`super::native_act::monotonic_us`]).
    fn now_us(&self) -> Option<u64>;
    fn sleep(&self, wait: std::time::Duration);
}

struct HostPace;

impl Pace for HostPace {
    fn now_us(&self) -> Option<u64> {
        super::native_act::monotonic_us()
    }

    fn sleep(&self, wait: std::time::Duration) {
        std::thread::sleep(wait);
    }
}

/// The wait before a connection drive's next host poll: [`Backoff::FLOOR`] after a poll that
/// moved (a Resume the host typed, the old agent's exit, a phase whose launch it reported
/// unissued), doubling to [`Backoff::CAP`] while every answer is unknown or a refusal.
pub(super) struct Backoff(std::time::Duration);

impl Backoff {
    pub(super) const FLOOR: std::time::Duration = std::time::Duration::from_millis(100);
    pub(super) const CAP: std::time::Duration = std::time::Duration::from_secs(2);

    pub(super) fn new() -> Self {
        Self(Self::FLOOR)
    }

    pub(super) fn next(&mut self, moved: bool) -> std::time::Duration {
        let wait = if moved { Self::FLOOR } else { self.0 };
        self.0 = wait.saturating_mul(2).min(Self::CAP);
        wait
    }
}

/// How long a drive asks for its Forward phase before it asks for its rollback instead.
const FORWARD_US: u64 = 60_000_000;

/// [`drive_connection_paced`] on this host's clock, for the transport tests.
#[cfg(test)]
pub(super) fn drive_connection(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    state: super::upgrade_drive::St,
    client: super::upgrade_drive::Client,
    signal_original: bool,
) -> Option<RestartCompletion> {
    drive_connection_paced(
        opts,
        sid,
        prepare,
        state,
        client,
        signal_original,
        &HostPace,
    )
}

/// Drive one admitted operation to completion on `pace`'s clock. A phase the host answered
/// Resumed for is not asked again: the host typed its line, and repeats are answered unknown
/// until it proves that launch unissued, when it may be typed once more. Every other answer
/// (unknown while an earlier image settles, a refusal, no answer) is asked again on the next
/// poll. Between polls the drive backs off ([`Backoff`]), waking at once when the old agent's
/// exit is first seen.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeReplayPublication",
        action = "Attempt",
        project = "aterm_agent::harness::native_restart::transport_tests::replay_model::renewal_transition"
    )
)]
pub(super) fn drive_connection_paced(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    prepare: &super::native_act::Prepare,
    mut state: super::upgrade_drive::St,
    mut client: super::upgrade_drive::Client,
    signal_original: bool,
    pace: &impl Pace,
) -> Option<RestartCompletion> {
    use super::{native_act, native_restart, upgrade_drive};
    restart_wire(prepare)?;
    if opts.dry_run {
        return None;
    }
    if state.native_completed_prepare.is_some() || state.native_noexec_prepare.is_some() {
        return recover_completion(opts, sid, prepare, &state);
    }
    let key = format!("renewal-{}", prepare.id);
    if !matching_restart(&state, prepare) {
        return None;
    }
    if state.native_retired_unissued {
        upgrade_drive::save_checked(opts, &key, &state).ok()?;
        return None;
    }
    let binding = state.native.clone()?;
    let native_act::NativePlan::Restart { process, .. } = &prepare.plan else {
        return None;
    };
    // A loaded or previously checked record can follow an unacknowledged
    // rename or a newer writer. This attempt needs its own durable save.
    upgrade_drive::save_checked(opts, &key, &state).ok()?;
    if signal_original {
        let signal = native_restart::signal(&mut client, sid, &binding);
        // An unread/truncated response makes this connection unusable, not evidence
        // that the effect was absent. Reconcile the same retained operation anew.
        if client.reply_unread() {
            return None;
        }
        // A renewal is a private restart, which no lapse answers ([`native_restart::Recovery::Lapsed`]).
        if !matches!(signal, upgrade_drive::Terminated::Sent)
            && native_restart::abort(opts, &key, &mut client, sid, &mut state).is_some()
        {
            upgrade_drive::save_checked(opts, &key, &state).ok()?;
            return completion_value(&state, sid, prepare);
        }
    }
    if state.native_retired_unissued {
        upgrade_drive::save_checked(opts, &key, &state).ok()?;
        return None;
    }
    let started = pace.now_us();
    let exited = || {
        native_act::Process::observe(process.pid)
            .is_none_or(|current| current.birth != process.birth)
    };
    let mut backoff = Backoff::new();
    let mut gone = false;
    // The phase the host answered Resumed for: it typed that phase's line.
    let mut typed: Option<native_act::ImagePhase> = None;
    while pace.now_us().is_some_and(|now| now < prepare.deadline_us) {
        let poll = native_restart::poll_completion(opts, &key, &mut client, sid, &mut state);
        if poll.completed {
            // Preserve closure even when the host has no usable observation.
            // A later read must remain inert instead of replaying the operation.
            upgrade_drive::save_checked(opts, &key, &state).ok()?;
            return completion_value(&state, sid, prepare);
        }
        if state.native_noexec_prepare.is_some() {
            upgrade_drive::save_checked(opts, &key, &state).ok()?;
            return completion_value(&state, sid, prepare);
        }
        if state.native_retired_unissued {
            upgrade_drive::save_checked(opts, &key, &state).ok()?;
            return None;
        }
        if client.reply_unread() {
            return None;
        }
        let mut moved = false;
        if poll.unissued.is_some() && poll.unissued == typed {
            // A launch the host proved unissued may be typed again at once.
            typed = None;
            moved = true;
        }
        if !gone && exited() {
            gone = true;
            moved = true;
        }
        let phase = if state.native_rollback
            || started
                .zip(pace.now_us())
                .is_some_and(|(started, now)| now.saturating_sub(started) >= FORWARD_US)
        {
            native_act::ImagePhase::Rollback
        } else {
            native_act::ImagePhase::Forward
        };
        if gone
            && typed != Some(phase)
            && let Some(screen) = upgrade_drive::screen(&mut client, sid)
            && let Some(generation) = screen.generation
            && native_restart::resume(
                &mut client,
                sid,
                &binding,
                phase,
                &generation,
                prepare.expected_human_seq?,
            )
            .is_ok()
        {
            typed = Some(phase);
            moved = true;
            if phase == native_act::ImagePhase::Rollback {
                state.native_rollback = true;
                let _saved = upgrade_drive::save_checked(opts, &key, &state);
            }
        }
        if client.reply_unread() {
            return None;
        }
        let mut left = backoff.next(moved);
        while !left.is_zero() {
            let slice = left.min(Backoff::FLOOR);
            pace.sleep(slice);
            left = left.saturating_sub(slice);
            if !gone && exited() {
                break;
            }
        }
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
    let (file, meta) = super::footer::open_regular_with_meta(&path)?;
    if meta.len() > 512 * 1024 * 1024 {
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

fn completed_header_matches(
    state: &super::upgrade_drive::St,
    sid: &str,
    expected: &super::native_act::Prepare,
) -> bool {
    use super::{native_act::NativePlan, upgrade::Phase};
    restart_wire(expected).is_some()
        && state.tab == sid
        && state.native_coordination
        && state.native.is_none()
        && state.native_prepare.is_empty()
        && !state.native_retired_unissued
        && state.native_account_candidate.is_none()
        && matches!(state.phase, Phase::Relaunched { .. } | Phase::Done)
        && state.native_completed_prepare.as_deref() == Some(expected)
        && state.native_noexec_prepare.is_none()
        && matches!(&expected.plan, NativePlan::Restart { rollback, .. }
            if !state.native_rollback || rollback.is_some())
}

/// Validate historical completion data for this exact operation. This neither
/// proves current process liveness nor grants native execution authority.
fn completed_state(
    state: &super::upgrade_drive::St,
    sid: &str,
    expected: &super::native_act::Prepare,
) -> Option<(bool, super::native_observe::SavedObservation)> {
    use super::native_act::NativePlan;
    if !completed_header_matches(state, sid, expected) {
        return None;
    }
    let observed = state.native_observation.as_ref()?;
    // The observation's own codec validates bounds and pairs its independent
    // registration and binding IDs/epochs. Its ID is not the native operation ID.
    observed.encode()?;
    let NativePlan::Restart {
        process,
        shell,
        forward,
        rollback,
        ..
    } = &expected.plan
    else {
        return None;
    };
    let command = if state.native_rollback {
        rollback.as_ref()?
    } else {
        forward
    };
    let vendor = &observed.registration.vendor;
    (observed.registration.host_epoch == expected.host_epoch
        && &observed.registration.shell == shell
        && vendor.pid != shell.pid
        && vendor.parent == shell.pid
        && vendor.session == shell.session
        && vendor.group == vendor.pid
        && (vendor.pid != process.pid || vendor.birth != process.birth)
        && &vendor.image == command.settled())
    .then(|| (!state.native_rollback, observed.clone()))
}

/// A fresh read must acknowledge its own durable save before publishing the
/// completion. A failed barrier or a newer writer keeps this attempt inert.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "NextFault",
        reason = "The harness selects malformed retained data or mismatched caller evidence"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "Rollback",
        reason = "An earlier host execution completed in the rollback phase"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "Wrapper",
        reason = "The planner captured a command with a separate settled image"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "Dry",
        reason = "The caller supplies options forbidding durable changes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "Live",
        reason = "The caller supplies ordinary execution options"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "FailBarrier",
        reason = "The injected filesystem refuses the directory durability barrier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeCompletionRecovery",
        action = "RepairBarrier",
        reason = "The injected filesystem resumes acknowledging directory durability"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeCompletionRecovery",
        action = "Attempt",
        project = "aterm_agent::harness::native_restart::transport_tests::completion::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeNoexecRecovery",
        action = "NextFault",
        reason = "The caller or retained record supplies mismatched recovery evidence"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeNoexecRecovery",
        action = "Expire",
        reason = "The original host deadline passes before a later historical read"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeNoexecRecovery",
        action = "Dry",
        reason = "The caller supplies dry-run options"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeNoexecRecovery",
        action = "Live",
        reason = "The caller supplies ordinary execution options"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeNoexecRecovery",
        action = "FailBarrier",
        reason = "The filesystem refuses the directory durability barrier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeNoexecRecovery",
        action = "RepairBarrier",
        reason = "The filesystem resumes acknowledging directory durability"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeNoexecRecovery",
        action = "Attempt",
        project = "aterm_agent::harness::native_restart::transport_tests::noexec::transition"
    )
)]
pub(super) fn recover_completion(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    expected: &super::native_act::Prepare,
    state: &super::upgrade_drive::St,
) -> Option<RestartCompletion> {
    if opts.dry_run {
        return None;
    }
    let noexec = noexec_state(state, sid, expected);
    if !noexec && !completed_header_matches(state, sid, expected) {
        return None;
    }
    let completion = completion_value(state, sid, expected);
    if !noexec && state.native_observation.is_some() && completion.is_none() {
        return None;
    }
    super::upgrade_drive::save_checked(opts, &format!("renewal-{}", expected.id), state).ok()?;
    completion
}

fn noexec_state(
    state: &super::upgrade_drive::St,
    sid: &str,
    expected: &super::native_act::Prepare,
) -> bool {
    restart_wire(expected).is_some()
        && state.tab == sid
        && super::native_restart::noexec_prepare(state) == Some(expected)
}

fn completion_value(
    state: &super::upgrade_drive::St,
    sid: &str,
    expected: &super::native_act::Prepare,
) -> Option<RestartCompletion> {
    if noexec_state(state, sid, expected) {
        return Some(RestartCompletion::NotStarted);
    }
    let (forward, observation) = completed_state(state, sid, expected)?;
    Some(if forward {
        RestartCompletion::Forward(observation)
    } else {
        RestartCompletion::Rollback(observation)
    })
}

/// Revalidate and durably acknowledge the private restart record for the full
/// expected request and session, without preparing, signalling or launching
/// anything, including after a host crash or the original deadline.
pub fn restart_completion(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    expected: &super::native_act::Prepare,
) -> Option<RestartCompletion> {
    if opts.dry_run {
        return None;
    }
    restart_wire(expected)?;
    let state =
        super::upgrade_drive::load_record(opts, &format!("renewal-{}", expected.id)).ok()??;
    recover_completion(opts, sid, expected, &state)
}

/// The observation-only view excludes attempts proven never to have started.
pub fn completed_restart(
    opts: &super::upgrade_drive::Opts,
    sid: &str,
    expected: &super::native_act::Prepare,
) -> Option<(bool, super::native_observe::SavedObservation)> {
    match restart_completion(opts, sid, expected)? {
        RestartCompletion::Forward(observation) => Some((true, observation)),
        RestartCompletion::Rollback(observation) => Some((false, observation)),
        RestartCompletion::NotStarted => None,
    }
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
    let Ok(Some(state)) = super::upgrade_drive::load_record(opts, &key) else {
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
#[path = "codex_renewal_tests.rs"]
mod tests;
