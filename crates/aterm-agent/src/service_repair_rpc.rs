// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded service-repair messages carried by an authenticated native fleet Drive.
//! The binary supplies the local implementation; this transport never accepts a
//! shell command, an environment, or a remote filesystem path as an operation.

use aterm_json::Value;
#[cfg(any(unix, test))]
use aterm_json::json;
use std::sync::OnceLock;
use std::time::Duration;

/// Native control verb, intercepted by the node before any window relay.
pub const VERB: &str = "service-repair";
/// Maximum compact request JSON, below the control protocol's line limit.
pub const MAX_REQUEST: usize = 48 * 1024;
/// Maximum complete reply, including its signed result. The core's 256 KiB
/// canonical request plus a 16 KiB diagnostic escaping to at most 96 KiB and
/// the outcome/signature wrappers fit below 512 KiB, including local recoveries.
pub const MAX_REPLY: usize = 512 * 1024;
/// Total dial, request and response budget. An uncertain activation is never retried.
pub const WITHIN: Duration = Duration::from_secs(240);
/// Local operation implementation; `caller` is the authenticated Drive node.
pub type Dispatcher = fn(&str, &Value, &str, &str) -> Result<Value, String>;
static DISPATCH: OnceLock<Dispatcher> = OnceLock::new();

/// Coordinator startup recovery. The implementation must schedule its work in
/// the background; registration and ordinary CLI startup perform no recovery.
#[cfg(unix)]
pub type StartupRecovery = fn(aterm_secrets::client::Paths, std::path::PathBuf);
#[cfg(unix)]
static STARTUP_RECOVERY: OnceLock<StartupRecovery> = OnceLock::new();
#[cfg(unix)]
static RECOVERY_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Register the binary's asynchronous recovery callback without invoking it.
/// # Errors
/// A startup callback was already registered.
#[cfg(unix)]
pub fn install_startup_recovery(recover: StartupRecovery) -> Result<(), String> {
    register_recovery(&STARTUP_RECOVERY, recover)
}

#[cfg(unix)]
fn register_recovery(
    slot: &OnceLock<StartupRecovery>,
    recover: StartupRecovery,
) -> Result<(), String> {
    slot.set(recover)
        .map_err(|_| "service repair startup recovery is already installed".into())
}

/// Register the binary's local implementation before starting any listeners.
/// # Errors
/// A different implementation was already registered.
pub fn install_dispatcher(dispatch: Dispatcher) -> Result<(), String> {
    DISPATCH
        .set(dispatch)
        .map_err(|_| "service repair dispatcher is already installed".into())
}

/// A validated operation and its JSON payload.
pub struct Request {
    /// One of inspect, prepare, activate or recover.
    pub operation: String,
    /// Operation-specific, bounded input with no transport filesystem pointers.
    pub payload: Value,
}

fn exact_fields(value: &Value, fields: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("service repair payload must be an object")?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err("service repair payload has missing or unknown fields".into());
    }
    Ok(())
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("service repair requires {field}"))
}
fn job(payload: &Value) -> Result<(), String> {
    match text(payload, "job")? {
        "server" | "subscriber" => Ok(()),
        _ => Err("service repair job must be server or subscriber".into()),
    }
}
fn validate(operation: &str, payload: &Value) -> Result<(), String> {
    match operation {
        "inspect" | "recover" => {
            exact_fields(payload, &["job"])?;
            job(payload)
        }
        "prepare" => {
            exact_fields(payload, &["job", "image", "nonce"])?;
            job(payload)?;
            let image = text(payload, "image")?;
            if image.len() != 64
                || !image
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("service repair image must be a lowercase SHA-256 identity".into());
            }
            let nonce = text(payload, "nonce")?;
            if !(16..=128).contains(&nonce.len())
                || !nonce
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(
                    "service repair nonce must be 16..128 ASCII letters, digits or hyphens".into(),
                );
            }
            Ok(())
        }
        "activate" => {
            exact_fields(payload, &["signed"])?;
            exact_fields(&payload["signed"], &["request", "key", "signature"])?;
            if !payload["signed"]["request"].is_object() {
                return Err("service repair signed request must be an object".into());
            }
            text(&payload["signed"], "key")?;
            text(&payload["signed"], "signature")?;
            Ok(())
        }
        _ => Err("service repair operation must be inspect, prepare, activate or recover".into()),
    }
}

