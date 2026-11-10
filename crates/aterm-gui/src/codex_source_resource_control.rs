// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resource validation and ACK are actual counted native continuations, not
//! client declarations of readiness or permission to reopen the origin.
use super::use_control::{cached, error};
use super::*;
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Report {
    registration: String,
    boundary: String,
    assignment_generation: u64,
    credential_generation: u64,
    digest: String,
    account: String,
    user: String,
    auth: registry::AuthState,
}
impl Report {
    fn parse(fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        let registration = fields.remove("registration")?;
        let boundary = fields.remove("boundary")?;
        let assignment_generation = number(fields.remove("assignment_generation")?)?;
        let credential_generation = number(fields.remove("credential_generation")?)?;
        let digest = fields.remove("digest")?;
        let account = fields.remove("account")?;
        let user = fields.remove("user")?;
        if !hex(registration, 32)
            || !hex(boundary, 32)
            || assignment_generation == 0
            || credential_generation == 0
            || ![digest, account, user].iter().all(|v| hex(v, 64))
        {
            return None;
        }
        let auth = registry::AuthState {
            generation: number(fields.remove("auth_generation")?)?,
            owner: number(fields.remove("owner_generation")?)?,
            source: number(fields.remove("source_generation")?)?,
            login: number(fields.remove("login_generation")?)?,
        };
        Some(Self {
            registration: registration.into(),
            boundary: boundary.into(),
            assignment_generation,
            credential_generation,
            digest: digest.into(),
            account: account.into(),
            user: user.into(),
            auth,
        })
    }
    fn native<'a>(&'a self, request: &'a Request) -> registry::ResourceReport<'a> {
        registry::ResourceReport {
            operation: &request.operation,
            member: request.member.as_deref().unwrap_or(""),
            registration: &self.registration,
            boundary: &self.boundary,
            assignment_generation: self.assignment_generation,
            credential_generation: self.credential_generation,
            digest: &self.digest,
            account: &self.account,
            user: &self.user,
            auth: self.auth,
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Action {
    Resources {
        assignment: String,
        context: String,
        report: Report,
    },
    Ack {
        context: String,
        receipt: String,
    },
}
impl Action {
    pub(super) fn parse(action: &str, fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        let context = fields.remove("context")?;
        if !hex(context, 64) {
            return None;
        }
        match action {
            "member-target-resource-ready" => {
                let assignment = fields.remove("assignment")?;
                if !hex(assignment, 32) {
                    return None;
                }
                Some(Self::Resources {
                    assignment: assignment.into(),
                    context: context.into(),
                    report: Report::parse(fields)?,
                })
            }
            "member-target-ack" => {
                let receipt = fields.remove("resource_receipt")?;
                hex(receipt, 64).then(|| Self::Ack {
                    context: context.into(),
                    receipt: receipt.into(),
                })
            }
            _ => None,
        }
    }
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Resources { .. } => "member-target-resource-ready",
            Self::Ack { .. } => "member-target-ack",
        }
    }
}
pub(super) enum Command {
    Resources(Box<registry::TargetResourceCommand>),
    Ack(Box<registry::TargetAckCommand>),
}
enum Outcome {
    Resources(registry::TargetResourceReceipt),
    Applied(registry::TargetApplied),
}
fn response(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    outcome: Outcome,
) -> Result<String, registry::Error> {
    let mut value = envelope(request, caller, epoch, request.member.as_deref());
    let fields = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
    match outcome {
        Outcome::Resources(receipt) => {
            let super::Action::Resource(Action::Resources { report, .. }) = &request.action else {
                return Err(registry::Error::Denied);
            };
            fields.insert(
                "registration".into(),
                aterm_json::json!(report.registration),
            );
            fields.insert(
                "resource_receipt".into(),
                aterm_json::json!(receipt.token()),
            );
        }
        Outcome::Applied(applied) => {
            if applied.operation() != request.operation {
                return Err(registry::Error::Stale);
            }
            // This is the immutable accepted ACK, never the owner's latest C.
            let auth = applied.auth();
            fields.insert("phase".into(), aterm_json::json!("applied"));
            fields.insert("boundary".into(), aterm_json::json!(applied.boundary()));
            fields.insert(
                "assignment_generation".into(),
                aterm_json::json!(applied.assignment_generation()),
            );
            fields.insert(
                "credential_generation".into(),
                aterm_json::json!(applied.credential_generation()),
            );
            fields.insert("digest".into(), aterm_json::json!(applied.digest()));
            fields.insert("account".into(), aterm_json::json!(applied.account()));
            fields.insert("user".into(), aterm_json::json!(applied.user()));
            fields.insert("auth_generation".into(), aterm_json::json!(auth.generation));
            fields.insert("owner_generation".into(), aterm_json::json!(auth.owner));
            fields.insert("source_generation".into(), aterm_json::json!(auth.source));
            fields.insert("login_generation".into(), aterm_json::json!(auth.login));
        }
    }
    frame(request.name(), &value).ok_or(registry::Error::Unavailable)
}
fn prepare(
    job: &Job,
    registry: &registry::Registry,
    connection: &registry::Connection,
    epoch: &str,
) -> Result<(), registry::Error> {
    let mut command = job
        .prepared_resource
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if cached(job).is_some()
        || command.is_some()
        || matches!(
            *job.progress.lock().unwrap_or_else(|e| e.into_inner()),
            Progress::Running { .. }
        )
    {
        return Ok(());
    }
    let request = &job.request;
    let super::Action::Resource(action) = &request.action else {
        return Err(registry::Error::Denied);
    };
    let prepared = match action {
        Action::Resources {
            assignment,
            context,
            report,
        } => match registry.prepare_registered_target_resources(
            &job.caller.caller,
            connection,
            assignment,
            context,
            report.native(request),
        )? {
            registry::ResourcePreparation::Ready(receipt) => Err(Outcome::Resources(receipt)),
            registry::ResourcePreparation::Pending(command) => Ok(Command::Resources(command)),
        },
        Action::Ack { context, receipt } => match registry.prepare_registered_target_ack(
            &job.caller.caller,
            &request.operation,
            context,
            receipt,
        )? {
            registry::AckPreparation::Applied(applied) => Err(Outcome::Applied(applied)),
            registry::AckPreparation::Pending(command) => Ok(Command::Ack(command)),
        },
    };
    match prepared {
        Ok(owned) => {
            *command = Some(owned);
            job.resource_accounted.store(true, Ordering::Release);
        }
        Err(outcome) => job.finish(response(request, &job.caller, epoch, outcome)),
    }
    Ok(())
}
impl Command {
    fn run(self, registry: &registry::Registry) -> Result<Outcome, registry::Error> {
        let env = super::super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
        match self {
            Self::Resources(command) => registry
                .run_target_resources(command, &env)
                .map(Outcome::Resources),
            Self::Ack(command) => registry.run_target_ack(command, &env).map(Outcome::Applied),
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
    if let Err(why) = prepare(&job, registry, &connection, epoch) {
        return error(why);
    }
    if cached(&job).is_none() {
        let (_, start) = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve_ready(job.clone());
        if start {
            let Some(command) = job
                .prepared_resource
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            else {
                job.finish(Err(registry::Error::Unavailable));
                return UNAVAILABLE.into();
            };
            let worker = job.clone();
            let registry = registry.clone();
            if std::thread::Builder::new()
                .name("codex-target-resources".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        command.run(&registry).and_then(|outcome| {
                            response(&worker.request, &worker.caller, epoch, outcome)
                        })
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
mod tests {
    use super::*;
    fn resource() -> String {
        format!(
            "member-target-resource-ready schema=1 request=0123456789abcdef0000000000000001 operation={} member={} assignment={} context={} registration={} boundary={} assignment_generation=7 credential_generation=11 digest={} account={} user={} auth_generation=13 owner_generation=17 source_generation=19 login_generation=23",
            "a".repeat(32),
            "b".repeat(32),
            "c".repeat(32),
            "d".repeat(64),
            "e".repeat(32),
            "f".repeat(32),
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64)
        )
    }
    fn ack() -> String {
        format!(
            "member-target-ack schema=1 request=0123456789abcdef0000000000000001 operation={} context={} resource_receipt={}",
            "c".repeat(32),
            "d".repeat(64),
            "e".repeat(64)
        )
    }
    #[test]
    fn sdk_resource_and_ack_grammar_has_exact_roles_and_no_invented_source_completion_fields() {
        let resource = resource();
        let ack = ack();
        let request = Request::parse(&resource).unwrap();
        assert_eq!(request.name(), "member-target-resource-ready");
        assert!(request.member.is_some());
        assert!(request.preload_sequence.is_none());
        let super::super::Action::Resource(Action::Resources { report, .. }) = &request.action
        else {
            panic!("resource parser");
        };
        let report = report.native(&request);
        assert_eq!(report.operation, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(report.auth.source, 19);
        let request = Request::parse(&ack).unwrap();
        assert_eq!(request.name(), "member-target-ack");
        assert!(request.member.is_none());
        for invalid in [
            format!("{resource} preload_sequence=11111111111111110000000000000001"),
            resource.replace("credential_generation=11", "credential_generation=0"),
            resource.replace("assignment_generation=7", "assignment_generation=0"),
            resource.replace(" auth_generation=13", ""),
            format!("{ack} member={}", "b".repeat(32)),
            format!("{ack} assignment={}", "c".repeat(32)),
            format!("{ack} credential_generation=11"),
            format!("{ack} resource_receipt={}", "e".repeat(64)),
        ] {
            assert!(
                Request::parse(&invalid).is_none(),
                "strict closed action body"
            );
        }
    }
    #[test]
    fn immutable_resource_body_cannot_change_and_neither_action_schedules_before_native_custody() {
        let caller = super::super::tests::caller();
        for line in [resource(), ack()] {
            let request = Request::parse(&line).unwrap();
            let mut jobs = Jobs::default();
            let (job, start) = jobs.admit(78001, &caller, &request).unwrap();
            assert!(!start);
            assert!(!jobs.reserve_ready(job).1);
            let changed = if line.starts_with("member-target-ack") {
                line.replace(&"e".repeat(64), &"f".repeat(64))
            } else {
                line.replace("source_generation=19", "source_generation=20")
            };
            assert!(matches!(
                jobs.admit(78001, &caller, &Request::parse(&changed).unwrap()),
                Err(AdmissionError::Changed)
            ));
        }
    }
}
