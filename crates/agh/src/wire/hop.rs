// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The one redirect hop (design §11.4): a fresh request to an allowlisted CDN origin, with no
//! credential.

use crate::error::{Error, Kind, Result, Sub};
#[cfg(any(test, debug_assertions))]
use crate::wire::door;
use crate::wire::{Endpoints, Origin, WireRequest};

/// Rebuild `original` for the `Location` it was redirected to.
/// # Errors
/// Conflict (`Sub::Moved`) for a method other than GET and HEAD; CouldNotRun (`Sub::Protocol`)
/// when `location` does not parse or its origin is not one of `endpoints.asset_hosts()`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AghRouteWall",
        action = "Hop",
        project = "agh::wire::hop::project_hop"
    )
)]
pub fn rebuild(
    original: &WireRequest<'_>,
    location: &str,
    endpoints: &Endpoints,
    user_agent: &str,
) -> Result<WireRequest<'static>> {
    let method = original.method;
    if !method.is_read() {
        return Err(Error::new(
            Kind::Conflict,
            format!("a {} is never redirected", method.as_str()),
        )
        .with_sub(Sub::Moved {
            to: location.to_string(),
        }));
    }
    let leaves =
        |origin: &str| Error::protocol(format!("the redirect leaves the asset hosts: {origin}"));
    let Some((origin, path_and_query)) = Origin::parse(location) else {
        return Err(leaves(
            "a Location that does not parse as an http(s) URL with a plain host",
        ));
    };
    if !endpoints.asset_hosts().contains(&origin) {
        return Err(leaves(&origin.base()));
    }
    let mut headers = Vec::with_capacity(2);
    if let Some(accept) = super::header_in(&original.headers, "accept") {
        headers.push(("Accept".to_string(), accept.to_string()));
    }
    headers.push(("User-Agent".to_string(), user_agent.to_string()));
    Ok(WireRequest::new(
        method,
        format!("{}{path_and_query}", origin.base()),
        headers,
        original.limit,
        original.deadlines,
    ))
}

/// The model's view of a hop: `(hop, hopauth)`, where `hop` is 1 and `hopauth` is 1 when the
/// request carries `auth` or any header for which `door::is_authorization` holds.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_hop(req: &WireRequest<'_>) -> (i64, i64) {
    let carries = req.auth.is_some() || req.headers.iter().any(|(n, _)| door::is_authorization(n));
    (1, i64::from(carries))
}
