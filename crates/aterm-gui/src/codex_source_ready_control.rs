// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Target readiness completes an already-owned native PRELOAD. It neither
//! enters new App work nor borrows generic foreground/source admission.
use super::use_control::{cached, error};
use super::*;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Report {
    Manager {
        source: Source,
        auth: registry::AuthState,
    },
    Reader(Source),
    Unavailable,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Source {
    generation: u64,
    digest: String,
    account: String,
    user: String,
}
impl Report {
    pub(super) fn parse(action: &str, fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        if action == "member-unavailable-target" {
            return Some(Self::Unavailable);
        }
        let generation = number(fields.remove("credential_generation")?)?;
        let digest = fields.remove("digest")?.to_owned();
        let account = fields.remove("account")?.to_owned();
        let user = fields.remove("user")?.to_owned();
        if generation == 0
            || ![&digest, &account, &user]
                .into_iter()
                .all(|value| hex(value, 64))
        {
            return None;
        }
        let source = Source {
            generation,
            digest,
            account,
            user,
        };
        match action {
            "member-reader-ready-target" => Some(Self::Reader(source)),
            "member-ready-target" => Some(Self::Manager {
                source,
                auth: registry::AuthState {
                    generation: number(fields.remove("auth_generation")?)?,
                    owner: number(fields.remove("owner_generation")?)?,
                    source: number(fields.remove("source_generation")?)?,
                    login: number(fields.remove("login_generation")?)?,
                },
            }),
            _ => None,
        }
    }
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Manager { .. } => "member-ready-target",
            Self::Reader(_) => "member-reader-ready-target",
            Self::Unavailable => "member-unavailable-target",
        }
    }
    pub(super) fn native(&self) -> registry::Ready {
        match self {
            Self::Manager { source, auth } => registry::Ready::Loaded {
                credential_generation: source.generation,
                digest: source.digest.clone(),
                account: source.account.clone(),
                user: source.user.clone(),
                auth: *auth,
            },
            Self::Reader(source) => registry::Ready::ReaderLoaded {
                credential_generation: source.generation,
                digest: source.digest.clone(),
                account: source.account.clone(),
                user: source.user.clone(),
            },
            Self::Unavailable => registry::Ready::PolicyUnavailable,
        }
    }
}

fn response(request: &Request, caller: &Caller, epoch: &str) -> Result<String, registry::Error> {
    let Action::TargetReady { registration, .. } = &request.action else {
        return Err(registry::Error::Denied);
    };
    let mut value = completed_envelope(request, caller, epoch, request.member.as_deref());
    value
        .as_object_mut()
        .ok_or(registry::Error::Unavailable)?
        .insert("registration".into(), aterm_json::json!(registration));
    frame(request.name(), &value).ok_or(registry::Error::Unavailable)
}
fn prepare(
    job: &Job,
    registry: &registry::Registry,
    connection: &registry::Connection,
    epoch: &str,
) -> Result<(), registry::Error> {
    let mut command = job.prepared_ready.lock().unwrap_or_else(|e| e.into_inner());
    {
        let progress = job.progress.lock().unwrap_or_else(|e| e.into_inner());
        match &*progress {
            Progress::Complete(_) => return Ok(()),
            Progress::Running { .. } => {
                return if job.target_accounted.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(registry::Error::Unavailable)
                };
            }
            Progress::Waiting => (),
        }
    }
    if command.is_some() {
        return Ok(());
    }
    let Action::TargetReady {
        assignment,
        context,
        generation,
        registration,
        report,
    } = &job.request.action
    else {
        return Err(registry::Error::Denied);
    };
    match registry.prepare_target_ready(
        &job.caller.caller,
        connection,
        assignment,
        context,
        registry::TargetReadyReport {
            preload_sequence: job.request.owned_preload_sequence()?,
            assignment_generation: *generation,
            member: job.request.member.clone().ok_or(registry::Error::Denied)?,
            operation: job.request.operation.clone(),
            registration: registration.clone(),
            ready: report.native(),
        },
    ) {
        Ok(registry::TargetReadyPreparation::Pending(pending)) => {
            *command = Some(pending);
            job.target_accounted.store(true, Ordering::Release);
            Ok(())
        }
        Ok(registry::TargetReadyPreparation::Settled) => {
            job.finish(response(&job.request, &job.caller, epoch));
            Ok(())
        }
        Err(why) => {
            if why != registry::Error::Busy {
                job.finish(Err(why));
            }
            Err(why)
        }
    }
}

pub(super) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
) -> String {
    serve_identified(request, registry, id, || identify(ctx))
}

pub(super) fn serve_identified(
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
    identity: impl Fn() -> Result<Caller, registry::Error>,
) -> String {
    let caller = match identity() {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let connection = match registry.connection(id) {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let (job, _) = match admit_job(id, &caller, &request) {
        Ok(value) => value,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if let Err(why) = prepare(&job, registry, &connection, epoch) {
        return error(why);
    }
    if cached(&job).is_none() {
        let (_, start) = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve_ready(Arc::clone(&job));
        if start {
            let command = job
                .prepared_ready
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let Some(command) = command else {
                job.finish(Err(registry::Error::Unavailable));
                return UNAVAILABLE.into();
            };
            let worker = Arc::clone(&job);
            let registry = Arc::clone(registry);
            if std::thread::Builder::new()
                .name("codex-target-ready".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let env = super::super::codex_account_env()
                            .map_err(|_| registry::Error::Unavailable)?;
                        registry.run_target_ready(&env, command)?;
                        response(&worker.request, &worker.caller, epoch)
                    }))
                    .unwrap_or(Err(registry::Error::Unavailable));
                    worker.finish(result);
                })
                .is_err()
            {
                job.finish(Err(registry::Error::Unavailable));
            }
        }
    }
    if !connection.is_live() || identity().as_ref() != Ok(&caller) {
        return error(registry::Error::Denied);
    }
    match cached(&job) {
        Some(Ok(reply)) => reply,
        Some(Err(why)) => error(why),
        None => frame(
            "member-waiting",
            &envelope(&request, &caller, epoch, request.member.as_deref()),
        )
        .unwrap_or_else(|| UNAVAILABLE.into()),
    }
}
