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
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[path = "codex_source_census_control.rs"]
mod census_control;
#[path = "codex_prepared_policy_control.rs"]
mod policy_control;
pub(super) use policy_control::serve as serve_policy;
#[path = "codex_source_context_control.rs"]
mod context_control;
#[path = "codex_source_derived_control.rs"]
mod derived_control;
#[path = "codex_source_plain_retirement_control.rs"]
mod plain_retirement_control;
#[path = "codex_source_ready_control.rs"]
mod ready_control;
#[path = "codex_source_rebind_control.rs"]
mod rebind_control;
#[path = "codex_source_resource_control.rs"]
mod resource_control;
#[path = "codex_source_retirement_control.rs"]
mod retirement_control;
#[path = "codex_source_target_control.rs"]
mod target_control;
#[path = "codex_source_use_control.rs"]
mod use_control;

const MAX_FRAME: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 256;
const MAX_PENDING: usize = 256;
const MAX_RUNNING: usize = 64;
const MAX_CLEANUP_PENDING: usize = 64;
const MAX_CLEANUP_RUNNING: usize = 16;
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
    Peer,
    Context(context_control::Action),
    Rebind(rebind_control::Original),
    Resource(resource_control::Action),
    OpenDerived {
        use_member: String,
        use_operation: registry::UseNonce,
        capability: String,
    },
    Use(registry::UseAction),
    Reserve(aterm_agent::harness::codex_assignment::protocol::Request),
    Reservation,
    TargetContext,
    OpenTarget {
        assignment: String,
        context: String,
    },
    PreloadTarget {
        assignment: String,
        context: String,
    },
    TargetReady {
        assignment: String,
        context: String,
        generation: u64,
        registration: String,
        report: ready_control::Report,
    },
    CloseTargetOpen {
        open_operation: String,
        assignment: String,
        context: String,
    },
    Seal {
        capability: String,
    },
    Install {
        capability: String,
        boundary: String,
    },
    Census {
        capability: String,
        cursor: Option<(String, String)>,
    },
    Quiesce {
        capability: String,
        registration: String,
        generation: u64,
        digest: String,
        auth: Option<registry::AuthState>,
    },
    Open,
    ControlBegin,
    ControlAck {
        nonce: String,
    },
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
    RetireOwned {
        preload_operation: String,
        registration: Option<String>,
    },
}
#[derive(Clone, PartialEq, Eq)]
struct Request {
    request: String,
    sequence: Sequence,
    operation: String,
    preload_sequence: Option<registry::PreloadSequence>,
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
        let preload_sequence = if matches!(
            action,
            "member-preload"
                | "member-preload-rebound"
                | "member-rebound-ready"
                | "member-preload-use"
                | "member-preload-use-resample"
                | "member-preload-resample"
                | "member-preload-target-resample"
                | "member-ready-use"
                | "member-reader-ready-use"
                | "member-unavailable-use"
                | "member-ready"
                | "member-reader-ready"
                | "member-unavailable"
                | "member-preload-target"
                | "member-ready-target"
                | "member-reader-ready-target"
                | "member-unavailable-target"
                | "member-retire-owned"
        ) {
            Some(registry::PreloadSequence::parse(
                fields.remove("preload_sequence")?,
            )?)
        } else {
            None
        };
        let member = if action == "member-target-open-close" {
            match fields.remove("member") {
                Some(member) if hex(member, 32) => Some(member.into()),
                Some(_) => return None,
                None => None,
            }
        } else if matches!(
            action,
            "member-peer"
                | "member-open"
                | "member-open-derived"
                | "member-target-context"
                | "member-target-ack"
                | "member-open-target"
        ) {
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
            "member-peer" => Action::Peer,
            "member-target-resource-ready" | "member-target-ack" => {
                Action::Resource(resource_control::Action::parse(action, &mut fields)?)
            }
            "member-work-rebind" => Action::Rebind(rebind_control::Original::parse(&mut fields)?),
            "member-open-derived" => {
                let use_member = fields.remove("use_member")?;
                let use_operation = registry::UseNonce::parse(fields.remove("use_operation")?)?;
                let capability = fields.remove("capability")?;
                if !hex(use_member, 32) || !hex(capability, 64) {
                    return None;
                }
                Action::OpenDerived {
                    use_member: use_member.into(),
                    use_operation,
                    capability: capability.into(),
                }
            }
            "member-preload-rebound"
            | "member-rebound-ready"
            | "member-preload-use"
            | "member-preload-use-resample"
            | "member-preload-resample"
            | "member-preload-target-resample"
            | "member-ready-use"
            | "member-reader-ready-use"
            | "member-unavailable-use"
            | "member-refresh-use" => {
                Action::Context(context_control::Action::parse(action, &mut fields)?)
            }
            "member-ready-target" | "member-reader-ready-target" | "member-unavailable-target" => {
                let assignment = fields.remove("assignment")?;
                let context = fields.remove("context")?;
                let generation = number(fields.remove("assignment_generation")?)?;
                if !hex(assignment, 32) || !hex(context, 64) || generation == 0 {
                    return None;
                }
                Action::TargetReady {
                    assignment: assignment.into(),
                    context: context.into(),
                    generation,
                    registration: registration(&mut fields)?,
                    report: ready_control::Report::parse(action, &mut fields)?,
                }
            }
            "member-target-open-close" => {
                let open_operation = fields.remove("open_operation")?;
                let assignment = fields.remove("assignment")?;
                let context = fields.remove("context")?;
                if !hex(open_operation, 32) || !hex(assignment, 32) || !hex(context, 64) {
                    return None;
                }
                Action::CloseTargetOpen {
                    open_operation: open_operation.into(),
                    assignment: assignment.into(),
                    context: context.into(),
                }
            }
            "member-open-target" | "member-preload-target" => {
                let assignment = fields.remove("assignment")?;
                let context = fields.remove("context")?;
                if !hex(assignment, 32) || !hex(context, 64) {
                    return None;
                }
                if action == "member-open-target" {
                    Action::OpenTarget {
                        assignment: assignment.into(),
                        context: context.into(),
                    }
                } else {
                    Action::PreloadTarget {
                        assignment: assignment.into(),
                        context: context.into(),
                    }
                }
            }
            "member-target-context" => Action::TargetContext,
            "member-seal" | "member-install" => {
                let capability = fields.remove("capability")?;
                if !hex(capability, 64) {
                    return None;
                }
                if action == "member-seal" {
                    Action::Seal {
                        capability: capability.into(),
                    }
                } else {
                    let boundary = fields.remove("boundary")?;
                    if !hex(boundary, 32) {
                        return None;
                    }
                    Action::Install {
                        capability: capability.into(),
                        boundary: boundary.into(),
                    }
                }
            }
            "member-reservation" => Action::Reservation,
            "member-quiesce" => {
                let capability = fields.remove("capability")?;
                let registration = registration(&mut fields)?;
                let generation = number(fields.remove("credential_generation")?)?;
                let digest = fields.remove("digest")?;
                if !hex(capability, 64) || !hex(digest, 64) || generation == 0 {
                    return None;
                }
                let auth = match fields.remove("role")? {
                    "reader" => None,
                    "manager" => Some(registry::AuthState {
                        generation: number(fields.remove("auth_generation")?)?,
                        owner: number(fields.remove("owner_generation")?)?,
                        source: number(fields.remove("source_generation")?)?,
                        login: number(fields.remove("login_generation")?)?,
                    }),
                    _ => return None,
                };
                Action::Quiesce {
                    capability: capability.into(),
                    registration,
                    generation,
                    digest: digest.into(),
                    auth,
                }
            }
            "member-census" => {
                let capability = fields.remove("capability")?;
                if !hex(capability, 64) {
                    return None;
                }
                let cursor = match (fields.remove("revision"), fields.remove("after")) {
                    (None, None) => None,
                    (Some(revision), Some(after)) if hex(revision, 64) && hex(after, 32) => {
                        Some((revision.into(), after.into()))
                    }
                    _ => return None,
                };
                Action::Census {
                    capability: capability.into(),
                    cursor,
                }
            }
            "member-reserve" => {
                let pool_revision = fields.remove("pool_revision")?;
                let expected_assignment = fields.remove("expected_assignment")?;
                let selection = fields.remove("selection")?;
                let prepared = aterm_agent::harness::codex_assignment::protocol::Request::parse(
                    &format!(
                        "assignment-prepare schema=1 operation={operation} pool_revision={pool_revision} expected_assignment={expected_assignment} selection={selection}"
                    ),
                )?;
                Action::Reserve(prepared)
            }
            "member-use-begin" | "member-use-close" => {
                registry::UseNonce::parse(&operation)?;
                let registration = registration(&mut fields)?;
                Action::Use(if action == "member-use-begin" {
                    registry::UseAction::Begin { registration }
                } else {
                    registry::UseAction::Close { registration }
                })
            }
            "member-use-end" => {
                registry::UseNonce::parse(&operation)?;
                let capability = fields.remove("capability")?;
                if !hex(capability, 64) {
                    return None;
                }
                Action::Use(registry::UseAction::End {
                    capability: capability.into(),
                })
            }
            "member-open" => Action::Open,
            "member-control-begin" => Action::ControlBegin,
            "member-control-ack" => {
                let nonce = fields.remove("nonce")?;
                if !hex(nonce, 32) {
                    return None;
                }
                Action::ControlAck {
                    nonce: nonce.into(),
                }
            }
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
            "member-retire" | "member-retire-owned" => {
                let value = fields.remove("registration")?;
                let registration = if value == "none" {
                    None
                } else if hex(value, 32) {
                    Some(value.into())
                } else {
                    return None;
                };
                if action == "member-retire-owned" {
                    let preload_operation = fields.remove("preload_operation")?;
                    if !hex(preload_operation, 32) {
                        return None;
                    }
                    Action::RetireOwned {
                        preload_operation: preload_operation.into(),
                        registration,
                    }
                } else {
                    Action::Retire { registration }
                }
            }
            _ => return None,
        };
        fields.is_empty().then_some(Self {
            request,
            sequence,
            operation,
            preload_sequence,
            member,
            action,
        })
    }
    fn owned_preload_sequence(&self) -> Result<registry::PreloadSequence, registry::Error> {
        self.preload_sequence.ok_or(registry::Error::Denied)
    }
    fn cleanup(&self) -> bool {
        // Target READY owns its original PRELOAD scope before scheduling; it
        // uses bounded completion capacity, never ordinary work admission.
        matches!(
            &self.action,
            Action::Retire { .. }
                | Action::Resource(_)
                | Action::Context(context_control::Action::Ready { .. })
                | Action::Use(registry::UseAction::End { .. } | registry::UseAction::Close { .. })
                | Action::CloseTargetOpen { .. }
                | Action::TargetReady { .. }
                | Action::RetireOwned { .. }
        )
    }
    fn name(&self) -> &'static str {
        match self.action {
            Action::Peer => "member-peer",
            Action::Context(ref action) => action.name(),
            Action::Rebind(_) => "member-work-rebind",
            Action::Resource(ref action) => action.name(),
            Action::OpenDerived { .. } => "member-open",
            Action::Use(_) => "member-use",
            Action::Reserve(_) => "member-reserved",
            Action::Reservation => "member-reserved",
            Action::TargetContext => "member-target-context",
            Action::TargetReady { ref report, .. } => report.name(),
            Action::OpenTarget { .. } => "member-open-target",
            Action::PreloadTarget { .. } => "member-preload-target",
            Action::CloseTargetOpen { .. } => "member-target-open-close",
            Action::Seal { .. } => "member-sealed",
            Action::Install { .. } => "member-installed",
            Action::Census { .. } => "member-census",
            Action::Quiesce { .. } => "member-quiescent",
            Action::Open => "member-open",
            Action::ControlBegin => "member-control-begin",
            Action::ControlAck { .. } => "member-control-ack",
            Action::Preload => "member-preload",
            Action::Ready { .. } => "member-ready",
            Action::ReaderReady { .. } => "member-reader-ready",
            Action::Unavailable { .. } => "member-unavailable",
            Action::Unrelated { .. } => "member-unrelated",
            Action::Retire { .. } => "member-retire",
            Action::RetireOwned { .. } => "member-retire-owned",
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
fn identify(ctx: &crate::SessionCtx) -> Result<Caller, registry::Error> {
    identify_for_session(ctx.self_id.as_str())
}
fn identify_for_session(sid: &str) -> Result<Caller, registry::Error> {
    let pid = crate::control::serving_peer_pid().ok_or(registry::Error::Denied)?;
    let process = Process::observe(pid).ok_or(registry::Error::Unavailable)?;
    let caller = slot::Caller {
        sid: sid.into(),
        pid,
        birth: process.birth,
    };
    let start = restart_stamp::kernel_start(pid).ok_or(registry::Error::Unavailable)?;
    if Process::observe(pid).as_ref() != Some(&process) {
        return Err(registry::Error::Stale);
    }
    Ok(Caller {
        caller,
        process,
        start,
    })
}
fn claim(
    ctx: &crate::SessionCtx,
    master: i32,
    registry: &registry::Registry,
) -> Result<(Caller, registry::Peer), registry::Error> {
    let caller = identify(ctx)?;
    let args =
        atpkg::caller_shell::process_args(caller.caller.pid).ok_or(registry::Error::Unavailable)?;
    let peer = if let Some(token) = args.env_var(registry::LAUNCH_ENV) {
        registry.reserved_peer(&caller.caller, token)?
    } else {
        if master < 0 {
            return Err(registry::Error::Denied);
        }
        // SAFETY: borrow the actual session PTY; no descriptor is transferred.
        registry::Peer::foreground(&caller.caller, unsafe { BorrowedFd::borrow_raw(master) })?
    };
    if Process::observe(caller.caller.pid).as_ref() != Some(&caller.process) {
        return Err(registry::Error::Stale);
    }
    Ok((caller, peer))
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
fn completed_envelope(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    member: Option<&str>,
) -> aterm_json::Value {
    let mut value = envelope(request, caller, epoch, member);
    if let Some(sequence) = request.preload_sequence
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "preload_sequence".into(),
            aterm_json::json!(sequence.encoded()),
        );
    }
    value
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
    scope: &account_work::Scope,
) -> Result<String, registry::Error> {
    let mut value = completed_envelope(request, caller, epoch, request.member.as_deref());
    let member = request.member.as_deref().unwrap_or("");
    match &request.action {
        Action::Peer
        | Action::Context(_)
        | Action::Rebind(_)
        | Action::Resource(_)
        | Action::Retire { .. }
        | Action::OpenDerived { .. }
        | Action::Use(_)
        | Action::OpenTarget { .. }
        | Action::PreloadTarget { .. }
        | Action::CloseTargetOpen { .. }
        | Action::TargetReady { .. }
        | Action::RetireOwned { .. } => {
            return Err(registry::Error::Denied);
        }
        Action::TargetContext => {
            let installed = registry.installed_target(&peer, &request.operation, None)?;
            let object = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
            object.insert("context".into(), aterm_json::json!(installed.context()?));
            object.insert(
                "assignment_generation".into(),
                aterm_json::json!(installed.assignment_generation()?),
            );
            object.insert(
                "target_ref".into(),
                aterm_json::json!(installed.target_ref()?),
            );
        }
        Action::Seal { capability } => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            let reservation = registry.resume_reservation(&peer, &request.operation, capability)?;
            let sealed = reservation.seal(&env, &peer)?;
            if !sealed.current() {
                return Err(registry::Error::Stale);
            }
        }
        Action::Install {
            capability,
            boundary,
        } => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            let reservation = registry.resume_reservation(&peer, &request.operation, capability)?;
            // Obtain the actual registry-owned exclusion again. A prior wire
            // acknowledgement is correlation only and cannot stand in for it.
            let sealed = reservation.seal(&env, &peer)?;
            let installed = reservation.install_target(&env, &peer, &sealed, boundary)?;
            let object = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
            object.insert("context".into(), aterm_json::json!(installed.context()?));
            object.insert(
                "assignment_generation".into(),
                aterm_json::json!(installed.assignment_generation()?),
            );
            object.insert(
                "target_ref".into(),
                aterm_json::json!(installed.target_ref()?),
            );
        }
        Action::Reservation => {
            let reservation = registry
                .reservation_for_peer(&peer)?
                .ok_or(registry::Error::Unavailable)?;
            if reservation.operation(&peer)? != request.operation {
                return Err(registry::Error::Stale);
            }
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert(
                    "capability".into(),
                    aterm_json::json!(reservation.capability(&peer)?),
                );
        }
        Action::Quiesce {
            capability,
            registration,
            generation,
            digest,
            auth,
        } => {
            let reservation = registry.resume_reservation(&peer, &request.operation, capability)?;
            reservation.quiesce(
                &peer,
                connection,
                registry::Quiescence {
                    member,
                    registration,
                    credential_generation: *generation,
                    digest,
                    auth: *auth,
                },
            )?;
        }
        Action::Census { capability, cursor } => {
            let reservation = registry.resume_reservation(&peer, &request.operation, capability)?;
            let cursor = cursor
                .as_ref()
                .map(|(revision, after)| registry::CensusCursor { revision, after });
            let page = reservation.census_page(&peer, cursor)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("census".into(), census_control::value(page));
        }
        Action::Reserve(prepared) => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            let source = registry.ready_source(&caller.caller, member)?;
            let reservation = registry.reserve_prepared(&env, &source, prepared, scope)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert(
                    "capability".into(),
                    aterm_json::json!(reservation.capability(&peer)?),
                );
        }
        Action::Open => {
            let allocation = registry.open_member(&peer, connection, &request.operation)?;
            value
                .as_object_mut()
                .ok_or(registry::Error::Unavailable)?
                .insert("member".into(), aterm_json::json!(allocation.member));
        }
        Action::ControlBegin => {
            registry.begin_owned_control(&peer, connection, member, &request.operation)?;
        }
        Action::ControlAck { nonce } => {
            registry.acknowledge_owned_control(
                &peer,
                connection,
                member,
                &request.operation,
                nonce,
            )?;
        }
        Action::Preload => {
            let env = super::codex_account_env().map_err(|_| registry::Error::Unavailable)?;
            let preload = registry.preload_managed(
                &env,
                peer,
                member,
                &request.operation,
                connection,
                request.owned_preload_sequence()?,
            )?;
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
                request.owned_preload_sequence()?,
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
                request.owned_preload_sequence()?,
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
                request.owned_preload_sequence()?,
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
    prepared_use: Mutex<Option<registry::UseCommand>>,
    use_accounted: AtomicBool,
    prepared_target: Mutex<Option<registry::TargetConstruction>>,
    prepared_target_cleanup: Mutex<Option<registry::TargetOpenCleanup>>,
    prepared_ready: Mutex<Option<Box<registry::TargetReadyCommand>>>,
    prepared_member_retirement: Mutex<Option<Box<registry::OwnedMemberRetirement>>>,
    prepared_context: Mutex<Option<context_control::Command>>,
    prepared_resource: Mutex<Option<resource_control::Command>>,
    resource_accounted: AtomicBool,
    context_accounted: AtomicBool,
    target_accounted: AtomicBool,
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
    cleanup_running: Arc<AtomicUsize>,
    retiring: Vec<Arc<Job>>,
}
#[derive(Debug, PartialEq, Eq)]
enum AdmissionError {
    Changed,
    Stale,
    Capacity,
}
impl Jobs {
    /// Called only after the exact native member/PRELOAD fence was accepted.
    /// Return all captures; their Drop must occur outside Jobs and State.
    fn fence_member_preload(
        &mut self,
        caller: &Caller,
        member: &str,
        preload: &str,
    ) -> (
        Vec<registry::TargetConstruction>,
        Vec<registry::TargetReadyCommand>,
        Vec<context_control::Command>,
    ) {
        self.fence_member_preload_at(caller, member, preload, || {})
    }
    fn fence_member_preload_at(
        &mut self,
        caller: &Caller,
        member: &str,
        preload: &str,
        before_context: impl Fn(),
    ) -> (
        Vec<registry::TargetConstruction>,
        Vec<registry::TargetReadyCommand>,
        Vec<context_control::Command>,
    ) {
        let mut contextual = Vec::new();
        let mut targets = Vec::new();
        let mut ready = Vec::new();
        for connection in self.connections.values() {
            for job in connection.active.values() {
                if &job.caller != caller {
                    continue;
                }
                let mut target = job
                    .prepared_target
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let mut completion = job.prepared_ready.lock().unwrap_or_else(|e| e.into_inner());
                let captured_member = target
                    .as_ref()
                    .is_some_and(|command| command.member() == member);
                let request_matches = job.request.member.as_deref() == Some(member)
                    && job.request.operation == preload
                    && matches!(
                        &job.request.action,
                        Action::Preload
                            | Action::PreloadTarget { .. }
                            | Action::Ready { .. }
                            | Action::ReaderReady { .. }
                            | Action::Unavailable { .. }
                            | Action::TargetReady { .. }
                            | Action::Context(_)
                    );
                if !captured_member && !request_matches {
                    continue;
                }
                // Same ordering as contextual prepare: command custody before
                // progress. Both remain locked through fencing/taking the capture.
                before_context();
                let mut contextual_command = job
                    .prepared_context
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let mut progress = job.progress.lock().unwrap_or_else(|e| e.into_inner());
                if matches!(*progress, Progress::Waiting) {
                    *progress = Progress::Complete(Err(registry::Error::Stale));
                    if let Some(command) = target.take() {
                        targets.push(command);
                    }
                    if let Some(command) = completion.take() {
                        ready.push(*command);
                    }
                    if let Some(command) = contextual_command.take() {
                        contextual.push(command);
                    }
                }
            }
        }
        (targets, ready, contextual)
    }
    /// Only after native admits the exact release-only OPEN cleanup. Return
    /// captures to the caller so their Drop runs outside Jobs and native State.
    fn fence_target_open(
        &mut self,
        caller: &Caller,
        open: &str,
    ) -> Vec<registry::TargetConstruction> {
        let mut released = Vec::new();
        for connection in self.connections.values() {
            for job in connection.active.values() {
                if &job.caller != caller
                    || job.request.operation != open
                    || !matches!(job.request.action, Action::OpenTarget { .. })
                {
                    continue;
                }
                let mut command = job
                    .prepared_target
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let mut progress = job.progress.lock().unwrap_or_else(|e| e.into_inner());
                if matches!(*progress, Progress::Waiting) {
                    *progress = Progress::Complete(Err(registry::Error::Stale));
                    if let Some(command) = command.take() {
                        released.push(command);
                    }
                }
            }
        }
        released
    }
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
                if let Some((_, job)) = connection.recent.pop_front() {
                    self.retiring.push(job);
                }
            }
        }
    }
    fn reserve(&self, job: Arc<Job>) -> (Arc<Job>, bool) {
        if matches!(
            &job.request.action,
            Action::Peer
                | Action::Context(_)
                | Action::Rebind(_)
                | Action::Resource(_)
                | Action::Retire { .. }
                | Action::OpenDerived { .. }
                | Action::Use(_)
                | Action::OpenTarget { .. }
                | Action::PreloadTarget { .. }
                | Action::CloseTargetOpen { .. }
                | Action::TargetReady { .. }
                | Action::RetireOwned { .. }
        ) {
            return (job, false);
        }
        self.reserve_ready(job)
    }
    fn reserve_ready(&self, job: Arc<Job>) -> (Arc<Job>, bool) {
        if matches!(&job.request.action, Action::Resource(_))
            && !job.resource_accounted.load(Ordering::Acquire)
        {
            return (job, false);
        }
        if matches!(&job.request.action, Action::Context(_))
            && !job.context_accounted.load(Ordering::Acquire)
        {
            return (job, false);
        }
        if matches!(&job.request.action, Action::Use(_))
            && !job.use_accounted.load(Ordering::Acquire)
        {
            return (job, false);
        }
        if matches!(
            &job.request.action,
            Action::OpenTarget { .. }
                | Action::PreloadTarget { .. }
                | Action::CloseTargetOpen { .. }
                | Action::TargetReady { .. }
                | Action::RetireOwned { .. }
        ) && !job.target_accounted.load(Ordering::Acquire)
        {
            return (job, false);
        }
        let (running, limit) = if job.request.cleanup() {
            (&self.cleanup_running, MAX_CLEANUP_RUNNING)
        } else {
            (&self.running, MAX_RUNNING)
        };
        let room = running.load(Ordering::Acquire) < limit;
        let start = {
            let mut state = job.progress.lock().unwrap_or_else(|p| p.into_inner());
            if room && matches!(*state, Progress::Waiting) {
                running.fetch_add(1, Ordering::AcqRel);
                *state = Progress::Running {
                    _permit: RunningPermit(Arc::clone(running)),
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
            .map(|c| {
                c.active
                    .values()
                    .filter(|job| job.request.cleanup() == request.cleanup())
                    .count()
            })
            .sum::<usize>()
            >= if request.cleanup() {
                MAX_CLEANUP_PENDING
            } else {
                MAX_PENDING
            }
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
            prepared_use: Mutex::new(None),
            use_accounted: AtomicBool::new(false),
            prepared_target: Mutex::new(None),
            prepared_target_cleanup: Mutex::new(None),
            prepared_ready: Mutex::new(None),
            prepared_member_retirement: Mutex::new(None),
            prepared_context: Mutex::new(None),
            prepared_resource: Mutex::new(None),
            resource_accounted: AtomicBool::new(false),
            context_accounted: AtomicBool::new(false),
            target_accounted: AtomicBool::new(false),
        });
        connection.active.insert(sequence.counter, Arc::clone(&job));
        Ok(self.reserve(job))
    }
    fn closed(&mut self, id: u64) -> Option<ConnectionJobs> {
        self.connections.remove(&id)
    }
}
fn jobs() -> &'static Mutex<Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

