// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit native account management. Accepted operations own their work; wire
//! requests carry opaque row references and never credential paths or commands.

use super::*;
use aterm_agent::harness::{account_work, accounts, cli::Env, codex_enrollment as enrollment};
use aterm_json::{Value, json};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::OnceLock;

#[path = "codex_enrollment_cleanup.rs"]
mod cleanup;

const MAX_JOBS: usize = 32;
const MAX_FRAME: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Enroll {
        operation: String,
        revision: String,
        account: String,
    },
    Poll(String),
    Cancel(String),
    Discover(String),
    Diagnostics {
        request: String,
        operation: String,
    },
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse(line: &str, read: bool) -> Option<Command> {
    let words: Vec<_> = line.split_ascii_whitespace().collect();
    let field = |i: usize, prefix: &str, len: usize| {
        let value = words.get(i)?.strip_prefix(prefix)?;
        hex(value, len).then(|| value.to_owned())
    };
    if words.get(1) != Some(&"schema=1") {
        return None;
    }
    match (words.first().copied()?, read, words.len()) {
        ("enroll", false, 5) => Some(Command::Enroll {
            operation: field(2, "operation=", 32)?,
            revision: field(3, "revision=", 64)?,
            account: field(4, "account=", 64)?,
        }),
        ("enrollment-poll", false, 3) => Some(Command::Poll(field(2, "operation=", 32)?)),
        ("enrollment-cancel", false, 3) => Some(Command::Cancel(field(2, "operation=", 32)?)),
        ("enrollment-status", true, 3) => Some(Command::Discover(field(2, "request=", 32)?)),
        ("enrollment-diagnostics", true, 4) => Some(Command::Diagnostics {
            request: field(2, "request=", 32)?,
            operation: field(3, "operation=", 32)?,
        }),
        _ => None,
    }
}

impl Command {
    fn id(&self) -> &str {
        match self {
            Self::Enroll { operation, .. }
            | Self::Poll(operation)
            | Self::Cancel(operation)
            | Self::Discover(operation) => operation,
            Self::Diagnostics { request, .. } => request,
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Self::Enroll { .. } => "enroll",
            Self::Poll(_) => "poll",
            Self::Cancel(_) => "cancel",
            Self::Discover(_) => "discover",
            Self::Diagnostics { .. } => "diagnostics",
        }
    }
    fn may_publish(&self) -> bool {
        matches!(self, Self::Enroll { .. } | Self::Poll(_))
    }
}

fn caller(sid: &str, proof: &crate::self_claim::Proven) -> String {
    let bytes = json!([
        "aterm-codex-enrollment-caller-v1",
        sid,
        proof.pid,
        proof.start
    ]);
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
        aterm_json::to_vec(&bytes).expect("native caller facts serialize"),
    ))
}

fn session(sid: &str, proof: &crate::self_claim::Proven) -> Option<Value> {
    Some(
        json!({"sid":sid,"process_pid":proof.pid,"process_birth":proof.start,"epoch":host_epoch()?}),
    )
}

fn still_native(ctx: &crate::SessionCtx, proof: &crate::self_claim::Proven, master: i32) -> bool {
    if !proof.alive()
        || i64::from(crate::quit_safety::foreground_pgrp(master)) != i64::from(proof.pid)
    {
        return false;
    }
    let holder = format!("codex-alab@{}", proof.pid);
    let qualified = ctx.native_policy.holder(&holder).map_or_else(
        || crate::self_claim::runs_alab_build(proof.pid),
        |(row, pid)| pid == proof.pid && ctx.native_policy.actor_image(row, pid),
    );
    qualified && proof.alive()
}

fn progress(request: &enrollment::RejectedIntent, status: &enrollment::Status) -> Value {
    json!({"operation":request.id,"account_id":request.account_id,
        "phase":status.phase,"generation":status.generation})
}

