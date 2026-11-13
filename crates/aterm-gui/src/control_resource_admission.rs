// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact local-owner ingress. Authorization delegates no allocation or launch.
use crate::{
    control::{Principal, Scope},
    session_store::{SessionState, Store},
};
use aterm_ctl::resource_admission_transport::{self as transport, Peer};
use aterm_session::{LaunchNonce, SessionId};
use aterm_uds::CtlStream;
use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

const HANDOFF: Duration = Duration::from_secs(10);
const MAX_COMMAND: usize = 128;
const MAX_REPLY: usize = 65536;

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Capabilities,
    Attach,
    AttachAccounted,
    AttachAccountedExpected,
    Inspect(&'a str),
    Enroll(&'a str),
}
fn canonical_session(sid: &str) -> bool {
    sid.strip_prefix("s-").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 62
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn parse(rest: &str) -> Option<Command<'_>> {
    if rest.len() > MAX_COMMAND {
        return None;
    }
    let words: Vec<_> = rest.split_whitespace().collect();
    match words.as_slice() {
        ["capabilities"] => Some(Command::Capabilities),
        ["attach"] => Some(Command::Attach),
        ["attach-accounted-v1"] => Some(Command::AttachAccounted),
        ["attach-accounted-original-store-v1"] => Some(Command::AttachAccountedExpected),
        ["enroll", field] => {
            let nonce = field.strip_prefix("nonce=")?;
            aterm_codec::hex::is_lower(nonce, 64).then_some(Command::Enroll(nonce))
        }
        ["inspect", field] => {
            let sid = field.strip_prefix("session=")?;
            canonical_session(sid).then_some(Command::Inspect(sid))
        }
        _ => None,
    }
}
fn allowed(scope: Scope, principal: Principal, selected: bool, remote: bool) -> bool {
    scope == Scope::Owner && principal == Principal::Instance && !selected && !remote
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "resource admission session authorization refused",
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SessionFence {
    local_id: u64,
    nonce: LaunchNonce,
    shell: aterm_pty::ShellIdentity,
}
fn capture(store: &Store, sid: &str) -> Option<SessionFence> {
    if !canonical_session(sid) {
        return None;
    }
    // A busy or poisoned registry is unknown authorization, never permission
    // to wait past the socket handoff deadline while holding a control lane.
    let current = store.try_read().ok()?;
    let handle = current.by_sid(&SessionId::new(sid.to_owned()))?;
    if handle.state != SessionState::Alive
        || handle.sid.as_str() != sid
        || handle.shell_identity.pid <= 1
        || handle.shell_identity.birth.is_none_or(|birth| birth == 0)
    {
        return None;
    }
    Some(SessionFence {
        local_id: handle.local_id,
        nonce: handle.nonce,
        shell: handle.shell_identity,
    })
}
fn fence_current(store: &Store, sid: &str, fence: SessionFence, deadline: Instant) -> bool {
    Instant::now() < deadline && capture(store, sid) == Some(fence) && fence.shell.verify()
}
fn session_matches(
    store: &Store,
    sid: &str,
    peer: Peer,
    fence: SessionFence,
    deadline: Instant,
) -> bool {
    if !fence_current(store, sid, fence, deadline) {
        return false;
    }
    let Some(birth) = fence.shell.birth else {
        return false;
    };
    let Ok(pid) = u32::try_from(fence.shell.pid) else {
        return false;
    };
    aterm_uds::process::stable_descendant(
        peer.pid,
        aterm_uds::process::Birth(peer.birth),
        pid,
        aterm_uds::process::Birth(birth),
        deadline,
    ) && fence_current(store, sid, fence, deadline)
}
fn session_authorized(
    store: &Store,
    sid: &str,
    peer: Peer,
    deadline: Instant,
) -> Option<SessionFence> {
    let fence = capture(store, sid)?;
    session_matches(store, sid, peer, fence, deadline).then_some(fence)
}
fn json_reply(kind: &str, value: aterm_json::Value) -> io::Result<String> {
    let body = value.to_string();
    if body.len() > MAX_REPLY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized resource admission observation",
        ));
    }
    Ok(format!("OK resource-admission-v3 {kind}\n{body}\n"))
}

