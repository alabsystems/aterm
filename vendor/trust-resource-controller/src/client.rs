// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Direct read-only context bootstrap as the actual native producer. Producing
//! member execution uses the separately authenticated helper/proof protocol.

use std::io;
use std::sync::Arc;
use std::time::Instant;

use trust_resource_provenance::{PinnedHelper, Store, process};

use crate::{Channel, ContextQuery, ContextReply, ContextResponse};

/// Delivery facts, never a claim that the original root did not launch or that
/// any resources were released. None authorizes CPU or an execution retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    BeforeRequest,
    Unknown,
    ContextUnavailable,
}
#[derive(Debug)]
pub struct CallError {
    pub delivery: Delivery,
    pub error: io::Error,
}
impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native resource context {:?}: {}", self.delivery, self.error)
    }
}
impl std::error::Error for CallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
fn before(error: io::Error) -> CallError {
    CallError { delivery: Delivery::BeforeRequest, error }
}

/// Cache the original image/host selection once per root. Reconnects validate
/// actual kernel peer and original backing without repeatedly hashing images.
#[derive(Clone)]
pub struct Client {
    host: Arc<PinnedHelper>,
}
impl Client {
    pub fn discover(deadline: Instant) -> io::Result<Self> {
        let host = Store::open_account()?.connect_ancestor(deadline)?;
        Ok(Self::from_helper(Arc::new(host.into_helper(deadline)?)))
    }
    pub fn from_helper(host: Arc<PinnedHelper>) -> Self {
        Self { host }
    }
    /// Obtain observations on an authenticated direct native connection before
    /// choosing initial-vs-borrowed admission. No helper process is spawned and
    /// no CPU credit, memory claim or subgroup is created by this lookup.
    pub fn context(
        &self,
        query: &ContextQuery,
        deadline: Instant,
    ) -> Result<ContextReply, CallError> {
        let payload = query.encode().map_err(before)?;
        let host = self.host.connect(deadline).map_err(before)?;
        let mut channel =
            Channel::new(host.stream().try_clone().map_err(before)?).map_err(before)?;
        // Canonical SID validation above excludes JSON quoting/injection.
        let hello = format!(
            "{{\"version\":3,\"role\":\"native-context\",\"session\":\"{}\"}}",
            query.binding.session
        );
        channel.send(hello.as_bytes(), deadline).map_err(before)?;
        let exchange = (|| {
            channel.send(&payload, deadline)?;
            let response = ContextResponse::decode(&channel.receive(deadline)?, query)?;
            host.revalidate(deadline)?;
            if let ContextResponse::Ready(reply) = &response {
                let (machine, boot) = process::host_boot()?;
                if reply.context.host != machine || reply.context.boot != boot {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "native context host or boot changed",
                    ));
                }
            }
            // Keep the original connected Host alive until the server observes
            // ACK; Darwin can lose peer credentials as soon as it closes.
            channel.send(&[1], deadline)?;
            Ok(response)
        })();
        match exchange {
            Ok(ContextResponse::Ready(reply)) => Ok(reply),
            Ok(ContextResponse::Unavailable(value)) => Err(CallError {
                delivery: Delivery::ContextUnavailable,
                error: io::Error::new(io::ErrorKind::PermissionDenied, value.reason),
            }),
            Err(error) => Err(CallError { delivery: Delivery::Unknown, error }),
        }
    }
}

mod member;
pub use member::{MemberError, MemberIo, ProxyChild};

mod telemetry;
pub use telemetry::ReceivedTelemetry;

mod liveness;
pub use liveness::HostLifeline;
