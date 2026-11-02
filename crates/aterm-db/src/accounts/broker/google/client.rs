// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trusted one-call owner. The client receives only the short-lived capability;
//! provider credentials stay in the host. The durable logical ID is caller-owned.

use super::{Broker, Limits, bind};
use crate::accounts::broker::Selection;
use crate::{
    Error, Result,
    accounts::broker::{
        host::Host,
        transport::{self, Call, Reply},
    },
};
use aterm_uds::secret::Secret;
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

/// Run one Google text request through a private local broker. The caller is
/// trusted launch control; a grant selector alone is never wire authentication.
/// Keep the same logical ID and exact input when recovering a lost response.
/// # Errors
/// Invalid time/ID, missing current authority, local transport or host failure.
/// An error can follow a durable admission and never licenses a new-ID retry.
pub fn request_until(
    broker: Broker,
    grant: [u8; 16],
    logical: [u8; 16],
    payload: Secret,
    deadline: Instant,
) -> Result<Reply> {
    request_with(
        broker,
        Selection::Grant(grant),
        logical,
        payload,
        deadline,
        bind,
    )
}

/// Run with the unique current local-writer grant for this account and the
/// fixed Google adapter. No match or ambiguity refuses without a provider call.
/// # Errors
/// The same authority, deadline and transport refusals as `request_until`.
pub fn request_account_until(
    broker: Broker,
    account: astream_wire::sync::Digest,
    logical: [u8; 16],
    payload: Secret,
    deadline: Instant,
) -> Result<Reply> {
    request_with(
        broker,
        Selection::Account {
            account,
            adapter: super::ADAPTER.into(),
        },
        logical,
        payload,
        deadline,
        bind,
    )
}

fn request_with(
    broker: Broker,
    selection: Selection,
    logical: [u8; 16],
    payload: Secret,
    deadline: Instant,
    bind: impl FnOnce(&Path, Broker, Limits) -> std::io::Result<Host>,
) -> Result<Reply> {
    let within = deadline
        .checked_duration_since(Instant::now())
        .filter(|n| *n >= Duration::from_millis(1) && *n <= Duration::from_secs(60))
        .ok_or_else(|| Error::new("Google request deadline must be within sixty seconds"))?;
    if logical == [0; 16] {
        return Err(Error::new(
            "Google request needs a nonzero durable request ID",
        ));
    }
    // Keep the socket path within Unix sockaddr_un even with a long TMPDIR.
    let dir = aterm_tempfile::Builder::new()
        .prefix("aibr-")
        .tempdir_in("/tmp")
        .map_err(|_| Error::new("private request directory unavailable"))?;
    aterm_uds::private_file::create_private_dir(dir.path())
        .map_err(|_| Error::new("request directory is not private"))?;
    let mut host = bind(
        dir.path(),
        broker,
        Limits::new(1, within).map_err(|_| Error::new("invalid request host limits"))?,
    )
    .map_err(|_| Error::new("private request host unavailable"))?;
    let token = host.issue_selected_until(&selection, within, deadline)?;
    let delivery = token
        .delivery_until(deadline)
        .map_err(|_| Error::new("private request token delivery failed"))?;
    let path = dir.path().to_owned();
    let task = thread::Builder::new()
        .name("account-request".into())
        .spawn(move || {
            let token = transport::receive_token_until(delivery, deadline)?;
            transport::call_until(
                &path,
                Call {
                    token: token.expose(),
                    logical,
                    adapter: super::ADAPTER,
                    payload: payload.expose(),
                },
                deadline,
            )
        })
        .map_err(|_| Error::new("private request client unavailable"))?;
    let result = loop {
        if task.is_finished() {
            break match task.join() {
                Ok(reply) => reply.map_err(|_| {
                    Error::new("request response unavailable; retain the same request ID")
                }),
                Err(_) => Err(Error::new(
                    "private request client failed; retain the same request ID",
                )),
            };
        }
        if Instant::now() >= deadline {
            break Err(Error::new(
                "request deadline elapsed; retain the same request ID",
            ));
        }
        if host.poll().is_err() {
            break Err(Error::new(
                "request host failed; retain the same request ID",
            ));
        }
        thread::sleep(Duration::from_millis(2));
    };
    // Intake closes before revocation or a join. No caller can reuse this
    // endpoint, and even a lost response retains its durable request identity.
    host.stop();
    let _ = host.revoke(token.bytes());
    let _ = host.drain_until(deadline);
    // Intake is closed even if teardown outlasts the request. Never discard a
    // fully received, non-cached provider body just because cleanup timed out.
    result
}

#[cfg(test)]
pub(crate) fn test_request_until(
    broker: Broker,
    selection: Selection,
    logical: [u8; 16],
    payload: Secret,
    deadline: Instant,
    origin: String,
) -> Result<Reply> {
    request_with(
        broker,
        selection,
        logical,
        payload,
        deadline,
        move |dir, broker, limits| super::bind_at(dir, broker, limits, origin),
    )
}