/// Invoked before drive promotion, forwarding or ordinary dispatch. The host
/// alone owns replay authorization, resource allocation, lifecycle and launch.
pub(crate) fn serve(
    stream: &CtlStream,
    rest: &str,
    scope: Scope,
    selected: bool,
    store: &Store,
) -> String {
    if !allowed(
        scope,
        crate::control::serving_principal(),
        selected,
        crate::fabric::serving_driver().is_some(),
    ) {
        return "ERR resource-admission-v3 refused local-owner-only\n".into();
    }
    let Some(command) = parse(rest) else {
        return "ERR resource-admission-v3 invalid request\n".into();
    };
    let deadline = Instant::now() + HANDOFF;
    let result = (|| {
        let peer = transport::local_peer(stream)?;
        match command {
            Command::Enroll(nonce) => {
                // This branch is reached only through the exact Instance owner
                // gate above. A live Store and this actual listener distinguish
                // GUI authority from another CLI using the same executable.
                drop(store.try_read().map_err(|_| denied())?);
                let address = stream.local_addr()?;
                let endpoint = address.as_pathname().ok_or_else(denied)?;
                let nonce = aterm_resource_host::answer_enrollment(peer, nonce, endpoint)?;
                drop(store.try_read().map_err(|_| denied())?);
                json_reply("enroll", aterm_json::json!({"version":3,"nonce":nonce}))
            }
            Command::Capabilities => {
                let address = stream.local_addr()?;
                let endpoint = address.as_pathname().ok_or_else(denied)?;
                json_reply(
                    "capabilities",
                    crate::resource_admission_host::capabilities_at(endpoint),
                )
            }
            Command::Inspect(sid) => {
                // A host-launched root need not descend from the tab shell.
                // Its actual original launch/custody binding belongs to the
                // host, which accepts only this kernel-authenticated peer.
                let fence = capture(store, sid)
                    .filter(|fence| fence_current(store, sid, *fence, deadline))
                    .ok_or_else(denied)?;
                let address = stream.local_addr()?;
                let endpoint = address.as_pathname().ok_or_else(denied)?;
                let observation = crate::resource_admission_host::inspect_at(endpoint, peer, sid)?;
                if transport::local_peer(stream)? != peer
                    || !fence_current(store, sid, fence, deadline)
                {
                    return Err(denied());
                }
                json_reply("inspect", observation)
            }
            Command::Attach | Command::AttachAccounted | Command::AttachAccountedExpected => {
                let accounted = !matches!(command, Command::Attach);
                let admitted = Cell::new(None);
                let authorize = |request: &aterm_types::resource_admission::Submission,
                                 peer: Peer| {
                    let Some(fence) =
                        session_authorized(store, &request.binding.session, peer, deadline)
                    else {
                        return false;
                    };
                    if !crate::resource_admission_host::authorize(request, peer) {
                        return false;
                    }
                    admitted.set(Some(fence));
                    true
                };
                let submit = |authorized: transport::AuthorizedSubmission| {
                    let fence = admitted.get().ok_or_else(denied)?;
                    if !session_matches(
                        store,
                        &authorized.request.binding.session,
                        authorized.peer,
                        fence,
                        deadline,
                    ) {
                        return Err(denied());
                    }
                    if accounted {
                        crate::resource_admission_host::submit_accounted(authorized)
                    } else {
                        crate::resource_admission_host::submit(authorized)
                    }
                };
                if matches!(command, Command::AttachAccountedExpected) {
                    let address = stream.local_addr()?;
                    let endpoint = address.as_pathname().ok_or_else(denied)?;
                    crate::resource_admission_attach::attach_accounted_expected(
                        stream,
                        true,
                        deadline,
                        authorize,
                        |expected| {
                            crate::resource_admission_host::matches_original_store_at(
                                endpoint, expected,
                            )
                        },
                        |authorized, expected| {
                            let fence = admitted.get().ok_or_else(denied)?;
                            if !session_matches(
                                store,
                                &authorized.request.binding.session,
                                authorized.peer,
                                fence,
                                deadline,
                            ) {
                                return Err(denied());
                            }
                            crate::resource_admission_host::submit_accounted_expected(
                                authorized, expected,
                            )
                        },
                    )
                } else if accounted {
                    crate::resource_admission_attach::attach_accounted(
                        stream, true, deadline, authorize, submit,
                    )
                } else {
                    crate::resource_admission_attach::attach(
                        stream, true, deadline, authorize, submit,
                    )
                }?;
                // The attachment ACK was written directly. No second status,
                // especially no claimed Started event, belongs on this socket.
                Ok(String::new())
            }
        }
    })();
    if matches!(
        command,
        Command::Attach | Command::AttachAccounted | Command::AttachAccountedExpected
    ) {
        // A final error reply cannot buy more time after the handoff expired.
        // Closing without another line preserves ambiguous-delivery semantics.
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return String::new();
        };
        if remaining.is_zero()
            || stream.set_nonblocking(false).is_err()
            || stream.set_write_timeout(Some(remaining)).is_err()
        {
            return String::new();
        }
    }
    result.unwrap_or_else(|_| "ERR resource-admission-v3 incomplete\n".into())
}

#[cfg(test)]
#[path = "control_resource_admission_tests.rs"]
mod tests;