struct Job {
    cleanup_owner: Option<enrollment::RetirementControl>,
    command: Command,
    proof: crate::self_claim::Proven,
    started: std::time::Instant,
    intent_sequence: u64,
    result: Mutex<Option<Result<Option<Value>, ()>>>,
}

type Key = (String, &'static str, String);
#[derive(Default)]
struct Registry {
    entries: BTreeMap<Key, Arc<Job>>,
    sequence: u64,
    exhausted: bool,
}

impl Registry {
    fn changed(&mut self) {
        match self.sequence.checked_add(1) {
            Some(sequence) => self.sequence = sequence,
            None => self.exhausted = true,
        }
    }
    fn insert(&mut self, key: Key, job: Arc<Job>) {
        if job.command.may_publish() {
            self.changed();
        }
        self.entries.insert(key, job);
    }
    fn remove(&mut self, key: &Key, job: &Arc<Job>) {
        if self
            .entries
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, job))
        {
            if job.command.may_publish() {
                self.changed();
            }
            self.entries.remove(key);
        }
    }
    fn prune(&mut self) {
        let expired: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, job)| {
                job.started.elapsed() >= Duration::from_secs(120)
                    && job
                        .result
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some()
            })
            .map(|(key, job)| (key.clone(), Arc::clone(job)))
            .collect();
        for (key, job) in expired {
            self.remove(&key, &job);
        }
    }
}

fn jobs() -> &'static Mutex<Registry> {
    static JOBS: OnceLock<Mutex<Registry>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

/// Rolls an unstarted dispatch back before its last admission scope retires.
struct Reservation {
    key: Key,
    job: Option<Arc<Job>>,
    scope: Option<account_work::Scope>,
}

impl Reservation {
    fn new(key: Key) -> Self {
        Self {
            key,
            job: None,
            scope: None,
        }
    }

    fn arm(&mut self, job: &Arc<Job>, scope: &account_work::Scope) {
        self.scope = Some(scope.clone());
        self.job = Some(Arc::clone(job));
    }

    fn dispatched(mut self) {
        self.job = None;
        drop(self.scope.take());
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            jobs()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.key, job);
        }
    }
}

fn complete(job: &Arc<Job>, result: Result<Option<Value>, ()>) {
    complete_in(jobs(), job, result);
}

fn complete_in(registry: &Mutex<Registry>, job: &Arc<Job>, result: Result<Option<Value>, ()>) {
    let mut table = registry.lock().unwrap_or_else(|p| p.into_inner());
    if job.command.may_publish() {
        table.changed();
    }
    *job.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
}

fn owned_completion(
    job: &Arc<Job>,
    scope: account_work::Scope,
    work: impl FnOnce() -> Result<Option<Value>, ()> + Send + 'static,
) -> impl FnOnce() + Send + 'static {
    let job = Arc::clone(job);
    scope.wrap(move || {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
            Ok(result) => result,
            Err(payload) => {
                if let Err(secondary) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        drop(payload);
                    }))
                {
                    std::mem::forget(secondary);
                }
                Err(())
            }
        };
        complete(&job, result);
    })
}

fn launch(key: &Key, job: &Arc<Job>, spawn: impl FnOnce() -> std::io::Result<()>) -> bool {
    if spawn().is_ok() {
        return true;
    }
    complete(job, Err(()));
    jobs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(key, job);
    false
}

struct OwnedControl {
    control: enrollment::Control,
    env: Env,
}

impl std::ops::Deref for OwnedControl {
    type Target = enrollment::Control;
    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

type Controls = BTreeMap<(String, String), Arc<OwnedControl>>;
fn controls() -> &'static Mutex<Controls> {
    static CONTROLS: OnceLock<Mutex<Controls>> = OnceLock::new();
    CONTROLS.get_or_init(Mutex::default)
}

fn remove_control(key: &(String, String), control: &Arc<OwnedControl>) {
    let mut table = controls().lock().unwrap_or_else(|p| p.into_inner());
    if table
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, control))
    {
        table.remove(key);
    }
}