impl Request {
    /// Parse a bounded operation without invoking its local implementation.
    /// # Errors
    /// Invalid JSON, schema, operation, size, unknown fields or malformed input.
    pub fn parse(rest: &str) -> Result<Self, String> {
        if rest.len() > MAX_REQUEST {
            return Err("service repair request exceeds 48 KiB".into());
        }
        let value: Value = aterm_json::from_str(rest).map_err(|e| e.to_string())?;
        exact_fields(&value, &["schema", "op", "payload"])?;
        if value["schema"].as_u64() != Some(1) {
            return Err("unsupported service repair RPC schema".into());
        }
        let operation = text(&value, "op")?.to_owned();
        let payload = value["payload"].clone();
        validate(&operation, &payload)?;
        Ok(Self { operation, payload })
    }
}

#[cfg(any(unix, test))]
fn serve_with(
    caller: &str,
    key: &str,
    rest: &str,
    dispatch: impl FnOnce(&str, &Value, &str, &str) -> Result<Value, String>,
) -> String {
    let answer = (|| {
        if caller.is_empty() || key.is_empty() {
            return Err("service repair requires an authenticated Drive caller and key".into());
        }
        let request = Request::parse(rest)?;
        let result = dispatch(&request.operation, &request.payload, caller, key)?;
        let body =
            aterm_json::to_string(&json!({"schema":1,"op":request.operation,"result":result}))
                .map_err(|e| e.to_string())?;
        if body.len() > MAX_REPLY {
            return Err("service repair result exceeds 512 KiB".into());
        }
        Ok(format!("OK 1\n{body}\n"))
    })();
    answer.unwrap_or_else(|e: String| {
        format!(
            "ERR service-repair: {}\n",
            aterm_codec::tty::one_line(&e, 400)
        )
    })
}

/// Handle one message after the listener admitted the current native Drive grant.
/// The dispatcher rechecks signed membership, grant and request authorization.
#[cfg(unix)]
fn serve(caller: &str, key: &str, rest: &str) -> String {
    serve_with(caller, key, rest, |operation, payload, caller, key| {
        DISPATCH
            .get()
            .ok_or("this image has no service repair dispatcher")?(
            operation, payload, caller, key
        )
    })
}

#[cfg(any(unix, test))]
fn in_context(
    configured: Option<(&std::path::Path, &std::path::Path)>,
    state: &std::path::Path,
    live: impl FnOnce() -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), String>,
    dispatch: impl FnOnce() -> String,
) -> String {
    let Some((home, vault)) = configured else {
        return "ERR service-repair: listener has no configured vault context\n".into();
    };
    match live() {
        Ok((live_home, live_vault, live_state))
            if home == live_home && vault == live_vault && state == live_state =>
        {
            dispatch()
        }
        _ => "ERR service-repair: listener context differs from the live service context\n".into(),
    }
}

#[cfg(any(unix, test))]
fn launch_once(started: &std::sync::atomic::AtomicBool, launch: impl FnOnce()) -> bool {
    if started
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    launch();
    true
}

/// Schedule recovery once from an actual coordinator startup, after its configured
/// context is known. An absent or custom context never reaches the live callback.
#[cfg(unix)]
pub fn start_recovery_in_context(
    configured: Option<&aterm_secrets::client::Paths>,
    state: &std::path::Path,
) -> bool {
    let (Some(recover), Some(paths)) = (STARTUP_RECOVERY.get(), configured) else {
        return false;
    };
    let mut launched = false;
    let _ = in_context(
        Some((paths.home.as_path(), paths.dir.as_path())),
        state,
        || {
            let live = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
            let state = aterm_types::dirs::state_dir().ok_or("no live fleet state directory")?;
            Ok((live.home, live.dir, state))
        },
        || {
            launched = launch_once(&RECOVERY_STARTED, || {
                recover(paths.clone(), state.to_path_buf())
            });
            String::new()
        },
    );
    launched
}

/// Schedule recovery only for a normally bound live GUI control listener.
#[cfg(unix)]
pub fn start_recovery_in_window(socket_dir: &std::path::Path) -> bool {
    if aterm_uds::control_socket_dir().as_deref() != Some(socket_dir) {
        return false;
    }
    let (Ok(paths), Some(state)) = (
        aterm_secrets::client::Paths::live(),
        aterm_types::dirs::state_dir(),
    ) else {
        return false;
    };
    start_recovery_in_context(Some(&paths), &state)
}

