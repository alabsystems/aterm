// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Display-free real control front door. Its no-argument entry uses shipping policy;
//! an explicit Rust factory can supply per-session discovery policy. Neither entry
//! creates a vendor child, PTY, claim or native mutation case.

use super::control_lanes::LaneService as _;
use super::*;
use aterm_agent::harness::native_act::{Constructor, Reply, Request as NativeRequest};
use aterm_window::platform::headless::EventLoopBuilderExtHeadless as _;
use std::io;
use std::time::{Duration, Instant};

const MUST_HAPPEN: Duration = Duration::from_secs(60);

fn failure(words: impl Into<String>) -> io::Error {
    io::Error::other(words.into())
}

/// A finite fixture worker uses the production context/authentication/dispatch
/// and EOF claim cleanup, with one-minute read/write hang detectors. No control
/// lane pool or instance listener/fleet singleton is started by this facade.
fn serve_connection(context: &ControlWorkerContext, stream: CtlStream) -> io::Result<bool> {
    control_auth::peer_check(&stream).map_err(failure)?;
    // The production lane establishes blocking mode before authentication too.
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(MUST_HAPPEN))?;
    let _connection = ConnectionScope::enter(&context.store, &context.proxy, &stream);
    let _direct = DirectPeer::enter();
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(&stream, &mut unread);
    let Some((scope, principal, inline)) = context.authenticate(&stream, &mut reader) else {
        return Ok(false);
    };
    if principal != Principal::Instance
        || serving_peer_pid() != Some(std::process::id())
        || serving_connection().is_none()
    {
        return Err(failure(
            "real instance authentication/peer attribution differed",
        ));
    }
    let _principal = ServingPrincipal::enter(principal);
    let mut memo = None;
    let _caller = ServingCaller::enter(&mut memo);
    let mut inline = inline.filter(|line| !line.is_empty());
    let mut out = control_lanes::Outbox::default();
    loop {
        let line = match inline.take() {
            Some(line) => line,
            None => {
                let Some(line) = read_request_line_until(
                    &stream,
                    &mut reader,
                    &mut out,
                    Instant::now() + MUST_HAPPEN,
                ) else {
                    out.finish_waiting(&stream, &mut reader)?;
                    // A parser timeout/refusal is not proof the owner reached
                    // EOF. Require the actual peer closure before reporting it.
                    stream.set_nonblocking(true)?;
                    return match (&stream).read(&mut [0]) {
                        Ok(0) if reader.buffer().is_empty() => Ok(true),
                        _ => Err(failure("fixture connection did not close at EOF")),
                    };
                };
                line
            }
        };
        let request = Request::parse(line);
        let _final_reply = FINAL_REPLIES
            .enter(&request.head())
            .or_else(|| context.owed_input_reply(&request));
        let disposition = context.serve_line(request, scope, &stream, &mut reader, &mut out);
        out.finish_waiting(&stream, &mut reader)?;
        match disposition {
            Some(ServeDisposition::Close) => return Ok(true),
            Some(ServeDisposition::Subscribe { .. }) => {
                return Err(failure("fixture does not own a subscription worker"));
            }
            None if out.closes() => return Ok(true),
            None => {}
        }
    }
}

