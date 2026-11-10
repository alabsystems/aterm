// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Registered owed work and release-only cleanup. Neither branch constructs a
//! generic foreground Peer or enters ordinary App admission for cleanup.

use super::*;

pub(super) fn response(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    progress: registry::UseProgress,
) -> Result<String, registry::Error> {
    let mut value = envelope(request, caller, epoch, request.member.as_deref());
    let fields = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
    let (state, capability) = match progress {
        registry::UseProgress::Active(receipt) => ("active", Some(receipt.capability)),
        registry::UseProgress::Closed => ("closed", None),
    };
    fields.insert("state".into(), aterm_json::json!(state));
    fields.insert("capability".into(), aterm_json::json!(capability));
    frame("member-use", &value).ok_or(registry::Error::Unavailable)
}

pub(super) fn error(error: registry::Error) -> String {
    match error {
        registry::Error::Denied => "ERR codex-auth denied\n".into(),
        registry::Error::Stale => "ERR codex-auth stale\n".into(),
        registry::Error::Unsupported => "ERR codex-auth unsupported\n".into(),
        // A semantic intent may already exist from an earlier lost reply.
        // Transport contention never proves that intent was not admitted.
        registry::Error::Busy | registry::Error::Unavailable => UNAVAILABLE.into(),
    }
}

pub(super) fn cached(job: &Job) -> Option<Result<String, registry::Error>> {
    match &*job.progress.lock().unwrap_or_else(|p| p.into_inner()) {
        Progress::Complete(result) => Some(result.clone()),
        _ => None,
    }
}

/// The real call site must store counted native work before exposing Waiting.
pub(super) fn prepare_before_waiting(
    job: &Job,
    prepare: impl FnOnce() -> Result<registry::UseCommand, registry::Error>,
) -> Result<(), registry::Error> {
    let mut prepared = job.prepared_use.lock().unwrap_or_else(|p| p.into_inner());
    {
        let progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
        match &*progress {
            Progress::Complete(_) => return Ok(()),
            Progress::Running { .. } => {
                return if job.use_accounted.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(registry::Error::Unavailable)
                };
            }
            Progress::Waiting => {}
        }
    }
    if prepared.is_none() {
        match prepare() {
            Ok(command) => {
                *prepared = Some(command);
                // Release publishes native custody before any concurrent poll
                // may reserve a worker or expose transport Waiting.
                job.use_accounted.store(true, Ordering::Release);
            }
            Err(why) => {
                if why != registry::Error::Busy {
                    *job.progress.lock().unwrap_or_else(|p| p.into_inner()) =
                        Progress::Complete(Err(why));
                }
                return Err(why);
            }
        }
    }
    Ok(())
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
    observe: impl Fn() -> Result<Caller, registry::Error>,
) -> String {
    // PID is read only from the actual serving socket. This identity is joined
    // to the retained member/process/App namespace by the native Registry.
    let caller = match observe() {
        Ok(caller) => caller,
        Err(why) => return error(why),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let connection = match registry.connection(id) {
        Ok(connection) => connection,
        Err(why) => return error(why),
    };
    let (job, _) = match admit_job(id, &caller, &request) {
        Ok(value) => value,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if cached(&job).is_none() {
        let Action::Use(action) = &request.action else {
            return "ERR codex-auth invalid request\n".into();
        };
        if let Err(why) = prepare_before_waiting(&job, || {
            registry.prepare_use_command(
                &caller.caller,
                &connection,
                &request.operation,
                request.member.as_deref().unwrap_or(""),
                action,
            )
        }) {
            return error(why);
        }
        let (_, start) = jobs()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .reserve_ready(Arc::clone(&job));
        if start {
            let command = job
                .prepared_use
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            let Some(command) = command else {
                job.finish(Err(registry::Error::Unavailable));
                return UNAVAILABLE.into();
            };
            let worker = Arc::clone(&job);
            let registry = Arc::clone(registry);
            if std::thread::Builder::new()
                .name("codex-source-use".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        registry.execute_use_command(command).and_then(|progress| {
                            response(&worker.request, &worker.caller, epoch, progress)
                        })
                    }))
                    .unwrap_or(Err(registry::Error::Unavailable));
                    worker.finish(result);
                })
                .is_err()
            {
                // The Registry still owns an accepted Pending/Active intent.
                // No failed thread creation is a Closed/retirement receipt.
                job.finish(Err(registry::Error::Unavailable));
            }
        }
    }
    if observe().as_ref() != Ok(&caller) || !connection.is_live() {
        return "ERR codex-auth denied\n".into();
    }
    match cached(&job) {
        Some(Ok(line)) => line,
        Some(Err(why)) => error(why),
        None => frame(
            "member-waiting",
            &envelope(&request, &caller, epoch, request.member.as_deref()),
        )
        .unwrap_or_else(|| UNAVAILABLE.into()),
    }
}