/// Dispatch only when the listener's configured vault and state are exactly the
/// context used by the binary's live service implementation. Isolated listeners
/// never fall back to the owner's vault or services.
#[cfg(unix)]
pub fn serve_in_context(
    caller: &str,
    key: &str,
    rest: &str,
    configured: Option<&aterm_secrets::client::Paths>,
    state: &std::path::Path,
) -> String {
    in_context(
        configured.map(|paths| (paths.home.as_path(), paths.dir.as_path())),
        state,
        || {
            let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
            let state = aterm_types::dirs::state_dir().ok_or("no live fleet state directory")?;
            Ok((paths.home, paths.dir, state))
        },
        || serve(caller, key, rest),
    )
}

/// The window control listener must also be the live control socket before its
/// requests may reach the live service implementation.
#[cfg(unix)]
pub fn serve_in_window(
    caller: &str,
    key: &str,
    rest: &str,
    socket_dir: &std::path::Path,
) -> String {
    if aterm_uds::control_socket_dir().as_deref() != Some(socket_dir) {
        return "ERR service-repair: custom window control context cannot repair live services\n"
            .into();
    }
    let (Ok(paths), Some(state)) = (
        aterm_secrets::client::Paths::live(),
        aterm_types::dirs::state_dir(),
    ) else {
        return "ERR service-repair: window has no live service context\n".into();
    };
    serve_in_context(caller, key, rest, Some(&paths), &state)
}

/// Service repair has no native service implementation on this platform.
#[cfg(not(unix))]
pub fn serve_in_window(
    _caller: &str,
    _key: &str,
    _rest: &str,
    _socket_dir: &std::path::Path,
) -> String {
    "ERR service-repair: native service repair is unavailable on this platform\n".into()
}

#[cfg(any(unix, test))]
fn encode(operation: &str, payload: &Value) -> Result<String, String> {
    validate(operation, payload)?;
    let body = aterm_json::to_string(&json!({"schema":1,"op":operation,"payload":payload}))
        .map_err(|e| e.to_string())?;
    Request::parse(&body)?;
    Ok(format!("{VERB} {body}"))
}
#[cfg(any(unix, test))]
fn decode(operation: &str, body: &str) -> Result<Value, String> {
    if body.len() > MAX_REPLY {
        return Err("service repair reply exceeds 512 KiB".into());
    }
    let value: Value = aterm_json::from_str(body).map_err(|e| e.to_string())?;
    exact_fields(&value, &["schema", "op", "result"])?;
    if value["schema"].as_u64() != Some(1) || value["op"].as_str() != Some(operation) {
        return Err("service repair reply belongs to another protocol or operation".into());
    }
    Ok(value["result"].clone())
}

#[cfg(any(unix, test))]
fn verify_peer(result: &Value, node: &str, key: &str) -> Result<(), String> {
    if result["node"].as_str() != Some(node) || result["key"].as_str() != Some(key) {
        return Err("service repair result is not signed by the authenticated target".into());
    }
    Ok(())
}

/// Send exactly one operation over the existing authenticated Drive transport.
/// # Errors
/// Invalid input, refused grant, deadline, oversized/malformed reply, or remote refusal.
/// A lost activation response remains uncertain; repeat only the same signed nonce.
#[cfg(unix)]
pub fn request(host: &str, operation: &str, payload: &Value) -> Result<Value, String> {
    let line = encode(operation, payload)?;
    let (link, peer) = crate::drive_fleet_proven(
        host,
        &aterm_types::control_verbs::Requester::default(),
        WITHIN,
    )
    .map_err(|e| e.to_string())?;
    let mut client = crate::RelayClient::new(link.stream);
    let (header, body) = client
        .request_counted_with_limit(&line, MAX_REPLY)
        .map_err(|e| {
            format!("service repair completion unconfirmed: {e}; retain the same signed nonce")
        })?;
    if header != "OK 1" {
        return Err(aterm_codec::tty::one_line(&header, 400));
    }
    let result = decode(operation, &body)?;
    verify_peer(&result, &peer.node, &peer.key.hex())?;
    Ok(result)
}

#[cfg(test)]
#[path = "service_repair_rpc_tests.rs"]
mod tests;
