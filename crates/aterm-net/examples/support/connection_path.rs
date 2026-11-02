// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The explicit connection probe's Network.framework observation. A BSD no-route
//! error alone does not establish a Local Network denial; the connection's own
//! path can report that reason (Apple TN3179). No payload is sent.

use aterm_objc::{RcBlock, SerialQueue};
use std::ffi::{CString, c_char, c_int, c_void};
use std::net::SocketAddr;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// SAFETY: Network.framework's public C declarations in endpoint.h, parameters.h,
// connection.h, path.h and error.h. Each nw_*_t and configuration block is an
// object pointer; the enums and error codes use 32-bit integer ABI slots.
#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    static _nw_parameters_configure_protocol_disable: *const c_void;
    static _nw_parameters_configure_protocol_default_configuration: *const c_void;
    fn nw_endpoint_create_host(host: *const c_char, port: *const c_char) -> *mut c_void;
    fn nw_parameters_create_secure_tcp(tls: *const c_void, tcp: *const c_void) -> *mut c_void;
    fn nw_connection_create(endpoint: *mut c_void, parameters: *mut c_void) -> *mut c_void;
    fn nw_connection_set_queue(connection: *mut c_void, queue: *mut c_void);
    fn nw_connection_set_state_changed_handler(connection: *mut c_void, handler: *mut c_void);
    fn nw_connection_start(connection: *mut c_void);
    fn nw_connection_cancel(connection: *mut c_void);
    fn nw_connection_copy_current_path(connection: *mut c_void) -> *mut c_void;
    fn nw_path_get_status(path: *mut c_void) -> c_int;
    fn nw_path_get_unsatisfied_reason(path: *mut c_void) -> c_int;
    fn nw_error_get_error_domain(error: *mut c_void) -> c_int;
    fn nw_error_get_error_code(error: *mut c_void) -> c_int;
    fn nw_release(object: *mut c_void);
}

/// One create/copy reference, released exactly once.
struct Owned(NonNull<c_void>);

impl Owned {
    fn new(raw: *mut c_void) -> Option<Self> {
        NonNull::new(raw).map(Self)
    }

    fn raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: this holder owns one reference from a create/copy call.
        unsafe { nw_release(self.raw()) };
    }
}

/// Keep the callback and queue alive through cancellation and release. The
/// callback owns only a channel sender, so a late call borrows no stack data.
struct Connection {
    raw: Owned,
    _handler: RcBlock,
    _queue: SerialQueue,
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: raw owns the live connection, and its handler and queue are
        // still alive. Network.framework retains its own references while an
        // asynchronous cancellation finishes; our raw reference drops next.
        unsafe { nw_connection_cancel(self.raw.raw()) };
    }
}

#[derive(Debug)]
pub(super) struct Reading {
    pub state: c_int,
    pub path: Option<c_int>,
    pub reason: Option<c_int>,
    pub error_domain: Option<c_int>,
    pub error_code: Option<c_int>,
}

impl Reading {
    pub fn state_name(&self) -> &'static str {
        match self.state {
            1 => "waiting",
            2 => "preparing",
            3 => "ready",
            4 => "failed",
            5 => "cancelled",
            _ => "unknown",
        }
    }

    pub fn path_name(&self) -> &'static str {
        match self.path {
            Some(1) => "satisfied",
            Some(2) => "unsatisfied",
            Some(3) => "satisfiable",
            _ => "unobserved",
        }
    }

    pub fn reason_name(&self) -> &'static str {
        match self.reason {
            Some(0) => "not-available",
            Some(1) => "cellular-denied",
            Some(2) => "wifi-denied",
            Some(3) => "local-network-denied",
            Some(4) => "vpn-inactive",
            _ => "unobserved",
        }
    }
}