type OwnedControlEntry = ((String, String), Arc<OwnedControl>);

fn owned_controls(lifetime: &account_work::Lifetime) -> Vec<OwnedControlEntry> {
    controls()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(_, control)| control.owned_by(lifetime))
        .map(|(key, control)| (key.clone(), Arc::clone(control)))
        .collect()
}

fn settle_control_custody(lifetime: &account_work::Lifetime) -> bool {
    let mut pending = false;
    for (_, control) in owned_controls(lifetime) {
        if control.has_pending_custody() {
            let _ = control.settle_custody(&control.env);
            pending |= control.has_pending_custody();
        }
    }
    pending
}

fn seal_controls(lifetime: &account_work::Lifetime) -> bool {
    let mut sealed = true;
    for (key, control) in owned_controls(lifetime) {
        if !control.work_retired() {
            sealed = false;
            continue;
        }
        if let Some(proof) = control.retired()
            && proof.finish(&control.env).is_err()
        {
            sealed = false;
            continue;
        }
        // A replay that never acquired resource custody cannot mint a proof.
        // Removing its empty runtime handle leaves durable uncertainty intact.
        remove_control(&key, &control);
    }
    sealed
}

impl crate::App {
    pub(crate) fn settle_enrollment_custody(lifetime: &account_work::Lifetime) -> bool {
        settle_control_custody(lifetime)
    }

    pub(crate) fn enrollment_retirement_pending(lifetime: &account_work::Lifetime) -> bool {
        controls()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .any(|control| control.owned_by(lifetime))
            || cleanup::pending(lifetime)
    }

    pub(crate) fn seal_enrollment_retirement(lifetime: &account_work::Lifetime) -> bool {
        if !Self::enrollment_retirement_pending(lifetime) {
            return true;
        }
        // Both sets may hold exact historical receipts for one operation. A
        // successful current receipt makes older terminal retirement idempotent.
        let normal = seal_controls(lifetime);
        let cleanup = cleanup::seal_controls(lifetime);
        let normal = normal || seal_controls(lifetime);
        let cleanup = cleanup || cleanup::seal_controls(lifetime);
        normal && cleanup
    }
}

fn discovery(env: &Env, sid: &str, proof: &crate::self_claim::Proven) -> Result<Option<Value>, ()> {
    discovery_with(env, sid, proof, || {})
}

fn discovery_with(
    env: &Env,
    sid: &str,
    proof: &crate::self_claim::Proven,
    after_read: impl FnOnce(),
) -> Result<Option<Value>, ()> {
    let (sequence, pending) = {
        let table = jobs().lock().unwrap_or_else(|p| p.into_inner());
        if table.exhausted {
            return Err(());
        }
        let pending: Vec<_> = table
            .entries
            .iter()
            .filter(|((job_sid, _, _), job)| {
                job_sid == sid && job.command.may_publish() && job.proof == *proof
            })
            .map(|(_, job)| Arc::clone(job))
            .collect();
        (table.sequence, pending)
    };
    let config = env
        .config
        .as_deref()
        .and_then(std::path::Path::parent)
        .ok_or(())?;
    let store = env.accounts_store_for(accounts::Cli::Codex).ok_or(())?;
    let path = config.join(accounts::Cli::Codex.accounts().roster_file);
    let records = store.peek_account_records(&path, env.now).map_err(|_| ())?;
    after_read();
    // Insertion, completion and removal all advance under the same registry
    // lock. A completed and removed intent cannot disappear behind an old read.
    let table = jobs().lock().unwrap_or_else(|p| p.into_inner());
    if table.exhausted {
        return Err(());
    }
    if table.sequence != sequence {
        return Ok(None);
    }
    let owner = caller(sid, proof);
    for job in pending {
        if records
            .logins
            .enrollment
            .retained(job.command.id(), &owner)
            .is_err()
            && job
                .result
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none()
        {
            return Ok(None);
        }
    }
    drop(table);
    if records.roster.rows.len() > 128 {
        return Err(());
    }
    let targets: Result<Vec<_>, ()> = records
        .roster
        .rows
        .iter()
        .map(|row| {
            if row.label.chars().count() > 128 || row.label.chars().any(char::is_control) {
                return Err(());
            }
            Ok(json!({"account_id":accounts_control::account_id(&row.label),"label":row.label}))
        })
        .collect();
    let operations: Vec<_> = records
        .logins
        .enrollment
        .retained_for(&caller(sid, proof))
        .map_err(|_| ())?
        .iter()
        .map(|(request, status)| {
            let mut value = progress(request, status);
            value
                .as_object_mut()
                .expect("progress object")
                .insert("revision".into(), json!(request.revision));
            value
        })
        .collect();
    // Source-aware creation has a separate admission contract. Enrollment here
    // resolves only existing source rows, and never changes session assignment.
    Ok(Some(
        json!({"schema":1,"revision":accounts::pool_authority_revision(&records.raw_roster,&records.logins).map_err(|_| ())?,
        "capabilities":{"enroll":true,"create":false},"targets":targets?,"operations":operations}),
    ))
}

