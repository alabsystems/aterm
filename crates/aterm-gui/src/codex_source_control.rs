// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Persistent, private source membership. A loaded member is not a process
//! freeze, assignment permit, or permission to publish an OAuth grant.

use super::*;
use aterm_agent::harness::{
    account_work, codex_session_slot as slot, codex_source_registry as registry, restart_stamp,
};
use std::collections::{BTreeMap, VecDeque};
use std::os::fd::BorrowedFd;
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

const MAX_FRAME: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 256;
const MAX_PENDING: usize = 256;
const MAX_RUNNING: usize = 64;
const RECENT_PER_CONNECTION: usize = 8;
const UNAVAILABLE: &str = "ERR codex-auth unavailable\n";

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn number(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Sequence {
    prefix: u64,
    counter: u64,
}
impl Sequence {
    fn parse(value: &str) -> Option<Self> {
        if !hex(value, 32) {
            return None;
        }
        let prefix = u64::from_str_radix(&value[..16], 16).ok()?;
        let counter = u64::from_str_radix(&value[16..], 16).ok()?;
        (counter > 0).then_some(Self { prefix, counter })
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Action {
    Open,
    Preload,
    Ready {
        registration: String,
        generation: u64,
        digest: String,
        account: String,
        user: String,
        auth: registry::AuthState,
    },
    ReaderReady {
        registration: String,
        generation: u64,
        digest: String,
        account: String,
        user: String,
    },
    Unavailable {
        registration: String,
    },
    Unrelated {
        kind: registry::NoNativeReaderKind,
    },
    Retire {
        registration: Option<String>,
    },
}
#[derive(Clone, PartialEq, Eq)]
struct Request {
    request: String,
    sequence: Sequence,
    operation: String,
    member: Option<String>,
    action: Action,
}
impl Request {
    fn parse(line: &str) -> Option<Self> {
        if line.len() > MAX_FRAME {
            return None;
        }
        let mut words = line.split_ascii_whitespace();
        let action = words.next()?;
        let mut fields = BTreeMap::new();
        for word in words {
            let (key, value) = word.split_once('=')?;
            if value.is_empty() || fields.insert(key, value).is_some() {
                return None;
            }
        }
        if fields.remove("schema")? != "1" {
            return None;
        }
        let request = fields.remove("request")?.to_owned();
        let operation = fields.remove("operation")?.to_owned();
        let sequence = Sequence::parse(&request)?;
        if !hex(&operation, 32) {
            return None;
        }
        let member = if action == "member-open" {
            None
        } else {
            let member = fields.remove("member")?;
            if !hex(member, 32) {
                return None;
            }
            Some(member.to_owned())
        };
        let registration = |fields: &mut BTreeMap<&str, &str>| -> Option<String> {
            let value = fields.remove("registration")?;
            hex(value, 32).then(|| value.to_owned())
        };
        let action = match action {
            "member-open" => Action::Open,
            "member-preload" => Action::Preload,
            "member-ready" | "member-reader-ready" => {
                let registration = registration(&mut fields)?;
                let generation = number(fields.remove("credential_generation")?)?;
                if generation == 0 {
                    return None;
                }
                let digest = fields.remove("digest")?.to_owned();
                let account = fields.remove("account")?.to_owned();
                let user = fields.remove("user")?.to_owned();
                if ![&digest, &account, &user]
                    .into_iter()
                    .all(|value| hex(value, 64))
                {
                    return None;
                }
                if action == "member-reader-ready" {
                    Action::ReaderReady {
                        registration,
                        generation,
                        digest,
                        account,
                        user,
                    }
                } else {
                    Action::Ready {
                        registration,
                        generation,
                        digest,
                        account,
                        user,
                        auth: registry::AuthState {
                            generation: number(fields.remove("auth_generation")?)?,
                            owner: number(fields.remove("owner_generation")?)?,
                            source: number(fields.remove("source_generation")?)?,
                            login: number(fields.remove("login_generation")?)?,
                        },
                    }
                }
            }
            "member-unavailable" => Action::Unavailable {
                registration: registration(&mut fields)?,
            },
            "member-unrelated" => Action::Unrelated {
                kind: match fields.remove("kind")? {
                    "api_key" => registry::NoNativeReaderKind::ApiKey,
                    "external" => registry::NoNativeReaderKind::External,
                    "workload" => registry::NoNativeReaderKind::Workload,
                    "bedrock_api_key" => registry::NoNativeReaderKind::BedrockApiKey,
                    "bedrock_access_keys" => registry::NoNativeReaderKind::BedrockAccessKeys,
                    "headers" => registry::NoNativeReaderKind::Headers,
                    "agent_identity" => registry::NoNativeReaderKind::AgentIdentity,
                    "personal_access_token" => registry::NoNativeReaderKind::PersonalAccessToken,
                    "no_credentials" => registry::NoNativeReaderKind::NoCredentials,
                    _ => return None,
                },
            },
            "member-retire" => {
                let value = fields.remove("registration")?;
                Action::Retire {
                    registration: if value == "none" {
                        None
                    } else if hex(value, 32) {
                        Some(value.into())
                    } else {
                        return None;
                    },
                }
            }
            _ => return None,
        };
        fields.is_empty().then_some(Self {
            request,
            sequence,
            operation,
            member,
            action,
        })
    }
    fn name(&self) -> &'static str {
        match self.action {
            Action::Open => "member-open",
            Action::Preload => "member-preload",
            Action::Ready { .. } => "member-ready",
            Action::ReaderReady { .. } => "member-reader-ready",
            Action::Unavailable { .. } => "member-unavailable",
            Action::Unrelated { .. } => "member-unrelated",
            Action::Retire { .. } => "member-retire",
        }
    }
}

pub(super) fn handles(line: &str) -> bool {
    line.split_ascii_whitespace()
        .next()
        .is_some_and(|action| action.starts_with("member-"))
}

#[derive(Clone, PartialEq, Eq)]
struct Caller {
    caller: slot::Caller,
    process: Process,
    start: String,
}
fn claim(
    ctx: &crate::SessionCtx,
    master: i32,
    registry: &registry::Registry,
) -> Result<(Caller, registry::Peer), registry::Error> {
    let pid = crate::control::serving_peer_pid().ok_or(registry::Error::Denied)?;
    let process = Process::observe(pid).ok_or(registry::Error::Unavailable)?;
    let caller = slot::Caller {
        sid: ctx.self_id.as_str().into(),
        pid,
        birth: process.birth,
    };
    let args = atpkg::caller_shell::process_args(pid).ok_or(registry::Error::Unavailable)?;
    let peer = if let Some(token) = args.env_var(registry::LAUNCH_ENV) {
        registry.reserved_peer(&caller, token)?
    } else {
        if master < 0 {
            return Err(registry::Error::Denied);
        }
        // SAFETY: borrow the actual session PTY; no descriptor is transferred.
        registry::Peer::foreground(&caller, unsafe { BorrowedFd::borrow_raw(master) })?
    };
    let start = restart_stamp::kernel_start(pid).ok_or(registry::Error::Unavailable)?;
    if Process::observe(pid).as_ref() != Some(&process) {
        return Err(registry::Error::Stale);
    }
    Ok((
        Caller {
            caller,
            process,
            start,
        },
        peer,
    ))
}

fn session(caller: &Caller, epoch: &str) -> aterm_json::Value {
    aterm_json::json!({"sid":caller.caller.sid,"process_pid":caller.caller.pid,"process_birth":caller.start,"epoch":epoch})
}
fn envelope(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    member: Option<&str>,
) -> aterm_json::Value {
    aterm_json::json!({"schema":1,"request":request.request,"operation":request.operation,"session":session(caller,epoch),"member":member})
}
fn frame(name: &str, value: &aterm_json::Value) -> Option<String> {
    let body = aterm_json::to_string(value).ok()?;
    let line = format!("OK codex-auth {name} {body}\n");
    (line.len() <= MAX_FRAME).then_some(line)
}
fn execute(
    registry: &registry::Registry,
    connection: &registry::Connection,
    peer: registry::Peer,
    request: &Request,
    caller: &Caller,
    epoch: &str,
) -> Result<String, registry::Error> {
    let mut value = envelope(request, caller, epoch, request.member.as_deref());
    let member = request.member.as_deref().unwrap_or("");
    match &request.action {
        Action::Open => {
            let allocation = registry.open_member(&peer, connection, &request.operation)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("member".into(), aterm_json::json!(allocation.member));
        }
        Action::Preload => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            let preload =
                registry.preload_managed(&env, peer, member, &request.operation, connection)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert(
                    "registration".into(),
                    aterm_json::json!(preload.registration),
                );
            let source = super::auth_control::source_value(
                &request.operation,
                session(caller, epoch),
                preload.source(),
            )
            .ok_or(registry::Error::Unavailable)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("source".into(), source);
        }
        Action::Ready {
            registration,
            generation,
            digest,
            account,
            user,
            auth,
        } => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            registry.ready(
                &env,
                peer,
                member,
                &request.operation,
                registration,
                connection,
                registry::Ready::Loaded {
                    credential_generation: *generation,
                    digest: digest.clone(),
                    account: account.clone(),
                    user: user.clone(),
                    auth: *auth,
                },
            )?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("registration".into(), aterm_json::json!(registration));
        }
        Action::ReaderReady {
            registration,
            generation,
            digest,
            account,
            user,
        } => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            registry.ready(
                &env,
                peer,
                member,
                &request.operation,
                registration,
                connection,
                registry::Ready::ReaderLoaded {
                    credential_generation: *generation,
                    digest: digest.clone(),
                    account: account.clone(),
                    user: user.clone(),
                },
            )?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("registration".into(), aterm_json::json!(registration));
        }
        Action::Unavailable { registration } => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            registry.ready(
                &env,
                peer,
                member,
                &request.operation,
                registration,
                connection,
                registry::Ready::PolicyUnavailable,
            )?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("registration".into(), aterm_json::json!(registration));
        }
        Action::Unrelated { kind } => {
            let registration =
                registry.complete_unrelated(peer, connection, member, &request.operation, *kind)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("registration".into(), aterm_json::json!(registration));
        }
        Action::Retire { registration } => {
            registry.retire_member(peer, connection, member, registration.as_deref())?
        }
    }
    frame(request.name(), &value).ok_or(registry::Error::Unavailable)
}