/// Start one TCP connection to the explicit endpoint, wait for a useful state
/// within the caller's budget, copy its current path, then cancel it. The path
/// describes this connection; it is not the machine's global path monitor.
pub(super) fn read(peer: SocketAddr, within: Duration) -> Result<Reading, String> {
    let deadline = Instant::now()
        .checked_add(within)
        .ok_or("connection observation deadline is out of range")?;
    let host = match peer {
        SocketAddr::V6(address) if address.scope_id() != 0 => {
            format!("{}%{}", address.ip(), address.scope_id())
        }
        _ => peer.ip().to_string(),
    };
    let host = CString::new(host).map_err(|e| e.to_string())?;
    let port = CString::new(peer.port().to_string()).map_err(|e| e.to_string())?;
    // SAFETY: both strings are NUL-terminated and live through this create call.
    let endpoint = Owned::new(unsafe { nw_endpoint_create_host(host.as_ptr(), port.as_ptr()) })
        .ok_or("Network.framework did not create the endpoint")?;
    // SAFETY: these exported blocks request disabled TLS and default TCP. The
    // created parameters are retained by this holder until the connection ends.
    let parameters = Owned::new(unsafe {
        nw_parameters_create_secure_tcp(
            _nw_parameters_configure_protocol_disable,
            _nw_parameters_configure_protocol_default_configuration,
        )
    })
    .ok_or("Network.framework did not create TCP parameters")?;
    // SAFETY: endpoint and parameters are live framework objects. The returned
    // create reference is owned here; both inputs outlive this connection.
    let raw = Owned::new(unsafe { nw_connection_create(endpoint.raw(), parameters.raw()) })
        .ok_or("Network.framework did not create the connection")?;
    let queue = SerialQueue::new(c"aterm.connection-probe")
        .ok_or("could not create the observation queue")?;
    let (sender, receiver) = mpsc::channel();
    // SAFETY: connection.h declares void (^)(nw_connection_state_t, nw_error_t).
    // The error is borrowed only during the callback, and only scalar readings
    // enter the channel. No connection pointer or caller's stack is captured.
    let handler = unsafe {
        RcBlock::new2(move |state: c_int, error: *mut c_void| {
            let (domain, code) = if error.is_null() {
                (None, None)
            } else {
                (
                    Some(nw_error_get_error_domain(error)),
                    Some(nw_error_get_error_code(error)),
                )
            };
            let _ = sender.send((state, domain, code));
        })
    }
    .ok_or("could not create the observation handler")?;
    let connection = Connection {
        raw,
        _handler: handler,
        _queue: queue,
    };
    // SAFETY: the owned connection is not started yet; its retained queue and
    // block have the documented types and stay alive through cancellation.
    unsafe {
        nw_connection_set_queue(connection.raw.raw(), connection._queue.as_id().as_ptr());
        nw_connection_set_state_changed_handler(connection.raw.raw(), connection._handler.as_ptr());
        nw_connection_start(connection.raw.raw());
    }
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("no completed path observation within {within:?}"));
        }
        let (state, error_domain, error_code) = receiver
            .recv_timeout(left)
            .map_err(|e| format!("no completed path observation within {within:?}: {e}"))?;
        if !matches!(state, 1 | 3 | 4 | 5) {
            continue;
        }
        // SAFETY: the connection remains owned and uncancelled; a non-null copy
        // result is a new reference whose status/reason are immutable readings.
        let path = Owned::new(unsafe { nw_connection_copy_current_path(connection.raw.raw()) });
        let (path, reason) = path.as_ref().map_or((None, None), |path| {
            // SAFETY: path owns the live copy reference for both scalar reads.
            unsafe {
                (
                    Some(nw_path_get_status(path.raw())),
                    Some(nw_path_get_unsatisfied_reason(path.raw())),
                )
            }
        });
        return Ok(Reading {
            state,
            path,
            reason,
            error_domain,
            error_code,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn a_ready_and_a_refused_loopback_connection_do_not_claim_local_network_denial() {
        // One-minute hang detector, never a latency budget.
        let within = Duration::from_secs(60);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let peer = listener.local_addr().unwrap();
        let ready = read(peer, within).unwrap();
        assert_eq!(ready.state, 3, "{ready:?}");
        assert_eq!(ready.path, Some(1), "{ready:?}");
        assert_ne!(ready.reason, Some(3), "{ready:?}");
        listener.set_nonblocking(true).unwrap();
        let _accepted = listener.accept().unwrap();
        drop(listener);
        let refused = read(peer, within).unwrap();
        assert_ne!(refused.reason, Some(3), "{refused:?}");
        assert!(matches!(refused.state, 1 | 4), "{refused:?}");
        assert!(refused.error_code.is_some(), "{refused:?}");
    }
}