fn diagnostics(
    env: &Env,
    sid: &str,
    proof: &crate::self_claim::Proven,
    operation: &str,
) -> Result<Option<Value>, ()> {
    let store = env.accounts_store_for(accounts::Cli::Codex).ok_or(())?;
    // Canonical witnessed metadata only: a read never repairs pending publication.
    let (logins, warning) = store.load_logins(env.now).map_err(|_| ())?;
    if warning.is_some() {
        return Err(());
    }
    let (intent, status, stage) = logins
        .enrollment
        .diagnostics(operation, &caller(sid, proof))
        .map_err(|_| ())?;
    let mut value = progress(&intent, &status);
    value.as_object_mut().ok_or(())?.insert(
        "preparation_stage".into(),
        json!(stage.map(|stage| stage.as_str())),
    );
    Ok(Some(value))
}

fn run(
    command: &Command,
    sid: &str,
    proof: &crate::self_claim::Proven,
    lifetime: &account_work::Lifetime,
) -> Result<Option<Value>, ()> {
    let env = crate::native_act::codex_account_env().map_err(|_| ())?;
    run_in(command, sid, proof, lifetime, &env, enrollment::Work::run)
}

fn run_enrollment(
    command: &Command,
    sid: &str,
    proof: &crate::self_claim::Proven,
    lifetime: &account_work::Lifetime,
    env: &Env,
    drive: impl FnOnce(enrollment::Work, &Env) -> Result<enrollment::Status, String>,
) -> Result<Option<Value>, ()> {
    let Command::Enroll {
        operation,
        revision,
        account,
    } = command
    else {
        return Err(());
    };
    let store = env.accounts_store_for(accounts::Cli::Codex).ok_or(())?;
    let owner = caller(sid, proof);
    let key = (sid.to_owned(), command.id().to_owned());
    let intent = enrollment::RejectedIntent {
        id: operation.clone(),
        caller: owner.clone(),
        account_id: account.clone(),
        revision: revision.clone(),
    };
    let refuse = || {
        enrollment::reject(env, &intent)
            .map(|status| Some(progress(&intent, &status)))
            .map_err(|_| ())
    };
    let (logins, warning) = store.load_logins(env.now).map_err(|_| ())?;
    if warning.is_some() {
        return Err(());
    }
    let mut native_recovery = false;
    let mut native_retirement = false;
    let recovery = if let Ok((retained, status)) = logins.enrollment.retained(operation, &owner) {
        if retained.revision != *revision || retained.account_id != *account {
            return Err(());
        }
        let request = logins.enrollment.admitted(operation, &owner).ok();
        match request {
            Some(request) => {
                native_recovery = logins
                    .enrollment
                    .native_recovery_pending(&request)
                    .map_err(|_| ())?;
                native_retirement =
                    native_recovery && status.phase == enrollment::Phase::CancelRequested;
                if native_recovery || logins.enrollment.recoverable(&request).map_err(|_| ())? {
                    Some(request)
                } else {
                    return Ok(Some(progress(&retained, &status)));
                }
            }
            None => return Ok(Some(progress(&retained, &status))),
        }
    } else {
        None
    };
    let request = if native_retirement {
        // Cancellation owns only original custody. Current roster policy and
        // staging-file availability cannot authorize or prevent its retirement.
        recovery.clone().ok_or(())?
    } else {
        let config = env
            .config
            .as_deref()
            .and_then(std::path::Path::parent)
            .ok_or(())?;
        let path = config.join(accounts::Cli::Codex.accounts().roster_file);
        let records = store.peek_account_records(&path, env.now).map_err(|_| ())?;
        let Some(row) = records
            .roster
            .rows
            .iter()
            .find(|row| accounts_control::account_id(&row.label) == *account)
        else {
            return refuse();
        };
        if row.held_by.is_some()
            || row.expected_identity_email().is_none()
            // Exact retained native work authenticates its original row again under
            // native resources. Another row's publication may advance this revision.
            || (!native_recovery
                && accounts::pool_authority_revision(&records.raw_roster, &records.logins)
                    .map_err(|_| ())?
                    != *revision)
        {
            return refuse();
        }
        let request = recovery.clone().unwrap_or_else(|| enrollment::Request {
            id: operation.clone(),
            caller: owner,
            label: row.label.clone(),
            revision: revision.clone(),
        });
        if request.label != row.label {
            return Err(());
        }
        request
    };
    let Some(scope) = lifetime.enter() else {
        return refuse();
    };
    let (work, control) = enrollment::owned_work(request.clone(), scope);
    let control = Arc::new(OwnedControl {
        control,
        env: env.clone(),
    });
    {
        let mut table = controls().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(previous) = table.get(&key) {
            if recovery.is_some() && previous.work_retired() {
                table.remove(&key);
            } else {
                drop(table);
                return refuse();
            }
        }
        if table.len() >= MAX_JOBS {
            drop(table);
            return refuse();
        }
        table.insert(key.clone(), Arc::clone(&control));
    }
    let result = drive(work, env);
    control.settle_custody(env).map_err(|_| ())?;
    let result = match result {
        Ok(_) => {
            let (current, warning) = store.load_logins(env.now).map_err(|_| ())?;
            if warning.is_some() {
                return Err(());
            }
            let (intent, status) = current
                .enrollment
                .retained(&request.id, &request.caller)
                .map_err(|_| ())?;
            if intent != request.intent() {
                return Err(());
            }
            Ok(status)
        }
        other => other,
    };
    let result = match result {
        Ok(status)
            if matches!(
                status.phase,
                enrollment::Phase::CancelRequested | enrollment::Phase::Running
            ) =>
        {
            control
                .retired()
                .map(|proof| proof.finish(env))
                .unwrap_or(Ok(status))
        }
        other => other,
    };
    if control.work_retired() && result.is_ok() {
        remove_control(&key, &control);
    }
    Ok(Some(progress(&request.intent(), &result.map_err(|_| ())?)))
}