struct RunningPermit(Arc<AtomicUsize>);
impl Drop for RunningPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

enum Progress {
    Waiting,
    Running { _permit: RunningPermit },
    Complete(Result<String, registry::Error>),
}
struct Job {
    request: Request,
    caller: Caller,
    progress: Mutex<Progress>,
}
impl Job {
    fn finish(&self, result: Result<String, registry::Error>) {
        *self.progress.lock().unwrap_or_else(|p| p.into_inner()) = match result {
            // A retry can only re-enter this same immutable request. No later
            // body may borrow its nonce after the backend reports contention.
            Err(registry::Error::Busy) => Progress::Waiting,
            result => Progress::Complete(result),
        };
    }
}
struct ConnectionJobs {
    prefix: u64,
    highwater: u64,
    caller: Caller,
    active: BTreeMap<u64, Arc<Job>>,
    recent: VecDeque<(u64, Arc<Job>)>,
}
#[derive(Default)]
struct Jobs {
    connections: BTreeMap<u64, ConnectionJobs>,
    running: Arc<AtomicUsize>,
}
#[derive(Debug, PartialEq, Eq)]
enum AdmissionError {
    Changed,
    Stale,
    Capacity,
}
impl Jobs {
    fn reap(&mut self) {
        for connection in self.connections.values_mut() {
            let completed: Vec<_> = connection
                .active
                .iter()
                .filter_map(|(&counter, job)| {
                    matches!(
                        *job.progress.lock().unwrap_or_else(|p| p.into_inner()),
                        Progress::Complete(_)
                    )
                    .then_some(counter)
                })
                .collect();
            for counter in completed {
                if let Some(job) = connection.active.remove(&counter) {
                    connection.recent.push_back((counter, job));
                }
            }
            while connection.recent.len() > RECENT_PER_CONNECTION {
                connection.recent.pop_front();
            }
        }
    }
    fn reserve(&self, job: Arc<Job>) -> (Arc<Job>, bool) {
        let room = self.running.load(Ordering::Acquire) < MAX_RUNNING;
        let start = {
            let mut state = job.progress.lock().unwrap_or_else(|p| p.into_inner());
            if room && matches!(*state, Progress::Waiting) {
                self.running.fetch_add(1, Ordering::AcqRel);
                *state = Progress::Running {
                    _permit: RunningPermit(Arc::clone(&self.running)),
                };
                true
            } else {
                false
            }
        };
        (job, start)
    }
    fn admit(
        &mut self,
        id: u64,
        caller: &Caller,
        request: &Request,
    ) -> Result<(Arc<Job>, bool), AdmissionError> {
        self.reap();
        let sequence = request.sequence;
        if let Some(connection) = self.connections.get(&id) {
            if connection.prefix != sequence.prefix || connection.caller != *caller {
                return Err(AdmissionError::Stale);
            }
            // Active work wins even when newer counters have completed. An
            // older waiting request must remain retryable without retargeting.
            if let Some(job) = connection.active.get(&sequence.counter).or_else(|| {
                connection
                    .recent
                    .iter()
                    .find(|(counter, _)| *counter == sequence.counter)
                    .map(|(_, job)| job)
            }) {
                if job.request != *request || job.caller != *caller {
                    return Err(AdmissionError::Changed);
                }
                return Ok(self.reserve(Arc::clone(job)));
            }
            if sequence.counter <= connection.highwater {
                return Err(AdmissionError::Stale);
            }
        } else if self.connections.len() >= MAX_CONNECTIONS {
            return Err(AdmissionError::Capacity);
        }
        if self
            .connections
            .values()
            .map(|c| c.active.len())
            .sum::<usize>()
            >= MAX_PENDING
        {
            // No counter/prefix/body has been accepted and no worker started.
            return Err(AdmissionError::Capacity);
        }
        let connection = self
            .connections
            .entry(id)
            .or_insert_with(|| ConnectionJobs {
                prefix: sequence.prefix,
                highwater: 0,
                caller: caller.clone(),
                active: BTreeMap::new(),
                recent: VecDeque::new(),
            });
        connection.highwater = sequence.counter;
        let job = Arc::new(Job {
            request: request.clone(),
            caller: caller.clone(),
            progress: Mutex::new(Progress::Waiting),
        });
        connection.active.insert(sequence.counter, Arc::clone(&job));
        Ok(self.reserve(job))
    }
    fn closed(&mut self, id: u64) {
        self.connections.remove(&id);
    }
}
fn jobs() -> &'static Mutex<Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}
/// Called only after the real connection token has synchronously become false.
/// Workers retain that original token and cannot reinsert transport state.
pub(super) fn connection_closed(id: u64) {
    jobs().lock().unwrap_or_else(|p| p.into_inner()).closed(id);
}

