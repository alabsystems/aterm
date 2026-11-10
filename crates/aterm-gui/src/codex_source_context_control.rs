// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exact owned contextual reads/completions. Raw role fields grant nothing.
use super::use_control::{cached, error};
use super::*;
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Parent {
    member: String,
    nonce: registry::UseNonce,
    capability: String,
}
impl Parent {
    fn parse(fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        let member = fields.remove("use_member")?;
        let nonce = registry::UseNonce::parse(fields.remove("use_operation")?)?;
        let capability = fields.remove("capability")?;
        (hex(member, 32) && hex(capability, 64)).then(|| Self {
            member: member.into(),
            nonce,
            capability: capability.into(),
        })
    }
    fn identity(&self) -> registry::PreloadUseIdentity<'_> {
        registry::PreloadUseIdentity {
            member: &self.member,
            nonce: self.nonce,
            capability: &self.capability,
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Role {
    Ordinary,
    Use(Parent),
    Target { assignment: String, context: String },
}
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Action {
    ReboundPreload,
    ReboundReady {
        registration: String,
        report: ready_control::Report,
    },
    Preload {
        role: Role,
        previous: Option<registry::PreloadPredecessor>,
    },
    Ready {
        parent: Parent,
        registration: String,
        report: ready_control::Report,
    },
    Refresh {
        parent: Parent,
        slot: String,
        digest: String,
    },
}
impl Action {
    pub(super) fn parse(action: &str, fields: &mut BTreeMap<&str, &str>) -> Option<Self> {
        match action {
            "member-preload-rebound" => Some(Self::ReboundPreload),
            "member-rebound-ready" => {
                let registration = fields.remove("registration")?;
                if !hex(registration, 32) {
                    return None;
                }
                let report = ready_control::Report::parse("member-ready-target", fields)?;
                Some(Self::ReboundReady {
                    registration: registration.into(),
                    report,
                })
            }
            "member-ready-use" | "member-reader-ready-use" | "member-unavailable-use" => {
                let parent = Parent::parse(fields)?;
                let registration = fields.remove("registration")?;
                if !hex(registration, 32) {
                    return None;
                }
                let report = ready_control::Report::parse(
                    match action {
                        "member-ready-use" => "member-ready-target",
                        "member-reader-ready-use" => "member-reader-ready-target",
                        _ => "member-unavailable-target",
                    },
                    fields,
                )?;
                Some(Self::Ready {
                    parent,
                    registration: registration.into(),
                    report,
                })
            }
            "member-refresh-use" => {
                let parent = Parent::parse(fields)?;
                let slot = fields.remove("slot")?;
                let digest = fields.remove("expected_digest")?;
                if slot.is_empty()
                    || slot.len() > 512
                    || slot.len() % 2 != 0
                    || !hex(slot, slot.len())
                    || !hex(digest, 64)
                {
                    return None;
                }
                Some(Self::Refresh {
                    parent,
                    slot: slot.into(),
                    digest: digest.into(),
                })
            }
            "member-preload-use"
            | "member-preload-use-resample"
            | "member-preload-resample"
            | "member-preload-target-resample" => {
                let role = match action {
                    "member-preload-use" | "member-preload-use-resample" => {
                        Role::Use(Parent::parse(fields)?)
                    }
                    "member-preload-target-resample" => {
                        let assignment = fields.remove("assignment")?;
                        let context = fields.remove("context")?;
                        if !hex(assignment, 32) || !hex(context, 64) {
                            return None;
                        }
                        Role::Target {
                            assignment: assignment.into(),
                            context: context.into(),
                        }
                    }
                    _ => Role::Ordinary,
                };
                let previous = if action.ends_with("-resample") {
                    let registration = fields.remove("previous_registration")?;
                    let credential_generation =
                        number(fields.remove("expected_credential_generation")?)?;
                    let digest = fields.remove("expected_digest")?;
                    if !hex(registration, 32) || credential_generation == 0 || !hex(digest, 64) {
                        return None;
                    }
                    Some(registry::PreloadPredecessor {
                        registration: registration.into(),
                        credential_generation,
                        digest: digest.into(),
                    })
                } else {
                    None
                };
                Some(Self::Preload { role, previous })
            }
            _ => None,
        }
    }
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::ReboundPreload => "member-preload-rebound",
            Self::ReboundReady { .. } => "member-rebound-ready",
            Self::Preload {
                role: Role::Use(_),
                previous: None,
            } => "member-preload",
            Self::Preload {
                role: Role::Use(_),
                previous: Some(_),
            } => "member-preload-use-resample",
            Self::Preload {
                role: Role::Target { .. },
                ..
            } => "member-preload-target-resample",
            Self::Preload { .. } => "member-preload-resample",
            Self::Ready { report, .. } => match report {
                ready_control::Report::Manager { .. } => "member-ready",
                ready_control::Report::Reader(_) => "member-reader-ready",
                _ => "member-unavailable",
            },
            Self::Refresh { .. } => "member-refresh",
        }
    }
}
pub(super) enum Command {
    ReboundReady(Box<registry::ReboundReadyCommand>),
    Preload(Box<registry::CapturedPreloadCommand>),
    Resample(
        Box<registry::CapturedPreloadCommand>,
        registry::PreloadPredecessor,
    ),
    Target(Box<registry::TargetResampleCommand>),
    Ready(Box<registry::CapturedReadyCommand>),
    Refresh(Box<registry::OwnedUseRefresh>),
}
enum Outcome {
    Preload(registry::PreloadResample),
    Ready,
    ReboundChanged,
    Refresh(slot::RefreshProgress),
}
fn response(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    outcome: Outcome,
) -> Result<String, registry::Error> {
    if matches!(&outcome, Outcome::ReboundChanged) {
        return Ok("ERR codex-auth rebound changed\n".into());
    }
    let mut value = completed_envelope(request, caller, epoch, request.member.as_deref());
    let object = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
    match outcome {
        Outcome::ReboundChanged => return Err(registry::Error::Denied),
        Outcome::Ready => {
            let super::Action::Context(
                Action::Ready { registration, .. } | Action::ReboundReady { registration, .. },
            ) = &request.action
            else {
                return Err(registry::Error::Denied);
            };
            object.insert("registration".into(), aterm_json::json!(registration));
        }
        Outcome::Preload(result) => match result {
            registry::PreloadResample::Preload(preload) => {
                if matches!(
                    &request.action,
                    super::Action::Context(Action::Preload {
                        previous: Some(_),
                        ..
                    })
                ) {
                    object.insert("state".into(), aterm_json::json!("preload"));
                }
                object.insert(
                    "registration".into(),
                    aterm_json::json!(preload.registration),
                );
                object.insert(
                    "source".into(),
                    super::super::auth_control::source_value(
                        &request.operation,
                        session(caller, epoch),
                        preload.source(),
                    )
                    .ok_or(registry::Error::Unavailable)?,
                );
            }
            registry::PreloadResample::ReadyCommitted { previous } => {
                object.insert("state".into(), aterm_json::json!("ready_committed"));
                object.insert(
                    "previous_registration".into(),
                    aterm_json::json!(previous.registration),
                );
                object.insert(
                    "expected_credential_generation".into(),
                    aterm_json::json!(previous.credential_generation),
                );
                object.insert("expected_digest".into(), aterm_json::json!(previous.digest));
            }
        },
        Outcome::Refresh(progress) => {
            let super::Action::Context(Action::Refresh { slot, digest, .. }) = &request.action
            else {
                return Err(registry::Error::Denied);
            };
            let (state, revision) = match progress {
                slot::RefreshProgress::Ready(revision) => ("ready", revision),
                slot::RefreshProgress::Waiting(revision) => ("waiting", revision),
                _ => return Err(registry::Error::Unavailable),
            };
            object.insert("slot".into(), aterm_json::json!(slot));
            object.insert("expected_digest".into(), aterm_json::json!(digest));
            object.insert("state".into(), aterm_json::json!(state));
            object.insert("generation".into(), aterm_json::json!(revision.generation));
            object.insert("digest".into(), aterm_json::json!(revision.digest));
        }
    }
    frame(request.name(), &value).ok_or(registry::Error::Unavailable)
}
fn resample_command(
    prepared: registry::ResamplePreparation,
    previous: &registry::PreloadPredecessor,
) -> Result<Command, Outcome> {
    match prepared {
        registry::ResamplePreparation::Pending(command) => {
            Ok(Command::Resample(command, previous.clone()))
        }
        registry::ResamplePreparation::Settled(result) => Err(Outcome::Preload(result)),
    }
}
fn prepare(
    job: &Job,
    registry: &registry::Registry,
    connection: &registry::Connection,
    epoch: &str,
) -> Result<(), registry::Error> {
    let mut owned = job
        .prepared_context
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if cached(job).is_some()
        || owned.is_some()
        || matches!(
            *job.progress.lock().unwrap_or_else(|e| e.into_inner()),
            Progress::Running { .. }
        )
    {
        return Ok(());
    }
    let request = &job.request;
    let caller = &job.caller.caller;
    let member = request.member.as_deref().ok_or(registry::Error::Denied)?;
    let super::Action::Context(action) = &request.action else {
        return Err(registry::Error::Denied);
    };
    let prepared: Result<Command, Outcome> = match action {
        Action::ReboundPreload => Ok(Command::Preload(registry.prepare_rebound_preload(
            caller,
            connection,
            member,
            &request.operation,
            request.owned_preload_sequence()?,
        )?)),
        Action::ReboundReady {
            registration,
            report,
        } => Ok(Command::ReboundReady(registry.prepare_rebound_ready(
            caller,
            connection,
            registry::ReboundReadyRequest {
                member,
                operation: &request.operation,
                sequence: request.owned_preload_sequence()?,
                registration,
                ready: report.native(),
            },
        )?)),
        Action::Preload { role, previous } => {
            let seq = request.owned_preload_sequence()?;
            match (role, previous) {
                (Role::Use(parent), None) => {
                    Ok(Command::Preload(Box::new(registry.prepare_preload_use(
                        caller,
                        member,
                        &request.operation,
                        seq,
                        connection,
                        parent.identity(),
                    )?)))
                }
                (Role::Use(parent), Some(previous)) => resample_command(
                    registry.prepare_contextual_preload_resample(
                        caller,
                        member,
                        &request.operation,
                        seq,
                        connection,
                        previous,
                        parent.identity(),
                    )?,
                    previous,
                ),
                (Role::Ordinary, Some(previous)) => resample_command(
                    registry.prepare_ordinary_preload_resample(
                        caller,
                        member,
                        &request.operation,
                        seq,
                        connection,
                        previous,
                    )?,
                    previous,
                ),
                (
                    Role::Target {
                        assignment,
                        context,
                    },
                    Some(previous),
                ) => {
                    match registry.prepare_target_resample(
                        caller,
                        connection,
                        registry::TargetResampleRequest {
                            assignment,
                            context,
                            member,
                            operation: &request.operation,
                            sequence: seq,
                            previous,
                        },
                    ) {
                        Ok(registry::TargetResamplePreparation::Pending(command)) => {
                            Ok(Command::Target(command))
                        }
                        Ok(registry::TargetResamplePreparation::Settled(result)) => {
                            Err(Outcome::Preload(result))
                        }
                        Err(registry::Error::Stale) => resample_command(
                            registry.prepare_normalized_resample(
                                caller,
                                connection,
                                registry::NormalizedMemberRequest {
                                    assignment,
                                    context,
                                    member,
                                    operation: &request.operation,
                                },
                                seq,
                                previous,
                            )?,
                            previous,
                        ),
                        Err(error) => return Err(error),
                    }
                }
                _ => return Err(registry::Error::Denied),
            }
        }
        Action::Ready {
            parent,
            registration,
            report,
        } => match registry.prepare_preload_use_ready(
            caller,
            connection,
            member,
            &request.operation,
            request.owned_preload_sequence()?,
            parent.identity(),
            registration,
            report.native(),
        )? {
            registry::CapturedReadyPreparation::Pending(command) => Ok(Command::Ready(command)),
            registry::CapturedReadyPreparation::Settled => Err(Outcome::Ready),
        },
        Action::Refresh {
            parent,
            slot,
            digest,
        } => {
            if parent.member != member {
                return Err(registry::Error::Denied);
            }
            let active = registry.registered_source_use(
                caller,
                connection,
                &parent.member,
                parent.nonce,
                &parent.capability,
            )?;
            let env =
                super::super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            Ok(Command::Refresh(Box::new(active.prepare_refresh(
                caller,
                env.home.as_deref().ok_or(registry::Error::Unavailable)?,
                slot::RefreshRequest::new(slot.clone(), request.operation.clone(), digest.clone()),
            )?)))
        }
    };
    match prepared {
        Ok(command) => {
            *owned = Some(command);
            job.context_accounted.store(true, Ordering::Release);
        }
        Err(outcome) => job.finish(response(request, &job.caller, epoch, outcome)),
    }
    Ok(())
}
impl Command {
    fn run(self, registry: &registry::Registry) -> Result<Outcome, registry::Error> {
        let env = super::super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
        match self {
            Self::ReboundReady(command) => (*command).run(&env).map(|actual| match actual {
                registry::ReboundReadyOutcome::Ready => Outcome::Ready,
                registry::ReboundReadyOutcome::SourceChanged => Outcome::ReboundChanged,
            }),
            Self::Preload(mut command) => command
                .preload(&env)
                .map(registry::PreloadResample::Preload)
                .map(Outcome::Preload),
            Self::Resample(mut command, previous) => {
                command.resample(&env, &previous).map(Outcome::Preload)
            }
            Self::Refresh(command) => command.run(&env).map(Outcome::Refresh),
            Self::Ready(command) => (*command).run(&env).map(|()| Outcome::Ready),
            Self::Target(command) => registry
                .run_target_resample(&env, command)
                .map(Outcome::Preload),
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
                .prepared_context
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
                .name("codex-source-context".into())
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
    fn base() -> String {
        format!(
            "schema=1 request=0123456789abcdef0000000000000001 operation={} member={}",
            "a".repeat(32),
            "b".repeat(32)
        )
    }
    #[test]
    fn rebound_routes_are_distinct_manager_only_closed_bodies() {
        let preload = format!("member-preload-rebound {} {}", base(), sequence());
        let request = Request::parse(&preload).unwrap();
        assert!(matches!(
            request.action,
            super::super::Action::Context(Action::ReboundPreload)
        ));
        assert_eq!(request.name(), "member-preload-rebound");
        assert!(Request::parse(&format!("{preload} {}", parent())).is_none());
        assert!(Request::parse(&format!("{preload} context={}", "1".repeat(64))).is_none());
        assert!(Request::parse(&preload.replace(sequence(), "")).is_none());
        let report = format!(
            "registration={} credential_generation=7 digest={} account={} user={} auth_generation=1 owner_generation=2 source_generation=3 login_generation=4",
            "c".repeat(32),
            "d".repeat(64),
            "e".repeat(64),
            "f".repeat(64)
        );
        let ready = format!("member-rebound-ready {} {} {report}", base(), sequence());
        let request = Request::parse(&ready).unwrap();
        assert_eq!(request.name(), "member-rebound-ready");
        assert!(Request::parse(&ready.replace("auth_generation=1", "")).is_none());
        assert!(Request::parse(&format!("{ready} {}", parent())).is_none());
        let pid = std::process::id();
        let process = Process::observe(pid).unwrap();
        let caller = Caller {
            caller: slot::Caller {
                sid: "s-rebound-frame".into(),
                pid,
                birth: process.birth,
            },
            process,
            start: restart_stamp::kernel_start(pid).unwrap(),
        };
        assert_eq!(
            response(&request, &caller, "test-epoch", Outcome::ReboundChanged).unwrap(),
            "ERR codex-auth rebound changed\n"
        );
    }
    fn parent() -> String {
        format!(
            "use_member={} use_operation=1234567890abcdef0000000000000001 capability={}",
            "b".repeat(32),
            "c".repeat(64)
        )
    }
    fn sequence() -> &'static str {
        "preload_sequence=0123456789abcdef0000000000000001"
    }
    fn predecessor() -> String {
        format!(
            "previous_registration={} expected_credential_generation=7 expected_digest={}",
            "d".repeat(32),
            "e".repeat(64)
        )
    }
    #[test]
    fn actual_parser_requires_exact_owned_sequence_role_and_closed_reports() {
        let lines = [
            format!("member-preload-use {} {} {}", base(), sequence(), parent()),
            format!(
                "member-preload-use-resample {} {} {} {}",
                base(),
                sequence(),
                parent(),
                predecessor()
            ),
            format!(
                "member-preload-resample {} {} {}",
                base(),
                sequence(),
                predecessor()
            ),
            format!(
                "member-preload-target-resample {} {} {} assignment={} context={}",
                base(),
                sequence(),
                predecessor(),
                "f".repeat(32),
                "a".repeat(64)
            ),
            format!(
                "member-unavailable-use {} {} {} registration={}",
                base(),
                sequence(),
                parent(),
                "d".repeat(32)
            ),
            format!(
                "member-reader-ready-use {} {} {} registration={} credential_generation=7 digest={} account={} user={}",
                base(),
                sequence(),
                parent(),
                "d".repeat(32),
                "e".repeat(64),
                "f".repeat(64),
                "a".repeat(64)
            ),
            format!(
                "member-ready-use {} {} {} registration={} credential_generation=7 digest={} account={} user={} auth_generation=3 owner_generation=4 source_generation=5 login_generation=6",
                base(),
                sequence(),
                parent(),
                "d".repeat(32),
                "e".repeat(64),
                "f".repeat(64),
                "a".repeat(64)
            ),
            format!(
                "member-refresh-use {} {} slot=736c6f74 expected_digest={}",
                base(),
                parent(),
                "e".repeat(64)
            ),
        ];
        for line in lines {
            let parsed = Request::parse(&line).expect("current exact SDK route");
            assert!(matches!(parsed.action, super::super::Action::Context(_)));
            assert!(Request::parse(&format!("{line} unknown=1")).is_none());
            assert!(Request::parse(&format!("{line} operation={}", "a".repeat(32))).is_none());
            if line.contains("preload_sequence=") {
                assert!(Request::parse(&line.replace(sequence(), "")).is_none());
                assert!(
                    Request::parse(&line.replace(
                        "preload_sequence=0123456789abcdef0000000000000001",
                        "preload_sequence=0123456789abcdef0000000000000000"
                    ))
                    .is_none()
                );
            }
        }
        let unavailable = format!(
            "member-unavailable-use {} {} {} registration={} credential_generation=7",
            base(),
            sequence(),
            parent(),
            "d".repeat(32)
        );
        assert!(
            Request::parse(&unavailable).is_none(),
            "unavailable cannot report a credential observation"
        );
    }
    #[test]
    fn uncounted_context_cannot_schedule_and_same_rpc_cannot_change_its_role_body() {
        let request = Request::parse(&format!(
            "member-preload-use {} {} {}",
            base(),
            sequence(),
            parent()
        ))
        .unwrap();
        let caller = super::super::tests::caller();
        let mut jobs = Jobs::default();
        let (job, start) = jobs.admit(8001, &caller, &request).unwrap();
        assert!(!start);
        assert!(
            !jobs.reserve_ready(job).1,
            "native command custody is required before asynchronous scheduling"
        );
        for changed in [
            format!(
                "member-preload-use {} {} {}",
                base().replace(&"a".repeat(32), &"d".repeat(32)),
                sequence(),
                parent()
            ),
            format!(
                "member-preload-use {} preload_sequence=0123456789abcdef0000000000000002 {}",
                base(),
                parent()
            ),
            format!(
                "member-preload-use {} {} {}",
                base(),
                sequence(),
                parent().replace(&"c".repeat(64), &"f".repeat(64))
            ),
        ] {
            assert!(matches!(
                jobs.admit(8001, &caller, &Request::parse(&changed).unwrap()),
                Err(AdmissionError::Changed)
            ));
        }
    }
    #[test]
    fn ready_committed_echoes_only_exact_predecessor_not_source_or_readiness() {
        let request = Request::parse(&format!(
            "member-preload-use-resample {} {} {} {}",
            base(),
            sequence(),
            parent(),
            predecessor()
        ))
        .unwrap();
        let previous = registry::PreloadPredecessor {
            registration: "d".repeat(32),
            credential_generation: 7,
            digest: "e".repeat(64),
        };
        let line = response(
            &request,
            &super::super::tests::caller(),
            "native-epoch",
            Outcome::Preload(registry::PreloadResample::ReadyCommitted { previous }),
        )
        .unwrap();
        let value: aterm_json::Value = aterm_json::from_str(
            line.strip_prefix("OK codex-auth member-preload-use-resample ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["state"].as_str(), Some("ready_committed"));
        assert_eq!(
            value["preload_sequence"].as_str(),
            Some("0123456789abcdef0000000000000001")
        );
        assert_eq!(
            value["previous_registration"].as_str(),
            Some("dddddddddddddddddddddddddddddddd")
        );
        assert_eq!(value["expected_credential_generation"].as_u64(), Some(7));
        assert!(value.get("source").is_none());
        assert!(value.get("registration").is_none());
    }
}