fn run_in(
    command: &Command,
    sid: &str,
    proof: &crate::self_claim::Proven,
    lifetime: &account_work::Lifetime,
    env: &Env,
    drive: impl FnOnce(enrollment::Work, &Env) -> Result<enrollment::Status, String>,
) -> Result<Option<Value>, ()> {
    if let Command::Discover(_) = command {
        return discovery(env, sid, proof);
    }
    if let Command::Diagnostics { operation, .. } = command {
        return diagnostics(env, sid, proof, operation);
    }
    // The authenticated worker owns recovery of an already admitted record
    // publication too. Keep it counted before reading metadata that a pending
    // publication deliberately fences; discovery never repairs these records.
    let _work = lifetime.enter().ok_or(())?;
    let store = env.accounts_store_for(accounts::Cli::Codex).ok_or(())?;
    let owner = caller(sid, proof);
    // A pending account-record transaction can fence load_logins itself.
    // Failure to observe an exact native cancellation selects the ordinary
    // counted record-repair path; it supplies no native cleanup authority.
    let native_metadata_dispatch = if let Ok((before, None)) = store.load_logins(env.now) {
        if let Ok((retained, status)) = before.enrollment.retained(command.id(), &owner) {
            let body_matches = match command {
                Command::Enroll {
                    revision, account, ..
                } => retained.revision == *revision && retained.account_id == *account,
                _ => true,
            };
            let cleanup_phase = status.phase == enrollment::Phase::CancelRequested
                || (matches!(command, Command::Cancel(_))
                    && status.phase == enrollment::Phase::Running);
            let terminal = matches!(
                status.phase,
                enrollment::Phase::Cancelled | enrollment::Phase::Applied
            );
            if body_matches && (cleanup_phase || terminal) {
                let request = before
                    .enrollment
                    .admitted(command.id(), &owner)
                    .map_err(|_| ())?;
                if terminal {
                    before
                        .enrollment
                        .native_admitted(&request)
                        .map_err(|_| ())?
                } else if matches!(command, Command::Cancel(_)) {
                    before
                        .enrollment
                        .native_cancellation(&request)
                        .map_err(|_| ())?
                } else {
                    before
                        .enrollment
                        .native_recovery_pending(&request)
                        .map_err(|_| ())?
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };
    if !native_metadata_dispatch {
        let config = env
            .config
            .as_deref()
            .and_then(std::path::Path::parent)
            .ok_or(())?;
        let roster = config.join(accounts::Cli::Codex.accounts().roster_file);
        store.recover_records(&roster).map_err(|_| ())?;
    }
    let key = (sid.to_owned(), command.id().to_owned());
    match command {
        Command::Enroll { .. } => run_enrollment(command, sid, proof, lifetime, env, drive),
        Command::Poll(operation) | Command::Cancel(operation) => {
            let (logins, warning) = store.load_logins(env.now).map_err(|_| ())?;
            if warning.is_some() {
                return Err(());
            }
            let (request, mut status) = logins
                .enrollment
                .retained(operation, &owner)
                .map_err(|_| ())?;
            if status.phase == enrollment::Phase::Refused {
                let mut table = controls().lock().unwrap_or_else(|p| p.into_inner());
                if table
                    .get(&key)
                    .is_some_and(|control| control.work_retired())
                {
                    table.remove(&key);
                }
                return Ok(Some(progress(&request, &status)));
            }
            let admitted = logins
                .enrollment
                .admitted(operation, &owner)
                .map_err(|_| ())?;
            if matches!(command, Command::Cancel(_))
                && status.phase == enrollment::Phase::Running
                && !controls()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains_key(&key)
            {
                enrollment::settle_not_dispatched(env, &admitted).map_err(|_| ())?;
                enrollment::settle_retired_rejection(env, &admitted).map_err(|_| ())?;
                let (current, warning) = store.load_logins(env.now).map_err(|_| ())?;
                if warning.is_some() {
                    return Err(());
                }
                status = current
                    .enrollment
                    .retained(operation, &owner)
                    .map_err(|_| ())?
                    .1;
                if status.phase == enrollment::Phase::Refused {
                    return Ok(Some(progress(&request, &status)));
                }
            }
            if matches!(command, Command::Poll(_)) {
                let control = controls()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&key)
                    .cloned();
                if let Some(control) = control {
                    control.settle_custody(env).map_err(|_| ())?;
                } else if status.phase == enrollment::Phase::Running {
                    enrollment::settle_not_dispatched(env, &admitted).map_err(|_| ())?;
                    enrollment::settle_retired_rejection(env, &admitted).map_err(|_| ())?;
                }
                let (current, warning) = store.load_logins(env.now).map_err(|_| ())?;
                if warning.is_some() {
                    return Err(());
                }
                status = current
                    .enrollment
                    .retained(operation, &owner)
                    .map_err(|_| ())?
                    .1;
            }
            if matches!(
                status.phase,
                enrollment::Phase::Running | enrollment::Phase::CancelRequested
            ) {
                let control = controls()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&key)
                    .cloned();
                if let Some(control) = control
                    && let Some(proof) = control.retired()
                {
                    status = proof.finish(env).map_err(|_| ())?;
                    remove_control(&key, &control);
                }
            }
            if matches!(command, Command::Poll(_))
                && matches!(
                    status.phase,
                    enrollment::Phase::Running | enrollment::Phase::CancelRequested
                )
            {
                let (current, warning) = store.load_logins(env.now).map_err(|_| ())?;
                if warning.is_some() {
                    return Err(());
                }
                if current
                    .enrollment
                    .native_recovery_pending(&admitted)
                    .map_err(|_| ())?
                    || current.enrollment.recoverable(&admitted).map_err(|_| ())?
                {
                    let enrollment = Command::Enroll {
                        operation: admitted.id.clone(),
                        revision: admitted.revision.clone(),
                        account: accounts_control::account_id(&admitted.label),
                    };
                    return run_enrollment(&enrollment, sid, proof, lifetime, env, drive);
                }
            }
            if matches!(command, Command::Cancel(_)) {
                let control = controls()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&key)
                    .cloned();
                status = match control.as_ref() {
                    Some(control) => {
                        let status = control.cancel(env).map_err(|_| ())?;
                        if status.phase == enrollment::Phase::CancelRequested {
                            control
                                .retired()
                                .map(|proof| proof.finish(env))
                                .unwrap_or(Ok(status))
                                .map_err(|_| ())?
                        } else {
                            status
                        }
                    }
                    None => enrollment::cancel(env, &admitted).map_err(|_| ())?,
                };
                if status.phase == enrollment::Phase::Cancelled
                    && let Some(control) = control
                {
                    remove_control(&key, &control);
                }
            }
            Ok(Some(progress(&request, &status)))
        }
        Command::Discover(_) | Command::Diagnostics { .. } => unreachable!(),
    }
}

