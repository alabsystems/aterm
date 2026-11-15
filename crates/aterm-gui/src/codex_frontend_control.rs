// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Independent controller tenure. No Source member or provider grant is issued.
use super::*;

pub(super) fn handles(line: &str) -> bool {
    matches!(
        line.split_ascii_whitespace().next(),
        Some("controller-open" | "controller-close")
    )
}

pub(super) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    lifetime: &account_work::Lifetime,
    registry: &registry::Registry,
    connection_id: Option<u64>,
) -> String {
    let run = || -> Result<String, registry::Error> {
        if line.len() > MAX_FRAME {
            return Err(registry::Error::Denied);
        }
        let mut words = line.split_ascii_whitespace();
        let action = words.next().ok_or(registry::Error::Denied)?;
        let mut fields = BTreeMap::new();
        for word in words {
            let (key, value) = word.split_once('=').ok_or(registry::Error::Denied)?;
            if value.is_empty() || fields.insert(key, value).is_some() {
                return Err(registry::Error::Denied);
            }
        }
        if fields.remove("schema") != Some("1") {
            return Err(registry::Error::Denied);
        }
        let request = fields.remove("request").ok_or(registry::Error::Denied)?;
        let operation = fields.remove("operation").ok_or(registry::Error::Denied)?;
        if Sequence::parse(request).is_none() || !hex(operation, 32) {
            return Err(registry::Error::Denied);
        }
        let close = if action == "controller-close" {
            let member = fields.remove("member").ok_or(registry::Error::Denied)?;
            let original = fields
                .remove("controller_operation")
                .ok_or(registry::Error::Denied)?;
            if !hex(member, 32) || !hex(original, 32) {
                return Err(registry::Error::Denied);
            }
            Some((member, original))
        } else if action == "controller-open" {
            None
        } else {
            return Err(registry::Error::Denied);
        };
        if !fields.is_empty() {
            return Err(registry::Error::Denied);
        }
        let id = connection_id
            .filter(|id| *id != 0)
            .ok_or(registry::Error::Denied)?;
        let (caller, peer) = claim(ctx, master, registry)?;
        let connection = registry.connection(id)?;
        let member = if let Some((member, original)) = close {
            // Withdrawal is release-only and may finish while new App work is closed.
            registry.close_frontend_controller(&peer, &connection, member, original)?;
            member.to_owned()
        } else {
            let scope = lifetime.enter().ok_or(registry::Error::Denied)?;
            registry
                .open_frontend_controller(&peer, &connection, operation, &scope)?
                .member
        };
        if !peer.current() || !connection.is_live() {
            return Err(registry::Error::Stale);
        }
        let epoch = host_epoch().ok_or(registry::Error::Unavailable)?;
        frame(
            action,
            &aterm_json::json!({"schema":1,"request":request,"operation":operation,
            "session":session(&caller, epoch),"member":member}),
        )
        .ok_or(registry::Error::Unavailable)
    };
    run().unwrap_or_else(use_control::error)
}