fn admit_job(
    id: u64,
    caller: &Caller,
    request: &Request,
) -> Result<(Arc<Job>, bool), AdmissionError> {
    let (result, retired) = {
        let mut state = jobs().lock().unwrap_or_else(|e| e.into_inner());
        let result = state.admit(id, caller, request);
        let retired = std::mem::take(&mut state.retiring);
        (result, retired)
    };
    drop(retired);
    result
}
/// Called only after the real connection token has synchronously become false.
/// Workers retain that original token and cannot reinsert transport state.
pub(super) fn connection_closed(id: u64) {
    let (connection, retired) = {
        let mut state = jobs().lock().unwrap_or_else(|e| e.into_inner());
        (state.closed(id), std::mem::take(&mut state.retiring))
    };
    drop((connection, retired));
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
    if matches!(&request.action, Action::Retire { .. }) {
        return plain_retirement_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::Resource(_)) {
        return resource_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::Rebind(_)) {
        return rebind_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::Context(_)) {
        return context_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::OpenDerived { .. }) {
        return derived_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::Peer) {
        return serve_peer(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::TargetReady { .. }) {
        return ready_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::RetireOwned { .. }) {
        return retirement_control::serve(ctx, request, registry, id);
    }
    if matches!(&request.action, Action::Use(_)) {
        return use_control::serve(ctx, request, registry, id);
    }
    if matches!(
        &request.action,
        Action::OpenTarget { .. } | Action::PreloadTarget { .. }
    ) {
        return target_control::serve(ctx, master, request, registry, id);
    }
    if matches!(&request.action, Action::CloseTargetOpen { .. }) {
        return target_control::serve_cleanup(ctx, request, registry, id);
    }
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
    let (job, start) = match admit_job(id, &caller, &request) {
        Ok(admitted) => admitted,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return "ERR codex-auth busy\n".into(),
    };
    if start {
        if let Some(scope) = lifetime.enter() {
            let reservation_scope = scope.clone();
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
                            &reservation_scope,
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

/// Metadata on this exact authenticated transport, before the client may send
/// its first ordinary OPEN. This creates no member, Scope, source or Ready.
fn serve_peer(
    ctx: &crate::SessionCtx,
    request: Request,
    registry: &registry::Registry,
    id: u64,
) -> String {
    serve_peer_identified(request, registry, id, || identify(ctx))
}
fn serve_peer_identified(
    request: Request,
    registry: &registry::Registry,
    id: u64,
    identify_peer: impl Fn() -> Result<Caller, registry::Error>,
) -> String {
    let caller = match identify_peer() {
        Ok(caller) => caller,
        Err(why) => return use_control::error(why),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let connection = match registry.connection(id) {
        Ok(connection) => connection,
        Err(why) => return use_control::error(why),
    };
    let (job, _) = match admit_job(id, &caller, &request) {
        Ok(job) => job,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if use_control::cached(&job).is_none() {
        job.finish(
            frame("member-peer", &envelope(&request, &caller, epoch, None))
                .ok_or(registry::Error::Unavailable),
        );
    }
    if !connection.is_live() || identify_peer().as_ref() != Ok(&caller) {
        return use_control::error(registry::Error::Stale);
    }
    match use_control::cached(&job) {
        Some(Ok(reply)) => reply,
        Some(Err(why)) => use_control::error(why),
        None => UNAVAILABLE.into(),
    }
}

#[cfg(test)]
#[path = "codex_source_control_tests.rs"]
mod tests;
