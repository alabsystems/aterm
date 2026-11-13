// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only operational observations. A configured unit is not running-image proof,
//! and a recorded steward is not evidence that its service is reachable.

use crate::{Error, client, service};
use aterm_json::{Value, json};
use client::Paths;

fn observed<T>(result: crate::Result<T>, value: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(item) => json!({"state": "known", "value": value(item)}),
        Err(error) => json!({"state": "unknown", "reason": error.to_string()}),
    }
}

fn service_observation(state: crate::Result<service::State>) -> Value {
    observed(state, |state| match state {
        service::State::Absent => json!({"installed": false}),
        service::State::Installed { pid, unit } => json!({
            "installed": true,
            "pid": pid,
            "unit": unit.to_string_lossy().into_owned(),
            "configured_program": service::unit_program(&unit).map(|p| p.to_string_lossy().into_owned()),
            "running_image_verified": false,
        }),
    })
}

/// The membership `status` reads, as JSON: a Mac its vault let go is `dropped` where no act took
/// it out (`client::dropped`), `revoked` otherwise.
fn membership(paths: &Paths, status: &client::Status) -> Value {
    use client::Membership;
    match &status.membership {
        Membership::Unconfigured => json!({"state": "unconfigured"}),
        Membership::Pending => {
            json!({"state": "pending", "approve_command": status.approve_command()})
        }
        Membership::In { seq, heard } => json!({
            "state": "in", "sequence": seq, "local_processing_marker": heard,
        }),
        Membership::Revoked { seq } => {
            let state = if client::dropped(paths) {
                "dropped"
            } else {
                "revoked"
            };
            json!({"state": state, "sequence": seq})
        }
        Membership::Plain => json!({"state": "plain"}),
    }
}

/// Read local, value-free observations. This does not fetch a queue, prove a
/// credential afresh, execute a configured program, or contact an authority.
pub fn snapshot(paths: &Paths) -> Value {
    snapshot_until(
        paths,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
}

/// Read local observations with one deadline for all service-manager children.
pub fn snapshot_until(paths: &Paths, until: std::time::Instant) -> Value {
    let mut value = metadata(paths);
    if let Some(object) = value.as_object_mut() {
        object.insert("services".into(), services_until(paths, until));
    }
    value
}

/// Observe local vault metadata without spawning service-manager children.
pub fn metadata(paths: &Paths) -> Value {
    let status = observed(client::status(paths), |status| {
        json!({
            "vault": status.config.as_ref().map(|c| &c.name),
            "host": status.config.as_ref().map(|c| &c.host),
            "configured_signer": status.config.as_ref().and_then(|c| c.signer.as_ref()).map(|key| crate::b64::encode_padded(key)),
            "join_code": status.machine.as_ref().map(|(_, code)| code),
            "membership": membership(paths, &status),
            "materialized_names": status.names,
        })
    });
    let admin = observed(client::can_administer(paths), |available| json!(available));
    // Who writes this Mac's vault, read at once: a busy state is unknown, never "no log".
    let route = client::write_route(paths, std::time::Instant::now()).and_then(|route| {
        if route == client::WriteRoute::Busy {
            return Err(Error::new("this Mac's vault state is busy"));
        }
        Ok(route)
    });
    let verbs = match &route {
        Ok(route) => json!({"state": "known", "value": crate::cli::admin_verbs(*route)}),
        Err(error) => json!({"state": "unknown", "reason": error.to_string()}),
    };
    let writer = observed(route, |route| {
        json!(match route {
            client::WriteRoute::Seated => "member",
            client::WriteRoute::NoLog => "admin",
            client::WriteRoute::NotSeated => "none",
            client::WriteRoute::Busy => "unknown",
        })
    });
    let steward = observed(client::swarm_lease_holder(paths), |holder| {
        json!({
            "recorded_holder": holder, "reachability": "unobserved",
        })
    });
    let signing = observed(client::release::signing_status(paths), |status| {
        use client::release::SigningStatus;
        match status {
            SigningStatus::Ready(detail) => json!({"state": "ready", "detail": detail}),
            SigningStatus::Pending(reason) => json!({"state": "pending", "reason": reason}),
            SigningStatus::NotApplicable(reason) => {
                json!({"state": "not_applicable", "reason": reason})
            }
        }
    });
    json!({
        "schema": 1,
        "kind": "vault-operational-status",
        "vault": status,
        "capabilities": {
            "vault_admin": admin, "vault_writer": writer, "admin_verbs": verbs,
            "release_signing": signing,
        },
        "authority": steward,
        "operations": {
            "inspect_pending": "aterm secrets requests --json",
            "sync_state": "aterm secrets sync --diagnostic",
            "repair_installed_services": "aterm secrets repair --json",
            "inspect_remote_authority": "aterm ctl --host <candidate> vault status",
            "approve_remote": "aterm ctl --host <authority> vault approve <join-code> <32-lowercase-hex-nonce>",
        },
    })
}

/// Observe configured vault services within one shared child-query deadline.
pub fn services_until(paths: &Paths, until: std::time::Instant) -> Value {
    let subscriber = service_observation(service::state_until(
        &paths.home,
        service::Job::Subscriber,
        until,
    ));
    let user_server = service::state_until(&paths.home, service::Job::Server, until);
    let server = if matches!(user_server, Ok(service::State::Absent)) {
        service_observation(service::daemon_state_for_home_until(&paths.home, until))
    } else {
        service_observation(user_server)
    };
    json!({"subscriber":subscriber,"server":server})
}

/// Serialize [`snapshot`] for the CLI.
/// # Errors
/// The JSON serialization fails.
pub fn read(paths: &Paths) -> crate::Result<String> {
    aterm_json::to_string(&snapshot(paths))
        .map_err(|error| Error::new(format!("cannot encode operational status: {error}")))
}

#[cfg(test)]
#[path = "operational_tests.rs"]
mod tests;
