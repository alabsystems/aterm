// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A derived constructor inherits only an actual active native parent use.
//! It does not repeat ordinary foreground/App admission while that work drains.
use super::use_control::{cached, error};
use super::*;

pub(super) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
) -> String {
    let caller = match identify(ctx) {
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
        Ok(job) => job,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if cached(&job).is_none() {
        let result = (|| {
            let Action::OpenDerived {
                use_member,
                use_operation,
                capability,
            } = &request.action
            else {
                return Err(registry::Error::Denied);
            };
            let allocation = registry.open_registered_derived(
                &caller.caller,
                &connection,
                &request.operation,
                registry::UseIdentity {
                    member: use_member,
                    nonce: *use_operation,
                    capability,
                },
            )?;
            frame(
                "member-open",
                &envelope(&request, &caller, epoch, Some(&allocation.member)),
            )
            .ok_or(registry::Error::Unavailable)
        })();
        // Busy is not a terminal allocation outcome. The same exact request can
        // retry after contention without issuing a different constructor nonce.
        if !matches!(
            result,
            Err(registry::Error::Busy | registry::Error::Unavailable)
        ) {
            job.finish(result);
        } else {
            return UNAVAILABLE.into();
        }
    }
    if !connection.is_live() || identify(ctx).as_ref().ok() != Some(&caller) {
        return "ERR codex-auth stale\n".into();
    }
    match cached(&job) {
        Some(Ok(reply)) => reply,
        Some(Err(why)) => error(why),
        None => UNAVAILABLE.into(),
    }
}