pub(crate) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    lifetime: &account_work::Lifetime,
    read: bool,
) -> String {
    let namespace = if read {
        "codex-accounts"
    } else {
        "codex-accounts-change"
    };
    let Some(command) = parse(line, read) else {
        return format!("ERR {namespace} refused\n");
    };
    let Some(before) = accounts_control::claim(ctx, master) else {
        return format!("ERR {namespace} denied\n");
    };
    if !read && ctx.fabric.hold().is_some() {
        return format!("ERR {namespace} denied\n");
    }
    if !read
        && let Some(reply) = cleanup::serve_if_eligible(ctx, master, &command, &before, lifetime)
    {
        return reply;
    }
    let key = (
        ctx.self_id.as_str().to_owned(),
        command.kind(),
        command.id().to_owned(),
    );
    // The rollback owner outlives the inner registry guard, including unwind.
    let mut reservation = Reservation::new(key.clone());
    let (job, start) = {
        let mut table = jobs().lock().unwrap_or_else(|p| p.into_inner());
        table.prune();
        if table.exhausted {
            return format!("ERR {namespace} refused\n");
        }
        if let Some(job) = table.entries.get(&key) {
            if job.proof != before || job.command != command {
                return format!("ERR {namespace} refused\n");
            }
            (Arc::clone(job), None)
        } else {
            if table.entries.len() >= MAX_JOBS {
                return format!("ERR {namespace} refused\n");
            }
            let Some(scope) = lifetime.enter() else {
                return format!("ERR {namespace} refused\n");
            };
            let job = Arc::new(Job {
                cleanup_owner: None,
                command: command.clone(),
                proof: before.clone(),
                started: std::time::Instant::now(),
                intent_sequence: table.sequence,
                result: Mutex::new(None),
            });
            // SAFETY: the dispatch target owns this live PTY master while this
            // request is served. The duplicate owns its identity through the
            // worker's admission recheck and cannot name a reused descriptor.
            let Ok(owned_master) = (unsafe { BorrowedFd::borrow_raw(master) }).try_clone_to_owned()
            else {
                return format!("ERR {namespace} refused\n");
            };
            reservation.arm(&job, &scope);
            table.insert(key.clone(), Arc::clone(&job));
            (job, Some((scope, owned_master)))
        }
    };
    if let Some((scope, owned_master)) = start {
        let worker = Arc::clone(&job);
        let sid = key.0.clone();
        let weak = Arc::downgrade(ctx);
        let lifetime = lifetime.clone();
        // Release the registry before starting work. Even an immediate
        // completion observes its original reservation.
        if !launch(&key, &job, || {
            std::thread::Builder::new()
                .name("codex-enrollment".into())
                .spawn(owned_completion(&job, scope, move || {
                    let admitted = weak.upgrade().is_some_and(|ctx| {
                        still_native(&ctx, &worker.proof, owned_master.as_raw_fd())
                            && (read || ctx.fabric.hold().is_none())
                    });
                    drop(owned_master);
                    if admitted {
                        run(&worker.command, &sid, &worker.proof, &lifetime)
                    } else {
                        Err(())
                    }
                }))
                .map(|_| ())
        }) {
            return format!("ERR {namespace} refused\n");
        }
    }
    reservation.dispatched();

    reply(
        ctx,
        master,
        (namespace, read),
        &command,
        &before,
        &job,
        jobs(),
    )
}

