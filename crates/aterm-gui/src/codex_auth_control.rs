// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private credential-source discovery and owner-managed refresh. Only a live,
//! qualified Codex peer may receive a source; no reply is a switch permit.

use super::*;
use aterm_agent::harness::{account_work, codex_session_slot as slot};
use std::sync::OnceLock;
use std::time::Instant;

const MAX_FRAME: usize = 16 * 1024;
const UNAVAILABLE: &str = "ERR codex-auth unavailable\n";

#[derive(Clone, PartialEq, Eq)]
enum Request {
    Source {
        operation: String,
    },
    Refresh {
        slot: String,
        operation: String,
        expected_digest: String,
    },
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Request {
    fn parse(line: &str) -> Option<Self> {
        if line.len() > 512 {
            return None;
        }
        let mut words = line.split_ascii_whitespace();
        match words.next()? {
            "source" => {
                let operation = words.next()?.strip_prefix("operation=")?;
                (words.next().is_none() && hex(operation, 32)).then(|| Self::Source {
                    operation: operation.into(),
                })
            }
            "refresh" => {
                let slot = words.next()?.strip_prefix("slot=")?;
                let operation = words.next()?.strip_prefix("operation=")?;
                let digest = words.next()?.strip_prefix("expected_digest=")?;
                (words.next().is_none()
                    && !slot.is_empty()
                    && slot.len() <= 256
                    && slot.len().is_multiple_of(2)
                    && hex(slot, slot.len())
                    && hex(operation, 32)
                    && hex(digest, 64))
                .then(|| Self::Refresh {
                    slot: slot.into(),
                    operation: operation.into(),
                    expected_digest: digest.into(),
                })
            }
            _ => None,
        }
    }

    fn operation(&self) -> &str {
        match self {
            Self::Source { operation } | Self::Refresh { operation, .. } => operation,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Caller {
    proof: crate::self_claim::Proven,
    process: Process,
}

fn claim(ctx: &crate::SessionCtx, master: i32) -> Option<Caller> {
    let proof = accounts_control::claim(ctx, master)?;
    let process = Process::observe(proof.pid)?;
    // Recheck the independent kernel-start spelling too; observing a numeric
    // birth must not accidentally qualify a replacement PID.
    proof.alive().then_some(Caller { proof, process })
}

fn session(sid: &str, caller: &Caller, epoch: &str) -> aterm_json::Value {
    aterm_json::json!({"sid":sid,"process_pid":caller.proof.pid,
        "process_birth":caller.proof.start,"epoch":epoch})
}

fn source_reply(
    sid: &str,
    caller: &Caller,
    epoch: &str,
    operation: &str,
    progress: slot::SourceProgress,
) -> Option<String> {
    let mut value = aterm_json::json!({"schema":1,"operation":operation,
        "session":session(sid, caller, epoch)});
    let prefix = match progress {
        slot::SourceProgress::Waiting => "OK codex-auth source-waiting ",
        slot::SourceProgress::Ready(source) => {
            value = source_value(operation, session(sid, caller, epoch), &source)?;
            "OK codex-auth source "
        }
    };
    let body = aterm_json::to_string(&value).ok()?;
    (prefix.len() + body.len() < MAX_FRAME).then(|| format!("{prefix}{body}\n"))
}

pub(super) fn source_value(
    operation: &str,
    session: aterm_json::Value,
    source: &slot::SourceHandle,
) -> Option<aterm_json::Value> {
    let mut value = aterm_json::json!({"schema":1,"operation":operation,"session":session});
    let path = source.path().to_str()?;
    if path.len() > 4096
        || !source.path().is_absolute()
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| matches!(part, "." | ".."))
        || !hex(source.account(), 64)
        || !hex(source.user(), 64)
        || source.credential_generation() == 0
        || !hex(source.digest(), 64)
        || source.slot().is_empty()
        || source.slot().len() > 256
        || !source.slot().len().is_multiple_of(2)
        || !hex(source.slot(), source.slot().len())
        || match (source.assignment_generation(), source.native_intent()) {
            (0, None) => false,
            (generation, Some(intent)) if generation > 0 && hex(intent, 32) => false,
            _ => true,
        }
    {
        return None;
    }
    let object = value.as_object_mut()?;
    for (key, field) in [
        ("slot", aterm_json::json!(source.slot())),
        ("path", aterm_json::json!(path)),
        ("account", aterm_json::json!(source.account())),
        ("user", aterm_json::json!(source.user())),
        (
            "credential_generation",
            aterm_json::json!(source.credential_generation()),
        ),
        ("digest", aterm_json::json!(source.digest())),
        (
            "assignment_generation",
            aterm_json::json!(source.assignment_generation()),
        ),
        ("native_intent", aterm_json::json!(source.native_intent())),
    ] {
        object.insert(key.into(), field);
    }
    Some(value)
}

fn refresh_reply(request: &Request, progress: &slot::RefreshProgress) -> Option<String> {
    let Request::Refresh {
        slot,
        operation,
        expected_digest,
    } = request
    else {
        return None;
    };
    let (state, revision) = match progress {
        slot::RefreshProgress::Waiting(revision) => ("waiting", revision),
        slot::RefreshProgress::Ready(revision) => ("ready", revision),
        slot::RefreshProgress::Retaining(_) => return None,
    };
    (revision.generation > 0 && hex(&revision.digest, 64)).then(|| format!(
        "OK codex-auth slot={slot} operation={operation} expected_digest={expected_digest} state={state} generation={} digest={}\n",
        revision.generation, revision.digest))
}

fn read(
    sid: &str,
    caller: &Caller,
    epoch: &str,
    request: &Request,
    scope: &account_work::Scope,
) -> Option<String> {
    let env = super::codex_account_env().ok()?;
    let owner = slot::Caller {
        sid: sid.into(),
        pid: caller.proof.pid,
        birth: caller.process.birth,
    };
    match request {
        Request::Source { operation } => source_reply(
            sid,
            caller,
            epoch,
            operation,
            slot::source(&env, &owner, operation).ok()?,
        ),
        Request::Refresh {
            slot,
            operation,
            expected_digest,
        } => {
            let original =
                slot::RefreshRequest::new(slot.clone(), operation.clone(), expected_digest.clone());
            loop {
                if scope.cancelled() {
                    return None;
                }
                let progress = slot::refresh(&env, &owner, &original, scope).ok()?;
                if let slot::RefreshProgress::Retaining(mut retention) = progress {
                    // Cancellation cannot discard a rotated response. This
                    // counted worker retains its claim until encryption and
                    // fsync settle; retry never performs another HTTP request.
                    loop {
                        match retention.retry() {
                            Ok(continuation) => match continuation.reconcile(&env, scope) {
                                slot::RefreshRecovery::Complete => break,
                                slot::RefreshRecovery::Unavailable => return None,
                                slot::RefreshRecovery::Retaining(owned) => {
                                    retention = owned;
                                    std::thread::sleep(Duration::from_secs(1));
                                }
                            },
                            Err(owned) => {
                                retention = owned;
                                std::thread::sleep(Duration::from_secs(1));
                            }
                        }
                    }
                    continue;
                }
                return refresh_reply(request, &progress);
            }
        }
    }
}

struct Job {
    caller: Caller,
    request: Request,
    started: Instant,
    result: Mutex<Option<Option<String>>>,
}
type Jobs = BTreeMap<(String, String), Arc<Job>>;
fn jobs() -> &'static Mutex<Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

pub(crate) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    lifetime: &account_work::Lifetime,
) -> String {
    let Some(request) = Request::parse(line) else {
        return "ERR codex-auth invalid request\n".into();
    };
    let Some(caller) = claim(ctx, master) else {
        return "ERR codex-auth denied\n".into();
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let key = (
        ctx.self_id.as_str().to_owned(),
        request.operation().to_owned(),
    );
    let job = {
        let mut table = jobs().lock().unwrap_or_else(|p| p.into_inner());
        table.retain(|_, job| {
            job.started.elapsed() < Duration::from_secs(120)
                || job
                    .result
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_none()
        });
        if let Some(job) = table.get(&key) {
            if job.caller != caller || job.request != request {
                return "ERR codex-auth stale operation\n".into();
            }
            Arc::clone(job)
        } else {
            if table.len() >= 32 {
                return "ERR codex-auth busy\n".into();
            }
            let Some(scope) = lifetime.enter() else {
                return UNAVAILABLE.into();
            };
            let job = Arc::new(Job {
                caller: caller.clone(),
                request: request.clone(),
                started: Instant::now(),
                result: Mutex::new(None),
            });
            let worker = Arc::clone(&job);
            let sid = key.0.clone();
            let admission = scope.clone();
            if std::thread::Builder::new()
                .name("codex-private-auth".into())
                .spawn(scope.wrap(move || {
                    let result = if admission.cancelled() {
                        None
                    } else {
                        read(&sid, &worker.caller, epoch, &worker.request, &admission)
                    };
                    let result = if admission.cancelled() { None } else { result };
                    *worker.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
                }))
                .is_err()
            {
                return UNAVAILABLE.into();
            }
            // The lifetime owns the worker even if this connection disappears.
            table.insert(key.clone(), Arc::clone(&job));
            job
        }
    };
    let result = job.result.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let Some(result) = result else {
        return match request {
            Request::Source { ref operation } => source_reply(
                &key.0,
                &caller,
                epoch,
                operation,
                slot::SourceProgress::Waiting,
            )
            .unwrap_or_else(|| UNAVAILABLE.into()),
            // Keep polling the original operation before the owner has supplied
            // its real revision; never invent a generation or digest.
            Request::Refresh {
                ref slot,
                ref operation,
                ref expected_digest,
            } => format!(
                "OK codex-auth refresh-pending slot={slot} operation={operation} expected_digest={expected_digest}\n"
            ),
        };
    };
    if claim(ctx, master).as_ref() != Some(&caller) || !caller.proof.alive() {
        return "ERR codex-auth stale session\n".into();
    }
    let mut table = jobs().lock().unwrap_or_else(|p| p.into_inner());
    if table
        .get(&key)
        .is_some_and(|current| Arc::ptr_eq(current, &job))
    {
        table.remove(&key);
    }
    result.unwrap_or_else(|| UNAVAILABLE.into())
}

#[cfg(test)]
#[path = "codex_auth_control_tests.rs"]
mod tests;