fn accept_until(listener: &CtlListener, deadline: Instant) -> io::Result<CtlStream> {
    loop {
        match aterm_uds::kept_out(|| listener.accept()) {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(failure("fixture connection accept hang detector expired"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
}

fn observe_policy(socket: &str, sid: &str, expected: Vec<Constructor>) -> io::Result<()> {
    let mut wrong = aterm_agent::RelayClient::connect_local(socket, "invalid-fixture-token")?
        .bounded(MUST_HAPPEN);
    let refusal = wrong.request_line(&format!("@{sid} native-act constructors"));
    if !matches!(refusal.as_deref(), Ok("ERR auth")) {
        return Err(failure("wrong-token connection did not receive ERR auth"));
    }
    drop(wrong);

    // This uses the actual beside-token connector, not a pre-resolved owner or
    // an injected Principal. The server independently checks this peer's uid.
    let mut client =
        aterm_agent::harness::upgrade_drive::harness_client_within(socket, None, MUST_HAPPEN)?;
    let info = aterm_agent::harness::native_act::exchange(&mut client, sid, &NativeRequest::Info)?;
    if !matches!(info, Reply::Info { .. }) {
        return Err(failure("real native host Info was not served"));
    }
    if aterm_agent::harness::native_act::exchange(&mut client, sid, &NativeRequest::Constructors)?
        != Reply::Constructors(expected)
    {
        return Err(failure(
            "constructor policy differed from the configured case",
        ));
    }
    // Dropping the actual connector supplies EOF to the real connection guard.
    drop(client);
    Ok(())
}

/// Run the first real-wire prerequisite in this original harness-capable image.
/// # Errors
/// Headless loop, private endpoint, authentication, real dispatch or owned worker
/// closure fails. No skipped case is reported as a pass.
/// # Panics
/// The platform refuses event-loop construction off the process main thread.
pub fn run_native_host_fixture() -> Result<(), String> {
    run_native_host_fixture_inner(None).map_err(|error| error.to_string())
}

/// The same read-only front door with an explicit per-session Rust policy. This does not
/// create a PTY, prove a shell or enable a CLI/environment activation path.
/// # Errors
/// Session installation or one of [`run_native_host_fixture`]'s observations fails.
/// # Panics
/// The platform refuses event-loop construction off the process main thread.
pub fn run_native_host_fixture_with_policy(
    policy: crate::native_session_policy::FixturePolicy,
) -> Result<(), String> {
    run_native_host_fixture_inner(Some(policy)).map_err(|error| error.to_string())
}

fn run_native_host_fixture_inner(
    policy: Option<crate::native_session_policy::FixturePolicy>,
) -> io::Result<()> {
    // Explicitly selects the display-free backend even where a desktop exists.
    let mut builder = crate::quiet_driver_event_loop_builder::<Wake>();
    builder.with_headless();
    let event_loop = builder
        .build()
        .map_err(|error| failure(error.to_string()))?;
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    // This existing headless factory creates the actual SessionCtx, timeline,
    // lease/custody state and frame. Its master/shell are explicitly unproven:
    // this slice makes no foreground, child-identity or mutation assertion.
    let mut handle = super::control_redraw_conformance::registered_session(1, &term);
    let configured = policy.is_some();
    let expected = policy
        .as_ref()
        .map_or_else(Vec::new, |p| p.enabled_constructors());
    if let Some(policy) = policy {
        Arc::get_mut(&mut handle.ctx)
            .ok_or_else(|| failure("fixture context already shared"))?
            .install_fixture_policy(policy)
            .map_err(|_| failure("fixture policy already installed"))?;
    }
    let sid = handle.sid.as_str().to_owned();
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle.clone());
    let active = Arc::new(Mutex::new(Some(ActiveSession {
        term,
        master: handle.master,
        id: handle.local_id,
        ctx: Arc::clone(&handle.ctx),
    })));
    // No executable is created in this short private socket world.
    let world = aterm_tempfile::Builder::new()
        .prefix("native-host-")
        .tempdir_in("/tmp")?
        .keep();
    println!("WORLD native_host={}", world.display());
    control_auth::ensure_private_dir(&world)?;
    let socket = world.join("c.sock");
    let socket_text = socket
        .to_str()
        .ok_or_else(|| failure("non-UTF8 fixture socket"))?
        .to_owned();
    let token = control_auth::random_token_hex().ok_or_else(|| failure("no token entropy"))?;
    let names = aterm_types::control_socket::token_names_for_sock("c.sock");
    aterm_uds::atomic_file::write(
        &world.join(&names[0]),
        format!("{token}\n").as_bytes(),
        0o600,
    )?;
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket))?;
    listener.set_nonblocking(true)?;
    let context = ControlWorkerContext {
        active,
        store,
        proxy: event_loop.create_proxy(),
        token: Arc::new(token),
        sock_dir: world,
        recovery_count_admission: Arc::new(
            crate::keeper_link::RecoveryCountAdmission::from_retained(Vec::new()),
        ),
    };
    let worker = std::thread::Builder::new()
        .name("native-host-fixture".into())
        .spawn(move || {
            let rejected = accept_until(&listener, Instant::now() + MUST_HAPPEN)?;
            if serve_connection(&context, rejected)? {
                return Err(failure("wrong-token peer was authenticated"));
            }
            let accepted = accept_until(&listener, Instant::now() + MUST_HAPPEN)?;
            if !serve_connection(&context, accepted)? {
                return Err(failure("beside-token peer was not authenticated"));
            }
            Ok(())
        })?;
    // Always join after client errors too: their owners drop before the worker
    // sees EOF. The finite accept/read/write detectors bound every owned wait.
    let result = observe_policy(&socket_text, &sid, expected);
    let closed = worker
        .join()
        .map_err(|_| failure("native host fixture worker panicked"))?;
    result?;
    closed?;
    if handle
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor
        .is_some()
    {
        return Err(failure("read-only policy probe created a supervisor claim"));
    }
    println!(
        "PASS real-host {} policy and wrong-token refusal; worker EOF joined",
        if configured {
            "fixture-configured"
        } else {
            "shipping-empty"
        }
    );
    Ok(())
}