fn reply(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    (namespace, read): (&str, bool),
    command: &Command,
    before: &crate::self_claim::Proven,
    job: &Arc<Job>,
    registry: &Mutex<Registry>,
) -> String {
    let key = (
        ctx.self_id.as_str().to_owned(),
        command.kind(),
        command.id().to_owned(),
    );
    if !before.alive() || accounts_control::claim(ctx, master).as_ref() != Some(before) {
        return format!("ERR {namespace} denied\n");
    }
    let result = job.result.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let Some(result) = result else {
        if read {
            return format!("OK codex-accounts state=waiting request={}\n", command.id());
        }
        let Some(binding) = session(ctx.self_id.as_str(), before) else {
            return format!("ERR {namespace} refused\n");
        };
        return format!(
            "OK codex-accounts-change waiting {}\n",
            json!({"schema":1,"operation":command.id(),"session":binding})
        );
    };
    {
        let mut table = registry.lock().unwrap_or_else(|p| p.into_inner());
        let stale = read && (table.exhausted || table.sequence != job.intent_sequence);
        table.remove(&key, job);
        if stale {
            return format!("OK codex-accounts state=waiting request={}\n", command.id());
        }
    }
    let Ok(value) = result else {
        return format!("ERR {namespace} refused\n");
    };
    let Some(mut value) = value else {
        return format!("OK codex-accounts state=waiting request={}\n", command.id());
    };
    let Some(binding) = session(ctx.self_id.as_str(), before) else {
        return format!("ERR {namespace} refused\n");
    };
    let object = value.as_object_mut().expect("worker result object");
    object.insert("schema".into(), json!(1));
    object.insert("session".into(), binding);
    if read {
        object.insert("request".into(), json!(command.id()));
    }
    let prefix = if matches!(command, Command::Diagnostics { .. }) {
        "OK codex-accounts enrollment-diagnostics "
    } else if read {
        "OK codex-accounts enrollment-status "
    } else {
        "OK codex-accounts-change enrollment "
    };
    match aterm_json::to_string(&value) {
        Ok(body) if prefix.len() + body.len() < MAX_FRAME => format!("{prefix}{body}\n"),
        _ => format!("ERR {namespace} refused\n"),
    }
}

#[cfg(test)]
#[path = "codex_enrollment_control_tests.rs"]
mod tests;
