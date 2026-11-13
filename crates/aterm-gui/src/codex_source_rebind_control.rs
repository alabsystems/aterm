// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Existing allocation correlation on a replacement authenticated connection.
//! Bound never means Ready, a live use, or permission to select current auth.
use super::use_control::{cached, error};
use super::*;
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Original {
    Ordinary,
    Derived {
        use_member: String,
        use_operation: String,
        capability: String,
    },
    Target {
        assignment: String,
        context: String,
    },
}
impl Original {
    pub(super) fn parse(fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        match fields.remove("open_kind")? {
            "ordinary" => Some(Self::Ordinary),
            "derived" => {
                let use_member = fields.remove("use_member")?;
                let use_operation = fields.remove("use_operation")?;
                let capability = fields.remove("capability")?;
                if !hex(use_member, 32)
                    || registry::UseNonce::parse(use_operation).is_none()
                    || !hex(capability, 64)
                {
                    return None;
                }
                Some(Self::Derived {
                    use_member: use_member.into(),
                    use_operation: use_operation.into(),
                    capability: capability.into(),
                })
            }
            "target" => {
                let assignment = fields.remove("assignment")?;
                let context = fields.remove("context")?;
                if !hex(assignment, 32) || !hex(context, 64) {
                    return None;
                }
                Some(Self::Target {
                    assignment: assignment.into(),
                    context: context.into(),
                })
            }
            _ => None,
        }
    }
    fn native(&self) -> registry::WorkRebindKind<'_> {
        match self {
            Self::Ordinary => registry::WorkRebindKind::Ordinary,
            Self::Derived {
                use_member,
                use_operation,
                capability,
            } => registry::WorkRebindKind::Derived {
                use_member,
                use_operation,
                capability,
            },
            Self::Target {
                assignment,
                context,
            } => registry::WorkRebindKind::Target {
                assignment,
                context,
            },
        }
    }
    fn echo(&self, object: &mut aterm_json::Map) {
        match self {
            Self::Ordinary => {
                object.insert("open_kind".into(), aterm_json::json!("ordinary"));
            }
            Self::Derived {
                use_member,
                use_operation,
                capability,
            } => {
                object.insert("open_kind".into(), aterm_json::json!("derived"));
                object.insert("use_member".into(), aterm_json::json!(use_member));
                object.insert("use_operation".into(), aterm_json::json!(use_operation));
                object.insert("capability".into(), aterm_json::json!(capability));
            }
            Self::Target {
                assignment,
                context,
            } => {
                object.insert("open_kind".into(), aterm_json::json!("target"));
                object.insert("assignment".into(), aterm_json::json!(assignment));
                object.insert("context".into(), aterm_json::json!(context));
            }
        }
    }
}
fn response(request: &Request, caller: &Caller, epoch: &str) -> Result<String, registry::Error> {
    let Action::Rebind(original) = &request.action else {
        return Err(registry::Error::Denied);
    };
    let mut value = envelope(request, caller, epoch, request.member.as_deref());
    let object = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
    object.insert("state".into(), aterm_json::json!("bound"));
    original.echo(object);
    frame("member-work-rebind", &value).ok_or(registry::Error::Unavailable)
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
    let caller = match observe() {
        Ok(caller) => caller,
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
    if cached(&job).is_none() {
        let Action::Rebind(original) = &request.action else {
            return error(registry::Error::Denied);
        };
        // Synchronous metadata transition, with native-owned original body and
        // actual pending worker/Scope checks. No application/source worker is spawned.
        let result = registry
            .rebind_member_work(
                &caller.caller,
                &connection,
                registry::WorkRebindRequest {
                    operation: &request.operation,
                    member: request.member.as_deref().unwrap_or(""),
                    kind: original.native(),
                },
            )
            .and_then(|()| response(&request, &caller, epoch));
        job.finish(result);
    }
    if observe().as_ref() != Ok(&caller) || !connection.is_live() {
        return error(registry::Error::Denied);
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
#[cfg(test)]
#[path = "codex_source_rebind_control_tests.rs"]
mod tests;