pub(super) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    lifetime: &account_work::Lifetime,
    registry: &Arc<registry::Registry>,
    connection_id: Option<u64>,
) -> String {
    let Some(request) = Request::parse(line) else {
        return "ERR codex-auth invalid request\n".into();
    };
    let Some(id) = connection_id.filter(|id| *id != 0) else {
        return "ERR codex-auth denied\n".into();
    };
    let (caller, peer) = match claim(ctx, master, registry) {
        Ok(claimed) => claimed,
        Err(registry::Error::Busy) => return "ERR codex-auth busy\n".into(),
        Err(registry::Error::Unavailable) => return UNAVAILABLE.into(),
        Err(_) => return "ERR codex-auth denied\n".into(),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    // This token must exist before asynchronous I/O so EOF cannot race an
    // unregistered worker and accidentally create a live source afterward.
    let Ok(connection) = registry.connection(id) else {
        return UNAVAILABLE.into();
    };
    let (job, start) = match jobs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .admit(id, &caller, &request)
    {
        Ok(admitted) => admitted,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return "ERR codex-auth busy\n".into(),
    };
    if start {
        if let Some(scope) = lifetime.enter() {
            let worker = Arc::clone(&job);
            let registry = Arc::clone(registry);
            if std::thread::Builder::new()
                .name("codex-source-member".into())
                .spawn(scope.wrap(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        execute(
                            &registry,
                            &connection,
                            peer,
                            &worker.request,
                            &worker.caller,
                            epoch,
                        )
                    }))
                    .unwrap_or(Err(registry::Error::Unavailable));
                    worker.finish(result);
                }))
                .is_err()
            {
                job.finish(Err(registry::Error::Unavailable));
            }
        } else {
            job.finish(Err(registry::Error::Unavailable));
        }
    }
    // Match the entire current kernel/foreground-or-constructor admission, not
    // just a PID/image snapshot, before returning a private source description.
    match claim(ctx, master, registry) {
        Ok((fresh, _)) if fresh == caller => {}
        Err(registry::Error::Busy) => {
            return frame(
                "member-waiting",
                &envelope(&request, &caller, epoch, request.member.as_deref()),
            )
            .unwrap_or_else(|| UNAVAILABLE.into());
        }
        _ => return "ERR codex-auth denied\n".into(),
    }
    let result = {
        let progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
        match &*progress {
            Progress::Complete(result) => Some(result.clone()),
            _ => None,
        }
    };
    match result {
        Some(Ok(line)) => line,
        Some(Err(registry::Error::Denied)) => "ERR codex-auth denied\n".into(),
        Some(Err(registry::Error::Stale)) => "ERR codex-auth stale\n".into(),
        Some(Err(registry::Error::Unsupported)) => "ERR codex-auth unsupported\n".into(),
        Some(Err(registry::Error::Busy | registry::Error::Unavailable)) => UNAVAILABLE.into(),
        None => frame(
            "member-waiting",
            &envelope(&request, &caller, epoch, request.member.as_deref()),
        )
        .unwrap_or_else(|| UNAVAILABLE.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Ledger metadata only: this does not construct a qualified source Peer.
    fn caller() -> Caller {
        let pid = std::process::id();
        let process = Process::observe(pid).unwrap();
        Caller {
            caller: slot::Caller {
                sid: "replay-fixture".into(),
                pid,
                birth: process.birth,
            },
            process,
            start: restart_stamp::kernel_start(pid).unwrap(),
        }
    }
    fn request(counter: u64) -> Request {
        Request::parse(&format!(
            "member-open schema=1 request=1234567890abcdef{counter:016x} operation={counter:032x}"
        ))
        .unwrap()
    }
    #[test]
    fn completed_requests_do_not_exhaust_worker_capacity_and_evicted_ids_never_execute() {
        let caller = caller();
        let mut jobs = Jobs::default();
        for counter in 1..=192 {
            let (job, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
            assert!(start);
            job.finish(Ok("fixture".into()));
        }
        jobs.reap();
        let connection = &jobs.connections[&1];
        assert!(connection.active.is_empty());
        assert_eq!(connection.recent.len(), RECENT_PER_CONNECTION);
        assert_eq!(connection.highwater, 192);
        assert!(matches!(
            jobs.admit(1, &caller, &request(1)),
            Err(AdmissionError::Stale)
        ));
        let mut changed = request(1);
        changed.operation = "f".repeat(32);
        assert!(matches!(
            jobs.admit(1, &caller, &changed),
            Err(AdmissionError::Stale)
        ));
    }
    #[test]
    fn busy_retry_retains_exact_body_and_old_active_requests_survive_newer_counters() {
        let caller = caller();
        let mut jobs = Jobs::default();
        let original = request(1);
        let (job, start) = jobs.admit(1, &caller, &original).unwrap();
        assert!(start);
        let (same, start) = jobs.admit(1, &caller, &original).unwrap();
        assert!(!start && Arc::ptr_eq(&job, &same));
        job.finish(Err(registry::Error::Busy));
        for counter in 2..=32 {
            let (next, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
            assert!(start);
            next.finish(Ok("fixture".into()));
        }
        let mut changed = original.clone();
        changed.operation = "f".repeat(32);
        assert!(matches!(
            jobs.admit(1, &caller, &changed),
            Err(AdmissionError::Changed)
        ));
        let (same, start) = jobs.admit(1, &caller, &original).unwrap();
        assert!(start && Arc::ptr_eq(&job, &same));
        same.finish(Ok("result".into()));
        assert!(!jobs.admit(1, &caller, &original).unwrap().1);
    }
    #[test]
    fn capacity_refuses_before_acceptance_and_eof_cannot_resurrect_completed_workers() {
        let caller = caller();
        let mut jobs = Jobs::default();
        let mut first = None;
        for counter in 1..=MAX_PENDING as u64 {
            let (job, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
            assert_eq!(start, counter <= MAX_RUNNING as u64);
            if counter == 1 {
                first = Some(job);
            }
        }
        assert!(matches!(
            jobs.admit(1, &caller, &request(257)),
            Err(AdmissionError::Capacity)
        ));
        assert_eq!(jobs.connections[&1].highwater, 256);
        let first = first.unwrap();
        first.finish(Ok("fixture".into()));
        assert!(jobs.admit(1, &caller, &request(257)).unwrap().1);
        jobs.closed(1);
        first.finish(Ok("late".into()));
        assert!(jobs.connections.is_empty());
        assert!(jobs.admit(2, &caller, &request(1)).unwrap().1);
    }
    #[test]
    fn eof_does_not_release_running_capacity_before_the_old_worker_finishes() {
        let caller = caller();
        let mut jobs = Jobs::default();
        let mut workers = Vec::new();
        for counter in 1..=MAX_RUNNING as u64 {
            let (job, start) = jobs.admit(1, &caller, &request(counter)).unwrap();
            assert!(start);
            workers.push(job);
        }
        jobs.closed(1);
        let (next, start) = jobs.admit(2, &caller, &request(1)).unwrap();
        assert!(!start);
        assert_eq!(jobs.running.load(Ordering::Acquire), MAX_RUNNING);
        workers.pop().unwrap().finish(Ok("retired".into()));
        let (same, start) = jobs.admit(2, &caller, &request(1)).unwrap();
        assert!(start && Arc::ptr_eq(&same, &next));
        assert_eq!(jobs.running.load(Ordering::Acquire), MAX_RUNNING);
        for worker in workers {
            worker.finish(Ok("retired".into()));
        }
        next.finish(Ok("retired".into()));
        assert_eq!(jobs.running.load(Ordering::Acquire), 0);
    }
    #[test]
    fn sequence_gaps_prefix_changes_and_exhaustion_fail_closed() {
        let caller = caller();
        let mut jobs = Jobs::default();
        jobs.admit(1, &caller, &request(10)).unwrap();
        assert!(matches!(
            jobs.admit(1, &caller, &request(9)),
            Err(AdmissionError::Stale)
        ));
        let mut changed = request(11);
        changed.sequence.prefix ^= 1;
        assert!(matches!(
            jobs.admit(1, &caller, &changed),
            Err(AdmissionError::Stale)
        ));
        assert!(jobs.admit(1, &caller, &request(u64::MAX)).is_ok());
        assert!(Sequence::parse("1234567890abcdef0000000000000000").is_none());
    }
    #[test]
    fn private_member_requests_separate_request_nonce_from_snapshot_and_refuse_extra_authority() {
        let base = format!(
            "schema=1 request={} operation={}",
            "a".repeat(32),
            "b".repeat(32)
        );
        assert!(Request::parse(&format!("member-open {base}")).is_some());
        for suffix in [
            " member=cc",
            " path=/auth.json",
            " launch=secret",
            " schema=1",
        ] {
            assert!(Request::parse(&format!("member-open {base}{suffix}")).is_none());
        }
        let ready = format!(
            "member-ready {base} member={} registration={} credential_generation=3 digest={} account={} user={} auth_generation=7 owner_generation=11 source_generation=13 login_generation=17",
            "c".repeat(32),
            "d".repeat(32),
            "e".repeat(64),
            "f".repeat(64),
            "0".repeat(64)
        );
        assert!(Request::parse(&ready).is_some());
        assert!(
            Request::parse(&ready.replace("credential_generation=3", "credential_generation=0"))
                .is_none()
        );
        assert!(
            Request::parse(&ready.replace("auth_generation=7", "auth_generation=07")).is_none()
        );
        assert!(Request::parse(&format!("{ready} boundary={}", "1".repeat(32))).is_none());
        let updated = ready.replace(&"a".repeat(32), &"2".repeat(32));
        let first = Request::parse(&ready).unwrap();
        let next = Request::parse(&updated).unwrap();
        assert!(first.request != next.request && first.operation == next.operation);
    }
    #[test]
    fn direct_reader_ready_never_invents_or_accepts_manager_counters() {
        let reader = format!(
            "member-reader-ready schema=1 request={} operation={} member={} registration={} credential_generation=3 digest={} account={} user={}",
            "a".repeat(32),
            "b".repeat(32),
            "c".repeat(32),
            "d".repeat(32),
            "e".repeat(64),
            "f".repeat(64),
            "0".repeat(64)
        );
        let parsed = Request::parse(&reader).unwrap();
        assert_eq!(parsed.name(), "member-reader-ready");
        assert!(matches!(
            parsed.action,
            Action::ReaderReady { generation: 3, .. }
        ));
        assert!(
            Request::parse(&reader.replacen("member-reader-ready", "member-ready", 1)).is_none()
        );
        for extra in [
            " auth_generation=0 owner_generation=0 source_generation=0 login_generation=0",
            " path=/auth.json",
            " credential_generation=3",
        ] {
            assert!(Request::parse(&format!("{reader}{extra}")).is_none());
        }
        for generation in ["0", "03"] {
            assert!(
                Request::parse(&reader.replace(
                    "credential_generation=3",
                    &format!("credential_generation={generation}")
                ))
                .is_none()
            );
        }
    }

    #[test]
    fn unrelated_modes_and_unloaded_retirement_do_not_claim_a_source() {
        let base = format!(
            "schema=1 request={} operation={} member={}",
            "a".repeat(32),
            "b".repeat(32),
            "c".repeat(32)
        );
        assert!(matches!(
            Request::parse(&format!("member-retire {base} registration=none"))
                .unwrap()
                .action,
            Action::Retire { registration: None }
        ));
        assert!(Request::parse(&format!("member-unrelated {base} kind=api_key")).is_some());
        assert!(Request::parse(&format!("member-unrelated {base} kind=chatgpt")).is_none());
        assert!(
            Request::parse(&format!(
                "member-unrelated {base} kind=api_key credential_generation=0"
            ))
            .is_none()
        );
    }
}
